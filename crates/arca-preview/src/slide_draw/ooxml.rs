//! PresentationML (`.pptx` and its variants) drawn as pages.
//!
//! The slide, its layout and its master are read into small trees (see [`super::parse`]); the
//! layout and the master supply the shapes under the slide's, the positions and look of
//! placeholders, the text styles and the theme colours. Everything is looked up by local name and
//! by *unprefixed* attribute (`x`, `cx`, `val`); relationship ids are read only from the attribute
//! bound to the relationships namespace.

use std::collections::{BTreeMap, BTreeSet};

use super::presets::{self, arc_to};
use super::{
    Align, Anchor, Canvas, CellDraw, Mat, Node, Outcome, Para, Piece, Rendered, RunStyle, Seg,
    Spacing, TextBody, draw_cells, draw_laid, f32c, family_of, fin, from_hsl, layout, num, parse,
    root_attr, side, to_hsl,
};
use crate::model::IMAGE_NONE;
use crate::opc::{self, Package};
use crate::page::Prim;
use crate::{PreviewError, PreviewPolicy};

/// Elements whose text is kept: runs and the table style id.
const KEEP_TEXT: [&str; 2] = ["t", "tableStyleId"];
/// Elements whose relationship attributes are read.
const REL_ELEMS: [&str; 3] = ["blip", "sldId", "relIds"];
/// Most rows and columns of a table that are drawn, and most cells.
const MAX_TABLE_LINE: usize = 500;
const MAX_TABLE_CELLS: usize = 10_000;
/// Slide size when the presentation names none: 4:3, 10 × 7.5 inches.
const DEFAULT_SIZE: (f64, f64) = (720.0, 540.0);

// ---- small readers ---------------------------------------------------------------------------

/// A length in EMU as points.
fn emu_pt(v: Option<&str>) -> f64 {
    let n = v.and_then(num).unwrap_or(0.0);
    fin(n.clamp(-1.0e11, 1.0e11) / 12_700.0)
}

/// A percentage in 1/100000 as a fraction.
fn frac(v: Option<&str>) -> Option<f64> {
    v.and_then(num).map(|n| n.clamp(-1.0e9, 1.0e9) / 100_000.0)
}

fn on(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("1" | "true" | "on"))
}

fn off(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("0" | "false" | "off"))
}

fn hex6(s: &str) -> Option<u32> {
    let s = s.trim();
    (s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
        .map(|v| 0xFF00_0000 | v)
}

fn preset_color(name: &str) -> Option<u32> {
    let v = match name {
        "black" => 0x00_0000,
        "white" => 0xFF_FFFF,
        "red" => 0xFF_0000,
        "green" => 0x00_8000,
        "lime" => 0x00_FF00,
        "blue" => 0x00_00FF,
        "yellow" => 0xFF_FF00,
        "cyan" | "aqua" => 0x00_FFFF,
        "magenta" | "fuchsia" => 0xFF_00FF,
        "gray" | "grey" => 0x80_8080,
        "dkGray" | "dkGrey" => 0xA9_A9A9,
        "ltGray" | "ltGrey" => 0xD3_D3D3,
        "orange" => 0xFF_A500,
        "purple" => 0x80_0080,
        "brown" => 0xA5_2A2A,
        "pink" => 0xFF_C0CB,
        "navy" => 0x00_0080,
        "maroon" => 0x80_0000,
        _ => return None,
    };
    Some(0xFF00_0000 | v)
}

/// Slot of a theme colour name in [`Look::colors`].
fn slot(name: &str) -> Option<usize> {
    Some(match name {
        "dk1" => 0,
        "lt1" => 1,
        "dk2" => 2,
        "lt2" => 3,
        "accent1" => 4,
        "accent2" => 5,
        "accent3" => 6,
        "accent4" => 7,
        "accent5" => 8,
        "accent6" => 9,
        "hlink" => 10,
        "folHlink" => 11,
        _ => return None,
    })
}

/// The colours of the Office theme, for a presentation without a readable one.
const DEFAULT_COLORS: [u32; 12] = [
    0xFF00_0000,
    0xFFFF_FFFF,
    0xFF1F_497D,
    0xFFEE_ECE1,
    0xFF4F_81BD,
    0xFFC0_504D,
    0xFF9B_BB59,
    0xFF80_64A2,
    0xFF4B_ACC6,
    0xFFF7_9646,
    0xFF00_00FF,
    0xFF80_0080,
];

fn channels(c: u32) -> [f64; 3] {
    [
        f64::from((c >> 16) & 255) / 255.0,
        f64::from((c >> 8) & 255) / 255.0,
        f64::from(c & 255) / 255.0,
    ]
}

fn pack(rgb: [f64; 3], alpha: f64) -> u32 {
    crate::page::argb(rgb[0], rgb[1], rgb[2], alpha)
}

// ---- text properties -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Sp {
    Pct(f64),
    Pts(f64),
}

#[derive(Debug, Clone, PartialEq)]
enum Bu {
    None,
    Char(String),
    Auto(String, u32),
}

/// The properties one level of a text style can set; `None` is "not set here".
#[derive(Debug, Clone, Default, PartialEq)]
struct Lvl {
    size: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    color: Option<u32>,
    mono: Option<bool>,
    serif: Option<bool>,
    shift: Option<i8>,
    algn: Option<Align>,
    mar_l: Option<f64>,
    indent: Option<f64>,
    bullet: Option<Bu>,
    before: Option<Sp>,
    after: Option<Sp>,
    line: Option<Sp>,
}

impl Lvl {
    /// Lets the properties set in `o` win.
    fn over(&mut self, o: &Self) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f.clone_from(&o.$f); } )* };
        }
        take!(
            size, bold, italic, underline, strike, color, mono, serif, shift, algn, mar_l, indent,
            bullet, before, after, line
        );
    }
}

type Levels = [Lvl; 9];

fn no_levels() -> Levels {
    std::array::from_fn(|_| Lvl::default())
}

// ---- the look: theme, colour map, text styles --------------------------------------------------

struct Look {
    colors: [u32; 12],
    map: Vec<(String, String)>,
    major: String,
    minor: String,
    fills: Vec<Node>,
    bg_fills: Vec<Node>,
    lines: Vec<Node>,
    default_lvls: Levels,
    title: Levels,
    body: Levels,
    other: Levels,
}

/// A fill or a line paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paint {
    Unset,
    None,
    Color(u32),
    /// A picture fill (`blipFill`): the caller reads the picture from the element and draws it,
    /// or says that it cannot.
    Picture,
}

/// Whether the picture fill of the first property element in `chain` that has one is tiled or
/// cropped (`a:tile`, or an `a:srcRect` with an edge), which is not applied.
fn tiled_or_cropped(chain: &[&Node]) -> bool {
    chain.iter().find_map(|p| p.child("blipFill")).is_some_and(|b| {
        b.child("tile").is_some()
            || b.child("srcRect").is_some_and(|r| {
                ["l", "t", "r", "b"].iter().any(|k| r.attr(k).is_some_and(|v| v != "0"))
            })
    })
}

/// The picture a fill element holds: `holder` is the node whose child is `blipFill` (a shape's
/// `spPr`, a background's `bgPr`); [`IMAGE_NONE`] if there is none, it is not embedded, or the
/// relationship does not lead to a picture of the package.
fn fill_picture(holder: &Node, rels: &RelTab) -> u32 {
    holder
        .child("blipFill")
        .and_then(|b| b.child("blip"))
        .and_then(|b| b.rel("embed").or_else(|| b.rel("link")))
        .and_then(|rid| rels.get(rid).copied().flatten())
        .unwrap_or(IMAGE_NONE)
}

impl Look {
    fn scheme(&self, name: &str, ph: Option<u32>) -> Option<u32> {
        if name == "phClr" {
            return ph;
        }
        let default = match name {
            "bg1" => "lt1",
            "tx1" => "dk1",
            "bg2" => "lt2",
            "tx2" => "dk2",
            other => other,
        };
        let mapped = self.map.iter().find(|(k, _)| k == name).map_or(default, |(_, v)| v.as_str());
        slot(mapped).and_then(|i| self.colors.get(i).copied())
    }

