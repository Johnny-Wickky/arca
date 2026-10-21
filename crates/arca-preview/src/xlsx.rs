//! SpreadsheetML (`.xlsx`, `.xlsm`, `.xltx`, `.xltm`).
//!
//! Cells are shown the way the author sees them: numbers go through their
//! number format (dates, percentages, currency), shared strings are resolved,
//! formulas show their cached result (or their text when none was stored) and
//! are never evaluated. Hidden sheets, rows and comments are included and
//! labelled.

use std::collections::BTreeMap;

use crate::PreviewError;
use crate::model::{CellKind, ParaStyle, Paragraph, Run, SectionKind, style};
use crate::numfmt::{self, Format};
use crate::opc::Package;
use crate::sink::{CellLink, GridBuilder, Sink, clip_href};
use crate::xml::{self, Flow, Handler, Tag, XmlError, unescape};

// ---------------------------------------------------------------------------
// workbook.xml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SheetRef {
    name: String,
    hidden: bool,
    rid: String,
}

#[derive(Default)]
struct WorkbookHandler {
    date1904: bool,
    sheets: Vec<SheetRef>,
}

impl Handler for WorkbookHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "workbookPr" => {
                self.date1904 = tag.attr("date1904").is_some_and(|v| matches!(&*v, "1" | "true"));
            }
            "sheet" => {
                self.sheets.push(SheetRef {
                    name: tag.attr("name").unwrap_or_default().into_owned(),
                    hidden: tag
                        .attr("state")
                        .is_some_and(|s| matches!(&*s, "hidden" | "veryHidden")),
                    rid: tag.attr("id").unwrap_or_default().into_owned(),
                });
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
// sharedStrings.xml
// ---------------------------------------------------------------------------

struct SstHandler {
    out: Vec<String>,
    cur: String,
    in_si: bool,
    in_t: bool,
    bytes: usize,
    max_bytes: usize,
    max_items: usize,
    max_one: usize,
    truncated: bool,
}

impl Handler for SstHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "si" => {
                self.in_si = true;
                self.cur.clear();
            }
            "t" if self.in_si => self.in_t = true,
            // Phonetic guides (furigana) duplicate the text.
            "rPh" => return Flow::SkipChildren,
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "t" => self.in_t = false,
            "si" => {
                self.in_si = false;
                if self.out.len() >= self.max_items || self.bytes >= self.max_bytes {
                    self.truncated = true;
                    // Keep indices aligned for the entries that fit.
                    return;
                }
                let s = std::mem::take(&mut self.cur);
                self.bytes = self.bytes.saturating_add(s.len());
                self.out.push(s);
            }
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        if self.in_t && self.cur.len() < self.max_one {
            if cdata {
                self.cur.push_str(raw);
            } else {
                self.cur.push_str(&unescape(raw)?);
            }
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.truncated
    }
}

// ---------------------------------------------------------------------------
// styles.xml
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StylesHandler {
    custom: BTreeMap<u32, String>,
    xfs: Vec<u32>,
    in_xfs: bool,
}

impl Handler for StylesHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "numFmt" => {
                if let (Some(id), Some(code)) = (
                    tag.attr("numFmtId").and_then(|v| v.parse::<u32>().ok()),
                    tag.attr("formatCode"),
                ) {
                    self.custom.insert(id, code.into_owned());
                }
            }
            "cellXfs" => self.in_xfs = true,
            "xf" if self.in_xfs => {
                self.xfs.push(tag.attr("numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0));
            }
            _ => {}
        }
        Flow::Continue
    }
    fn end(&mut self, local: &str) {
        if local == "cellXfs" {
            self.in_xfs = false;
        }
    }
    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

/// Number formats by style index.
struct Formats {
    custom: BTreeMap<u32, String>,
    xfs: Vec<u32>,
    cache: BTreeMap<u32, Format>,
}

impl Formats {
    fn for_style(&mut self, style_idx: usize) -> Option<&Format> {
        let id = self.xfs.get(style_idx).copied().unwrap_or(0);
        if id == 0 {
            return None;
        }
        if !self.cache.contains_key(&id) {
            let code = self.custom.get(&id).map(String::as_str).or_else(|| numfmt::builtin(id))?;
            self.cache.insert(id, numfmt::parse(code));
        }
        self.cache.get(&id)
    }
}

// ---------------------------------------------------------------------------
// Worksheet
// ---------------------------------------------------------------------------

