//! The `ulite/kmp` plugin's JVM-target slice: compiles a Kotlin
//! multiplatform module's shared and JVM source sets into a jar.
//!
//! The module's `kmp {}` block declares source sets — blocks carrying a
//! `sources` list of `.java`/`.kt` file paths and optionally a `deps {}`
//! block, named after the hierarchy Kotlin publishes by default
//! (`commonMain`, `jvmMain`, ...) — and target configs, blocks carrying
//! neither `sources` nor `deps` (`jvm { classesDir ... jarFile ... }`).
//! The host resolves each source set's `deps {}` block independently and
//! injects the results as `classpathSourceSets`, keyed by the source set's
//! path under the model (`kmp.commonMain`, `kmp.jvmMain`); the JVM target
//! compiles the union of the `commonMain` and `jvmMain` sources against
//! the union of their compile classpaths. `compile` runs javac over `.java` sources,
//! `compile-kotlin` runs kotlinc over `.kt` sources (waiting for `compile`
//! when both coexist, with the classes directory on its classpath so the
//! Kotlin sees the Java classes), and `assemble` packs the classes into
//! the target's `jarFile`. Target and source-set paths are resolved against
//! the injected `projectDir`; task inputs are the source directories, which
//! the host fingerprints as trees, so an edit inside one reruns the
//! dependent chain. Consumed keys are documented in `docs/kmp-plugin.md`
//! (Uliab/docs/architecture.md §5.3).

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
        compile_args, jar_args, merged_classpath, optional_string_list, partition_sources,
        reject_unknown_extensions, resolve_path, resolve_paths,
    };
    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};
    use serde_json::Value;
    use ulite::ulb::task_registrar::{self, Action, AllowlistedTool, RunToolArgs, Task};

    /// Source sets the JVM target compiles; names follow the default
    /// hierarchy Kotlin publishes (`commonMain` shared by every target,
    /// `jvmMain` JVM-only).
    const JVM_SOURCE_SETS: &[&str] = &["commonMain", "jvmMain"];

    /// Target names this plugin recognizes as target config blocks.
    /// `jvm` is implemented; the others are recognized so a module that
    /// declares one fails with an explicit message instead of being
    /// silently ignored.
    const KNOWN_TARGETS: &[&str] = &["jvm", "android", "ios", "desktop", "native", "wasm"];

    /// Implements the exported `ulb-plugin` interface.
    struct KmpPlugin;

    impl Guest for KmpPlugin {
        fn manifest() -> PluginManifest {
            PluginManifest {
                name: "ulite/kmp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                abi_version: ulb_plugin_sdk::ABI_VERSION.to_string(),
                // Every run-tool task below uses one of these tools.
                tools: vec![
                    "javac".to_string(),
                    "kotlinc".to_string(),
                    "jar".to_string(),
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
            let kmp = config
                .get("kmp")
                .ok_or_else(|| "module config has no 'kmp' block".to_owned())?;
            let kmp = kmp
                .as_object()
                .ok_or_else(|| "the 'kmp' block is not an object".to_owned())?;

            // Classify the block's entries: a block with `sources` or
            // `deps` is a source set, a block named after a known target is
            // a target config, and anything else is a mistake worth naming.
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

            for (name, _) in &targets {
                if name != "jvm" {
                    return Err(format!(
                        "the 'kmp.{name}' target is not implemented; this slice of the kmp \
                         plugin compiles the 'jvm' target only"
                    ));
                }
            }
            let jvm = targets
                .iter()
                .find(|(name, _)| name == "jvm")
                .map(|(_, value)| *value)
                .ok_or_else(|| "the 'kmp' block declares no 'jvm' target".to_owned())?;

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

            // Collect the source files of the JVM source sets in hierarchy
            // order (shared first) and their compile classpaths. Sources
            // are explicit file paths, matching the `jvm` plugin's model;
            // the compilers are partitioned by extension below.
            let mut sources = Vec::new();
            for (name, value) in &source_sets {
                if !JVM_SOURCE_SETS.contains(&name.as_str()) {
                    return Err(format!(
                        "the '{name}' source set is not compiled by the jvm target; this slice \
                         supports {}",
                        JVM_SOURCE_SETS.join(" and ")
                    ));
                }
                sources.extend(optional_string_list(value, "sources")?);
            }
            if sources.is_empty() {
                return Err("the 'kmp' block declares no sources for the jvm target".to_owned());
            }
            let sources = resolve_paths(project_dir, &sources);
            reject_unknown_extensions(&sources)?;
            let (java_sources, kotlin_sources) = partition_sources(&sources);
            let compile_classpath = merged_classpath(&config, JVM_SOURCE_SETS);

            // One javac task for the source sets' own java files, one
            // kotlinc task for the kotlin files. Kotlin can see the java
            // classes, so the kotlin task waits for the java one when both
            // coexist. The source files are the task inputs, so an edit to
            // one reruns its task.
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
                depends_on: compile_tasks,
                action: Action::RunTool(RunToolArgs {
                    tool: AllowlistedTool::Jar,
                    args: jar_args(&jar_file, &classes_dir),
                    cwd: ".".to_owned(),
                }),
            })?;

            Ok(())
        }

        fn run(input: String) -> String {
            input
        }
    }

    // The export generates wasm component symbols (`export_name` with a
    // component-model name), which only link on the wasm32-wasip2 target.
    #[cfg(target_arch = "wasm32")]
    export!(KmpPlugin);
}

