//! What the readers hand to the painter: styles, sizes along the axes, cells, merged ranges,
//! the page setup, and the splitting of the used range into printed pages.
//!
//! Everything a document declares is a *claim* here: a size may be NaN or 1e30, a style index
//! may point nowhere, a merged range may cover the whole sheet. [`sane_size`] and the caps below
//! turn the claims into numbers the painter can use, and every cap that is hit is reported.

use std::collections::HashMap;

use super::text;
use crate::model::{CellKind, Grid};

/// Rows of a worksheet.
pub(super) const MAX_ROWS: u32 = 1_048_576;
/// Columns of a worksheet.
pub(super) const MAX_COLS: u32 = 16_384;
/// Most printed pages one sheet is split into.
pub(super) const MAX_PAGES: usize = 5_000;
/// Most cell records (cells with text, plus empty cells that are painted).
pub(super) const MAX_RECS: usize = 600_000;
/// Most cells with a style that a reader keeps.
pub(super) const MAX_STYLED: usize = 600_000;
/// Most merged ranges kept.
pub(super) const MAX_MERGES: usize = 100_000;
/// Most cells a column, row or repeated-cell style may be spread over.
pub(super) const MAX_EXPAND: usize = 400_000;
/// Most steps spent hiding the cells under merged ranges.
const MAX_COVER_WORK: usize = 8_000_000;
/// Most runs of rows or columns that differ in size.
pub(super) const MAX_RUNS: usize = 1_100_000;
/// Most fonts, fills, borders and cell formats kept.
pub(super) const MAX_STYLE_ITEMS: usize = 100_000;
/// Smallest size of a row or column that is not hidden, in points.
const MIN_SIZE: f32 = 1.0;
/// Widest column, in points (255 characters of the default font).
pub(super) const MAX_COL_PT: f32 = 1_400.0;
/// Tallest row, in points.
pub(super) const MAX_ROW_PT: f32 = 409.5;

/// Space between a cell's edge and its text, in points.
pub(super) const PAD_X: f32 = 2.0;
/// The same above and below.
pub(super) const PAD_Y: f32 = 0.5;
/// Line height as a multiple of the font size.
pub(super) const LINE: f32 = 1.2;

/// A conversion that cannot overflow: the callers keep `v` in range.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn f32_of(v: f64) -> f32 {
    v as f32
}

/// `v` as an index; `0` for a value that is negative or not a number.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(super) fn u32_of(v: f64) -> u32 {
    if v.is_finite() { v.clamp(0.0, f64::from(u32::MAX)) as u32 } else { 0 }
}

/// A number that is finite, from text.
pub(super) fn number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A size in points from a claim of the document: `None` for something that is not a usable
/// length (not a number, negative), `0` for zero (the row or column is not shown), and a value
/// between the smallest and `max` points otherwise.
pub(super) fn sane_size(v: f64, max: f32) -> Option<f32> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    if v == 0.0 {
        return Some(0.0);
    }
    Some(f32_of(v.min(f64::from(max))).max(MIN_SIZE))
}

/// A font size in points the painter can use.
pub(super) fn sane_font_size(v: f64, default: f32) -> f32 {
    if v.is_finite() && v >= 1.0 { f32_of(v.min(409.0)) } else { default }
}

/// An opaque colour from `0xRRGGBB`.
pub(super) const fn opaque(rgb: u32) -> u32 {
    0xFF00_0000 | (rgb & 0x00FF_FFFF)
}

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum HAlign {
    /// By the kind of the content: text left, numbers right, logical values and errors centred.
    #[default]
    General,
    Left,
    Center,
    Right,
}

/// Vertical alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum VAlign {
    Top,
    Center,
    #[default]
    Bottom,
}

/// A cell format: indices into [`Styles`] plus alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Xf {
    pub(super) font: u32,
    pub(super) fill: u32,
    pub(super) border: u32,
    pub(super) h: HAlign,
    pub(super) v: VAlign,
    pub(super) wrap: bool,
    pub(super) indent: u8,
    /// The text is rotated (it is drawn horizontally).
    pub(super) rot: bool,
}

