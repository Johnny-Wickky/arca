#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds the Wasm guest, the three libarca_jni.so and the Android package twice, from two copies
# of the tracked files in directories with different names, and fails unless the guest, each
# library and the *unsigned* package (the aligned package before apksigner) have the same SHA-256
# in both. A signature differs with the key and is not part of the comparison.
#
#   ANDROID_HOME=... NDK_HOME=... tools/android/check-reproducible-apk.sh
#
# Needs the Android SDK (build-tools, the target platform) and the NDK, like the package job.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
: "${ANDROID_HOME:?set ANDROID_HOME}" "${NDK_HOME:?set NDK_HOME}"

copies=("$work/first/arca" "$work/second/arca-copy-with-another-name")
guest=target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm
for dir in "${copies[@]}"; do
    mkdir -p "$dir"
    (cd "$root" && git ls-files -z | tar --null -T - -cf -) | tar -xf - -C "$dir"
    (
        cd "$dir"
        . "$dir/tools/guest-env.sh"
        arca_guest_env "$dir"
        arca_guest_build >/dev/null 2>&1
        # build-native.sh sets its own flags; the guest's must not leak into the libraries.
        unset RUSTFLAGS
        bash tools/android/build-native.sh >/dev/null
        ARCA_NATIVE_DIR="$dir/native" ARCA_OUT="$dir/dist" ARCA_THROWAWAY_KEY=1 \
            bash tools/android/build-apk.sh >/dev/null
    )
done

status=0
compare() {
    local what="$1" a="$2" b="$3"
    if [ "$a" = "$b" ]; then
        printf 'same   %s  %s\n' "${a:0:16}…" "$what"
    else
        printf 'DIFFER %s\n   first  %s\n   second %s\n' "$what" "$a" "$b" >&2
        status=1
    fi
}
sum() { sha256sum "$1" | cut -d' ' -f1; }
compare "the Wasm guest" "$(sum "${copies[0]}/$guest")" "$(sum "${copies[1]}/$guest")"
for abi in arm64-v8a armeabi-v7a x86_64; do
    compare "libarca_jni.so ($abi)" "$(sum "${copies[0]}/native/$abi/libarca_jni.so")" "$(sum "${copies[1]}/native/$abi/libarca_jni.so")"
done
u0="$(cut -d' ' -f1 "${copies[0]}"/dist/arca-*.unsigned.sha256)"
u1="$(cut -d' ' -f1 "${copies[1]}"/dist/arca-*.unsigned.sha256)"
compare "the unsigned package (everything but the signature)" "$u0" "$u1"
[ "$status" = 0 ] && echo "reproducible: the guest, the libraries and the unsigned package are the same in both builds"
exit "$status"
