# Architecture

## Workspace

A Cargo workspace (`resolver = "2"`, edition 2024) with four members:

| Crate | Plugin | Job |
|---|---|---|
| `hello-plugin` (0.5.0) | `ulite/hello` | Minimal world implementation; establishes the build/test path |
| `jvm-plugin` (0.6.0) | `ulite/jvm` | Compile Java/Kotlin, package a jar, run tests, run KSP2 |
| `android-plugin` (0.3.0) | `ulite/android` | Discover the Android SDK toolchain; compile against the platform jar |
| `kmp-plugin` (0.2.0) | `ulite/kmp` | Compile a Kotlin multiplatform module's shared and jvm source sets into a jar |

All are `cdylib` crates, depend on `ulb-plugin-sdk` by path
(`../../Uliab/crates/ulb-plugin-sdk`), and share the workspace's
`unsafe_code = "deny"` lint. All Rust code is GPL-3.0 (the SDK itself is
MIT and lives in the Uliab repo).

## What the plugins own

The core (`Uliab`) is target-agnostic: the DSL, the task DAG, the Maven
resolver, the registry client, and the wasmtime host. Every piece of
toolchain knowledge is a plugin concern:

- `ulite/jvm` knows `javac`, `kotlinc`, `jar`, `java`, and the KSP2 tool
  invocation — nothing in the core does.
- `ulite/android` knows where an SDK keeps its platform jar and
  build-tools, and how to compile against them — nothing in the core does
  (the host only hands it the SDK root and read-only access to it, see
  below).
- `ulite/kmp` is the roadmap; it will follow the same shape: a `configure`
  that validates a module block and registers tasks.

Per the core architecture (`Uliab/docs/architecture.md` §5.1), the
`jvm` plugin family owns classpath scoping beyond compile/test as future
milestones of the same plugin; `ulite/android`'s variant matrix, resource
merging, and packaging are future milestones of its own.

## The SDK capability

A wasm plugin has no ambient filesystem, so `ulite/android` cannot locate
an Android SDK by itself. The host resolves the root (its `--android-sdk`
flag, or `ANDROID_HOME`/`ANDROID_SDK_ROOT`/`~/Android/Sdk`), injects it as
the `androidSdkDir` configuration key, and **preopens the directory
read-only into the plugin's WASI filesystem at its real path**
(`Uliab/docs/architecture.md` §3.2). That capability is what lets
`configure` discover the platform jar and build-tools inside the SDK; the
guest filesystem is otherwise empty.

## The plugin lifecycle

1. **Build** — `cargo build -p <plugin> --release --target wasm32-wasip2`.
2. **Wrap** — `wasm-tools component new` turns the core wasm into a
   component (the tests and CI rely on this step in the host's
   `manifest`-read path).
3. **Publish** — the component is uploaded as a release asset, and the
   registry `index.json` gains a version row pointing at it
   ([registry.md](registry.md)).
4. **Resolve** — a `libs.ulb` `plugins {}` table declares
   `"ulite/jvm" @ "0.6.0"`; the host's registry client downloads the
   artifact and verifies its manifest against the index row.
5. **Configure** — the host instantiates the component, checks the
   manifest's `abi-version`, and calls `configure(module_config_json)`.
   The plugin validates its module block and registers tasks.
6. **Execute** — the host's task engine runs the registered tasks through
   the allowlisted tools.

Steps 4–6 are what the CI jobs prove with the *built* artifact, not a
mock.

## The WIT path contract

Plugins generate guest bindings with a literal `path:` argument:

```rust
wit_bindgen::generate!({
    path: "../../Uliab/crates/ulb-plugin-sdk/plugin.wit",
    world: "plugin",
});
```

The path is relative to the crate source, which is why the repository
layout (Uliab adjacent to ulb-plugins) is not cosmetic: it must hold both
for cargo's `path =` dependency and for this `path:` argument. The core
repo's documentation enforces the same rule for the host side (its
`bindgen!` uses the same WIT text).

## Failure semantics

`configure` returns `Err(String)` for a malformed module block or a
contradictory configuration (e.g. KSP deps with no Kotlin sources, or both
`testClass` and `testRunner`). The host surfaces that error and never
executes a partially-configured graph. A failing task (a broken test
assertion, a javac error) fails the build with the task's failure payload.
Both behaviors are pinned by CI ([ci.md](ci.md)).
