//! The `ulite/kmp` plugin: compiles a Kotlin Multiplatform module's
//! shared and platform source sets.
//!
//! The module's `kmp {}` block declares source sets — blocks carrying a
//! `sources` list of `.java`/`.kt` file paths and optionally a `deps {}`
//! block — and target configs (`jvm { classesDir ... jarFile ... }`,
//! `android {}`).
//!
//! **JVM target:** compiles `commonMain` + `jvmMain` into a jar, with
//! optional test support (`testClassesDir`/`testClass`/`testRunner` keys)
//! that compiles `commonTest` + `jvmTest` sources and runs them.
//!
//! **Android target:** compiles `commonMain` + `androidMain` Kotlin, then
//! per-variant `assembleAndroid<Variant>` tasks merge the kmp dex with the
//! `ulite/android` plugin's dex and graft the result into the unsigned APK.
//! If signing is configured, a separate `signKmpAndroid<Variant>` task signs
//! the APK after the dex graft.
//!
//! Consumed keys are documented in `docs/kmp-plugin.md`
//! (Uliab/docs/architecture.md §5.3).

mod bindings {
    #![allow(unsafe_code)]
    #![allow(clippy::missing_safety_doc)]

    wit_bindgen::generate!({
        path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
        world: "plugin",
    });

