#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds, signs and verifies the Android package without Gradle: aapt2, javac, d8, zipalign and
# apksigner from the Android SDK build-tools, nothing else. Every step is visible here, and
# there is no plugin or dependency resolver between the source and the APK.
#
#   tools/android/build-apk.sh
#
# Inputs (environment):
#   ANDROID_HOME            SDK root (default: $ANDROID_SDK_ROOT)
#   ARCA_BUILD_TOOLS        build-tools version directory name (default: newest installed)
#   ARCA_NATIVE_DIR         directory with <abi>/libarca_jni.so for arm64-v8a, armeabi-v7a, x86_64
#   ARCA_VERSION_NAME       default: versionName from the manifest
#   ARCA_VERSION_CODE       default: versionCode from the manifest
#   ARCA_OUT                output directory (default: dist)
#   Signing, either
#     ARCA_KEYSTORE, ARCA_KEYSTORE_PASS, ARCA_KEY_ALIAS [, ARCA_KEY_PASS]   a release key, or
#     ARCA_THROWAWAY_KEY=1    a key generated for this build and thrown away: the package
#                             installs, but cannot be updated by a later build. Marked as such.
#
# Output: $ARCA_OUT/arca-<version>.apk, its SHA-256 and a report of what was verified.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP="$ROOT/android/app/src/main"
OUT="${ARCA_OUT:-$ROOT/dist}"
MIN_SDK=26
TARGET_SDK=36
ABIS=(arm64-v8a armeabi-v7a x86_64)

die() { printf 'build-apk: %s\n' "$*" >&2; exit 1; }
step() { printf '\n== %s\n' "$*"; }

# ---- tools -------------------------------------------------------------------------------------
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
[ -d "$SDK" ] || die "set ANDROID_HOME to the Android SDK"
BT_NAME="${ARCA_BUILD_TOOLS:-$(ls "$SDK/build-tools" 2>/dev/null | sort -V | tail -n1)}"
BT="$SDK/build-tools/$BT_NAME"
[ -d "$BT" ] || die "no build-tools in $SDK/build-tools"
AAPT2="$BT/aapt2"; D8="$BT/d8"; ZIPALIGN="$BT/zipalign"; APKSIGNER="$BT/apksigner"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do [ -x "$t" ] || die "missing $t"; done
ANDROID_JAR="$SDK/platforms/android-$TARGET_SDK/android.jar"
[ -f "$ANDROID_JAR" ] || die "missing $ANDROID_JAR (sdkmanager \"platforms;android-$TARGET_SDK\")"
command -v zip >/dev/null || die "zip is required"
command -v javac >/dev/null || die "a JDK (javac) is required"

NATIVE="${ARCA_NATIVE_DIR:-}"
[ -d "$NATIVE" ] || die "set ARCA_NATIVE_DIR to the directory with <abi>/libarca_jni.so"
for abi in "${ABIS[@]}"; do
    [ -s "$NATIVE/$abi/libarca_jni.so" ] || die "missing $NATIVE/$abi/libarca_jni.so"
done

manifest_attr() { sed -n "s/.*android:$1=\"\([^\"]*\)\".*/\1/p" "$APP/AndroidManifest.xml" | head -n1; }
VERSION_NAME="${ARCA_VERSION_NAME:-$(manifest_attr versionName)}"
VERSION_CODE="${ARCA_VERSION_CODE:-$(manifest_attr versionCode)}"
[[ "$VERSION_CODE" =~ ^[0-9]+$ ]] || die "bad version code '$VERSION_CODE'"
[[ "$VERSION_NAME" =~ ^[0-9A-Za-z._+-]+$ ]] || die "bad version name '$VERSION_NAME'"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"
APK="$OUT/arca-$VERSION_NAME.apk"

# ---- 1. resources ------------------------------------------------------------------------------
step "1. resources (aapt2 $BT_NAME, minSdk $MIN_SDK, targetSdk $TARGET_SDK)"
"$AAPT2" compile --dir "$APP/res" -o "$WORK/res.zip"

# Third-party notices are generated from the dependency graph and shipped as an asset.
ASSETS="$WORK/assets"
mkdir -p "$ASSETS"
python3 "$ROOT/tools/gen_third_party.py" --out "$ASSETS/THIRD_PARTY_NOTICES.txt"

