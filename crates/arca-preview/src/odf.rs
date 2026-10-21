//! OpenDocument (`.odt`, `.ods`, `.odp`, `.odg` and templates).
//!
//! Names are compared by local part, the same policy the scanner uses, so what the
//! scanner inspected is what is shown. Spreadsheet cells show the text the document
//! cached for them; formulas are never evaluated. Hidden text, hidden sheets and
//! slides, deleted and inserted text and annotations are shown and labelled.

use std::collections::{BTreeMap, BTreeSet};

use crate::PreviewError;
use crate::listfmt;
use crate::model::{
    Align, Block, Cell, CellKind, ImageInfo, NoticeKind, ParaStyle, Paragraph, Row, SectionKind,
    Table, style,
};
use crate::opc::Package;
use crate::sink::{GridBuilder, ParaBuilder, Sink};
use crate::xml::{self, Flow, Handler, Tag, XmlError, unescape};

/// Largest number of copies made of a repeated row or cell.
const MAX_REPEAT: u32 = 64;
/// Largest list nesting that is tracked.
const MAX_LIST_DEPTH: usize = 12;
/// Longest style inheritance chain that is followed.
const MAX_STYLE_CHAIN: usize = 8;
/// Largest number of blocks buffered for one slide.
const MAX_PAGE_BLOCKS: usize = 20_000;

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fam {
    Text,
    Para,
    Table,
    Page,
    Other,
}

impl Fam {
    fn of(family: &str) -> Self {
        match family {
            "text" => Self::Text,
            "paragraph" => Self::Para,
            "table" => Self::Table,
            "drawing-page" => Self::Page,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct StyleInfo {
    parent: String,
    display_name: String,
    /// Style bits turned on / off by this style.
    set: u16,
    clear: u16,
    align: Option<Align>,
    list_style: String,
    table_hidden: bool,
    page_hidden: bool,
}

#[derive(Debug, Clone, Default)]
struct ListLevel {
    number: bool,
    format: String,
    prefix: String,
    suffix: String,
    bullet: String,
    start: u32,
}

#[derive(Debug, Default)]
struct Styles {
    by_name: BTreeMap<(Fam, String), StyleInfo>,
    lists: BTreeMap<String, Vec<ListLevel>>,
}

/// What a style (and its ancestors) amounts to.
#[derive(Debug, Clone, Default)]
struct Resolved {
    flags: u16,
    /// Bits the style turns off (as opposed to merely not setting).
    cleared: u16,
    align: Option<Align>,
    list_style: String,
    role: Option<ParaStyle>,
    table_hidden: bool,
    page_hidden: bool,
}

impl Styles {
    fn resolve(&self, fam: Fam, name: &str) -> Resolved {
        let mut chain: Vec<(&str, &StyleInfo)> = Vec::new();
        let mut cur = name;
        while !cur.is_empty() && chain.len() < MAX_STYLE_CHAIN {
            let Some((key, info)) = self.by_name.get_key_value(&(fam, cur.to_owned())) else {
                break;
            };
            chain.push((key.1.as_str(), info));
            cur = info.parent.as_str();
        }
        let mut out = Resolved::default();
        for (_, info) in chain.iter().rev() {
            out.flags = (out.flags | info.set) & !info.clear;
            out.cleared = (out.cleared | info.clear) & !info.set;
        }
        for (n, info) in &chain {
            if out.align.is_none() {
                out.align = info.align;
            }
            if out.list_style.is_empty() && !info.list_style.is_empty() {
                out.list_style.clone_from(&info.list_style);
            }
            if out.role.is_none() {
                out.role = role_of(n).or_else(|| role_of(&info.display_name));
            }
            out.table_hidden |= info.table_hidden;
            out.page_hidden |= info.page_hidden;
        }
        out
    }

    fn marker(&self, list_style: &str, depth: usize, counter: u32) -> String {
        let level = self
            .lists
            .get(list_style)
            .and_then(|l| l.get(depth.saturating_sub(1)))
            .filter(|l| l.number || !l.bullet.is_empty());
        match level {
            Some(l) if l.number => {
                let fmt = if l.format.is_empty() { "1" } else { l.format.as_str() };
                let n = counter.saturating_add(l.start.max(1) - 1);
                format!("{}{}{}", l.prefix, listfmt::counter(fmt, n), l.suffix)
            }
            Some(l) => l.bullet.clone(),
            None => "•".to_owned(),
        }
    }
}

/// Paragraph role implied by a well-known style name.
fn role_of(name: &str) -> Option<ParaStyle> {
    let n = name.replace("_20_", " ").to_lowercase();
    let n = n.trim();
    Some(match n {
        "title" => ParaStyle::Title,
        "subtitle" => ParaStyle::Subtitle,
        "quotations" | "quotation" | "quote" | "block quotation" => ParaStyle::Quote,
        "preformatted text" | "source text" | "code" | "monospace" => ParaStyle::Code,
        "footnote" => ParaStyle::Footnote,
        "header" | "footer" | "header and footer" => ParaStyle::HeaderFooter,
        _ => return None,
    })
}

fn is_off(v: &str) -> bool {
    matches!(v.trim(), "none" | "false" | "normal" | "no")
}

#[derive(Default)]
struct StyleHandler {
    styles: Styles,
    cur: Option<(Fam, String, StyleInfo)>,
    cur_list: Option<String>,
    /// Stop once the automatic styles of the content part are read.
    until_automatic_end: bool,
    finished: bool,
}

impl Handler for StyleHandler {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "style" => {
                let name = tag.attr("name").unwrap_or_default().into_owned();
                let fam = Fam::of(&tag.attr("family").unwrap_or_default());
                let info = StyleInfo {
                    parent: tag.attr("parent-style-name").unwrap_or_default().into_owned(),
                    display_name: tag.attr("display-name").unwrap_or_default().into_owned(),
                    list_style: tag.attr("list-style-name").unwrap_or_default().into_owned(),
                    ..StyleInfo::default()
                };
                self.cur = Some((fam, name, info));
            }
            "text-properties" => {
                if let Some((_, _, info)) = self.cur.as_mut() {
                    text_props(tag, info);
                }
            }
            "paragraph-properties" => {
                if let Some((_, _, info)) = self.cur.as_mut() {
                    info.align = match tag.attr("text-align").as_deref() {
                        Some("center") => Some(Align::Center),
                        Some("end" | "right") => Some(Align::End),
                        Some("justify") => Some(Align::Justify),
                        Some("start" | "left") => Some(Align::Start),
                        _ => None,
                    };
                }
            }
            "table-properties" => {
                if let Some((_, _, info)) = self.cur.as_mut() {
                    info.table_hidden = tag.attr("display").is_some_and(|v| is_off(&v));
                }
            }
            "drawing-page-properties" => {
                if let Some((_, _, info)) = self.cur.as_mut() {
                    info.page_hidden = tag.attr("visibility").is_some_and(|v| v == "hidden");
                }
            }
            "list-style" => {
                self.cur_list = tag.attr("name").map(std::borrow::Cow::into_owned);
            }
            "list-level-style-number" | "list-level-style-bullet" | "list-level-style-image" => {
                if let Some(name) = self.cur_list.clone() {
                    let level = tag
                        .attr("level")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, MAX_LIST_DEPTH);
                    let number = tag.local() == "list-level-style-number";
                    let bullet = tag.attr("bullet-char").unwrap_or_default();
                    // Symbol-font code points render as boxes; use a neutral bullet.
                    let bullet = match bullet.chars().next() {
                        Some(c) if !('\u{E000}'..='\u{F8FF}').contains(&c) => c.to_string(),
                        _ => "•".to_owned(),
                    };
                    let lv = ListLevel {
                        number,
                        format: tag.attr("num-format").unwrap_or_default().into_owned(),
                        prefix: tag.attr("num-prefix").unwrap_or_default().into_owned(),
                        suffix: tag.attr("num-suffix").unwrap_or_default().into_owned(),
                        bullet,
                        start: tag.attr("start-value").and_then(|v| v.parse().ok()).unwrap_or(1),
                    };
                    let levels = self.styles.lists.entry(name).or_default();
                    while levels.len() < level {
                        levels.push(ListLevel::default());
                    }
                    if let Some(slot) = levels.get_mut(level - 1) {
                        *slot = lv;
                    }
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "style" => {
                if let Some((fam, name, info)) = self.cur.take() {
                    self.styles.by_name.insert((fam, name), info);
                }
            }
            "list-style" => self.cur_list = None,
            "automatic-styles" if self.until_automatic_end => self.finished = true,
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }

    fn done(&self) -> bool {
        self.finished
    }
}

fn text_props(tag: &Tag<'_>, info: &mut StyleInfo) {
    let mut toggle = |on: bool, bit: u16| {
        if on {
            info.set |= bit;
            info.clear &= !bit;
        } else {
            info.clear |= bit;
            info.set &= !bit;
        }
    };
    if let Some(w) = tag.attr("font-weight") {
        let heavy = w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600);
        toggle(heavy, style::BOLD);
    }
    if let Some(v) = tag.attr("font-style") {
        toggle(!is_off(&v), style::ITALIC);
    }
    if let Some(v) = tag.attr("text-underline-style") {
        toggle(!is_off(&v), style::UNDERLINE);
    }
    if let Some(v) = tag.attr("text-line-through-style") {
        toggle(!is_off(&v), style::STRIKE);
    }
    if let Some(v) = tag.attr("display") {
        toggle(v == "none", style::HIDDEN);
    }
    if let Some(pos) = tag.attr("text-position") {
        let first = pos.split_whitespace().next().unwrap_or("");
        let up =
            first == "super" || first.trim_end_matches('%').parse::<i32>().is_ok_and(|n| n > 0);
        let down =
            first == "sub" || first.trim_end_matches('%').parse::<i32>().is_ok_and(|n| n < 0);
        toggle(up, style::SUPER);
        toggle(down, style::SUB);
    }
    let font = tag.attr("font-name").or_else(|| tag.attr("font-family"));
    if let Some(f) = font {
        let f = f.to_ascii_lowercase();
        let mono = ["courier", "consolas", "mono", "lucida console", "liberation mono"]
            .iter()
            .any(|m| f.contains(m));
        if mono {
            info.set |= style::MONO;
        }
    }
}

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Not inside a document body yet.
    None,
    /// Only headers and footers are collected (`styles.xml`).
    Headers,
    Text,
    Sheet,
    Slides,
}

struct ParaCtx {
    b: ParaBuilder,
    style: ParaStyle,
    align: Align,
    marker: String,
    level: u8,
    base: u16,
    spans: Vec<(u16, u16)>,
    hrefs: Vec<String>,
    /// A collapsed run of white space waiting for the next visible character, with the
    /// formatting in force where it was written.
    pending_space: Option<(u16, String)>,
    pending: Vec<Block>,
    /// The paragraph is read but not kept.
    skip: bool,
}

impl ParaCtx {
    fn flags(&self, extra: u16) -> u16 {
        let mut f = self.base;
        for (set, clear) in &self.spans {
            f = (f | set) & !clear;
        }
        f | extra
    }
}

#[derive(Default)]
struct TblCtx {
    rows: Vec<Row>,
    cells: Vec<Cell>,
    cell: Option<Cell>,
    covered: bool,
    row_rep: u32,
    col_rep: u32,
}

struct ListCtx {
    style: String,
    counter: u32,
}

struct CellCtx {
    rep: u32,
    vtype: String,
    value: String,
    formula: bool,
    error: bool,
    text: String,
    has_text: bool,
    /// The first outside address a link inside the cell leads to.
    href: String,
}

struct SheetCtx {
    name: String,
    hidden: bool,
    grid: GridBuilder,
    row: u32,
    col: u32,
    row_rep: u32,
    row_hidden: bool,
    row_cells: Vec<(u32, CellKind, String, String)>,
    cell: Option<CellCtx>,
    full: bool,
}

struct PageCtx {
    name: String,
    hidden: bool,
    title: String,
    blocks: Vec<Block>,
}

#[derive(Default)]
struct FrameCtx {
    /// Number of open paragraphs around the frame.
    para_depth: usize,
    class: String,
    name: String,
    title: String,
    desc: String,
    image: Option<ImageInfo>,
    skip: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    None,
    Title,
    Desc,
    Author,
}

#[derive(Default)]
struct Annotation {
    /// Number of paragraphs that were already open around the annotation.
    base: usize,
    author: String,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Insertion,
    Deletion,
    Other,
}

struct Odf<'a, 'p> {
    sink: &'a mut Sink<'p>,
    styles: &'a Styles,
    pkg: &'a Package<'a>,
    mode: Mode,
    slide_kind: SectionKind,
    stop: bool,

