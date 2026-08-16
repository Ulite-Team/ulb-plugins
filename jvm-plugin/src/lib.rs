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
//! `testSources`/`testClassesDir` pair plus either a `testClass` or a
//! `testRunner` adds `compile-tests` (javac over the test sources against
//! the test-compile classpath) and `test` (a `java` run of the named
//! runner). With `testRunner = "junit-platform"` the plugin generates a
//! JUnit Platform Launcher-API runner over the test classes directory, so
//! no console-standalone jar or explicit class list is needed. A module
//! whose `deps {}` block carries `ksp` declarations gets a `ksp` task that
//! runs the KSP compiler-command-line tool against the module's Kotlin
//! sources, feeding the generated sources into the compile tasks and
//! ordering generate → compile → package. Paths
//! written into the module block are resolved against the injected
//! `projectDir`, so a build succeeds regardless of the directory the host
//! was invoked from. Task inputs and outputs are the source files and the
//! produced classes/jar, so the host's fingerprinting leaves a task alone
//! until one of its inputs or dependencies changes. Consumed keys are
//! documented in `docs/jvm-plugin.md` (Uliab/docs/architecture.md §5.1).

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
        TEST_RUNNER_SOURCE, TEST_RUNNER_SOURCE_PATH, compile_args, jar_args, ksp_args,
        ksp_output_dirs, partition_sources, reject_unknown_extensions, resolve_path, run_test_args,
        string_list,
    };
    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};
    use serde_json::Value;
    use ulite::ulb::task_registrar::{
        self, Action, AllowlistedTool, RunToolArgs, Task, WriteFileArgs,
    };

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
            let processor_classpath = classpath_bucket(&config, "processor");

            // KSP processes the module's kotlin sources before either
            // compiler runs. Its runner and the processors come from the
            // `processor` bucket, which holds the `ksp` declarations, so a
            // module with ksp deps must also have kotlin sources.
            let mut ksp_generated = None;
            if !processor_classpath.is_empty() {
                if kotlin_sources.is_empty() {
                    return Err("the module declares ksp deps but has no .kt sources".to_owned());
                }
                let (kotlin_out, java_out) = ksp_output_dirs(project_dir);
                task_registrar::register_task(&Task {
                    name: "ksp".to_owned(),
                    inputs: kotlin_sources.clone(),
                    outputs: vec![kotlin_out.clone(), java_out.clone()],
                    depends_on: Vec::new(),
                    action: Action::RunTool(RunToolArgs {
                        tool: AllowlistedTool::Java,
                        args: ksp_args(
                            project_dir,
                            &kotlin_sources,
                            &compile_classpath,
                            &processor_classpath,
                        ),
                        cwd: ".".to_owned(),
                    }),
                })?;
                ksp_generated = Some((kotlin_out, java_out));
            }

            // Main compilation: one javac task for the module's own java
            // sources, one kotlinc task for the kotlin sources. Kotlin can
            // see the java classes, so the kotlin task waits for the java
            // one when both source sets exist; when ksp ran, its generated
            // kotlin directory joins the kotlinc task's source list (the
            // generated java directory stays a ksp output: kotlinc emits no
            // classes for `.java` sources, and javac requires an explicit
            // file list that a task's static inputs cannot enumerate after
            // ksp runs). Both compilers wait for ksp to finish generating.
            let mut compile_tasks = Vec::new();
            if !java_sources.is_empty() {
                let mut depends_on = Vec::new();
                if ksp_generated.is_some() {
                    depends_on.push("ksp".to_owned());
                }
                task_registrar::register_task(&Task {
                    name: "compile".to_owned(),
                    inputs: java_sources.clone(),
                    outputs: vec![classes_dir.clone()],
                    depends_on,
                    action: Action::RunTool(RunToolArgs {
                        tool: AllowlistedTool::Javac,
                        args: compile_args(&classes_dir, &compile_classpath, &java_sources),
                        cwd: ".".to_owned(),
                    }),
                })?;
                compile_tasks.push("compile".to_owned());
            }
            if !kotlin_sources.is_empty() {
                let mut kotlinc_sources = kotlin_sources.clone();
                let mut depends_on = Vec::new();
                if !java_sources.is_empty() {
                    depends_on.push("compile".to_owned());
                }
                if let Some((kotlin_out, _)) = &ksp_generated {
                    kotlinc_sources.push(kotlin_out.clone());
                    depends_on.push("ksp".to_owned());
                }
                // The kotlin compiler resolves the module's own java classes
                // the same way it would resolve a dependency jar, so the
                // classes dir joins the compile classpath.
                let mut kotlin_classpath = compile_classpath.clone();
                kotlin_classpath.push(classes_dir.clone());
                task_registrar::register_task(&Task {
                    name: "compile-kotlin".to_owned(),
                    inputs: kotlinc_sources.clone(),
                    outputs: vec![classes_dir.clone()],
                    depends_on,
                    action: Action::RunTool(RunToolArgs {
                        tool: AllowlistedTool::Kotlinc,
                        args: compile_args(&classes_dir, &kotlin_classpath, &kotlinc_sources),
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
    /// carries a test suite. `testSources` and `testClassesDir` stand or
    /// fall together, and the run target is exactly one of two forms:
    /// `testClass` names a class with a `main` that `java` runs (an
    /// optional `testArgs` list follows it, so the class can be a
    /// framework runner such as JUnitCore), or `testRunner = "junit-platform"`
    /// makes the plugin write [`TEST_RUNNER_SOURCE`] into the project and
    /// run it instead, which discovers the tests by scanning the test
    /// classes directory and needs neither a class list nor a
    /// console-standalone jar. Test compilation reuses the main compile's
    /// tasks as a dependency so a changed main class forces the tests to
    /// recompile, and the run task depends on that compilation.
    fn register_tests(
        jvm: &Value,
        project_dir: &str,
        config: &Value,
        classes_dir: &str,
        compile_tasks: &[String],
    ) -> Result<(), String> {
        let test_sources = jvm.get("testSources");
        let test_classes_dir = jvm.get("testClassesDir").and_then(Value::as_str);
        if test_sources.is_none() != test_classes_dir.is_none() {
            return Err(
                "the 'jvm' block must set testSources and testClassesDir together".to_owned(),
            );
        }
        let Some(test_classes_dir) = test_classes_dir else {
            return Ok(());
        };

        let test_class = jvm.get("testClass").and_then(Value::as_str);
        let test_runner = jvm.get("testRunner").and_then(Value::as_str);
        if test_class.is_some() && test_runner.is_some() {
            return Err(
                "the 'jvm' block sets both testClass and testRunner; choose one".to_owned(),
            );
        }
        if test_class.is_none() && test_runner.is_none() {
            return Err(
                "the 'jvm' block sets testSources without a testClass or testRunner".to_owned(),
            );
        }

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

        let mut compile_sources = test_sources.clone();
        let mut compile_tests_depends = compile_tasks.to_vec();
        let test_run_args;
        if let Some(test_runner) = test_runner {
            if test_runner != "junit-platform" {
                return Err(format!(
                    "unsupported testRunner '{test_runner}'; the supported value is \
                     'junit-platform'"
                ));
            }
            let generated_source = resolve_path(project_dir, TEST_RUNNER_SOURCE_PATH);
            task_registrar::register_task(&Task {
                name: "generate-test-runner".to_owned(),
                inputs: Vec::new(),
                outputs: vec![generated_source.clone()],
                depends_on: Vec::new(),
                action: Action::WriteFile(WriteFileArgs {
                    path: generated_source.clone(),
                    contents: TEST_RUNNER_SOURCE.to_owned(),
                }),
            })?;
            compile_sources.push(generated_source.clone());
            compile_tests_depends.push("generate-test-runner".to_owned());
            test_run_args = run_test_args(
                &test_runtime_classpath,
                "ulite.TestRunner",
                std::slice::from_ref(&test_classes_dir),
            );
        } else {
            let test_class = test_class.expect("validated above");
            // Extra arguments passed to the runner after the class name, so
            // a framework main (JUnitCore, the JUnit Platform console
            // launcher) can receive the classes it should execute.
            let test_args = optional_string_list(jvm, "testArgs")?;
            test_run_args = run_test_args(&test_runtime_classpath, test_class, &test_args);
        }

        task_registrar::register_task(&Task {
            name: "compile-tests".to_owned(),
            inputs: compile_sources.clone(),
            outputs: vec![test_classes_dir.clone()],
            depends_on: compile_tests_depends,
            action: Action::RunTool(RunToolArgs {
                tool: AllowlistedTool::Javac,
                args: compile_args(&test_classes_dir, &test_compile_classpath, &compile_sources),
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
                args: test_run_args,
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

    /// Reads an optional string-list key of the module block; a missing key
    /// is an empty list, a present non-list or non-string entry is an error.
    fn optional_string_list(jvm: &Value, key: &str) -> Result<Vec<String>, String> {
        let Some(value) = jvm.get(key) else {
            return Ok(Vec::new());
        };
        let entries = value
            .as_array()
            .ok_or_else(|| format!("the 'jvm' block's '{key}' is not a list"))?;
        entries
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("a 'jvm.{key}' entry is not a string"))
            })
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

/// Where the generated JUnit Platform runner lands, relative to the
/// injected project directory.
const TEST_RUNNER_SOURCE_PATH: &str = "build/generated-test-src/ulite/TestRunner.java";

/// The source of the runner the `testRunner = "junit-platform"` mode
/// generates. It drives the JUnit Platform Launcher API directly: it scans
/// the test classes directory passed on the command line, executes whatever
/// engine is on the classpath (Jupiter, Vintage, ...), and exits non-zero
/// when any test failed or errored so the `test` task fails the build. The
/// Launcher API is stable across JUnit 5 versions, so the runner never
/// needs the console-standalone jar and is agnostic to which engine — or
/// version of one — the module resolves.
const TEST_RUNNER_SOURCE: &str = r#"// Generated by the ulite/jvm plugin. Runs the tests compiled into the
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
/// test-runtime jars, in that order), passing the optional `testArgs`
/// entries after the class name.
fn run_test_args(classpath: &[String], test_class: &str, args: &[String]) -> Vec<String> {
    let mut invocation = vec!["-cp".to_owned(), classpath.join(":")];
    invocation.push(test_class.to_owned());
    invocation.extend(args.iter().cloned());
    invocation
}

/// The directories KSP writes its generated sources into, as a
/// `(kotlin, java)` pair. The compile tasks add these to their source
/// lists, so the ordering contract is generate → compile → package.
fn ksp_output_dirs(project_dir: &str) -> (String, String) {
    (
        format!("{project_dir}/build/generated/ksp/kotlin"),
        format!("{project_dir}/build/generated/ksp/java"),
    )
}

/// The `java` invocation that runs the KSP command-line tool against the
/// module's kotlin sources. The `-cp` is the processor classpath: the
/// `ksp` declarations, headed by the KSP2 toolchain jar whose
/// `com.google.devtools.ksp.cmdline.KSPJvmMain` main class is the entry
/// point. The same classpath is passed again as the final positional
/// argument, which KSP uses to load the `ProcessorProvider` services.
/// The compile classpath becomes `-libraries` so the processors resolve
/// the module's own dependencies, and the generated sources land in the
/// `*-output-dir` directories (`ksp_output_dirs`).
fn ksp_args(
    project_dir: &str,
    kotlin_sources: &[String],
    compile_classpath: &[String],
    processor_classpath: &[String],
) -> Vec<String> {
    let (kotlin_out, java_out) = ksp_output_dirs(project_dir);
    let mut args = vec![
        "-cp".to_owned(),
        processor_classpath.join(":"),
        "com.google.devtools.ksp.cmdline.KSPJvmMain".to_owned(),
        "-jvm-target".to_owned(),
        "11".to_owned(),
        "-module-name".to_owned(),
        "main".to_owned(),
        format!("-source-roots={}", kotlin_sources.join(":")),
        format!("-project-base-dir={project_dir}"),
        format!("-output-base-dir={project_dir}/build"),
        format!("-caches-dir={project_dir}/build/ksp-caches"),
        format!("-class-output-dir={project_dir}/build/ksp-classes"),
        format!("-kotlin-output-dir={kotlin_out}"),
        format!("-java-output-dir={java_out}"),
        format!("-resource-output-dir={project_dir}/build/ksp-resources"),
        "-language-version".to_owned(),
        "2.0".to_owned(),
        "-api-version".to_owned(),
        "2.0".to_owned(),
    ];
    if !compile_classpath.is_empty() {
        args.push(format!("-libraries={}", compile_classpath.join(":")));
    }
    args.push(processor_classpath.join(":"));
    args
}

#[cfg(test)]
mod tests {
    use super::{
        TEST_RUNNER_SOURCE, TEST_RUNNER_SOURCE_PATH, compile_args, jar_args, ksp_args,
        ksp_output_dirs, partition_sources, reject_unknown_extensions, resolve_path, run_test_args,
        string_list,
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
            &[],
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
    fn test_run_invocation_appends_runner_arguments_after_the_class() {
        let args = run_test_args(
            &[
                "/proj/build/test-classes".to_owned(),
                "/proj/build/classes".to_owned(),
            ],
            "org.junit.runner.JUnitCore",
            &["com.example.AppTest".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/proj/build/test-classes:/proj/build/classes".to_owned(),
                "org.junit.runner.JUnitCore".to_owned(),
                "com.example.AppTest".to_owned(),
            ]
        );
    }

    #[test]
    fn ksp_invocation_runs_the_tool_against_the_processor_classpath() {
        let args = ksp_args(
            "/proj",
            &["/proj/src/Main.kt".to_owned()],
            &["/repos/one.jar".to_owned()],
            &[
                "/repos/ksp-toolchain.jar".to_owned(),
                "/repos/processor.jar".to_owned(),
            ],
        );
        assert_eq!(
            args,
            vec![
                "-cp".to_owned(),
                "/repos/ksp-toolchain.jar:/repos/processor.jar".to_owned(),
                "com.google.devtools.ksp.cmdline.KSPJvmMain".to_owned(),
                "-jvm-target".to_owned(),
                "11".to_owned(),
                "-module-name".to_owned(),
                "main".to_owned(),
                "-source-roots=/proj/src/Main.kt".to_owned(),
                "-project-base-dir=/proj".to_owned(),
                "-output-base-dir=/proj/build".to_owned(),
                "-caches-dir=/proj/build/ksp-caches".to_owned(),
                "-class-output-dir=/proj/build/ksp-classes".to_owned(),
                "-kotlin-output-dir=/proj/build/generated/ksp/kotlin".to_owned(),
                "-java-output-dir=/proj/build/generated/ksp/java".to_owned(),
                "-resource-output-dir=/proj/build/ksp-resources".to_owned(),
                "-language-version".to_owned(),
                "2.0".to_owned(),
                "-api-version".to_owned(),
                "2.0".to_owned(),
                "-libraries=/repos/one.jar".to_owned(),
                "/repos/ksp-toolchain.jar:/repos/processor.jar".to_owned(),
            ]
        );
    }

    #[test]
    fn ksp_invocation_omits_libraries_when_the_compile_classpath_is_empty() {
        let args = ksp_args(
            "/proj",
            &["/proj/src/Main.kt".to_owned()],
            &[],
            &["/repos/processor.jar".to_owned()],
        );
        assert!(!args.contains(&"-libraries=/repos/one.jar".to_owned()));
        assert_eq!(args.last().unwrap(), "/repos/processor.jar");
    }

    #[test]
    fn ksp_output_dirs_split_kotlin_and_java_generation() {
        let (kotlin_out, java_out) = ksp_output_dirs("/proj");
        assert_eq!(kotlin_out, "/proj/build/generated/ksp/kotlin");
        assert_eq!(java_out, "/proj/build/generated/ksp/java");
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

    #[test]
    fn generated_runner_source_targets_the_launcher_api() {
        assert!(TEST_RUNNER_SOURCE.contains("LauncherDiscoveryRequestBuilder.request()"));
        assert!(TEST_RUNNER_SOURCE.contains("SummaryGeneratingListener"));
        assert!(TEST_RUNNER_SOURCE.contains("summary.getTotalFailureCount() > 0"));
        assert!(TEST_RUNNER_SOURCE.contains("class TestRunner"));
        assert!(TEST_RUNNER_SOURCE_PATH.ends_with("ulite/TestRunner.java"));
    }

    #[test]
    fn generated_runner_invocation_scan_arguments_follow_the_class() {
        let args = run_test_args(
            &[
                "/repos/launcher.jar".to_owned(),
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
                "/repos/launcher.jar:/proj/build/test-classes:/proj/build/classes".to_owned(),
                "ulite.TestRunner".to_owned(),
                "/proj/build/test-classes".to_owned(),
            ]
        );
    }
}
