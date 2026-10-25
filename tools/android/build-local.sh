#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds the signed package on a developer machine (Linux or WSL) the same way the `package`
# job of .github/workflows/android.yml does: Wasm guest -> native libraries for the three ABIs
# (16 KiB page size, linker only from the NDK) -> tools/android/build-apk.sh.
#
#   tools/android/build-local.sh
#
# Environment:
#   ANDROID_HOME        SDK root with build-tools;36.0.0, platforms;android-36 and the NDK
#                       (default: $HOME/android-sdk)
#   ARCA_NDK            NDK directory (default: newest under $ANDROID_HOME/ndk)
#   ARCA_RELEASE=1      release build: refuses placeholder attribution (use for tagged builds)
#   ARCA_KEYSTORE, ARCA_KEYSTORE_PASS, ARCA_KEY_ALIAS [, ARCA_KEY_PASS]
#                       the release key; without it ARCA_THROWAWAY_KEY=1 is used (test builds)
#   ARCA_OUT            output directory (default: dist)
# The passwords are read from the environment only; they are never printed or written.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
die() { printf 'build-local: %s\n' "$*" >&2; exit 1; }

export ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
[ -d "$ANDROID_HOME" ] || die "ANDROID_HOME ($ANDROID_HOME) does not exist"
NDK="${ARCA_NDK:-$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -n1)}"
[ -d "$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin" ] || die "no NDK under $ANDROID_HOME/ndk"
API=26
BIN="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"

if [ -z "${ARCA_KEYSTORE:-}" ]; then export ARCA_THROWAWAY_KEY=1; fi

echo "== Wasm guest"
bash tools/build-guest.sh

echo "== native libraries (arm64-v8a, armeabi-v7a, x86_64)"
export ARCA_GUEST_WASM="${CARGO_TARGET_DIR:-$ROOT/target}/wasm32-unknown-unknown/guest/arca_guest_xray.wasm"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$BIN/aarch64-linux-android$API-clang"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$BIN/armv7a-linux-androideabi$API-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$BIN/x86_64-linux-android$API-clang"
align="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
remap="--remap-path-prefix=$ROOT=/arca --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$HOME/.rustup=/rustup"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-D warnings $align $remap"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_RUSTFLAGS="-D warnings $align $remap"
export CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS="-D warnings $align $remap"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
NATIVE="${ARCA_NATIVE_DIR:-$ROOT/native}"
rm -rf "$NATIVE"; mkdir -p "$NATIVE"
for entry in aarch64-linux-android:arm64-v8a:aarch64 armv7-linux-androideabi:armeabi-v7a:arm x86_64-linux-android:x86_64:x86_64; do
    IFS=: read -r triple abi machine <<<"$entry"
    cargo build --release --locked -p arca-jni --target "$triple"
    mkdir -p "$NATIVE/$abi"
    cp "$TARGET_DIR/$triple/release/libarca_jni.so" "$NATIVE/$abi/"
    python3 tools/android/check_elf.py "$NATIVE/$abi/libarca_jni.so" --align 16384 --machine "$machine"
done
sha256sum "$NATIVE"/*/libarca_jni.so "$ARCA_GUEST_WASM" | tee "$NATIVE/SHA256SUMS"

echo "== package"
export ARCA_NATIVE_DIR="$NATIVE" ARCA_OUT="${ARCA_OUT:-$ROOT/dist}"
export ARCA_BUILD_TOOLS="${ARCA_BUILD_TOOLS:-36.0.0}"
bash tools/android/build-apk.sh
cp "$NATIVE/SHA256SUMS" "$ARCA_OUT/native.SHA256SUMS"