/// A font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Font {
    pub(super) size: f32,
    pub(super) color: u32,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) underline: bool,
    pub(super) strike: bool,
    pub(super) mono: bool,
    pub(super) serif: bool,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            size: 11.0,
            color: 0xFF00_0000,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            mono: false,
            serif: false,
        }
    }
}

impl Font {
    /// Sets the family flags from a font name.
    pub(super) fn set_family(&mut self, name: &str) {
        let n = name.to_ascii_lowercase();
        self.mono = ["courier", "consolas", "mono", "menlo", "fixed", "terminal", "typewriter"]
            .iter()
            .any(|k| n.contains(k));
        self.serif = !self.mono
            && !n.contains("sans")
            && [
                "times", "georgia", "cambria", "garamond", "serif", "palatino", "antiqua",
                "century", "roman",
            ]
            .iter()
            .any(|k| n.contains(k));
    }
}

/// One side of a cell's border: its width in points (0 for none) and colour.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Edge {
    pub(super) width: f32,
    pub(super) color: u32,
    /// Dashed, dotted or double in the document, drawn solid.
    pub(super) approx: bool,
}

/// Left, top, right and bottom edges.
pub(super) type Borders = [Edge; 4];

/// The look of the cells of a workbook.
#[derive(Debug, Clone, Default)]
pub(super) struct Styles {
    pub(super) xfs: Vec<Xf>,
    pub(super) fonts: Vec<Font>,
    /// Fill colours as `0xAARRGGBB`; alpha 0 means no fill.
    pub(super) fills: Vec<u32>,
    pub(super) borders: Vec<Borders>,
    /// More styles were declared than are kept.
    pub(super) over: bool,
    /// Fills (by index) that are only approximations of a pattern or a gradient.
    pub(super) approx_fills: Vec<u32>,
}

impl Styles {
    pub(super) fn xf(&self, i: u32) -> Xf {
        usize::try_from(i).ok().and_then(|i| self.xfs.get(i)).copied().unwrap_or_default()
    }

    pub(super) fn font(&self, i: u32) -> Font {
        usize::try_from(i).ok().and_then(|i| self.fonts.get(i)).copied().unwrap_or_default()
    }

    pub(super) fn fill(&self, i: u32) -> u32 {
        usize::try_from(i).ok().and_then(|i| self.fills.get(i)).copied().unwrap_or(0)
    }

    pub(super) fn edges(&self, i: u32) -> Borders {
        usize::try_from(i).ok().and_then(|i| self.borders.get(i)).copied().unwrap_or_default()
    }

    /// Whether the format paints something of its own (a fill or a border).
    pub(super) fn visible(&self, xf: u32) -> bool {
        let x = self.xf(xf);
        self.fill(x.fill) >> 24 != 0 || self.edges(x.border).iter().any(|e| e.width > 0.0)
    }

    pub(super) fn push_xf(&mut self, x: Xf) -> u32 {
        push_capped(&mut self.xfs, x, &mut self.over)
    }

    pub(super) fn push_font(&mut self, f: Font) -> u32 {
        push_capped(&mut self.fonts, f, &mut self.over)
    }

    pub(super) fn push_fill(&mut self, f: u32) -> u32 {
        push_capped(&mut self.fills, f, &mut self.over)
    }

    pub(super) fn push_border(&mut self, b: Borders) -> u32 {
        push_capped(&mut self.borders, b, &mut self.over)
    }
}

/// Appends and returns the index; at the cap nothing is added, `over` is set and the index is
/// `0` (the default format).
fn push_capped<T>(v: &mut Vec<T>, item: T, over: &mut bool) -> u32 {
    if v.len() >= MAX_STYLE_ITEMS {
        *over = true;
        return 0;
    }
    v.push(item);
    u32::try_from(v.len() - 1).unwrap_or(0)
}

/// A border style name or width reduced to what is drawn: a width in points, and whether the
/// line is dashed, dotted or double in the document (and so only approximated).
pub(super) fn border_look(name: &str) -> Option<(f32, bool)> {
    Some(match name {
        "hair" => (0.25, false),
        "thin" | "single" | "solid" => (0.75, false),
        "medium" => (1.5, false),
        "thick" => (2.25, false),
        "dashed" | "dotted" | "dashDot" | "dashDotDot" | "slantDashDot" => (0.75, true),
        "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" => (1.5, true),
        "double" => (2.25, true),
        _ => return None,
    })
}

