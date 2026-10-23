// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! JSON views of the engine's results.
//!
//! Everything the apps display is produced here from *validated, typed* values
//! (the wire decoder has already bounded every string and list), never from raw
//! guest bytes. The shapes are a contract with the Java side
//! (`android/app/src/main/java/org/arca/viewer/core/Model.java`); every change
//! here is mirrored there and covered by the tests of both.
//!
//! ## X-ray report
//!
//! ```text
//! {"format":"Docx","family":"Text","verdict":"HiddenTrackers","rejection":"","sha256":"…",
//!  "counters":{"trackers":1,"active":0,"findings":3},
//!  "findings":[{"kind":"ExternalImage","cat":"RemoteContent","sev":"Medium",
//!               "loc":"word/_rels/document.xml.rels","detail":"https://…","blocked":true}],
//!  "macros":[{"lang":"Vba","loc":"…","name":"…","source":"…","truncated":false,
//!             "autoExec":[…],"suspicious":[…],"iocs":[…]}],
//!  "refs":[{"target":"…","kind":"Image","class":"Web","loc":"…","auto":true}],
//!  "meta":[{"key":"Author","name":"…","value":"…","src":"…"}],
//!  "embedded":[{"loc":"…","kind":"Package","class":"","clsid":"","file":"","size":0,"sha256":"…"}],
//!  "stats":{"parts":0,"xmlEvents":0,"inflated":0,"nesting":0}}
//! ```
//!
//! ## Preview
//!
//! The metadata lists the sections; the content is fetched in pages of *items*.
//! A paragraph, picture or rule is one item, a table contributes one item per row
//! and a spreadsheet grid one header item plus one item per row, so a sheet with a
//! million rows costs the app nothing until the user scrolls to them.
//!
//! ```text
//! meta:  {"format":"Docx","family":"Text","sections":[{"kind":"Body","name":"","hidden":false,"items":12}],
//!         "notices":[{"kind":"HiddenShown","count":2,"detail":"text"}],
//!         "stats":{"chars":0,"items":0,"truncated":false}}
//! items: [ {"k":"p","s":"Heading1","a":1,"l":1,"m":"1.","r":[["text",1],["link",4,"https://…"]]},
//!          {"k":"tr","n":3,"first":true,"last":false,"cells":[{"c":1,"h":true,"p":[{…}]}]},
//!          {"k":"gh","cols":3,"w":[8,12,6],"rows":120,"trunc":false},
//!          {"k":"gr","i":7,"h":false,"c":[[0,0,"text","https://link"],[2,1,"1,5"]]},
//!          {"k":"i","alt":"logo","media":"png","bytes":1234,"ext":false},
//!          {"k":"r"} ]
//! ```
//!
//! Paragraph keys with their default value (`s` = `Body`, `a` = 0 start, `l` = 0,
//! `m` empty) are left out. Run style bits are those of `arca_preview::model::style`.

use std::collections::HashMap;

use arca_attribution::passport::PatronPassport;
use arca_attribution::screen::{CardRole, CreatorCard, Lang, PatronageScreen};
use arca_preview::model::{
    Align, Block, CellKind, Grid, IMAGE_NONE, ImageInfo, Paragraph, Preview, Row, Run, Section,
};
use arca_preview::page::{PageModel, Prim};
use arca_xray::sanitize::SanitizedDocument;
use arca_xray::{Category, Severity, XRayReport};

use crate::json::JsonWriter;

/// Longest column, in characters, a grid asks the app to reserve.
const MAX_COL_CHARS: usize = 40;
/// Narrowest column, in characters.
const MIN_COL_CHARS: usize = 4;

fn u64_of(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// X-ray report
// ---------------------------------------------------------------------------

/// Counters the app feeds into the Patron Passport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Remote-content findings of Medium severity or higher (trackers, remote templates).
    pub trackers: u64,
    /// Active-content findings of Medium severity or higher.
    pub active: u64,
    /// All stored findings.
    pub findings: u64,
}

