#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Mechanical enforcement of the "no unsafe, no placeholders, no C" policy.
# Complements the compiler lints, which a crate manifest could weaken.
#
#   tools/check-unsafe.sh            # from the repository root
#
# Exit status 0 = policy holds; 1 = violation (each one is printed).

set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
violation() { printf 'policy violation: %s\n' "$*" >&2; fail=1; }

GUEST=crates/arca-guest-xray

# 1. Every crate root forbids unsafe code; the only exception is the Wasm guest,
#    which must `deny` it (exports need #[unsafe(no_mangle)], see its Cargo.toml).
for lib in crates/*/src/lib.rs; do
    crate=${lib%/src/lib.rs}
    if [[ $crate == "$GUEST" ]]; then
        grep -q '^#!\[deny(unsafe_code)\]' "$lib" || violation "$lib must contain #![deny(unsafe_code)]"
    else
        grep -q '^#!\[forbid(unsafe_code)\]' "$lib" || violation "$lib must contain #![forbid(unsafe_code)]"
    fi
done

# 2. Every crate inherits the workspace lint table (except the guest, checked in 4).
for manifest in crates/*/Cargo.toml; do
    [[ ${manifest%/Cargo.toml} == "$GUEST" ]] && continue
    awk '/^\[lints\]/{f=1;next} /^\[/{f=0} f && /^workspace *= *true/{ok=1} END{exit !ok}' "$manifest" \
        || violation "$manifest must contain [lints] workspace = true"
done

# 3. No unsafe blocks, functions, impls, traits or extern blocks anywhere.
if grep -rnE '(^|[^[:alnum:]_])unsafe[[:space:]]*(\{|fn[[:space:]]|impl[[:space:]<]|trait[[:space:]]|extern[[:space:]])' \
        --include='*.rs' crates; then
    violation "unsafe code found (lines above)"
fi

# 4. The guest: exactly five #[allow(unsafe_code)], each directly followed by
#    #[unsafe(no_mangle)], and lints otherwise identical to the workspace table.
allows=$(grep -c 'allow(unsafe_code' "$GUEST/src/lib.rs" || true)
[[ $allows == 5 ]] || violation "$GUEST/src/lib.rs: expected 5 #[allow(unsafe_code)], found $allows"
pairs=$(grep -A1 'allow(unsafe_code' "$GUEST/src/lib.rs" | grep -cE '^[[:space:]]*#\[unsafe\(no_mangle\)\][[:space:]]*$' || true)
[[ $pairs == 5 ]] || violation "$GUEST/src/lib.rs: every #[allow(unsafe_code)] must precede #[unsafe(no_mangle)]"
if grep -rn 'allow(unsafe_code' --include='*.rs' crates | grep -v "^$GUEST/src/lib.rs:"; then
    violation "#[allow(unsafe_code)] outside the guest export table"
fi
table() { awk -v h="$2" '$0==h{f=1;next} /^\[/{f=0} f && NF' "$1" | sed 's/[[:space:]]*#.*//' | grep -v '^$' | sort; }
diff <(table Cargo.toml '[workspace.lints.clippy]') <(table "$GUEST/Cargo.toml" '[lints.clippy]') >/dev/null \
    || violation "$GUEST/Cargo.toml [lints.clippy] differs from [workspace.lints.clippy]"
diff <(table Cargo.toml '[workspace.lints.rust]' | sed 's/^unsafe_code = "forbid"$/unsafe_code = "deny"/') \
     <(table "$GUEST/Cargo.toml" '[lints.rust]') >/dev/null \
    || violation "$GUEST/Cargo.toml [lints.rust] differs from [workspace.lints.rust] (other than unsafe_code)"

# 5. No placeholders in production code.
if grep -rnE '\b(todo|unimplemented)!|\bTODO\b|\bFIXME\b|\bXXX\b' --include='*.rs' crates; then
    violation "placeholder found (lines above)"
fi

# 6. No C/C++ in the dependency graph: no *-sys crates, no C build helpers.
if [[ -f Cargo.lock ]]; then
    if grep -E '^name = "([a-z0-9_-]+-sys|cc|cmake|bindgen|pkg-config|vcpkg)"$' Cargo.lock; then
        violation "native (C/C++) build dependency in Cargo.lock (lines above)"
    fi
fi
if find crates -name '*.c' -o -name '*.cc' -o -name '*.cpp' -o -name '*.h' -o -name '*.hpp' | grep .; then
    violation "C/C++ sources in crates/"
fi

if [[ $fail == 0 ]]; then
    echo "check-unsafe: policy holds"
fi
exit "$fail"
