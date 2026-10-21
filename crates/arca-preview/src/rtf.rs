//! Rich Text Format.
//!
//! A single forward pass with an explicit group stack (no recursion). Everything that is
//! not plainly text is dropped: font and colour tables, list tables, binary
//! data (`\binN` is skipped by its declared length), unknown `\*` destinations, shape
//! properties. The style sheet is read only for the names of heading styles. Embedded pictures and OLE objects become placeholders. Field instructions
//! are never executed; only `HYPERLINK` targets are kept, as display-only links.

use std::collections::BTreeMap;

use arca_container::text::{CodePage, decode_codepage};

use crate::PreviewError;
use crate::model::{
    Align, Block, Cell, ImageInfo, NoticeKind, ParaStyle, Paragraph, Row, SectionKind, Table, style,
};
use crate::sink::{ParaBuilder, Sink};

/// Deepest group nesting that keeps its own state.
const MAX_DEPTH: usize = 512;
/// Longest control word kept.
const MAX_WORD: usize = 32;
/// Most fonts remembered.
const MAX_FONTS: usize = 4_096;
/// Most distinct link targets remembered.
const MAX_LINKS: usize = 4_096;
/// Longest field instruction / object class / font name kept.
const MAX_SMALL: usize = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dest {
    /// Document text.
    Text,
    /// Everything inside is ignored.
    Skip,
    /// Font table (names and character sets are read).
    FontTbl,
    /// Style sheet (only the names of paragraph styles are read).
    StyleSheet,
    /// A list label such as `1.` or a bullet.
    Marker,
    /// Field instruction.
    FldInst,
    /// Object class name.
    ObjClass,
    /// Footnote text.
    Footnote,
    /// Header or footer text.
    Side,
    /// A picture; its data is skipped.
    Pict,
    /// An embedded object; its data is skipped except for the class name.
    Object,
}

#[derive(Debug, Clone, Copy)]
struct G {
    dest: Dest,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    hidden: bool,
    sup: bool,
    sub: bool,
    deleted: bool,
    inserted: bool,
    /// Bytes to skip after `\u` (the `\ucN` value).
    uc: u8,
    font: i32,
    /// Index into the link table, `0` = none.
    href: u32,
    /// Where this group started (for sizing pictures).
    start: usize,
}

impl G {
    const fn new() -> Self {
        Self {
            dest: Dest::Text,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            hidden: false,
            sup: false,
            sub: false,
            deleted: false,
            inserted: false,
            uc: 1,
            font: -1,
            href: 0,
            start: 0,
        }
    }

    fn flags(&self, mono: bool) -> u16 {
        let mut f = 0;
        for (on, bit) in [
            (self.bold, style::BOLD),
            (self.italic, style::ITALIC),
            (self.underline, style::UNDERLINE),
            (self.strike, style::STRIKE),
            (self.hidden, style::HIDDEN),
            (self.sup, style::SUPER),
            (self.sub, style::SUB),
            (self.deleted, style::DELETED),
            (self.inserted, style::INSERTED),
            (mono, style::MONO),
        ] {
            if on {
                f |= bit;
            }
        }
        f
    }
}

#[derive(Debug, Clone, Default)]
struct Font {
    codepage: Option<u16>,
    mono: bool,
}

/// RTF character set (`\fcharsetN`) → Windows code page.
const fn charset_codepage(n: i32) -> Option<u16> {
    Some(match n {
        128 => 932,
        129 => 949,
        130 => 1361,
        134 => 936,
        136 => 950,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        254 => 437,
        255 => 850,
        _ => return None,
    })
}

struct Parser<'a, 's, 'p> {
    data: &'a [u8],
    pos: usize,
    sink: &'s mut Sink<'p>,
    sec: crate::sink::SectionId,
    stack: Vec<G>,
    /// Fallback state if the stack were ever empty (it never is).
    scratch: G,
    overflow: usize,
    /// No word has been read yet in the current group.
    group_fresh: bool,
    deff: i32,
    stopped: bool,

    doc_cp: u16,
    fonts: BTreeMap<i32, Font>,
    cur_font_def: i32,
    font_name: String,
    links: Vec<String>,
    pending_href: u32,

    // Paragraph state.
    para: ParaBuilder,
    align: Align,
    outline: Option<u8>,
    ilvl: u8,
    marker: String,
    marker_pending: String,
    in_table: bool,
    pending_notes: Vec<Paragraph>,
    pending_blocks: Vec<Block>,

    // Table state.
    cell_paras: Vec<Paragraph>,
    row_cells: Vec<Cell>,
    rows: Vec<Row>,

    // Small captures.
    inst: String,
    objclass: String,
    note_b: ParaBuilder,
    side_b: ParaBuilder,
    headers: Vec<Paragraph>,
    pict_media: String,
    pict_bin: u64,

    // Text decoding.
    skip_left: u32,
    high_surrogate: Option<u16>,
    ignorable: bool,

    // Styles and nested tables.
    /// Heading and title styles of the style sheet, by style number.
    styles: BTreeMap<i32, ParaStyle>,
    /// Style being defined in the style sheet, `-1` outside one.
    style_id: i32,
    style_name: String,
    style_level: Option<u8>,
    /// Style of the current paragraph (`\sN`), `-1` for none.
    pstyle: i32,
    /// Table nesting depth of the current paragraph (`\itapN`).
    itap: u8,
    /// Finished paragraphs of the nested cell being written.
    nest_paras: Vec<Paragraph>,
    /// Cells of the nested row being written.
    nest_cells: Vec<Vec<Paragraph>>,

    // Findings.
    objects: u64,
    lossy_codepage: bool,
}