mkdir -p "$WORK/gen"
"$AAPT2" link \
    -I "$ANDROID_JAR" \
    --manifest "$APP/AndroidManifest.xml" \
    --min-sdk-version "$MIN_SDK" --target-sdk-version "$TARGET_SDK" \
    --version-code "$VERSION_CODE" --version-name "$VERSION_NAME" \
    -A "$ASSETS" \
    --java "$WORK/gen" \
    -0 arsc \
    --no-auto-version \
    -o "$WORK/base.apk" "$WORK/res.zip"

# ---- 2. Java -----------------------------------------------------------------------------------
step "2. Java sources -> classes"
find "$APP/java" -name '*.java' > "$WORK/sources.txt"
find "$WORK/gen" -name '*.java' >> "$WORK/sources.txt"
mkdir -p "$WORK/classes"
javac -nowarn -Xlint:-options -source 8 -target 8 -encoding UTF-8 \
    -bootclasspath "$ANDROID_JAR" -d "$WORK/classes" @"$WORK/sources.txt"
echo "$(find "$WORK/classes" -name '*.class' | wc -l) classes"

# ---- 3. dex ------------------------------------------------------------------------------------
step "3. dex (d8, min-api $MIN_SDK)"
mkdir -p "$WORK/dex"
find "$WORK/classes" -name '*.class' -print0 | xargs -0 "$D8" --release --min-api "$MIN_SDK" \
    --lib "$ANDROID_JAR" --output "$WORK/dex"
[ -f "$WORK/dex/classes.dex" ] || die "d8 produced no classes.dex"
ls -l "$WORK/dex"

# ---- 4. assemble -------------------------------------------------------------------------------
step "4. assemble: resources + dex + native libraries (stored, page aligned)"
cp "$WORK/base.apk" "$WORK/unaligned.apk"
(cd "$WORK/dex" && zip -X -q "$WORK/unaligned.apk" classes.dex)
for abi in "${ABIS[@]}"; do
    mkdir -p "$WORK/pkg/lib/$abi"
    cp "$NATIVE/$abi/libarca_jni.so" "$WORK/pkg/lib/$abi/"
done
# -0: store without compression, so the system maps the library straight out of the APK
# (android:extractNativeLibs="false") and zipalign can page-align it.
(cd "$WORK/pkg" && zip -X -q -0 -r -D "$WORK/unaligned.apk" lib)
# 16 KiB alignment of the stored .so files: required by Android 15+ devices with 16 KiB pages
# and by Google Play for apps targeting Android 15 and later.
"$ZIPALIGN" -f -P 16 4 "$WORK/unaligned.apk" "$WORK/aligned.apk"

# ---- 5. sign -----------------------------------------------------------------------------------
step "5. sign (APK Signature Scheme v2/v3; minSdk $MIN_SDK makes the v1 JAR signature unnecessary)"
SIGNING="release key"
if [ -n "${ARCA_KEYSTORE:-}" ]; then
    [ -f "$ARCA_KEYSTORE" ] || die "ARCA_KEYSTORE does not exist"
    : "${ARCA_KEYSTORE_PASS:?set ARCA_KEYSTORE_PASS}" "${ARCA_KEY_ALIAS:?set ARCA_KEY_ALIAS}"
    KS="$ARCA_KEYSTORE"; KEY_ALIAS="$ARCA_KEY_ALIAS"
    export ARCA_KEY_PASS="${ARCA_KEY_PASS:-$ARCA_KEYSTORE_PASS}"
elif [ "${ARCA_THROWAWAY_KEY:-0}" = 1 ]; then
    SIGNING="THROWAWAY key (generated for this build; later builds cannot update this install)"
    command -v keytool >/dev/null || die "keytool (JDK) is required"
    KS="$WORK/throwaway.p12"
    ARCA_KEYSTORE_PASS="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    export ARCA_KEYSTORE_PASS ARCA_KEY_PASS="$ARCA_KEYSTORE_PASS"
    KEY_ALIAS=arca-throwaway
    keytool -genkeypair -keystore "$KS" -storetype PKCS12 -storepass:env ARCA_KEYSTORE_PASS \
        -keypass:env ARCA_KEY_PASS -alias "$KEY_ALIAS" -keyalg RSA -keysize 3072 -validity 3650 \
        -dname "CN=Arca throwaway build key, O=Arca" >/dev/null
else
    die "no signing key: set ARCA_KEYSTORE (+ ARCA_KEYSTORE_PASS, ARCA_KEY_ALIAS) or ARCA_THROWAWAY_KEY=1"
