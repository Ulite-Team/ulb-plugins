# `ulite/android` plugin reference

The `ulite/android` plugin compiles the module's Java sources against the
Android platform jar for its declared `compileSdk`, merges the module's
resources with `aapt2`, dexes the classes with `d8`, and assembles the
APK. It implements the compile-and-package part of what
`Uliab/docs/architecture.md §5.2` assigns to `ulite/android`; the variant
matrix and manifest merging are future slices of the same plugin.

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
never modify it. A module-declared `sdkDir` is preopened too, so a
per-module SDK that differs from the host's root is discoverable the same
way: the host preopens both the resolved root and the module's own path.

## Module block

Inside the module's top-level block, the `android {}` sub-block owns the
following keys:

| Key | Type | Meaning |
|---|---|---|
| `compileSdk` | integer | The API level to compile against. Required. `configure` looks for the matching platform jar. |
| `minSdk` | integer | The minimum API level the APK runs on; `aapt2 link` records it and `d8` uses it as `--min-api`. Required. |
| `targetSdk` | integer, optional | The API level the APK targets; defaults to `compileSdk`. A supplied value that is not an integer is a configure error — it never silently falls back to `compileSdk`. |
| `namespace` | string | The package the generated `R` class lives in, handed to `aapt2 link` as `--custom-package`. Required. |
| `sources` | list of strings | `.java` files to compile. At least one entry is required. Kotlin sources are not supported yet. |
| `classesDir` | string | Directory `javac` writes `.class` files to. |
| `manifest` | string | The `AndroidManifest.xml` `aapt2 link` merges and packages. Required. |
| `resDir` | string | The `res/` directory `aapt2 compile` merges. Required. |
| `apk` | string | The APK the module produces. Required. |
| `sdkDir` | string, optional | Per-module SDK root, overriding the host-injected `androidSdkDir`. Relative paths resolve against the project directory, like every other block path. |

The values are resolved against the project directory the host injects
(`projectDir`); absolute paths are used as written.

Example:

```text
android {
  compileSdk = 36
  minSdk = 21
  namespace = "com.example.ulite"
  sources = ["src/Main.java"]
  classesDir = "build/classes"
  manifest = "AndroidManifest.xml"
  resDir = "res"
  apk = "build/app-debug.apk"
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
the injected `projectDir`; the module's `apk` is the only declared path
outside that tree:

- `build/android/res.zip` — the merged resources, `aapt2 compile` output.
- `build/android/resources.apk` — the linked resources APK.
- `build/android/R/` — the generated `R.java` (`<namespace>/R.java`).
- `build/classes.jar` — the compiled classes, archived for d8.
- `build/dex/` — the d8 output, `classes.dex` (plus `classes2.dex`, ... for a module that overflows one dex file).

## Registered tasks

The tasks form the packaging chain, each depending on the ones that
produce its inputs:

| Task | Tool | Action |
|---|---|---|
| `prepareBuildDir` | `mkdir` | `mkdir -p <build>/android` (aapt2 creates neither its output parent nor the dex dir). |
| `prepareApkDir` | `mkdir` | `mkdir -p <apk parent>` — the apk may live outside `<build>/android`, and the `prepareApk` copy cannot create its own parent. |
| `mergeResources` | `aapt2` | `aapt2 compile --dir <resDir> -o <build>/android/res.zip`. |
| `linkResources` | `aapt2` | `aapt2 link -o <build>/android/resources.apk --manifest <manifest> -I <android.jar> --java <build>/android/R --custom-package <namespace> --min-sdk-version <minSdk> --target-sdk-version <targetSdk> <res.zip>`. |
| `prepareApk` | `cp` | `cp <build>/android/resources.apk <apk>`, seeding the module's apk before the dex is grafted (`jar uf` refuses a missing archive). Depends on `prepareApkDir`. |
| `compile` | `javac` | `javac --release 17 -d <classesDir> -cp <android.jar>:<classpath.compile> -sourcepath <build>/android/R <sources> <build>/android/R/<namespace>/R.java`. |
| `jarClasses` | `jar` | `jar cf <build>/classes.jar -C <classesDir> .`, since d8 accepts archives but not directories. |
| `prepareDex` | `mkdir` | `mkdir -p <build>/dex`. |
| `compileDex` | `java` | `java -cp <build-tools>/lib/d8.jar com.android.tools.r8.D8 --lib <android.jar> --min-api <minSdk> --output <build>/dex <build>/classes.jar`. |
| `packageApk` | `jar` | `jar uf <apk> -C <build>/dex .`, grafting every `classes*.dex` d8 emitted onto the resources. Depends on `prepareApk` so it never runs before the seeded apk exists. |

Tasks declare their real inputs and outputs, so the host's fingerprinting
skips a task until its own sources change, and a changed resource cascades
only through the tasks that consume resources. The platform jar is
deliberately **not** an input: it is a large, externally-fixed artifact
the build never modifies, and hashing it on every run would buy nothing —
a change of SDK root already changes the configuration hash, which reruns
the graph.

`--release 17` is pinned on the javac invocation because d8 rejects class
files newer than its supported range; pinning keeps the output identical
regardless of the JDK the host runs.

## Manifest

The plugin declares `javac`, `aapt2`, `jar`, `java`, `mkdir`, and `cp`
as the tools of its run-tool tasks, per the host's
manifest-declared-tools check. It reports ABI `0.5`.