    use crate::{
        TEST_RUNNER_SOURCE, TEST_RUNNER_SOURCE_PATH, compile_args, compute_variants,
        find_compose_compiler_jar, jar_args, kotlinc_android_args, merged_classpath,
        merged_classpath_bucket, optional_string_list, partition_sources,
        reject_unknown_extensions, resolve_path, resolve_paths, run_test_args,
    };
    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};
    use serde_json::Value;
    use ulite::ulb::task_registrar::{self, Action, AllowlistedTool, RunToolArgs, Task};

    const JVM_SOURCE_SETS: &[&str] = &["commonMain", "jvmMain"];
    const JVM_TEST_SOURCE_SETS: &[&str] = &["commonTest", "jvmTest"];
    const ANDROID_SOURCE_SETS: &[&str] = &["commonMain", "androidMain"];
    const KNOWN_TARGETS: &[&str] = &["jvm", "android", "ios", "desktop", "native", "wasm"];

    struct KmpPlugin;

    impl Guest for KmpPlugin {
        fn manifest() -> PluginManifest {
            PluginManifest {
                name: "ulite/kmp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                abi_version: ulb_plugin_sdk::ABI_VERSION.to_string(),
                tools: vec![
                    "javac".to_string(),
                    "kotlinc".to_string(),
                    "jar".to_string(),
                    "java".to_string(),
                    "apksigner".to_string(),
                ],
                dependencies: vec!["ulite/android".to_string()],
            }
        }

        fn configure(module_config: String) -> Result<(), String> {
            let config: Value = serde_json::from_str(&module_config)
                .map_err(|error| format!("invalid module config JSON: {error}"))?;
            let project_dir = config
                .get("projectDir")
                .and_then(Value::as_str)
                .ok_or_else(|| "module config is missing 'projectDir'".to_owned())?;
            let kmp = config
                .get("kmp")
                .ok_or_else(|| "module config has no 'kmp' block".to_owned())?;
            let kmp = kmp
                .as_object()
                .ok_or_else(|| "the 'kmp' block is not an object".to_owned())?;

            let mut source_sets = Vec::new();
            let mut targets = Vec::new();
            for (name, value) in kmp {
                let block = value
                    .as_object()
                    .ok_or_else(|| format!("the 'kmp.{name}' entry is not a block"))?;
                if block.contains_key("sources") || block.contains_key("deps") {
                    source_sets.push((name.clone(), value));
                } else if KNOWN_TARGETS.contains(&name.as_str()) {
                    targets.push((name.clone(), value));
                } else {
                    return Err(format!(
                        "the 'kmp.{name}' entry is neither a source set (a block with a \
                         'sources' or 'deps' key) nor a known target ({})",
                        KNOWN_TARGETS.join(", ")
                    ));
                }
            }

            let has_jvm = targets.iter().any(|(name, _)| name == "jvm");
            let has_android = targets.iter().any(|(name, _)| name == "android");

            for (name, _) in &targets {
                if name != "jvm" && name != "android" {
                    return Err(format!("the 'kmp.{name}' target is not implemented"));
                }
            }

            if !has_jvm && !has_android {
                return Err("the 'kmp' block declares no 'jvm' or 'android' target".to_owned());
            }

            // ── JVM target ────────────────────────────────────────────
            if has_jvm {
                let jvm = targets
                    .iter()
                    .find(|(name, _)| name == "jvm")
                    .map(|(_, value)| *value)
                    .unwrap();

                let classes_dir = resolve_path(
                    project_dir,
                    jvm.get("classesDir")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            "the 'kmp.jvm' block is missing a 'classesDir' string".to_owned()
                        })?,
                );
                let jar_file = resolve_path(
                    project_dir,
                    jvm.get("jarFile").and_then(Value::as_str).ok_or_else(|| {
                        "the 'kmp.jvm' block is missing a 'jarFile' string".to_owned()
                    })?,
                );

                let mut sources = Vec::new();
                for (name, value) in &source_sets {
                    if JVM_SOURCE_SETS.contains(&name.as_str()) {
                        sources.extend(optional_string_list(value, "sources")?);
                    }
                }
                if sources.is_empty() {
                    return Err("the 'kmp' block declares no sources for the jvm target".to_owned());
                }
                let sources = resolve_paths(project_dir, &sources);
                reject_unknown_extensions(&sources)?;
                let (java_sources, kotlin_sources) = partition_sources(&sources);
                let compile_classpath = merged_classpath(&config, JVM_SOURCE_SETS);

                let mut compile_tasks = Vec::new();
                if !java_sources.is_empty() {
                    task_registrar::register_task(&Task {
                        name: "compile".to_owned(),
                        inputs: java_sources.clone(),
                        outputs: vec![classes_dir.clone()],
                        depends_on: Vec::new(),
                        action: Action::RunTool(RunToolArgs {
                            tool: AllowlistedTool::Javac,
                            args: compile_args(&classes_dir, &compile_classpath, &java_sources),
                            cwd: ".".to_owned(),
                        }),
                    })?;
                    compile_tasks.push("compile".to_owned());
                }
                if !kotlin_sources.is_empty() {
                    let mut depends_on = Vec::new();
                    if !java_sources.is_empty() {
                        depends_on.push("compile".to_owned());
                    }
                    let mut kotlin_classpath = compile_classpath.clone();
                    kotlin_classpath.push(classes_dir.clone());
                    task_registrar::register_task(&Task {
                        name: "compile-kotlin".to_owned(),
                        inputs: kotlin_sources.clone(),
                        outputs: vec![classes_dir.clone()],
                        depends_on,
                        action: Action::RunTool(RunToolArgs {
                            tool: AllowlistedTool::Kotlinc,
                            args: compile_args(&classes_dir, &kotlin_classpath, &kotlin_sources),
                            cwd: ".".to_owned(),
                        }),
                    })?;
                    compile_tasks.push("compile-kotlin".to_owned());
                }

                task_registrar::register_task(&Task {
                    name: "assemble".to_owned(),
                    inputs: vec![classes_dir.clone()],
                    outputs: vec![jar_file.clone()],
                    depends_on: compile_tasks.clone(),
                    action: Action::RunTool(RunToolArgs {
                        tool: AllowlistedTool::Jar,
                        args: jar_args(&jar_file, &classes_dir),
                        cwd: ".".to_owned(),
                    }),
                })?;

                // ── JVM target: tests ──────────────────────────────
                let test_classes_dir_opt = jvm
                    .get("testClassesDir")
                    .and_then(Value::as_str)
                    .map(|dir| resolve_path(project_dir, dir));
                if let Some(test_classes_dir) = test_classes_dir_opt {
                    let test_class = jvm
                        .get("testClass")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let test_runner = jvm
                        .get("testRunner")
                        .and_then(Value::as_str)
                        .map(str::to_owned);

                    if test_class.is_none() && test_runner.is_none() {
                        return Err("the 'kmp.jvm' block has 'testClassesDir' but neither \
                             'testClass' nor 'testRunner'"
                            .to_owned());
                    }
                    if test_class.is_some() && test_runner.is_some() {
                        return Err("the 'kmp.jvm' block specifies both 'testClass' and \
                             'testRunner'; these are mutually exclusive"
                            .to_owned());
                    }

                    let mut test_sources = Vec::new();
                    for (name, value) in &source_sets {
                        if JVM_TEST_SOURCE_SETS.contains(&name.as_str()) {
                            test_sources.extend(optional_string_list(value, "sources")?);
                        }
                    }
                    if !test_sources.is_empty() {
                        let test_sources = resolve_paths(project_dir, &test_sources);
                        reject_unknown_extensions(&test_sources)?;
                    }

                    let test_compile_classpath =
                        merged_classpath_bucket(&config, JVM_TEST_SOURCE_SETS, "testCompile");
                    let test_runtime_classpath =
                        merged_classpath_bucket(&config, JVM_TEST_SOURCE_SETS, "testRuntime");

                    let mut compile_tests_depends = compile_tasks.clone();
                    let mut compile_test_sources = test_sources.clone();
                    let generated_source = resolve_path(project_dir, TEST_RUNNER_SOURCE_PATH);

                    if let Some(ref runner) = test_runner {
                        if runner != "junit-platform" {
                            return Err(format!(
                                "unknown 'testRunner' value '{runner}'; \
                                 the only supported value is 'junit-platform'"
                            ));
                        }
                        task_registrar::register_task(&Task {
                            name: "generate-test-runner".to_owned(),
                            inputs: Vec::new(),
                            outputs: vec![generated_source.clone()],
                            depends_on: Vec::new(),
                            action: Action::WriteFile(ulite::ulb::task_registrar::WriteFileArgs {
                                path: generated_source.clone(),
                                contents: TEST_RUNNER_SOURCE.to_owned(),
                            }),
                        })?;
                        compile_test_sources.push(generated_source.clone());
                        compile_tests_depends.push("generate-test-runner".to_owned());
                    }

                    if !compile_test_sources.is_empty() {
                        task_registrar::register_task(&Task {
                            name: "compile-tests".to_owned(),
                            inputs: compile_test_sources,
                            outputs: vec![test_classes_dir.clone()],
                            depends_on: compile_tests_depends,
                            action: Action::RunTool(RunToolArgs {
                                tool: AllowlistedTool::Javac,
                                args: {
                                    let mut cp = test_compile_classpath.clone();
                                    cp.push(classes_dir.clone());
                                    compile_args(&test_classes_dir, &cp, &[])
                                },
                                cwd: ".".to_owned(),
                            }),
                        })?;

                        let test_args_list =
                            optional_string_list(jvm, "testArgs").unwrap_or_default();

                        let test_run_invocation = if test_runner.is_some() {
                            run_test_args(
                                &{
                                    let mut tp = test_runtime_classpath;
                                    tp.push(test_classes_dir.clone());
                                    tp.push(classes_dir.clone());
                                    tp
                                },
                                "ulite.TestRunner",
                                std::slice::from_ref(&test_classes_dir),
                            )
                        } else {
                            let tc = test_class.expect("validated above");
                            run_test_args(
                                &{
                                    let mut tp = test_runtime_classpath;
                                    tp.push(test_classes_dir.clone());
                                    tp.push(classes_dir.clone());
                                    tp
                                },
                                &tc,
                                &test_args_list,
                            )
                        };

                        task_registrar::register_task(&Task {
                            name: "test".to_owned(),
                            inputs: vec![test_classes_dir.clone(), classes_dir.clone()],
                            outputs: Vec::new(),
                            depends_on: vec!["compile-tests".to_owned()],
                            action: Action::RunTool(RunToolArgs {
                                tool: AllowlistedTool::Java,
                                args: test_run_invocation,
                                cwd: ".".to_owned(),
                            }),
                        })?;
                    }
                }
            }

            // ── Android target ─────────────────────────────────────────
            if has_android {
                let android_build_dir = std::path::Path::new(project_dir).join("build/kmp/android");
                let kmp_jar = android_build_dir.join("classes.jar");
                let kmp_classes_dir = android_build_dir.join("classes");

                let compile_sdk = config
                    .get("android")
                    .and_then(|a| a.get("compileSdk"))
                    .and_then(Value::as_i64)
                    .ok_or_else(|| {
                        "the 'android' block is missing a numeric 'compileSdk'".to_owned()
                    })?;

                let sdk_root = crate::resolve_sdk_root(&config, project_dir)?;
                let platform_jar = crate::android_jar(&sdk_root, compile_sdk)?;

                let mut sources = Vec::new();
                for (name, value) in &source_sets {
                    if ANDROID_SOURCE_SETS.contains(&name.as_str()) {
                        sources.extend(optional_string_list(value, "sources")?);
                    }
                }
                if sources.is_empty() {
                    return Err(
                        "the 'kmp' block declares no sources for the android target".to_owned()
                    );
                }
                let sources = resolve_paths(project_dir, &sources);
                reject_unknown_extensions(&sources)?;
                let (java_sources, kotlin_sources) = partition_sources(&sources);
                if !java_sources.is_empty() {
                    return Err("the Android target only compiles Kotlin (.kt) sources; \
                         move .java files to the JVM target or remove them from the \
                         Android source sets"
                        .to_owned());
                }

                let compile_classpath = merged_classpath(&config, ANDROID_SOURCE_SETS);

                let compose = config
                    .get("android")
                    .and_then(|a| a.get("compose"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let compose_compiler_jar = if compose {
                    Some(find_compose_compiler_jar(&compile_classpath).ok_or_else(|| {
                        "compose = true but the compose compiler plugin JAR was not found on the \
                         compile classpath; add a dependency such as \
                         \"org.jetbrains.kotlin:compose-compiler-plugin:<kotlin-version>\""
                            .to_owned()
                    })?)
                } else {
                    None
                };

                task_registrar::register_task(&Task {
                    name: "compileAndroid".to_owned(),
                    inputs: kotlin_sources.clone(),
                    outputs: vec![kmp_classes_dir.to_string_lossy().into_owned()],
                    depends_on: vec!["ulite/android:prepareBuildDir".to_owned()],
                    action: Action::RunTool(RunToolArgs {
                        tool: AllowlistedTool::Kotlinc,
                        args: kotlinc_android_args(
                            &kmp_classes_dir.to_string_lossy(),
                            &platform_jar.to_string_lossy(),
                            &compile_classpath,
                            &kotlin_sources,
                            compose_compiler_jar.as_deref(),
                        ),
                        cwd: ".".to_owned(),
                    }),
                })?;

                task_registrar::register_task(&Task {
                    name: "jarKmpAndroid".to_owned(),
                    inputs: vec![kmp_classes_dir.to_string_lossy().into_owned()],
                    outputs: vec![kmp_jar.to_string_lossy().into_owned()],
                    depends_on: vec!["compileAndroid".to_owned()],
                    action: Action::RunTool(RunToolArgs {
                        tool: AllowlistedTool::Jar,
                        args: jar_args(
                            &kmp_jar.to_string_lossy(),
                            &kmp_classes_dir.to_string_lossy(),
                        ),
                        cwd: ".".to_owned(),
                    }),
                })?;

                let variants = compute_variants(&config)?;
                let has_signing = config.get("signing").is_some();

                let mut d8_extra_jars: Vec<String> = Vec::new();
                for jar in &compile_classpath {
                    if jar.contains("kotlin-stdlib") {
                        d8_extra_jars.push(jar.clone());
                    }
                }

                let build_tools_dir = sdk_root
                    .join("build-tools")
                    .join(crate::find_build_tools_version(&sdk_root)?);
                let signing_config = if has_signing {
                    let signing = config.get("signing").unwrap();
                    let keystore = resolve_path(
                        project_dir,
                        signing
                            .get("storeFile")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                "the 'signing' block is missing 'storeFile'".to_owned()
                            })?,
                    );
                    let key_alias = signing
                        .get("keyAlias")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "the 'signing' block is missing 'keyAlias'".to_owned())?
                        .to_owned();
                    let ks_password_file =
                        std::path::Path::new(project_dir).join("build/android/ks-password.txt");
                    let key_password_file =
                        std::path::Path::new(project_dir).join("build/android/key-password.txt");
                    Some((keystore, key_alias, ks_password_file, key_password_file))
                } else {
                    None
                };

                for variant in &variants {
                    let variant_build = std::path::Path::new(project_dir)
                        .join("build/android")
                        .join(&variant.variant_dir);
                    let variant_classes_jar = variant_build.join("classes.jar");
                    let variant_apk = variant_build.join(&variant.apk_filename);
                    let merged_dex_dir =
                        android_build_dir.join(format!("dex-{}", variant.variant_dir));
                    let merged_dex = merged_dex_dir.join("classes.dex");

                    let package_task = format!("ulite/android:packageApk{}", variant.name);

                    task_registrar::register_task(&Task {
                        name: format!("mergeDex{}", variant.name),
                        inputs: {
                            let mut inp = vec![
                                variant_classes_jar.to_string_lossy().into_owned(),
                                kmp_jar.to_string_lossy().into_owned(),
                            ];
                            inp.extend(d8_extra_jars.iter().cloned());
                            inp
                        },
                        outputs: vec![merged_dex.to_string_lossy().into_owned()],
                        depends_on: vec![
                            "jarKmpAndroid".to_owned(),
                            format!("ulite/android:jarClasses{}", variant.name),
                        ],
                        action: Action::RunTool(RunToolArgs {
                            tool: AllowlistedTool::Java,
                            args: crate::d8_merge_args(
                                &sdk_root,
                                &[
                                    &variant_classes_jar.to_string_lossy(),
                                    &kmp_jar.to_string_lossy(),
                                ],
                                &merged_dex_dir.to_string_lossy(),
                                variant.min_sdk,
                                &platform_jar.to_string_lossy(),
                                &d8_extra_jars,
                            )?,
                            cwd: ".".to_owned(),
                        }),
                    })?;

                    task_registrar::register_task(&Task {
                        name: format!("assembleAndroid{}", variant.name),
                        inputs: vec![
                            merged_dex.to_string_lossy().into_owned(),
                            variant_apk.to_string_lossy().into_owned(),
                        ],
                        outputs: vec![variant_apk.to_string_lossy().into_owned()],
                        depends_on: vec![format!("mergeDex{}", variant.name), package_task],
                        action: Action::RunTool(RunToolArgs {
                            tool: AllowlistedTool::Jar,
                            args: vec![
                                "uf".to_owned(),
                                variant_apk.to_string_lossy().into_owned(),
                                "-C".to_owned(),
                                merged_dex_dir.to_string_lossy().into_owned(),
                                "classes.dex".to_owned(),
                            ],
                            cwd: ".".to_owned(),
                        }),
                    })?;

                    if let Some((ref keystore, ref key_alias, ref ks_pw, ref key_pw)) =
                        signing_config
                    {
                        task_registrar::register_task(&Task {
                            name: format!("signKmpAndroid{}", variant.name),
                            inputs: vec![
                                variant_apk.to_string_lossy().into_owned(),
                                keystore.clone(),
                                ks_pw.to_string_lossy().into_owned(),
                                key_pw.to_string_lossy().into_owned(),
                            ],
                            outputs: vec![variant_apk.to_string_lossy().into_owned()],
                            depends_on: vec![
                                format!("assembleAndroid{}", variant.name),
                                "ulite/android:writeSigningPasswords".to_owned(),
                                "ulite/android:writeSigningKeyPassword".to_owned(),
                            ],
                            action: Action::RunTool(RunToolArgs {
                                tool: AllowlistedTool::Apksigner,
                                args: vec![
                                    build_tools_dir.to_string_lossy().into_owned(),
                                    "sign".to_owned(),
                                    "--ks".to_owned(),
                                    keystore.clone(),
                                    "--ks-key-alias".to_owned(),
                                    key_alias.clone(),
                                    "--ks-pass".to_owned(),
                                    format!("file:{}", ks_pw.to_string_lossy()),
                                    "--key-pass".to_owned(),
                                    format!("file:{}", key_pw.to_string_lossy()),
                                    variant_apk.to_string_lossy().into_owned(),
                                ],
                                cwd: ".".to_owned(),
                            }),
                        })?;
                    }
                }
            }

            Ok(())
        }

        fn run(input: String) -> String {
            input
        }
    }

    #[cfg(target_arch = "wasm32")]
    export!(KmpPlugin);
}

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