/// `AB12` → (zero-based column, one-based row).
fn parse_ref(r: &str) -> Option<(u32, u32)> {
    let letters = r.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 || letters > 3 {
        return None;
    }
    let mut col = 0u32;
    for b in r.bytes().take(letters) {
        col = col.checked_mul(26)?.checked_add(u32::from(b.to_ascii_uppercase() - b'A' + 1))?;
    }
    let row: u32 = r.get(letters..)?.parse().ok()?;
    Some((col.checked_sub(1)?, row))
}

/// A `<hyperlink>` of a worksheet as written: the cells, the relationship and the location.
struct RawLink {
    cells: String,
    rid: String,
    location: String,
}

/// `A1` or `A1:B2` → (first row, last row, first column, last column); rows one-based.
fn link_range(r: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    let (c1, r1) = parse_ref(a.trim())?;
    let (c2, r2) = parse_ref(b.trim())?;
    Some((r1.min(r2), r1.max(r2), c1.min(c2), c1.max(c2)))
}

struct SheetHandler<'a> {
    grid: GridBuilder,
    links: Vec<RawLink>,
    sst: &'a [String],
    formats: &'a mut Formats,
    date1904: bool,
    // Position.
    row: u32,
    next_col: u32,
    col: u32,
    row_ok: bool,
    // Current cell.
    in_cell: bool,
    ctype: String,
    style_idx: usize,
    v: String,
    f: String,
    is: String,
    in_v: bool,
    in_f: bool,
    in_is: bool,
    in_t: bool,
    formulas: u64,
    stop: bool,
    max_text: usize,
}

impl SheetHandler<'_> {
    fn finish_cell(&mut self) {
        self.in_cell = false;
        if !self.row_ok {
            return;
        }
        let v = self.v.trim();
        let (text, kind) = match self.ctype.as_str() {
            "s" => match v.parse::<usize>().ok().and_then(|i| self.sst.get(i)) {
                Some(s) => (s.clone(), CellKind::Text),
                None => (String::new(), CellKind::Text),
            },
            "inlineStr" => (self.is.clone(), CellKind::Text),
            "str" => (self.v.clone(), CellKind::Text),
            "b" => (
                (if v == "1" || v.eq_ignore_ascii_case("true") { "TRUE" } else { "FALSE" })
                    .to_owned(),
                CellKind::Bool,
            ),
            "e" => (v.to_owned(), CellKind::Error),
            "d" => (v.to_owned(), CellKind::Date),
            _ => match v.parse::<f64>() {
                Ok(n) if !v.is_empty() => {
                    let date1904 = self.date1904;
                    match self.formats.for_style(self.style_idx) {
                        Some(fmt) => fmt.number(n, date1904),
                        None => (numfmt::general(n), CellKind::Number),
                    }
                }
                _ => (self.v.clone(), CellKind::Text),
            },
        };
        let has_formula = !self.f.trim().is_empty();
        let (text, kind) = if has_formula {
            self.formulas = self.formulas.saturating_add(1);
            if text.is_empty() {
                (format!("={}", self.f.trim()), CellKind::Formula)
            } else {
                (text, kind)
            }
        } else {
            (text, kind)
        };
        if !self.grid.cell(self.col, kind, text) {
            self.stop = true;
        }
    }
}

