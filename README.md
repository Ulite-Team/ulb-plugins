# ulb-plugins

Official WASM plugins for the `ulb` build tool, plus the plugin registry
index. The plugins are the build logic: each is a `wasm32-wasip2`
component exporting the `ulb-plugin` world defined in
[`Ulite-Team/Uliab`](https://github.com/Ulite-Team/Uliab)
(`crates/ulb-plugin-sdk/plugin.wit`). The core stays target-agnostic; all
Java, Kotlin, and Android knowledge lives here.

## Plugins

| Plugin | Version | What it does |
|---|---|---|
| `ulite/hello` | 0.5.0 | The minimal plugin: establishes the build/test path, echoes its input under the host |
| `ulite/jvm` | 0.6.0 | Compiles a module's Java/Kotlin sources, packages a jar, compiles and runs tests (JUnit 4/5), and runs the KSP2 tool over Kotlin sources |
| `ulite/android` | 0.3.0 | Discovers the Android SDK and compiles a module's Java sources and resources into per-variant APKs |
| `ulite/kmp` | 0.2.0 | Compiles a Kotlin multiplatform module's shared and jvm source sets into a jar |

## Repository layout

```
hello-plugin/       ulite/hello — minimal world implementation
jvm-plugin/         ulite/jvm — the reference plugin
registry/index.json The plugin registry index
fixtures/ksp-hello/ A real KSP processor fixture for the ksp task
.github/workflows/  plugin-build: system tests against real components
docs/               Documentation (see below)
```

## Building a plugin

```sh
rustup target add wasm32-wasip2
cargo build -p hello-plugin --release --target wasm32-wasip2   # or jvm-plugin
wasm-tools component new target/wasm32-wasip2/release/hello_plugin.wasm \
  -o hello_plugin.wasm
uliab run hello_plugin.wasm 'input'
```

The sdk dependency is a path dependency
(`../../Uliab/crates/ulb-plugin-sdk`), so the Uliab repository must sit
next to this one:

```
somewhere/
  Uliab/          # plugin sdk + uliab host
  ulb-plugins/    # this repo
```

## Development

```sh
cargo build --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo test
```

A plugin change is not done until the wasm component builds, loads, and
runs under the real host (`uliab run`).

## Documentation

Everything lives in [`docs/`](docs/index.md):
[architecture](docs/architecture.md), the
[`ulite/jvm` reference](docs/jvm-plugin.md), [`ulite/hello`](docs/hello-plugin.md),
the [registry index](docs/registry.md), the [CI workflow](docs/ci.md),
and the [KSP fixture](docs/ksp-fixture.md).

## License

GPL-3.0. See `LICENSE`.
