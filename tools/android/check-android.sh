#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Checks the Android app without the Android build tools (aapt2, d8): everything that can be
# verified from a plain JDK, the Rust arca-devtools tool and two android.jar files.
#
#   1. resources and manifest are well-formed XML, consistent between languages, complete with
#      respect to the Rust enums, and every reference resolves  (arca-devtools check-resources)
#   2. the whole app compiles against the TARGET SDK's android.jar (API 36)
#   3. against the MINIMUM SDK's android.jar (API 26) it compiles too, except for the version-
#      specific classes isolated in ui/Compat.java: a call to a newer API anywhere else is a
#      crash on Android 8 and fails here
#   4. the pure-Java core is tested on the JVM (android/core-test)
#
# Environment:
#   ANDROID_JAR_MIN      android.jar of API 26      (default: $ARCA_ANDROID_JARS/android-26/android.jar)
#   ANDROID_JAR_TARGET   android.jar of API 36      (default: $ARCA_ANDROID_JARS/android-36/android.jar)
#   ARCA_ANDROID_JARS    directory holding android-NN/android.jar
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP="$ROOT/android/app/src/main"
JARS="${ARCA_ANDROID_JARS:-}"
MIN_JAR="${ANDROID_JAR_MIN:-}"
TARGET_JAR="${ANDROID_JAR_TARGET:-}"
for d in "$JARS/android-platforms/android-26" "$JARS/android-26"; do
    [ -z "$MIN_JAR" ] && [ -f "$d/android.jar" ] && MIN_JAR="$d/android.jar"
done
for d in "$JARS/aosp-android-jar/android-36" "$JARS/android-36"; do
    [ -z "$TARGET_JAR" ] && [ -f "$d/android.jar" ] && TARGET_JAR="$d/android.jar"
done
[ -f "${MIN_JAR:-/nonexistent}" ] || { echo "check-android: android.jar for API 26 not found (set ANDROID_JAR_MIN)" >&2; exit 2; }
[ -f "${TARGET_JAR:-/nonexistent}" ] || { echo "check-android: android.jar for API 36 not found (set ANDROID_JAR_TARGET)" >&2; exit 2; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

step() { printf '\n== %s\n' "$*"; }
# The helper tool is Rust (crates/arca-devtools); cargo builds it on first use.
# shellcheck disable=SC2086
devtools() { cargo run --quiet --locked --manifest-path "$ROOT/Cargo.toml" -p arca-devtools ${ARCA_CARGO_FLAGS:-} -- "$@"; }

step "1. XML well-formedness"
if command -v xmllint >/dev/null 2>&1; then
    find "$APP" -name '*.xml' -print0 | xargs -0 xmllint --noout
    echo "ok"
else
    echo "skipped (xmllint not installed; step 2 parses every XML file with the strict reader of arca-container)"
fi

step "2. Resources: languages, engine identifiers, references"
devtools check-resources "$ROOT/android" --android-jar "$MIN_JAR" --repo-root "$ROOT"

step "3. Compile against the target SDK (API 36)"
devtools gen-r "$APP/res" org.arca.viewer "$WORK/gen"
find "$APP/java" -name '*.java' > "$WORK/sources.txt"
find "$WORK/gen" -name '*.java' >> "$WORK/sources.txt"
mkdir -p "$WORK/out36" "$WORK/out26"
javac -nowarn -Xlint:-options -source 8 -target 8 -encoding UTF-8 -Xmaxerrs 500 \
    -bootclasspath "$TARGET_JAR" -d "$WORK/out36" @"$WORK/sources.txt"
echo "ok: $(find "$WORK/out36" -name '*.class' | wc -l) classes"

step "4. Compile against the minimum SDK (API 26): newer APIs only inside Compat"
set +e
javac -nowarn -Xlint:-options -source 8 -target 8 -encoding UTF-8 -Xmaxerrs 2000 \
    -bootclasspath "$MIN_JAR" -d "$WORK/out26" @"$WORK/sources.txt" 2> "$WORK/min.log"
set -e
# Every error must be located in Compat.java (its nested ApiNN classes are guarded by SDK_INT).
errors="$(grep -E '^[^ ].*\.java:[0-9]+: error:' "$WORK/min.log" || true)"
outside="$(printf '%s\n' "$errors" | grep -v '/ui/Compat.java:' | grep -v '^$' || true)"
if [ -n "$outside" ]; then
    echo "APIs newer than API 26 are used outside ui/Compat.java:" >&2
    printf '%s\n' "$outside" >&2
    exit 1
fi
inside="$(printf '%s\n' "$errors" | grep -c '/ui/Compat.java:' || true)"
echo "ok: no use of APIs newer than 26 outside Compat.java ($inside guarded use(s) inside it)"
# Those inside Compat must sit in the nested ApiNN classes, never in the code that runs on API 26.
devtools check-compat "$APP/java/org/arca/viewer/ui/Compat.java" "$WORK/min.log"

step "5. Core tests on the JVM"
bash "$ROOT/android/core-test/run.sh"

printf '\ncheck-android: all checks passed\n'