impl Handler for SheetHandler<'_> {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "row" => {
                let r = tag.attr("r").and_then(|v| v.parse::<u32>().ok());
                self.row = r.unwrap_or_else(|| self.row.saturating_add(1));
                self.next_col = 0;
                let hidden = tag.attr("hidden").is_some_and(|v| matches!(&*v, "1" | "true"));
                self.row_ok = self.grid.row(self.row, hidden);
                if !self.row_ok {
                    self.stop = true;
                }
            }
            "c" => {
                self.in_cell = true;
                if let Some((col, row)) = tag.attr("r").as_deref().and_then(parse_ref) {
                    self.col = col;
                    if row != self.row && row > 0 && self.row_ok {
                        // A cell that disagrees with its row: keep the row's number.
                    }
                } else {
                    self.col = self.next_col;
                }
                self.next_col = self.col.saturating_add(1);
                self.ctype = tag.attr("t").unwrap_or_default().into_owned();
                self.style_idx = tag.attr("s").and_then(|v| v.parse().ok()).unwrap_or(0);
                self.v.clear();
                self.f.clear();
                self.is.clear();
            }
            "v" if self.in_cell => self.in_v = true,
            "f" if self.in_cell => self.in_f = true,
            "is" if self.in_cell => self.in_is = true,
            "t" if self.in_is => self.in_t = true,
            "rPh" => return Flow::SkipChildren,
            "hyperlink" if self.links.len() < 20_000 => {
                self.links.push(RawLink {
                    cells: tag.attr("ref").unwrap_or_default().into_owned(),
                    rid: tag.attr("id").unwrap_or_default().into_owned(),
                    location: tag.attr("location").unwrap_or_default().into_owned(),
                });
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "c" => self.finish_cell(),
            "v" => self.in_v = false,
            "f" => self.in_f = false,
            "is" => self.in_is = false,
            "t" => self.in_t = false,
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        let target = if self.in_v {
            &mut self.v
        } else if self.in_f {
            &mut self.f
        } else if self.in_t {
            &mut self.is
        } else {
            return Ok(());
        };
        if target.len() < self.max_text {
            if cdata {
                target.push_str(raw);
            } else {
                target.push_str(&unescape(raw)?);
            }
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.stop
    }
}

// ---------------------------------------------------------------------------
// Cell comments (legacy notes)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CommentsHandler {
    authors: Vec<String>,
    in_author: bool,
    cur_ref: String,
    cur_author: usize,
    in_text: bool,
    in_comment: bool,
    text: String,
    out: Vec<(String, String, String)>,
}

