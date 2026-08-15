# `ulite/jvm` plugin reference

The `ulite/jvm` plugin compiles the module's Java sources and packages
the result into a jar. It is the plain-Java slice of what
`ARCHITECTURE.md §5.1` assigns to the `ulite/jvm` plugin; Kotlin
compilation, the `test` task, and per-scope classpath semantics beyond
`compile` are future milestones of the same plugin.

## Module block

Inside the module's top-level block, the `jvm {}` sub-block owns the
following keys:

| Key | Type | Meaning |
|---|---|---|
| `sources` | list of strings | `.java` files to compile. At least one entry is required. |
| `classesDir` | string | Directory `javac` writes `.class` files to. |
| `jarFile` | string | Output jar path the `assemble` task produces. |

The values are resolved against the project directory the host injects
(`projectDir`); absolute paths are used as written.

Example:

```text
jvm {
  sources = ["src/App.java", "src/Main.java"]
  classesDir = "build/classes"
  jarFile = "build/app.jar"
}
```

## Host-injected keys

The host supplies these alongside the module model; the plugin reads
them but they are not part of the `jvm {}` block:

| Key | Meaning |
|---|---|
| `projectDir` | The project directory the build was started for. |
| `classpath.compile` | Jar paths resolved from the module's `deps {}` block for the compile scope. |

## Registered tasks

| Task | Tool | Action |
|---|---|---|
| `compile` | `javac` | `javac -d <classesDir> [-cp <classpath.compile, colon-separated>] <sources>` |
| `assemble` | `jar` | `jar cf <jarFile> -C <classesDir> .` (depends on `compile`) |

The classpath is joined with `:` (the separator of the unix hosts the
toolchain targets). An empty classpath omits `-cp` entirely.

`compile` declares the source files as its inputs and `classesDir` as
its output; `assemble` declares `classesDir` as its input and the jar as
its output, and depends on `compile`. Because a task only counts as
up-to-date when every dependency is up-to-date, a changed source reruns
both tasks even though `assemble`'s declared input is a directory the
fingerprinter reads as opaque.

## Manifest

The plugin declares `javac` and `jar` as the tools of its run-tool
tasks, per the host's manifest-declared-tools check.
