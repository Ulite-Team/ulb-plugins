# `ulite/jvm` plugin reference

The `ulite/jvm` plugin compiles the module's Java and Kotlin sources,
packages the result into a jar, and optionally compiles and runs the
module's tests. A module that declares `ksp` dependencies gets the KSP2
command-line tool run against its Kotlin sources before compilation, with
the generated sources feeding the compile tasks (generate → compile →
package). It is the plain-JVM slice of what `Uliab/docs/architecture.md §5.1`
assigns to the `ulite/jvm` plugin; per-scope classpath semantics beyond
compile/test are future milestones of the same plugin.

## Module block

Inside the module's top-level block, the `jvm {}` sub-block owns the
following keys:

| Key | Type | Meaning |
|---|---|---|
| `sources` | list of strings | `.java` and/or `.kt` files to compile. At least one entry is required. |
| `classesDir` | string | Directory the compilers write `.class` files to. |
| `jarFile` | string | Output jar path the `assemble` task produces. |
| `testSources` | list of strings, optional | `.java` test files to compile. Kotlin test compilation is not supported yet. Requires `testClassesDir` and one of `testClass`/`testRunner`. |
| `testClassesDir` | string, optional | Directory `javac` writes test `.class` files to. |
| `testClass` | string, optional | Fully qualified class with a `main` method that the `test` task runs. Mutually exclusive with `testRunner`. |
| `testArgs` | list of strings, optional | Extra arguments passed to `testClass` after the class name (for framework runners such as `org.junit.runner.JUnitCore` or the JUnit Platform console launcher). |
| `testRunner` | `"junit-platform"`, optional | Makes the plugin generate a JUnit Platform Launcher-API runner that discovers and runs the tests compiled into `testClassesDir`. Mutually exclusive with `testClass`. |

`testSources` and `testClassesDir` stand or fall together, and the run
target is exactly one of `testClass` or `testRunner`. The values are
resolved against the project directory the host injects (`projectDir`);
absolute paths are used as written.

### `testRunner = "junit-platform"` mode

The plugin writes `build/generated-test-src/ulite/TestRunner.java` (a
`write-file` task, so the generated source regenerates when the plugin
changes) and compiles it together with the test sources. The `test` task
then runs `ulite.TestRunner` with the test classes directory as its one
argument; the runner scans that directory through the JUnit Platform
Launcher API and executes whatever test engine is on the classpath
(Jupiter, Vintage, ...). No class list in `testArgs` and no
`junit-platform-console-standalone` jar are needed, and the Launcher API
is stable across JUnit 5 versions.

The module's `testImplementation` dependencies must supply the launcher
and at least one engine, e.g. for Jupiter:

```text
deps {
  implementation "org.slf4j:slf4j-api:2.0.16"
  testImplementation "org.junit.platform:junit-platform-launcher:1.11.4"
  testImplementation "org.junit.jupiter:junit-jupiter:5.11.4"
}

jvm {
  sources = ["src/App.java"]
  classesDir = "build/classes"
  jarFile = "build/app.jar"
  testSources = ["src/AppTest.java"]
  testClassesDir = "build/test-classes"
  testRunner = "junit-platform"
}
```

The generated runner exits non-zero when any test failed or errored, so
the `test` task fails the build on a broken assertion. Because engines
are discovered from the classpath, the same runner also runs JUnit 4
tests when `org.junit.vintage:junit-vintage-engine` is resolved instead
of the Jupiter engine.

Example:

```text
jvm {
  sources = ["src/App.java", "src/Util.kt"]
  classesDir = "build/classes"
  jarFile = "build/app.jar"
  testSources = ["src/AppTest.java"]
  testClassesDir = "build/test-classes"
  testClass = "com.example.AppTest"
}
```

Example with a JUnit 4 suite, where the runner comes from the
`testImplementation` dependencies and the `testArgs` name the classes it
should execute:

```text
deps {
  implementation "org.slf4j:slf4j-api:2.0.16"
  testImplementation "junit:junit:4.13.2"
}

jvm {
  sources = ["src/App.java", "src/Util.kt"]
  classesDir = "build/classes"
  jarFile = "build/app.jar"
  testSources = ["src/AppTest.java"]
  testClassesDir = "build/test-classes"
  testClass = "org.junit.runner.JUnitCore"
  testArgs = ["com.example.AppTest"]
}
```

`JUnitCore` exits non-zero when a test fails, so the `test` task fails
the build on a broken assertion. The JUnit Platform console launcher
(`org.junit.platform.console.ConsoleLauncher`, from the
`junit-platform-console-standalone` artifact) works the same way, with
its own `--scan-class-path` arguments in `testArgs`.

## Host-injected keys

The host supplies these alongside the module model; the plugin reads
them but they are not part of the `jvm {}` block:

