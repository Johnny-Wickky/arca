#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Verification suite for iOS:
# 1. Builds ArcaCore.xcframework (Rust FFI core for physical & sim targets);
# 2. Verifies Swift package build and runs all unit tests in app/;
# 3. Validates iOS Zero-Network and Zero-Telemetry invariants (no CFNetwork/URLSession/Advertising imports).
#
#   tools/ios/check-ios.sh

set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "check-ios: must be run on macOS (Darwin)" >&2
    exit 1
fi

echo "=== 1. Building native Rust XCFramework ==="
bash tools/ios/build-xcframework.sh

echo "=== 2. Verifying Zero-Network & Zero-Telemetry invariants in Swift sources ==="
# Invariant: Arca on iOS must not link network, tracking, or analytics frameworks
for pattern in "import CFNetwork" "import Network" "import URLSession" "import AdSupport" "import AppTrackingTransparency" "Firebase" "Telemetry"; do
    if grep -rn "$pattern" app/Sources/ 2>/dev/null; then
        echo "Policy violation: forbidden network/telemetry import found in app/Sources/!" >&2
        exit 1
    fi
done
echo "Zero-Network & Zero-Telemetry invariants hold."

echo "=== 3. Compiling Swift package (ArcaKit) ==="
swift build --package-path app -c release

echo "=== 4. Running Swift unit tests (ArcaKitTests) ==="
swift test --package-path app

echo "check-ios: all iOS checks passed successfully."