impl<'a, 's, 'p> Parser<'a, 's, 'p> {
    fn new(data: &'a [u8], sink: &'s mut Sink<'p>, sec: crate::sink::SectionId) -> Self {
        Self {
            data,
            pos: 0,
            sink,
            sec,
            stack: vec![G::new()],
            scratch: G::new(),
            overflow: 0,
            group_fresh: false,
            deff: -1,
            stopped: false,
            doc_cp: 1252,
            fonts: BTreeMap::new(),
            cur_font_def: -1,
            font_name: String::new(),
            links: vec![String::new()],
            pending_href: 0,
            para: ParaBuilder::new(),
            align: Align::Start,
            outline: None,
            ilvl: 0,
            marker: String::new(),
            marker_pending: String::new(),
            in_table: false,
            pending_notes: Vec::new(),
            pending_blocks: Vec::new(),
            cell_paras: Vec::new(),
            row_cells: Vec::new(),
            rows: Vec::new(),
            inst: String::new(),
            objclass: String::new(),
            note_b: ParaBuilder::new(),
            side_b: ParaBuilder::new(),
            headers: Vec::new(),
            pict_media: String::new(),
            pict_bin: 0,
            skip_left: 0,
            high_surrogate: None,
            ignorable: false,
            styles: BTreeMap::new(),
            style_id: -1,
            style_name: String::new(),
            style_level: None,
            pstyle: -1,
            itap: 0,
            nest_paras: Vec::new(),
            nest_cells: Vec::new(),
            objects: 0,
            lossy_codepage: false,
        }
    }

    fn g(&self) -> G {
        self.stack.last().copied().unwrap_or(G::new())
    }

    fn g_mut(&mut self) -> &mut G {
        match self.stack.last_mut() {
            Some(g) => g,
            None => &mut self.scratch,
        }
    }