fn resolve_paths(project_dir: &str, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|path| resolve_path(project_dir, path))
        .collect()
}

fn optional_string_list(block: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = block.get(key) else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| format!("the block's '{key}' is not a list"))?;
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

fn partition_sources(sources: &[String]) -> (Vec<String>, Vec<String>) {
    sources
        .iter()
        .cloned()
        .partition(|source| source.ends_with(".java"))
}

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

#[allow(dead_code)]
fn source_set_classpath(config: &serde_json::Value, path: &str) -> Vec<String> {
    source_set_classpath_bucket(config, path, "compile")
}

fn source_set_classpath_bucket(
    config: &serde_json::Value,
    path: &str,
    bucket: &str,
) -> Vec<String> {
    config
        .get("classpathSourceSets")
        .and_then(|sets| sets.get(path))
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

fn merged_classpath(config: &serde_json::Value, source_sets: &[&str]) -> Vec<String> {
    merged_classpath_bucket(config, source_sets, "compile")
}

fn merged_classpath_bucket(
    config: &serde_json::Value,
    source_sets: &[&str],
    bucket: &str,
) -> Vec<String> {
    let mut merged = Vec::new();
    for name in source_sets {
        for jar in source_set_classpath_bucket(config, &format!("kmp.{name}"), bucket) {
            if !merged.contains(&jar) {
                merged.push(jar);
            }
        }
    }
    merged
}

fn compile_args(classes_dir: &str, classpath: &[String], sources: &[String]) -> Vec<String> {
    let mut args = vec!["-d".to_owned(), classes_dir.to_owned()];
    if !classpath.is_empty() {
        args.extend(["-cp".to_owned(), classpath.join(":")]);
    }
    args.extend(sources.iter().cloned());
    args
}

fn find_compose_compiler_jar(classpath: &[String]) -> Option<String> {
    classpath
        .iter()
        .find(|p| p.contains("compose-compiler-plugin") && p.ends_with(".jar"))
        .cloned()
}

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
    args.extend(["-cp".to_owned(), cp.join(":")]);
    args.extend(["-jvm-target".to_owned(), "17".to_owned()]);
    if let Some(jar) = compose_compiler_jar {
        args.push(format!("-Xplugin={jar}"));
    }
    args.extend(sources.iter().cloned());
    args
}

