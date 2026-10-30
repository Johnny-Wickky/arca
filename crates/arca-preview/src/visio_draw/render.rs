//! The drawing of a Visio page: shapes, groups, masters, styles, text.
//!
//! Coordinates. Visio keeps lengths in inches and angles in radians, y pointing up. A shape has a
//! pin (`PinX`, `PinY`) in the coordinates of its parent, a local pin (`LocPinX`, `LocPinY`) in its
//! own, a `Width` and `Height`, an `Angle` (counter-clockwise) and the flips `FlipX`, `FlipY`. A
//! point `p` of the shape is placed in its parent at
//! `pin + rotate(angle) * flip * (p - locpin)`, where the flip negates the coordinate (the flip is
//! about the local pin). Children of a group are placed in the group's coordinates, so the chain of
//! the transforms is multiplied from the page down. The page's `PageScale / DrawingScale` ratio
//! turns drawing units into paper inches. This arithmetic is the one the SDK reference describes
//! for the pin and rotation; the flip about the pin follows the behaviour of known viewers and was
//! not checked against Microsoft's own files, which the notes of the reading view say.
//!
//! Inheritance. A cell that a shape does not define comes from its master shape and then from the
//! style sheet its line, fill or text style names (and that style's parents), with depth limits and
//! cycle guards. Formulas are not evaluated: the cached value of the cell is used.

use std::borrow::Cow;

use super::geom::{Builder, Flags, Seg, round_corners};
use super::model::{Cells, Doc, Fmt, Geom, Kind, PageDef, Row, Sheet, Val, cell, num};
use crate::model::IMAGE_NONE;
use crate::page::{MAX_PATH_NUMBERS, MAX_PRIMS, PageModel, Prim, op, style as pstyle};
use crate::sink::{CleanCounts, clean_text};

/// Most shapes drawn on one page.
const MAX_SHAPES: usize = 20_000;
/// Deepest nesting of groups that is drawn.
pub(crate) const MAX_DEPTH: usize = 24;
/// Deepest chain of style sheets followed.
const MAX_STYLE_DEPTH: usize = 16;
/// Most path segments of one page.
const MAX_SEGMENTS: usize = 1_500_000;
/// Most characters of text laid out on one page.
const MAX_TEXT_CHARS: usize = 1 << 20;
/// Most lines in one text block.
const MAX_LINES: usize = 500;
/// Page sides outside this range (in points) are not believed.
const MIN_SIDE: f64 = 1.0;
const MAX_SIDE: f64 = 14_400.0;
/// Points per inch.
const PT: f64 = 72.0;

/// An affine transform; `then` applies `self` first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Mat {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) e: f64,
    pub(crate) f: f64,
}

impl Mat {
    pub(crate) const ID: Self = Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    /// The transform that applies `self` first and `o` second.
    pub(crate) fn then(self, o: Self) -> Self {
        Self {
            a: self.a * o.a + self.b * o.c,
            b: self.a * o.b + self.b * o.d,
            c: self.c * o.a + self.d * o.c,
            d: self.c * o.b + self.d * o.d,
            e: self.e * o.a + self.f * o.c + o.e,
            f: self.e * o.b + self.f * o.d + o.f,
        }
    }

    pub(crate) fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (x * self.a + y * self.c + self.e, x * self.b + y * self.d + self.f)
    }

    /// Where a shape's local point goes in its parent: see the module documentation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn shape(
        pin: (f64, f64),
        loc: (f64, f64),
        angle: f64,
        flip_x: bool,
        flip_y: bool,
    ) -> Self {
        let translate_loc = Self { e: -loc.0, f: -loc.1, ..Self::ID };
        let fx = if flip_x { -1.0 } else { 1.0 };
        let fy = if flip_y { -1.0 } else { 1.0 };
        let flip = Self { a: fx, d: fy, ..Self::ID };
        let (s, c) = angle.sin_cos();
        let rot = Self { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 };
        let place = Self { e: pin.0, f: pin.1, ..Self::ID };
        translate_loc.then(flip).then(rot).then(place)
    }
}

fn fin(v: f64, lim: f64) -> f64 {
    if v.is_finite() { v.clamp(-lim, lim) } else { 0.0 }
}

#[allow(clippy::cast_possible_truncation)] // a coordinate on a page fits an f32
fn f32c(v: f64) -> f32 {
    fin(v, 1.0e7) as f32
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

/// The default colour table of Visio (indices 0 to 23), used when the file has none.
const DEFAULT_COLORS: [u32; 24] = [
    0x00_0000, 0xFF_FFFF, 0xFF_0000, 0x00_FF00, 0x00_00FF, 0xFF_FF00, 0xFF_00FF, 0x00_FFFF,
    0x80_0000, 0x00_8000, 0x00_0080, 0x80_8000, 0x80_0080, 0x00_8080, 0xC0_C0C0, 0xE6_E6E6,
    0xCD_CDCD, 0xB3_B3B3, 0x9A_9A9A, 0x80_8080, 0x66_6666, 0x4D_4D4D, 0x33_3333, 0x1A_1A1A,
];

/// Why a page is only approximate, and what is left out. Each reason once.
pub(crate) type Notes = Vec<&'static str>;

/// The result of drawing a page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rendered {
    pub(crate) page: PageModel,
    pub(crate) notes: Notes,
    /// Hidden shapes (or those on hidden layers) that were left out.
    pub(crate) hidden_omitted: u64,
    /// Hidden shapes that were drawn, faintly.
    pub(crate) hidden_shown: u64,
}

// ---- looking cells up ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grp {
    Own,
    Line,
    Fill,
    Text,
}

fn group_of(name: &str) -> Grp {
    match name {
        "LineWeight"
        | "LineColor"
        | "LinePattern"
        | "LineColorTrans"
        | "Rounding"
        | "BeginArrow"
        | "EndArrow"
        | "BeginArrowSize"
        | "EndArrowSize"
        | "QuickStyleLineColor"
        | "QuickStyleLineMatrix" => Grp::Line,
        "FillForegnd"
        | "FillBkgnd"
        | "FillPattern"
        | "FillForegndTrans"
        | "FillBkgndTrans"
        | "ShdwPattern"
        | "QuickStyleFillColor"
        | "QuickStyleFillMatrix"
        | "QuickStyleShadowColor" => Grp::Fill,
        "LeftMargin"
        | "RightMargin"
        | "TopMargin"
        | "BottomMargin"
        | "VerticalAlign"
        | "TextBkgnd"
        | "QuickStyleFontColor" => Grp::Text,
        _ => Grp::Own,
    }
}

