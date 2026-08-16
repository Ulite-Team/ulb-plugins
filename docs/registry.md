# Plugin registry

`registry/index.json` is the index the host's registry client resolves
`libs.ulb` plugin coordinates against. The client itself lives in the
Uliab core (`crates/uliab/src/registry.rs`, documented in
`Uliab/docs/plugin-registry.md`); this repository owns the **content** of
the index and the release assets it points at.

## Format

```json
{
  "schema_version": 1,
  "plugins": {
    "ulite/hello": {
      "versions": {
        "0.4.0": {
          "abi": { "min": "0.4", "max": "0.4" },
          "artifact_url": "https://github.com/Ulite-Team/ulb-plugins/releases/download/hello-plugin-v0.4.0/hello_plugin.wasm"
        }
      }
    }
  }
}
```

- `schema_version: 1` — the index layout version the host understands.
- Each plugin name maps to a `versions` map; each version row carries an
  inclusive ABI range (`{min, max}`) and an `artifact_url`.
- `artifact_url` may be HTTPS, `file://`, or relative (resolved against
  the index file's directory). CI uses a relative URL
  (`"hello_plugin.wasm"`) to point at a locally-built artifact.
- The `abi` range records the host ABI the plugin was built against, so
  the client can refuse an incompatible build before running it.

## The committed index today

`ulite/hello@0.4.0` is the only row. Its `artifact_url` points at a
release asset (`hello-plugin-v0.4.0/hello_plugin.wasm`); until that
release exists, the registry jobs in CI seed a *local* index from the
checkout's build output rather than resolving from GitHub
([ci.md](ci.md)). The `jvm-build` and KSP jobs do the same with
`ulite/jvm@0.5.0`, which is intentionally absent from the committed index
until its artifact is released.

## Publishing a version

1. Bump the plugin's `version` in its `Cargo.toml` and build the
   component:
   `cargo build -p <plugin> --release --target wasm32-wasip2`
   then `wasm-tools component new` the artifact.
2. Create a GitHub release named after the tag convention the index
   points at (e.g. `hello-plugin-v0.4.0`) and upload the component under
   the file name the URL references.
3. Add the version row to `registry/index.json` with the plugin's actual
   ABI range and artifact URL, and commit.

The host verifies the downloaded artifact's manifest `name`/`version`/
`abi-version` against the index row, so the row must match what the plugin
actually reports — the index is not a promise, it is a declaration the
client checks.