    /// The colour an element such as `srgbClr` or `schemeClr` stands for, with its transforms.
    fn color(&self, n: &Node, ph: Option<u32>) -> Option<u32> {
        let (base, alpha0) = match n.name.as_str() {
            "srgbClr" => (hex6(n.attr("val")?)?, 1.0),
            "schemeClr" => {
                let c = self.scheme(n.attr("val")?, ph)?;
                (c, f64::from(c >> 24) / 255.0)
            }
            "sysClr" => {
                let c = n.attr("lastClr").and_then(hex6).or_else(|| match n.attr("val") {
                    Some("window") => Some(0xFFFF_FFFF),
                    _ => Some(0xFF00_0000),
                })?;
                (c, 1.0)
            }
            "prstClr" => (preset_color(n.attr("val")?)?, 1.0),
            "scrgbClr" => {
                let ch = |k: &str| frac(n.attr(k)).unwrap_or(0.0).clamp(0.0, 1.0);
                (pack([ch("r"), ch("g"), ch("b")], 1.0), 1.0)
            }
            "hslClr" => {
                let h = n.attr("hue").and_then(num).unwrap_or(0.0) / 60_000.0;
                let s = frac(n.attr("sat")).unwrap_or(0.0);
                let l = frac(n.attr("lum")).unwrap_or(0.0);
                (pack(from_hsl(h, s, l), 1.0), 1.0)
            }
            _ => return None,
        };
        let mut rgb = channels(base);
        let mut alpha = alpha0;
        for t in &n.kids {
            let v = frac(t.attr("val"));
            match (t.name.as_str(), v) {
                ("alpha", Some(v)) => alpha = v,
                ("alphaMod", Some(v)) => alpha *= v,
                ("alphaOff", Some(v)) => alpha += v,
                ("lumMod" | "lumOff" | "lum" | "satMod" | "satOff" | "sat", Some(v)) => {
                    let (h, mut s, mut l) = to_hsl(rgb);
                    match t.name.as_str() {
                        "lumMod" => l *= v,
                        "lumOff" => l += v,
                        "lum" => l = v,
                        "satMod" => s *= v,
                        "satOff" => s += v,
                        _ => s = v,
                    }
                    rgb = from_hsl(h, s, l);
                }
                ("hueOff", _) | ("hue", _) | ("hueMod", _) => {
                    let deg = t.attr("val").and_then(num).unwrap_or(0.0) / 60_000.0;
                    let (h, s, l) = to_hsl(rgb);
                    let h = match t.name.as_str() {
                        "hue" => deg,
                        "hueOff" => h + deg,
                        _ => h * deg / 360.0 * 6.0,
                    };
                    rgb = from_hsl(h, s, l);
                }
                ("tint", Some(v)) => {
                    let v = v.clamp(0.0, 1.0);
                    rgb = rgb.map(|c| c * v + (1.0 - v));
                }
                ("shade", Some(v)) => {
                    let v = v.clamp(0.0, 1.0);
                    rgb = rgb.map(|c| c * v);
                }
                ("gray", _) => {
                    let y = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
                    rgb = [y, y, y];
                }
                ("inv" | "comp", _) => rgb = rgb.map(|c| 1.0 - c),
                _ => {}
            }
        }
        Some(pack(rgb, alpha.clamp(0.0, 1.0)))
    }

    /// The first colour among the children of `holder` (a `solidFill`, a `gs`, ...).
    fn color_in(&self, holder: &Node, ph: Option<u32>) -> Option<u32> {
        holder.kids.iter().find_map(|k| self.color(k, ph))
    }

    /// The paint of one fill element (`solidFill`, `noFill`, ...); `None` if it is not one.
    fn paint_el(&self, el: &Node, ph: Option<u32>, cv: &mut Canvas) -> Option<Paint> {
        Some(match el.name.as_str() {
            "noFill" => Paint::None,
            "solidFill" => self.color_in(el, ph).map_or(Paint::Unset, Paint::Color),
            "gradFill" => {
                cv.approximated("a gradient is drawn with its first colour");
                el.child("gsLst")
                    .and_then(|l| l.child("gs"))
                    .and_then(|gs| self.color_in(gs, ph))
                    .map_or(Paint::Unset, Paint::Color)
            }
            "pattFill" => {
                cv.approximated("a pattern is drawn with its foreground colour");
                el.child("fgClr")
                    .and_then(|c| self.color_in(c, ph))
                    .map_or(Paint::Unset, Paint::Color)
            }
            "blipFill" => Paint::Picture,
            "grpFill" => Paint::Unset,
            _ => return None,
        })
    }

    /// The paint a shape property element (`spPr`, `ln`, `bgPr`, ...) sets.
    fn paint_in(&self, holder: &Node, ph: Option<u32>, cv: &mut Canvas) -> Paint {
        holder.kids.iter().find_map(|k| self.paint_el(k, ph, cv)).unwrap_or(Paint::Unset)
    }

    /// A theme fill style (`fillRef`) for a colour.
    fn ref_fill(&self, r: &Node, cv: &mut Canvas) -> Paint {
        let idx = r.attr("idx").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        if idx == 0 {
            return Paint::None;
        }
        let ph = self.color_in(r, None);
        let el = if idx >= 1001 { self.bg_fills.get(idx - 1001) } else { self.fills.get(idx - 1) };
        el.and_then(|e| self.paint_el(e, ph, cv)).unwrap_or(Paint::Unset)
    }

    /// A theme line style (`lnRef`): its width in points, if it has one, and its paint.
    fn ref_line(&self, r: &Node, cv: &mut Canvas) -> (Option<f64>, Paint) {
        let idx = r.attr("idx").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        if idx == 0 {
            return (None, Paint::None);
        }
        let ph = self.color_in(r, None);
        match self.lines.get(idx - 1) {
            Some(ln) => (ln.attr("w").map(|w| emu_pt(Some(w))), self.paint_in(ln, ph, cv)),
            None => (None, Paint::Unset),
        }
    }

    fn typeface_family(&self, face: &str) -> (bool, bool) {
        match face {
            "+mj-lt" | "+mj-ea" | "+mj-cs" => family_of(&self.major),
            "+mn-lt" | "+mn-ea" | "+mn-cs" => family_of(&self.minor),
            other => family_of(other),
        }
    }

    // ---- text style readers --------------------------------------------------------------

    fn read_rpr(&self, n: &Node, out: &mut Lvl) {
        if let Some(sz) = n.attr("sz").and_then(num) {
            out.size = Some((sz / 100.0).clamp(1.0, 2000.0));
        }
        if let Some(v) = n.attr("b") {
            out.bold = Some(on(Some(v)));
        }
        if let Some(v) = n.attr("i") {
            out.italic = Some(on(Some(v)));
        }
        if let Some(v) = n.attr("u") {
            out.underline = Some(v.trim() != "none");
        }
        if let Some(v) = n.attr("strike") {
            out.strike = Some(v.trim() != "noStrike");
        }
        if let Some(b) = n.attr("baseline").and_then(num) {
            out.shift = Some(if b > 0.0 {
                1
            } else if b < 0.0 {
                -1
            } else {
                0
            });
        }
        for k in &n.kids {
            match k.name.as_str() {
                "solidFill" => {
                    if let Some(c) = self.color_in(k, None) {
                        out.color = Some(c);
                    }
                }
                "latin" => {
                    if let Some(face) = k.attr("typeface") {
                        let (mono, serif) = self.typeface_family(face);
                        out.mono = Some(mono);
                        out.serif = Some(serif);
                    }
                }
                _ => {}
            }
        }
    }

    fn read_spacing(n: &Node) -> Option<Sp> {
        if let Some(v) = frac(n.child("spcPct").and_then(|c| c.attr("val"))) {
            return Some(Sp::Pct(v.clamp(0.0, 10.0)));
        }
        n.child("spcPts")
            .and_then(|c| c.attr("val"))
            .and_then(num)
            .map(|v| Sp::Pts((v / 100.0).clamp(0.0, 1000.0)))
    }

    /// A paragraph-properties element (`pPr`, `lvl1pPr`, ...) with its default run properties.
    fn read_ppr(&self, n: &Node) -> Lvl {
        let mut out = Lvl::default();
        if let Some(v) = n.attr("marL") {
            out.mar_l = Some(emu_pt(Some(v)).max(0.0));
        }
        if let Some(v) = n.attr("indent") {
            out.indent = Some(emu_pt(Some(v)));
        }
        out.algn = match n.attr("algn").map(str::trim) {
            Some("l") => Some(Align::Start),
            Some("ctr") => Some(Align::Center),
            Some("r") => Some(Align::End),
            Some("just" | "dist" | "justLow" | "thaiDist") => Some(Align::Justify),
            _ => None,
        };
        for k in &n.kids {
            match k.name.as_str() {
                "lnSpc" => out.line = Self::read_spacing(k),
                "spcBef" => out.before = Self::read_spacing(k),
                "spcAft" => out.after = Self::read_spacing(k),
                "buNone" => out.bullet = Some(Bu::None),
                "buChar" => {
                    let c = k.attr("char").unwrap_or_default();
                    // Symbol-font code points would draw as boxes.
                    let c = c.chars().next().filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c));
                    out.bullet = Some(Bu::Char(c.map_or_else(|| "•".to_owned(), String::from)));
                }
                "buAutoNum" => {
                    let start = k.attr("startAt").and_then(|v| v.trim().parse::<u32>().ok());
                    out.bullet = Some(Bu::Auto(
                        k.attr("type").unwrap_or_default().to_owned(),
                        start.unwrap_or(1).clamp(1, 100_000),
                    ));
                }
                "defRPr" => self.read_rpr(k, &mut out),
                _ => {}
            }
        }
        out
    }

    /// The nine levels of a list style (`lstStyle`, `titleStyle`, `defaultTextStyle`, ...).
    fn read_levels(&self, n: &Node) -> Levels {
        std::array::from_fn(|i| {
            n.child(&format!("lvl{}pPr", i + 1)).map(|k| self.read_ppr(k)).unwrap_or_default()
        })
    }
}

// ---- geometry ----------------------------------------------------------------------------------

/// A shape's frame, in points, in its parent's coordinates.
#[derive(Debug, Clone, Copy)]
struct Xf {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    rot: f64,
    flip_h: bool,
    flip_v: bool,
}

fn read_xf(n: &Node) -> Xf {
    let off = n.child("off");
    let ext = n.child("ext");
    Xf {
        x: emu_pt(off.and_then(|o| o.attr("x"))),
        y: emu_pt(off.and_then(|o| o.attr("y"))),
        w: emu_pt(ext.and_then(|o| o.attr("cx"))).max(0.0),
        h: emu_pt(ext.and_then(|o| o.attr("cy"))).max(0.0),
        rot: fin(n.attr("rot").and_then(num).unwrap_or(0.0).rem_euclid(21_600_000.0) / 60_000.0)
            .to_radians(),
        flip_h: on(n.attr("flipH")),
        flip_v: on(n.attr("flipV")),
    }
}