    fn dest(&self) -> Dest {
        self.g().dest
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    // -- main loop -------------------------------------------------------------------

    fn run(&mut self) {
        while let Some(&b) = self.data.get(self.pos) {
            if self.stopped || self.sink.is_full() {
                break;
            }
            self.pos += 1;
            match b {
                b'{' => self.open_group(),
                b'}' => self.close_group(),
                b'\\' => self.control(),
                b'\r' | b'\n' => {}
                _ => {
                    self.ignorable = false;
                    self.group_fresh = false;
                    self.text_byte(b);
                }
            }
        }
        // Unterminated groups are closed at the end of the data.
        while self.stack.len() > 1 {
            self.close_group();
        }
        self.end_paragraph();
        self.flush_nested();
        self.flush_table();
    }

    fn open_group(&mut self) {
        self.ignorable = false;
        self.group_fresh = true;
        self.skip_left = 0;
        let mut g = self.g();
        g.start = self.pos;
        if self.stack.len() >= MAX_DEPTH {
            self.overflow += 1;
            self.sink.truncated("nesting");
            return;
        }
        // Skipped groups stay skipped; everything else inherits its parent's formatting.
        self.stack.push(g);
    }

    fn close_group(&mut self) {
        self.skip_left = 0;
        self.group_fresh = false;
        self.ignorable = false;
        if self.overflow > 0 {
            self.overflow -= 1;
            return;
        }
        if self.stack.len() <= 1 {
            return;
        }
        let Some(g) = self.stack.pop() else { return };
        // A group nested in one of the same kind (`{\fldinst{HYPERLINK ...}}`) is part of it.
        let nested = self.g().dest == g.dest;
        if nested {
            return;
        }
        match g.dest {
            Dest::FontTbl => self.finish_font(),
            Dest::FldInst => {
                let href = parse_hyperlink(&self.inst);
                self.inst.clear();
                self.pending_href = if let Some(h) = href { self.intern_link(h) } else { 0 };
            }
            Dest::Marker => {
                self.marker_pending = std::mem::take(&mut self.marker_pending)
                    .trim_matches(|c: char| c.is_whitespace())
                    .to_owned();
            }
            Dest::Footnote => {
                let mut p = self.note_b.take(ParaStyle::Footnote);
                trim_end(&mut p);
                if !p.runs.is_empty() {
                    self.pending_notes.push(p);
                }
            }
            Dest::Side => {
                let mut p = self.side_b.take(ParaStyle::HeaderFooter);
                trim_end(&mut p);
                if !p.is_blank() && !self.headers.iter().any(|h| h.text() == p.text()) {
                    self.headers.push(p);
                }
            }
            Dest::Pict => {
                let size = u64::try_from(self.pos.saturating_sub(g.start)).unwrap_or(0) / 2;
                let bytes = if self.pict_bin > 0 { self.pict_bin } else { size };
                let info = ImageInfo {
                    alt: String::new(),
                    media: std::mem::take(&mut self.pict_media),
                    bytes,
                    external: false,
                };
                self.pict_bin = 0;
                self.pending_blocks.push(Block::Image(info));
            }
            Dest::Object => {
                self.objects = self.objects.saturating_add(1);
                let class = std::mem::take(&mut self.objclass);
                let label = if class.trim().is_empty() {
                    "[embedded object]".to_owned()
                } else {
                    format!("[embedded object: {}]", class.trim())
                };
                self.para.text(&label, style::ITALIC, "");
            }
            _ => {}
        }
    }

    fn intern_link(&mut self, href: String) -> u32 {
        if let Some(i) = self.links.iter().position(|l| *l == href) {
            return u32::try_from(i).unwrap_or(0);
        }
        if self.links.len() >= MAX_LINKS {
            return 0;
        }
        self.links.push(href);
        u32::try_from(self.links.len().saturating_sub(1)).unwrap_or(0)
    }

    // -- control words and symbols ---------------------------------------------------

    fn control(&mut self) {
        let Some(c) = self.peek() else { return };
        self.pos += 1;
        if c != b'*' && !c.is_ascii_alphabetic() {
            self.group_fresh = false;
            if c != b'*' {
                self.ignorable = false;
            }
        }
        match c {
            b'\'' => {
                let hi = self.peek().and_then(hex);
                let lo = self.data.get(self.pos + 1).copied().and_then(hex);
                if let (Some(h), Some(l)) = (hi, lo) {
                    self.pos += 2;
                    self.text_byte_escaped((h << 4) | l);
                }
            }
            b'*' => self.ignorable = true,
            b'\\' | b'{' | b'}' => self.text_char(char::from(c)),
            b'~' => self.text_char('\u{A0}'),
            b'_' => self.text_char('\u{2011}'),
            b'-' | b':' | b'|' => {}
            b'\r' | b'\n' => self.paragraph_break(),
            b if b.is_ascii_alphabetic() => self.word(b),
            _ => {}
        }
    }

    fn word(&mut self, first: u8) {
        let mut name = String::with_capacity(8);
        name.push(char::from(first));
        while let Some(b) = self.peek() {
            if !b.is_ascii_alphabetic() {
                break;
            }
            self.pos += 1;
            if name.len() < MAX_WORD {
                name.push(char::from(b));
            }
        }
        // Optional signed parameter.
        let mut neg = false;
        if self.peek() == Some(b'-') {
            neg = true;
            self.pos += 1;
        }
        let mut num: Option<i64> = None;
        while let Some(b) = self.peek() {
            if !b.is_ascii_digit() {
                break;
            }
            self.pos += 1;
            let n = num.unwrap_or(0).saturating_mul(10).saturating_add(i64::from(b - b'0'));
            num = Some(n.min(i64::from(i32::MAX)));
        }
        if neg {
            num = num.map(|n| -n);
        }
        // One space after a control word is part of it.
        if self.peek() == Some(b' ') {
            self.pos += 1;
        }
        let param = num.and_then(|n| i32::try_from(n).ok());
        let ignorable = std::mem::take(&mut self.ignorable);
        let fresh = std::mem::take(&mut self.group_fresh);
        self.skip_left = 0;
        self.dispatch(&name, param, fresh, ignorable);
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&mut self, name: &str, param: Option<i32>, fresh: bool, ignorable: bool) {
        // `\binN`: N raw bytes follow, which may contain anything.
        if name == "bin" {
            let n = usize::try_from(param.unwrap_or(0)).unwrap_or(0);
            let end = self.pos.saturating_add(n).min(self.data.len());
            if self.dest() == Dest::Pict {
                self.pict_bin =
                    self.pict_bin.saturating_add(u64::try_from(end - self.pos).unwrap_or(0));
            }
            self.pos = end;
            return;
        }
        // A group's first control word decides what the group is.
        if self.set_destination(name, fresh, ignorable) {
            return;
        }
        match self.dest() {
            Dest::Skip => return,
            Dest::FontTbl => {
                match name {
                    "f" => {
                        self.finish_font();
                        self.cur_font_def = param.unwrap_or(-1);
                    }
                    "fcharset" => {
                        let cp = param.and_then(charset_codepage);
                        let id = self.cur_font_def;
                        if self.fonts.len() < MAX_FONTS {
                            self.fonts.entry(id).or_default().codepage = cp;
                        }
                    }
                    _ => {}
                }
                return;
            }
            // Localised style names are written with Unicode escapes.
            Dest::StyleSheet if matches!(name, "u" | "uc") => {}
            Dest::StyleSheet => {
                match name {
                    "s" => {
                        self.style_id = param.unwrap_or(-1);
                        self.style_name.clear();
                        self.style_level = None;
                    }
                    "outlinelevel" => {
                        self.style_level = param
                            .and_then(|n| u8::try_from(n).ok())
                            .filter(|&n| n < 6)
                            .map(|n| n + 1);
                    }
                    _ => {}
                }
                return;
            }
            Dest::Pict => {
                self.pict_media = match name {
                    "pngblip" => "png",
                    "jpegblip" => "jpeg",
                    "emfblip" => "emf",
                    "wmetafile" => "wmf",
                    "dibitmap" | "wbitmap" => "bmp",
                    "macpict" => "pict",
                    _ => return,
                }
                .to_owned();
                return;
            }
            // Only the font and character escapes matter for a list label.
            Dest::Marker if matches!(name, "f" | "u" | "uc" | "plain") => {}
            Dest::Marker | Dest::FldInst | Dest::ObjClass | Dest::Object => return,
            _ => {}
        }

        match name {
            // -- document properties --
            "ansicpg" => self.doc_cp = param.and_then(|n| u16::try_from(n).ok()).unwrap_or(1252),
            "pc" => self.doc_cp = 437,
            "pca" => self.doc_cp = 850,
            "mac" => self.doc_cp = 10000,
            "ansi" => {}
            "uc" => self.g_mut().uc = u8::try_from(param.unwrap_or(1)).unwrap_or(1).min(8),
            "u" => self.unicode(param.unwrap_or(0)),

            // -- character formatting --
            "plain" => {
                let keep = self.g();
                *self.g_mut() = G {
                    dest: keep.dest,
                    uc: keep.uc,
                    href: keep.href,
                    start: keep.start,
                    ..G::new()
                };
            }
            "deff" => self.deff = param.unwrap_or(-1),
            "b" => self.g_mut().bold = param != Some(0),
            "i" => self.g_mut().italic = param != Some(0),
            "ul" | "uld" | "uldash" | "uldashd" | "uldashdd" | "uldb" | "ulth" | "ulw"
            | "ulwave" | "ulhwave" | "ulldash" | "ulthdash" => {
                self.g_mut().underline = param != Some(0);
            }
            "ulnone" => self.g_mut().underline = false,
            "strike" | "striked" => self.g_mut().strike = param != Some(0),
            "v" => self.g_mut().hidden = param != Some(0),
            "super" => {
                let g = self.g_mut();
                g.sup = true;
                g.sub = false;
            }
            "sub" => {
                let g = self.g_mut();
                g.sub = true;
                g.sup = false;
            }
            "nosupersub" => {
                let g = self.g_mut();
                g.sup = false;
                g.sub = false;
            }
            "deleted" => self.g_mut().deleted = param != Some(0),
            "revised" => self.g_mut().inserted = param != Some(0),
            "f" => self.g_mut().font = param.unwrap_or(-1),

            // -- paragraph formatting --
            "pard" => {
                self.align = Align::Start;
                self.outline = None;
                self.ilvl = 0;
                self.in_table = false;
                self.pstyle = -1;
                self.itap = 0;
            }
            "s" => self.pstyle = param.unwrap_or(-1),
            "itap" => self.itap = param.and_then(|n| u8::try_from(n).ok()).unwrap_or(0),
            "ql" => self.align = Align::Start,
            "qc" => self.align = Align::Center,
            "qr" => self.align = Align::End,
            "qj" | "qd" => self.align = Align::Justify,
            "outlinelevel" => self.outline = param.and_then(|n| u8::try_from(n).ok()),
            "ilvl" => self.ilvl = param.and_then(|n| u8::try_from(n).ok()).unwrap_or(0),
            "intbl" => self.in_table = true,

            // -- structure --
            "par" | "sect" => self.paragraph_break(),
            "line" => self.text_char('\n'),
            "tab" => self.text_char('\t'),
            "page" | "column" => {}
            "cell" => self.end_cell(),
            "row" => self.end_row(),
            "nestcell" => self.end_nested_cell(),
            "nestrow" => self.end_nested_row(),
            "trowd" => {
                self.in_table = true;
            }

            // -- symbols --
            "emdash" => self.text_char('\u{2014}'),
            "endash" => self.text_char('\u{2013}'),
            "emspace" => self.text_char('\u{2003}'),
            "enspace" => self.text_char('\u{2002}'),
            "qmspace" => self.text_char('\u{2005}'),
            "bullet" => self.text_char('\u{2022}'),
            "lquote" => self.text_char('\u{2018}'),
            "rquote" => self.text_char('\u{2019}'),
            "ldblquote" => self.text_char('\u{201C}'),
            "rdblquote" => self.text_char('\u{201D}'),
            "zwj" => self.text_char('\u{200D}'),
            "zwnj" => self.text_char('\u{200C}'),
            _ => {}
        }
    }

    /// Handles the control words that open a destination. Returns `true` if `name` was one.
    fn set_destination(&mut self, name: &str, fresh: bool, ignorable: bool) -> bool {
        // Only the first word of a group (or of a `\*` group) names its destination.
        if !fresh && !ignorable {
            return false;
        }
        let dest = match name {
            "fonttbl" => Dest::FontTbl,
            "stylesheet" => Dest::StyleSheet,
            "pict" => Dest::Pict,
            "object" => Dest::Object,
            "objclass" => Dest::ObjClass,
            "fldinst" => Dest::FldInst,
            "listtext" | "pntext" => Dest::Marker,
            "footnote" => Dest::Footnote,
            "header" | "headerl" | "headerr" | "headerf" | "footer" | "footerl" | "footerr"
            | "footerf" => Dest::Side,
            // Destinations whose content is text we do want.
            "fldrslt" | "field" | "shp" | "shpinst" | "shptxt" | "shppict" | "ud" | "upr"
            | "nesttableprops" => {
                if name == "fldrslt" {
                    let href = std::mem::take(&mut self.pending_href);
                    self.g_mut().href = href;
                }
                return true;
            }
            // Known destinations with nothing to show.
            "colortbl" | "info" | "private1" | "pgdsctbl" | "listtable" | "listoverridetable"
            | "revtbl" | "rsidtbl" | "generator" | "xmlnstbl" | "themedata"
            | "colorschememapping" | "datastore" | "latentstyles" | "fchars" | "lchars"
            | "objdata" | "result" | "background" | "template" | "defchp" | "defpap" | "pn"
            | "pnseclvl" | "listpicture" | "docvar" | "sp" | "sn" | "sv" | "shprslt"
            | "nonshppict" | "bkmkstart" | "bkmkend" | "xe" | "tc" | "txe" | "ftnsep"
            | "ftnsepc" | "aftnsep" | "aftnsepc" | "datafield" | "mmath" | "wgrffmtfilter"
            | "fontemb" | "rsidroot" | "protusertbl" | "userprops" | "falt" | "panose"
            | "factoidname" | "objtime" => Dest::Skip,
            // Any other `\*` destination is optional by definition.
            _ if ignorable => Dest::Skip,
            _ => return false,
        };
        match dest {
            Dest::Footnote => self.note_b = ParaBuilder::new(),
            Dest::Side => self.side_b = ParaBuilder::new(),
            Dest::Marker => self.marker_pending.clear(),
            Dest::StyleSheet => {
                self.style_id = -1;
                self.style_name.clear();
                self.style_level = None;
            }
            Dest::Pict => {
                self.pict_media.clear();
                self.pict_bin = 0;
            }
            Dest::ObjClass => self.objclass.clear(),
            Dest::FontTbl => {
                self.cur_font_def = -1;
                self.font_name.clear();
            }
            _ => {}
        }
        // Skipped parents stay skipped.
        if self.dest() != Dest::Skip {
            self.g_mut().dest = dest;
        }
        true
    }

    // -- text ------------------------------------------------------------------------

    fn font_codepage(&self) -> CodePage {
        let font = self.g().font;
        let id = if font < 0 { self.deff } else { font };
        let cp = self.fonts.get(&id).and_then(|f| f.codepage).unwrap_or(self.doc_cp);
        CodePage::from_id(cp)
    }

    fn text_byte(&mut self, b: u8) {
        if self.skip_left > 0 {
            self.skip_left -= 1;
            return;
        }
        if b < 0x80 {
            self.text_char(char::from(b));
        } else {
            self.text_byte_escaped(b);
        }
    }

    /// A byte ≥ 0x80 (from `\'hh` or written literally).
    fn text_byte_escaped(&mut self, b: u8) {
        if self.skip_left > 0 {
            self.skip_left -= 1;
            return;
        }
        let cp = self.font_codepage();
        if !cp.is_supported() {
            self.lossy_codepage = true;
        }
        let s = decode_codepage(&[b], cp);
        for c in s.chars() {
            self.text_char(c);
        }
    }

    fn unicode(&mut self, n: i32) {
        let unit = u16::try_from(n.rem_euclid(65_536)).unwrap_or(0xFFFD);
        self.skip_left = u32::from(self.g().uc);
        match (self.high_surrogate.take(), unit) {
            (Some(hi), lo @ 0xDC00..=0xDFFF) => {
                let c = char::decode_utf16([hi, lo])
                    .next()
                    .and_then(Result::ok)
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                self.text_char(c);
            }
            (_, hi @ 0xD800..=0xDBFF) => self.high_surrogate = Some(hi),
            (_, u) => {
                let c = char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER);
                self.text_char(c);
            }
        }
    }

