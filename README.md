# ulb-plugins

Official WASM plugins for the `ulb` build tool, plus the plugin registry
index. The plugins are the build logic: each is a `wasm32-wasip2`
component exporting the `ulb-plugin` world defined in
[`Ulite-Team/Uliab`](https://github.com/Ulite-Team/Uliab)
(`crates/ulb-plugin-sdk/plugin.wit`). The core stays target-agnostic; all
Java, Kotlin, and Android knowledge lives here.

## Plugins

| Plugin | Version | ABI | What it does |
|---|---|---|---|
| `ulite/hello` | 0.5.0 | 0.7 | The minimal plugin: establishes the build/test path, echoes its input under the host |
| `ulite/jvm` | 0.6.0 | 0.7 | Compiles a module's Java/Kotlin sources, packages a jar, compiles and runs tests (JUnit 4 via a runner class, JUnit 5 via a generated platform launcher runner), and runs the KSP2 tool over Kotlin sources |
| `ulite/android` | 0.3.0 | 0.7 | Discovers the Android SDK, compiles Java and Kotlin sources, merges resources with `aapt2`, dexes with `d8`, packages per-variant APKs (`buildTypes {}` × `productFlavors {}`), signs them with `apksigner`, and wires the Compose compiler plugin |
| `ulite/kmp` | 0.3.0 | 0.7 | Compiles a multiplatform module's shared and jvm source sets into a jar with per-target JVM tests, and builds an Android target (APKs) by composing with `ulite/android` across the plugin ABI |

All four are published as GitHub release assets at ABI 0.7 and indexed in
[`registry/index.json`](registry/index.json), so the core tool resolves
them from its default registry URL with no configuration.

## Repository layout

```
hello-plugin/       ulite/hello — minimal world implementation
jvm-plugin/         ulite/jvm — the reference plugin
android-plugin/     ulite/android — compile/package/sign chain, variants
kmp-plugin/         ulite/kmp — shared/jvm source sets + Android target
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
the [`ulite/android` reference](docs/android-plugin.md), the
[`ulite/kmp` reference](docs/kmp-plugin.md), the
[registry index](docs/registry.md), the [CI workflow](docs/ci.md),
and the [KSP fixture](docs/ksp-fixture.md).

## License

GPL-3.0. See `LICENSE`.