/// A run of rows or columns of one size, before it is checked.
#[derive(Debug, Clone, Copy)]
pub(super) struct RawRun {
    pub(super) first: u32,
    pub(super) last: u32,
    /// The size the document gives; `None` for the default.
    pub(super) size: Option<f32>,
    pub(super) hidden: bool,
}

/// The sizes of the rows or the columns: a default, and runs that differ from it.
#[derive(Debug, Clone, Default)]
pub(super) struct Axis {
    pub(super) default: f32,
    /// Disjoint, sorted: first, last (inclusive), size.
    runs: Vec<(u32, u32, f32)>,
    /// `before[k]` is the sum of `(size - default) * length` of the runs before run `k`.
    before: Vec<f64>,
    /// Disjoint, sorted ranges whose size the document gave (such a row is not auto-sized).
    explicit: Vec<(u32, u32)>,
}

impl Axis {
    /// Builds the axis for `count` items. `omit_hidden` gives hidden items no size; the second
    /// value is how many hidden items there are.
    pub(super) fn build(
        default: f32,
        mut raw: Vec<RawRun>,
        omit_hidden: bool,
        count: u32,
    ) -> (Self, u64) {
        raw.sort_by_key(|r| r.first);
        let mut axis = Self { default, ..Self::default() };
        let mut hidden = 0u64;
        let mut next = 0u32;
        for r in raw {
            let first = r.first.max(next);
            let last = r.last.min(count.saturating_sub(1));
            if count == 0 || first > last || r.last < next {
                continue;
            }
            next = last.saturating_add(1);
            if r.hidden {
                hidden = hidden.saturating_add(u64::from(last - first) + 1);
            }
            let size = if r.hidden && omit_hidden { 0.0 } else { r.size.unwrap_or(default) };
            if r.size.is_some() {
                match axis.explicit.last_mut() {
                    Some(e) if e.1.saturating_add(1) == first => e.1 = last,
                    _ => axis.explicit.push((first, last)),
                }
            }
            if (size - default).abs() > 1e-4 {
                axis.runs.push((first, last, size));
            }
        }
        let mut acc = 0.0f64;
        axis.before.push(0.0);
        for &(first, last, size) in &axis.runs {
            acc += f64::from(size - default) * (f64::from(last - first) + 1.0);
            axis.before.push(acc);
        }
        (axis, hidden)
    }

    /// Where item `i` starts, in points from the start of the axis.
    pub(super) fn start(&self, i: u32) -> f64 {
        let idx = self.runs.partition_point(|r| r.1 < i);
        let mut pos =
            f64::from(i) * f64::from(self.default) + self.before.get(idx).copied().unwrap_or(0.0);
        if let Some(&(first, _, size)) = self.runs.get(idx)
            && first < i
        {
            pos += f64::from(size - self.default) * f64::from(i - first);
        }
        pos
    }

    /// The size of item `i`.
    pub(super) fn size(&self, i: u32) -> f32 {
        let idx = self.runs.partition_point(|r| r.1 < i);
        match self.runs.get(idx) {
            Some(&(first, _, size)) if first <= i => size,
            _ => self.default,
        }
    }

    /// Whether the document gave the size of item `i`.
    pub(super) fn is_explicit(&self, i: u32) -> bool {
        let idx = self.explicit.partition_point(|r| r.1 < i);
        matches!(self.explicit.get(idx), Some(&(first, _)) if first <= i)
    }
}

/// A merged range, zero-based and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Merge {
    pub(super) r1: u32,
    pub(super) c1: u32,
    pub(super) r2: u32,
    pub(super) c2: u32,
}

impl Merge {
    /// A merged range if it is one: at least two cells, inside the sheet.
    pub(super) fn new(r1: u32, c1: u32, r2: u32, c2: u32) -> Option<Self> {
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        if r2 >= MAX_ROWS || c2 >= MAX_COLS || (r1 == r2 && c1 == c2) {
            return None;
        }
        Some(Self { r1, c1, r2, c2 })
    }
}

