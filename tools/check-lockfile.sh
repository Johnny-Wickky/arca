#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Supply-chain check for Cargo.lock.
#
#   * every external package comes from crates.io (no git, no path, no other
#     registry);
#   * every crates.io package carries a SHA-256 checksum, so `cargo --locked`
#     verifies the downloaded source against what was reviewed;
#   * no package appears twice in the same version.
#
# A missing Cargo.lock is a warning in ordinary CI and an error for releases
# (`ARCA_RELEASE=1`): a release must be reproducible from a reviewed lockfile.
#
# A lockfile produced on a machine without access to crates.io (for example
# from a vendored directory source) has no checksums and fails this check on
# purpose: regenerate it with network access:
#
#     rm -f Cargo.lock && cargo generate-lockfile && bash tools/check-lockfile.sh
set -euo pipefail
cd "$(dirname "$0")/.."

if [ ! -f Cargo.lock ]; then
  if [ "${ARCA_RELEASE:-0}" = "1" ]; then
    echo "error: Cargo.lock is missing; a release needs a committed, reviewed lockfile" >&2
    exit 1
  fi
  echo "warning: Cargo.lock is not committed yet (see docs/ARCHITECTURE.md section 7)" >&2
  exit 0
fi

python3 -I - Cargo.lock <<'PY'
import re
import sys

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
SPARSE = "sparse+https://index.crates.io/"

text = open(sys.argv[1], encoding="utf-8").read()
blocks = [b for b in text.split("[[package]]")[1:]]
errors = []
seen = set()
external = 0

for block in blocks:
    def field(key):
        m = re.search(rf'^{key} = "([^"]*)"$', block, re.M)
        return m.group(1) if m else None

    name, version, source, checksum = (field(k) for k in ("name", "version", "source", "checksum"))
    ident = (name, version)
    if ident in seen:
        errors.append(f"{name} {version}: listed twice")
    seen.add(ident)
    if source is None:
        continue  # workspace member
    external += 1
    if source not in (CRATES_IO, SPARSE):
        errors.append(f"{name} {version}: source {source!r} is not crates.io")
    elif checksum is None or not re.fullmatch(r"[0-9a-f]{64}", checksum):
        errors.append(f"{name} {version}: no SHA-256 checksum in Cargo.lock")

if errors:
    print("Cargo.lock is not acceptable:", file=sys.stderr)
    for e in errors:
        print(f"  - {e}", file=sys.stderr)
    sys.exit(1)
print(f"Cargo.lock ok: {len(blocks)} packages, {external} external, all checksummed")
PY