/// Counts what the Patron Passport reports for `report`.
#[must_use]
pub fn counters(report: &XRayReport) -> Counters {
    let mut c = Counters { findings: u64_of(report.findings.len()), ..Counters::default() };
    for f in &report.findings {
        if f.severity < Severity::Medium {
            continue;
        }
        match f.kind.category() {
            Category::RemoteContent => c.trackers += 1,
            Category::ActiveContent => c.active += 1,
            _ => {}
        }
    }
    c
}

fn strings(w: &mut JsonWriter, key: &str, items: &[String]) {
    w.key(key);
    w.begin_array();
    for s in items {
        w.string(s);
    }
    w.end_array();
}

/// Writes the X-ray report as JSON.
#[must_use]
pub fn report_json(report: &XRayReport) -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_name("format", &report.format);
    w.field_name("family", &report.format.family());
    w.field_name("verdict", &report.verdict);
    w.field_str("rejection", &report.rejection);
    w.field_str("sha256", &report.sha256);
    let c = counters(report);
    w.key("counters");
    w.begin_object();
    w.field_u64("trackers", c.trackers);
    w.field_u64("active", c.active);
    w.field_u64("findings", c.findings);
    w.end_object();

    w.key("findings");
    w.begin_array();
    for f in &report.findings {
        w.begin_object();
        w.field_str("kind", f.kind.id());
        w.field_name("cat", &f.kind.category());
        w.field_name("sev", &f.severity);
        w.field_str("loc", &f.location);
        w.field_str("detail", &f.detail);
        w.field_bool("blocked", f.neutralized);
        w.end_object();
    }
    w.end_array();

    w.key("macros");
    w.begin_array();
    for m in &report.macros {
        w.begin_object();
        w.field_name("lang", &m.language);
        w.field_str("loc", &m.location);
        w.field_str("name", &m.name);
        w.field_str("source", &m.source);
        w.field_bool("truncated", m.truncated);
        strings(&mut w, "autoExec", &m.auto_exec);
        strings(&mut w, "suspicious", &m.suspicious);
        strings(&mut w, "iocs", &m.iocs);
        w.end_object();
    }
    w.end_array();

    w.key("refs");
    w.begin_array();
    for r in &report.external_refs {
        w.begin_object();
        w.field_str("target", &r.target);
        w.field_name("kind", &r.kind);
        w.field_name("class", &r.class);
        w.field_str("loc", &r.location);
        w.field_bool("auto", r.auto_load);
        w.end_object();
    }
    w.end_array();

    w.key("meta");
    w.begin_array();
    for m in &report.metadata {
        w.begin_object();
        w.field_name("key", &m.key);
        w.field_str("name", &m.name);
        w.field_str("value", &m.value);
        w.field_str("src", &m.source);
        w.end_object();
    }
    w.end_array();

    w.key("embedded");
    w.begin_array();
    for e in &report.embedded {
        w.begin_object();
        w.field_str("loc", &e.location);
        w.field_name("kind", &e.kind);
        w.field_str("class", &e.class_name);
        w.field_str("clsid", &e.clsid);
        w.field_str("file", &e.file_name);
        w.field_u64("size", e.size);
        w.field_str("sha256", &e.sha256);
        w.end_object();
    }
    w.end_array();

    w.key("stats");
    w.begin_object();
    w.field_u64("parts", report.stats.parts_scanned);
    w.field_u64("xmlEvents", report.stats.xml_events);
    w.field_u64("inflated", report.stats.bytes_inflated);
    w.field_u64("nesting", report.stats.max_nesting);
    w.end_object();
    w.end_object();
    w.finish()
}

// ---------------------------------------------------------------------------
// Sanitize
// ---------------------------------------------------------------------------