/// A range of cells that has a format whether or not it holds anything: a whole column, a
/// whole row, a repeated cell. It is spread over the used range once that is known.
#[derive(Debug, Clone, Copy)]
pub(super) struct Wide {
    pub(super) r1: u32,
    pub(super) r2: u32,
    pub(super) c1: u32,
    pub(super) c2: u32,
    pub(super) xf: u32,
}

/// How the sheet is printed.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PageSetup {
    /// The paper, portrait: width and height in points.
    pub(super) paper: [f32; 2],
    pub(super) landscape: bool,
    /// Scale as a factor (1 is 100 %).
    pub(super) scale: f32,
    /// Fit to this many pages wide and tall (0 is "as many as needed").
    pub(super) fit: Option<(u32, u32)>,
    /// Left, top, right, bottom margins in points.
    pub(super) margins: [f32; 4],
    pub(super) print_grid: bool,
    pub(super) over_then_down: bool,
    /// Manual breaks: a page starts at this row or column (zero-based, sorted).
    pub(super) row_breaks: Vec<u32>,
    pub(super) col_breaks: Vec<u32>,
}

/// A4.
pub(super) const A4: [f32; 2] = [595.28, 841.89];

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper: A4,
            landscape: false,
            scale: 1.0,
            fit: None,
            margins: [50.4, 54.0, 50.4, 54.0],
            print_grid: false,
            over_then_down: false,
            row_breaks: Vec::new(),
            col_breaks: Vec::new(),
        }
    }
}

impl PageSetup {
    /// Sets the margins from inches, keeping what is usable.
    pub(super) fn set_margin_inches(&mut self, side: usize, inches: f64) {
        if let Some(slot) = self.margins.get_mut(side)
            && inches.is_finite()
            && (0.0..=20.0).contains(&inches)
        {
            *slot = f32_of(inches * 72.0);
        }
    }

    /// Sets the paper from a size in points; sizes that are not usable are ignored.
    pub(super) fn set_paper(&mut self, w: f64, h: f64) {
        if w.is_finite()
            && h.is_finite()
            && (36.0..=14_400.0).contains(&w)
            && (36.0..=14_400.0).contains(&h)
        {
            self.paper = [f32_of(w.min(h)), f32_of(w.max(h))];
        }
    }
}

/// The size of a paper by its code in a spreadsheet file (`paperSize`, BIFF `iPaperSize`), in
/// points, portrait.
pub(super) fn paper_size(code: u32) -> Option<[f32; 2]> {
    let mm = |w: f32, h: f32| [w * 72.0 / 25.4, h * 72.0 / 25.4];
    let inch = |w: f32, h: f32| [w * 72.0, h * 72.0];
    Some(match code {
        1 | 2 => inch(8.5, 11.0),
        3 | 4 => inch(11.0, 17.0),
        5 => inch(8.5, 14.0),
        6 => inch(5.5, 8.5),
        7 => inch(7.25, 10.5),
        8 => mm(297.0, 420.0),
        9 | 10 => mm(210.0, 297.0),
        11 => mm(148.0, 210.0),
        12 => mm(250.0, 353.0),
        13 => mm(182.0, 257.0),
        14 => inch(8.5, 13.0),
        66 => mm(420.0, 594.0),
        _ => return None,
    })
}

/// Facts a reader found that the painter does not draw (or draws approximately); each becomes
/// a notice.
#[derive(Debug, Clone, Default)]
pub(super) struct Notes {
    pub(super) items: Vec<&'static str>,
}

impl Notes {
    pub(super) fn add(&mut self, what: &'static str) {
        if !self.items.contains(&what) && self.items.len() < 32 {
            self.items.push(what);
        }
    }
}

/// What a reader finds out about one sheet.
#[derive(Debug, Clone, Default)]
pub(super) struct Layout {
    pub(super) cols: Vec<RawRun>,
    pub(super) rows: Vec<RawRun>,
    pub(super) default_col: f32,
    pub(super) default_row: f32,
    /// `(row, column, format)` of the cells that have a format of their own, zero-based.
    pub(super) styled: Vec<(u32, u32, u32)>,
    pub(super) wide: Vec<Wide>,
    pub(super) merges: Vec<Merge>,
    pub(super) setup: PageSetup,
    pub(super) gridlines: bool,
    pub(super) notes: Notes,
    pub(super) truncated: bool,
    /// Something was only approximated.
    pub(super) approximate: bool,
}

