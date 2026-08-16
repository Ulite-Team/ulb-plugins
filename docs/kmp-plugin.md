# `ulite/kmp` plugin reference

The `ulite/kmp` plugin compiles a Kotlin multiplatform module's shared and
JVM source sets into a jar. The module's `kmp {}` block declares source
sets — blocks carrying a `sources` list and optionally their own `deps {}`
block — and target configs, blocks carrying neither. The host resolves
each source set's `deps {}` block independently and injects the results as
`classpathSourceSets`, so a source set's dependency list is applied only
to the compilation of that source set. It is the JVM-target slice of what
`Uliab/docs/architecture.md §5.1` assigns to the `ulite/kmp` plugin; other
targets (android, ios, ...) and per-target `implementation`/`api` scopes
are future milestones of the same plugin.

## Module block

Inside the module's top-level block, the `kmp {}` sub-block owns the
following entries:

| Key | Type | Meaning |
|---|---|---|
| `<sourceSet> { ... }` | block | A block carrying a `sources` list and optionally a `deps {}` block. The supported source sets are `commonMain` (shared by every target) and `jvmMain` (JVM-only). Any other source set name is rejected with an error. |
| `jvm { ... }` | block | The JVM target config, below. |

Source sets compile in hierarchy order — `commonMain` first, then
`jvmMain` — against the union, in that order, of their resolved compile
classpaths. A jar declared in both the shared and the platform source set
appears once.

### `jvm` target config

| Key | Type | Meaning |
|---|---|---|
| `classesDir` | string | Directory the compilers write `.class` files to. |
| `jarFile` | string | Output jar path the `assemble` task produces. |

A `kmp {}` block must declare exactly the `jvm` target among the target
configs; any other known target (`android`, `ios`, ...) is rejected with
an error stating that this slice compiles the `jvm` target only. An entry
that is neither a source set (no `sources`/`deps`) nor a known target is
rejected as well, so a misspelled block cannot be silently ignored.

Source files are explicit `.java`/`.kt` paths, matching the `jvm` plugin's
model; a path with any other extension is rejected. Values are resolved
against the project directory the host injects (`projectDir`); absolute
paths are used as written.

Example:

```text
kmp {
  commonMain {
    sources = ["src/commonMain/Shared.kt"]
    deps { implementation "com.example:lib:1.0" }
  }
  jvmMain {
    sources = ["src/jvmMain/App.java", "src/jvmMain/Main.kt"]
  }
  jvm {
    classesDir = "build/classes"
    jarFile = "build/app.jar"
  }
}
```

## Host-injected keys

The host supplies these alongside the module model; the plugin reads them
but they are not part of the `kmp {}` block:

| Key | Meaning |
|---|---|
| `projectDir` | The project directory the build was started for. |
| `classpathSourceSets` | A map from source-set path under the model (`kmp.commonMain`, `kmp.jvmMain`) to that source set's resolved `classpath` (the same `compile`/`runtime`/... shape as the module-level `classpath`). The host injects an entry only for source sets that declare a `deps {}` block; a missing entry reads as an empty classpath. |

The module's own top-level `deps {}` block is not consulted by this
plugin: dependencies belong to source sets.

## Registered tasks

| Task | Tool | Action |
|---|---|---|
| `compile` | `javac` | `javac -d <classesDir> [-cp <classpathSourceSets[kmp.commonMain+kmp.jvmMain].compile, colon-separated>] <java sources>` (only when the module has `.java` sources) |
| `compile-kotlin` | `kotlinc` | `kotlinc -d <classesDir> [-cp <merged compile classpath:classesDir>] <kotlin sources>` (only when the module has `.kt` sources; waits for `compile` when both coexist, so the Kotlin resolves the module's own Java classes) |
| `assemble` | `jar` | `jar cf <jarFile> -C <classesDir> .` (after the present compile tasks) |

Classpaths are joined with `:` (the separator of the unix hosts the
toolchain targets). An empty classpath omits `-cp` entirely. The
`classesDir` joins the Kotlin compile task's classpath so Kotlin sees the
Java classes the module compiled itself, the same way it would see a
dependency jar.

Each compile task declares its source files as inputs and `classesDir` as
output; `assemble` declares `classesDir` as input and the jar as output.
Because a task only counts as up-to-date when every dependency is
up-to-date, an edit to any source file reruns that file's compile task and
then `assemble`.

## Manifest

The plugin declares `javac`, `kotlinc`, and `jar` as the tools of its
run-tool tasks, per the host's manifest-declared-tools check.
