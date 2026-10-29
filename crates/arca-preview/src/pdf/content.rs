//! Content-stream interpreter: runs the text-related operators of a page and records the
//! text it shows — nothing is drawn, and nothing in the stream is ever executed beyond
//! updating a handful of numbers.
//!
//! The interpreter also decides, for every piece of text, whether a reader of the original
//! document could actually *see* it: invisible render mode, zero opacity, white text on an
//! otherwise unpainted page, text far outside the page box, a microscopic font, or content
//! in an optional-content group that is switched off. Such text is kept and flagged
//! [`style::HIDDEN`], which is the core of the "show what the file really contains" promise.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::draw::{BLACK, Rec, Rgb, cmyk, color};
use super::file::PdfFile;
use super::font::Font;
use super::lex::{Dict, Lexer, MAX_ELEMS, Obj, Stream, Tok, int_to_f64, is_ws};
use crate::model::{ImageInfo, style};
use crate::page::{Prim, style as pstyle};

/// Largest decoded content stream (and form XObject).
const MAX_CONTENT: usize = 96 << 20;
/// Deepest nesting of form XObjects.
const MAX_FORM_DEPTH: usize = 8;
/// Form XObjects executed per page.
const MAX_FORMS_PER_PAGE: usize = 4_000;
/// Forms run in one document.
const MAX_FORMS_PER_DOCUMENT: usize = 100_000;
/// Pictures recorded per page.
const MAX_IMAGES_PER_PAGE: usize = 5_000;
/// Operands kept for one operator.
const MAX_OPERANDS: usize = 64;
/// Graphics-state stack depth.
const MAX_STACK: usize = 128;
/// Most fonts cached for a document.
const MAX_FONTS: usize = 4_000;
/// Text lines recorded per page.
const MAX_LINES_PER_PAGE: usize = 200_000;
/// Painted areas remembered for the white-text check.
const MAX_BACKDROPS: usize = 512;

/// A 2-D affine transform `[a b c d e f]` in PDF's row-vector convention.
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

    pub(super) fn from_numbers(n: &[f64]) -> Option<Self> {
        match n {
            [a, b, c, d, e, f] if [a, b, c, d, e, f].iter().all(|v| v.is_finite()) => {
                Some(Self { a: *a, b: *b, c: *c, d: *d, e: *e, f: *f })
            }
            _ => None,
        }
    }
}

/// A run of text with uniform style.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Seg {
    pub(super) text: String,
    pub(super) style: u16,
}

/// One line of text as laid out in the file.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line {
    pub(super) segs: Vec<Seg>,
    /// Start and end along the baseline, in user-space units.
    pub(super) a0: f64,
    pub(super) a1: f64,
    /// Baseline position across the text direction.
    pub(super) across: f64,
    /// Font size in user-space units.
    pub(super) size: f64,
}

impl Line {
    pub(super) fn text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }
}

/// What a page contains, in drawing order.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Item {
    Line(Line),
    Image(ImageInfo),
}

/// Everything the interpreter needs to know about a page.
#[derive(Debug)]
pub(super) struct PageInput {
    pub(super) resources: Rc<Dict>,
    pub(super) contents: Vec<Rc<Stream>>,
    /// Visible area `[x0, y0, x1, y1]`.
    pub(super) visible: [f64; 4],
    /// Record what the page draws (for rendering), besides its text.
    pub(super) record: bool,
}

/// Result of running a page.
#[derive(Debug, Default)]
pub(super) struct PageOut {
    pub(super) items: Vec<Item>,
    /// Codes without a Unicode mapping (shown as U+FFFD).
    pub(super) unmapped: usize,
    /// Codes whose text mapping disagrees with the font's encoding: what the page shows may not
    /// be what the text says.
    pub(super) mismatched: usize,
    /// Pieces of text flagged hidden.
    pub(super) hidden: usize,
    /// The page was cut short by an operator, form or line limit.
    pub(super) truncated: bool,
    /// A content stream could not be decoded.
    pub(super) damaged: bool,
    /// What the page draws, if it was asked for.
    pub(super) drawn: Option<Rec>,
}

/// State shared by all pages of a document.
#[derive(Debug)]
pub(super) struct Shared {
    pub(super) fonts: HashMap<u32, Rc<Font>>,
    pub(super) ocg_off: HashSet<u32>,
    pub(super) ops_left: u64,
    /// Form XObjects that may still be run in the whole document: the per-page limit alone lets
    /// a thousand pages each run thousands of them.
    pub(super) forms_left: usize,
    pub(super) fallback: Rc<Font>,
}

