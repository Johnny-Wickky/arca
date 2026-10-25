//! Legacy Excel workbooks (`.xls`): BIFF8 (Excel 97–2003) and BIFF5/7 (Excel 5/95).
//!
//! Worksheets become [`Grid`](crate::model::Grid)s of the *stored* values, formatted with the
//! cell's number format. Nothing is calculated or executed:
//!
//! * formulas keep their cached result ([`NoticeKind::FormulasNotEvaluated`]); Excel 4.0 macro
//!   sheets (XLM) — a common malware carrier — are listed and flagged, their formulas are not
//!   shown or run;
//! * hidden and "very hidden" sheets and hidden rows are shown and labelled;
//! * cell notes (with their author) and text boxes are collected as well;
//! * VBA, OLE objects, charts and pictures are never interpreted.
//!
//! Password-protected workbooks (`FILEPASS`) are reported, not opened.

use std::collections::BTreeMap;

use arca_container::bytes::{slice, u16_le, u32_le};
use arca_container::cfb::CompoundFile;
use arca_container::text::{CodePage, decode_codepage};

use crate::PreviewError;
use crate::model::{Block, CellKind, NoticeKind, ParaStyle, Paragraph, Run, SectionKind, style};
use crate::numfmt::{self, Format};
use crate::sink::{CellLink, GridBuilder, Sink, clip_href};

/// Most records scanned per stream.
const MAX_RECORDS: usize = 32 << 20;
/// Most strings kept in the shared string table.
const MAX_STRINGS: usize = 4 << 20;
/// Most cell formats kept.
const MAX_XF: usize = 1 << 20;
/// Most worksheets.
const MAX_SHEETS: usize = 4_096;

// Record identifiers.
const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const CONTINUE: u16 = 0x003C;
const FILEPASS: u16 = 0x002F;
const CODEPAGE: u16 = 0x0042;
const DATEMODE: u16 = 0x0022;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00FC;
const XF: u16 = 0x00E0;
const FORMAT: u16 = 0x041E;
const ROW: u16 = 0x0208;
const COLINFO: u16 = 0x007D;
const LABELSST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const RSTRING: u16 = 0x00D6;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const BOOLERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;
/// Records that may sit between a FORMULA and its STRING result.
const SHRFMLA: u16 = 0x04BC;
const ARRAY: u16 = 0x0221;
const TABLE: u16 = 0x0236;
const NOTE: u16 = 0x001C;
const OBJ: u16 = 0x005D;
const TXO: u16 = 0x01B6;
const HLINK: u16 = 0x01B8;

fn malformed(why: &str) -> PreviewError {
    PreviewError::Malformed(format!("Excel workbook: {why}"))
}

fn usz(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

// ---------------------------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------------------------

struct Rec<'a> {
    id: u16,
    data: &'a [u8],
}

/// A cursor over the records of a BIFF stream.
#[derive(Clone)]
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    seen: usize,
}

impl<'a> Reader<'a> {
    fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos, seen: 0 }
    }

    fn peek_id(&self) -> Option<u16> {
        u16_le(self.data, self.pos)
    }

    fn next_rec(&mut self) -> Option<Rec<'a>> {
        if self.seen >= MAX_RECORDS {
            return None;
        }
        let id = u16_le(self.data, self.pos)?;
        let len = usize::from(u16_le(self.data, self.pos.checked_add(2)?)?);
        let data = slice(self.data, self.pos.checked_add(4)?, len)?;
        self.pos = self.pos.checked_add(4)?.checked_add(len)?;
        self.seen += 1;
        Some(Rec { id, data })
    }

    /// The `CONTINUE` records that directly follow, consumed.
    fn continues(&mut self) -> Vec<&'a [u8]> {
        let mut out = Vec::new();
        while self.peek_id() == Some(CONTINUE) {
            let Some(r) = self.next_rec() else { break };
            out.push(r.data);
            if out.len() > 65_536 {
                break;
            }
        }
        out
    }
}

/// A byte cursor over the pieces of a record and its `CONTINUE` records.
struct Chunks<'a> {
    parts: Vec<&'a [u8]>,
    ci: usize,
    pos: usize,
}

impl<'a> Chunks<'a> {
    fn new(parts: Vec<&'a [u8]>) -> Self {
        Self { parts, ci: 0, pos: 0 }
    }

    /// Moves past exhausted pieces; `false` at the end.
    fn settle(&mut self) -> bool {
        while let Some(p) = self.parts.get(self.ci) {
            if self.pos < p.len() {
                return true;
            }
            self.ci += 1;
            self.pos = 0;
        }
        false
    }

    fn u8(&mut self) -> Option<u8> {
        if !self.settle() {
            return None;
        }
        let b = *self.parts.get(self.ci)?.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        let a = self.u8()?;
        let b = self.u8()?;
        Some(u16::from_le_bytes([a, b]))
    }

    fn u32(&mut self) -> Option<u32> {
        let a = self.u16()?;
        let b = self.u16()?;
        Some(u32::from(a) | (u32::from(b) << 16))
    }

    fn skip(&mut self, mut n: usize) {
        while n > 0 && self.settle() {
            let avail = self.parts.get(self.ci).map_or(0, |p| p.len() - self.pos);
            let take = avail.min(n);
            self.pos += take;
            n -= take;
        }
    }

    /// Reads `cch` characters of a string whose encoding flag is `high` (UTF-16 when set,
    /// otherwise Latin-1). A string that runs across a record boundary restarts with a new
    /// flag byte. At most `keep` characters are returned; the rest are consumed.
    fn chars(&mut self, cch: usize, mut high: bool, keep: usize) -> String {
        let mut units: Vec<u16> = Vec::with_capacity(cch.min(keep).min(4_096));
        let mut left = cch;
        while left > 0 {
            let Some(part) = self.parts.get(self.ci) else { break };
            if self.pos >= part.len() {
                self.ci += 1;
                self.pos = 0;
                let Some(next) = self.parts.get(self.ci) else { break };
                high = next.first().is_some_and(|f| f & 1 == 1);
                self.pos = 1;
                continue;
            }
            let avail = part.len() - self.pos;
            let n = if high { (avail / 2).min(left) } else { avail.min(left) };
            if n == 0 {
                self.pos = part.len();
                continue;
            }
            let Some(bytes) = part.get(self.pos..self.pos + if high { n * 2 } else { n }) else {
                break;
            };
            if high {
                for c in bytes.chunks_exact(2) {
                    if units.len() < keep {
                        units.push(u16::from_le_bytes([
                            c.first().copied().unwrap_or(0),
                            c.get(1).copied().unwrap_or(0),
                        ]));
                    }
                }
            } else {
                for &b in bytes {
                    if units.len() < keep {
                        units.push(u16::from(b));
                    }
                }
            }
            self.pos += bytes.len();
            left -= n;
        }
        String::from_utf16_lossy(&units)
    }

    /// A string with a 16-bit length, a flag byte and optional rich-text and phonetic data.
    fn xl_string(&mut self, keep: usize) -> Option<String> {
        let cch = usize::from(self.u16()?);
        self.xl_string_body(cch, keep)
    }

    fn xl_string_body(&mut self, cch: usize, keep: usize) -> Option<String> {
        let flags = self.u8()?;
        let runs = if flags & 0x08 != 0 { usize::from(self.u16()?) } else { 0 };
        let ext = if flags & 0x04 != 0 { usz(self.u32()?) } else { 0 };
        let s = self.chars(cch, flags & 1 == 1, keep);
        self.skip(runs.saturating_mul(4));
        self.skip(ext);
        Some(s)
    }
}

// ---------------------------------------------------------------------------------------------
// Workbook
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SheetInfo {
    offset: usize,
    hidden: bool,
    /// BIFF sheet type: 0 worksheet, 1 Excel 4.0 macro sheet, 2 chart, 6 VBA module.
    kind: u8,
    name: String,
}