/// Writes the drawing of a page as JSON: `{"w","h","index","pages","truncated","approx","p"}`
/// where `p` is a list of primitives, each an array that starts with a letter:
///
/// * `["P", fill, stroke, width, evenOdd, [ops]]` — a path; colours are `0xAARRGGBB` as numbers
///   (alpha `0` means none); `ops` is a flat list: `0 x y` move, `1 x y` line,
///   `2 x1 y1 x2 y2 x y` curve, `3` close;
/// * `["T", x, y, size, angle, width, style, color, "text"]`;
/// * `["I", a, b, c, d, e, f, "label"]`;
/// * `["S"]` save, `["R"]` restore, `["C", x, y, w, h]` clip.
#[must_use]
pub fn page_json(page: &PageModel) -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.key("w");
    w.f32(page.width);
    w.key("h");
    w.f32(page.height);
    w.field_u64("index", u64::from(page.index));
    w.field_u64("pages", u64::from(page.pages));
    w.field_bool("truncated", page.truncated);
    w.field_bool("approx", page.approximate);
    w.key("p");
    w.begin_array();
    for p in &page.prims {
        w.begin_array();
        match p {
            Prim::Path { ops, fill, stroke, width, even_odd } => {
                w.string("P");
                w.u64(u64::from(*fill));
                w.u64(u64::from(*stroke));
                w.f32(*width);
                w.u64(u64::from(*even_odd));
                w.begin_array();
                for v in ops {
                    w.f32(*v);
                }
                w.end_array();
            }
            Prim::Text { x, y, size, angle, width, style, color, text } => {
                w.string("T");
                for v in [x, y, size] {
                    w.f32(*v);
                }
                // The angle needs more precision than a coordinate.
                w.f32((*angle * 1000.0).round() / 1000.0);
                w.f32(*width);
                w.u64(u64::from(*style));
                w.u64(u64::from(*color));
                w.string(text);
            }
            Prim::Image { m, label, id } => {
                w.string("I");
                for v in m {
                    w.f32(*v);
                }
                w.string(label);
                w.u64(u64::from(*id));
            }
            Prim::Save => w.string("S"),
            Prim::Restore => w.string("R"),
            Prim::Clip { x, y, w: cw, h } => {
                w.string("C");
                for v in [x, y, cw, h] {
                    w.f32(*v);
                }
            }
        }
        w.end_array();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

/// Writes the description of a sterile copy (not the bytes) as JSON.
#[must_use]
pub fn sanitized_json(doc: &SanitizedDocument) -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_name("format", &doc.format);
    w.field_u64("size", u64_of(doc.bytes.len()));
    w.key("actions");
    w.begin_array();
    for a in &doc.actions {
        w.begin_object();
        w.field_name("kind", &a.kind);
        w.field_str("target", &a.target);
        w.end_object();
    }
    w.end_array();
    w.key("verification");
    w.begin_object();
    w.field_name("verdict", &doc.verification.verdict);
    w.field_str("sha256", &doc.verification.sha256);
    w.field_u64("findings", u64_of(doc.verification.findings.len()));
    w.end_object();
    w.end_object();
    w.finish()
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// A preview together with the tables that make paging by *item* cheap.
#[derive(Debug)]
pub struct PreviewIndex {
    preview: Preview,
    sections: Vec<SectionIndex>,
}

#[derive(Debug, Default)]
struct SectionIndex {
    /// `starts[b]` is the number of items before block `b`; one extra entry holds the total.
    starts: Vec<u64>,
    /// Column widths (characters) of the grid blocks, by block index.
    widths: HashMap<usize, Vec<u8>>,
}

fn items_of(block: &Block) -> u64 {
    match block {
        Block::Para(_) | Block::Image(_) | Block::Rule => 1,
        Block::Table(t) => u64_of(t.rows.len()),
        Block::Grid(g) => 1u64.saturating_add(u64_of(g.rows.len())),
    }
}

fn grid_widths(g: &Grid) -> Vec<u8> {
    let cols = usize::try_from(g.cols).unwrap_or(usize::MAX).min(16_384);
    let mut widths = vec![MIN_COL_CHARS; cols];
    for row in &g.rows {
        for cell in &row.cells {
            let Ok(col) = usize::try_from(cell.col) else { continue };
            if let Some(slot) = widths.get_mut(col) {
                let n = cell.text.chars().take(MAX_COL_CHARS).count();
                *slot = (*slot).max(n);
            }
        }
    }
    widths.into_iter().map(|n| u8::try_from(n.min(MAX_COL_CHARS)).unwrap_or(u8::MAX)).collect()
}

impl PreviewIndex {
    /// Indexes `preview`.
    #[must_use]
    pub fn new(preview: Preview) -> Self {
        let sections = preview
            .sections
            .iter()
            .map(|s| {
                let mut idx = SectionIndex::default();
                let mut total = 0u64;
                idx.starts.push(0);
                for (b, block) in s.blocks.iter().enumerate() {
                    if let Block::Grid(g) = block {
                        idx.widths.insert(b, grid_widths(g));
                    }
                    total = total.saturating_add(items_of(block));
                    idx.starts.push(total);
                }
                idx
            })
            .collect();
        Self { preview, sections }
    }

    /// The indexed preview.
    #[must_use]
    pub const fn preview(&self) -> &Preview {
        &self.preview
    }

    /// Number of items in `section`.
    #[must_use]
    pub fn item_count(&self, section: usize) -> u64 {
        self.sections.get(section).and_then(|s| s.starts.last().copied()).unwrap_or(0)
    }

    /// Section list, notices and totals as JSON.
    #[must_use]
    pub fn meta_json(&self) -> Vec<u8> {
        let p = &self.preview;
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_name("format", &p.format);
        w.field_name("family", &p.format.family());
        w.key("sections");
        w.begin_array();
        for (i, s) in p.sections.iter().enumerate() {
            w.begin_object();
            w.field_name("kind", &s.kind);
            w.field_str("name", &s.name);
            w.field_bool("hidden", s.hidden);
            w.field_u64("items", self.item_count(i));
            w.end_object();
        }
        w.end_array();
        w.key("notices");
        w.begin_array();
        for n in &p.notices {
            w.begin_object();
            w.field_name("kind", &n.kind);
            w.field_u64("count", n.count);
            w.field_str("detail", &n.detail);
            w.end_object();
        }
        w.end_array();
        w.key("stats");
        w.begin_object();
        w.field_u64("chars", p.stats.chars);
        w.field_u64("items", p.stats.items);
        w.field_bool("truncated", p.stats.truncated);
        w.end_object();
        w.end_object();
        w.finish()
    }

    /// Up to `count` items of `section` starting at item `from`, as a JSON array.
    /// `None` if the section does not exist; a range past the end gives `[]`.
    #[must_use]
    pub fn items_json(&self, section: usize, from: u64, count: usize) -> Option<Vec<u8>> {
        let sec = self.preview.sections.get(section)?;
        let idx = self.sections.get(section)?;
        let total = idx.starts.last().copied().unwrap_or(0);
        let end = from.saturating_add(u64_of(count)).min(total);
        let mut w = JsonWriter::new();
        w.begin_array();
        let mut pos = from;
        while pos < end {
            let b = idx.starts.partition_point(|&s| s <= pos).saturating_sub(1);
            let (Some(&start), Some(&next)) = (idx.starts.get(b), idx.starts.get(b + 1)) else {
                break;
            };
            let Some(block) = sec.blocks.get(b) else { break };
            let offset = pos - start;
            let take = (end - pos).min((next - start).saturating_sub(offset));
            if take == 0 {
                break;
            }
            write_items(&mut w, block, idx.widths.get(&b), offset, take);
            pos += take;
        }
        w.end_array();
        Some(w.finish())
    }

    /// Section accessor for tests and callers that need the typed value.
    #[must_use]
    pub fn section(&self, section: usize) -> Option<&Section> {
        self.preview.sections.get(section)
    }
}

fn write_items(
    w: &mut JsonWriter,
    block: &Block,
    widths: Option<&Vec<u8>>,
    offset: u64,
    take: u64,
) {
    let off = usize::try_from(offset).unwrap_or(usize::MAX);
    let n = usize::try_from(take).unwrap_or(usize::MAX);
    match block {
        Block::Para(p) => {
            w.begin_object();
            w.field_str("k", "p");
            para_fields(w, p);
            w.end_object();
        }
        Block::Image(i) => image(w, i),
        Block::Rule => {
            w.begin_object();
            w.field_str("k", "r");
            w.end_object();
        }
        Block::Table(t) => {
            let columns = t.rows.iter().map(row_span).max().unwrap_or(1).max(1);
            let last = t.rows.len().saturating_sub(1);
            for (i, row) in t.rows.iter().enumerate().skip(off).take(n) {
                table_row(w, row, columns, i == 0, i == last);
            }
        }
        Block::Grid(g) => {
            let mut skip = off;
            let mut left = n;
            if skip == 0 {
                grid_header(w, g, widths);
                left -= 1;
            } else {
                skip -= 1;
            }
            for row in g.rows.iter().skip(skip).take(left) {
                w.begin_object();
                w.field_str("k", "gr");
                w.field_u64("i", u64::from(row.index));
                w.field_bool("h", row.hidden);
                w.key("c");
                w.begin_array();
                for cell in &row.cells {
                    w.begin_array();
                    w.u64(u64::from(cell.col));
                    w.u64(cell_kind_code(cell.kind));
                    w.string(&cell.text);
                    if !cell.href.is_empty() {
                        w.string(&cell.href);
                    }
                    w.end_array();
                }
                w.end_array();
                w.end_object();
            }
        }
    }
}

const fn cell_kind_code(kind: CellKind) -> u64 {
    match kind {
        CellKind::Text => 0,
        CellKind::Number => 1,
        CellKind::Date => 2,
        CellKind::Bool => 3,
        CellKind::Error => 4,
        CellKind::Formula => 5,
        CellKind::Calculated => 6,
    }
}

const fn align_code(a: Align) -> u64 {
    match a {
        Align::Start => 0,
        Align::Center => 1,
        Align::End => 2,
        Align::Justify => 3,
    }
}

fn row_span(row: &Row) -> usize {
    row.cells.iter().map(|c| usize::from(c.span.max(1))).sum()
}

fn grid_header(w: &mut JsonWriter, g: &Grid, widths: Option<&Vec<u8>>) {
    w.begin_object();
    w.field_str("k", "gh");
    w.field_u64("cols", u64::from(g.cols));
    w.key("w");
    w.begin_array();
    for width in widths.into_iter().flatten() {
        w.u64(u64::from(*width));
    }
    w.end_array();
    w.field_u64("rows", u64_of(g.rows.len()));
    w.field_bool("trunc", g.truncated);
    w.end_object();
}

fn image(w: &mut JsonWriter, i: &ImageInfo) {
    w.begin_object();
    w.field_str("k", "i");
    w.field_str("alt", &i.alt);
    w.field_str("media", &i.media);
    w.field_u64("bytes", i.bytes);
    w.field_bool("ext", i.external);
    if i.id != IMAGE_NONE {
        w.field_u64("id", u64::from(i.id));
    }
    w.end_object();
}

fn table_row(w: &mut JsonWriter, row: &Row, columns: usize, first: bool, last: bool) {
    w.begin_object();
    w.field_str("k", "tr");
    w.field_u64("n", u64_of(columns));
    w.field_bool("first", first);
    w.field_bool("last", last);
    w.key("cells");
    w.begin_array();
    for cell in &row.cells {
        w.begin_object();
        w.field_u64("c", u64::from(cell.span.max(1)));
        w.field_bool("h", cell.header);
        w.key("p");
        w.begin_array();
        for p in &cell.paras {
            w.begin_object();
            para_fields(w, p);
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.end_object();
}

fn para_fields(w: &mut JsonWriter, p: &Paragraph) {
    if p.style != arca_preview::model::ParaStyle::Body {
        w.field_name("s", &p.style);
    }
    if p.align != Align::Start {
        w.field_u64("a", align_code(p.align));
    }
    if p.level != 0 {
        w.field_u64("l", u64::from(p.level));
    }
    if !p.marker.is_empty() {
        w.field_str("m", &p.marker);
    }
    w.key("r");
    w.begin_array();
    for r in &p.runs {
        run(w, r);
    }
    w.end_array();
}

fn run(w: &mut JsonWriter, r: &Run) {
    w.begin_array();
    w.string(&r.text);
    w.u64(u64::from(r.style));
    if !r.href.is_empty() {
        w.string(&r.href);
    }
    w.end_array();
}

// ---------------------------------------------------------------------------
// About, passport, licences
// ---------------------------------------------------------------------------

/// Language code used by the apps: `0` Russian, anything else English.
#[must_use]
pub const fn lang_from_code(code: u32) -> Lang {
    if code == 0 { Lang::Ru } else { Lang::En }
}

fn card(w: &mut JsonWriter, c: &CreatorCard) {
    w.begin_object();
    w.field_str(
        "role",
        match c.role {
            CardRole::UpstreamCreators => "Upstream",
            CardRole::ForkMaintainers => "Fork",
        },
    );
    w.field_str("title", &c.title);
    w.field_str("project", &c.project);
    w.key("people");
    w.begin_array();
    for p in &c.people {
        w.begin_object();
        w.field_str("name", &p.name);
        w.key("url");
        match &p.url {
            Some(u) => w.string(u),
            None => w.null(),
        }
        w.end_object();
    }
    w.end_array();
    w.field_str("supportLabel", c.support_label);
    w.key("support");
    w.begin_array();
    for l in &c.support_links {
        w.begin_object();
        w.field_str("label", &l.label);
        w.field_str("url", &l.url);
        w.end_object();
    }
    w.end_array();
    for (key, value) in
        [("homepage", &c.homepage), ("source", &c.source), ("modifications", &c.modifications)]
    {
        w.key(key);
        match value {
            Some(v) => w.string(v),
            None => w.null(),
        }
    }
    w.end_object();
}

fn screen(w: &mut JsonWriter, s: &PatronageScreen) {
    w.begin_object();
    w.field_str("lang", if matches!(s.lang, Lang::Ru) { "ru" } else { "en" });
    w.key("cards");
    w.begin_array();
    for c in &s.cards {
        card(w, c);
    }
    w.end_array();
    w.field_str("legal", s.legal_notice);
    w.key("modified");
    match &s.modified_version_notice {
        Some(m) => w.string(m),
        None => w.null(),
    }
    w.end_object();
}

/// The "About & Support" screen (cards in required order, upstream first).
#[must_use]
pub fn about_json(lang: Lang) -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("appName", &arca_attribution::app_name());
    w.field_str("fingerprint", &arca_attribution::upstream_fingerprint().to_string());
    w.key("screen");
    screen(&mut w, &arca_attribution::patronage_screen(lang));
    w.end_object();
    w.finish()
}

/// The Patron Passport for the stored `record` (36 bytes, or anything else for "start fresh").
#[must_use]
pub fn passport_json(lang: Lang, record: &[u8], today: u32) -> Vec<u8> {
    let stats = arca_attribution::PatronStats::decode(record)
        .unwrap_or_else(|_| arca_attribution::PatronStats::starting(today));
    let passport: PatronPassport = arca_attribution::patron_passport(lang, &stats, today);
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("title", passport.title);
    w.field_str("body", &passport.body);
    w.field_str("cta", passport.call_to_action);
    w.field_u64("documents", stats.documents_opened);
    w.field_u64("trackers", stats.trackers_blocked);
    w.field_u64("active", stats.active_content_neutralized);
    w.field_u64("days", u64::from(stats.days_in_use(today)));
    w.key("screen");
    screen(&mut w, &passport.support);
    w.end_object();
    w.finish()
}

/// The stored record after one more document (a fresh record if `record` is not valid).
#[must_use]
pub fn passport_update(record: &[u8], today: u32, trackers: u32, active: u32) -> Vec<u8> {
    let mut stats = arca_attribution::PatronStats::decode(record)
        .unwrap_or_else(|_| arca_attribution::PatronStats::starting(today));
    stats.record_document(trackers, active);
    stats.encode().to_vec()
}

/// A licence text: `0` the GNU AGPL v3, `1` the additional terms (Russian, binding),
/// `2` their English translation.
#[must_use]
pub const fn license_text(which: u32) -> Option<&'static str> {
    match which {
        0 => Some(arca_attribution::LICENSE_TEXT),
        1 => Some(arca_attribution::ADDITIONAL_TERMS_TEXT),
        2 => Some(arca_attribution::ADDITIONAL_TERMS_TRANSLATION_EN),
        _ => None,
    }
}

/// Release blockers of this build as JSON strings (empty for a releasable build).
#[must_use]
pub fn release_blockers_json() -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    match arca_attribution::release_readiness() {
        Ok(()) => {
            w.field_bool("ready", true);
            w.key("blockers");
            w.begin_array();
            w.end_array();
        }
        Err(blockers) => {
            w.field_bool("ready", false);
            w.key("blockers");
            w.begin_array();
            for b in blockers {
                w.string(&b.to_string());
            }
            w.end_array();
        }
    }
    w.end_object();
    w.finish()
}

