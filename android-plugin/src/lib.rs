//! The `ulite/android` plugin: compiles the module's Java sources against
//! the Android platform jar for its declared `compileSdk`, merges the
//! module's resources with `aapt2`, dexes the classes with `d8`, and
//! assembles the APK.
//!
//! The module's `android {}` block describes the sources, the class output
//! directory, the SDK compile level, the manifest and resource directory,
//! the minimum SDK, and the APK the module produces. Unlike the plain-JVM
//! plugins, this one cannot run without an Android SDK, and the SDK is not
//! something a module ships with — so the root is expected from the host:
//! `configure` receives `androidSdkDir`, which the host injects from its
//! own `--android-sdk` flag or the usual environment conventions
//! (`ANDROID_HOME`, `ANDROID_SDK_ROOT`, `~/Android/Sdk`). A per-module
//! `sdkDir` key overrides that default when a module targets a different
//! SDK than the rest of the build.
//!
//! `configure` validates the block and performs the toolchain discovery
//! the tasks consume: the `android.jar` for the declared `compileSdk` must
//! exist under `<sdk>/platforms/`, and a `build-tools` release carrying
//! both `aapt2` and `lib/d8.jar` must be present (the highest such release
//! is the one the packaging tasks invoke). Both checks fail at configure
//! time so a broken SDK is reported before anything executes, not at a
//! task boundary.
//!
//! The registered tasks form the packaging chain (all run-tool, all paths
//! absolute):
//!
//! - `prepareBuildDir` / `prepareDex` / `prepareApkDir` — `mkdir -p` the
//!   derived output directories (`aapt2` and `d8` refuse to create their own
//!   parents). The apk's parent is created by its own task because a module
//!   may place the apk outside `<build>/android`.
//! - `mergeResources` — `aapt2 compile --dir <resDir> -o <build>/res.zip`.
//! - `linkResources` — `aapt2 link` the compiled resources with the
//!   manifest into `<build>/resources.apk`, generating `R.java` under
//!   `<build>/R`.
//! - `prepareApk` — `cp <build>/resources.apk <apk>`, seeding the module's
//!   apk with the linked resources before the dex is grafted on (`jar uf`
//!   refuses a missing archive).
//! - `compile` — `javac --release 17 -d <classesDir> -cp <android.jar>:...
//!   -sourcepath <build>/R <sources> <build>/R/<namespace>/R.java`, with
//!   the release pinned because d8 rejects class files newer than 17
//!   regardless of the JDK the host runs. The `R.java` path is derived
//!   from the module's `namespace`, which `linkResources` also hands aapt2
//!   as `--custom-package` so the generated class lands at that path.
//! - `jarClasses` — `jar cf <build>/classes.jar -C <classesDir> .`, since
//!   d8 accepts archives but not directories as input.
//! - `compileDex` — `java -cp <build-tools>/lib/d8.jar D8 --lib
//!   <android.jar> --min-api <minSdk> --output <build>/dex
//!   <build>/classes.jar`.
//! - `packageApk` — `jar uf <apk> -C <build>/dex .`, grafting every
//!   `classes*.dex` d8 emitted (a module that overflows one dex file yields
//!   `classes2.dex`, `classes3.dex`, ...) onto the resources the
//!   `prepareApk` copy placed in the apk.
//! - `signApk` (when `signing {}` is present) — `apksigner sign` with
//!   passwords read from temp files, avoiding password arguments on the
//!   command line.
//!
//! Task inputs are the source files and the resource directory, so the
//! host's fingerprinting leaves a task alone until its own sources change;
//! the platform jar is deliberately not an input. Consumed keys are
//! documented in `docs/android-plugin.md` (Uliab/docs/architecture.md
//! §5.2).

mod bindings {
    #![allow(unsafe_code)]
    #![allow(clippy::missing_safety_doc)]

    wit_bindgen::generate!({
        // The WIT text is the sdk crate's plugin.wit; the path keeps both
        // sides generating from the single source of truth.
        path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
        world: "plugin",
    });