    fn text_char(&mut self, c: char) {
        let g = self.g();
        match g.dest {
            Dest::Text => {
                let font = if g.font < 0 { self.deff } else { g.font };
                let mono = self.fonts.get(&font).is_some_and(|f| f.mono);
                let mut buf = [0u8; 4];
                let href =
                    self.links.get(usize::try_from(g.href).unwrap_or(0)).map_or("", String::as_str);
                // Paragraph label from a preceding list marker.
                if !self.marker_pending.is_empty() {
                    self.marker = std::mem::take(&mut self.marker_pending);
                }
                self.para.text(c.encode_utf8(&mut buf), g.flags(mono), href);
            }
            Dest::Marker => {
                if self.marker_pending.len() < 32 && c != '\t' {
                    self.marker_pending.push(marker_glyph(c));
                }
            }
            Dest::StyleSheet => {
                if c == ';' {
                    self.finish_style();
                } else if self.style_name.len() < 128 {
                    self.style_name.push(c);
                }
            }
            Dest::FldInst => {
                if self.inst.len() < MAX_SMALL {
                    self.inst.push(c);
                }
            }
            Dest::ObjClass => {
                if self.objclass.len() < 128 {
                    self.objclass.push(c);
                }
            }
            Dest::Footnote => {
                let mut buf = [0u8; 4];
                self.note_b.text(c.encode_utf8(&mut buf), g.flags(false), "");
            }
            Dest::Side => {
                let mut buf = [0u8; 4];
                let c = if c == '\n' { ' ' } else { c };
                self.side_b.text(c.encode_utf8(&mut buf), g.flags(false), "");
            }
            Dest::FontTbl => {
                if c == ';' {
                    self.finish_font();
                } else if self.font_name.len() < MAX_SMALL {
                    self.font_name.push(c);
                }
            }
            Dest::Skip | Dest::Pict | Dest::Object => {}
        }
    }

