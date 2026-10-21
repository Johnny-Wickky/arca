#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Writes the password-protected Office fixtures in tests/fixtures/:

    enc-agile.docx      report.docx, agile encryption (Office 2010+), password: пароль-123
    enc-agile.xlsx      sheet.xlsx,  agile encryption,                password: s3cret
    enc-standard.docx   report.docx, standard encryption (Office 2007, AES-128), password: Password1234_

    pip install msoffcrypto-tool
    python3 tools/fixtures/make_encrypted.py tests/fixtures

The agile files are made by `msoffcrypto-tool`, an independent implementation. It cannot write the
2007 "standard" form, so that one is built here from the specification (MS-OFFCRYPTO 2.3.4) and then
checked by decrypting it with msoffcrypto-tool's own reader: if the two disagreed, this script stops.
"""
import io
import os
import struct
import sys
from hashlib import sha1

import msoffcrypto
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from msoffcrypto.method.container.ecma376_encrypted import ECMA376Encrypted
from msoffcrypto.method.ecma376_standard import ECMA376Standard

out = sys.argv[1] if len(sys.argv) > 1 else "."


def agile(src, dst, password):
    with open(os.path.join(out, src), "rb") as f, open(os.path.join(out, dst), "wb") as o:
        msoffcrypto.OfficeFile(f).encrypt(password, o)


def standard(src, dst, password):
    plain = open(os.path.join(out, src), "rb").read()
    salt = bytes(range(0x10, 0x20))
    key = ECMA376Standard.makekey_from_password(password, 0x660E, 0x8004, 0x18, 128, 16, salt)
    ecb = lambda data: Cipher(algorithms.AES(key), modes.ECB()).encryptor()
    verifier = bytes(range(0x40, 0x50))
    e = ecb(0)
    enc_verifier = e.update(verifier)
    e = ecb(0)
    enc_hash = e.update(sha1(verifier).digest() + bytes(12))
    csp = "Microsoft Enhanced RSA and AES Cryptographic Provider".encode("utf-16-le") + b"\0\0"
    header = struct.pack("<IIIIIIII", 0x24, 0, 0x660E, 0x8004, 128, 0x18, 0, 0) + csp
    verifier_block = struct.pack("<I", 16) + salt + enc_verifier + struct.pack("<I", 20) + enc_hash
    info = struct.pack("<HHII", 4, 2, 0x24, len(header)) + header + verifier_block
    padded = plain + bytes(-len(plain) % 16)
    e = ecb(0)
    package = struct.pack("<Q", len(plain)) + e.update(padded)
    buf = io.BytesIO()
    ECMA376Encrypted(package, info).write_to(buf)
    data = buf.getvalue()
    # The independent reader must agree.
    check = msoffcrypto.OfficeFile(io.BytesIO(data))
    check.load_key(password=password)
    back = io.BytesIO()
    check.decrypt(back)
    assert back.getvalue() == plain, "msoffcrypto-tool does not read the standard file back"
    open(os.path.join(out, dst), "wb").write(data)


agile("report.docx", "enc-agile.docx", "пароль-123")
agile("sheet.xlsx", "enc-agile.xlsx", "s3cret")
standard("report.docx", "enc-standard.docx", "Password1234_")