    use crate::{
        android_jar, classpath_bucket, compile_args, d8_args, highest_build_tools, int_value,
        optional_int, package_args, reject_unknown_extensions, resolve_path, resolve_sdk_root,
        rgen_java_path, string_list, string_value,
    };
    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};
    use serde_json::Value;
    use ulite::ulb::task_registrar::{self, Action, AllowlistedTool, RunToolArgs, Task};

    /// Implements the exported `ulb-plugin` interface.
    struct AndroidPlugin;

    impl Guest for AndroidPlugin {
        fn manifest() -> PluginManifest {
            PluginManifest {
                name: "ulite/android".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                abi_version: ulb_plugin_sdk::ABI_VERSION.to_string(),
                // The packaging tasks below invoke exactly these tools.
                tools: vec![
                    "javac".to_string(),
                    "aapt2".to_string(),
                    "jar".to_string(),
                    "java".to_string(),
                    "mkdir".to_string(),
                    "cp".to_string(),
                    "apksigner".to_string(),
                ],
            }
        }

        fn configure(module_config: String) -> Result<(), String> {
            let config: Value = serde_json::from_str(&module_config)
                .map_err(|error| format!("invalid module config JSON: {error}"))?;
            let project_dir = config
                .get("projectDir")
                .and_then(Value::as_str)
                .ok_or_else(|| "module config is missing 'projectDir'".to_owned())?;
            let android = config
                .get("android")
                .ok_or_else(|| "module config has no 'android' block".to_owned())?;

            let compile_sdk = int_value(android, "compileSdk")?;
            let min_sdk = int_value(android, "minSdk")?;
            let target_sdk = optional_int(android, "targetSdk")?.unwrap_or(compile_sdk);
            let namespace = string_value(android, "namespace")?;
            let sources = resolve_paths(project_dir, &string_list(android, "sources")?);
            if sources.is_empty() {
                return Err("the 'android' block declares no sources".to_owned());
            }
            reject_unknown_extensions(&sources)?;
            let classes_dir = resolve_path(project_dir, &string_value(android, "classesDir")?);
            let manifest = resolve_path(project_dir, &string_value(android, "manifest")?);
            let res_dir = resolve_path(project_dir, &string_value(android, "resDir")?);
            let apk = resolve_path(project_dir, &string_value(android, "apk")?);

            // Toolchain discovery doubles as validation: a module cannot
            // compile without its platform jar, and the build-tools release
            // it would package with must carry aapt2 and the d8 jar, so a
            // broken SDK is reported before anything executes.
            let sdk_root = resolve_sdk_root(&config, android, project_dir)?;
            let platform_jar = android_jar(&sdk_root, compile_sdk)?;
            let build_tools = highest_build_tools(&sdk_root)?;
            let d8_jar = build_tools.join("lib").join("d8.jar");
            if !d8_jar.exists() {
                return Err(format!(
                    "no d8.jar under '{}'; expected it at '{}'",
                    build_tools.display(),
                    d8_jar.display()
                ));
            }

            // The platform jar heads the compile classpath so the module's
            // own sources can reference the SDK types.
            let mut classpath = vec![platform_jar.to_string_lossy().into_owned()];
            classpath.extend(classpath_bucket(&config, "compile"));

            // Derived build products live under <project>/build/android, all
            // of them rewritten by the tools below; the apk the module
            // declares is the only path that appears outside that tree.
            let build_dir = std::path::Path::new(project_dir).join("build/android");
            let res_zip = build_dir.join("res.zip");
            let resources_apk = build_dir.join("resources.apk");
            let rgen_dir = build_dir.join("R");
            let rgen_java = rgen_java_path(&rgen_dir, &namespace);
            let classes_jar = std::path::Path::new(project_dir).join("build/classes.jar");
            let dex_dir = std::path::Path::new(project_dir).join("build/dex");
            let apk_dir = std::path::Path::new(&apk).parent().ok_or_else(|| {
                format!("the configured apk path '{apk}' has no parent directory")
            })?;

            run_tool_task(
                "prepareBuildDir",
                vec![],
                vec![],
                vec![],
                AllowlistedTool::Mkdir,
                vec!["-p".to_owned(), build_dir.to_string_lossy().into_owned()],
            )?;
            run_tool_task(
                "prepareApkDir",
                vec![],
                vec![],
                vec![],
                AllowlistedTool::Mkdir,
                vec!["-p".to_owned(), apk_dir.to_string_lossy().into_owned()],
            )?;
            run_tool_task(
                "mergeResources",
                vec![res_dir.clone()],
                vec![res_zip.to_string_lossy().into_owned()],
                vec!["prepareBuildDir".to_owned()],
                AllowlistedTool::Aapt2,
                vec![
                    build_tools.to_string_lossy().into_owned(),
                    "compile".to_owned(),
                    "--dir".to_owned(),
                    res_dir,
                    "-o".to_owned(),
                    res_zip.to_string_lossy().into_owned(),
                ],
            )?;
            run_tool_task(
                "linkResources",
                vec![res_zip.to_string_lossy().into_owned(), manifest.clone()],
                vec![
                    resources_apk.to_string_lossy().into_owned(),
                    rgen_dir.to_string_lossy().into_owned(),
                ],
                vec!["prepareBuildDir".to_owned(), "mergeResources".to_owned()],
                AllowlistedTool::Aapt2,
                vec![
                    build_tools.to_string_lossy().into_owned(),
                    "link".to_owned(),
                    "-o".to_owned(),
                    resources_apk.to_string_lossy().into_owned(),
                    "--manifest".to_owned(),
                    manifest,
                    "-I".to_owned(),
                    platform_jar.to_string_lossy().into_owned(),
                    "--java".to_owned(),
                    rgen_dir.to_string_lossy().into_owned(),
                    "--custom-package".to_owned(),
                    namespace.clone(),
                    "--min-sdk-version".to_owned(),
                    min_sdk.to_string(),
                    "--target-sdk-version".to_owned(),
                    target_sdk.to_string(),
                    res_zip.to_string_lossy().into_owned(),
                ],
            )?;
            // The linked resources become the module's apk, then packageApk
            // grafts the dex onto it; `jar uf` refuses a missing archive, so
            // the copy must run first.
            run_tool_task(
                "prepareApk",
                vec![resources_apk.to_string_lossy().into_owned()],
                vec![apk.clone()],
                vec!["linkResources".to_owned(), "prepareApkDir".to_owned()],
                AllowlistedTool::Cp,
                vec![resources_apk.to_string_lossy().into_owned(), apk.clone()],
            )?;
            run_tool_task(
                "compile",
                sources.clone(),
                vec![classes_dir.clone()],
                vec!["linkResources".to_owned()],
                AllowlistedTool::Javac,
                compile_args(&classes_dir, &classpath, &sources, &rgen_dir, &rgen_java),
            )?;
            run_tool_task(
                "jarClasses",
                vec![classes_dir.clone()],
                vec![classes_jar.to_string_lossy().into_owned()],
                vec!["compile".to_owned()],
                AllowlistedTool::Jar,
                vec![
                    "cf".to_owned(),
                    classes_jar.to_string_lossy().into_owned(),
                    "-C".to_owned(),
                    classes_dir,
                    ".".to_owned(),
                ],
            )?;
            run_tool_task(
                "prepareDex",
                vec![],
                vec![],
                vec![],
                AllowlistedTool::Mkdir,
                vec!["-p".to_owned(), dex_dir.to_string_lossy().into_owned()],
            )?;
            run_tool_task(
                "compileDex",
                vec![classes_jar.to_string_lossy().into_owned()],
                vec![dex_dir.to_string_lossy().into_owned()],
                vec!["jarClasses".to_owned(), "prepareDex".to_owned()],
                AllowlistedTool::Java,
                d8_args(&d8_jar, &platform_jar, min_sdk, &dex_dir, &classes_jar),
            )?;
            run_tool_task(
                "packageApk",
                vec![
                    resources_apk.to_string_lossy().into_owned(),
                    dex_dir.to_string_lossy().into_owned(),
                ],
                vec![apk.clone()],
                vec![
                    "linkResources".to_owned(),
                    "prepareApk".to_owned(),
                    "compileDex".to_owned(),
                ],
                AllowlistedTool::Jar,
                package_args(std::path::Path::new(&apk), &dex_dir),
            )?;

            // APK signing: when the module's `signing {}` block is present,
            // write the passwords to temp files and register a `signApk`
            // task that invokes `apksigner` with `--ks-pass file:`/
            // `--key-pass file:` (avoids passwords on the command line).
            if let Some(signing) = config.get("signing") {
                let store_file = signing
                    .get("storeFile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "the 'signing' block is missing 'storeFile'".to_owned()
                    })?;
                let store_password = signing
                    .get("storePassword")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "the 'signing' block is missing 'storePassword'".to_owned()
                    })?;
                let key_alias = signing
                    .get("keyAlias")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "the 'signing' block is missing 'keyAlias'".to_owned()
                    })?;
                let key_password = signing
                    .get("keyPassword")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "the 'signing' block is missing 'keyPassword'".to_owned()
                    })?;

                let keystore = resolve_path(project_dir, store_file);
                let ks_password_file =
                    build_dir.join("ks-password.txt").to_string_lossy().into_owned();
                let key_password_file =
                    build_dir.join("key-password.txt").to_string_lossy().into_owned();

                // Write the passwords to files so `apksigner` can read
                // them via `--ks-pass file:` / `--key-pass file:`.
                let write_task_name = "writeSigningPasswords";
                task_registrar::register_task(&Task {
                    name: write_task_name.to_owned(),
                    inputs: vec![],
                    outputs: vec![
                        ks_password_file.clone(),
                        key_password_file.clone(),
                    ],
                    depends_on: vec![],
                    action: Action::WriteFile(write_file_args(
                        &ks_password_file,
                        store_password,
                    )),
                })?;
                task_registrar::register_task(&Task {
                    name: "writeSigningKeyPassword".to_owned(),
                    inputs: vec![],
                    outputs: vec![key_password_file.clone()],
                    depends_on: vec![],
                    action: Action::WriteFile(write_file_args(
                        &key_password_file,
                        key_password,
                    )),
                })?;

                run_tool_task(
                    "signApk",
                    vec![
                        apk.clone(),
                        keystore.clone(),
                        ks_password_file.clone(),
                        key_password_file.clone(),
                    ],
                    vec![apk.clone()],
                    vec![
                        "packageApk".to_owned(),
                        write_task_name.to_owned(),
                        "writeSigningKeyPassword".to_owned(),
                    ],
                    AllowlistedTool::Apksigner,
                    vec![
                        build_tools.to_string_lossy().into_owned(),
                        "sign".to_owned(),
                        "--ks".to_owned(),
                        keystore,
                        "--ks-key-alias".to_owned(),
                        key_alias.to_owned(),
                        "--ks-pass".to_owned(),
                        format!("file:{ks_password_file}"),
                        "--key-pass".to_owned(),
                        format!("file:{key_password_file}"),
                        apk,
                    ],
                )?;
            }

            Ok(())
        }

        fn run(input: String) -> String {
            input
        }
    }

    fn resolve_paths(project_dir: &str, paths: &[String]) -> Vec<String> {
        paths
            .iter()
            .map(|path| resolve_path(project_dir, path))
            .collect()
    }

    /// Registers one run-tool task of the packaging chain with the host.
    fn run_tool_task(
        name: &str,
        inputs: Vec<String>,
        outputs: Vec<String>,
        depends_on: Vec<String>,
        tool: AllowlistedTool,
        args: Vec<String>,
    ) -> Result<(), String> {
        task_registrar::register_task(&Task {
            name: name.to_owned(),
            inputs,
            outputs,
            depends_on,
            action: Action::RunTool(RunToolArgs {
                tool,
                args,
                cwd: ".".to_owned(),
            }),
        })
    }

    /// Builds a `write-file` action for the given path and contents.
    fn write_file_args(path: &str, contents: &str) -> task_registrar::WriteFileArgs {
        task_registrar::WriteFileArgs {
            path: path.to_owned(),
            contents: contents.to_owned(),
        }
    }

    // The export generates wasm component symbols (`export_name` with a
    // component-model name), which only link on the wasm32-wasip2 target.
    #[cfg(target_arch = "wasm32")]
    export!(AndroidPlugin);
}

