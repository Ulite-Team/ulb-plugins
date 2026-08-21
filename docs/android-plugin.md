# `ulite/android` plugin reference

The `ulite/android` plugin compiles the module's Java sources against the
Android platform jar for its declared `compileSdk`, merges the module's
resources with `aapt2`, dexes the classes with `d8`, and assembles
per-variant APKs. It implements the compile-and-package part of what
`Uliab/docs/architecture.md §5.2` assigns to `ulite/android`; manifest
merging and additional packaging features are future slices of the same
plugin.

## The SDK

An Android module cannot compile without an Android SDK, and a module does
not ship one — so the SDK root comes from the build, not the module:

1. the module block's `sdkDir` key, when set, or
2. the `androidSdkDir` the host injects (its own `--android-sdk` flag, or
   the `ANDROID_HOME`/`ANDROID_SDK_ROOT`/`~/Android/Sdk` conventions it
   probes), or
3. a configure error — the SDK cannot be invented.

The host also **preopens** the chosen root into the plugin's WASI
filesystem, read-only, at its real path (`Uliab/docs/architecture.md
§3.2`): that is how `configure` can inspect it at all, since a wasm guest
has no ambient filesystem. Access is read-only — a plugin can read the SDK
but never modify it. A module-declared `sdkDir` is preopened too, so a
per-module SDK that differs from the host's root is discoverable the same
way: the host preopens both the resolved root and the module's own path.

## Module block

Inside the module's top-level block, the `android {}` sub-block owns the
following keys:

| Key | Type | Meaning |
|---|---|---|
| `compileSdk` | integer | The API level to compile against. Required. `configure` looks for the matching platform jar. |
| `minSdk` | integer | The default minimum API level; `aapt2 link` records it and `d8` uses it as `--min-api`. A per-flavor `minSdk` in `productFlavors {}` overrides this. Required. |
| `targetSdk` | integer, optional | The default target API level; defaults to `compileSdk`. A supplied value that is not an integer is a configure error. |
| `namespace` | string | The package the generated `R` class lives in, handed to `aapt2 link` as `--custom-package`. Required. |
| `sources` | list of strings | `.java` files to compile. At least one entry is required. Kotlin sources are not supported yet. |
| `manifest` | string | The `AndroidManifest.xml` `aapt2 link` merges and packages. Required. |
| `resDir` | string | The `res/` directory `aapt2 compile` merges. Required. |
| `sdkDir` | string, optional | Per-module SDK root, overriding the host-injected `androidSdkDir`. Relative paths resolve against the project directory. |

The values are resolved against the project directory the host injects
(`projectDir`); absolute paths are used as written.

Example:

```text
android {
  compileSdk = 36
  minSdk = 21
  namespace = "com.example.ulite"
  sources = ["src/Main.java"]
  manifest = "AndroidManifest.xml"
  resDir = "res"
}
```

## Host-injected keys

The host supplies these alongside the module model; the plugin reads them
but they are not part of the `android {}` block:

| Key | Meaning |
|---|---|
| `projectDir` | The project directory the build was started for, always absolute. |
| `androidSdkDir` | The resolved SDK root (from the host's `--android-sdk` flag or environment conventions), when one exists. |
| `classpath.compile` | Jar paths resolved from the module's `deps {}` block for the compile scope. |

## Build types and product flavors

`buildTypes {}` and `productFlavors {}` are optional blocks that define
the variant matrix. The plugin computes the cartesian product of build
types x flavors; each cell becomes a separate variant with its own
compile, dex, and packaging tasks.

Without either block, the default pair `[debug, release]` is used and
two variants are produced: `Debug` and `Release`.

### `buildTypes {}`

Named blocks (`debug`, `release`, or custom). The plugin currently
recognizes these keys but does not yet act on them (minification and
shrinking are deferred):

| Key | Type | Meaning |
|---|---|---|
| `minifyEnabled` | boolean | R8/minification |
| `shrinkResources` | boolean | resource shrinking |
| `proguardFiles` | list of strings | proguard rule files |

### `productFlavors {}`

The `productFlavors {}` block declares flavor dimensions and flavor
blocks:

- `dimension "tier"` (pair statement) declares a flavor dimension.
- Flavor blocks (`free { }`, `paid { }`) may carry any of:

| Key | Type | Meaning |
|---|---|---|
| `dimension` | string | which dimension this flavor belongs to; optional when exactly one dimension is declared |
| `applicationIdSuffix` | string | appended to the namespace as `--rename-manifest-package` on `aapt2 link` |
| `minSdk` | number | flavor-specific floor, overriding `android.minSdk` |

Example with flavors:

```text
productFlavors {
  dimension "tier"

  free {
    applicationIdSuffix = ".free"
  }
  paid {
    applicationIdSuffix = ".paid"
  }
}
```

This produces four variants: `DebugFree`, `DebugPaid`, `ReleaseFree`,
`ReleasePaid`.

### Variant naming

- Task suffixes are PascalCase: `compileDebug`, `linkResourcesReleaseFree`.
- Variant directories are camelCase: `debug`, `releaseFree`.
- APK filenames: `app-debug.apk`, `app-releaseFree.apk`.
- Each variant's APK lives under `<project>/build/<variant>/`.

## Signing

The optional `signing {}` block at the module level configures APK
signing. Signing is shared across all variants: the password files are
written once, and each variant's `signApk` task reads the same keystore
and passwords.

| Key | Type | Meaning |
|---|---|---|
| `storeFile` | string | Path to the keystore, resolved against the project directory. Required. |
| `storePassword` | string | Keystore password. Required. |
| `keyAlias` | string | Key alias within the keystore. Required. |
| `keyPassword` | string | Key password. Required. |

Example:

```text
signing {
  storeFile = "release.keystore"
  storePassword = env("RELEASE_STORE_PASSWORD")
  keyAlias = "release"
  keyPassword = env("RELEASE_KEY_PASSWORD")
}
```

**Security note:** passwords declared in `signing {}` are written to
plaintext files (`ks-password.txt`, `key-password.txt`) in the build
directory. Use `env()` to pull values from environment variables rather
than embedding them in source-controlled files. The build directory
should be in `.gitignore`.

## Toolchain discovery

`configure` performs the discovery the packaging tasks consume, and fails
at configure time — before anything runs — when the SDK is unusable:

- **Platform jar** — `<sdk>/platforms/android-<compileSdk>/android.jar`
  must exist. It heads the compile classpath and is d8's `--lib`, so the
  module's own sources can reference the SDK types and the dex can
  resolve them.
- **Build tools** — the highest `<sdk>/build-tools/<version>` directory
  that carries both `aapt2` and `lib/d8.jar` must exist; a partially
  installed release does not count, and a directory whose name is not a
  numeric dotted version is skipped. This is the release the packaging
  tasks invoke.

## Build products

Everything the tools produce lives under `<project>/build/`, derived from
the injected `projectDir`:

- `build/android/res.zip` — the merged resources, `aapt2 compile` output.
- `build/<variant>/resources.apk` — the linked resources APK for the variant.
- `build/<variant>/R/` — the generated `R.java` for the variant.
- `build/<variant>/classes` — the compiled `.class` files for the variant.
- `build/<variant>/classes.jar` — the classes archived for d8.
- `build/<variant>/dex/` — the d8 output for the variant.
- `build/<variant>/app-<variant>.apk` — the final APK for the variant.

## Registered tasks

Shared tasks (registered once):

| Task | Tool | Action |
|---|---|---|
| `prepareBuildDir` | `mkdir` | `mkdir -p <build>/android`. |
| `mergeResources` | `aapt2` | `aapt2 compile --dir <resDir> -o <build>/android/res.zip`. |
| `writeSigningPasswords` | `write_file` | Writes `<build>/android/ks-password.txt` from `signing.storePassword`. Only when `signing {}` is present. |
| `writeSigningKeyPassword` | `write_file` | Writes `<build>/android/key-password.txt` from `signing.keyPassword`. Only when `signing {}` is present. |

Per-variant tasks (suffixed with PascalCase variant name):

| Task | Tool | Action |
|---|---|---|
| `prepareApk<V>` | `mkdir` | `mkdir -p <build>/<variant>/`. |
| `prepareDex<V>` | `mkdir` | `mkdir -p <build>/<variant>/dex/`. |
| `linkResources<V>` | `aapt2` | `aapt2 link -o <variant>/resources.apk --manifest <manifest> -I <android.jar> --java <variant>/R --custom-package <ns> --min-sdk-version <minSdk> --target-sdk-version <targetSdk> [--rename-manifest-package <ns><suffix>] <res.zip>`. |
| `seedApk<V>` | `cp` | `cp <variant>/resources.apk <variant>/app-<variant>.apk`. |
| `compile<V>` | `javac` | `javac --release 17 -d <variant>/classes -cp <android.jar>:<classpath> -sourcepath <variant>/R <sources> <variant>/R/<namespace>/R.java`. |
| `jarClasses<V>` | `jar` | `jar cf <variant>/classes.jar -C <variant>/classes .`. |
| `compileDex<V>` | `java` | `java -cp <build-tools>/lib/d8.jar com.android.tools.r8.D8 --lib <android.jar> --min-api <minSdk> --output <variant>/dex <variant>/classes.jar`. |
| `packageApk<V>` | `jar` | `jar uf <variant>/app-<variant>.apk -C <variant>/dex .`. |
| `signApk<V>` | `apksigner` | `apksigner sign --ks <keystore> --ks-key-alias <alias> --ks-pass file:<ks-password> --key-pass file:<key-password> <variant>/app-<variant>.apk`. Only when `signing {}` is present. |

Tasks declare their real inputs and outputs, so the host's fingerprinting
skips a task until its own sources change, and a changed resource cascades
only through the tasks that consume resources. The platform jar is
deliberately **not** an input: it is a large, externally-fixed artifact
the build never modifies, and hashing it on every run would buy nothing —
a change of SDK root already changes the configuration hash, which reruns
the graph.

**Known limitation:** the host fingerprints declared task inputs and
outputs, but does not verify output existence on disk. If an output is
deleted externally (e.g. `rm -rf build/`), the task is still considered
UP-TO-DATE until an input changes. A full clean requires deleting the
fingerprint store (`.uliab/state.json`).

`--release 17` is pinned on the javac invocation because d8 rejects class
files newer than its supported range; pinning keeps the output identical
regardless of the JDK the host runs.

## Manifest

The plugin declares `javac`, `aapt2`, `jar`, `java`, `mkdir`, `cp`, and
`apksigner` as the tools of its run-tool tasks, per the host's
manifest-declared-tools check. It reports ABI `0.6`.
