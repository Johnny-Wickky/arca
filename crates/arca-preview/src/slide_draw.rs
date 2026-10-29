//! A slide as a page: what it looks like, as the list of things to draw of [`crate::page`].
//!
//! Presentations (`.pptx` family: ISO 29500 PresentationML/DrawingML; `.odp`: ODF `draw`) are
//! drawn from their *flat geometry* only: filled and stroked paths for shapes (preset shapes from a
//! small table, custom geometry paths), text laid out with estimated glyph widths, tables as cell
//! rectangles with text, and pictures, charts, SmartArt and embedded objects as the image
//! placeholder of the page model. Nothing is decoded or loaded: no fonts, no rasters, no effects.
//!
//! Everything a slide can make large is bounded: the number of nodes read from a part, shapes
//! drawn, nesting of groups, characters laid out, primitives and path numbers. When a bound is hit
//! the page says so ([`PageModel::truncated`]); everything only approximated (gradients, dashes,
//! unknown presets, text metrics that could not be known) sets [`PageModel::approximate`]
//! and leaves a reason in [`Rendered::notes`].
//!
//! Text is cleaned the way every other string of the preview is (see [`crate::sink::clean_text`])
//! before it enters a primitive.

use crate::detect::{self, Doc};
use crate::model::FileFormat;
use crate::page::{MAX_PATH_NUMBERS, MAX_PRIMS, PageModel, Prim, op, style as pstyle};
use crate::sink::{CleanCounts, clean_text};
use crate::xml::{Ev, Reader, XmlError, XmlLimits, unescape};
use crate::{PreviewError, PreviewPolicy};

mod odp;
mod ooxml;
mod presets;
#[cfg(test)]
mod tests;

/// Most shapes (of the slide, its layout and its master together) drawn on one page.
const MAX_SHAPES: usize = 20_000;
/// Most nodes read from one part.
const MAX_NODES: usize = 600_000;
/// Most numbers in all the paths of one page.
const MAX_PAGE_NUMBERS: usize = 4_000_000;
/// Deepest nesting of groups that is drawn.
const MAX_GROUP_DEPTH: usize = 12;
/// Most characters of text laid out on one page.
const MAX_TEXT_CHARS: usize = 1 << 20;
/// Most lines in one text body.
const MAX_LINES: usize = 4_000;
/// Largest coordinate kept, in points.
const MAX_COORD: f64 = 1.0e6;
/// Page sides outside this range (in points) are not believed.
const MIN_SIDE: f64 = 1.0;
const MAX_SIDE: f64 = 5_000.0;

// ---- numbers ---------------------------------------------------------------------------------

/// A finite number within the coordinate bound.
fn fin(v: f64) -> f64 {
    if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 }
}

#[allow(clippy::cast_possible_truncation)] // a coordinate on a page fits an f32
fn f32c(v: f64) -> f32 {
    fin(v) as f32
}

/// Packs an opacity multiplier into a colour.
fn fade(color: u32, k: f64) -> u32 {
    if k >= 1.0 || color >> 24 == 0 {
        return color;
    }
    let a = f64::from(color >> 24) * k.clamp(0.0, 1.0);
    // `a` is in 0..=255.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let a = a.round() as u32;
    (a.min(255) << 24) | (color & 0x00FF_FFFF)
}

/// An affine transform; `then` applies `self` first (same convention as the PDF recorder).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Mat {
    pub(super) a: f64,
    pub(super) b: f64,
    pub(super) c: f64,
    pub(super) d: f64,
    pub(super) e: f64,
    pub(super) f: f64,
}

impl Mat {
    pub(super) const ID: Self = Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub(super) const fn translate(tx: f64, ty: f64) -> Self {
        Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }

    pub(super) const fn scale(sx: f64, sy: f64) -> Self {
        Self { a: sx, b: 0.0, c: 0.0, d: sy, e: 0.0, f: 0.0 }
    }

    /// A rotation by `rad` (clockwise on the screen, since y points down).
    pub(super) fn rotate(rad: f64) -> Self {
        let (s, c) = rad.sin_cos();
        Self { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// The transform that applies `self` first and `o` second.
    pub(super) fn then(self, o: Self) -> Self {
        Self {
            a: self.a * o.a + self.b * o.c,
            b: self.a * o.b + self.b * o.d,
            c: self.c * o.a + self.d * o.c,
            d: self.c * o.b + self.d * o.d,
            e: self.e * o.a + self.f * o.c + o.e,
            f: self.e * o.b + self.f * o.d + o.f,
        }
    }

    pub(super) fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (x * self.a + y * self.c + self.e, x * self.b + y * self.d + self.f)
    }

    /// Length of the image of the x axis and of the y axis.
    pub(super) fn axis_scales(self) -> (f64, f64) {
        (self.a.hypot(self.b), self.c.hypot(self.d))
    }
}

// ---- the page --------------------------------------------------------------------------------

/// One path operation in the shape's own units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Seg {
    M(f64, f64),
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
    Z,
}

