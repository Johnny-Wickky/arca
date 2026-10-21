//! Legacy Word documents (`.doc`, `.dot`): the binary format of Word 97–2003, and Word 6/95
//! as plain text.
//!
//! Only *text and structure* are read — nothing is executed, rendered or decoded:
//!
//! * the **piece table** (`Clx`) maps character positions (CPs) to the bytes of the
//!   `WordDocument` stream, 8-bit or UTF-16; every CP of the file is therefore reachable,
//!   including text the author hid, deleted under revision marking, or placed in text boxes,
//!   headers, footnotes and comments;
//! * **character properties** (`PlcfBteChpx`) give the style bits and, above all, mark
//!   *hidden* text, *tracked deletions* and *insertions* so the viewer can label them;
//! * **paragraph properties** (`PlcfBtePapx`) give tables, lists, alignment and headings;
//! * macros (VBA), OLE objects, pictures and drawing shapes are never interpreted; pictures and
//!   embedded objects become placeholders.
//!
//! Password-protected files (`fEncrypted`) are reported, not opened. All offsets and counts come
//! from the (untrusted) file and are bounds-checked; sizes are capped so a hostile file cannot
//! make the extractor allocate or loop without limit.

use std::collections::BTreeMap;

use arca_container::bytes::{slice, u16_le, u32_le};
use arca_container::cfb::CompoundFile;

use crate::PreviewError;
use crate::listfmt;
use crate::model::{
    Align, Block, Cell, ImageInfo, NoticeKind, ParaStyle, Paragraph, Row, SectionKind, Table, style,
};
use crate::sink::{ParaBuilder, SectionId, Sink};

/// Characters (UTF-16 units) of text kept in memory.
const MAX_CP: usize = 16 << 20;
/// Pieces of the piece table.
const MAX_PIECES: usize = 1 << 20;
/// Property runs of each kind.
const MAX_RUNS: usize = 4 << 20;
/// Formatted disk pages inspected per kind.
const MAX_PAGES: usize = 1 << 19;
/// Styles read from the style sheet.
const MAX_STYLES: usize = 16_384;
/// Nesting of fields tracked.
const MAX_FIELDS: usize = 64;
/// Characters of a field instruction kept.
const MAX_INSTR: usize = 2_048;
/// Size of a formatted disk page.
const PAGE: usize = 512;

fn malformed(why: &str) -> PreviewError {
    PreviewError::Malformed(format!("Word document: {why}"))
}

fn usz(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

fn narrow(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------------------------
// File information block
// ---------------------------------------------------------------------------------------------

/// Indices of the `(fc, lcb)` pairs of `FibRgFcLcb97` that are used.
mod pair {
    pub(super) const STSHF: usize = 1;
    pub(super) const PLCF_FND_REF: usize = 2;
    pub(super) const PLCF_AND_REF: usize = 4;
    pub(super) const PLCF_AND_TXT: usize = 5;
    pub(super) const PLCF_HDD: usize = 11;
    pub(super) const PLCF_BTE_CHPX: usize = 12;
    pub(super) const PLCF_BTE_PAPX: usize = 13;
    pub(super) const CLX: usize = 33;
    pub(super) const GRP_XST_ATN_OWNERS: usize = 36;
    pub(super) const PLCF_END_REF: usize = 46;
    pub(super) const PLF_LST: usize = 73;
    pub(super) const PLF_LFO: usize = 74;
}

/// Story order inside the character space of a document.
#[derive(Clone, Copy)]
enum Story {
    Main = 0,
    Footnotes = 1,
    Headers = 2,
    Macros = 3,
    Comments = 4,
    Endnotes = 5,
    TextBoxes = 6,
    HeaderTextBoxes = 7,
}

#[derive(Debug)]
struct Fib {
    /// Word 97 and later (separate table stream, Unicode-capable piece table).
    modern: bool,
    flags: u16,
    /// Lengths of the stories: text, footnotes, headers, macros, comments, endnotes, text
    /// boxes, header text boxes.
    ccp: [u32; 8],
    pairs: Vec<(u32, u32)>,
    fc_min: u32,
    fc_mac: u32,
    /// Word 6/95 character set: `256` is the Macintosh set, anything else Windows ANSI.
    chse: u16,
}

impl Fib {
    fn parse(wd: &[u8]) -> Result<Self, PreviewError> {
        let ident = u16_le(wd, 0).ok_or_else(|| malformed("the header is truncated"))?;
        let flags = u16_le(wd, 0x0A).unwrap_or(0);
        match ident {
            0xA5EC => Self::parse97(wd, flags),
            0xA5DC => Ok(Self::parse6(wd, flags)),
            _ => Err(malformed("the stream is not a Word file information block")),
        }
    }

    fn parse97(wd: &[u8], flags: u16) -> Result<Self, PreviewError> {
        let csw =
            usize::from(u16_le(wd, 0x20).ok_or_else(|| malformed("the header is truncated"))?);
        let lw_at = 0x22usize.saturating_add(csw.saturating_mul(2));
        let cslw =
            usize::from(u16_le(wd, lw_at).ok_or_else(|| malformed("the header is truncated"))?);
        let lw = lw_at.saturating_add(2);
        let mut ccp = [0u32; 8];
        for (i, slot) in ccp.iter_mut().enumerate() {
            // FibRgLw97: ccpText is the fourth value.
            let idx = i.saturating_add(3);
            if idx < cslw {
                *slot = u32_le(wd, lw.saturating_add(idx.saturating_mul(4))).unwrap_or(0);
            }
        }
        let fc_at = lw.saturating_add(cslw.saturating_mul(4));
        let count = usize::from(u16_le(wd, fc_at).unwrap_or(0)).min(256);
        let mut pairs = Vec::with_capacity(count);
        for i in 0..count {
            let at = fc_at.saturating_add(2).saturating_add(i.saturating_mul(8));
            let (Some(fc), Some(lcb)) = (u32_le(wd, at), u32_le(wd, at.saturating_add(4))) else {
                break;
            };
            pairs.push((fc, lcb));
        }
        Ok(Self {
            modern: true,
            flags,
            ccp,
            pairs,
            fc_min: u32_le(wd, 0x18).unwrap_or(0),
            fc_mac: u32_le(wd, 0x1C).unwrap_or(0),
            chse: 0,
        })
    }

    fn parse6(wd: &[u8], flags: u16) -> Self {
        let mut ccp = [0u32; 8];
        for (i, slot) in ccp.iter_mut().enumerate() {
            *slot = u32_le(wd, 0x34usize.saturating_add(i.saturating_mul(4))).unwrap_or(0);
        }
        let mut pairs = Vec::new();
        for i in 0..64usize {
            let at = 0x5Cusize.saturating_add(i.saturating_mul(8));
            let (Some(fc), Some(lcb)) = (u32_le(wd, at), u32_le(wd, at.saturating_add(4))) else {
                break;
            };
            pairs.push((fc, lcb));
        }
        Self {
            modern: false,
            flags,
            ccp,
            pairs,
            fc_min: u32_le(wd, 0x18).unwrap_or(0),
            fc_mac: u32_le(wd, 0x1C).unwrap_or(0),
            chse: u16_le(wd, 0x14).unwrap_or(0),
        }
    }

    fn encrypted(&self) -> bool {
        self.flags & 0x0100 != 0
    }

    fn uses_table_1(&self) -> bool {
        self.flags & 0x0200 != 0
    }

    fn complex(&self) -> bool {
        self.flags & 0x0004 != 0
    }

    fn ext_char(&self) -> bool {
        self.flags & 0x1000 != 0
    }

    /// `(offset, length)` of structure `index` in the table stream, if it is present.
    fn pair(&self, index: usize) -> Option<(usize, usize)> {
        let &(fc, lcb) = self.pairs.get(index)?;
        if lcb == 0 { None } else { Some((usz(fc), usz(lcb))) }
    }

    fn ccp(&self, story: Story) -> usize {
        self.ccp.get(story as usize).map_or(0, |v| usz(*v))
    }

    /// First CP of a story.
    fn story_start(&self, story: Story) -> usize {
        let mut at = 0usize;
        for s in 0..(story as usize) {
            at = at.saturating_add(self.ccp.get(s).map_or(0, |v| usz(*v)));
        }
        at
    }
}

// ---------------------------------------------------------------------------------------------
// Piece table and text
// ---------------------------------------------------------------------------------------------

/// A run of characters stored contiguously in the `WordDocument` stream.
#[derive(Debug, Clone, Copy)]
struct Piece {
    cp0: u32,
    cp1: u32,
    /// Byte offset in the `WordDocument` stream.
    fc: usize,
    /// UTF-16 (two bytes per character) rather than 8-bit.
    wide: bool,
}

/// Reads the `Clx` structure: skips the property modifiers, then reads the piece table.
fn parse_clx(table: &[u8], fc: usize, lcb: usize, modern: bool) -> Option<Vec<Piece>> {
    let clx = slice(table, fc, lcb)?;
    let mut pos = 0usize;
    loop {
        match *clx.get(pos)? {
            1 => {
                let cb = usize::from(u16_le(clx, pos.checked_add(1)?)?);
                pos = pos.checked_add(3)?.checked_add(cb)?;
            }
            2 => break,
            _ => return None,
        }
    }
    let plc_len = usz(u32_le(clx, pos.checked_add(1)?)?);
    let plc = slice(clx, pos.checked_add(5)?, plc_len)?;
    // n + 1 character positions, then n piece descriptors of 8 bytes.
    let n = plc.len().checked_sub(4)? / 12;
    if n == 0 || n > MAX_PIECES {
        return None;
    }
    let data = n.checked_add(1)?.checked_mul(4)?;
    let mut pieces = Vec::with_capacity(n);
    let mut prev = 0u32;
    for i in 0..n {
        let cp0 = u32_le(plc, i.checked_mul(4)?)?;
        let cp1 = u32_le(plc, i.checked_add(1)?.checked_mul(4)?)?;
        let at = data.checked_add(i.checked_mul(8)?)?;
        let raw = u32_le(plc, at.checked_add(2)?)?;
        if i == 0 && cp0 != 0 {
            return None;
        }
        if cp0 < prev || cp1 < cp0 {
            return None;
        }
        prev = cp1;
        if cp1 == cp0 {
            continue;
        }
        let (fc, wide) = if modern {
            if raw & 0x4000_0000 != 0 {
                (usz((raw & 0x3FFF_FFFF) / 2), false)
            } else {
                (usz(raw & 0x3FFF_FFFF), true)
            }
        } else {
            (usz(raw), false)
        };
        pieces.push(Piece { cp0, cp1, fc, wide });
    }
    if pieces.is_empty() { None } else { Some(pieces) }
}

/// Windows-1252 for `0x80..=0x9F`.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

fn ansi_unit(b: u8) -> u16 {
    match b {
        0x80..=0x9F => CP1252_HIGH.get(usize::from(b) - 0x80).copied().unwrap_or(u16::from(b)),
        _ => u16::from(b),
    }
}

/// Decodes the text of all pieces into UTF-16 units indexed by CP.
///
/// Characters that lie outside the stream stay `U+0000`, so later CPs keep their positions.
/// `mac` selects Macintosh Roman for 8-bit text of old files.
fn load_text(wd: &[u8], pieces: &[Piece], mac: bool) -> (Vec<u16>, bool) {
    let mut text: Vec<u16> = Vec::new();
    let mut damaged = false;
    for p in pieces {
        let cp0 = usz(p.cp0);
        if cp0 >= MAX_CP {
            break;
        }
        let cp1 = usz(p.cp1).min(MAX_CP);
        if text.len() < cp0 {
            text.resize(cp0, 0);
            damaged = true;
        }
        if text.len() > cp0 {
            continue;
        }
        let n = cp1 - cp0;
        let unit = if p.wide { 2 } else { 1 };
        let want = n.saturating_mul(unit);
        let avail = wd.get(p.fc..).unwrap_or(&[]);
        let have = avail.get(..want.min(avail.len())).unwrap_or(&[]);
        if have.len() < want {
            damaged = true;
        }
        if p.wide {
            text.extend(have.chunks_exact(2).map(|c| {
                u16::from_le_bytes([
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                ])
            }));
        } else if mac {
            text.extend(have.iter().map(|&b| mac_unit(b)));
        } else {
            text.extend(have.iter().map(|&b| ansi_unit(b)));
        }
        text.resize(cp0.saturating_add(n), 0);
    }
    (text, damaged)
}

/// Macintosh Roman for the bytes that differ from ASCII, as far as they are letters and common
/// punctuation; other bytes become `U+FFFD`.
fn mac_unit(b: u8) -> u16 {
    const HIGH: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";
    if b < 0x80 {
        return u16::from(b);
    }
    HIGH.chars()
        .nth(usize::from(b) - 0x80)
        .and_then(|c| u16::try_from(u32::from(c)).ok())
        .unwrap_or(0xFFFD)
}

/// File offsets → character positions.
struct FcMap {
    /// `(fc0, fc1, cp0, wide)` sorted by `fc0`.
    v: Vec<(usize, usize, u32, bool)>,
}

impl FcMap {
    fn new(pieces: &[Piece]) -> Self {
        let mut v: Vec<_> = pieces
            .iter()
            .map(|p| {
                let n = usz(p.cp1.saturating_sub(p.cp0));
                let w = if p.wide { 2 } else { 1 };
                (p.fc, p.fc.saturating_add(n.saturating_mul(w)), p.cp0, p.wide)
            })
            .collect();
        v.sort_by_key(|e| e.0);
        Self { v }
    }

    /// Calls `f(cp0, cp1)` for each character range backed by file bytes `fc0..fc1`.
    fn each(&self, fc0: usize, fc1: usize, mut f: impl FnMut(u32, u32)) {
        if fc0 >= fc1 {
            return;
        }
        let mut i = self.v.partition_point(|e| e.0 <= fc0).saturating_sub(1);
        // A few steps back cover overlapping pieces of damaged files; the walk is bounded.
        i = i.saturating_sub(3);
        let mut steps = 0usize;
        while let Some(&(p0, p1, cp0, wide)) = self.v.get(i) {
            if p0 >= fc1 || steps > 128 {
                break;
            }
            let a = fc0.max(p0);
            let b = fc1.min(p1);
            if a < b {
                let w = if wide { 2 } else { 1 };
                let c0 = cp0.saturating_add(narrow((a - p0) / w));
                let c1 = cp0.saturating_add(narrow((b - p0) / w));
                if c0 < c1 {
                    f(c0, c1);
                }
            }
            i += 1;
            steps += 1;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Property runs
// ---------------------------------------------------------------------------------------------

/// Non-overlapping, sorted runs of equal properties over the character space.
struct RunSet<T> {
    runs: Vec<(u32, u32, T)>,
    hint: std::cell::Cell<usize>,
}

impl<T: Copy + Default + PartialEq> RunSet<T> {
    fn empty() -> Self {
        Self { runs: Vec::new(), hint: std::cell::Cell::new(0) }
    }

    fn build(mut v: Vec<(u32, u32, T)>) -> Self {
        v.sort_by_key(|r| r.0);
        let mut out: Vec<(u32, u32, T)> = Vec::with_capacity(v.len());
        for (a, b, t) in v {
            let a = out.last().map_or(a, |l| a.max(l.1));
            if a >= b {
                continue;
            }
            if let Some(l) = out.last_mut()
                && l.1 == a
                && l.2 == t
            {
                l.1 = b;
                continue;
            }
            out.push((a, b, t));
        }
        Self { runs: out, hint: std::cell::Cell::new(0) }
    }

    /// Properties at `cp` and the range over which they stay the same.
    fn at(&self, cp: u32) -> (T, std::ops::Range<u32>) {
        let h = self.hint.get();
        for probe in [h, h.saturating_add(1)] {
            if let Some(r) = self.runs.get(probe)
                && r.0 <= cp
                && cp < r.1
            {
                self.hint.set(probe);
                return (r.2, r.0..r.1);
            }
        }
        let idx = self.runs.partition_point(|r| r.0 <= cp);
        if let Some(r) = idx.checked_sub(1).and_then(|i| self.runs.get(i))
            && cp < r.1
        {
            self.hint.set(idx - 1);
            return (r.2, r.0..r.1);
        }
        let from = idx.checked_sub(1).and_then(|i| self.runs.get(i)).map_or(0, |r| r.1);
        let to = self.runs.get(idx).map_or(u32::MAX, |r| r.0);
        (T::default(), from..to)
    }
}

/// An iterator over the `(opcode, operand)` pairs of a property list (`grpprl`).
///
/// The operand of variable-length modifiers includes their leading size byte(s).
struct Sprms<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Sprms<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for Sprms<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let op = u16_le(self.data, self.pos)?;
        let start = self.pos.checked_add(2)?;
        let len = match op >> 13 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => match op {
                // sprmTDefTable: a 16-bit size (minus one) that includes itself.
                0xD608 => usize::from(u16_le(self.data, start)?).checked_add(1)?,
                // sprmPChgTabs: a size byte, or 255 for "computed".
                0xC615 => {
                    let cb = *self.data.get(start)?;
                    if cb == 255 {
                        let del = usize::from(*self.data.get(start.checked_add(1)?)?);
                        let at = start.checked_add(2)?.checked_add(del.checked_mul(4)?)?;
                        let add = usize::from(*self.data.get(at)?);
                        2usize.checked_add(del * 4)?.checked_add(1)?.checked_add(add * 3)?
                    } else {
                        usize::from(cb) + 1
                    }
                }
                _ => usize::from(*self.data.get(start)?) + 1,
            },
        };
        let operand = slice(self.data, start, len)?;
        self.pos = start.checked_add(len)?;
        Some((op, operand))
    }
}

/// Character properties that matter for a text preview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Chp {
    /// [`style`] bits.
    bits: u16,
    /// `fSpec`: the character is a placeholder (picture, reference mark, symbol, …).
    spec: bool,
    /// `fOle2`: the placeholder stands for an embedded object.
    ole: bool,
    /// `sprmCSymbol` character, `0` if none.
    symbol: u16,
}

fn toggle(bits: &mut u16, mask: u16, v: u8) {
    match v {
        0 => *bits &= !mask,
        // "On", or "opposite of the style" (the styles are not consulted: assume off).
        1 | 0x81 => *bits |= mask,
        _ => {}
    }
}

fn parse_chp(prl: &[u8]) -> Chp {
    let mut c = Chp::default();
    for (op, arg) in Sprms::new(prl) {
        let b = arg.first().copied().unwrap_or(0);
        match op {
            0x0800 => toggle(&mut c.bits, style::DELETED, b),
            0x0801 => toggle(&mut c.bits, style::INSERTED, b),
            0x0835 => toggle(&mut c.bits, style::BOLD, b),
            0x0836 => toggle(&mut c.bits, style::ITALIC, b),
            0x0837 | 0x2A53 => toggle(&mut c.bits, style::STRIKE, b),
            0x083C => toggle(&mut c.bits, style::HIDDEN, b),
            0x2A3E => {
                if b == 0 {
                    c.bits &= !style::UNDERLINE;
                } else {
                    c.bits |= style::UNDERLINE;
                }
            }
            0x2A48 => {
                c.bits &= !(style::SUPER | style::SUB);
                match b {
                    1 => c.bits |= style::SUPER,
                    2 => c.bits |= style::SUB,
                    _ => {}
                }
            }
            0x0855 => c.spec = b & 1 == 1,
            0x080A => c.ole = b & 1 == 1,
            0x6A09 => c.symbol = arg.get(2..4).and_then(|s| u16_le(s, 0)).unwrap_or(0),
            _ => {}
        }
    }
    c
}

/// Paragraph properties that matter for a text preview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Pap {
    istd: u16,
    in_table: bool,
    /// End-of-row mark.
    ttp: bool,
    /// The paragraph mark ends a cell of a table nested in a cell (`sprmPFInnerTableCell`).
    inner_cell: bool,
    /// Nesting depth of tables (`0` outside).
    itap: u8,
    jc: u8,
    ilfo: u16,
    ilvl: u8,
    outline: Option<u8>,
}

