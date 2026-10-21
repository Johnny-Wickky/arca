<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Fixture sources

`*.fodt` / `*.fods` are flat OpenDocument sources (open in LibreOffice, or convert with
`soffice --headless --convert-to doc|xls|docx|...`). `make_decks.py` builds the PPTX decks.
The outputs are checked in under `tests/fixtures/`; the very large stress files (a 5 MB DOC,
a 2.6 MB PPT, a 1 MB XLS) are generated on demand and not committed.