    body: Option<crate::sink::SectionId>,
    paras: Vec<ParaCtx>,
    lists: Vec<ListCtx>,
    item_marker_pending: Vec<bool>,
    tables: Vec<TblCtx>,
    header_rows: u32,
    sheet: Option<SheetCtx>,
    table_depth: u32,
    page: Option<PageCtx>,
    frames: Vec<FrameCtx>,
    in_notes: bool,
    capture: Capture,
    annotation: Option<Annotation>,
    note_depth: u32,
    cite_depth: u32,
    cite: String,
    note_marker: String,
    hidden_depth: u32,
    section_hidden: Vec<bool>,
    hf_depth: u32,
    headers: Vec<Paragraph>,
    seen_headers: BTreeSet<String>,
    ruby_text: u32,

    // Tracked changes.
    changes: BTreeMap<String, ChangeKind>,
    deleted: BTreeMap<String, String>,
    cur_change: Option<String>,
    capture_deleted: Option<String>,
    open_insertions: Vec<String>,

    // Findings.
    formulas: u64,
    unsupported: bool,
    comments: Vec<(String, String, String, String)>,
    sheets_seen: u32,
}

fn rep_attr(tag: &Tag<'_>, name: &str) -> u32 {
    tag.attr(name).and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).max(1)
}

fn col_name(mut c: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(char::from(b'A'.saturating_add(u8::try_from(c % 26).unwrap_or(0))));
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.iter().rev().collect()
}

impl<'a, 'p> Odf<'a, 'p> {
    fn new(sink: &'a mut Sink<'p>, styles: &'a Styles, pkg: &'a Package<'a>, mode: Mode) -> Self {
        Self {
            sink,
            styles,
            pkg,
            mode,
            slide_kind: SectionKind::Slide,
            stop: false,
            body: None,
            paras: Vec::new(),
            lists: Vec::new(),
            item_marker_pending: Vec::new(),
            tables: Vec::new(),
            header_rows: 0,
            sheet: None,
            table_depth: 0,
            page: None,
            frames: Vec::new(),
            in_notes: false,
            capture: Capture::None,
            annotation: None,
            note_depth: 0,
            cite_depth: 0,
            cite: String::new(),
            note_marker: String::new(),
            hidden_depth: 0,
            section_hidden: Vec::new(),
            hf_depth: 0,
            headers: Vec::new(),
            seen_headers: BTreeSet::new(),
            ruby_text: 0,
            changes: BTreeMap::new(),
            deleted: BTreeMap::new(),
            cur_change: None,
            capture_deleted: None,
            open_insertions: Vec::new(),
            formulas: 0,
            unsupported: false,
            comments: Vec::new(),
            sheets_seen: 0,
        }
    }

    fn accepting(&self) -> bool {
        let frame_skip = self.frames.iter().any(|f| f.skip);
        match self.mode {
            Mode::None => false,
            Mode::Headers => self.hf_depth > 0,
            Mode::Text | Mode::Slides => !frame_skip,
            Mode::Sheet => self.sheet.as_ref().is_some_and(|s| s.cell.is_some()),
        }
    }

    fn extra_flags(&self) -> u16 {
        let mut f = 0;
        if self.hidden_depth > 0 {
            f |= style::HIDDEN;
        }
        if !self.open_insertions.is_empty() {
            f |= style::INSERTED;
        }
        if self.cite_depth > 0 {
            f |= style::SUPER;
        }
        f
    }

    // -- paragraphs ---------------------------------------------------------

    fn begin_para(&mut self, tag: &Tag<'_>, heading: bool) {
        if !self.accepting() {
            // Keep the stack balanced for the matching end tag.
            self.paras.push(ParaCtx::skipped());
            return;
        }
        let style_name = tag.attr("style-name").unwrap_or_default();
        let r = self.styles.resolve(Fam::Para, &style_name);
        let mut ps = if heading {
            let level = tag.attr("outline-level").and_then(|v| v.parse::<u8>().ok()).unwrap_or(1);
            ParaStyle::heading(level.clamp(1, 6))
        } else {
            r.role.unwrap_or(ParaStyle::Body)
        };
        if let Some(frame) = self.frames.iter().rev().find(|f| !f.class.is_empty()) {
            ps = match frame.class.as_str() {
                "title" => ParaStyle::Title,
                "subtitle" => ParaStyle::Subtitle,
                "notes" => ParaStyle::Note,
                "header" | "footer" => ParaStyle::HeaderFooter,
                _ => ps,
            };
        }
        if self.in_notes && ps == ParaStyle::Body {
            ps = ParaStyle::Note;
        }
        if self.hf_depth > 0 {
            ps = ParaStyle::HeaderFooter;
        }
        if self.annotation.is_none() && self.note_depth > 0 {
            ps = ParaStyle::Footnote;
        }
        let mut ctx = ParaCtx {
            b: ParaBuilder::new(),
            style: ps,
            align: r.align.unwrap_or(Align::Start),
            marker: String::new(),
            level: 0,
            base: r.flags,
            spans: Vec::new(),
            hrefs: Vec::new(),
            pending_space: None,
            pending: Vec::new(),
            skip: false,
        };
        if self.note_depth > 0 && self.annotation.is_none() {
            ctx.marker = std::mem::take(&mut self.note_marker);
        }
        if !self.lists.is_empty() && self.note_depth == 0 && self.annotation.is_none() {
            let depth = self.lists.len();
            ctx.level = u8::try_from(depth).unwrap_or(u8::MAX);
            let first = self.item_marker_pending.last_mut().is_some_and(std::mem::take);
            if first && let Some(l) = self.lists.last() {
                ctx.marker = self.styles.marker(&l.style, depth, l.counter);
            }
        }
        // Page titles are taken from the first title paragraph of a slide.
        self.paras.push(ctx);
    }

