#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Helpers shared by the tools/ios scripts. Sourced, not run; the caller has done `set -euo pipefail`
# and `cd`'d to the repository root, and has set ROOT to its absolute path.
#
# A note on `pipefail`: never write `cmd | grep -q ...` or `cmd | head -n 1` here. When grep or head
# has what it wants and leaves, `cmd` gets SIGPIPE and the whole pipeline "fails" although the answer
# was found (that is how a symbol that was there once looked missing). Read into a variable or a
# file first, or use `sed -n '1p'`, which reads to the end.

require_macos() {
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "$1: must be run on macOS (Darwin)" >&2
        exit 1
    fi
}

# run_xcodebuild <name> <xcodebuild arguments...>
# Keeps the whole log in target/ios-logs/<name>.log and, when the build fails, prints the lines that
# say why (the log of a full build is thousands of lines).
run_xcodebuild() {
    local name="$1"
    shift
    mkdir -p "$ROOT/target/ios-logs"
    local log="$ROOT/target/ios-logs/$name.log"
    if xcodebuild "$@" >"$log" 2>&1; then
        echo "xcodebuild $name: ok (log: target/ios-logs/$name.log)"
        return 0
    fi
    echo "xcodebuild $name: FAILED (log: target/ios-logs/$name.log)" >&2
    grep -E "error:|Undefined symbols|duplicate symbol|Testing failed|Test Case .* failed|\*\* .* FAILED \*\*" "$log" \
        | sed -n '1,80p' >&2 || true
    echo "--- last 40 lines of the log ---" >&2
    tail -n 40 "$log" >&2
    return 1
}

# pick_simulator: the id of the first iPhone simulator that is installed.
pick_simulator() {
    local id
    id="$(xcrun simctl list devices available | grep -E '^ +iPhone' \
        | grep -oE '[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}' | sed -n '1p' || true)"
    if [[ -z "$id" ]]; then
        echo "no iPhone simulator is installed (Xcode > Settings > Platforms)" >&2
        return 1
    fi
    printf '%s\n' "$id"
}

# package_scheme: the Xcode scheme that builds and tests the ArcaKit package (Xcode names the one that
# covers every target "<package>-Package" when the package has more than one product).
package_scheme() {
    local list
    list="$(cd "$ROOT/app" && xcodebuild -list 2>/dev/null || true)"
    if grep -qx '[[:space:]]*ArcaKit-Package' <<<"$list"; then
        echo "ArcaKit-Package"
    else
        echo "ArcaKit"
    fi
}
