//! The look of a legacy XLS sheet (BIFF8): column widths, row heights, cell formats (fonts,
//! fills, borders, alignment), merged cells and the page setup.
//!
//! The text comes from the reading view. This pass walks the records of the workbook stream
//! again for the ones that describe looks. BIFF5 (Excel 5/95) has another cell format layout;
//! for it only sizes and merged cells are read and the formats are left out with a notice.

use arca_container::bytes::{slice, u16_le, u32_le};
use arca_container::cfb::CompoundFile;

use super::model::{
    Borders, Edge, Font, HAlign, Layout, MAX_COL_PT, MAX_COLS, MAX_MERGES, MAX_ROW_PT, MAX_ROWS,
    MAX_RUNS, MAX_STYLE_ITEMS, MAX_STYLED, Merge, RawRun, Styles, VAlign, Wide, Xf, opaque,
    paper_size, sane_font_size, sane_size,
};
use super::xlsx::{INDEXED, col_points};
use super::{Meta, Picked, Read};
use crate::PreviewError;

/// Most records walked in one stream.
const MAX_RECORDS: usize = 32 << 20;
/// Most sheets listed.
const MAX_SHEETS: usize = 4_096;

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const BOUNDSHEET: u16 = 0x0085;
const FONT: u16 = 0x0031;
const XF: u16 = 0x00E0;
const PALETTE: u16 = 0x0092;
const FILEPASS: u16 = 0x002F;
const COLINFO: u16 = 0x007D;
const DEFCOLWIDTH: u16 = 0x0055;
const DEFAULTROWHEIGHT: u16 = 0x0225;
const ROW: u16 = 0x0208;
const MERGEDCELLS: u16 = 0x00E5;
const PAGESETUP: u16 = 0x00A1;
const WSBOOL: u16 = 0x0081;
const WINDOW2: u16 = 0x023E;
const PRINTGRIDLINES: u16 = 0x002B;
const HPAGEBREAKS: u16 = 0x001B;
const VPAGEBREAKS: u16 = 0x001A;
const LEFTMARGIN: u16 = 0x0026;
const RIGHTMARGIN: u16 = 0x0027;
const TOPMARGIN: u16 = 0x0028;
const BOTTOMMARGIN: u16 = 0x0029;
const CF: u16 = 0x01B1;
const OBJ: u16 = 0x005D;
const BLANK: u16 = 0x0201;
const NUMBER: u16 = 0x0203;
const LABEL: u16 = 0x0204;
const BOOLERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
const LABELSST: u16 = 0x00FD;
const RK: u16 = 0x027E;
const RSTRING: u16 = 0x00D6;
const MULBLANK: u16 = 0x00BE;
const MULRK: u16 = 0x00BD;

/// A cursor over the records of a BIFF stream.
struct Recs<'a> {
    data: &'a [u8],
    pos: usize,
    seen: usize,
}

impl<'a> Iterator for Recs<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.seen >= MAX_RECORDS {
            return None;
        }
        let id = u16_le(self.data, self.pos)?;
        let len = usize::from(u16_le(self.data, self.pos.checked_add(2)?)?);
        let body = slice(self.data, self.pos.checked_add(4)?, len)?;
        self.pos = self.pos.checked_add(4)?.checked_add(len)?;
        self.seen += 1;
        Some((id, body))
    }
}

