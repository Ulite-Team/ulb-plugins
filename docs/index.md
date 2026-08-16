# ulb-plugins — Documentation

This repository holds the official WASM plugins for the `ulb` build tool
(`ulite/hello`, `ulite/jvm`, `ulite/android`), the plugin registry index,
and the build pipeline that exercises the whole system end to end.

## Documents

| Document | What it covers |
|---|---|
| [architecture.md](architecture.md) | The workspace: what each plugin owns, how plugins are built and published, and how the pieces interact |
| [hello-plugin.md](hello-plugin.md) | `ulite/hello` — the minimal plugin that establishes the build path every later plugin follows |
| [jvm-plugin.md](jvm-plugin.md) | `ulite/jvm` — the reference plugin: module block, classpath buckets, tasks, KSP support (moved from `jvm-plugin/REFERENCE.md`) |
| [android-plugin.md](android-plugin.md) | `ulite/android` — SDK discovery and the compile task against the platform jar |
| [registry.md](registry.md) | The `registry/index.json` format and how artifacts are published |
| [ci.md](ci.md) | The `plugin-build` workflow and what each job actually proves |
| [ksp-fixture.md](ksp-fixture.md) | The `fixtures/ksp-hello` processor fixture and the offline Maven layout it installs into |

## The plugin contract in one paragraph

Every plugin is a `wasm32-wasip2` component exporting the `ulb-plugin`
world defined in `Uliab/crates/ulb-plugin-sdk/plugin.wit` (the *single
source of truth* for the ABI). Host and guest bind from the same WIT:
plugins generate guest bindings with
`wit_bindgen::generate!({ path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit" })`,
so the interface cannot drift between the core and the plugins. Plugins
declare their tools in the manifest and register tasks only during
`configure`; the host runs those tasks through the allowlisted-tool
capability. Building a plugin, wrapping it into a component, resolving it
through the registry, and running it under the host is the Definition of
Done for every plugin change.

## The layout contract

The workspace mirrors the dev machine: `Uliab` sits next to `ulb-plugins`
so `../../Uliab/crates/ulb-plugin-sdk` resolves both in cargo and in the
WIT `path:` argument. CI reproduces exactly that layout
([ci.md](ci.md)). A released plugin would switch the sdk dependency to a
published crate without changing the WIT story.