| Key | Meaning |
|---|---|
| `projectDir` | The project directory the build was started for. |
| `classpath.compile` | Jar paths resolved from the module's `deps {}` block for the compile scope. |
| `classpath.processor` | Jar paths resolved from the module's `ksp` declarations, headed by the KSP2 toolchain jar (`com.google.devtools.ksp:symbol-processing-aa`) whose `com.google.devtools.ksp.cmdline.KSPJvmMain` main class runs the tool. Present only when the module declares `ksp` deps. |
| `classpath.testCompile` | Jar paths for compiling tests (when the test keys are set). |
| `classpath.testRuntime` | Jar paths for running tests (when the test keys are set). |

## KSP support

A `ksp` declaration in the module's `deps {}` block routes the dependency
into the `classpath.processor` bucket instead of the compile/runtime
buckets. When that bucket is non-empty the plugin registers a `ksp` task
and requires the module to have Kotlin sources (KSP has nothing to
process otherwise, and the plugin rejects the configuration). The task
runs the KSP2 command-line tool:

```
java -cp <classpath.processor> com.google.devtools.ksp.cmdline.KSPJvmMain \
  -jvm-target 11 -module-name main -source-roots=<kotlin sources, ':'-joined> \
  -project-base-dir <projectDir> -output-base-dir <projectDir>/build \
  -caches-dir <projectDir>/build/ksp-caches -class-output-dir <projectDir>/build/ksp-classes \
  -kotlin-output-dir <projectDir>/build/generated/ksp/kotlin \
  -java-output-dir <projectDir>/build/generated/ksp/java \
  -resource-output-dir <projectDir>/build/ksp-resources \
  -language-version 2.0 -api-version 2.0 [-libraries <classpath.compile>] \
  <classpath.processor>
```

The trailing processor classpath is how KSP discovers
`ProcessorProvider` services. Generated Kotlin lands in
`build/generated/ksp/kotlin`, which the `compile-kotlin` task adds to its
source list, so the ordering is ksp → compile → package. Generated Java
in `build/generated/ksp/java` is written but not compiled: `kotlinc`
emits no classes for `.java` sources, and `javac` needs an explicit file
list that a task's static inputs cannot enumerate after KSP has run.

## Registered tasks

| Task | Tool | Action |
|---|---|---|
| `ksp` | `java` | Runs the KSP2 command-line tool over the Kotlin sources (only when the module has `ksp` deps; see above) |
| `compile` | `javac` | `javac -d <classesDir> [-cp <classpath.compile, colon-separated>] <java sources>` (only when the module has `.java` sources; waits for `ksp` when present) |
| `compile-kotlin` | `kotlinc` | `kotlinc -d <classesDir> [-cp <classpath.compile:classesDir>] <kotlin sources + generated/ksp/kotlin>` (only when the module has `.kt` sources; waits for `compile` when both source sets exist and for `ksp` when present) |
| `assemble` | `jar` | `jar cf <jarFile> -C <classesDir> .` (after the present compile tasks) |
| `generate-test-runner` | — | Writes `build/generated-test-src/ulite/TestRunner.java` (only with `testRunner = "junit-platform"`) |
| `compile-tests` | `javac` | `javac -d <testClassesDir> -cp <classpath.testCompile:classesDir> <testSources>` — plus the generated runner source when `testRunner` is set (only when the test keys are set) |
| `test` | `java` | `java -cp <classpath.testRuntime:testClassesDir:classesDir> <testClass> [<testArgs>]`, or `java -cp <...> ulite.TestRunner <testClassesDir>` with `testRunner` (after `compile-tests`) |

Classpaths are joined with `:` (the separator of the unix hosts the
toolchain targets). An empty main classpath omits `-cp` entirely.

`compile`/`compile-kotlin` declare their source files as inputs and
`classesDir` as output; `ksp` declares the Kotlin sources as inputs and
the generated directories as outputs; `assemble` declares `classesDir` as
input and the jar as output. `generate-test-runner` produces the generated source
and carries no inputs — its fingerprint folds the runner text, so it
regenerates exactly when the plugin's runner changes. `compile-tests`
declares the test sources (and the generated source, when present) as
inputs and depends on the main compile tasks plus `generate-test-runner`;
`test` declares the test and main class directories as inputs and depends
on `compile-tests`. Because a task only counts as up-to-date when every
dependency is up-to-date, a changed source reruns the dependent chain
even though directory inputs are read as opaque by the fingerprinter.

`test` declares no outputs, so the host never treats it as the producer
of a file; a rebuild with unchanged inputs and dependencies skips it,
and any source change reruns it through the dependency chain.

## Manifest

The plugin declares `javac`, `kotlinc`, `jar`, and `java` as the tools
of its run-tool tasks, per the host's manifest-declared-tools check.
