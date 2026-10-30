#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Verification suite for iOS (macOS with Xcode 15+, rustup):
#   1. the Zero-Network and Zero-Telemetry invariants hold in the Swift sources;
#   2. the ArcaKit Swift package compiles for the iOS Simulator (needs no Rust: Swift errors show first);
#   3. ArcaCore.xcframework is built (the Rust core for devices and simulators);
#   4. the unit tests and the engine tests run on an iOS Simulator, linked with the real core;
#   5. the Arca app builds for the Simulator and for a device (unsigned), and the results are packed
#      into target/ios-app/ (Arca-simulator.app.zip, Arca-unsigned.ipa).
#
#   tools/ios/check-ios.sh
#
# Needs XcodeGen for step 5: brew install xcodegen.

set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
# shellcheck source=tools/ios/common.sh
source tools/ios/common.sh

require_macos check-ios

echo "=== 1. Zero-Network & Zero-Telemetry invariants in the Swift sources ==="
# Real APIs and frameworks, not words: comments are cut off before the search, so "Telemetry" in a
# comment is not a violation and a use of URLSession is.
FORBIDDEN='URLSession|NWConnection|NWPathMonitor|NWEndpoint|CFNetwork|CFSocket|WKWebView|SFSafariViewController|ASWebAuthenticationSession|AdSupport|AppTrackingTransparency|ASIdentifierManager|Firebase|Crashlytics|Sentry|Amplitude|Mixpanel|import Network|import WebKit|import SafariServices'
violations=0
while IFS= read -r -d '' file; do
    hits="$(sed 's|//.*$||' "$file" | grep -nE "$FORBIDDEN" || true)"
    if [[ -n "$hits" ]]; then
        echo "Policy violation in $file:" >&2
        printf '%s\n' "$hits" >&2
        violations=1
    fi
done < <(find app/Sources app/App -name '*.swift' -print0)
if [[ "$violations" != 0 ]]; then
    echo "Policy violation: network or telemetry APIs found in the Swift sources." >&2
    exit 1
fi
echo "Zero-Network & Zero-Telemetry invariants hold."

SCHEME="$(package_scheme)"
echo "Swift package scheme: $SCHEME"
DERIVED="$ROOT/target/xcode-derived"

echo "=== 2. Compiling the ArcaKit package for the iOS Simulator ==="
(cd app && run_xcodebuild swift-compile build \
    -scheme "$SCHEME" \
    -destination 'generic/platform=iOS Simulator' \
    -derivedDataPath "$DERIVED" \
    CODE_SIGNING_ALLOWED=NO)

echo "=== 3. Building ArcaCore.xcframework ==="
bash tools/ios/build-xcframework.sh

echo "=== 4. Running the Swift tests on an iOS Simulator ==="
SIM_ID="$(pick_simulator)"
SIM_LIB="$ROOT/target/ios-sim-universal"
if [[ "$SIM_LIB" == *" "* ]]; then
    echo "check-ios: the repository path must not contain spaces (it goes into a linker flag): $ROOT" >&2
    exit 1
fi
rm -rf "$ROOT/target/ios-test.xcresult"
# The tests link the core as a static library: the simulator slice (arm64 + x86_64) of libarca_ffi.a.
(cd app && run_xcodebuild swift-test test \
    -scheme "$SCHEME" \
    -destination "platform=iOS Simulator,id=$SIM_ID" \
    -derivedDataPath "$DERIVED" \
    -resultBundlePath "$ROOT/target/ios-test.xcresult" \
    CODE_SIGNING_ALLOWED=NO \
    "OTHER_LDFLAGS=\$(inherited) -L$SIM_LIB -larca_ffi")

echo "=== 5. Building the app for the Simulator and for a device ==="
bash tools/ios/build-app.sh simulator
bash tools/ios/build-app.sh device

echo "check-ios: all iOS checks passed successfully."
