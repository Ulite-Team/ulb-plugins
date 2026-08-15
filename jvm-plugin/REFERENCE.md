# `ulite/jvm` plugin reference

The `ulite/jvm` plugin compiles the module's Java and Kotlin sources,
packages the result into a jar, and optionally compiles and runs the
module's tests. It is the plain-JVM slice of what `ARCHITECTURE.md §5.1`
assigns to the `ulite/jvm` plugin; JUnit-style test frameworks and
per-scope classpath semantics beyond compile/test are future milestones
of the same plugin.

## Module block

Inside the module's top-level block, the `jvm {}` sub-block owns the
following keys:

| Key | Type | Meaning |
|---|---|---|
| `sources` | list of strings | `.java` and/or `.kt` files to compile. At least one entry is required. |
| `classesDir` | string | Directory the compilers write `.class` files to. |
| `jarFile` | string | Output jar path the `assemble` task produces. |
| `testSources` | list of strings, optional | `.java` test files to compile. Kotlin test compilation is not supported yet. Requires `testClassesDir` and `testClass`. |
| `testClassesDir` | string, optional | Directory `javac` writes test `.class` files to. |
| `testClass` | string, optional | Fully qualified class with a `main` method that the `test` task runs. |

The test keys stand or fall together: all three must be set, or none.
The values are resolved against the project directory the host injects
(`projectDir`); absolute paths are used as written.

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

## Host-injected keys

The host supplies these alongside the module model; the plugin reads
them but they are not part of the `jvm {}` block:

| Key | Meaning |
|---|---|
| `projectDir` | The project directory the build was started for. |
| `classpath.compile` | Jar paths resolved from the module's `deps {}` block for the compile scope. |
| `classpath.testCompile` | Jar paths for compiling tests (when the test keys are set). |
| `classpath.testRuntime` | Jar paths for running tests (when the test keys are set). |

## Registered tasks

| Task | Tool | Action |
|---|---|---|
| `compile` | `javac` | `javac -d <classesDir> [-cp <classpath.compile, colon-separated>] <java sources>` (only when the module has `.java` sources) |
| `compile-kotlin` | `kotlinc` | `kotlinc -d <classesDir> [-cp <classpath.compile:classesDir>] <kotlin sources>` (only when the module has `.kt` sources; waits for `compile` when both exist) |
| `assemble` | `jar` | `jar cf <jarFile> -C <classesDir> .` (after the present compile tasks) |
| `compile-tests` | `javac` | `javac -d <testClassesDir> -cp <classpath.testCompile:classesDir> <testSources>` (only when the test keys are set) |
| `test` | `java` | `java -cp <classpath.testRuntime:testClassesDir:classesDir> <testClass>` (after `compile-tests`) |

Classpaths are joined with `:` (the separator of the unix hosts the
toolchain targets). An empty main classpath omits `-cp` entirely.

`compile`/`compile-kotlin` declare their source files as inputs and
`classesDir` as output; `assemble` declares `classesDir` as input and
the jar as output. `compile-tests` declares the test sources as inputs
and depends on the main compile tasks; `test` declares the test and main
class directories as inputs and depends on `compile-tests`. Because a
task only counts as up-to-date when every dependency is up-to-date, a
changed source reruns the dependent chain even though directory inputs
are read as opaque by the fingerprinter.

`test` declares no outputs, so the host never treats it as the producer
of a file; a rebuild with unchanged inputs and dependencies skips it,
and any source change reruns it through the dependency chain.

## Manifest

The plugin declares `javac`, `kotlinc`, `jar`, and `java` as the tools
of its run-tool tasks, per the host's manifest-declared-tools check.
