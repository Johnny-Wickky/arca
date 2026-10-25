//! The chunks of a binary Visio drawing, read into the shape model.
//!
//! Layouts (offsets are within the chunk's data; a "number" is a tag byte followed by a
//! little-endian `f64`, so the `n`-th number of a record starts at `1 + 9 * n`):
//!
//! | chunk | type | content |
//! |---|---|---|
//! | Shape, group, guide, foreign | 0x48, 0x47, 0x4d, 0x4e | parent id at 10, master page at 18, master shape at 26, fill style at 34, line style at 42, text style at 50 (all `u32`, `0xFFFFFFFF` for none) |
//! | StyleSheet | 0x4a | line style at 34, fill style at 42, text style at 50 (the parent styles) |
//! | XForm | 0x9b | numbers PinX, PinY, Width, Height, LocPinX, LocPinY, Angle; FlipX, FlipY as bytes at 63, 64 |
//! | XForm1D | 0x9d | numbers BeginX, BeginY, EndX, EndY |
//! | TextXForm | 0x9c | numbers TxtPinX, TxtPinY, TxtWidth, TxtHeight, TxtLocPinX, TxtLocPinY, TxtAngle |
//! | Line | 0x85 | number weight; RGBA at 10; pattern at 14; number rounding at 15; begin and end arrow at 25, 26 |
//! | Fill and shadow | 0x86 | colour index at 0, RGBA at 1; background index at 5, RGBA at 6; pattern at 10; shadow pattern at 21 |
//! | Text block | 0x87 | numbers left, right, top, bottom margin; vertical alignment at 36 |
//! | Geometry list, geometry | 0x6c, 0x89 | a new path; flags at 0 (1 no fill, 2 no line, 4 no show) |
//! | MoveTo, LineTo | 0x8a, 0x8b | numbers X, Y |
//! | ArcTo | 0x8c | numbers X, Y, bow |
//! | InfiniteLine | 0x8d | numbers X, Y, A, B |
//! | Ellipse | 0x8f | numbers X, Y, A, B, C, D |
//! | EllipticalArcTo | 0x90 | numbers X, Y, A, B, angle, ratio |
//! | SplineStart | 0xa5 | numbers X, Y; then `f64` without a tag: second knot, first knot, last knot; degree byte at 42 |
//! | SplineKnot | 0xa6 | numbers X, Y; then an `f64` knot at 18 |
//! | PolylineTo, NURBSTo | 0xc1, 0xc3 | numbers X, Y (and for NURBS four `f64` A, B, C, D), then formula blocks or a reference to a shape-data chunk |
//! | Shape data | 0xd1 | kind byte at 0 (0x80 polyline, 0x82 NURBS), the points from 16 |
//! | Text | 0x0e | 8 bytes, then the text (UTF-16, or one byte per character in Visio 6) |
//! | Character, paragraph | 0x94, 0x95 | character count `u32` at 0 and the format (size as an `f64` at 18 in inches, style bits at 11, RGBA at 7; alignment byte at 58) |
//! | Colours | 0x16 | count byte at 2, RGBA entries from 4 |
//! | Page properties | 0x92 | numbers PageWidth, PageHeight, shadow x, y, PageScale; unit byte; DrawingScale at 46 |
//! | Layer, layer membership | 0xa8, 0xa7 | visible byte at 14 (after the colour); text of ids at 14 |
//! | Misc | 0xa4 | flags at 0 (0x20 hide text); connector ends in formula blocks from 45 |
//!
//! These are the layouts the open-source readers use. The chunks that carry shape data
//! (0xb6, 0xb4), hyperlinks (0xc4) and the connections (inside 0xa4) have no layout I could
//! confirm, so their text is recovered by scanning for strings and the page says so.

use std::collections::BTreeMap;

use arca_container::bytes::{u16_le, u32_le};

use super::{Chunk, Ev, MAX_NAME};
use crate::visio_draw::geom::MAX_ARGS;
use crate::visio_draw::model::{
    Cells, Connect, Fmt, Geom, Kind, Layer, Master, PageDef, Piece, Row, RowKind, Sheet, Val,
};

/// Most shapes read from one page or stencil page.
const MAX_SHAPES: usize = 100_000;
/// Deepest nesting of groups built.
const MAX_NEST: usize = 64;
/// Most geometry paths per shape.
const MAX_GEOMS: usize = 64;
/// Most rows per path.
const MAX_ROWS: usize = 100_000;
/// Most character or paragraph rows per shape.
const MAX_FMT: usize = 256;
/// Most bytes of text per shape.
const MAX_TEXT: usize = 1 << 20;
/// Most points in a shape-data block.
const MAX_POINTS: usize = MAX_ARGS / 4;

const T_SHAPE_GROUP: u32 = 0x47;
const T_SHAPE_SHAPE: u32 = 0x48;
const T_SHAPE_GUIDE: u32 = 0x4d;
const T_SHAPE_FOREIGN: u32 = 0x4e;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    None,
    Page,
    Stencil,
    Styles,
}

/// A block of shape data a NURBS or polyline row refers to.
#[derive(Debug, Clone)]
enum Block {
    Poly { x_type: f64, y_type: f64, points: Vec<(f64, f64)> },
    Nurbs { last_knot: f64, degree: f64, x_type: f64, y_type: f64, points: Vec<[f64; 4]> },
}

