# CI (`plugin-build.yml`)

A single workflow, `plugin-build`, with seven jobs on `push: [main]`,
`pull_request` (against `main`), and `workflow_dispatch`. Every job builds
real `wasm32-wasip2` components and
runs them under the real `uliab` host — there are no mocks anywhere.

## Common setup

Every job reproduces the dev layout and its consequences:

1. checkout this repo into `ulb-plugins`;
2. checkout `Ulite-Team/Uliab` into `Uliab` (the sdk path dependency
   `../../Uliab/crates/ulb-plugin-sdk` and the WIT `path:` argument both
   resolve against this layout; requires `secrets.ULITE_PAT` because the
   repos are private);
3. `dtolnay/rust-toolchain@stable` with the `wasm32-wasip2` target;
4. `Swatinem/rust-cache@v2`.

Then each job builds the plugin(s) it needs and `cargo build -p uliab`.

## The jobs

### `build-and-run` — hello-plugin runs under the host

Builds `hello-plugin`, then `uliab run <component> 'hello from CI'` and
asserts the echo string. This is the smoke test: if the component cannot
instantiate, no other job can.

### `registry-resolve` — the full resolve → verify → cache path

Seeds a local registry from the built artifact, runs
`uliab plugins resolve` twice, and asserts the first invocation reports
`(registry)` and the second reports `(cache)` — proving the client
downloaded, verified, and cached. Then runs the cached component and
asserts its output. Also asserts the committed `registry/index.json` is
well-formed (`schema_version == 1`).

### `jvm-build` — the reference plugin end to end

Three sub-scenarios against local indices pointing at the built
`jvm-plugin`:

1. **A Java module with a test suite** — `javac`/`jar`/`java` tasks:
   asserts `4 ran, 0 up-to-date`, the jar contains `App.class`, and a
   second build from a **different working directory** reports
   `0 ran, 4 up-to-date` (proving relative paths resolve against the
   injected `projectDir`, not the invocation dir). A changed source
   reruns the whole chain.
2. **A Kotlin-only module** — `kotlinc` + `jar` only, no javac task:
   `2 ran, 0 up-to-date`, jar contains `MainKt.class`.
3. **A broken Java test must fail the build** — a failing assertion
   surfaces as a task failure (see `jvm-scoped-classpath` below for the
   full version).

### `jvm-scoped-classpath` — scope buckets end to end

Resolves `deps { implementation slf4j; testImplementation junit; … }` and
asserts: `slf4j-api` reaches the compile bucket, `junit` does **not**,
`junit` reaches testCompile. Then builds and runs the JUnit 4 suite via
`testClass = JUnitCore` + `testArgs`. Two negative proofs:

- a broken assertion fails the build;
- **removing the `implementation` dep fails main compilation**, proving
  `testImplementation` jars never leak onto the main compile classpath.

### `android-build` — packaging end to end against a real SDK

Builds an android module with a manifest, `res/` layout and strings, and a
`Main.java` that references `android.*` types and `R.*` ids through the
**full packaging chain** (`9 ran, 0 up-to-date`): `aapt2 compile` → `aapt2
link` → `cp` the linked resources to the module's apk → `javac` (with
`--release 17`) → `jar cf` → `d8` → `jar uf` the dex onto the apk. It
asserts the apk really contains `AndroidManifest.xml`, the merged layout,
`resources.arsc`, and `classes.dex`, and that `R.java` was generated under
the module's `namespace`.

The SDK is real, not hermetic: ubuntu-latest ships an Android SDK with
`sdkmanager`, and the job installs the exact `platforms;android-36` and
`build-tools;36.0.0` the module declares (~150MB). A fake SDK could not
run the real aapt2/d8 chain this job is here to prove.

Incremental behavior is asserted end to end:

- an unchanged build from a **different working directory** is
  `0 ran, 9 up-to-date` (derived paths resolve against the injected
  `projectDir`, never the invocation dir);
- a resource edit reruns the resource chain and its consumers
  (`7 ran, 2 up-to-date`);
- a source edit reruns only compile and the dex/package chain
  (`4 ran, 5 up-to-date`);
- a `compileSdk` the SDK does not have fails at configure time with
  `no android.jar for compileSdk 99`, proving discovery ran inside the
  plugin rather than javac failing opaquely.

Like every job here, `android-build` checks out Uliab from `main`, so on
this repo's own PRs it only goes green once the Uliab host changes it
exercises (the aapt2 tool, ABI 0.5, directory-tree fingerprints, and the
SDK injection) have landed on Uliab `main` — merge Uliab's packaging PR
first.

### `jvm-runner-discovery` — the generated JUnit Platform runner

With `testRunner = "junit-platform"`: asserts the plugin writes
`build/generated-test-src/ulite/TestRunner.java`, compiles it, runs
Jupiter tests (5 tasks), a broken assertion fails the build with
`AssertionFailedError` in the output, a test-source change reruns **only**
the test chain (`2 ran, 3 up-to-date`), and the app jar contains `App.class`
but **not** `AppTest` (tests never packaged).

### `jvm-ksp` — KSP2 end to end

Builds the `ksp-hello` fixture processor into a `file://` Maven layout
([ksp-fixture.md](ksp-fixture.md)), then builds a Kotlin module whose
`deps { ksp … }` route through the processor classpath: the `ksp` task
runs the real KSP2 tool, generated Kotlin feeds `kotlinc`, `assemble`
packages the jar (`3 ran, 0 up-to-date`). Asserts the generated source and
class exist, then runs the produced jar on the resolved runtime classpath
and checks its stdout — including after a source change forces a rebuild.

## What the jobs are not

- Not unit tests — the host's `build_driver`/`deps_resolve` integration
  tests cover that in the Uliab repo. These jobs are the *system* test:
  real components, real toolchain, real host.
- Not a publish pipeline — uploading release assets and updating the
  committed index is still a manual step
  ([registry.md](registry.md)). The `registry-resolve` job works around
  the missing assets by seeding a local index.