/// Maps the shape's own coordinates (`0..w × 0..h`) to its parent's: flips and the rotation turn
/// about the centre of the frame.
fn shape_mat(xf: &Xf, parent: &Mat) -> Mat {
    let (fx, fy) = (if xf.flip_h { -1.0 } else { 1.0 }, if xf.flip_v { -1.0 } else { 1.0 });
    Mat::translate(-xf.w / 2.0, -xf.h / 2.0)
        .then(Mat::scale(fx, fy))
        .then(Mat::rotate(xf.rot))
        .then(Mat::translate(xf.x + xf.w / 2.0, xf.y + xf.h / 2.0))
        .then(*parent)
}

/// The frame text is laid out in: its size on the page (text is not scaled with its group) and the
/// transform from it to the page. `rect` is in the coordinates `m` maps; `angle` is the direction
/// of the text lines.
fn text_frame(m: &Mat, rect: [f64; 4], angle: f64) -> (Mat, f64, f64) {
    let [x, y, w, h] = rect;
    let c = m.apply(x + w / 2.0, y + h / 2.0);
    let (sx, sy) = m.axis_scales();
    let (tw, th) = (w * sx, h * sy);
    let mt = Mat::translate(-tw / 2.0, -th / 2.0)
        .then(Mat::rotate(angle))
        .then(Mat::translate(c.0, c.1));
    (mt, tw, th)
}

/// A path of a shape's geometry.
struct GPath {
    segs: Vec<Seg>,
    fill: bool,
    stroke: bool,
}

/// The value of the first `gd` named `want` in an `avLst`: `val N`.
fn guide(av: Option<&Node>, want: &str) -> Option<f64> {
    av?.kids_named("gd")
        .find(|g| g.attr("name") == Some(want))
        .and_then(|g| g.attr("fmla"))
        .and_then(|f| f.trim().strip_prefix("val"))
        .and_then(num)
}

/// Custom geometry: the paths, scaled from their own `w × h` to the frame.
fn cust_geom(g: &Node, w: f64, h: f64, cv: &mut Canvas) -> Vec<GPath> {
    let mut out = Vec::new();
    let Some(list) = g.child("pathLst") else { return out };
    let mut approx = false;
    for path in list.kids_named("path").take(64) {
        let pw = path.attr("w").and_then(num).filter(|v| *v > 0.0);
        let ph = path.attr("h").and_then(num).filter(|v| *v > 0.0);
        let sx = pw.map_or(1.0 / 12_700.0, |pw| w / pw);
        let sy = ph.map_or(1.0 / 12_700.0, |ph| h / ph);
        let mut segs: Vec<Seg> = Vec::new();
        let mut cur = (0.0, 0.0);
        let mut start = (0.0, 0.0);
        let pt = |n: &Node, approx: &mut bool| -> (f64, f64) {
            let c = |k: &str, approx: &mut bool| -> f64 {
                match n.attr(k).and_then(num) {
                    Some(v) => v,
                    None => {
                        // A guide name: formulas are not evaluated.
                        *approx = true;
                        0.0
                    }
                }
            };
            (fin(c("x", approx) * sx), fin(c("y", approx) * sy))
        };
        let pts = |n: &Node, approx: &mut bool| -> Vec<(f64, f64)> {
            n.kids_named("pt").take(3).map(|p| pt(p, approx)).collect()
        };
        for cmd in path.kids.iter().take(50_000) {
            match cmd.name.as_str() {
                "moveTo" => {
                    if let Some(p) = pts(cmd, &mut approx).first() {
                        segs.push(Seg::M(p.0, p.1));
                        cur = *p;
                        start = *p;
                    }
                }
                "lnTo" => {
                    if let Some(p) = pts(cmd, &mut approx).first() {
                        segs.push(Seg::L(p.0, p.1));
                        cur = *p;
                    }
                }
                "cubicBezTo" => {
                    if let [a, b, c] = pts(cmd, &mut approx)[..] {
                        segs.push(Seg::C(a.0, a.1, b.0, b.1, c.0, c.1));
                        cur = c;
                    }
                }
                "quadBezTo" => {
                    if let [a, b] = pts(cmd, &mut approx)[..] {
                        // Raise the quadratic curve to a cubic one.
                        let c1 =
                            (cur.0 + 2.0 / 3.0 * (a.0 - cur.0), cur.1 + 2.0 / 3.0 * (a.1 - cur.1));
                        let c2 = (b.0 + 2.0 / 3.0 * (a.0 - b.0), b.1 + 2.0 / 3.0 * (a.1 - b.1));
                        segs.push(Seg::C(c1.0, c1.1, c2.0, c2.1, b.0, b.1));
                        cur = b;
                    }
                }
                "arcTo" => {
                    let get = |k: &str| cmd.attr(k).and_then(num);
                    if let (Some(wr), Some(hr), Some(st), Some(sw)) =
                        (get("wR"), get("hR"), get("stAng"), get("swAng"))
                    {
                        cur = arc_to(
                            &mut segs,
                            cur,
                            fin(wr * sx),
                            fin(hr * sy),
                            st / 60_000.0,
                            sw / 60_000.0,
                        );
                    } else {
                        approx = true;
                    }
                }
                "close" => {
                    segs.push(Seg::Z);
                    cur = start;
                }
                _ => {}
            }
        }
        if !segs.is_empty() {
            out.push(GPath {
                segs,
                fill: path.attr("fill").is_none_or(|v| v.trim() != "none"),
                stroke: !off(path.attr("stroke")),
            });
        }
    }
    if approx {
        cv.approximated("custom geometry uses formulas, which are not evaluated");
    }
    out
}

// ---- the pass over one slide ---------------------------------------------------------------------

type RelTab = BTreeMap<String, Option<u32>>;

struct TblRegion {
    fill: Option<u32>,
    bold: Option<bool>,
    color: Option<u32>,
    border: Option<(u32, f64)>,
}

impl TblRegion {
    const fn empty() -> Self {
        Self { fill: None, bold: None, color: None, border: None }
    }

    fn over(&mut self, o: &Self) {
        if o.fill.is_some() {
            self.fill = o.fill;
        }
        if o.bold.is_some() {
            self.bold = o.bold;
        }
        if o.color.is_some() {
            self.color = o.color;
        }
        if o.border.is_some() {
            self.border = o.border;
        }
    }
}

struct Pass<'a> {
    cv: &'a mut Canvas,
    look: &'a Look,
    reveal: bool,
    slide_no: usize,
    layout: Option<&'a Node>,
    master: Option<&'a Node>,
    rels: &'a RelTab,
    table_styles: Option<&'a Node>,
    /// The cached drawings of the slide's SmartArt diagrams, by the relationship id of the
    /// diagram's data part (the one the frame names in `dgm:relIds`).
    diagrams: &'a BTreeMap<String, super::Parsed>,
}

/// The kind of text style a placeholder takes from the master.
fn ph_kind(ty: Option<&str>) -> &str {
    match ty.map(str::trim) {
        None
        | Some(
            "body" | "obj" | "subTitle" | "tbl" | "chart" | "dgm" | "media" | "clipArt" | "pic"
            | "",
        ) => "body",
        Some("title" | "ctrTitle") => "title",
        Some(other) => other,
    }
}

/// The `ph` element of a shape node, if it is a placeholder.
fn ph_of(sp: &Node) -> Option<&Node> {
    ["nvSpPr", "nvPicPr", "nvGraphicFramePr"]
        .iter()
        .find_map(|n| sp.child(n))
        .and_then(|nv| nv.child("nvPr"))
        .and_then(|p| p.child("ph"))
}

fn is_hidden(sp: &Node) -> bool {
    ["nvSpPr", "nvPicPr", "nvGraphicFramePr", "nvGrpSpPr", "nvCxnSpPr"]
        .iter()
        .find_map(|n| sp.child(n))
        .and_then(|nv| nv.child("cNvPr"))
        .is_some_and(|c| on(c.attr("hidden")))
}