// ---------------------------------------------------------------------------
// Pictures
// ---------------------------------------------------------------------------

/// Colours multiplied by the alpha, as Android's `Bitmap` keeps them.
#[must_use]
pub fn premultiplied(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.chunks_exact_mut(4) {
        if let [r, g, b, a] = px
            && *a != 255
        {
            let a = u32::from(*a);
            for c in [r, g, b] {
                *c = u8::try_from((u32::from(*c) * a + 127) / 255).unwrap_or(255);
            }
        }
    }
    rgba
}

/// Facts about a picture as JSON (the pixels travel separately):
///
/// ```text
/// {"format":"Jpeg","width":2048,"height":1536,"srcWidth":4000,"srcHeight":3000,"bits":8,
///  "flags":260,"frames":1,"trailing":0,"metadataBytes":1234,
///  "exif":{"make":"Acme","model":"…","software":"","datetime":"…","artist":"","copyright":"",
///          "owner":"","orientation":1,"serial":false,"makerNote":false,"thumbnail":true,
///          "gps":true,"position":"55.75833, 37.61730"}}
/// ```
///
/// `flags` are the bits of `arca_image::flag`. Everything textual came from the document and is
/// shown by the app as untrusted text.
#[must_use]
pub fn image_json(img: &arca_image::Image) -> Vec<u8> {
    let f = &img.facts;
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_name("format", &f.format);
    w.field_u64("width", u64::from(img.width));
    w.field_u64("height", u64::from(img.height));
    w.field_u64("srcWidth", u64::from(f.width));
    w.field_u64("srcHeight", u64::from(f.height));
    w.field_u64("bits", u64::from(f.bits));
    w.field_u64("flags", u64::from(f.flags));
    w.field_u64("frames", u64::from(f.frames));
    w.field_u64("trailing", f.trailing);
    w.field_u64("metadataBytes", f.metadata_bytes);
    w.key("exif");
    match &f.exif {
        None => w.null(),
        Some(e) => {
            w.begin_object();
            w.field_str("make", &e.make);
            w.field_str("model", &e.model);
            w.field_str("software", &e.software);
            w.field_str("datetime", &e.datetime);
            w.field_str("artist", &e.artist);
            w.field_str("copyright", &e.copyright);
            w.field_str("owner", &e.owner);
            w.field_u64("orientation", u64::from(e.orientation));
            w.field_bool("serial", e.serial);
            w.field_bool("makerNote", e.maker_note);
            w.field_bool("thumbnail", e.thumbnail);
            w.field_bool("gps", e.gps);
            if e.has_position {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "millionths of a degree: well within f64"
                )]
                let position =
                    format!("{:.5}, {:.5}", e.lat_micro as f64 / 1e6, e.lon_micro as f64 / 1e6);
                w.field_str("position", &position);
            }
            w.end_object();
        }
    }
    w.end_object();
    w.finish()
}
