//! The `ulite/android` plugin: compiles the module's Java and Kotlin
//! sources against the Android platform jar for its declared
//! `compileSdk`, merges the module's resources with `aapt2`, dexes the
//! classes with `d8`, and assembles per-variant APKs.
//!
//! The module's `android {}` block describes the sources, the SDK compile
//! level, the manifest and resource directory, and the minimum SDK. Unlike
//! the plain-JVM plugins, this one cannot run without an Android SDK, and
//! the SDK is not something a module ships with — so the root is expected
//! from the host: `configure` receives `androidSdkDir`, which the host
//! injects from its own `--android-sdk` flag or the usual environment
//! conventions (`ANDROID_HOME`, `ANDROID_SDK_ROOT`, `~/Android/Sdk`). A
//! per-module `sdkDir` key overrides that default when a module targets a
//! different SDK than the rest of the build.
//!
//! ## Variants
//!
//! When the module declares `buildTypes {}` or `productFlavors {}`, the
//! plugin computes the variant matrix as the cartesian product of build
//! types × flavors (one variant per cell). Without either block, the
//! default pair `[debug, release]` is used. Each variant produces its own
//! APK at `<project>/build/<variant>/app-<variant>.apk`.
//!
//! Per-variant `linkResources` tasks pass `--rename-manifest-package` to
//! `aapt2 link` when a flavor sets `applicationIdSuffix`, giving each
//! variant its own identity in the manifest. The `minSdk` and `targetSdk`
//! values passed to `d8` and `aapt2 link` are per-variant: a flavor's
//! `minSdk` overrides the base `android.minSdk` when present.
//!
//! Signing is shared across all variants: the `signing {}` block produces
//! a single pair of password files, and each variant's `signApk` task
//! reads the same keystore and passwords.
//!
//! ## Toolchain discovery
//!
//! `configure` validates the block and performs the toolchain discovery the
//! tasks consume: the `android.jar` for the declared `compileSdk` must
//! exist under `<sdk>/platforms/`, and a `build-tools` release carrying
//! both `aapt2` and `lib/d8.jar` must be present (the highest such
//! release is the one the packaging tasks invoke). Both checks fail at
//! configure time so a broken SDK is reported before anything executes, not
//! at a task boundary.
//!
//! ## Registered tasks
//!
//! The shared tasks (`prepareBuildDir`, `mergeResources`) are registered
//! once. Per-variant tasks are suffixed with the PascalCase variant name
//! (e.g. `compileDebug`, `compileReleaseFree`):
//!
//! - `linkResources<V>` — `aapt2 link` the compiled resources with the
//!   manifest into `<build>/<variant>/resources.apk`, generating `R.java`
//!   under `<build>/<variant>/R/`. When a flavor carries an
//!   `applicationIdSuffix`, the task passes `--rename-manifest-package`.
//! - `seedApk<V>` — copies the variant's resources.apk as the APK seed.
//! - `compile<V>` — `javac` the module's sources against the platform jar.
//! - `jarClasses<V>` — archive the `.class` files for d8.
//! - `compileDex<V>` — `d8` with per-variant `--min-api`.
//! - `packageApk<V>` — graft the dex onto the seeded APK.
//! - `signApk<V>` — `apksigner sign` when the module has a `signing {}`
//!   block.
//!
//! Task inputs are the source files and the resource directory, so the
//! host's fingerprinting leaves a task alone until its own sources change;
//! the platform jar is deliberately not an input. Consumed keys are
//! documented in `docs/android-plugin.md` (Uliab/docs/architecture.md
//! §5.2).

use serde_json::Value;
use ulb_plugin_sdk::UlbConfig;

/// Top-level module configuration for the `ulite/android` plugin.
///
/// The host serializes a module's `android {}` block (plus host-injected
/// keys like `projectDir` and `androidSdkDir`) into this shape and passes
/// it to `configure` as a JSON string.
#[derive(UlbConfig, serde::Deserialize)]
pub struct AndroidPluginConfig {
    /// Host-injected project root directory; all relative block paths
    /// resolve against this.
    #[ulb(rename = "projectDir")]
    pub project_dir: String,

    /// The module's `android {}` block describing sources, SDK levels,
    /// namespace, and resource directories.
    pub android: AndroidBlock,

    /// Host-injected Android SDK root (from `--android-sdk`, `ANDROID_HOME`,
    /// or `ANDROID_SDK_ROOT`); overridden by `android.sdkDir` when present.
    #[serde(default)]
    #[ulb(rename = "androidSdkDir")]
    pub android_sdk_dir: Option<String>,

    /// The module's `signing {}` block; when absent, produced APKs are
    /// unsigned.
    #[serde(default)]
    pub signing: Option<SigningBlock>,

    /// Map of build-type name to its block. Keys define the build-type
    /// dimension of the variant matrix. When absent, defaults to
    /// `{ "debug": {}, "release": {} }`.
    #[serde(default)]
    #[ulb(rename = "buildTypes")]
    #[ulb(
        description = "Build-type map; keys become variant dimensions (default: debug + release)"
    )]
    pub build_types: Option<serde_json::Value>,

    /// Map of flavor name to its block, plus optional top-level `dimension`
    /// key. When absent, no flavor dimension exists.
    #[serde(default)]
    #[ulb(rename = "productFlavors")]
    pub product_flavors: Option<serde_json::Value>,

    /// Host-resolved dependency jars keyed by bucket name (`"compile"`,
    /// `"testCompile"`, etc.).
    #[serde(default)]
    pub classpath: Option<ClasspathBlock>,
}

/// Fields declared inside the `android {}` DSL block.
#[derive(UlbConfig, serde::Deserialize)]
pub struct AndroidBlock {
    /// Android API level to compile against (e.g. `36`). Determines which
    /// `platforms/android-<N>/android.jar` the toolchain uses.
    #[ulb(rename = "compileSdk")]
    pub compile_sdk: i64,

    /// Java package namespace (e.g. `"com.example.app"`). Drives R.java
    /// generation and the `BuildConfig` package.
    pub namespace: String,

    /// Whether Jetpack Compose is enabled. When true, the compose compiler
    /// plugin JAR must be present on the compile classpath.
    #[serde(default)]
    pub compose: Option<bool>,

    /// Source file paths (`.java` and `.kt`) relative to the project
    /// directory. Must be non-empty.
    pub sources: Vec<String>,

    /// Path to `AndroidManifest.xml`, relative to the project directory.
    pub manifest: String,

    /// Path to the Android resources directory for `aapt2 compile --dir`,
    /// relative to the project directory.
    #[ulb(rename = "resDir")]
    pub res_dir: String,

    /// Per-module Android SDK root override (resolved against `projectDir`).
    /// Takes precedence over the host-injected `androidSdkDir`.
    #[serde(default)]
    #[ulb(rename = "sdkDir")]
    pub sdk_dir: Option<String>,

    /// Minimum Android API level. Passed to `d8 --min-api` and
    /// `aapt2 link --min-sdk-version`. Product flavors may override this.
    #[ulb(rename = "minSdk")]
    pub min_sdk: i64,

    /// Target Android API level. Passed to `aapt2 link --target-sdk-version`.
    /// Defaults to `compileSdk` when absent.
    #[serde(default)]
    #[ulb(rename = "targetSdk")]
    pub target_sdk: Option<i64>,

    /// Version code written into the generated `BuildConfig.VERSION_CODE`.
    /// Defaults to `1`.
    #[serde(default)]
    #[ulb(rename = "versionCode")]
    pub version_code: Option<i64>,

    /// Version name written into the generated `BuildConfig.VERSION_NAME`.
    /// Defaults to the empty string.
    #[serde(default)]
    #[ulb(rename = "versionName")]
    pub version_name: Option<String>,