/// A cell that is painted: it has text, or a format that paints.
#[derive(Debug, Clone)]
pub(super) struct Rec {
    pub(super) row: u32,
    pub(super) col: u32,
    pub(super) xf: u32,
    pub(super) kind: CellKind,
    pub(super) text: String,
    /// Under a merged range, and not its first cell.
    pub(super) covered: bool,
}

/// The records of one row: `recs[from..to]`.
#[derive(Debug, Clone, Copy)]
pub(super) struct RowSpan {
    pub(super) row: u32,
    pub(super) from: usize,
    pub(super) to: usize,
}

/// A sheet ready to be split into pages and painted.
#[derive(Debug, Clone)]
pub(super) struct Sheet {
    pub(super) recs: Vec<Rec>,
    pub(super) rows_idx: Vec<RowSpan>,
    pub(super) merges: Vec<Merge>,
    pub(super) anchors: HashMap<(u32, u32), usize>,
    pub(super) cols: Axis,
    pub(super) rows: Axis,
    pub(super) last_row: u32,
    pub(super) last_col: u32,
    pub(super) setup: PageSetup,
    pub(super) gridlines: bool,
    pub(super) notes: Notes,
    pub(super) truncated: bool,
    pub(super) approximate: bool,
    pub(super) hidden_rows: u64,
    pub(super) hidden_cols: u64,
}

impl Sheet {
    /// The record at `(row, col)`.
    pub(super) fn rec_at(&self, row: u32, col: u32) -> Option<usize> {
        self.recs.binary_search_by_key(&(row, col), |r| (r.row, r.col)).ok()
    }
}

