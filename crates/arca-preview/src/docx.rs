//! WordprocessingML (`.docx`, `.docm`, `.dotx`, `.dotm`).

use std::collections::BTreeMap;

use crate::PreviewError;
use crate::model::{
    Align, Block, Cell, IMAGE_NONE, ImageInfo, ParaStyle, Row, SectionKind, Table, style,
};
use crate::opc::Package;
use crate::sink::{ParaBuilder, SectionId, Sink};
use crate::xml::{self, Flow, Handler, Tag, XmlError, unescape};

/// Elements whose subtree describes *previous* formatting or layout, never content.
const SKIPPED: [&str; 12] = [
    "rPrChange",
    "pPrChange",
    "tblPrChange",
    "sectPrChange",
    "trPrChange",
    "tcPrChange",
    "numberingChange",
    "tblGridChange",
    "Fallback",
    "sectPr",
    "tblPr",
    "tblGrid",
];

// ---------------------------------------------------------------------------
// styles.xml
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct StyleDef {
    name: String,
    based_on: String,
    outline: Option<u8>,
    run: u16,
    num: Option<(u32, u32)>,
}

/// Paragraph styles by id.
#[derive(Debug, Default)]
pub(crate) struct Styles {
    defs: BTreeMap<String, StyleDef>,
}

/// Resolved properties of a style chain.
#[derive(Debug, Default, Clone, Copy)]
struct Resolved {
    para: ParaStyle,
    run: u16,
    num: Option<(u32, u32)>,
}

fn para_style_by_name(name: &str) -> Option<ParaStyle> {
    let n = name.trim().to_ascii_lowercase();
    if let Some(rest) = n.strip_prefix("heading") {
        let digits: String = rest.trim().chars().take_while(char::is_ascii_digit).collect();
        if let Ok(level) = digits.parse::<u8>()
            && (1..=9).contains(&level)
        {
            return Some(ParaStyle::heading(level));
        }
    }
    match n.as_str() {
        "title" => Some(ParaStyle::Title),
        "subtitle" => Some(ParaStyle::Subtitle),
        "quote" | "intense quote" | "block text" | "block quotation" => Some(ParaStyle::Quote),
        "source code" | "code" | "html code" | "html preformatted" | "preformatted text"
        | "plain text" | "macro text" => Some(ParaStyle::Code),
        _ => None,
    }
}

impl Styles {
    fn resolve(&self, id: &str) -> Resolved {
        let mut out = Resolved::default();
        let mut cur = id;
        let mut para_found = false;
        let mut seen = 0;
        while let Some(def) = self.defs.get(cur) {
            if !para_found {
                if let Some(p) = para_style_by_name(&def.name) {
                    out.para = p;
                    para_found = true;
                } else if let Some(level) = def.outline
                    && level < 9
                {
                    out.para = ParaStyle::heading(level.saturating_add(1));
                    para_found = true;
                }
            }
            out.run |= def.run;
            if out.num.is_none() {
                out.num = def.num;
            }
            if def.based_on.is_empty() {
                break;
            }
            cur = &def.based_on;
            seen += 1;
            if seen > 12 {
                break;
            }
        }
        if !para_found {
            // Style ids are usually the name without spaces: "Heading1".
            if let Some(p) = para_style_by_name(&split_digits(id)) {
                out.para = p;
            }
        }
        out
    }
}

/// `Heading1` → `Heading 1`.
fn split_digits(id: &str) -> String {
    let at = id.find(|c: char| c.is_ascii_digit()).unwrap_or(id.len());
    let (head, tail) = id.split_at(at);
    if tail.is_empty() { head.to_owned() } else { format!("{head} {tail}") }
}

struct StylesHandler<'a> {
    styles: &'a mut Styles,
    cur: Option<(String, StyleDef)>,
    in_rpr: bool,
    in_ppr: bool,
    in_num: bool,
    num_id: u32,
    ilvl: u32,
}

fn mono_font(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["courier", "consolas", "mono", "lucida console", "menlo", "fixed", "terminal"]
        .iter()
        .any(|m| n.contains(m))
}

/// Run property element → style bits (shared by styles and runs).
fn run_prop(tag: &Tag<'_>) -> u16 {
    match tag.local() {
        "b" if tag.flag("val") => style::BOLD,
        "i" if tag.flag("val") => style::ITALIC,
        "u" if tag.attr("val").is_none_or(|v| v != "none") => style::UNDERLINE,
        "strike" | "dstrike" if tag.flag("val") => style::STRIKE,
        "vanish" | "specVanish" if tag.flag("val") => style::HIDDEN,
        "vertAlign" => match tag.attr("val").as_deref() {
            Some("superscript") => style::SUPER,
            Some("subscript") => style::SUB,
            _ => 0,
        },
        "rFonts" => {
            let font = tag
                .attr("ascii")
                .or_else(|| tag.attr("hAnsi"))
                .or_else(|| tag.attr("cs"))
                .unwrap_or_default();
            if mono_font(&font) { style::MONO } else { 0 }
        }
        _ => 0,
    }
}