fn jar_args(jar_file: &str, classes_dir: &str) -> Vec<String> {
    vec![
        "cf".to_owned(),
        jar_file.to_owned(),
        "-C".to_owned(),
        classes_dir.to_owned(),
        ".".to_owned(),
    ]
}

fn run_test_args(classpath: &[String], test_class: &str, extra_args: &[String]) -> Vec<String> {
    let mut invocation = vec!["-cp".to_owned(), classpath.join(":")];
    invocation.push(test_class.to_owned());
    invocation.extend(extra_args.iter().cloned());
    invocation
}

const TEST_RUNNER_SOURCE_PATH: &str = "build/generated-test-src/ulite/TestRunner.java";

const TEST_RUNNER_SOURCE: &str = r#"// Generated by the ulite/kmp plugin. Runs the tests compiled into the
// directory given on the command line through the JUnit Platform Launcher
// API; the engine(s) on the classpath are discovered automatically. Exits
// non-zero when any test failed or errored.
package ulite;

import java.io.File;
import java.io.PrintWriter;
import java.util.Collections;

import org.junit.platform.engine.discovery.DiscoverySelectors;
import org.junit.platform.launcher.Launcher;
import org.junit.platform.launcher.LauncherDiscoveryRequest;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;
import org.junit.platform.launcher.listeners.SummaryGeneratingListener;
import org.junit.platform.launcher.listeners.TestExecutionSummary;

