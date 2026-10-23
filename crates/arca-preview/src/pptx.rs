//! PresentationML (`.pptx`, `.pptm`, `.ppsx`, `.ppsm`, `.potx`, `.potm`).
//!
//! One section per slide, in presentation order. Shapes are read in document
//! order with titles first; tables become tables; pictures, charts and
//! SmartArt are not rendered. Hidden slides and hidden shapes are included
//! and labelled; speaker notes follow the slide's content.

use std::collections::BTreeMap;

use crate::PreviewError;
use crate::model::{
    Align, Block, Cell, IMAGE_NONE, ImageInfo, NoticeKind, ParaStyle, Row, SectionKind, Table,
    style,
};
use crate::opc::Package;
use crate::sink::{ParaBuilder, Sink};
use crate::xml::{self, Flow, Handler, Tag, XmlError, unescape};

#[derive(Debug, Clone, Default)]
struct RelInfo {
    target: String,
    external: bool,
    bytes: u64,
    ext: String,
    /// The member's number in the package, for a picture that is part of it.
    id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Slide,
    Notes,
}

#[derive(Default)]
struct ParaAcc {
    b: ParaBuilder,
    level: u32,
    align: Align,
    bullet: Bullet,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Bullet {
    /// Inherit: bullets in body placeholders, none elsewhere.
    #[default]
    Inherit,
    None,
    Char(String),
    Auto(String),
}

#[derive(Default)]
struct TblState {
    rows: Vec<Row>,
    cells: Vec<Cell>,
    cell: Option<Cell>,
    skip_cell: bool,
}

struct SlideHandler {
    mode: Mode,
    out: Vec<(bool, Block)>,
    rels: BTreeMap<String, RelInfo>,
    // Shape state: one flag per open shape element ("is it hidden?").
    shapes: Vec<bool>,
    ph: Option<String>,
    in_tx: bool,
    para: Option<ParaAcc>,
    run_style: u16,
    run_href: String,
    in_t: bool,
    in_run: bool,
    autonum: [u32; 10],
    tables: Vec<TblState>,
    // Picture state.
    pic: Option<ImageInfo>,
    in_pic: u32,
    unsupported: bool,
    slide_hidden: bool,
    title: String,
}

impl SlideHandler {
    fn new(mode: Mode, rels: BTreeMap<String, RelInfo>) -> Self {
        Self {
            mode,
            out: Vec::new(),
            rels,
            shapes: Vec::new(),
            ph: None,
            in_tx: false,
            para: None,
            run_style: 0,
            run_href: String::new(),
            in_t: false,
            in_run: false,
            autonum: [0; 10],
            tables: Vec::new(),
            pic: None,
            in_pic: 0,
            unsupported: false,
            slide_hidden: false,
            title: String::new(),
        }
    }

    fn is_title(&self) -> bool {
        matches!(self.ph.as_deref(), Some("title" | "ctrTitle"))
    }

    fn hidden(&self) -> bool {
        self.shapes.iter().any(|h| *h)
    }

    fn accepts_text(&self) -> bool {
        match self.mode {
            Mode::Slide => !matches!(self.ph.as_deref(), Some("sldNum" | "dt")),
            // The notes body placeholder, or a plain text box: LibreOffice writes the speaker
            // notes as one, and a box somebody put on the notes page is theirs to read.
            // Slide image, number, date, header and footer placeholders are page furniture.
            Mode::Notes => matches!(self.ph.as_deref(), Some("body") | None),
        }
    }

    fn add_text(&mut self, text: &str) {
        let mut st = self.run_style;
        if self.hidden() {
            st |= style::HIDDEN;
        }
        let href = self.run_href.clone();
        if let Some(p) = self.para.as_mut() {
            p.b.text(text, st, &href);
        }
    }