    /// User-defined BuildConfig fields. Each entry is a
    /// `["TYPE", "NAME", "INITIALIZER"]` triple. The evaluator's
    /// `insert_accumulating` may flatten the first triple and nest
    /// subsequent ones as sub-arrays.
    #[serde(default)]
    #[ulb(rename = "buildConfigField")]
    #[ulb(
        description = "List of [\"TYPE\", \"NAME\", \"INITIALIZER\"] triples for custom BuildConfig fields"
    )]
    pub build_config_field: Option<Vec<serde_json::Value>>,
}

/// APK signing configuration. All four fields are required when the block
/// is present. Produces two password files shared across all variants.
#[derive(UlbConfig, serde::Deserialize)]
pub struct SigningBlock {
    /// Path to the keystore file (resolved against the project directory).
    #[ulb(rename = "storeFile")]
    pub store_file: String,

    /// Keystore password, written to a temporary file and passed to
    /// `apksigner` via `--ks-pass file:`.
    #[ulb(rename = "storePassword")]
    pub store_password: String,

    /// Key alias within the keystore.
    #[ulb(rename = "keyAlias")]
    pub key_alias: String,

    /// Key password, written to a temporary file and passed to `apksigner`
    /// via `--key-pass file:`.
    #[ulb(rename = "keyPassword")]
    pub key_password: String,
}

/// Host-resolved dependency jars keyed by bucket name.
#[derive(UlbConfig, serde::Deserialize)]
pub struct ClasspathBlock {
    /// Compile-time dependency jar paths; prepended after the platform jar.
    #[serde(default)]
    pub compile: Option<Vec<String>>,
}

mod bindings {
    #![allow(unsafe_code)]
    #![allow(clippy::missing_safety_doc)]

    wit_bindgen::generate!({
        // The WIT text is the sdk crate's plugin.wit; the path keeps both
        // sides generating from the single source of truth.
        path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
        world: "plugin",
    });

