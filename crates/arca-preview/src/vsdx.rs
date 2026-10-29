//! Visio drawings (VSDX, VSDM and the template/stencil variants).
//!
//! The *reading view* of a drawing; the pages themselves (shapes, geometry, masters, styles) are
//! drawn by `visio_draw`, which reads the same package into a shape model. The reading view shows
//! what a reader needs from a diagram as text: the text of every shape, in reading order (top to bottom, left to right) and
//! with the groups kept together, the shape data ("Cost: 12"), the places shapes link to, the
//! connections between shapes ("Start → Check") and a marker for every picture or embedded
//! object. Nothing is executed or fetched: macros, add-ons, linked files and external data
//! connections are not looked at, and links are only shown.
//!
//! Lines the viewer makes up (a connection, the name of a shape that has no text) carry
//! [`style::SYNTHETIC`], so that a shape whose own text reads "A → B" cannot pass for a
//! connection. What is cut by a limit (shape data, texts, connections, pages) is reported.

use std::collections::BTreeMap;

use crate::PreviewError;
use crate::model::{Block, ImageInfo, NoticeKind, ParaStyle, Paragraph, Run, SectionKind, style};
use crate::opc::Package;
use crate::sink::{SectionId, Sink, clip_href};
use crate::xml::{self, Flow, Handler, Tag, XmlError, unescape};

/// Most shapes read from one page.
const MAX_SHAPES: usize = 100_000;
/// Deepest group nesting that is walked.
const MAX_DEPTH: usize = 64;
/// Most shape-data rows kept per shape.
const MAX_DATA: usize = 64;
/// Most pages shown.
const MAX_PAGES: usize = 4_096;
/// Longest shape name kept.
const MAX_NAME: usize = 256;