    fn end_para(&mut self) {
        let Some(mut acc) = self.para.take() else { return };
        let level = acc.level.min(8);
        let mut para = acc.b.take(if self.mode == Mode::Notes {
            ParaStyle::Note
        } else {
            match self.ph.as_deref() {
                Some("title" | "ctrTitle") => ParaStyle::Title,
                Some("subTitle") => ParaStyle::Subtitle,
                _ => ParaStyle::Body,
            }
        });
        para.align = acc.align;
        let body_ph = matches!(self.ph.as_deref(), Some("body" | "obj"));
        let marker = match &acc.bullet {
            Bullet::None => None,
            Bullet::Char(c) => Some(c.clone()),
            Bullet::Auto(kind) => {
                let slot = usize::try_from(level).unwrap_or(0);
                if let Some(n) = self.autonum.get_mut(slot) {
                    *n += 1;
                }
                for deeper in self.autonum.iter_mut().skip(slot + 1) {
                    *deeper = 0;
                }
                let n = self.autonum.get(slot).copied().unwrap_or(1);
                Some(auto_marker(kind, n))
            }
            Bullet::Inherit if body_ph && self.mode == Mode::Slide => Some("•".to_owned()),
            Bullet::Inherit => None,
        };
        if !matches!(acc.bullet, Bullet::Auto(_)) {
            let slot = usize::try_from(level).unwrap_or(0);
            if let Some(n) = self.autonum.get_mut(slot) {
                *n = 0;
            }
        }
        if let Some(m) = marker {
            para.marker = m;
            para.level = u8::try_from(level + 1).unwrap_or(1);
        }
        if para.runs.is_empty() && para.marker.is_empty() {
            return;
        }
        if let Some(cell) = self.tables.last_mut().and_then(|t| t.cell.as_mut()) {
            para.marker.clear();
            para.level = 0;
            cell.paras.push(para);
            return;
        }
        if self.is_title() && self.title.is_empty() {
            self.title = para.text().chars().take(80).collect();
        }
        let title = self.is_title();
        self.out.push((title, Block::Para(para)));
    }
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

impl Handler for SlideHandler {
    #[allow(clippy::too_many_lines)]
    fn start(&mut self, tag: &Tag<'_>, empty: bool) -> Flow {
        let local = tag.local();
        match local {
            "Fallback" => return Flow::SkipChildren,
            "sld" => {
                self.slide_hidden = tag.attr("show").is_some_and(|v| matches!(&*v, "0" | "false"));
            }
            "sp" | "grpSp" | "graphicFrame" | "cxnSp" => {
                self.shapes.push(false);
                if local == "sp" {
                    self.ph = None;
                    self.autonum = [0; 10];
                }
            }
            "cNvPr" => {
                let hidden = tag.attr("hidden").is_some_and(|v| matches!(&*v, "1" | "true"));
                if self.in_pic > 0
                    && let Some(p) = self.pic.as_mut()
                {
                    let alt = tag
                        .attr("descr")
                        .filter(|d| !d.trim().is_empty())
                        .or_else(|| tag.attr("title").filter(|d| !d.trim().is_empty()))
                        .or_else(|| tag.attr("name"))
                        .unwrap_or_default();
                    p.alt = alt.into_owned();
                }
                if hidden && let Some(top) = self.shapes.last_mut() {
                    *top = true;
                }
            }
            "ph" => {
                self.ph =
                    Some(tag.attr("type").map_or_else(|| "obj".to_owned(), |t| t.into_owned()));
            }
            "pic" => {
                self.shapes.push(false);
                self.in_pic += 1;
                self.pic = Some(ImageInfo::default());
            }
            "blip" if self.in_pic > 0 => {
                let embed = tag.attr("embed");
                let link = tag.attr("link");
                if let Some(p) = self.pic.as_mut() {
                    if let Some(r) =
                        embed.as_deref().or(link.as_deref()).and_then(|id| self.rels.get(id))
                    {
                        p.bytes = r.bytes;
                        p.id = r.id.unwrap_or(IMAGE_NONE);
                        p.media = r.ext.clone();
                        p.external = r.external || (embed.is_none() && link.is_some());
                    } else {
                        p.external = link.is_some();
                    }
                }
            }
            "graphicData" => {
                let uri = tag.attr("uri").unwrap_or_default();
                if uri.contains("chart") || uri.contains("diagram") || uri.contains("ole") {
                    self.unsupported = true;
                }
            }
            "txBody" => {
                // Table cells keep accepting text whatever the enclosing frame is.
                self.in_tx =
                    self.tables.last().is_some_and(|t| t.cell.is_some()) || self.accepts_text();
            }
            "tbl" => self.tables.push(TblState::default()),
            "tr" => {
                if let Some(t) = self.tables.last_mut() {
                    t.cells.clear();
                }
            }
            "tc" => {
                if let Some(t) = self.tables.last_mut() {
                    let span = tag.attr("gridSpan").and_then(|v| v.parse().ok()).unwrap_or(1);
                    let merged = tag.attr("hMerge").is_some_and(|v| matches!(&*v, "1" | "true"));
                    t.skip_cell = merged;
                    t.cell = Some(Cell { span, header: false, paras: Vec::new() });
                }
                self.in_tx = true;
            }
            // PresentationML has no `p:p`; this is always the DrawingML paragraph.
            "p" if self.in_tx => self.para = Some(ParaAcc::default()),
            "pPr" => {
                if let Some(p) = self.para.as_mut() {
                    p.level = tag.attr("lvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                    p.align = match tag.attr("algn").as_deref() {
                        Some("ctr") => Align::Center,
                        Some("r") => Align::End,
                        Some("just" | "dist") => Align::Justify,
                        _ => Align::Start,
                    };
                }
            }
            "buNone" => {
                if let Some(p) = self.para.as_mut() {
                    p.bullet = Bullet::None;
                }
            }
            "buChar" => {
                if let Some(p) = self.para.as_mut() {
                    let c = tag.attr("char").unwrap_or_default();
                    // Symbol-font code points render as boxes; use a neutral bullet.
                    let c = c.chars().next().filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c));
                    p.bullet = Bullet::Char(c.map_or_else(|| "•".to_owned(), String::from));
                }
            }
            "buAutoNum" => {
                if let Some(p) = self.para.as_mut() {
                    p.bullet = Bullet::Auto(tag.attr("type").unwrap_or_default().into_owned());
                }
            }
            "r" | "fld" if self.para.is_some() => {
                self.in_run = true;
                self.run_style = 0;
                self.run_href.clear();
            }
            "rPr" if self.in_run => {
                if tag.attr("b").is_some_and(|v| matches!(&*v, "1" | "true")) {
                    self.run_style |= style::BOLD;
                }
                if tag.attr("i").is_some_and(|v| matches!(&*v, "1" | "true")) {
                    self.run_style |= style::ITALIC;
                }
                if tag.attr("u").is_some_and(|v| v != "none") {
                    self.run_style |= style::UNDERLINE;
                }
                if tag.attr("strike").is_some_and(|v| v != "noStrike") {
                    self.run_style |= style::STRIKE;
                }
                if let Some(b) = tag.attr("baseline").and_then(|v| v.parse::<i32>().ok()) {
                    if b > 0 {
                        self.run_style |= style::SUPER;
                    } else if b < 0 {
                        self.run_style |= style::SUB;
                    }
                }
            }
            "latin" | "ea" | "cs" if self.in_run => {
                if tag.attr("typeface").is_some_and(|f| {
                    let f = f.to_ascii_lowercase();
                    ["courier", "consolas", "mono", "lucida console"].iter().any(|m| f.contains(m))
                }) {
                    self.run_style |= style::MONO;
                }
            }
            "hlinkClick" if self.in_run => {
                if let Some(id) = tag.attr("id")
                    && let Some(r) = self.rels.get(&*id)
                    && r.external
                {
                    self.run_href.clone_from(&r.target);
                }
            }
            "t" if self.in_run => self.in_t = !empty,
            "br" if self.para.is_some() => self.add_text("\n"),
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "t" => self.in_t = false,
            "r" | "fld" => self.in_run = false,
            "p" if self.in_tx => self.end_para(),
            "txBody" => {
                self.in_tx = false;
                // Table cells keep `in_tx` through their own end tag.
                if self.tables.last().is_some_and(|t| t.cell.is_some()) {
                    self.in_tx = true;
                }
            }
            "tc" => {
                self.in_tx = false;
                if let Some(t) = self.tables.last_mut() {
                    let skip = t.skip_cell;
                    if let Some(cell) = t.cell.take()
                        && !skip
                    {
                        t.cells.push(cell);
                    }
                }
            }
            "tr" => {
                if let Some(t) = self.tables.last_mut() {
                    let cells = std::mem::take(&mut t.cells);
                    if !cells.is_empty() {
                        let header = t.rows.is_empty();
                        t.rows.push(Row {
                            cells: cells.into_iter().map(|c| Cell { header, ..c }).collect(),
                        });
                    }
                }
            }
            "tbl" => {
                if let Some(t) = self.tables.pop()
                    && !t.rows.is_empty()
                {
                    let hidden = self.hidden();
                    let mut table = Table { rows: t.rows };
                    if hidden {
                        for row in &mut table.rows {
                            for c in &mut row.cells {
                                for p in &mut c.paras {
                                    for r in &mut p.runs {
                                        r.style |= style::HIDDEN;
                                    }
                                }
                            }
                        }
                    }
                    self.out.push((false, Block::Table(table)));
                }
            }
            "pic" => {
                self.in_pic = self.in_pic.saturating_sub(1);
                self.shapes.pop();
                if let Some(p) = self.pic.take() {
                    self.out.push((false, Block::Image(p)));
                }
            }
            "sp" => {
                self.shapes.pop();
                self.ph = None;
            }
            "grpSp" | "graphicFrame" | "cxnSp" => {
                self.shapes.pop();
            }
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        if self.in_t && self.para.is_some() {
            let s = if cdata { std::borrow::Cow::Borrowed(raw) } else { unescape(raw)? };
            self.add_text(&s);
        }
        Ok(())
    }
}

fn rel_map(pkg: &mut Package<'_>, part: &str) -> Result<BTreeMap<String, RelInfo>, PreviewError> {
    let mut map = BTreeMap::new();
    for r in pkg.rels_of(part)? {
        let target = if r.external {
            r.target.clone()
        } else {
            crate::opc::resolve(part, &r.target).unwrap_or_else(|| r.target.clone())
        };
        let bytes = if r.external { 0 } else { pkg.size_of(&target).unwrap_or(0) };
        let id = if r.external { None } else { pkg.entry_id(&target) };
        let ext = target.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let ext = if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            ext
        } else {
            String::new()
        };
        map.insert(r.id.clone(), RelInfo { target, external: r.external, bytes, ext, id });
    }
    Ok(map)
}