    fn end_para(&mut self) {
        let Some(ctx) = self.paras.pop() else { return };
        if ctx.skip {
            return;
        }
        let ParaCtx { mut b, style: ps, align, marker, level, pending, .. } = ctx;
        let mut p = b.take(ps);
        p.align = align;
        p.marker = marker;
        p.level = level;
        if !(p.runs.is_empty() && p.marker.is_empty()) {
            self.route(Block::Para(p));
        }
        for block in pending {
            self.route(block);
        }
    }

    /// Sends a finished block to wherever the current context wants it.
    fn route(&mut self, block: Block) {
        // 1. Text of deleted content is collected, not shown in place.
        if let Some(buf) = self.capture_deleted.as_mut() {
            if let Block::Para(p) = &block {
                if !buf.is_empty() {
                    buf.push(' ');
                }
                buf.push_str(&p.text());
            }
            return;
        }
        // 2. Annotation text is collected.
        if let Some(a) = self.annotation.as_mut() {
            if let Block::Para(p) = &block {
                if !a.text.is_empty() {
                    a.text.push('\n');
                }
                a.text.push_str(&p.text());
            }
            return;
        }
        // 3. Spreadsheet cells collect their paragraphs.
        if self.mode == Mode::Sheet {
            match (&block, self.sheet.as_mut().and_then(|s| s.cell.as_mut())) {
                (Block::Para(p), Some(c)) => {
                    if c.has_text {
                        c.text.push('\n');
                    }
                    c.text.push_str(&p.text());
                    c.has_text = true;
                }
                (Block::Image(_), _) => self.unsupported = true,
                _ => {}
            }
            return;
        }
        // 4. Headers and footers.
        if self.mode == Mode::Headers {
            if let Block::Para(p) = block {
                let key = p.text();
                if !key.trim().is_empty() && self.seen_headers.insert(key) {
                    self.headers.push(p);
                }
            }
            return;
        }
        // 5. Nested inside another paragraph: shown after it.
        if let Some(outer) = self.paras.last_mut() {
            outer.pending.push(block);
            return;
        }
        // 6. Inside a table cell.
        if let Some(cell) = self.tables.last_mut().and_then(|t| t.cell.as_mut()) {
            match block {
                Block::Para(p) => cell.paras.push(p),
                Block::Image(_) => self.sink.count(NoticeKind::ImagesNotShown, "", 1),
                Block::Table(t) => {
                    for row in t.rows {
                        for c in row.cells {
                            cell.paras.extend(c.paras);
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        self.emit(block);
    }

    fn emit(&mut self, block: Block) {
        match self.mode {
            Mode::Text => {
                if let Some(sec) = self.body {
                    let ok = match block {
                        Block::Image(i) => self.sink.image(sec, i),
                        other => self.sink.push(sec, other),
                    };
                    if !ok {
                        self.stop = true;
                    }
                }
            }
            Mode::Slides => {
                if let Some(pg) = self.page.as_mut() {
                    if pg.title.is_empty()
                        && let Block::Para(p) = &block
                        && p.style == ParaStyle::Title
                    {
                        pg.title = p.text().chars().take(80).collect();
                    }
                    if pg.blocks.len() < MAX_PAGE_BLOCKS {
                        pg.blocks.push(block);
                    } else {
                        self.sink.truncated("blocks");
                    }
                }
            }
            _ => {}
        }
    }

    fn add_text(&mut self, text: &str) {
        let extra = self.extra_flags();
        let ruby = self.ruby_text > 0;
        let Some(ctx) = self.paras.last_mut() else { return };
        if ctx.is_skipped() {
            return;
        }
        let st = ctx.flags(extra);
        let href = ctx.hrefs.last().cloned().unwrap_or_default();
        let mut buf = String::new();
        // White space seen since the last visible character of this call.
        let mut space = false;
        for c in text.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r') {
                // White space collapses; leading and trailing space is dropped.
                space = true;
            } else {
                if let Some((sst, shref)) = ctx.pending_space.take() {
                    // A space carried over from before this run keeps its own formatting.
                    ctx.b.text(" ", sst, &shref);
                } else if space && (!buf.is_empty() || !ctx.b.is_empty()) {
                    buf.push(' ');
                }
                space = false;
                buf.push(c);
            }
        }
        if !buf.is_empty() {
            let text = if ruby { format!("({buf})") } else { buf };
            ctx.b.text(&text, st, &href);
        }
        if space && ctx.pending_space.is_none() && !ctx.b.is_empty() {
            ctx.pending_space = Some((st, href));
        }
    }

    /// Text that is taken literally (`text:s`, `text:tab`, line breaks).
    fn add_literal(&mut self, text: &str) {
        let extra = self.extra_flags();
        let Some(ctx) = self.paras.last_mut() else { return };
        if ctx.is_skipped() {
            return;
        }
        let st = ctx.flags(extra);
        let href = ctx.hrefs.last().cloned().unwrap_or_default();
        if let Some((sst, shref)) = ctx.pending_space.take() {
            ctx.b.text(" ", sst, &shref);
        }
        ctx.b.text(text, st, &href);
    }

    fn add_flagged(&mut self, text: &str, bits: u16) {
        let extra = self.extra_flags() | bits;
        let Some(ctx) = self.paras.last_mut() else { return };
        if ctx.is_skipped() {
            return;
        }
        let st = ctx.flags(extra);
        if let Some((sst, shref)) = ctx.pending_space.take() {
            ctx.b.text(" ", sst, &shref);
        }
        ctx.b.text(text, st, "");
    }

    // -- tables -------------------------------------------------------------

    fn start_table(&mut self, tag: &Tag<'_>) -> Flow {
        self.table_depth += 1;
        match self.mode {
            Mode::Sheet => {
                if self.table_depth > 1 || self.sheet.is_some() {
                    self.unsupported = true;
                    return Flow::SkipChildren;
                }
                let name = tag.attr("name").unwrap_or_default().into_owned();
                let style_name = tag.attr("style-name").unwrap_or_default();
                let hidden = self.styles.resolve(Fam::Table, &style_name).table_hidden;
                self.sheets_seen += 1;
                self.sheet = Some(SheetCtx {
                    name,
                    hidden,
                    grid: GridBuilder::new(self.sink.policy, self.sink.items_left()),
                    row: 1,
                    col: 0,
                    row_rep: 1,
                    row_hidden: false,
                    row_cells: Vec::new(),
                    cell: None,
                    full: false,
                });
            }
            Mode::Text | Mode::Slides => self.tables.push(TblCtx::default()),
            _ => return Flow::SkipChildren,
        }
        Flow::Continue
    }

    fn end_table(&mut self) {
        self.table_depth = self.table_depth.saturating_sub(1);
        match self.mode {
            Mode::Sheet => {
                if self.table_depth == 0
                    && let Some(sh) = self.sheet.take()
                {
                    let grid = sh.grid.finish();
                    let truncated = grid.truncated;
                    if let Some(sec) =
                        self.sink.open_section(SectionKind::Sheet, &sh.name, sh.hidden)
                    {
                        if truncated {
                            self.sink.truncated("grid");
                        }
                        if !self.sink.push(sec, Block::Grid(grid)) {
                            self.stop = true;
                        }
                    } else {
                        self.stop = true;
                    }
                }
            }
            Mode::Text | Mode::Slides => {
                let Some(t) = self.tables.pop() else { return };
                if !t.rows.is_empty() {
                    self.route(Block::Table(Table { rows: t.rows }));
                }
            }
            _ => {}
        }
    }

    fn start_row(&mut self, tag: &Tag<'_>) {
        let rep = rep_attr(tag, "number-rows-repeated");
        if let Some(sh) = self.sheet.as_mut() {
            sh.row_rep = rep;
            sh.col = 0;
            sh.row_cells.clear();
            sh.row_hidden =
                tag.attr("visibility").is_some_and(|v| v == "collapse" || v == "filter");
        } else if let Some(t) = self.tables.last_mut() {
            t.row_rep = rep;
            t.cells.clear();
        }
    }

    fn end_row(&mut self) {
        if let Some(sh) = self.sheet.as_mut() {
            let rep = sh.row_rep.min(MAX_REPEAT);
            if !sh.row_cells.is_empty() && !sh.full {
                let cells = std::mem::take(&mut sh.row_cells);
                for k in 0..rep {
                    if !sh.grid.row(sh.row.saturating_add(k), sh.row_hidden) {
                        sh.full = true;
                        break;
                    }
                    for (c, kind, text, href) in &cells {
                        if !sh.grid.linked_cell(*c, *kind, text.clone(), href.clone()) {
                            sh.full = true;
                            break;
                        }
                    }
                }
            }
            sh.row = sh.row.saturating_add(sh.row_rep);
            sh.row_cells.clear();
            if sh.full {
                self.stop = true;
            }
        } else if let Some(t) = self.tables.last_mut() {
            let mut cells = std::mem::take(&mut t.cells);
            while cells.last().is_some_and(|c| c.paras.is_empty()) {
                cells.pop();
            }
            if cells.is_empty() {
                return;
            }
            let rep = t.row_rep.clamp(1, 16);
            for _ in 1..rep {
                t.rows.push(Row { cells: cells.clone() });
            }
            t.rows.push(Row { cells });
        }
    }

    fn start_cell(&mut self, tag: &Tag<'_>, covered: bool) {
        let rep = rep_attr(tag, "number-columns-repeated");
        if let Some(sh) = self.sheet.as_mut() {
            if covered {
                sh.col = sh.col.saturating_add(rep);
                return;
            }
            let vtype = tag
                .attr_q("office:value-type")
                .or_else(|| tag.attr("value-type"))
                .unwrap_or_default()
                .into_owned();
            let value = tag
                .attr("value")
                .or_else(|| tag.attr("date-value"))
                .or_else(|| tag.attr("time-value"))
                .or_else(|| tag.attr("boolean-value"))
                .or_else(|| tag.attr("string-value"))
                .unwrap_or_default()
                .into_owned();
            let error =
                tag.attrs().any(|(n, v)| xml::local_part(n) == "value-type" && v == "error");
            sh.cell = Some(CellCtx {
                rep,
                vtype,
                value,
                formula: tag.attr("formula").is_some(),
                error,
                text: String::new(),
                has_text: false,
                href: String::new(),
            });
        } else if let Some(t) = self.tables.last_mut() {
            let span = tag.attr("number-columns-spanned").and_then(|v| v.parse().ok()).unwrap_or(1);
            t.covered = covered;
            t.col_rep = rep;
            t.cell = Some(Cell { span, header: self.header_rows > 0, paras: Vec::new() });
        }
    }

    fn end_cell(&mut self) {
        if let Some(sh) = self.sheet.as_mut() {
            let Some(c) = sh.cell.take() else { return };
            let (mut text, mut kind) = match c.vtype.as_str() {
                "float" | "percentage" | "currency" => (c.value.clone(), CellKind::Number),
                "date" | "time" => (c.value.clone(), CellKind::Date),
                "boolean" => (
                    if c.value.eq_ignore_ascii_case("true") { "TRUE" } else { "FALSE" }.to_owned(),
                    CellKind::Bool,
                ),
                _ => (String::new(), CellKind::Text),
            };
            // The text the document cached is what the producer displayed.
            if c.has_text && !c.text.trim().is_empty() {
                text = c.text;
            } else if c.vtype == "string" && text.is_empty() {
                text = c.value;
                kind = CellKind::Text;
            }
            if c.error {
                kind = CellKind::Error;
            }
            if c.formula {
                self.formulas = self.formulas.saturating_add(1);
                if text.is_empty() {
                    kind = CellKind::Formula;
                }
            }
            if !text.is_empty() {
                for k in 0..c.rep.min(MAX_REPEAT) {
                    sh.row_cells.push((
                        sh.col.saturating_add(k),
                        kind,
                        text.clone(),
                        c.href.clone(),
                    ));
                }
            }
            sh.col = sh.col.saturating_add(c.rep);
        } else if let Some(t) = self.tables.last_mut() {
            let Some(cell) = t.cell.take() else { return };
            if t.covered {
                t.covered = false;
                return;
            }
            for _ in 1..t.col_rep.min(8) {
                t.cells.push(cell.clone());
            }
            t.cells.push(cell);
        }
    }

    // -- slides -------------------------------------------------------------

    fn end_page(&mut self) {
        let Some(pg) = self.page.take() else { return };
        let first_text = pg.blocks.iter().find_map(|b| match b {
            Block::Para(p) if !p.is_blank() => Some(p.text().chars().take(80).collect::<String>()),
            _ => None,
        });
        let name = if !pg.title.is_empty() {
            pg.title.clone()
        } else {
            first_text.unwrap_or_else(|| pg.name.clone())
        };
        let Some(sec) = self.sink.open_section(self.slide_kind, &name, pg.hidden) else {
            self.stop = true;
            return;
        };
        for b in pg.blocks {
            let ok = match b {
                Block::Image(i) => self.sink.image(sec, i),
                other => self.sink.push(sec, other),
            };
            if !ok {
                self.stop = true;
                break;
            }
        }
    }

    fn end_frame(&mut self) {
        let Some(f) = self.frames.pop() else { return };
        if let Some(mut img) = f.image {
            img.alt = [f.title, f.desc, f.name]
                .into_iter()
                .find(|s| !s.trim().is_empty())
                .unwrap_or_default();
            if !f.skip {
                self.route(Block::Image(img));
            }
        }
    }

    fn image_info(&self, href: &str) -> ImageInfo {
        let external = href.contains("://") || href.starts_with("//");
        let path = href.trim_start_matches("./");
        let bytes = if external { 0 } else { self.pkg.size_of(path).unwrap_or(0) };
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let media = if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            ext
        } else {
            String::new()
        };
        ImageInfo { alt: String::new(), media, bytes, external }
    }
}

impl ParaCtx {
    /// Placeholder used where a paragraph is read but not kept.
    fn skipped() -> Self {
        Self {
            b: ParaBuilder::new(),
            style: ParaStyle::Body,
            align: Align::Start,
            marker: String::new(),
            level: 0,
            base: 0,
            spans: Vec::new(),
            hrefs: Vec::new(),
            pending_space: None,
            pending: Vec::new(),
            skip: true,
        }
    }

    const fn is_skipped(&self) -> bool {
        self.skip
    }
}

/// Elements whose subtree never carries text that should be shown.
const SKIPPED: [&str; 17] = [
    "scripts",
    "font-face-decls",
    "automatic-styles",
    "styles",
    "forms",
    "named-expressions",
    "content-validations",
    "database-ranges",
    "calculation-settings",
    "conditional-formats",
    "sequence-decls",
    "variable-decls",
    "user-field-decls",
    "binary-data",
    "dde-links",
    "settings",
    "page-thumbnail",
];

impl Handler for Odf<'_, '_> {
    #[allow(clippy::too_many_lines)]
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        let local = tag.local();
        if SKIPPED.contains(&local) {
            return Flow::SkipChildren;
        }
        match local {
            // Master styles carry the headers and footers.
            "header" | "footer" | "header-left" | "footer-left" | "header-first"
            | "footer-first" => self.hf_depth += 1,

            // -- document bodies --
            "text" if self.mode == Mode::None => {
                self.mode = Mode::Text;
                self.body = self.sink.open_section(SectionKind::Body, "", false);
            }
            "spreadsheet" if self.mode == Mode::None => self.mode = Mode::Sheet,
            "presentation" | "drawing" if self.mode == Mode::None => {
                self.mode = Mode::Slides;
                self.slide_kind =
                    if local == "drawing" { SectionKind::Page } else { SectionKind::Slide };
            }
            "page" if self.mode == Mode::Slides && self.page.is_none() => {
                let style_name = tag.attr("style-name").unwrap_or_default();
                let hidden = tag.attr("visibility").is_some_and(|v| v == "hidden")
                    || self.styles.resolve(Fam::Page, &style_name).page_hidden;
                self.page = Some(PageCtx {
                    name: tag.attr("name").unwrap_or_default().into_owned(),
                    hidden,
                    title: String::new(),
                    blocks: Vec::new(),
                });
            }
            "notes" if self.mode == Mode::Slides => self.in_notes = true,

            // -- paragraphs and inline content --
            "p" => self.begin_para(tag, false),
            "h" => self.begin_para(tag, true),
            "span" => {
                if let Some(ctx) = self.paras.last_mut() {
                    let r_name = tag.attr("style-name").unwrap_or_default();
                    let r = self.styles.resolve(Fam::Text, &r_name);
                    // `resolve` folds the chain into one set/clear pair.
                    ctx.spans.push((r.flags, r.cleared));
                }
            }
            "a" => {
                if let Some(ctx) = self.paras.last_mut() {
                    let href = tag.attr("href").unwrap_or_default().into_owned();
                    ctx.hrefs.push(href);
                }
                // A link inside a spreadsheet cell: the cell shows where it leads. A jump to
                // another place in the workbook (`#Sheet2.A1`) is not a link out.
                if let Some(c) = self.sheet.as_mut().and_then(|sh| sh.cell.as_mut())
                    && c.href.is_empty()
                {
                    let href = tag.attr("href").unwrap_or_default();
                    if !href.is_empty() && !href.starts_with('#') {
                        c.href = crate::sink::clip_href(&href, 2_048);
                    }
                }
            }
            "s" => {
                let n = rep_attr(tag, "c").min(64);
                let spaces = " ".repeat(usize::try_from(n).unwrap_or(1));
                self.add_literal(&spaces);
            }
            "tab" => self.add_literal("\t"),
            "line-break" => self.add_literal("\n"),
            "ruby-text" => self.ruby_text += 1,
            "hidden-text" => {
                if let Some(t) = tag.attr("string-value") {
                    let t = t.into_owned();
                    self.add_flagged(&t, style::HIDDEN);
                }
            }
            "section" => {
                let hidden = tag.attr("display").is_some_and(|v| v == "none");
                if hidden {
                    self.hidden_depth += 1;
                }
                self.section_hidden.push(hidden);
            }

            // -- footnotes and annotations --
            "note" => {}
            "note-citation" => {
                self.cite_depth += 1;
                self.cite.clear();
            }
            "note-body" => {
                self.note_depth += 1;
                self.note_marker = self.cite.clone();
            }
            "annotation" => {
                self.annotation =
                    Some(Annotation { base: self.paras.len(), ..Annotation::default() });
            }
            "creator" if self.annotation.is_some() => self.capture = Capture::Author,

            // -- lists --
            "list" => {
                let style_name = tag.attr("style-name").map(std::borrow::Cow::into_owned);
                let inherited = self.lists.last().map(|l| l.style.clone()).unwrap_or_default();
                self.lists.push(ListCtx { style: style_name.unwrap_or(inherited), counter: 0 });
                self.item_marker_pending.push(false);
            }
            "list-item" => {
                if let Some(l) = self.lists.last_mut() {
                    l.counter = match tag.attr("start-value").and_then(|v| v.parse::<u32>().ok()) {
                        Some(v) => v,
                        None => l.counter.saturating_add(1),
                    };
                }
                if let Some(p) = self.item_marker_pending.last_mut() {
                    *p = true;
                }
            }
            "list-header" => {
                if let Some(p) = self.item_marker_pending.last_mut() {
                    *p = false;
                }
            }

            // -- tables --
            "table" => return self.start_table(tag),
            "table-header-rows" => self.header_rows += 1,
            "table-row" => self.start_row(tag),
            "table-cell" => self.start_cell(tag, false),
            "covered-table-cell" => self.start_cell(tag, true),

            // -- frames and images --
            "frame" | "custom-shape" | "g" if self.mode != Mode::Sheet => {
                let class = tag.attr("class").unwrap_or_default().into_owned();
                let skip = matches!(class.as_str(), "page-number" | "date-time");
                self.frames.push(FrameCtx {
                    para_depth: self.paras.len(),
                    class,
                    name: tag.attr("name").unwrap_or_default().into_owned(),
                    skip,
                    ..FrameCtx::default()
                });
            }
            "image" => {
                if let Some(href) = tag.attr("href") {
                    let info = self.image_info(&href);
                    if let Some(f) = self.frames.last_mut() {
                        f.image = Some(info);
                    }
                }
            }
            // `svg:title` / `svg:desc` sit directly in a frame; `text:title` is a field inside a
            // paragraph and must stay text.
            "title" | "desc"
                if self.frames.last().is_some_and(|f| f.para_depth == self.paras.len()) =>
            {
                self.capture = if local == "title" { Capture::Title } else { Capture::Desc };
            }
            "object" | "object-ole" | "plugin" | "applet" | "floating-frame" => {
                self.unsupported = true;
            }

            // -- tracked changes --
            "changed-region" => self.cur_change = tag.attr("id").map(std::borrow::Cow::into_owned),
            "insertion" => {
                if let Some(id) = self.cur_change.clone() {
                    self.changes.insert(id, ChangeKind::Insertion);
                }
            }
            "deletion" => {
                if let Some(id) = self.cur_change.clone() {
                    self.changes.insert(id.clone(), ChangeKind::Deletion);
                    self.deleted.entry(id).or_default();
                    self.capture_deleted = Some(String::new());
                }
            }
            "format-change" => {
                if let Some(id) = self.cur_change.clone() {
                    self.changes.entry(id).or_insert(ChangeKind::Other);
                }
            }
            "change" | "change-start" => {
                if let Some(id) = tag.attr("change-id").map(std::borrow::Cow::into_owned) {
                    match self.changes.get(&id) {
                        Some(ChangeKind::Deletion) => {
                            let text = self.deleted.get(&id).cloned().unwrap_or_default();
                            if !text.is_empty() {
                                self.add_flagged(&text, style::DELETED);
                            }
                        }
                        Some(ChangeKind::Insertion) if local == "change-start" => {
                            self.open_insertions.push(id);
                        }
                        _ => {}
                    }
                }
            }
            "change-end" => {
                if let Some(id) = tag.attr("change-id")
                    && let Some(i) = self.open_insertions.iter().rposition(|x| *x == *id)
                {
                    self.open_insertions.remove(i);
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "header" | "footer" | "header-left" | "footer-left" | "header-first"
            | "footer-first" => self.hf_depth = self.hf_depth.saturating_sub(1),
            "p" | "h" => self.end_para(),
            "span" => {
                if let Some(ctx) = self.paras.last_mut() {
                    ctx.spans.pop();
                }
            }
            "a" => {
                if let Some(ctx) = self.paras.last_mut() {
                    ctx.hrefs.pop();
                }
            }
            "ruby-text" => self.ruby_text = self.ruby_text.saturating_sub(1),
            "section" => {
                if self.section_hidden.pop() == Some(true) {
                    self.hidden_depth = self.hidden_depth.saturating_sub(1);
                }
            }
            "note-citation" => self.cite_depth = self.cite_depth.saturating_sub(1),
            "note-body" => self.note_depth = self.note_depth.saturating_sub(1),
            "annotation" => {
                if let Some(a) = self.annotation.take() {
                    let text = a.text.trim().to_owned();
                    if !text.is_empty() {
                        if let Some(sh) = self.sheet.as_ref() {
                            let r = format!("{}{}", col_name(sh.col), sh.row);
                            self.comments.push((sh.name.clone(), r, a.author, text));
                        } else {
                            let p = Paragraph {
                                style: ParaStyle::Comment,
                                marker: a.author,
                                ..Paragraph::plain(&text)
                            };
                            self.route(Block::Para(p));
                        }
                    }
                }
            }
            "creator" | "title" | "desc" => self.capture = Capture::None,
            "list" => {
                self.lists.pop();
                self.item_marker_pending.pop();
            }
            "table" => self.end_table(),
            "table-header-rows" => self.header_rows = self.header_rows.saturating_sub(1),
            "table-row" => self.end_row(),
            "table-cell" | "covered-table-cell" => self.end_cell(),
            "frame" | "custom-shape" | "g" if self.mode != Mode::Sheet => self.end_frame(),
            "notes" => self.in_notes = false,
            "page" if self.mode == Mode::Slides => self.end_page(),
            "deletion" => {
                if let (Some(buf), Some(id)) =
                    (self.capture_deleted.take(), self.cur_change.clone())
                {
                    self.deleted.insert(id, buf);
                }
            }
            "changed-region" => self.cur_change = None,
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        let s = if cdata { std::borrow::Cow::Borrowed(raw) } else { unescape(raw)? };
        match self.capture {
            Capture::Author => {
                if let Some(a) = self.annotation.as_mut() {
                    a.author.push_str(s.trim());
                }
                return Ok(());
            }
            Capture::Title | Capture::Desc => {
                if let Some(f) = self.frames.last_mut() {
                    let slot =
                        if self.capture == Capture::Title { &mut f.title } else { &mut f.desc };
                    if slot.len() < 1024 {
                        slot.push_str(s.trim());
                    }
                }
                return Ok(());
            }
            Capture::None => {}
        }
        // Dates and the like inside an annotation, outside its own paragraphs, are metadata.
        if self.annotation.as_ref().is_some_and(|a| self.paras.len() <= a.base) {
            return Ok(());
        }
        if self.cite_depth > 0 {
            self.cite.push_str(s.trim());
        }
        if !self.paras.is_empty() {
            self.add_text(&s);
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.stop || self.sink.is_full()
    }
}

fn read_part(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

/// Extracts an OpenDocument package.
pub(crate) fn extract(
    pkg: &mut Package<'_>,
    sink: &mut Sink<'_>,
    format: crate::model::FileFormat,
) -> Result<(), PreviewError> {
    if let Some(manifest) = read_part(pkg, "META-INF/manifest.xml")?
        && manifest.contains("encryption-data")
    {
        return Err(PreviewError::Encrypted);
    }
    let content = read_part(pkg, "content.xml")?
        .ok_or_else(|| PreviewError::Malformed("the package has no content.xml".into()))?;
    let styles_xml = read_part(pkg, "styles.xml")?;

    let mut sh = StyleHandler::default();
    if let Some(s) = &styles_xml
        && xml::drive(s, pkg.limits(), &mut sh).is_err()
    {
        sink.skipped("styles.xml");
    }
    // Automatic styles live in the content part.
    sh.until_automatic_end = true;
    if xml::drive(&content, pkg.limits(), &mut sh).is_err() {
        sink.skipped("content.xml (styles)");
    }
    let styles = sh.styles;

    let (res, found, text_document, formulas, unsupported, comments) = {
        let mut h = Odf::new(sink, &styles, pkg, Mode::None);
        let res = xml::drive(&content, pkg.limits(), &mut h);
        let found = h.mode != Mode::None;
        let text_document = h.mode == Mode::Text;
        (res, found, text_document, h.formulas, h.unsupported, std::mem::take(&mut h.comments))
    };
    if !found {
        return Err(PreviewError::Unsupported(format));
    }
    if res.is_err() {
        sink.skipped("content.xml");
    }

    // Headers and footers come from the master styles. Spreadsheets and presentations only
    // carry print / master-page boilerplate there, so it is not shown.
    if let Some(s) = &styles_xml
        && !sink.is_full()
        && text_document
    {
        let mut h = Odf::new(sink, &styles, pkg, Mode::Headers);
        if xml::drive(s, pkg.limits(), &mut h).is_err() {
            // A damaged styles part only costs the headers.
        }
        let headers = std::mem::take(&mut h.headers);
        if !headers.is_empty()
            && let Some(sec) = sink.open_section(SectionKind::Headers, "", false)
        {
            for p in headers {
                if !sink.para(sec, p) {
                    break;
                }
            }
        }
    }

    if !comments.is_empty()
        && let Some(sec) = sink.open_section(SectionKind::Comments, "", false)
    {
        for (sheet, cell, author, text) in comments {
            let p = Paragraph {
                style: ParaStyle::Comment,
                marker: format!("{sheet}!{cell}"),
                runs: vec![
                    crate::model::Run {
                        text: if author.is_empty() { String::new() } else { format!("{author}: ") },
                        style: style::BOLD,
                        href: String::new(),
                    },
                    crate::model::Run { text, style: 0, href: String::new() },
                ],
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                break;
            }
        }
    }
    if formulas > 0 {
        sink.count(NoticeKind::FormulasNotEvaluated, "", formulas);
    }
    if unsupported {
        sink.note(NoticeKind::Unsupported, "embedded objects and drawings");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileFormat, Preview};
    use crate::testkit::{ok, paragraphs, run, section_of, text_of, zip};

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:calcext="urn:org:documentfoundation:names:experimental:calc:xmlns:calcext:1.0""#;

    fn package(
        mime: &str,
        content: &str,
        styles: Option<&str>,
        extra: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let mut parts: Vec<(&str, &[u8])> = vec![("mimetype", mime.as_bytes())];
        parts.push(("content.xml", content.as_bytes()));
        if let Some(s) = styles {
            parts.push(("styles.xml", s.as_bytes()));
        }
        parts.extend_from_slice(extra);
        zip(&parts)
    }

    fn doc(kind: &str, auto: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}><office:automatic-styles>{auto}</office:automatic-styles><office:body><office:{kind}>{body}</office:{kind}></office:body></office:document-content>"#
        )
    }

    fn odt(auto: &str, body: &str) -> Vec<u8> {
        package("application/vnd.oasis.opendocument.text", &doc("text", auto, body), None, &[])
    }

    fn ods(auto: &str, body: &str) -> Vec<u8> {
        package(
            "application/vnd.oasis.opendocument.spreadsheet",
            &doc("spreadsheet", auto, body),
            None,
            &[],
        )
    }

    fn odp(auto: &str, body: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        package(
            "application/vnd.oasis.opendocument.presentation",
            &doc("presentation", auto, body),
            None,
            extra,
        )
    }

    fn body_text(p: &Preview) -> String {
        p.sections.first().map(text_of).unwrap_or_default()
    }

    #[test]
    fn formatting_whitespace_and_links() {
        let auto = r#"<style:style style:name="B" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
            <style:style style:name="I" style:family="text" style:parent-style-name="B"><style:text-properties fo:font-style="italic" fo:font-weight="normal"/></style:style>
            <style:style style:name="H" style:family="text"><style:text-properties text:display="none"/></style:style>
            <style:style style:name="U" style:family="text"><style:text-properties style:text-underline-style="solid" style:text-line-through-style="solid" style:text-position="super 58%"/></style:style>"#;
        let body = r#"<text:p>  Hello   <text:span text:style-name="B">bold</text:span> and<text:s text:c="3"/>x<text:tab/>y<text:line-break/>z <text:span text:style-name="I">ital</text:span><text:span text:style-name="H">secret</text:span> <text:span text:style-name="U">u</text:span> <text:a xlink:href="http://e.example/x">link</text:a>  </text:p>"#;
        let p = ok(&odt(auto, body));
        let para =
            paragraphs(p.sections.first().expect("body")).into_iter().next().expect("para").clone();
        assert_eq!(para.text(), "Hello bold and   x\ty\nz italsecret u link");
        let st = |t: &str| para.runs.iter().find(|r| r.text.contains(t)).map(|r| r.style);
        assert_eq!(st("bold"), Some(style::BOLD));
        // The space before a styled span keeps the plain style.
        assert_eq!(para.runs.first().map(|r| (r.text.as_str(), r.style)), Some(("Hello ", 0)));
        assert_eq!(st("ital"), Some(style::ITALIC));
        assert_eq!(st("secret"), Some(style::HIDDEN));
        assert_eq!(st("u"), Some(style::UNDERLINE | style::STRIKE | style::SUPER));
        let link = para.runs.iter().find(|r| r.text == "link").expect("link");
        assert_eq!(link.href, "http://e.example/x");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "text"));
    }

    #[test]
    fn headings_roles_and_alignment() {
        let auto = r#"<style:style style:name="C" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>
            <style:style style:name="Title" style:family="paragraph"/>
            <style:style style:name="Quotations" style:family="paragraph"/>
            <style:style style:name="Preformatted_20_Text" style:display-name="Preformatted Text" style:family="paragraph"/>
            <style:style style:name="X" style:family="paragraph" style:parent-style-name="Quotations"/>"#;
        let body = r#"<text:p text:style-name="Title">T</text:p><text:h text:outline-level="2">H2</text:h><text:h text:outline-level="99">H99</text:h>
            <text:p text:style-name="C">c</text:p><text:p text:style-name="X">q</text:p><text:p text:style-name="Preformatted_20_Text">code</text:p>"#;
        let p = ok(&odt(auto, body));
        let got: Vec<(ParaStyle, Align, String)> = paragraphs(p.sections.first().expect("s"))
            .iter()
            .map(|p| (p.style, p.align, p.text()))
            .collect();
        assert_eq!(
            got,
            [
                (ParaStyle::Title, Align::Start, "T".to_owned()),
                (ParaStyle::heading(2), Align::Start, "H2".to_owned()),
                (ParaStyle::heading(6), Align::Start, "H99".to_owned()),
                (ParaStyle::Body, Align::Center, "c".to_owned()),
                (ParaStyle::Quote, Align::Start, "q".to_owned()),
                (ParaStyle::Code, Align::Start, "code".to_owned()),
            ]
        );
    }

    #[test]
    fn lists_markers_and_nesting() {
        let auto = r#"<text:list-style style:name="L1">
              <text:list-level-style-bullet text:level="1" text:bullet-char="–"/>
              <text:list-level-style-number text:level="2" style:num-format="a" style:num-suffix=")"/>
            </text:list-style>
            <text:list-style style:name="N"><text:list-level-style-number text:level="1" style:num-format="I" style:num-suffix="." text:start-value="4"/></text:list-style>"#;
        let body = r#"<text:list text:style-name="L1">
              <text:list-item><text:p>one</text:p><text:p>one-b</text:p>
                <text:list><text:list-item><text:p>sub1</text:p></text:list-item><text:list-item><text:p>sub2</text:p></text:list-item></text:list></text:list-item>
              <text:list-item><text:p>two</text:p></text:list-item></text:list>
            <text:list text:style-name="N"><text:list-item><text:p>iv</text:p></text:list-item><text:list-item><text:p>v</text:p></text:list-item></text:list>"#;
        let p = ok(&odt(auto, body));
        let got: Vec<(u8, String, String)> = paragraphs(p.sections.first().expect("s"))
            .iter()
            .map(|p| (p.level, p.marker.clone(), p.text()))
            .collect();
        assert_eq!(
            got,
            [
                (1, "–".to_owned(), "one".to_owned()),
                (1, String::new(), "one-b".to_owned()),
                (2, "a)".to_owned(), "sub1".to_owned()),
                (2, "b)".to_owned(), "sub2".to_owned()),
                (1, "–".to_owned(), "two".to_owned()),
                (1, "IV.".to_owned(), "iv".to_owned()),
                (1, "V.".to_owned(), "v".to_owned()),
            ]
        );
    }

    #[test]
    fn tables_spans_headers_and_nesting() {
        let body = r#"<table:table table:name="T"><table:table-column table:number-columns-repeated="3"/>
            <table:table-header-rows><table:table-row>
              <table:table-cell table:number-columns-spanned="2"><text:p>Head</text:p></table:table-cell><table:covered-table-cell><text:p>covered</text:p></table:covered-table-cell><table:table-cell><text:p>H3</text:p></table:table-cell>
            </table:table-row></table:table-header-rows>
            <table:table-row table:number-rows-repeated="2"><table:table-cell><text:p>a</text:p></table:table-cell><table:table-cell table:number-columns-repeated="100000"/></table:table-row>
            <table:table-row table:number-rows-repeated="1000000"><table:table-cell table:number-columns-repeated="1000000"/></table:table-row>
            <table:table-row><table:table-cell><table:table><table:table-row><table:table-cell><text:p>inner</text:p></table:table-cell></table:table-row></table:table></table:table-cell></table:table-row>
            </table:table>"#;
        let p = ok(&odt("", body));
        let Some(Block::Table(t)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no table")
        };
        assert_eq!(t.rows.len(), 4, "{t:?}");
        let head = t.rows.first().expect("head");
        assert_eq!(head.cells.len(), 2);
        assert!(head.cells.iter().all(|c| c.header));
        assert_eq!(head.cells.first().map(|c| c.span), Some(2));
        // Repeated rows are copied (bounded); the trailing empty cells are cut.
        assert_eq!(t.rows.get(1).map(|r| r.cells.len()), Some(1));
        assert_eq!(t.rows.get(3).and_then(|r| r.cells.first()).map(|c| c.paras.len()), Some(1));
        assert!(!text_of(p.sections.first().expect("s")).contains("covered"));
    }

    #[test]
    fn notes_annotations_and_fields() {
        let body = r#"<text:p>Body<text:note text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>Foot text</text:p></text:note-body></text:note> more<office:annotation><dc:creator>Eve</dc:creator><dc:date>2020-01-01</dc:date><text:p>remember</text:p></office:annotation></text:p>
            <text:p><text:hidden-text text:string-value="invisible" text:is-hidden="true"/> tail <text:title>Doc Title</text:title></text:p>
            <text:p><text:ruby><text:ruby-base>漢</text:ruby-base><text:ruby-text>kan</text:ruby-text></text:ruby></text:p>"#;
        let p = ok(&odt("", body));
        let paras = paragraphs(p.sections.first().expect("s"));
        let texts: Vec<(ParaStyle, String, String)> =
            paras.iter().map(|p| (p.style, p.marker.clone(), p.text())).collect();
        assert_eq!(texts.first().map(|t| t.2.as_str()), Some("Body1 more"));
        assert!(
            texts.contains(&(ParaStyle::Footnote, "1".to_owned(), "Foot text".to_owned())),
            "{texts:?}"
        );
        assert!(
            texts.contains(&(ParaStyle::Comment, "Eve".to_owned(), "remember".to_owned())),
            "{texts:?}"
        );
        assert!(texts.iter().any(|t| t.2 == "invisible tail Doc Title"), "{texts:?}");
        assert!(texts.iter().any(|t| t.2 == "漢(kan)"), "{texts:?}");
    }

    #[test]
    fn tracked_changes_and_hidden_sections() {
        let body = r#"<text:tracked-changes>
              <text:changed-region text:id="ct1"><text:deletion><office:change-info><dc:creator>X</dc:creator></office:change-info><text:p>old words</text:p></text:deletion></text:changed-region>
              <text:changed-region text:id="ct2"><text:insertion><office:change-info><dc:creator>Y</dc:creator></office:change-info></text:insertion></text:changed-region>
            </text:tracked-changes>
            <text:p>keep <text:change text:change-id="ct1"/><text:change-start text:change-id="ct2"/>fresh<text:change-end text:change-id="ct2"/> end</text:p>
            <text:section text:display="none"><text:p>hidden section</text:p></text:section><text:p>visible</text:p>"#;
        let p = ok(&odt("", body));
        let paras = paragraphs(p.sections.first().expect("s"));
        let first = paras.first().expect("first");
        let by = |t: &str| first.runs.iter().find(|r| r.text.contains(t)).map(|r| r.style);
        assert_eq!(by("old words"), Some(style::DELETED));
        assert_eq!(by("fresh"), Some(style::INSERTED));
        assert_eq!(by("keep"), Some(0));
        // The deleted text is not also shown as a stray paragraph.
        assert_eq!(paras.iter().filter(|p| p.text().contains("old words")).count(), 1);
        let hidden = paras.iter().find(|p| p.text() == "hidden section").expect("hidden");
        assert!(hidden.runs.iter().all(|r| r.style & style::HIDDEN != 0));
        let vis = paras.iter().find(|p| p.text() == "visible").expect("visible");
        assert!(vis.runs.iter().all(|r| r.style == 0));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::TrackedChanges));
    }

