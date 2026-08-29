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
        "0.5.0": {
          "abi": { "min": "0.7", "max": "0.8" },
          "artifact_url": "https://github.com/Ulite-Team/ulb-plugins/releases/download/hello-plugin-v0.5.0/hello_plugin.wasm"
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

The newest published rows are `ulite/hello@0.5.0`, `ulite/jvm@0.6.0`,
`ulite/android@0.3.0`, and `ulite/kmp@0.3.0`, all declaring ABI range
`{min: 0.7, max: 0.8}`. Older rows (e.g. `ulite/hello@0.4.0`) remain in
the index for hosts still resolving older ABI targets; their
`artifact_url` rows point at release assets
(`hello-plugin-v0.4.0/hello_plugin.wasm`,
`jvm-plugin-v0.5.0/jvm_plugin.wasm`) published by the `release`
workflow. The registry jobs in CI additionally
seed a *local* index from the checkout's build output so the resolve path
is exercised without network ([ci.md](ci.md)).

## Publishing a version

1. Bump the plugin's `version` in its `Cargo.toml`.
2. Run the `release` workflow (`.github/workflows/release.yml`): it builds
   each plugin's component (`cargo build -p <plugin> --release --target
   wasm32-wasip2`, linked into a component by the wasm32-wasip2 target) and
   uploads it to a GitHub release named after the tag convention the index
   points at, under the file name the `artifact_url` references.
3. Add the version row to `registry/index.json` with the plugin's actual
   ABI range and artifact URL, and commit.

The host verifies the downloaded artifact's manifest `name`/`version`/
`abi-version` against the index row, so the row must match what the plugin
actually reports — the index is not a promise, it is a declaration the
client checks.
