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

# shellcheck disable=SC2086
cargo run --quiet --locked -p arca-devtools ${ARCA_CARGO_FLAGS:-} -- check-lockfile Cargo.lock