/// Puts the cells of a sheet together: the text from the reading view's grid and the formats
/// from the reader, the range the sheet uses, the merged ranges, and the sizes of rows and
/// columns (rows that wrap text and have no height of their own grow to fit it).
pub(super) fn assemble(layout: Layout, styles: &Styles, grid: Option<Grid>, reveal: bool) -> Sheet {
    let Layout {
        cols,
        rows,
        default_col,
        default_row,
        mut styled,
        wide,
        merges,
        setup,
        gridlines,
        mut notes,
        mut truncated,
        approximate,
    } = layout;
    let mut approximate = approximate;

    // The text of the cells.
    let mut texts: Vec<Rec> = Vec::new();
    if let Some(mut g) = grid {
        truncated |= g.truncated;
        'grid: for row in std::mem::take(&mut g.rows) {
            let Some(r) = row.index.checked_sub(1) else { continue };
            for c in row.cells {
                if texts.len() >= MAX_RECS {
                    truncated = true;
                    break 'grid;
                }
                texts.push(Rec {
                    row: r,
                    col: c.col,
                    xf: 0,
                    kind: c.kind,
                    text: c.text,
                    covered: false,
                });
            }
        }
    }

    // Join them with the formats: the last format given for a cell wins.
    styled.sort_by_key(|s| (s.0, s.1));
    let mut fmts: Vec<(u32, u32, u32)> = Vec::with_capacity(styled.len());
    for s in styled {
        match fmts.last_mut() {
            Some(last) if last.0 == s.0 && last.1 == s.1 => *last = s,
            _ => fmts.push(s),
        }
    }
    let mut recs: Vec<Rec> = Vec::with_capacity(texts.len());
    let empty = |row: u32, col: u32, xf: u32| Rec {
        row,
        col,
        xf,
        kind: CellKind::Text,
        text: String::new(),
        covered: false,
    };
    let mut fi = fmts.iter().peekable();
    for mut t in texts {
        while let Some(&&(r, c, xf)) = fi.peek()
            && (r, c) < (t.row, t.col)
        {
            fi.next();
            if styles.visible(xf) && recs.len() < MAX_RECS {
                recs.push(empty(r, c, xf));
            }
        }
        if let Some(&&(r, c, xf)) = fi.peek()
            && (r, c) == (t.row, t.col)
        {
            t.xf = xf;
            fi.next();
        }
        recs.push(t);
    }
    for &(r, c, xf) in fi {
        if styles.visible(xf) && recs.len() < MAX_RECS {
            recs.push(empty(r, c, xf));
        }
    }

    // The used range.
    let mut merges: Vec<Merge> = merges;
    merges.sort_by_key(|m| (m.r1, m.c1));
    let mut last_row = 0u32;
    let mut last_col = 0u32;
    for r in &recs {
        last_row = last_row.max(r.row);
        last_col = last_col.max(r.col);
    }
    for m in &merges {
        last_row = last_row.max(m.r2);
        last_col = last_col.max(m.c2);
    }
    last_row = last_row.min(MAX_ROWS - 1);
    last_col = last_col.min(MAX_COLS - 1);

    // Formats of whole rows and columns and of repeated cells, over the used range only.
    if !wide.is_empty() {
        let mut spread: HashMap<(u32, u32), u32> = HashMap::new();
        let mut steps = 0usize;
        'wide: for w in &wide {
            for r in w.r1..=w.r2.min(last_row) {
                for c in w.c1..=w.c2.min(last_col) {
                    steps += 1;
                    if steps > MAX_EXPAND {
                        truncated = true;
                        break 'wide;
                    }
                    spread.insert((r, c), w.xf);
                }
            }
        }
        let mut added: Vec<Rec> = Vec::new();
        for ((r, c), xf) in spread {
            let has = recs.binary_search_by_key(&(r, c), |x| (x.row, x.col)).is_ok();
            if !has && styles.visible(xf) && recs.len() + added.len() < MAX_RECS {
                added.push(empty(r, c, xf));
            }
        }
        recs.extend(added);
        recs.sort_by_key(|r| (r.row, r.col));
    }

    // Where each row's records are.
    let mut rows_idx: Vec<RowSpan> = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        match rows_idx.last_mut() {
            Some(s) if s.row == r.row => s.to = i + 1,
            _ => rows_idx.push(RowSpan { row: r.row, from: i, to: i + 1 }),
        }
    }

    // What the cells use that is only approximated.
    for r in &recs {
        let x = styles.xf(r.xf);
        if x.rot && !r.text.is_empty() {
            notes.add("rotated text (drawn horizontally)");
        }
        if styles.edges(x.border).iter().any(|e| e.approx && e.width > 0.0) {
            notes.add("dashed, dotted and double borders (drawn solid)");
        }
        if styles.approx_fills.binary_search(&x.fill).is_ok() {
            notes.add("cell patterns and gradients (drawn as flat colour)");
        }
    }
    if styles.over {
        notes.add("more cell formats than are kept");
    }

    // Merged ranges: the cells under them are not shown. The work is bounded, whatever the
    // file declares.
    let mut anchors: HashMap<(u32, u32), usize> = HashMap::new();
    let mut covered: Vec<usize> = Vec::new();
    let mut work = 0usize;
    for (k, m) in merges.iter().enumerate() {
        anchors.entry((m.r1, m.c1)).or_insert(k);
        let mut si = rows_idx.partition_point(|s| s.row < m.r1);
        while let Some(span) = rows_idx.get(si) {
            if span.row > m.r2 || work > MAX_COVER_WORK {
                break;
            }
            si += 1;
            let Some(slice) = recs.get(span.from..span.to) else { continue };
            let first = slice.partition_point(|r| r.col < m.c1);
            work = work.saturating_add(1);
            for (off, r) in slice.iter().enumerate().skip(first) {
                if r.col > m.c2 {
                    break;
                }
                work = work.saturating_add(1);
                if !(r.row == m.r1 && r.col == m.c1) {
                    covered.push(span.from + off);
                }
            }
        }
        if work > MAX_COVER_WORK {
            approximate = true;
            break;
        }
    }
    for i in covered {
        if let Some(r) = recs.get_mut(i) {
            r.covered = true;
        }
    }

    // Sizes.
    let count_c = last_col.saturating_add(1);
    let count_r = last_row.saturating_add(1);
    let default_col = if default_col > 0.0 { default_col } else { 48.0 };
    let default_row = if default_row > 0.0 { default_row } else { 15.0 };
    let (cols_axis, hidden_cols) = Axis::build(default_col, cols, !reveal, count_c);
    let (rows_axis, hidden_rows) = Axis::build(default_row, rows.clone(), !reveal, count_r);

    // Rows that wrap text and have no height of their own are as tall as their text needs.
    let mut rows_raw = rows;
    let mut need: HashMap<u32, f32> = HashMap::new();
    for r in &recs {
        if r.text.is_empty() || r.covered || rows_axis.is_explicit(r.row) {
            continue;
        }
        let x = styles.xf(r.xf);
        if !x.wrap || anchors.contains_key(&(r.row, r.col)) {
            continue;
        }
        let font = styles.font(x.font);
        let w = cols_axis.size(r.col) - 2.0 * PAD_X;
        let (lines, _) = text::wrap(&r.text, w, &font);
        let h = f32_of(lines.len() as f64) * font.size * LINE + 2.0 * PAD_Y;
        if h > default_row {
            let slot = need.entry(r.row).or_insert(0.0);
            *slot = slot.max(h.min(MAX_ROW_PT));
        }
    }
    let rows_axis = if need.is_empty() {
        rows_axis
    } else {
        for (row, h) in need {
            if rows_raw.len() < MAX_RUNS {
                rows_raw.push(RawRun { first: row, last: row, size: Some(h), hidden: false });
            }
        }
        Axis::build(default_row, rows_raw, !reveal, count_r).0
    };

    Sheet {
        recs,
        rows_idx,
        merges,
        anchors,
        cols: cols_axis,
        rows: rows_axis,
        last_row,
        last_col,
        setup,
        gridlines,
        notes,
        truncated,
        approximate,
        hidden_rows,
        hidden_cols,
    }
}

