//! The hello-plugin: a minimal `ulb-plugin` world implementation.
//!
//! This crate establishes the build path every later plugin follows. It
//! generates its guest bindings from the sdk crate's `plugin.wit` (the
//! single WIT text the host also binds from), and it reports the SDK's ABI
//! version verbatim in its manifest so the host's ABI check cannot be
//! tricked by a hand-typed constant. It also exports
//! `configure` — required for the full-world instantiation every plugin
//! goes through — and, having no build tasks, declares no tools and
//! accepts any well-formed module configuration.

/// The whole plugin lives inside this module because the `export!` macro
/// resolves `self::exports` relative to the module where `generate!` ran.
/// The `unsafe` and `export_name` the two macros emit are confined here;
/// nothing outside this module uses them.
mod bindings {
    #![allow(unsafe_code)]
    #![allow(clippy::missing_safety_doc)]

    wit_bindgen::generate!({
        // The WIT text is the sdk crate's plugin.wit; the path keeps both
        // sides generating from the single source of truth.
        path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
        world: "plugin",
    });

    use exports::ulite::ulb::ulb_plugin::{Guest, PluginManifest};

    /// Implements the exported `ulb-plugin` interface.
    struct HelloPlugin;

    impl Guest for HelloPlugin {
        fn manifest() -> PluginManifest {
            PluginManifest {
                // The registry identity the plugin is published under; the
                // registry client verifies the manifest name against the
                // coordinate it resolved.
                name: "ulite/hello".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                abi_version: ulb_plugin_sdk::ABI_VERSION.to_string(),
                // This plugin registers no tasks, so it declares no tools.
                tools: Vec::new(),
            }
        }

        fn configure(module_config: String) -> Result<(), String> {
            // The hello-plugin has no tasks to register; it still parses
            // the configuration so a malformed module block surfaces as a
            // configure error rather than a silent success.
            serde_json::from_str::<serde_json::Value>(&module_config)
                .map_err(|error| format!("invalid module config JSON: {error}"))?;
            Ok(())
        }

        fn run(input: String) -> String {
            format!("hello-plugin says: {input}")
        }
    }

    // The export generates wasm component symbols (`export_name` with a
    // component-model name), which only link on the wasm32-wasip2 target.
    #[cfg(target_arch = "wasm32")]
    export!(HelloPlugin);
}