fi
# Passwords go to apksigner through the environment, not the command line.
"$APKSIGNER" sign --ks "$KS" --ks-pass env:ARCA_KEYSTORE_PASS --ks-key-alias "$KEY_ALIAS" \
    --key-pass env:ARCA_KEY_PASS --min-sdk-version "$MIN_SDK" \
    --v1-signing-enabled false --v2-signing-enabled true --v3-signing-enabled true \
    --out "$APK" "$WORK/aligned.apk"
echo "signed with: $SIGNING"

# ---- 6. verify ---------------------------------------------------------------------------------
step "6. verify the finished package"
REPORT="$OUT/arca-$VERSION_NAME.verify.txt"
{
    echo "package:  $(basename "$APK")"
    echo "signing:  $SIGNING"
    echo
    echo "-- apksigner verify"
    "$APKSIGNER" verify --verbose --print-certs --min-sdk-version "$MIN_SDK" "$APK"
    echo
    echo "-- zipalign (4-byte, 16 KiB for stored shared libraries)"
    "$ZIPALIGN" -c -P 16 -v 4 "$APK" | grep -E "lib/|Verification" || true
    echo
    echo "-- aapt2 dump badging"
    "$AAPT2" dump badging "$APK" | grep -E "^(package|sdkVersion|targetSdkVersion|uses-|native-code|application-label:|launchable|supports-screens|densities|feature-group|locales)" || true
} > "$REPORT" 2>&1 || { cat "$REPORT"; die "verification failed"; }
cat "$REPORT"

fail=0
need() { # need "what" command...
    local what=$1; shift
    if ! "$@"; then echo "CHECK FAILED: $what" >&2; fail=1; fi
}
BADGING="$("$AAPT2" dump badging "$APK")"
LISTING="$(unzip -v "$APK")"
badging_has() { grep -q -- "$1" <<<"$BADGING"; }
badging_lacks() { ! grep -qi -- "$1" <<<"$BADGING"; }
listing_has() { grep -q -- "$1" <<<"$LISTING"; }
stored() { # every line of the listing matching $1 is Stored, and there is at least one
    local lines; lines="$(grep -E -- "$1" <<<"$LISTING" || true)"
    [ -n "$lines" ] && ! grep -qv Stored <<<"$lines"
}
need "package org.arca.viewer $VERSION_CODE/$VERSION_NAME" \
    badging_has "^package: name='org.arca.viewer' versionCode='$VERSION_CODE' versionName='$VERSION_NAME'"
need "minSdk $MIN_SDK" badging_has "^\(min\)\{0,1\}[sS]dkVersion:'$MIN_SDK'"
need "targetSdk $TARGET_SDK" badging_has "^targetSdkVersion:'$TARGET_SDK'"
# Every uses-permission* / uses-implied-permission line of the badging must be exactly
# POST_NOTIFICATIONS (a lookahead does not exist in grep's basic or extended syntax, so the
# earlier single-pattern test could never fail).
only_notifications() {
    local lines extra
    lines="$(grep -E '^uses-[a-z0-9-]*permission' <<<"$BADGING" || true)"
    extra="$(grep -vxE "uses-permission: name='android\.permission\.POST_NOTIFICATIONS'" <<<"$lines" || true)"
    if [ -n "$extra" ]; then echo "unexpected permission line(s): $extra" >&2; return 1; fi
    grep -qx "uses-permission: name='android.permission.POST_NOTIFICATIONS'" <<<"$lines"
}
need "only POST_NOTIFICATIONS is requested" only_notifications
need "no INTERNET permission" badging_lacks "android.permission.INTERNET"
for abi in "${ABIS[@]}"; do
    need "native code for $abi" badging_has "^native-code:.*'$abi'"
done
need "a launchable activity" badging_has "^launchable-activity"
need "16 KiB aligned libraries" "$ZIPALIGN" -c -P 16 4 "$APK"
need "libraries are stored, not compressed" stored 'lib/.*\.so'
need "resources.arsc is stored" stored 'resources\.arsc'
need "third-party notices shipped" listing_has "assets/THIRD_PARTY_NOTICES.txt"
need "classes.dex present" listing_has "classes.dex"
not_debuggable() { ! "$AAPT2" dump xmltree --file AndroidManifest.xml "$APK" | grep -q 'debuggable'; }
need "not debuggable" not_debuggable
[ "$fail" = 0 ] || die "package checks failed"

( cd "$OUT" && sha256sum "$(basename "$APK")" | tee "$(basename "$APK").sha256" )
echo
echo "built: $APK ($(wc -c <"$APK") bytes), signed with $SIGNING"
