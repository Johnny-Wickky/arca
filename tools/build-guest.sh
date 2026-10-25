#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Builds the WebAssembly guest exactly as CI and releases do and checks the one property the
# sandbox depends on: the module imports nothing.
#
#   tools/build-guest.sh        # -> target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm
#
# Extra cargo flags (for example --offline) can be passed in ARCA_CARGO_FLAGS.
set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck disable=SC2086
cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest --locked ${ARCA_CARGO_FLAGS:-}
wasm="${CARGO_TARGET_DIR:-target}"/wasm32-unknown-unknown/guest/arca_guest_xray.wasm
python3 - "$wasm" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
assert data[:8] == b"\0asm\x01\0\0\0", "not a wasm v1 module"
pos = 8
def leb(p):
    r = s = 0
    while True:
        b = data[p]; p += 1; r |= (b & 0x7F) << s; s += 7
        if b < 0x80:
            return r, p
while pos < len(data):
    sid = data[pos]; size, pos = leb(pos + 1)
    assert sid != 2, "guest module has an import section"
    pos += size
print("guest imports nothing")
PY
sha256sum "$wasm"