impl TableAcc {
    /// Ends the row of a nested table: its cells become one paragraph of the outer cell.
    fn flush_inner(&mut self) {
        if !self.inner_paras.is_empty() {
            let cell = std::mem::take(&mut self.inner_paras);
            self.inner_cells.push(cell);
        }
        let cells = std::mem::take(&mut self.inner_cells);
        let mut para = Paragraph::default();
        for (i, cell) in cells.into_iter().enumerate() {
            if i > 0 {
                para.runs.push(crate::model::Run {
                    text: " │ ".to_owned(),
                    style: 0,
                    href: String::new(),
                });
            }
            for (k, p) in cell.into_iter().enumerate() {
                if k > 0 {
                    para.runs.push(crate::model::Run {
                        text: " ".to_owned(),
                        style: 0,
                        href: String::new(),
                    });
                }
                if !p.marker.is_empty() {
                    para.runs.push(crate::model::Run {
                        text: format!("{} ", p.marker),
                        style: 0,
                        href: String::new(),
                    });
                }
                para.runs.extend(p.runs);
            }
        }
        if !para.runs.is_empty() {
            self.count = self.count.saturating_add(para.runs.len());
            self.paras.push(para);
        }
    }
}

impl Pap {
    fn depth(&self) -> u8 {
        if self.itap > 0 { self.itap } else { u8::from(self.in_table) }
    }

    fn align(&self) -> Align {
        match self.jc {
            1 => Align::Center,
            2 => Align::End,
            3 | 4 => Align::Justify,
            _ => Align::Start,
        }
    }
}

fn parse_pap(istd: u16, prl: &[u8]) -> Pap {
    let mut p = Pap { istd, ..Pap::default() };
    for (op, arg) in Sprms::new(prl) {
        let b = arg.first().copied().unwrap_or(0);
        match op {
            0x4600 => p.istd = u16_le(arg, 0).unwrap_or(p.istd),
            0x2416 => p.in_table = b & 1 == 1,
            0x2417 | 0x244C => p.ttp = b & 1 == 1,
            0x244B => p.inner_cell = b & 1 == 1,
            0x6649 => p.itap = u8::try_from(u32_le(arg, 0).unwrap_or(0).min(255)).unwrap_or(0),
            0x2403 | 0x2461 => p.jc = b,
            0x460B => p.ilfo = u16_le(arg, 0).unwrap_or(0),
            0x260A => p.ilvl = b,
            0x2640 => p.outline = Some(b),
            _ => {}
        }
    }
    p
}

/// The pages named by a `PlcfBte` (an array of `n + 1` file positions and `n` page numbers).
fn fkp_pages(table: &[u8], fc: usize, lcb: usize) -> Vec<usize> {
    let Some(plc) = slice(table, fc, lcb) else { return Vec::new() };
    let n = plc.len().saturating_sub(4) / 8;
    let first = n.saturating_add(1).saturating_mul(4);
    (0..n.min(MAX_PAGES))
        .filter_map(|i| u32_le(plc, first.checked_add(i.checked_mul(4)?)?))
        .map(|pn| usz(pn & 0x3F_FFFF))
        .collect()
}

fn collect_chp(wd: &[u8], table: &[u8], fib: &Fib, map: &FcMap) -> RunSet<Chp> {
    let Some((fc, lcb)) = fib.pair(pair::PLCF_BTE_CHPX) else { return RunSet::empty() };
    let mut out: Vec<(u32, u32, Chp)> = Vec::new();
    for pn in fkp_pages(table, fc, lcb) {
        let Some(page) = pn.checked_mul(PAGE).and_then(|off| slice(wd, off, PAGE)) else {
            continue;
        };
        let crun = usize::from(page.get(PAGE - 1).copied().unwrap_or(0));
        let rgb = (crun + 1) * 4;
        if rgb + crun > PAGE - 1 {
            continue;
        }
        for i in 0..crun {
            let (Some(f0), Some(f1)) = (u32_le(page, i * 4), u32_le(page, (i + 1) * 4)) else {
                continue;
            };
            let b = usize::from(page.get(rgb + i).copied().unwrap_or(0));
            if b == 0 {
                continue;
            }
            let off = b * 2;
            let cb = usize::from(page.get(off).copied().unwrap_or(0));
            let Some(prl) = slice(page, off + 1, cb) else { continue };
            let chp = parse_chp(prl);
            if chp == Chp::default() {
                continue;
            }
            map.each(usz(f0), usz(f1), |c0, c1| out.push((c0, c1, chp)));
            if out.len() > MAX_RUNS {
                return RunSet::build(out);
            }
        }
    }
    RunSet::build(out)
}

fn collect_pap(wd: &[u8], table: &[u8], fib: &Fib, map: &FcMap) -> RunSet<Pap> {
    let Some((fc, lcb)) = fib.pair(pair::PLCF_BTE_PAPX) else { return RunSet::empty() };
    let mut out: Vec<(u32, u32, Pap)> = Vec::new();
    for pn in fkp_pages(table, fc, lcb) {
        let Some(page) = pn.checked_mul(PAGE).and_then(|off| slice(wd, off, PAGE)) else {
            continue;
        };
        let crun = usize::from(page.get(PAGE - 1).copied().unwrap_or(0));
        let rgbx = (crun + 1) * 4;
        if rgbx + crun * 13 > PAGE - 1 {
            continue;
        }
        for i in 0..crun {
            let (Some(f0), Some(f1)) = (u32_le(page, i * 4), u32_le(page, (i + 1) * 4)) else {
                continue;
            };
            let bx = usize::from(page.get(rgbx + i * 13).copied().unwrap_or(0)) * 2;
            let pap = if bx == 0 {
                Pap::default()
            } else {
                let cb = usize::from(page.get(bx).copied().unwrap_or(0));
                let content = if cb == 0 {
                    let cb2 = usize::from(page.get(bx + 1).copied().unwrap_or(0));
                    slice(page, bx + 2, cb2 * 2)
                } else {
                    slice(page, bx + 1, cb * 2 - 1)
                };
                match content {
                    Some(c) => parse_pap(u16_le(c, 0).unwrap_or(0), c.get(2..).unwrap_or(&[])),
                    None => Pap::default(),
                }
            };
            if pap == Pap::default() {
                continue;
            }
            map.each(usz(f0), usz(f1), |c0, c1| out.push((c0, c1, pap)));
            if out.len() > MAX_RUNS {
                return RunSet::build(out);
            }
        }
    }
    RunSet::build(out)
}