/// The recorder of one page.
pub(super) struct Canvas {
    prims: Vec<Prim>,
    /// A bound was hit and something was left out.
    pub(super) full: bool,
    pub(super) approximate: bool,
    numbers: usize,
    shapes_left: usize,
    chars_left: usize,
    /// Why the page is approximate (each reason once).
    pub(super) notes: Vec<&'static str>,
    /// Opacity of what is drawn now (1 normally, less for shapes the document hides).
    pub(super) opacity: f64,
}

impl Canvas {
    pub(super) fn new() -> Self {
        Self {
            prims: Vec::new(),
            full: false,
            approximate: false,
            numbers: 0,
            shapes_left: MAX_SHAPES,
            chars_left: MAX_TEXT_CHARS,
            notes: Vec::new(),
            opacity: 1.0,
        }
    }

    pub(super) fn push(&mut self, p: Prim) {
        if self.prims.len() >= MAX_PRIMS {
            self.full = true;
        } else {
            self.prims.push(p);
        }
    }

    /// Says that the page is only approximate, and why.
    pub(super) fn approximated(&mut self, why: &'static str) {
        self.approximate = true;
        if !self.notes.contains(&why) {
            self.notes.push(why);
        }
    }

    /// Takes one shape from the budget; `false` (and the page is marked cut) when it is spent.
    pub(super) fn take_shape(&mut self) -> bool {
        if self.shapes_left == 0 {
            self.full = true;
            return false;
        }
        self.shapes_left -= 1;
        true
    }

    /// Takes `n` characters from the text budget.
    fn take_chars(&mut self, n: usize) -> bool {
        if self.chars_left < n {
            self.chars_left = 0;
            self.full = true;
            return false;
        }
        self.chars_left -= n;
        true
    }

    /// Text as it may enter a primitive: cleaned like every string of the preview, on one line.
    pub(super) fn tidy(&mut self, s: &str) -> String {
        let mut counts = CleanCounts::default();
        let cleaned = clean_text(s, &mut counts);
        if counts.total() > 0 {
            self.approximated("characters removed from the text");
        }
        cleaned.chars().map(|c| if matches!(c, '\n' | '\t') { ' ' } else { c }).collect()
    }

    /// Paints a path: `segs` in the shape's units mapped by `m`.
    pub(super) fn path(
        &mut self,
        segs: &[Seg],
        m: &Mat,
        fill: u32,
        stroke: u32,
        width: f64,
        even_odd: bool,
    ) {
        let (fill, stroke) = (fade(fill, self.opacity), fade(stroke, self.opacity));
        if fill >> 24 == 0 && stroke >> 24 == 0 {
            return;
        }
        let mut ops: Vec<f32> = Vec::with_capacity(segs.len().min(1024) * 3);
        for s in segs {
            let need = match s {
                Seg::M(..) | Seg::L(..) => 3,
                Seg::C(..) => 7,
                Seg::Z => 1,
            };
            if ops.len() + need > MAX_PATH_NUMBERS
                || self.numbers + ops.len() + need > MAX_PAGE_NUMBERS
            {
                self.full = true;
                break;
            }
            match *s {
                Seg::M(x, y) => {
                    let (x, y) = m.apply(x, y);
                    ops.extend([op::MOVE, f32c(x), f32c(y)]);
                }
                Seg::L(x, y) => {
                    let (x, y) = m.apply(x, y);
                    ops.extend([op::LINE, f32c(x), f32c(y)]);
                }
                Seg::C(x1, y1, x2, y2, x, y) => {
                    let (a, b) = m.apply(x1, y1);
                    let (c, d) = m.apply(x2, y2);
                    let (e, f) = m.apply(x, y);
                    ops.extend([op::CURVE, f32c(a), f32c(b), f32c(c), f32c(d), f32c(e), f32c(f)]);
                }
                Seg::Z => ops.push(op::CLOSE),
            }
        }
        if ops.is_empty() {
            return;
        }
        self.numbers += ops.len();
        let width = if width.is_finite() { width.clamp(0.0, 1_000.0) } else { 0.0 };
        self.push(Prim::Path { ops, fill, stroke, width: f32c(width), even_odd });
    }

    /// A picture, or what stands for one: the unit square under the matrix mapping the frame
    /// `0..w × 0..h` by `m`, with the image convention of the PDF page (the unit square's origin
    /// is the picture's bottom left corner).
    pub(super) fn image(&mut self, m: &Mat, w: f64, h: f64, label: &str, id: u32) {
        let label = self.tidy(label);
        let mat = [
            f32c(m.a * w),
            f32c(m.b * w),
            f32c(-m.c * h),
            f32c(-m.d * h),
            f32c(m.e + m.c * h),
            f32c(m.f + m.d * h),
        ];
        self.push(Prim::Image { m: mat, label, id });
    }

