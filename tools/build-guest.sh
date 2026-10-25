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
ROOT="$(pwd)"

# The hash of the guest is what users compare, so the module must not depend on where the sources
# or the Cargo and rustup homes live: strip those prefixes from panic locations and debug info.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$ROOT=/arca --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"

# shellcheck disable=SC2086
cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest --locked ${ARCA_CARGO_FLAGS:-}
wasm="${CARGO_TARGET_DIR:-target}"/wasm32-unknown-unknown/guest/arca_guest_xray.wasm
# wasm-imports fails when the module has an import section with entries, and prints the SHA-256.
# shellcheck disable=SC2086
cargo run --quiet --locked -p arca-devtools ${ARCA_CARGO_FLAGS:-} -- wasm-imports "$wasm"
