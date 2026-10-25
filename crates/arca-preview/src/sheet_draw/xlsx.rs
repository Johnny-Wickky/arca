//! The look of an XLSX worksheet: sizes, formats, merged ranges and the page setup.
//!
//! The text of the cells is not read here (it comes from the reading view, so the page and the
//! text preview always agree); this reads `styles.xml`, the theme, the workbook's sheet list
//! and the one worksheet that is drawn.

use super::model::{
    Borders, Edge, Font, HAlign, Layout, MAX_COL_PT, MAX_COLS, MAX_MERGES, MAX_ROW_PT, MAX_ROWS,
    MAX_RUNS, MAX_STYLE_ITEMS, MAX_STYLED, Merge, RawRun, Styles, VAlign, Wide, Xf, border_look,
    f32_of, number, opaque, paper_size, sane_font_size, sane_size, u32_of,
};
use super::{Meta, Picked, Read};
use crate::PreviewError;
use crate::opc::Package;
use crate::xml::{self, Flow, Handler, Tag, XmlError};

/// Most sheets listed.
const MAX_SHEETS: usize = 4_096;
/// Most `<col>` elements read.
const MAX_COL_ELEMENTS: usize = 16_384;

/// Points of a column `width` as the file gives it (characters of the default font plus
/// padding): seven pixels per character of Calibri 11, 0.75 point per pixel.
pub(super) fn col_points(width: f64) -> f64 {
    (width * 7.0).round() * 0.75
}

/// `AB12` to (zero-based column, zero-based row).
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let r = r.trim().trim_start_matches('$');
    let letters = r.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 || letters > 3 {
        return None;
    }
    let mut col = 0u32;
    for b in r.bytes().take(letters) {
        col = col.checked_mul(26)?.checked_add(u32::from(b.to_ascii_uppercase() - b'A' + 1))?;
    }
    let digits = r.get(letters..)?.trim_start_matches('$');
    let row: u32 = digits.parse().ok()?;
    Some((col.checked_sub(1)?, row.checked_sub(1)?))
}

/// `A1:B2` (or `A1`) to a merged range.
fn range_ref(r: &str) -> Option<Merge> {
    let (a, b) = r.split_once(':')?;
    let (c1, r1) = cell_ref(a)?;
    let (c2, r2) = cell_ref(b)?;
    Merge::new(r1, c1, r2, c2)
}

fn on(tag: &Tag<'_>, name: &str) -> bool {
    tag.attr(name).is_some_and(|v| matches!(&*v, "1" | "true"))
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// A colour as a file states it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) enum Col {
    #[default]
    None,
    Rgb(u32),
    Theme(u32, f64),
    Indexed(u32, f64),
    Auto,
}

pub(super) fn color_of(tag: &Tag<'_>) -> Col {
    let tint = tag.attr("tint").as_deref().and_then(number).unwrap_or(0.0);
    if let Some(v) = tag.attr("rgb") {
        let hex = v.trim().trim_start_matches('#');
        if matches!(hex.len(), 6 | 8)
            && let Ok(n) = u32::from_str_radix(hex, 16)
        {
            return Col::Rgb(n & 0x00FF_FFFF);
        }
        return Col::None;
    }
    if let Some(t) = tag.attr("theme").and_then(|v| v.trim().parse::<u32>().ok()) {
        return Col::Theme(t, tint);
    }
    if let Some(i) = tag.attr("indexed").and_then(|v| v.trim().parse::<u32>().ok()) {
        return Col::Indexed(i, tint);
    }
    if on(tag, "auto") { Col::Auto } else { Col::None }
}

/// The standard 64-colour palette of Excel (indices 0 to 63).
pub(super) const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
    0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
    0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
    0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
    0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
    0x333333,
];

/// Theme and palette colours of a workbook.
#[derive(Debug, Clone)]
pub(super) struct Palette {
    /// Excel's numbering: light 1, dark 1, light 2, dark 2, six accents, two link colours.
    pub(super) theme: [u32; 12],
    pub(super) custom: Vec<u32>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            theme: [
                0xFFFFFF, 0x000000, 0xE7E6E6, 0x44546A, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000,
                0x5B9BD5, 0x70AD47, 0x0563C1, 0x954F72,
            ],
            custom: Vec::new(),
        }
    }
}

