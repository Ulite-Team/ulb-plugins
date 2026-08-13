//! The hello-plugin: a minimal `ulb-plugin` world implementation.
//!
//! This crate establishes the build path every later plugin follows. It
//! generates its guest bindings from the sdk crate's `plugin.wit` (core
literal://! repo's single WIT text), so the host and the plugin share one WIT file, and
//! it reports the SDK's ABI version verbatim in its manifest so the host's
//! ABI check cannot be tricked by a hand-typed constant.

/// The whole plugin lives inside this module because the `export!` macro
/// resolves `self::exports` relative to the module where `generate!` ran.
/// The `unsafe` and `export_name` the two macros emit are confined here;
/// nothing outside this module uses them.
mod bindings {
    #![allow(unsafe_code)]
    #![allow(clippy::missing_safety_doc)]

    wit_bindgen::generate!({
        path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
        world: "plugin",
    });

    use exports::ulite::ulb::ulb_plugin::{PluginManifest, Guest};

    /// Implements the exported `ulb-plugin` interface.
    struct HelloPlugin;

    impl Guest for HelloPlugin {
        fn manifest() -> PluginManifest {
            PluginManifest {
                name: "hello-plugin".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                abi_version: ulb_plugin_sdk::ABI_VERSION.to_string(),
            }
        }

        fn run(input: String) -> String {
            format!("hello-plugin says: {input}")
        }
    }

    export!(HelloPlugin);
}
