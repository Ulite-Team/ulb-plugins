#!/usr/bin/env bash
# Builds the ksp-hello fixture processor and installs it into a file://
# maven repository layout at $1 (e.g. /tmp/ksp-m2), so a project can
# resolve it with `deps { ksp "ulite:ksp-hello:1.0.0" }` plus the `--repo`
# flag. The processor implements the real KSP API (symbol-processing-api)
# and registers itself via META-INF/services, so the plugin's ksp task
# discovers it from the processor classpath.
set -euo pipefail

dest="${1:?usage: build-fixture.sh <dest-m2-dir>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
version="1.0.0"
api_version="2.2.0-2.0.2"

api_jar="$here/out/symbol-processing-api-$api_version.jar"
mkdir -p "$here/out"
if [[ ! -f "$api_jar" ]]; then
  curl -sSLo "$api_jar" \
    "https://repo1.maven.org/maven2/com/google/devtools/ksp/symbol-processing-api/$api_version/symbol-processing-api-$api_version.jar"
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
kotlinc \
  -cp "$api_jar" \
  -d "$stage/ksp-hello.jar" \
  "$here/src/annotation/Hello.kt" \
  "$here/src/processor/HelloProcessor.kt"
mkdir -p "$stage/META-INF/services"
printf 'ulite.hello.HelloProcessorProvider\n' \
  > "$stage/META-INF/services/com.google.devtools.ksp.processing.SymbolProcessorProvider"
jar uf "$stage/ksp-hello.jar" \
  -C "$stage" META-INF

mkdir -p "$dest/ulite/ksp-hello/$version"
cp "$stage/ksp-hello.jar" "$dest/ulite/ksp-hello/$version/ksp-hello-$version.jar"
cat > "$dest/ulite/ksp-hello/$version/ksp-hello-$version.pom" <<POM
<?xml version="1.0" encoding="UTF-8"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>ulite</groupId>
  <artifactId>ksp-hello</artifactId>
  <version>$version</version>
  <packaging>jar</packaging>
</project>
POM

echo "installed ksp-hello $version into $dest"