/// Lightens or darkens `rgb` the way a spreadsheet applies a tint (in HLS space).
pub(super) fn tinted(rgb: u32, tint: f64) -> u32 {
    if !tint.is_finite() || tint == 0.0 {
        return rgb;
    }
    let t = tint.clamp(-1.0, 1.0);
    let ch = |shift: u32| f64::from((rgb >> shift) & 0xFF) / 255.0;
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h / 6.0, s)
    };
    let l = if t < 0.0 { l * (1.0 + t) } else { l * (1.0 - t) + t };
    let hue = |p: f64, q: f64, mut x: f64| {
        x = x.rem_euclid(1.0);
        if x < 1.0 / 6.0 {
            p + (q - p) * 6.0 * x
        } else if x < 0.5 {
            q
        } else if x < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - x) * 6.0
        } else {
            p
        }
    };
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (hue(p, q, h + 1.0 / 3.0), hue(p, q, h), hue(p, q, h - 1.0 / 3.0))
    };
    let byte = |v: f64| u32_of((v * 255.0).round().clamp(0.0, 255.0));
    (byte(r) << 16) | (byte(g) << 8) | byte(b)
}

impl Palette {
    /// The colour as `0xAARRGGBB`; `None` for a colour that is not stated.
    pub(super) fn resolve(&self, c: Col, auto: u32) -> Option<u32> {
        Some(opaque(match c {
            Col::None => return None,
            Col::Auto => auto,
            Col::Rgb(v) => v,
            Col::Theme(i, t) => {
                let base = usize::try_from(i).ok().and_then(|i| self.theme.get(i)).copied()?;
                tinted(base, t)
            }
            Col::Indexed(i, t) => {
                let idx = usize::try_from(i).ok()?;
                let base = match self.custom.get(idx) {
                    Some(&c) => c,
                    None => match idx {
                        64 => 0x000000,
                        65 => 0xFFFFFF,
                        _ => INDEXED.get(idx).copied()?,
                    },
                };
                tinted(base, t)
            }
        }))
    }
}

// ---------------------------------------------------------------------------
// theme1.xml
// ---------------------------------------------------------------------------

/// Reads the colour scheme. A colour that is given by reference to another scheme colour (which
/// the format does not allow) is ignored, so there is nothing to follow and nothing to loop on.
struct ThemeH {
    slots: [Option<u32>; 12],
    in_scheme: bool,
    cur: Option<usize>,
}