impl<'a> Pass<'a> {
    /// The layout's and the master's counterpart of a slide placeholder.
    fn find_ph(&self, ph: &Node) -> (Option<&'a Node>, Option<&'a Node>) {
        fn scan<'n>(tree: &'n Node, ty: Option<&str>, idx: Option<&str>) -> Option<&'n Node> {
            let want = ph_kind(ty);
            let mut by_kind = None;
            for sp in tree.kids.iter().filter(|k| matches!(k.name.as_str(), "sp" | "pic")) {
                let Some(p) = ph_of(sp) else { continue };
                if idx.is_some() && p.attr("idx") == idx && idx != Some("0") {
                    return Some(sp);
                }
                if by_kind.is_none() && ph_kind(p.attr("type")) == want {
                    by_kind = Some(sp);
                }
            }
            by_kind
        }
        let (ty, idx) = (ph.attr("type"), ph.attr("idx"));
        let lay = self.layout.and_then(|t| scan(t, ty, idx));
        let lay_ty = lay.and_then(ph_of).and_then(|p| p.attr("type")).or(ty);
        let mas = self.master.and_then(|t| scan(t, lay_ty, None));
        (lay, mas)
    }

    /// The shapes of a tree (`spTree`, a group, a `Choice`) in drawing order.
    fn shapes(&mut self, parent: &Node, m: &Mat, depth: usize, layer: bool) {
        for k in &parent.kids {
            if self.cv.full {
                return;
            }
            match k.name.as_str() {
                "sp" => self.sp(k, m, layer, false),
                "cxnSp" => self.sp(k, m, layer, true),
                "pic" => self.pic(k, m, layer),
                "grpSp" => self.group(k, m, depth, layer),
                "graphicFrame" => self.frame(k, m, layer),
                "AlternateContent" => {
                    let pick = k.child("Choice").or_else(|| k.child("Fallback"));
                    if let Some(c) = pick {
                        self.shapes(c, m, depth, layer);
                    }
                }
                _ => {}
            }
        }
    }

    /// Starts a shape: `false` if it is not drawn (over the budget, or hidden and hidden content
    /// is not shown). Hidden shapes that are shown are drawn faded.
    fn begin(&mut self, node: &Node) -> Option<f64> {
        if !self.cv.take_shape() {
            return None;
        }
        let before = self.cv.opacity;
        if is_hidden(node) {
            if !self.reveal {
                return None;
            }
            self.cv.opacity *= 0.35;
        }
        Some(before)
    }

    fn group(&mut self, g: &Node, parent: &Mat, depth: usize, layer: bool) {
        if depth >= super::MAX_GROUP_DEPTH {
            self.cv.full = true;
            return;
        }
        let Some(before) = self.begin(g) else { return };
        let m = match g.child("grpSpPr").and_then(|p| p.child("xfrm")) {
            Some(x) => {
                let xf = read_xf(x);
                let off = x.child("chOff");
                let ext = x.child("chExt");
                let (cox, coy) =
                    (emu_pt(off.and_then(|o| o.attr("x"))), emu_pt(off.and_then(|o| o.attr("y"))));
                let (cw, ch) = (
                    emu_pt(ext.and_then(|o| o.attr("cx"))),
                    emu_pt(ext.and_then(|o| o.attr("cy"))),
                );
                let sx = if cw.abs() > 1e-9 { xf.w / cw } else { 1.0 };
                let sy = if ch.abs() > 1e-9 { xf.h / ch } else { 1.0 };
                let (fx, fy) =
                    (if xf.flip_h { -1.0 } else { 1.0 }, if xf.flip_v { -1.0 } else { 1.0 });
                let (cx, cy) = (xf.x + xf.w / 2.0, xf.y + xf.h / 2.0);
                Mat::translate(-cox, -coy)
                    .then(Mat::scale(sx, sy))
                    .then(Mat::translate(xf.x, xf.y))
                    .then(Mat::translate(-cx, -cy))
                    .then(Mat::scale(fx, fy))
                    .then(Mat::rotate(xf.rot))
                    .then(Mat::translate(cx, cy))
                    .then(*parent)
            }
            None => *parent,
        };
        self.shapes(g, &m, depth + 1, layer);
        self.cv.opacity = before;
    }

    // ---- paint of a shape ------------------------------------------------------------------

    /// The fill colour (0 for none) from the property elements in `chain`, then the style; and,
    /// when the fill is a picture the shape itself holds, its id (else [`IMAGE_NONE`]).
    fn fill_of(&mut self, chain: &[&Node], style: Option<&Node>) -> (u32, u32) {
        let look = self.look;
        for sp_pr in chain {
            match look.paint_in(sp_pr, None, self.cv) {
                Paint::Unset => {}
                Paint::None => return (0, IMAGE_NONE),
                Paint::Color(c) => return (c, IMAGE_NONE),
                Paint::Picture => {
                    let id = fill_picture(sp_pr, self.rels);
                    if id == IMAGE_NONE {
                        self.cv.approximated("a picture fill that is not in the file is not drawn");
                    }
                    return (0, id);
                }
            }
        }
        match style.and_then(|s| s.child("fillRef")).map(|r| look.ref_fill(r, self.cv)) {
            Some(Paint::Color(c)) => (c, IMAGE_NONE),
            Some(Paint::Picture) => {
                self.cv.approximated("a picture fill from the theme is not drawn");
                (0, IMAGE_NONE)
            }
            _ => (0, IMAGE_NONE),
        }
    }

    /// The line colour (0 for none) and width in points.
    fn line_of(&mut self, chain: &[&Node], style: Option<&Node>) -> (u32, f64) {
        let look = self.look;
        let lns: Vec<&Node> = chain.iter().filter_map(|p| p.child("ln")).collect();
        let mut width = lns.iter().find_map(|l| l.attr("w")).map(|w| emu_pt(Some(w)));
        let mut paint = Paint::Unset;
        for ln in &lns {
            paint = look.paint_in(ln, None, self.cv);
            if paint != Paint::Unset {
                break;
            }
        }
        if lns
            .iter()
            .any(|l| l.child("prstDash").and_then(|d| d.attr("val")).is_some_and(|v| v != "solid"))
        {
            self.cv.approximated("dashed lines are drawn solid");
        }
        if (paint == Paint::Unset || width.is_none())
            && let Some(r) = style.and_then(|s| s.child("lnRef"))
        {
            let (w, p) = look.ref_line(r, self.cv);
            if paint == Paint::Unset {
                paint = p;
            }
            if width.is_none() {
                width = w;
            }
        }
        match paint {
            Paint::Color(c) => {
                let w = width.unwrap_or(0.75);
                (c, if w < 0.25 { 0.5 } else { w })
            }
            Paint::Picture => {
                self.cv.approximated("a line filled with a picture is not drawn");
                (0, 0.0)
            }
            _ => (0, 0.0),
        }
    }

    // ---- shapes ----------------------------------------------------------------------------

    fn sp(&mut self, sp: &Node, parent: &Mat, layer: bool, connector: bool) {
        let ph = ph_of(sp);
        if layer && ph.is_some() {
            // A layout's or master's placeholders are prompts; only the slide's own are drawn.
            return;
        }
        let Some(before) = self.begin(sp) else { return };
        self.sp_inner(sp, ph, parent, layer, connector);
        self.cv.opacity = before;
    }

    fn sp_inner(
        &mut self,
        sp: &Node,
        ph: Option<&Node>,
        parent: &Mat,
        layer: bool,
        connector: bool,
    ) {
        let look = self.look;
        let (lay, mas) = match ph {
            Some(p) if !layer => self.find_ph(p),
            _ => (None, None),
        };
        let mut chain: Vec<&Node> = Vec::new();
        for s in [Some(sp), lay, mas].into_iter().flatten() {
            if let Some(p) = s.child("spPr") {
                chain.push(p);
            }
        }
        let style = sp.child("style");
        let Some(xf_node) = chain.iter().find_map(|p| p.child("xfrm")) else {
            if sp.child("txBody").is_some_and(|t| t.child("p").is_some()) {
                self.cv.approximated("a placeholder has no position");
            }
            return;
        };
        let xf = read_xf(xf_node);
        let m = shape_mat(&xf, parent);

        // Geometry.
        let geom = chain.iter().find_map(|p| p.child("custGeom").or_else(|| p.child("prstGeom")));
        let mut outline_only = false;
        let paths: Vec<GPath> = match geom {
            Some(g) if g.name == "custGeom" => cust_geom(g, xf.w, xf.h, self.cv),
            Some(g) => {
                let prst = g.attr("prst").unwrap_or("rect");
                let av = g.child("avLst");
                let adj = [guide(av, "adj").or_else(|| guide(av, "adj1")), guide(av, "adj2")];
                match presets::preset(prst, xf.w, xf.h, adj) {
                    Some(p) => {
                        if p.rough {
                            self.cv.approximated("a preset shape is drawn as a rough likeness");
                        }
                        vec![GPath { segs: p.segs, fill: !p.open, stroke: true }]
                    }
                    None => {
                        self.cv
                            .approximated("a shape outside the preset table is drawn as its frame");
                        outline_only = true;
                        vec![GPath { segs: presets::rect(xf.w, xf.h), fill: false, stroke: true }]
                    }
                }
            }
            None => vec![GPath { segs: presets::rect(xf.w, xf.h), fill: true, stroke: true }],
        };
        let (fill, picture) = if connector { (0, IMAGE_NONE) } else { self.fill_of(&chain, style) };
        if picture != IMAGE_NONE {
            // The picture takes the place of the fill: under the outline and the text.
            let rect = geom
                .is_none_or(|g| g.name == "prstGeom" && g.attr("prst").is_none_or(|p| p == "rect"));
            if !rect || tiled_or_cropped(&chain) {
                self.cv
                    .approximated("a picture fill is drawn stretched over the frame of the shape");
            }
            self.cv.image(&m, xf.w, xf.h, "", picture);
        }
        let (mut stroke, mut width) = self.line_of(&chain, style);
        if outline_only && stroke >> 24 == 0 {
            // The frame is the only trace of the shape: keep it visible.
            stroke = 0xFF80_8080;
            width = 0.75;
        }
        let multi = paths.len() > 1;
        for p in &paths {
            self.cv.path(
                &p.segs,
                &m,
                if p.fill { fill } else { 0 },
                if p.stroke { stroke } else { 0 },
                width,
                multi,
            );
        }

        // Text.
        let Some(tx) = sp.child("txBody") else { return };
        if tx.child("p").is_none() {
            return;
        }
        let tx_chain: Vec<&Node> =
            [Some(sp), lay, mas].into_iter().flatten().filter_map(|s| s.child("txBody")).collect();
        let font_ref = style.and_then(|s| s.child("fontRef")).and_then(|r| look.color_in(r, None));
        let kind = match ph {
            Some(p) => {
                ph_kind(p.attr("type").or_else(|| lay.and_then(ph_of).and_then(|l| l.attr("type"))))
            }
            None => "other",
        };
        let Some(tb) = self.text_body(&tx_chain, kind, font_ref, None) else { return };
        let rot = xf.rot + if xf.flip_v { std::f64::consts::PI } else { 0.0 };
        let angle = Mat::rotate(rot).then(*parent);
        let (mt, tw, th) = text_frame(&m, [0.0, 0.0, xf.w, xf.h], angle.b.atan2(angle.a));
        let laid = layout(&tb, tw, self.cv);
        draw_laid(self.cv, &tb, &laid, tw, th, &mt);
    }

    fn pic(&mut self, pic: &Node, parent: &Mat, layer: bool) {
        let ph = ph_of(pic);
        if layer && ph.is_some() {
            return;
        }
        let Some(before) = self.begin(pic) else { return };
        let (lay, mas) = match ph {
            Some(p) if !layer => self.find_ph(p),
            _ => (None, None),
        };
        let xf_node = [Some(pic), lay, mas]
            .into_iter()
            .flatten()
            .filter_map(|s| s.child("spPr"))
            .find_map(|p| p.child("xfrm"));
        if let Some(xn) = xf_node {
            let xf = read_xf(xn);
            let m = shape_mat(&xf, parent);
            let alt = pic
                .child("nvPicPr")
                .and_then(|n| n.child("cNvPr"))
                .and_then(|c| {
                    ["descr", "title", "name"]
                        .iter()
                        .filter_map(|k| c.attr(k))
                        .find(|v| !v.trim().is_empty())
                })
                .unwrap_or("");
            let id = pic
                .child("blipFill")
                .and_then(|b| b.child("blip"))
                .and_then(|b| b.rel("embed").or_else(|| b.rel("link")))
                .and_then(|rid| self.rels.get(rid).copied().flatten())
                .unwrap_or(IMAGE_NONE);
            self.cv.image(&m, xf.w, xf.h, alt, id);
        }
        self.cv.opacity = before;
    }

    /// Draws a SmartArt diagram from its cached drawing (`dsp:drawing`), whose shapes sit in EMU
    /// relative to the frame. `false` if the file carries no such drawing.
    fn diagram(&mut self, data: &Node, xf: &Xf, parent: &Mat) -> bool {
        let Some(tree) = data
            .child("relIds")
            .and_then(|r| r.rel("dm"))
            .and_then(|id| self.diagrams.get(id))
            .and_then(|d| d.root.child("spTree"))
        else {
            return false;
        };
        let origin = Mat::translate(xf.x, xf.y).then(*parent);
        self.shapes(tree, &origin, 1, false);
        true
    }

    fn frame(&mut self, gf: &Node, parent: &Mat, layer: bool) {
        let ph = ph_of(gf);
        if layer && ph.is_some() {
            return;
        }
        let Some(before) = self.begin(gf) else { return };
        let (lay, mas) = match ph {
            Some(p) if !layer => self.find_ph(p),
            _ => (None, None),
        };
        let xf_node = gf.child("xfrm").or_else(|| {
            [lay, mas]
                .into_iter()
                .flatten()
                .filter_map(|s| s.child("spPr"))
                .find_map(|p| p.child("xfrm"))
        });
        let data = gf.child("graphic").and_then(|g| g.child("graphicData"));
        if let (Some(xn), Some(data)) = (xf_node, data) {
            let mut xf = read_xf(xn);
            xf.rot = 0.0;
            let uri = data.attr("uri").unwrap_or_default();
            if uri.ends_with("/table") {
                if let Some(tbl) = data.child("tbl") {
                    self.table(tbl, &xf, parent);
                }
            } else if uri.contains("diagram") && self.diagram(data, &xf, parent) {
                // drawn from the diagram's cached drawing
            } else {
                let label = if uri.contains("chart") {
                    "chart"
                } else if uri.contains("diagram") {
                    "SmartArt"
                } else {
                    "object"
                };
                let m = shape_mat(&xf, parent);
                self.cv.image(&m, xf.w, xf.h, label, IMAGE_NONE);
            }
        }
        self.cv.opacity = before;
    }

    // ---- text ------------------------------------------------------------------------------

    /// The text of a shape. `tx_chain` is the shape's `txBody` first, then its layout's and
    /// master's placeholders' ones.
    fn text_body(
        &mut self,
        tx_chain: &[&Node],
        kind: &str,
        font_ref: Option<u32>,
        extra: Option<&Lvl>,
    ) -> Option<TextBody> {
        let look = self.look;
        let tx = tx_chain.first()?;
        let master_style: &Levels = match kind {
            "title" => &look.title,
            "other" | "tbl" => &look.other,
            "dt" | "ftr" | "sldNum" | "hdr" => &look.other,
            _ => &look.body,
        };
        let own: Vec<Levels> = tx_chain
            .iter()
            .rev()
            .map(|t| t.child("lstStyle").map(|l| look.read_levels(l)).unwrap_or_else(no_levels))
            .collect();
        let mut layers: Vec<&Levels> = vec![&look.default_lvls, master_style];
        layers.extend(own.iter());

        // Body properties: the first that sets an attribute wins.
        let bps: Vec<&Node> = tx_chain.iter().filter_map(|t| t.child("bodyPr")).collect();
        let bp = |k: &str| bps.iter().find_map(|b| b.attr(k));
        let ins = [
            emu_pt(bp("lIns").or(Some("91440"))).max(0.0),
            emu_pt(bp("tIns").or(Some("45720"))).max(0.0),
            emu_pt(bp("rIns").or(Some("91440"))).max(0.0),
            emu_pt(bp("bIns").or(Some("45720"))).max(0.0),
        ];
        let anchor = match bp("anchor").map(str::trim) {
            Some("ctr") => Anchor::Middle,
            Some("b") => Anchor::Bottom,
            _ => Anchor::Top,
        };
        if bp("vert").is_some_and(|v| v != "horz") {
            self.cv.approximated("vertical text is drawn horizontally");
        }
        let scale = bps
            .iter()
            .find_map(|b| b.child("normAutofit"))
            .and_then(|a| frac(a.attr("fontScale")))
            .map_or(1.0, |s| s.clamp(0.1, 1.0));
        let mut body = TextBody {
            paras: Vec::new(),
            ins,
            anchor,
            wrap: bp("wrap").map(str::trim) != Some("none"),
            scale,
        };
        let mut counters = [0u32; 9];
        for p in tx.kids_named("p").take(2_000) {
            let para = self.para(p, &layers, font_ref, extra, &mut counters);
            body.paras.push(para);
        }
        body.paras
            .iter()
            .any(|p| p.pieces.iter().any(|pc| !pc.text.trim().is_empty()))
            .then_some(body)
    }

    fn para(
        &mut self,
        p: &Node,
        layers: &[&Levels],
        font_ref: Option<u32>,
        extra: Option<&Lvl>,
        counters: &mut [u32; 9],
    ) -> Para {
        let look = self.look;
        let ppr = p.child("pPr");
        let level = ppr
            .and_then(|n| n.attr("lvl"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .map_or(0, |l| l.min(8));
        let mut eff = Lvl::default();
        for l in layers {
            if let Some(lv) = l.get(level) {
                eff.over(lv);
            }
        }
        if font_ref.is_some() {
            eff.color = font_ref;
        }
        if let Some(e) = extra {
            eff.over(e);
        }
        if let Some(n) = ppr {
            eff.over(&look.read_ppr(n));
        }
        let tx1 = look.scheme("tx1", None).unwrap_or(0xFF00_0000);
        let style_of = |lvl: &Lvl| RunStyle {
            size: lvl.size.unwrap_or(18.0),
            bold: lvl.bold.unwrap_or(false),
            italic: lvl.italic.unwrap_or(false),
            mono: lvl.mono.unwrap_or(false),
            serif: lvl.serif.unwrap_or(false),
            underline: lvl.underline.unwrap_or(false),
            strike: lvl.strike.unwrap_or(false),
            color: lvl.color.unwrap_or(tx1),
            shift: lvl.shift.unwrap_or(0),
        };
        let run_style = |rpr: Option<&Node>| -> RunStyle {
            let mut l = eff.clone();
            if let Some(r) = rpr {
                let mut own = Lvl::default();
                look.read_rpr(r, &mut own);
                l.over(&own);
            }
            style_of(&l)
        };
        let mut pieces: Vec<Piece> = Vec::new();
        for k in &p.kids {
            match k.name.as_str() {
                "r" | "fld" => {
                    let st = run_style(k.child("rPr"));
                    let raw = if k.name == "fld"
                        && k.attr("type").is_some_and(|t| t.starts_with("slidenum"))
                    {
                        self.slide_no.to_string()
                    } else {
                        k.child("t").map(Node::own_text).unwrap_or_default()
                    };
                    if !raw.is_empty() {
                        let text = self.cv.tidy(&raw);
                        pieces.push(Piece { text, st });
                    }
                }
                "br" => pieces.push(Piece { text: "\n".to_owned(), st: run_style(k.child("rPr")) }),
                _ => {}
            }
        }
        let end_size = style_of(&{
            let mut l = eff.clone();
            if let Some(r) = p.child("endParaRPr") {
                let mut own = Lvl::default();
                look.read_rpr(r, &mut own);
                l.over(&own);
            }
            l
        })
        .size;
        // Numbering restarts after any paragraph that is not numbered.
        let marker = match &eff.bullet {
            Some(Bu::Char(c)) => Some(c.clone()),
            Some(Bu::Auto(kind, start)) => {
                if let Some(slot) = counters.get_mut(level) {
                    *slot = if *slot == 0 { *start } else { slot.saturating_add(1) };
                }
                for deeper in counters.iter_mut().skip(level + 1) {
                    *deeper = 0;
                }
                let n = counters.get(level).copied().unwrap_or(1);
                Some(auto_marker(kind, n))
            }
            _ => None,
        };
        if !matches!(eff.bullet, Some(Bu::Auto(..))) {
            for c in counters.iter_mut().skip(level) {
                *c = 0;
            }
        }
        let first = pieces.iter().find(|pc| pc.text != "\n").map(|pc| pc.st.clone());
        let bullet = marker.zip(first).map(|(m, st)| Piece { text: m, st });
        let size_hint = first_size(&pieces).unwrap_or(end_size);
        let to_pts = |s: Option<Sp>| match s {
            Some(Sp::Pct(k)) => k * size_hint * 1.2,
            Some(Sp::Pts(v)) => v,
            None => 0.0,
        };
        Para {
            pieces,
            align: eff.algn.unwrap_or_default(),
            left: eff.mar_l.unwrap_or(0.0),
            indent: eff.indent.unwrap_or(0.0),
            bullet,
            before: to_pts(eff.before),
            after: to_pts(eff.after),
            line: match eff.line {
                Some(Sp::Pct(k)) => Spacing::Pct(k.clamp(0.1, 10.0)),
                Some(Sp::Pts(v)) => Spacing::Exact(v.clamp(1.0, 1000.0)),
                None => Spacing::Pct(1.0),
            },
            end_size,
        }
    }

    // ---- tables ----------------------------------------------------------------------------

    /// A region of a table style (`wholeTbl`, `firstRow`, ...) read from the style definition.
    fn region(&self, style: &Node, name: &str) -> TblRegion {
        let look = self.look;
        let mut r = TblRegion::empty();
        let Some(reg) = style.child(name) else { return r };
        if let Some(tx) = reg.child("tcTxStyle") {
            if let Some(b) = tx.attr("b") {
                r.bold = Some(on(Some(b)));
            }
            r.color = look.color_in(tx, None);
        }
        if let Some(tc) = reg.child("tcStyle") {
            if let Some(fill) = tc.child("fill") {
                let mut scratch = Canvas::new();
                if let Paint::Color(c) = look.paint_in(fill, None, &mut scratch) {
                    r.fill = Some(c);
                }
            }
            if let Some(b) = tc.child("tcBdr") {
                let ln = ["insideH", "insideV", "left", "top", "right", "bottom"]
                    .iter()
                    .filter_map(|s| b.child(s))
                    .filter_map(|s| s.child("ln"))
                    .next();
                if let Some(ln) = ln {
                    let mut scratch = Canvas::new();
                    if let Paint::Color(c) = look.paint_in(ln, None, &mut scratch) {
                        r.border =
                            Some((c, ln.attr("w").map_or(0.75, |w| emu_pt(Some(w)).max(0.25))));
                    }
                }
            }
        }
        r
    }

    /// The regions of the built-in "Medium Style 2 - Accent 1", for decks that name it without
    /// defining it (PowerPoint does not write the built-in styles out).
    fn builtin_medium2(&self) -> BTreeMap<&'static str, TblRegion> {
        let look = self.look;
        let acc = look.scheme("accent1", None).unwrap_or(0xFF4F_81BD);
        let lt1 = look.scheme("lt1", None).unwrap_or(0xFFFF_FFFF);
        let dk1 = look.scheme("dk1", None).unwrap_or(0xFF00_0000);
        let tint = |t: f64| {
            let c = channels(acc);
            pack(c.map(|v| v * t + (1.0 - t)), 1.0)
        };
        let white = Some((lt1, 1.0));
        let mut m = BTreeMap::new();
        m.insert(
            "wholeTbl",
            TblRegion { fill: Some(tint(0.2)), bold: None, color: Some(dk1), border: white },
        );
        m.insert("band1H", TblRegion { fill: Some(tint(0.4)), ..TblRegion::empty() });
        m.insert(
            "firstRow",
            TblRegion { fill: Some(acc), bold: Some(true), color: Some(lt1), border: white },
        );
        m.insert(
            "lastRow",
            TblRegion { fill: Some(acc), bold: Some(true), color: Some(lt1), border: white },
        );
        m.insert(
            "firstCol",
            TblRegion { fill: Some(acc), bold: Some(true), color: Some(lt1), border: white },
        );
        m.insert(
            "lastCol",
            TblRegion { fill: Some(acc), bold: Some(true), color: Some(lt1), border: white },
        );
        m
    }

    #[allow(clippy::too_many_lines)] // one pass: sizes, then fills, borders and text
    fn table(&mut self, tbl: &Node, xf: &Xf, parent: &Mat) {
        let look = self.look;
        let base = Mat::translate(xf.x, xf.y).then(*parent);
        let cols: Vec<f64> = tbl
            .child("tblGrid")
            .map(|g| {
                g.kids_named("gridCol")
                    .take(MAX_TABLE_LINE)
                    .map(|c| emu_pt(c.attr("w")).max(0.0))
                    .collect()
            })
            .unwrap_or_default();
        let rows: Vec<&Node> = tbl.kids_named("tr").take(MAX_TABLE_LINE).collect();
        if cols.is_empty() || rows.is_empty() {
            return;
        }
        if tbl.child("tblGrid").is_some_and(|g| g.kids_named("gridCol").count() > MAX_TABLE_LINE)
            || tbl.kids_named("tr").count() > MAX_TABLE_LINE
        {
            self.cv.full = true;
        }

        // The table style.
        let pr = tbl.child("tblPr");
        let flag = |k: &str| pr.is_some_and(|p| on(p.attr(k)));
        let style_id = pr
            .and_then(|p| p.child("tableStyleId"))
            .map(Node::own_text)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let defined = self.table_styles.and_then(|t| {
            t.kids_named("tblStyle").find(|s| s.attr("styleId") == Some(style_id.as_str()))
        });
        let mut regions: BTreeMap<&'static str, TblRegion> = BTreeMap::new();
        if let Some(def) = defined {
            for name in [
                "wholeTbl", "band1H", "band2H", "band1V", "band2V", "firstCol", "lastCol",
                "firstRow", "lastRow",
            ] {
                regions.insert(name, self.region(def, name));
            }
        } else if style_id.eq_ignore_ascii_case("{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}") {
            regions = self.builtin_medium2();
        }

        // Which cell covers which grid position, and the rows' heights.
        let mut draws: Vec<CellDraw> = Vec::new();
        let mut row_h: Vec<f64> = rows.iter().map(|r| emu_pt(r.attr("h")).max(0.0)).collect();
        let col_x: Vec<f64> = {
            let mut acc = 0.0;
            std::iter::once(0.0)
                .chain(cols.iter().map(|w| {
                    acc += w;
                    acc
                }))
                .collect()
        };
        // (row, first column, columns spanned, rows spanned, node)
        let mut placed: Vec<(usize, usize, usize, usize, &Node)> = Vec::new();
        for (ri, row) in rows.iter().enumerate() {
            for (ci, tc) in row.kids_named("tc").enumerate() {
                if placed.len() >= MAX_TABLE_CELLS {
                    self.cv.full = true;
                    break;
                }
                let span = tc
                    .attr("gridSpan")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_TABLE_LINE);
                let rspan = tc
                    .attr("rowSpan")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_TABLE_LINE);
                if !(on(tc.attr("hMerge")) || on(tc.attr("vMerge"))) {
                    placed.push((ri, ci, span, rspan, tc));
                }
            }
        }
        let last_row = rows.len().saturating_sub(1);
        let last_col = cols.len().saturating_sub(1);
        struct Prepared<'n> {
            ri: usize,
            ci: usize,
            span: usize,
            rspan: usize,
            tc: &'n Node,
            body: Option<TextBody>,
            laid: super::Laid,
            fill: u32,
            borders: [Option<(u32, f64)>; 4],
            mar: [f64; 4],
            anchor: Anchor,
        }
        let mut prepared: Vec<Prepared<'_>> = Vec::new();
        for (ri, ci, span, rspan, tc) in placed {
            if ci >= cols.len() {
                continue;
            }
            // Which style regions apply here.
            let mut reg = TblRegion::empty();
            if let Some(r) = regions.get("wholeTbl") {
                reg.over(r);
            }
            let first_row = flag("firstRow");
            if flag("bandRow") {
                let k = ri.saturating_sub(usize::from(first_row));
                if let Some(r) = regions.get(if k % 2 == 0 { "band1H" } else { "band2H" }) {
                    reg.over(r);
                }
            }
            if flag("bandCol")
                && let Some(r) = regions.get(if ci % 2 == 0 { "band1V" } else { "band2V" })
            {
                reg.over(r);
            }
            for (on_flag, cond, name) in [
                (flag("firstCol"), ci == 0, "firstCol"),
                (flag("lastCol"), ci == last_col, "lastCol"),
                (first_row, ri == 0, "firstRow"),
                (flag("lastRow"), ri == last_row, "lastRow"),
            ] {
                if on_flag
                    && cond
                    && let Some(r) = regions.get(name)
                {
                    reg.over(r);
                }
            }
            let tcpr = tc.child("tcPr");
            let mar = [
                emu_pt(tcpr.and_then(|p| p.attr("marL")).or(Some("91440"))).max(0.0),
                emu_pt(tcpr.and_then(|p| p.attr("marT")).or(Some("45720"))).max(0.0),
                emu_pt(tcpr.and_then(|p| p.attr("marR")).or(Some("91440"))).max(0.0),
                emu_pt(tcpr.and_then(|p| p.attr("marB")).or(Some("45720"))).max(0.0),
            ];
            let anchor = match tcpr.and_then(|p| p.attr("anchor")).map(str::trim) {
                Some("ctr") => Anchor::Middle,
                Some("b") => Anchor::Bottom,
                _ => Anchor::Top,
            };
            let fill = match tcpr.map(|p| look.paint_in(p, None, self.cv)) {
                Some(Paint::Color(c)) => c,
                Some(Paint::None) => 0,
                Some(Paint::Picture) => {
                    self.cv.approximated("a picture fill of a table cell is not drawn");
                    reg.fill.unwrap_or(0)
                }
                _ => reg.fill.unwrap_or(0),
            };
            let mut borders = [None; 4];
            for (i, name) in ["lnL", "lnT", "lnR", "lnB"].iter().enumerate() {
                let own = tcpr.and_then(|p| p.child(name));
                let b = match own {
                    Some(ln) => match look.paint_in(ln, None, self.cv) {
                        Paint::Color(c) => {
                            Some((c, ln.attr("w").map_or(0.75, |w| emu_pt(Some(w)).max(0.25))))
                        }
                        Paint::None => None,
                        Paint::Unset | Paint::Picture => reg.border,
                    },
                    None => reg.border,
                };
                if let Some(slot) = borders.get_mut(i) {
                    *slot = b;
                }
            }
            let extra = Lvl { bold: reg.bold, color: reg.color, ..Lvl::default() };
            let width: f64 = cols.iter().skip(ci).take(span).sum();
            let body = tc.child("txBody").and_then(|t| {
                let chain = [t];
                self.text_body(&chain, "tbl", None, Some(&extra))
            });
            let mut body = body;
            let laid = match body.as_mut() {
                Some(b) => {
                    b.ins = mar;
                    b.anchor = anchor;
                    layout(b, width, self.cv)
                }
                None => super::Laid::default(),
            };
            if rspan == 1 {
                let need = laid.height + mar[1] + mar[3];
                if let Some(h) = row_h.get_mut(ri) {
                    *h = h.max(need.min(5_000.0));
                }
            }
            prepared.push(Prepared {
                ri,
                ci,
                span,
                rspan,
                tc,
                body,
                laid,
                fill,
                borders,
                mar,
                anchor,
            });
        }
        let row_y: Vec<f64> = {
            let mut acc = 0.0;
            std::iter::once(0.0)
                .chain(row_h.iter().map(|h| {
                    acc += h;
                    acc
                }))
                .collect()
        };
        for p in prepared {
            let _ = (p.tc, p.mar, p.anchor);
            let x0 = col_x.get(p.ci).copied().unwrap_or(0.0);
            let x1 = col_x.get((p.ci + p.span).min(cols.len())).copied().unwrap_or(x0);
            let y0 = row_y.get(p.ri).copied().unwrap_or(0.0);
            let y1 = row_y.get((p.ri + p.rspan).min(rows.len())).copied().unwrap_or(y0);
            let body = p.body.unwrap_or(TextBody {
                paras: Vec::new(),
                ins: [0.0; 4],
                anchor: Anchor::Top,
                wrap: true,
                scale: 1.0,
            });
            draws.push(CellDraw {
                rect: [x0, y0, x1 - x0, y1 - y0],
                fill: p.fill,
                borders: p.borders,
                body,
                laid: p.laid,
            });
        }
        draw_cells(self.cv, &draws, &base);
    }
}

