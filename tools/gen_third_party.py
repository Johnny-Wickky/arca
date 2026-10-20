#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
"""Writes the third-party notices shown in the app (Licences -> Third-party).

The list is computed, not typed: it is every crate that ends up inside libarca_jni.so (the
normal dependencies of arca-jni for the given target, found with `cargo metadata`), each with
its declared licence and the licence/notice files that ship in its source. MIT and Apache-2.0
oblige redistributors to carry those texts; this is how the release does.

    python3 tools/gen_third_party.py [--target aarch64-linux-android] [--out FILE]

Environment: ARCA_CARGO_FLAGS (for example "--offline" or "--locked") is passed to cargo.
The script fails (exit 1) when a crate has no declared licence or no licence text at all, so
a dependency that cannot be redistributed correctly stops the release instead of slipping in.
"""
import argparse
import json
import os
import pathlib
import shlex
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FALLBACKS = ROOT / "tools" / "third_party_licenses"
ROOT_PACKAGE = "arca-jni"
NOTICE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")
MAX_FILE = 200_000

HEADER = """\
THIRD-PARTY SOFTWARE IN THIS APPLICATION
========================================

Arca itself is free software under the GNU Affero General Public License, version 3, with the
additional terms shown in the other tabs of this screen. The Rust libraries below are compiled
into the application's native library. Each is used under the licence it declares; the texts
that accompany each library are reproduced in full.

The list is generated from the build's dependency graph (tools/gen_third_party.py), so it
matches the build it ships in.
"""


def cargo_metadata(target: str) -> dict:
    flags = shlex.split(os.environ.get("ARCA_CARGO_FLAGS", ""))
    cmd = ["cargo", "metadata", "--format-version", "1", "--filter-platform", target, *flags]
    done = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        sys.exit(f"gen_third_party: cargo metadata failed:\n{done.stderr}")
    return json.loads(done.stdout)


def shipped_packages(meta: dict) -> list[dict]:
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [p["id"] for p in meta["packages"] if p["name"] == ROOT_PACKAGE]
    if not roots:
        sys.exit(f"gen_third_party: package {ROOT_PACKAGE} not found")
    seen: set[str] = set()
    stack = list(roots)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            # Normal dependencies only: build scripts and tests do not ship.
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    out = []
    for pid in seen:
        pkg = by_id[pid]
        if pkg.get("source") is None:  # a workspace member: Arca's own code
            continue
        out.append(pkg)
    return sorted(out, key=lambda p: (p["name"], p["version"]))


def notice_files(pkg: dict) -> list[pathlib.Path]:
    crate = pathlib.Path(pkg["manifest_path"]).parent
    found = []
    for entry in sorted(crate.iterdir()):
        if entry.is_file() and entry.name.upper().startswith(NOTICE_PREFIXES):
            found.append(entry)
    if found:
        return found
    # Some published crates carry no licence file (the project keeps it at the repository
    # root, outside the package). tools/third_party_licenses/<name>.txt holds the text, taken
    # from that repository; a family of crates (wasmi_core, wasmi_ir, ...) shares the file of
    # its first name part.
    for stem in (pkg["name"], pkg["name"].split("_")[0]):
        fallback = FALLBACKS / f"{stem}.txt"
        if fallback.is_file():
            return [fallback]
    return []


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--target", default="aarch64-linux-android")
    ap.add_argument(
        "--out",
        default=str(ROOT / "android/app/src/main/assets/THIRD_PARTY_NOTICES.txt"),
    )
    args = ap.parse_args()

    packages = shipped_packages(cargo_metadata(args.target))
    problems = []
    parts = [HEADER]
    parts.append(f"{len(packages)} libraries:\n")
    for pkg in packages:
        parts.append(f"  - {pkg['name']} {pkg['version']}  ({pkg.get('license') or 'NO LICENCE'})")
    parts.append("")
    printed: dict[str, str] = {}
    for pkg in packages:
        name = f"{pkg['name']} {pkg['version']}"
        lic = pkg.get("license")
        if not lic:
            problems.append(f"{name}: no declared licence")
        files = notice_files(pkg)
        if not files:
            problems.append(f"{name}: no licence text in the source")
        parts.append("=" * 78)
        parts.append(name)
        parts.append(f"Licence: {lic or 'unknown'}")
        if pkg.get("repository"):
            parts.append(f"Source:  {pkg['repository']}")
        if pkg.get("authors"):
            parts.append("Authors: " + "; ".join(a.split("<")[0].strip() for a in pkg["authors"]))
        parts.append("=" * 78)
        for path in files:
            data = path.read_bytes()[:MAX_FILE].decode("utf-8", errors="replace").strip("\n")
            parts.append(f"--- {path.name} ---")
            if data in printed:
                # Word-for-word the same text as above (the Apache-2.0 text, usually).
                parts.append(f"(identical to {printed[data]})")
            else:
                printed[data] = f"{path.name} of {name}"
                parts.append(data)
            parts.append("")
    if problems:
        print("gen_third_party: cannot produce complete notices:", file=sys.stderr)
        for line in problems:
            print("  " + line, file=sys.stderr)
        return 1
    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(parts) + "\n", encoding="utf-8")
    print(f"gen_third_party: {len(packages)} libraries -> {out} ({out.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
