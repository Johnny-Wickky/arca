//! `.vsdx` / `.vsdm` / `.vstx` (Visio Open Packaging) into the shape model.
//!
//! The package holds `visio/document.xml` (the colour table and the style sheets), `masters/`
//! (the stencil shapes) and `pages/` (one part per page, listed in `pages.xml` together with the
//! page sheet). Elements: `Shape` (attributes `ID`, `Type`, `Master`, `MasterShape`, `LineStyle`,
//! `FillStyle`, `TextStyle`), `Cell` (`N` name, `V` cached value, `F` formula), `Section` (`N`,
//! `IX`) with `Row` (`T` type, `IX`, `N`), `Text` with `cp`/`pp`/`tp`/`fld` markers, `Connect`
//! (`FromSheet`, `FromCell`, `ToSheet`, `ToCell`) and `ForeignData`.
//!
//! Nothing is evaluated and nothing is loaded: formulas are looked at only to read the numbers of
//! `NURBS(...)` and `POLYLINE(...)`; the values of cells are their cached results.

use std::collections::BTreeSet;

use super::geom::MAX_ARGS;
use super::model::{
    Cells, Connect, Doc, Fmt, Geom, Kind, Layer, Master, PageDef, Piece, Row, RowKind, Sheet, Val,
    intern, parse_val,
};
use crate::opc::Package;
use crate::xml::{self, Flow, Handler, Tag, XmlError, unescape};
use crate::{PreviewError, PreviewPolicy};

/// Most shapes read from one part.
const MAX_SHAPES: usize = 100_000;
/// Most rows of all sections of one part.
const MAX_ROWS: usize = 600_000;
/// Most rows of one Geometry section.
const MAX_GEOM_ROWS: usize = 100_000;
/// Most sections of one kind per shape.
const MAX_SECTIONS: usize = 64;
/// Most cells kept per sheet or row.
const MAX_CELLS: usize = 256;
/// Most shape-data rows or hyperlinks per shape.
const MAX_DATA: usize = 64;
/// Most pages listed.
const MAX_PAGES: usize = 4_096;
/// Longest name or value kept.
const MAX_NAME: usize = 256;
/// Most masters loaded.
const MAX_MASTERS: usize = 512;
/// Deepest nesting of shapes read.
const MAX_NEST: usize = 64;

/// The page list entry.
#[derive(Debug, Clone, Default)]
pub(crate) struct PageInfo {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) background: bool,
    pub(crate) back_page: Option<u32>,
    pub(crate) rid: String,
}

#[derive(Debug, Clone, Default)]
struct MasterInfo {
    id: u32,
    name: String,
    rid: String,
}

/// What kind of element a frame stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Shape,
    Style,
    PageSheet,
    /// Over the limit: read and thrown away.
    Dummy,
}

struct Frame {
    kind: FrameKind,
    sheet: Sheet,
}

#[derive(Default)]
struct SecState {
    name: String,
    ix: u32,
    geom: Option<Geom>,
    /// Cells that stand directly in the section.
    cells: Cells,
    rows: usize,
}