fn first_size(pieces: &[Piece]) -> Option<f64> {
    pieces.iter().find(|p| p.text != "\n").map(|p| p.st.size)
}

fn auto_marker(kind: &str, n: u32) -> String {
    let format = if kind.starts_with("alphaLc") {
        "a"
    } else if kind.starts_with("alphaUc") {
        "A"
    } else if kind.starts_with("romanLc") {
        "i"
    } else if kind.starts_with("romanUc") {
        "I"
    } else {
        "1"
    };
    let body = crate::listfmt::counter(format, n);
    if kind.contains("ParenBoth") {
        format!("({body})")
    } else if kind.contains("ParenR") {
        format!("{body})")
    } else {
        format!("{body}.")
    }
}

// ---- the deck --------------------------------------------------------------------------------

fn read_part(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

fn read_tree(
    pkg: &mut Package<'_>,
    part: &str,
    root: &str,
) -> Result<Option<super::Parsed>, PreviewError> {
    let Some(text) = read_part(pkg, part)? else { return Ok(None) };
    Ok(parse(&text, pkg.limits(), &KEEP_TEXT, &REL_ELEMS).ok().filter(|p| p.root.name == root))
}

fn rel_tab(pkg: &mut Package<'_>, part: &str) -> Result<RelTab, PreviewError> {
    let mut tab = RelTab::new();
    for r in pkg.rels_of(part)? {
        let id = if r.external {
            None
        } else {
            opc::resolve(part, &r.target).and_then(|t| pkg.entry_id(&t))
        };
        tab.insert(r.id, id);
    }
    Ok(tab)
}

/// Slide, layout and master (and the theme): the parts a slide is drawn from.
struct Parts {
    slide: super::Parsed,
    layout: Option<super::Parsed>,
    master: Option<super::Parsed>,
    theme: Option<super::Parsed>,
}

/// The number in the name of a diagram part: `3` for `ppt/diagrams/data3.xml` and prefix `data`.
fn diagram_number<'p>(part: &'p str, prefix: &str) -> Option<&'p str> {
    let name = part.rsplit('/').next()?;
    let digits = name.strip_prefix(prefix)?.strip_suffix(".xml")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// Most diagrams drawn on one slide.
