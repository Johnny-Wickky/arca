#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Signs a package built with a throwaway key (tools/android/build-local.sh without ARCA_KEYSTORE)
# with the permanent release key, verifies it and writes its SHA-256. The password is read from
# the environment (ARCA_KEYSTORE_PASS) or asked for on the terminal; it is never printed or kept.
#
#   tools/android/sign-release.sh dist/arca-0.4.5-throwaway-signed.apk dist/arca-0.4.5.apk
#
# Environment: ARCA_KEYSTORE (default ../arca-keys/arca-release.p12), ARCA_KEY_ALIAS (default
# arca), ARCA_KEY_PASS (default: the keystore password), ARCA_BUILD_TOOLS_DIR (directory with
# apksigner; default: the newest under $ANDROID_HOME/build-tools).
set -euo pipefail

in="${1:?usage: sign-release.sh <input.apk> <output.apk>}"
out="${2:?usage: sign-release.sh <input.apk> <output.apk>}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ks="${ARCA_KEYSTORE:-$root/../arca-keys/arca-release.p12}"
alias_="${ARCA_KEY_ALIAS:-arca}"
[ -f "$in" ] || { echo "sign-release: $in does not exist" >&2; exit 1; }
[ -f "$ks" ] || { echo "sign-release: keystore $ks does not exist" >&2; exit 1; }

bt="${ARCA_BUILD_TOOLS_DIR:-}"
if [ -z "$bt" ]; then
    sdk="${ANDROID_HOME:-$HOME/android-sdk}"
    bt="$(ls -d "$sdk"/build-tools/* 2>/dev/null | sort -V | tail -n1)"
fi
apksigner="$bt/apksigner"
[ -x "$apksigner" ] || apksigner="$(command -v apksigner || true)"
[ -n "$apksigner" ] || { echo "sign-release: apksigner not found (set ARCA_BUILD_TOOLS_DIR)" >&2; exit 1; }

if [ -z "${ARCA_KEYSTORE_PASS:-}" ]; then
    read -r -s -p "Keystore password: " ARCA_KEYSTORE_PASS
    echo
fi
export ARCA_KEYSTORE_PASS ARCA_KEY_PASS="${ARCA_KEY_PASS:-$ARCA_KEYSTORE_PASS}"

# apksigner replaces whatever signature the input carries; zip alignment is kept.
"$apksigner" sign --ks "$ks" --ks-pass env:ARCA_KEYSTORE_PASS --ks-key-alias "$alias_" \
    --key-pass env:ARCA_KEY_PASS --min-sdk-version 26 --out "$out" "$in"
"$apksigner" verify --verbose --print-certs "$out"
( cd "$(dirname "$out")" && sha256sum "$(basename "$out")" | tee "$(basename "$out").sha256" )
