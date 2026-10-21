#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Writes password-protected copies of tests/fixtures/report.pdf with pikepdf (qpdf):

    enc-rc4-40.pdf     revision 2, RC4 40-bit        user: user-40      owner: owner
    enc-rc4-128.pdf    revision 3, RC4 128-bit       user: user-128     owner: owner
    enc-aes-128.pdf    revision 4, AES-128           user: pässword       owner: owner
    enc-aes-256.pdf    revision 6, AES-256           user: user-256     owner: owner
    enc-open.pdf       revision 4, AES-128, empty user password (protected against editing only)

    pip install pikepdf
    python3 tools/fixtures/make_encrypted_pdf.py tests/fixtures
"""
import os
import sys

import pikepdf

out = sys.argv[1] if len(sys.argv) > 1 else "."
src = os.path.join(out, "report.pdf")
settings = [
    ("enc-rc4-40.pdf", "user-40", dict(R=2, aes=False, metadata=False)),
    ("enc-rc4-128.pdf", "user-128", dict(R=3, aes=False, metadata=False)),
    ("enc-aes-128.pdf", "pässword", dict(R=4, aes=True)),
    ("enc-aes-256.pdf", "user-256", dict(R=6)),
    ("enc-open.pdf", "", dict(R=4, aes=True)),
]
for name, user, kw in settings:
    with pikepdf.open(src) as pdf:
        pdf.save(os.path.join(out, name), encryption=pikepdf.Encryption(user=user, owner="owner", **kw))
