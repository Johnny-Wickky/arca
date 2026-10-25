#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Runs the JNI tests on a desktop JVM with -Xcheck:jni:
#
#   NativeTest    every native method, bad handles and descriptors, cancellation, threads;
#   ContractTest  real documents (and seeded mutations of them) through the real engine and
#                 the app's real JSON model.
#
#   ARCA_GUEST_WASM=path/to/arca_guest_xray.wasm [ARCA_CORPUS_DIR=dir] [ARCA_FUZZ_ITERS=n] \
#       android/host-test/run.sh
#
# Builds libarca_jni.so for the host (with the guest embedded), compiles the Java sources,
# and fails on any complaint of the JVM's JNI checker.
set -euo pipefail
cd "$(dirname "$0")/../.."

: "${ARCA_GUEST_WASM:?set ARCA_GUEST_WASM to the built arca_guest_xray.wasm}"
export ARCA_GUEST_WASM

# shellcheck disable=SC2086
cargo build --release -p arca-jni ${ARCA_CARGO_FLAGS:-}
# shellcheck disable=SC2086
target_dir=$(cargo metadata --format-version 1 --no-deps ${ARCA_CARGO_FLAGS:-} | cargo run --quiet --locked -p arca-devtools ${ARCA_CARGO_FLAGS:-} -- json-get target_directory)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
javac -Xlint:all -Werror -d "$work" \
    android/app/src/main/java/org/arca/viewer/Native.java \
    android/app/src/main/java/org/arca/viewer/core/*.java \
    android/host-test/NativeTest.java android/host-test/ContractTest.java android/host-test/StatsNativeTest.java

fail=0
for test in NativeTest ContractTest StatsNativeTest; do
    echo "== $test"
    log="$work/$test.log"
    set +e
    java -Xcheck:jni -XX:+CheckJNICalls \
        --add-opens java.base/java.io=ALL-UNNAMED \
        -Djava.library.path="$target_dir/release" \
        -cp "$work" "$test" 2>&1 | grep -v '^Picked up JAVA_TOOL_OPTIONS' | tee "$log"
    status=${PIPESTATUS[0]}
    set -e
    if grep -E 'WARNING in native method|FATAL ERROR in native method|JNI (ERROR|WARNING)' "$log"; then
        echo "run.sh: -Xcheck:jni reported problems in $test (lines above)" >&2
        fail=1
    fi
    [[ $status == 0 ]] || fail=1
done
exit "$fail"