/// How a sheet is split into pages.
#[derive(Debug, Clone)]
pub(super) struct Pages {
    /// The page, in points.
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) margins: [f32; 4],
    pub(super) scale: f64,
    /// The room between the margins, in points on paper.
    pub(super) avail_w: f64,
    pub(super) avail_h: f64,
    pub(super) over_then_down: bool,
    /// First column and row of every band, and one past the last column and row covered.
    pub(super) col_starts: Vec<u32>,
    pub(super) col_end: u32,
    pub(super) row_starts: Vec<u32>,
    pub(super) row_end: u32,
    pub(super) count: usize,
    pub(super) truncated: bool,
}

/// Splits one axis into bands that each fit in `avail` points.
fn bands(axis: &Axis, last: u32, avail: f64, breaks: &[u32]) -> (Vec<u32>, u32, bool) {
    let mut starts = vec![0u32];
    let mut acc = 0.0f64;
    let mut i = 0u32;
    while i <= last {
        let s = f64::from(axis.size(i));
        let forced = i > 0 && breaks.binary_search(&i).is_ok();
        if i > 0 && (forced || (acc > 0.0 && acc + s > avail + 1e-6)) {
            if starts.len() >= MAX_PAGES {
                return (starts, i, true);
            }
            starts.push(i);
            acc = 0.0;
        }
        acc += s;
        i += 1;
    }
    (starts, last.saturating_add(1), false)
}

/// Splits the used range of `sheet` into printed pages.
pub(super) fn paginate(sheet: &Sheet) -> Pages {
    let s = &sheet.setup;
    let [mut w, mut h] = s.paper;
    if s.landscape == (w < h) {
        std::mem::swap(&mut w, &mut h);
    }
    let [ml, mt, mr, mb] = s.margins;
    let aw = f64::from((w - ml - mr).max(72.0));
    let ah = f64::from((h - mt - mb).max(72.0));
    let total_w = sheet.cols.start(sheet.last_col.saturating_add(1));
    let total_h = sheet.rows.start(sheet.last_row.saturating_add(1));
    let scale = match s.fit {
        Some((fw, fh)) => {
            let mut k = 1.0f64;
            if fw > 0 && total_w > 0.0 {
                k = k.min(aw * f64::from(fw) / total_w);
            }
            if fh > 0 && total_h > 0.0 {
                k = k.min(ah * f64::from(fh) / total_h);
            }
            k.clamp(0.1, 1.0)
        }
        None => f64::from(s.scale).clamp(0.1, 4.0),
    };
    let (col_starts, col_end, ct) = bands(&sheet.cols, sheet.last_col, aw / scale, &s.col_breaks);
    let (row_starts, row_end, rt) = bands(&sheet.rows, sheet.last_row, ah / scale, &s.row_breaks);
    let all = col_starts.len().saturating_mul(row_starts.len());
    Pages {
        width: w,
        height: h,
        margins: [ml, mt, mr, mb],
        scale,
        avail_w: aw,
        avail_h: ah,
        over_then_down: s.over_then_down,
        col_starts,
        col_end,
        row_starts,
        row_end,
        count: all.min(MAX_PAGES),
        truncated: ct || rt || all > MAX_PAGES,
    }
}