    /// Ends the page.
    pub(super) fn finish(
        mut self,
        width: f64,
        height: f64,
        index: usize,
        pages: usize,
        damaged: bool,
    ) -> Rendered {
        if damaged {
            self.approximated("a damaged part was read as far as it goes");
        }
        let page = PageModel {
            width: f32c(width),
            height: f32c(height),
            index: u32::try_from(index).unwrap_or(u32::MAX),
            pages: u32::try_from(pages).unwrap_or(u32::MAX),
            prims: self.prims,
            truncated: self.full,
            approximate: self.approximate,
        };
        Rendered { page, notes: self.notes }
    }
}

/// A rendered slide and the reasons it is approximate (for tests and for the developer tools).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rendered {
    pub(crate) page: PageModel,
    pub(crate) notes: Vec<&'static str>,
}

// ---- text ------------------------------------------------------------------------------------

/// Horizontal alignment of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Align {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

/// Where the text sits in its frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Anchor {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// How a run looks.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RunStyle {
    pub(super) size: f64,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) mono: bool,
    pub(super) serif: bool,
    pub(super) underline: bool,
    pub(super) strike: bool,
    pub(super) color: u32,
    /// `1` raised (superscript), `-1` lowered, `0` neither.
    pub(super) shift: i8,
}

impl RunStyle {
    pub(super) const fn plain(size: f64, color: u32) -> Self {
        Self {
            size,
            bold: false,
            italic: false,
            mono: false,
            serif: false,
            underline: false,
            strike: false,
            color,
            shift: 0,
        }
    }
}

/// A piece of text in one style; `"\n"` is a forced line break.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Piece {
    pub(super) text: String,
    pub(super) st: RunStyle,
}

/// Line spacing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Spacing {
    /// A multiple of the single spacing.
    Pct(f64),
    /// A fixed distance between baselines, in points.
    Exact(f64),
}

/// One paragraph.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Para {
    pub(super) pieces: Vec<Piece>,
    pub(super) align: Align,
    /// Left margin, points.
    pub(super) left: f64,
    /// Indent of the first line relative to the margin (negative: hanging bullet).
    pub(super) indent: f64,
    pub(super) bullet: Option<Piece>,
    pub(super) before: f64,
    pub(super) after: f64,
    pub(super) line: Spacing,
    /// Size of the (empty) line when the paragraph has no text.
    pub(super) end_size: f64,
}

/// The text of a shape.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TextBody {
    pub(super) paras: Vec<Para>,
    /// Left, top, right, bottom insets in points.
    pub(super) ins: [f64; 4],
    pub(super) anchor: Anchor,
    pub(super) wrap: bool,
    /// Factor applied to every size (normal autofit's font scale).
    pub(super) scale: f64,
}

// Advance widths (1/1000 em) of the standard fonts the PDF text path also uses when a font brings
// none: Helvetica and Times-Roman for U+0020..=U+007E.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];
const TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

// Advance widths (1/1000 em) of the Russian alphabet in a Helvetica-like face, А..Я and а..я
// (U+0410..=U+044F); Ё and ё take the width of Е and е. Without them every Cyrillic capital would
// be counted alike and the lines of a Russian slide would wrap too early or too late.
const CYRILLIC_UPPER: [u16; 32] = [
    667, 637, 667, 535, 700, 667, 995, 611, 722, 722, 667, 690, 833, 722, 778, 722, 667, 722, 611,
    667, 860, 667, 750, 648, 1026, 1060, 806, 925, 658, 732, 1050, 703,
];
const CYRILLIC_LOWER: [u16; 32] = [
    556, 556, 546, 410, 570, 556, 796, 498, 565, 565, 517, 545, 688, 562, 556, 562, 556, 500, 467,
    500, 797, 500, 570, 516, 828, 856, 607, 745, 518, 521, 842, 525,
];

/// The advance of a Russian letter in a sans-serif face, in 1/1000 em.
fn cyrillic_units(code: u32) -> Option<f64> {
    let table_at = |table: &[u16; 32], base: u32| {
        let i = usize::try_from(code.checked_sub(base)?).ok()?;
        table.get(i).copied().map(f64::from)
    };
    match code {
        0x0401 => Some(667.0),
        0x0451 => Some(556.0),
        0x0410..=0x042F => table_at(&CYRILLIC_UPPER, 0x0410),
        0x0430..=0x044F => table_at(&CYRILLIC_LOWER, 0x0430),
        _ => None,
    }
}