/// Resolves module-block paths against the project directory; an absolute
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

fn resolve_paths(project_dir: &str, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|path| resolve_path(project_dir, path))
        .collect()
}

/// Reads an optional string-list key of a source-set block; a missing key
/// is an empty list, a present non-list or non-string entry is an error.
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

/// Splits source paths into `.java` files and everything else (the `.kt`
/// files), preserving order within each half.
fn partition_sources(sources: &[String]) -> (Vec<String>, Vec<String>) {
    sources
        .iter()
        .cloned()
        .partition(|source| source.ends_with(".java"))
}

/// Rejects a source file that is neither `.java` nor `.kt`; the compilers
/// are partitioned by this extension, so anything else would be silently
/// dropped.
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

/// The host-resolved compile classpath of one source-set path under the
/// `kmp` block, e.g. `kmp.commonMain`. The host injects an entry only for
/// source sets that declare a `deps {}` block, so a missing entry reads as
/// an empty classpath.
fn source_set_classpath(config: &serde_json::Value, path: &str) -> Vec<String> {
    config
        .get("classpathSourceSets")
        .and_then(|sets| sets.get(path))
        .and_then(|classpath| classpath.get("compile"))
        .and_then(serde_json::Value::as_array)
        .map(|jars| {
            jars.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

/// The compile classpath of a target: the union, in hierarchy order, of its
/// source sets' resolved compile classpaths, deduplicated so a jar declared
/// in both the shared and the platform source set appears once.
fn merged_classpath(config: &serde_json::Value, source_sets: &[&str]) -> Vec<String> {
    let mut merged = Vec::new();
    for name in source_sets {
        for jar in source_set_classpath(config, &format!("kmp.{name}")) {
            if !merged.contains(&jar) {
                merged.push(jar);
            }
        }
    }
    merged
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

/// The `jar` invocation for an assemble task: create the jar and pack the
/// classes directory.
fn jar_args(jar_file: &str, classes_dir: &str) -> Vec<String> {
    vec![
        "cf".to_owned(),
        jar_file.to_owned(),
        "-C".to_owned(),
        classes_dir.to_owned(),
        ".".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        compile_args, jar_args, merged_classpath, optional_string_list, partition_sources,
        reject_unknown_extensions, resolve_path, source_set_classpath,
    };

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
        assert!(reject_unknown_extensions(&["/proj/src/commonMain".to_owned()]).is_err());
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
        // shared appears once, in the commonMain position; jvm.jar joins
        // after commonMain's jars.
        assert_eq!(
            merged_classpath(&config, &["commonMain", "jvmMain"]),
            vec![
                "/repos/shared.jar".to_owned(),
                "/repos/other.jar".to_owned(),
                "/repos/jvm.jar".to_owned(),
            ]
        );
    }
}