impl Handler for ThemeH {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "clrScheme" => self.in_scheme = true,
            name if self.in_scheme && self.cur.is_none() => {
                self.cur = match name {
                    "lt1" => Some(0),
                    "dk1" => Some(1),
                    "lt2" => Some(2),
                    "dk2" => Some(3),
                    "accent1" => Some(4),
                    "accent2" => Some(5),
                    "accent3" => Some(6),
                    "accent4" => Some(7),
                    "accent5" => Some(8),
                    "accent6" => Some(9),
                    "hlink" => Some(10),
                    "folHlink" => Some(11),
                    _ => None,
                };
            }
            "srgbClr" | "sysClr" if self.cur.is_some() => {
                let v = tag.attr("val").filter(|_| tag.local() == "srgbClr");
                let v = v.or_else(|| tag.attr("lastClr"));
                if let (Some(slot), Some(v)) = (self.cur.and_then(|i| self.slots.get_mut(i)), v)
                    && v.len() == 6
                    && let Ok(n) = u32::from_str_radix(&v, 16)
                {
                    *slot = Some(n);
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "clrScheme" => self.in_scheme = false,
            "lt1" | "dk1" | "lt2" | "dk2" | "accent1" | "accent2" | "accent3" | "accent4"
            | "accent5" | "accent6" | "hlink" | "folHlink" => self.cur = None,
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// styles.xml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct FontRaw {
    size: Option<f64>,
    color: Col,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    name: String,
}

#[derive(Debug, Clone, Default)]
struct FillRaw {
    pattern: String,
    fg: Col,
    bg: Col,
    gradient: bool,
}

#[derive(Debug, Clone, Default)]
struct BorderRaw {
    sides: [(String, Col); 4],
}

#[derive(Debug, Clone, Copy, Default)]
struct XfRaw {
    font: u32,
    fill: u32,
    border: u32,
    h: HAlign,
    v: VAlign,
    wrap: bool,
    indent: u8,
    rot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Sec {
    #[default]
    None,
    Fonts,
    Fills,
    Borders,
    CellXfs,
    Indexed,
}

#[derive(Default)]
struct StyleH {
    sec: Sec,
    fonts: Vec<FontRaw>,
    fills: Vec<FillRaw>,
    borders: Vec<BorderRaw>,
    xfs: Vec<XfRaw>,
    indexed: Vec<u32>,
    font: Option<FontRaw>,
    fill: Option<FillRaw>,
    border: Option<BorderRaw>,
    side: Option<usize>,
    in_pattern: bool,
    over: bool,
}

impl Handler for StyleH {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            // Differential formats, named styles and extensions are not cell formats.
            "dxfs" | "cellStyleXfs" | "cellStyles" | "tableStyles" | "extLst" | "numFmts" => {
                return Flow::SkipChildren;
            }
            "fonts" => self.sec = Sec::Fonts,
            "fills" => self.sec = Sec::Fills,
            "borders" => self.sec = Sec::Borders,
            "cellXfs" => self.sec = Sec::CellXfs,
            "indexedColors" => self.sec = Sec::Indexed,
            "rgbColor" if self.sec == Sec::Indexed => {
                let v = tag.attr("rgb").and_then(|v| {
                    let hex = v.trim();
                    (matches!(hex.len(), 6 | 8))
                        .then(|| u32::from_str_radix(hex, 16).ok())
                        .flatten()
                });
                if self.indexed.len() < 256 {
                    self.indexed.push(v.unwrap_or(0) & 0x00FF_FFFF);
                }
            }
            "font" if self.sec == Sec::Fonts => self.font = Some(FontRaw::default()),
            "sz" => {
                if let Some(f) = self.font.as_mut() {
                    f.size = tag.attr("val").as_deref().and_then(number);
                }
            }
            "b" => {
                if let Some(f) = self.font.as_mut() {
                    f.bold = tag.flag("val");
                }
            }
            "i" => {
                if let Some(f) = self.font.as_mut() {
                    f.italic = tag.flag("val");
                }
            }
            "strike" => {
                if let Some(f) = self.font.as_mut() {
                    f.strike = tag.flag("val");
                }
            }
            "u" => {
                if let Some(f) = self.font.as_mut() {
                    f.underline = !matches!(tag.attr("val").as_deref(), Some("none"));
                }
            }
            "name" => {
                if let Some(f) = self.font.as_mut() {
                    f.name = tag.attr("val").unwrap_or_default().into_owned();
                }
            }
            "color" => {
                let c = color_of(tag);
                if let Some(f) = self.font.as_mut() {
                    f.color = c;
                } else if let (Some(b), Some(side)) = (self.border.as_mut(), self.side)
                    && let Some(slot) = b.sides.get_mut(side)
                {
                    slot.1 = c;
                } else if let Some(f) = self.fill.as_mut()
                    && f.gradient
                    && f.fg == Col::None
                {
                    f.fg = c;
                }
            }
            "fill" if self.sec == Sec::Fills => self.fill = Some(FillRaw::default()),
            "patternFill" => {
                self.in_pattern = true;
                if let Some(f) = self.fill.as_mut() {
                    f.pattern = tag.attr("patternType").unwrap_or_default().into_owned();
                }
            }
            "gradientFill" => {
                if let Some(f) = self.fill.as_mut() {
                    f.gradient = true;
                }
            }
            "fgColor" if self.in_pattern => {
                let c = color_of(tag);
                if let Some(f) = self.fill.as_mut() {
                    f.fg = c;
                }
            }
            "bgColor" if self.in_pattern => {
                let c = color_of(tag);
                if let Some(f) = self.fill.as_mut() {
                    f.bg = c;
                }
            }
            "border" if self.sec == Sec::Borders => self.border = Some(BorderRaw::default()),
            "left" | "start" | "top" | "right" | "end" | "bottom" if self.border.is_some() => {
                let side = match tag.local() {
                    "left" | "start" => 0,
                    "top" => 1,
                    "right" | "end" => 2,
                    _ => 3,
                };
                self.side = Some(side);
                if let Some(b) = self.border.as_mut()
                    && let Some(slot) = b.sides.get_mut(side)
                {
                    slot.0 = tag.attr("style").unwrap_or_default().into_owned();
                }
            }
            "xf" if self.sec == Sec::CellXfs => {
                if self.xfs.len() >= MAX_STYLE_ITEMS {
                    self.over = true;
                } else {
                    let id = |n: &str| tag.attr(n).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    self.xfs.push(XfRaw {
                        font: id("fontId"),
                        fill: id("fillId"),
                        border: id("borderId"),
                        ..XfRaw::default()
                    });
                }
            }
            "alignment" if self.sec == Sec::CellXfs => {
                if let Some(x) = self.xfs.last_mut() {
                    x.h = match tag.attr("horizontal").as_deref() {
                        Some("left" | "justify" | "distributed" | "fill") => HAlign::Left,
                        Some("center" | "centerContinuous") => HAlign::Center,
                        Some("right") => HAlign::Right,
                        _ => HAlign::General,
                    };
                    x.v = match tag.attr("vertical").as_deref() {
                        Some("top") => VAlign::Top,
                        Some("center" | "distributed" | "justify") => VAlign::Center,
                        _ => VAlign::Bottom,
                    };
                    x.wrap = on(tag, "wrapText");
                    x.indent = tag
                        .attr("indent")
                        .and_then(|v| v.trim().parse::<u8>().ok())
                        .map_or(0, |v| v.min(15));
                    x.rot = tag
                        .attr("textRotation")
                        .and_then(|v| v.trim().parse::<u32>().ok())
                        .is_some_and(|v| v != 0 && v != 255);
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "fonts" | "fills" | "borders" | "cellXfs" | "indexedColors" => self.sec = Sec::None,
            "font" => {
                if let Some(f) = self.font.take() {
                    if self.fonts.len() < MAX_STYLE_ITEMS {
                        self.fonts.push(f);
                    } else {
                        self.over = true;
                    }
                }
            }
            "patternFill" => self.in_pattern = false,
            "fill" => {
                if let Some(f) = self.fill.take() {
                    if self.fills.len() < MAX_STYLE_ITEMS {
                        self.fills.push(f);
                    } else {
                        self.over = true;
                    }
                }
            }
            "left" | "start" | "top" | "right" | "end" | "bottom" => self.side = None,
            "border" => {
                if let Some(b) = self.border.take() {
                    if self.borders.len() < MAX_STYLE_ITEMS {
                        self.borders.push(b);
                    } else {
                        self.over = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

/// Blends two colours (`0xRRGGBB`) half and half.
fn mix(a: u32, b: u32) -> u32 {
    let ch = |shift: u32| (((a >> shift) & 0xFF) + ((b >> shift) & 0xFF)) / 2;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

fn build_styles(h: StyleH, pal: &Palette) -> Styles {
    let mut st = Styles { over: h.over, ..Styles::default() };
    // Index 0 of every list is what an unknown index falls back to.
    for f in h.fonts {
        let mut font = Font {
            size: sane_font_size(f.size.unwrap_or(11.0), 11.0),
            color: pal.resolve(f.color, 0x000000).unwrap_or(0xFF00_0000),
            bold: f.bold,
            italic: f.italic,
            underline: f.underline,
            strike: f.strike,
            ..Font::default()
        };
        font.set_family(&f.name);
        st.push_font(font);
    }
    if st.fonts.is_empty() {
        st.fonts.push(Font::default());
    }
    for (i, f) in h.fills.into_iter().enumerate() {
        let fg = pal.resolve(f.fg, 0x000000);
        let bg = pal.resolve(f.bg, 0xFFFFFF);
        let color = if f.gradient {
            fg.map_or(0, |c| {
                st.approx_fills.push(u32::try_from(i).unwrap_or(0));
                c
            })
        } else {
            match f.pattern.as_str() {
                "solid" => fg.or(bg).unwrap_or(0),
                "" | "none" => 0,
                // The reserved fill of index 1 is never painted.
                _ if i == 1 => 0,
                _ => {
                    st.approx_fills.push(u32::try_from(i).unwrap_or(0));
                    let a = fg.unwrap_or(0xFF00_0000);
                    let b = bg.unwrap_or(0xFFFF_FFFF);
                    opaque(mix(a, b))
                }
            }
        };
        st.push_fill(color);
    }
    if st.fills.is_empty() {
        st.fills.push(0);
    }
    for b in h.borders {
        let mut edges: Borders = [Edge::default(); 4];
        for (slot, (style, col)) in edges.iter_mut().zip(b.sides.iter()) {
            if let Some((width, approx)) = border_look(style) {
                *slot = Edge {
                    width,
                    color: pal.resolve(*col, 0x000000).unwrap_or(0xFF00_0000),
                    approx,
                };
            }
        }
        st.push_border(edges);
    }
    if st.borders.is_empty() {
        st.borders.push([Edge::default(); 4]);
    }
    let (nf, nl, nb) = (st.fonts.len(), st.fills.len(), st.borders.len());
    let valid = |v: u32, n: usize| if usize::try_from(v).is_ok_and(|v| v < n) { v } else { 0 };
    for x in h.xfs {
        st.push_xf(Xf {
            font: valid(x.font, nf),
            fill: valid(x.fill, nl),
            border: valid(x.border, nb),
            h: x.h,
            v: x.v,
            wrap: x.wrap,
            indent: x.indent,
            rot: x.rot,
        });
    }
    st
}

// ---------------------------------------------------------------------------
// workbook.xml
// ---------------------------------------------------------------------------

struct WbSheet {
    name: String,
    hidden: bool,
    rid: String,
}

#[derive(Default)]
struct WbH {
    sheets: Vec<WbSheet>,
    /// `localSheetId`s that have print titles and a print area.
    titles: Vec<u32>,
    areas: Vec<u32>,
}

impl Handler for WbH {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "sheet" if self.sheets.len() < MAX_SHEETS => self.sheets.push(WbSheet {
                name: tag.attr("name").unwrap_or_default().into_owned(),
                hidden: tag.attr("state").is_some_and(|s| matches!(&*s, "hidden" | "veryHidden")),
                rid: tag.attr_rel("id").unwrap_or_default().into_owned(),
            }),
            "definedName" => {
                let local = tag.attr("localSheetId").and_then(|v| v.trim().parse::<u32>().ok());
                if let Some(local) = local {
                    match tag.attr("name").as_deref() {
                        Some("_xlnm.Print_Titles") if self.titles.len() < 4_096 => {
                            self.titles.push(local);
                        }
                        Some("_xlnm.Print_Area") if self.areas.len() < 4_096 => {
                            self.areas.push(local);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, _local: &str) {}

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// worksheet
// ---------------------------------------------------------------------------

struct SheetH {
    lay: Layout,
    /// One-based number of the current row.
    row: u32,
    next_col: u32,
    row_ok: bool,
    in_row_breaks: bool,
    in_col_breaks: bool,
    fit_to_page: bool,
    fit: (u32, u32),
    saw_view: bool,
    default_col_width: Option<f64>,
    base_col_width: f64,
    cols_seen: usize,
}

impl SheetH {
    fn new() -> Self {
        let mut lay = Layout { gridlines: true, default_row: 15.0, ..Layout::default() };
        lay.setup.fit = None;
        Self {
            lay,
            row: 0,
            next_col: 0,
            row_ok: false,
            in_row_breaks: false,
            in_col_breaks: false,
            fit_to_page: false,
            fit: (1, 1),
            saw_view: false,
            default_col_width: None,
            base_col_width: 8.0,
            cols_seen: 0,
        }
    }

    fn finish(mut self) -> Layout {
        // Default column width: stated, or from the base width the way a spreadsheet rounds it
        // (to a whole number of eight pixels).
        self.lay.default_col = match self.default_col_width {
            Some(w) if w > 0.0 => sane_size(col_points(w), MAX_COL_PT).unwrap_or(48.0),
            _ => {
                let px = ((self.base_col_width * 7.0 + 5.0) / 8.0).ceil() * 8.0;
                sane_size(px * 0.75, MAX_COL_PT).unwrap_or(48.0)
            }
        };
        if self.fit_to_page {
            self.lay.setup.fit = Some(self.fit);
        }
        self.lay.setup.row_breaks.sort_unstable();
        self.lay.setup.row_breaks.dedup();
        self.lay.setup.col_breaks.sort_unstable();
        self.lay.setup.col_breaks.dedup();
        self.lay
    }
}

impl Handler for SheetH {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "pageSetUpPr" => self.fit_to_page = on(tag, "fitToPage"),
            "sheetFormatPr" => {
                self.default_col_width = tag.attr("defaultColWidth").as_deref().and_then(number);
                if let Some(b) = tag.attr("baseColWidth").as_deref().and_then(number) {
                    self.base_col_width = b.clamp(0.0, 255.0);
                }
                if let Some(h) = tag.attr("defaultRowHeight").as_deref().and_then(number)
                    && let Some(h) = sane_size(h, MAX_ROW_PT)
                    && h > 0.0
                {
                    self.lay.default_row = h;
                }
            }
            "sheetView" if !self.saw_view => {
                self.saw_view = true;
                if tag.attr("showGridLines").is_some_and(|v| matches!(&*v, "0" | "false")) {
                    self.lay.gridlines = false;
                }
            }
            "pane" => {
                if matches!(tag.attr("state").as_deref(), Some("frozen" | "frozenSplit")) {
                    self.lay.notes.add("frozen panes (ignored)");
                }
            }
            "col" if self.cols_seen < MAX_COL_ELEMENTS => {
                self.cols_seen += 1;
                let num = |n: &str| tag.attr(n).and_then(|v| v.trim().parse::<u32>().ok());
                if let (Some(min), Some(max)) = (num("min"), num("max")) {
                    let first = min.saturating_sub(1);
                    let last = max.saturating_sub(1).min(MAX_COLS - 1);
                    if first <= last {
                        let size = tag
                            .attr("width")
                            .as_deref()
                            .and_then(number)
                            .and_then(|w| sane_size(col_points(w), MAX_COL_PT));
                        let hidden = on(tag, "hidden");
                        self.lay.cols.push(RawRun { first, last, size, hidden });
                        if let Some(xf) = num("style").filter(|&s| s != 0) {
                            self.lay.wide.push(Wide { r1: 0, r2: MAX_ROWS - 1, c1: first, c2: last, xf });
                        }
                    }
                }
            }
            "row" => {
                let r = tag.attr("r").and_then(|v| v.trim().parse::<u32>().ok());
                self.row = r.unwrap_or_else(|| self.row.saturating_add(1));
                self.next_col = 0;
                self.row_ok = (1..=MAX_ROWS).contains(&self.row);
                if self.row_ok {
                    let row0 = self.row - 1;
                    let ht = tag
                        .attr("ht")
                        .as_deref()
                        .and_then(number)
                        .and_then(|h| sane_size(h, MAX_ROW_PT));
                    let hidden = on(tag, "hidden");
                    if (ht.is_some() || hidden) && self.lay.rows.len() < MAX_RUNS {
                        self.lay.rows.push(RawRun { first: row0, last: row0, size: ht, hidden });
                    }
                    if on(tag, "customFormat")
                        && let Some(xf) =
                            tag.attr("s").and_then(|v| v.trim().parse::<u32>().ok()).filter(|&s| s != 0)
                    {
                        self.lay.wide.push(Wide { r1: row0, r2: row0, c1: 0, c2: MAX_COLS - 1, xf });
                    }
                }
            }
            "c" => {
                let col = tag
                    .attr("r")
                    .as_deref()
                    .and_then(cell_ref)
                    .map_or(self.next_col, |(c, _)| c);
                self.next_col = col.saturating_add(1);
                let xf = tag.attr("s").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0);
                if self.row_ok && xf != 0 && col < MAX_COLS {
                    if self.lay.styled.len() < MAX_STYLED {
                        self.lay.styled.push((self.row - 1, col, xf));
                    } else {
                        self.lay.truncated = true;
                    }
                }
            }
            "mergeCell" => {
                if let Some(m) = tag.attr("ref").as_deref().and_then(range_ref) {
                    if self.lay.merges.len() < MAX_MERGES {
                        self.lay.merges.push(m);
                    } else {
                        self.lay.truncated = true;
                    }
                }
            }
            "conditionalFormatting" | "colorScale" | "iconSet" | "cfRule" => {
                self.lay.notes.add("conditional formatting (not drawn)");
            }
            "dataBar" => self.lay.notes.add("data bars (not drawn)"),
            "sparklineGroup" | "sparklineGroups" => self.lay.notes.add("sparklines (not drawn)"),
            "drawing" | "legacyDrawing" | "picture" | "oleObject" | "control" => {
                self.lay.notes.add("drawings, charts and pictures (not drawn)");
            }
            "pageMargins" => {
                for (side, name) in [(0, "left"), (1, "top"), (2, "right"), (3, "bottom")] {
                    if let Some(v) = tag.attr(name).as_deref().and_then(number) {
                        self.lay.setup.set_margin_inches(side, v);
                    }
                }
            }
            "pageSetup" => {
                let u = |n: &str| tag.attr(n).and_then(|v| v.trim().parse::<u32>().ok());
                if let Some(p) = u("paperSize").and_then(paper_size) {
                    self.lay.setup.paper = p;
                }
                self.lay.setup.landscape = tag.attr("orientation").as_deref() == Some("landscape");
                if let Some(s) = u("scale") {
                    self.lay.setup.scale = f32_of(f64::from(s.clamp(10, 400)) / 100.0);
                }
                if let Some(w) = u("fitToWidth") {
                    self.fit.0 = w.min(1_000);
                }
                if let Some(h) = u("fitToHeight") {
                    self.fit.1 = h.min(1_000);
                }
                self.lay.setup.over_then_down = tag.attr("pageOrder").as_deref() == Some("overThenDown");
            }
            "printOptions" => self.lay.setup.print_grid = on(tag, "gridLines"),
            "rowBreaks" => self.in_row_breaks = true,
            "colBreaks" => self.in_col_breaks = true,
            "brk" => {
                let id = tag.attr("id").and_then(|v| v.trim().parse::<u32>().ok());
                if let Some(id) = id.filter(|_| on(tag, "man")) {
                    let list = if self.in_row_breaks {
                        Some(&mut self.lay.setup.row_breaks)
                    } else if self.in_col_breaks {
                        Some(&mut self.lay.setup.col_breaks)
                    } else {
                        None
                    };
                    if let Some(list) = list
                        && list.len() < 2_048
                    {
                        list.push(id);
                    }
                }
            }
            "rPh" => return Flow::SkipChildren,
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "rowBreaks" => self.in_row_breaks = false,
            "colBreaks" => self.in_col_breaks = false,
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

fn read_xml(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

/// Lists the sheets of the workbook and reads the look of the one at `want` (counted among
/// the sheets that are shown: hidden ones only when `reveal`).
pub(super) fn read(
    pkg: &mut Package<'_>,
    want: Option<usize>,
    reveal: bool,
) -> Result<Read, PreviewError> {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no workbook part".into()))?;
    let Some(wb_text) = read_xml(pkg, &main)? else {
        return Err(PreviewError::Malformed("the workbook part is missing".into()));
    };
    let mut wb = WbH::default();
    if xml::drive(&wb_text, pkg.limits(), &mut wb).is_err() && wb.sheets.is_empty() {
        return Err(PreviewError::Malformed("the workbook part is not readable".into()));
    }
    let mut metas: Vec<Meta> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut picked_at: Option<usize> = None;
    let mut shown = 0usize;
    for sheet in &wb.sheets {
        let Some((part, external)) = pkg.rel_by_id(&main, &sheet.rid)? else { continue };
        if external || pkg.size_of(&part).is_none() {
            continue;
        }
        if reveal || !sheet.hidden {
            if want == Some(shown) {
                picked_at = Some(metas.len());
            }
            shown += 1;
        }
        metas.push(Meta { name: sheet.name.clone(), hidden: sheet.hidden });
        parts.push(part);
    }
    let Some(index) = picked_at else {
        if want.is_some() {
            return Err(PreviewError::Malformed("the workbook has no such sheet".into()));
        }
        return Ok(Read { metas, picked: None });
    };
    let Some(part) = parts.get(index).cloned() else {
        return Ok(Read { metas, picked: None });
    };

    // Theme and formats.
    let mut pal = Palette::default();
    if let Some(p) = pkg.rel_target(&main, "theme")?
        && let Some(text) = read_xml(pkg, &p)?
    {
        let mut h = ThemeH { slots: [None; 12], in_scheme: false, cur: None };
        // A damaged theme costs the theme colours only.
        let _ = xml::drive(&text, pkg.limits(), &mut h);
        for (slot, found) in pal.theme.iter_mut().zip(h.slots) {
            if let Some(c) = found {
                *slot = c;
            }
        }
    }
    let mut styles = Styles::default();
    let mut notes_over = false;
    if let Some(p) = pkg.rel_target(&main, "styles")?
        && let Some(text) = read_xml(pkg, &p)?
    {
        let mut h = StyleH::default();
        let _ = xml::drive(&text, pkg.limits(), &mut h);
        if h.indexed.len() > 8 {
            pal.custom = std::mem::take(&mut h.indexed);
        }
        notes_over = h.over;
        styles = build_styles(h, &pal);
    }
    if styles.fonts.is_empty() {
        styles.fonts.push(Font::default());
    }

    // The sheet.
    let mut h = SheetH::new();
    let mut ok = true;
    if let Some(text) = read_xml(pkg, &part)? {
        ok = xml::drive(&text, pkg.limits(), &mut h).is_ok();
    }
    let mut layout = h.finish();
    layout.truncated |= !ok;
    if notes_over {
        layout.notes.add("more cell formats than are kept");
    }
    if wb.titles.iter().any(|&i| usize::try_from(i).is_ok_and(|i| i == index)) {
        layout.notes.add("print titles (ignored)");
    }
    if wb.areas.iter().any(|&i| usize::try_from(i).is_ok_and(|i| i == index)) {
        layout.notes.add("print area (ignored)");
    }
    Ok(Read { metas, picked: Some(Picked { index, styles, layout }) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_widths_become_points() {
        // 8.43 characters (the default) is 64 pixels, that is 48 points.
        assert!((col_points(9.140_625) - 48.0).abs() < 1e-9);
        // 10 characters (70 pixels).
        assert!((col_points(10.0) - 52.5).abs() < 1e-9);
        assert!(col_points(0.0).abs() < 1e-9);
    }

    #[test]
    fn references_and_ranges() {
        assert_eq!(cell_ref("A1"), Some((0, 0)));
        assert_eq!(cell_ref("$B$3"), Some((1, 2)));
        assert_eq!(cell_ref("XFD1048576"), Some((16_383, 1_048_575)));
        assert_eq!(cell_ref("A0"), None);
        assert_eq!(cell_ref("1A"), None);
        assert_eq!(range_ref("A1:B2"), Merge::new(0, 0, 1, 1));
        assert_eq!(range_ref("A1:A1"), None);
        assert_eq!(range_ref("A1"), None);
        assert_eq!(range_ref("A1:XFE5"), None);
    }

    #[test]
    fn tints_follow_the_spreadsheet_rule() {
        assert_eq!(tinted(0x00_00_00, 0.5), 0x80_80_80);
        assert_eq!(tinted(0xFF_FF_FF, -0.5), 0x80_80_80);
        assert_eq!(tinted(0x12_34_56, 0.0), 0x12_34_56);
        assert_eq!(tinted(0x12_34_56, f64::NAN), 0x12_34_56);
        // Saturated colours keep their hue.
        let c = tinted(0x44_72_C4, 0.4);
        assert!(c & 0xFF > 0xC4 && (c >> 16) > 0x44);
    }

    #[test]
    fn colours_resolve_through_theme_and_palette() {
        let pal = Palette::default();
        assert_eq!(pal.resolve(Col::Rgb(0x123456), 0), Some(0xFF12_3456));
        assert_eq!(pal.resolve(Col::Theme(1, 0.0), 0), Some(0xFF00_0000));
        assert_eq!(pal.resolve(Col::Theme(99, 0.0), 0), None);
        assert_eq!(pal.resolve(Col::Indexed(2, 0.0), 0), Some(0xFFFF_0000));
        assert_eq!(pal.resolve(Col::Indexed(65, 0.0), 0), Some(0xFFFF_FFFF));
        assert_eq!(pal.resolve(Col::Indexed(999, 0.0), 0), None);
        assert_eq!(pal.resolve(Col::Auto, 0x00FF00), Some(0xFF00_FF00));
        assert_eq!(pal.resolve(Col::None, 0), None);
    }
}
