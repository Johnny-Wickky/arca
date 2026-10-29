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
JNI=crates/arca-jni
JNI_FFI=$JNI/src/ffi.rs
FFI=crates/arca-ffi
FFI_C=$FFI/src/ffi.rs

# 1. Every crate root forbids unsafe code; the only exceptions are the Wasm guest and the
#    JNI bridge, which must `deny` it (exports need #[unsafe(no_mangle)], see their manifests)
#    and allow it in a single, named place each.
for lib in crates/*/src/lib.rs; do
    crate=${lib%/src/lib.rs}
    if [[ $crate == "$GUEST" || $crate == "$JNI" || $crate == "$FFI" ]]; then
        grep -q '^#!\[deny(unsafe_code)\]' "$lib" || violation "$lib must contain #![deny(unsafe_code)]"
    else
        grep -q '^#!\[forbid(unsafe_code)\]' "$lib" || violation "$lib must contain #![forbid(unsafe_code)]"
    fi
done

# 2. Every crate inherits the workspace lint table (except the guest and the JNI bridge,
#    whose own tables are compared line by line in step 4).
for manifest in crates/*/Cargo.toml; do
    [[ ${manifest%/Cargo.toml} == "$GUEST" || ${manifest%/Cargo.toml} == "$JNI" || ${manifest%/Cargo.toml} == "$FFI" ]] && continue
    awk '/^\[lints\]/{f=1;next} /^\[/{f=0} f && /^workspace *= *true/{ok=1} END{exit !ok}' "$manifest" \
        || violation "$manifest must contain [lints] workspace = true"
done

# 3. No unsafe blocks, functions, impls, traits or extern blocks anywhere, except in the
#    JNI boundary file (the guest has none: its exports are safe functions).
if grep -rnE '(^|[^[:alnum:]_])unsafe[[:space:]]*(\{|fn[[:space:]]|impl[[:space:]<]|trait[[:space:]]|extern[[:space:]])' \
        --include='*.rs' crates | grep -v "^$JNI_FFI:" | grep -v "^$FFI_C:"; then
    violation "unsafe code found (lines above)"
fi

# 4. The guest: exactly five #[allow(unsafe_code)], each directly followed by
#    #[unsafe(no_mangle)], and lints otherwise identical to the workspace table.
allows=$(grep -c 'allow(unsafe_code' "$GUEST/src/lib.rs" || true)
[[ $allows == 5 ]] || violation "$GUEST/src/lib.rs: expected 5 #[allow(unsafe_code)], found $allows"
pairs=$(grep -A1 'allow(unsafe_code' "$GUEST/src/lib.rs" | grep -cE '^[[:space:]]*#\[unsafe\(no_mangle\)\][[:space:]]*$' || true)
[[ $pairs == 5 ]] || violation "$GUEST/src/lib.rs: every #[allow(unsafe_code)] must precede #[unsafe(no_mangle)]"
if grep -rn 'allow(unsafe_code' --include='*.rs' crates | grep -v "^$GUEST/src/lib.rs:" | grep -v "^$JNI_FFI:" | grep -v "^$FFI_C:"; then
    violation "allow(unsafe_code) outside the guest export table and the JNI boundary file"
fi
table() { tr -d '\r' < "$1" | awk -v h="$2" '$0==h{f=1;next} /^\[/{f=0} f && NF' | sed 's/[[:space:]]*#.*//' | grep -v '^$' | sort; }
diff <(table Cargo.toml '[workspace.lints.clippy]') <(table "$GUEST/Cargo.toml" '[lints.clippy]') >/dev/null \
    || violation "$GUEST/Cargo.toml [lints.clippy] differs from [workspace.lints.clippy]"
diff <(table Cargo.toml '[workspace.lints.rust]' | sed 's/^unsafe_code = "forbid"$/unsafe_code = "deny"/') \
     <(table "$GUEST/Cargo.toml" '[lints.rust]') >/dev/null \
    || violation "$GUEST/Cargo.toml [lints.rust] differs from [workspace.lints.rust] (other than unsafe_code)"