struct Book {
    biff8: bool,
    codepage: CodePage,
    date1904: bool,
    sheets: Vec<SheetInfo>,
    sst: Vec<String>,
    /// Number format id of each cell format (`XF`).
    xf: Vec<u16>,
    formats: BTreeMap<u16, String>,
    cache: BTreeMap<u16, Format>,
    max_text: usize,
}

impl Book {
    /// An 8-bit string in the workbook's code page.
    fn ansi(&self, bytes: &[u8]) -> String {
        decode_codepage(bytes, self.codepage)
    }

    fn format_for(&mut self, ixfe: u16) -> Option<&Format> {
        let ifmt = self.xf.get(usize::from(ixfe)).copied().unwrap_or(0);
        if ifmt == 0 {
            return None;
        }
        if !self.cache.contains_key(&ifmt) {
            let code = match self.formats.get(&ifmt) {
                Some(c) => c.as_str(),
                None => numfmt::builtin(u32::from(ifmt))?,
            };
            let parsed = numfmt::parse(code);
            self.cache.insert(ifmt, parsed);
        }
        self.cache.get(&ifmt)
    }

    fn number(&mut self, ixfe: u16, v: f64) -> (String, CellKind) {
        let date1904 = self.date1904;
        match self.format_for(ixfe) {
            Some(f) => f.number(v, date1904),
            None => (numfmt::general(v), CellKind::Number),
        }
    }
}

fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        0x2B => "#GETTING_DATA",
        _ => "#ERROR!",
    }
}

/// Decodes an `RK` number.
fn rk_value(rk: u32) -> f64 {
    let scaled = rk & 1 == 1;
    let v = if rk & 2 == 2 {
        f64::from(rk.cast_signed() >> 2)
    } else {
        f64::from_bits(u64::from(rk & 0xFFFF_FFFC) << 32)
    };
    if scaled { v / 100.0 } else { v }
}