    /// A style definition ended (`Heading 1;`): remember it if it is a heading or a title.
    fn finish_style(&mut self) {
        let name = std::mem::take(&mut self.style_name).trim().to_lowercase();
        let level = self.style_level.take().or_else(|| heading_level(&name));
        let id = std::mem::replace(&mut self.style_id, -1);
        let style = match (name.as_str(), level) {
            ("title" | "название", _) => ParaStyle::Title,
            ("subtitle" | "подзаголовок", _) => ParaStyle::Subtitle,
            (_, Some(n)) => ParaStyle::heading(n),
            _ => return,
        };
        if id > 0 && self.styles.len() < MAX_FONTS {
            self.styles.insert(id, style);
        }
    }

    fn finish_font(&mut self) {
        let name = std::mem::take(&mut self.font_name).to_ascii_lowercase();
        if self.cur_font_def >= 0 && self.fonts.len() < MAX_FONTS {
            let mono = ["courier", "consolas", "mono", "lucida console", "terminal"]
                .iter()
                .any(|m| name.contains(m));
            if mono {
                self.fonts.entry(self.cur_font_def).or_default().mono = true;
            }
        }
    }

    // -- paragraphs and tables -------------------------------------------------------

    fn paragraph_break(&mut self) {
        if self.dest() == Dest::Footnote {
            self.text_char(' ');
            return;
        }
        if self.dest() == Dest::Side {
            self.text_char(' ');
            return;
        }
        if self.dest() != Dest::Text {
            return;
        }
        self.end_paragraph();
    }

    fn take_paragraph(&mut self) -> Option<Paragraph> {
        let marker = std::mem::take(&mut self.marker);
        let ps = match self.outline {
            Some(n) if n < 6 => ParaStyle::heading(n + 1),
            _ => self.styles.get(&self.pstyle).copied().unwrap_or(ParaStyle::Body),
        };
        let mut p = self.para.take(ps);
        p.align = self.align;
        if !marker.is_empty() {
            p.marker = marker;
            p.level = self.ilvl.saturating_add(1);
        }
        if p.runs.is_empty() && p.marker.is_empty() { None } else { Some(p) }
    }

    fn end_paragraph(&mut self) {
        let p = self.take_paragraph();
        if let Some(p) = p {
            if self.in_table && self.itap >= 2 {
                if self.nest_paras.len() < 4_096 {
                    self.nest_paras.push(p);
                }
            } else if self.in_table {
                self.cell_paras.push(p);
            } else {
                self.flush_table();
                self.push_block(Block::Para(p));
            }
        }
        for n in std::mem::take(&mut self.pending_notes) {
            self.push_block(Block::Para(n));
        }
        for b in std::mem::take(&mut self.pending_blocks) {
            self.push_block(b);
        }
    }

    fn end_cell(&mut self) {
        self.flush_nested();
        if let Some(p) = self.take_paragraph() {
            self.cell_paras.push(p);
        }
        let paras = std::mem::take(&mut self.cell_paras);
        if self.row_cells.len() < 4_096 {
            self.row_cells.push(Cell { span: 1, header: false, paras });
        }
    }

    fn end_row(&mut self) {
        self.flush_nested();
        if let Some(p) = self.take_paragraph() {
            self.cell_paras.push(p);
        }
        if !self.cell_paras.is_empty() {
            let paras = std::mem::take(&mut self.cell_paras);
            self.row_cells.push(Cell { span: 1, header: false, paras });
        }
        let cells = std::mem::take(&mut self.row_cells);
        if !cells.is_empty() {
            self.rows.push(Row { cells });
        }
        self.in_table = false;
        for n in std::mem::take(&mut self.pending_notes) {
            self.push_block(Block::Para(n));
        }
    }

