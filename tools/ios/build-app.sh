#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds the Arca app for iPhone and iPad from app/project.yml (XcodeGen) and the ArcaKit package.
#
#   tools/ios/build-app.sh project     only writes app/Xcode/Arca.xcodeproj: open it in Xcode, choose
#                                      your Team (Signing & Capabilities), pick the iPhone, press Run
#   tools/ios/build-app.sh simulator   target/ios-app/Arca-simulator.app.zip
#                                      (install: xcrun simctl install booted Arca.app)
#   tools/ios/build-app.sh device      target/ios-app/Arca-unsigned.ipa
#                                      (unsigned: sign it with your own Apple ID, e.g. in Xcode's
#                                      Devices window or with a sideloading tool)
#
# Needs macOS with Xcode 15+ and XcodeGen (brew install xcodegen). Builds ArcaCore.xcframework first
# if it is not there.

set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
# shellcheck source=tools/ios/common.sh
source tools/ios/common.sh

require_macos build-app

MODE="${1:-}"
case "$MODE" in
    project | simulator | device) ;;
    *)
        echo "usage: tools/ios/build-app.sh project|simulator|device" >&2
        exit 2
        ;;
esac

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "build-app: XcodeGen is needed: brew install xcodegen" >&2
    exit 1
fi

if [[ ! -d app/Frameworks/ArcaCore.xcframework ]]; then
    bash tools/ios/build-xcframework.sh
fi

echo "=== Writing app/Xcode/Arca.xcodeproj from app/project.yml ==="
mkdir -p app/Xcode
xcodegen generate --spec app/project.yml --project app/Xcode

if [[ "$MODE" == "project" ]]; then
    echo "Open app/Xcode/Arca.xcodeproj in Xcode, set the Team of the Arca target, choose the iPhone, Run."
    exit 0
fi

PROJECT="app/Xcode/Arca.xcodeproj"
DERIVED="$ROOT/target/ios-derived"
OUT="$ROOT/target/ios-app"
mkdir -p "$OUT"

if [[ "$MODE" == "simulator" ]]; then
    run_xcodebuild app-simulator build \
        -project "$PROJECT" -scheme Arca -configuration Debug \
        -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
        -derivedDataPath "$DERIVED" \
        CODE_SIGNING_ALLOWED=NO
    APP="$DERIVED/Build/Products/Debug-iphonesimulator/Arca.app"
    if [[ ! -d "$APP" ]]; then
        echo "build-app: $APP was not produced" >&2
        exit 1
    fi
    rm -f "$OUT/Arca-simulator.app.zip"
    (cd "$(dirname "$APP")" && zip -qry "$OUT/Arca-simulator.app.zip" Arca.app)
    echo "Built $OUT/Arca-simulator.app.zip"
else
    run_xcodebuild app-device build \
        -project "$PROJECT" -scheme Arca -configuration Release \
        -sdk iphoneos -destination 'generic/platform=iOS' \
        -derivedDataPath "$DERIVED" \
        CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=
    APP="$DERIVED/Build/Products/Release-iphoneos/Arca.app"
    if [[ ! -d "$APP" ]]; then
        echo "build-app: $APP was not produced" >&2
        exit 1
    fi
    rm -rf "$OUT/ipa" "$OUT/Arca-unsigned.ipa"
    mkdir -p "$OUT/ipa/Payload"
    cp -R "$APP" "$OUT/ipa/Payload/"
    (cd "$OUT/ipa" && zip -qry "$OUT/Arca-unsigned.ipa" Payload)
    rm -rf "$OUT/ipa"
    echo "Built $OUT/Arca-unsigned.ipa"
fi