/// The 8-bit or UTF-16 text of a BIFF8 string of `cch` characters that starts at `at`.
fn biff8_text(d: &[u8], at: usize, cch: usize, flags: u8) -> String {
    let bytes = d.get(at..).unwrap_or(&[]);
    if flags & 1 == 1 {
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
}

struct Sheet {
    offset: usize,
}

#[derive(Default)]
struct Globals {
    biff8: bool,
    sheets: Vec<(Sheet, Meta)>,
    fonts: Vec<(Font, u16)>,
    xfs: Vec<[u8; 20]>,
    palette: Vec<u32>,
}

fn read_globals(data: &[u8]) -> Result<Globals, PreviewError> {
    let mut rd = Recs { data, pos: 0, seen: 0 };
    let Some((BOF, first)) = rd.next() else {
        return Err(PreviewError::Malformed("the stream does not start with a BOF record".into()));
    };
    let version = u16_le(first, 0).unwrap_or(0);
    let mut g =
        Globals { biff8: version >= 0x0600, palette: INDEXED.to_vec(), ..Globals::default() };
    if !g.biff8 && version != 0x0500 {
        return Err(PreviewError::Unsupported(crate::model::FileFormat::Xls));
    }
    let mut depth = 1i32;
    for (id, d) in rd {
        match id {
            BOF => depth += 1,
            EOF => {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
            }
            FILEPASS => return Err(PreviewError::Encrypted),
            _ if depth > 1 => {}
            BOUNDSHEET if g.sheets.len() < MAX_SHEETS => {
                let (Some(off), Some(state), Some(kind)) =
                    (u32_le(d, 0), d.get(4).copied(), d.get(5).copied())
                else {
                    continue;
                };
                // Worksheets and Excel 4.0 macro sheets are the ones with cells.
                if kind > 1 {
                    continue;
                }
                let cch = usize::from(d.get(6).copied().unwrap_or(0));
                let name = if g.biff8 {
                    biff8_text(d, 8, cch, d.get(7).copied().unwrap_or(0))
                } else {
                    biff8_text(d, 7, cch, 0)
                };
                g.sheets.push((
                    Sheet { offset: usize::try_from(off).unwrap_or(usize::MAX) },
                    Meta { name, hidden: state & 3 != 0 },
                ));
            }
            FONT if g.fonts.len() < MAX_STYLE_ITEMS => {
                g.fonts.push(font_of(d));
            }
            XF if g.xfs.len() < MAX_STYLE_ITEMS && g.biff8 => {
                let mut x = [0u8; 20];
                for (slot, b) in x.iter_mut().zip(d.iter()) {
                    *slot = *b;
                }
                g.xfs.push(x);
            }
            PALETTE => {
                let n = usize::from(u16_le(d, 0).unwrap_or(0)).min(56);
                for k in 0..n {
                    if let Some(c) = slice(d, 2 + 4 * k, 3)
                        && let [r, gr, b] = c
                        && let Some(slot) = g.palette.get_mut(8 + k)
                    {
                        *slot = (u32::from(*r) << 16) | (u32::from(*gr) << 8) | u32::from(*b);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(g)
}

fn colour(palette: &[u32], icv: u16) -> u32 {
    match icv {
        0x7FFF | 64 => 0x000000,
        65 => 0xFFFFFF,
        i => palette.get(usize::from(i)).copied().unwrap_or(0),
    }
}

fn font_of(d: &[u8]) -> (Font, u16) {
    let size = f64::from(u16_le(d, 0).unwrap_or(220)) / 20.0;
    let flags = u16_le(d, 2).unwrap_or(0);
    let mut font = Font {
        size: sane_font_size(size, 10.0),
        bold: u16_le(d, 6).unwrap_or(400) >= 700,
        italic: flags & 0x02 != 0,
        strike: flags & 0x08 != 0,
        underline: d.get(10).copied().unwrap_or(0) != 0,
        ..Font::default()
    };
    let cch = usize::from(d.get(14).copied().unwrap_or(0));
    let name = biff8_text(d, 16, cch, d.get(15).copied().unwrap_or(0));
    font.set_family(&name);
    (font, u16_le(d, 4).unwrap_or(0x7FFF))
}

/// Border style codes of BIFF to a width and whether the line is only approximated.
fn border_of(code: u16) -> Option<(f32, bool)> {
    Some(match code {
        1 => (0.75, false),
        2 => (1.5, false),
        3 | 4 | 9 | 11 | 13 => (0.75, true),
        5 => (2.25, false),
        6 => (2.25, true),
        7 => (0.25, false),
        8 | 10 | 12 => (1.5, true),
        _ => return None,
    })
}

fn build_styles(g: &Globals) -> Styles {
    // Font index 4 does not exist in the file: the numbers above it are shifted.
    let fonts = g
        .fonts
        .iter()
        .map(|(f, icv)| Font { color: opaque(colour(&g.palette, *icv)), ..*f })
        .collect();
    let mut st = Styles { fonts, ..Styles::default() };
    if st.fonts.is_empty() {
        st.fonts.push(Font::default());
    }
    st.fills.push(0);
    st.borders.push([Edge::default(); 4]);
    for x in &g.xfs {
        let w = |at: usize| u16_le(x, at).unwrap_or(0);
        let ifnt = usize::from(w(0));
        let font_ix = if ifnt >= 5 { ifnt - 1 } else { ifnt };
        let font = if font_ix < st.fonts.len() { u32::try_from(font_ix).unwrap_or(0) } else { 0 };
        let al = w(6);
        let h = match al & 7 {
            1 | 4 | 5 => HAlign::Left,
            2 | 6 => HAlign::Center,
            3 => HAlign::Right,
            _ => HAlign::General,
        };
        let v = match (al >> 4) & 7 {
            0 => VAlign::Top,
            1 | 3 | 4 => VAlign::Center,
            _ => VAlign::Bottom,
        };
        let trot = (al >> 8) & 0xFF;
        let indent = u8::try_from(w(8) & 0x0F).unwrap_or(0);
        // Borders.
        let bs = w(10);
        let cols_lr = w(12);
        let cols_tb = u32_le(x, 14).unwrap_or(0);
        let codes = [bs & 0xF, (bs >> 4) & 0xF, (bs >> 8) & 0xF, (bs >> 12) & 0xF];
        let icvs = [
            cols_lr & 0x7F,
            u16::try_from(cols_tb & 0x7F).unwrap_or(0),
            (cols_lr >> 7) & 0x7F,
            u16::try_from((cols_tb >> 7) & 0x7F).unwrap_or(0),
        ];
        // Left, top, right, bottom.
        let order = [(0usize, 0usize), (2, 1), (1, 2), (3, 3)];
        let mut edges: Borders = [Edge::default(); 4];
        for (slot, (code_ix, icv_ix)) in edges.iter_mut().zip(order) {
            let code = codes.get(code_ix).copied().unwrap_or(0);
            let icv = icvs.get(icv_ix).copied().unwrap_or(0);
            if let Some((width, approx)) = border_of(code) {
                *slot = Edge { width, color: opaque(colour(&g.palette, icv)), approx };
            }
        }
        let has_edges = edges.iter().any(|e| e.width > 0.0);
        // Fill.
        let pattern = (cols_tb >> 26) & 0x3F;
        let pat_cols = w(18);
        let fore = colour(&g.palette, pat_cols & 0x7F);
        let back = colour(&g.palette, (pat_cols >> 7) & 0x7F);
        let fill = match pattern {
            0 => 0,
            1 => opaque(fore),
            _ => {
                let ch = |s: u32| (((fore >> s) & 0xFF) + ((back >> s) & 0xFF)) / 2;
                let id = u32::try_from(st.fills.len()).unwrap_or(0);
                st.approx_fills.push(id);
                opaque((ch(16) << 16) | (ch(8) << 8) | ch(0))
            }
        };
        let fill_id = if fill == 0 { 0 } else { st.push_fill(fill) };
        let border_id = if has_edges { st.push_border(edges) } else { 0 };
        st.push_xf(Xf {
            font,
            fill: fill_id,
            border: border_id,
            h,
            v,
            wrap: al & 0x08 != 0,
            indent,
            rot: trot != 0 && trot != 255,
        });
    }
    st
}

fn read_sheet(data: &[u8], offset: usize, biff8: bool) -> Layout {
    let mut lay =
        Layout { gridlines: true, default_col: 48.0, default_row: 12.75, ..Layout::default() };
    let mut rd = Recs { data, pos: offset, seen: 0 };
    let Some((BOF, _)) = rd.next() else {
        lay.truncated = true;
        return lay;
    };
    let mut depth = 1i32;
    let mut fit_to_page = false;
    let mut fit = (1u32, 1u32);
    let mut frozen = false;
    let mut defcol: Option<u16> = None;
    let mut ended = false;
    for (id, d) in &mut rd {
        match id {
            BOF => {
                depth += 1;
                lay.notes.add("charts and drawings (not drawn)");
                continue;
            }
            EOF => {
                depth -= 1;
                if depth <= 0 {
                    ended = true;
                    break;
                }
                continue;
            }
            _ if depth > 1 => continue,
            _ => {}
        }
        let xf_at = |at: usize| u32::from(u16_le(d, at).unwrap_or(0));
        match id {
            COLINFO => {
                if let (Some(first), Some(last)) = (u16_le(d, 0), u16_le(d, 2)) {
                    let width = f64::from(u16_le(d, 4).unwrap_or(0)) / 256.0;
                    let size = sane_size(col_points(width), MAX_COL_PT);
                    let hidden = u16_le(d, 8).unwrap_or(0) & 1 != 0;
                    let (first, last) = (u32::from(first), u32::from(last).min(MAX_COLS - 1));
                    if first <= last && lay.cols.len() < 20_000 {
                        lay.cols.push(RawRun { first, last, size, hidden });
                        let xf = xf_at(6);
                        if xf != 0 && biff8 {
                            lay.wide.push(Wide {
                                r1: 0,
                                r2: MAX_ROWS - 1,
                                c1: first,
                                c2: last,
                                xf,
                            });
                        }
                    }
                }
            }
            DEFCOLWIDTH => defcol = u16_le(d, 0),
            DEFAULTROWHEIGHT => {
                if let Some(t) = u16_le(d, 2)
                    && let Some(h) = sane_size(f64::from(t) / 20.0, MAX_ROW_PT)
                    && h > 0.0
                {
                    lay.default_row = h;
                }
            }
            ROW => {
                let row = u32::from(u16_le(d, 0).unwrap_or(0));
                let height = u16_le(d, 6).unwrap_or(0x8000);
                let flags = u16_le(d, 12).unwrap_or(0);
                let size = (height & 0x8000 == 0)
                    .then(|| sane_size(f64::from(height) / 20.0, MAX_ROW_PT))
                    .flatten();
                let hidden = flags & 0x20 != 0;
                if (size.is_some() || hidden) && lay.rows.len() < MAX_RUNS {
                    lay.rows.push(RawRun { first: row, last: row, size, hidden });
                }
            }
            MERGEDCELLS => {
                let n = usize::from(u16_le(d, 0).unwrap_or(0));
                for k in 0..n {
                    let at = 2 + 8 * k;
                    let (Some(r1), Some(r2), Some(c1), Some(c2)) =
                        (u16_le(d, at), u16_le(d, at + 2), u16_le(d, at + 4), u16_le(d, at + 6))
                    else {
                        break;
                    };
                    if let Some(m) =
                        Merge::new(u32::from(r1), u32::from(c1), u32::from(r2), u32::from(c2))
                    {
                        if lay.merges.len() < MAX_MERGES {
                            lay.merges.push(m);
                        } else {
                            lay.truncated = true;
                        }
                    }
                }
            }
            PAGESETUP => {
                if let Some(p) = u16_le(d, 0).and_then(|c| paper_size(u32::from(c))) {
                    lay.setup.paper = p;
                }
                if let Some(s) = u16_le(d, 2).filter(|s| (10..=400).contains(s)) {
                    lay.setup.scale = f32::from(s) / 100.0;
                }
                fit = (
                    u32::from(u16_le(d, 6).unwrap_or(1)).min(1_000),
                    u32::from(u16_le(d, 8).unwrap_or(1)).min(1_000),
                );
                let grbit = u16_le(d, 10).unwrap_or(2);
                // Bit 1 set is portrait; the "no printer set up" bit leaves the default.
                lay.setup.landscape = grbit & 0x0002 == 0 && grbit & 0x0004 == 0;
                lay.setup.over_then_down = grbit & 0x0001 != 0;
            }
            WSBOOL => fit_to_page = u16_le(d, 0).unwrap_or(0) & 0x0100 != 0,
            WINDOW2 => {
                let g = u16_le(d, 0).unwrap_or(0);
                lay.gridlines = g & 0x0002 != 0;
                frozen = g & 0x0008 != 0;
            }
            PRINTGRIDLINES => lay.setup.print_grid = u16_le(d, 0).unwrap_or(0) != 0,
            LEFTMARGIN | RIGHTMARGIN | TOPMARGIN | BOTTOMMARGIN => {
                if let Some(b) = slice(d, 0, 8).and_then(|b| <[u8; 8]>::try_from(b).ok()) {
                    let side = match id {
                        LEFTMARGIN => 0,
                        TOPMARGIN => 1,
                        RIGHTMARGIN => 2,
                        _ => 3,
                    };
                    lay.setup.set_margin_inches(side, f64::from_le_bytes(b));
                }
            }
            HPAGEBREAKS | VPAGEBREAKS => {
                let n = usize::from(u16_le(d, 0).unwrap_or(0)).min(1_026);
                for k in 0..n {
                    if let Some(at) = u16_le(d, 2 + 6 * k) {
                        let list = if id == HPAGEBREAKS {
                            &mut lay.setup.row_breaks
                        } else {
                            &mut lay.setup.col_breaks
                        };
                        if list.len() < 2_048 {
                            list.push(u32::from(at));
                        }
                    }
                }
            }
            CF => lay.notes.add("conditional formatting (not drawn)"),
            OBJ => lay.notes.add("charts and drawings (not drawn)"),
            BLANK | NUMBER | LABEL | BOOLERR | FORMULA | LABELSST | RK | RSTRING => {
                if biff8 && lay.styled.len() < MAX_STYLED {
                    let (row, col, xf) = (xf_at(0), xf_at(2), xf_at(4));
                    if xf != 0 && col < MAX_COLS {
                        lay.styled.push((row, col, xf));
                    }
                } else if biff8 {
                    lay.truncated = true;
                }
            }
            MULBLANK => {
                let row = xf_at(0);
                let first = xf_at(2);
                let last = d.len().checked_sub(2).and_then(|at| u16_le(d, at)).map(u32::from);
                if let (true, Some(last)) = (biff8, last) {
                    for (k, col) in (first..=last).enumerate().take(16_384) {
                        let xf = xf_at(4 + 2 * k);
                        if xf != 0 && col < MAX_COLS && lay.styled.len() < MAX_STYLED {
                            lay.styled.push((row, col, xf));
                        }
                    }
                }
            }
            MULRK => {
                let row = xf_at(0);
                let first = xf_at(2);
                let last = d.len().checked_sub(2).and_then(|at| u16_le(d, at)).map(u32::from);
                if let (true, Some(last)) = (biff8, last) {
                    for (k, col) in (first..=last).enumerate().take(16_384) {
                        let xf = xf_at(4 + 6 * k);
                        if xf != 0 && col < MAX_COLS && lay.styled.len() < MAX_STYLED {
                            lay.styled.push((row, col, xf));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !ended {
        lay.truncated = true;
    }
    // Default column width from the number of characters, rounded to whole eight pixels.
    let chars = f64::from(defcol.unwrap_or(8));
    let px = ((chars * 7.0 + 5.0) / 8.0).ceil() * 8.0;
    lay.default_col = sane_size(px * 0.75, MAX_COL_PT).unwrap_or(48.0);
    if fit_to_page {
        lay.setup.fit = Some(fit);
    }
    if frozen {
        lay.notes.add("frozen panes (ignored)");
    }
    lay.setup.row_breaks.sort_unstable();
    lay.setup.row_breaks.dedup();
    lay.setup.col_breaks.sort_unstable();
    lay.setup.col_breaks.dedup();
    lay
}

/// Lists the sheets and reads the look of the one at `want` (counted among the sheets that are
/// shown: hidden ones only when `reveal`).
pub(super) fn read(
    cf: &CompoundFile<'_>,
    want: Option<usize>,
    reveal: bool,
) -> Result<Read, PreviewError> {
    let idx = cf
        .find("Workbook")
        .or_else(|| cf.find("Book"))
        .ok_or_else(|| PreviewError::Malformed("the Workbook stream is missing".into()))?;
    let data =
        cf.read_stream(idx).map_err(|e| PreviewError::Malformed(format!("Excel workbook: {e}")))?;
    let g = read_globals(&data)?;
    let mut picked_at = None;
    let mut shown = 0usize;
    for (i, (_, meta)) in g.sheets.iter().enumerate() {
        if reveal || !meta.hidden {
            if want == Some(shown) {
                picked_at = Some(i);
            }
            shown += 1;
        }
    }
    let metas: Vec<Meta> = g.sheets.iter().map(|(_, m)| m.clone()).collect();
    let Some(index) = picked_at else {
        if want.is_some() {
            return Err(PreviewError::Malformed("the workbook has no such sheet".into()));
        }
        return Ok(Read { metas, picked: None });
    };
    let Some((sheet, _)) = g.sheets.get(index) else { return Ok(Read { metas, picked: None }) };
    let mut styles = if g.biff8 { build_styles(&g) } else { Styles::default() };
    if styles.fonts.is_empty() {
        styles.fonts.push(Font::default());
    }
    let mut layout = read_sheet(&data, sheet.offset, g.biff8);
    if !g.biff8 {
        layout.notes.add("cell formats of Excel 5/95 files (not drawn)");
    }
    Ok(Read { metas, picked: Some(Picked { index, styles, layout }) })
}