    /// A cell of a nested table ended (`\nestcell`).
    fn end_nested_cell(&mut self) {
        let mut paras = std::mem::take(&mut self.nest_paras);
        if let Some(p) = self.take_paragraph() {
            paras.push(p);
        }
        if self.nest_cells.len() < 4_096 {
            self.nest_cells.push(paras);
        }
    }

    /// A row of a nested table ended (`\nestrow`): like in DOCX, it becomes one paragraph of
    /// the cell that contains the table, its cells separated by `│`.
    fn end_nested_row(&mut self) {
        if !self.nest_paras.is_empty() {
            self.end_nested_cell();
        }
        let cells = std::mem::take(&mut self.nest_cells);
        if cells.is_empty() {
            return;
        }
        let mut b = ParaBuilder::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                b.text(" │ ", 0, "");
            }
            for (k, p) in cell.iter().enumerate() {
                if k > 0 {
                    b.text(" ", 0, "");
                }
                for r in &p.runs {
                    b.text(&r.text, r.style, &r.href);
                }
            }
        }
        let p = b.take(ParaStyle::Body);
        if !p.is_blank() {
            self.cell_paras.push(p);
        }
    }

    /// Nested cells that never got their `\nestrow` (a damaged file) still keep their text.
    fn flush_nested(&mut self) {
        if !self.nest_paras.is_empty() || !self.nest_cells.is_empty() {
            self.end_nested_row();
        }
    }

    fn flush_table(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        self.push_block(Block::Table(Table { rows }));
    }

    fn push_block(&mut self, b: Block) {
        let ok = match b {
            Block::Image(i) => self.sink.image(self.sec, i),
            other => self.sink.push(self.sec, other),
        };
        if !ok {
            self.stopped = true;
        }
    }
}

/// A list label character that is a private-use glyph of the Symbol/Wingdings fonts
/// (Word and LibreOffice write bullets as `U+F0B7` and friends) becomes a visible bullet.
fn marker_glyph(c: char) -> char {
    match c {
        '\u{F0A7}' | '\u{F06E}' | '\u{F0A0}' => '▪',
        '\u{F0D8}' => '➢',
        '\u{F0FC}' => '✓',
        '\u{F076}' => '❖',
        '\u{F000}'..='\u{F8FF}' => '•',
        _ => c,
    }
}

/// `1…6` for style names such as `heading 2` or `заголовок 3`.
fn heading_level(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("heading").or_else(|| name.strip_prefix("заголовок"))?;
    rest.trim().parse::<u8>().ok().filter(|n| (1..=9).contains(n)).map(|n| n.min(6))
}

