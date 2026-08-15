//! The `ulite/jvm` plugin: compiles a plain-Java or Kotlin module,
//! packages it into a jar, and optionally compiles and runs its tests.
//!
//! The module's `jvm {}` block describes the sources and artifacts; the
//! host resolves the module's `deps {}` block into compile/test classpaths
//! and injects them, along with the project directory, into this plugin's
//! configuration. `configure` turns all of that into tasks: `compile` runs
//! javac over `.java` sources, `compile-kotlin` runs kotlinc over `.kt`
//! sources (after `compile` when both coexist), `assemble` runs
//! `jar cf <jarFile> -C <classesDir> .` after the compilers, and a
//! `testSources`/`testClassesDir`/`testClass` trio adds `compile-tests`
//! (javac over the test sources against the test-compile classpath) and
//! `test` (`java -cp <testRuntime>:<testClassesDir>:<classesDir> <class>`).
//! Paths written into the module block are resolved against the injected
//! `projectDir`, so a build succeeds regardless of the directory the host
//! was invoked from. Task inputs and outputs are the source files and the
//! produced classes/jar, so the host's fingerprinting leaves a task alone
//! until one of its inputs or dependencies changes. Consumed keys are
//! documented in `REFERENCE.md` (ARCHITECTURE.md §5.1).

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
        compile_args, jar_args, partition_sources, reject_unknown_extensions, resolve_path,
        run_test_args, string_list,
    };
    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};
    use serde_json::Value;
    use ulite::ulb::task_registrar::{self, Action, AllowlistedTool, RunToolArgs, Task};

    /// Implements the exported `ulb-plugin` interface.
    struct JvmPlugin;

    impl Guest for JvmPlugin {
        fn manifest() -> PluginManifest {
            PluginManifest {
                name: "ulite/jvm".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                abi_version: ulb_plugin_sdk::ABI_VERSION.to_string(),
                // Every run-tool task below uses one of these tools.
                tools: vec![
                    "javac".to_string(),
                    "kotlinc".to_string(),
                    "jar".to_string(),
                    "java".to_string(),
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
            let jvm = config
                .get("jvm")
                .ok_or_else(|| "module config has no 'jvm' block".to_owned())?;

            let sources = string_list(jvm, "sources")?;
            if sources.is_empty() {
                return Err("the 'jvm' block declares no sources".to_owned());
            }
            let sources = resolve_paths(project_dir, &sources);
            reject_unknown_extensions(&sources)?;
            let (java_sources, kotlin_sources) = partition_sources(&sources);

            let classes_dir = resolve_path(
                project_dir,
                jvm.get("classesDir")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "the 'jvm' block is missing a 'classesDir' string".to_owned())?,
            );
            let jar_file = resolve_path(
                project_dir,
                jvm.get("jarFile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "the 'jvm' block is missing a 'jarFile' string".to_owned())?,
            );

            // The host already resolved the module's `deps {}` block into
            // jar paths; the plugin only decides how they reach the tools.
            let compile_classpath = classpath_bucket(&config, "compile");

            // Main compilation: one javac task for the java sources, one
            // kotlinc task for the kotlin sources. Kotlin can see the java
            // classes, so the kotlin task waits for the java one when both
            // source sets exist.
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
                // The kotlin compiler resolves the module's own java classes
                // the same way it would resolve a dependency jar, so the
                // classes dir joins the compile classpath.
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

            register_tests(jvm, project_dir, &config, &classes_dir, &compile_tasks)?;

            Ok(())
        }

        fn run(input: String) -> String {
            input
        }
    }

    /// Registers the `compile-tests` and `test` tasks when the module block
    /// carries a test suite. The three keys must be set together: the test
    /// sources, the directory their classes land in, and the fully-qualified
    /// class with a `main` that `java` runs. Test compilation reuses the
    /// main compile's tasks as a dependency so a changed main class forces
    /// the tests to recompile, and the run task depends on that compilation.
    fn register_tests(
        jvm: &Value,
        project_dir: &str,
        config: &Value,
        classes_dir: &str,
        compile_tasks: &[String],
    ) -> Result<(), String> {
        let test_sources = jvm.get("testSources");
        let test_classes_dir = jvm.get("testClassesDir").and_then(Value::as_str);
        let test_class = jvm.get("testClass").and_then(Value::as_str);
        let test_keys_present = usize::from(test_sources.is_some())
            + usize::from(test_classes_dir.is_some())
            + usize::from(test_class.is_some());
        if test_keys_present != 0 && test_keys_present != 3 {
            return Err(
                "the 'jvm' block must set testSources, testClassesDir, and testClass together"
                    .to_owned(),
            );
        }
        let (Some(_), Some(test_classes_dir), Some(test_class)) =
            (test_sources, test_classes_dir, test_class)
        else {
            return Ok(());
        };

        let test_sources = resolve_paths(project_dir, &string_list(jvm, "testSources")?);
        if test_sources.is_empty() {
            return Err("the 'jvm' block declares no test sources".to_owned());
        }
        reject_unknown_extensions(&test_sources)?;
        for source in &test_sources {
            if !source.ends_with(".java") {
                return Err(format!(
                    "test source '{source}' is not a .java file; kotlin test compilation \
                     is not supported"
                ));
            }
        }

        let test_classes_dir = resolve_path(project_dir, test_classes_dir);
        // The test compiler sees the app classes and the test-compile
        // classpath; the test run sees those plus the runtime classpath.
        let mut test_compile_classpath = classpath_bucket(config, "testCompile");
        test_compile_classpath.push(classes_dir.to_owned());
        let mut test_runtime_classpath = classpath_bucket(config, "testRuntime");
        test_runtime_classpath.push(test_classes_dir.clone());
        test_runtime_classpath.push(classes_dir.to_owned());

        task_registrar::register_task(&Task {
            name: "compile-tests".to_owned(),
            inputs: test_sources.clone(),
            outputs: vec![test_classes_dir.clone()],
            depends_on: compile_tasks.to_vec(),
            action: Action::RunTool(RunToolArgs {
                tool: AllowlistedTool::Javac,
                args: compile_args(&test_classes_dir, &test_compile_classpath, &test_sources),
                cwd: ".".to_owned(),
            }),
        })?;

        task_registrar::register_task(&Task {
            name: "test".to_owned(),
            inputs: vec![test_classes_dir.clone(), classes_dir.to_owned()],
            outputs: Vec::new(),
            depends_on: vec!["compile-tests".to_owned()],
            action: Action::RunTool(RunToolArgs {
                tool: AllowlistedTool::Java,
                args: run_test_args(&test_runtime_classpath, test_class),
                cwd: ".".to_owned(),
            }),
        })?;

        Ok(())
    }

    fn resolve_paths(project_dir: &str, paths: &[String]) -> Vec<String> {
        paths
            .iter()
            .map(|path| resolve_path(project_dir, path))
            .collect()
    }

    /// The host-resolved jar list of one classpath bucket from the module
    /// configuration (`compile`, `testCompile`, `testRuntime`, ...).
    fn classpath_bucket(config: &Value, bucket: &str) -> Vec<String> {
        config
            .get("classpath")
            .and_then(|classpath| classpath.get(bucket))
            .and_then(Value::as_array)
            .map(|jars| {
                jars.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default()
    }

    // The export generates wasm component symbols (`export_name` with a
    // component-model name), which only link on the wasm32-wasip2 target.
    #[cfg(target_arch = "wasm32")]
    export!(JvmPlugin);
}

/// Reads a string-list key of the module block, erroring when the key is
/// absent or holds a non-string entry.
fn string_list(jvm: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    let entries = jvm
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("the 'jvm' block is missing a '{key}' list"))?;
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

/// Rejects a source file the compilers would not understand, so a typo in
/// the block surfaces as a configure error instead of a javac/kotlinc run.
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

/// Resolves a module-block path against the project directory; an
/// absolute path passes through untouched.
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

/// Splits the module's sources into `.java` files and everything else
/// (the `.kt` files), preserving order within each half.
fn partition_sources(sources: &[String]) -> (Vec<String>, Vec<String>) {
    sources
        .iter()
        .cloned()
        .partition(|source| source.ends_with(".java"))
}

/// The compiler invocation for a compile task: emit classes to `-d`, feed
/// the resolved classpath to `-cp` when one exists (so an empty classpath
/// keeps the compiler's own defaults), then the sources. Shared by the
/// javac and kotlinc tasks; kotlinc accepts the same flag shape.
fn compile_args(classes_dir: &str, classpath: &[String], sources: &[String]) -> Vec<String> {
    let mut args = vec!["-d".to_owned(), classes_dir.to_owned()];
    if !classpath.is_empty() {
        args.extend(["-cp".to_owned(), classpath.join(":")]);
    }
    args.extend(sources.iter().cloned());
    args
}

/// The `jar` invocation for an assemble task: create the jar and pack
/// the classes directory.
fn jar_args(jar_file: &str, classes_dir: &str) -> Vec<String> {
    vec![
        "cf".to_owned(),
        jar_file.to_owned(),
        "-C".to_owned(),
        classes_dir.to_owned(),
        ".".to_owned(),
    ]
}

/// The `java` invocation for a test task: run the test class with the
/// runtime classpath (test classes, app classes, and the resolved
/// test-runtime jars, in that order).
fn run_test_args(classpath: &[String], test_class: &str) -> Vec<String> {
    vec!["-cp".to_owned(), classpath.join(":")]
        .into_iter()
        .chain(std::iter::once(test_class.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        compile_args, jar_args, partition_sources, reject_unknown_extensions, resolve_path,
        run_test_args, string_list,
    };

    #[test]
    fn compiler_invocation_carries_classpath_and_sources() {
        let args = compile_args(
            "/proj/build/classes",
            &["/repos/one.jar".to_owned(), "/repos/two.jar".to_owned()],
            &["/proj/src/App.java".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "-cp".to_owned(),
                "/repos/one.jar:/repos/two.jar".to_owned(),
                "/proj/src/App.java".to_owned(),
            ]
        );
    }

    #[test]
    fn compiler_invocation_omits_cp_for_an_empty_classpath() {
        let args = compile_args(
            "/proj/build/classes",
            &[],
            &["/proj/src/App.java".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                "/proj/build/classes".to_owned(),
                "/proj/src/App.java".to_owned(),
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
    fn test_run_invocation_prepends_the_classpath_and_names_the_class() {
        let args = run_test_args(
            &[
                "/repos/junit.jar".to_owned(),
                "/proj/build/test-classes".to_owned(),
                "/proj/build/classes".to_owned(),
            ],
            "com.example.AppTest",
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/repos/junit.jar:/proj/build/test-classes:/proj/build/classes".to_owned(),
                "com.example.AppTest".to_owned(),
            ]
        );
    }

    #[test]
    fn relative_block_paths_resolve_against_the_project_dir() {
        assert_eq!(resolve_path("/proj", "src/App.java"), "/proj/src/App.java");
        assert_eq!(resolve_path("/proj", "/abs/App.java"), "/abs/App.java");
    }

    #[test]
    fn sources_partition_by_extension() {
        let sources = vec![
            "/proj/src/Main.kt".to_owned(),
            "/proj/src/App.java".to_owned(),
            "/proj/src/Util.kt".to_owned(),
        ];
        let (java, kotlin) = partition_sources(&sources);
        assert_eq!(java, vec!["/proj/src/App.java".to_owned()]);
        assert_eq!(
            kotlin,
            vec![
                "/proj/src/Main.kt".to_owned(),
                "/proj/src/Util.kt".to_owned(),
            ]
        );
    }

    #[test]
    fn unknown_source_extensions_are_rejected() {
        assert!(reject_unknown_extensions(&["/proj/src/App.txt".to_owned()]).is_err());
        assert!(reject_unknown_extensions(&["/proj/src/App.java".to_owned()]).is_ok());
    }

    #[test]
    fn string_list_reads_and_errors() {
        let block = serde_json::json!({ "sources": ["a.java", "b.java"] });
        assert_eq!(
            string_list(&block, "sources").expect("parses"),
            vec!["a.java".to_owned(), "b.java".to_owned()]
        );
        assert!(string_list(&block, "missing").is_err());
    }
}