impl Handler for CommentsHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "author" => {
                self.in_author = true;
                self.authors.push(String::new());
            }
            "comment" => {
                self.in_comment = true;
                self.cur_ref = tag.attr("ref").unwrap_or_default().into_owned();
                self.cur_author = tag.attr("authorId").and_then(|v| v.parse().ok()).unwrap_or(0);
                self.text.clear();
            }
            "t" if self.in_comment => self.in_text = true,
            "rPh" => return Flow::SkipChildren,
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "author" => self.in_author = false,
            "t" => self.in_text = false,
            "comment" => {
                self.in_comment = false;
                let author = self.authors.get(self.cur_author).cloned().unwrap_or_default();
                self.out.push((
                    std::mem::take(&mut self.cur_ref),
                    author,
                    std::mem::take(&mut self.text),
                ));
            }
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        let s = if cdata { std::borrow::Cow::Borrowed(raw) } else { unescape(raw)? };
        if self.in_author {
            if let Some(a) = self.authors.last_mut()
                && a.len() < 256
            {
                a.push_str(&s);
            }
        } else if self.in_text && self.text.len() < 32_768 {
            self.text.push_str(&s);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn read_xml(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

/// The links of a sheet that lead out of the workbook. A link that only names a location
/// (`Sheet2!A1`) is a jump inside the workbook and is not shown as a link.
fn sheet_links(
    pkg: &mut Package<'_>,
    part: &str,
    raw: &[RawLink],
    max_href: usize,
) -> Result<Vec<CellLink>, PreviewError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    // One lookup table instead of a scan of the relationships per link.
    let targets: BTreeMap<String, String> =
        pkg.rels_of(part)?.into_iter().filter(|r| r.external).map(|r| (r.id, r.target)).collect();
    let mut out = Vec::new();
    for l in raw {
        let (Some((row1, row2, col1, col2)), Some(target)) =
            (link_range(&l.cells), targets.get(&l.rid))
        else {
            continue;
        };
        let mut href = target.trim().to_owned();
        if href.is_empty() {
            continue;
        }
        if !l.location.is_empty() {
            href.push('#');
            href.push_str(&l.location);
        }
        out.push(CellLink { row1, row2, col1, col2, href: clip_href(&href, max_href) });
    }
    Ok(out)
}

/// Extracts a workbook.
pub(crate) fn extract(pkg: &mut Package<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no workbook part".into()))?;
    let Some(wb_text) = read_xml(pkg, &main)? else {
        return Err(PreviewError::Malformed("the workbook part is missing".into()));
    };
    let mut wb = WorkbookHandler::default();
    if xml::drive(&wb_text, pkg.limits(), &mut wb).is_err() && wb.sheets.is_empty() {
        return Err(PreviewError::Malformed("the workbook part is not readable".into()));
    }

    // Shared strings.
    let mut sst = Vec::new();
    if let Some(p) = pkg.rel_target(&main, "sharedStrings")?
        && let Some(text) = read_xml(pkg, &p)?
    {
        let mut h = SstHandler {
            out: Vec::new(),
            cur: String::new(),
            in_si: false,
            in_t: false,
            bytes: 0,
            max_bytes: sink.policy.max_chars.saturating_mul(4),
            max_items: sink.policy.max_items,
            max_one: sink.policy.max_cell_bytes,
            truncated: false,
        };
        let res = xml::drive(&text, pkg.limits(), &mut h);
        if res.is_err() {
            sink.skipped(&p);
        }
        if h.truncated {
            sink.truncated("shared strings");
        }
        sst = h.out;
    }

    // Number formats.
    let mut formats = Formats { custom: BTreeMap::new(), xfs: Vec::new(), cache: BTreeMap::new() };
    if let Some(p) = pkg.rel_target(&main, "styles")?
        && let Some(text) = read_xml(pkg, &p)?
    {
        let mut h = StylesHandler::default();
        if xml::drive(&text, pkg.limits(), &mut h).is_err() {
            sink.skipped(&p);
        }
        formats.custom = h.custom;
        formats.xfs = h.xfs;
    }

    let mut comments: Vec<(String, String, String, String)> = Vec::new();
    let mut formulas = 0u64;
    let mut drawings = false;

    for sheet in &wb.sheets {
        if sink.is_full() {
            break;
        }
        let Some((part, external)) = pkg.rel_by_id(&main, &sheet.rid)? else {
            sink.skipped(&sheet.name);
            continue;
        };
        if external {
            sink.skipped(&sheet.name);
            continue;
        }
        let Some(sec) = sink.open_section(SectionKind::Sheet, &sheet.name, sheet.hidden) else {
            break;
        };
        let Some(text) = read_xml(pkg, &part)? else {
            sink.skipped(&part);
            sink.drop_if_empty(sec);
            continue;
        };
        let mut h = SheetHandler {
            grid: GridBuilder::new(sink.policy, sink.items_left()),
            links: Vec::new(),
            sst: &sst,
            formats: &mut formats,
            date1904: wb.date1904,
            row: 0,
            next_col: 0,
            col: 0,
            row_ok: false,
            in_cell: false,
            ctype: String::new(),
            style_idx: 0,
            v: String::new(),
            f: String::new(),
            is: String::new(),
            in_v: false,
            in_f: false,
            in_is: false,
            in_t: false,
            formulas: 0,
            stop: false,
            max_text: sink.policy.max_cell_bytes,
        };
        let res = xml::drive(&text, pkg.limits(), &mut h);
        formulas = formulas.saturating_add(h.formulas);
        let raw_links = std::mem::take(&mut h.links);
        let mut grid = h.grid.finish();
        grid.attach_links(&sheet_links(pkg, &part, &raw_links, sink.policy.max_href_bytes)?);
        if res.is_err() {
            sink.skipped(&part);
        }
        if grid.truncated {
            sink.truncated("grid");
        }
        if grid.rows.is_empty() && !grid.truncated {
            // Empty sheets still list their name so nothing appears to be missing.
            sink.push(sec, crate::model::Block::Grid(grid));
        } else {
            sink.push(sec, crate::model::Block::Grid(grid));
        }

        // Notes attached to cells, and drawings we do not render.
        for target in pkg.rel_targets(&part, "comments")? {
            if let Some(ctext) = read_xml(pkg, &target)? {
                let mut ch = CommentsHandler::default();
                if xml::drive(&ctext, pkg.limits(), &mut ch).is_ok() {
                    for (r, a, t) in ch.out {
                        comments.push((sheet.name.clone(), r, a, t));
                    }
                } else {
                    sink.skipped(&target);
                }
            }
        }
        if !pkg.rel_targets(&part, "drawing")?.is_empty() {
            drawings = true;
        }
    }

    if !comments.is_empty()
        && let Some(sec) = sink.open_section(SectionKind::Comments, "", false)
    {
        for (sheet, cell, author, text) in comments {
            let mut runs = Vec::new();
            if !author.is_empty() {
                runs.push(Run {
                    text: format!("{author}: "),
                    style: style::BOLD,
                    href: String::new(),
                });
            }
            runs.push(Run { text, style: 0, href: String::new() });
            let p = Paragraph {
                style: ParaStyle::Comment,
                marker: format!("{sheet}!{cell}"),
                runs,
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                break;
            }
        }
    }
    if formulas > 0 {
        sink.count(crate::model::NoticeKind::FormulasNotEvaluated, "", formulas);
    }
    if drawings {
        sink.note(crate::model::NoticeKind::Unsupported, "drawings and charts");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Grid, NoticeKind, Preview};
    use crate::testkit::{ok, opc, paragraphs, rels, section_of};

    const SS_NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
    const XLSX_CT: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";

    fn workbook(sheets: &str, props: &str) -> String {
        format!(r#"<workbook {SS_NS}>{props}<sheets>{sheets}</sheets></workbook>"#)
    }

    fn xlsx(sheet_xml: &[&str], sst: &str, styles: &str, wb_props: &str) -> Vec<u8> {
        let sheets: String = (1..=sheet_xml.len())
            .map(|i| format!(r#"<sheet name="Sheet{i}" sheetId="{i}" r:id="rId{}"/>"#, i + 10))
            .collect();
        let wb = workbook(&sheets, wb_props);
        let mut rel_list: Vec<(String, &str, String)> = (1..=sheet_xml.len())
            .map(|i| (format!("rId{}", i + 10), "worksheet", format!("worksheets/sheet{i}.xml")))
            .collect();
        rel_list.push(("rId1".into(), "sharedStrings", "sharedStrings.xml".into()));
        rel_list.push(("rId2".into(), "styles", "styles.xml".into()));
        let rl: Vec<(&str, &str, &str)> =
            rel_list.iter().map(|(a, b, c)| (a.as_str(), *b, c.as_str())).collect();
        let wb_rels = rels(&rl);
        let sheets_full: Vec<String> = sheet_xml
            .iter()
            .map(|s| format!(r#"<worksheet {SS_NS}><sheetData>{s}</sheetData></worksheet>"#))
            .collect();
        let names: Vec<String> =
            (1..=sheet_xml.len()).map(|i| format!("xl/worksheets/sheet{i}.xml")).collect();
        let mut parts: Vec<(&str, &str)> = vec![
            ("xl/workbook.xml", wb.as_str()),
            ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
            ("xl/sharedStrings.xml", sst),
            ("xl/styles.xml", styles),
        ];
        for (n, s) in names.iter().zip(&sheets_full) {
            parts.push((n.as_str(), s.as_str()));
        }
        opc(("xl/workbook.xml", XLSX_CT), &parts)
    }

    fn grid(p: &Preview, sheet: usize) -> &Grid {
        let sec = p.sections.get(sheet).expect("section");
        match sec.blocks.first() {
            Some(Block::Grid(g)) => g,
            other => panic!("not a grid: {other:?}"),
        }
    }

    fn cell(g: &Grid, row: u32, col: u32) -> Option<(&str, CellKind)> {
        g.rows
            .iter()
            .find(|r| r.index == row)?
            .cells
            .iter()
            .find(|c| c.col == col)
            .map(|c| (c.text.as_str(), c.kind))
    }

    fn sst_xml(items: &[&str]) -> String {
        let si: String = items.iter().map(|s| format!("<si><t>{s}</t></si>")).collect();
        format!(r#"<sst {SS_NS} count="1">{si}</sst>"#)
    }

    fn styles_xml(custom: &str, xfs: &[u32]) -> String {
        let xf: String = xfs.iter().map(|id| format!(r#"<xf numFmtId="{id}"/>"#)).collect();
        format!(
            r#"<styleSheet {SS_NS}><numFmts>{custom}</numFmts><cellStyleXfs><xf numFmtId="14"/></cellStyleXfs><cellXfs>{xf}</cellXfs></styleSheet>"#
        )
    }

    /// A preview may legitimately have no sections at all: every sheet hidden and hidden
    /// content not asked for. Clients must cope (the Android screen shows "nothing to show"
    /// together with the notice); this pins the shape they are written against.
    #[test]
    fn a_workbook_of_hidden_sheets_gives_a_preview_without_sections_and_says_why() {
        let sheets = r#"<sheet name="Only" sheetId="1" state="hidden" r:id="rId11"/>"#;
        let wb = workbook(sheets, "");
        let wb_rels = rels(&[("rId11", "worksheet", "worksheets/sheet1.xml")]);
        let s1 = format!(
            r#"<worksheet {SS_NS}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row></sheetData></worksheet>"#
        );
        let data = opc(
            ("xl/workbook.xml", XLSX_CT),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/worksheets/sheet1.xml", s1.as_str()),
            ],
        );
        let policy =
            crate::PreviewPolicy { reveal_hidden: false, ..crate::PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert!(p.sections.is_empty(), "the only sheet is hidden and was not asked for");
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted && n.detail == "sheets"),
            "the person is told something was left out"
        );
        // The same file with hidden content asked for shows the sheet.
        let shown = ok(&data);
        assert_eq!(shown.sections.len(), 1);
    }

    #[test]
    fn cell_references() {
        assert_eq!(parse_ref("A1"), Some((0, 1)));
        assert_eq!(parse_ref("Z9"), Some((25, 9)));
        assert_eq!(parse_ref("AA10"), Some((26, 10)));
        assert_eq!(parse_ref("XFD1048576"), Some((16_383, 1_048_576)));
        assert_eq!(parse_ref("1A"), None);
        assert_eq!(parse_ref("ABCD1"), None);
        assert_eq!(parse_ref("A"), None);
        assert_eq!(parse_ref(""), None);
    }

    #[test]
    fn strings_numbers_booleans_errors_and_inline() {
        let data = xlsx(
            &[
                r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1"><v>3.5</v></c></row>
                 <row r="3"><c r="A3" t="b"><v>1</v></c><c r="B3" t="e"><v>#DIV/0!</v></c>
                 <c r="D3" t="inlineStr"><is><t>inline</t></is></c><c r="E3" t="str"><v>fstr</v></c></row>"#,
            ],
            &sst_xml(&["Имя", "A &amp; B"]),
            &styles_xml("", &[0]),
            "",
        );
        let p = ok(&data);
        let g = grid(&p, 0);
        assert_eq!(g.cols, 5);
        assert_eq!(cell(g, 1, 0), Some(("Имя", CellKind::Text)));
        assert_eq!(cell(g, 1, 1), Some(("A & B", CellKind::Text)));
        assert_eq!(cell(g, 1, 2), Some(("3.5", CellKind::Number)));
        assert_eq!(cell(g, 3, 0), Some(("TRUE", CellKind::Bool)));
        assert_eq!(cell(g, 3, 1), Some(("#DIV/0!", CellKind::Error)));
        assert_eq!(cell(g, 3, 3), Some(("inline", CellKind::Text)));
        assert_eq!(cell(g, 3, 4), Some(("fstr", CellKind::Text)));
        assert_eq!(g.rows.len(), 2, "empty row 2 is not stored");
    }

    #[test]
    fn number_formats_dates_and_1904() {
        let custom = r##"<numFmt numFmtId="164" formatCode="#,##0.00&quot; руб.&quot;"/><numFmt numFmtId="165" formatCode="dd/mm/yyyy"/>"##;
        let rows = r#"<row r="1"><c r="A1" s="1"><v>1234.5</v></c><c r="B1" s="2"><v>0.256</v></c><c r="C1" s="3"><v>45356</v></c>
            <c r="D1" s="4"><v>45356.75</v></c><c r="E1" s="5"><v>45356</v></c></row>"#;
        let data = xlsx(&[rows], &sst_xml(&[]), &styles_xml(custom, &[0, 164, 9, 14, 22, 165]), "");
        let p = ok(&data);
        let g = grid(&p, 0);
        assert_eq!(cell(g, 1, 0), Some(("1,234.50 руб.", CellKind::Number)));
        assert_eq!(cell(g, 1, 1), Some(("26%", CellKind::Number)));
        assert_eq!(cell(g, 1, 2), Some(("2024-03-05", CellKind::Date)));
        assert_eq!(cell(g, 1, 3), Some(("2024-03-05 18:00", CellKind::Date)));
        assert_eq!(cell(g, 1, 4), Some(("05/03/2024", CellKind::Date)));

        let data = xlsx(
            &[r#"<row r="1"><c r="A1" s="1"><v>0</v></c></row>"#],
            &sst_xml(&[]),
            &styles_xml("", &[0, 14]),
            r#"<workbookPr date1904="1"/>"#,
        );
        assert_eq!(cell(grid(&ok(&data), 0), 1, 0), Some(("1904-01-01", CellKind::Date)));
    }

    #[test]
    fn formulas_show_cached_values_or_their_text() {
        let data = xlsx(
            &[r#"<row r="1"><c r="A1"><f>SUM(B1:B3)</f><v>6</v></c></row>
                 <row r="2"><c r="A2"><f>cmd|' /C calc'!A0</f></c></row>
                 <row r="3"><c r="A3" t="str"><f>A1&amp;A2</f><v>joined</v></c></row>"#],
            &sst_xml(&[]),
            &styles_xml("", &[0]),
            "",
        );
        let p = ok(&data);
        let g = grid(&p, 0);
        assert_eq!(cell(g, 1, 0), Some(("6", CellKind::Number)));
        assert_eq!(cell(g, 2, 0), Some(("=cmd|' /C calc'!A0", CellKind::Formula)));
        assert_eq!(cell(g, 3, 0), Some(("joined", CellKind::Text)));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::FormulasNotEvaluated));
    }

    #[test]
    fn hidden_sheets_and_rows_are_shown_and_labelled() {
        let sheets = r#"<sheet name="Visible" sheetId="1" r:id="rId11"/><sheet name="Секрет" sheetId="2" state="veryHidden" r:id="rId12"/>"#;
        let wb = workbook(sheets, "");
        let wb_rels = rels(&[
            ("rId11", "worksheet", "worksheets/sheet1.xml"),
            ("rId12", "worksheet", "worksheets/sheet2.xml"),
        ]);
        let s1 = format!(
            r#"<worksheet {SS_NS}><sheetData><row r="1" hidden="1"><c r="A1" t="inlineStr"><is><t>hidden row</t></is></c></row></sheetData></worksheet>"#
        );
        let s2 = format!(
            r#"<worksheet {SS_NS}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>password</t></is></c></row></sheetData></worksheet>"#
        );
        let data = opc(
            ("xl/workbook.xml", XLSX_CT),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/worksheets/sheet1.xml", s1.as_str()),
                ("xl/worksheets/sheet2.xml", s2.as_str()),
            ],
        );
        let p = ok(&data);
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections.get(1).map(|s| (s.name.as_str(), s.hidden)), Some(("Секрет", true)));
        assert!(grid(&p, 0).rows.first().is_some_and(|r| r.hidden));
        assert_eq!(cell(grid(&p, 1), 1, 0), Some(("password", CellKind::Text)));
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "sheets")
        );
    }

    #[test]
    fn hyperlinks_of_cells_are_attached_and_never_followed() {
        let sheets = r#"<sheet name="S" sheetId="1" r:id="rId11"/>"#;
        let wb = workbook(sheets, "");
        let wb_rels = rels(&[("rId11", "worksheet", "worksheets/sheet1.xml")]);
        let s1 = format!(
            r#"<worksheet {SS_NS}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>site</t></is></c><c r="B1"><v>2</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>jump</t></is></c><c r="C2" t="inlineStr"><is><t>bound</t></is></c><c r="D2" t="inlineStr"><is><t>same</t></is></c></row></sheetData><hyperlinks><hyperlink ref="A1" r:id="rId1"/><hyperlink ref="A2" location="Sheet2!A1" display="inside"/><hyperlink ref="C2:D2" r:id="rId2" location="top"/><hyperlink ref="Z9" r:id="rId1"/><hyperlink ref="bad ref" r:id="rId1"/><hyperlink ref="B1" r:id="rId404"/></hyperlinks></worksheet>"#
        );
        let s1_rels = rels(&[
            ("rId1", "hyperlink", "https://example.org/a?x=1&y=2"),
            ("rId2", "hyperlink", "https://example.org/b"),
        ]);
        let data = opc(
            ("xl/workbook.xml", XLSX_CT),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/worksheets/sheet1.xml", s1.as_str()),
                ("xl/worksheets/_rels/sheet1.xml.rels", s1_rels.as_str()),
            ],
        );
        let p = ok(&data);
        let g = grid(&p, 0);
        let href = |row: u32, col: u32| {
            g.rows
                .iter()
                .find(|r| r.index == row)
                .and_then(|r| r.cells.iter().find(|c| c.col == col))
                .map(|c| c.href.as_str())
        };
        assert_eq!(href(1, 0), Some("https://example.org/a?x=1&y=2"));
        assert_eq!(href(1, 1), Some(""), "an unknown relationship is no link");
        assert_eq!(href(2, 0), Some(""), "a jump inside the workbook is no link out");
        assert_eq!(href(2, 2), Some("https://example.org/b#top"), "a range, with its location");
        assert_eq!(href(2, 3), Some("https://example.org/b#top"));
    }

    #[test]
    fn comments_and_drawings() {
        let sheets = r#"<sheet name="S" sheetId="1" r:id="rId11"/>"#;
        let wb = workbook(sheets, "");
        let wb_rels = rels(&[("rId11", "worksheet", "worksheets/sheet1.xml")]);
        let s1 = format!(
            r#"<worksheet {SS_NS}><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#
        );
        let s1_rels = rels(&[
            ("r1", "comments", "../comments1.xml"),
            ("r2", "drawing", "../drawings/drawing1.xml"),
        ]);
        let comments = format!(
            r#"<comments {SS_NS}><authors><author>Eve</author></authors><commentList><comment ref="A1" authorId="0"><text><r><t>look </t></r><r><t>here</t></r></text></comment></commentList></comments>"#
        );
        let data = opc(
            ("xl/workbook.xml", XLSX_CT),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/worksheets/sheet1.xml", s1.as_str()),
                ("xl/worksheets/_rels/sheet1.xml.rels", s1_rels.as_str()),
                ("xl/comments1.xml", comments.as_str()),
            ],
        );
        let p = ok(&data);
        let c = section_of(&p, SectionKind::Comments).expect("comments section");
        let para = paragraphs(c).into_iter().next().expect("comment").clone();
        assert_eq!(para.marker, "S!A1");
        assert_eq!(para.text(), "Eve: look here");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported));
    }

    #[test]
    fn huge_sparse_sheets_are_cut_not_expanded() {
        // 3 cells in rows near the limit of a tiny budget.
        let mut rows = String::new();
        for r in 1..=200 {
            rows.push_str(&format!(
                r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c><c r="B{r}"><v>1</v></c></row>"#
            ));
        }
        let data = xlsx(&[&rows], &sst_xml(&[]), &styles_xml("", &[0]), "");
        let policy = crate::PreviewPolicy { max_grid_cells: 50, ..crate::PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        let g = grid(&p, 0);
        assert!(g.truncated);
        assert_eq!(g.rows.iter().map(|r| r.cells.len()).sum::<usize>(), 50);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Truncated));
    }

    #[test]
    fn rows_far_out_and_columns_beyond_limits() {
        let data = xlsx(
            &[
                r#"<row r="1048576"><c r="XFD1048576"><v>1</v></c></row><row r="2000000"><c r="A1"><v>2</v></c></row>"#,
            ],
            &sst_xml(&[]),
            &styles_xml("", &[0]),
            "",
        );
        let p = ok(&data);
        let g = grid(&p, 0);
        assert_eq!(g.rows.len(), 1);
        assert_eq!(g.cols, 16_384);
        assert!(g.truncated, "row beyond the grid limit");
    }

    #[test]
    fn phonetic_runs_and_rich_text_in_shared_strings() {
        let sst = format!(
            r#"<sst {SS_NS}><si><r><t>Rich </t></r><r><t>text</t></r><rPh sb="0" eb="1"><t>ふりがな</t></rPh></si></sst>"#
        );
        let data = xlsx(
            &[r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>99</v></c></row>"#],
            &sst,
            &styles_xml("", &[0]),
            "",
        );
        let p = ok(&data);
        assert_eq!(cell(grid(&p, 0), 1, 0), Some(("Rich text", CellKind::Text)));
        assert_eq!(cell(grid(&p, 0), 1, 1), None, "out-of-range string index shows nothing");
    }

    #[test]
    fn hostile_sheet_xml_never_panics() {
        let base = r#"<row r="1"><c r="A1" t="s" s="9"><v>0</v></c><c t="b"><v>x</v></c><c r="ZZZ9"><f>1+1</f></c><c r="B1" s="1"><v>1e400</v></c></row><row><c><v>-0</v></c></row>"#;
        let bytes = base.as_bytes();
        for i in 0..bytes.len() {
            for mask in [0x01u8, 0x20, 0xFF] {
                let mut m = bytes.to_vec();
                if let Some(b) = m.get_mut(i) {
                    *b ^= mask;
                }
                if let Ok(s) = std::str::from_utf8(&m) {
                    let data = xlsx(&[s], &sst_xml(&["a"]), &styles_xml("", &[0, 14]), "");
                    let _ = crate::preview(&data, &crate::PreviewPolicy::default());
                }
            }
        }
    }
}
