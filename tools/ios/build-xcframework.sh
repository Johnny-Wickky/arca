#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds libarca_ffi for physical iOS devices (aarch64-apple-ios) and universal
# iOS Simulators (aarch64-apple-ios-sim + x86_64-apple-ios), and packages them into
# target/ArcaCore.xcframework with header include/arca.h.
#
#   tools/ios/build-xcframework.sh
#
# Requires: macOS with Xcode Command Line Tools, rustup, lipo, xcodebuild.

set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-xcframework: must be run on macOS (Darwin)" >&2
    exit 1
fi

echo "=== 1. Checking required Rust targets ==="
for target in wasm32-unknown-unknown aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios; do
    if ! rustup target list --installed | grep -q "^$target$"; then
        echo "Installing missing rust target: $target"
        rustup target add "$target"
    fi
done

echo "=== 2. Building Wasm guest (0 imports sandbox payload) ==="
GUEST_WASM="target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm"
if [[ ! -f "$GUEST_WASM" ]]; then
    bash tools/build-guest.sh
fi
export ARCA_GUEST_WASM="$(pwd)/$GUEST_WASM"
echo "ARCA_GUEST_WASM embedded: $ARCA_GUEST_WASM"

LOCKED="--locked"
PROFILE="--release"

echo "=== 3. Compiling arca-ffi for iOS device (arm64) ==="
cargo build -p arca-ffi --target aarch64-apple-ios $PROFILE $LOCKED

echo "=== 4. Compiling arca-ffi for iOS simulator (arm64 Apple Silicon) ==="
cargo build -p arca-ffi --target aarch64-apple-ios-sim $PROFILE $LOCKED

echo "=== 5. Compiling arca-ffi for iOS simulator (x86_64 Intel) ==="
cargo build -p arca-ffi --target x86_64-apple-ios $PROFILE $LOCKED

echo "=== 6. Creating universal iOS simulator static library with lipo ==="
SIM_UNIVERSAL_DIR="target/ios-sim-universal"
mkdir -p "$SIM_UNIVERSAL_DIR"
lipo -create \
    target/aarch64-apple-ios-sim/release/libarca_ffi.a \
    target/x86_64-apple-ios/release/libarca_ffi.a \
    -output "$SIM_UNIVERSAL_DIR/libarca_ffi.a"

lipo -info "$SIM_UNIVERSAL_DIR/libarca_ffi.a"

echo "=== 7. Assembling ArcaCore.xcframework ==="
OUTPUT_FRAMEWORK="target/ArcaCore.xcframework"
rm -rf "$OUTPUT_FRAMEWORK"

# Setup headers directory for xcframework
HEADERS_DIR="target/xcframework-headers"
rm -rf "$HEADERS_DIR"
mkdir -p "$HEADERS_DIR"
cp include/arca.h "$HEADERS_DIR/"
cp include/module.modulemap "$HEADERS_DIR/"

xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libarca_ffi.a \
    -headers "$HEADERS_DIR" \
    -library "$SIM_UNIVERSAL_DIR/libarca_ffi.a" \
    -headers "$HEADERS_DIR" \
    -output "$OUTPUT_FRAMEWORK"

echo "=== 8. Validating symbols in XCFramework ==="
DEVICE_LIB="$OUTPUT_FRAMEWORK/ios-arm64/libarca_ffi.a"
# The objects are written by the LLVM of the pinned Rust, which is newer than the one in Xcode: its
# `nm` cannot read them ("Unknown attribute kind") and the symbol would look missing. The Rust
# toolchain's own llvm-nm reads them; Xcode's `nm` is only the fallback.
rustup component add llvm-tools >/dev/null 2>&1 || true
NM="$(find "$(rustc --print sysroot)/lib/rustlib" -name llvm-nm -type f 2>/dev/null | head -n 1)"
[ -n "$NM" ] || NM=nm
SYMBOLS="$("$NM" "$DEVICE_LIB" 2>/dev/null || true)"
for sym in arca_init arca_doc_new arca_run arca_buffer_free arca_about; do
    if ! printf '%s\n' "$SYMBOLS" | grep -Eq "T _?$sym\$"; then
        echo "Error: required symbol $sym missing from device library!" >&2
        exit 1
    fi
done
echo "All critical FFI entry points verified."

# Copy to app directory for local development and testing
mkdir -p app/Frameworks
rm -rf app/Frameworks/ArcaCore.xcframework
cp -R "$OUTPUT_FRAMEWORK" app/Frameworks/

echo "Successfully built $OUTPUT_FRAMEWORK and mirrored to app/Frameworks/"