impl Handler for StylesHandler<'_> {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "style" => {
                let id = tag.attr("styleId").unwrap_or_default().into_owned();
                self.cur = Some((id, StyleDef::default()));
            }
            "name" => {
                if let Some((_, d)) = self.cur.as_mut() {
                    d.name = tag.attr("val").unwrap_or_default().into_owned();
                }
            }
            "basedOn" => {
                if let Some((_, d)) = self.cur.as_mut() {
                    d.based_on = tag.attr("val").unwrap_or_default().into_owned();
                }
            }
            "pPr" => self.in_ppr = true,
            "rPr" => self.in_rpr = true,
            "numPr" if self.in_ppr => {
                self.in_num = true;
                self.num_id = 0;
                self.ilvl = 0;
            }
            "numId" if self.in_num => {
                self.num_id = tag.attr("val").and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "ilvl" if self.in_num => {
                self.ilvl = tag.attr("val").and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "outlineLvl" if self.in_ppr => {
                if let Some((_, d)) = self.cur.as_mut() {
                    d.outline = tag.attr("val").and_then(|v| v.parse().ok());
                }
            }
            _ if self.in_rpr => {
                let bits = run_prop(tag);
                if let Some((_, d)) = self.cur.as_mut() {
                    d.run |= bits;
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "style" => {
                if let Some((id, def)) = self.cur.take()
                    && !id.is_empty()
                {
                    self.styles.defs.insert(id, def);
                }
            }
            "pPr" => self.in_ppr = false,
            "rPr" => self.in_rpr = false,
            "numPr" => {
                if self.in_num
                    && self.num_id != 0
                    && let Some((_, d)) = self.cur.as_mut()
                {
                    d.num = Some((self.num_id, self.ilvl));
                }
                self.in_num = false;
            }
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// numbering.xml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct LevelDef {
    fmt: String,
    text: String,
    start: u32,
}

/// List definitions.
#[derive(Debug, Default)]
pub(crate) struct Numbering {
    nums: BTreeMap<u32, u32>,
    abstracts: BTreeMap<u32, Vec<LevelDef>>,
}

struct NumberingHandler<'a> {
    out: &'a mut Numbering,
    abstract_id: Option<u32>,
    num_id: Option<u32>,
    ilvl: Option<usize>,
    levels: Vec<LevelDef>,
}

impl Handler for NumberingHandler<'_> {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "abstractNum" => {
                self.abstract_id = tag.attr("abstractNumId").and_then(|v| v.parse().ok());
                self.levels.clear();
            }
            "lvl" if self.abstract_id.is_some() => {
                let i = tag.attr("ilvl").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                if i < 9 {
                    self.ilvl = Some(i);
                    while self.levels.len() <= i {
                        self.levels.push(LevelDef::default());
                    }
                    if let Some(l) = self.levels.get_mut(i) {
                        l.start = 1;
                    }
                }
            }
            "numFmt" => {
                if let (Some(i), Some(l)) =
                    (self.ilvl, self.ilvl.and_then(|i| self.levels.get_mut(i)))
                {
                    let _ = i;
                    l.fmt = tag.attr("val").unwrap_or_default().into_owned();
                }
            }
            "lvlText" => {
                if let Some(l) = self.ilvl.and_then(|i| self.levels.get_mut(i)) {
                    l.text = tag.attr("val").unwrap_or_default().into_owned();
                }
            }
            "start" => {
                if let Some(l) = self.ilvl.and_then(|i| self.levels.get_mut(i)) {
                    l.start = tag.attr("val").and_then(|v| v.parse().ok()).unwrap_or(1);
                }
            }
            "num" => self.num_id = tag.attr("numId").and_then(|v| v.parse().ok()),
            "abstractNumId" if self.num_id.is_some() => {
                if let (Some(n), Some(a)) =
                    (self.num_id, tag.attr("val").and_then(|v| v.parse::<u32>().ok()))
                {
                    self.out.nums.insert(n, a);
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "abstractNum" => {
                if let Some(id) = self.abstract_id.take() {
                    self.out.abstracts.insert(id, std::mem::take(&mut self.levels));
                }
                self.ilvl = None;
            }
            "lvl" => self.ilvl = None,
            "num" => self.num_id = None,
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

fn letters(mut n: u32, upper: bool) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        let base = if upper { b'A' } else { b'a' };
        s.push(char::from(base.saturating_add(u8::try_from(n % 26).unwrap_or(0))));
        n /= 26;
        if s.len() > 8 {
            break;
        }
    }
    s.iter().rev().collect()
}

fn roman(n: u32, upper: bool) -> String {
    if n == 0 || n >= 4000 {
        return n.to_string();
    }
    const T: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    let mut rest = n;
    for (v, s) in T {
        while rest >= v {
            out.push_str(s);
            rest -= v;
        }
    }
    if upper { out } else { out.to_lowercase() }
}

fn format_number(n: u32, fmt: &str) -> String {
    match fmt {
        "lowerLetter" => letters(n, false),
        "upperLetter" => letters(n, true),
        "lowerRoman" => roman(n, false),
        "upperRoman" => roman(n, true),
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

impl Numbering {
    /// Marker text for the next item at (`num_id`, `ilvl`), advancing `counters`.
    fn marker(&self, num_id: u32, ilvl: u32, counters: &mut BTreeMap<u32, [u32; 9]>) -> String {
        let level = usize::try_from(ilvl).unwrap_or(0).min(8);
        let levels = self.nums.get(&num_id).and_then(|a| self.abstracts.get(a));
        let def = levels.and_then(|l| l.get(level));
        let fmt = def.map_or("decimal", |d| d.fmt.as_str());
        if matches!(fmt, "bullet" | "none") {
            let custom = def.map(|d| d.text.as_str()).unwrap_or_default();
            let mut chars = custom.chars();
            if let (Some(c), None) = (chars.next(), chars.next())
                && (c.is_ascii_punctuation() || ('\u{2010}'..='\u{25FF}').contains(&c))
            {
                return c.to_string();
            }
            return match level {
                0 => "•",
                1 => "◦",
                _ => "▪",
            }
            .to_owned();
        }
        let c = counters.entry(num_id).or_insert([0; 9]);
        let start = |l: usize| levels.and_then(|v| v.get(l)).map_or(1, |d| d.start.max(1));
        if let Some(slot) = c.get_mut(level) {
            *slot = if *slot == 0 { start(level) } else { slot.saturating_add(1) };
        }
        for deeper in c.iter_mut().skip(level + 1) {
            *deeper = 0;
        }
        let pattern = def.map_or("", |d| d.text.as_str());
        if pattern.is_empty() {
            return format!("{}.", format_number(c.get(level).copied().unwrap_or(1), fmt));
        }
        let mut out = String::new();
        let mut chars = pattern.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%'
                && let Some(d) = chars.peek().and_then(|d| d.to_digit(10))
                && (1..=9).contains(&d)
            {
                chars.next();
                let l = usize::try_from(d - 1).unwrap_or(0);
                let lf = levels.and_then(|v| v.get(l)).map_or("decimal", |x| x.fmt.as_str());
                let n = c.get(l).copied().unwrap_or(0);
                out.push_str(&format_number(if n == 0 { start(l) } else { n }, lf));
            } else {
                out.push(ch);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The content walker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct RelInfo {
    target: String,
    external: bool,
    bytes: u64,
    ext: String,
    /// The member's number in the package, for a picture that is part of it.
    id: Option<u32>,
}

#[derive(Debug, Default)]
struct ParaState {
    b: ParaBuilder,
    style_id: String,
    outline: Option<u8>,
    num: Option<(u32, u32)>,
    align: Align,
    images: Vec<ImageInfo>,
    page_break: bool,
}

#[derive(Debug, Default)]
struct TableState {
    rows: Vec<Row>,
    cells: Vec<Cell>,
    cell: Option<Cell>,
    header_row: bool,
    span: u16,
}

#[derive(Debug, Default)]
struct FieldState {
    in_code: bool,
    code: String,
    href: String,
}

/// What kind of part the walker is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Body,
    Footnotes,
    Comments,
    Headers,
}

struct Walker<'a, 'p> {
    sink: &'a mut Sink<'p>,
    sec: SectionId,
    mode: Mode,
    styles: &'a Styles,
    numbering: &'a Numbering,
    counters: &'a mut BTreeMap<u32, [u32; 9]>,
    rels: BTreeMap<String, RelInfo>,
    // Footnote / comment bookkeeping.
    refs: &'a mut Vec<String>,
    // Parsing state.
    para: Option<ParaState>,
    saved: Vec<Option<ParaState>>,
    tables: Vec<TableState>,
    in_ppr: bool,
    in_rpr: bool,
    in_text: bool,
    in_instr: bool,
    in_deltext: bool,
    run_style: u16,
    run_base: u16,
    ins: u32,
    del: u32,
    hrefs: Vec<String>,
    fields: Vec<FieldState>,
    // Footnote / comment being read.
    label: Option<(String, String)>,
    skip_note: u32,
    depth_note: u32,
    drawing: u32,
    alt: String,
}

impl<'a, 'p> Walker<'a, 'p> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        sink: &'a mut Sink<'p>,
        sec: SectionId,
        mode: Mode,
        styles: &'a Styles,
        numbering: &'a Numbering,
        counters: &'a mut BTreeMap<u32, [u32; 9]>,
        rels: BTreeMap<String, RelInfo>,
        refs: &'a mut Vec<String>,
    ) -> Self {
        Self {
            sink,
            sec,
            mode,
            styles,
            numbering,
            counters,
            rels,
            refs,
            para: None,
            saved: Vec::new(),
            tables: Vec::new(),
            in_ppr: false,
            in_rpr: false,
            in_text: false,
            in_instr: false,
            in_deltext: false,
            run_style: 0,
            run_base: 0,
            ins: 0,
            del: 0,
            hrefs: Vec::new(),
            fields: Vec::new(),
            label: None,
            skip_note: 0,
            depth_note: 0,
            drawing: 0,
            alt: String::new(),
        }
    }

    fn current_href(&self) -> &str {
        for f in self.fields.iter().rev() {
            if !f.href.is_empty() && !f.in_code {
                return &f.href;
            }
        }
        self.hrefs.last().map_or("", String::as_str)
    }

    fn begin_para(&mut self) {
        self.para = Some(ParaState::default());
    }

    fn add_text(&mut self, text: &str) {
        if self.skip_note > 0 {
            return;
        }
        if self.para.is_none() {
            self.begin_para();
        }
        let mut st = self.run_style | self.run_base;
        if self.del > 0 || self.in_deltext {
            st |= style::DELETED;
        }
        if self.ins > 0 {
            st |= style::INSERTED;
        }
        let href = self.current_href().to_owned();
        if let Some(p) = self.para.as_mut() {
            p.b.text(text, st, &href);
        }
    }

    fn finish_para(&mut self) {
        let Some(mut ps) = self.para.take() else { return };
        let mut resolved = self.styles.resolve(&ps.style_id);
        if let Some(level) = ps.outline
            && level < 9
            && resolved.para == ParaStyle::Body
        {
            resolved.para = ParaStyle::heading(level.saturating_add(1));
        }
        let mut para = ps.b.take(match self.mode {
            Mode::Footnotes => ParaStyle::Footnote,
            Mode::Comments => ParaStyle::Comment,
            Mode::Headers => ParaStyle::HeaderFooter,
            Mode::Body => resolved.para,
        });
        para.align = ps.align;
        // Lists.
        let num = ps.num.or(resolved.num);
        if let Some((num_id, ilvl)) = num
            && num_id != 0
        {
            para.marker = self.numbering.marker(num_id, ilvl, self.counters);
            para.level = u8::try_from(ilvl.min(8).saturating_add(1)).unwrap_or(1);
        }
        // Footnote and comment labels go on the first paragraph.
        if let Some((marker, author)) = self.label.take() {
            para.marker = marker;
            if !author.is_empty() {
                para.runs.insert(
                    0,
                    crate::model::Run {
                        text: format!("{author}: "),
                        style: style::BOLD,
                        href: String::new(),
                    },
                );
            }
        }
        let blank = para.runs.is_empty() && para.marker.is_empty();

        if let Some(cell) = self.tables.last_mut().and_then(|t| t.cell.as_mut()) {
            for img in ps.images.drain(..) {
                para.runs.push(crate::model::Run {
                    text: format!("▣ {}", img.alt),
                    style: style::ITALIC,
                    href: String::new(),
                });
            }
            if !(blank && para.runs.is_empty()) {
                cell.paras.push(para);
            }
            return;
        }
        if !blank {
            self.sink.para(self.sec, para);
        }
        for img in ps.images.drain(..) {
            self.sink.image(self.sec, img);
        }
        if ps.page_break {
            self.sink.push(self.sec, Block::Rule);
        }
    }

    fn rel(&self, id: &str) -> Option<&RelInfo> {
        self.rels.get(id)
    }

    fn open_field_instr(&mut self, instr: &str) {
        if let Some(href) = hyperlink_of(instr)
            && let Some(f) = self.fields.last_mut()
        {
            f.href = href;
        }
    }
}

/// Extracts the target of `HYPERLINK "url"` field instructions.
pub(crate) fn hyperlink_of(instr: &str) -> Option<String> {
    let t = instr.trim_start();
    let rest = t.strip_prefix("HYPERLINK").or_else(|| t.strip_prefix("hyperlink"))?;
    let rest = rest.trim_start();
    if let Some(q) = rest.strip_prefix('"') {
        return q.split('"').next().map(str::to_owned);
    }
    rest.split_whitespace().next().filter(|s| !s.starts_with('\\')).map(str::to_owned)
}

impl Handler for Walker<'_, '_> {
    #[allow(clippy::too_many_lines)]
    fn start(&mut self, tag: &Tag<'_>, empty: bool) -> Flow {
        let local = tag.local();
        if SKIPPED.contains(&local) {
            return Flow::SkipChildren;
        }
        // Footnote / comment containers.
        match (self.mode, local) {
            (Mode::Footnotes, "footnote" | "endnote") => {
                let id = tag.attr("id").unwrap_or_default().into_owned();
                let key = format!("{local}:{id}");
                if let Some(pos) = self.refs.iter().position(|r| *r == key) {
                    self.label = Some((format!("[{}]", pos + 1), String::new()));
                    self.depth_note = 0;
                } else {
                    return Flow::SkipChildren;
                }
            }
            (Mode::Comments, "comment") => {
                let id = tag.attr("id").unwrap_or_default().into_owned();
                let key = format!("comment:{id}");
                let n = self.refs.iter().position(|r| *r == key).map_or(0, |p| p + 1);
                let author = tag.attr("author").unwrap_or_default().into_owned();
                let marker = if n > 0 { format!("[{n}]") } else { "[•]".to_owned() };
                self.label = Some((marker, author));
            }
            _ => {}
        }
        match local {
            "p" => {
                if self.para.is_some() {
                    // A paragraph inside a paragraph without a text-box wrapper:
                    // close the outer one so no text is lost.
                    self.finish_para();
                }
                self.begin_para();
            }
            "pPr" => self.in_ppr = true,
            "rPr" => self.in_rpr = true,
            "pStyle" if self.in_ppr => {
                if let Some(p) = self.para.as_mut() {
                    p.style_id = tag.attr("val").unwrap_or_default().into_owned();
                    self.run_base = self.styles.resolve(&p.style_id).run;
                }
            }
            "outlineLvl" if self.in_ppr => {
                if let Some(p) = self.para.as_mut() {
                    p.outline = tag.attr("val").and_then(|v| v.parse().ok());
                }
            }
            "jc" if self.in_ppr => {
                if let Some(p) = self.para.as_mut() {
                    p.align = match tag.attr("val").as_deref() {
                        Some("center") => Align::Center,
                        Some("right" | "end") => Align::End,
                        Some("both" | "distribute") => Align::Justify,
                        _ => Align::Start,
                    };
                }
            }
            "numId" if self.in_ppr => {
                let id = tag.attr("val").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                if let Some(p) = self.para.as_mut() {
                    let lvl = p.num.map_or(0, |(_, l)| l);
                    p.num = Some((id, lvl));
                }
            }
            "ilvl" if self.in_ppr => {
                let l = tag.attr("val").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                if let Some(p) = self.para.as_mut() {
                    let id = p.num.map_or(0, |(i, _)| i);
                    p.num = Some((id, l));
                }
            }
            "r" => {
                self.run_style = 0;
            }
            "rStyle" if self.in_rpr && !self.in_ppr => {
                let id = tag.attr("val").unwrap_or_default();
                self.run_style |= self.styles.resolve(&id).run;
            }
            _ if self.in_rpr && !self.in_ppr => {
                self.run_style |= run_prop(tag);
            }
            "t" => self.in_text = !empty,
            "delText" => {
                self.in_text = !empty;
                self.in_deltext = !empty;
            }
            "instrText" => self.in_instr = !empty,
            "tab" | "ptab" if !self.in_ppr => self.add_text("\t"),
            "br" | "cr" => {
                let kind = tag.attr("type");
                if kind.as_deref() == Some("page") {
                    if let Some(p) = self.para.as_mut() {
                        p.page_break = true;
                    }
                } else {
                    self.add_text("\n");
                }
            }
            "noBreakHyphen" => self.add_text("-"),
            "sym" => {
                if let Some(c) = tag
                    .attr("char")
                    .and_then(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16).ok())
                    .and_then(char::from_u32)
                    // Symbol-font code points are private-use glyphs; show a neutral bullet.
                    .map(|c| if ('\u{F000}'..='\u{F8FF}').contains(&c) { '•' } else { c })
                {
                    self.add_text(&c.to_string());
                }
            }
            "ins" | "moveTo" => self.ins += 1,
            "del" | "moveFrom" => self.del += 1,
            "hyperlink" => {
                let href = if let Some(id) = tag.attr("id") {
                    self.rel(&id).map(|r| r.target.clone()).unwrap_or_default()
                } else if let Some(a) = tag.attr("anchor") {
                    format!("#{a}")
                } else {
                    String::new()
                };
                self.hrefs.push(href);
            }
            "fldSimple" => {
                let mut f = FieldState::default();
                if let Some(instr) = tag.attr("instr") {
                    f.href = hyperlink_of(&instr).unwrap_or_default();
                }
                self.fields.push(f);
            }
            "fldChar" => match tag.attr("fldCharType").as_deref() {
                Some("begin") => {
                    self.fields.push(FieldState { in_code: true, ..FieldState::default() })
                }
                Some("separate") => {
                    let code = self.fields.last().map(|f| f.code.clone()).unwrap_or_default();
                    self.open_field_instr(&code);
                    if let Some(f) = self.fields.last_mut() {
                        f.in_code = false;
                    }
                }
                Some("end") => {
                    self.fields.pop();
                }
                _ => {}
            },
            "footnoteReference" | "endnoteReference" | "commentReference" => {
                let kind = match local {
                    "footnoteReference" => "footnote",
                    "endnoteReference" => "endnote",
                    _ => "comment",
                };
                let id = tag.attr("id").unwrap_or_default();
                let key = format!("{kind}:{id}");
                let n = match self.refs.iter().position(|r| *r == key) {
                    Some(p) => p + 1,
                    None => {
                        self.refs.push(key);
                        self.refs.len()
                    }
                };
                let saved = self.run_style;
                self.run_style |= style::SUPER;
                self.add_text(&format!("[{n}]"));
                self.run_style = saved;
            }
            "tbl" => self.tables.push(TableState::default()),
            "tr" => {
                if let Some(t) = self.tables.last_mut() {
                    t.cells.clear();
                    t.header_row = false;
                }
            }
            "tblHeader" => {
                if let Some(t) = self.tables.last_mut() {
                    t.header_row = tag.flag("val");
                }
            }
            "tc" => {
                if let Some(t) = self.tables.last_mut() {
                    t.span = 1;
                    t.cell = Some(Cell { span: 1, header: t.header_row, paras: Vec::new() });
                }
            }
            "gridSpan" => {
                if let Some(t) = self.tables.last_mut()
                    && let Some(c) = t.cell.as_mut()
                {
                    c.span = tag.attr("val").and_then(|v| v.parse().ok()).unwrap_or(1);
                }
            }
            "txbxContent" => {
                // Text box: its paragraphs are independent of the enclosing one.
                self.saved.push(self.para.take());
            }
            "drawing" | "pict" | "object" => {
                self.drawing += 1;
                self.alt.clear();
            }
            "docPr" | "cNvPr" if self.drawing > 0 => {
                let d = tag
                    .attr("descr")
                    .filter(|d| !d.trim().is_empty())
                    .or_else(|| tag.attr("title").filter(|d| !d.trim().is_empty()))
                    .or_else(|| tag.attr("name"))
                    .unwrap_or_default();
                self.alt = d.into_owned();
            }
            "blip" | "imagedata" if self.drawing > 0 => {
                let embed = tag.attr("embed").or_else(|| tag.attr("id"));
                let link = tag.attr("link");
                let (bytes, ext, external, pic_id) = if let Some(id) =
                    embed.as_deref().or(link.as_deref())
                    && let Some(r) = self.rel(id)
                {
                    (
                        r.bytes,
                        r.ext.clone(),
                        r.external || (embed.is_none() && link.is_some()),
                        r.id.unwrap_or(IMAGE_NONE),
                    )
                } else {
                    (0, String::new(), link.is_some(), IMAGE_NONE)
                };
                let info =
                    ImageInfo { alt: self.alt.clone(), media: ext, bytes, external, id: pic_id };
                if self.para.is_none() {
                    self.begin_para();
                }
                if let Some(p) = self.para.as_mut() {
                    p.images.push(info);
                }
            }
            "OLEObject" if self.drawing > 0 => {
                let prog = tag.attr("ProgID").unwrap_or_default();
                let info = ImageInfo {
                    alt: format!("OLE: {prog}"),
                    media: "ole".to_owned(),
                    bytes: 0,
                    external: tag.attr("Type").is_some_and(|t| t == "Link"),
                    ..ImageInfo::default()
                };
                if self.para.is_none() {
                    self.begin_para();
                }
                if let Some(p) = self.para.as_mut() {
                    p.images.push(info);
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "p" => {
                self.finish_para();
                self.in_ppr = false;
                self.in_rpr = false;
                self.run_base = 0;
                self.run_style = 0;
            }
            "pPr" => self.in_ppr = false,
            "rPr" => self.in_rpr = false,
            "r" => self.run_style = 0,
            "t" => self.in_text = false,
            "delText" => {
                self.in_text = false;
                self.in_deltext = false;
            }
            "instrText" => self.in_instr = false,
            "ins" | "moveTo" => self.ins = self.ins.saturating_sub(1),
            "del" | "moveFrom" => self.del = self.del.saturating_sub(1),
            "hyperlink" => {
                self.hrefs.pop();
            }
            "fldSimple" => {
                self.fields.pop();
            }
            "txbxContent" => {
                self.finish_para();
                self.para = self.saved.pop().flatten();
            }
            "drawing" | "pict" | "object" => self.drawing = self.drawing.saturating_sub(1),
            "tc" => {
                if let Some(t) = self.tables.last_mut()
                    && let Some(cell) = t.cell.take()
                {
                    t.cells.push(cell);
                }
            }
            "tr" => {
                if let Some(t) = self.tables.last_mut() {
                    let cells = std::mem::take(&mut t.cells);
                    if !cells.is_empty() {
                        t.rows.push(Row { cells });
                    }
                }
            }
            "tbl" => {
                if let Some(t) = self.tables.pop() {
                    let rows = t.rows;
                    if rows.is_empty() {
                        return;
                    }
                    if let Some(outer) = self.tables.last_mut().and_then(|o| o.cell.as_mut()) {
                        // Nested table: flatten into the outer cell, one paragraph per row.
                        for row in rows {
                            let mut b = ParaBuilder::new();
                            for (i, c) in row.cells.iter().enumerate() {
                                if i > 0 {
                                    b.text(" │ ", 0, "");
                                }
                                for (k, p) in c.paras.iter().enumerate() {
                                    if k > 0 {
                                        b.text(" ", 0, "");
                                    }
                                    for r in &p.runs {
                                        b.text(&r.text, r.style, &r.href);
                                    }
                                }
                            }
                            outer.paras.push(b.take(ParaStyle::Body));
                        }
                    } else {
                        self.sink.push(self.sec, Block::Table(Table { rows }));
                    }
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError> {
        if self.in_text {
            let s = if cdata { std::borrow::Cow::Borrowed(raw) } else { unescape(raw)? };
            self.add_text(&s);
        } else if self.in_instr {
            let s = if cdata { std::borrow::Cow::Borrowed(raw) } else { unescape(raw)? };
            if let Some(f) = self.fields.last_mut()
                && f.code.len() < 4096
            {
                f.code.push_str(&s);
            }
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.sink.is_full()
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn rel_map(pkg: &mut Package<'_>, part: &str) -> Result<BTreeMap<String, RelInfo>, PreviewError> {
    let mut map = BTreeMap::new();
    for r in pkg.rels_of(part)? {
        let target = if r.external {
            r.target.clone()
        } else {
            crate::opc::resolve(part, &r.target).unwrap_or_else(|| r.target.clone())
        };
        let id = if r.external { None } else { pkg.entry_id(&target) };
        let (bytes, ext) = if r.external {
            (0, r.target.rsplit('.').next().unwrap_or("").to_ascii_lowercase())
        } else {
            (
                pkg.size_of(&target).unwrap_or(0),
                target.rsplit('.').next().unwrap_or("").to_ascii_lowercase(),
            )
        };
        let ext = if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            ext
        } else {
            String::new()
        };
        map.insert(r.id.clone(), RelInfo { target, external: r.external, bytes, ext, id });
    }
    Ok(map)
}

fn read_xml<'k>(pkg: &mut Package<'k>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

/// Extracts a Word document.
pub(crate) fn extract(pkg: &mut Package<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let main = pkg
        .main_part()?
        .ok_or_else(|| PreviewError::Malformed("the package has no main document part".into()))?;

    // Styles and numbering.
    let mut styles = Styles::default();
    if let Some(p) = pkg.rel_target(&main, "styles")?
        && let Some(text) = read_xml(pkg, &p)?
    {
        let mut h = StylesHandler {
            styles: &mut styles,
            cur: None,
            in_rpr: false,
            in_ppr: false,
            in_num: false,
            num_id: 0,
            ilvl: 0,
        };
        if xml::drive(&text, pkg.limits(), &mut h).is_err() {
            sink.skipped(&p);
        }
    }
    let mut numbering = Numbering::default();
    if let Some(p) = pkg.rel_target(&main, "numbering")?
        && let Some(text) = read_xml(pkg, &p)?
    {
        let mut h = NumberingHandler {
            out: &mut numbering,
            abstract_id: None,
            num_id: None,
            ilvl: None,
            levels: Vec::new(),
        };
        if xml::drive(&text, pkg.limits(), &mut h).is_err() {
            sink.skipped(&p);
        }
    }

    let mut counters: BTreeMap<u32, [u32; 9]> = BTreeMap::new();
    let mut refs: Vec<String> = Vec::new();
    let rels = rel_map(pkg, &main)?;

    // Body.
    let Some(body) = read_xml(pkg, &main)? else {
        return Err(PreviewError::Malformed("the main document part is missing".into()));
    };
    let Some(sec) = sink.open_section(SectionKind::Body, "", false) else { return Ok(()) };
    let failed = {
        let mut w =
            Walker::new(sink, sec, Mode::Body, &styles, &numbering, &mut counters, rels, &mut refs);
        let res = xml::drive(&body, pkg.limits(), &mut w);
        w.finish_para();
        res.is_err()
    };
    if failed {
        sink.skipped(&main);
    }

    // Footnotes, endnotes, comments.
    for (suffix, mode, kind) in [
        ("footnotes", Mode::Footnotes, SectionKind::Notes),
        ("endnotes", Mode::Footnotes, SectionKind::Notes),
        ("comments", Mode::Comments, SectionKind::Comments),
    ] {
        if sink.is_full() {
            break;
        }
        let Some(part) = pkg.rel_target(&main, suffix)? else { continue };
        let Some(text) = read_xml(pkg, &part)? else { continue };
        let rels = rel_map(pkg, &part)?;
        // Reuse the section of the same kind if the footnotes part already opened it.
        let Some(id) = sink.open_section(kind, "", false) else { break };
        let failed = {
            let mut w =
                Walker::new(sink, id, mode, &styles, &numbering, &mut counters, rels, &mut refs);
            let res = xml::drive(&text, pkg.limits(), &mut w);
            w.finish_para();
            res.is_err()
        };
        if failed {
            sink.skipped(&part);
        }
        sink.drop_if_empty(id);
    }

    // Headers and footers (each distinct text once).
    let mut targets = pkg.rel_targets(&main, "header")?;
    targets.extend(pkg.rel_targets(&main, "footer")?);
    if !targets.is_empty()
        && !sink.is_full()
        && let Some(id) = sink.open_section(SectionKind::Headers, "", false)
    {
        // Word repeats identical first/even/default headers: show each distinct one once.
        let mut seen: Vec<String> = Vec::new();
        for part in targets {
            let Some(text) = read_xml(pkg, &part)? else { continue };
            if seen.contains(&text) {
                continue;
            }
            let rels = rel_map(pkg, &part)?;
            let failed = {
                let mut w = Walker::new(
                    sink,
                    id,
                    Mode::Headers,
                    &styles,
                    &numbering,
                    &mut counters,
                    rels,
                    &mut refs,
                );
                let res = xml::drive(&text, pkg.limits(), &mut w);
                w.finish_para();
                res.is_err()
            };
            if failed {
                sink.skipped(&part);
            }
            seen.push(text);
        }
        sink.drop_if_empty(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_names() {
        assert_eq!(para_style_by_name("heading 2"), Some(ParaStyle::Heading2));
        assert_eq!(para_style_by_name("Heading 1"), Some(ParaStyle::Heading1));
        assert_eq!(para_style_by_name("Title"), Some(ParaStyle::Title));
        assert_eq!(para_style_by_name("Source Code"), Some(ParaStyle::Code));
        assert_eq!(para_style_by_name("Normal"), None);
        assert_eq!(split_digits("Heading1"), "Heading 1");
        assert_eq!(split_digits("Title"), "Title");
    }

    #[test]
    fn numbering_formats() {
        assert_eq!(letters(1, false), "a");
        assert_eq!(letters(27, false), "aa");
        assert_eq!(letters(26, true), "Z");
        assert_eq!(roman(1994, true), "MCMXCIV");
        assert_eq!(roman(4, false), "iv");
        assert_eq!(roman(0, true), "0");
        assert_eq!(format_number(7, "decimalZero"), "07");
    }

    #[test]
    fn hyperlink_fields() {
        assert_eq!(
            hyperlink_of(" HYPERLINK \"http://a.example/x\" \\o tip").as_deref(),
            Some("http://a.example/x")
        );
        assert_eq!(hyperlink_of("HYPERLINK http://b.example").as_deref(), Some("http://b.example"));
        assert_eq!(hyperlink_of("PAGE"), None);
        assert_eq!(hyperlink_of("HYPERLINK \\l bookmark"), None);
    }
}

#[cfg(test)]
mod docx_tests {
    use super::*;
    use crate::model::{NoticeKind, SectionKind};
    use crate::testkit::{docx, docx_with, ok, paragraphs, rels, run, section_of, text_of};

    fn body(p: &crate::model::Preview) -> &crate::model::Section {
        section_of(p, SectionKind::Body).expect("body section")
    }

    #[test]
    fn runs_and_formatting() {
        let p = ok(&docx(
            r#"<w:p><w:r><w:t>plain </w:t></w:r><w:r><w:rPr><w:b/><w:i w:val="1"/></w:rPr><w:t>bold-italic</w:t></w:r>
            <w:r><w:rPr><w:u w:val="single"/><w:strike/></w:rPr><w:t xml:space="preserve"> under</w:t></w:r>
            <w:r><w:rPr><w:rFonts w:ascii="Consolas"/><w:vertAlign w:val="superscript"/></w:rPr><w:t>m</w:t></w:r>
            <w:r><w:rPr><w:b w:val="0"/></w:rPr><w:t>not bold</w:t></w:r>
            <w:r><w:tab/><w:t>a</w:t><w:br/><w:t>b</w:t></w:r></w:p>"#,
        ));
        let paras = paragraphs(body(&p));
        assert_eq!(paras.len(), 1);
        let para = paras.first().expect("paragraph");
        assert_eq!(para.text(), "plain bold-italic undermnot bold\ta\nb");
        let by = |t: &str| para.runs.iter().find(|r| r.text == t).map(|r| r.style);
        assert_eq!(by("bold-italic"), Some(style::BOLD | style::ITALIC));
        assert_eq!(by(" under"), Some(style::UNDERLINE | style::STRIKE));
        assert_eq!(by("m"), Some(style::MONO | style::SUPER));
        let tail = para.runs.iter().find(|r| r.text.starts_with("not bold")).map(|r| r.style);
        assert_eq!(tail, Some(0), "w:val=0 switches a toggle off; equal neighbours merge");
        let _ = by;
    }

    #[test]
    fn headings_titles_alignment_and_code() {
        let styles = format!(
            r#"<w:styles {}><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
            <w:style w:type="paragraph" w:styleId="Kapitel"><w:name w:val="Kapitel"/><w:basedOn w:val="Heading1"/></w:style>
            <w:style w:type="paragraph" w:styleId="Tit"><w:name w:val="Title"/><w:rPr><w:b/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Code"><w:name w:val="Source Code"/></w:style></w:styles>"#,
            crate::testkit::W_NS
        );
        let r = rels(&[("rId1", "styles", "styles.xml")]);
        let p = ok(&docx_with(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>One</w:t></w:r></w:p>
               <w:p><w:pPr><w:pStyle w:val="Kapitel"/><w:jc w:val="center"/></w:pPr><w:r><w:t>Derived</w:t></w:r></w:p>
               <w:p><w:pPr><w:pStyle w:val="Tit"/></w:pPr><w:r><w:t>Big</w:t></w:r></w:p>
               <w:p><w:pPr><w:outlineLvl w:val="2"/></w:pPr><w:r><w:t>Outline</w:t></w:r></w:p>
               <w:p><w:pPr><w:pStyle w:val="Code"/></w:pPr><w:r><w:t>x = 1</w:t></w:r></w:p>
               <w:p><w:pPr><w:pStyle w:val="Missing"/></w:pPr><w:r><w:t>Normal</w:t></w:r></w:p>"#,
            &[("word/styles.xml", styles.as_str()), ("word/_rels/document.xml.rels", r.as_str())],
        ));
        let paras = paragraphs(body(&p));
        let got: Vec<(ParaStyle, Align, String)> =
            paras.iter().map(|p| (p.style, p.align, p.text())).collect();
        assert_eq!(
            got,
            [
                (ParaStyle::Heading1, Align::Start, "One".to_owned()),
                (ParaStyle::Heading1, Align::Center, "Derived".to_owned()),
                (ParaStyle::Title, Align::Start, "Big".to_owned()),
                (ParaStyle::Heading3, Align::Start, "Outline".to_owned()),
                (ParaStyle::Code, Align::Start, "x = 1".to_owned()),
                (ParaStyle::Body, Align::Start, "Normal".to_owned()),
            ]
        );
        // The style's own bold applies to its runs.
        assert_eq!(paras.get(2).and_then(|p| p.runs.first()).map(|r| r.style), Some(style::BOLD));
    }

    #[test]
    fn lists_are_numbered_per_level() {
        let numbering = format!(
            r#"<w:numbering {}>
            <w:abstractNum w:abstractNumId="0">
              <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
              <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/></w:lvl>
            </w:abstractNum>
            <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/></w:lvl></w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
            <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#,
            crate::testkit::W_NS
        );
        let r = rels(&[("rId1", "numbering", "numbering.xml")]);
        let item = |num: u32, lvl: u32, t: &str| {
            format!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="{num}"/></w:numPr></w:pPr><w:r><w:t>{t}</w:t></w:r></w:p>"#
            )
        };
        let b = [
            item(1, 0, "a"),
            item(1, 1, "b"),
            item(1, 1, "c"),
            item(1, 0, "d"),
            item(1, 1, "e"),
            item(2, 0, "f"),
        ]
        .concat();
        let p = ok(&docx_with(
            &b,
            &[
                ("word/numbering.xml", numbering.as_str()),
                ("word/_rels/document.xml.rels", r.as_str()),
            ],
        ));
        let got: Vec<(String, u8, String)> =
            paragraphs(body(&p)).iter().map(|p| (p.marker.clone(), p.level, p.text())).collect();
        assert_eq!(
            got,
            [
                ("1.".to_owned(), 1, "a".to_owned()),
                ("1.a)".to_owned(), 2, "b".to_owned()),
                ("1.b)".to_owned(), 2, "c".to_owned()),
                ("2.".to_owned(), 1, "d".to_owned()),
                ("2.a)".to_owned(), 2, "e".to_owned()),
                ("•".to_owned(), 1, "f".to_owned()),
            ]
        );
    }

    #[test]
    fn tables_with_spans_headers_and_nesting() {
        let p = ok(&docx(
            r#"<w:tbl><w:tblPr><w:tblW w:w="0"/></w:tblPr><w:tblGrid><w:gridCol/></w:tblGrid>
            <w:tr><w:trPr><w:tblHeader/></w:trPr>
              <w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Head</w:t></w:r></w:p></w:tc></w:tr>
            <w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc>
              <w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p>
                <w:tbl><w:tr><w:tc><w:p><w:r><w:t>n1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>n2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
              </w:tc></w:tr></w:tbl>
            <w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
        ));
        let sec = body(&p);
        let Some(Block::Table(t)) = sec.blocks.first() else {
            panic!("no table: {:?}", sec.blocks)
        };
        assert_eq!(t.rows.len(), 2);
        let head = t.rows.first().and_then(|r| r.cells.first()).expect("head cell");
        assert_eq!((head.span, head.header), (2, true));
        let second = t.rows.get(1).expect("row 2");
        assert!(second.cells.iter().all(|c| !c.header));
        let texts: Vec<String> = second
            .cells
            .get(1)
            .map(|c| c.paras.iter().map(|p| p.text()).collect())
            .unwrap_or_default();
        assert_eq!(texts, ["b", "n1 │ n2"]);
        assert!(matches!(sec.blocks.get(1), Some(Block::Para(p)) if p.text() == "after"));
    }

    #[test]
    fn hyperlinks_and_fields_show_text_not_codes() {
        let r = rels(&[("rId5", "hyperlink", "https://evil.example/a?b=1&c=2")]);
        let p = ok(&docx_with(
            r#"<w:p><w:hyperlink r:id="rId5"><w:r><w:t>site</w:t></w:r></w:hyperlink>
               <w:fldSimple w:instr=" HYPERLINK &quot;http://simple.example&quot; "><w:r><w:t>simple</w:t></w:r></w:fldSimple>
               <w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> HYPERLINK "http://complex.example" </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>complex</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>
               <w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> DDEAUTO c:\\windows\\system32\\cmd.exe </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>result</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>
               <w:hyperlink w:anchor="bm1"><w:r><w:t>inner</w:t></w:r></w:hyperlink></w:p>"#,
            &[("word/_rels/document.xml.rels", r.as_str())],
        ));
        let para = paragraphs(body(&p)).into_iter().next().expect("paragraph").clone();
        assert_eq!(para.text(), "sitesimplecomplexresultinner");
        let href = |t: &str| para.runs.iter().find(|r| r.text == t).map(|r| r.href.clone());
        assert_eq!(href("site").as_deref(), Some("https://evil.example/a?b=1&c=2"));
        assert_eq!(href("simple").as_deref(), Some("http://simple.example"));
        assert_eq!(href("complex").as_deref(), Some("http://complex.example"));
        assert_eq!(href("result").as_deref(), Some(""));
        assert_eq!(href("inner").as_deref(), Some("#bm1"));
        assert!(!para.text().contains("DDEAUTO"));
    }

    #[test]
    fn tracked_changes_and_hidden_text_are_shown_and_labelled() {
        let p = ok(&docx(
            r#"<w:p><w:r><w:t>keep </w:t></w:r><w:ins w:id="1" w:author="a"><w:r><w:t>added </w:t></w:r></w:ins>
            <w:del w:id="2" w:author="a"><w:r><w:delText>removed </w:delText></w:r></w:del>
            <w:r><w:rPr><w:vanish/></w:rPr><w:t>secret</w:t></w:r>
            <w:r><w:rPr><w:rPrChange w:id="3"><w:rPr><w:b/></w:rPr></w:rPrChange></w:rPr><w:t> tail</w:t></w:r></w:p>"#,
        ));
        let para = paragraphs(body(&p)).into_iter().next().expect("paragraph").clone();
        let by = |t: &str| para.runs.iter().find(|r| r.text.trim() == t).map(|r| r.style);
        assert_eq!(by("keep"), Some(0));
        assert_eq!(by("added"), Some(style::INSERTED));
        assert_eq!(by("removed"), Some(style::DELETED));
        assert_eq!(by("secret"), Some(style::HIDDEN));
        assert_eq!(by("tail"), Some(0), "formatting history must not leak into the current run");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "text"));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::TrackedChanges && n.count == 2));
    }

    #[test]
    fn notes_comments_and_headers() {
        let fnotes = format!(
            r#"<w:footnotes {}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>
            <w:footnote w:id="2"><w:p><w:r><w:t>Footnote text</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
            crate::testkit::W_NS
        );
        let comments = format!(
            r#"<w:comments {}><w:comment w:id="0" w:author="Mallory"><w:p><w:r><w:t>Check this</w:t></w:r></w:p></w:comment></w:comments>"#,
            crate::testkit::W_NS
        );
        let header = format!(
            r#"<w:hdr {}><w:p><w:r><w:t>CONFIDENTIAL</w:t></w:r></w:p></w:hdr>"#,
            crate::testkit::W_NS
        );
        let r = rels(&[
            ("r1", "footnotes", "footnotes.xml"),
            ("r2", "comments", "comments.xml"),
            ("r3", "header", "header1.xml"),
            ("r4", "header", "header2.xml"),
            ("r5", "footer", "header1.xml"),
        ]);
        let p = ok(&docx_with(
            r#"<w:p><w:r><w:t>Body</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r>
               <w:commentRangeStart w:id="0"/><w:r><w:t> flagged</w:t></w:r><w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
            &[
                ("word/footnotes.xml", fnotes.as_str()),
                ("word/comments.xml", comments.as_str()),
                ("word/header1.xml", header.as_str()),
                ("word/header2.xml", header.as_str()),
                ("word/_rels/document.xml.rels", r.as_str()),
            ],
        ));
        assert_eq!(text_of(body(&p)), "Body[1] flagged[2]");
        let notes = section_of(&p, SectionKind::Notes).expect("notes");
        let note = paragraphs(notes).into_iter().next().expect("note").clone();
        assert_eq!(
            (note.marker.as_str(), note.text().as_str(), note.style),
            ("[1]", "Footnote text", ParaStyle::Footnote)
        );
        let comm = section_of(&p, SectionKind::Comments).expect("comments");
        let c = paragraphs(comm).into_iter().next().expect("comment").clone();
        assert_eq!(c.marker, "[2]");
        assert_eq!(c.text(), "Mallory: Check this");
        let h = section_of(&p, SectionKind::Headers).expect("headers");
        assert_eq!(paragraphs(h).len(), 1, "identical headers are shown once");
    }

    #[test]
    fn alternate_content_prefers_choice_and_text_boxes_are_read() {
        let p = ok(&docx(
            r#"<w:p><w:r><w:t>before </w:t></w:r>
              <mc:AlternateContent>
                <mc:Choice Requires="wps"><w:r><w:drawing><wp:inline><wp:docPr id="1" name="Box" descr="a box"/>
                  <a:graphic><a:graphicData><wps:wsp xmlns:wps="x"><wps:txbx><w:txbxContent>
                    <w:p><w:r><w:t>inside box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic>
                </wp:inline></w:drawing></w:r></mc:Choice>
                <mc:Fallback><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>DUPLICATE</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></mc:Fallback>
              </mc:AlternateContent><w:r><w:t>after</w:t></w:r></w:p>"#,
        ));
        let text = text_of(body(&p));
        assert!(text.contains("inside box"), "{text}");
        assert!(!text.contains("DUPLICATE"), "{text}");
        assert!(text.contains("before after"), "outer paragraph survives the text box: {text}");
    }

    #[test]
    fn pictures_become_placeholders() {
        let r = rels(&[
            ("rId9", "image", "media/logo.png"),
            ("rId8", "image", "http://t.example/pixel.gif"),
        ]);
        let p = ok(&docx_with(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" name="Picture 1" descr="Company logo"/>
               <a:graphic><a:graphicData><a:blip r:embed="rId9"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
               <w:p><w:r><w:drawing><wp:inline><wp:docPr id="2" name="Tracker"/>
               <a:graphic><a:graphicData><a:blip r:link="rId8"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
            &[("word/media/logo.png", "PNGDATA"), ("word/_rels/document.xml.rels", r.as_str())],
        ));
        let images: Vec<&ImageInfo> = body(&p)
            .blocks
            .iter()
            .filter_map(|b| if let Block::Image(i) = b { Some(i) } else { None })
            .collect();
        assert_eq!(images.len(), 2);
        assert_eq!(
            images.first().map(|i| (i.alt.as_str(), i.media.as_str(), i.bytes, i.external)),
            Some(("Company logo", "png", 7, false))
        );
        assert_eq!(images.get(1).map(|i| (i.alt.as_str(), i.external)), Some(("Tracker", true)));
        // The picture that is part of the package can be opened (it has an id); the remote one
        // cannot, and only that one is counted as not shown.
        assert_ne!(images.first().map(|i| i.id), Some(crate::model::IMAGE_NONE));
        assert_eq!(images.get(1).map(|i| i.id), Some(crate::model::IMAGE_NONE));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::ImagesNotShown && n.count == 1));
    }

    #[test]
    fn invisible_and_bidi_characters_are_removed() {
        let p =
            ok(&docx("<w:p><w:r><w:t>invoice\u{202E}gpj.exe\u{200B}x\u{E0049}</w:t></w:r></w:p>"));
        assert_eq!(text_of(body(&p)), "invoicegpj.exex");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::InvisibleRemoved && n.count == 3));
    }

    #[test]
    fn entity_attacks_do_not_expand_and_do_not_crash() {
        let evil = r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/passwd">]><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>&x;</w:t></w:r></w:p></w:body></w:document>"#;
        let data = crate::testkit::opc(
            ("word/document.xml", crate::testkit::DOCX_CT),
            &[("word/document.xml", evil)],
        );
        let p = ok(&data);
        assert!(text_of(body(&p)).is_empty());
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Skipped));
    }

    #[test]
    fn missing_main_part_is_an_error() {
        let data = crate::testkit::opc(("word/document.xml", crate::testkit::DOCX_CT), &[]);
        assert!(matches!(run(&data), Err(PreviewError::Malformed(_))));
    }

    #[test]
    fn hostile_documents_never_panic() {
        let base = format!(
            r#"<?xml version="1.0"?><w:document {}><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="3"/><w:numId w:val="9"/></w:numPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>héllo</w:t></w:r><w:hyperlink r:id="x"><w:r><w:t>l</w:t></w:r></w:hyperlink></w:p><w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"#,
            crate::testkit::W_NS
        );
        let bytes = base.as_bytes();
        for i in (0..bytes.len()).step_by(3) {
            for mask in [0x01u8, 0x20, 0xFF] {
                let mut m = bytes.to_vec();
                if let Some(b) = m.get_mut(i) {
                    *b ^= mask;
                }
                if let Ok(s) = std::str::from_utf8(&m) {
                    let data = crate::testkit::opc(
                        ("word/document.xml", crate::testkit::DOCX_CT),
                        &[("word/document.xml", s)],
                    );
                    let _ = run(&data);
                }
            }
        }
    }

    #[test]
    fn budget_truncates_huge_documents() {
        let para = "<w:p><w:r><w:t>0123456789</w:t></w:r></w:p>".repeat(5000);
        let data = docx(&para);
        let policy = crate::PreviewPolicy { max_chars: 5000, ..crate::PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert!(p.stats.truncated);
        assert!(p.stats.chars <= 5000);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Truncated));
    }
}
