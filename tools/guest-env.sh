# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# The one set of flags the WebAssembly guest is built with. It is sourced by build-guest.sh,
# check-reproducible.sh and repro-guest-container.sh, so the module whose SHA-256 users compare
# is the same wherever it is built:
#
#   . tools/guest-env.sh; arca_guest_env <source root>
#
# The module must not depend on where the sources, Cargo or rustup live, nor on the build time:
# those prefixes are stripped from panic locations and debug info, the time is fixed, and the
# environment's own RUSTFLAGS are not inherited.

arca_guest_env() {
    local root="${1:?arca_guest_env: source root}"
    export SOURCE_DATE_EPOCH=1700000000
    export RUSTFLAGS="--remap-path-prefix=$root=/arca --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"
    # Cargo's own configuration must not add flags either (build.rustflags, target.*.rustflags).
    unset CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS
}

# Builds the guest in the current directory with the flags above and prints nothing.
arca_guest_build() {
    # shellcheck disable=SC2086
    cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest --locked ${ARCA_CARGO_FLAGS:-}
}