/// Whether a cell holds `Themed`: its value comes from the theme, not from the file.
fn is_themed(v: &Val) -> bool {
    matches!(v, Val::Text(t) if t == "Themed")
}

/// The line pattern: the number in the cell; for a themed one, solid if the theme gives the shape
/// a line and none if it does not.
fn line_pattern(look: &Look<'_>) -> f64 {
    match look.cell("LinePattern") {
        Some(v) if is_themed(v) => {
            if look.theme_draws("QuickStyleLineMatrix") {
                1.0
            } else {
                0.0
            }
        }
        _ => look.num("LinePattern").unwrap_or(1.0),
    }
}

/// The fill pattern: the number in the cell; for a themed one, solid if the theme gives the shape
/// a fill and none if it does not.
fn fill_pattern(look: &Look<'_>) -> f64 {
    match look.cell("FillPattern") {
        Some(v) if is_themed(v) => {
            if look.theme_draws("QuickStyleFillMatrix") {
                1.0
            } else {
                0.0
            }
        }
        _ => look.num("FillPattern").unwrap_or(0.0),
    }
}

/// A sheet seen together with the master shape and the styles it inherits from.
#[derive(Clone, Copy)]
struct Look<'a> {
    doc: &'a Doc,
    sheet: &'a Sheet,
    target: Option<&'a Sheet>,
}

impl<'a> Look<'a> {
    fn style_id(&self, grp: Grp) -> Option<u32> {
        let pick = |s: &Sheet| match grp {
            Grp::Line => s.line_style,
            Grp::Fill => s.fill_style,
            Grp::Text => s.text_style,
            Grp::Own => None,
        };
        pick(self.sheet).or_else(|| self.target.and_then(pick))
    }

    /// The style sheets that apply for `grp`, nearest first.
    fn styles(&self, grp: Grp) -> Vec<&'a Sheet> {
        let mut out: Vec<&Sheet> = Vec::new();
        let mut next = self.style_id(grp);
        let mut seen: Vec<u32> = Vec::new();
        while let Some(id) = next {
            if out.len() >= MAX_STYLE_DEPTH || seen.contains(&id) {
                break;
            }
            seen.push(id);
            let Some(st) = self.doc.styles.get(&id) else { break };
            out.push(st);
            next = match grp {
                Grp::Line => st.line_style,
                Grp::Fill => st.fill_style,
                Grp::Text => st.text_style,
                Grp::Own => None,
            };
        }
        out
    }

    fn cell(&self, name: &str) -> Option<&'a Val> {
        if let Some(v) = cell(&self.sheet.cells, name) {
            return Some(v);
        }
        if let Some(v) = self.target.and_then(|t| cell(&t.cells, name)) {
            return Some(v);
        }
        let grp = group_of(name);
        if grp == Grp::Own {
            return None;
        }
        self.styles(grp).into_iter().find_map(|s| cell(&s.cells, name))
    }

    fn num(&self, name: &str) -> Option<f64> {
        match self.cell(name)? {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn color(&self, name: &str) -> Option<u32> {
        let v = self.cell(name)?;
        if is_themed(v) {
            return self.themed(name);
        }
        self.rgb(v)
    }

    /// The colour a `Themed` cell stands for: the slot of the theme that the shape's quick-style
    /// cell names (`QuickStyleLineColor` for a line, `QuickStyleFillColor` for a fill, ...).
    fn themed(&self, name: &str) -> Option<u32> {
        let quick = match name {
            "LineColor" => "QuickStyleLineColor",
            "FillForegnd" | "FillBkgnd" => "QuickStyleFillColor",
            "ShdwForegnd" => "QuickStyleShadowColor",
            "Color" => "QuickStyleFontColor",
            _ => return None,
        };
        let number = float_index(self.num(quick)?)?;
        // Text takes a scheme colour when the number names one, and the ordinary text colour (dark
        // 1) when it names a colour variation: a variation colour is for lines and fills, and the
        // files made by Visio store black for the text of an unstyled shape.
        if name == "Color" && number >= 100 {
            return self.doc.theme.color(0, 0);
        }
        self.doc.theme.color(number, 0)
    }

    /// Whether the theme gives the shape this kind of look at all: its quick-style matrix cell is
    /// not 0 (a text-only shape has 0 and draws neither a line nor a fill).
    fn theme_draws(&self, matrix: &str) -> bool {
        self.num(matrix).is_some_and(|n| n > 0.0)
    }

    /// A colour value: a literal colour, or an index into the colour table.
    fn rgb(&self, v: &Val) -> Option<u32> {
        match v {
            Val::Rgb(c) => Some(*c),
            Val::Num(n) => {
                let ix = float_index(*n)?;
                if self.doc.colors.is_empty() {
                    DEFAULT_COLORS.get(ix).copied()
                } else {
                    self.doc.colors.get(ix).copied()
                }
            }
            Val::Text(_) => None,
        }
    }

    /// A cell of the Character (`para` false) or Paragraph row `ix`.
    fn fmt(&self, para: bool, ix: u32, name: &str) -> Option<&'a Val> {
        let rows = |s: &'a Sheet| -> &'a [Fmt] { if para { &s.paras } else { &s.chars } };
        let find = |s: &'a Sheet, want: u32| {
            rows(s).iter().find(|r| r.ix == want).and_then(|r| cell(&r.cells, name))
        };
        if let Some(v) = find(self.sheet, ix) {
            return Some(v);
        }
        if let Some(v) = self.target.and_then(|t| find(t, ix)) {
            return Some(v);
        }
        for st in self.styles(Grp::Text) {
            if let Some(v) = find(st, ix).or_else(|| find(st, 0)) {
                return Some(v);
            }
        }
        None
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn float_index(n: f64) -> Option<usize> {
    (n.is_finite() && (0.0..=1.0e6).contains(&n)).then(|| n.round() as usize)
}

/// The on/off lengths of a Visio line pattern, in line widths (`None`: solid). Patterns 2 to 5
/// are dash, dot, dash-dot and dash-dot-dot; the longer ones are drawn as long dashes.
fn dash_lengths(pattern: f64) -> Option<&'static [f64]> {
    match float_index(pattern)? {
        0 | 1 => None,
        2 => Some(&[4.0, 3.0]),
        3 => Some(&[1.0, 2.0]),
        4 => Some(&[4.0, 2.0, 1.0, 2.0]),
        5 => Some(&[4.0, 2.0, 1.0, 2.0, 1.0, 2.0]),
        _ => Some(&[8.0, 3.0]),
    }
}