const MAX_DIAGRAMS: usize = 64;

/// The cached drawings of the diagrams of a slide, by the relationship id of the diagram's data
/// part. PowerPoint numbers the parts of one diagram alike (`data3.xml`, `drawing3.xml`).
fn load_diagrams(
    pkg: &mut Package<'_>,
    slide_part: &str,
) -> Result<BTreeMap<String, super::Parsed>, PreviewError> {
    let mut out = BTreeMap::new();
    let rels = pkg.rels_of(slide_part)?;
    for data in rels.iter().filter(|r| !r.external && r.suffix() == "diagramData") {
        if out.len() >= MAX_DIAGRAMS {
            break;
        }
        let Some(data_part) = opc::resolve(slide_part, &data.target) else { continue };
        let Some(n) = diagram_number(&data_part, "data") else { continue };
        let drawing = rels
            .iter()
            .filter(|r| !r.external && r.suffix() == "diagramDrawing")
            .filter_map(|r| opc::resolve(slide_part, &r.target))
            .find(|p| diagram_number(p, "drawing") == Some(n));
        if let Some(part) = drawing
            && let Some(tree) = read_tree(pkg, &part, "drawing")?
        {
            out.insert(data.id.clone(), tree);
        }
    }
    Ok(out)
}

fn load_parts(
    pkg: &mut Package<'_>,
    main: &str,
    slide_part: &str,
) -> Result<(Parts, [RelTab; 3]), PreviewError> {
    let slide = read_tree(pkg, slide_part, "sld")?
        .ok_or_else(|| PreviewError::Malformed("the slide could not be read".into()))?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(slide_part.to_ascii_lowercase());
    // The chain slide -> layout -> master is followed once each, through parts of the right kind
    // that have not been met, so no cycle of relationships can be walked twice.
    let mut layout_part = None;
    let mut layout = None;
    if let Some(p) = pkg.rel_target(slide_part, "slideLayout")?
        && seen.insert(p.to_ascii_lowercase())
    {
        layout = read_tree(pkg, &p, "sldLayout")?;
        layout_part = Some(p);
    }
    let mut master_part = None;
    let mut master = None;
    if let Some(lp) = &layout_part
        && layout.is_some()
        && let Some(p) = pkg.rel_target(lp, "slideMaster")?
        && seen.insert(p.to_ascii_lowercase())
    {
        master = read_tree(pkg, &p, "sldMaster")?;
        master_part = Some(p);
    }
    let theme_part = match &master_part {
        Some(mp) => pkg.rel_target(mp, "theme")?,
        None => None,
    };
    let theme_part = match theme_part {
        Some(t) => Some(t),
        None => pkg.rel_target(main, "theme")?,
    };
    let theme = match theme_part {
        Some(t) => read_tree(pkg, &t, "theme")?,
        None => None,
    };
    let tabs = [
        rel_tab(pkg, slide_part)?,
        match &layout_part {
            Some(p) if layout.is_some() => rel_tab(pkg, p)?,
            _ => RelTab::new(),
        },
        match &master_part {
            Some(p) if master.is_some() => rel_tab(pkg, p)?,
            _ => RelTab::new(),
        },
    ];
    Ok((Parts { slide, layout, master, theme }, tabs))
}

