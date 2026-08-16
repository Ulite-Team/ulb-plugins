//! The `ulite/android` plugin: compiles the module's Java sources against
//! the Android platform jar for its declared `compileSdk`.
//!
//! The module's `android {}` block describes the sources, the class output
//! directory, the SDK compile level, and optionally the SDK root to use.
//! Unlike the plain-JVM plugins, this one cannot run without an Android
//! SDK, and the SDK is not something a module ships with — so the root is
//! expected from the host: `configure` receives `androidSdkDir`, which the
//! host injects from its own `--android-sdk` flag or the usual environment
//! conventions (`ANDROID_HOME`, `ANDROID_SDK_ROOT`, `~/Android/Sdk`). A
//! per-module `sdkDir` key overrides that default when a module targets a
//! different SDK than the rest of the build.
//!
//! `configure` validates the block and performs the toolchain discovery a
//! later release of this plugin will consume: the `android.jar` for the
//! declared `compileSdk` must exist under `<sdk>/platforms/`, and a
//! `build-tools` release carrying both `aapt2` and `d8` must be present
//! (the highest such release is the one a future packaging task would
//! use). Both checks fail at configure time so a broken SDK is reported
//! before anything executes, not at a task boundary.
//!
//! The one task registered today is `compile`: `javac` emits the module's
//! `.java` sources into `classesDir` against the platform jar, with the
//! host-resolved compile classpath following it. Resource merging and dex
//! packaging, the steps that actually invoke `aapt2` and `d8`, are the
//! next slice of this plugin; the discovery done here is the part of that
//! work a configure-time validation can pin down without running the
//! tools. Paths written into the module block are resolved against the
//! injected `projectDir`, so a build succeeds regardless of the directory
//! the host was invoked from. Task inputs are the source files and the
//! output the classes directory, so the host's fingerprinting leaves the
//! task alone until a source changes; the platform jar is deliberately not
//! an input (it is a large, externally-fixed artifact the build never
//! modifies). Consumed keys are documented in `docs/android-plugin.md`
//! (Uliab/docs/architecture.md §5.2).

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
        android_jar, classpath_bucket, compile_args, highest_build_tools, int_value,
        reject_unknown_extensions, resolve_path, resolve_sdk_root, string_list, string_value,
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
                // Every run-tool task below uses this tool.
                tools: vec!["javac".to_string()],
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
            let sources = resolve_paths(project_dir, &string_list(android, "sources")?);
            if sources.is_empty() {
                return Err("the 'android' block declares no sources".to_owned());
            }
            reject_unknown_extensions(&sources)?;
            let classes_dir = resolve_path(project_dir, &string_value(android, "classesDir")?);

            // Toolchain discovery doubles as validation: a module cannot
            // compile without its platform jar, and a build-tools release
            // lacking aapt2 or d8 cannot later package the module, so both
            // must exist for configure to succeed.
            let sdk_root = resolve_sdk_root(&config, android)?;
            let platform_jar = android_jar(&sdk_root, compile_sdk)?;
            highest_build_tools(&sdk_root)?;

            // The platform jar heads the compile classpath so the module's
            // own sources can reference the SDK types.
            let mut classpath = vec![platform_jar.to_string_lossy().into_owned()];
            classpath.extend(classpath_bucket(&config, "compile"));

            task_registrar::register_task(&Task {
                name: "compile".to_owned(),
                inputs: sources.clone(),
                outputs: vec![classes_dir.clone()],
                depends_on: Vec::new(),
                action: Action::RunTool(RunToolArgs {
                    tool: AllowlistedTool::Javac,
                    args: compile_args(&classes_dir, &classpath, &sources),
                    cwd: ".".to_owned(),
                }),
            })?;

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

/// The SDK root the build uses: the module block's `sdkDir` when set,
/// otherwise the root the host injected as `androidSdkDir` (its own
/// `--android-sdk` flag or environment conventions). Both missing is a
/// configure error — the SDK cannot be invented.
fn resolve_sdk_root(
    config: &serde_json::Value,
    android: &serde_json::Value,
) -> Result<std::path::PathBuf, String> {
    if let Some(sdk_dir) = android.get("sdkDir").and_then(serde_json::Value::as_str) {
        return Ok(std::path::PathBuf::from(sdk_dir));
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
/// `aapt2` and `d8`. A release counts only when both binaries are present,
/// because a partially installed build-tools version would break packaging
/// unpredictably; a directory whose name is not a numeric dotted version is
/// skipped. This is the release a future packaging task of this plugin
/// would invoke, discovered and validated here so an unusable SDK fails at
/// configure time.
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
        if path.join("aapt2").exists() && path.join("d8").exists() {
            candidates.push((rank, path));
        }
    }
    candidates
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
        .ok_or_else(|| {
            format!(
                "no build-tools release with aapt2 and d8 under '{}'",
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
/// to `-cp`, then the sources. The classpath is never empty because the
/// platform jar is always on it.
fn compile_args(classes_dir: &str, classpath: &[String], sources: &[String]) -> Vec<String> {
    let mut args = vec!["-d".to_owned(), classes_dir.to_owned()];
    args.extend(["-cp".to_owned(), classpath.join(":")]);
    args.extend(sources.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::{
        android_jar, classpath_bucket, compile_args, highest_build_tools, int_value,
        reject_unknown_extensions, resolve_path, resolve_sdk_root, string_list, string_value,
        version_rank,
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
            std::fs::write(tools.join("d8"), b"").expect("fake d8");
        }
    }

    #[test]
    fn compiler_invocation_heads_the_classpath_with_the_platform_jar() {
        let args = compile_args(
            "/proj/build/classes",
            &[
                "/sdk/platforms/android-36/android.jar".to_owned(),
                "/repos/one.jar".to_owned(),
            ],
            &["/proj/src/Main.java".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/sdk/platforms/android-36/android.jar:/repos/one.jar".to_owned(),
                "/proj/src/Main.java".to_owned(),
            ]
        );
    }

    #[test]
    fn compiler_invocation_keeps_cp_for_a_dep_free_module() {
        let args = compile_args(
            "/proj/build/classes",
            &["/sdk/platforms/android-36/android.jar".to_owned()],
            &["/proj/src/Main.java".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/sdk/platforms/android-36/android.jar".to_owned(),
                "/proj/src/Main.java".to_owned(),
            ]
        );
    }

    #[test]
    fn sdk_root_prefers_the_module_sdk_dir_over_the_injected_root() {
        let config = json!({ "androidSdkDir": "/default/sdk" });
        let block = json!({ "sdkDir": "/module/sdk" });
        assert_eq!(
            resolve_sdk_root(&config, &block).expect("resolves"),
            PathBuf::from("/module/sdk")
        );
    }

    #[test]
    fn sdk_root_falls_back_to_the_injected_root() {
        let config = json!({ "androidSdkDir": "/default/sdk" });
        let block = json!({});
        assert_eq!(
            resolve_sdk_root(&config, &block).expect("resolves"),
            PathBuf::from("/default/sdk")
        );
    }

    #[test]
    fn sdk_root_errors_when_nowhere_to_be_found() {
        let config = json!({});
        let block = json!({});
        assert!(resolve_sdk_root(&config, &block).is_err());
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
        assert!(error.contains("aapt2 and d8"), "{error}");
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