# 4b. The JNI bridge: unsafe is allowed by exactly one inner attribute, at the top of
#    ffi.rs; its manifest differs from the workspace tables in unsafe_code only.
inner=$(grep -c '^#!\[allow(unsafe_code' "$JNI_FFI" || true)
[[ $inner == 1 ]] || violation "$JNI_FFI: expected exactly one #![allow(unsafe_code, ...)], found $inner"
grep -q '^mod ffi;' "$JNI/src/lib.rs" || violation "$JNI/src/lib.rs must declare 'mod ffi;' (private)"
diff <(table Cargo.toml '[workspace.lints.clippy]') <(table "$JNI/Cargo.toml" '[lints.clippy]') >/dev/null \
    || violation "$JNI/Cargo.toml [lints.clippy] differs from [workspace.lints.clippy]"
diff <(table Cargo.toml '[workspace.lints.rust]' | sed 's/^unsafe_code = "forbid"$/unsafe_code = "deny"/') \
     <(table "$JNI/Cargo.toml" '[lints.rust]') >/dev/null \
    || violation "$JNI/Cargo.toml [lints.rust] differs from [workspace.lints.rust] (other than unsafe_code)"

# 4c. The C/iOS FFI bridge: unsafe is allowed by exactly one inner attribute, at the top of
#    ffi.rs; its manifest differs from the workspace tables in unsafe_code only.
inner=$(grep -c '^#!\[allow(unsafe_code' "$FFI_C" || true)
[[ $inner == 1 ]] || violation "$FFI_C: expected exactly one #![allow(unsafe_code, ...)], found $inner"
grep -q '^mod ffi;' "$FFI/src/lib.rs" || violation "$FFI/src/lib.rs must declare 'mod ffi;' (private)"
diff <(table Cargo.toml '[workspace.lints.clippy]') <(table "$FFI/Cargo.toml" '[lints.clippy]') >/dev/null \
    || violation "$FFI/Cargo.toml [lints.clippy] differs from [workspace.lints.clippy]"
diff <(table Cargo.toml '[workspace.lints.rust]' | sed 's/^unsafe_code = "forbid"$/unsafe_code = "deny"/') \
     <(table "$FFI/Cargo.toml" '[lints.rust]') >/dev/null \
    || violation "$FFI/Cargo.toml [lints.rust] differs from [workspace.lints.rust] (other than unsafe_code)"

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

# 7. The build tooling (crates/arca-devtools) is a workspace member and nothing else: no crate
#    and no fuzz target depends on it, so it cannot be linked into the app or a Wasm guest.
if grep -rn 'arca-devtools' --include='Cargo.toml' crates fuzz | grep -v '^crates/arca-devtools/Cargo.toml:'; then
    violation "something depends on arca-devtools (lines above); it must stay a standalone tool"
fi

# 8. The build and verification chain is Rust and POSIX shell only: no Python. (The fixture
#    generators under tools/fixtures are one-off, documented dev aids that are not part of any
#    build; their output is committed.)
if grep -rInE --exclude-dir=fixtures --exclude=check-unsafe.sh '(^|[^[:alnum:]_])python[0-9.]*([^[:alnum:]_]|$)|\.py([^[:alnum:]_]|$)' \
        tools .github android/host-test android/core-test; then
    violation "Python in the build chain (lines above)"
fi
find tools android -name '*.py' -not -path 'tools/fixtures/*' | grep . && violation "Python scripts outside tools/fixtures"

# 8. No signing keys or keystores in the repository (Zero-Trust: the release key lives outside it).
#    Files tracked by git (or staged) are checked by name; .gitignore keeps the rest out.
if git rev-parse --git-dir >/dev/null 2>&1; then
    keys="$(git ls-files --cached --others --exclude-standard | grep -iE '\.(p12|pfx|jks|keystore|key|pem)$' || true)"
    if [[ -n $keys ]]; then
        printf '%s
' "$keys"
        violation "signing key or keystore file in the repository (lines above); keep keys outside it"
    fi
fi

if [[ $fail == 0 ]]; then
    echo "check-unsafe: policy holds"
fi
exit "$fail"