public final class TestRunner {
    public static void main(String[] args) {
        if (args.length != 1) {
            System.err.println("usage: TestRunner <test-classes-dir>");
            System.exit(2);
        }
        File classesDir = new File(args[0]);
        LauncherDiscoveryRequest request = LauncherDiscoveryRequestBuilder.request()
                .selectors(DiscoverySelectors.selectClasspathRoots(
                        Collections.singleton(classesDir.toPath())))
                .build();
        Launcher launcher = LauncherFactory.create();
        SummaryGeneratingListener listener = new SummaryGeneratingListener();
        launcher.execute(request, listener);
        TestExecutionSummary summary = listener.getSummary();
        PrintWriter out = new PrintWriter(System.out, true);
        summary.printTo(out);
        summary.printFailuresTo(out);
        if (summary.getTotalFailureCount() > 0) {
            System.exit(1);
        }
    }
}
"#;

struct Variant {
    name: String,
    variant_dir: String,
    min_sdk: i64,
    apk_filename: String,
}

fn compute_variants(config: &serde_json::Value) -> Result<Vec<Variant>, String> {
    let android = config
        .get("android")
        .ok_or_else(|| "module config has no 'android' block".to_owned())?;
    let base_min_sdk = android
        .get("minSdk")
        .and_then(serde_json::Value::as_i64)
        .ok_or("the 'android' block is missing a numeric 'minSdk'")?;

    let build_type_names: Vec<String> = match config.get("buildTypes") {
        Some(bt) => bt
            .as_object()
            .ok_or_else(|| "'buildTypes' must be a block".to_owned())?
            .keys()
            .cloned()
            .collect(),
        None => vec!["debug".to_owned(), "release".to_owned()],
    };

    let flavors: std::collections::BTreeMap<String, ()> = match config.get("productFlavors") {
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
                let has_dimension = block_obj
                    .get("dimension")
                    .and_then(serde_json::Value::as_str)
                    .is_some();
                if !has_dimension && declared_dimensions.len() != 1 {
                    return Err(format!("flavor '{name}' is missing a 'dimension' key"));
                }
                map.insert(name.to_owned(), ());
            }
            map
        }
        None => std::collections::BTreeMap::new(),
    };

    let mut variants = Vec::new();

    let bt_min_sdk = |bt_name: &str| -> Option<i64> {
        config
            .get("buildTypes")
            .and_then(|bt| bt.get(bt_name))
            .and_then(|v| v.as_object())
            .and_then(|b| b.get("minSdk"))
            .and_then(serde_json::Value::as_i64)
    };

    let flavor_min_sdk = |flavor_name: &str| -> Option<i64> {
        config
            .get("productFlavors")
            .and_then(|pf| pf.get(flavor_name))
            .and_then(|v| v.as_object())
            .and_then(|b| b.get("minSdk"))
            .and_then(serde_json::Value::as_i64)
    };

    if flavors.is_empty() {
        for bt in &build_type_names {
            let name = to_pascal_case(bt);
            let variant_dir = to_pascal_case(bt).to_lowercase();
            let apk_filename = format!("app-{variant_dir}.apk");
            let effective_min_sdk = bt_min_sdk(bt).unwrap_or(base_min_sdk);
            variants.push(Variant {
                name,
                variant_dir,
                min_sdk: effective_min_sdk,
                apk_filename,
            });
        }
    } else {
        for bt in &build_type_names {
            for flavor_name in flavors.keys() {
                let variant_name = format!("{}{}", to_pascal_case(bt), to_pascal_case(flavor_name));
                let variant_dir = format!(
                    "{}{}",
                    to_pascal_case(bt).to_lowercase(),
                    to_pascal_case(flavor_name)
                );
                let apk_filename = format!("app-{}.apk", variant_dir);
                let effective_min_sdk = flavor_min_sdk(flavor_name)
                    .or_else(|| bt_min_sdk(bt))
                    .unwrap_or(base_min_sdk);
                variants.push(Variant {
                    name: variant_name,
                    variant_dir,
                    min_sdk: effective_min_sdk,
                    apk_filename,
                });
            }
        }
    }

    Ok(variants)
}

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

