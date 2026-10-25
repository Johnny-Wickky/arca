#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Runs INSIDE a clean container (ubuntu:22.04, debian:bookworm, ...): installs rustup and the
# toolchain pinned by rust-toolchain.toml, builds arca_guest_xray.wasm from a copy of the source
# in a directory named after the container, and writes its SHA-256 to <outdir>/<label>.sha256.
# The CI job `reproducible-guest` runs this in two different distributions and fails unless the
# two hashes are equal (see docs/RELEASING.md).
#
#   docker run --rm -v "$PWD:/src:ro" -v "$PWD/out:/out" ubuntu:22.04 \
#       bash /src/tools/repro-guest-container.sh ubuntu-22.04 /out
set -euo pipefail

label="${1:?usage: repro-guest-container.sh <label> <outdir>}"
outdir="${2:?usage: repro-guest-container.sh <label> <outdir>}"

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# build-essential is only the host linker for build scripts; no C code is compiled into the guest.
apt-get install -y -qq --no-install-recommends ca-certificates curl build-essential git >/dev/null

export RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
export PATH="$CARGO_HOME/bin:$PATH"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain none --profile minimal >/dev/null

# A different build directory per container, so a path leaking into the module shows up.
src="/build/arca-$label"
mkdir -p "$src"
cp -a /src/. "$src"/
cd "$src"
rustup toolchain install >/dev/null

export SOURCE_DATE_EPOCH=1700000000
export CARGO_TARGET_DIR="$src/target"
export RUSTFLAGS="--remap-path-prefix=$src=/arca --remap-path-prefix=$CARGO_HOME=/cargo --remap-path-prefix=$RUSTUP_HOME=/rustup"
cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest --locked
wasm="$CARGO_TARGET_DIR/wasm32-unknown-unknown/guest/arca_guest_xray.wasm"

rustc --version
mkdir -p "$outdir"
(cd "$(dirname "$wasm")" && sha256sum arca_guest_xray.wasm) | tee "$outdir/$label.sha256"