impl Shared {
    pub(super) fn new(ocg_off: HashSet<u32>, ops: u64) -> Self {
        Self {
            fonts: HashMap::new(),
            ocg_off,
            ops_left: ops,
            forms_left: MAX_FORMS_PER_DOCUMENT,
            fallback: Rc::new(Font::fallback()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Space {
    Gray,
    Rgb,
    Cmyk,
    Other,
}

#[derive(Debug, Clone)]
struct Gs {
    ctm: Mat,
    space: Space,
    white: bool,
    alpha: f64,
    font: Option<Rc<Font>>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
    render: i64,
    /// Fill and stroke colours, and the stroke's colour space and width.
    fill_rgb: Rgb,
    stroke_rgb: Rgb,
    stroke_space: Space,
    line_width: f64,
}

impl Gs {
    fn new(ctm: Mat) -> Self {
        Self {
            ctm,
            space: Space::Gray,
            white: false,
            alpha: 1.0,
            font: None,
            size: 0.0,
            tc: 0.0,
            tw: 0.0,
            th: 1.0,
            tl: 0.0,
            rise: 0.0,
            render: 0,
            fill_rgb: BLACK,
            stroke_rgb: BLACK,
            stroke_space: Space::Gray,
            line_width: 1.0,
        }
    }
}

#[derive(Debug)]
struct Cur {
    line: Line,
    dir: (f64, f64),
    last_text: String,
    last_start: f64,
    last_end: f64,
}

struct Interp<'p, 'a> {
    pdf: &'p mut PdfFile<'a>,
    shared: &'p mut Shared,
    out: PageOut,
    gs: Gs,
    stack: Vec<Gs>,
    tm: Mat,
    tlm: Mat,
    cur: Option<Cur>,
    mc: Vec<bool>,
    mc_hidden: usize,
    path: Option<([f64; 2], [f64; 2])>,
    /// A large area (page-sized fill, shading or picture) has been painted.
    bg_page: bool,
    /// Smaller non-white filled areas `[x0, y0, x1, y1]` (badges, bars, table cells).
    bg_rects: Vec<[f64; 4]>,
    visible: [f64; 4],
    page_area: f64,
    form_depth: usize,
    forms_left: usize,
    images_left: usize,
    active_forms: Vec<u32>,
    stopped: bool,
    rec: Option<Rec>,
}

/// Runs the content of one page.
pub(super) fn run_page(pdf: &mut PdfFile<'_>, shared: &mut Shared, page: &PageInput) -> PageOut {
    let [x0, y0, x1, y1] = page.visible;
    let area = (x1 - x0).abs() * (y1 - y0).abs();
    let mut it = Interp {
        pdf,
        shared,
        out: PageOut::default(),
        gs: Gs::new(Mat::ID),
        stack: Vec::new(),
        tm: Mat::ID,
        tlm: Mat::ID,
        cur: None,
        mc: Vec::new(),
        mc_hidden: 0,
        path: None,
        bg_page: false,
        bg_rects: Vec::new(),
        visible: page.visible,
        page_area: if area > 1.0 { area } else { 612.0 * 792.0 },
        form_depth: 0,
        forms_left: MAX_FORMS_PER_PAGE,
        images_left: MAX_IMAGES_PER_PAGE,
        active_forms: Vec::new(),
        stopped: false,
        rec: page.record.then(Rec::default),
    };
    // Several content streams form one stream; they may split between tokens only.
    let mut all: Vec<u8> = Vec::new();
    for s in &page.contents {
        match it.pdf.stream_data(s, MAX_CONTENT.saturating_sub(all.len())) {
            Ok(d) => {
                it.out.damaged |= d.damaged;
                it.out.truncated |= d.capped;
                all.extend_from_slice(&d.data);
                all.push(b'\n');
            }
            Err(_) => it.out.damaged = true,
        }
    }
    it.exec(&all, &page.resources);
    it.flush_line();
    it.out.drawn = it.rec.take();
    it.out
}

fn near_white(space: Space, v: &[f64]) -> bool {
    match space {
        Space::Gray => matches!(v, [g] if *g >= 0.98),
        Space::Rgb => matches!(v, [r, g, b] if *r >= 0.98 && *g >= 0.98 && *b >= 0.98),
        Space::Cmyk => {
            matches!(v, [c, m, y, k] if *c <= 0.02 && *m <= 0.02 && *y <= 0.02 && *k <= 0.02)
        }
        Space::Other => false,
    }
}

fn nums(operands: &[Obj]) -> Vec<f64> {
    operands.iter().filter_map(Obj::as_f64).collect()
}

fn ends_with_space(segs: &[Seg]) -> bool {
    segs.last().and_then(|s| s.text.chars().last()).is_some_and(char::is_whitespace)
}

impl Interp<'_, '_> {
    fn hidden_now(&self) -> bool {
        self.mc_hidden > 0
    }

    fn exec(&mut self, data: &[u8], resources: &Rc<Dict>) {
        let mut lx = Lexer::new(data, 0);
        let mut operands: Vec<Obj> = Vec::new();
        loop {
            if self.stopped {
                return;
            }
            let tok = lx.next_tok();
            match tok {
                Tok::Eof => return,
                Tok::Kw(op) if !matches!(op, b"true" | b"false" | b"null") => {
                    if self.shared.ops_left == 0 {
                        self.out.truncated = true;
                        self.stopped = true;
                        return;
                    }
                    self.shared.ops_left -= 1;
                    if op == b"BI" {
                        self.inline_image(&mut lx);
                    } else {
                        self.operator(op, &operands, resources);
                    }
                    operands.clear();
                }
                other => {
                    let mut budget = MAX_ELEMS;
                    if let Some(o) = lx.object_from(other, false, 0, &mut budget)
                        && operands.len() < MAX_OPERANDS
                    {
                        operands.push(o);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn operator(&mut self, op: &[u8], a: &[Obj], res: &Rc<Dict>) {
        match op {
            b"q" => {
                if self.stack.len() < MAX_STACK {
                    self.stack.push(self.gs.clone());
                    self.record(Prim::Save);
                }
            }
            b"Q" => {
                if let Some(g) = self.stack.pop() {
                    self.gs = g;
                    self.record(Prim::Restore);
                }
            }
            b"w" => self.gs.line_width = a.first().and_then(Obj::as_f64).unwrap_or(1.0).abs(),
            b"cm" => {
                if let Some(m) = Mat::from_numbers(&nums(a)) {
                    self.gs.ctm = m.then(self.gs.ctm);
                }
            }
            b"gs" => self.ext_gstate(a, res),
            // Text state.
            b"Tc" => self.gs.tc = a.first().and_then(Obj::as_f64).unwrap_or(0.0),
            b"Tw" => self.gs.tw = a.first().and_then(Obj::as_f64).unwrap_or(0.0),
            b"Tz" => self.gs.th = a.first().and_then(Obj::as_f64).unwrap_or(100.0) / 100.0,
            b"TL" => self.gs.tl = a.first().and_then(Obj::as_f64).unwrap_or(0.0),
            b"Ts" => self.gs.rise = a.first().and_then(Obj::as_f64).unwrap_or(0.0),
            b"Tr" => self.gs.render = a.first().and_then(Obj::as_int).unwrap_or(0),
            b"Tf" => {
                if let (Some(name), Some(size)) =
                    (a.first().and_then(Obj::as_name), a.get(1).and_then(Obj::as_f64))
                {
                    self.gs.font = Some(self.font(name, res));
                    self.gs.size = size;
                }
            }
            // Text objects and positioning.
            b"BT" => {
                self.tm = Mat::ID;
                self.tlm = Mat::ID;
            }
            b"ET" => {}
            b"Td" => self.move_text(a),
            b"TD" => {
                if let Some(ty) = a.get(1).and_then(Obj::as_f64) {
                    self.gs.tl = -ty;
                }
                self.move_text(a);
            }
            b"Tm" => {
                if let Some(m) = Mat::from_numbers(&nums(a)) {
                    self.tm = m;
                    self.tlm = m;
                }
            }
            b"T*" => self.next_line(),
            // Showing text.
            b"Tj" => {
                if let Some(s) = a.first().and_then(Obj::as_str) {
                    self.show(s);
                }
            }
            b"'" => {
                self.next_line();
                if let Some(s) = a.first().and_then(Obj::as_str) {
                    self.show(s);
                }
            }
            b"\"" => {
                if let (Some(aw), Some(ac)) =
                    (a.first().and_then(Obj::as_f64), a.get(1).and_then(Obj::as_f64))
                {
                    self.gs.tw = aw;
                    self.gs.tc = ac;
                }
                self.next_line();
                if let Some(s) = a.get(2).and_then(Obj::as_str) {
                    self.show(s);
                }
            }
            b"TJ" => {
                if let Some(items) = a.first().and_then(Obj::as_arr) {
                    for item in items {
                        match item {
                            Obj::Str(s) => self.show(s),
                            n => {
                                if let Some(v) = n.as_f64() {
                                    let tx = -v / 1000.0 * self.gs.size * self.gs.th;
                                    self.tm = Mat::translate(tx, 0.0).then(self.tm);
                                }
                            }
                        }
                    }
                }
            }
            // Colour.
            b"g" => self.set_fill(Space::Gray, &nums(a)),
            b"rg" => self.set_fill(Space::Rgb, &nums(a)),
            b"k" => self.set_fill(Space::Cmyk, &nums(a)),
            b"G" => self.set_stroke(Space::Gray, &nums(a)),
            b"RG" => self.set_stroke(Space::Rgb, &nums(a)),
            b"K" => self.set_stroke(Space::Cmyk, &nums(a)),
            b"cs" => {
                let space = a
                    .first()
                    .and_then(Obj::as_name)
                    .map_or(Space::Other, |n| self.space_of(n, res));
                self.gs.space = space;
                // Initial colour: black for device spaces; other spaces are never "white".
                self.gs.white = false;
                self.gs.fill_rgb = BLACK;
            }
            b"CS" => {
                let space = a
                    .first()
                    .and_then(Obj::as_name)
                    .map_or(Space::Other, |n| self.space_of(n, res));
                self.gs.stroke_space = space;
                self.gs.stroke_rgb = BLACK;
            }
            b"sc" | b"scn" => {
                let v = nums(a);
                self.gs.white = near_white(self.gs.space, &v);
                self.gs.fill_rgb = self.colour_of(self.gs.space, &v, a);
            }
            b"SC" | b"SCN" => {
                let v = nums(a);
                self.gs.stroke_rgb = self.colour_of(self.gs.stroke_space, &v, a);
            }
            // Paths: only the area they cover matters (a painted background).
            b"m" | b"l" => {
                if let (Some(x), Some(y)) =
                    (a.first().and_then(Obj::as_f64), a.get(1).and_then(Obj::as_f64))
                {
                    self.path_point(x, y);
                    let (px, py) = self.gs.ctm.apply(x, y);
                    if let Some(r) = self.rec.as_mut()
                        && px.is_finite()
                        && py.is_finite()
                    {
                        if op == b"m" {
                            r.move_to(px, py);
                        } else {
                            r.line_to(px, py);
                        }
                    }
                }
            }
            b"c" | b"v" | b"y" => {
                let v = nums(a);
                for pair in v.chunks_exact(2) {
                    if let [x, y] = pair {
                        self.path_point(*x, *y);
                    }
                }
                let ctm = self.gs.ctm;
                if let Some(r) = self.rec.as_mut() {
                    let pt = |i: usize| -> Option<[f64; 2]> {
                        let (x, y) = (v.get(2 * i)?, v.get(2 * i + 1)?);
                        let (px, py) = ctm.apply(*x, *y);
                        (px.is_finite() && py.is_finite()).then_some([px, py])
                    };
                    let cur = r.current();
                    let curve = match (op, v.len()) {
                        (b"c", 6) => pt(0).zip(pt(1)).zip(pt(2)).map(|((p1, p2), p3)| (p1, p2, p3)),
                        (b"v", 4) => pt(0).zip(pt(1)).map(|(p2, p3)| (cur, p2, p3)),
                        (b"y", 4) => pt(0).zip(pt(1)).map(|(p1, p3)| (p1, p3, p3)),
                        _ => None,
                    };
                    if let Some((p1, p2, p3)) = curve {
                        r.curve_to(p1, p2, p3);
                    }
                }
            }
            b"h" => {
                if let Some(r) = self.rec.as_mut() {
                    r.close();
                }
            }
            b"re" => {
                if let [x, y, w, h] = nums(a).as_slice() {
                    self.path_point(*x, *y);
                    self.path_point(*x + *w, *y + *h);
                    let ctm = self.gs.ctm;
                    if let Some(r) = self.rec.as_mut() {
                        let c = [
                            ctm.apply(*x, *y),
                            ctm.apply(*x + *w, *y),
                            ctm.apply(*x + *w, *y + *h),
                            ctm.apply(*x, *y + *h),
                        ];
                        if c.iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
                            r.rect(c.map(|p| [p.0, p.1]));
                        }
                    }
                }
            }
            b"f" | b"F" => self.paint_op(true, false, false, false),
            b"f*" => self.paint_op(true, false, true, false),
            b"B" => self.paint_op(true, true, false, false),
            b"B*" => self.paint_op(true, true, true, false),
            b"b" => self.paint_op(true, true, false, true),
            b"b*" => self.paint_op(true, true, true, true),
            b"S" => self.paint_op(false, true, false, false),
            b"s" => self.paint_op(false, true, false, true),
            b"n" => self.paint_op(false, false, false, false),
            b"W" => {
                if let Some(r) = self.rec.as_mut() {
                    r.will_clip(false);
                }
            }
            b"W*" => {
                if let Some(r) = self.rec.as_mut() {
                    r.will_clip(true);
                }
            }
            b"sh" => {
                self.bg_page = true;
                if let Some(r) = self.rec.as_mut() {
                    r.approximate = true;
                }
            }
            // XObjects.
            b"Do" => {
                if let Some(name) = a.first().and_then(Obj::as_name) {
                    self.do_xobject(name, res);
                }
            }
            // Marked content (optional-content layers).
            b"BMC" => self.mc.push(false),
            b"BDC" => {
                let mut hidden = false;
                if a.first().and_then(Obj::as_name) == Some("OC") {
                    hidden = match a.get(1) {
                        Some(Obj::Name(n)) => {
                            let raw = self.resource("Properties", n, res);
                            self.oc_hidden(&raw)
                        }
                        Some(o @ Obj::Dict(_)) => self.oc_hidden(&o.clone()),
                        _ => false,
                    };
                }
                if hidden {
                    self.mc_hidden += 1;
                }
                if self.mc.len() < 4 * MAX_STACK {
                    self.mc.push(hidden);
                } else if hidden {
                    self.mc_hidden -= 1;
                }
            }
            b"EMC" => {
                let was_hidden = self.mc.pop() == Some(true);
                if was_hidden {
                    self.mc_hidden = self.mc_hidden.saturating_sub(1);
                }
            }
            _ => {}
        }
    }

    fn set_fill(&mut self, space: Space, v: &[f64]) {
        self.gs.space = space;
        self.gs.white = near_white(space, v);
        self.gs.fill_rgb = self.colour_of(space, v, &[]);
    }

    fn set_stroke(&mut self, space: Space, v: &[f64]) {
        self.gs.stroke_space = space;
        self.gs.stroke_rgb = self.colour_of(space, v, &[]);
    }

    /// The colour `v` is in `space`; a pattern or a colour space this reader does not convert
    /// is drawn as a light grey and marks the page as approximate.
    fn colour_of(&mut self, space: Space, v: &[f64], operands: &[Obj]) -> Rgb {
        if matches!(operands.last(), Some(Obj::Name(_))) {
            if let Some(r) = self.rec.as_mut() {
                r.approximate = true;
            }
            return [0.85, 0.85, 0.85];
        }
        let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
        match (space, v.len()) {
            (Space::Gray, 1) => [c(0).clamp(0.0, 1.0); 3],
            (Space::Rgb | Space::Other, 3) => {
                [c(0).clamp(0.0, 1.0), c(1).clamp(0.0, 1.0), c(2).clamp(0.0, 1.0)]
            }
            (Space::Cmyk | Space::Other, 4) => cmyk(c(0), c(1), c(2), c(3)),
            // A tint of a spot colour or an indexed colour: by its tint, as grey.
            (Space::Other, 1) => [1.0 - c(0).clamp(0.0, 1.0); 3],
            (Space::Other, _) => {
                if let Some(r) = self.rec.as_mut() {
                    r.approximate = true;
                }
                [0.5; 3]
            }
            _ => BLACK,
        }
    }

    fn record(&mut self, p: Prim) {
        if let Some(r) = self.rec.as_mut() {
            r.push(p);
            if r.full {
                self.out.truncated = true;
                self.stopped = true;
            }
        }
    }

    /// Paints (and/or strokes) the path under construction, applies a pending clip, and ends
    /// the path.
    fn paint_op(&mut self, fill: bool, stroke: bool, even_odd: bool, close: bool) {
        if fill {
            self.paint_fill();
        }
        self.path = None;
        if self.rec.is_none() {
            return;
        }
        let hidden = self.hidden_now();
        let alpha = self.gs.alpha;
        let (f, s) = (color(self.gs.fill_rgb, alpha), color(self.gs.stroke_rgb, alpha));
        let m = self.gs.ctm;
        let scale = ((m.a * m.d - m.b * m.c).abs()).sqrt();
        let width = (self.gs.line_width * scale).max(0.25);
        if let Some(r) = self.rec.as_mut() {
            if close {
                r.close();
            }
            if !hidden {
                r.paint(if fill { f } else { 0 }, if stroke { s } else { 0 }, width, even_odd);
            }
        }
        if let Some(mut r) = self.rec.take() {
            r.end_path();
            self.rec = Some(r);
        }
        if self.rec.as_ref().is_some_and(|r| r.full) {
            self.out.truncated = true;
            self.stopped = true;
        }
    }

    fn space_of(&mut self, name: &str, res: &Rc<Dict>) -> Space {
        match name {
            "DeviceGray" | "G" | "CalGray" => Space::Gray,
            "DeviceRGB" | "RGB" | "CalRGB" => Space::Rgb,
            "DeviceCMYK" | "CMYK" => Space::Cmyk,
            "Pattern" => Space::Other,
            other => {
                let raw = self.resource("ColorSpace", other, res);
                let cs = self.pdf.resolve(&raw);
                match &cs {
                    Obj::Name(n) => match n.as_ref() {
                        "DeviceGray" | "CalGray" => Space::Gray,
                        "DeviceRGB" | "CalRGB" => Space::Rgb,
                        "DeviceCMYK" => Space::Cmyk,
                        _ => Space::Other,
                    },
                    Obj::Arr(items) => match items.first().and_then(Obj::as_name) {
                        Some("CalGray") => Space::Gray,
                        Some("CalRGB") => Space::Rgb,
                        Some("ICCBased") => {
                            let s = items.get(1).map(|o| self.pdf.resolve(o)).unwrap_or_default();
                            let n = s.as_dict().and_then(|d| d.get("N")).and_then(Obj::as_int);
                            match n {
                                Some(1) => Space::Gray,
                                Some(3) => Space::Rgb,
                                Some(4) => Space::Cmyk,
                                _ => Space::Other,
                            }
                        }
                        _ => Space::Other,
                    },
                    _ => Space::Other,
                }
            }
        }
    }

    fn ext_gstate(&mut self, a: &[Obj], res: &Rc<Dict>) {
        let Some(name) = a.first().and_then(Obj::as_name) else { return };
        let raw = self.resource("ExtGState", name, res);
        let d = self.pdf.resolve(&raw);
        if let Some(d) = d.as_dict() {
            let d = d.clone();
            if let Some(ca) = self.pdf.get(&d, "ca").as_f64() {
                self.gs.alpha = ca;
            }
        }
    }

    /// The raw (unresolved) entry `name` of the resource category `kind`.
    fn resource(&mut self, kind: &str, name: &str, res: &Rc<Dict>) -> Obj {
        let cat = self.pdf.get(res, kind);
        cat.as_dict().and_then(|d| d.get(name)).cloned().unwrap_or(Obj::Null)
    }

    fn font(&mut self, name: &str, res: &Rc<Dict>) -> Rc<Font> {
        let raw = self.resource("Font", name, res);
        if let Obj::Ref(n, _) = raw {
            if let Some(f) = self.shared.fonts.get(&n) {
                return f.clone();
            }
            let obj = self.pdf.object(n);
            let Some(dict) = obj.as_dict().cloned() else { return self.shared.fallback.clone() };
            let f = Rc::new(Font::load(self.pdf, &dict));
            if self.shared.fonts.len() < MAX_FONTS {
                self.shared.fonts.insert(n, f.clone());
            }
            return f;
        }
        match raw.as_dict().cloned() {
            Some(d) => Rc::new(Font::load(self.pdf, &d)),
            None => self.shared.fallback.clone(),
        }
    }

    // ---- text ---------------------------------------------------------------------------

    fn move_text(&mut self, a: &[Obj]) {
        if let (Some(tx), Some(ty)) =
            (a.first().and_then(Obj::as_f64), a.get(1).and_then(Obj::as_f64))
        {
            self.tlm = Mat::translate(tx, ty).then(self.tlm);
            self.tm = self.tlm;
        }
    }

    fn next_line(&mut self) {
        let tl = self.gs.tl;
        self.tlm = Mat::translate(0.0, -tl).then(self.tlm);
        self.tm = self.tlm;
    }

    /// `true` if something non-white was painted behind the point (so white text on it is
    /// legible: a badge, a coloured bar, a dark slide).
    fn backed(&self, x: f64, y: f64) -> bool {
        self.bg_page
            || self
                .bg_rects
                .iter()
                .any(|r| x >= r[0] - 2.0 && x <= r[2] + 2.0 && y >= r[1] - 2.0 && y <= r[3] + 2.0)
    }

    fn off_page(&self, x: f64, y: f64) -> bool {
        let [x0, y0, x1, y1] = self.visible;
        let margin = 4.0;
        x < x0.min(x1) - margin
            || x > x0.max(x1) + margin
            || y < y0.min(y1) - margin
            || y > y0.max(y1) + margin
    }

    fn show(&mut self, bytes: &[u8]) {
        if self.stopped {
            return;
        }
        let font = self.gs.font.clone().unwrap_or_else(|| self.shared.fallback.clone());
        let shown = font.show(bytes);
        let fs = self.gs.size;
        let tx = (shown.advance / 1000.0 * fs
            + int_to_f64(i64::try_from(shown.glyphs).unwrap_or(0)) * self.gs.tc
            + int_to_f64(i64::try_from(shown.spaces).unwrap_or(0)) * self.gs.tw)
            * self.gs.th;
        self.out.unmapped = self.out.unmapped.saturating_add(shown.unmapped);
        self.out.mismatched = self.out.mismatched.saturating_add(shown.mismatched);
        if shown.clipped {
            self.out.truncated = true;
        }
        let m = self.tm.then(self.gs.ctm);
        let (x, y) = m.apply(0.0, self.gs.rise);
        let scale_x = m.a.hypot(m.b);
        let size = fs.abs() * m.c.hypot(m.d);
        if !shown.text.is_empty() && scale_x > 1e-9 && x.is_finite() && y.is_finite() {
            let (ux, uy) = (m.a / scale_x, m.b / scale_x);
            let along = x * ux + y * uy;
            let across = -x * uy + y * ux;
            let blank = shown.text.trim().is_empty();
            let hidden = !blank
                && (self.gs.render == 3
                    || self.gs.render == 7
                    || self.gs.alpha <= 0.01
                    || size < 1.0
                    || self.hidden_now()
                    || self.off_page(x, y)
                    || (self.gs.white && !self.backed(x, y)));
            let mut st = 0u16;
            if font.bold {
                st |= style::BOLD;
            }
            if font.italic {
                st |= style::ITALIC;
            }
            if font.mono {
                st |= style::MONO;
            }
            if hidden {
                st |= style::HIDDEN;
                self.out.hidden += 1;
            }
            let end = along + tx * scale_x;
            if !hidden && !blank && self.rec.is_some() && !matches!(self.gs.render, 3 | 7) {
                let rgb = if matches!(self.gs.render, 1 | 5) {
                    self.gs.stroke_rgb
                } else {
                    self.gs.fill_rgb
                };
                let mut bits = 0u8;
                if font.bold {
                    bits |= pstyle::BOLD;
                }
                if font.italic {
                    bits |= pstyle::ITALIC;
                }
                if font.mono {
                    bits |= pstyle::MONO;
                }
                if font.serif {
                    bits |= pstyle::SERIF;
                }
                #[allow(clippy::cast_possible_truncation)]
                let prim = Prim::Text {
                    x: x as f32,
                    y: y as f32,
                    size: size as f32,
                    angle: uy.atan2(ux) as f32,
                    width: (tx * scale_x).abs() as f32,
                    style: bits,
                    color: color(rgb, self.gs.alpha),
                    text: shown.text.clone(),
                };
                self.record(prim);
            }
            self.emit(&shown.text, st, along, across, size, (ux, uy), end);
        }
        self.tm = Mat::translate(tx, 0.0).then(self.tm);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        text: &str,
        st: u16,
        along: f64,
        across: f64,
        size: f64,
        dir: (f64, f64),
        end: f64,
    ) {
        let sz = size.abs().max(0.1);
        if let Some(cur) = self.cur.as_mut() {
            // Text drawn twice at (almost) the same place is a bold/shadow effect.
            let span = (cur.last_end - cur.last_start).abs().max(0.1 * sz);
            if cur.last_text == text
                && (cur.last_start - along).abs() < 0.5 * span
                && (cur.line.across - across).abs() < 0.25 * sz
            {
                return;
            }
            let same_dir = cur.dir.0 * dir.0 + cur.dir.1 * dir.1 > 0.95;
            let dc = (across - cur.line.across).abs();
            let gap = along - cur.line.a1;
            if !same_dir || dc > 0.45 * sz.max(cur.line.size) || gap < -sz {
                self.flush_line();
            } else {
                if gap > 3.0 * sz {
                    cur.line.segs.push(Seg { text: "\t".into(), style: 0 });
                } else if gap > 0.15 * sz
                    && !ends_with_space(&cur.line.segs)
                    && !text.starts_with(char::is_whitespace)
                {
                    cur.line.segs.push(Seg { text: " ".into(), style: 0 });
                }
                match cur.line.segs.last_mut() {
                    Some(last) if last.style == st => last.text.push_str(text),
                    _ => cur.line.segs.push(Seg { text: text.to_owned(), style: st }),
                }
                cur.line.a1 = end;
                cur.line.size = cur.line.size.max(sz);
                cur.last_text = text.to_owned();
                cur.last_start = along;
                cur.last_end = end;
                return;
            }
        }
        self.cur = Some(Cur {
            line: Line {
                segs: vec![Seg { text: text.to_owned(), style: st }],
                a0: along,
                a1: end,
                across,
                size: sz,
            },
            dir,
            last_text: text.to_owned(),
            last_start: along,
            last_end: end,
        });
    }

    fn flush_line(&mut self) {
        let Some(cur) = self.cur.take() else { return };
        if self.out.items.len() >= MAX_LINES_PER_PAGE {
            self.out.truncated = true;
            self.stopped = true;
            return;
        }
        if cur.line.segs.iter().any(|s| !s.text.trim().is_empty()) {
            self.out.items.push(Item::Line(cur.line));
        }
    }

    // ---- paths and pictures -----------------------------------------------------------

    fn path_point(&mut self, x: f64, y: f64) {
        let (px, py) = self.gs.ctm.apply(x, y);
        if !px.is_finite() || !py.is_finite() {
            return;
        }
        self.path = Some(match self.path {
            None => ([px, py], [px, py]),
            Some((lo, hi)) => ([lo[0].min(px), lo[1].min(py)], [hi[0].max(px), hi[1].max(py)]),
        });
    }

    fn add_backdrop(&mut self, lo: [f64; 2], hi: [f64; 2]) {
        let area = (hi[0] - lo[0]) * (hi[1] - lo[1]);
        if area >= 0.25 * self.page_area {
            self.bg_page = true;
        } else if area >= 4.0 {
            if self.bg_rects.len() >= MAX_BACKDROPS {
                self.bg_rects.remove(0);
            }
            self.bg_rects.push([lo[0], lo[1], hi[0], hi[1]]);
        }
    }

    fn paint_fill(&mut self) {
        if self.gs.white {
            return;
        }
        if let Some((lo, hi)) = self.path {
            self.add_backdrop(lo, hi);
        }
    }

    /// A picture covers the unit square under the current transform.
    fn note_image_area(&mut self) {
        let m = self.gs.ctm;
        let corners = [m.apply(0.0, 0.0), m.apply(1.0, 0.0), m.apply(0.0, 1.0), m.apply(1.0, 1.0)];
        let xs = corners.iter().map(|c| c.0);
        let ys = corners.iter().map(|c| c.1);
        let lo =
            [xs.clone().fold(f64::INFINITY, f64::min), ys.clone().fold(f64::INFINITY, f64::min)];
        let hi = [xs.fold(f64::NEG_INFINITY, f64::max), ys.fold(f64::NEG_INFINITY, f64::max)];
        if lo.iter().chain(hi.iter()).all(|v| v.is_finite()) {
            self.add_backdrop(lo, hi);
        }
    }

    fn push_image(&mut self, info: ImageInfo) {
        self.flush_line();
        if self.images_left == 0 {
            self.out.truncated = true;
            return;
        }
        self.images_left -= 1;
        self.out.items.push(Item::Image(info));
    }

    /// A picture covers the unit square under the current transform; it is drawn as a placeholder.
    fn record_image(&mut self, info: &ImageInfo) {
        if self.rec.is_none() || self.hidden_now() {
            return;
        }
        let m = self.gs.ctm;
        if ![m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite()) {
            return;
        }
        if let Some(r) = self.rec.as_mut() {
            r.approximate = true;
        }
        #[allow(clippy::cast_possible_truncation)]
        let prim = Prim::Image {
            m: [m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32],
            label: info.alt.clone(),
            id: info.id,
        };
        self.record(prim);
    }

    fn do_xobject(&mut self, name: &str, res: &Rc<Dict>) {
        let raw = self.resource("XObject", name, res);
        let num = raw.ref_num();
        let obj = self.pdf.resolve(&raw);
        let Some(stream) = obj.as_stream().cloned() else { return };
        let subtype = self.pdf.get(&stream.dict, "Subtype").as_name().unwrap_or("").to_owned();
        let oc = stream.dict.get("OC").cloned().unwrap_or(Obj::Null);
        let oc_hidden = self.oc_hidden(&oc);
        match subtype.as_str() {
            "Image" => {
                self.note_image_area();
                let w = self.pdf.get(&stream.dict, "Width").as_int().unwrap_or(0);
                let h = self.pdf.get(&stream.dict, "Height").as_int().unwrap_or(0);
                let filter = self.pdf.get(&stream.dict, "Filter");
                let last = match &filter {
                    Obj::Name(n) => Some(n.to_string()),
                    Obj::Arr(a) => a.last().and_then(Obj::as_name).map(str::to_owned),
                    _ => None,
                };
                let info = ImageInfo {
                    alt: if w > 0 && h > 0 { format!("{w}×{h}") } else { String::new() },
                    media: media_of(last.as_deref()).to_owned(),
                    bytes: u64::try_from(self.pdf.raw_len(&stream)).unwrap_or(0),
                    external: false,
                    id: num.unwrap_or(crate::model::IMAGE_NONE),
                    ..ImageInfo::default()
                };
                self.record_image(&info);
                self.push_image(info);
            }
            "Form" => {
                if self.form_depth >= MAX_FORM_DEPTH
                    || self.forms_left == 0
                    || self.shared.forms_left == 0
                {
                    self.out.truncated = true;
                    return;
                }
                if let Some(n) = num
                    && self.active_forms.contains(&n)
                {
                    return;
                }
                self.forms_left -= 1;
                self.shared.forms_left -= 1;
                let Ok(decoded) = self.pdf.stream_data(&stream, MAX_CONTENT) else {
                    self.out.damaged = true;
                    return;
                };
                self.out.truncated |= decoded.capped;
                let matrix = Mat::from_numbers(&self.pdf.get_numbers(&stream.dict, "Matrix"))
                    .unwrap_or(Mat::ID);
                let own = self.pdf.get(&stream.dict, "Resources");
                let form_res = match own {
                    Obj::Dict(d) => d,
                    _ => res.clone(),
                };
                // Run the form with its own graphics and text state.
                let saved_gs = self.gs.clone();
                let saved_stack = std::mem::take(&mut self.stack);
                let (saved_tm, saved_tlm) = (self.tm, self.tlm);
                let saved_mc = self.mc.len();
                let saved_hidden = self.mc_hidden;
                self.gs.ctm = matrix.then(self.gs.ctm);
                if oc_hidden {
                    self.mc_hidden += 1;
                }
                self.form_depth += 1;
                if let Some(n) = num {
                    self.active_forms.push(n);
                }
                // What a form clips stays inside it.
                self.record(Prim::Save);
                self.exec(&decoded.data, &form_res);
                self.record(Prim::Restore);
                if num.is_some() {
                    self.active_forms.pop();
                }
                self.form_depth -= 1;
                self.gs = saved_gs;
                self.stack = saved_stack;
                self.tm = saved_tm;
                self.tlm = saved_tlm;
                self.mc.truncate(saved_mc);
                self.mc_hidden = saved_hidden;
            }
            _ => {}
        }
    }

    /// `true` if the optional-content group or membership dictionary `oc` is switched off.
    fn oc_hidden(&mut self, oc: &Obj) -> bool {
        match oc {
            Obj::Null => false,
            Obj::Ref(n, _) => {
                if self.shared.ocg_off.contains(n) {
                    return true;
                }
                let d = self.pdf.object(*n);
                self.membership_hidden(&d)
            }
            other => self.membership_hidden(&other.clone()),
        }
    }

    fn membership_hidden(&mut self, d: &Obj) -> bool {
        let Some(dict) = d.as_dict() else { return false };
        if dict.get("Type").and_then(Obj::as_name) != Some("OCMD") {
            return false;
        }
        let dict = dict.clone();
        let groups = match dict.get("OCGs") {
            Some(Obj::Ref(n, _)) => vec![*n],
            Some(Obj::Arr(a)) => a.iter().filter_map(Obj::ref_num).collect(),
            _ => Vec::new(),
        };
        !groups.is_empty() && groups.iter().all(|g| self.shared.ocg_off.contains(g))
    }

    fn inline_image(&mut self, lx: &mut Lexer<'_>) {
        let mut dict = Dict::new();
        let mut budget = 256usize;
        loop {
            match lx.next_tok() {
                Tok::Kw(b"ID") => break,
                Tok::Eof => return,
                Tok::Name(key) => {
                    let t = lx.next_tok();
                    if t == Tok::Kw(b"ID") {
                        break;
                    }
                    if let Some(v) = lx.object_from(t, false, 0, &mut budget) {
                        dict.insert(Rc::from(key), v);
                    }
                }
                _ => {}
            }
            if dict.len() > 32 {
                break;
            }
        }
        // One white-space byte separates `ID` from the data.
        if lx.data.get(lx.pos).is_some_and(|b| is_ws(*b)) {
            lx.pos += 1;
        }
        let start = lx.pos;
        let get = |long: &str, short: &str| dict.get(long).or_else(|| dict.get(short));
        let int = |long: &str, short: &str| get(long, short).and_then(Obj::as_int).unwrap_or(0);
        let (w, h, bpc) = (int("Width", "W"), int("Height", "H"), int("BitsPerComponent", "BPC"));
        let filter = get("Filter", "F").cloned();
        let comps = match get("ColorSpace", "CS").and_then(Obj::as_name) {
            Some("G" | "DeviceGray" | "CalGray" | "I" | "Indexed") => Some(1),
            Some("RGB" | "DeviceRGB" | "CalRGB") => Some(3),
            Some("CMYK" | "DeviceCMYK") => Some(4),
            _ => None,
        };
        let is_mask = matches!(get("ImageMask", "IM"), Some(Obj::Bool(true)));
        // Unfiltered data has a computable length; trust it when it is followed by `EI`.
        let mut end = None;
        if filter.is_none() && w > 0 && h > 0 {
            let bits = if is_mask { Some((1, 1)) } else { comps.map(|c| (bpc.max(1), c)) };
            if let Some((b, c)) = bits {
                let row = (w.saturating_mul(b).saturating_mul(c) + 7) / 8;
                if let Some(len) = row.checked_mul(h).and_then(|l| usize::try_from(l).ok())
                    && let Some(e) = start.checked_add(len)
                    && lx.data.get(e..).is_some_and(|rest| {
                        let t =
                            rest.iter().position(|b| !is_ws(*b)).and_then(|p| rest.get(p..p + 2));
                        t == Some(b"EI")
                    })
                {
                    end = Some(e);
                }
            }
        }
        let end = end.unwrap_or_else(|| find_ei(lx.data, start));
        // Continue after the `EI` keyword.
        let after = lx
            .data
            .get(end..)
            .and_then(|rest| rest.iter().position(|b| !is_ws(*b)))
            .map_or(end, |p| end + p);
        lx.pos = (after + 2).min(lx.data.len());
        let media = match filter.as_ref() {
            Some(Obj::Name(n)) => media_of(Some(n)),
            Some(Obj::Arr(a)) => media_of(a.last().and_then(Obj::as_name)),
            _ => "bitmap",
        };
        self.note_image_area();
        let info = ImageInfo {
            alt: if w > 0 && h > 0 { format!("{w}×{h}") } else { String::new() },
            media: media.to_owned(),
            bytes: u64::try_from(end.saturating_sub(start)).unwrap_or(0),
            external: false,
            ..ImageInfo::default()
        };
        self.record_image(&info);
        self.push_image(info);
    }
}

/// Offset of the `EI` that ends inline-image data starting at `start`.
fn find_ei(data: &[u8], start: usize) -> usize {
    let mut from = start;
    while let Some(p) = super::file::find(data, b"EI", from) {
        let before_ok = p == start || data.get(p - 1).is_some_and(|b| is_ws(*b));
        let after_ok = data.get(p + 2).is_none_or(|b| is_ws(*b));
        if before_ok && after_ok {
            return p;
        }
        from = p + 2;
    }
    data.len()
}

fn media_of(filter: Option<&str>) -> &'static str {
    match filter {
        Some("DCTDecode" | "DCT") => "jpeg",
        Some("JPXDecode") => "jp2",
        Some("JBIG2Decode") => "jbig2",
        Some("CCITTFaxDecode" | "CCF") => "ccitt",
        _ => "bitmap",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewPolicy;
    use crate::pdf::testpdf::PdfBuilder;

    fn run(content: &str) -> PageOut {
        run_with(PdfBuilder::with_content(content))
    }

    fn run_with(b: PdfBuilder) -> PageOut {
        let data = b.build();
        let mut pdf = PdfFile::open(&data, &PreviewPolicy::default()).expect("open");
        let page = pdf.object(3);
        let dict = page.as_dict().cloned().expect("page");
        let res = match pdf.get(&dict, "Resources") {
            Obj::Dict(d) => d,
            _ => Rc::new(Dict::new()),
        };
        let contents = match pdf.get(&dict, "Contents") {
            Obj::Stream(s) => vec![s],
            _ => Vec::new(),
        };
        let mut shared = Shared::new(HashSet::new(), 1_000_000);
        run_page(
            &mut pdf,
            &mut shared,
            &PageInput {
                resources: res,
                contents,
                visible: [0.0, 0.0, 612.0, 792.0],
                record: false,
            },
        )
    }

    fn lines(o: &PageOut) -> Vec<(String, u16)> {
        o.items
            .iter()
            .filter_map(|i| match i {
                Item::Line(l) => Some((l.text(), l.segs.first().map_or(0, |s| s.style))),
                Item::Image(_) => None,
            })
            .collect()
    }

    fn texts(o: &PageOut) -> Vec<String> {
        lines(o).into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn basic_lines_and_gaps() {
        let o = run("BT /F1 12 Tf 72 700 Td (Hello) Tj 0 -14 Td (second line) Tj ET \
                     BT /F1 12 Tf 72 600 Td [(Wo) -20 (rld) -400 (gap)] TJ ET");
        assert_eq!(texts(&o), ["Hello", "second line", "World gap"]);
        assert_eq!(o.hidden, 0);
    }

    #[test]
    fn same_line_pieces_join_with_space_or_tab() {
        let o = run(
            "BT /F1 10 Tf 72 700 Td (A) Tj 1 0 0 1 100 700 Tm (B) Tj 1 0 0 1 300 700 Tm (C) Tj ET",
        );
        assert_eq!(texts(&o), ["A B\tC"]);
    }

    #[test]
    fn quote_operators_and_leading() {
        let o = run("BT /F1 12 Tf 14 TL 72 700 Td (one) Tj T* (two) Tj (three) ' 1 2 (four) \" ET");
        assert_eq!(texts(&o), ["one", "two", "three", "four"]);
    }

    #[test]
    fn hidden_render_mode_alpha_size_and_offpage() {
        let o = run(
            "BT /F1 12 Tf 72 700 Td (visible) Tj 3 Tr 0 -20 Td (invisible) Tj 0 Tr 0 -20 Td (tiny) Tj ET \
             BT /F1 0.5 Tf 72 500 Td (microscopic) Tj ET \
             BT /F1 12 Tf 5000 300 Td (offpage) Tj ET",
        );
        let l = lines(&o);
        let hidden = |t: &str| l.iter().find(|(x, _)| x == t).map(|(_, s)| s & style::HIDDEN != 0);
        assert_eq!(hidden("visible"), Some(false));
        assert_eq!(hidden("invisible"), Some(true));
        assert_eq!(hidden("tiny"), Some(false));
        assert_eq!(hidden("microscopic"), Some(true));
        assert_eq!(hidden("offpage"), Some(true));
        assert_eq!(o.hidden, 3);
    }

    #[test]
    fn white_text_is_hidden_unless_something_dark_was_painted() {
        let on_blank = run("1 g BT /F1 12 Tf 72 700 Td (ghost) Tj ET 0 g BT 72 680 Td (ink) Tj ET");
        let l = lines(&on_blank);
        assert_eq!(
            l.iter().map(|(t, s)| (t.as_str(), s & style::HIDDEN != 0)).collect::<Vec<_>>(),
            [("ghost", true), ("ink", false)]
        );
        let on_dark =
            run("0.1 0.1 0.4 rg 0 600 612 192 re f 1 g BT /F1 12 Tf 72 700 Td (title) Tj ET");
        assert_eq!(
            lines(&on_dark).iter().map(|(_, s)| s & style::HIDDEN != 0).collect::<Vec<_>>(),
            [false]
        );
        // CMYK and RGB whites; a Separation tint of 1 is not white.
        let o = run("1 1 1 rg BT /F1 12 Tf 72 700 Td (a) Tj ET 0 0 0 0 k BT 72 680 Td (b) Tj ET");
        assert_eq!(o.hidden, 2);
    }

    #[test]
    fn white_text_on_a_small_badge_is_legible_but_beside_it_is_not() {
        let o = run(
            "0.8 0 0 rg 100 690 120 24 re f 1 g BT /F1 10 Tf 105 698 Td (on badge) Tj 1 0 0 1 300 698 Tm (off badge) Tj ET",
        );
        let l = lines(&o);
        let h: Vec<(String, bool)> = o
            .items
            .iter()
            .filter_map(|i| if let Item::Line(l) = i { Some(l) } else { None })
            .flat_map(|l| l.segs.iter().map(|s| (s.text.clone(), s.style & style::HIDDEN != 0)))
            .collect();
        assert!(!l.is_empty());
        assert!(h.iter().any(|(t, hid)| t.contains("on badge") && !hid), "{h:?}");
        assert!(h.iter().any(|(t, hid)| t.contains("off badge") && *hid), "{h:?}");
    }

    #[test]
    fn repeated_narrow_glyphs_survive_and_double_strikes_collapse() {
        let o = run("BT /F1 12 Tf 72 700 Td (Stel) Tj (l) Tj (ar) Tj ET");
        assert_eq!(texts(&o), ["Stellar"]);
        let p = run("BT /F1 12 Tf 72 700 Td (i) Tj 1 0 0 1 72.5 700 Tm (i) Tj ET");
        assert_eq!(texts(&p), ["i"]);
    }

    #[test]
    fn graphics_state_stack_restores_colour() {
        let o = run("q 1 g Q BT /F1 12 Tf 72 700 Td (kept) Tj ET");
        assert_eq!(o.hidden, 0);
        let p = run("q 1 g BT /F1 12 Tf 72 700 Td (gone) Tj ET Q");
        assert_eq!(p.hidden, 1);
    }

    #[test]
    fn duplicate_draws_are_merged_and_backwards_text_starts_a_line() {
        let o = run("BT /F1 12 Tf 72 700 Td (Bold) Tj 1 0 0 1 72.4 700 Tm (Bold) Tj ET");
        assert_eq!(texts(&o), ["Bold"]);
        let p = run("BT /F1 12 Tf 300 700 Td (right) Tj 1 0 0 1 72 700 Tm (left) Tj ET");
        assert_eq!(texts(&p), ["right", "left"]);
    }

    #[test]
    fn rotated_text_keeps_one_line() {
        let o = run("BT /F1 12 Tf 0 1 -1 0 300 100 Tm (up) Tj 0 1 -1 0 300 118 Tm (ward) Tj ET");
        assert_eq!(texts(&o), ["up ward"]);
    }

    #[test]
    fn forms_images_and_recursion() {
        let mut b = PdfBuilder::with_content(
            "q 1 0 0 1 0 0 cm /Fm1 Do Q /Im1 Do BI /W 2 /H 2 /BPC 8 /CS /G ID abcd EI BT /F1 12 Tf 72 100 Td (after) Tj ET",
        );
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R /Im1 7 0 R >> >> >>",
        );
        b.add_stream(6, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /Font << /F1 5 0 R >> /XObject << /Self 6 0 R >> >>", b"BT /F1 12 Tf 72 700 Td (inside form) Tj ET /Self Do");
        b.add_stream(7, "/Type /XObject /Subtype /Image /Width 40 /Height 30 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", b"\xff\xd8\xff");
        let o = run_with(b);
        assert_eq!(texts(&o), ["inside form", "after"]);
        let imgs: Vec<&ImageInfo> = o
            .items
            .iter()
            .filter_map(|i| if let Item::Image(x) = i { Some(x) } else { None })
            .collect();
        assert_eq!(imgs.len(), 2);
        assert_eq!(
            (imgs[0].alt.as_str(), imgs[0].media.as_str(), imgs[0].bytes),
            ("40×30", "jpeg", 3)
        );
        assert_eq!(
            (imgs[1].alt.as_str(), imgs[1].media.as_str(), imgs[1].bytes),
            ("2×2", "bitmap", 4)
        );
    }

    #[test]
    fn exponential_form_nesting_is_bounded() {
        // Each form draws the next one ten times: 10^N executions if unchecked.
        let mut b = PdfBuilder::with_content("/Fm1 Do");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >> >>",
        );
        for n in 6..16u32 {
            let next = n + 1;
            let body = "/Next Do ".repeat(10);
            b.add_stream(
                n,
                &format!("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Next {next} 0 R >> >>"),
                body.as_bytes(),
            );
        }
        b.add_stream(16, "/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b"q Q");
        let start = std::time::Instant::now();
        let o = run_with(b);
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
        assert!(o.truncated);
    }

    #[test]
    fn the_form_budget_belongs_to_the_document_not_to_each_page() {
        let mut b = PdfBuilder::with_content("/Fm1 Do /Fm1 Do /Fm1 Do");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Fm1 6 0 R >> >> >>",
        );
        b.add_stream(6, "/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b"q Q");
        let data = b.build();
        let mut pdf = PdfFile::open(&data, &PreviewPolicy::default()).expect("open");
        let page = pdf.object(3);
        let dict = page.as_dict().cloned().expect("page");
        let contents = match pdf.get(&dict, "Contents") {
            Obj::Stream(s) => vec![s],
            _ => Vec::new(),
        };
        let res = match pdf.get(&dict, "Resources") {
            Obj::Dict(d) => d,
            _ => Rc::new(Dict::new()),
        };
        let input = PageInput {
            resources: res,
            contents,
            visible: [0.0, 0.0, 612.0, 792.0],
            record: false,
        };
        let mut shared = Shared::new(HashSet::new(), 1_000_000);
        shared.forms_left = 4;
        // Two pages of three forms each: the second one is cut short by what the first one used.
        let first = run_page(&mut pdf, &mut shared, &input);
        assert!(!first.truncated);
        assert_eq!(shared.forms_left, 1);
        let second = run_page(&mut pdf, &mut shared, &input);
        assert!(second.truncated);
        assert_eq!(shared.forms_left, 0);
    }

    #[test]
    fn operator_budget_stops_runaway_pages() {
        let big = "q Q ".repeat(50_000);
        let data = PdfBuilder::with_content(&big).build();
        let mut pdf = PdfFile::open(&data, &PreviewPolicy::default()).expect("open");
        let page = pdf.object(3);
        let dict = page.as_dict().cloned().expect("page");
        let contents = match pdf.get(&dict, "Contents") {
            Obj::Stream(s) => vec![s],
            _ => Vec::new(),
        };
        let mut shared = Shared::new(HashSet::new(), 1_000);
        let o = run_page(
            &mut pdf,
            &mut shared,
            &PageInput {
                resources: Rc::new(Dict::new()),
                contents,
                visible: [0.0, 0.0, 612.0, 792.0],
                record: false,
            },
        );
        assert!(o.truncated);
        assert_eq!(shared.ops_left, 0);
    }

    #[test]
    fn optional_content_layers_that_are_off_are_hidden() {
        let mut b = PdfBuilder::with_content(
            "/OC /Hid BDC BT /F1 12 Tf 72 700 Td (secret layer) Tj ET EMC BT /F1 12 Tf 72 600 Td (open layer) Tj ET",
        );
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /Properties << /Hid 20 0 R >> >> >>",
        );
        b.add_raw(20, "<< /Type /OCG /Name (Hidden) >>");
        let data = b.build();
        let mut pdf = PdfFile::open(&data, &PreviewPolicy::default()).expect("open");
        let page = pdf.object(3);
        let dict = page.as_dict().cloned().expect("page");
        let res = match pdf.get(&dict, "Resources") {
            Obj::Dict(d) => d,
            _ => Rc::new(Dict::new()),
        };
        let contents = match pdf.get(&dict, "Contents") {
            Obj::Stream(s) => vec![s],
            _ => Vec::new(),
        };
        let mut shared = Shared::new(HashSet::from([20u32]), 1_000_000);
        let o = run_page(
            &mut pdf,
            &mut shared,
            &PageInput {
                resources: res,
                contents,
                visible: [0.0, 0.0, 612.0, 792.0],
                record: false,
            },
        );
        let l = lines(&o);
        assert_eq!(
            l.iter().map(|(t, s)| (t.as_str(), s & style::HIDDEN != 0)).collect::<Vec<_>>(),
            [("secret layer", true), ("open layer", false)]
        );
    }

    #[test]
    fn hostile_streams_never_panic() {
        let seeds = [
            "BT /F1 12 Tf 72 700 Td (x) Tj ET",
            "q 1 0 0 1 5 5 cm /Fm1 Do Q BI /W 1 ID x EI",
            "[(a) 5 (b)] TJ ' \" T* Tm Td TD",
            "1e999 -1e999 re f 0 0 0 0 0 0 cm (z) Tj",
            "((((( 1 2 3 4 5 6 7 8 9 BT",
        ];
        for s in seeds {
            let base = s.as_bytes().to_vec();
            for i in 0..base.len() {
                for mask in [0x01u8, 0x20, 0xFF] {
                    let mut m = base.clone();
                    if let Some(b) = m.get_mut(i) {
                        *b ^= mask;
                    }
                    let text = String::from_utf8_lossy(&m).into_owned();
                    let _ = run(&text);
                }
            }
        }
    }
}
