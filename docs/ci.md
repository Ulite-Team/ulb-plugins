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

### `android-build` — the SDK capability end to end

Builds a Java module against a **hermetic fake SDK**: a valid empty zip
as `android.jar`, empty `aapt2`/`d8` markers in two `build-tools`
releases, no real SDK download. This proves the whole SDK capability
chain without network or gigabytes:

- the host's `--android-sdk` resolves the root and preopens it into the
  plugin's filesystem;
- `configure` discovers `android-36/android.jar` and picks the highest
  complete `build-tools` release (36.0.0 over 35.0.0);
- the `compile` task runs real `javac` against the platform jar
  (`1 ran, 0 up-to-date`, `Main.class` produced), and a second build from
  a different working directory is `0 ran, 1 up-to-date`;
- a `compileSdk` the SDK does not have fails at configure time with
  `no android.jar for compileSdk 99`, proving discovery ran inside the
  plugin rather than javac failing opaquely.

Like every job here, `android-build` checks out Uliab from `main`, so on
this repo's own PRs it only goes green once the Uliab host changes it
exercises (`--android-sdk`, `androidSdkDir` injection, the read-only
preopen) have landed on Uliab `main` — merge Uliab's SDK PR first.

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