fn build_look(parts: &Parts, pres: &Node) -> Look {
    let mut look = Look {
        colors: DEFAULT_COLORS,
        map: Vec::new(),
        major: String::new(),
        minor: String::new(),
        fills: Vec::new(),
        bg_fills: Vec::new(),
        lines: Vec::new(),
        default_lvls: no_levels(),
        title: no_levels(),
        body: no_levels(),
        other: no_levels(),
    };
    if let Some(theme) = &parts.theme {
        let els = theme.root.child("themeElements");
        if let Some(scheme) = els.and_then(|e| e.child("clrScheme")) {
            for k in &scheme.kids {
                let Some(i) = slot(&k.name) else { continue };
                let c = k.kids.iter().find_map(|c| match c.name.as_str() {
                    "srgbClr" => c.attr("val").and_then(hex6),
                    "sysClr" => c.attr("lastClr").and_then(hex6).or(Some(
                        if c.attr("val") == Some("window") { 0xFFFF_FFFF } else { 0xFF00_0000 },
                    )),
                    _ => None,
                });
                if let (Some(c), Some(slot)) = (c, look.colors.get_mut(i)) {
                    *slot = c;
                }
            }
        }
        if let Some(fonts) = els.and_then(|e| e.child("fontScheme")) {
            let face = |n: &str| {
                fonts
                    .child(n)
                    .and_then(|f| f.child("latin"))
                    .and_then(|l| l.attr("typeface"))
                    .unwrap_or_default()
                    .to_owned()
            };
            look.major = face("majorFont");
            look.minor = face("minorFont");
        }
        if let Some(fmt) = els.and_then(|e| e.child("fmtScheme")) {
            let list = |n: &str| fmt.child(n).map(|l| l.kids.clone()).unwrap_or_default();
            look.fills = list("fillStyleLst");
            look.bg_fills = list("bgFillStyleLst");
            look.lines = list("lnStyleLst");
        }
    }
    // Colour map: the master's, overridden by the layout's and the slide's.
    let mut set_map = |n: &Node| {
        for (k, v) in n.attrs_iter() {
            if let Some(slot) = look.map.iter_mut().find(|(a, _)| a == k) {
                slot.1 = v.to_owned();
            } else {
                look.map.push((k.to_owned(), v.to_owned()));
            }
        }
    };
    if let Some(m) = parts.master.as_ref().and_then(|m| m.root.child("clrMap")) {
        set_map(m);
    }
    for tree in [parts.layout.as_ref(), Some(&parts.slide)].into_iter().flatten() {
        if let Some(o) = tree.root.child("clrMapOvr").and_then(|c| c.child("overrideClrMapping")) {
            set_map(o);
        }
    }
    // Text styles (they need the colour map, so they come last).
    let default_lvls = pres.child("defaultTextStyle").map(|n| look.read_levels(n));
    let styles = parts.master.as_ref().and_then(|m| m.root.child("txStyles"));
    let get = |n: &str| styles.and_then(|s| s.child(n)).map(|l| look.read_levels(l));
    let (title, body, other) = (get("titleStyle"), get("bodyStyle"), get("otherStyle"));
    if let Some(l) = default_lvls {
        look.default_lvls = l;
    }
    if let Some(l) = title {
        look.title = l;
    }
    if let Some(l) = body {
        look.body = l;
    }
    if let Some(l) = other {
        look.other = l;
    }
    look
}

