<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Fixture sources

`*.fodt` / `*.fods` are flat OpenDocument sources (open in LibreOffice, or convert with
`soffice --headless --convert-to doc|xls|docx|...`). `make_decks.py` builds the PPTX decks.
The outputs are checked in under `tests/fixtures/`; the very large stress files (a 5 MB DOC,
a 2.6 MB PPT, a 1 MB XLS) are generated on demand and not committed.

## The `make_*.py` scripts are one-off development aids, not part of any build

Arca's build and verification chain is Rust and POSIX shell only (see `crates/arca-devtools`);
`tools/check-unsafe.sh` fails when Python appears anywhere else under `tools/`, `.github/` or
`android/`. The scripts in this directory are the single, deliberate exception, and only because
they document how a committed binary fixture was made:

* every file they generate is **committed** under `tests/fixtures/`; no build, test or CI job
  runs a script, imports it or needs its output to exist;
* they use third-party libraries that exist only in Python (`openpyxl`, `python-pptx`, `python-docx`,
  `pikepdf`, `msoffcrypto-tool`); there is no Rust equivalent, which is why the files are made
  once by independent software and kept, not regenerated;
* run one only to change or extend a fixture, in a throwaway virtual environment, and review the
  resulting binary like any other change (the test suite pins what it expects from each file).

| script | writes | needs |
|--------|--------|-------|
| `make_decks.py` | `deck-rich.pptx` (the binary `deck-rich.ppt` is converted from it with LibreOffice) | python-pptx, Pillow |
| `make_encrypted.py` | `enc-agile.docx`, `enc-agile.xlsx`, `enc-standard.docx` | msoffcrypto-tool, cryptography |
| `make_encrypted_pdf.py` | `enc-rc4-40.pdf`, `enc-rc4-128.pdf`, `enc-aes-128.pdf`, `enc-aes-256.pdf`, `enc-open.pdf` | pikepdf |
| `make_formulas.py` | `formulas.xlsx` | openpyxl |
| `make_nested.py` | `nested.docx` (converted to the committed `nested.doc` with LibreOffice) | python-docx |
| `make_visio.py` | `drawing.vsdx` (deterministic) | standard library only |
| `make_chart_deck.py` | `charts.pptx` (the chart parts are written by hand the way PowerPoint writes them) | Python standard library |
| `make_heavy_deck.py` | nothing committed: a deck of about 15 MB (picture backgrounds, picture-filled shapes, many photos) for checking "Pictures in the text" on a heavy presentation by hand | python-pptx, Pillow |