// ---------------------------------------------------------------------------------------------
// Style sheet and lists
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct StyleInfo {
    sti: u16,
    base: u16,
}

fn parse_styles(table: &[u8], fib: &Fib) -> Vec<StyleInfo> {
    let Some((fc, lcb)) = fib.pair(pair::STSHF) else { return Vec::new() };
    let Some(blob) = slice(table, fc, lcb) else { return Vec::new() };
    let cb_stshi = usize::from(u16_le(blob, 0).unwrap_or(0));
    let cstd = usize::from(u16_le(blob, 2).unwrap_or(0)).min(MAX_STYLES);
    let mut pos = cb_stshi.saturating_add(2);
    let mut out = Vec::with_capacity(cstd);
    for _ in 0..cstd {
        let Some(cb) = u16_le(blob, pos) else { break };
        let cb = usize::from(cb);
        pos = pos.saturating_add(2);
        if cb == 0 {
            out.push(StyleInfo { sti: 0x0FFF, base: 0x0FFF });
            continue;
        }
        let Some(std) = slice(blob, pos, cb) else { break };
        let w0 = u16_le(std, 0).unwrap_or(0);
        let w1 = u16_le(std, 2).unwrap_or(0);
        out.push(StyleInfo { sti: w0 & 0x0FFF, base: w1 >> 4 });
        pos = pos.saturating_add(cb);
    }
    out
}

#[derive(Debug, Clone)]
struct ListLevel {
    start: u32,
    nfc: u8,
    text: Vec<u16>,
}

/// A list in use (`LFO`): which definition, and what the paragraph's list overrides in it.
#[derive(Debug, Clone, Default)]
struct Instance {
    lsid: u32,
    /// Level restarted at another number (`LFOLVL.iStartAt`).
    starts: [Option<u32>; 9],
    /// Level defined again for this instance only.
    formats: Vec<(u8, ListLevel)>,
}

/// Numbered and bulleted lists: `PlfLst` (definitions) and `PlfLfo` (instances).
#[derive(Debug, Default)]
struct Lists {
    defs: BTreeMap<u32, Vec<ListLevel>>,
    /// The list instances; `ilfo` is a one-based index.
    instances: Vec<Instance>,
}

/// One `LVL`: 28 fixed bytes, the character and paragraph properties of the number, and the
/// number text. Returns it with the position after it.
fn parse_level(blob: &[u8], pos: usize) -> Option<(ListLevel, usize)> {
    let start = u32_le(blob, pos)?;
    let nfc = blob.get(pos.saturating_add(4)).copied().unwrap_or(0);
    let cb_chpx = usize::from(blob.get(pos.saturating_add(24)).copied().unwrap_or(0));
    let cb_papx = usize::from(blob.get(pos.saturating_add(25)).copied().unwrap_or(0));
    let mut at = pos.saturating_add(28).saturating_add(cb_chpx).saturating_add(cb_papx);
    let cch = usize::from(u16_le(blob, at)?).min(256);
    at = at.saturating_add(2);
    let text: Vec<u16> = (0..cch).filter_map(|k| u16_le(blob, at.saturating_add(k * 2))).collect();
    Some((
        ListLevel { start: u32::try_from(start.cast_signed()).unwrap_or(1), nfc, text },
        at.saturating_add(cch * 2),
    ))
}

impl Lists {
    fn parse(table: &[u8], fib: &Fib) -> Self {
        let mut me = Self::default();
        if let Some((fc, lcb)) = fib.pair(pair::PLF_LST)
            && let Some(heads) = slice(table, fc, lcb)
        {
            // The level records are stored right after the structure, not inside it.
            me.parse_defs(heads, table.get(fc.saturating_add(lcb)..).unwrap_or(&[]));
        }
        if let Some((fc, lcb)) = fib.pair(pair::PLF_LFO)
            && let Some(blob) = slice(table, fc, lcb)
        {
            me.parse_instances(blob);
        }
        me
    }

    /// `PlfLfo`: the `LFO` records, then for each one the overrides (`LFOData`) it announces.
    fn parse_instances(&mut self, blob: &[u8]) {
        let n = usz(u32_le(blob, 0).unwrap_or(0)).min(65_536);
        let mut counts: Vec<usize> = Vec::new();
        for i in 0..n {
            let Some(at) = i.checked_mul(16).map(|o| o.saturating_add(4)) else { break };
            let Some(lsid) = u32_le(blob, at) else { break };
            self.instances.push(Instance { lsid, ..Instance::default() });
            counts.push(usize::from(blob.get(at.saturating_add(12)).copied().unwrap_or(0)));
        }
        let mut pos = n.saturating_mul(16).saturating_add(4);
        'all: for (inst, count) in self.instances.iter_mut().zip(counts) {
            // `LFOData`: a position in the text, then the overridden levels.
            pos = pos.saturating_add(4);
            for _ in 0..count {
                let (Some(start), Some(flags)) =
                    (u32_le(blob, pos), blob.get(pos.saturating_add(4)))
                else {
                    break 'all;
                };
                pos = pos.saturating_add(8);
                let ilvl = usize::from(flags & 0x0F);
                if flags & 0x10 != 0
                    && let Some(slot) = inst.starts.get_mut(ilvl)
                {
                    *slot = Some(start);
                }
                if flags & 0x20 != 0 {
                    let Some((level, next)) = parse_level(blob, pos) else { break 'all };
                    pos = next;
                    if inst.formats.len() < 9 {
                        inst.formats.push((flags & 0x0F, level));
                    }
                }
            }
        }
    }

    fn parse_defs(&mut self, head: &[u8], blob: &[u8]) {
        let count = usize::from(u16_le(head, 0).unwrap_or(0)).min(4_096);
        // Definitions are 28 bytes each; the levels of all of them follow.
        let mut heads: Vec<(u32, usize)> = Vec::new();
        for i in 0..count {
            let at = 2usize.saturating_add(i.saturating_mul(28));
            let (Some(lsid), Some(flags)) = (u32_le(head, at), head.get(at.saturating_add(26)))
            else {
                return;
            };
            heads.push((lsid, if flags & 1 == 1 { 1 } else { 9 }));
        }
        let mut pos = 0usize;
        for (lsid, levels) in heads {
            let mut list = Vec::new();
            for _ in 0..levels {
                let Some((level, next)) = parse_level(blob, pos) else { return };
                pos = next;
                list.push(level);
            }
            self.defs.insert(lsid, list);
        }
    }

    /// The marker of a list paragraph and its nesting level (`1`-based).
    fn marker(
        &self,
        ilfo: u16,
        ilvl: u8,
        counters: &mut BTreeMap<u16, [u32; 9]>,
    ) -> Option<(String, u8)> {
        let inst = self.instances.get(usize::from(ilfo).checked_sub(1)?)?;
        let levels = self.defs.get(&inst.lsid)?;
        let li = usize::from(ilvl).min(8);
        // The level as this instance has it: its own definition if it redefines the level.
        let level_at = |k: usize| -> Option<&ListLevel> {
            inst.formats
                .iter()
                .find(|(l, _)| usize::from(*l) == k)
                .map(|(_, l)| l)
                .or_else(|| levels.get(k))
        };
        let level = level_at(li).or_else(|| levels.first())?;
        let level_no = u8::try_from(li + 1).unwrap_or(9);
        let c = counters.entry(ilfo).or_insert([0; 9]);
        let slot = c.get_mut(li)?;
        let first = inst.starts.get(li).copied().flatten().unwrap_or(level.start);
        *slot = if *slot == 0 { first } else { slot.saturating_add(1) };
        for deeper in c.iter_mut().skip(li + 1) {
            *deeper = 0;
        }
        match level.nfc {
            23 => return Some(("•".to_owned(), level_no)),
            255 => return Some((String::new(), level_no)),
            _ => {}
        }
        let number = |k: usize, c: &[u32; 9]| -> String {
            let n = c.get(k).copied().unwrap_or(0).max(1);
            let nfc = level_at(k).map_or(level.nfc, |l| l.nfc);
            number_text(nfc, n)
        };
        let mut s = String::new();
        if level.text.is_empty() {
            s = number(li, c);
            s.push('.');
        } else {
            for &u in &level.text {
                if u < 9 {
                    s.push_str(&number(usize::from(u), c));
                } else if let Some(ch) = char::from_u32(u32::from(u)) {
                    s.push(ch);
                }
            }
        }
        Some((s.trim_end().to_owned(), level_no))
    }
}