/// A shape while its chunks arrive.
#[derive(Default)]
struct ShapeB {
    sheet: Sheet,
    parent: u32,
    /// Ids a group lists as its children.
    listed: Vec<u32>,
    text: Option<String>,
    char_counts: Vec<u32>,
    para_counts: Vec<u32>,
    blocks: BTreeMap<u32, Block>,
    /// Rows that refer to a shape-data block: (path, row, block id, is NURBS, row cells).
    fixups: Vec<(usize, usize, u32)>,
    begin_id: Option<u32>,
    end_id: Option<u32>,
}

/// The reader.
pub(super) struct Reader {
    version: u16,
    colors: Vec<u32>,
    styles: BTreeMap<u32, Sheet>,
    masters: BTreeMap<u32, Master>,
    unverified: Vec<&'static str>,
    scope: Scope,
    shapes: Vec<ShapeB>,
    cur: Option<usize>,
    /// Shape-list owner while ids of a list arrive.
    listing: Option<usize>,
    page: PageDef,
    page_done: Option<PageDef>,
    stencil: (u32, String),
    style: Option<Sheet>,
    cut: bool,
}

fn put(cells: &mut Cells, name: &'static str, v: Val) {
    if let Some(slot) = cells.iter_mut().find(|(n, _)| *n == name) {
        slot.1 = v;
    } else {
        cells.push((name, v));
    }
}

/// The number whose tag byte is at `at`: the `f64` that follows.
fn d(b: &[u8], at: usize) -> Option<f64> {
    let raw: [u8; 8] = b.get(at..at.checked_add(8)?)?.try_into().ok()?;
    let v = f64::from_le_bytes(raw);
    v.is_finite().then_some(v)
}

/// The number at the `n`-th position of a record of tagged numbers.
fn tagged(b: &[u8], n: usize) -> Option<f64> {
    d(b, 1 + 9 * n)
}

fn num(v: f64) -> Val {
    Val::Num(v)
}

fn id_opt(v: Option<u32>) -> Option<u32> {
    v.filter(|v| *v != u32::MAX)
}

/// RGBA at `at`: a colour literal, or `None` if all four bytes are zero.
fn rgba(b: &[u8], at: usize) -> Option<(u32, f64)> {
    let r = *b.get(at)?;
    let g = *b.get(at + 1)?;
    let bl = *b.get(at + 2)?;
    let a = *b.get(at + 3)?;
    let rgb = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(bl);
    Some((rgb, f64::from(a) / 255.0))
}