    use crate::AndroidPluginConfig;
    use crate::{
        BuildConfigParams, android_jar, bool_value, classpath_bucket, compile_args,
        compute_variants, d8_args, find_compose_compiler_jar, generate_buildconfig_source,
        highest_build_tools, int_value, kotlinc_android_args, merge_variant_sources, package_args,
        parse_build_config_fields, partition_sources, reject_unknown_extensions, resolve_path,
        resolve_sdk_root, rgen_java_path, string_list, string_value, to_pascal_case,
    };
    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};
    use serde_json::Value;
    use ulb_plugin_sdk::embed_schema;
    use ulite::ulb::task_registrar::{self, Action, AllowlistedTool, RunToolArgs, Task};

    embed_schema!(AndroidPluginConfig);

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
                    "kotlinc".to_string(),
                    "aapt2".to_string(),
                    "jar".to_string(),
                    "java".to_string(),
                    "mkdir".to_string(),
                    "cp".to_string(),
                    "apksigner".to_string(),
                ],
                // The Android toolchain is self-contained; no other plugin
                // is required at configure time.
                dependencies: Vec::new(),
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
            let namespace = string_value(android, "namespace")?;
            let compose = bool_value(android, "compose")?;
            let all_sources = resolve_paths(project_dir, &string_list(android, "sources")?);
            if all_sources.is_empty() {
                return Err("the 'android' block declares no sources".to_owned());
            }
            reject_unknown_extensions(&all_sources)?;
            let manifest = resolve_path(project_dir, &string_value(android, "manifest")?);
            let res_dir = resolve_path(project_dir, &string_value(android, "resDir")?);

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

            // Kotlin-stdlib handling moved into the variant loop below: a
            // variant needs the stdlib jar dexed in exactly when THAT
            // variant's merged sources contain Kotlin — which, with flavor
            // source layering, can differ per variant even when the base
            // `android.sources` is pure Java.

            // Derived build products live under <project>/build/android; the
            // variant-specific subdirectories are created inside the variant
            // loop below.
            let build_dir = std::path::Path::new(project_dir).join("build/android");
            let res_zip = build_dir.join("res.zip");

            // Shared tasks (registered once; all variants depend on them).
            run_tool_task(
                "prepareBuildDir",
                vec![],
                vec![],
                vec![],
                AllowlistedTool::Mkdir,
                vec!["-p".to_owned(), build_dir.to_string_lossy().into_owned()],
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

            // Variant matrix. The flavors map is returned alongside so the
            // per-variant source merge below can resolve each selected
            // flavor's `sources`.
            let (variants, flavor_infos) = compute_variants(&config)?;

            // User-defined BuildConfig fields from `buildConfigField`
            // triples inside the `android {}` block.
            let build_config_fields = parse_build_config_fields(android);

            // Signing (shared across all variants). Password files are
            // written once; each variant's signApk task reads the same files.
            let signing = config.get("signing");
            let mut ks_password_file = String::new();
            let mut key_password_file = String::new();
            let mut keystore = String::new();
            let mut key_alias = String::new();
            if let Some(signing_block) = signing {
                let store_file = signing_block
                    .get("storeFile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "the 'signing' block is missing 'storeFile'".to_owned())?;
                let store_password = signing_block
                    .get("storePassword")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "the 'signing' block is missing 'storePassword'".to_owned())?;
                let alias = signing_block
                    .get("keyAlias")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "the 'signing' block is missing 'keyAlias'".to_owned())?;
                let key_pass = signing_block
                    .get("keyPassword")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "the 'signing' block is missing 'keyPassword'".to_owned())?;

                keystore = resolve_path(project_dir, store_file);
                key_alias = alias.to_owned();
                ks_password_file = build_dir
                    .join("ks-password.txt")
                    .to_string_lossy()
                    .into_owned();
                key_password_file = build_dir
                    .join("key-password.txt")
                    .to_string_lossy()
                    .into_owned();

                task_registrar::register_task(&Task {
                    name: "writeSigningPasswords".to_owned(),
                    inputs: vec![],
                    outputs: vec![ks_password_file.clone()],
                    depends_on: vec![],
                    action: Action::WriteFile(write_file_args(&ks_password_file, store_password)),
                })?;
                task_registrar::register_task(&Task {
                    name: "writeSigningKeyPassword".to_owned(),
                    inputs: vec![],
                    outputs: vec![key_password_file.clone()],
                    depends_on: vec![],
                    action: Action::WriteFile(write_file_args(&key_password_file, key_pass)),
                })?;
            }

            // Per-variant tasks.
            for variant in &variants {
                // Source-set layering: the variant's effective sources are
                // the module's base `android.sources` plus every selected
                // flavor's `sources`.
                let variant_all_sources = merge_variant_sources(
                    &all_sources,
                    project_dir,
                    &variant.flavors,
                    &flavor_infos,
                )?;
                let (variant_java_sources, variant_kotlin_sources) =
                    partition_sources(&variant_all_sources);

                let variant_build = build_dir.join(&variant.variant_dir);
                let variant_classes_dir = variant_build.join("classes");
                let variant_classes_jar = variant_build.join("classes.jar");
                let variant_dex_dir = variant_build.join("dex");
                let variant_resources_apk = variant_build.join("resources.apk");
                let variant_rgen_dir = variant_build.join("R");
                let variant_rgen_java = rgen_java_path(&variant_rgen_dir, &namespace);
                let variant_apk = variant_build.join(&variant.apk_filename);
                let variant_apk_dir = variant_apk.parent().ok_or_else(|| {
                    format!(
                        "variant apk path '{}' has no parent directory",
                        variant_apk.display()
                    )
                })?;

                // When THIS variant's merged sources contain Kotlin, resolve
                // kotlin-stdlib from the compile classpath so D8 dexes it
                // into the APK. kotlinc bundles stdlib on its own classpath
                // during compilation, but the resulting bytecode references
                // stdlib types that must also be present in the dex output.
                let mut d8_extra_jars: Vec<String> = Vec::new();
                if !variant_kotlin_sources.is_empty()
                    && let Some(stdlib) = classpath.iter().find(|j| {
                        j.contains("kotlin-stdlib")
                            && j.ends_with(".jar")
                            && !j.ends_with("-sources.jar")
                    })
                {
                    d8_extra_jars.push(stdlib.clone());
                }

                run_tool_task(
                    &format!("prepareApk{}", variant.name),
                    vec![],
                    vec![],
                    vec![],
                    AllowlistedTool::Mkdir,
                    vec![
                        "-p".to_owned(),
                        variant_apk_dir.to_string_lossy().into_owned(),
                    ],
                )?;
                run_tool_task(
                    &format!("prepareDex{}", variant.name),
                    vec![],
                    vec![],
                    vec![],
                    AllowlistedTool::Mkdir,
                    vec![
                        "-p".to_owned(),
                        variant_dex_dir.to_string_lossy().into_owned(),
                    ],
                )?;

                // BuildConfig.java generation: a WriteFile task that
                // produces the source file for javac. The file lives under
                // <build>/<variant>/generated/buildconfig/<pkg>/ so the
                // namespace's dot segments map to the expected directory
                // structure.
                let buildconfig_pkg_dir = namespace.replace('.', "/");
                let buildconfig_dir = variant_build
                    .join("generated")
                    .join("buildconfig")
                    .join(&buildconfig_pkg_dir);
                let buildconfig_java = buildconfig_dir.join("BuildConfig.java");
                let application_id = format!("{}{}", namespace, variant.application_id_suffix);
                let flavor_name = variant.flavors.first().cloned().unwrap_or_default();
                let build_type_lower = {
                    // Derive the lowercase build type from the variant name:
                    // "DebugFree" → the build type is the first PascalCase
                    // component that is NOT a flavor name.  For simple
                    // debug/release, the entire name IS the build type.
                    if variant.flavors.is_empty() {
                        variant.name.to_lowercase()
                    } else {
                        // Strip each flavor suffix (PascalCase) from the
                        // variant name to recover the build type.
                        let mut remainder = variant.name.as_str();
                        for f in &variant.flavors {
                            let pascal = to_pascal_case(f);
                            if let Some(pos) = remainder.rfind(&pascal) {
                                remainder = &remainder[..pos];
                            }
                        }
                        remainder.to_lowercase()
                    }
                };
                let is_debug = build_type_lower == "debug";
                let android_version_code = int_value(android, "versionCode").unwrap_or(1);
                let android_version_name = string_value(android, "versionName").unwrap_or_default();
                let compile_sdk = int_value(android, "compileSdk")?;
                let buildconfig_source = generate_buildconfig_source(&BuildConfigParams {
                    namespace: &namespace,
                    application_id: &application_id,
                    build_type: &build_type_lower,
                    debug: is_debug,
                    flavor: &flavor_name,
                    version_code: android_version_code,
                    version_name: &android_version_name,
                    min_sdk: variant.min_sdk,
                    target_sdk: variant.target_sdk,
                    compile_sdk,
                    user_fields: &build_config_fields,
                });
                task_registrar::register_task(&Task {
                    name: format!("generateBuildConfig{}", variant.name),
                    inputs: vec![],
                    outputs: vec![buildconfig_java.to_string_lossy().into_owned()],
                    depends_on: vec!["prepareBuildDir".to_owned()],
                    action: Action::WriteFile(write_file_args(
                        &buildconfig_java.to_string_lossy(),
                        &buildconfig_source,
                    )),
                })?;

                run_tool_task(
                    &format!("linkResources{}", variant.name),
                    vec![res_zip.to_string_lossy().into_owned(), manifest.clone()],
                    vec![
                        variant_resources_apk.to_string_lossy().into_owned(),
                        variant_rgen_dir.to_string_lossy().into_owned(),
                    ],
                    vec![
                        "prepareBuildDir".to_owned(),
                        "mergeResources".to_owned(),
                        format!("prepareApk{}", variant.name),
                    ],
                    AllowlistedTool::Aapt2,
                    {
                        let mut args = vec![
                            build_tools.to_string_lossy().into_owned(),
                            "link".to_owned(),
                            "-o".to_owned(),
                            variant_resources_apk.to_string_lossy().into_owned(),
                            "--manifest".to_owned(),
                            manifest.clone(),
                            "-I".to_owned(),
                            platform_jar.to_string_lossy().into_owned(),
                            "--java".to_owned(),
                            variant_rgen_dir.to_string_lossy().into_owned(),
                            "--custom-package".to_owned(),
                            namespace.clone(),
                            "--min-sdk-version".to_owned(),
                            variant.min_sdk.to_string(),
                            "--target-sdk-version".to_owned(),
                            variant.target_sdk.to_string(),
                        ];
                        if !variant.application_id_suffix.is_empty() {
                            args.push("--rename-manifest-package".to_owned());
                            args.push(format!("{}{}", namespace, variant.application_id_suffix));
                        }
                        args.push(res_zip.to_string_lossy().into_owned());
                        args
                    },
                )?;
                run_tool_task(
                    &format!("seedApk{}", variant.name),
                    vec![variant_resources_apk.to_string_lossy().into_owned()],
                    vec![variant_apk.to_string_lossy().into_owned()],
                    vec![
                        format!("linkResources{}", variant.name),
                        format!("prepareApk{}", variant.name),
                    ],
                    AllowlistedTool::Cp,
                    vec![
                        variant_resources_apk.to_string_lossy().into_owned(),
                        variant_apk.to_string_lossy().into_owned(),
                    ],
                )?;
                // Java compilation: always runs because R.java must be
                // compiled so kotlinc can reference R.* classes even in a
                // pure-Kotlin module.
                //
                // Note on compilation order: Java compiles before Kotlin
                // here.  This is correct for Android modules because R.java
                // (generated by aapt2) must be compiled before kotlinc can
                // resolve R.* references.  The limitation is that user Java
                // sources cannot reference Kotlin declarations — a mixed
                // project where Java code depends on Kotlin types will need
                // a multi-pass compilation (compile R.java → compile Kotlin
                // → compile user Java).  Pure-Kotlin and pure-Java modules
                // are unaffected.
                let mut java_inputs = variant_java_sources.clone();
                java_inputs.push(variant_rgen_java.to_string_lossy().into_owned());
                let variant_buildconfig_dir = variant_build.join("generated").join("buildconfig");
                run_tool_task(
                    &format!("compileJava{}", variant.name),
                    java_inputs,
                    vec![variant_classes_dir.to_string_lossy().into_owned()],
                    vec![
                        format!("linkResources{}", variant.name),
                        format!("generateBuildConfig{}", variant.name),
                    ],
                    AllowlistedTool::Javac,
                    compile_args(
                        &variant_classes_dir.to_string_lossy(),
                        &classpath,
                        &variant_java_sources,
                        &variant_rgen_dir,
                        &variant_rgen_java,
                        Some(&variant_buildconfig_dir),
                    ),
                )?;

                // Kotlin compilation: kotlinc sees the classes dir so it
                // resolves the module's own Java classes and R.java.
                let compose_compiler_jar = if compose {
                    Some(find_compose_compiler_jar(&classpath).ok_or_else(|| {
                        "compose = true but the compose compiler plugin JAR was not found on the \
                         compile classpath; add a dependency such as \
                         \"org.jetbrains.kotlin:compose-compiler-plugin:<kotlin-version>\""
                            .to_owned()
                    })?)
                } else {
                    None
                };
                let mut compile_tasks = vec![format!("compileJava{}", variant.name)];
                if !variant_kotlin_sources.is_empty() {
                    run_tool_task(
                        &format!("compileKotlin{}", variant.name),
                        variant_kotlin_sources.clone(),
                        vec![variant_classes_dir.to_string_lossy().into_owned()],
                        vec![format!("compileJava{}", variant.name)],
                        AllowlistedTool::Kotlinc,
                        kotlinc_android_args(
                            &variant_classes_dir.to_string_lossy(),
                            &platform_jar.to_string_lossy(),
                            &classpath,
                            &variant_kotlin_sources,
                            compose_compiler_jar.as_deref(),
                        ),
                    )?;
                    compile_tasks.push(format!("compileKotlin{}", variant.name));
                }

                run_tool_task(
                    &format!("jarClasses{}", variant.name),
                    vec![variant_classes_dir.to_string_lossy().into_owned()],
                    vec![variant_classes_jar.to_string_lossy().into_owned()],
                    compile_tasks,
                    AllowlistedTool::Jar,
                    vec![
                        "cf".to_owned(),
                        variant_classes_jar.to_string_lossy().into_owned(),
                        "-C".to_owned(),
                        variant_classes_dir.to_string_lossy().into_owned(),
                        ".".to_owned(),
                    ],
                )?;
                let mut dex_inputs = vec![variant_classes_jar.to_string_lossy().into_owned()];
                dex_inputs.extend(d8_extra_jars.iter().cloned());
                run_tool_task(
                    &format!("compileDex{}", variant.name),
                    dex_inputs,
                    vec![variant_dex_dir.to_string_lossy().into_owned()],
                    vec![
                        format!("jarClasses{}", variant.name),
                        format!("prepareDex{}", variant.name),
                    ],
                    AllowlistedTool::Java,
                    d8_args(
                        &d8_jar,
                        &platform_jar,
                        variant.min_sdk,
                        &variant_dex_dir,
                        &variant_classes_jar,
                        &d8_extra_jars,
                    ),
                )?;
                run_tool_task(
                    &format!("packageApk{}", variant.name),
                    vec![
                        variant_resources_apk.to_string_lossy().into_owned(),
                        variant_dex_dir.to_string_lossy().into_owned(),
                    ],
                    vec![variant_apk.to_string_lossy().into_owned()],
                    vec![
                        format!("linkResources{}", variant.name),
                        format!("seedApk{}", variant.name),
                        format!("compileDex{}", variant.name),
                    ],
                    AllowlistedTool::Jar,
                    package_args(&variant_apk, &variant_dex_dir),
                )?;
                if signing.is_some() {
                    run_tool_task(
                        &format!("signApk{}", variant.name),
                        vec![
                            variant_apk.to_string_lossy().into_owned(),
                            keystore.clone(),
                            ks_password_file.clone(),
                            key_password_file.clone(),
                        ],
                        vec![variant_apk.to_string_lossy().into_owned()],
                        vec![
                            format!("packageApk{}", variant.name),
                            "writeSigningPasswords".to_owned(),
                            "writeSigningKeyPassword".to_owned(),
                        ],
                        AllowlistedTool::Apksigner,
                        vec![
                            build_tools.to_string_lossy().into_owned(),
                            "sign".to_owned(),
                            "--ks".to_owned(),
                            keystore.clone(),
                            "--ks-key-alias".to_owned(),
                            key_alias.clone(),
                            "--ks-pass".to_owned(),
                            format!("file:{ks_password_file}"),
                            "--key-pass".to_owned(),
                            format!("file:{key_password_file}"),
                            variant_apk.to_string_lossy().into_owned(),
                        ],
                    )?;
                }
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

/// A flavor's parsed `productFlavors {}` entry: its dimension, effective
/// SDK floor override, application-id suffix, and any flavor-specific
/// source paths (raw; resolved against the project directory when a
/// variant's sources are merged).
#[derive(Debug)]
struct FlavorInfo {
    dimension: String,
    min_sdk: Option<i64>,
    application_id_suffix: String,
    /// Raw (unresolved) source paths declared on the flavor.
    sources: Vec<String>,
}

/// A single build variant derived from the cartesian product of build types
/// and product flavors.
#[derive(Debug)]
struct Variant {
    /// PascalCase variant name used as a task suffix (e.g. `Debug`,
    /// `Release`, `DebugFree`).
    name: String,
    /// The effective `minSdk` for this variant (base `android.minSdk`
    /// overridden by the flavor's `minSdk` when present).
    min_sdk: i64,
    /// The effective `targetSdk` for this variant.
    target_sdk: i64,
    /// Suffix appended to the base `applicationId` (from the flavor's
    /// `applicationIdSuffix`, or empty when absent).
    application_id_suffix: String,
    /// Lowercase directory name for this variant's build products
    /// (e.g. `debug`, `release`, `debugFree`).
    variant_dir: String,
    /// APK filename (e.g. `app-debug.apk`, `app-debugFree.apk`).
    apk_filename: String,
    /// Names of the flavors selected into this variant, in product-flavors
    /// map order. A build type without flavors selects none.
    flavors: Vec<String>,
}

/// Computes the variant matrix from the module config's `buildTypes {}` and
/// `productFlavors {}` blocks.
///
/// The matrix is the cartesian product of build types × flavors. When
/// `buildTypes {}` is absent, `[debug, release]` is the default. When
/// `productFlavors {}` is absent, each build type stands alone as a variant.
///
/// Multiple flavor dimensions are not yet supported: all flavors must share
/// the same `dimension`. The cartesian product crosses every build type with
/// every flavor regardless of dimension value.
///
/// Each variant's `minSdk` is the base `android.minSdk` overridden by the
/// flavor's `minSdk` when present. The `applicationIdSuffix` comes from the
/// flavor only.
fn compute_variants(
    config: &serde_json::Value,
) -> Result<(Vec<Variant>, std::collections::BTreeMap<String, FlavorInfo>), String> {
    let android = config
        .get("android")
        .ok_or_else(|| "module config has no 'android' block".to_owned())?;
    let base_min_sdk = int_value(android, "minSdk")?;
    let compile_sdk = int_value(android, "compileSdk")?;
    let base_target_sdk = optional_int(android, "targetSdk")?.unwrap_or(compile_sdk);

    // Build types: explicit from `buildTypes {}`, or the default pair.
    let build_type_names: Vec<String> = match config.get("buildTypes") {
        Some(bt) => bt
            .as_object()
            .ok_or_else(|| "'buildTypes' must be a block".to_owned())?
            .keys()
            .cloned()
            .collect(),
        None => vec!["debug".to_owned(), "release".to_owned()],
    };

    // Product flavors: absent means no flavors (each build type is a variant).

    let flavors: std::collections::BTreeMap<String, FlavorInfo> = match config.get("productFlavors")
    {
        Some(pf) => {
            let obj = pf
                .as_object()
                .ok_or_else(|| "'productFlavors' must be a block".to_owned())?;

            let declared_dimensions: Vec<String> = obj
                .iter()
                .filter(|(k, _)| *k == "dimension")
                .filter_map(|(_, v)| v.as_str().map(str::to_owned))
                .collect();

            let mut map = std::collections::BTreeMap::new();
            for (name, block) in obj {
                if name == "dimension" {
                    continue;
                }
                let block_obj = block
                    .as_object()
                    .ok_or_else(|| format!("flavor '{name}' must be a block"))?;
                let dimension = match block_obj.get("dimension").and_then(Value::as_str) {
                    Some(d) => d.to_owned(),
                    None if declared_dimensions.len() == 1 => declared_dimensions[0].clone(),
                    None => {
                        return Err(format!("flavor '{name}' is missing a 'dimension' key"));
                    }
                };
                let min_sdk = match block_obj.get("minSdk") {
                    Some(v) => Some(
                        v.as_i64()
                            .ok_or_else(|| format!("flavor '{name}' minSdk must be an integer"))?,
                    ),
                    None => None,
                };
                let application_id_suffix = block_obj
                    .get("applicationIdSuffix")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let sources = match block_obj.get("sources") {
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| format!("flavor '{name}' sources must be strings"))
                        })
                        .collect::<Result<Vec<String>, String>>()?,
                    Some(_) => {
                        return Err(format!("flavor '{name}' sources must be a list of strings"));
                    }
                    None => Vec::new(),
                };
                map.insert(
                    name.to_owned(),
                    FlavorInfo {
                        dimension,
                        min_sdk,
                        application_id_suffix,
                        sources,
                    },
                );
            }
            map
        }
        None => std::collections::BTreeMap::new(),
    };

    // Validate: every flavor must have a dimension.
    for (name, info) in &flavors {
        if info.dimension.is_empty() {
            return Err(format!("flavor '{name}' has an empty 'dimension'"));
        }
    }

    let mut variants = Vec::new();

    if flavors.is_empty() {
        // No flavors: each build type is a standalone variant.
        for bt in &build_type_names {
            let name = to_pascal_case(bt);
            let variant_dir = to_pascal_case(bt).to_lowercase();
            let apk_filename = format!("app-{variant_dir}.apk");
            variants.push(Variant {
                name,
                min_sdk: base_min_sdk,
                target_sdk: base_target_sdk,
                application_id_suffix: String::new(),
                variant_dir,
                apk_filename,
                flavors: Vec::new(),
            });
        }
    } else {
        // Cartesian product: build types × flavors.
        // With a single dimension (the common case), every flavor is valid.
        for bt in &build_type_names {
            for (flavor_name, flavor_info) in &flavors {
                let variant_name = format!("{}{}", to_pascal_case(bt), to_pascal_case(flavor_name));
                let variant_dir = format!(
                    "{}{}",
                    to_pascal_case(bt).to_lowercase(),
                    to_pascal_case(flavor_name)
                );
                let apk_filename = format!("app-{}.apk", variant_dir);
                variants.push(Variant {
                    name: variant_name,
                    min_sdk: flavor_info.min_sdk.unwrap_or(base_min_sdk),
                    target_sdk: base_target_sdk,
                    application_id_suffix: flavor_info.application_id_suffix.clone(),
                    variant_dir,
                    apk_filename,
                    flavors: vec![flavor_name.clone()],
                });
            }
        }
    }

    Ok((variants, flavors))
}

