# `ulite/android` plugin reference

The `ulite/android` plugin compiles the module's Java sources against the
Android platform jar for its declared `compileSdk`. It is the compile
slice of what `Uliab/docs/architecture.md §5.2` assigns to the
`ulite/android` plugin: resource merging (`aapt2`), dexing and APK
packaging (`d8`), the variant matrix, and manifest merging are future
slices of the same plugin. What exists today is the SDK discovery and the
`compile` task that depends on it.

## The SDK

An Android module cannot compile without an Android SDK, and a module does
not ship one — so the SDK root comes from the build, not the module:

1. the module block's `sdkDir` key, when set, or
2. the `androidSdkDir` the host injects (its own `--android-sdk` flag, or
   the `ANDROID_HOME`/`ANDROID_SDK_ROOT`/`~/Android/Sdk` conventions it
   probes), or
3. a configure error — the SDK cannot be invented.

The host also **preopens** the chosen root into the plugin's WASI
filesystem, read-only, at its real path (`Uliab/docs/architecture.md §3.2`):
that is how `configure` can inspect it at all, since a wasm guest has no
ambient filesystem. Access is read-only — a plugin can read the SDK but
never modify it.

## Module block

Inside the module's top-level block, the `android {}` sub-block owns the
following keys:

| Key | Type | Meaning |
|---|---|---|
| `compileSdk` | integer | The API level to compile against. Required. `configure` looks for the matching platform jar. |
| `sources` | list of strings | `.java` files to compile. At least one entry is required. Kotlin sources are not supported yet. |
| `classesDir` | string | Directory `javac` writes `.class` files to. |
| `sdkDir` | string, optional | Per-module SDK root, overriding the host-injected `androidSdkDir`. |

The values are resolved against the project directory the host injects
(`projectDir`); absolute paths are used as written.

Example:

```text
android {
  compileSdk = 36
  sources = ["src/Main.java"]
  classesDir = "build/classes"
}
```

## Host-injected keys

The host supplies these alongside the module model; the plugin reads them
but they are not part of the `android {}` block:

| Key | Meaning |
|---|---|
| `projectDir` | The project directory the build was started for. |
| `androidSdkDir` | The resolved SDK root (from the host's `--android-sdk` flag or environment conventions), when one exists. |
| `classpath.compile` | Jar paths resolved from the module's `deps {}` block for the compile scope. |

## Toolchain discovery

`configure` performs the discovery a packaging slice of this plugin will
later consume, and fails at configure time — before anything runs — when
the SDK is unusable:

- **Platform jar** — `<sdk>/platforms/android-<compileSdk>/android.jar`
  must exist. It heads the compile classpath, so the module's own sources
  can reference the SDK types.
- **Build tools** — the highest `<sdk>/build-tools/<version>` directory
  that carries *both* `aapt2` and `d8` must exist; a partially installed
  release does not count, and a directory whose name is not a numeric
  dotted version is skipped. This is the release a future packaging task
  would invoke.

## Registered tasks

| Task | Tool | Action |
|---|---|---|
| `compile` | `javac` | `javac -d <classesDir> -cp <android.jar>:<classpath.compile> <sources>` |

`compile` declares the source files as inputs and `classesDir` as output.
The platform jar is deliberately **not** an input: it is a large,
externally-fixed artifact the build never modifies, and hashing it on
every run would buy nothing — a change of SDK root already changes the
configuration hash, which reruns the graph.

## Manifest

The plugin declares `javac` as the only tool of its run-tool tasks, per
the host's manifest-declared-tools check. It reports ABI `0.4`.