/// Reads a required string-list key of the module block, erroring when the
/// key is absent or holds a non-string entry.
fn string_list(android: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    let entries = android
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("the 'android' block is missing a '{key}' list"))?;
    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("a '{key}' entry is not a string"))
        })
        .collect()
}

/// Reads a required string key of the module block, erroring when the key
/// is absent or holds a non-string value.
fn string_value(android: &serde_json::Value, key: &str) -> Result<String, String> {
    android
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("the 'android' block is missing a '{key}' string"))
}

/// Reads a required integer key of the module block, erroring when the key
/// is absent or holds a non-number. The host serializes integer scalars as
/// JSON numbers, so `compileSdk = 36` arrives as `36`.
fn int_value(android: &serde_json::Value, key: &str) -> Result<i64, String> {
    android
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| format!("the 'android' block is missing a numeric '{key}'"))
}

/// The SDK root the build uses: the module block's `sdkDir` when set
/// (resolved against the project directory like every other block path),
/// otherwise the root the host injected as `androidSdkDir` (its own
/// `--android-sdk` flag or environment conventions, always absolute). Both
/// missing is a configure error — the SDK cannot be invented. The host
/// preopens both roots read-only at their real paths, so either is
/// discoverable from the guest regardless of which one is chosen.
fn resolve_sdk_root(
    config: &serde_json::Value,
    android: &serde_json::Value,
    project_dir: &str,
) -> Result<std::path::PathBuf, String> {
    if let Some(sdk_dir) = android.get("sdkDir").and_then(serde_json::Value::as_str) {
        return Ok(std::path::PathBuf::from(resolve_path(project_dir, sdk_dir)));
    }
    config
        .get("androidSdkDir")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            "no Android SDK root: set 'sdkDir' in the 'android' block, or pass \
             --android-sdk / ANDROID_HOME / ANDROID_SDK_ROOT to the host"
                .to_owned()
        })
}