/// Strings of a record that has no layout I could confirm: runs of printable characters.
fn strings_of(body: &[u8], version: u16) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let ok = |c: char| {
        let u = c as u32;
        (0x20..0x7f).contains(&u)
            || (0xa0..0x250).contains(&u)
            || (0x370..0x530).contains(&u)
            || (0x590..0x700).contains(&u)
            || (0x3040..0xa000).contains(&u)
    };
    let mut cur = String::new();
    let mut flush = |cur: &mut String, out: &mut Vec<String>| {
        let ascii = cur.chars().all(|c| (c as u32) < 0x250);
        let min = if ascii { 3 } else { 2 };
        if cur.chars().count() >= min && out.len() < 16 {
            out.push(cur.chars().take(MAX_NAME).collect());
        }
        cur.clear();
    };
    if version > 6 {
        for u in body.chunks_exact(2) {
            let v = u16::from_le_bytes([u.first().copied().unwrap_or(0), u.get(1).copied().unwrap_or(0)]);
            match char::from_u32(u32::from(v)).filter(|c| ok(*c)) {
                Some(c) => cur.push(c),
                None => flush(&mut cur, &mut out),
            }
        }
    } else {
        for &b in body {
            if (0x20..0x7f).contains(&b) {
                cur.push(char::from(b));
            } else {
                flush(&mut cur, &mut out);
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Splits text by the character counts of the character and paragraph rows.
fn pieces(text: &str, chars: &[u32], paras: &[u32]) -> Vec<Piece> {
    fn advance(counts: &[u32], ix: &mut usize, left: &mut usize) {
        // Moves to the next row when the current one is used up (the last row goes on).
        while *left == 0 && ix.saturating_add(1) < counts.len() {
            *ix += 1;
            *left = counts.get(*ix).map_or(0, |c| usize::try_from(*c).unwrap_or(usize::MAX));
        }
    }
    let mut out: Vec<Piece> = Vec::new();
    let (mut ci, mut pi) = (0usize, 0usize);
    let mut cl = chars.first().map_or(usize::MAX, |c| usize::try_from(*c).unwrap_or(usize::MAX));
    let mut pl = paras.first().map_or(usize::MAX, |c| usize::try_from(*c).unwrap_or(usize::MAX));
    for ch in text.chars() {
        advance(chars, &mut ci, &mut cl);
        advance(paras, &mut pi, &mut pl);
        let (c, p) = (u32::try_from(ci).unwrap_or(0), u32::try_from(pi).unwrap_or(0));
        match out.last_mut() {
            Some(last) if last.char_ix == c && last.para_ix == p => last.text.push(ch),
            _ => out.push(Piece { text: ch.to_string(), char_ix: c, para_ix: p }),
        }
        cl = cl.saturating_sub(1);
        pl = pl.saturating_sub(1);
    }
    out
}

impl Reader {
    pub(super) fn new(version: u16) -> Self {
        Self {
            version,
            colors: Vec::new(),
            styles: BTreeMap::new(),
            masters: BTreeMap::new(),
            unverified: Vec::new(),
            scope: Scope::None,
            shapes: Vec::new(),
            cur: None,
            listing: None,
            page: PageDef::default(),
            page_done: None,
            stencil: (0, String::new()),
            style: None,
            cut: false,
        }
    }

    pub(super) fn take_page(&mut self) -> Option<PageDef> {
        self.page_done.take()
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn take_shared(
        &mut self,
    ) -> (BTreeMap<u32, Master>, BTreeMap<u32, Sheet>, Vec<u32>, Vec<&'static str>) {
        (
            std::mem::take(&mut self.masters),
            std::mem::take(&mut self.styles),
            std::mem::take(&mut self.colors),
            std::mem::take(&mut self.unverified),
        )
    }

    fn unverified(&mut self, what: &'static str) {
        if !self.unverified.contains(&what) {
            self.unverified.push(what);
        }
    }

    pub(super) fn event(&mut self, ev: &Ev<'_>) {
        match ev {
            Ev::Begin { ty, idx, name, foreground } => self.begin(*ty, *idx, *name, *foreground),
            Ev::End { ty } => self.end(*ty),
            Ev::Chunk(c) => self.chunk(c),
        }
    }

    fn begin(&mut self, ty: i32, idx: u32, name: Option<&str>, foreground: bool) {
        match ty {
            0x15 => {
                self.scope = Scope::Page;
                self.shapes.clear();
                self.cur = None;
                self.listing = None;
                self.cut = false;
                self.page = PageDef {
                    id: idx,
                    name: name.unwrap_or("").to_owned(),
                    background: !foreground,
                    ..PageDef::default()
                };
            }
            0x1a => {
                self.scope = Scope::Styles;
                self.style = None;
            }
            0x1e => {
                self.scope = Scope::Stencil;
                self.shapes.clear();
                self.cur = None;
                self.listing = None;
                self.cut = false;
                self.stencil = (idx, name.unwrap_or("").to_owned());
            }
            _ => {}
        }
    }

    fn end(&mut self, ty: i32) {
        match ty {
            0x15 if self.scope == Scope::Page => {
                let (roots, connects) = self.build();
                let mut page = std::mem::take(&mut self.page);
                page.shapes = roots;
                page.connects = connects;
                page.truncated |= self.cut;
                self.page_done = Some(page);
                self.scope = Scope::None;
            }
            0x1a if self.scope == Scope::Styles => {
                self.flush_style();
                self.scope = Scope::None;
            }
            0x1e if self.scope == Scope::Stencil => {
                let (roots, _) = self.build();
                let (id, name) = std::mem::take(&mut self.stencil);
                self.masters.entry(id).or_insert_with(|| Master::new(id, name, roots));
                self.scope = Scope::None;
            }
            _ => {}
        }
    }

    fn flush_style(&mut self) {
        if let Some(s) = self.style.take() {
            self.styles.entry(s.id).or_insert(s);
        }
    }

    fn chunk(&mut self, c: &Chunk<'_>) {
        if c.ty != 0x83 && c.ty != 0x65 {
            self.listing = None;
        }
        if c.ty == 0x16 {
            self.colours(c.body);
            return;
        }
        match self.scope {
            Scope::Styles => self.style_chunk(c),
            Scope::Page | Scope::Stencil => self.shape_chunk(c),
            Scope::None => {}
        }
    }

    fn colours(&mut self, b: &[u8]) {
        let n = usize::from(b.get(2).copied().unwrap_or(0));
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            match rgba(b, 4 + 4 * i) {
                Some((rgb, _)) => out.push(rgb),
                None => break,
            }
        }
        if !out.is_empty() {
            self.colors = out;
        }
    }

    fn style_chunk(&mut self, c: &Chunk<'_>) {
        if c.ty == 0x4a {
            self.flush_style();
            let s = Sheet {
                id: c.id,
                line_style: id_opt(u32_le(c.body, 34)),
                fill_style: id_opt(u32_le(c.body, 42)),
                text_style: id_opt(u32_le(c.body, 50)),
                ..Sheet::default()
            };
            self.style = Some(s);
            return;
        }
        let version = self.version;
        let Some(style) = self.style.as_mut() else { return };
        let mut counts = (Vec::new(), Vec::new());
        apply_format(style, c, version, &mut counts);
    }

    fn shape_chunk(&mut self, c: &Chunk<'_>) {
        match c.ty {
            T_SHAPE_GROUP | T_SHAPE_SHAPE | T_SHAPE_GUIDE | T_SHAPE_FOREIGN => {
                if self.shapes.len() >= MAX_SHAPES {
                    self.cut = true;
                    self.cur = None;
                    return;
                }
                let kind = match c.ty {
                    T_SHAPE_GROUP => Kind::Group,
                    T_SHAPE_GUIDE => Kind::Guide,
                    T_SHAPE_FOREIGN => Kind::Foreign,
                    _ => Kind::Shape,
                };
                let b = c.body;
                let sheet = Sheet {
                    id: c.id,
                    kind,
                    master: id_opt(u32_le(b, 18)),
                    master_shape: id_opt(u32_le(b, 26)),
                    fill_style: id_opt(u32_le(b, 34)),
                    line_style: id_opt(u32_le(b, 42)),
                    text_style: id_opt(u32_le(b, 50)),
                    foreign: (kind == Kind::Foreign).then(String::new),
                    ..Sheet::default()
                };
                self.shapes.push(ShapeB {
                    sheet,
                    parent: u32_le(b, 10).unwrap_or(0),
                    ..ShapeB::default()
                });
                self.cur = Some(self.shapes.len() - 1);
                return;
            }
            // The page sheet has chunks of its own; they are not a shape's.
            0x46 => {
                self.cur = None;
                return;
            }
            0x15 => {
                if self.scope == Scope::Page {
                    let id = u32_le(c.body, 8).filter(|v| *v != 0 && *v != u32::MAX);
                    self.page.back_page = id.filter(|v| *v != self.page.id);
                }
                return;
            }
            0x92 => {
                if self.scope == Scope::Page {
                    let cells = &mut self.page.sheet;
                    if let Some(w) = tagged(c.body, 0) {
                        put(cells, "PageWidth", num(w));
                    }
                    if let Some(h) = tagged(c.body, 1) {
                        put(cells, "PageHeight", num(h));
                    }
                    if let Some(s) = tagged(c.body, 4) {
                        put(cells, "PageScale", num(s));
                    }
                    if let Some(s) = d(c.body, 46) {
                        put(cells, "DrawingScale", num(s));
                    }
                }
                return;
            }
            0xa8 => {
                if self.scope == Scope::Page && self.page.layers.len() < 1_024 {
                    let b = c.body;
                    let colour = b.get(8).copied().unwrap_or(0);
                    let at = 13;
                    let visible = b.get(at + 1).copied().unwrap_or(1) != 0;
                    self.page.layers.push(Layer { ix: c.id, visible });
                }
                return;
            }
            0x65 => {
                self.listing = self.cur;
                return;
            }
            0x83 => {
                if let (Some(i), Some(id)) = (self.listing, u32_le(c.body, 0))
                    && let Some(s) = self.shapes.get_mut(i)
                    && s.listed.len() < MAX_SHAPES
                {
                    s.listed.push(id);
                }
                return;
            }
            _ => {}
        }
        let version = self.version;
        let colors_known = !self.colors.is_empty();
        let _ = colors_known;
        let Some(b) = self.cur.and_then(|i| self.shapes.get_mut(i)) else { return };
        match c.ty {
            0x9b => {
                const NAMES: [&str; 7] =
                    ["PinX", "PinY", "Width", "Height", "LocPinX", "LocPinY", "Angle"];
                for (n, name) in NAMES.iter().enumerate() {
                    if let Some(v) = tagged(c.body, n) {
                        put(&mut b.sheet.cells, name, num(v));
                    }
                }
                for (at, name) in [(63usize, "FlipX"), (64, "FlipY")] {
                    if let Some(f) = c.body.get(at) {
                        put(&mut b.sheet.cells, name, num(f64::from(u8::from(*f != 0))));
                    }
                }
            }
            0x9d => {
                for (n, name) in ["BeginX", "BeginY", "EndX", "EndY"].iter().enumerate() {
                    if let Some(v) = tagged(c.body, n) {
                        put(&mut b.sheet.cells, name, num(v));
                    }
                }
            }
            0x9c => {
                const NAMES: [&str; 7] = [
                    "TxtPinX", "TxtPinY", "TxtWidth", "TxtHeight", "TxtLocPinX", "TxtLocPinY",
                    "TxtAngle",
                ];
                for (n, name) in NAMES.iter().enumerate() {
                    if let Some(v) = tagged(c.body, n) {
                        put(&mut b.sheet.cells, name, num(v));
                    }
                }
            }
            0x0e => {
                let t = super::text(c.body, version);
                b.text = Some(t.chars().take(MAX_TEXT).collect());
            }
            0x85 | 0x86 | 0x87 | 0x94 | 0x95 => {
                let mut counts = (std::mem::take(&mut b.char_counts), std::mem::take(&mut b.para_counts));
                apply_format(&mut b.sheet, c, version, &mut counts);
                b.char_counts = counts.0;
                b.para_counts = counts.1;
            }
            0x6c => {
                if b.sheet.geoms.len() < MAX_GEOMS {
                    let ix = u32::try_from(b.sheet.geoms.len()).unwrap_or(0);
                    b.sheet.geoms.push(Geom { ix, ..Geom::default() });
                } else {
                    self.cut = true;
                }
            }
            0x89 => {
                if b.sheet.geoms.is_empty() {
                    b.sheet.geoms.push(Geom::default());
                }
                let flags = c.body.first().copied().unwrap_or(0);
                if let Some(g) = b.sheet.geoms.last_mut() {
                    g.no_fill = Some(flags & 1 != 0);
                    g.no_line = Some(flags & 2 != 0);
                    g.no_show = Some(flags & 4 != 0);
                }
            }
            0x8a | 0x8b | 0x8c | 0x8d | 0x8f | 0x90 | 0xa5 | 0xa6 | 0xc1 | 0xc3 => {
                let mut unverified = None;
                row(b, c, &mut self.cut, &mut unverified);
                if let Some(u) = unverified {
                    self.unverified(u);
                }
            }
            0xd1 => {
                if let Some(block) = shape_data(c.body) {
                    b.blocks.insert(c.id, block);
                }
            }
            0x98 => {
                b.sheet.foreign.get_or_insert_with(String::new);
            }
            0xa4 => {
                let flags = c.body.first().copied().unwrap_or(0);
                if flags & 0x20 != 0 {
                    put(&mut b.sheet.cells, "HideText", num(1.0));
                }
                let (begin, end) = glue(c.body);
                b.begin_id = b.begin_id.or(begin);
                b.end_id = b.end_id.or(end);
                if begin.is_some() || end.is_some() {
                    self.unverified("binary Visio connections are read from formula tokens");
                }
            }
            0xa7 => {
                let n = usize::from(c.body.get(13).copied().unwrap_or(0));
                let ids: Vec<u32> = if version > 6 {
                    let raw = c.body.get(14..14 + n * 2).unwrap_or(&[]);
                    let s = arca_container::text::utf16le_lossy(raw);
                    s.split(';').filter_map(|p| p.trim().parse().ok()).take(64).collect()
                } else {
                    let raw = c.body.get(14..14 + n).unwrap_or(&[]);
                    let s: String = raw.iter().map(|b| char::from(*b)).collect();
                    s.split(';').filter_map(|p| p.trim().parse().ok()).take(64).collect()
                };
                b.sheet.layers = ids;
            }
            0xb6 | 0xb4 => {
                let strs = strings_of(c.body, version);
                if let Some((label, rest)) = strs.split_first()
                    && b.sheet.data.len() < 64
                {
                    b.sheet.data.push((label.clone(), rest.join(" | ")));
                    self.unverified("binary Visio shape data are recovered by scanning for text");
                }
            }
            0xc4 => {
                let strs = strings_of(c.body, version);
                if !strs.is_empty() && b.sheet.data.len() < 64 {
                    b.sheet.data.push(("Hyperlink".to_owned(), strs.join(" | ")));
                    self.unverified("binary Visio hyperlinks are recovered by scanning for text");
                }
            }
            _ => {}
        }
    }

    /// Builds the tree of the shapes read in a page or stencil page, and the connections.
    fn build(&mut self) -> (Vec<Sheet>, Vec<Connect>) {
        let shapes = std::mem::take(&mut self.shapes);
        self.cur = None;
        let n = shapes.len();
        let mut index: BTreeMap<u32, usize> = BTreeMap::new();
        for (i, s) in shapes.iter().enumerate() {
            index.entry(s.sheet.id).or_insert(i);
        }
        let mut listed_parent: BTreeMap<u32, usize> = BTreeMap::new();
        for (i, s) in shapes.iter().enumerate() {
            for k in &s.listed {
                listed_parent.entry(*k).or_insert(i);
            }
        }
        let mut connects = Vec::new();
        let mut parent: Vec<Option<usize>> = Vec::with_capacity(n);
        for (i, s) in shapes.iter().enumerate() {
            let by_field = (s.parent != 0 && s.parent != s.sheet.id)
                .then(|| index.get(&s.parent).copied())
                .flatten()
                .filter(|p| *p != i);
            let p = by_field
                .or_else(|| listed_parent.get(&s.sheet.id).copied().filter(|p| *p != i));
            parent.push(p);
            for (end, begin) in [(s.begin_id, true), (s.end_id, false)] {
                if let Some(to) = end
                    && connects.len() < 200_000
                {
                    connects.push(Connect { from: s.sheet.id, begin, to });
                }
            }
        }
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut roots: Vec<usize> = Vec::new();
        for (i, p) in parent.iter().enumerate() {
            match p.and_then(|p| children.get_mut(p)) {
                Some(list) => list.push(i),
                None => roots.push(i),
            }
        }
        let mut slots: Vec<Option<ShapeB>> = shapes.into_iter().map(Some).collect();
        let version = self.version;
        let mut out = Vec::new();
        let mut cut = false;
        for r in roots {
            if let Some(s) = attach(&mut slots, &children, r, 0, version, &mut cut) {
                out.push(s);
            }
        }
        // Members of a cycle of parents are reachable from no root.
        for r in 0..n {
            if slots.get(r).is_some_and(Option::is_some)
                && let Some(s) = attach(&mut slots, &children, r, 0, version, &mut cut)
            {
                out.push(s);
            }
        }
        self.cut |= cut;
        (out, connects)
    }
}

/// Finishes a shape: its text split by the formats, the shape-data references resolved.
fn attach(
    slots: &mut [Option<ShapeB>],
    children: &[Vec<usize>],
    i: usize,
    depth: usize,
    version: u16,
    cut: &mut bool,
) -> Option<Sheet> {
    let mut b = slots.get_mut(i)?.take()?;
    let _ = version;
    if let Some(t) = b.text.take() {
        b.sheet.text = pieces(&t, &b.char_counts, &b.para_counts);
    }
    for (g, r, id) in std::mem::take(&mut b.fixups) {
        let Some(block) = b.blocks.get(&id) else { continue };
        let Some(row) = b.sheet.geoms.get_mut(g).and_then(|g| g.rows.get_mut(r)) else { continue };
        row.args = Some(match block {
            Block::Poly { x_type, y_type, points } => {
                let mut a = vec![*x_type, *y_type];
                for (x, y) in points {
                    a.extend([*x, *y]);
                }
                a
            }
            Block::Nurbs { last_knot, degree, x_type, y_type, points } => {
                let mut a = vec![*last_knot, *degree, *x_type, *y_type];
                for p in points {
                    a.extend(p);
                }
                a
            }
        });
    }
    let kids = children.get(i).cloned().unwrap_or_default();
    if depth + 1 >= MAX_NEST {
        if !kids.is_empty() {
            *cut = true;
        }
    } else {
        for k in kids {
            if let Some(s) = attach(slots, children, k, depth + 1, version, cut) {
                b.sheet.kids.push(s);
            }
        }
    }
    Some(b.sheet)
}

/// Line, fill, text block, character and paragraph chunks (of a shape or a style sheet).
fn apply_format(
    s: &mut Sheet,
    c: &Chunk<'_>,
    version: u16,
    counts: &mut (Vec<u32>, Vec<u32>),
) {
    let b = c.body;
    let _ = version;
    match c.ty {
        0x85 => {
            if let Some(w) = tagged(b, 0) {
                put(&mut s.cells, "LineWeight", num(w));
            }
            if let Some((rgb, a)) = rgba(b, 10) {
                put(&mut s.cells, "LineColor", Val::Rgb(rgb));
                put(&mut s.cells, "LineColorTrans", num(a));
            }
            if let Some(p) = b.get(14) {
                put(&mut s.cells, "LinePattern", num(f64::from(*p)));
            }
            if let Some(r) = d(b, 16) {
                put(&mut s.cells, "Rounding", num(r));
            }
            if let Some(m) = b.get(25) {
                put(&mut s.cells, "BeginArrow", num(f64::from(*m)));
            }
            if let Some(m) = b.get(26) {
                put(&mut s.cells, "EndArrow", num(f64::from(*m)));
            }
        }
        0x86 => {
            let fg = rgba(b, 1);
            let bg = rgba(b, 6);
            let unset = |x: Option<(u32, f64)>| x.is_none_or(|(rgb, a)| rgb == 0 && a == 0.0);
            let both_unset = unset(fg) && unset(bg);
            let colour = |ix_at: usize, x: Option<(u32, f64)>| {
                if both_unset {
                    b.get(ix_at).map(|i| Val::Num(f64::from(*i)))
                } else {
                    x.map(|(rgb, _)| Val::Rgb(rgb))
                }
            };
            if let Some(v) = colour(0, fg) {
                put(&mut s.cells, "FillForegnd", v);
            }
            if let Some(v) = colour(5, bg) {
                put(&mut s.cells, "FillBkgnd", v);
            }
            if let Some((_, a)) = fg {
                put(&mut s.cells, "FillForegndTrans", num(a));
            }
            if let Some((_, a)) = bg {
                put(&mut s.cells, "FillBkgndTrans", num(a));
            }
            if let Some(p) = b.get(10) {
                put(&mut s.cells, "FillPattern", num(f64::from(*p)));
            }
            if let Some(p) = b.get(21) {
                put(&mut s.cells, "ShdwPattern", num(f64::from(*p)));
            }
        }
        0x87 => {
            for (n, name) in ["LeftMargin", "RightMargin", "TopMargin", "BottomMargin"]
                .iter()
                .enumerate()
            {
                if let Some(v) = tagged(b, n) {
                    put(&mut s.cells, name, num(v));
                }
            }
            if let Some(v) = b.get(36) {
                put(&mut s.cells, "VerticalAlign", num(f64::from(*v)));
            }
        }
        0x94 => {
            let count = u32_le(b, 0).unwrap_or(0);
            let mut cells: Cells = Vec::new();
            if let Some(sz) = d(b, 18) {
                put(&mut cells, "Size", num(sz));
            }
            let mut bits = 0.0;
            let m = b.get(11).copied().unwrap_or(0);
            if m & 1 != 0 {
                bits += 1.0;
            }
            if m & 2 != 0 {
                bits += 2.0;
            }
            if m & 4 != 0 {
                bits += 4.0;
            }
            if m & 8 != 0 {
                bits += 8.0;
            }
            put(&mut cells, "Style", num(bits));
            match rgba(b, 7) {
                Some((rgb, a)) if rgb != 0 || a != 0.0 => put(&mut cells, "Color", Val::Rgb(rgb)),
                _ => {
                    if let Some(i) = b.get(6) {
                        put(&mut cells, "Color", num(f64::from(*i)));
                    }
                }
            }
            if counts.0.len() < MAX_FMT {
                let ix = u32::try_from(s.chars.len()).unwrap_or(0);
                s.chars.push(Fmt { ix, cells });
                counts.0.push(count);
            }
        }
        0x95 => {
            let count = u32_le(b, 0).unwrap_or(0);
            let mut cells: Cells = Vec::new();
            if let Some(a) = b.get(58) {
                put(&mut cells, "HorzAlign", num(f64::from(*a)));
            }
            if counts.1.len() < MAX_FMT {
                let ix = u32::try_from(s.paras.len()).unwrap_or(0);
                s.paras.push(Fmt { ix, cells });
                counts.1.push(count);
            }
        }
        _ => {}
    }
    let _ = u16_le;
}

/// One geometry row.
fn row(b: &mut ShapeB, c: &Chunk<'_>, cut: &mut bool, unverified: &mut Option<&'static str>) {
    if b.sheet.geoms.is_empty() {
        b.sheet.geoms.push(Geom::default());
    }
    let (kind, names): (RowKind, &[&'static str]) = match c.ty {
        0x8a => (RowKind::MoveTo, &["X", "Y"]),
        0x8b => (RowKind::LineTo, &["X", "Y"]),
        0x8c => (RowKind::ArcTo, &["X", "Y", "A"]),
        0x8d => (RowKind::InfiniteLine, &["X", "Y", "A", "B"]),
        0x8f => (RowKind::Ellipse, &["X", "Y", "A", "B", "C", "D"]),
        0x90 => (RowKind::EllipticalArcTo, &["X", "Y", "A", "B", "C", "D"]),
        0xa5 => (RowKind::SplineStart, &["X", "Y"]),
        0xa6 => (RowKind::SplineKnot, &["X", "Y"]),
        0xc1 => (RowKind::PolylineTo, &["X", "Y"]),
        _ => (RowKind::NurbsTo, &["X", "Y"]),
    };
    let mut cells: Cells = Vec::new();
    for (n, name) in names.iter().enumerate() {
        if let Some(v) = tagged(c.body, n) {
            put(&mut cells, name, num(v));
        }
    }
    let mut args = None;
    let mut fixup = None;
    match c.ty {
        0xa5 => {
            for (at, name) in [(18usize, "A"), (26, "B"), (34, "C")] {
                if let Some(v) = d(c.body, at) {
                    put(&mut cells, name, num(v));
                }
            }
            if let Some(deg) = c.body.get(42) {
                put(&mut cells, "D", num(f64::from(*deg)));
            }
        }
        0xa6 => {
            if let Some(v) = d(c.body, 18) {
                put(&mut cells, "A", num(v));
            }
        }
        0xc1 => {
            let (a, f, u) = polyline_formula(c.body);
            args = a;
            fixup = f;
            *unverified = u;
        }
        0xc3 => {
            for (at, name) in [(18usize, "A"), (26, "B"), (34, "C"), (42, "D")] {
                if let Some(v) = d(c.body, at) {
                    put(&mut cells, name, num(v));
                }
            }
            let (a, f, u) = nurbs_formula(c.body);
            args = a;
            fixup = f;
            *unverified = u;
        }
        _ => {}
    }
    let Some(g) = b.sheet.geoms.last_mut() else { return };
    if g.rows.len() >= MAX_ROWS {
        *cut = true;
        return;
    }
    let ix = u32::try_from(g.rows.len()).unwrap_or(0);
    let gi = b.sheet.geoms.len() - 1;
    let ri = b.sheet.geoms.last().map_or(0, |g| g.rows.len());
    if let Some(g) = b.sheet.geoms.last_mut() {
        g.rows.push(Row { kind, ix, del: false, cells, args });
    }
    if let Some(id) = fixup {
        b.fixups.push((gi, ri, id));
    }
}

type Formula = (Option<Vec<f64>>, Option<u32>, Option<&'static str>);

const FORMULA_NOTE: &str = "binary Visio NURBS and polyline formulas are read from their blocks";

/// The blocks after the fixed part of a polyline row: `[length u32][1 byte][cell index u8]`
/// then, for cell 2 (`A`), the formula `POLYLINE(xType, yType, x1, y1, ...)` with each number a
/// tag byte and an `f64` (tag 0x20) or a `u16` (other tags), ended by a tag of 0x81. A row may
/// also refer to a shape-data chunk (tag 0x8b at 19, id at 23).
fn polyline_formula(b: &[u8]) -> Formula {
    if b.get(19) == Some(&0x8b) {
        return (None, u32_le(b, 23), Some(FORMULA_NOTE));
    }
    let mut pos = 29usize;
    loop {
        let Some(len) = u32_le(b, pos).and_then(|l| usize::try_from(l).ok()) else { return (None, None, None) };
        if len < 6 {
            return (None, None, None);
        }
        let Some(cell) = b.get(pos + 5) else { return (None, None, None) };
        if *cell >= 2 {
            break;
        }
        pos = pos.saturating_add(len);
    }
    let mut at = pos + 6;
    let end = pos.saturating_add(u32_le(b, pos).map_or(0, |l| usize::try_from(l).unwrap_or(0)));
    // xType and yType: a tag byte and a u16 each.
    let (Some(x), Some(y)) = (u16_le(b, at + 1), u16_le(b, at + 4)) else {
        return (None, None, None);
    };
    at += 6;
    let mut args = vec![f64::from(x), f64::from(y)];
    while at < end && args.len() < MAX_ARGS {
        let tag = b.get(at).copied().unwrap_or(0x81);
        if tag == 0x81 {
            break;
        }
        let mut vals = [0.0f64; 2];
        for v in &mut vals {
            let t = b.get(at).copied().unwrap_or(0);
            if t == 0x20 {
                *v = d(b, at + 1).unwrap_or(0.0);
                at += 9;
            } else {
                *v = f64::from(u16_le(b, at + 1).unwrap_or(0));
                at += 3;
            }
        }
        args.extend(vals);
    }
    (Some(args), None, Some(FORMULA_NOTE))
}

/// The formula of a NURBS row (cell 6, `E`): either a fixed layout (tag 0x8a: last knot `f64`,
/// degree `u16`, x and y type bytes, a repetition count, then four `f64` per point) or typed
/// numbers. A row may also refer to a shape-data chunk (tag 0x8a at 51, id at 55).
fn nurbs_formula(b: &[u8]) -> Formula {
    if b.get(51) == Some(&0x8a) {
        return (None, u32_le(b, 55), Some(FORMULA_NOTE));
    }
    let mut pos = 61usize;
    loop {
        let Some(len) = u32_le(b, pos).and_then(|l| usize::try_from(l).ok()) else { return (None, None, None) };
        if len < 6 {
            return (None, None, None);
        }
        let Some(cell) = b.get(pos + 5) else { return (None, None, None) };
        if *cell == 6 {
            break;
        }
        if *cell > 6 {
            return (None, None, None);
        }
        pos = pos.saturating_add(len);
    }
    let at = pos + 6;
    let end = pos.saturating_add(u32_le(b, pos).map_or(0, |l| usize::try_from(l).unwrap_or(0)));
    if b.get(at) != Some(&0x8a) {
        return (typed_nurbs(b, at, end), None, Some(FORMULA_NOTE));
    }
    let (Some(last), Some(degree)) = (d(b, at + 1), u16_le(b, at + 9)) else {
        return (None, None, None);
    };
    let (x_type, y_type) = (b.get(at + 11).copied().unwrap_or(1), b.get(at + 12).copied().unwrap_or(1));
    let reps = u32_le(b, at + 13).and_then(|r| usize::try_from(r).ok()).unwrap_or(0);
    let mut args = vec![last, f64::from(degree), f64::from(x_type), f64::from(y_type)];
    let mut p = at + 17;
    for _ in 0..reps.min(MAX_POINTS) {
        let (Some(x), Some(y), Some(k), Some(w)) = (d(b, p), d(b, p + 8), d(b, p + 16), d(b, p + 24))
        else {
            break;
        };
        args.extend([x, y, k, w]);
        p += 32;
    }
    (Some(args), None, Some(FORMULA_NOTE))
}

/// A number of a typed formula at `*at`: the tag byte 0x20 says an `f64` follows, any other tag a
/// `u16`. Moves `*at` past it.
fn typed(b: &[u8], at: &mut usize) -> Option<f64> {
    let tag = *b.get(*at)?;
    if tag == 0x20 {
        let v = d(b, *at + 1)?;
        *at += 9;
        Some(v)
    } else {
        let v = f64::from(u16_le(b, *at + 1)?);
        *at += 3;
        Some(v)
    }
}

/// The typed form of a NURBS formula: last knot, degree, x type and y type (each a tag byte
/// and a number), then points of x, y, knot and weight until a tag of 0x81. The knot and the
/// weight are present only with the tags 0x20 or 0x62.
fn typed_nurbs(b: &[u8], start: usize, end: usize) -> Option<Vec<f64>> {
    let mut at = start;
    let last = typed(b, &mut at)?;
    let degree = typed(b, &mut at)?;
    let x_type = typed(b, &mut at)?;
    let y_type = typed(b, &mut at)?;
    let mut args = vec![last, degree, x_type, y_type];
    while at < end && args.len() < MAX_ARGS {
        if b.get(at).copied()? == 0x81 {
            break;
        }
        let x = typed(b, &mut at)?;
        let y = typed(b, &mut at)?;
        let mut optional = |at: &mut usize| match b.get(*at) {
            Some(0x20 | 0x62) => typed(b, at),
            _ => Some(0.0),
        };
        let k = optional(&mut at)?;
        let w = optional(&mut at)?;
        args.extend([x, y, k, w]);
    }
    Some(args)
}

fn shape_data(b: &[u8]) -> Option<Block> {
    match b.first()? {
        0x80 => {
            let (x_type, y_type) = (f64::from(*b.get(16)?), f64::from(*b.get(17)?));
            let count = usize::try_from(u32_le(b, 18)?).ok()?.min(b.len().saturating_sub(22) / 16);
            let mut points = Vec::new();
            for i in 0..count.min(MAX_POINTS) {
                points.push((d(b, 22 + i * 16)?, d(b, 30 + i * 16)?));
            }
            Some(Block::Poly { x_type, y_type, points })
        }
        0x82 => {
            let last_knot = d(b, 16)?;
            let degree = f64::from(u16_le(b, 24)?);
            let (x_type, y_type) = (f64::from(*b.get(26)?), f64::from(*b.get(27)?));
            let count = usize::try_from(u32_le(b, 28)?).ok()?.min(b.len().saturating_sub(32) / 32);
            let mut points = Vec::new();
            for i in 0..count.min(MAX_POINTS) {
                let at = 32 + i * 32;
                points.push([d(b, at)?, d(b, at + 8)?, d(b, at + 16)?, d(b, at + 24)?]);
            }
            Some(Block::Nurbs { last_knot, degree, x_type, y_type, points })
        }
        _ => None,
    }
}

/// The shapes a connector is glued to, from the formula tokens of the Misc chunk: blocks from
/// offset 45, each `[length u32][type u8][1 byte]` and, for type 2, the token 0x74 followed by
/// `0x6000004e`, the shape id, 0x7a and `0x40000073`. The first such block is the begin, the
/// second the end.
fn glue(b: &[u8]) -> (Option<u32>, Option<u32>) {
    let (mut begin, mut end) = (None, None);
    let mut pos = 45usize;
    let mut guard = 0;
    while let Some(len) = u32_le(b, pos).and_then(|l| usize::try_from(l).ok()) {
        guard += 1;
        if len == 0 || guard > 4_096 {
            break;
        }
        if b.get(pos + 4) == Some(&2)
            && b.get(pos + 6) == Some(&0x74)
            && u32_le(b, pos + 7) == Some(0x6000_004e)
            && b.get(pos + 15) == Some(&0x7a)
            && u32_le(b, pos + 16) == Some(0x4000_0073)
            && let Some(id) = u32_le(b, pos + 11)
        {
            if begin.is_none() {
                begin = Some(id);
            } else if end.is_none() {
                end = Some(id);
            }
        }
        pos = pos.saturating_add(len);
    }
    (begin, end)
}