/// The page size from `sldSz`, with a note when it is missing or not believable.
fn slide_size(pres: &Node, cv: &mut Canvas) -> (f64, f64) {
    let sz = pres.child("sldSz");
    let w = side(emu_pt(sz.and_then(|s| s.attr("cx"))));
    let h = side(emu_pt(sz.and_then(|s| s.attr("cy"))));
    match (w, h) {
        (Some(w), Some(h)) => (w, h),
        _ => {
            cv.approximated("the slide size is missing or not believable");
            DEFAULT_SIZE
        }
    }
}

/// What the slide is painted with before its shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Background {
    /// A colour (`0` for none).
    Color(u32),
    /// A picture of the package, stretched over the slide.
    Picture(u32),
}

/// The background paint of the slide: its own, else the layout's, else the master's. `rels` are
/// the relationship tables of the slide, the layout and the master, in that order.
fn background(look: &Look, parts: &Parts, rels: &[RelTab; 3], cv: &mut Canvas) -> Background {
    for (tree, rels) in [Some(&parts.slide), parts.layout.as_ref(), parts.master.as_ref()]
        .into_iter()
        .zip(rels)
        .filter_map(|(t, r)| t.map(|t| (t, r)))
    {
        let Some(bg) = tree.root.child("cSld").and_then(|c| c.child("bg")) else { continue };
        if let Some(pr) = bg.child("bgPr") {
            match look.paint_in(pr, None, cv) {
                Paint::Color(c) => return Background::Color(c),
                Paint::None => return Background::Color(0),
                Paint::Picture => {
                    let id = fill_picture(pr, rels);
                    if id != IMAGE_NONE {
                        if pr.child("blipFill").is_some_and(|b| b.child("tile").is_some()) {
                            cv.approximated("a tiled picture background is drawn stretched");
                        }
                        return Background::Picture(id);
                    }
                    cv.approximated("a picture background that is not in the file is not drawn");
                    return Background::Color(0);
                }
                Paint::Unset => {}
            }
        }
        if let Some(r) = bg.child("bgRef") {
            match look.ref_fill(r, cv) {
                Paint::Color(c) => return Background::Color(c),
                Paint::Picture => {
                    cv.approximated("a picture background from the theme is not drawn")
                }
                Paint::None | Paint::Unset => {}
            }
            if let Some(c) = look.color_in(r, None) {
                return Background::Color(c);
            }
        }
    }
    Background::Color(0)
}

/// Runs the deck: counts the slides and, if `want` is given, draws that slide.
pub(super) fn run(pkg: &mut Package<'_>, want: Option<usize>, policy: &PreviewPolicy) -> Outcome {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no presentation part".into()))?;
    let text = read_part(pkg, &main)?
        .ok_or_else(|| PreviewError::Malformed("the presentation part is missing".into()))?;
    let pres = parse(&text, pkg.limits(), &[], &REL_ELEMS)
        .map_err(|_| PreviewError::Malformed("the presentation part is not readable".into()))?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(list) = pres.root.child("sldIdLst") {
        for id in list.kids_named("sldId").take(policy.max_sections) {
            let Some(rid) = id.rel("id") else { continue };
            let Some((part, external)) = pkg.rel_by_id(&main, rid)? else { continue };
            if external || pkg.size_of(&part).is_none() {
                continue;
            }
            parts.push(part);
        }
    }
    if !policy.reveal_hidden {
        let mut shown = Vec::with_capacity(parts.len());
        for part in parts {
            let hidden = match read_part(pkg, &part)? {
                Some(t) => {
                    root_attr(&t, pkg.limits(), "show").is_some_and(|v| off(Some(v.as_str())))
                }
                None => false,
            };
            if !hidden {
                shown.push(part);
            }
        }
        parts = shown;
    }
    let pages = parts.len();
    let Some(index) = want else { return Ok((pages, None)) };
    let slide_part = parts
        .get(index)
        .ok_or_else(|| PreviewError::Malformed("the document has no such page".into()))?
        .clone();

    let (loaded, tabs) = load_parts(pkg, &main, &slide_part)?;
    let diagrams = load_diagrams(pkg, &slide_part)?;
    let table_styles = match pkg.rel_target(&main, "tableStyles")? {
        Some(p) => read_tree(pkg, &p, "tblStyleLst")?,
        None => None,
    };
    let mut cv = Canvas::new();
    let (w, h) = slide_size(&pres.root, &mut cv);
    let look = build_look(&loaded, &pres.root);
    cv.push(Prim::Save);
    cv.push(Prim::Clip { x: 0.0, y: 0.0, w: f32c(w), h: f32c(h) });
    match background(&look, &loaded, &tabs, &mut cv) {
        Background::Color(bg) if bg >> 24 != 0 && bg != 0xFFFF_FFFF => {
            cv.path(&presets::rect(w, h), &Mat::ID, bg, 0, 0.0, false);
        }
        Background::Picture(id) => cv.image(&Mat::ID, w, h, "", id),
        Background::Color(_) => {}
    }
    fn tree(p: &super::Parsed) -> Option<&Node> {
        p.root.child("cSld").and_then(|c| c.child("spTree"))
    }
    let slide_tree = tree(&loaded.slide);
    let layout_tree = loaded.layout.as_ref().and_then(tree);
    let master_tree = loaded.master.as_ref().and_then(tree);
    let slide_shows = !off(loaded.slide.root.attr("showMasterSp"));
    let layout_shows = loaded.layout.as_ref().is_none_or(|l| !off(l.root.attr("showMasterSp")));
    {
        let mut pass = Pass {
            cv: &mut cv,
            look: &look,
            reveal: policy.reveal_hidden,
            slide_no: index + 1,
            layout: layout_tree,
            master: master_tree,
            rels: &tabs[2],
            table_styles: table_styles.as_ref().map(|t| &t.root),
            diagrams: &diagrams,
        };
        if slide_shows {
            if layout_shows && let Some(t) = master_tree {
                pass.rels = &tabs[2];
                pass.shapes(t, &Mat::ID, 0, true);
            }
            if let Some(t) = layout_tree {
                pass.rels = &tabs[1];
                pass.shapes(t, &Mat::ID, 0, true);
            }
        }
        if let Some(t) = slide_tree {
            pass.rels = &tabs[0];
            pass.shapes(t, &Mat::ID, 0, false);
        }
    }
    cv.push(Prim::Restore);
    let damaged = loaded.slide.damaged || loaded.slide.cut;
    if loaded.slide.cut {
        cv.full = true;
    }
    let Rendered { page, notes } = cv.finish(w, h, index, pages, damaged);
    Ok((pages, Some(Rendered { page, notes })))
}