/// The platform jar for `compile_sdk`: `<sdk>/platforms/android-<N>/android.jar`.
/// A missing jar is an error — the module cannot compile against a compile
/// level the SDK does not have.
fn android_jar(sdk: &std::path::Path, compile_sdk: i64) -> Result<std::path::PathBuf, String> {
    let jar = sdk
        .join("platforms")
        .join(format!("android-{compile_sdk}"))
        .join("android.jar");
    if !jar.exists() {
        return Err(format!(
            "no android.jar for compileSdk {compile_sdk} under '{}'",
            sdk.display()
        ));
    }
    Ok(jar)
}

/// The highest `build-tools` release under the SDK that carries both
/// `aapt2` and `lib/d8.jar`. A release counts only when both are present,
/// because a partially installed build-tools version would break packaging
/// unpredictably; a directory whose name is not a numeric dotted version is
/// skipped. The release is validated here so an unusable SDK fails at
/// configure time, and returned so the packaging tasks know what to invoke.
fn highest_build_tools(sdk: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let build_tools = sdk.join("build-tools");
    let mut candidates: Vec<(Vec<u64>, std::path::PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(&build_tools)
        .map_err(|error| format!("cannot list '{}': {error}", build_tools.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read '{}': {error}", build_tools.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rank) = version_rank(&name) else {
            continue;
        };
        if path.join("aapt2").exists() && path.join("lib").join("d8.jar").exists() {
            candidates.push((rank, path));
        }
    }
    candidates
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
        .ok_or_else(|| {
            format!(
                "no build-tools release with aapt2 and lib/d8.jar under '{}'",
                build_tools.display()
            )
        })
}

/// Numeric major.minor.patch rank of a version directory name; `None` when
/// any component is not a number, so malformed directory names never win
/// the build-tools selection.
fn version_rank(name: &str) -> Option<Vec<u64>> {
    name.split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// Resolves a module-block path against the project directory; an absolute
/// path passes through untouched.
fn resolve_path(project_dir: &str, path: &str) -> String {
    if std::path::Path::new(path).is_absolute() {
        path.to_owned()
    } else {
        std::path::Path::new(project_dir)
            .join(path)
            .to_string_lossy()
            .into_owned()
    }
}

/// Rejects a source file the compiler would not understand, so a typo in
/// the block surfaces as a configure error instead of a javac run. Only
/// `.java` sources are accepted; kotlin compilation is a future slice of
/// this plugin.
fn reject_unknown_extensions(sources: &[String]) -> Result<(), String> {
    for source in sources {
        if !source.ends_with(".java") {
            return Err(format!(
                "source '{source}' is not a .java file; kotlin sources are not supported yet"
            ));
        }
    }
    Ok(())
}

/// The host-resolved jar list of one classpath bucket from the module
/// configuration (`compile`, `testCompile`, ...).
fn classpath_bucket(config: &serde_json::Value, bucket: &str) -> Vec<String> {
    config
        .get("classpath")
        .and_then(|classpath| classpath.get(bucket))
        .and_then(serde_json::Value::as_array)
        .map(|jars| {
            jars.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

/// The javac invocation for the compile task: emit classes to `-d`, feed
/// the classpath (the platform jar headed by any resolved dependency jars)
/// to `-cp`, then the sources followed by the `R.java` aapt2 generated for
/// the module's `namespace` under `rgen_dir`. The `R` directory is on
/// `-sourcepath` so javac can also resolve the per-resource-type `R$*.java`
/// files older aapt2 releases emit alongside `R.java`.
///
/// The `--release` is pinned to 17: d8 rejects class files newer than its
/// supported range, and pinning keeps the output identical regardless of
/// the JDK the host happens to run.
fn compile_args(
    classes_dir: &str,
    classpath: &[String],
    sources: &[String],
    rgen_dir: &std::path::Path,
    rgen_java: &std::path::Path,
) -> Vec<String> {
    let mut args = vec![
        "--release".to_owned(),
        "17".to_owned(),
        "-d".to_owned(),
        classes_dir.to_owned(),
    ];
    args.extend(["-cp".to_owned(), classpath.join(":")]);
    args.extend([
        "-sourcepath".to_owned(),
        rgen_dir.to_string_lossy().into_owned(),
    ]);
    args.extend(sources.iter().cloned());
    args.push(rgen_java.to_string_lossy().into_owned());
    args
}

/// The `R.java` aapt2 emits for `namespace` under `rgen_dir`: the dot
/// segments of the namespace become the directory path, matching the
/// package aapt2 writes the class under when `link` is given the module's
/// `--custom-package`.
fn rgen_java_path(rgen_dir: &std::path::Path, namespace: &str) -> std::path::PathBuf {
    rgen_dir.join(namespace.replace('.', "/")).join("R.java")
}

/// The `java` invocation that runs d8: `-cp <build-tools>/lib/d8.jar`,
/// then the `D8` main class with the platform jar as `--lib`, the module's
/// `minSdk` as `--min-api`, and the class jar as the only input (d8
/// accepts archives, not directories).
fn d8_args(
    d8_jar: &std::path::Path,
    platform_jar: &std::path::Path,
    min_sdk: i64,
    dex_dir: &std::path::Path,
    classes_jar: &std::path::Path,
) -> Vec<String> {
    vec![
        "-cp".to_owned(),
        d8_jar.to_string_lossy().into_owned(),
        "com.android.tools.r8.D8".to_owned(),
        "--lib".to_owned(),
        platform_jar.to_string_lossy().into_owned(),
        "--min-api".to_owned(),
        min_sdk.to_string(),
        "--output".to_owned(),
        dex_dir.to_string_lossy().into_owned(),
        classes_jar.to_string_lossy().into_owned(),
    ]
}

/// Reads an optional integer key of the module block: `Ok(None)` when the
/// key is absent, `Ok(Some(n))` when it holds a number, and an error when a
/// supplied value is not a number — `targetSdk = "36"` must fail configure
/// rather than silently fall back to `compileSdk`.
fn optional_int(android: &serde_json::Value, key: &str) -> Result<Option<i64>, String> {
    match android.get(key) {
        None => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            format!("the 'android' block key '{key}' must be an integer, found {value}")
        }),
    }
}

/// The `jar` invocation that grafts the dex onto the seeded apk: everything
/// under the d8 output directory, so a module that overflows a single dex
/// file (d8 then emits `classes.dex`, `classes2.dex`, ...) gets every
/// emitted dex packaged rather than only the first.
fn package_args(apk: &std::path::Path, dex_dir: &std::path::Path) -> Vec<String> {
    vec![
        "uf".to_owned(),
        apk.to_string_lossy().into_owned(),
        "-C".to_owned(),
        dex_dir.to_string_lossy().into_owned(),
        ".".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        android_jar, classpath_bucket, compile_args, d8_args, highest_build_tools, int_value,
        optional_int, package_args, reject_unknown_extensions, resolve_path, resolve_sdk_root,
        rgen_java_path, string_list, string_value, version_rank,
    };
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("android-plugin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn fake_sdk(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("platforms").join("android-36")).expect("fake platforms");
        std::fs::write(
            dir.join("platforms").join("android-36").join("android.jar"),
            b"",
        )
        .expect("fake android.jar");
        for version in ["35.0.0", "36.0.0"] {
            let tools = dir.join("build-tools").join(version);
            std::fs::create_dir_all(&tools).expect("fake build-tools");
            std::fs::write(tools.join("aapt2"), b"").expect("fake aapt2");
            std::fs::create_dir_all(tools.join("lib")).expect("fake lib");
            std::fs::write(tools.join("lib").join("d8.jar"), b"").expect("fake d8.jar");
        }
    }

    #[test]
    fn compiler_invocation_pins_release_and_heads_the_classpath_with_the_platform_jar() {
        let rgen = std::path::Path::new("/proj/build/android/R");
        let args = compile_args(
            "/proj/build/classes",
            &[
                "/sdk/platforms/android-36/android.jar".to_owned(),
                "/repos/one.jar".to_owned(),
            ],
            &["/proj/src/Main.java".to_owned()],
            rgen,
            &rgen_java_path(rgen, "com.example.ulite"),
        );
        assert_eq!(
            args,
            vec![
                "--release".to_owned(),
                "17".to_owned(),
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/sdk/platforms/android-36/android.jar:/repos/one.jar".to_owned(),
                "-sourcepath".to_owned(),
                "/proj/build/android/R".to_owned(),
                "/proj/src/Main.java".to_owned(),
                "/proj/build/android/R/com/example/ulite/R.java".to_owned(),
            ]
        );
    }

    #[test]
    fn compiler_invocation_keeps_cp_for_a_dep_free_module() {
        let rgen = std::path::Path::new("/proj/build/android/R");
        let args = compile_args(
            "/proj/build/classes",
            &["/sdk/platforms/android-36/android.jar".to_owned()],
            &["/proj/src/Main.java".to_owned()],
            rgen,
            &rgen_java_path(rgen, "com.example.ulite"),
        );
        assert_eq!(
            args,
            vec![
                "--release".to_owned(),
                "17".to_owned(),
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/sdk/platforms/android-36/android.jar".to_owned(),
                "-sourcepath".to_owned(),
                "/proj/build/android/R".to_owned(),
                "/proj/src/Main.java".to_owned(),
                "/proj/build/android/R/com/example/ulite/R.java".to_owned(),
            ]
        );
    }

    #[test]
    fn rgen_java_path_maps_namespace_dots_to_directories() {
        assert_eq!(
            rgen_java_path(
                std::path::Path::new("/proj/build/android/R"),
                "com.example.ulite"
            ),
            std::path::PathBuf::from("/proj/build/android/R/com/example/ulite/R.java")
        );
    }

    #[test]
    fn d8_invocation_runs_the_jar_through_the_d8_main_class() {
        let args = d8_args(
            std::path::Path::new("/sdk/build-tools/36.0.0/lib/d8.jar"),
            std::path::Path::new("/sdk/platforms/android-36/android.jar"),
            21,
            std::path::Path::new("/proj/build/dex"),
            std::path::Path::new("/proj/build/classes.jar"),
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/sdk/build-tools/36.0.0/lib/d8.jar".to_owned(),
                "com.android.tools.r8.D8".to_owned(),
                "--lib".to_owned(),
                "/sdk/platforms/android-36/android.jar".to_owned(),
                "--min-api".to_owned(),
                "21".to_owned(),
                "--output".to_owned(),
                "/proj/build/dex".to_owned(),
                "/proj/build/classes.jar".to_owned(),
            ]
        );
    }

    #[test]
    fn sdk_root_prefers_the_module_sdk_dir_over_the_injected_root() {
        let config = json!({ "androidSdkDir": "/default/sdk" });
        let block = json!({ "sdkDir": "/module/sdk" });
        assert_eq!(
            resolve_sdk_root(&config, &block, "/proj").expect("resolves"),
            PathBuf::from("/module/sdk")
        );
    }

    #[test]
    fn sdk_root_resolves_a_relative_module_sdk_dir_against_the_project_dir() {
        // The host preopens the module sdkDir at `<projectDir>/<path>`, so
        // the plugin must resolve it the same way to see the same directory.
        let config = json!({});
        let block = json!({ "sdkDir": "vendor/sdk" });
        assert_eq!(
            resolve_sdk_root(&config, &block, "/proj").expect("resolves"),
            PathBuf::from("/proj/vendor/sdk")
        );
    }

    #[test]
    fn sdk_root_falls_back_to_the_injected_root() {
        let config = json!({ "androidSdkDir": "/default/sdk" });
        let block = json!({});
        assert_eq!(
            resolve_sdk_root(&config, &block, "/proj").expect("resolves"),
            PathBuf::from("/default/sdk")
        );
    }

    #[test]
    fn sdk_root_errors_when_nowhere_to_be_found() {
        let config = json!({});
        let block = json!({});
        assert!(resolve_sdk_root(&config, &block, "/proj").is_err());
    }

    #[test]
    fn android_jar_accepts_a_present_platform_jar() {
        let sdk = temp_dir("jar-present");
        fake_sdk(&sdk);
        assert_eq!(
            android_jar(&sdk, 36).expect("found"),
            sdk.join("platforms").join("android-36").join("android.jar")
        );
    }

    #[test]
    fn android_jar_rejects_an_unknown_compile_level() {
        let sdk = temp_dir("jar-missing");
        fake_sdk(&sdk);
        let error = android_jar(&sdk, 99).expect_err("missing jar");
        assert!(error.contains("compileSdk 99"), "{error}");
    }

    #[test]
    fn build_tools_picks_the_highest_release_with_both_tools() {
        let sdk = temp_dir("tools-highest");
        fake_sdk(&sdk);
        assert_eq!(
            highest_build_tools(&sdk).expect("found"),
            sdk.join("build-tools").join("36.0.0")
        );
    }

    #[test]
    fn build_tools_skips_releases_missing_a_tool() {
        let sdk = temp_dir("tools-incomplete");
        std::fs::create_dir_all(sdk.join("build-tools").join("35.0.0")).expect("partial release");
        std::fs::write(sdk.join("build-tools").join("35.0.0").join("aapt2"), b"")
            .expect("only aapt2");
        let error = highest_build_tools(&sdk).expect_err("no complete release");
        assert!(error.contains("aapt2 and lib/d8.jar"), "{error}");
    }

    #[test]
    fn optional_int_reads_present_absent_and_invalid() {
        let block = json!({ "minSdk": 21, "targetSdk": "36" });
        assert_eq!(optional_int(&block, "minSdk"), Ok(Some(21)));
        assert_eq!(optional_int(&block, "compileSdk"), Ok(None));
        let error = optional_int(&block, "targetSdk").expect_err("non-number");
        assert!(error.contains("targetSdk"), "{error}");
    }

    #[test]
    fn package_invocation_grafts_every_dex_file_onto_the_apk() {
        let args = package_args(
            std::path::Path::new("/proj/build/app-debug.apk"),
            std::path::Path::new("/proj/build/dex"),
        );
        assert_eq!(
            args,
            vec![
                "uf".to_owned(),
                "/proj/build/app-debug.apk".to_owned(),
                "-C".to_owned(),
                "/proj/build/dex".to_owned(),
                ".".to_owned(),
            ]
        );
    }

    #[test]
    fn version_rank_parses_numeric_dotted_names_only() {
        assert_eq!(version_rank("36.0.0"), Some(vec![36, 0, 0]));
        assert_eq!(version_rank("0.0.1"), Some(vec![0, 0, 1]));
        assert_eq!(version_rank("rc-1"), None);
        assert_eq!(version_rank("36.0."), None);
    }

    #[test]
    fn block_strings_and_lists_read_and_error() {
        let block =
            json!({ "compileSdk": 36, "sources": ["a.java"], "classesDir": "build/classes" });
        assert_eq!(int_value(&block, "compileSdk").expect("number"), 36);
        assert_eq!(
            string_list(&block, "sources").expect("list"),
            vec!["a.java".to_owned()]
        );
        assert_eq!(
            string_value(&block, "classesDir").expect("string"),
            "build/classes"
        );
        assert!(int_value(&block, "sources").is_err());
        assert!(string_value(&block, "compileSdk").is_err());
        assert!(string_list(&block, "missing").is_err());
        assert!(int_value(&block, "missing").is_err());
    }

    #[test]
    fn classpath_bucket_reads_and_defaults() {
        let config = json!({ "classpath": { "compile": ["/repos/one.jar"] } });
        assert_eq!(
            classpath_bucket(&config, "compile"),
            vec!["/repos/one.jar".to_owned()]
        );
        assert!(classpath_bucket(&config, "testCompile").is_empty());
        assert!(classpath_bucket(&json!({}), "compile").is_empty());
    }

    #[test]
    fn relative_block_paths_resolve_against_the_project_dir() {
        assert_eq!(
            resolve_path("/proj", "src/Main.java"),
            "/proj/src/Main.java"
        );
        assert_eq!(resolve_path("/proj", "/abs/Main.java"), "/abs/Main.java");
    }

    #[test]
    fn non_java_sources_are_rejected() {
        assert!(reject_unknown_extensions(&["/proj/src/Main.kt".to_owned()]).is_err());
        assert!(reject_unknown_extensions(&["/proj/src/Main.java".to_owned()]).is_ok());
    }
}