/// East Asian wide characters and emoji: a full em, and a line may break after each.
fn is_wide(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// Characters that take no room.
fn is_zero_width(c: char) -> bool {
    matches!(u32::from(c), 0x0300..=0x036F | 0x200B..=0x200F | 0x2060..=0x2064 | 0xFE00..=0xFE0F | 0xFEFF)
}

/// The advance of `c` in `st`, in ems.
pub(super) fn advance(c: char, st: &RunStyle) -> f64 {
    let code = u32::from(c);
    let units = if is_zero_width(c) {
        0.0
    } else if st.mono {
        600.0
    } else if is_wide(c) {
        1000.0
    } else if (0x20..=0x7E).contains(&code) {
        let idx = usize::try_from(code - 0x20).unwrap_or(0);
        let table = if st.serif { &TIMES } else { &HELVETICA };
        f64::from(table.get(idx).copied().unwrap_or(500))
    } else if let Some(units) = cyrillic_units(code) {
        // Times-like faces are about a twelfth narrower than Arial-like ones.
        if st.serif { units * 0.92 } else { units }
    } else if c.is_uppercase() {
        if st.serif { 690.0 } else { 680.0 }
    } else if st.serif {
        480.0
    } else {
        540.0
    };
    let bold = if st.bold && !st.mono { 1.06 } else { 1.0 };
    units * bold / 1000.0
}

/// The width of `text` in `st`, in points.
pub(super) fn text_width(text: &str, st: &RunStyle) -> f64 {
    text.chars().map(|c| advance(c, st)).sum::<f64>() * st.size
}

#[cfg(test)]
mod advance_tests {
    use super::*;

    #[test]
    fn russian_letters_have_their_own_widths() {
        let st = RunStyle::plain(10.0, 0xFF00_0000);
        // "Ш" is much wider than "Г", "ь" narrower than "ы": the old flat guess missed both.
        assert!(advance('Ш', &st) > 1.5 * advance('Г', &st));
        assert!(advance('ы', &st) > advance('ь', &st));
        assert!((advance('Ё', &st) - advance('Е', &st)).abs() < 1e-9);
        assert!((advance('ё', &st) - advance('е', &st)).abs() < 1e-9);
        // A Russian word is somewhat wider than the Latin word of the same length.
        let ru = text_width("Привет", &st);
        let en = text_width("Hellos", &st);
        assert!(ru / en > 1.0 && ru / en < 1.4, "{ru} {en}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokKind {
    Word,
    Space,
    Break,
}

struct Tok {
    text: String,
    piece: usize,
    w: f64,
    kind: TokKind,
}

fn tokens(p: &Para, scale: f64) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    for (pi, piece) in p.pieces.iter().enumerate() {
        if piece.text == "\n" {
            out.push(Tok { text: String::new(), piece: pi, w: 0.0, kind: TokKind::Break });
            continue;
        }
        let sz = piece.st.size * scale;
        let mut word = String::new();
        let mut word_w = 0.0;
        let flush = |word: &mut String, word_w: &mut f64, out: &mut Vec<Tok>| {
            if !word.is_empty() {
                out.push(Tok {
                    text: std::mem::take(word),
                    piece: pi,
                    w: *word_w,
                    kind: TokKind::Word,
                });
                *word_w = 0.0;
            }
        };
        for c in piece.text.chars() {
            let w = advance(c, &piece.st) * sz;
            if c.is_whitespace() {
                flush(&mut word, &mut word_w, &mut out);
                // Consecutive spaces of one piece are one token.
                match out.last_mut() {
                    Some(t) if t.kind == TokKind::Space && t.piece == pi => {
                        t.text.push(' ');
                        t.w += w;
                    }
                    _ => out.push(Tok { text: " ".to_owned(), piece: pi, w, kind: TokKind::Space }),
                }
            } else if is_wide(c) {
                flush(&mut word, &mut word_w, &mut out);
                out.push(Tok { text: c.to_string(), piece: pi, w, kind: TokKind::Word });
            } else {
                word.push(c);
                word_w += w;
            }
        }
        flush(&mut word, &mut word_w, &mut out);
    }
    out
}

/// A run of one style on a line.
#[derive(Debug, Clone)]
pub(super) struct Frag {
    pub(super) x: f64,
    pub(super) text: String,
    pub(super) st: RunStyle,
    pub(super) w: f64,
}

/// A laid-out line.
#[derive(Debug, Clone)]
pub(super) struct LLine {
    pub(super) frags: Vec<Frag>,
    pub(super) width: f64,
    pub(super) x0: f64,
    /// Distance between this baseline and the previous one's reference (the line's height).
    pub(super) pitch: f64,
    /// The baseline below the top of the line.
    pub(super) base: f64,
}

/// A laid-out text body.
#[derive(Debug, Clone, Default)]
pub(super) struct Laid {
    pub(super) paras: Vec<LaidPara>,
    pub(super) height: f64,
}

/// A laid-out paragraph.
#[derive(Debug, Clone)]
pub(super) struct LaidPara {
    pub(super) before: f64,
    pub(super) lines: Vec<LLine>,
    pub(super) after: f64,
    pub(super) align: Align,
    /// Where the bullet goes (from the margin), and what it is.
    pub(super) bullet: Option<(f64, Piece)>,
}

struct Item {
    text: String,
    piece: usize,
    w: f64,
    space: bool,
}

/// Lays out one paragraph: greedy line breaking by the estimated widths.
fn layout_para(p: &Para, avail: f64, wrap: bool, scale: f64, room: &mut usize) -> Vec<LLine> {
    let bullet_w = p.bullet.as_ref().map_or(0.0, |b| text_width(&b.text, &b.st) * scale);
    let (first_x0, rest_x0) = if p.bullet.is_some() {
        let bullet_x = (p.left + p.indent).max(0.0);
        let text_x = if p.indent < 0.0 { p.left } else { bullet_x + bullet_w };
        let text_x = text_x.max(bullet_x + bullet_w + 0.25 * p.end_size * scale);
        (text_x, p.left.max(0.0))
    } else {
        ((p.left + p.indent).max(0.0), p.left.max(0.0))
    };
    let mut lines: Vec<LLine> = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let mut cur_w = 0.0_f64;
    let mut x0 = first_x0;

    let make_line = |cur: &mut Vec<Item>, x0: f64| -> LLine {
        while cur.last().is_some_and(|i| i.space) {
            cur.pop();
        }
        let mut frags: Vec<Frag> = Vec::new();
        let mut x = 0.0;
        let mut size = 0.0_f64;
        for it in cur.drain(..) {
            let Some(piece) = p.pieces.get(it.piece) else { continue };
            if !it.space {
                size = size.max(piece.st.size * scale);
            }
            match frags.last_mut() {
                Some(f) if f.st == piece.st => {
                    f.text.push_str(&it.text);
                    f.w += it.w;
                }
                _ => frags.push(Frag { x, text: it.text, st: piece.st.clone(), w: it.w }),
            }
            x += it.w;
        }
        if size == 0.0 {
            size = p.end_size * scale;
        }
        let base_pitch = size * 1.2;
        let pitch = match p.line {
            Spacing::Pct(k) => base_pitch * k,
            Spacing::Exact(pts) => pts,
        };
        LLine { frags, width: x, x0, pitch, base: (pitch - 0.2 * size).max(size * 0.5) }
    };

    for tok in tokens(p, scale) {
        if *room == 0 {
            break;
        }
        match tok.kind {
            TokKind::Break => {
                lines.push(make_line(&mut cur, x0));
                *room = room.saturating_sub(1);
                cur_w = 0.0;
                x0 = rest_x0;
            }
            TokKind::Space => {
                // A line that was wrapped does not start with the space it was wrapped at.
                if !cur.is_empty() || x0 == first_x0 && lines.is_empty() {
                    cur_w += tok.w;
                    cur.push(Item { text: tok.text, piece: tok.piece, w: tok.w, space: true });
                }
            }
            TokKind::Word => {
                let line_avail = if wrap { (avail - x0).max(1.0) } else { f64::INFINITY };
                if wrap && cur_w + tok.w > line_avail + 0.01 && cur.iter().any(|i| !i.space) {
                    lines.push(make_line(&mut cur, x0));
                    *room = room.saturating_sub(1);
                    cur_w = 0.0;
                    x0 = rest_x0;
                }
                let line_avail = if wrap { (avail - x0).max(1.0) } else { f64::INFINITY };
                if wrap && tok.w > line_avail + 0.01 {
                    // A word longer than a line is cut where the line ends.
                    let Some(piece) = p.pieces.get(tok.piece) else { continue };
                    for c in tok.text.chars() {
                        let w = advance(c, &piece.st) * piece.st.size * scale;
                        let line_avail = (avail - x0).max(1.0);
                        if cur_w + w > line_avail + 0.01 && cur.iter().any(|i| !i.space) {
                            lines.push(make_line(&mut cur, x0));
                            *room = room.saturating_sub(1);
                            cur_w = 0.0;
                            x0 = rest_x0;
                        }
                        cur_w += w;
                        cur.push(Item { text: c.to_string(), piece: tok.piece, w, space: false });
                    }
                } else {
                    cur_w += tok.w;
                    cur.push(Item { text: tok.text, piece: tok.piece, w: tok.w, space: false });
                }
            }
        }
    }
    lines.push(make_line(&mut cur, x0));
    *room = room.saturating_sub(1);
    lines
}

/// Lays out a text body in a frame `w` wide.
pub(super) fn layout(tb: &TextBody, w: f64, cv: &mut Canvas) -> Laid {
    let chars: usize = tb
        .paras
        .iter()
        .map(|p| p.pieces.iter().map(|pc| pc.text.len()).sum::<usize>())
        .sum::<usize>();
    if !cv.take_chars(chars) {
        return Laid::default();
    }
    let inner = (w - tb.ins[0] - tb.ins[2]).max(0.0);
    let scale = if tb.scale.is_finite() { tb.scale.clamp(0.1, 1.0) } else { 1.0 };
    let mut room = MAX_LINES;
    let mut laid = Laid::default();
    for (i, p) in tb.paras.iter().enumerate() {
        if room == 0 {
            cv.full = true;
            break;
        }
        let lines = layout_para(p, inner, tb.wrap, scale, &mut room);
        let size = p.end_size * scale;
        let spc = |v: f64| if v.is_finite() { v.clamp(0.0, 1_000.0) } else { 0.0 };
        let before = if i == 0 { 0.0 } else { spc(p.before) };
        let after = spc(p.after);
        let bullet = p.bullet.as_ref().map(|b| {
            let mut piece = b.clone();
            // The bullet is drawn only next to text.
            if p.pieces.iter().all(|pc| pc.text.trim().is_empty()) {
                piece.text.clear();
            }
            ((p.left + p.indent).max(0.0), piece)
        });
        let _ = size;
        laid.height += before + lines.iter().map(|l| l.pitch).sum::<f64>() + after;
        laid.paras.push(LaidPara { before, lines, after, align: p.align, bullet });
    }
    laid
}

/// Draws a laid-out body in the frame `0..w × 0..h`, which `mt` maps to the page.
pub(super) fn draw_laid(cv: &mut Canvas, tb: &TextBody, laid: &Laid, w: f64, h: f64, mt: &Mat) {
    let scale = if tb.scale.is_finite() { tb.scale.clamp(0.1, 1.0) } else { 1.0 };
    let inner_w = (w - tb.ins[0] - tb.ins[2]).max(0.0);
    let inner_h = (h - tb.ins[1] - tb.ins[3]).max(0.0);
    let mut y = match tb.anchor {
        Anchor::Top => tb.ins[1],
        Anchor::Middle => tb.ins[1] + (inner_h - laid.height) / 2.0,
        Anchor::Bottom => tb.ins[1] + inner_h - laid.height,
    };
    let angle = mt.b.atan2(mt.a);
    for lp in &laid.paras {
        y += lp.before;
        for (li, line) in lp.lines.iter().enumerate() {
            let avail = (inner_w - line.x0).max(0.0);
            let shift = match lp.align {
                Align::Center => (avail - line.width) / 2.0,
                Align::End => avail - line.width,
                Align::Start | Align::Justify => 0.0,
            };
            let base = y + line.base;
            if li == 0
                && let Some((bx, piece)) = &lp.bullet
                && !piece.text.is_empty()
            {
                let x = tb.ins[0] + bx;
                let bw = text_width(&piece.text, &piece.st) * scale;
                put_text(cv, mt, angle, x, base, scale, &piece.text, &piece.st, bw);
            }
            for f in &line.frags {
                let x = tb.ins[0] + line.x0 + shift + f.x;
                put_text(cv, mt, angle, x, base, scale, &f.text, &f.st, f.w);
            }
            y += line.pitch;
        }
        y += lp.after;
    }
}

/// One text primitive (and its underline or strike-through) at the local point `(x, base)`.
#[allow(clippy::too_many_arguments)]
fn put_text(
    cv: &mut Canvas,
    mt: &Mat,
    angle: f64,
    x: f64,
    base: f64,
    scale: f64,
    text: &str,
    st: &RunStyle,
    width: f64,
) {
    if text.trim().is_empty() {
        return;
    }
    let mut size = st.size * scale;
    let mut base = base;
    let mut width = width;
    if st.shift != 0 {
        base -= f64::from(st.shift) * size * 0.33;
        let shrink = 0.66;
        width *= shrink;
        size *= shrink;
    }
    if size < 0.5 {
        return;
    }
    let (px, py) = mt.apply(x, base);
    let mut bits = 0u8;
    if st.bold {
        bits |= pstyle::BOLD;
    }
    if st.italic {
        bits |= pstyle::ITALIC;
    }
    if st.mono {
        bits |= pstyle::MONO;
    }
    if st.serif {
        bits |= pstyle::SERIF;
    }
    let color = fade(st.color, cv.opacity);
    let text = cv.tidy(text);
    cv.push(Prim::Text {
        x: f32c(px),
        y: f32c(py),
        size: f32c(size),
        angle: f32c(angle),
        width: f32c(width.max(0.0)),
        style: bits,
        color,
        text,
    });
    for (on, dy) in [(st.underline, size * 0.14), (st.strike, -size * 0.28)] {
        if on {
            let segs = [Seg::M(x, base + dy), Seg::L(x + width, base + dy)];
            cv.path(&segs, mt, 0, st.color, (size / 16.0).max(0.4), false);
        }
    }
}

/// `(mono, serif)` guessed from a typeface name.
pub(super) fn family_of(face: &str) -> (bool, bool) {
    let f = face.to_ascii_lowercase();
    let mono = ["courier", "consolas", "mono", "lucida console", "menlo", "typewriter"]
        .iter()
        .any(|m| f.contains(m));
    let serif = !mono
        && (["times", "georgia", "cambria", "garamond", "palatino", "book antiqua", "century"]
            .iter()
            .any(|m| f.contains(m))
            || (f.contains("serif") && !f.contains("sans")));
    (mono, serif)
}

/// One cell of a table, ready to draw.
pub(super) struct CellDraw {
    /// `x, y, w, h` in the table's coordinates.
    pub(super) rect: [f64; 4],
    pub(super) fill: u32,
    /// Left, top, right, bottom borders: colour and width.
    pub(super) borders: [Option<(u32, f64)>; 4],
    pub(super) body: TextBody,
    pub(super) laid: Laid,
}

/// Draws table cells; `base` maps the table's coordinates to the page. Fills first, then
/// borders, then text, so that no fill covers a neighbour's border.
pub(super) fn draw_cells(cv: &mut Canvas, cells: &[CellDraw], base: &Mat) {
    for c in cells {
        if c.fill >> 24 != 0 {
            let [x, y, w, h] = c.rect;
            cv.path(&presets::rect(w, h), &Mat::translate(x, y).then(*base), c.fill, 0, 0.0, false);
        }
    }
    for c in cells {
        let [x, y, w, h] = c.rect;
        let edges = [
            [Seg::M(x, y), Seg::L(x, y + h)],
            [Seg::M(x, y), Seg::L(x + w, y)],
            [Seg::M(x + w, y), Seg::L(x + w, y + h)],
            [Seg::M(x, y + h), Seg::L(x + w, y + h)],
        ];
        for (edge, b) in edges.iter().zip(c.borders.iter()) {
            if let Some((color, width)) = b {
                cv.path(edge, base, 0, *color, *width, false);
            }
        }
    }
    let angle = base.b.atan2(base.a);
    for c in cells {
        if c.laid.paras.is_empty() {
            continue;
        }
        let [x, y, w, h] = c.rect;
        let ctr = base.apply(x + w / 2.0, y + h / 2.0);
        let (sx, sy) = base.axis_scales();
        let (tw, th) = (w * sx, h * sy);
        let mt = Mat::translate(-tw / 2.0, -th / 2.0)
            .then(Mat::rotate(angle))
            .then(Mat::translate(ctr.0, ctr.1));
        draw_laid(cv, &c.body, &c.laid, tw, th, &mt);
    }
}

// ---- a small document tree -------------------------------------------------------------------

/// An element of a part, kept with just what the renderers look at.
///
/// Names are local names; an element in a foreign namespace has an empty name (see
/// [`crate::xml::Tag::local`]), so it is never taken for a look-alike. Attributes keep the name as
/// written (`x`, `svg:x`). Text sits in `#text` children, kept only inside the elements the
/// caller names.
#[derive(Debug, Clone, Default)]
pub(super) struct Node {
    pub(super) name: String,
    attrs: Vec<(String, String)>,
    rels: Vec<(&'static str, String)>,
    pub(super) kids: Vec<Node>,
    pub(super) text: String,
}

impl Node {
    /// The value of the attribute with exactly this name as written: DrawingML's attributes have
    /// no prefix (`x`, `cx`, `val`), ODF's are always written with their customary one
    /// (`svg:x`). An attribute of another namespace is never taken for one of these.
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// The attributes as written, `(qualified name, value)`.
    pub(super) fn attrs_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.attrs.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// An attribute whose prefix is bound to the relationships namespace (`r:embed`, `r:id`).
    pub(super) fn rel(&self, local: &str) -> Option<&str> {
        self.rels.iter().find(|(n, _)| *n == local).map(|(_, v)| v.as_str())
    }

    pub(super) fn child(&self, local: &str) -> Option<&Node> {
        self.kids.iter().find(|k| k.name == local)
    }

    pub(super) fn kids_named<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a Node> {
        self.kids.iter().filter(move |k| k.name == local)
    }

    /// The text of the `#text` children, joined.
    pub(super) fn own_text(&self) -> String {
        self.kids.iter().filter(|k| k.name == "#text").map(|k| k.text.as_str()).collect()
    }
}

/// A parsed part.
pub(super) struct Parsed {
    pub(super) root: Node,
    /// The part had more than [`MAX_NODES`] nodes; the rest was dropped.
    pub(super) cut: bool,
    /// The part was not well formed from some point on; what came before it is kept.
    pub(super) damaged: bool,
}

/// Reads a part into a tree. `keep_text` names the elements whose text is kept; `rel_elems` those
/// whose relationship attributes are read.
pub(super) fn parse(
    src: &str,
    limits: &XmlLimits,
    keep_text: &[&str],
    rel_elems: &[&str],
) -> Result<Parsed, XmlError> {
    let mut rd = Reader::new(src, limits)?;
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut count = 0usize;
    let (mut cut, mut damaged) = (false, false);

    fn attach(stack: &mut [Node], root: &mut Option<Node>, node: Node) {
        match stack.last_mut() {
            Some(parent) => parent.kids.push(node),
            None => {
                if root.is_none() {
                    *root = Some(node);
                }
            }
        }
    }

    loop {
        let ev = match rd.next() {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(e) => {
                if root.is_none() && stack.is_empty() {
                    return Err(e);
                }
                damaged = true;
                break;
            }
        };
        match ev {
            Ev::Start(t) | Ev::Empty(t) => {
                count += 1;
                if count > MAX_NODES {
                    cut = true;
                    break;
                }
                let local = t.local();
                let mut node = Node { name: local.to_owned(), ..Node::default() };
                for (n, v) in t.attrs() {
                    if let Ok(v) = unescape(v) {
                        node.attrs.push((n.to_owned(), v.into_owned()));
                    }
                }
                if rel_elems.contains(&local) {
                    for want in ["embed", "link", "id", "dm"] {
                        if let Some(v) = t.attr_rel(want) {
                            node.rels.push((want, v.into_owned()));
                        }
                    }
                }
                // An empty element is followed by its end event, which closes it.
                stack.push(node);
            }
            Ev::End(_) => {
                if let Some(node) = stack.pop() {
                    attach(&mut stack, &mut root, node);
                }
            }
            Ev::Text(raw) | Ev::CData(raw) => {
                let is_cdata = matches!(ev, Ev::CData(_));
                if let Some(top) = stack.last_mut()
                    && keep_text.contains(&top.name.as_str())
                {
                    let s =
                        if is_cdata { Ok(std::borrow::Cow::Borrowed(raw)) } else { unescape(raw) };
                    match s {
                        Ok(s) if !s.is_empty() => {
                            count += 1;
                            if count > MAX_NODES {
                                cut = true;
                                break;
                            }
                            top.kids.push(Node {
                                name: "#text".to_owned(),
                                text: s.into_owned(),
                                ..Node::default()
                            });
                        }
                        Ok(_) => {}
                        Err(_) => damaged = true,
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        attach(&mut stack, &mut root, node);
    }
    match root {
        Some(root) => Ok(Parsed { root, cut, damaged }),
        None => Err(XmlError::Malformed("the part has no root element".into())),
    }
}

/// The value of attribute `name` on the root element, read without parsing the rest.
fn root_attr(src: &str, limits: &XmlLimits, name: &str) -> Option<String> {
    let mut rd = Reader::new(src, limits).ok()?;
    while let Ok(Some(ev)) = rd.next() {
        if let Ev::Start(t) | Ev::Empty(t) = ev {
            return t.attr_plain(name).map(std::borrow::Cow::into_owned);
        }
    }
    None
}

// ---- lengths and colours ---------------------------------------------------------------------

/// A number that is finite.
fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// RGB (0..=1 each) to hue (degrees), saturation, lightness.
fn to_hsl(rgb: [f64; 3]) -> (f64, f64, f64) {
    let [r, g, b] = rgb;
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    if (mx - mn).abs() < 1e-12 {
        return (0.0, 0.0, l);
    }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if (mx - r).abs() < 1e-12 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (mx - g).abs() < 1e-12 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> [f64; 3] {
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    if s == 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let h = h.rem_euclid(360.0) / 360.0;
    let chan = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [chan(h + 1.0 / 3.0), chan(h), chan(h - 1.0 / 3.0)]
}

// ---- entry points ----------------------------------------------------------------------------

fn is_presentation_package(format: FileFormat) -> bool {
    matches!(
        format,
        FileFormat::Pptx
            | FileFormat::Pptm
            | FileFormat::Ppsx
            | FileFormat::Ppsm
            | FileFormat::Potx
            | FileFormat::Potm
    )
}

/// What a deck renders: the number of pages and, if one was asked for, that page.
type Outcome = Result<(usize, Option<Rendered>), PreviewError>;

fn run(data: &[u8], want: Option<usize>, policy: &PreviewPolicy) -> Outcome {
    if data.len() > policy.max_input_bytes {
        return Err(PreviewError::TooLarge(policy.max_input_bytes));
    }
    match detect::open(data, policy)? {
        Doc::Opc { mut pkg, format } if is_presentation_package(format) => {
            ooxml::run(&mut pkg, want, policy)
        }
        Doc::Odf { mut pkg, format: FileFormat::Odp } => odp::run(&mut pkg, want, policy),
        _ => Err(PreviewError::Unsupported(detect::format_of(data, policy))),
    }
}

/// The number of slides of a presentation (`.pptx` family or `.odp`) in presentation order. A
/// hidden slide counts only if the policy reveals hidden content.
///
/// # Errors
/// [`PreviewError::Unsupported`] for other formats; [`PreviewError::Malformed`] when the
/// presentation part cannot be read.
#[allow(dead_code)] // wired by the page-rendering entry point of the engine
pub(crate) fn slide_count(data: &[u8], policy: &PreviewPolicy) -> Result<usize, PreviewError> {
    run(data, None, policy).map(|(n, _)| n)
}

/// Draws slide `index` (from 0) of a presentation as a page.
///
/// # Errors
/// As [`slide_count`]; [`PreviewError::Malformed`] when there is no such slide.
pub(crate) fn render_slide(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<PageModel, PreviewError> {
    render_slide_noted(data, index, policy).map(|r| r.page)
}

/// [`render_slide`] together with the reasons the page is approximate.
pub(crate) fn render_slide_noted(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<Rendered, PreviewError> {
    match run(data, Some(index), policy)? {
        (_, Some(r)) => Ok(r),
        _ => Err(PreviewError::Malformed("the document has no such page".into())),
    }
}

/// A page side from a length in points, or `None` if it is not believable.
fn side(v: f64) -> Option<f64> {
    (v.is_finite() && (MIN_SIDE..=MAX_SIDE).contains(&v)).then_some(v)
}