struct RowState {
    t: String,
    n: String,
    ix: u32,
    del: bool,
    cells: Cells,
    args: Option<Vec<f64>>,
    strings: Vec<(&'static str, String)>,
}

/// The reader of every part of the package.
#[derive(Default)]
struct Build {
    stack: Vec<Frame>,
    top: Vec<Sheet>,
    /// The page sheet of every page of the page list (by position), with its layers.
    page_sheets: Vec<Option<(Cells, Vec<Layer>)>>,
    styles: Vec<Sheet>,
    connects: Vec<Connect>,
    pages: Vec<PageInfo>,
    masters: Vec<MasterInfo>,
    colors: Vec<(usize, u32)>,
    layers: Vec<Layer>,
    shapes: usize,
    rows: usize,
    truncated: bool,
    in_page: bool,
    in_master: bool,
    in_text: bool,
    cur_char: u32,
    cur_para: u32,
    text_bytes: usize,
    max_text: usize,
    sec: Option<SecState>,
    row: Option<RowState>,
}

fn ix_of(tag: &Tag<'_>) -> u32 {
    tag.attr("IX").and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

fn id_of(tag: &Tag<'_>, name: &str) -> Option<u32> {
    tag.attr(name).and_then(|v| v.trim().parse().ok())
}

/// The numbers of a formula such as `NURBS(1, 3, 0, 0, 0.1, 0.2, 0, 1)`; `None` if an argument
/// is anything but a plain number (optionally followed by a unit). The formula is not evaluated.
pub(crate) fn formula_numbers(f: &str) -> Option<Vec<f64>> {
    let open = f.find('(')?;
    let close = f.rfind(')')?;
    if close <= open {
        return None;
    }
    let inner = f.get(open + 1..close)?;
    let mut out = Vec::new();
    for tok in inner.split(',') {
        if out.len() >= MAX_ARGS {
            return None;
        }
        let t = tok.trim();
        let n = match t.parse::<f64>() {
            Ok(n) => n,
            Err(_) => {
                let end = t
                    .char_indices()
                    .find(|(i, c)| !(c.is_ascii_digit() || matches!(c, '.' | '+' | '-') || (*i > 0 && matches!(c, 'e' | 'E'))))
                    .map_or(t.len(), |(i, _)| i);
                let rest = t.get(end..)?.trim();
                if end == 0 || !rest.chars().all(|c| c.is_ascii_alphabetic()) {
                    return None;
                }
                t.get(..end)?.parse::<f64>().ok()?
            }
        };
        if !n.is_finite() {
            return None;
        }
        out.push(n);
    }
    Some(out)
}

/// A visual effect that is not drawn, from the name of a cell.
fn effect_of(name: &str) -> Option<bool> {
    let b = name.as_bytes().first().copied()?;
    if !matches!(b, b'B' | b'E' | b'R' | b'G' | b'S' | b'T' | b'P') {
        return None;
    }
    const D3: [&str; 5] = ["Bevel", "Extrusion", "Rotation3D", "ThreeD", "Perspective3D"];
    const OTHER: [&str; 4] = ["Glow", "SoftEdge", "Reflection", "ShapeShdwBlur"];
    if D3.iter().any(|p| name.starts_with(p)) {
        Some(true)
    } else if OTHER.iter().any(|p| name.starts_with(p)) {
        Some(false)
    } else {
        None
    }
}

impl Build {
    fn frame(&mut self, kind: FrameKind, tag: &Tag<'_>) {
        let mut s = Sheet {
            id: id_of(tag, "ID").unwrap_or(0),
            master: id_of(tag, "Master"),
            master_shape: id_of(tag, "MasterShape"),
            line_style: id_of(tag, "LineStyle"),
            fill_style: id_of(tag, "FillStyle"),
            text_style: id_of(tag, "TextStyle"),
            ..Sheet::default()
        };
        if kind == FrameKind::Shape {
            let name = tag.attr("NameU").or_else(|| tag.attr("Name")).unwrap_or_default();
            s.name = name.chars().take(MAX_NAME).collect();
            s.kind = match tag.attr("Type").as_deref().map(str::trim) {
                Some("Group") => Kind::Group,
                Some("Foreign") => Kind::Foreign,
                Some("Guide") => Kind::Guide,
                _ => Kind::Shape,
            };
        }
        self.stack.push(Frame { kind, sheet: s });
    }

    fn sheet(&mut self) -> Option<&mut Sheet> {
        self.stack.last_mut().map(|f| &mut f.sheet)
    }

    fn cell(&mut self, tag: &Tag<'_>) {
        let Some(n) = tag.attr("N") else { return };
        let v = tag.attr("V");
        // The effects that are not drawn are noticed whatever the cell holds.
        if let Some(d3) = effect_of(&n)
            && v.as_deref().is_some_and(|x| !matches!(x.trim(), "" | "0" | "0.0"))
            && let Some(s) = self.sheet()
        {
            if d3 {
                s.effect_3d = true;
            } else {
                s.effect_other = true;
            }
        }
        let Some(name) = intern(&n) else { return };
        let val = v.as_deref().map(parse_val);
        if let Some(r) = self.row.as_mut() {
            if r.cells.len() < MAX_CELLS
                && let Some(val) = val.clone()
            {
                r.cells.push((name, val));
            }
            if (r.t == "NURBSTo" && name == "E") || (r.t == "PolylineTo" && name == "A") {
                r.args = tag.attr("F").and_then(|f| formula_numbers(&f));
            }
            if matches!(name, "Label" | "Value" | "Prompt" | "Address" | "SubAddress" | "Description" | "Name")
                && let Some(raw) = v
            {
                let s: String = raw.chars().take(MAX_NAME).collect();
                r.strings.push((name, s));
            }
        } else if let Some(sec) = self.sec.as_mut() {
            if let Some(val) = val
                && sec.cells.len() < MAX_CELLS
            {
                sec.cells.push((name, val));
            }
        } else if let Some(val) = val
            && let Some(s) = self.sheet()
            && s.cells.len() < MAX_CELLS
        {
            s.cells.push((name, val));
        }
    }

    fn start_section(&mut self, tag: &Tag<'_>) -> Flow {
        let name = tag.attr("N").unwrap_or_default().into_owned();
        let ix = ix_of(tag);
        let Some(top) = self.stack.last() else { return Flow::SkipChildren };
        if top.kind == FrameKind::Dummy {
            return Flow::SkipChildren;
        }
        let del = tag.attr("Del").is_some_and(|v| matches!(v.trim(), "1" | "true"));
        let count = |f: &Frame| f.sheet.geoms.len().max(f.sheet.chars.len()).max(f.sheet.paras.len());
        if count(top) >= MAX_SECTIONS {
            self.truncated = true;
            return Flow::SkipChildren;
        }
        match name.as_str() {
            "Geometry" => {
                self.sec = Some(SecState {
                    name,
                    ix,
                    geom: Some(Geom { ix, del, ..Geom::default() }),
                    ..SecState::default()
                });
            }
            "Character" | "Paragraph" | "Property" | "User" | "Hyperlink" | "Layer" | "LayerMem" => {
                self.sec = Some(SecState { name, ix, ..SecState::default() });
            }
            _ => return Flow::SkipChildren,
        }
        Flow::Continue
    }

    fn start_row(&mut self, tag: &Tag<'_>) {
        if self.sec.is_none() {
            return;
        }
        self.rows += 1;
        if self.rows > MAX_ROWS {
            self.truncated = true;
            return;
        }
        self.row = Some(RowState {
            t: tag.attr("T").unwrap_or_default().into_owned(),
            n: tag.attr("N").unwrap_or_default().chars().take(MAX_NAME).collect(),
            ix: ix_of(tag),
            del: tag.attr("Del").is_some_and(|v| matches!(v.trim(), "1" | "true")),
            cells: Vec::new(),
            args: None,
            strings: Vec::new(),
        });
    }

    fn end_row(&mut self) {
        let Some(r) = self.row.take() else { return };
        let Some(sec) = self.sec.as_mut() else { return };
        sec.rows += 1;
        let get = |k: &str| r.strings.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        match sec.name.as_str() {
            "Geometry" => {
                if let (Some(g), Some(kind)) = (sec.geom.as_mut(), RowKind::from_name(&r.t)) {
                    if g.rows.len() < MAX_GEOM_ROWS {
                        g.rows.push(Row { kind, ix: r.ix, del: r.del, cells: r.cells, args: r.args });
                    } else {
                        self.truncated = true;
                    }
                }
            }
            "Character" | "Paragraph" => {
                let fmt = Fmt { ix: r.ix, cells: r.cells };
                let para = sec.name == "Paragraph";
                if let Some(f) = self.stack.last_mut().map(|f| &mut f.sheet) {
                    let list = if para { &mut f.paras } else { &mut f.chars };
                    if list.len() < MAX_SECTIONS {
                        list.push(fmt);
                    }
                }
            }
            "Property" | "User" => {
                let label = if sec.name == "User" { get("Prompt") } else { get("Label") };
                let label = label.filter(|l| !l.is_empty()).unwrap_or(r.n.as_str()).to_owned();
                let value = get("Value").unwrap_or("").to_owned();
                if !(label.is_empty() && value.is_empty())
                    && let Some(f) = self.stack.last_mut().map(|f| &mut f.sheet)
                {
                    if f.data.len() < MAX_DATA {
                        f.data.push((label, value));
                    } else {
                        self.truncated = true;
                    }
                }
            }
            "Hyperlink" => {
                let address = get("Address").unwrap_or("").trim().to_owned();
                let sub = get("SubAddress").unwrap_or("").trim().to_owned();
                let mut href = address;
                if !sub.is_empty() {
                    href.push('#');
                    href.push_str(&sub);
                }
                if !href.is_empty()
                    && let Some(f) = self.stack.last_mut().map(|f| &mut f.sheet)
                    && f.links.len() < MAX_DATA
                {
                    f.links.push(href);
                }
            }
            "Layer" => {
                let visible = !matches!(get("Visible").map(str::trim), Some("0"));
                let vis = match r.cells.iter().find(|(n, _)| *n == "Visible") {
                    Some((_, Val::Num(n))) => *n != 0.0,
                    _ => visible,
                };
                if self.layers.len() < 1_024 {
                    self.layers.push(Layer { ix: r.ix, visible: vis });
                }
            }
            "LayerMem" => {
                let member = get("LayerMember").unwrap_or("");
                let ids: Vec<u32> =
                    member.split(';').filter_map(|p| p.trim().parse().ok()).take(64).collect();
                if let Some(f) = self.stack.last_mut().map(|f| &mut f.sheet) {
                    f.layers.extend(ids);
                    f.layers.truncate(64);
                }
            }
            _ => {}
        }
    }

    fn end_section(&mut self) {
        let Some(sec) = self.sec.take() else { return };
        if let Some(mut g) = sec.geom {
            let flag = |name: &str| match sec.cells.iter().find(|(n, _)| *n == name) {
                Some((_, Val::Num(n))) => Some(*n != 0.0),
                _ => None,
            };
            g.no_fill = flag("NoFill");
            g.no_line = flag("NoLine");
            g.no_show = flag("NoShow");
            if let Some(f) = self.stack.last_mut().map(|f| &mut f.sheet) {
                f.geoms.push(g);
            }
        }
        // The cells that stand directly in a Character or Paragraph section are not kept.
        let _ = (sec.name, sec.ix);
    }

    fn end_frame(&mut self) {
        let Some(f) = self.stack.pop() else { return };
        match f.kind {
            FrameKind::Dummy => {}
            FrameKind::Shape => match self.stack.last_mut() {
                Some(parent) if parent.kind == FrameKind::Shape => parent.sheet.kids.push(f.sheet),
                _ => self.top.push(f.sheet),
            },
            FrameKind::Style => self.styles.push(f.sheet),
            FrameKind::PageSheet => {
                let layers = std::mem::take(&mut self.layers);
                if self.in_page
                    && let Some(slot) = self.page_sheets.last_mut()
                {
                    *slot = Some((f.sheet.cells, layers));
                }
            }
        }
    }
}

impl Handler for Build {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "Shape" => {
                self.shapes += 1;
                if self.shapes > MAX_SHAPES || self.stack.len() >= MAX_NEST {
                    self.truncated = true;
                    self.stack.push(Frame { kind: FrameKind::Dummy, sheet: Sheet::default() });
                    return Flow::SkipChildren;
                }
                self.frame(FrameKind::Shape, tag);
            }
            "StyleSheet" => self.frame(FrameKind::Style, tag),
            "PageSheet" => self.frame(FrameKind::PageSheet, tag),
            "Page" => {
                self.in_page = true;
                if self.pages.len() < MAX_PAGES + 1 {
                    self.page_sheets.push(None);
                    let name = tag.attr("Name").or_else(|| tag.attr("NameU")).unwrap_or_default();
                    self.pages.push(PageInfo {
                        id: id_of(tag, "ID").unwrap_or(0),
                        name: name.chars().take(MAX_NAME).collect(),
                        background: tag
                            .attr("Background")
                            .is_some_and(|v| matches!(&*v, "1" | "true")),
                        back_page: id_of(tag, "BackPage"),
                        rid: String::new(),
                    });
                }
            }
            "Master" => {
                self.in_master = true;
                if self.masters.len() < MAX_MASTERS * 4 {
                    let name = tag.attr("NameU").or_else(|| tag.attr("Name")).unwrap_or_default();
                    self.masters.push(MasterInfo {
                        id: id_of(tag, "ID").unwrap_or(0),
                        name: name.chars().take(MAX_NAME).collect(),
                        rid: String::new(),
                    });
                }
            }
            "Rel" if self.stack.is_empty() => {
                let rid = tag.attr_rel("id").unwrap_or_default().into_owned();
                if self.in_page {
                    if let Some(p) = self.pages.last_mut()
                        && p.rid.is_empty()
                    {
                        p.rid = rid;
                    }
                } else if self.in_master
                    && let Some(m) = self.masters.last_mut()
                    && m.rid.is_empty()
                {
                    m.rid = rid;
                }
            }
            "ColorEntry" => {
                let ix = id_of(tag, "IX").and_then(|v| usize::try_from(v).ok());
                if let (Some(ix), Some(Val::Rgb(c))) =
                    (ix, tag.attr("RGB").map(|v| parse_val(&v)))
                    && self.colors.len() < 4_096
                {
                    self.colors.push((ix, c));
                }
            }
            "Section" => return self.start_section(tag),
            "Row" => self.start_row(tag),
            "Cell" => self.cell(tag),
            "Text" if self.sec.is_none() && !self.stack.is_empty() => self.in_text = true,
            "cp" if self.in_text => self.cur_char = ix_of(tag),
            "pp" if self.in_text => self.cur_para = ix_of(tag),
            "ForeignData" => {
                if let Some(s) = self.sheet() {
                    let kind = tag.attr("ForeignType").unwrap_or_default().to_ascii_lowercase();
                    s.foreign = Some(if kind.is_empty() { "image".into() } else { kind });
                }
            }
            "Connect" if self.stack.is_empty() => {
                if self.connects.len() >= 200_000 {
                    self.truncated = true;
                } else {
                    let from = id_of(tag, "FromSheet");
                    let to = id_of(tag, "ToSheet");
                    let cell = tag.attr("FromCell").unwrap_or_default();
                    if let (Some(from), Some(to)) = (from, to) {
                        if cell.starts_with("Begin") {
                            self.connects.push(Connect { from, begin: true, to });
                        } else if cell.starts_with("End") {
                            self.connects.push(Connect { from, begin: false, to });
                        }
                    }
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "Shape" | "StyleSheet" | "PageSheet" => self.end_frame(),
            "Page" => self.in_page = false,
            "Master" => self.in_master = false,
            "Section" => self.end_section(),
            "Row" => self.end_row(),
            "Text" => {
                self.in_text = false;
                self.cur_char = 0;
                self.cur_para = 0;
            }
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        if !self.in_text {
            return Ok(());
        }
        let (cc, cp) = (self.cur_char, self.cur_para);
        let max = self.max_text;
        let used = self.text_bytes;
        let Some(f) = self.stack.last_mut() else { return Ok(()) };
        if f.kind == FrameKind::Dummy {
            return Ok(());
        }
        if used >= max {
            self.truncated = true;
            return Ok(());
        }
        let piece = if cdata { raw.to_owned() } else { unescape(raw)?.into_owned() };
        let add = piece.len();
        match f.sheet.text.last_mut() {
            Some(p) if p.char_ix == cc && p.para_ix == cp => p.text.push_str(&piece),
            _ => f.sheet.text.push(Piece { text: piece, char_ix: cc, para_ix: cp }),
        }
        self.text_bytes += add;
        Ok(())
    }
}

impl Build {
    fn new(policy: &PreviewPolicy) -> Self {
        Self { max_text: policy.max_cell_bytes.min(1 << 20), ..Self::default() }
    }

    /// Resets what belongs to one shape's text.
    fn reset_text(&mut self) {
        self.text_bytes = 0;
    }
}

fn read_xml(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

/// Parses one part; the error of a broken part keeps what was read before it.
fn parse(pkg: &Package<'_>, text: &str, b: &mut Build) -> bool {
    b.reset_text();
    let ok = xml::drive(text, pkg.limits(), b).is_ok();
    // Shapes still open when the part broke off are kept.
    while !b.stack.is_empty() {
        b.end_frame();
    }
    ok
}

/// What to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    /// The page list only.
    Count,
    /// The page list and the content of foreground page `n` (and its background page).
    Page(usize),
    /// Every page.
    All,
}

fn collect_masters(sheets: &[Sheet], out: &mut BTreeSet<u32>, depth: usize) {
    for s in sheets {
        if let Some(m) = s.master {
            out.insert(m);
        }
        if depth < MAX_NEST {
            collect_masters(&s.kids, out, depth + 1);
        }
    }
}

/// Reads the drawing in a package.
///
/// # Errors
/// [`PreviewError::Malformed`] when the page list is missing or unreadable.
pub(crate) fn read(
    pkg: &mut Package<'_>,
    policy: &PreviewPolicy,
    want: Want,
) -> Result<Doc, PreviewError> {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no drawing part".into()))?;
    let Some(pages_part) = pkg.rel_target(&main, "pages")? else {
        return Err(PreviewError::Malformed("the drawing lists no pages".into()));
    };
    let Some(pages_text) = read_xml(pkg, &pages_part)? else {
        return Err(PreviewError::Malformed("the page list is missing".into()));
    };
    let mut pb = Build::new(policy);
    if !parse(pkg, &pages_text, &mut pb) && pb.pages.is_empty() {
        return Err(PreviewError::Malformed("the page list is not readable".into()));
    }
    let mut infos = std::mem::take(&mut pb.pages);
    let mut sheets = std::mem::take(&mut pb.page_sheets).into_iter();
    infos.truncate(MAX_PAGES);
    let mut doc = Doc::default();
    doc.pages_cut = infos.len() >= MAX_PAGES;
    for info in &infos {
        let (cells, layers) = sheets.next().flatten().unwrap_or_default();
        doc.pages.push(PageDef {
            id: info.id,
            name: info.name.clone(),
            background: info.background,
            back_page: info.back_page,
            sheet: cells,
            layers,
            ..PageDef::default()
        });
    }

    // Which pages to load.
    let fg: Vec<usize> =
        doc.pages.iter().enumerate().filter(|(_, p)| !p.background).map(|(i, _)| i).collect();
    let mut load: Vec<usize> = match want {
        Want::Count => Vec::new(),
        Want::All => (0..doc.pages.len()).collect(),
        Want::Page(n) => {
            let mut v = Vec::new();
            if let Some(&i) = fg.get(n) {
                if let Some(bp) = doc.pages.get(i).and_then(|p| p.back_page)
                    && let Some(j) = doc.pages.iter().position(|p| p.id == bp && p.background)
                {
                    v.push(j);
                }
                v.push(i);
            }
            v
        }
    };
    load.dedup();
    let mut used: BTreeSet<u32> = BTreeSet::new();
    for i in load {
        let (Some(info), Some(def)) = (infos.get(i), doc.pages.get_mut(i)) else { continue };
        let Some((part, external)) = pkg.rel_by_id(&pages_part, &info.rid)? else { continue };
        if external {
            continue;
        }
        let Some(text) = read_xml(pkg, &part)? else { continue };
        let mut b = Build::new(policy);
        let ok = parse(pkg, &text, &mut b);
        def.shapes = std::mem::take(&mut b.top);
        def.connects = std::mem::take(&mut b.connects);
        def.truncated = b.truncated || !ok;
        def.loaded = true;
        collect_masters(&def.shapes, &mut used, 0);
    }
    if want != Want::Count {
        read_document(pkg, policy, &main, &mut doc)?;
        read_masters(pkg, policy, &main, &used, &mut doc)?;
    }
    Ok(doc)
}

fn read_document(
    pkg: &mut Package<'_>,
    policy: &PreviewPolicy,
    main: &str,
    doc: &mut Doc,
) -> Result<(), PreviewError> {
    let Some(text) = read_xml(pkg, main)? else { return Ok(()) };
    let mut b = Build::new(policy);
    parse(pkg, &text, &mut b);
    if !b.colors.is_empty() {
        let max = b.colors.iter().map(|(i, _)| *i).max().unwrap_or(0).min(4_095);
        let mut table = vec![0u32; max + 1];
        for (i, c) in &b.colors {
            if let Some(slot) = table.get_mut(*i) {
                *slot = *c;
            }
        }
        doc.colors = table;
    }
    for s in std::mem::take(&mut b.styles) {
        doc.styles.entry(s.id).or_insert(s);
    }
    Ok(())
}

fn read_masters(
    pkg: &mut Package<'_>,
    policy: &PreviewPolicy,
    main: &str,
    used: &BTreeSet<u32>,
    doc: &mut Doc,
) -> Result<(), PreviewError> {
    if used.is_empty() {
        return Ok(());
    }
    let Some(masters_part) = pkg.rel_target(main, "masters")? else { return Ok(()) };
    let Some(text) = read_xml(pkg, &masters_part)? else { return Ok(()) };
    let mut list = Build::new(policy);
    parse(pkg, &text, &mut list);
    for info in list.masters.iter().filter(|m| used.contains(&m.id)).take(MAX_MASTERS) {
        let Some((part, external)) = pkg.rel_by_id(&masters_part, &info.rid)? else { continue };
        if external {
            continue;
        }
        let Some(xml) = read_xml(pkg, &part)? else { continue };
        let mut b = Build::new(policy);
        parse(pkg, &xml, &mut b);
        let m = Master::new(info.id, info.name.clone(), std::mem::take(&mut b.top));
        doc.masters.entry(info.id).or_insert(m);
        // A master's own style sheets are not separate: the document's apply.
    }
    Ok(())
}