impl Pages {
    /// The bands (column band, row band) of page `p`.
    pub(super) fn band_of(&self, p: usize) -> (usize, usize) {
        let (nc, nr) = (self.col_starts.len().max(1), self.row_starts.len().max(1));
        if self.over_then_down { (p % nc, p / nc) } else { (p / nr, p % nr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(v: &[(u32, u32, Option<f32>, bool)]) -> Vec<RawRun> {
        v.iter().map(|&(first, last, size, hidden)| RawRun { first, last, size, hidden }).collect()
    }

    #[test]
    fn an_axis_adds_up_its_runs() {
        let (a, hidden) =
            Axis::build(10.0, runs(&[(2, 3, Some(30.0), false), (5, 5, None, true)]), true, 10);
        assert_eq!(hidden, 1);
        assert!((a.start(0)).abs() < 1e-9);
        assert!((a.start(2) - 20.0).abs() < 1e-9);
        assert!((a.start(3) - 50.0).abs() < 1e-9);
        assert!((a.start(4) - 80.0).abs() < 1e-9);
        assert!((a.start(6) - 90.0).abs() < 1e-9, "the hidden item 5 has no size");
        assert!((a.start(10) - 130.0).abs() < 1e-9);
        assert!((a.size(2) - 30.0).abs() < 1e-6);
        assert!(a.size(5).abs() < 1e-6);
        assert!((a.size(9) - 10.0).abs() < 1e-6);
        assert!(a.is_explicit(3) && !a.is_explicit(4));
        // Shown hidden items keep the default size.
        let (a, _) = Axis::build(10.0, runs(&[(5, 5, None, true)]), false, 10);
        assert!((a.start(10) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn overlapping_and_absurd_runs_do_not_break_the_axis() {
        let raw = runs(&[
            (0, 4_000_000_000, Some(5.0), false),
            (3, 7, Some(99.0), false),
            (9, 2, Some(1.0), false),
        ]);
        let (a, _) = Axis::build(10.0, raw, true, 20);
        assert!((a.start(20) - 100.0).abs() < 1e-9);
        assert!((a.size(3) - 5.0).abs() < 1e-6);
        let (a, _) = Axis::build(10.0, Vec::new(), true, 0);
        assert!(a.start(0).abs() < 1e-9);
    }

    #[test]
    fn sizes_are_made_sane() {
        assert_eq!(sane_size(f64::NAN, 100.0), None);
        assert_eq!(sane_size(-3.0, 100.0), None);
        assert_eq!(sane_size(f64::INFINITY, 100.0), None);
        assert_eq!(sane_size(0.0, 100.0), Some(0.0));
        assert_eq!(sane_size(1e30, 100.0), Some(100.0));
        assert_eq!(sane_size(0.01, 100.0), Some(1.0));
        assert_eq!(sane_size(12.5, 100.0), Some(12.5));
        assert!((sane_font_size(f64::NAN, 11.0) - 11.0).abs() < f32::EPSILON);
        assert!((sane_font_size(1e9, 11.0) - 409.0).abs() < f32::EPSILON);
    }

    #[test]
    fn merges_are_checked() {
        assert!(Merge::new(0, 0, 0, 0).is_none());
        assert_eq!(Merge::new(3, 3, 1, 1), Some(Merge { r1: 1, c1: 1, r2: 3, c2: 3 }));
        assert!(Merge::new(0, 0, MAX_ROWS, 1).is_none());
        assert!(Merge::new(0, 0, 1, MAX_COLS).is_none());
        assert!(Merge::new(0, 0, MAX_ROWS - 1, MAX_COLS - 1).is_some());
    }
}