/// Merges a variant's effective source list: the module's base sources plus
/// every selected flavor's `sources` (raw paths resolved against the project
/// directory), deduplicated with first occurrence winning.  All returned paths
/// are absolute (resolved against `project_dir`).  Supported extensions are
/// re-validated on the merged list so a flavor cannot sneak in an unsupported
/// file type.
fn merge_variant_sources(
    base: &[String],
    project_dir: &str,
    selected_flavors: &[String],
    flavor_infos: &std::collections::BTreeMap<String, FlavorInfo>,
) -> Result<Vec<String>, String> {
    let mut merged: Vec<String> = base
        .iter()
        .map(|path| resolve_path(project_dir, path))
        .collect();
    for name in selected_flavors {
        if let Some(info) = flavor_infos.get(name) {
            for raw in &info.sources {
                let resolved = resolve_path(project_dir, raw);
                if !merged.contains(&resolved) {
                    merged.push(resolved);
                }
            }
        }
    }
    reject_unknown_extensions(&merged)?;
    Ok(merged)
}

/// Converts a lowercase identifier to PascalCase.
fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    upper + &chars.collect::<String>()
                }
            }
        })
        .collect()
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

/// Reads an optional boolean key, returning `false` when absent.  The host
/// serializes `compose = true` as a JSON `true` and `compose = false` as
/// `false`; a non-boolean value is a configure error.
fn bool_value(android: &serde_json::Value, key: &str) -> Result<bool, String> {
    match android.get(key) {
        Some(serde_json::Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!(
            "the 'android' block key '{key}' must be true or false, got {other}"
        )),
        None => Ok(false),
    }
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
/// the block surfaces as a configure error instead of a compiler run.
/// Only `.java` and `.kt` sources are accepted.
fn reject_unknown_extensions(sources: &[String]) -> Result<(), String> {
    for source in sources {
        if !source.ends_with(".java") && !source.ends_with(".kt") {
            return Err(format!(
                "source '{source}' is neither a .java nor a .kt file"
            ));
        }
    }
    Ok(())
}

/// Splits source paths into `.java` files and everything else (the `.kt`
/// files), preserving order within each half.
fn partition_sources(sources: &[String]) -> (Vec<String>, Vec<String>) {
    sources
        .iter()
        .cloned()
        .partition(|source| source.ends_with(".java"))
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
    buildconfig_dir: Option<&std::path::Path>,
) -> Vec<String> {
    let mut args = vec![
        "--release".to_owned(),
        "17".to_owned(),
        "-d".to_owned(),
        classes_dir.to_owned(),
    ];
    args.extend(["-cp".to_owned(), classpath.join(":")]);
    // Build the -sourcepath: always includes the R directory for R.java;
    // when present, also includes the generated BuildConfig source directory
    // so javac resolves the BuildConfig class automatically.
    let mut sourcepath = rgen_dir.to_string_lossy().into_owned();
    if let Some(bc_dir) = buildconfig_dir {
        sourcepath.push(':');
        sourcepath.push_str(&bc_dir.to_string_lossy());
    }
    args.extend(["-sourcepath".to_owned(), sourcepath]);
    args.extend(sources.iter().cloned());
    args.push(rgen_java.to_string_lossy().into_owned());
    args
}

/// The compose compiler plugin JAR is a Maven artifact whose filename
/// contains `compose-compiler-plugin`.  Scanning the compile classpath
/// avoids hard-coding an artifact coordinate or path convention.
fn find_compose_compiler_jar(classpath: &[String]) -> Option<String> {
    classpath
        .iter()
        .find(|p| p.contains("compose-compiler-plugin") && p.ends_with(".jar"))
        .cloned()
}

/// The kotlinc invocation for the Kotlin compile task: emit classes to `-d`,
/// feed the classpath (the platform jar + dependency jars + the classes dir
/// so kotlinc resolves the module's own Java classes and R.java) to `-cp`,
/// pin the JVM target to 17 to match javac's `--release 17`, then the
/// source files.  When a Compose compiler plugin JAR is provided, it is
/// loaded via `-Xplugin=<path>` — the standard Kotlin CLI contract for
/// compiler plugins.
fn kotlinc_android_args(
    classes_dir: &str,
    platform_jar: &str,
    classpath: &[String],
    sources: &[String],
    compose_compiler_jar: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["-d".to_owned(), classes_dir.to_owned()];
    let mut cp = vec![platform_jar.to_owned()];
    cp.extend(classpath.iter().cloned());
    cp.push(classes_dir.to_owned());
    args.extend(["-cp".to_owned(), cp.join(":")]);
    args.extend(["-jvm-target".to_owned(), "17".to_owned()]);
    if let Some(jar) = compose_compiler_jar {
        args.push(format!("-Xplugin={jar}"));
    }
    args.extend(sources.iter().cloned());
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
/// `minSdk` as `--min-api`, and `classes_jar` plus any additional jars
/// (such as `kotlin-stdlib.jar`) as program inputs.
fn d8_args(
    d8_jar: &std::path::Path,
    platform_jar: &std::path::Path,
    min_sdk: i64,
    dex_dir: &std::path::Path,
    classes_jar: &std::path::Path,
    extra_jars: &[String],
) -> Vec<String> {
    let mut args = vec![
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
    ];
    args.extend(extra_jars.iter().cloned());
    args
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

/// A single user-defined BuildConfig field: its Java type name, constant
/// name, and the literal initializer expression (including quotes for
/// strings, e.g. `"\"abc123\""`).
struct BuildConfigField {
    java_type: String,
    name: String,
    initializer: String,
}

/// Parses `buildConfigField` entries from the `android {}` block. Each entry
/// is a list triple `["TYPE", "NAME", "INITIALIZER"]`.
///
/// The evaluator's `insert_accumulating` flattens repeated list-valued keys.
/// When a single `buildConfigField` is declared, the value is a flat
/// 3-element list. When two or more are declared, the first triple's
/// elements remain as top-level strings and subsequent triples arrive as
/// nested sub-arrays — e.g. `["String", "A", "x", ["int", "B", "3"]]`.
/// This function walks the array and extracts triples from both forms.
fn parse_build_config_fields(android: &serde_json::Value) -> Vec<BuildConfigField> {
    let mut fields = Vec::new();
    if let Some(obj) = android.as_object()
        && let Some(arr) = obj.get("buildConfigField").and_then(|v| v.as_array())
    {
        let mut i = 0;
        while i < arr.len() {
            if let serde_json::Value::Array(sub) = &arr[i]
                && sub.len() == 3
            {
                let java_type = sub[0].as_str().unwrap_or("Object").to_owned();
                let name = sub[1].as_str().unwrap_or("_UNKNOWN_").to_owned();
                let initializer = sub[2].as_str().unwrap_or("null").to_owned();
                fields.push(BuildConfigField {
                    java_type,
                    name,
                    initializer,
                });
                i += 1;
                continue;
            }
            if i + 2 < arr.len()
                && let (Some(a), Some(b), Some(c)) =
                    (arr[i].as_str(), arr[i + 1].as_str(), arr[i + 2].as_str())
            {
                fields.push(BuildConfigField {
                    java_type: a.to_owned(),
                    name: b.to_owned(),
                    initializer: c.to_owned(),
                });
                i += 3;
                continue;
            }
            i += 1;
        }
    }
    fields
}

/// Parameters for generating a single variant's `BuildConfig.java`.
struct BuildConfigParams<'a> {
    namespace: &'a str,
    application_id: &'a str,
    build_type: &'a str,
    debug: bool,
    flavor: &'a str,
    version_code: i64,
    version_name: &'a str,
    min_sdk: i64,
    target_sdk: i64,
    compile_sdk: i64,
    user_fields: &'a [BuildConfigField],
}

/// Generates the Java source for `BuildConfig.java` given the module's
/// android block values, the effective variant info, and any user-defined
/// fields.
fn generate_buildconfig_source(params: &BuildConfigParams<'_>) -> String {
    let mut out = String::new();
    out.push_str("package ");
    out.push_str(params.namespace);
    out.push_str(";\n\n");
    out.push_str("public final class BuildConfig {\n");

    let default_fields = [
        (
            "String",
            "APPLICATION_ID",
            quote_string(params.application_id),
        ),
        ("String", "BUILD_TYPE", quote_string(params.build_type)),
        ("boolean", "DEBUG", params.debug.to_string()),
        ("String", "FLAVOR", quote_string(params.flavor)),
        ("int", "VERSION_CODE", params.version_code.to_string()),
        ("String", "VERSION_NAME", quote_string(params.version_name)),
        ("int", "MIN_SDK_VERSION", params.min_sdk.to_string()),
        ("int", "TARGET_SDK_VERSION", params.target_sdk.to_string()),
        ("int", "COMPILE_SDK_VERSION", params.compile_sdk.to_string()),
    ];

    for (java_type, name, init) in &default_fields {
        out.push_str("  public static final ");
        out.push_str(java_type);
        out.push(' ');
        out.push_str(name);
        out.push_str(" = ");
        out.push_str(init);
        out.push_str(";\n");
    }

    for field in params.user_fields {
        out.push_str("  public static final ");
        out.push_str(&field.java_type);
        out.push(' ');
        out.push_str(&field.name);
        out.push_str(" = ");
        out.push_str(&field.initializer);
        out.push_str(";\n");
    }

    out.push_str("}\n");
    out
}

/// Wraps a string value in Java string literal quotes, escaping backslashes
/// and double quotes to produce valid Java syntax.
fn quote_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
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
        BuildConfigField, BuildConfigParams, FlavorInfo, android_jar, bool_value, classpath_bucket,
        compile_args, compute_variants, d8_args, find_compose_compiler_jar,
        generate_buildconfig_source, highest_build_tools, int_value, kotlinc_android_args,
        merge_variant_sources, optional_int, package_args, parse_build_config_fields,
        partition_sources, reject_unknown_extensions, resolve_path, resolve_sdk_root,
        rgen_java_path, string_list, string_value, to_pascal_case, version_rank,
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
            None,
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
            None,
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
            &[],
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
    fn d8_invocation_includes_extra_jars_for_kotlin_stdlib() {
        let args = d8_args(
            std::path::Path::new("/sdk/build-tools/36.0.0/lib/d8.jar"),
            std::path::Path::new("/sdk/platforms/android-36/android.jar"),
            21,
            std::path::Path::new("/proj/build/dex"),
            std::path::Path::new("/proj/build/classes.jar"),
            &["/libs/kotlin-stdlib-2.0.0.jar".to_owned()],
        );
        assert!(args.contains(&"/libs/kotlin-stdlib-2.0.0.jar".to_owned()));
        assert_eq!(args.len(), 11);
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
        assert!(reject_unknown_extensions(&["/proj/src/Main.txt".to_owned()]).is_err());
        assert!(reject_unknown_extensions(&["/proj/src/Main.java".to_owned()]).is_ok());
        assert!(reject_unknown_extensions(&["/proj/src/Main.kt".to_owned()]).is_ok());
        assert!(
            reject_unknown_extensions(&[
                "/proj/src/Main.java".to_owned(),
                "/proj/src/Main.kt".to_owned()
            ])
            .is_ok()
        );
    }

    #[test]
    fn to_pascal_case_splits_on_underscores_and_capitalizes() {
        assert_eq!(to_pascal_case("debug"), "Debug");
        assert_eq!(to_pascal_case("release"), "Release");
        assert_eq!(to_pascal_case("free"), "Free");
        assert_eq!(to_pascal_case("paid"), "Paid");

        // Mixed-case tails are preserved; digit-leading parts survive.
        assert_eq!(to_pascal_case("myFlavor"), "MyFlavor");
        assert_eq!(to_pascal_case("3d"), "3d");
    }

    #[test]
    fn compute_variants_carries_selected_flavor_names_and_sources() {
        // Variants carry their selected flavor names so the per-variant
        // source merge can find each flavor's `sources`.
        let config = serde_json::json!({
            "android": { "compileSdk": 36, "minSdk": 21 },
            "buildTypes": {
                "debug": {},
                "release": {}
            },
            "productFlavors": {
                "dimension": "tier",
                "free": {
                    "dimension": "tier",
                    "sources": ["src/free/Free.kt"]
                },
                "paid": {
                    "dimension": "tier"
                }
            }
        });
        let (variants, flavor_infos) = compute_variants(&config).expect("variants");
        let free_debug = variants
            .iter()
            .find(|v| v.name == "DebugFree")
            .expect("DebugFree variant");
        assert_eq!(free_debug.flavors, ["free".to_owned()]);
        assert_eq!(
            flavor_infos.get("free").expect("free flavor").sources,
            ["src/free/Free.kt".to_owned()]
        );
        let paid_release = variants
            .iter()
            .find(|v| v.name == "ReleasePaid")
            .expect("ReleasePaid variant");
        assert_eq!(paid_release.flavors, ["paid".to_owned()]);

        // A build type without flavors selects none.
        let plain = serde_json::json!({
            "android": { "compileSdk": 36, "minSdk": 21 }
        });
        let (variants, _) = compute_variants(&plain).expect("variants");
        assert!(variants.iter().all(|v| v.flavors.is_empty()));
    }

    #[test]
    fn compute_variants_defaults_to_debug_and_release() {
        let config = json!({
            "android": { "compileSdk": 36, "minSdk": 21 }
        });
        let (variants, _) = compute_variants(&config).expect("variants");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].name, "Debug");
        assert_eq!(variants[0].variant_dir, "debug");
        assert_eq!(variants[0].apk_filename, "app-debug.apk");
        assert_eq!(variants[1].name, "Release");
        assert_eq!(variants[1].variant_dir, "release");
        assert_eq!(variants[1].apk_filename, "app-release.apk");
    }

    #[test]
    fn compute_variants_explicit_build_types_no_flavors() {
        let config = json!({
            "android": { "compileSdk": 36, "minSdk": 21 },
            "buildTypes": {
                "debug": { "minifyEnabled": false },
                "release": { "minifyEnabled": true }
            }
        });
        let (variants, _) = compute_variants(&config).expect("variants");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].name, "Debug");
        assert_eq!(variants[1].name, "Release");
    }

    #[test]
    fn compute_variants_with_flavors() {
        let config = json!({
            "android": { "compileSdk": 36, "minSdk": 21 },
            "buildTypes": {
                "debug": {},
                "release": {}
            },
            "productFlavors": {
                "dimension": "tier",
                "free": { "applicationIdSuffix": ".free" },
                "paid": { "applicationIdSuffix": ".paid" }
            }
        });
        let (variants, _) = compute_variants(&config).expect("variants");
        assert_eq!(variants.len(), 4);
        let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"DebugFree"));
        assert!(names.contains(&"DebugPaid"));
        assert!(names.contains(&"ReleaseFree"));
        assert!(names.contains(&"ReleasePaid"));
        let debug_free = variants.iter().find(|v| v.name == "DebugFree").unwrap();
        assert_eq!(debug_free.variant_dir, "debugFree");
        assert_eq!(debug_free.apk_filename, "app-debugFree.apk");
        assert_eq!(debug_free.application_id_suffix, ".free");
    }

    #[test]
    fn compute_variants_flavor_overrides_min_sdk() {
        let config = json!({
            "android": { "compileSdk": 36, "minSdk": 21 },
            "productFlavors": {
                "dimension": "tier",
                "free": { "minSdk": 24 }
            }
        });
        let (variants, _) = compute_variants(&config).expect("variants");
        assert_eq!(variants.len(), 2);
        let free = variants
            .iter()
            .find(|v| v.variant_dir == "debugFree")
            .unwrap();
        assert_eq!(free.min_sdk, 24);
        let release_free = variants
            .iter()
            .find(|v| v.variant_dir == "releaseFree")
            .unwrap();
        assert_eq!(release_free.min_sdk, 24);
    }

    #[test]
    fn compute_variants_flavor_missing_dimension_is_error() {
        let config = json!({
            "android": { "compileSdk": 36, "minSdk": 21 },
            "productFlavors": {
                "free": {}
            }
        });
        let error = compute_variants(&config).expect_err("missing dimension");
        assert!(error.contains("dimension"), "{error}");
    }

    #[test]
    fn partition_sources_separates_java_from_kotlin() {
        let sources = vec![
            "/proj/src/Foo.java".to_owned(),
            "/proj/src/Bar.kt".to_owned(),
            "/proj/src/Baz.java".to_owned(),
        ];
        let (java, kotlin) = partition_sources(&sources);
        assert_eq!(
            java,
            vec![
                "/proj/src/Foo.java".to_owned(),
                "/proj/src/Baz.java".to_owned()
            ]
        );
        assert_eq!(kotlin, vec!["/proj/src/Bar.kt".to_owned()]);
    }

    #[test]
    fn partition_sources_handles_pure_java() {
        let sources = vec!["/proj/src/A.java".to_owned(), "/proj/src/B.java".to_owned()];
        let (java, kotlin) = partition_sources(&sources);
        assert_eq!(java.len(), 2);
        assert!(kotlin.is_empty());
    }

    #[test]
    fn partition_sources_handles_pure_kotlin() {
        let sources = vec!["/proj/src/A.kt".to_owned(), "/proj/src/B.kt".to_owned()];
        let (java, kotlin) = partition_sources(&sources);
        assert!(java.is_empty());
        assert_eq!(kotlin.len(), 2);
    }

    #[test]
    fn kotlinc_android_invocation_heads_classpath_with_platform_jar() {
        let args = kotlinc_android_args(
            "/proj/build/classes",
            "/sdk/platforms/android-36/android.jar",
            &["/repos/one.jar".to_owned()],
            &["/proj/src/Main.kt".to_owned()],
            None,
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/sdk/platforms/android-36/android.jar:/repos/one.jar:/proj/build/classes"
                    .to_owned(),
                "-jvm-target".to_owned(),
                "17".to_owned(),
                "/proj/src/Main.kt".to_owned(),
            ]
        );
    }

    #[test]
    fn kotlinc_android_invocation_for_dep_free_module() {
        let args = kotlinc_android_args(
            "/proj/build/classes",
            "/sdk/platforms/android-36/android.jar",
            &[],
            &["/proj/src/Main.kt".to_owned()],
            None,
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/sdk/platforms/android-36/android.jar:/proj/build/classes".to_owned(),
                "-jvm-target".to_owned(),
                "17".to_owned(),
                "/proj/src/Main.kt".to_owned(),
            ]
        );
    }

    #[test]
    fn kotlinc_android_invocation_loads_compose_plugin_via_xplugin() {
        let args = kotlinc_android_args(
            "/proj/build/classes",
            "/sdk/platforms/android-36/android.jar",
            &[],
            &["/proj/src/Main.kt".to_owned()],
            Some("/maven/compose-compiler-plugin-2.0.jar"),
        );
        assert!(
            args.contains(&"-Xplugin=/maven/compose-compiler-plugin-2.0.jar".to_owned()),
            "compose JAR must be loaded via -Xplugin=<path>, got: {args:?}"
        );
    }

    #[test]
    fn kotlinc_android_invocation_omits_compose_plugin_when_absent() {
        let args = kotlinc_android_args(
            "/proj/build/classes",
            "/sdk/platforms/android-36/android.jar",
            &[],
            &["/proj/src/Main.kt".to_owned()],
            None,
        );
        assert!(
            !args.iter().any(|a| a.starts_with("-Xplugin=")),
            "None must not add a -Xplugin arg"
        );
    }

    #[test]
    fn find_compose_compiler_jar_matches_jar_filename() {
        let cp = vec![
            "/maven/appcompat-1.7.0.jar".to_owned(),
            "/maven/compose-compiler-plugin-2.0.0.jar".to_owned(),
        ];
        assert_eq!(
            find_compose_compiler_jar(&cp),
            Some("/maven/compose-compiler-plugin-2.0.0.jar".to_owned())
        );
    }

    #[test]
    fn find_compose_compiler_jar_returns_none_when_absent() {
        let cp = vec!["/maven/appcompat-1.7.0.jar".to_owned()];
        assert_eq!(find_compose_compiler_jar(&cp), None);
    }

    #[test]
    fn bool_value_parses_true() {
        let v = serde_json::json!({ "compose": true });
        assert!(bool_value(&v, "compose").unwrap());
    }

    #[test]
    fn bool_value_parses_false() {
        let v = serde_json::json!({ "compose": false });
        assert!(!bool_value(&v, "compose").unwrap());
    }

    #[test]
    fn bool_value_defaults_to_false_when_absent() {
        let v = serde_json::json!({});
        assert!(!bool_value(&v, "compose").unwrap());
    }

    #[test]
    fn bool_value_rejects_non_boolean() {
        let v = serde_json::json!({ "compose": "yes" });
        let err = bool_value(&v, "compose").unwrap_err();
        assert!(err.contains("true or false"), "{err}");
    }

    #[test]
    fn merge_variant_sources_deduplicates_first_occurrence_wins() {
        let infos = std::collections::BTreeMap::from([(
            "free".to_owned(),
            FlavorInfo {
                dimension: "tier".to_owned(),
                min_sdk: None,
                application_id_suffix: ".free".to_owned(),
                sources: vec!["src/free/Free.kt".to_owned()],
            },
        )]);
        // The base already lists the flavor's file; the resolved flavor
        // path matches the resolved base path, so dedup keeps only the first.
        let merged = merge_variant_sources(
            &["src/Main.java".to_owned(), "src/free/Free.kt".to_owned()],
            "/proj",
            &["free".to_owned()],
            &infos,
        )
        .expect("merges");
        assert_eq!(
            merged,
            [
                "/proj/src/Main.java".to_owned(),
                "/proj/src/free/Free.kt".to_owned()
            ]
        );
    }

    #[test]
    fn merge_variant_sources_appends_flavor_sources_without_overlap() {
        let infos = std::collections::BTreeMap::from([(
            "paid".to_owned(),
            FlavorInfo {
                dimension: "tier".to_owned(),
                min_sdk: None,
                application_id_suffix: ".paid".to_owned(),
                sources: vec!["src/paid/Paid.java".to_owned()],
            },
        )]);
        let merged = merge_variant_sources(
            &["src/Main.java".to_owned()],
            "/proj",
            &["paid".to_owned()],
            &infos,
        )
        .expect("merges");
        assert_eq!(
            merged,
            [
                "/proj/src/Main.java".to_owned(),
                "/proj/src/paid/Paid.java".to_owned()
            ]
        );
    }

    #[test]
    fn merge_variant_sources_resolves_relative_paths_against_project_dir() {
        let infos = std::collections::BTreeMap::from([(
            "paid".to_owned(),
            FlavorInfo {
                dimension: "tier".to_owned(),
                min_sdk: None,
                application_id_suffix: String::new(),
                sources: vec!["src/paid/Paid.java".to_owned()],
            },
        )]);
        let merged =
            merge_variant_sources(&[], "/proj", &["paid".to_owned()], &infos).expect("merges");
        assert_eq!(merged.len(), 1);
        assert!(
            merged[0].starts_with("/proj/"),
            "relative flavor source must resolve against the project dir: {}",
            merged[0]
        );
        assert!(merged[0].ends_with("src/paid/Paid.java"));
    }

    #[test]
    fn merge_variant_sources_rejects_bad_extensions() {
        let infos = std::collections::BTreeMap::from([(
            "evil".to_owned(),
            FlavorInfo {
                dimension: "tier".to_owned(),
                min_sdk: None,
                application_id_suffix: String::new(),
                sources: vec!["src/evil/payload.exe".to_owned()],
            },
        )]);
        let err = merge_variant_sources(&[], "/proj", &["evil".to_owned()], &infos)
            .expect_err("unsupported extension");
        assert!(err.contains("neither a .java nor a .kt"), "{err}");
    }

    #[test]
    fn parse_build_config_fields_extracts_triples() {
        // With two buildConfigField entries, insert_accumulating produces a
        // flat first triple + a nested second triple:
        // ["String", "API_KEY", "\"abc123\"", ["boolean", "FEATURE_FLAG", "true"]]
        let android = serde_json::json!({
            "compileSdk": 36,
            "buildConfigField": [
                "String", "API_KEY", "\"abc123\"",
                ["boolean", "FEATURE_FLAG", "true"]
            ],
            "namespace": "com.example",
        });
        let fields = parse_build_config_fields(&android);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].java_type, "String");
        assert_eq!(fields[0].name, "API_KEY");
        assert_eq!(fields[0].initializer, "\"abc123\"");
        assert_eq!(fields[1].java_type, "boolean");
        assert_eq!(fields[1].name, "FEATURE_FLAG");
        assert_eq!(fields[1].initializer, "true");
    }

    #[test]
    fn parse_build_config_fields_single_triple() {
        let android = serde_json::json!({
            "buildConfigField": ["String", "API_KEY", "abc123"],
        });
        let fields = parse_build_config_fields(&android);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].java_type, "String");
        assert_eq!(fields[0].name, "API_KEY");
        assert_eq!(fields[0].initializer, "abc123");
    }

    #[test]
    fn parse_build_config_fields_three_entries() {
        // Three buildConfigField declarations produce:
        // ["String", "A", "\"x\"", ["int", "B", "3"], ["boolean", "C", "true"]]
        let android = serde_json::json!({
            "buildConfigField": [
                "String", "A", "\"x\"",
                ["int", "B", "3"],
                ["boolean", "C", "true"]
            ],
        });
        let fields = parse_build_config_fields(&android);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].java_type, "String");
        assert_eq!(fields[0].name, "A");
        assert_eq!(fields[1].java_type, "int");
        assert_eq!(fields[1].name, "B");
        assert_eq!(fields[1].initializer, "3");
        assert_eq!(fields[2].java_type, "boolean");
        assert_eq!(fields[2].name, "C");
        assert_eq!(fields[2].initializer, "true");
    }

    #[test]
    fn parse_build_config_fields_ignores_non_triple_values() {
        let android = serde_json::json!({
            "buildConfigField": "not a list",
        });
        let fields = parse_build_config_fields(&android);
        assert!(fields.is_empty());
    }

    #[test]
    fn parse_build_config_fields_returns_empty_when_absent() {
        let android = serde_json::json!({ "compileSdk": 36 });
        assert!(parse_build_config_fields(&android).is_empty());
    }

    #[test]
    fn generate_buildconfig_source_produces_valid_java() {
        let source = generate_buildconfig_source(&BuildConfigParams {
            namespace: "com.example.app",
            application_id: "com.example.app",
            build_type: "debug",
            debug: true,
            flavor: "",
            version_code: 1,
            version_name: "1.0",
            min_sdk: 21,
            target_sdk: 33,
            compile_sdk: 36,
            user_fields: &[],
        });
        assert!(source.contains("package com.example.app;"));
        assert!(source.contains("public final class BuildConfig"));
        assert!(
            source.contains("public static final String APPLICATION_ID = \"com.example.app\";")
        );
        assert!(source.contains("public static final String BUILD_TYPE = \"debug\";"));
        assert!(source.contains("public static final boolean DEBUG = true;"));
        assert!(source.contains("public static final String FLAVOR = \"\";"));
        assert!(source.contains("public static final int VERSION_CODE = 1;"));
        assert!(source.contains("public static final String VERSION_NAME = \"1.0\";"));
        assert!(source.contains("public static final int MIN_SDK_VERSION = 21;"));
        assert!(source.contains("public static final int TARGET_SDK_VERSION = 33;"));
        assert!(source.contains("public static final int COMPILE_SDK_VERSION = 36;"));
    }

    #[test]
    fn generate_buildconfig_source_includes_user_fields() {
        let user_fields = vec![
            BuildConfigField {
                java_type: "String".to_owned(),
                name: "API_KEY".to_owned(),
                initializer: "\"secret\"".to_owned(),
            },
            BuildConfigField {
                java_type: "int".to_owned(),
                name: "MAX_RETRIES".to_owned(),
                initializer: "3".to_owned(),
            },
        ];
        let source = generate_buildconfig_source(&BuildConfigParams {
            namespace: "com.example",
            application_id: "com.example",
            build_type: "release",
            debug: false,
            flavor: "paid",
            version_code: 42,
            version_name: "2.0",
            min_sdk: 24,
            target_sdk: 34,
            compile_sdk: 36,
            user_fields: &user_fields,
        });
        assert!(source.contains("public static final String API_KEY = \"secret\";"));
        assert!(source.contains("public static final int MAX_RETRIES = 3;"));
        assert!(source.contains("public static final String FLAVOR = \"paid\";"));
        assert!(source.contains("public static final boolean DEBUG = false;"));
        assert!(source.contains("public static final int VERSION_CODE = 42;"));
    }

    #[test]
    fn generate_buildconfig_source_includes_application_id_suffix() {
        let source = generate_buildconfig_source(&BuildConfigParams {
            namespace: "com.example",
            application_id: "com.example.free",
            build_type: "debug",
            debug: true,
            flavor: "free",
            version_code: 1,
            version_name: "",
            min_sdk: 21,
            target_sdk: 33,
            compile_sdk: 36,
            user_fields: &[],
        });
        assert!(
            source.contains("APPLICATION_ID = \"com.example.free\""),
            "APPLICATION_ID should include suffix: {source}"
        );
    }

    #[test]
    fn compile_args_includes_buildconfig_dir_on_sourcepath() {
        let rgen = std::path::Path::new("/proj/build/android/R");
        let bc = std::path::Path::new("/proj/build/android/generated/buildconfig");
        let args = compile_args(
            "/proj/build/classes",
            &["/sdk/platforms/android-36/android.jar".to_owned()],
            &["/proj/src/Main.java".to_owned()],
            rgen,
            &rgen_java_path(rgen, "com.example"),
            Some(bc),
        );
        let sp_idx = args.iter().position(|a| a == "-sourcepath").unwrap();
        let sp_value = &args[sp_idx + 1];
        assert!(
            sp_value.contains("/proj/build/android/generated/buildconfig"),
            "sourcepath must include buildconfig dir: {sp_value}"
        );
        assert!(
            sp_value.starts_with("/proj/build/android/R:"),
            "sourcepath must start with R dir: {sp_value}"
        );
    }
}