/// Removes trailing white space from a paragraph.
fn trim_end(p: &mut Paragraph) {
    while let Some(last) = p.runs.last_mut() {
        let keep = last.text.trim_end().len();
        last.text.truncate(keep);
        if last.text.is_empty() {
            p.runs.pop();
        } else {
            break;
        }
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The target of a `HYPERLINK "url"` field instruction.
fn parse_hyperlink(inst: &str) -> Option<String> {
    let t = inst.trim();
    let rest = t.strip_prefix("HYPERLINK").or_else(|| t.strip_prefix("hyperlink"))?;
    let rest = rest.trim_start();
    let mut target = None;
    let mut it = rest.split('"');
    // Text before the first quote may hold switches such as `\l` or `\o`.
    let _switches = it.next();
    if let Some(url) = it.next()
        && !url.is_empty()
    {
        target = Some(url.to_owned());
    }
    // `HYPERLINK \l "bookmark"` is an internal jump: not a link.
    if rest.trim_start().starts_with("\\l") {
        return None;
    }
    target
}

/// Extracts an RTF document.
pub(crate) fn extract(data: &[u8], sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let Some(sec) = sink.open_section(SectionKind::Body, "", false) else {
        return Err(PreviewError::Malformed("no room for a section".into()));
    };
    let (headers, objects, lossy) = {
        let mut p = Parser::new(data, sink, sec);
        p.run();
        (std::mem::take(&mut p.headers), p.objects, p.lossy_codepage)
    };
    if !headers.is_empty()
        && let Some(hs) = sink.open_section(SectionKind::Headers, "", false)
    {
        for h in headers {
            if !sink.para(hs, h) {
                break;
            }
        }
    }
    if objects > 0 {
        sink.note(NoticeKind::Unsupported, "embedded objects");
    }
    if lossy {
        sink.note(NoticeKind::BestEffort, "code page");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Preview;
    use crate::testkit::{ok, paragraphs, run, text_of};

    fn rtf(body: &str) -> Preview {
        ok(format!("{{\\rtf1\\ansi\\ansicpg1252\\deff0 {body}}}").as_bytes())
    }

    fn lines(p: &Preview) -> Vec<String> {
        p.sections
            .first()
            .map(|s| paragraphs(s).iter().map(|p| p.text()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn plain_text_paragraphs_and_symbols() {
        let p = rtf(r"Hello\~world\par Second\line line\tab tab\emdash x \'e9\'80\par");
        assert_eq!(p.format, crate::model::FileFormat::Rtf);
        assert_eq!(lines(&p), ["Hello\u{A0}world", "Second\nline\ttab—x é€"]);
        let p = rtf(r"a\{b\}c\\d");
        assert_eq!(lines(&p), [r"a{b}c\d"]);
    }

    #[test]
    fn character_formatting_is_group_scoped() {
        let p = rtf(r"a {\b b {\i bi}} \ul u\ulnone n\plain p{\v hidden}{\super s}{\deleted d}");
        let para =
            paragraphs(p.sections.first().expect("s")).into_iter().next().expect("p").clone();
        let st = |t: &str| para.runs.iter().find(|r| r.text == t).map(|r| r.style);
        assert_eq!(st("a "), Some(0));
        assert_eq!(st("b "), Some(style::BOLD));
        assert_eq!(st("bi"), Some(style::BOLD | style::ITALIC));
        assert_eq!(st("u"), Some(style::UNDERLINE));
        assert_eq!(st("np"), Some(0));
        assert_eq!(st("hidden"), Some(style::HIDDEN));
        assert_eq!(st("s"), Some(style::SUPER));
        assert_eq!(st("d"), Some(style::DELETED));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown));
    }

    #[test]
    fn unicode_escapes_skip_fallbacks_and_pair_surrogates() {
        let p = rtf(r"\uc1\u1055?\u1088?x \uc2\u1055ab|c \uc1\u-10179?\u-8704? \u233?! \u1?z");
        assert_eq!(lines(&p), ["Прx П|c 😀 é! z"]);
        // A lone surrogate is replaced, never passed on.
        let p = rtf(r"\uc1\u-10179?x");
        assert_eq!(lines(&p), ["x"]);
    }

    #[test]
    fn code_pages_come_from_the_font_or_the_document() {
        // Cyrillic via \fcharset204 and the Windows-1251 byte values.
        let p = ok(br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0\fswiss\fcharset0 Arial;}{\f1\fcharset204 Times;}}\f1 \'cf\'f0\'e8\'e2\'e5\'f2\f0  \'e9\par}");
        assert_eq!(lines(&p), ["Привет é"]);
        // Document code page Greek.
        let p = ok(br"{\rtf1\ansi\ansicpg1253 \'c1\'e2\par}");
        assert_eq!(lines(&p), ["Αβ"]);
        // Multi-byte code pages cannot be decoded; this is reported, not guessed.
        let p = ok(br"{\rtf1\ansi\ansicpg932 \'93\'fa\par}");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort));
    }

    #[test]
    fn destinations_are_skipped_and_binary_data_is_not_parsed() {
        let body = r"{\fonttbl{\f0 Arial;}}{\colortbl;\red0\green0\blue0;}{\stylesheet{\s1 heading 1;}}{\info{\title Secret title}{\author Eve}}{\*\generator Tool 1.0;}{\*\unknownthing visible?}{\pict\pngblip\bin5 {}}{{}\par tail";
        let p = rtf(&format!("keep {body}"));
        let t = text_of(p.sections.first().expect("s"));
        assert!(t.contains("keep"), "{t}");
        assert!(t.contains("tail"), "{t}");
        for bad in ["Secret", "Eve", "Tool", "visible", "Arial", "heading"] {
            assert!(!t.contains(bad), "{bad} leaked: {t}");
        }
        // The five payload bytes `{}}{{` did not disturb the group structure.
        let img = p.sections.first().and_then(|s| {
            s.blocks.iter().find_map(|b| if let Block::Image(i) = b { Some(i) } else { None })
        });
        assert_eq!(img.map(|i| (i.media.as_str(), i.bytes)), Some(("png", 5)));
    }

    #[test]
    fn pictures_objects_and_fields() {
        let body = r#"{\pict\pngblip\picw10 89504e470d0a1a0a}
            {\object\objemb{\*\objclass Excel.Sheet.8}{\*\objdata 0123456789abcdef}{\result {\pict\wmetafile8 00}}}
            {\field{\*\fldinst{HYPERLINK "http://evil.example/x"}}{\fldrslt{\ul click}}} and
            {\field{\*\fldinst DDEAUTO c:\\windows\\system32\\cmd.exe "/c calc"}{\fldrslt harmless}}\par"#;
        let p = rtf(body);
        let sec = p.sections.first().expect("s");
        let img = sec
            .blocks
            .iter()
            .find_map(|b| if let Block::Image(i) = b { Some(i) } else { None })
            .expect("img");
        assert_eq!(img.media, "png");
        assert!(img.bytes >= 8);
        let para = paragraphs(sec).into_iter().find(|p| p.text().contains("click")).expect("para");
        let click = para.runs.iter().find(|r| r.text == "click").expect("run");
        assert_eq!(click.href, "http://evil.example/x");
        assert!(click.style & style::UNDERLINE != 0);
        assert!(para.text().contains("[embedded object: Excel.Sheet.8]"), "{}", para.text());
        assert!(
            para.text().contains("harmless") && !para.text().contains("calc"),
            "{}",
            para.text()
        );
        assert!(
            para.runs.iter().filter(|r| r.text.contains("harmless")).all(|r| r.href.is_empty())
        );
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported));
    }

    #[test]
    fn headings_alignment_lists_and_tables() {
        let body = concat!(
            r"\pard\s1\qc\outlinelevel0 Title\par",
            r"\pard\plain\fi-360\li720{\listtext\f1\'b7\tab}Item one\par",
            r"\pard{\listtext 2.\tab}Item two\par",
            r"\pard\intbl\trowd\cellx1000 a\cell b\cell\row",
            r"\trowd\intbl c\cell\row",
            r"\pard After\par",
        );
        let p = rtf(body);
        let sec = p.sections.first().expect("s");
        let paras: Vec<&Paragraph> = sec
            .blocks
            .iter()
            .filter_map(|b| if let Block::Para(p) = b { Some(p) } else { None })
            .collect();
        assert_eq!((paras[0].style, paras[0].align), (ParaStyle::heading(1), Align::Center));
        assert_eq!(
            (paras[1].marker.as_str(), paras[1].level, paras[1].text()),
            ("·", 1, "Item one".to_owned())
        );
        assert_eq!((paras[2].marker.as_str(), paras[2].text()), ("2.", "Item two".to_owned()));
        let table = sec
            .blocks
            .iter()
            .find_map(|b| if let Block::Table(t) = b { Some(t) } else { None })
            .expect("table");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[1].paras[0].text(), "b");
        assert_eq!(paras.last().map(|p| p.text()), Some("After".to_owned()));
    }

    #[test]
    fn symbol_font_bullets_are_visible() {
        // LibreOffice and Word write bullets as private-use glyphs of the Symbol font.
        let p = rtf(
            r"\pard{\listtext\pard\plain \u61623\'3f\tab}One\par\pard{\listtext\pard\plain \u61607\'a7\tab}Two\par",
        );
        let paras = paragraphs(p.sections.first().expect("s"));
        assert_eq!(paras.len(), 2);
        assert_eq!((paras[0].marker.as_str(), paras[0].text()), ("•", "One".to_owned()));
        assert_eq!((paras[1].marker.as_str(), paras[1].text()), ("▪", "Two".to_owned()));
    }

    #[test]
    fn heading_styles_come_from_the_style_sheet() {
        let p = rtf(
            r"{\stylesheet{\ql Normal;}{\s1\ql\sbasedon0\snext0 heading 1;}{\s2\outlinelevel1\ql Custom;}{\s3\ql Title;}{\*\cs10 Default Paragraph Font;}{\s4\ql \u1047?\u1072?\u1075?\u1086?\u1083?\u1086?\u1074?\u1086?\u1082? 3;}}\pard\s1 One\par\pard\s2 Two\par\pard\s3 Three\par\pard\s4 Four\par\pard\s0 Five\par\pard\s1\outlinelevel3 Six\par\pard Seven\par",
        );
        let styles: Vec<_> = paragraphs(p.sections.first().expect("s"))
            .iter()
            .map(|p| (p.text(), p.style))
            .collect();
        assert_eq!(
            styles,
            [
                ("One".to_owned(), ParaStyle::Heading1),
                ("Two".to_owned(), ParaStyle::Heading2),
                ("Three".to_owned(), ParaStyle::Title),
                ("Four".to_owned(), ParaStyle::Heading3),
                ("Five".to_owned(), ParaStyle::Body),
                ("Six".to_owned(), ParaStyle::Heading4),
                ("Seven".to_owned(), ParaStyle::Body),
            ]
        );
        // The style's text is not document text.
        assert!(!text_of(p.sections.first().expect("s")).contains("Normal"));
    }

    #[test]
    fn nested_tables_do_not_widen_the_outer_row() {
        let p = rtf(
            r"\trowd\cellx3000\cellx6000\pard\intbl\itap1 A\cell\pard\intbl\itap1 Outer\par\pard\intbl\itap2 N1\nestcell\pard\intbl\itap2 N2\nestcell{\*\nesttableprops\trowd\cellx1000\cellx2000\nestrow}\pard\intbl\itap1\cell\row\pard After\par",
        );
        let sec = p.sections.first().expect("s");
        let table = sec
            .blocks
            .iter()
            .find_map(|b| if let Block::Table(t) = b { Some(t) } else { None })
            .expect("table");
        assert_eq!(table.rows.len(), 1);
        let row = table.rows.first().expect("row");
        assert_eq!(row.cells.len(), 2, "the nested cells stay inside the outer cell");
        let second: Vec<String> =
            row.cells.get(1).expect("cell").paras.iter().map(Paragraph::text).collect();
        assert_eq!(second, ["Outer", "N1 │ N2"]);
        assert!(lines(&p).contains(&"After".to_owned()));
        // A nested row that never ends keeps its text.
        let p = rtf(r"\trowd\cellx3000\pard\intbl\itap2 Lost\nestcell\pard\intbl\itap1\cell\row");
        assert!(text_of(p.sections.first().expect("s")).contains("Lost"));
    }

    #[test]
    fn footnotes_and_headers() {
        let body = r"Body text{\footnote\pard\plain\chftn Foot note text}.\par {\header\pard Page header\par}{\footer Page footer}";
        let p = rtf(body);
        let sec = p.sections.first().expect("s");
        let texts: Vec<(ParaStyle, String)> =
            paragraphs(sec).iter().map(|p| (p.style, p.text())).collect();
        assert_eq!(texts.first(), Some(&(ParaStyle::Body, "Body text.".to_owned())));
        assert!(texts.contains(&(ParaStyle::Footnote, "Foot note text".to_owned())), "{texts:?}");
        let h = p.sections.iter().find(|s| s.kind == SectionKind::Headers).expect("headers");
        let ht: Vec<String> = paragraphs(h).iter().map(|p| p.text()).collect();
        assert_eq!(ht, ["Page header", "Page footer"]);
    }

    #[test]
    fn hostile_input_stays_bounded() {
        // Nesting far beyond the cap, unterminated groups, huge parameters.
        let deep = format!("{{\\rtf1 {}text{}", "{".repeat(100_000), "}".repeat(10));
        let p = ok(deep.as_bytes());
        assert!(p.stats.truncated);
        assert!(text_of(p.sections.first().expect("s")).contains("text"));
        let p = ok(br"{\rtf1 \b99999999999999999999 \u99999999999999 \bin999999999999 x}");
        assert!(!p.sections.is_empty());
        // \bin longer than the data.
        let _ = run(br"{\rtf1 \bin100 abc");
        // Odd escapes.
        let _ = run(b"{\\rtf1 \\'zz \\' \\u \\* \\*\\ \\ {\\*\\}} \\");
        // Control characters and bidi controls in text are removed by the sink.
        let p = ok("{\\rtf1 a\\u8238?b\\u1?c\\par}".as_bytes());
        assert_eq!(lines(&p), ["abc"]);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::InvisibleRemoved));
    }

    #[test]
    fn mutation_never_panics() {
        let base = br#"{\rtf1\ansi\ansicpg1251{\fonttbl{\f0\fcharset204 Times;}}\pard\b Hi\b0 \'cf\'f0 \u1055?{\pict\pngblip\bin3 abc}{\field{\*\fldinst HYPERLINK "http://x"}{\fldrslt l}}\intbl a\cell\row\par}"#;
        for i in 0..base.len() {
            for mask in [0x01u8, 0x20, 0x80, 0xFF] {
                let mut m = base.to_vec();
                if let Some(b) = m.get_mut(i) {
                    *b ^= mask;
                }
                let _ = run(&m);
            }
            let _ = run(base.get(..i).unwrap_or_default());
        }
    }
}