/// Cuts the stroked polylines of `ops` into dashes. Curves are flattened first. Returns `None`
/// where the result would be too big (the line is then drawn solid).
fn dash_path(ops: &[f32], pattern: &[f64], width: f32, limit: usize) -> Option<Vec<f32>> {
    type Pt = (f64, f64);
    // Flatten into polylines.
    let mut lines: Vec<Vec<Pt>> = Vec::new();
    let mut cur: Vec<Pt> = Vec::new();
    let mut i = 0usize;
    let num = |k: usize| ops.get(k).map(|v| f64::from(*v));
    let flush = |cur: &mut Vec<Pt>, lines: &mut Vec<Vec<Pt>>| {
        if cur.len() > 1 {
            lines.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    while let Some(&code) = ops.get(i) {
        if code == op::MOVE {
            flush(&mut cur, &mut lines);
            cur.push((num(i + 1)?, num(i + 2)?));
            i += 3;
        } else if code == op::LINE {
            cur.push((num(i + 1)?, num(i + 2)?));
            i += 3;
        } else if code == op::CURVE {
            let (x0, y0) = *cur.last()?;
            let (c1, c2, e) = (
                (num(i + 1)?, num(i + 2)?),
                (num(i + 3)?, num(i + 4)?),
                (num(i + 5)?, num(i + 6)?),
            );
            for s in 1..=12u8 {
                let t = f64::from(s) / 12.0;
                let u = 1.0 - t;
                let b = |a: f64, b1: f64, c: f64, d: f64| {
                    u * u * u * a + 3.0 * u * u * t * b1 + 3.0 * u * t * t * c + t * t * t * d
                };
                cur.push((b(x0, c1.0, c2.0, e.0), b(y0, c1.1, c2.1, e.1)));
            }
            i += 7;
        } else {
            // CLOSE: back to where the subpath began.
            if let Some(&first) = cur.first() {
                cur.push(first);
            }
            flush(&mut cur, &mut lines);
            i += 1;
        }
    }
    flush(&mut cur, &mut lines);

    let unit = f64::from(width.max(0.75));
    let period: f64 = pattern.iter().sum::<f64>() * unit;
    if period <= 0.0 {
        return None;
    }
    let mut out: Vec<f32> = Vec::new();
    for pts in &lines {
        let length: f64 = pts.windows(2).map(|w| segment(w).2).sum();
        if length / period * 6.0 * (pattern.len() as f64 / 2.0) > limit as f64 {
            return None;
        }
        let (mut k, mut left, mut on) = (0usize, pattern.first().copied()? * unit, true);
        let mut open = false;
        for w in pts.windows(2) {
            let (a, (dx, dy), seg) = segment(w);
            if seg < 1e-6 {
                continue;
            }
            let mut at = 0.0f64;
            while at < seg {
                let step = left.min(seg - at);
                let from = (a.0 + dx * at, a.1 + dy * at);
                let to = (from.0 + dx * step, from.1 + dy * step);
                if on {
                    if !open {
                        out.extend([op::MOVE, f32c(from.0), f32c(from.1)]);
                        open = true;
                    }
                    out.extend([op::LINE, f32c(to.0), f32c(to.1)]);
                }
                at += step;
                left -= step;
                if left <= 1e-9 {
                    k = (k + 1) % pattern.len();
                    left = pattern.get(k).copied()? * unit;
                    on = k % 2 == 0;
                    open = false;
                }
                if out.len() > limit {
                    return None;
                }
            }
        }
    }
    Some(out)
}

/// The start, unit direction and length of the segment between the first two points of `w`.
fn segment(w: &[(f64, f64)]) -> ((f64, f64), (f64, f64), f64) {
    let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else { return ((0.0, 0.0), (0.0, 0.0), 0.0) };
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    if len < 1e-12 {
        (a, (0.0, 0.0), 0.0)
    } else {
        (a, ((b.0 - a.0) / len, (b.1 - a.1) / len), len)
    }
}

// ---- the recorder -------------------------------------------------------------------------------

struct Painter<'a> {
    doc: &'a Doc,
    page: &'a PageDef,
    reveal_hidden: bool,
    prims: Vec<Prim>,
    notes: Notes,
    full: bool,
    approximate: bool,
    shapes_left: usize,
    segs_left: usize,
    numbers: usize,
    chars_left: usize,
    hidden_omitted: u64,
    hidden_shown: u64,
    /// Opacity of what is drawn now (less for hidden shapes that are shown).
    opacity: f64,
    /// The line pattern of the shape being drawn.
    pattern: f64,
    /// Half the length of lines that stand for infinite ones, in inches.
    reach: f64,
}

impl<'a> Painter<'a> {
    fn push(&mut self, p: Prim) {
        if self.prims.len() >= MAX_PRIMS {
            self.full = true;
        } else {
            self.prims.push(p);
        }
    }

    fn note(&mut self, why: &'static str) {
        self.approximate = true;
        if !self.notes.contains(&why) {
            self.notes.push(why);
        }
    }

    fn visible(&self, s: &Sheet) -> bool {
        if s.layers.is_empty() || self.page.layers.is_empty() {
            return true;
        }
        let mut known = false;
        for ix in &s.layers {
            match self.page.layers.iter().find(|l| l.ix == *ix) {
                Some(l) if l.visible => return true,
                Some(_) => known = true,
                None => return true,
            }
        }
        !known
    }

    /// Draws one shape and what it holds.
    fn shape(&mut self, s: &'a Sheet, parent: Mat, ctx: Option<u32>, depth: usize, angle: f64) {
        if depth >= MAX_DEPTH {
            self.full = true;
            return;
        }
        if self.shapes_left == 0 {
            self.full = true;
            return;
        }
        self.shapes_left -= 1;
        let saved_opacity = self.opacity;
        if !self.visible(s) {
            if self.reveal_hidden {
                self.hidden_shown += 1;
                self.opacity *= 0.35;
            } else {
                self.hidden_omitted += 1;
                return;
            }
        }
        let (target, mctx) = self.master_of(s, ctx);
        let look = Look { doc: self.doc, sheet: s, target };
        self.effects(&look);

        let w = fin(look.num("Width").unwrap_or(0.0), 1.0e6);
        let h = fin(look.num("Height").unwrap_or(0.0), 1.0e6);
        let pin = (
            fin(look.num("PinX").unwrap_or(0.0), 1.0e7),
            fin(look.num("PinY").unwrap_or(0.0), 1.0e7),
        );
        let loc = (
            fin(look.num("LocPinX").unwrap_or(w / 2.0), 1.0e7),
            fin(look.num("LocPinY").unwrap_or(h / 2.0), 1.0e7),
        );
        let ang = fin(look.num("Angle").unwrap_or(0.0), 1.0e3);
        let flip_x = look.num("FlipX").unwrap_or(0.0) != 0.0;
        let flip_y = look.num("FlipY").unwrap_or(0.0) != 0.0;
        let m = Mat::shape(pin, loc, ang, flip_x, flip_y).then(parent);
        let angle_sum = angle + ang;

        let kind = if s.kind == Kind::Shape { target.map_or(s.kind, |t| t.kind) } else { s.kind };
        if kind == Kind::Foreign || s.foreign.is_some() {
            self.picture(&look, m, w, h);
        }
        if kind != Kind::Guide {
            self.geometry(&look, m, w, h, parent);
        }
        if depth + 1 < MAX_DEPTH {
            for k in &s.kids {
                if self.full && self.shapes_left == 0 {
                    break;
                }
                self.shape(k, m, mctx, depth + 1, angle_sum);
            }
        }
        if look.num("HideText").unwrap_or(0.0) == 0.0 {
            self.text(&look, m, w, h, angle_sum);
        }
        self.opacity = saved_opacity;
    }

    /// The master shape a sheet inherits from, and the master its children belong to.
    fn master_of(&self, s: &Sheet, ctx: Option<u32>) -> (Option<&'a Sheet>, Option<u32>) {
        let id = s.master.or(ctx);
        let Some(m) = id.and_then(|i| self.doc.masters.get(&i)) else {
            return (None, ctx);
        };
        let target = match (s.master_shape, s.master) {
            (Some(ms), _) => m.find(ms),
            (None, Some(_)) => m.top(),
            (None, None) => None,
        };
        (target, Some(m.id))
    }

    fn effects(&mut self, look: &Look<'_>) {
        let t = look.target;
        if look.sheet.effect_3d || t.is_some_and(|t| t.effect_3d) {
            self.note("3-D effects of shapes (bevel, extrusion, rotation) are not drawn");
        }
        if look.sheet.effect_other || t.is_some_and(|t| t.effect_other) {
            self.note("shape effects (glow, soft edges, reflection) are not drawn");
        }
        if look.num("ShdwPattern").is_some_and(|p| p > 0.0) {
            self.note("shadows are not drawn");
        }
    }

    // ---- geometry ----

    fn merged_geoms(look: &Look<'a>) -> Vec<Cow<'a, Geom>> {
        let own = &look.sheet.geoms;
        let inherited: &[Geom] = look.target.map_or(&[], |t| t.geoms.as_slice());
        let mut out: Vec<Cow<'a, Geom>> = Vec::new();
        if inherited.is_empty() {
            out.extend(own.iter().map(Cow::Borrowed));
        } else {
            for base in inherited {
                match own.iter().find(|g| g.ix == base.ix) {
                    None => out.push(Cow::Borrowed(base)),
                    Some(g) => out.push(Cow::Owned(merge_geom(base, g))),
                }
            }
            for g in own {
                if !inherited.iter().any(|b| b.ix == g.ix) {
                    out.push(Cow::Borrowed(g));
                }
            }
        }
        out.retain(|g| !g.del);
        out.sort_by_key(|g| g.ix);
        out
    }

    fn geometry(&mut self, look: &Look<'a>, m: Mat, w: f64, h: f64, parent: Mat) {
        let geoms = Self::merged_geoms(look);
        let line_pt = self.line_width(look);
        let (line_color, line_on) = self.line_color(look);
        self.pattern = line_pattern(look);
        let (fill_color, fill_on) = self.fill_color(look);
        if !geoms.is_empty() && (line_on || fill_on) {
            self.style_notes(look);
        }
        let mut first_path: Option<Vec<(f64, f64)>> = None;
        for g in &geoms {
            if g.no_show == Some(true) {
                continue;
            }
            let mut left = self.segs_left;
            let mut b = Builder::new(w, h, self.reach, &mut left);
            for r in &g.rows {
                b.row(r);
            }
            b.finish();
            let (segs, flags) = (std::mem::take(&mut b.segs), b.flags);
            self.segs_left = left;
            // `Rounding` (inches) rounds the corners where straight lines meet.
            let segs = match look.num("Rounding") {
                Some(r) if r > 0.0 => round_corners(&segs, r),
                _ => segs,
            };
            self.flag_notes(flags);
            let want_fill = fill_on && g.no_fill != Some(true);
            let want_line = line_on && g.no_line != Some(true);
            if first_path.is_none() {
                first_path = Some(points_of(&segs, m));
            }
            self.emit_path(&segs, m, want_fill, want_line, fill_color, line_color, line_pt);
        }
        if geoms.is_empty() && look.cell("BeginX").is_some() && look.cell("EndX").is_some() {
            self.straight(look, parent, line_pt, line_color, line_on);
        }
        if line_on && let Some(pts) = first_path {
            self.arrows(look, &pts, line_pt, line_color);
        }
    }

    fn flag_notes(&mut self, f: Flags) {
        if f.nurbs {
            self.note("NURBS curves are drawn as sampled polylines");
        }
        if f.spline {
            self.note("splines are drawn as sampled polylines");
        }
        if f.bad_formula {
            self.note("a polyline or curve formula that is not plain numbers is drawn as a line");
        }
        if f.degree_capped {
            self.note("curves of a degree above 8 are drawn with degree 8");
        }
        if f.cut {
            self.full = true;
        }
    }

    fn style_notes(&mut self, look: &Look<'_>) {
        if look.cell("FillPattern").is_some_and(is_themed)
            || look.cell("LineColor").is_some_and(is_themed)
        {
            self.note("theme styles are drawn with the theme colours only");
        }
        if dash_lengths(line_pattern(look)).is_some() {
            self.note("dash patterns are approximated by regular dashes");
        }
        if look.num("Rounding").is_some_and(|r| r > 0.0) {
            self.note("rounded corners are drawn approximately");
        }
        if fill_pattern(look) > 1.0 {
            self.note("fill patterns and gradients are drawn as flat colours");
        }
    }

    fn line_width(&self, look: &Look<'_>) -> f64 {
        let w = look.num("LineWeight").unwrap_or(1.0 / PT);
        fin(w, 100.0).abs().max(0.0) * PT
    }

    /// The stroke colour with opacity, and whether there is a stroke.
    fn line_color(&self, look: &Look<'_>) -> (u32, bool) {
        let pattern = line_pattern(look);
        let rgb = look.color("LineColor").unwrap_or(0);
        let trans = look.num("LineColorTrans").unwrap_or(0.0).clamp(0.0, 1.0);
        let c = 0xFF00_0000 | rgb;
        (fade(fade(c, 1.0 - trans), self.opacity), pattern >= 1.0)
    }

    fn fill_color(&self, look: &Look<'_>) -> (u32, bool) {
        let pattern = fill_pattern(look);
        if pattern < 1.0 {
            return (0, false);
        }
        let fg = look.color("FillForegnd").unwrap_or(0xFF_FFFF);
        // A themed background is the foreground colour: the theme gives one fill colour.
        let bg = if look.cell("FillBkgnd").is_some_and(is_themed) {
            fg
        } else {
            look.color("FillBkgnd").unwrap_or(0xFF_FFFF)
        };
        let rgb = if pattern > 1.0 && pattern < 25.0 { mix(fg, bg) } else { fg };
        let trans = look.num("FillForegndTrans").unwrap_or(0.0).clamp(0.0, 1.0);
        let c = 0xFF00_0000 | rgb;
        (fade(fade(c, 1.0 - trans), self.opacity), true)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_path(
        &mut self,
        segs: &[Seg],
        m: Mat,
        want_fill: bool,
        want_line: bool,
        fill: u32,
        line: u32,
        width_pt: f64,
    ) {
        if segs.is_empty() || (!want_fill && !want_line) {
            return;
        }
        let closed = is_closed(segs);
        let fill_on = want_fill && closed;
        if !fill_on && !want_line {
            return;
        }
        let mut ops: Vec<f32> = Vec::with_capacity(segs.len() * 4);
        let map = |x: f64, y: f64| m.apply(x, y);
        for s in segs {
            if ops.len() + 7 > MAX_PATH_NUMBERS {
                self.full = true;
                break;
            }
            match *s {
                Seg::M(x, y) => {
                    let p = map(x, y);
                    ops.extend([op::MOVE, f32c(p.0), f32c(p.1)]);
                }
                Seg::L(x, y) => {
                    let p = map(x, y);
                    ops.extend([op::LINE, f32c(p.0), f32c(p.1)]);
                }
                Seg::C(c) => {
                    let p1 = map(c[0], c[1]);
                    let p2 = map(c[2], c[3]);
                    let p3 = map(c[4], c[5]);
                    ops.extend([
                        op::CURVE,
                        f32c(p1.0),
                        f32c(p1.1),
                        f32c(p2.0),
                        f32c(p2.1),
                        f32c(p3.0),
                        f32c(p3.1),
                    ]);
                }
                Seg::Z => ops.push(op::CLOSE),
            }
        }
        if self.numbers.saturating_add(ops.len()) > MAX_SEGMENTS * 3 {
            self.full = true;
            return;
        }
        self.numbers += ops.len();
        let width = f32c(width_pt.max(0.25));
        // A dashed line: the fill (if any) first, then the dashes as a path of their own.
        let dashes = if want_line {
            dash_lengths(self.pattern).and_then(|pat| dash_path(&ops, pat, width, MAX_PATH_NUMBERS))
        } else {
            None
        };
        if let Some(dashes) = dashes {
            if fill_on {
                self.push(Prim::Path { ops, fill, stroke: 0, width, even_odd: true });
            }
            if self.numbers.saturating_add(dashes.len()) > MAX_SEGMENTS * 3 {
                self.full = true;
                return;
            }
            self.numbers += dashes.len();
            self.push(Prim::Path { ops: dashes, fill: 0, stroke: line, width, even_odd: false });
            return;
        }
        self.push(Prim::Path {
            ops,
            fill: if fill_on { fill } else { 0 },
            stroke: if want_line { line } else { 0 },
            width,
            even_odd: true,
        });
    }

    /// A one-dimensional shape without geometry: the straight line from begin to end (in the
    /// coordinates of its parent).
    fn straight(&mut self, look: &Look<'_>, parent: Mat, width_pt: f64, line: u32, on: bool) {
        if !on {
            return;
        }
        let g = |n: &str| fin(look.num(n).unwrap_or(0.0), 1.0e7);
        let segs = [Seg::M(g("BeginX"), g("BeginY")), Seg::L(g("EndX"), g("EndY"))];
        self.emit_path(&segs, parent, false, true, 0, line, width_pt);
    }

    fn arrows(&mut self, look: &Look<'_>, pts: &[(f64, f64)], width_pt: f64, color: u32) {
        let begin = look.num("BeginArrow").unwrap_or(0.0) > 0.0;
        let end = look.num("EndArrow").unwrap_or(0.0) > 0.0;
        if !begin && !end {
            return;
        }
        self.note("arrowheads are drawn as plain triangles");
        let size_of = |name: &str| -> f64 {
            const SIZES: [f64; 7] = [2.0, 2.5, 3.0, 4.0, 5.5, 7.0, 9.0];
            let ix = float_index(look.num(name).unwrap_or(2.0)).unwrap_or(2).min(6);
            SIZES.get(ix).copied().unwrap_or(3.0) * width_pt.max(0.75)
        };
        if end && let [.., from, tip] = pts {
            self.arrow(*tip, *from, size_of("EndArrowSize"), color);
        }
        if begin && let [tip, from, ..] = pts {
            self.arrow(*tip, *from, size_of("BeginArrowSize"), color);
        }
    }

    /// A filled triangle at `tip`, pointing away from `from`, `len` points long.
    fn arrow(&mut self, tip: (f64, f64), from: (f64, f64), len: f64, color: u32) {
        let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
        let d = dx.hypot(dy);
        if d < 1e-9 {
            return;
        }
        let (ux, uy) = (dx / d, dy / d);
        let base = (tip.0 - ux * len, tip.1 - uy * len);
        let half = len * 0.4;
        let (l, r) =
            ((base.0 - uy * half, base.1 + ux * half), (base.0 + uy * half, base.1 - ux * half));
        self.push(Prim::Path {
            ops: vec![
                op::MOVE,
                f32c(tip.0),
                f32c(tip.1),
                op::LINE,
                f32c(l.0),
                f32c(l.1),
                op::LINE,
                f32c(r.0),
                f32c(r.1),
                op::CLOSE,
            ],
            fill: color,
            stroke: 0,
            width: 0.0,
            even_odd: false,
        });
    }

    fn picture(&mut self, look: &Look<'_>, m: Mat, w: f64, h: f64) {
        let label = match look
            .sheet
            .foreign
            .as_deref()
            .or(look.target.and_then(|t| t.foreign.as_deref()))
        {
            Some(k) if !k.is_empty() => format!("picture ({k})"),
            _ => "picture".to_owned(),
        };
        // The unit square is mapped onto the shape's box.
        let sq = Mat { a: w, d: h, ..Mat::ID }.then(m);
        self.push(Prim::Image {
            m: [f32c(sq.a), f32c(sq.b), f32c(sq.c), f32c(sq.d), f32c(sq.e), f32c(sq.f)],
            label,
            id: IMAGE_NONE,
        });
        self.note("pictures and embedded objects are shown as placeholders");
    }

    // ---- text ----

    fn text(&mut self, look: &Look<'a>, m: Mat, w: f64, h: f64, angle_sum: f64) {
        let pieces: &[super::model::Piece] = if look.sheet.text.is_empty() {
            look.target.map_or(&[], |t| t.text.as_slice())
        } else {
            &look.sheet.text
        };
        if pieces.iter().all(|p| p.text.trim().is_empty()) {
            return;
        }
        // The text block, in the shape's coordinates.
        let tw = fin(look.num("TxtWidth").unwrap_or(w), 1.0e6).max(0.0);
        let th = fin(look.num("TxtHeight").unwrap_or(h), 1.0e6).max(0.0);
        let tpin = (
            fin(look.num("TxtPinX").unwrap_or(w / 2.0), 1.0e7),
            fin(look.num("TxtPinY").unwrap_or(h / 2.0), 1.0e7),
        );
        let tloc = (
            fin(look.num("TxtLocPinX").unwrap_or(tw / 2.0), 1.0e7),
            fin(look.num("TxtLocPinY").unwrap_or(th / 2.0), 1.0e7),
        );
        let tang = fin(look.num("TxtAngle").unwrap_or(0.0), 1.0e3);
        let tm = Mat::shape(tpin, tloc, tang, false, false).then(m);
        let margin = |n: &str| fin(look.num(n).unwrap_or(0.0), 1.0e6).max(0.0);
        let (ml, mr, mt, mb) = (
            margin("LeftMargin"),
            margin("RightMargin"),
            margin("TopMargin"),
            margin("BottomMargin"),
        );
        let avail = (tw - ml - mr).max(0.0);
        let rect_h = (th - mt - mb).max(0.0);

        let paras = self.paragraphs(look, pieces);
        let lay = layout(&paras, avail * PT, self.chars_left);
        self.chars_left = self.chars_left.saturating_sub(lay.chars);
        if lay.cut {
            self.full = true;
        }
        if lay.lines.is_empty() {
            return;
        }
        let valign = look.num("VerticalAlign").unwrap_or(1.0);
        let block_h: f64 = lay.lines.iter().map(|l| l.height).sum::<f64>() / PT;
        // Top of the first line, in the text block's coordinates (y up).
        let top = if valign < 0.5 {
            th - mt
        } else if valign > 1.5 {
            mb + block_h
        } else {
            mb + rect_h / 2.0 + block_h / 2.0
        };
        let page_angle = -(angle_sum + tang);
        let (ca, sa) = (page_angle.cos(), page_angle.sin());
        let mut y = top;
        for line in &lay.lines {
            let baseline = y - line.ascent / PT;
            y -= line.height / PT;
            let line_w = line.width / PT;
            let x_start = match line.align {
                Align::Left => ml,
                Align::Right => ml + (avail - line_w).max(0.0),
                Align::Center => ml + (avail - line_w) / 2.0,
            };
            let center_x = x_start + line_w / 2.0;
            let (cx, cy) = tm.apply(center_x, baseline);
            let mut off = -line.width / 2.0;
            for run in &line.runs {
                let sx = cx + ca * off;
                let sy = cy + sa * off;
                self.push(Prim::Text {
                    x: f32c(sx),
                    y: f32c(sy),
                    size: f32c(run.size),
                    angle: f32c(page_angle),
                    width: f32c(run.width),
                    style: run.style,
                    color: fade(run.color, self.opacity),
                    text: run.text.clone(),
                });
                off += run.width;
            }
        }
    }

    fn paragraphs(&mut self, look: &Look<'a>, pieces: &[super::model::Piece]) -> Vec<Para> {
        let mut paras: Vec<Para> = Vec::new();
        let mut cur: Option<Para> = None;
        for p in pieces {
            let fmt = self.run_fmt(look, p.char_ix);
            let align = match look.fmt(true, p.para_ix, "HorzAlign").and_then(|v| match v {
                Val::Num(n) => Some(*n),
                _ => None,
            }) {
                Some(a) if a < 0.5 => Align::Left,
                Some(a) if a > 1.5 && a < 2.5 => Align::Right,
                _ => Align::Center,
            };
            let mut rest = p.text.as_str();
            loop {
                let (head, tail) = match rest.find(['\n', '\r', '\u{b}', '\u{2028}']) {
                    Some(i) => (rest.get(..i).unwrap_or(""), Some(rest.get(i..).unwrap_or(""))),
                    None => (rest, None),
                };
                let para = cur.get_or_insert_with(|| Para { runs: Vec::new(), align });
                if !head.is_empty() {
                    para.runs.push((head.to_owned(), fmt));
                }
                match tail {
                    Some(t) => {
                        if let Some(done) = cur.take() {
                            paras.push(done);
                        } else {
                            paras.push(Para { runs: Vec::new(), align });
                        }
                        let mut chars = t.chars();
                        chars.next();
                        rest = chars.as_str();
                        // A new paragraph starts after the break.
                        cur = Some(Para { runs: Vec::new(), align });
                        if paras.len() >= MAX_LINES {
                            return paras;
                        }
                    }
                    None => break,
                }
            }
        }
        if let Some(c) = cur {
            paras.push(c);
        }
        // Clean the text the way every string of the preview is.
        let mut counts = CleanCounts::default();
        for p in &mut paras {
            for (t, _) in &mut p.runs {
                let cleaned = clean_text(t, &mut counts);
                if cleaned != *t {
                    *t = cleaned.into_owned();
                }
            }
        }
        paras
    }

    fn run_fmt(&self, look: &Look<'_>, ix: u32) -> RunFmt {
        let n = |name: &str| match look.fmt(false, ix, name) {
            Some(Val::Num(v)) => Some(*v),
            _ => None,
        };
        let size = fin(n("Size").unwrap_or(8.0 / PT), 1.0e3).abs() * PT;
        let size = if size < 1.0 { 8.0 } else { size.min(1000.0) };
        let bits = float_index(n("Style").unwrap_or(0.0)).unwrap_or(0);
        let mut style = 0u8;
        if bits & 1 != 0 {
            style |= pstyle::BOLD;
        }
        if bits & 2 != 0 {
            style |= pstyle::ITALIC;
        }
        let rgb = look
            .fmt(false, ix, "Color")
            .and_then(|v| if is_themed(v) { look.themed("Color") } else { look.rgb(v) })
            .unwrap_or(0);
        let trans = n("ColorTrans").unwrap_or(0.0).clamp(0.0, 1.0);
        RunFmt { size, style, color: fade(0xFF00_0000 | rgb, 1.0 - trans) }
    }
}

fn mix(a: u32, b: u32) -> u32 {
    let ch = |s: u32| (((a >> s) & 0xFF) + ((b >> s) & 0xFF)) / 2;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

fn merge_geom(base: &Geom, own: &Geom) -> Geom {
    let mut rows: Vec<Row> = Vec::new();
    for br in &base.rows {
        match own.rows.iter().find(|r| r.ix == br.ix) {
            None => rows.push(br.clone()),
            Some(r) => {
                let mut cells: Cells = r.cells.clone();
                for (n, v) in &br.cells {
                    if cell(&r.cells, n).is_none() {
                        cells.push((n, v.clone()));
                    }
                }
                rows.push(Row {
                    kind: r.kind,
                    ix: r.ix,
                    del: r.del,
                    cells,
                    args: r.args.clone().or_else(|| br.args.clone()),
                });
            }
        }
    }
    for r in &own.rows {
        if !base.rows.iter().any(|b| b.ix == r.ix) {
            rows.push(r.clone());
        }
    }
    Geom {
        ix: own.ix,
        no_fill: own.no_fill.or(base.no_fill),
        no_line: own.no_line.or(base.no_line),
        no_show: own.no_show.or(base.no_show),
        del: own.del,
        rows,
    }
}

#[allow(unused_assignments)]
fn is_closed(segs: &[Seg]) -> bool {
    let mut start = (0.0f64, 0.0f64);
    let mut cur = (0.0f64, 0.0f64);
    let mut drawn = false;
    for s in segs {
        match *s {
            Seg::M(x, y) => {
                start = (x, y);
                cur = (x, y);
                drawn = false;
            }
            Seg::L(x, y) => {
                cur = (x, y);
                drawn = true;
            }
            Seg::C(c) => {
                cur = (c[4], c[5]);
                drawn = true;
            }
            Seg::Z => return true,
        }
        if drawn && (start.0 - cur.0).abs() < 1e-9 && (start.1 - cur.1).abs() < 1e-9 {
            return true;
        }
    }
    false
}

/// The vertices of a path in page coordinates (control points of curves included).
fn points_of(segs: &[Seg], m: Mat) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for s in segs {
        match *s {
            Seg::M(x, y) | Seg::L(x, y) => out.push(m.apply(x, y)),
            Seg::C(c) => {
                out.push(m.apply(c[2], c[3]));
                out.push(m.apply(c[4], c[5]));
            }
            Seg::Z => {}
        }
    }
    out
}

// ---- text layout --------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct RunFmt {
    size: f64,
    style: u8,
    color: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

struct Para {
    runs: Vec<(String, RunFmt)>,
    align: Align,
}

struct LaidRun {
    text: String,
    size: f64,
    style: u8,
    color: u32,
    /// Estimated width in points.
    width: f64,
}

struct Line {
    runs: Vec<LaidRun>,
    align: Align,
    /// Width in points.
    width: f64,
    /// Height of the line in points.
    height: f64,
    /// Distance from the top of the line to the baseline, in points.
    ascent: f64,
}

struct Laid {
    lines: Vec<Line>,
    chars: usize,
    cut: bool,
}

/// The estimated advance of a character in em.
fn advance(c: char, bold: bool) -> f64 {
    let base = match c {
        ' ' => 0.28,
        'i' | 'l' | 'I' | 'j' | '.' | ',' | ':' | ';' | '\'' | '|' | '!' => 0.27,
        'm' | 'w' | 'M' | 'W' => 0.82,
        c if c.is_ascii_uppercase() => 0.66,
        c if c.is_ascii() => 0.52,
        c if (c as u32) >= 0x2E80 => 1.0,
        _ => 0.56,
    };
    if bold { base * 1.06 } else { base }
}

fn text_width(s: &str, f: RunFmt) -> f64 {
    let bold = f.style & pstyle::BOLD != 0;
    s.chars().map(|c| advance(c, bold)).sum::<f64>() * f.size
}

/// Lays paragraphs out in lines no wider than `max_w` points (no wrapping when it is zero).
fn layout(paras: &[Para], max_w: f64, chars_left: usize) -> Laid {
    let mut out = Laid { lines: Vec::new(), chars: 0, cut: false };
    for p in paras {
        // Words with their format; a word keeps the space that follows it.
        let mut words: Vec<(String, RunFmt)> = Vec::new();
        for (t, f) in &p.runs {
            let mut cur = String::new();
            for c in t.chars() {
                cur.push(c);
                if c == ' ' {
                    words.push((std::mem::take(&mut cur), *f));
                }
            }
            if !cur.is_empty() {
                words.push((cur, *f));
            }
        }
        let default = RunFmt { size: 8.0, style: 0, color: 0xFF00_0000 };
        let mut line =
            Line { runs: Vec::new(), align: p.align, width: 0.0, height: 0.0, ascent: 0.0 };
        let finish = |line: &mut Line, out: &mut Laid, fallback: RunFmt| {
            if line.runs.is_empty() {
                // An empty line still takes the height of its paragraph's text.
                line.height = fallback.size * 1.2;
                line.ascent = fallback.size * 0.95;
            }
            let done = Line {
                runs: std::mem::take(&mut line.runs),
                align: line.align,
                width: line.width,
                height: line.height,
                ascent: line.ascent,
            };
            out.lines.push(done);
            line.width = 0.0;
            line.height = 0.0;
            line.ascent = 0.0;
        };
        let fallback = p.runs.first().map_or(default, |r| r.1);
        for (w, f) in words {
            out.chars += w.chars().count();
            if out.chars > chars_left || out.lines.len() >= MAX_LINES {
                out.cut = true;
                return out;
            }
            let wid = text_width(&w, f);
            let trimmed = text_width(w.trim_end(), f);
            if max_w > 0.0 && !line.runs.is_empty() && line.width + trimmed > max_w {
                finish(&mut line, &mut out, fallback);
            }
            line.height = line.height.max(f.size * 1.2);
            line.ascent = line.ascent.max(f.size * 0.95);
            line.width += wid;
            // Merge with the previous run when the format is the same.
            match line.runs.last_mut() {
                Some(last)
                    if last.size == f.size && last.style == f.style && last.color == f.color =>
                {
                    last.text.push_str(&w);
                    last.width += wid;
                }
                _ => line.runs.push(LaidRun {
                    text: w,
                    size: f.size,
                    style: f.style,
                    color: f.color,
                    width: wid,
                }),
            }
        }
        // The trailing space of the line is not part of its width.
        if let Some(last) = line.runs.last_mut() {
            let t = last.text.trim_end().to_owned();
            if t.len() != last.text.len() {
                let f = RunFmt { size: last.size, style: last.style, color: last.color };
                let nw = text_width(&t, f);
                line.width -= last.width - nw;
                last.width = nw;
                last.text = t;
            }
        }
        finish(&mut line, &mut out, fallback);
    }
    out
}

// ---- the page -----------------------------------------------------------------------------------

/// The size of a page and the scale from drawing units to points.
fn page_geometry(page: &PageDef) -> (f64, f64, f64) {
    let side = |name: &str, default: f64| {
        let v = num(&page.sheet, name).unwrap_or(default) * PT;
        if v.is_finite() && (MIN_SIDE..=MAX_SIDE).contains(&v) { v } else { default * PT }
    };
    let w = side("PageWidth", 8.5);
    let h = side("PageHeight", 11.0);
    let ps = num(&page.sheet, "PageScale").filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
    let ds = num(&page.sheet, "DrawingScale").filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
    let k = (ps / ds).clamp(1.0e-4, 1.0e4);
    (w, h, k)
}

/// Draws page `page` of `doc` (with the background page under it, if any).
pub(crate) fn draw_page<'a>(
    doc: &'a Doc,
    page: &'a PageDef,
    back: Option<&'a PageDef>,
    index: u32,
    pages: u32,
    reveal_hidden: bool,
) -> Rendered {
    let (w_pt, h_pt, k) = page_geometry(page);
    let mut p = Painter {
        doc,
        page,
        reveal_hidden,
        prims: Vec::new(),
        notes: Vec::new(),
        full: false,
        approximate: false,
        shapes_left: MAX_SHAPES,
        segs_left: MAX_SEGMENTS,
        numbers: 0,
        pattern: 1.0,
        chars_left: MAX_TEXT_CHARS,
        hidden_omitted: 0,
        hidden_shown: 0,
        opacity: 1.0,
        reach: (w_pt.max(h_pt) / PT / k) * 2.0,
    };
    // Drawing units to points, y flipped so that the origin is the top left of the page.
    let view = Mat { a: k * PT, b: 0.0, c: 0.0, d: -k * PT, e: 0.0, f: h_pt };
    let mut truncated = page.truncated;
    for src in back.into_iter().chain(std::iter::once(page)) {
        truncated |= src.truncated;
        p.page = src;
        for s in &src.shapes {
            if p.full && p.shapes_left == 0 {
                break;
            }
            p.shape(s, view, None, 0, 0.0);
        }
    }
    let approximate = p.approximate || !doc.unverified.is_empty();
    Rendered {
        page: PageModel {
            width: f32c(w_pt),
            height: f32c(h_pt),
            index,
            pages,
            prims: p.prims,
            truncated: p.full || truncated,
            approximate,
        },
        notes: p.notes,
        hidden_omitted: p.hidden_omitted,
        hidden_shown: p.hidden_shown,
    }
}