fn read_xml(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

#[derive(Default)]
struct PresentationHandler {
    slides: Vec<String>,
}

impl Handler for PresentationHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        if tag.local() == "sldId" {
            // The relationship id is the namespaced `r:id`, not the numeric `id`.
            let rid = tag
                .attrs()
                .find(|(n, _)| n.contains(':') && xml::local_part(n) == "id")
                .and_then(|(_, v)| unescape(v).ok());
            if let Some(rid) = rid {
                self.slides.push(rid.into_owned());
            }
        }
        Flow::Continue
    }
    fn end(&mut self, _local: &str) {}
    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

/// Extracts a presentation.
pub(crate) fn extract(pkg: &mut Package<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no presentation part".into()))?;
    let Some(text) = read_xml(pkg, &main)? else {
        return Err(PreviewError::Malformed("the presentation part is missing".into()));
    };
    let mut ph = PresentationHandler::default();
    if xml::drive(&text, pkg.limits(), &mut ph).is_err() && ph.slides.is_empty() {
        return Err(PreviewError::Malformed("the presentation part is not readable".into()));
    }

    let mut unsupported = false;
    for rid in &ph.slides {
        if sink.is_full() {
            break;
        }
        let Some((part, external)) = pkg.rel_by_id(&main, rid)? else { continue };
        if external {
            continue;
        }
        let Some(slide_xml) = read_xml(pkg, &part)? else {
            sink.skipped(&part);
            continue;
        };
        let rels = rel_map(pkg, &part)?;
        let mut h = SlideHandler::new(Mode::Slide, rels);
        if xml::drive(&slide_xml, pkg.limits(), &mut h).is_err() {
            sink.skipped(&part);
        }
        unsupported |= h.unsupported;
        let hidden = h.slide_hidden;
        let title = h.title.clone();
        let mut blocks: Vec<Block> = Vec::new();
        // Titles first, then the rest in document order.
        let (titles, rest): (Vec<_>, Vec<_>) = h.out.into_iter().partition(|(t, _)| *t);
        blocks.extend(titles.into_iter().map(|(_, b)| b));
        blocks.extend(rest.into_iter().map(|(_, b)| b));

        // Speaker notes.
        if let Some(notes_part) = pkg.rel_target(&part, "notesSlide")?
            && let Some(notes_xml) = read_xml(pkg, &notes_part)?
        {
            let mut nh = SlideHandler::new(Mode::Notes, BTreeMap::new());
            if xml::drive(&notes_xml, pkg.limits(), &mut nh).is_err() {
                sink.skipped(&notes_part);
            }
            blocks.extend(nh.out.into_iter().map(|(_, b)| b));
        }

        // Slides without a title placeholder are named after their first line of text.
        let name = if title.is_empty() {
            blocks
                .iter()
                .find_map(|b| match b {
                    Block::Para(p) if !p.is_blank() => Some(p.text().chars().take(80).collect()),
                    _ => None,
                })
                .unwrap_or_default()
        } else {
            title
        };
        let Some(sec) = sink.open_section(SectionKind::Slide, &name, hidden) else { break };
        for b in blocks {
            let pushed = match b {
                Block::Image(i) => sink.image(sec, i),
                other => sink.push(sec, other),
            };
            if !pushed {
                break;
            }
        }
    }
    if unsupported {
        sink.note(NoticeKind::Unsupported, "charts, SmartArt and embedded objects");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Preview;
    use crate::testkit::{ok, opc, paragraphs, rels, section_of};

    const P_NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;
    const PPTX_CT: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";

    fn shape(ph: &str, text_paras: &str) -> String {
        format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="s"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/>{text_paras}</p:txBody></p:sp>"#
        )
    }

    fn deck(slides: &[(&str, &str)], extra: &[(&str, &str)]) -> Vec<u8> {
        // slides: (slide xml body, slide rels xml or "")
        let ids: String = (1..=slides.len())
            .map(|i| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + i, i + 10))
            .collect();
        let pres =
            format!(r#"<p:presentation {P_NS}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#);
        let rl: Vec<(String, String)> = (1..=slides.len())
            .map(|i| (format!("rId{}", i + 10), format!("slides/slide{i}.xml")))
            .collect();
        let rl_ref: Vec<(&str, &str, &str)> =
            rl.iter().map(|(a, b)| (a.as_str(), "slide", b.as_str())).collect();
        let pres_rels = rels(&rl_ref);
        let slide_docs: Vec<String> = slides
            .iter()
            .map(|(body, _)| {
                let hidden = if body.starts_with("HIDDEN") { r#" show="0""# } else { "" };
                let body = body.trim_start_matches("HIDDEN");
                format!(
                    r#"<p:sld {P_NS}{hidden}><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:sld>"#
                )
            })
            .collect();
        let names: Vec<String> =
            (1..=slides.len()).map(|i| format!("ppt/slides/slide{i}.xml")).collect();
        let rel_names: Vec<String> =
            (1..=slides.len()).map(|i| format!("ppt/slides/_rels/slide{i}.xml.rels")).collect();
        let mut parts: Vec<(&str, &str)> = vec![
            ("ppt/presentation.xml", pres.as_str()),
            ("ppt/_rels/presentation.xml.rels", pres_rels.as_str()),
        ];
        for (i, (_, r)) in slides.iter().enumerate() {
            parts.push((names[i].as_str(), slide_docs[i].as_str()));
            if !r.is_empty() {
                parts.push((rel_names[i].as_str(), r));
            }
        }
        parts.extend_from_slice(extra);
        opc(("ppt/presentation.xml", PPTX_CT), &parts)
    }

    fn slide_texts(p: &Preview, i: usize) -> Vec<String> {
        p.sections
            .get(i)
            .map(|s| paragraphs(s).iter().map(|p| p.text()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn titles_come_first_and_bullets_follow_placeholders() {
        let body = shape(
            r#"<p:ph type="body" idx="1"/>"#,
            r#"<a:p><a:r><a:t>First</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:t>Nested</a:t></a:r></a:p>
               <a:p><a:pPr><a:buNone/></a:pPr><a:r><a:t>No bullet</a:t></a:r></a:p>"#,
        );
        let title = shape(
            r#"<p:ph type="title"/>"#,
            r#"<a:p><a:r><a:rPr b="1"/><a:t>Quarterly </a:t></a:r><a:r><a:t>review</a:t></a:r></a:p>"#,
        );
        let free = shape("", r#"<a:p><a:r><a:t>Text box</a:t></a:r></a:p>"#);
        let p = ok(&deck(&[(&format!("{body}{free}{title}"), "")], &[]));
        let sec = p.sections.first().expect("slide");
        assert_eq!(sec.kind, SectionKind::Slide);
        assert_eq!(sec.name, "Quarterly review");
        let paras = paragraphs(sec);
        let got: Vec<(ParaStyle, &str, u8, String)> =
            paras.iter().map(|p| (p.style, p.marker.as_str(), p.level, p.text())).collect();
        assert_eq!(
            got,
            [
                (ParaStyle::Title, "", 0, "Quarterly review".to_owned()),
                (ParaStyle::Body, "•", 1, "First".to_owned()),
                (ParaStyle::Body, "•", 2, "Nested".to_owned()),
                (ParaStyle::Body, "", 0, "No bullet".to_owned()),
                (ParaStyle::Body, "", 0, "Text box".to_owned()),
            ]
        );
    }

    #[test]
    fn numbered_lists_and_run_formatting() {
        let body = shape(
            "",
            r#"<a:p><a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>one</a:t></a:r></a:p>
               <a:p><a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:rPr b="1" i="1" u="sng" baseline="30000"/><a:t>two</a:t></a:r><a:br/><a:r><a:t>more</a:t></a:r></a:p>
               <a:p><a:pPr><a:buChar char="-"/></a:pPr><a:r><a:t>dash</a:t></a:r></a:p>
               <a:p><a:pPr><a:buAutoNum type="romanLcPeriod"/></a:pPr><a:r><a:t>roman</a:t></a:r></a:p>"#,
        );
        let p = ok(&deck(&[(&body, "")], &[]));
        let sec = p.sections.first().expect("slide");
        let paras = paragraphs(sec);
        assert_eq!(
            paras.iter().map(|p| p.marker.as_str()).collect::<Vec<_>>(),
            ["1.", "2.", "-", "i."]
        );
        let second = paras.get(1).expect("second");
        assert_eq!(second.text(), "two\nmore");
        assert_eq!(
            second.runs.first().map(|r| r.style),
            Some(style::BOLD | style::ITALIC | style::UNDERLINE | style::SUPER)
        );
    }

    #[test]
    fn tables_with_spans_and_merges() {
        let tbl = r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="t"/></p:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl>
            <a:tr><a:tc gridSpan="2"><a:txBody><a:p><a:r><a:t>Head</a:t></a:r></a:p></a:txBody></a:tc><a:tc hMerge="1"><a:txBody><a:p/></a:txBody></a:tc></a:tr>
            <a:tr><a:tc><a:txBody><a:p><a:r><a:t>a</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>b</a:t></a:r></a:p></a:txBody></a:tc></a:tr>
            </a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#;
        let p = ok(&deck(&[(tbl, "")], &[]));
        let Some(Block::Table(t)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no table")
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows.first().map(|r| r.cells.len()), Some(1));
        assert_eq!(
            t.rows.first().and_then(|r| r.cells.first()).map(|c| (c.span, c.header)),
            Some((2, true))
        );
        assert_eq!(t.rows.get(1).map(|r| r.cells.len()), Some(2));
    }

    #[test]
    fn hidden_slides_shapes_and_notes() {
        let visible = shape("", r#"<a:p><a:r><a:t>shown</a:t></a:r></a:p>"#);
        let hidden_shape = r#"<p:sp><p:nvSpPr><p:cNvPr id="9" name="h" hidden="1"/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>concealed</a:t></a:r></a:p></p:txBody></p:sp>"#;
        let notes = format!(
            r#"<p:notes {P_NS}><p:cSld><p:spTree>{}{}{}</p:spTree></p:cSld></p:notes>"#,
            shape(r#"<p:ph type="sldImg"/>"#, r#"<a:p><a:r><a:t>IGNORED</a:t></a:r></a:p>"#),
            shape(
                r#"<p:ph type="body" idx="1"/>"#,
                r#"<a:p><a:r><a:t>say this aloud</a:t></a:r></a:p>"#
            ),
            shape(r#"<p:ph type="sldNum"/>"#, r#"<a:p><a:r><a:t>7</a:t></a:r></a:p>"#),
        );
        let slide_rels = rels(&[("rId1", "notesSlide", "../notesSlides/notesSlide1.xml")]);
        let p = ok(&deck(
            &[
                (&format!("{visible}{hidden_shape}"), slide_rels.as_str()),
                (&format!("HIDDEN{visible}"), ""),
            ],
            &[("ppt/notesSlides/notesSlide1.xml", notes.as_str())],
        ));
        assert_eq!(p.sections.len(), 2);
        let s1 = p.sections.first().expect("s1");
        let paras = paragraphs(s1);
        let by = |t: &str| {
            paras.iter().find(|p| p.text() == t).map(|p| (p.style, p.runs.first().map(|r| r.style)))
        };
        assert_eq!(by("shown"), Some((ParaStyle::Body, Some(0))));
        assert_eq!(by("concealed"), Some((ParaStyle::Body, Some(style::HIDDEN))));
        assert_eq!(by("say this aloud"), Some((ParaStyle::Note, Some(0))));
        assert!(by("IGNORED").is_none() && by("7").is_none());
        assert!(p.sections.get(1).is_some_and(|s| s.hidden));
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "slides")
        );
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "text"));
    }

    #[test]
    fn notes_in_a_plain_text_box_are_shown_but_page_furniture_is_not() {
        // LibreOffice writes speaker notes as a text box without a placeholder.
        let notes = format!(
            r#"<p:notes {P_NS}><p:cSld><p:spTree>{}{}{}{}</p:spTree></p:cSld></p:notes>"#,
            shape("", r#"<a:p><a:r><a:t>notes from a text box</a:t></a:r></a:p>"#),
            shape(r#"<p:ph type="hdr"/>"#, r#"<a:p><a:r><a:t>HEADER</a:t></a:r></a:p>"#),
            shape(r#"<p:ph type="ftr"/>"#, r#"<a:p><a:r><a:t>FOOTER</a:t></a:r></a:p>"#),
            shape(r#"<p:ph type="dt"/>"#, r#"<a:p><a:r><a:t>DATE</a:t></a:r></a:p>"#),
        );
        let slide_rels = rels(&[("rId1", "notesSlide", "../notesSlides/notesSlide1.xml")]);
        let p = ok(&deck(
            &[(&shape("", r#"<a:p><a:r><a:t>slide</a:t></a:r></a:p>"#), slide_rels.as_str())],
            &[("ppt/notesSlides/notesSlide1.xml", notes.as_str())],
        ));
        let s1 = p.sections.first().expect("s1");
        let paras = paragraphs(s1);
        let note = |t: &str| paras.iter().find(|p| p.text() == t).map(|p| p.style);
        assert_eq!(note("notes from a text box"), Some(ParaStyle::Note));
        assert_eq!(note("slide"), Some(ParaStyle::Body));
        for furniture in ["HEADER", "FOOTER", "DATE"] {
            assert!(note(furniture).is_none(), "{furniture} is not a note");
        }
    }

    #[test]
    fn pictures_links_and_alternate_content() {
        let pic = r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="Pic" descr="Org chart"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId7"/></p:blipFill></p:pic>"#;
        let link = shape(
            "",
            r#"<a:p><a:r><a:rPr><a:hlinkClick r:id="rId8"/></a:rPr><a:t>click me</a:t></a:r></a:p>"#,
        );
        let alt = r#"<mc:AlternateContent><mc:Choice Requires="x"><p:sp><p:nvSpPr><p:cNvPr id="1" name="c"/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>choice</a:t></a:r></a:p></p:txBody></p:sp></mc:Choice><mc:Fallback><p:sp><p:nvSpPr><p:cNvPr id="1" name="f"/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>fallback</a:t></a:r></a:p></p:txBody></p:sp></mc:Fallback></mc:AlternateContent>"#;
        let r = rels(&[
            ("rId7", "image", "../media/image1.png"),
            ("rId8", "hyperlink", "http://evil.example/login"),
        ]);
        let p = ok(&deck(
            &[(&format!("{pic}{link}{alt}"), r.as_str())],
            &[("ppt/media/image1.png", "PNG!")],
        ));
        let sec = p.sections.first().expect("slide");
        let image = sec
            .blocks
            .iter()
            .find_map(|b| if let Block::Image(i) = b { Some(i) } else { None })
            .expect("image");
        assert_eq!(
            (image.alt.as_str(), image.media.as_str(), image.bytes),
            ("Org chart", "png", 4)
        );
        let texts = slide_texts(&p, 0);
        assert!(texts.contains(&"choice".to_owned()) && !texts.contains(&"fallback".to_owned()));
        let link_run = paragraphs(sec)
            .into_iter()
            .flat_map(|p| p.runs.iter())
            .find(|r| r.text == "click me")
            .cloned()
            .expect("link run");
        assert_eq!(link_run.href, "http://evil.example/login");
    }

    #[test]
    fn charts_are_reported_as_unsupported() {
        let gf = r#"<p:graphicFrame><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"/></a:graphic></p:graphicFrame>"#;
        let p = ok(&deck(&[(gf, "")], &[]));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported));
        assert!(section_of(&p, SectionKind::Slide).is_some());
    }

    #[test]
    fn hostile_slides_never_panic() {
        let base = shape(
            r#"<p:ph type="title"/>"#,
            r#"<a:p><a:pPr lvl="99"/><a:r><a:rPr b="1" baseline="x"/><a:t>héllo</a:t></a:r></a:p>"#,
        );
        let bytes = base.as_bytes();
        for i in 0..bytes.len() {
            for mask in [0x01u8, 0x20, 0xFF] {
                let mut m = bytes.to_vec();
                if let Some(b) = m.get_mut(i) {
                    *b ^= mask;
                }
                if let Ok(s) = std::str::from_utf8(&m) {
                    let _ =
                        crate::preview(&deck(&[(s, "")], &[]), &crate::PreviewPolicy::default());
                }
            }
        }
    }
}
