# The KSP fixture (`fixtures/ksp-hello`)

A real KSP (Kotlin Symbol Processing) processor used to prove the jvm
plugin's KSP2 support end to end — no stub, no mock: the processor
implements the actual `symbol-processing-api` interfaces.

## Sources

| File | Contents |
|---|---|
| `src/annotation/Hello.kt` | A `@Hello` annotation (class target, runtime retention) |
| `src/processor/HelloProcessor.kt` | A `SymbolProcessor` that finds every class annotated `@Hello`, generates a Kotlin file `ulite.hello.generated.HelloGenerated` declaring `val greetings: List<String>` listing those class names, and registers itself via a `SymbolProcessorProvider` |

## `build-fixture.sh <dest-m2-dir>`

Builds the processor and installs it into a `file://` Maven repository
layout at the given directory, so a project can resolve it with
`deps { ksp "ulite:ksp-hello:1.0.0" }` plus the host's `--repo` flag
(which prepends a custom repository ahead of Google/Maven Central).

Steps:

1. Downloads `symbol-processing-api-2.2.0-2.0.2.jar` from Maven Central
   into `fixtures/ksp-hello/out/` (cached by presence).
2. Compiles both Kotlin sources with `kotlinc -cp <api jar>`.
3. Writes `META-INF/services/com.google.devtools.ksp.processing.SymbolProcessorProvider`
   naming `ulite.hello.HelloProcessorProvider` and folds it into the jar —
   this is how the KSP2 tool discovers processors from the processor
   classpath.
4. Lays out `<dest>/ulite/ksp-hello/1.0.0/ksp-hello-1.0.0.jar` plus a
   minimal POM (the resolver only needs group/artifact/version/packaging).

## Why the fixture exists

The `jvm-ksp` CI job builds it into `/tmp/ksp-m2`, then builds a module
declaring `ksp "ulite:ksp-hello:1.0.0"` and
`ksp "com.google.devtools.ksp:symbol-processing-aa:2.2.0-2.0.2"` (the
KSP2 toolchain jar, resolved from Maven Central). The `ksp` task runs the
real tool; the generated `HelloGenerated.kt` feeds `kotlinc`; the final
jar's `main` prints the generated value, which CI asserts. It proves the
processor-classpath mechanics of the plugin with a processor that behaves
exactly like a production one — including error handling that matters
(e.g. an empty annotation match returns an empty list rather than failing).

## `out/`

Downloaded jars land in `fixtures/ksp-hello/out/`, which is gitignored
(`/fixtures/ksp-hello/out/`). Everything else is committed.
