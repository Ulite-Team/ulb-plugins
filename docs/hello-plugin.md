# `ulite/hello` (hello-plugin)

The first and deliberately minimal plugin. It exists to establish the
build and test path every later plugin follows, and to give the host and
CI a component that exercises the whole `manifest` → `configure` →
instantiation path with no toolchain involved.

## What it does

- Exports the `ulb-plugin` world: `manifest`, `configure`, and the legacy
  `run` entry point (kept so `uliab run <wasm> <input>` works against it).
- `manifest` reports `name: "ulite/hello"`, version from
  `CARGO_PKG_VERSION` (0.5.0), and `abi_version` taken verbatim from
  `ulb_plugin_sdk::ABI_VERSION` — never a hand-typed literal, so the
  host's ABI check cannot be tricked by drift in this crate.
- Declares **no tools** (it registers no tasks).
- `configure` parses the injected module config JSON and rejects a
  malformed one with an error — even a plugin with no tasks must not
  silently accept garbage configuration.
- `run` echoes its input: `hello-plugin says: {input}`.

## Why it exists as a separate crate

The CI needs a plugin that builds fast, has zero external toolchain
dependencies, and whose behavior is trivially verifiable in output
strings. `hello-plugin` is that plugin: `build-and-run` runs its
component under the real `uliab` host and greps for the echo; the
`registry-resolve` job does the same after resolving it through the
registry client's full download → verify → cache path.

## Structure

`src/lib.rs` is one `bindings` module:

- `#![allow(unsafe_code)]` is scoped to the module — the `export!`/`generate!`
  macros emit `unsafe` and component `export_name` symbols that only link
  on `wasm32-wasip2`; nothing outside the module uses unsafe.
- The guest bindings come from the sdk crate's `plugin.wit` by `path:`,
  per the WIT path contract in [architecture.md](architecture.md).
- `export!(HelloPlugin)` is `#[cfg(target_arch = "wasm32")]`, so the crate
  still compiles for the host target if anyone builds it that way.