fn read_xml(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

// ---------------------------------------------------------------------------
// pages.xml
// ---------------------------------------------------------------------------

struct PageInfo {
    name: String,
    background: bool,
    rid: String,
}

#[derive(Default)]
struct PagesHandler {
    pages: Vec<PageInfo>,
    open: bool,
}

impl Handler for PagesHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "Page" => {
                let name = tag.attr("Name").or_else(|| tag.attr("NameU")).unwrap_or_default();
                let background =
                    tag.attr("Background").is_some_and(|v| matches!(&*v, "1" | "true"));
                self.pages.push(PageInfo {
                    name: name.chars().take(MAX_NAME).collect(),
                    background,
                    rid: String::new(),
                });
                self.open = true;
            }
            // The cells of the page sheet (sizes, scales) are of no use here.
            "PageSheet" => return Flow::SkipChildren,
            "Rel" if self.open => {
                if let Some(p) = self.pages.last_mut()
                    && p.rid.is_empty()
                {
                    p.rid = tag.attr_rel("id").unwrap_or_default().into_owned();
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        if local == "Page" {
            self.open = false;
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }

    fn done(&self) -> bool {
        // One page more than is shown, to know that there were more.
        self.pages.len() > MAX_PAGES
    }
}

// ---------------------------------------------------------------------------
// pageN.xml
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Shape {
    id: u32,
    name: String,
    text: String,
    pin_x: f64,
    pin_y: f64,
    /// Shape data as `(label, value)`.
    data: Vec<(String, String)>,
    /// Where the shape's hyperlink leads, if outside the drawing.
    link: String,
    /// What a picture or embedded object is (`bitmap`, `metafile`, `object`).
    image: Option<String>,
    kids: Vec<Shape>,
}

#[derive(Default)]
struct Row {
    name: String,
    cells: Vec<(String, String)>,
}

impl Row {
    fn get(&self, cell: &str) -> &str {
        self.cells.iter().find(|(n, _)| n == cell).map_or("", |(_, v)| v.as_str())
    }
}

/// A connection end: connector `from` (cell `BeginX`/`EndX`, ...) is glued to shape `to`.
struct Connect {
    from: u32,
    begin: bool,
    to: u32,
}

#[derive(Default)]
struct PageHandler {
    stack: Vec<Shape>,
    top: Vec<Shape>,
    connects: Vec<Connect>,
    shapes: usize,
    truncated: bool,
    /// Something was cut by a limit other than the number of shapes: data rows, cells of a row,
    /// the length of a label, a value or a text, or the number of connections.
    cut: bool,
    in_text: bool,
    /// Name of the section being read, only for the ones that matter.
    section: String,
    row: Option<Row>,
    max_text: usize,
    max_href: usize,
}

fn number(v: &str) -> f64 {
    v.trim().parse::<f64>().ok().filter(|n| n.is_finite()).unwrap_or(0.0)
}

impl Handler for PageHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "Shape" => {
                self.shapes += 1;
                if self.shapes > MAX_SHAPES {
                    self.truncated = true;
                    return Flow::Continue;
                }
                let name = tag.attr("Name").or_else(|| tag.attr("NameU")).unwrap_or_default();
                self.stack.push(Shape {
                    id: tag.attr("ID").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
                    name: name.chars().take(MAX_NAME).collect(),
                    ..Shape::default()
                });
            }
            "Text" if !self.stack.is_empty() && self.section.is_empty() => self.in_text = true,
            "Section" if !self.stack.is_empty() => {
                let n = tag.attr("N").unwrap_or_default();
                if matches!(&*n, "Property" | "Hyperlink" | "User") {
                    self.section = n.into_owned();
                } else {
                    // Geometry, layers, user cells: rows and rows of numbers.
                    return Flow::SkipChildren;
                }
            }
            "Row" if !self.section.is_empty() => {
                self.row = Some(Row {
                    name: tag.attr("N").unwrap_or_default().into_owned(),
                    cells: vec![],
                });
            }
            "Cell" => {
                let n = tag.attr("N").unwrap_or_default();
                let v = tag.attr("V").unwrap_or_default();
                if let Some(row) = self.row.as_mut() {
                    if row.cells.len() < 32 {
                        row.cells.push((n.into_owned(), v.into_owned()));
                    } else {
                        self.cut = true;
                    }
                } else if self.section.is_empty()
                    && let Some(s) = self.stack.last_mut()
                {
                    match &*n {
                        "PinX" => s.pin_x = number(&v),
                        "PinY" => s.pin_y = number(&v),
                        _ => {}
                    }
                }
            }
            "ForeignData" => {
                if let Some(s) = self.stack.last_mut() {
                    let kind = tag.attr("ForeignType").unwrap_or_default().to_ascii_lowercase();
                    s.image = Some(if kind.is_empty() { "image".into() } else { kind });
                }
            }
            "Connect" if self.connects.len() >= 200_000 => self.cut = true,
            "Connect" => {
                let from = tag.attr("FromSheet").and_then(|v| v.trim().parse().ok());
                let to = tag.attr("ToSheet").and_then(|v| v.trim().parse().ok());
                let cell = tag.attr("FromCell").unwrap_or_default();
                if let (Some(from), Some(to)) = (from, to) {
                    if cell.starts_with("Begin") {
                        self.connects.push(Connect { from, begin: true, to });
                    } else if cell.starts_with("End") {
                        self.connects.push(Connect { from, begin: false, to });
                    }
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "Text" => self.in_text = false,
            "Row" => {
                let Some(row) = self.row.take() else { return };
                let max_href = self.max_href;
                let Some(s) = self.stack.last_mut() else { return };
                if self.section == "Property" || self.section == "User" {
                    let label = if self.section == "User" {
                        if row.get("Prompt").is_empty() {
                            row.name.as_str()
                        } else {
                            row.get("Prompt")
                        }
                    } else {
                        if row.get("Label").is_empty() {
                            row.name.as_str()
                        } else {
                            row.get("Label")
                        }
                    };
                    let value = row.get("Value");
                    if !(label.is_empty() && value.is_empty()) {
                        if s.data.len() < MAX_DATA {
                            if label.chars().nth(MAX_NAME).is_some()
                                || value.chars().nth(MAX_NAME).is_some()
                            {
                                self.cut = true;
                            }
                            s.data.push((
                                label.chars().take(MAX_NAME).collect(),
                                value.chars().take(MAX_NAME).collect(),
                            ));
                        } else {
                            self.cut = true;
                        }
                    }
                } else if self.section == "Hyperlink" && s.link.is_empty() {
                    // A link with only a sub-address is a jump inside the drawing.
                    let address = row.get("Address").trim();
                    if !address.is_empty() {
                        let mut href = address.to_owned();
                        let sub = row.get("SubAddress").trim();
                        if !sub.is_empty() {
                            href.push('#');
                            href.push_str(sub);
                        }
                        s.link = clip_href(&href, max_href);
                    }
                }
            }
            "Section" => self.section.clear(),
            "Shape" => {
                if self.shapes > MAX_SHAPES && self.stack.is_empty() {
                    return;
                }
                if let Some(done) = self.stack.pop() {
                    match self.stack.last_mut() {
                        Some(parent) => parent.kids.push(done),
                        None => self.top.push(done),
                    }
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        if !self.in_text {
            return Ok(());
        }
        let Some(s) = self.stack.last_mut() else { return Ok(()) };
        if s.text.len() >= self.max_text {
            self.cut = true;
            return Ok(());
        }
        if cdata {
            s.text.push_str(raw);
        } else {
            s.text.push_str(&unescape(raw)?);
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.truncated
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// What a shape is called when something refers to it: its first line of text, else its name.
fn label_of(s: &Shape) -> String {
    let line = s.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let label = if line.is_empty() { s.name.trim() } else { line };
    let label: String = label.chars().take(60).collect();
    if label.is_empty() { format!("#{}", s.id) } else { label }
}

fn collect_labels(shapes: &[Shape], out: &mut BTreeMap<u32, String>, depth: usize) {
    for s in shapes {
        if out.len() < MAX_SHAPES {
            out.entry(s.id).or_insert_with(|| label_of(s));
        }
        if depth < MAX_DEPTH {
            collect_labels(&s.kids, out, depth + 1);
        }
    }
}

/// Reading order: top to bottom (Visio's y axis points up), then left to right.
fn reading_order(shapes: &mut [Shape]) {
    #[allow(clippy::cast_possible_truncation)]
    let key = |s: &Shape| {
        (((s.pin_y * 20.0).round() as i64).saturating_neg(), (s.pin_x * 20.0).round() as i64, s.id)
    };
    shapes.sort_by_key(key);
}

fn emit(shapes: &[Shape], sec: SectionId, sink: &mut Sink<'_>, depth: usize) -> bool {
    for s in shapes {
        let level = u8::try_from(depth.min(6)).unwrap_or(6);
        let text = s.text.trim();
        if !text.is_empty() {
            for line in
                text.split(['\n', '\r', '\u{2028}']).map(str::trim).filter(|l| !l.is_empty())
            {
                let p = Paragraph {
                    level,
                    runs: vec![Run {
                        text: line.to_owned(),
                        style: 0,
                        href: s.link.clone(),
                        ..Run::default()
                    }],
                    ..Paragraph::default()
                };
                if !sink.para(sec, p) {
                    return false;
                }
            }
        } else if !s.data.is_empty() || !s.link.is_empty() {
            // A shape without text that still carries data or a link: name it.
            let p = Paragraph {
                level,
                runs: vec![Run {
                    text: label_of(s),
                    style: style::ITALIC | style::SYNTHETIC,
                    href: s.link.clone(),
                    ..Run::default()
                }],
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                return false;
            }
        }
        for (label, value) in &s.data {
            let mut runs = Vec::new();
            if !label.is_empty() {
                runs.push(Run {
                    text: format!("{label}: "),
                    style: style::BOLD,
                    href: String::new(),
                    ..Default::default()
                });
            }
            runs.push(Run { text: value.clone(), style: 0, href: String::new(), ..Run::default() });
            let p = Paragraph { style: ParaStyle::Note, level, runs, ..Paragraph::default() };
            if !sink.para(sec, p) {
                return false;
            }
        }
        if let Some(kind) = &s.image {
            let alt = if text.is_empty() { s.name.clone() } else { label_of(s) };
            let info = ImageInfo { alt, media: kind.clone(), ..ImageInfo::default() };
            if !sink.image(sec, info) {
                return false;
            }
        }
        if depth < MAX_DEPTH && !emit(&s.kids, sec, sink, depth + 1) {
            return false;
        }
    }
    true
}

/// "A → B" for every connector with at least one end glued to a shape.
fn emit_connections(
    h: &PageHandler,
    labels: &BTreeMap<u32, String>,
    sec: SectionId,
    sink: &mut Sink<'_>,
) {
    let mut ends: BTreeMap<u32, (Option<u32>, Option<u32>)> = BTreeMap::new();
    for c in &h.connects {
        let e = ends.entry(c.from).or_default();
        if c.begin {
            e.0.get_or_insert(c.to);
        } else {
            e.1.get_or_insert(c.to);
        }
    }
    let name = |id: Option<u32>| {
        id.map_or_else(
            || "?".to_owned(),
            |i| labels.get(&i).cloned().unwrap_or_else(|| format!("#{i}")),
        )
    };
    let mut first = true;
    for (begin, end) in ends.values() {
        if begin.is_none() && end.is_none() {
            continue;
        }
        if first {
            first = false;
            if !sink.push(sec, Block::Rule) {
                return;
            }
        }
        let p = Paragraph {
            runs: vec![Run {
                text: format!("{} → {}", name(*begin), name(*end)),
                style: style::SYNTHETIC,
                href: String::new(),
                ..Default::default()
            }],
            ..Paragraph::default()
        };
        if !sink.para(sec, p) {
            return;
        }
    }
}

fn has_picture(shapes: &[Shape]) -> bool {
    shapes.iter().any(|s| s.image.is_some() || has_picture(&s.kids))
}

/// Extracts a Visio drawing.
pub(crate) fn extract(pkg: &mut Package<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no drawing part".into()))?;
    let Some(pages_part) = pkg.rel_target(&main, "pages")? else {
        return Err(PreviewError::Malformed("the drawing lists no pages".into()));
    };
    let Some(pages_text) = read_xml(pkg, &pages_part)? else {
        return Err(PreviewError::Malformed("the page list is missing".into()));
    };
    let mut pages = PagesHandler::default();
    if xml::drive(&pages_text, pkg.limits(), &mut pages).is_err() && pages.pages.is_empty() {
        return Err(PreviewError::Malformed("the page list is not readable".into()));
    }

    if pages.pages.len() > MAX_PAGES {
        pages.pages.truncate(MAX_PAGES);
        sink.truncated("pages");
    }
    let mut pictures = false;
    for page in &pages.pages {
        if sink.is_full() {
            break;
        }
        let Some((part, external)) = pkg.rel_by_id(&pages_part, &page.rid)? else {
            sink.skipped(&page.name);
            continue;
        };
        if external {
            sink.skipped(&page.name);
            continue;
        }
        let Some(text) = read_xml(pkg, &part)? else {
            sink.skipped(&part);
            continue;
        };
        let mut h = PageHandler {
            max_text: sink.policy.max_cell_bytes,
            max_href: sink.policy.max_href_bytes,
            ..PageHandler::default()
        };
        if xml::drive(&text, pkg.limits(), &mut h).is_err() {
            sink.skipped(&part);
        }
        // Shapes still open when the part broke off are kept.
        while let Some(done) = h.stack.pop() {
            match h.stack.last_mut() {
                Some(parent) => parent.kids.push(done),
                None => h.top.push(done),
            }
        }
        if h.truncated {
            sink.truncated("shapes");
        }
        if h.cut {
            sink.truncated("shape data, texts and connections");
        }
        let title =
            if page.background { format!("{} (background)", page.name) } else { page.name.clone() };
        let Some(sec) = sink.open_section(SectionKind::Page, &title, false) else { break };
        pictures |= has_picture(&h.top);
        reading_order(&mut h.top);
        let mut labels = BTreeMap::new();
        collect_labels(&h.top, &mut labels, 0);
        if emit(&h.top, sec, sink, 0) {
            emit_connections(&h, &labels, sec, sink);
        }
        sink.drop_if_empty(sec);
    }
    if pictures {
        sink.note(NoticeKind::Unsupported, "pictures and embedded objects");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Preview;
    use crate::testkit::{ok, opc, paragraphs, rels, run};

    const VSDX_CT: &str = "application/vnd.ms-visio.drawing.main+xml";
    const NS: &str = r#"xmlns="http://schemas.microsoft.com/office/visio/2012/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn drawing(pages: &[(&str, &str)], page_xml: &[&str]) -> Vec<u8> {
        let mut list = String::new();
        let mut page_rels = Vec::new();
        for (i, (name, extra)) in pages.iter().enumerate() {
            list.push_str(&format!(
                r#"<Page ID="{i}" Name="{name}" NameU="{name}" {extra}><PageSheet><Cell N="PageWidth" V="8.5"/></PageSheet><Rel r:id="rId{}"/></Page>"#,
                i + 1
            ));
            page_rels.push((format!("rId{}", i + 1), format!("page{}.xml", i + 1)));
        }
        let pages_xml = format!(r#"<Pages {NS}>{list}</Pages>"#);
        let doc_rels = rels(&[("rId1", "pages", "pages/pages.xml")]);
        let pr: Vec<(&str, &str, &str)> =
            page_rels.iter().map(|(a, b)| (a.as_str(), "page", b.as_str())).collect();
        let pages_rels = rels(&pr);
        let page_names: Vec<String> =
            (1..=page_xml.len()).map(|i| format!("visio/pages/page{i}.xml")).collect();
        let mut parts: Vec<(&str, &str)> = vec![
            ("visio/_rels/document.xml.rels", doc_rels.as_str()),
            ("visio/pages/pages.xml", pages_xml.as_str()),
            ("visio/pages/_rels/pages.xml.rels", pages_rels.as_str()),
        ];
        for (n, x) in page_names.iter().zip(page_xml) {
            parts.push((n.as_str(), x));
        }
        let doc = format!(r#"<VisioDocument {NS}/>"#);
        parts.push(("visio/document.xml", doc.as_str()));
        opc(("visio/document.xml", VSDX_CT), &parts)
    }

    fn text_of_page(p: &Preview, i: usize) -> Vec<String> {
        paragraphs(&p.sections[i]).iter().map(|p| p.text()).collect()
    }

    #[test]
    fn shapes_are_read_in_reading_order_with_data_links_and_connections() {
        let page = format!(
            r#"<PageContents {NS}><Shapes>
              <Shape ID="2" Name="Check" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="5"/><Text><cp IX="0"/>Check <fld IX="0" Type="0">input</fld><pp IX="0"/> now</Text>
                <Section N="Property"><Row N="Owner"><Cell N="Label" V="Owner"/><Cell N="Value" V="Ann"/></Row><Row N="Cost"><Cell N="Value" V="12"/></Row></Section>
                <Section N="Hyperlink"><Row N="Hyperlink1"><Cell N="Address" V="https://example.org/p?a=1&amp;b=2"/><Cell N="SubAddress" V="part"/></Row></Section>
                <Section N="Geometry" IX="0"><Row T="LineTo" IX="1"><Cell N="X" V="1"/><Cell N="Y" V="2"/></Row></Section>
              </Shape>
              <Shape ID="1" Name="Start" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="8"/><Text>Start</Text></Shape>
              <Shape ID="3" Name="Group" Type="Group"><Cell N="PinX" V="6"/><Cell N="PinY" V="5"/>
                <Shapes><Shape ID="4" Name="Inner" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/><Text>Inner one</Text></Shape></Shapes>
              </Shape>
              <Shape ID="5" Name="Picture" Type="Foreign"><Cell N="PinX" V="9"/><Cell N="PinY" V="5"/><ForeignData ForeignType="Bitmap"><Rel r:id="rId9"/></ForeignData></Shape>
              <Shape ID="6" Name="Connector" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="6"/></Shape>
              <Shape ID="7" Name="Hanging" Type="Shape"><Cell N="PinX" V="3"/><Cell N="PinY" V="1"/></Shape>
              </Shapes>
              <Connects><Connect FromSheet="6" FromCell="BeginX" ToSheet="1" ToCell="PinX"/><Connect FromSheet="6" FromCell="EndX" ToSheet="2" ToCell="PinX"/><Connect FromSheet="7" FromCell="BeginX" ToSheet="99" ToCell="PinX"/><Connect FromSheet="x" FromCell="BeginX" ToSheet="1"/></Connects></PageContents>"#
        );
        let p = ok(&drawing(&[("Flow", "")], &[&page]));
        assert_eq!(p.format, crate::model::FileFormat::Vsdx);
        assert_eq!(p.sections.len(), 1);
        assert_eq!((p.sections[0].kind, p.sections[0].name.as_str()), (SectionKind::Page, "Flow"));
        let t = text_of_page(&p, 0);
        // Top to bottom: Start (y 8), then the middle row left to right, then the lowest.
        assert_eq!(t[0], "Start");
        assert_eq!(
            t[1], "Check input now",
            "fields keep their text; line breaks inside a shape split lines"
        );
        assert!(t.contains(&"Owner: Ann".to_owned()));
        assert!(t.contains(&"Cost: 12".to_owned()), "a row without a label uses its name: {t:?}");
        let inner = t.iter().position(|s| s == "Inner one").expect("grouped shape text");
        assert!(inner > 1, "the group comes after the shape on its left: {t:?}");
        assert!(t.contains(&"Start → Check input now".to_owned()), "{t:?}");
        assert!(t.contains(&"#99 → ?".to_owned()) || t.contains(&"? → #99".to_owned()), "{t:?}");
        // The link rides on the shape's text; the sub-address is part of it.
        let linked = paragraphs(&p.sections[0])
            .into_iter()
            .find(|q| q.text() == "Check input now")
            .and_then(|q| q.runs.first())
            .map(|r| r.href.clone());
        assert_eq!(linked.as_deref(), Some("https://example.org/p?a=1&b=2#part"));
        // The picture is a marker, and what was left out is said.
        assert!(
            p.sections[0]
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Image(i) if i.media == "bitmap"))
        );
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported));
    }

    #[test]
    fn lines_the_viewer_makes_up_cannot_be_mistaken_for_the_drawings_text() {
        // A shape whose text reads like a connection, a real connector, and a shape that has
        // no text but data (it is named by the viewer).
        let page = format!(
            r#"<PageContents {NS}><Shapes>
              <Shape ID="1" Name="One" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="9"/><Text>One</Text></Shape>
              <Shape ID="2" Name="Two" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="8"/><Text>Two</Text></Shape>
              <Shape ID="3" Name="Liar" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="7"/><Text>One → Two</Text></Shape>
              <Shape ID="4" Name="Wire" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="6"/></Shape>
              <Shape ID="5" Name="Named only" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="5"/><Section N="Property"><Row N="p"><Cell N="Value" V="v"/></Row></Section></Shape>
              </Shapes>
              <Connects><Connect FromSheet="4" FromCell="BeginX" ToSheet="1"/><Connect FromSheet="4" FromCell="EndX" ToSheet="2"/></Connects></PageContents>"#
        );
        let p = ok(&drawing(&[("P", "")], &[&page]));
        let lines: Vec<(String, u16)> = paragraphs(&p.sections[0])
            .iter()
            .map(|q| (q.text(), q.runs.first().map_or(0, |r| r.style)))
            .collect();
        let style_of =
            |t: &str, nth: usize| lines.iter().filter(|(x, _)| x == t).nth(nth).map(|(_, s)| *s);
        // The same words twice: the shape's own text is plain, the made-up connection is not.
        assert_eq!(style_of("One → Two", 0), Some(0));
        assert_eq!(style_of("One → Two", 1).map(|s| s & style::SYNTHETIC), Some(style::SYNTHETIC));
        // The viewer's name for a shape without text is marked too.
        assert_eq!(style_of("Named only", 0).map(|s| s & style::SYNTHETIC), Some(style::SYNTHETIC));
        assert_eq!(style_of("One", 0), Some(0));
    }

    #[test]
    fn what_a_limit_cuts_is_reported() {
        let rows: String = (0..70)
            .map(|i| {
                format!(
                    r#"<Row N="r{i}"><Cell N="Label" V="L{i}"/><Cell N="Value" V="V{i}"/></Row>"#
                )
            })
            .collect();
        let page = format!(
            r#"<PageContents {NS}><Shapes><Shape ID="1" Name="Many" Type="Shape"><Text>t</Text><Section N="Property">{rows}</Section></Shape></Shapes></PageContents>"#
        );
        let p = ok(&drawing(&[("P", "")], &[&page]));
        assert!(p.stats.truncated);
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Truncated && n.detail.contains("shape data"))
        );
        // A label longer than the limit is cut, and said.
        let long = "x".repeat(300);
        let page = format!(
            r#"<PageContents {NS}><Shapes><Shape ID="1" Name="One" Type="Shape"><Text>t</Text><Section N="Property"><Row N="a"><Cell N="Label" V="{long}"/><Cell N="Value" V="v"/></Row></Section></Shape></Shapes></PageContents>"#
        );
        let p = ok(&drawing(&[("P", "")], &[&page]));
        assert!(p.stats.truncated);
        // And a drawing under the limits says nothing of the kind.
        let page = format!(
            r#"<PageContents {NS}><Shapes><Shape ID="1" Name="One" Type="Shape"><Text>t</Text></Shape></Shapes></PageContents>"#
        );
        assert!(!ok(&drawing(&[("P", "")], &[&page])).stats.truncated);
    }

    #[test]
    fn pages_beyond_the_limit_are_reported() {
        let pages: Vec<(String, &str)> =
            (0..MAX_PAGES + 3).map(|i| (format!("P{i}"), "")).collect();
        let refs: Vec<(&str, &str)> = pages.iter().map(|(n, e)| (n.as_str(), *e)).collect();
        let page = format!(
            r#"<PageContents {NS}><Shapes><Shape ID="1" Name="s"><Text>t</Text></Shape></Shapes></PageContents>"#
        );
        let xml: Vec<&str> = refs.iter().map(|_| page.as_str()).collect();
        let p = ok(&drawing(&refs, &xml));
        assert!(p.sections.len() <= MAX_PAGES);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Truncated && n.detail == "pages"));
    }

    #[test]
    fn pages_background_missing_and_broken_parts() {
        let page = format!(
            r#"<PageContents {NS}><Shapes><Shape ID="1"><Text>One</Text></Shape></Shapes></PageContents>"#
        );
        let p = ok(&drawing(&[("Main", ""), ("Back", r#"Background="1""#)], &[&page, &page]));
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[1].name, "Back (background)");
        // A page whose part is missing is named and skipped; an empty page leaves no section.
        let empty = format!(r#"<PageContents {NS}><Shapes/></PageContents>"#);
        let p = ok(&drawing(&[("A", ""), ("B", "")], &[&empty, &page]));
        assert_eq!(p.sections.len(), 1);
        assert_eq!(p.sections[0].name, "B");
        // No pages relationship at all.
        let data =
            opc(("visio/document.xml", VSDX_CT), &[("visio/document.xml", "<VisioDocument/>")]);
        assert!(matches!(run(&data), Err(PreviewError::Malformed(_))));
    }

    #[test]
    fn hostile_drawings_stay_bounded() {
        // Deeply nested groups, a flood of shapes and cut-off XML never panic.
        let mut deep = String::new();
        for i in 0..200 {
            deep.push_str(&format!(r#"<Shape ID="{i}"><Text>t{i}</Text><Shapes>"#));
        }
        for _ in 0..200 {
            deep.push_str("</Shapes></Shape>");
        }
        let page = format!(r#"<PageContents {NS}><Shapes>{deep}</Shapes></PageContents>"#);
        let p = ok(&drawing(&[("Deep", "")], &[&page]));
        assert!(!p.sections.is_empty());
        let many = format!(
            r#"<PageContents {NS}><Shapes>{}</Shapes></PageContents>"#,
            (0..3_000)
                .map(|i| format!(r#"<Shape ID="{i}"><Text>s{i}</Text></Shape>"#))
                .collect::<String>()
        );
        let p = ok(&drawing(&[("Many", "")], &[&many]));
        assert_eq!(p.sections[0].blocks.len(), 3_000);
        let cut = many.chars().take(many.len() / 2).collect::<String>();
        let _ = run(&drawing(&[("Cut", "")], &[cut.as_str()]));
    }

    #[test]
    fn positions_beyond_the_range_of_integers_do_not_overflow_the_sort() {
        // Found by the damage test of the binary reader: negating the saturated key of a position
        // like -1e300 overflowed.
        let page = format!(
            r#"<PageContents {NS}><Shapes>
              <Shape ID="1"><Cell N="PinX" V="-1e300"/><Cell N="PinY" V="-1e300"/><Text>far below</Text></Shape>
              <Shape ID="2"><Cell N="PinX" V="1e300"/><Cell N="PinY" V="1e300"/><Text>far above</Text></Shape>
              <Shape ID="3"><Cell N="PinX" V="0"/><Cell N="PinY" V="0"/><Text>middle</Text></Shape>
            </Shapes></PageContents>"#
        );
        let p = ok(&drawing(&[("Far", "")], &[&page]));
        assert_eq!(text_of_page(&p, 0), ["far above", "middle", "far below"]);
    }

    #[test]
    fn mutated_drawings_never_panic() {
        use crate::testkit::{Lcg, fuzz_iterations};
        let page = format!(
            r#"<PageContents {NS}><Shapes><Shape ID="1" Name="A"><Cell N="PinX" V="1"/><Text>Hi <fld>x</fld></Text><Section N="Property"><Row N="r"><Cell N="Value" V="v"/></Row></Section><ForeignData ForeignType="Bitmap"/></Shape></Shapes><Connects><Connect FromSheet="1" FromCell="BeginX" ToSheet="1"/></Connects></PageContents>"#
        );
        let base = drawing(&[("P", "")], &[&page]);
        let mut rng = Lcg::new(11);
        for _ in 0..fuzz_iterations(300) {
            let mut m = base.clone();
            rng.mutate(&mut m, 4);
            let _ = run(&m);
        }
    }
}
