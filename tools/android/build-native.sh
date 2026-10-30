#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds libarca_jni.so for the three ABIs (the Wasm guest must be built first) with the NDK's
# linker, 16 KiB page alignment, full RELRO and a non-executable stack, checks every library
# (tools: arca-devtools elf-check) and writes native/<abi>/libarca_jni.so and native/SHA256SUMS.
# No C or C++ is compiled: the NDK's clang only links.
#
#   NDK_HOME=... tools/android/build-native.sh
#
# Environment:
#   NDK_HOME             the Android NDK (required)
#   ANDROID_API_MIN      the API level of the linker wrapper (default 26)
#   ARCA_GUEST_WASM      the guest module to embed (default target/wasm32-unknown-unknown/guest/...)
#   ARCA_RELEASE=1       a release build (the attribution crate refuses placeholder URLs)
#
# Run from any copy of the tree: the paths are remapped to /arca, /cargo and /rustup, so that the
# libraries do not depend on where the tree lives (tools/android/check-reproducible-apk.sh).
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

: "${NDK_HOME:?set NDK_HOME to the Android NDK}"
export ARCA_GUEST_WASM="${ARCA_GUEST_WASM:-$PWD/target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm}"
[ -f "$ARCA_GUEST_WASM" ] || { echo "build-native: build the guest first (tools/build-guest.sh)" >&2; exit 1; }
bin="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
api="${ANDROID_API_MIN:-26}"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$bin/aarch64-linux-android$api-clang"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$bin/armv7a-linux-androideabi$api-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$bin/x86_64-linux-android$api-clang"
# Shared libraries are laid out for 16 KiB pages (Android 15+ devices, Google Play).
align="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
harden="-C link-arg=-Wl,-z,relro,-z,now -C link-arg=-Wl,-z,noexecstack"
remap="--remap-path-prefix=$PWD=/arca --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-D warnings $align $harden $remap"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_RUSTFLAGS="-D warnings $align $harden $remap"
export CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS="-D warnings $align $harden $remap"
# The environment's own RUSTFLAGS would take precedence over the per-target ones above.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1700000000}"

mkdir -p native
for entry in aarch64-linux-android:arm64-v8a:aarch64 armv7-linux-androideabi:armeabi-v7a:arm x86_64-linux-android:x86_64:x86_64; do
    IFS=: read -r triple abi machine <<<"$entry"
    # shellcheck disable=SC2086
    cargo build --release --locked -p arca-jni --target "$triple" ${ARCA_CARGO_FLAGS:-}
    mkdir -p "native/$abi"
    cp "target/$triple/release/libarca_jni.so" "native/$abi/"
    # Every PT_LOAD segment must be 16 KiB aligned, and the machine must be the right one.
    # shellcheck disable=SC2086
    cargo run --quiet --locked -p arca-devtools ${ARCA_CARGO_FLAGS:-} -- elf-check "native/$abi/libarca_jni.so" --align 16384 --harden --machine "$machine"
done
guest_rel="${ARCA_GUEST_WASM#"$PWD"/}"
sha256sum native/*/libarca_jni.so "$guest_rel" | tee native/SHA256SUMS