fn resolve_sdk_root(
    config: &serde_json::Value,
    project_dir: &str,
) -> Result<std::path::PathBuf, String> {
    if let Some(sdk_dir) = config
        .get("android")
        .and_then(|a| a.get("sdkDir"))
        .and_then(serde_json::Value::as_str)
    {
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

fn d8_merge_args(
    sdk_root: &std::path::Path,
    input_jars: &[&str],
    output_dir: &str,
    min_sdk: i64,
    platform_jar: &str,
    extra_jars: &[String],
) -> Result<Vec<String>, String> {
    let version = find_build_tools_version(sdk_root)?;
    let d8_jar = sdk_root
        .join("build-tools")
        .join(version)
        .join("lib")
        .join("d8.jar");

    let mut args = vec![
        "-cp".to_owned(),
        d8_jar.to_string_lossy().into_owned(),
        "com.android.tools.r8.D8".to_owned(),
        format!("--min-api={min_sdk}"),
        "--lib".to_owned(),
        platform_jar.to_owned(),
        format!("--output={output_dir}"),
    ];
    for jar in input_jars {
        args.push(jar.to_string());
    }
    for jar in extra_jars {
        args.push(jar.clone());
    }
    Ok(args)
}

fn find_build_tools_version(sdk_root: &std::path::Path) -> Result<String, String> {
    let build_tools = sdk_root.join("build-tools");
    let mut candidates: Vec<(Vec<u64>, String)> = Vec::new();
    let entries = std::fs::read_dir(&build_tools)
        .map_err(|error| format!("cannot list '{}': {error}", build_tools.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read '{}': {error}", build_tools.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rank: Vec<u64> = name.split('.').filter_map(|p| p.parse().ok()).collect();
        if rank.len() == 3 {
            candidates.push((rank, name));
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(rank, _)| rank.clone())
        .map(|(_, name)| name)
        .ok_or_else(|| {
            format!(
                "no build-tools version found under '{}'; install the Android SDK build-tools",
                build_tools.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_invocation_carries_classpath_and_sources() {
        let args = compile_args(
            "/proj/build/classes",
            &["/repos/one.jar".to_owned(), "/repos/two.jar".to_owned()],
            &[
                "/proj/src/commonMain".to_owned(),
                "/proj/src/jvmMain".to_owned(),
            ],
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/repos/one.jar:/repos/two.jar".to_owned(),
                "/proj/src/commonMain".to_owned(),
                "/proj/src/jvmMain".to_owned(),
            ]
        );
    }

    #[test]
    fn compiler_invocation_omits_cp_for_an_empty_classpath() {
        let args = compile_args(
            "/proj/build/classes",
            &[],
            &["/proj/src/jvmMain".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "/proj/src/jvmMain".to_owned(),
            ]
        );
    }

    #[test]
    fn jar_invocation_packs_the_classes_directory() {
        assert_eq!(
            jar_args("/proj/build/app.jar", "/proj/build/classes"),
            vec![
                "cf".to_owned(),
                "/proj/build/app.jar".to_owned(),
                "-C".to_owned(),
                "/proj/build/classes".to_owned(),
                ".".to_owned(),
            ]
        );
    }

    #[test]
    fn relative_block_paths_resolve_against_the_project_dir() {
        assert_eq!(
            resolve_path("/proj", "src/commonMain"),
            "/proj/src/commonMain"
        );
        assert_eq!(resolve_path("/proj", "/abs/commonMain"), "/abs/commonMain");
    }

    #[test]
    fn sources_partition_by_extension() {
        let sources = vec![
            "/proj/src/commonMain/Shared.kt".to_owned(),
            "/proj/src/jvmMain/JvmApp.java".to_owned(),
            "/proj/src/commonMain/Util.kt".to_owned(),
        ];
        let (java, kotlin) = partition_sources(&sources);
        assert_eq!(java, vec!["/proj/src/jvmMain/JvmApp.java".to_owned()]);
        assert_eq!(
            kotlin,
            vec![
                "/proj/src/commonMain/Shared.kt".to_owned(),
                "/proj/src/commonMain/Util.kt".to_owned(),
            ]
        );
    }

    #[test]
    fn unknown_source_extensions_are_rejected() {
        assert!(reject_unknown_extensions(&["/proj/A.java".to_owned()]).is_ok());
        assert!(reject_unknown_extensions(&["/proj/A.kt".to_owned()]).is_ok());
        assert!(reject_unknown_extensions(&["/proj/A.txt".to_owned()]).is_err());
    }

    #[test]
    fn optional_string_list_reads_and_errors() {
        let block = serde_json::json!({ "sources": ["a.kt", "b.kt"] });
        assert_eq!(
            optional_string_list(&block, "sources").expect("parses"),
            vec!["a.kt".to_owned(), "b.kt".to_owned()]
        );
        assert!(
            optional_string_list(&block, "missing")
                .expect("missing is empty")
                .is_empty()
        );
        assert!(
            optional_string_list(&serde_json::json!({ "sources": "a.kt" }), "sources").is_err()
        );
    }

    #[test]
    fn source_set_classpath_reads_a_resolved_source_set() {
        let config = serde_json::json!({
            "classpathSourceSets": {
                "kmp.commonMain": { "compile": ["/repos/shared.jar"] },
            },
        });
        assert_eq!(
            source_set_classpath(&config, "kmp.commonMain"),
            vec!["/repos/shared.jar".to_owned()]
        );
        assert!(source_set_classpath(&config, "kmp.jvmMain").is_empty());
        assert!(source_set_classpath(&serde_json::json!({}), "kmp.commonMain").is_empty());
    }

    #[test]
    fn target_classpath_unions_source_sets_in_hierarchy_order() {
        let config = serde_json::json!({
            "classpathSourceSets": {
                "kmp.commonMain": { "compile": ["/repos/shared.jar", "/repos/other.jar"] },
                "kmp.jvmMain": { "compile": ["/repos/jvm.jar", "/repos/shared.jar"] },
            },
        });
        assert_eq!(
            merged_classpath(&config, &["commonMain", "jvmMain"]),
            vec![
                "/repos/shared.jar".to_owned(),
                "/repos/other.jar".to_owned(),
                "/repos/jvm.jar".to_owned(),
            ]
        );
    }

    #[test]
    fn compute_variants_defaults_to_debug_and_release() {
        let config = serde_json::json!({
            "android": { "compileSdk": 34, "minSdk": 24 }
        });
        let variants = compute_variants(&config).unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].name, "Debug");
        assert_eq!(variants[0].variant_dir, "debug");
        assert_eq!(variants[0].apk_filename, "app-debug.apk");
        assert_eq!(variants[1].name, "Release");
        assert_eq!(variants[1].variant_dir, "release");
        assert_eq!(variants[1].apk_filename, "app-release.apk");
    }

    #[test]
    fn kotlinc_android_includes_platform_jar_and_jvm_target() {
        let args = kotlinc_android_args(
            "/proj/build/classes",
            "/sdk/platforms/android-34/android.jar",
            &["/repos/lib.jar".to_owned()],
            &["/proj/src/Foo.kt".to_owned()],
            None,
        );
        assert!(args.contains(&"-d".to_owned()));
        assert!(args.contains(&"/proj/build/classes".to_owned()));
        assert!(args.contains(&"-cp".to_owned()));
        assert!(args.contains(&"-jvm-target".to_owned()));
        assert!(args.contains(&"17".to_owned()));
        assert!(args.contains(&"/proj/src/Foo.kt".to_owned()));
        assert!(!args.iter().any(|a| a.starts_with("-Xplugin=")));
    }

    #[test]
    fn kotlinc_android_loads_compose_plugin_via_xplugin() {
        let args = kotlinc_android_args(
            "/proj/build/classes",
            "/sdk/platforms/android-34/android.jar",
            &[],
            &["/proj/src/Foo.kt".to_owned()],
            Some("/maven/compose-compiler-plugin-2.0.jar"),
        );
        assert!(
            args.contains(&"-Xplugin=/maven/compose-compiler-plugin-2.0.jar".to_owned()),
            "compose JAR must be loaded via -Xplugin=<path>, got: {args:?}"
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
    fn to_pascal_case_handles_variants() {
        assert_eq!(to_pascal_case("debug"), "Debug");
        assert_eq!(to_pascal_case("release"), "Release");
        assert_eq!(to_pascal_case("free"), "Free");
    }

    #[test]
    fn find_build_tools_version_errs_when_sdk_has_no_build_tools() {
        let dir = std::env::temp_dir().join("ulb-test-no-buildtools");
        let bt = dir.join("build-tools");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&bt).unwrap();
        let err = find_build_tools_version(&dir).unwrap_err();
        assert!(
            err.contains("no build-tools version found"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_build_tools_version_picks_highest_version() {
        let dir = std::env::temp_dir().join("ulb-test-buildtools");
        let bt = dir.join("build-tools");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(bt.join("35.0.0")).unwrap();
        std::fs::create_dir_all(bt.join("36.0.1")).unwrap();
        std::fs::create_dir_all(bt.join("36.0.0")).unwrap();
        assert_eq!(find_build_tools_version(&dir).unwrap(), "36.0.1".to_owned());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_variants_respects_build_type_min_sdk_override() {
        let config = serde_json::json!({
            "android": { "compileSdk": 34, "minSdk": 21 },
            "buildTypes": {
                "debug": {},
                "release": { "minSdk": 24 }
            }
        });
        let variants = compute_variants(&config).unwrap();
        let debug = variants.iter().find(|v| v.name == "Debug").unwrap();
        let release = variants.iter().find(|v| v.name == "Release").unwrap();
        assert_eq!(debug.min_sdk, 21);
        assert_eq!(release.min_sdk, 24);
    }

    #[test]
    fn compute_variants_flavor_min_sdk_takes_precedence() {
        let config = serde_json::json!({
            "android": { "compileSdk": 34, "minSdk": 21 },
            "buildTypes": {
                "release": { "minSdk": 24 }
            },
            "productFlavors": {
                "dimension": "tier",
                "free": { "dimension": "tier" },
                "pro": { "dimension": "tier", "minSdk": 28 }
            }
        });
        let variants = compute_variants(&config).unwrap();
        let free_release = variants.iter().find(|v| v.name == "ReleaseFree").unwrap();
        let pro_release = variants.iter().find(|v| v.name == "ReleasePro").unwrap();
        assert_eq!(free_release.min_sdk, 24);
        assert_eq!(pro_release.min_sdk, 28);
    }

    #[test]
    fn source_set_classpath_bucket_reads_any_bucket() {
        let config = serde_json::json!({
            "classpathSourceSets": {
                "kmp.commonMain": {
                    "compile": ["/repos/compile.jar"],
                    "testCompile": ["/repos/test-compile.jar"],
                    "testRuntime": ["/repos/test-runtime.jar"],
                    "runtime": ["/repos/runtime.jar"]
                }
            }
        });
        assert_eq!(
            source_set_classpath_bucket(&config, "kmp.commonMain", "compile"),
            vec!["/repos/compile.jar".to_owned()]
        );
        assert_eq!(
            source_set_classpath_bucket(&config, "kmp.commonMain", "testCompile"),
            vec!["/repos/test-compile.jar".to_owned()]
        );
        assert_eq!(
            source_set_classpath_bucket(&config, "kmp.commonMain", "testRuntime"),
            vec!["/repos/test-runtime.jar".to_owned()]
        );
        assert_eq!(
            source_set_classpath_bucket(&config, "kmp.commonMain", "runtime"),
            vec!["/repos/runtime.jar".to_owned()]
        );
        assert!(source_set_classpath_bucket(&config, "kmp.jvmMain", "compile").is_empty());
    }

    #[test]
    fn merged_classpath_bucket_unions_across_source_sets() {
        let config = serde_json::json!({
            "classpathSourceSets": {
                "kmp.commonTest": { "testCompile": ["/repos/shared.jar", "/repos/other.jar"] },
                "kmp.jvmTest": { "testCompile": ["/repos/jvm-test.jar", "/repos/shared.jar"] }
            }
        });
        assert_eq!(
            merged_classpath_bucket(&config, &["commonTest", "jvmTest"], "testCompile"),
            vec![
                "/repos/shared.jar".to_owned(),
                "/repos/other.jar".to_owned(),
                "/repos/jvm-test.jar".to_owned(),
            ]
        );
    }

    #[test]
    fn run_test_args_builds_cp_and_class() {
        let args = run_test_args(
            &[
                "/repos/junit.jar".to_owned(),
                "/proj/build/test-classes".to_owned(),
            ],
            "com.example.AppTest",
            &[],
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/repos/junit.jar:/proj/build/test-classes".to_owned(),
                "com.example.AppTest".to_owned(),
            ]
        );
    }

    #[test]
    fn run_test_args_appends_extra_args() {
        let args = run_test_args(
            &["/repos/junit.jar".to_owned()],
            "org.junit.runner.JUnitCore",
            &["com.example.AppTest".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/repos/junit.jar".to_owned(),
                "org.junit.runner.JUnitCore".to_owned(),
                "com.example.AppTest".to_owned(),
            ]
        );
    }

    #[test]
    fn run_test_args_for_generated_runner() {
        let args = run_test_args(
            &[
                "/repos/junit.jar".to_owned(),
                "/proj/build/test-classes".to_owned(),
                "/proj/build/classes".to_owned(),
            ],
            "ulite.TestRunner",
            &["/proj/build/test-classes".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/repos/junit.jar:/proj/build/test-classes:/proj/build/classes".to_owned(),
                "ulite.TestRunner".to_owned(),
                "/proj/build/test-classes".to_owned(),
            ]
        );
    }

    #[test]
    fn generated_runner_source_mentions_launcher_api() {
        assert!(TEST_RUNNER_SOURCE.contains("LauncherFactory"));
        assert!(TEST_RUNNER_SOURCE.contains("DiscoverySelectors"));
        assert!(TEST_RUNNER_SOURCE.contains("SummaryGeneratingListener"));
    }
}