/// A counter in the numbering format `nfc`.
fn number_text(nfc: u8, n: u32) -> String {
    match nfc {
        1 => listfmt::roman(n),
        2 => listfmt::roman(n).to_lowercase(),
        3 => listfmt::letters(n, true),
        4 => listfmt::letters(n, false),
        5 => {
            let suffix = match (n % 10, n % 100) {
                (_, 11..=13) => "th",
                (1, _) => "st",
                (2, _) => "nd",
                (3, _) => "rd",
                _ => "th",
            };
            format!("{n}{suffix}")
        }
        22 => format!("{n:02}"),
        _ => n.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------------------------

struct Env {
    text: Vec<u16>,
    chp: RunSet<Chp>,
    pap: RunSet<Pap>,
    styles: Vec<StyleInfo>,
    lists: Lists,
    /// Positions of the footnote and endnote reference marks in the main text.
    fn_refs: Vec<u32>,
    en_refs: Vec<u32>,
    /// Positions of the comment reference marks.
    cm_refs: Vec<u32>,
}

impl Env {
    fn para_style(&self, pap: &Pap) -> ParaStyle {
        let mut cur = pap.istd;
        if self.styles.is_empty() {
            if (1..=9).contains(&cur) {
                return ParaStyle::heading(u8::try_from(cur).unwrap_or(1));
            }
        } else {
            for _ in 0..8 {
                let Some(s) = self.styles.get(usize::from(cur)) else { break };
                match s.sti {
                    1..=9 => return ParaStyle::heading(u8::try_from(s.sti).unwrap_or(1)),
                    62 => return ParaStyle::Title,
                    74 => return ParaStyle::Subtitle,
                    _ => {}
                }
                if s.base == 0x0FFF || s.base == cur {
                    break;
                }
                cur = s.base;
            }
        }
        match pap.outline {
            Some(l) if l < 9 => ParaStyle::heading(l.saturating_add(1)),
            _ => ParaStyle::Body,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Body,
    Footnote,
    Comment,
    Header,
    TextBox,
}

#[derive(Default)]
struct Field {
    /// Still reading the instruction (before the separator).
    code: bool,
    instr: String,
    href: String,
}

#[derive(Default)]
struct TableAcc {
    rows: Vec<Row>,
    cells: Vec<Cell>,
    paras: Vec<Paragraph>,
    /// The cell of a nested table being read, and the cells of its row so far. The model has no
    /// table inside a table, so a nested row becomes one paragraph with `│` between its cells.
    inner_paras: Vec<Paragraph>,
    inner_cells: Vec<Vec<Paragraph>>,
    count: usize,
    /// The table outgrew the output budget; the rest of it is dropped.
    dead: bool,
}

/// What a walk over one story found that the caller reports.
#[derive(Default)]
struct Found {
    drawings: bool,
    objects: bool,
}

struct Walker<'e, 's, 'p> {
    env: &'e Env,
    sink: &'s mut Sink<'p>,
    sec: SectionId,
    mode: Mode,
    para: ParaBuilder,
    buf: String,
    buf_style: u16,
    buf_href: String,
    fields: Vec<Field>,
    skipped_fields: usize,
    table: Option<TableAcc>,
    images: u32,
    label: Option<(String, String)>,
    refs: u32,
    /// Prefix of the numbered tags of notes (`e` for endnotes, `c` for comments).
    tag: &'static str,
    counters: BTreeMap<u16, [u32; 9]>,
    found: Found,
    stop: bool,
}

impl<'e, 's, 'p> Walker<'e, 's, 'p> {
    fn new(env: &'e Env, sink: &'s mut Sink<'p>, sec: SectionId, mode: Mode) -> Self {
        Self {
            env,
            sink,
            sec,
            mode,
            para: ParaBuilder::new(),
            buf: String::new(),
            buf_style: 0,
            buf_href: String::new(),
            fields: Vec::new(),
            skipped_fields: 0,
            table: None,
            images: 0,
            label: None,
            refs: 0,
            tag: "",
            counters: BTreeMap::new(),
            found: Found::default(),
            stop: false,
        }
    }

    fn in_code(&self) -> bool {
        self.fields.iter().any(|f| f.code)
    }

    fn href(&self) -> String {
        self.fields
            .iter()
            .rev()
            .find(|f| !f.href.is_empty())
            .map(|f| f.href.clone())
            .unwrap_or_default()
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let href = std::mem::take(&mut self.buf_href);
            self.para.text(&self.buf, self.buf_style, &href);
            self.buf.clear();
        }
    }

    /// Appends visible text with the given style.
    fn put(&mut self, c: char, bits: u16) {
        if self.in_code() {
            if let Some(f) = self.fields.iter_mut().rev().find(|f| f.code)
                && f.instr.len() < MAX_INSTR
            {
                f.instr.push(c);
            }
            return;
        }
        let href = self.href();
        if (self.buf_style != bits || self.buf_href != href) && !self.buf.is_empty() {
            self.flush();
        }
        if self.buf.is_empty() {
            self.buf_style = bits;
            self.buf_href = href;
        }
        self.buf.push(c);
    }

    fn put_str(&mut self, s: &str, bits: u16) {
        for c in s.chars() {
            self.put(c, bits);
        }
    }

    fn field_begin(&mut self) {
        if self.fields.len() >= MAX_FIELDS {
            self.skipped_fields = self.skipped_fields.saturating_add(1);
            return;
        }
        self.flush();
        self.fields.push(Field { code: true, ..Field::default() });
    }

    fn field_separator(&mut self) {
        if self.skipped_fields > 0 {
            return;
        }
        self.flush();
        if let Some(f) = self.fields.last_mut()
            && f.code
        {
            f.code = false;
            f.href = crate::docx::hyperlink_of(&f.instr).unwrap_or_default();
        }
    }

    fn field_end(&mut self) {
        if self.skipped_fields > 0 {
            self.skipped_fields -= 1;
            return;
        }
        self.flush();
        self.fields.pop();
    }

    fn run(&mut self, cp0: usize, cp1: usize) {
        let end = cp1.min(self.env.text.len());
        let mut cp = cp0;
        let mut chp = Chp::default();
        let mut valid = u32::MAX..u32::MAX;
        while cp < end && !self.stop {
            let at = narrow(cp);
            if !valid.contains(&at) {
                let (c, range) = self.env.chp.at(at);
                chp = c;
                valid = range;
            }
            let Some(&u) = self.env.text.get(cp) else { break };
            cp += 1;
            let bits = chp.bits;
            match u {
                0x0D => self.end_para(cp - 1, 0x0D),
                0x07 => self.end_para(cp - 1, 0x07),
                0x0C => {
                    self.end_para(cp - 1, 0x0D);
                    if self.table.is_none() && !self.stop {
                        self.sink.push(self.sec, Block::Rule);
                    }
                }
                0x0A | 0x0B => self.put('\n', bits),
                0x09 => self.put('\t', bits),
                0x0E => self.put(' ', bits),
                0x13 => self.field_begin(),
                0x14 => self.field_separator(),
                0x15 => self.field_end(),
                0x1E => self.put('\u{2011}', bits),
                0x01 if chp.spec => {
                    if chp.ole {
                        self.found.objects = true;
                    }
                    if !self.in_code() {
                        self.images = self.images.saturating_add(1);
                    }
                }
                0x02 if chp.spec => self.reference(true, cp - 1, bits),
                0x05 if chp.spec => self.reference(false, cp - 1, bits),
                0x08 if chp.spec => self.found.drawings = true,
                0x28 if chp.spec && chp.symbol != 0 => self.put('□', bits),
                0..=0x1F => {}
                _ => {
                    // Surrogate pairs are combined; a lone surrogate becomes U+FFFD.
                    let c = if (0xD800..0xDC00).contains(&u) {
                        match self.env.text.get(cp).copied() {
                            Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                                cp += 1;
                                let v = 0x10000
                                    + ((u32::from(u) - 0xD800) << 10)
                                    + (u32::from(lo) - 0xDC00);
                                char::from_u32(v).unwrap_or('\u{FFFD}')
                            }
                            _ => '\u{FFFD}',
                        }
                    } else {
                        char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}')
                    };
                    self.put(c, bits);
                }
            }
        }
        self.flush();
        // A story ends with a paragraph mark; text before an unterminated end still counts.
        self.finish_para_at(cp0.max(end.saturating_sub(1)), 0x0D, true);
        self.close_table();
    }

    /// A footnote (`0x02`) or comment (`0x05`) reference mark.
    fn reference(&mut self, footnote: bool, cp: usize, bits: u16) {
        self.refs = self.refs.saturating_add(1);
        match (self.mode, footnote) {
            (Mode::Body | Mode::Header | Mode::TextBox, _) => {
                if self.in_code() {
                    return;
                }
                let at = narrow(cp);
                let tag = if !footnote {
                    match self.env.cm_refs.binary_search(&at) {
                        Ok(i) => format!("[c{}]", i + 1),
                        Err(_) => format!("[c{}]", self.refs),
                    }
                } else if let Ok(i) = self.env.fn_refs.binary_search(&at) {
                    format!("[{}]", i + 1)
                } else if let Ok(i) = self.env.en_refs.binary_search(&at) {
                    format!("[e{}]", i + 1)
                } else {
                    format!("[{}]", self.refs)
                };
                self.put_str(&tag, bits | style::SUPER);
            }
            (Mode::Footnote, true) | (Mode::Comment, false) if self.label.is_none() => {
                self.label = Some((format!("[{}{}]", self.tag, self.refs), String::new()));
            }
            _ => {}
        }
    }

    fn end_para(&mut self, cp: usize, mark: u16) {
        self.finish_para_at(cp, mark, false);
    }

    /// Ends the paragraph whose mark is at `cp`. At the end of a story (`last`) an empty
    /// trailing paragraph produces nothing.
    fn finish_para_at(&mut self, cp: usize, mark: u16, last: bool) {
        self.flush();
        // A field instruction never spans paragraphs; an unterminated one must not swallow the
        // text that follows it.
        if self.fields.iter().any(|f| f.code) || self.skipped_fields > 0 {
            self.fields.retain(|f| !f.code);
            self.skipped_fields = 0;
        }
        if last && self.para.is_empty() && self.images == 0 && self.label.is_none() {
            return;
        }
        let pap = self.env.pap.at(narrow(cp)).0;
        let depth = if self.mode == Mode::Comment { 0 } else { pap.depth() };

        if depth == 0 {
            self.close_table();
            let para = self.make_para(&pap, false);
            let images = std::mem::take(&mut self.images);
            if let Some(p) = para
                && !self.sink.para(self.sec, p)
            {
                self.stop = true;
            }
            for _ in 0..images {
                if !self.sink.image(self.sec, ImageInfo::default()) {
                    self.stop = true;
                }
            }
            return;
        }

        if pap.ttp {
            // End-of-row mark: nothing to show.
            self.para.take(ParaStyle::Body);
            self.images = 0;
            if depth == 1 {
                self.close_row();
            } else if let Some(acc) = self.table.as_mut() {
                acc.flush_inner();
            }
            return;
        }
        let para = self.make_para(&pap, true);
        let images = std::mem::take(&mut self.images);
        self.sink.count(NoticeKind::ImagesNotShown, "", u64::from(images));
        let acc = self.table.get_or_insert_with(TableAcc::default);
        if acc.dead {
            return;
        }
        if depth >= 2 {
            // In a table inside a cell: collect the cell; its row ends at the row mark.
            if let Some(p) = para {
                acc.count = acc.count.saturating_add(p.runs.len().max(1));
                acc.inner_paras.push(p);
            }
            if mark == 0x07 || pap.inner_cell {
                let cell = std::mem::take(&mut acc.inner_paras);
                acc.inner_cells.push(cell);
            }
        } else if let Some(p) = para {
            acc.count = acc.count.saturating_add(p.runs.len().max(1));
            acc.paras.push(p);
        }
        if mark == 0x07 && depth == 1 {
            acc.flush_inner();
            let paras = std::mem::take(&mut acc.paras);
            acc.cells.push(Cell { span: 1, header: false, paras });
            acc.count = acc.count.saturating_add(1);
        }
        if acc.count > self.sink.items_left() {
            acc.dead = true;
            self.sink.truncated("table");
            self.close_table();
            self.table = Some(TableAcc { dead: true, ..TableAcc::default() });
        }
    }

    fn close_row(&mut self) {
        if let Some(acc) = self.table.as_mut() {
            acc.flush_inner();
            if !acc.paras.is_empty() {
                let paras = std::mem::take(&mut acc.paras);
                acc.cells.push(Cell { span: 1, header: false, paras });
            }
            if !acc.cells.is_empty() {
                let cells = std::mem::take(&mut acc.cells);
                acc.rows.push(Row { cells });
            }
        }
    }

    fn close_table(&mut self) {
        self.close_row();
        let Some(acc) = self.table.take() else { return };
        if acc.rows.is_empty() {
            return;
        }
        if !self.sink.push(self.sec, Block::Table(Table { rows: acc.rows })) {
            self.stop = true;
        }
    }

    /// Builds the paragraph from the pending runs. `in_cell`: inside a table.
    fn make_para(&mut self, pap: &Pap, in_cell: bool) -> Option<Paragraph> {
        let style = match self.mode {
            Mode::Footnote => ParaStyle::Footnote,
            Mode::Comment => ParaStyle::Comment,
            Mode::Header => ParaStyle::HeaderFooter,
            Mode::Body if !in_cell => self.env.para_style(pap),
            Mode::Body | Mode::TextBox => ParaStyle::Body,
        };
        let mut para = self.para.take(style);
        para.align = pap.align();
        if self.mode == Mode::Body
            && pap.ilfo != 0
            && pap.ilfo < 0x8000
            && let Some((marker, level)) =
                self.env.lists.marker(pap.ilfo, pap.ilvl, &mut self.counters)
        {
            para.marker = marker;
            para.level = level;
        }
        if let Some((marker, author)) = self.label.take() {
            if para.runs.is_empty() {
                // A note without text (the trailing mark of a story) is not shown.
                return None;
            }
            para.marker = marker;
            if let Some(first) = para.runs.first_mut() {
                first.text = first.text.trim_start().to_owned();
            }
            if !author.is_empty() {
                para.runs.insert(
                    0,
                    crate::model::Run {
                        text: format!("{author}: "),
                        style: style::BOLD,
                        href: String::new(),
                    },
                );
            }
        }
        if in_cell && self.images > 0 {
            para.runs.push(crate::model::Run {
                text: "▣".to_owned(),
                style: style::ITALIC,
                href: String::new(),
            });
        }
        if para.runs.is_empty() && para.marker.is_empty() { None } else { Some(para) }
    }
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

fn read_stream(cf: &CompoundFile<'_>, name: &str) -> Result<Option<Vec<u8>>, PreviewError> {
    let Some(idx) = cf.find(name) else { return Ok(None) };
    cf.read_stream(idx)
        .map(Some)
        .map_err(|e| PreviewError::Malformed(format!("Word document: stream {name}: {e}")))
}

/// Strings stored back to back, each a 16-bit length followed by UTF-16 (`GrpXstAtnOwners`).
fn parse_xst_list(table: &[u8], fc: usize, lcb: usize) -> Vec<String> {
    let Some(blob) = slice(table, fc, lcb) else { return Vec::new() };
    let mut pos = 0usize;
    let mut out = Vec::new();
    while out.len() < 4_096 {
        let Some(cch) = u16_le(blob, pos) else { break };
        let cch = usize::from(cch);
        pos = pos.saturating_add(2);
        let Some(bytes) = slice(blob, pos, cch.saturating_mul(2)) else { break };
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .take(512)
            .map(|c| {
                u16::from_le_bytes([
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        out.push(String::from_utf16_lossy(&units));
        pos = pos.saturating_add(cch.saturating_mul(2));
    }
    out
}

/// The character positions of a `Plc` without data (`n + 1` of them).
fn plc_cps(table: &[u8], fc: usize, lcb: usize) -> Vec<u32> {
    let Some(blob) = slice(table, fc, lcb) else { return Vec::new() };
    (0..(blob.len() / 4).min(65_536)).filter_map(|i| u32_le(blob, i * 4)).collect()
}

/// Extracts a Word 97–2003 (or Word 6/95) document.
pub(crate) fn extract(cf: &CompoundFile<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let wd = read_stream(cf, "WordDocument")?
        .ok_or_else(|| malformed("the WordDocument stream is missing"))?;
    let fib = Fib::parse(&wd)?;
    if fib.encrypted() {
        return Err(PreviewError::Encrypted);
    }
    let table_name = if fib.uses_table_1() { "1Table" } else { "0Table" };
    let table = if fib.modern {
        match read_stream(cf, table_name)? {
            Some(t) => t,
            None => match read_stream(cf, if fib.uses_table_1() { "0Table" } else { "1Table" })? {
                Some(t) => t,
                None => return Err(malformed("the table stream is missing")),
            },
        }
    } else {
        wd.clone()
    };

    // Text.
    let total_cp: u64 = fib.ccp.iter().map(|v| u64::from(*v)).sum();
    let mut pieces = fib.pair(pair::CLX).and_then(|(fc, lcb)| {
        if fib.modern || fib.complex() { parse_clx(&table, fc, lcb, fib.modern) } else { None }
    });
    let mut best_effort = false;
    if pieces.is_none() {
        // Without a piece table the text is stored contiguously from `fcMin`.
        best_effort = true;
        let n = u32::try_from(total_cp.min(u64::from(narrow(MAX_CP)))).unwrap_or(0);
        let wide = fib.modern && fib.ext_char();
        let avail = wd.len().saturating_sub(usz(fib.fc_min));
        let n = if wide { n.min(narrow(avail / 2)) } else { n.min(narrow(avail)) };
        let n = if n == 0 && fib.fc_mac > fib.fc_min {
            narrow(usz(fib.fc_mac - fib.fc_min) / if wide { 2 } else { 1 })
        } else {
            n
        };
        if n == 0 {
            return Err(malformed("the file has no readable text"));
        }
        pieces = Some(vec![Piece { cp0: 0, cp1: n, fc: usz(fib.fc_min), wide }]);
    }
    let pieces = pieces.unwrap_or_default();
    let mac = !fib.modern && fib.chse == 256;
    let (text, damaged) = load_text(&wd, &pieces, mac);
    if text.len() >= MAX_CP {
        sink.truncated("characters");
    }
    if damaged {
        sink.skipped("damaged Word text");
    }

    // Properties.
    let (chp, pap, styles, lists) = if fib.modern {
        let map = FcMap::new(&pieces);
        (
            collect_chp(&wd, &table, &fib, &map),
            collect_pap(&wd, &table, &fib, &map),
            parse_styles(&table, &fib),
            Lists::parse(&table, &fib),
        )
    } else {
        (RunSet::empty(), RunSet::empty(), Vec::new(), Lists::default())
    };
    let refs_of = |index: usize, record: usize| -> Vec<u32> {
        // A `Plcf` of references: n + 1 character positions, then n fixed-size records.
        let Some((fc, lcb)) = fib.pair(index) else { return Vec::new() };
        let n = lcb.saturating_sub(4) / record.saturating_add(4);
        let mut cps = plc_cps(&table, fc, lcb.min(n.saturating_mul(4).saturating_add(4)));
        cps.truncate(n);
        cps.sort_unstable();
        cps
    };
    let fn_refs = refs_of(pair::PLCF_FND_REF, 2);
    let en_refs = refs_of(pair::PLCF_END_REF, 2);
    let cm_refs = refs_of(pair::PLCF_AND_REF, 30);
    let env = Env { text, chp, pap, styles, lists, fn_refs, en_refs, cm_refs };
    if !fib.modern || best_effort {
        sink.note(NoticeKind::BestEffort, "word 6/95 or damaged structure");
    }

    let mut found = Found::default();
    let mut merge = |f: Found| {
        found.drawings |= f.drawings;
        found.objects |= f.objects;
    };

    // Main text.
    if let Some(sec) = sink.open_section(SectionKind::Body, "", false) {
        let mut w = Walker::new(&env, sink, sec, Mode::Body);
        w.run(0, fib.ccp(Story::Main));
        merge(w.found);
    }

    // Footnotes and endnotes.
    for (story, name) in [(Story::Footnotes, ""), (Story::Endnotes, "")] {
        let len = fib.ccp(story);
        if len == 0 || sink.is_full() {
            continue;
        }
        let start = fib.story_start(story);
        if let Some(sec) = sink.open_section(SectionKind::Notes, name, false) {
            let mut w = Walker::new(&env, sink, sec, Mode::Footnote);
            if matches!(story, Story::Endnotes) {
                w.tag = "e";
            }
            w.run(start, start.saturating_add(len));
            merge(w.found);
            sink.drop_if_empty(sec);
        }
    }

    // Comments.
    let atn = fib.ccp(Story::Comments);
    if atn > 0 && !sink.is_full() {
        let start = fib.story_start(Story::Comments);
        let authors = fib
            .pair(pair::GRP_XST_ATN_OWNERS)
            .map(|(fc, lcb)| parse_xst_list(&table, fc, lcb))
            .unwrap_or_default();
        let txt = fib
            .pair(pair::PLCF_AND_TXT)
            .map(|(fc, lcb)| plc_cps(&table, fc, lcb))
            .unwrap_or_default();
        let refs = fib.pair(pair::PLCF_AND_REF).and_then(|(fc, lcb)| slice(&table, fc, lcb));
        if let Some(sec) = sink.open_section(SectionKind::Comments, "", false) {
            // `PlcfandRef`: n + 1 positions, then n records of 30 bytes whose `ibst` (offset 20)
            // indexes the author list. The text positions are one entry longer (the story's
            // final paragraph mark).
            let n = refs.map_or(usize::MAX, |r| r.len().saturating_sub(4) / 34);
            let ranges: Vec<(usize, usize)> = txt
                .windows(2)
                .take(n.min(65_536))
                .filter_map(|w| Some((usz(*w.first()?), usz(*w.get(1)?))))
                .filter(|(a, b)| a < b)
                .collect();
            if ranges.is_empty() {
                let mut w = Walker::new(&env, sink, sec, Mode::Comment);
                w.tag = "c";
                w.run(start, start.saturating_add(atn));
                merge(w.found);
            } else {
                for (i, (a, b)) in ranges.iter().enumerate() {
                    if sink.is_full() {
                        break;
                    }
                    let author = refs
                        .filter(|_| i < n)
                        .and_then(|r| {
                            let at = (n + 1) * 4 + i * 30;
                            u16_le(r, at + 20)
                        })
                        .and_then(|ibst| authors.get(usize::from(ibst)))
                        .cloned()
                        .unwrap_or_default();
                    let mut w = Walker::new(&env, sink, sec, Mode::Comment);
                    w.label = Some((format!("[c{}]", i + 1), author));
                    w.refs = narrow(i + 1);
                    w.run(
                        start.saturating_add(*a),
                        start.saturating_add(*b).min(start.saturating_add(atn)),
                    );
                    merge(w.found);
                }
            }
            sink.drop_if_empty(sec);
        }
    }

    // Headers and footers.
    let hdd = fib.ccp(Story::Headers);
    if hdd > 0 && !sink.is_full() {
        let start = fib.story_start(Story::Headers);
        let cps =
            fib.pair(pair::PLCF_HDD).map(|(fc, lcb)| plc_cps(&table, fc, lcb)).unwrap_or_default();
        if let Some(sec) = sink.open_section(SectionKind::Headers, "", false) {
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            // The first six stories hold the footnote and endnote separators.
            for i in 6..cps.len().saturating_sub(1) {
                let (Some(&a), Some(&b)) = (cps.get(i), cps.get(i + 1)) else { break };
                let (a, b) = (usz(a).min(hdd), usz(b).min(hdd));
                if a < b {
                    ranges.push((start.saturating_add(a), start.saturating_add(b)));
                }
            }
            if cps.is_empty() {
                ranges.push((start, start.saturating_add(hdd)));
            }
            let mut seen: Vec<String> = Vec::new();
            for (a, b) in ranges.into_iter().take(4_096) {
                if sink.is_full() {
                    break;
                }
                let key: String = env
                    .text
                    .get(a..b.min(env.text.len()))
                    .unwrap_or(&[])
                    .iter()
                    .take(4_096)
                    .filter_map(|&u| char::from_u32(u32::from(u)))
                    .collect();
                if key.trim().is_empty() || seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let mut w = Walker::new(&env, sink, sec, Mode::Header);
                w.run(a, b);
                merge(w.found);
            }
            sink.drop_if_empty(sec);
        }
    }

    // Text boxes.
    for story in [Story::TextBoxes, Story::HeaderTextBoxes] {
        let len = fib.ccp(story);
        if len == 0 || sink.is_full() {
            continue;
        }
        let start = fib.story_start(story);
        if let Some(sec) = sink.open_section(SectionKind::Notes, "Text boxes", false) {
            let mut w = Walker::new(&env, sink, sec, Mode::TextBox);
            w.run(start, start.saturating_add(len));
            merge(w.found);
            sink.drop_if_empty(sec);
        }
    }

    if fib.ccp(Story::Macros) > 0 {
        sink.skipped("macro text");
    }
    if found.drawings {
        sink.note(NoticeKind::Unsupported, "drawings and shapes");
    }
    if found.objects {
        sink.note(NoticeKind::Unsupported, "embedded objects");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // synthetic fixtures with tiny sizes
mod tests {
    use super::*;
    use crate::model::{FileFormat, Preview};
    use crate::testkit::{Lcg, fuzz_iterations, ok, paragraphs, run, section_of, text_of};
    use arca_container::cfb::writer::CfbBuilder;

    // ---- sprm helpers ----------------------------------------------------------------------

    fn sp1(op: u16, v: u8) -> Vec<u8> {
        let mut o = op.to_le_bytes().to_vec();
        o.push(v);
        o
    }

    fn sp2(op: u16, v: u16) -> Vec<u8> {
        let mut o = op.to_le_bytes().to_vec();
        o.extend(v.to_le_bytes());
        o
    }

    fn sp4(op: u16, v: u32) -> Vec<u8> {
        let mut o = op.to_le_bytes().to_vec();
        o.extend(v.to_le_bytes());
        o
    }

    const HIDDEN: u16 = 0x083C;
    const DELETED: u16 = 0x0800;
    const INSERTED: u16 = 0x0801;
    const BOLD: u16 = 0x0835;
    const SPEC: u16 = 0x0855;

    // ---- a minimal Word 97 writer ---------------------------------------------------------

    #[derive(Default, Clone)]
    struct W {
        /// Text pieces and whether each is stored compressed (8-bit).
        pieces: Vec<(Vec<u16>, bool)>,
        /// Story lengths; `None` uses the whole text for the main story.
        ccp: Option<[u32; 8]>,
        chp: Vec<(usize, usize, Vec<u8>)>,
        pap: Vec<(usize, usize, u16, Vec<u8>)>,
        flags: u16,
        /// Structures of the table stream by `FibRgFcLcb` index.
        extra: Vec<(usize, Vec<u8>)>,
        no_clx: bool,
    }

    fn byte_of(u: u16) -> u8 {
        match u {
            0x20AC => 0x80,
            0..=0xFF => u8::try_from(u).unwrap_or(b'?'),
            _ => b'?',
        }
    }

    impl W {
        fn new() -> Self {
            Self::default()
        }

        fn piece(mut self, s: &str, compressed: bool) -> Self {
            self.pieces.push((s.encode_utf16().collect(), compressed));
            self
        }

        fn ccp(mut self, ccp: [u32; 8]) -> Self {
            self.ccp = Some(ccp);
            self
        }

        fn chp(mut self, a: usize, b: usize, prl: Vec<u8>) -> Self {
            self.chp.push((a, b, prl));
            self
        }

        fn pap(mut self, a: usize, b: usize, istd: u16, prl: Vec<u8>) -> Self {
            self.pap.push((a, b, istd, prl));
            self
        }

        fn flags(mut self, f: u16) -> Self {
            self.flags = f;
            self
        }

        fn extra(mut self, index: usize, data: Vec<u8>) -> Self {
            self.extra.push((index, data));
            self
        }

        /// Returns `(WordDocument, 1Table)`.
        #[allow(clippy::cast_possible_truncation)]
        fn streams(&self) -> (Vec<u8>, Vec<u8>) {
            const TEXT_AT: usize = 0x800;
            let mut wd = vec![0u8; TEXT_AT];
            let mut table: Vec<u8> = Vec::new();
            let mut pairs: Vec<(u32, u32)> = vec![(0, 0); 80];
            // (cp0, cp1, fc, wide)
            let mut info: Vec<(usize, usize, usize, bool)> = Vec::new();
            let mut cp = 0usize;
            for (units, compressed) in &self.pieces {
                let fc = wd.len();
                if *compressed {
                    wd.extend(units.iter().map(|&u| byte_of(u)));
                } else {
                    for u in units {
                        wd.extend(u.to_le_bytes());
                    }
                }
                info.push((cp, cp + units.len(), fc, !*compressed));
                cp += units.len();
            }
            let fc_of = |cp: usize| -> usize {
                for &(a, b, fc, wide) in &info {
                    if cp >= a && cp <= b {
                        return fc + (cp - a) * if wide { 2 } else { 1 };
                    }
                }
                0
            };
            // Piece table.
            if !self.no_clx {
                let mut plc = Vec::new();
                for &(a, _, _, _) in &info {
                    plc.extend((a as u32).to_le_bytes());
                }
                plc.extend((cp as u32).to_le_bytes());
                for &(_, _, fc, wide) in &info {
                    plc.extend(0u16.to_le_bytes());
                    let raw = if wide { fc as u32 } else { (fc as u32 * 2) | 0x4000_0000 };
                    plc.extend(raw.to_le_bytes());
                    plc.extend(0u16.to_le_bytes());
                }
                let mut clx = vec![2u8];
                clx.extend((plc.len() as u32).to_le_bytes());
                clx.extend(plc);
                pairs[pair::CLX] = (table.len() as u32, clx.len() as u32);
                table.extend(clx);
            }
            // Pad the text area to a page.
            while !wd.len().is_multiple_of(PAGE) {
                wd.push(0);
            }
            // Character FKPs.
            let mut plc_pages: Vec<(u32, u32)> = Vec::new();
            let mut runs: Vec<(usize, usize, Vec<u8>)> =
                self.chp.iter().map(|(a, b, p)| (fc_of(*a), fc_of(*b), p.clone())).collect();
            runs.sort_by_key(|r| r.0);
            for chunk in runs.chunks(24) {
                let mut filled: Vec<(usize, usize, Vec<u8>)> = Vec::new();
                for r in chunk {
                    if let Some(last) = filled.last()
                        && last.1 < r.0
                    {
                        let gap = (last.1, r.0, Vec::new());
                        filled.push(gap);
                    }
                    filled.push(r.clone());
                }
                let mut page = vec![0u8; PAGE];
                let crun = filled.len();
                page[PAGE - 1] = crun as u8;
                let rgb = (crun + 1) * 4;
                let mut top = PAGE - 1;
                for (i, (a, b, prl)) in filled.iter().enumerate() {
                    page[i * 4..i * 4 + 4].copy_from_slice(&(*a as u32).to_le_bytes());
                    page[(i + 1) * 4..(i + 1) * 4 + 4].copy_from_slice(&(*b as u32).to_le_bytes());
                    if !prl.is_empty() {
                        top -= prl.len() + 1;
                        top &= !1;
                        page[top] = prl.len() as u8;
                        page[top + 1..top + 1 + prl.len()].copy_from_slice(prl);
                        page[rgb + i] = (top / 2) as u8;
                    }
                }
                plc_pages.push((
                    wd.len() as u32 / PAGE as u32,
                    filled.first().map_or(0, |f| f.0 as u32),
                ));
                wd.extend(page);
            }
            if !plc_pages.is_empty() {
                let mut plc = Vec::new();
                for (_, first) in &plc_pages {
                    plc.extend(first.to_le_bytes());
                }
                plc.extend((wd.len() as u32).to_le_bytes());
                for (pn, _) in &plc_pages {
                    plc.extend(pn.to_le_bytes());
                }
                pairs[pair::PLCF_BTE_CHPX] = (table.len() as u32, plc.len() as u32);
                table.extend(plc);
            }
            // Paragraph FKPs.
            let mut plc_pages: Vec<(u32, u32)> = Vec::new();
            let mut pruns: Vec<(usize, usize, u16, Vec<u8>)> =
                self.pap.iter().map(|(a, b, i, p)| (fc_of(*a), fc_of(*b), *i, p.clone())).collect();
            pruns.sort_by_key(|r| r.0);
            for chunk in pruns.chunks(12) {
                let mut filled: Vec<(usize, usize, u16, Vec<u8>)> = Vec::new();
                for r in chunk {
                    if let Some(last) = filled.last()
                        && last.1 < r.0
                    {
                        let gap = (last.1, r.0, 0, Vec::new());
                        filled.push(gap);
                    }
                    filled.push(r.clone());
                }
                let mut page = vec![0u8; PAGE];
                let crun = filled.len();
                page[PAGE - 1] = crun as u8;
                let rgbx = (crun + 1) * 4;
                let mut top = PAGE - 1;
                for (i, (a, b, istd, prl)) in filled.iter().enumerate() {
                    page[i * 4..i * 4 + 4].copy_from_slice(&(*a as u32).to_le_bytes());
                    page[(i + 1) * 4..(i + 1) * 4 + 4].copy_from_slice(&(*b as u32).to_le_bytes());
                    if *istd != 0 || !prl.is_empty() {
                        let mut content = istd.to_le_bytes().to_vec();
                        content.extend(prl);
                        if content.len() % 2 == 0 {
                            content.push(0);
                        }
                        let cb = content.len().div_ceil(2);
                        top -= content.len() + 1;
                        top &= !1;
                        page[top] = cb as u8;
                        page[top + 1..top + 1 + content.len()].copy_from_slice(&content);
                        page[rgbx + i * 13] = (top / 2) as u8;
                    }
                }
                plc_pages.push((
                    wd.len() as u32 / PAGE as u32,
                    filled.first().map_or(0, |f| f.0 as u32),
                ));
                wd.extend(page);
            }
            if !plc_pages.is_empty() {
                let mut plc = Vec::new();
                for (_, first) in &plc_pages {
                    plc.extend(first.to_le_bytes());
                }
                plc.extend((wd.len() as u32).to_le_bytes());
                for (pn, _) in &plc_pages {
                    plc.extend(pn.to_le_bytes());
                }
                pairs[pair::PLCF_BTE_PAPX] = (table.len() as u32, plc.len() as u32);
                table.extend(plc);
            }
            for (index, data) in &self.extra {
                pairs[*index] = (table.len() as u32, data.len() as u32);
                table.extend(data);
            }
            // File information block.
            let mut ccp = self.ccp.unwrap_or([cp as u32, 0, 0, 0, 0, 0, 0, 0]);
            if self.ccp.is_none() {
                ccp[0] = cp as u32;
            }
            let mut fib = vec![0u8; 0x9A + 8 * pairs.len()];
            fib[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
            fib[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
            fib[0x0A..0x0C].copy_from_slice(&(self.flags | 0x0200 | 0x1000).to_le_bytes());
            fib[0x20..0x22].copy_from_slice(&14u16.to_le_bytes());
            fib[0x3E..0x40].copy_from_slice(&22u16.to_le_bytes());
            for (i, v) in ccp.iter().enumerate() {
                let at = 0x40 + 4 * (3 + i);
                fib[at..at + 4].copy_from_slice(&v.to_le_bytes());
            }
            fib[0x98..0x9A].copy_from_slice(&(pairs.len() as u16).to_le_bytes());
            for (i, (fc, lcb)) in pairs.iter().enumerate() {
                let at = 0x9A + 8 * i;
                fib[at..at + 4].copy_from_slice(&fc.to_le_bytes());
                fib[at + 4..at + 8].copy_from_slice(&lcb.to_le_bytes());
            }
            fib[0x18..0x1C].copy_from_slice(&(TEXT_AT as u32).to_le_bytes());
            wd[..fib.len()].copy_from_slice(&fib);
            (wd, table)
        }

        fn build(&self) -> Vec<u8> {
            let (wd, table) = self.streams();
            wrap(wd, table)
        }
    }

    fn wrap(wd: Vec<u8>, table: Vec<u8>) -> Vec<u8> {
        let mut b = CfbBuilder::new();
        assert!(b.add_stream("WordDocument", wd).is_ok());
        assert!(b.add_stream("1Table", table).is_ok());
        b.finish().unwrap_or_default()
    }

    fn body(p: &Preview) -> &crate::model::Section {
        section_of(p, SectionKind::Body).expect("body section")
    }

    fn texts(p: &Preview) -> Vec<String> {
        paragraphs(body(p)).iter().map(|q| q.text()).collect()
    }

    // ---- tests -----------------------------------------------------------------------------

    #[test]
    fn pieces_may_be_eight_bit_or_utf16_and_keep_their_order() {
        let p = ok(&W::new()
            .piece("Hello caf\u{e9} \u{20ac}5\r", true)
            .piece("Привет, мир\r", false)
            .build());
        assert_eq!(p.format, FileFormat::Doc);
        assert_eq!(texts(&p), vec!["Hello café €5", "Привет, мир"]);
    }

    #[test]
    fn surrogate_pairs_are_joined_and_lone_ones_replaced() {
        let mut w = W::new();
        w.pieces.push((vec![0x61, 0xD83D, 0xDE00, 0x62, 0xD83D, 0x63, 0x0D], false));
        assert_eq!(texts(&ok(&w.build())), vec!["a😀b\u{FFFD}c"]);
    }

    #[test]
    fn special_characters() {
        let p = ok(&W::new().piece("a\tb\x0bc\x1e\x1fd\r\x0cnext\r", true).build());
        let sec = body(&p);
        assert_eq!(text_of(sec), "a\tb\nc\u{2011}d\nnext");
        assert!(sec.blocks.iter().any(|b| matches!(b, Block::Rule)), "page break");
    }

    #[test]
    fn character_properties_hidden_deleted_inserted_and_styles() {
        // "plain hidden deleted inserted bold\r"
        let t = "plain hidden deleted inserted bold\r";
        let w = W::new()
            .piece(t, true)
            .chp(6, 12, sp1(HIDDEN, 1))
            .chp(13, 20, sp1(DELETED, 1))
            .chp(21, 29, sp1(INSERTED, 1))
            .chp(
                30,
                34,
                [sp1(BOLD, 1), sp1(0x0836, 0x81), sp1(0x2A3E, 1), sp1(0x2A48, 2)].concat(),
            );
        let p = ok(&w.build());
        let para = paragraphs(body(&p))[0].clone();
        let by = |s: &str| para.runs.iter().find(|r| r.text.trim() == s).map(|r| r.style);
        assert_eq!(by("plain"), Some(0));
        assert_eq!(by("hidden"), Some(style::HIDDEN));
        assert_eq!(by("deleted"), Some(style::DELETED));
        assert_eq!(by("inserted"), Some(style::INSERTED));
        assert_eq!(by("bold"), Some(style::BOLD | style::ITALIC | style::UNDERLINE | style::SUB));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.count == 1));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::TrackedChanges && n.count == 2));
    }

    #[test]
    fn hidden_text_can_be_left_out_by_policy() {
        let w = W::new().piece("shown SECRET end\r", true).chp(6, 12, sp1(HIDDEN, 1));
        let policy =
            crate::PreviewPolicy { reveal_hidden: false, ..crate::PreviewPolicy::default() };
        let p = crate::preview(&w.build(), &policy).expect("preview");
        assert_eq!(text_of(body(&p)), "shown  end");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted));
    }

    #[test]
    fn toggles_follow_the_operand() {
        let c = parse_chp(
            &[sp1(BOLD, 1), sp1(0x0836, 0x80), sp1(0x0837, 0x81), sp1(HIDDEN, 1), sp1(HIDDEN, 0)]
                .concat(),
        );
        assert_eq!(c.bits, style::BOLD | style::STRIKE);
    }

    #[test]
    fn fields_show_their_result_and_hyperlinks_keep_the_target() {
        let t = "see \x13 HYPERLINK \"http://e.example/x?a=1\" \x14link text\x15 and \x13 DDEAUTO c:\\\\cmd \x14result\x15 end\r";
        let p = ok(&W::new().piece(t, true).build());
        let para = paragraphs(body(&p))[0].clone();
        assert_eq!(para.text(), "see link text and result end");
        let href = |s: &str| para.runs.iter().find(|r| r.text.contains(s)).map(|r| r.href.clone());
        assert_eq!(href("link text").as_deref(), Some("http://e.example/x?a=1"));
        assert_eq!(href("result").as_deref(), Some(""));
    }

    #[test]
    fn deeply_nested_and_unterminated_fields_are_survivable() {
        let mut t = String::new();
        for _ in 0..500 {
            t.push_str("\x13 X ");
        }
        t.push_str("\x14visible");
        t.push_str(&"\x15".repeat(400));
        t.push_str("\rafter\r");
        let p = ok(&W::new().piece(&t, true).build());
        let all = text_of(body(&p));
        assert!(all.contains("after"), "{all}");
    }

    #[test]
    fn tables_become_rows_and_cells() {
        // A | B / C | D, then a paragraph.
        let text = "A\x07B\x07\x07C\x07D\x07\x07tail\r";
        let w = W::new()
            .piece(text, true)
            .pap(0, 4, 0, sp1(0x2416, 1))
            .pap(4, 5, 0, [sp1(0x2416, 1), sp1(0x2417, 1)].concat())
            .pap(5, 9, 0, sp1(0x2416, 1))
            .pap(9, 10, 0, [sp1(0x2416, 1), sp1(0x2417, 1)].concat());
        let p = ok(&w.build());
        let sec = body(&p);
        let Some(Block::Table(t)) = sec.blocks.first() else {
            panic!("table first: {:?}", sec.blocks)
        };
        let cell_text = |r: usize, c: usize| {
            t.rows[r].cells[c].paras.iter().map(|q| q.text()).collect::<String>()
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(
            (cell_text(0, 0), cell_text(0, 1), cell_text(1, 0), cell_text(1, 1)),
            ("A".into(), "B".into(), "C".into(), "D".into())
        );
        assert!(matches!(sec.blocks.get(1), Some(Block::Para(q)) if q.text() == "tail"));
    }

    #[test]
    fn nested_tables_are_flattened_into_the_outer_cell() {
        // The inner table's marks carry itap = 2.
        let text = "outer\rinner\x07\x07after\x07\x07";
        let outer = [sp1(0x2416, 1)].concat();
        let inner = [sp1(0x2416, 1), sp4(0x6649, 2)].concat();
        let inner_ttp = [sp1(0x2416, 1), sp4(0x6649, 2), sp1(0x244C, 1)].concat();
        let ttp = [sp1(0x2416, 1), sp1(0x2417, 1)].concat();
        let w = W::new()
            .piece(text, true)
            .pap(0, 6, 0, outer.clone())
            .pap(6, 12, 0, inner)
            .pap(12, 13, 0, inner_ttp)
            .pap(13, 19, 0, outer)
            .pap(19, 20, 0, ttp);
        let p = ok(&w.build());
        let Some(Block::Table(t)) = body(&p).blocks.first() else { panic!("no table") };
        let all: Vec<String> =
            t.rows.iter().flat_map(|r| &r.cells).flat_map(|c| &c.paras).map(|q| q.text()).collect();
        assert_eq!(all.join("|"), "outer|inner|after");
    }

    #[test]
    fn a_word_97_file_with_a_nested_table_keeps_the_nested_cells_apart() {
        // tools/fixtures/make_nested.py, converted by LibreOffice.
        let data =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/nested.doc"))
                .expect("the fixture");
        let p = ok(&data);
        let Some(Block::Table(t)) = body(&p).blocks.iter().find(|b| matches!(b, Block::Table(_)))
        else {
            panic!("no table")
        };
        let cell =
            t.rows.get(1).and_then(|r| r.cells.get(1)).expect("the cell with the inner table");
        let paras: Vec<String> = cell.paras.iter().map(|q| q.text()).collect();
        assert_eq!(paras, ["O11", "I00 │ I01", "I10 │ I11", "после вложенной"]);
        let markers: Vec<String> = paragraphs(body(&p))
            .iter()
            .map(|q| q.marker.clone())
            .filter(|m| !m.is_empty())
            .collect();
        assert_eq!(markers, ["1.", "2.", "1.", "2.", "3."]);
    }

    #[test]
    fn a_nested_row_keeps_its_cells_apart() {
        // Word ends a nested cell with a plain paragraph mark that carries sprmPFInnerTableCell
        // and a nested row with one that carries sprmPFInnerTtp.
        let text = "pre\rI1\rI2\r\rJ1\rJ2\r\r\x07\x07";
        let outer = sp1(0x2416, 1);
        let cell = [sp1(0x2416, 1), sp4(0x6649, 2), sp1(0x244B, 1)].concat();
        let row = [sp1(0x2416, 1), sp4(0x6649, 2), sp1(0x244C, 1)].concat();
        let ttp = [sp1(0x2416, 1), sp1(0x2417, 1)].concat();
        let w = W::new()
            .piece(text, true)
            .pap(0, 4, 0, outer.clone())
            .pap(4, 10, 0, cell.clone())
            .pap(10, 11, 0, row.clone())
            .pap(11, 17, 0, cell)
            .pap(17, 18, 0, row)
            .pap(18, 19, 0, outer)
            .pap(19, 20, 0, ttp);
        let p = ok(&w.build());
        let Some(Block::Table(t)) = body(&p).blocks.first() else { panic!("no table") };
        let cell = t.rows.first().and_then(|r| r.cells.first()).expect("a cell");
        let paras: Vec<String> = cell.paras.iter().map(|q| q.text()).collect();
        assert_eq!(
            paras,
            vec!["pre", "I1 │ I2", "J1 │ J2"].into_iter().map(String::from).collect::<Vec<_>>()
        );
    }

    #[test]
    fn headings_come_from_style_identifiers_and_outline_levels() {
        // Style sheet with Normal, "Heading 1" (istd 1) and a custom Title-like style (sti 62).
        let std = |sti: u16, base: u16| {
            let mut s = Vec::new();
            s.extend(sti.to_le_bytes());
            s.extend(((base << 4) | 1).to_le_bytes());
            s.extend([0u8; 6]);
            s
        };
        let mut stsh = Vec::new();
        stsh.extend(18u16.to_le_bytes()); // cbStshi
        stsh.extend(3u16.to_le_bytes()); // cstd
        stsh.extend(10u16.to_le_bytes()); // cbSTDBase
        stsh.extend([0u8; 14]);
        for (sti, base) in [(0u16, 0x0FFFu16), (1, 0), (62, 0)] {
            let s = std(sti, base);
            stsh.extend((s.len() as u16).to_le_bytes());
            stsh.extend(s);
        }
        let text = "Heading\rTitle\rBody\rOutlined\r";
        let w = W::new()
            .piece(text, true)
            .extra(pair::STSHF, stsh)
            .pap(0, 8, 1, vec![])
            .pap(8, 14, 2, vec![])
            .pap(14, 19, 0, vec![])
            .pap(19, 28, 0, sp1(0x2640, 2));
        let p = ok(&w.build());
        let styles: Vec<ParaStyle> = paragraphs(body(&p)).iter().map(|q| q.style).collect();
        assert_eq!(
            styles,
            vec![ParaStyle::Heading1, ParaStyle::Title, ParaStyle::Body, ParaStyle::Heading3]
        );
    }

    fn list_tables(nfc: u8, text: &[u16], start: i32) -> (Vec<u8>, Vec<u8>) {
        // PlfLst: one simple list (a single level); the level follows the structure.
        let mut lst = Vec::new();
        lst.extend(1u16.to_le_bytes());
        lst.extend(77u32.to_le_bytes()); // lsid
        lst.extend(0u32.to_le_bytes()); // tplc
        lst.extend([0xFFu8, 0x0F].repeat(9));
        lst.push(1); // fSimpleList
        lst.push(0);
        let mut lvl = Vec::new();
        lvl.extend(start.to_le_bytes());
        lvl.push(nfc);
        lvl.extend([0u8; 23]); // the rest of the 28-byte LVLF (no property lists)
        lvl.extend((text.len() as u16).to_le_bytes());
        for u in text {
            lvl.extend(u.to_le_bytes());
        }
        // The level records are stored right behind the structure: caller appends them.
        let mut lfo = Vec::new();
        lfo.extend(1u32.to_le_bytes());
        lfo.extend(77u32.to_le_bytes());
        lfo.extend([0u8; 12]);
        (lst.into_iter().chain(Vec::new()).collect::<Vec<u8>>(), [lvl, lfo].concat())
    }

    /// One `LVL` as stored: the 28 fixed bytes and the number text.
    fn lvl(start: i32, nfc: u8, text: &[u16]) -> Vec<u8> {
        let mut v = start.to_le_bytes().to_vec();
        v.push(nfc);
        v.extend([0u8; 23]);
        v.extend((text.len() as u16).to_le_bytes());
        for u in text {
            v.extend(u.to_le_bytes());
        }
        v
    }

    #[test]
    fn a_list_instance_can_restart_a_level_and_redefine_it() {
        // Definition 5: a simple decimal list. Instance 1 uses it as is, instance 2 restarts at 7,
        // instance 3 numbers with capital letters instead.
        let mut head = 1u16.to_le_bytes().to_vec();
        head.extend(5u32.to_le_bytes());
        head.extend(0u32.to_le_bytes());
        head.extend([0xFFu8, 0x0F].repeat(9));
        head.push(1);
        head.push(0);
        let defs = lvl(1, 0, &[0, u16::from(b'.')]);
        let mut lfo = 3u32.to_le_bytes().to_vec();
        for clfolvl in [0u8, 1, 1] {
            lfo.extend(5u32.to_le_bytes());
            lfo.extend([0u8; 8]);
            lfo.push(clfolvl);
            lfo.extend([0u8; 3]);
        }
        // LFOData of instance 2: restart level 0 at 7. Of instance 3: a new level definition.
        lfo.extend(0u32.to_le_bytes()); // instance 1: cp, no overrides
        lfo.extend(0u32.to_le_bytes());
        lfo.extend(7u32.to_le_bytes());
        lfo.extend([0x10, 0, 0, 0]);
        lfo.extend(0u32.to_le_bytes());
        lfo.extend(1u32.to_le_bytes());
        lfo.extend([0x20, 0, 0, 0]);
        lfo.extend(lvl(1, 3, &[0, u16::from(b')')]));
        let mut lists = Lists::default();
        lists.parse_defs(&head, &defs);
        lists.parse_instances(&lfo);
        assert_eq!(lists.instances.len(), 3);
        let mut counters = BTreeMap::new();
        let mut marks =
            |ilfo: u16| lists.marker(ilfo, 0, &mut counters).map(|m| m.0).expect("marker");
        assert_eq!((marks(1), marks(1)), ("1.".to_owned(), "2.".to_owned()));
        assert_eq!((marks(2), marks(2)), ("7.".to_owned(), "8.".to_owned()));
        assert_eq!((marks(3), marks(3)), ("A)".to_owned(), "B)".to_owned()));
        // Hostile: counts that run past the data, nothing to panic about.
        let mut broken = Lists::default();
        broken.parse_defs(&head, &defs);
        broken.parse_instances(&lfo[..lfo.len() - 9]);
        let mut again = Lists::default();
        again.parse_instances(&[0xFF; 40]);
        assert!(again.instances.len() <= 65_536);
    }

    #[test]
    fn numbered_and_bulleted_lists_get_markers() {
        for (nfc, text, start, want) in [
            (0u8, vec![0u16, u16::from(b'.')], 1, vec!["1.", "2."]),
            (2, vec![u16::from(b'('), 0, u16::from(b')')], 3, vec!["(iii)", "(iv)"]),
            (23, vec![0xF0B7], 1, vec!["•", "•"]),
        ] {
            let (lst, rest) = list_tables(nfc, &text, start);
            // PlfLst is followed immediately by its levels, which sit in front of PlfLfo.
            let lfo = rest[rest.len() - 20..].to_vec();
            let lvl = rest[..rest.len() - 20].to_vec();
            let w = W::new()
                .piece("one\rtwo\r", true)
                .extra(pair::PLF_LST, [lst, lvl].concat())
                .extra(pair::PLF_LFO, lfo)
                .pap(0, 8, 0, [sp2(0x460B, 1), sp1(0x260A, 0)].concat());
            // The structures were appended in order, but PlfLst's levels must directly follow it:
            // `extra` places PlfLst first, so the levels are inside its recorded length — shrink it.
            let mut w = w;
            let (_, lst_data) = w.extra.remove(0);
            let split = 2 + 28;
            let (head, levels) = lst_data.split_at(split);
            let lfo_entry = w.extra.remove(0);
            w.extra.push((pair::PLF_LST, head.to_vec()));
            w.extra.push((pair::PLF_LFO, lfo_entry.1));
            let (wd, mut table) = w.streams();
            // Move the level bytes right behind the PlfLst structure.
            let (fc, lcb) = {
                let fib = Fib::parse(&wd).expect("fib");
                fib.pair(pair::PLF_LST).expect("plflst")
            };
            // Rebuild: PlfLfo comes after PlfLst in `extra` order; insert levels between them.
            let at = fc + lcb;
            let mut fixed = table[..at].to_vec();
            fixed.extend(levels);
            fixed.extend(&table[at..]);
            table = fixed;
            // Shift the PlfLfo pair by the inserted length.
            let mut wd = wd;
            let lfo_pair_at = 0x9A + 8 * pair::PLF_LFO;
            let old = u32::from_le_bytes(wd[lfo_pair_at..lfo_pair_at + 4].try_into().expect("4"));
            wd[lfo_pair_at..lfo_pair_at + 4]
                .copy_from_slice(&(old + levels.len() as u32).to_le_bytes());
            let p = ok(&wrap(wd, table));
            let got: Vec<String> = paragraphs(body(&p)).iter().map(|q| q.marker.clone()).collect();
            assert_eq!(got, want, "nfc {nfc}");
        }
    }

    #[test]
    fn password_protected_documents_are_reported() {
        let r = run(&W::new().piece("secret\r", true).flags(0x0100).build());
        assert_eq!(r, Err(PreviewError::Encrypted));
    }

    #[test]
    fn notes_comments_headers_and_text_boxes_get_their_own_sections() {
        // main | footnote | header story | comments | endnotes | text box
        let main = "Body text\r";
        let ftn = "\x02 The footnote\r";
        let hdd = "H1 first\rH1 second\rFOOTER\r";
        let atn = "\x05 comment text\r";
        let edn = "\x02 The endnote\r";
        let txbx = "BOXED TEXT\r";
        let units = |s: &str| s.encode_utf16().count() as u32;
        let ccp = [units(main), units(ftn), units(hdd), 0, units(atn), units(edn), units(txbx), 0];
        // PlcfHdd: six separator stories (empty), then three real ones.
        let mut hdd_plc = Vec::new();
        let mut at = 0u32;
        let mut cps = vec![0u32; 7];
        for part in ["H1 first\r", "H1 second\r", "FOOTER\r"] {
            at += units(part);
            cps.push(at);
        }
        for c in &cps {
            hdd_plc.extend(c.to_le_bytes());
        }
        // Comments: authors, one reference and its text range.
        let owners: Vec<u8> = {
            let mut o = Vec::new();
            for name in ["Zed", "Alice Reviewer"] {
                o.extend((name.encode_utf16().count() as u16).to_le_bytes());
                for u in name.encode_utf16() {
                    o.extend(u.to_le_bytes());
                }
            }
            o
        };
        let mut and_ref = Vec::new();
        and_ref.extend(4u32.to_le_bytes());
        and_ref.extend(5u32.to_le_bytes());
        let mut atrd = vec![0u8; 30];
        atrd[20..22].copy_from_slice(&1u16.to_le_bytes());
        and_ref.extend(atrd);
        let mut and_txt = Vec::new();
        and_txt.extend(0u32.to_le_bytes());
        and_txt.extend(units(atn).to_le_bytes());
        and_txt.extend((units(atn) + 1).to_le_bytes());
        let w = W::new()
            .piece(&[main, ftn, hdd, atn, edn, txbx].concat(), true)
            .ccp(ccp)
            .extra(pair::PLCF_HDD, hdd_plc)
            .extra(pair::GRP_XST_ATN_OWNERS, owners)
            .extra(pair::PLCF_AND_REF, and_ref)
            .extra(pair::PLCF_AND_TXT, and_txt)
            .chp(10, 11, sp1(SPEC, 1))
            .chp(0, 0, vec![]);
        // The reference marks need fSpec; mark every 0x02/0x05 position.
        let all: Vec<u16> = [main, ftn, hdd, atn, edn, txbx].concat().encode_utf16().collect();
        let mut w = w;
        w.chp.clear();
        for (i, u) in all.iter().enumerate() {
            if matches!(u, 2 | 5) {
                w.chp.push((i, i + 1, sp1(SPEC, 1)));
            }
        }
        let p = ok(&w.build());
        assert_eq!(text_of(body(&p)), "Body text");
        let notes: Vec<&crate::model::Section> =
            p.sections.iter().filter(|s| s.kind == SectionKind::Notes).collect();
        assert_eq!(
            notes.len(),
            3,
            "{:?}",
            p.sections.iter().map(|s| (s.kind, &s.name)).collect::<Vec<_>>()
        );
        assert_eq!(text_of(notes[0]), "The footnote");
        assert_eq!(paragraphs(notes[0])[0].marker, "[1]");
        assert_eq!(text_of(notes[1]), "The endnote");
        assert_eq!(paragraphs(notes[1])[0].marker, "[e1]");
        assert_eq!(notes[2].name, "Text boxes");
        assert_eq!(text_of(notes[2]), "BOXED TEXT");
        let c = section_of(&p, SectionKind::Comments).expect("comments");
        let cp = paragraphs(c);
        assert_eq!(cp.len(), 1);
        assert_eq!(cp[0].marker, "[c1]");
        assert_eq!(cp[0].text(), "Alice Reviewer: comment text");
        let h = section_of(&p, SectionKind::Headers).expect("headers");
        assert_eq!(text_of(h), "H1 first\nH1 second\nFOOTER");
        assert!(paragraphs(h).iter().all(|q| q.style == ParaStyle::HeaderFooter));
    }

    #[test]
    fn reference_marks_in_the_text_use_the_reference_tables() {
        let main = "a\x02 b\x02 c\x05 d\r";
        let ftn = "\x02 N1\r";
        let edn = "\x02 E1\r";
        let units = |s: &str| s.encode_utf16().count() as u32;
        let plc = |cp: u32| {
            let mut v = Vec::new();
            v.extend(cp.to_le_bytes());
            v.extend((units(main)).to_le_bytes());
            v.extend([0u8; 2]);
            v
        };
        let mut cm = Vec::new();
        cm.extend(7u32.to_le_bytes());
        cm.extend(9u32.to_le_bytes());
        cm.extend([0u8; 30]);
        let w = W::new()
            .piece(&[main, ftn, edn].concat(), true)
            .ccp([units(main), units(ftn), 0, 0, 0, units(edn), 0, 0])
            .extra(pair::PLCF_FND_REF, plc(4))
            .extra(pair::PLCF_END_REF, plc(1))
            .extra(pair::PLCF_AND_REF, cm)
            .chp(1, 2, sp1(SPEC, 1))
            .chp(4, 5, sp1(SPEC, 1))
            .chp(7, 8, sp1(SPEC, 1));
        let p = ok(&w.build());
        let t = text_of(body(&p));
        // Position 1 is an endnote reference, 4 a footnote, 7 a comment.
        assert_eq!(t, "a[e1] b[1] c[c1] d");
    }

    #[test]
    fn pictures_embedded_objects_and_shapes_are_placeholders() {
        let text = "x\x01y\x08z\r";
        let w = W::new().piece(text, true).chp(1, 2, [sp1(SPEC, 1), sp1(0x080A, 1)].concat()).chp(
            3,
            4,
            sp1(SPEC, 1),
        );
        let p = ok(&w.build());
        let sec = body(&p);
        assert_eq!(text_of(sec), "xyz");
        assert!(sec.blocks.iter().any(|b| matches!(b, Block::Image(_))));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::ImagesNotShown));
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Unsupported && n.detail == "drawings and shapes")
        );
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Unsupported && n.detail == "embedded objects")
        );
    }

    #[test]
    fn symbols_with_a_font_become_a_neutral_box() {
        let w = W::new().piece("a\x28b\r", true).chp(
            1,
            2,
            [sp1(SPEC, 1), sp4(0x6A09, 0xF0B7_0003)].concat(),
        );
        assert_eq!(text_of(body(&ok(&w.build()))), "a□b");
        // Without fSpec it is an ordinary parenthesis.
        let w = W::new().piece("a\x28b\r", true);
        assert_eq!(text_of(body(&ok(&w.build()))), "a(b");
    }

    #[test]
    fn text_outside_the_stream_is_reported_not_fatal() {
        let (mut wd, table) = W::new().piece("0123456789\r", true).streams();
        wd.truncate(0x800 + 4);
        let p = ok(&wrap(wd, table));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Skipped), "{:?}", p.notices);
    }

    #[test]
    fn without_a_piece_table_the_text_is_read_from_fc_min() {
        let mut w = W::new().piece("plain old text\r", false);
        w.no_clx = true;
        let p = ok(&w.build());
        assert_eq!(texts(&p), vec!["plain old text"]);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort));
    }

    #[test]
    fn word_6_documents_are_read_as_text() {
        let text = b"Word six text\rsecond line\r";
        let mut wd = vec![0u8; 0x400];
        wd[0..2].copy_from_slice(&0xA5DCu16.to_le_bytes());
        wd[2..4].copy_from_slice(&101u16.to_le_bytes());
        let fc_min = 0x400u32;
        wd[0x18..0x1C].copy_from_slice(&fc_min.to_le_bytes());
        wd[0x1C..0x20].copy_from_slice(&(fc_min + 26).to_le_bytes());
        wd[0x34..0x38].copy_from_slice(&26u32.to_le_bytes());
        wd.extend(text);
        let mut b = CfbBuilder::new();
        assert!(b.add_stream("WordDocument", wd).is_ok());
        let p = ok(&b.finish().unwrap_or_default());
        assert_eq!(texts(&p), vec!["Word six text", "second line"]);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort));
    }

    #[test]
    fn not_a_word_document_is_malformed() {
        let mut b = CfbBuilder::new();
        let mut wd = vec![0u8; 0x400];
        wd[0] = 0x12;
        assert!(b.add_stream("WordDocument", wd).is_ok());
        assert!(matches!(run(&b.finish().unwrap_or_default()), Err(PreviewError::Malformed(_))));
    }

    #[test]
    fn property_lists_are_walked_safely() {
        // sprmTDefTable (2-byte size), sprmPChgTabs with the computed size, then a bold toggle.
        let mut prl = Vec::new();
        prl.extend(0xD608u16.to_le_bytes());
        prl.extend(5u16.to_le_bytes()); // size - 1 = 5 → 6 bytes including the size
        prl.extend([1u8, 2, 3, 4]);
        prl.extend(0xC615u16.to_le_bytes());
        prl.extend([255u8, 1, 0, 0, 0, 0, 1, 0, 0, 7]); // del = 1, add = 1
        prl.extend(sp1(BOLD, 1));
        let ops: Vec<u16> = Sprms::new(&prl).map(|(op, _)| op).collect();
        assert_eq!(ops, vec![0xD608, 0xC615, BOLD]);
        // Truncated lists stop cleanly.
        for cut in 0..prl.len() {
            let _ = Sprms::new(&prl[..cut]).count();
        }
    }

    #[test]
    fn run_sets_resolve_overlaps_and_gaps() {
        let rs =
            RunSet::build(vec![(10, 20, 1u8), (15, 30, 2), (40, 50, 1), (50, 60, 1), (0, 5, 0)]);
        assert_eq!(rs.at(12).0, 1);
        assert_eq!(rs.at(16).0, 1, "an earlier run wins where runs overlap");
        assert_eq!(rs.at(25).0, 2, "the later run starts where the earlier one ends");
        assert_eq!(rs.at(35), (0, 30..40));
        assert_eq!(rs.at(55).1, 40..60, "adjacent equal runs are merged");
        assert_eq!(rs.at(70).1, 60..u32::MAX);
    }

    #[test]
    fn mac_roman_table_is_complete() {
        assert_eq!(mac_unit(0x41), 0x41);
        assert_eq!(mac_unit(0x80), u16::try_from(u32::from('Ä')).unwrap());
        assert_eq!(mac_unit(0x8E), u16::try_from(u32::from('é')).unwrap());
        assert_eq!(mac_unit(0xFF), u16::try_from(u32::from('ˇ')).unwrap());
        assert_eq!(ansi_unit(0x80), 0x20AC);
        assert_eq!(ansi_unit(0xE9), 0xE9);
    }

    #[test]
    fn huge_claimed_sizes_do_not_allocate() {
        let mut w = W::new().piece("tiny\r", true);
        w.ccp = Some([u32::MAX, u32::MAX, u32::MAX, 0, 0, 0, 0, 0]);
        let r = run(&w.build());
        assert!(r.is_ok(), "{r:?}");
    }

    fn rich_sample() -> W {
        W::new()
            .piece("Heading\rcell\x07cell2\x07\x07\x13 HYPERLINK \"http://a.example\" \x14link\x15 hidden\x01\r", true)
            .piece("Привет мир\r", false)
            .chp(30, 36, sp1(HIDDEN, 1))
            .pap(0, 8, 1, vec![])
            .pap(8, 13, 0, sp1(0x2416, 1))
            .pap(13, 20, 0, [sp1(0x2416, 1), sp1(0x2417, 1)].concat())
    }

    #[test]
    fn mutated_streams_never_panic() {
        let (wd, table) = rich_sample().streams();
        let mut rng = Lcg::new(0x5EED);
        for i in 0..fuzz_iterations(400) {
            let (mut a, mut b) = (wd.clone(), table.clone());
            for _ in 0..1 + (i % 6) {
                let first = rng.below(2) == 0;
                let target = if first { &mut a } else { &mut b };
                // Concentrate on the structures: the FIB, the table stream and the FKP pages.
                let span =
                    if first && rng.below(2) == 0 { 0x400.min(target.len()) } else { target.len() };
                let pos = rng.below(span);
                let byte = rng.byte();
                if let Some(slot) = target.get_mut(pos) {
                    *slot = byte;
                }
            }
            if i % 7 == 0 {
                let cut = rng.below(a.len() + 1);
                a.truncate(cut);
            }
            let _ = run(&wrap(a, b));
        }
    }

    #[test]
    fn mutated_real_documents_never_panic() {
        let Ok(dir) = std::env::var("ARCA_CORPUS_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("report.doc")) else { return };
        let cf = CompoundFile::parse(&data, crate::PreviewPolicy::default().cfb).expect("cfb");
        let wd = cf.read_stream(cf.find("WordDocument").expect("wd")).expect("read");
        let table = cf
            .read_stream(cf.find("1Table").or_else(|| cf.find("0Table")).expect("table"))
            .expect("read");
        let which = if cf.find("1Table").is_some() { "1Table" } else { "0Table" };
        let mut rng = Lcg::new(99);
        for i in 0..fuzz_iterations(300) {
            let (mut a, mut b) = (wd.clone(), table.clone());
            for _ in 0..1 + (i % 8) {
                let target = if rng.below(2) == 0 { &mut a } else { &mut b };
                rng.mutate(target, 1);
            }
            let mut builder = CfbBuilder::new();
            assert!(builder.add_stream("WordDocument", a).is_ok());
            assert!(builder.add_stream(which, b).is_ok());
            let _ = run(&builder.finish().unwrap_or_default());
        }
    }

    #[test]
    fn real_libreoffice_document_keeps_its_structure() {
        let Ok(dir) = std::env::var("ARCA_CORPUS_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("report.doc")) else { return };
        let p = ok(&data);
        let paras = paragraphs(body(&p));
        assert_eq!(
            paras.first().map(|q| (q.style, q.text())),
            Some((ParaStyle::Heading1, "Квартальный отчёт".to_owned()))
        );
        assert!(paras.iter().any(|q| q.marker == "•" && q.text() == "Первый пункт"));
        assert!(paras.iter().any(|q| q.marker == "2." && q.text() == "Шаг два"));
        assert!(paras.iter().any(|q| q.runs.iter().any(|r| r.href == "https://example.org/docs")));
        assert!(body(&p).blocks.iter().any(|b| matches!(b, Block::Table(t) if t.rows.len() == 4)));
    }
}
