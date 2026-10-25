#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds the Wasm guest (and the host build of the JNI library, with that guest embedded) twice,
# from two copies of the tracked files in directories with different names and different target
# directories, and fails unless the SHA-256 of each result is the same. Paths, time and file
# order are the usual sources of a different hash; this catches them.
#
#   tools/check-reproducible.sh            # guest + libarca_jni.so (host)
#   ARCA_REPRO_GUEST_ONLY=1 tools/check-reproducible.sh
#   ARCA_REPRO_UNTRACKED=1 tools/check-reproducible.sh   # also copy files not committed yet
#
# Extra cargo flags (for example --offline) can be passed in ARCA_CARGO_FLAGS. The Android
# libraries need the NDK and are compared by the release workflow, not here.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

copies=("$work/first/arca" "$work/second/arca-copy-with-another-name")
guest=target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm
for dir in "${copies[@]}"; do
    mkdir -p "$dir"
    # ARCA_REPRO_UNTRACKED=1 also copies new files that are not committed yet and tolerates files
    # deleted but not yet committed (a pre-commit check of the working tree).
    tarflags=(); list=(git ls-files -z)
    if [ -n "${ARCA_REPRO_UNTRACKED:-}" ]; then list=(git ls-files -z --cached --others --exclude-standard); tarflags=(--ignore-failed-read); fi
    (cd "$root" && "${list[@]}" | tar "${tarflags[@]}" --null -T - -cf -) | tar -xf - -C "$dir"
    (
        cd "$dir"
        export CARGO_TARGET_DIR="$dir/target"
        # shellcheck disable=SC2086
        cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest --locked ${ARCA_CARGO_FLAGS:-} >/dev/null 2>&1
        if [ -z "${ARCA_REPRO_GUEST_ONLY:-}" ]; then
            # shellcheck disable=SC2086
            ARCA_GUEST_WASM="$dir/$guest" cargo build --release -p arca-jni --locked ${ARCA_CARGO_FLAGS:-} >/dev/null 2>&1
        fi
    )
done

status=0
compare() {
    local rel="$1" a b
    a=$(sha256sum "${copies[0]}/$rel" | cut -d' ' -f1)
    b=$(sha256sum "${copies[1]}/$rel" | cut -d' ' -f1)
    if [ "$a" = "$b" ]; then
        echo "same    $a  $(basename "$rel")"
    else
        echo "DIFFER  $(basename "$rel")"
        echo "        $a"
        echo "        $b"
        status=1
    fi
}
compare "$guest"
[ -n "${ARCA_REPRO_GUEST_ONLY:-}" ] || compare target/release/libarca_jni.so
exit "$status"
