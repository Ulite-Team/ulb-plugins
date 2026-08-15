//! The `ulite/jvm` plugin: compiles a plain-Java module and packages it.
//!
//! The module's `jvm {}` block describes the source list and the two
//! artifacts; the host resolves the module's `deps {}` block into a
//! compile classpath and injects it, along with the project directory,
//! into this plugin's configuration. `configure` turns all of that into
//! two tasks: `compile` runs `javac -d <classesDir> [-cp <classpath>]
//! <sources>` and `assemble` runs `jar cf <jarFile> -C <classesDir> .`
//! after `compile`. Paths written into the module block are resolved
//! against the injected `projectDir`, so a build succeeds regardless of
//! the directory the host was invoked from. The task inputs and outputs
//! are the source files and the produced classes/jar, so the host's
//! fingerprinting leaves both tasks alone until a source changes.
//! Consumed keys are documented in `REFERENCE.md` (ARCHITECTURE.md §5.1).

mod bindings {
    #![allow(unsafe_code)]
    #![allow(clippy::missing_safety_doc)]

    wit_bindgen::generate!({
        // The WIT text is the sdk crate's plugin.wit; the path keeps both
        // sides generating from the single source of truth.
        path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
        world: "plugin",
    });

    use crate::{jar_args, javac_args, resolve_path, sources_from_block};
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
                // Every run-tool task below uses one of these two tools.
                tools: vec!["javac".to_string(), "jar".to_string()],
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

            let sources = sources_from_block(jvm, project_dir)?;
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
            // jar paths; the plugin only decides how they reach javac.
            let compile_classpath = config
                .get("classpath")
                .and_then(|classpath| classpath.get("compile"))
                .and_then(Value::as_array)
                .map(|jars| {
                    jars.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();

            task_registrar::register_task(&Task {
                name: "compile".to_owned(),
                inputs: sources.clone(),
                outputs: vec![classes_dir.clone()],
                depends_on: Vec::new(),
                action: Action::RunTool(RunToolArgs {
                    tool: AllowlistedTool::Javac,
                    args: javac_args(&classes_dir, &compile_classpath, &sources),
                    cwd: ".".to_owned(),
                }),
            })?;

            task_registrar::register_task(&Task {
                name: "assemble".to_owned(),
                inputs: vec![classes_dir.clone()],
                outputs: vec![jar_file.clone()],
                depends_on: vec!["compile".to_owned()],
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
    export!(JvmPlugin);
}

/// Reads the module block's source list, rejecting an empty list, and
/// resolves each entry against the project directory.
fn sources_from_block(jvm: &serde_json::Value, project_dir: &str) -> Result<Vec<String>, String> {
    let sources = jvm
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "the 'jvm' block is missing a 'sources' list".to_owned())?;
    let sources = sources
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "a 'jvm.sources' entry is not a string".to_owned())
        })
        .collect::<Result<Vec<String>, _>>()?;
    if sources.is_empty() {
        return Err("the 'jvm' block declares no sources".to_owned());
    }
    Ok(sources
        .iter()
        .map(|source| resolve_path(project_dir, source))
        .collect())
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

/// The `javac` invocation for a compile task: emit classes to `-d`, feed
/// the host-resolved classpath to `-cp` when one exists (so an empty
/// classpath keeps javac's own defaults), then the sources.
fn javac_args(classes_dir: &str, classpath: &[String], sources: &[String]) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::{jar_args, javac_args, resolve_path, sources_from_block};

    #[test]
    fn javac_invocation_carries_classpath_and_sources() {
        let args = javac_args(
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
    fn javac_invocation_omits_cp_for_an_empty_classpath() {
        let args = javac_args(
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
    fn relative_block_paths_resolve_against_the_project_dir() {
        assert_eq!(resolve_path("/proj", "src/App.java"), "/proj/src/App.java");
        assert_eq!(resolve_path("/proj", "/abs/App.java"), "/abs/App.java");
    }

    #[test]
    fn sources_from_block_resolves_each_entry() {
        let block = serde_json::json!({ "sources": ["src/A.java", "/proj/lib/B.java"] });
        assert_eq!(
            sources_from_block(&block, "/proj").expect("block parses"),
            vec!["/proj/src/A.java".to_owned(), "/proj/lib/B.java".to_owned(),]
        );
    }

    #[test]
    fn sources_from_block_rejects_an_empty_list() {
        let block = serde_json::json!({ "sources": [] });
        assert!(sources_from_block(&block, "/proj").is_err());
    }
}
