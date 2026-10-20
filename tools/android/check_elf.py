#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
"""Checks that a shared library is built for Android devices with 16 KiB memory pages.

    check_elf.py libarca_jni.so --align 16384 [--machine aarch64|arm|x86_64]

Android 15+ can run with 16 KiB pages, and Google Play requires it of new apps. A library
whose loadable segments are aligned to only 4 KiB fails to load there. The check reads the
ELF program headers (no external tools):

  * every PT_LOAD segment has p_align >= --align, and offset and address agree modulo p_align;
  * the library is a shared object, ELF64 or ELF32, little-endian, for the expected machine.

Exit status 0 = the library qualifies.
"""
import argparse
import struct
import sys

PT_LOAD = 1
ET_DYN = 3
MACHINES = {"aarch64": 183, "arm": 40, "x86_64": 62, "x86": 3}


def fail(message: str) -> None:
    print(f"check_elf: {message}", file=sys.stderr)
    sys.exit(1)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("library")
    ap.add_argument("--align", type=int, default=16384)
    ap.add_argument("--machine", choices=sorted(MACHINES))
    args = ap.parse_args()

    with open(args.library, "rb") as handle:
        data = handle.read()
    if data[:4] != b"\x7fELF":
        fail(f"{args.library}: not an ELF file")
    bits = {1: 32, 2: 64}.get(data[4])
    if bits is None or data[5] != 1:
        fail(f"{args.library}: not a little-endian ELF32/ELF64 file")

    if bits == 64:
        e_type, e_machine = struct.unpack_from("<HH", data, 16)
        e_phoff, = struct.unpack_from("<Q", data, 32)
        e_phentsize, e_phnum = struct.unpack_from("<HH", data, 54)
    else:
        e_type, e_machine = struct.unpack_from("<HH", data, 16)
        e_phoff, = struct.unpack_from("<I", data, 28)
        e_phentsize, e_phnum = struct.unpack_from("<HH", data, 42)
    if e_type != ET_DYN:
        fail(f"{args.library}: not a shared object (e_type={e_type})")
    if args.machine and e_machine != MACHINES[args.machine]:
        fail(f"{args.library}: machine {e_machine}, expected {args.machine}")

    loads = 0
    for index in range(e_phnum):
        base = e_phoff + index * e_phentsize
        if bits == 64:
            p_type, _flags, p_offset, p_vaddr, _paddr, _filesz, _memsz, p_align = struct.unpack_from(
                "<IIQQQQQQ", data, base
            )
        else:
            p_type, p_offset, p_vaddr, _paddr, _filesz, _memsz, _flags, p_align = struct.unpack_from(
                "<IIIIIIII", data, base
            )
        if p_type != PT_LOAD:
            continue
        loads += 1
        if p_align < args.align:
            fail(f"{args.library}: PT_LOAD #{index} is aligned to {p_align}, need {args.align}")
        if p_align and (p_offset % p_align) != (p_vaddr % p_align):
            fail(f"{args.library}: PT_LOAD #{index} offset {p_offset:#x} and address {p_vaddr:#x} "
                 f"disagree modulo {p_align}")
    if loads == 0:
        fail(f"{args.library}: no loadable segments")
    print(f"check_elf: {args.library}: {loads} PT_LOAD segments aligned to >= {args.align} (ELF{bits})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