/// Reads the workbook-global records. Returns `Err` for protected workbooks.
fn read_globals(data: &[u8], sink: &mut Sink<'_>) -> Result<Book, PreviewError> {
    let mut rd = Reader::at(data, 0);
    let first = rd.next_rec().ok_or_else(|| malformed("the stream is empty"))?;
    if first.id != BOF {
        return Err(malformed("the stream does not start with a BOF record"));
    }
    let version = u16_le(first.data, 0).unwrap_or(0);
    let biff8 = version >= 0x0600;
    if !biff8 && version != 0x0500 {
        return Err(PreviewError::Unsupported(crate::model::FileFormat::Xls));
    }
    let mut book = Book {
        biff8,
        codepage: CodePage::from_id(1252),
        date1904: false,
        sheets: Vec::new(),
        sst: Vec::new(),
        xf: Vec::new(),
        formats: BTreeMap::new(),
        cache: BTreeMap::new(),
        max_text: sink.policy.max_cell_bytes,
    };
    let mut depth = 1i32;
    while let Some(rec) = rd.next_rec() {
        match rec.id {
            BOF => depth += 1,
            EOF => {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
            }
            FILEPASS => return Err(PreviewError::Encrypted),
            _ if depth > 1 => {}
            CODEPAGE => {
                if let Some(cp) = u16_le(rec.data, 0) {
                    book.codepage = CodePage::from_id(cp);
                }
            }
            DATEMODE => book.date1904 = u16_le(rec.data, 0) == Some(1),
            BOUNDSHEET if book.sheets.len() < MAX_SHEETS => {
                let (Some(off), Some(state), Some(kind)) =
                    (u32_le(rec.data, 0), rec.data.get(4).copied(), rec.data.get(5).copied())
                else {
                    continue;
                };
                let cch = usize::from(rec.data.get(6).copied().unwrap_or(0));
                let name = if biff8 {
                    let high = rec.data.get(7).is_some_and(|f| f & 1 == 1);
                    let bytes = rec.data.get(8..).unwrap_or(&[]);
                    if high {
                        let units: Vec<u16> = bytes
                            .chunks_exact(2)
                            .take(cch)
                            .map(|c| {
                                u16::from_le_bytes([
                                    c.first().copied().unwrap_or(0),
                                    c.get(1).copied().unwrap_or(0),
                                ])
                            })
                            .collect();
                        String::from_utf16_lossy(&units)
                    } else {
                        bytes.iter().take(cch).map(|&b| char::from(b)).collect()
                    }
                } else {
                    book.ansi(rec.data.get(7..).unwrap_or(&[]).get(..cch).unwrap_or(&[]))
                };
                book.sheets.push(SheetInfo {
                    offset: usz(off),
                    hidden: state & 3 != 0,
                    kind,
                    name,
                });
            }
            SST => {
                let mut parts = vec![rec.data];
                parts.extend(rd.continues());
                read_sst(&mut book, parts);
            }
            XF if book.xf.len() < MAX_XF => book.xf.push(u16_le(rec.data, 2).unwrap_or(0)),
            FORMAT => {
                if let Some(ifmt) = u16_le(rec.data, 0) {
                    let code = if biff8 {
                        Chunks::new(vec![rec.data.get(2..).unwrap_or(&[])]).xl_string(256)
                    } else {
                        let cch = usize::from(rec.data.get(2).copied().unwrap_or(0));
                        Some(book.ansi(rec.data.get(3..).unwrap_or(&[]).get(..cch).unwrap_or(&[])))
                    };
                    if let Some(code) = code {
                        book.formats.insert(ifmt, code);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(book)
}

fn read_sst(book: &mut Book, parts: Vec<&[u8]>) {
    let mut ch = Chunks::new(parts);
    let _total = ch.u32();
    let Some(unique) = ch.u32() else { return };
    let unique = usz(unique).min(MAX_STRINGS);
    for _ in 0..unique {
        match ch.xl_string(book.max_text) {
            Some(s) => book.sst.push(s),
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worksheets
// ---------------------------------------------------------------------------------------------

struct CellOut {
    row: u32,
    col: u32,
    kind: CellKind,
    text: String,
}

struct Note {
    row: u32,
    col: u32,
    id: u16,
    author: String,
    /// BIFF5: the note text is inside the record.
    inline: Option<String>,
}

#[derive(Default)]
struct SheetData {
    cells: Vec<CellOut>,
    hidden_rows: Vec<u32>,
    notes: Vec<Note>,
    /// Text objects: the id of the object they belong to and their text.
    texts: Vec<(Option<u16>, String)>,
    formulas: u64,
    truncated: bool,
    charts: bool,
    /// Hyperlinks that lead out of the workbook.
    links: Vec<CellLink>,
    /// Column ranges (zero-based, inclusive) the sheet hides.
    hidden_cols: Vec<(u32, u32)>,
    /// Bytes of cell text taken so far, and the most the sheet may take.
    bytes: usize,
    max_bytes: usize,
}

fn parse_sheet(
    book: &mut Book,
    data: &[u8],
    offset: usize,
    (max_cells, max_bytes): (usize, usize),
) -> SheetData {
    let mut out = SheetData { max_bytes, ..SheetData::default() };
    let mut rd = Reader::at(data, offset);
    let Some(first) = rd.next_rec() else { return out };
    if first.id != BOF {
        return out;
    }
    let mut depth = 1i32;
    let mut pending: Option<usize> = None;
    let mut last_obj: Option<u16> = None;
    while let Some(rec) = rd.next_rec() {
        match rec.id {
            BOF => {
                depth += 1;
                out.charts = true;
                continue;
            }
            EOF => {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                continue;
            }
            _ if depth > 1 => continue,
            _ => {}
        }
        if !matches!(rec.id, STRING | SHRFMLA | ARRAY | TABLE) {
            // The text of a string result follows its formula, after the shared-formula, array
            // or table record the formula heads (if any).
            pending = None;
        }
        match rec.id {
            COLINFO => {
                if u16_le(rec.data, 8).unwrap_or(0) & 1 != 0
                    && out.hidden_cols.len() < 4_096
                    && let (Some(first), Some(last)) = (u16_le(rec.data, 0), u16_le(rec.data, 2))
                {
                    out.hidden_cols.push((u32::from(first), u32::from(last)));
                }
            }
            ROW => {
                let row = u32::from(u16_le(rec.data, 0).unwrap_or(0));
                if u16_le(rec.data, 12).unwrap_or(0) & 0x20 != 0 {
                    out.hidden_rows.push(row);
                }
            }
            LABELSST => {
                let (Some(row), Some(col), Some(i)) =
                    (u16_le(rec.data, 0), u16_le(rec.data, 2), u32_le(rec.data, 6))
                else {
                    continue;
                };
                if let Some(s) = book.sst.get(usz(i))
                    && room_for(&mut out, max_cells, s.len())
                {
                    push_cell(&mut out, max_cells, row, col, CellKind::Text, s.clone());
                }
            }
            LABEL | RSTRING => {
                let (Some(row), Some(col)) = (u16_le(rec.data, 0), u16_le(rec.data, 2)) else {
                    continue;
                };
                let body = rec.data.get(6..).unwrap_or(&[]);
                let text = if book.biff8 && rec.id == LABEL {
                    Chunks::new(vec![body]).xl_string(book.max_text).unwrap_or_default()
                } else {
                    let cch = usize::from(u16_le(body, 0).unwrap_or(0));
                    let bytes = body.get(2..).unwrap_or(&[]);
                    book.ansi(bytes.get(..cch.min(bytes.len())).unwrap_or(&[]))
                };
                push_cell(&mut out, max_cells, row, col, CellKind::Text, text);
            }
            NUMBER => {
                let (Some(row), Some(col), Some(ixfe)) =
                    (u16_le(rec.data, 0), u16_le(rec.data, 2), u16_le(rec.data, 4))
                else {
                    continue;
                };
                let Some(bytes) = slice(rec.data, 6, 8) else { continue };
                let v = f64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]));
                let (text, kind) = book.number(ixfe, v);
                push_cell(&mut out, max_cells, row, col, kind, text);
            }
            RK => {
                let (Some(row), Some(col), Some(ixfe), Some(rk)) = (
                    u16_le(rec.data, 0),
                    u16_le(rec.data, 2),
                    u16_le(rec.data, 4),
                    u32_le(rec.data, 6),
                ) else {
                    continue;
                };
                let (text, kind) = book.number(ixfe, rk_value(rk));
                push_cell(&mut out, max_cells, row, col, kind, text);
            }
            MULRK => {
                let (Some(row), Some(first_col)) = (u16_le(rec.data, 0), u16_le(rec.data, 2))
                else {
                    continue;
                };
                let body = rec.data.get(4..rec.data.len().saturating_sub(2)).unwrap_or(&[]);
                for (i, chunk) in body.chunks_exact(6).enumerate() {
                    let ixfe = u16_le(chunk, 0).unwrap_or(0);
                    let rk = u32_le(chunk, 2).unwrap_or(0);
                    let col = first_col.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
                    let (text, kind) = book.number(ixfe, rk_value(rk));
                    push_cell(&mut out, max_cells, row, col, kind, text);
                }
            }
            BOOLERR => {
                let (Some(row), Some(col), Some(v), Some(is_err)) = (
                    u16_le(rec.data, 0),
                    u16_le(rec.data, 2),
                    rec.data.get(6).copied(),
                    rec.data.get(7).copied(),
                ) else {
                    continue;
                };
                if is_err != 0 {
                    push_cell(
                        &mut out,
                        max_cells,
                        row,
                        col,
                        CellKind::Error,
                        error_text(v).to_owned(),
                    );
                } else {
                    push_cell(
                        &mut out,
                        max_cells,
                        row,
                        col,
                        CellKind::Bool,
                        (if v != 0 { "TRUE" } else { "FALSE" }).to_owned(),
                    );
                }
            }
            FORMULA => {
                out.formulas = out.formulas.saturating_add(1);
                let (Some(row), Some(col), Some(ixfe)) =
                    (u16_le(rec.data, 0), u16_le(rec.data, 2), u16_le(rec.data, 4))
                else {
                    continue;
                };
                let Some(res) = slice(rec.data, 6, 8) else { continue };
                if u16_le(res, 6) == Some(0xFFFF) {
                    match res.first().copied().unwrap_or(0) {
                        0 => {
                            // The text follows in a STRING record.
                            let before = out.cells.len();
                            push_cell(&mut out, max_cells, row, col, CellKind::Text, String::new());
                            pending = if out.cells.len() > before { before.into() } else { None };
                        }
                        1 => {
                            let v = res.get(2).copied().unwrap_or(0);
                            push_cell(
                                &mut out,
                                max_cells,
                                row,
                                col,
                                CellKind::Bool,
                                (if v != 0 { "TRUE" } else { "FALSE" }).to_owned(),
                            );
                        }
                        2 => {
                            let v = res.get(2).copied().unwrap_or(0);
                            push_cell(
                                &mut out,
                                max_cells,
                                row,
                                col,
                                CellKind::Error,
                                error_text(v).to_owned(),
                            );
                        }
                        _ => {}
                    }
                } else {
                    let v = f64::from_le_bytes(res.try_into().unwrap_or([0; 8]));
                    let (text, kind) = book.number(ixfe, v);
                    push_cell(&mut out, max_cells, row, col, kind, text);
                }
            }
            STRING => {
                if let Some(i) = pending.take() {
                    let mut parts = vec![rec.data];
                    parts.extend(rd.continues());
                    let s = if book.biff8 {
                        Chunks::new(parts).xl_string(book.max_text).unwrap_or_default()
                    } else {
                        let cch = usize::from(u16_le(rec.data, 0).unwrap_or(0));
                        book.ansi(rec.data.get(2..).unwrap_or(&[]).get(..cch).unwrap_or(&[]))
                    };
                    if let Some(c) = out.cells.get_mut(i) {
                        c.text = s;
                    }
                }
            }
            NOTE => {
                let (Some(row), Some(col)) = (u16_le(rec.data, 0), u16_le(rec.data, 2)) else {
                    continue;
                };
                if book.biff8 {
                    let id = u16_le(rec.data, 6).unwrap_or(0);
                    let author = Chunks::new(vec![rec.data.get(8..).unwrap_or(&[])])
                        .xl_string(256)
                        .unwrap_or_default();
                    out.notes.push(Note {
                        row: u32::from(row),
                        col: u32::from(col),
                        id,
                        author,
                        inline: None,
                    });
                } else {
                    let cch = usize::from(u16_le(rec.data, 4).unwrap_or(0));
                    let text =
                        book.ansi(rec.data.get(6..).unwrap_or(&[]).get(..cch).unwrap_or(&[]));
                    out.notes.push(Note {
                        row: u32::from(row),
                        col: u32::from(col),
                        id: 0,
                        author: String::new(),
                        inline: Some(text),
                    });
                }
            }
            HLINK => {
                if out.links.len() < 20_000
                    && let Some(link) = parse_hlink(rec.data)
                {
                    out.links.push(link);
                }
            }
            OBJ => {
                // The common-object subrecord comes first: type, then the object id.
                if u16_le(rec.data, 0) == Some(0x0015) {
                    last_obj = u16_le(rec.data, 6);
                }
            }
            TXO => {
                let cch = usize::from(u16_le(rec.data, 10).unwrap_or(0));
                let mut parts = Vec::new();
                let mut have = 0usize;
                // Text may be split over several records; the run formatting follows.
                while have < cch && rd.peek_id() == Some(CONTINUE) {
                    let Some(c) = rd.next_rec() else { break };
                    have += if c.data.first().is_some_and(|f| f & 1 == 1) {
                        c.data.len().saturating_sub(1) / 2
                    } else {
                        c.data.len().saturating_sub(1)
                    };
                    parts.push(c.data);
                }
                let mut ch = Chunks::new(parts);
                let first_flag = ch.parts.first().and_then(|p| p.first()).copied().unwrap_or(0);
                ch.pos = 1;
                let text = ch.chars(cch, first_flag & 1 == 1, book.max_text);
                // Formatting runs.
                let _ = rd.continues();
                if out.texts.len() < 65_536 {
                    out.texts.push((last_obj.take(), text));
                }
            }
            _ => {}
        }
    }
    out
}

/// The class id of the URL moniker (`79EAC9E0-BAF9-11CE-8C82-00AA004BA90B`).
const URL_MONIKER: [u8; 16] = [
    0xE0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B, 0xA9, 0x0B,
];
/// The class id of the file moniker (`00000303-0000-0000-C000-000000000046`).
const FILE_MONIKER: [u8; 16] = [
    0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];
/// Longest string of a hyperlink record, in UTF-16 units.
const MAX_HLINK_CHARS: usize = 8_192;

/// A bounds-checked cursor over one record.
struct Cur<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = slice(self.d, self.pos, n)?;
        self.pos = self.pos.checked_add(n)?;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        let v = u16_le(self.d, self.pos)?;
        self.pos = self.pos.checked_add(2)?;
        Some(v)
    }

    fn u32(&mut self) -> Option<u32> {
        let v = u32_le(self.d, self.pos)?;
        self.pos = self.pos.checked_add(4)?;
        Some(v)
    }

    /// A `HyperlinkString`: a count of UTF-16 units (with the closing NUL), then the units.
    fn string(&mut self) -> Option<String> {
        let n = usz(self.u32()?);
        if n > MAX_HLINK_CHARS {
            return None;
        }
        Some(utf16_until_nul(self.bytes(n.checked_mul(2)?)?))
    }
}

fn utf16_until_nul(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| {
            u16::from_le_bytes([c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)])
        })
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A `HLINK` record: the cells it covers and where it leads (a web address or a file). A link
/// that only names a place inside the workbook is not a link out and gives `None`.
fn parse_hlink(d: &[u8]) -> Option<CellLink> {
    let row1 = u32::from(u16_le(d, 0)?);
    let row2 = u32::from(u16_le(d, 2)?);
    let col1 = u32::from(u16_le(d, 4)?);
    let col2 = u32::from(u16_le(d, 6)?);
    // 16 bytes of class id and a version (2) precede the flags.
    let flags = u32_le(d, 28)?;
    let mut cur = Cur { d, pos: 32 };
    if flags & 0x10 != 0 {
        cur.string()?; // display name
    }
    if flags & 0x80 != 0 {
        cur.string()?; // target frame
    }
    let mut target = None;
    if flags & 0x01 != 0 {
        if flags & 0x100 != 0 {
            target = Some(cur.string()?);
        } else {
            let class: [u8; 16] = cur.bytes(16)?.try_into().ok()?;
            if class == URL_MONIKER {
                let len = usz(cur.u32()?);
                target = Some(utf16_until_nul(cur.bytes(len)?));
            } else if class == FILE_MONIKER {
                cur.u16()?; // how many directories up
                let ansi_len = usz(cur.u32()?);
                let ansi = cur.bytes(ansi_len)?;
                cur.bytes(2 + 2 + 20)?; // end server, version, reserved
                let size = usz(cur.u32()?);
                target = Some(if size >= 6 {
                    let wide = usz(cur.u32()?);
                    cur.u16()?;
                    utf16_until_nul(cur.bytes(wide.min(size - 6))?)
                } else {
                    ansi.iter().take_while(|&&b| b != 0).map(|&b| char::from(b)).collect()
                });
            } else {
                // Composite, item and other monikers: not understood, so not shown.
                return None;
            }
        }
    }
    let mut href = target.filter(|t| !t.trim().is_empty())?.trim().to_owned();
    if flags & 0x08 != 0
        && let Some(location) = cur.string()
        && !location.is_empty()
    {
        href.push('#');
        href.push_str(&location);
    }
    Some(CellLink { row1: row1 + 1, row2: row2 + 1, col1, col2, href: clip_href(&href, 2_048) })
}

/// `true` if one more cell of `len` bytes of text fits the sheet's cell and byte limits; if not,
/// the sheet is marked truncated. Checked *before* a shared string is cloned for a cell: a few
/// long strings referenced by a million cells must not be multiplied in memory.
fn room_for(out: &mut SheetData, max_cells: usize, len: usize) -> bool {
    if out.cells.len() >= max_cells || out.bytes.saturating_add(len) > out.max_bytes {
        out.truncated = true;
        return false;
    }
    true
}

fn push_cell(
    out: &mut SheetData,
    max_cells: usize,
    row: u16,
    col: u16,
    kind: CellKind,
    text: String,
) {
    if !room_for(out, max_cells, text.len()) {
        return;
    }
    out.bytes = out.bytes.saturating_add(text.len());
    out.cells.push(CellOut { row: u32::from(row), col: u32::from(col), kind, text });
}

/// `A`, `B`, …, `AA`, … for a zero-based column.
fn column_name(mut col: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(char::from(b'A'.saturating_add(u8::try_from(col % 26).unwrap_or(0))));
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.iter().rev().collect()
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Extracts a workbook.
pub(crate) fn extract(cf: &CompoundFile<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let idx = cf
        .find("Workbook")
        .or_else(|| cf.find("Book"))
        .ok_or_else(|| malformed("the Workbook stream is missing"))?;
    let data =
        cf.read_stream(idx).map_err(|e| PreviewError::Malformed(format!("Excel workbook: {e}")))?;
    let mut book = read_globals(&data, sink)?;
    if !book.biff8 {
        sink.note(NoticeKind::BestEffort, "excel 5/95");
    }

    let mut formulas = 0u64;
    let mut comments: Vec<(String, String, String, String)> = Vec::new();
    let mut boxes: Vec<String> = Vec::new();
    let mut macro_sheets = false;
    let mut charts = false;
    let sheets = std::mem::take(&mut book.sheets);
    for sheet in &sheets {
        if sink.is_full() {
            break;
        }
        if sheet.kind != 0 && sheet.kind != 1 {
            // Charts and VBA modules have no cells.
            charts |= sheet.kind == 2;
            continue;
        }
        macro_sheets |= sheet.kind == 1;
        let name = if sheet.kind == 1 {
            format!("{} (Excel 4.0 macro sheet)", sheet.name)
        } else {
            sheet.name.clone()
        };
        let Some(sec) = sink.open_section(SectionKind::Sheet, &name, sheet.hidden) else { break };
        let max_cells = sink.policy.max_grid_cells.min(sink.items_left());
        let mut sd = parse_sheet(&mut book, &data, sheet.offset, (max_cells, sink.chars_left()));
        formulas = formulas.saturating_add(sd.formulas);
        charts |= sd.charts;

        // Stable sort: for a repeated cell the last record wins.
        sd.cells.sort_by_key(|c| (c.row, c.col));
        let mut grid =
            GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
        for &(first, last) in &sd.hidden_cols {
            grid.hide_columns(first, last);
        }
        sd.hidden_rows.sort_unstable();
        let mut current: Option<u32> = None;
        let mut iter = sd.cells.iter().peekable();
        'cells: while let Some(c) = iter.next() {
            if iter.peek().is_some_and(|n| n.row == c.row && n.col == c.col) {
                continue;
            }
            if current != Some(c.row) {
                let hidden = sd.hidden_rows.binary_search(&c.row).is_ok();
                if !grid.row(c.row.saturating_add(1), hidden) {
                    break 'cells;
                }
                current = Some(c.row);
            }
            if !grid.cell(c.col, c.kind, c.text.clone()) {
                break;
            }
        }
        let hidden_cols = grid.hidden_columns();
        if hidden_cols > 0 {
            let kind = if grid.omits_hidden() {
                NoticeKind::HiddenOmitted
            } else {
                NoticeKind::HiddenShown
            };
            sink.count(kind, "columns", hidden_cols);
        }
        let mut grid = grid.finish();
        if !grid.attach_links(&sd.links) {
            sink.truncated("hyperlinks");
        }
        grid.truncated |= sd.truncated;
        if grid.truncated {
            sink.truncated("grid");
        }
        sink.push(sec, Block::Grid(grid));

        // Notes, matched to their text objects by object id; other text objects are text boxes.
        let mut used = vec![false; sd.texts.len()];
        for n in &sd.notes {
            let text = match &n.inline {
                Some(t) => t.clone(),
                None => match sd.texts.iter().position(|(id, _)| *id == Some(n.id)) {
                    Some(p) => {
                        if let Some(u) = used.get_mut(p) {
                            *u = true;
                        }
                        sd.texts.get(p).map(|t| t.1.clone()).unwrap_or_default()
                    }
                    None => String::new(),
                },
            };
            if !text.is_empty() {
                comments.push((
                    sheet.name.clone(),
                    format!("{}{}", column_name(n.col), n.row.saturating_add(1)),
                    n.author.clone(),
                    text,
                ));
            }
        }
        for ((_, text), used) in sd.texts.iter().zip(&used) {
            if !used && !text.trim().is_empty() {
                boxes.push(text.clone());
            }
        }
    }

    if !comments.is_empty()
        && let Some(sec) = sink.open_section(SectionKind::Comments, "", false)
    {
        for (sheet, cell, author, text) in comments {
            let mut runs = Vec::new();
            if !author.is_empty() {
                runs.push(Run {
                    text: format!("{author}: "),
                    style: style::BOLD,
                    href: String::new(),
                });
            }
            runs.push(Run { text, style: 0, href: String::new() });
            let p = Paragraph {
                style: ParaStyle::Comment,
                marker: format!("{sheet}!{cell}"),
                runs,
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                break;
            }
        }
    }
    if !boxes.is_empty()
        && let Some(sec) = sink.open_section(SectionKind::Notes, "Text boxes", false)
    {
        for text in boxes {
            for line in text.split(['\r', '\n']).filter(|l| !l.trim().is_empty()) {
                if !sink.para(sec, Paragraph::plain(line)) {
                    break;
                }
            }
        }
    }
    if formulas > 0 {
        sink.count(NoticeKind::FormulasNotEvaluated, "", formulas);
    }
    if macro_sheets {
        sink.note(NoticeKind::Unsupported, "excel 4.0 macro sheets (formulas not shown)");
    }
    if charts {
        sink.note(NoticeKind::Unsupported, "charts");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileFormat, Grid, Preview};
    use crate::testkit::{Lcg, fuzz_iterations, ok, paragraphs, run, section_of, text_of};
    use arca_container::cfb::writer::CfbBuilder;

    fn rec(id: u16, data: &[u8]) -> Vec<u8> {
        let mut o = id.to_le_bytes().to_vec();
        o.extend(u16::try_from(data.len()).expect("record size").to_le_bytes());
        o.extend(data);
        o
    }

    fn bof(dt: u16, version: u16) -> Vec<u8> {
        let mut d = version.to_le_bytes().to_vec();
        d.extend(dt.to_le_bytes());
        d.extend([0u8; 12]);
        rec(BOF, &d)
    }

    fn eof() -> Vec<u8> {
        rec(EOF, &[])
    }

    /// A BIFF8 string with a 16-bit length (`wide` forces UTF-16).
    fn xl(s: &str, wide: bool) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let wide = wide || units.iter().any(|&u| u > 0xFF);
        let mut o = u16::try_from(units.len()).expect("len").to_le_bytes().to_vec();
        o.push(u8::from(wide));
        if wide {
            for u in units {
                o.extend(u.to_le_bytes());
            }
        } else {
            o.extend(units.iter().map(|&u| u8::try_from(u).unwrap_or(b'?')));
        }
        o
    }

    fn number(row: u16, col: u16, ixfe: u16, v: f64) -> Vec<u8> {
        let mut d = [row.to_le_bytes(), col.to_le_bytes(), ixfe.to_le_bytes()].concat();
        d.extend(v.to_le_bytes());
        rec(NUMBER, &d)
    }

    fn labelsst(row: u16, col: u16, i: u32) -> Vec<u8> {
        let mut d = [row.to_le_bytes(), col.to_le_bytes(), 0u16.to_le_bytes()].concat();
        d.extend(i.to_le_bytes());
        rec(LABELSST, &d)
    }

    fn xf(ifmt: u16) -> Vec<u8> {
        let mut d = vec![0u8; 20];
        d[2..4].copy_from_slice(&ifmt.to_le_bytes());
        rec(XF, &d)
    }

    fn boundsheet(offset: u32, state: u8, kind: u8, name: &str) -> Vec<u8> {
        let mut d = offset.to_le_bytes().to_vec();
        d.push(state);
        d.push(kind);
        let units: Vec<u16> = name.encode_utf16().collect();
        d.push(u8::try_from(units.len()).expect("name"));
        let wide = units.iter().any(|&u| u > 0xFF);
        d.push(u8::from(wide));
        for u in units {
            if wide {
                d.extend(u.to_le_bytes());
            } else {
                d.push(u8::try_from(u).unwrap_or(b'?'));
            }
        }
        rec(BOUNDSHEET, &d)
    }

    fn sst(strings: &[&str]) -> Vec<u8> {
        let mut d = u32::try_from(strings.len()).expect("n").to_le_bytes().to_vec();
        d.extend(u32::try_from(strings.len()).expect("n").to_le_bytes());
        for s in strings {
            d.extend(xl(s, false));
        }
        rec(SST, &d)
    }

    struct Sheet {
        name: &'static str,
        state: u8,
        kind: u8,
        records: Vec<Vec<u8>>,
    }

    fn sheet(name: &'static str, records: Vec<Vec<u8>>) -> Sheet {
        Sheet { name, state: 0, kind: 0, records }
    }

    /// Builds a workbook stream. `globals` are extra global records (formats, SST, …).
    fn workbook(globals: &[Vec<u8>], sheets: &[Sheet]) -> Vec<u8> {
        let build = |offsets: &[u32]| -> Vec<u8> {
            let mut g = bof(5, 0x0600);
            for r in globals {
                g.extend(r);
            }
            for (s, off) in sheets.iter().zip(offsets) {
                g.extend(boundsheet(*off, s.state, s.kind, s.name));
            }
            g.extend(eof());
            g
        };
        let probe = build(&vec![0; sheets.len()]);
        let mut out = probe.clone();
        let mut offsets = Vec::new();
        for s in sheets {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend(bof(0x10, 0x0600));
            for r in &s.records {
                out.extend(r);
            }
            out.extend(eof());
        }
        let mut final_out = build(&offsets);
        final_out.extend(out.get(probe.len()..).unwrap_or(&[]));
        final_out
    }

    fn file(stream: Vec<u8>) -> Vec<u8> {
        let mut b = CfbBuilder::new();
        assert!(b.add_stream("Workbook", stream).is_ok());
        b.finish().unwrap_or_default()
    }

    fn grid(p: &Preview, i: usize) -> &Grid {
        match p
            .sections
            .iter()
            .filter(|s| s.kind == SectionKind::Sheet)
            .nth(i)
            .and_then(|s| s.blocks.first())
        {
            Some(Block::Grid(g)) => g,
            other => panic!("no grid: {other:?}"),
        }
    }

    fn cell(g: &Grid, row: u32, col: u32) -> Option<(String, CellKind)> {
        g.rows
            .iter()
            .find(|r| r.index == row)?
            .cells
            .iter()
            .find(|c| c.col == col)
            .map(|c| (c.text.clone(), c.kind))
    }

    #[test]
    fn values_formats_strings_booleans_and_errors() {
        let mut mulrk = [2u16.to_le_bytes(), 0u16.to_le_bytes()].concat();
        for v in [100u32 << 2 | 2, 12_345 << 2 | 3] {
            mulrk.extend(0u16.to_le_bytes());
            mulrk.extend(v.to_le_bytes());
        }
        mulrk.extend(1u16.to_le_bytes());
        let boolerr = |row: u16, col: u16, v: u8, e: u8| {
            rec(
                BOOLERR,
                &[row.to_le_bytes(), col.to_le_bytes(), 0u16.to_le_bytes(), [v, e]].concat(),
            )
        };
        let wb = workbook(
            &[
                xf(0),
                xf(0),
                xf(10),
                xf(14),
                rec(FORMAT, &[[200u8, 0].as_slice(), &xl("0.0\" kg\"", false)].concat()),
                xf(200),
                sst(&["Hello", "Привет"]),
            ],
            &[sheet(
                "Data",
                vec![
                    labelsst(0, 0, 0),
                    labelsst(0, 1, 1),
                    number(1, 0, 2, 0.256),
                    number(1, 1, 3, 46_000.0),
                    number(1, 2, 4, 12.34),
                    number(1, 3, 0, 1234.5),
                    rec(MULRK, &mulrk),
                    boolerr(3, 0, 1, 0),
                    boolerr(3, 1, 0x07, 1),
                ],
            )],
        );
        let p = ok(&file(wb));
        assert_eq!(p.format, FileFormat::Xls);
        let g = grid(&p, 0);
        assert_eq!(cell(g, 1, 0), Some(("Hello".into(), CellKind::Text)));
        assert_eq!(cell(g, 1, 1), Some(("Привет".into(), CellKind::Text)));
        assert_eq!(cell(g, 2, 0), Some(("25.60%".into(), CellKind::Number)));
        assert_eq!(cell(g, 2, 1).map(|c| c.1), Some(CellKind::Date));
        assert_eq!(cell(g, 2, 2), Some(("12.3 kg".into(), CellKind::Number)));
        assert_eq!(cell(g, 2, 3), Some(("1234.5".into(), CellKind::Number)));
        // MULRK: 100 as an integer and 123.45 scaled.
        assert_eq!(cell(g, 3, 0), Some(("100".into(), CellKind::Number)));
        assert_eq!(cell(g, 3, 1), Some(("123.45".into(), CellKind::Number)));
        assert_eq!(cell(g, 4, 0), Some(("TRUE".into(), CellKind::Bool)));
        assert_eq!(cell(g, 4, 1), Some(("#DIV/0!".into(), CellKind::Error)));
    }

    #[test]
    fn rk_numbers() {
        assert!((rk_value((100 << 2) | 2) - 100.0).abs() < 1e-9);
        assert!((rk_value((12_345 << 2) | 3) - 123.45).abs() < 1e-9);
        assert!((rk_value(((-5i32 << 2) | 2).cast_unsigned()) + 5.0).abs() < 1e-9);
        // An IEEE value keeps only the top 30 bits of the mantissa.
        let bits = (1.5f64.to_bits() >> 32) as u32 & 0xFFFF_FFFC;
        assert!((rk_value(bits) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn column_names() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }

    #[test]
    fn shared_strings_continue_across_records_and_switch_encoding() {
        // "abcdef" stored 8-bit, split after "abc"; the rest arrives as UTF-16 with a new flag.
        let mut first = 2u32.to_le_bytes().to_vec();
        first.extend(2u32.to_le_bytes());
        first.extend(6u16.to_le_bytes());
        first.push(0);
        first.extend(b"abc");
        let mut cont = vec![1u8];
        for u in "déf".encode_utf16() {
            cont.extend(u.to_le_bytes());
        }
        // A second, rich string follows in the continuation: 2 formatting runs and phonetic data.
        cont.extend(xl("", false).iter().take(0));
        let mut second = 3u16.to_le_bytes().to_vec();
        second.push(0x08 | 0x04);
        second.extend(2u16.to_le_bytes());
        second.extend(4u32.to_le_bytes());
        second.extend(b"xyz");
        second.extend([0u8; 8]);
        second.extend([9u8; 4]);
        cont.extend(second);
        let wb = workbook(
            &[rec(SST, &first), rec(CONTINUE, &cont)],
            &[sheet("S", vec![labelsst(0, 0, 0), labelsst(0, 1, 1)])],
        );
        let p = ok(&file(wb));
        let g = grid(&p, 0);
        assert_eq!(cell(g, 1, 0).map(|c| c.0), Some("abcdéf".into()));
        assert_eq!(cell(g, 1, 1).map(|c| c.0), Some("xyz".into()));
    }

    #[test]
    fn hidden_sheets_and_rows_are_shown_and_labelled() {
        let row = |r: u16, hidden: bool| {
            let mut d = vec![0u8; 16];
            d[0..2].copy_from_slice(&r.to_le_bytes());
            if hidden {
                d[12] = 0x20;
            }
            rec(ROW, &d)
        };
        let mut s2 = sheet("Veryhidden", vec![number(0, 0, 0, 7.0)]);
        s2.state = 2;
        let mut s1 = sheet("Hid", vec![number(0, 0, 0, 1.0)]);
        s1.state = 1;
        let wb = workbook(
            &[xf(0)],
            &[
                sheet(
                    "Vis",
                    vec![row(0, false), row(1, true), number(0, 0, 0, 1.0), number(1, 0, 0, 2.0)],
                ),
                s1,
                s2,
            ],
        );
        let p = ok(&file(wb));
        let secs: Vec<(&str, bool)> =
            p.sections.iter().map(|s| (s.name.as_str(), s.hidden)).collect();
        assert_eq!(secs, vec![("Vis", false), ("Hid", true), ("Veryhidden", true)]);
        let g = grid(&p, 0);
        assert!(!g.rows[0].hidden && g.rows[1].hidden);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.count == 2));
        let policy =
            crate::PreviewPolicy { reveal_hidden: false, ..crate::PreviewPolicy::default() };
        let p = crate::preview(
            &file(workbook(
                &[xf(0)],
                &[sheet("Vis", vec![row(1, true), number(1, 0, 0, 2.0), number(0, 0, 0, 5.0)])],
            )),
            &policy,
        )
        .expect("preview");
        assert_eq!(grid(&p, 0).rows.len(), 1);
    }

    #[test]
    fn formulas_show_their_cached_results() {
        let formula = |row: u16, col: u16, result: [u8; 8]| {
            let mut d = [row.to_le_bytes(), col.to_le_bytes(), 0u16.to_le_bytes()].concat();
            d.extend(result);
            d.extend([0u8; 6]);
            d.extend(0u16.to_le_bytes());
            rec(FORMULA, &d)
        };
        let text_result = [0, 0, 0, 0, 0, 0, 0xFF, 0xFF];
        let bool_result = [1, 0, 1, 0, 0, 0, 0xFF, 0xFF];
        let err_result = [2, 0, 0x2A, 0, 0, 0, 0xFF, 0xFF];
        let wb = workbook(
            &[xf(0)],
            &[sheet(
                "F",
                vec![
                    formula(0, 0, 2.5f64.to_le_bytes()),
                    formula(0, 1, text_result),
                    rec(STRING, &xl("computed text", false)),
                    formula(0, 2, bool_result),
                    formula(0, 3, err_result),
                    // A STRING record that does not follow a formula is ignored.
                    rec(STRING, &xl("stray", false)),
                ],
            )],
        );
        let p = ok(&file(wb));
        let g = grid(&p, 0);
        assert_eq!(cell(g, 1, 0), Some(("2.5".into(), CellKind::Number)));
        assert_eq!(cell(g, 1, 1), Some(("computed text".into(), CellKind::Text)));
        assert_eq!(cell(g, 1, 2), Some(("TRUE".into(), CellKind::Bool)));
        assert_eq!(cell(g, 1, 3), Some(("#N/A".into(), CellKind::Error)));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::FormulasNotEvaluated));
    }

    #[test]
    fn a_string_result_after_a_shared_formula_record_is_not_lost() {
        let formula = |row: u16, col: u16| {
            let mut d = [row.to_le_bytes(), col.to_le_bytes(), 0u16.to_le_bytes()].concat();
            d.extend([0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
            d.extend([0u8; 6]);
            d.extend(0u16.to_le_bytes());
            rec(FORMULA, &d)
        };
        let wb = workbook(
            &[xf(0)],
            &[sheet(
                "F",
                vec![
                    formula(0, 0),
                    rec(SHRFMLA, &[0, 0, 1, 0, 0, 0, 0, 0, 0, 0]),
                    rec(STRING, &xl("after shrfmla", false)),
                    formula(1, 0),
                    rec(ARRAY, &[0u8; 14]),
                    rec(STRING, &xl("after array", false)),
                    formula(2, 0),
                    rec(TABLE, &[0u8; 16]),
                    rec(STRING, &xl("after table", false)),
                ],
            )],
        );
        let p = ok(&file(wb));
        let g = grid(&p, 0);
        assert_eq!(cell(g, 1, 0), Some(("after shrfmla".into(), CellKind::Text)));
        assert_eq!(cell(g, 2, 0), Some(("after array".into(), CellKind::Text)));
        assert_eq!(cell(g, 3, 0), Some(("after table".into(), CellKind::Text)));
    }

    #[test]
    fn hidden_columns_are_counted_and_left_out_when_not_asked_for() {
        let colinfo = |first: u16, last: u16, flags: u16| {
            let mut d = Vec::new();
            d.extend(first.to_le_bytes());
            d.extend(last.to_le_bytes());
            d.extend(2_000u16.to_le_bytes());
            d.extend(0u16.to_le_bytes());
            d.extend(flags.to_le_bytes());
            d.extend(0u16.to_le_bytes());
            rec(COLINFO, &d)
        };
        let data = file(workbook(
            &[xf(0)],
            &[sheet(
                "C",
                vec![
                    colinfo(1, 1, 1),
                    colinfo(3, 3, 0),
                    number(0, 0, 0, 10.0),
                    number(0, 1, 0, 20.0),
                    number(0, 2, 0, 30.0),
                    number(0, 3, 0, 40.0),
                ],
            )],
        ));
        let cols = |p: &Preview| -> Vec<u32> {
            g(p).rows.iter().flat_map(|r| r.cells.iter().map(|c| c.col)).collect()
        };
        fn g(p: &Preview) -> &Grid {
            grid(p, 0)
        }
        let p = ok(&data);
        assert_eq!(cols(&p), [0, 1, 2, 3]);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown
                && n.detail == "columns"
                && n.count == 1)
        );
        let policy =
            crate::PreviewPolicy { reveal_hidden: false, ..crate::PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert_eq!(cols(&p), [0, 2, 3]);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted
                && n.detail == "columns"
                && n.count == 1)
        );
    }

    #[test]
    fn a_workbook_whose_cells_outgrow_the_text_budget_is_cut_and_says_so() {
        let strings: Vec<Vec<u8>> = (0..400u16)
            .map(|i| {
                let mut d = Vec::new();
                d.extend(i.to_le_bytes());
                d.extend(0u16.to_le_bytes());
                d.extend(xl(&"x".repeat(100), false));
                rec(LABEL, &d)
            })
            .collect();
        let data = file(workbook(&[xf(0)], &[sheet("T", strings)]));
        let policy = crate::PreviewPolicy { max_chars: 2_000, ..crate::PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert!(p.stats.truncated);
        assert!(p.stats.chars <= 2_000);
    }

    #[test]
    fn excel_4_macro_sheets_are_flagged() {
        let mut m = sheet("Macro1", vec![number(0, 0, 0, 1.0)]);
        m.kind = 1;
        m.state = 2;
        let p = ok(&file(workbook(&[xf(0)], &[m])));
        let sec = &p.sections[0];
        assert!(sec.hidden && sec.name.contains("Excel 4.0 macro sheet"), "{}", sec.name);
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Unsupported && n.detail.contains("macro"))
        );
    }

    #[test]
    fn protected_workbooks_are_reported() {
        let r = run(&file(workbook(&[rec(FILEPASS, &[1, 0, 1, 0, 1, 0])], &[sheet("S", vec![])])));
        assert_eq!(r, Err(PreviewError::Encrypted));
    }

    #[test]
    fn notes_and_text_boxes() {
        let obj = |id: u16| {
            let mut d = Vec::new();
            d.extend(0x0015u16.to_le_bytes());
            d.extend(0x0012u16.to_le_bytes());
            d.extend(0x0019u16.to_le_bytes());
            d.extend(id.to_le_bytes());
            d.extend([0u8; 14]);
            rec(OBJ, &d)
        };
        let txo = |text: &str| {
            let mut d = vec![0u8; 18];
            d[10..12].copy_from_slice(
                &u16::try_from(text.encode_utf16().count()).expect("n").to_le_bytes(),
            );
            let mut body = vec![1u8];
            for u in text.encode_utf16() {
                body.extend(u.to_le_bytes());
            }
            [rec(TXO, &d), rec(CONTINUE, &body), rec(CONTINUE, &[0u8; 16])].concat()
        };
        let note = |row: u16, col: u16, id: u16, author: &str| {
            let mut d =
                [row.to_le_bytes(), col.to_le_bytes(), 0u16.to_le_bytes(), id.to_le_bytes()]
                    .concat();
            d.extend(xl(author, false));
            rec(NOTE, &d)
        };
        let wb = workbook(
            &[xf(0)],
            &[sheet(
                "Notes",
                vec![
                    number(0, 0, 0, 1.0),
                    obj(7),
                    txo("Note body"),
                    note(0, 0, 7, "Mallory"),
                    obj(9),
                    txo("FLOATING BOX"),
                ],
            )],
        );
        let p = ok(&file(wb));
        let c = section_of(&p, SectionKind::Comments).expect("comments");
        let para = paragraphs(c)[0].clone();
        assert_eq!(para.marker, "Notes!A1");
        assert_eq!(para.text(), "Mallory: Note body");
        let b = p.sections.iter().find(|s| s.name == "Text boxes").expect("text boxes");
        assert_eq!(text_of(b), "FLOATING BOX");
    }

    #[test]
    fn excel_5_workbooks_use_the_code_page() {
        // BIFF5: LABEL with an 8-bit string in windows-1251, sheet name 8-bit.
        let mut g = bof(5, 0x0500);
        g.extend(rec(CODEPAGE, &1251u16.to_le_bytes()));
        let sheet_at = g.len() + 4 + 12 + 4;
        let mut bs = u32::try_from(sheet_at).expect("off").to_le_bytes().to_vec();
        bs.extend([0u8, 0, 5]);
        bs.extend(b"Sheet");
        g.extend(rec(BOUNDSHEET, &bs));
        g.extend(eof());
        assert_eq!(g.len(), sheet_at);
        let mut label = [0u16.to_le_bytes(), 0u16.to_le_bytes(), 0u16.to_le_bytes()].concat();
        let text = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]; // "Привет"
        label.extend(u16::try_from(text.len()).expect("n").to_le_bytes());
        label.extend(text);
        g.extend(bof(0x10, 0x0500));
        g.extend(rec(LABEL, &label));
        g.extend(number(0, 1, 0, 3.0));
        g.extend(eof());
        let p = ok(&file(g));
        let gr = grid(&p, 0);
        assert_eq!(cell(gr, 1, 0).map(|c| c.0), Some("Привет".into()));
        assert_eq!(cell(gr, 1, 1).map(|c| c.0), Some("3".into()));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort));
    }

    /// A `HLINK` record for the cells `(row, col)` with the given moniker bytes and location.
    fn hlink(row: u16, col: u16, flags: u32, tail: &[u8]) -> Vec<u8> {
        let mut d =
            [row.to_le_bytes(), row.to_le_bytes(), col.to_le_bytes(), col.to_le_bytes()].concat();
        d.extend([
            0xD0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B,
            0xA9, 0x0B,
        ]);
        d.extend(2u32.to_le_bytes());
        d.extend(flags.to_le_bytes());
        d.extend(tail);
        rec(HLINK, &d)
    }

    /// A `HyperlinkString`.
    fn hstr(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
        let mut o = u32::try_from(units.len()).expect("len").to_le_bytes().to_vec();
        for u in units {
            o.extend(u.to_le_bytes());
        }
        o
    }

    fn url_moniker(url: &str) -> Vec<u8> {
        let mut o = URL_MONIKER.to_vec();
        let units: Vec<u16> = url.encode_utf16().chain([0]).collect();
        // The length covers the address and, as Excel writes them, 24 bytes of serial data.
        o.extend(u32::try_from(units.len() * 2 + 24).expect("len").to_le_bytes());
        for u in units {
            o.extend(u.to_le_bytes());
        }
        o.extend([0u8; 24]);
        o
    }

    #[test]
    fn hyperlinks_of_cells_are_attached_and_never_followed() {
        let sst = sst(&["site", "file", "jump", "unc"]);
        let mut file_moniker = FILE_MONIKER.to_vec();
        file_moniker.extend(0u16.to_le_bytes());
        let ansi = b"C:\\docs\\a.xls\0";
        file_moniker.extend(u32::try_from(ansi.len()).expect("len").to_le_bytes());
        file_moniker.extend(ansi);
        file_moniker.extend([0u8; 24]);
        let wide: Vec<u8> =
            "C:\\docs\\\u{44F}.xls".encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        file_moniker.extend(u32::try_from(wide.len() + 6).expect("len").to_le_bytes());
        file_moniker.extend(u32::try_from(wide.len()).expect("len").to_le_bytes());
        file_moniker.extend(3u16.to_le_bytes());
        file_moniker.extend(&wide);
        let mut unc = FILE_MONIKER.to_vec();
        unc.extend(0u16.to_le_bytes());
        let ansi = b"\\\\host\\share\0";
        unc.extend(u32::try_from(ansi.len()).expect("len").to_le_bytes());
        unc.extend(ansi);
        unc.extend([0u8; 24]);
        unc.extend(0u32.to_le_bytes());
        let book = workbook(
            &[sst],
            &[sheet(
                "S",
                vec![
                    labelsst(0, 0, 0),
                    labelsst(1, 0, 1),
                    labelsst(2, 0, 2),
                    labelsst(3, 0, 3),
                    // A web address with a display name and a location.
                    hlink(
                        0,
                        0,
                        0x01 | 0x10 | 0x08,
                        &[
                            hstr("shown").as_slice(),
                            &url_moniker("https://example.org/a"),
                            &hstr("top"),
                        ]
                        .concat(),
                    ),
                    // A file, with the wide path preferred over the ANSI one.
                    hlink(1, 0, 0x01, &file_moniker),
                    // Only a place inside the workbook: not a link out.
                    hlink(2, 0, 0x08, &hstr("Sheet2!A1")),
                    // A UNC path from the ANSI part.
                    hlink(3, 0, 0x01, &unc),
                    // A string moniker, and records that are cut short or name a moniker we do not know.
                    hlink(9, 9, 0x01 | 0x100, &hstr("https://str.example/")),
                    hlink(0, 0, 0x01, &URL_MONIKER[..10]),
                    hlink(0, 0, 0x01, &[0xAA; 16]),
                    rec(HLINK, &[1, 2, 3]),
                ],
            )],
        );
        let p = ok(&file(book));
        let g = grid(&p, 0);
        let href = |row: u32| {
            g.rows
                .iter()
                .find(|r| r.index == row)
                .and_then(|r| r.cells.first())
                .map(|c| c.href.as_str())
        };
        assert_eq!(href(1), Some("https://example.org/a#top"));
        assert_eq!(href(2), Some("C:\\docs\\\u{44F}.xls"));
        assert_eq!(href(3), Some(""), "a jump inside the workbook is no link out");
        assert_eq!(href(4), Some("\\\\host\\share"));
    }

    #[test]
    fn repeated_cells_keep_the_last_record() {
        let wb = workbook(
            &[xf(0)],
            &[sheet("S", vec![number(0, 0, 0, 1.0), number(0, 0, 0, 2.0), number(0, 1, 0, 9.0)])],
        );
        let p = ok(&file(wb));
        assert_eq!(cell(grid(&p, 0), 1, 0).map(|c| c.0), Some("2".into()));
    }

    #[test]
    fn broken_streams_fail_cleanly() {
        assert!(matches!(run(&file(vec![0u8; 10])), Err(PreviewError::Malformed(_))));
        assert!(matches!(run(&file(rec(0x1234, &[]))), Err(PreviewError::Malformed(_))));
        // Truncated in the middle of a record.
        let wb = workbook(&[xf(0)], &[sheet("S", vec![number(0, 0, 0, 1.0)])]);
        for cut in (0..wb.len()).step_by(7) {
            let _ = run(&file(wb[..cut].to_vec()));
        }
    }

    #[test]
    fn mutated_workbooks_never_panic() {
        let wb = workbook(
            &[xf(0), xf(14), sst(&["one", "two", "три"])],
            &[
                sheet("A", vec![labelsst(0, 0, 0), number(0, 1, 1, 40_000.0), labelsst(1, 0, 2)]),
                sheet("B", vec![number(3, 3, 0, 1.0)]),
            ],
        );
        let mut rng = Lcg::new(77);
        for i in 0..fuzz_iterations(600) {
            let mut m = wb.clone();
            rng.mutate(&mut m, 1 + (i % 5));
            let _ = run(&file(m));
        }
    }

    #[test]
    fn real_workbooks_from_the_corpus() {
        let Ok(dir) = std::env::var("ARCA_CORPUS_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("sheet.xls")) else { return };
        let p = ok(&data);
        assert_eq!(p.sections.iter().filter(|s| s.kind == SectionKind::Sheet).count(), 3);
        assert_eq!(cell(grid(&p, 0), 2, 0).map(|c| c.0), Some("Яблоки".into()));
        assert!(p.sections.iter().any(|s| s.hidden));
        // Mutations of the real file.
        let cf = CompoundFile::parse(&data, crate::PreviewPolicy::default().cfb).expect("cfb");
        let stream = cf.read_stream(cf.find("Workbook").expect("wb")).expect("read");
        let mut rng = Lcg::new(5);
        for _ in 0..fuzz_iterations(300) {
            let mut m = stream.clone();
            rng.mutate(&mut m, 4);
            let _ = run(&file(m));
        }
    }
}