    #[test]
    fn images_and_objects() {
        let body = r#"<text:p>pic<draw:frame draw:name="Frame1"><draw:image xlink:href="Pictures/a.PNG"/><svg:title>Chart</svg:title><svg:desc>of sales</svg:desc></draw:frame>
              <draw:frame><draw:image xlink:href="http://tracker.example/p.gif"/></draw:frame>
              <draw:frame><draw:object xlink:href="./Object 1"/></draw:frame></text:p>"#;
        let data = package(
            "application/vnd.oasis.opendocument.text",
            &doc("text", "", body),
            None,
            &[("Pictures/a.PNG", b"12345")],
        );
        let p = ok(&data);
        let imgs: Vec<&ImageInfo> = p
            .sections
            .first()
            .expect("s")
            .blocks
            .iter()
            .filter_map(|b| if let Block::Image(i) = b { Some(i) } else { None })
            .collect();
        assert_eq!(imgs.len(), 2);
        assert_eq!(
            (imgs[0].alt.as_str(), imgs[0].media.as_str(), imgs[0].bytes, imgs[0].external),
            ("Chart", "png", 5, false)
        );
        assert!(imgs[1].external && imgs[1].bytes == 0);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::ImagesNotShown && n.count == 2));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported));
    }

    #[test]
    fn links_in_spreadsheet_cells_are_shown_but_inner_jumps_are_not() {
        let body = r##"<table:table table:name="L">
              <table:table-row>
                <table:table-cell office:value-type="string"><text:p><text:a xlink:href="https://example.org/a">site</text:a></text:p></table:table-cell>
                <table:table-cell office:value-type="string"><text:p><text:a xlink:href="#Sheet2.A1">jump</text:a></text:p></table:table-cell>
                <table:table-cell office:value-type="string"><text:p>plain <text:a xlink:href="file:///tmp/x">one</text:a> and <text:a xlink:href="https://second.example/">two</text:a></text:p></table:table-cell>
                <table:table-cell table:number-columns-repeated="2" office:value-type="string"><text:p><text:a xlink:href="https://rep.example/">rep</text:a></text:p></table:table-cell>
              </table:table-row>
            </table:table>"##;
        let p = ok(&ods("", body));
        let Some(Block::Grid(g)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no grid")
        };
        let hrefs: Vec<(u32, &str)> =
            g.rows.first().expect("row").cells.iter().map(|c| (c.col, c.href.as_str())).collect();
        assert_eq!(
            hrefs,
            [
                (0, "https://example.org/a"),
                (1, ""),
                (2, "file:///tmp/x"),
                (3, "https://rep.example/"),
                (4, "https://rep.example/"),
            ]
        );
    }

    #[test]
    fn spreadsheet_values_formulas_and_hidden_things() {
        let auto = r#"<style:style style:name="ta2" style:family="table"><style:table-properties table:display="false"/></style:style>"#;
        let body = r#"<table:table table:name="Data">
              <table:table-row>
                <table:table-cell office:value-type="string" office:string-value="from attr"/>
                <table:table-cell office:value-type="float" office:value="0.5"><text:p>50,0 %</text:p></table:table-cell>
                <table:table-cell office:value-type="float" office:value="12" table:formula="of:=SUM([.A1])"><text:p>12</text:p></table:table-cell>
                <table:table-cell office:value-type="boolean" office:boolean-value="true"/>
                <table:table-cell table:formula="of:=NOW()"/>
                <table:table-cell office:value-type="string" calcext:value-type="error"><text:p>#DIV/0!</text:p></table:table-cell>
                <table:table-cell office:value-type="date" office:date-value="2024-02-29"><text:p>29.02.2024</text:p></table:table-cell>
              </table:table-row>
              <table:table-row table:number-rows-repeated="3"><table:table-cell office:value-type="string"><text:p>L1</text:p><text:p>L2</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2" office:value-type="float" office:value="7"/></table:table-row>
              <table:table-row table:visibility="collapse"><table:table-cell office:value-type="string"><text:p>hidden row</text:p></table:table-cell></table:table-row>
              <table:table-row table:number-rows-repeated="1048000"><table:table-cell table:number-columns-repeated="16000"/></table:table-row>
            </table:table>
            <table:table table:name="Secret" table:style-name="ta2"><table:table-row><table:table-cell office:value-type="string"><text:p>shh</text:p><office:annotation><dc:creator>Zed</dc:creator><text:p>note!</text:p></office:annotation></table:table-cell></table:table-row></table:table>"#;
        let p = ok(&ods(auto, body));
        assert_eq!(
            p.sections.len(),
            3,
            "{:?}",
            p.sections.iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
        );
        let data = p.sections.first().expect("data");
        assert_eq!(
            (data.kind, data.name.as_str(), data.hidden),
            (SectionKind::Sheet, "Data", false)
        );
        let Some(Block::Grid(g)) = data.blocks.first() else { panic!("no grid") };
        assert!(!g.truncated);
        let row = |i: u32| g.rows.iter().find(|r| r.index == i);
        let cell = |r: u32, c: u32| {
            row(r)
                .and_then(|r| r.cells.iter().find(|x| x.col == c))
                .map(|c| (c.kind, c.text.as_str()))
        };
        assert_eq!(cell(1, 0), Some((CellKind::Text, "from attr")));
        assert_eq!(cell(1, 1), Some((CellKind::Number, "50,0 %")));
        assert_eq!(cell(1, 2), Some((CellKind::Number, "12")));
        assert_eq!(cell(1, 3), Some((CellKind::Bool, "TRUE")));
        // A formula without a stored result leaves its cell empty.
        assert!(cell(1, 4).is_none());
        assert_eq!(cell(1, 5), Some((CellKind::Error, "#DIV/0!")));
        assert_eq!(cell(1, 6), Some((CellKind::Date, "29.02.2024")));
        // Repeated rows are expanded (bounded) and multi-paragraph cells are joined.
        assert_eq!(cell(2, 0), Some((CellKind::Text, "L1\nL2")));
        assert_eq!(cell(4, 2), Some((CellKind::Number, "7")));
        assert!(row(5).is_some_and(|r| r.hidden), "hidden row kept and flagged");
        // A million empty trailing rows must neither be stored nor flagged as truncation.
        assert_eq!(g.rows.len(), 5);
        let secret = p.sections.get(1).expect("secret");
        assert!(secret.hidden);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "sheets")
        );
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::FormulasNotEvaluated));
        let comments = section_of(&p, SectionKind::Comments).expect("comments");
        let c = paragraphs(comments).into_iter().next().expect("comment");
        assert_eq!((c.marker.as_str(), c.text().as_str()), ("Secret!A1", "Zed: note!"));
    }

    #[test]
    fn formula_without_cached_text_is_shown_as_formula() {
        let body = r#"<table:table table:name="F"><table:table-row><table:table-cell table:formula="of:=A1+1"/></table:table-row></table:table>"#;
        // No value and no text: nothing to show, but the formula is still counted.
        let p = ok(&ods("", body));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::FormulasNotEvaluated));
    }

    #[test]
    fn slides_titles_notes_hidden_and_images() {
        let auto = r#"<style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties presentation:visibility="hidden"/></style:style>"#;
        let body = r#"<draw:page draw:name="page1">
              <draw:frame presentation:class="title"><draw:text-box><text:p>Big title</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>point</text:p></text:list-item></text:list></draw:text-box></draw:frame>
              <draw:frame presentation:class="page-number"><draw:text-box><text:p>7</text:p></draw:text-box></draw:frame>
              <draw:frame draw:name="Logo"><draw:image xlink:href="Pictures/l.png"/></draw:frame>
              <presentation:notes><draw:page-thumbnail/><draw:frame presentation:class="notes"><draw:text-box><text:p>speak slowly</text:p></draw:text-box></draw:frame></presentation:notes>
            </draw:page>
            <draw:page draw:name="page2" draw:style-name="dp2"><draw:frame><draw:text-box><text:p>unseen</text:p></draw:text-box></draw:frame></draw:page>"#;
        let p = ok(&odp(auto, body, &[("Pictures/l.png", b"abc")]));
        assert_eq!(p.sections.len(), 2);
        let s1 = p.sections.first().expect("s1");
        assert_eq!(
            (s1.kind, s1.name.as_str(), s1.hidden),
            (SectionKind::Slide, "Big title", false)
        );
        let got: Vec<(ParaStyle, String, String)> =
            paragraphs(s1).iter().map(|p| (p.style, p.marker.clone(), p.text())).collect();
        assert_eq!(
            got,
            [
                (ParaStyle::Title, String::new(), "Big title".to_owned()),
                (ParaStyle::Body, "•".to_owned(), "point".to_owned()),
                (ParaStyle::Note, String::new(), "speak slowly".to_owned()),
            ]
        );
        assert!(
            s1.blocks
                .iter()
                .any(|b| matches!(b, Block::Image(i) if i.alt == "Logo" && i.bytes == 3))
        );
        let s2 = p.sections.get(1).expect("s2");
        assert!(s2.hidden);
        assert_eq!(s2.name, "unseen");
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "slides")
        );
    }

    #[test]
    fn headers_and_footers_of_text_documents() {
        let styles = format!(
            r#"<office:document-styles {NS}><office:master-styles><style:master-page style:name="Standard"><style:header><text:p>Company<text:s/>Confidential</text:p></style:header><style:header-left><text:p>Company Confidential</text:p></style:header-left><style:footer><text:p>Page <text:page-number>1</text:page-number></text:p></style:footer></style:master-page></office:master-styles></office:document-styles>"#
        );
        let data = package(
            "application/vnd.oasis.opendocument.text",
            &doc("text", "", "<text:p>x</text:p>"),
            Some(&styles),
            &[],
        );
        let p = ok(&data);
        let h = section_of(&p, SectionKind::Headers).expect("headers");
        let texts: Vec<String> = paragraphs(h).iter().map(|p| p.text()).collect();
        assert_eq!(texts, ["Company Confidential", "Page 1"]);
        assert!(paragraphs(h).iter().all(|p| p.style == ParaStyle::HeaderFooter));
    }

    #[test]
    fn styles_come_from_styles_xml_too() {
        let styles = format!(
            r#"<office:document-styles {NS}><office:styles><style:style style:name="Strong" style:family="text"><style:text-properties fo:font-weight="700"/></style:style><style:style style:name="Mono" style:family="text"><style:text-properties style:font-name="Liberation Mono"/></style:style></office:styles></office:document-styles>"#
        );
        let body = r#"<text:p><text:span text:style-name="Strong">s</text:span><text:span text:style-name="Mono">m</text:span></text:p>"#;
        let data = package(
            "application/vnd.oasis.opendocument.text",
            &doc("text", "", body),
            Some(&styles),
            &[],
        );
        let p = ok(&data);
        let para =
            paragraphs(p.sections.first().expect("s")).into_iter().next().expect("p").clone();
        assert_eq!(
            para.runs.iter().map(|r| r.style).collect::<Vec<_>>(),
            [style::BOLD, style::MONO]
        );
    }

    #[test]
    fn encrypted_packages_are_refused_and_unknown_bodies_are_unsupported() {
        let manifest = r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="content.xml"><manifest:encryption-data/></manifest:file-entry></manifest:manifest>"#;
        let data = package(
            "application/vnd.oasis.opendocument.text",
            &doc("text", "", ""),
            None,
            &[("META-INF/manifest.xml", manifest.as_bytes())],
        );
        assert!(matches!(run(&data), Err(PreviewError::Encrypted)));
        let chart =
            package("application/vnd.oasis.opendocument.chart", &doc("chart", "", ""), None, &[]);
        assert!(matches!(run(&chart), Err(PreviewError::Unsupported(FileFormat::OdfOther))));
        let missing = zip(&[("mimetype", b"application/vnd.oasis.opendocument.text")]);
        assert!(matches!(run(&missing), Err(PreviewError::Malformed(_))));
    }

    #[test]
    fn hostile_xml_never_panics_and_stays_bounded() {
        // Entity bombs and doctype tricks are refused by the tokenizer.
        let bomb =
            "<!DOCTYPE x [<!ENTITY a \"aaaa\">]><office:document-content xmlns:office=\"o\"/>";
        let data = package("application/vnd.oasis.opendocument.text", bomb, None, &[]);
        assert!(run(&data).is_err());
        // Deep list nesting and huge repeats.
        let deep = "<text:list><text:list-item>".repeat(300)
            + "<text:p>x</text:p>"
            + &"</text:list-item></text:list>".repeat(300);
        let _ = run(&odt("", &deep));
        let base = r#"<text:list><text:list-item><text:p>a<text:span text:style-name="s">b</text:span><text:note><text:note-body><text:p>n</text:p></text:note-body></text:note></text:p></text:list-item></text:list><table:table><table:table-row><table:table-cell><text:p>c</text:p></table:table-cell></table:table-row></table:table>"#;
        let bytes = base.as_bytes();
        for i in 0..bytes.len() {
            for mask in [0x01u8, 0x20, 0xFF] {
                let mut m = bytes.to_vec();
                if let Some(b) = m.get_mut(i) {
                    *b ^= mask;
                }
                if let Ok(s) = std::str::from_utf8(&m) {
                    let _ = run(&odt("", s));
                    let _ = run(&ods("", s));
                    let _ = run(&odp("", s, &[]));
                }
            }
        }
        assert_eq!(body_text(&ok(&odt("", "<text:p>fine</text:p>"))), "fine");
    }
}
