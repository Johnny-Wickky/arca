//! Legacy PowerPoint presentations (`.ppt`, `.pps`, `.pot`): the binary format of PowerPoint
//! 97–2003.
//!
//! Only *text and structure* are read — nothing is executed or rendered:
//!
//! * the **edit chain** (`Current User` → `UserEditAtom` → `PersistDirectoryAtom`) tells which
//!   records are the *current* slides and notes pages. Slides that older saves left behind are
//!   not shown, but counted, so a reader knows the file carries more than it displays;
//! * **slide text** comes from the drawing of each slide (`PPDrawing`, Escher shape containers)
//!   and from the document's text lists (`SlideListWithTexts`), which hold the text of
//!   placeholders. Text that no shape refers to is shown too — nothing is silently lost;
//! * **hidden slides** and **hidden or deleted shapes** are shown and labelled;
//! * **speaker notes** follow the slide's content;
//! * **hyperlink targets** are listed in their own section;
//! * pictures, embedded objects, macros (VBA) and animations are never interpreted.
//!
//! Password-protected presentations are reported, not opened. All offsets and counts come from
//! the (untrusted) file and are bounds-checked; every loop and recursion is capped.

use std::collections::{BTreeMap, BTreeSet};

use arca_container::bytes::{slice, u16_le, u32_le};
use arca_container::cfb::CompoundFile;

use crate::PreviewError;
use crate::model::{Align, Block, ImageInfo, NoticeKind, ParaStyle, Paragraph, SectionKind, style};
use crate::sink::{ParaBuilder, Sink};

/// Revisions followed in the edit chain.
const MAX_EDITS: usize = 4_096;
/// Entries of the persist directory.
const MAX_PERSIST: usize = 1 << 20;
/// Nesting of containers.
const MAX_DEPTH: usize = 24;
/// Records visited per presentation.
const MAX_RECORDS: usize = 4 << 20;
/// Slides listed.
const MAX_SLIDES: usize = 16_384;
/// UTF-16 units kept per text.
const MAX_UNITS: usize = 4 << 20;
/// Runs of formatting kept per text.
const MAX_FORMAT_RUNS: usize = 1 << 16;
/// Hyperlinks listed.
const MAX_LINKS: usize = 4_096;

const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE: u16 = 0x03EE;
const RT_SLIDE_ATOM: u16 = 0x03EF;
const RT_NOTES: u16 = 0x03F0;
const RT_SLIDE_PERSIST: u16 = 0x03F3;
const RT_SLIDE_SHOW_INFO: u16 = 0x03F9;
const RT_EX_OBJ_LIST: u16 = 0x0409;
const RT_PP_DRAWING: u16 = 0x040C;
const RT_OUTLINE_REF: u16 = 0x0F9E;
const RT_TEXT_HEADER: u16 = 0x0F9F;
const RT_TEXT_CHARS: u16 = 0x0FA0;
const RT_STYLE_TEXT: u16 = 0x0FA1;
const RT_TEXT_BYTES: u16 = 0x0FA8;
const RT_CSTRING: u16 = 0x0FBA;
const RT_SLIDE_NUMBER_FIELD: u16 = 0x0FD8;
const RT_DATE_FIELD: u16 = 0x0FF7;
const RT_GENERIC_DATE_FIELD: u16 = 0x0FF8;
const RT_HEADER_FIELD: u16 = 0x0FF9;
const RT_FOOTER_FIELD: u16 = 0x0FFA;
const RT_RTF_DATE_FIELD: u16 = 0x1015;
const RT_EX_EMBED: u16 = 0x0FCC;
const RT_EX_CONTROL: u16 = 0x0FEE;
const RT_EX_HYPERLINK: u16 = 0x0FD7;
const RT_LIST_WITH_TEXTS: u16 = 0x0FF0;
const RT_USER_EDIT: u16 = 0x0FF5;
const RT_EX_OLE_STORAGE: u16 = 0x1011;
const RT_PERSIST_DIRECTORY: u16 = 0x1772;

const ESC_SP: u16 = 0xF00A;
const ESC_OPT: u16 = 0xF00B;
const ESC_CLIENT_TEXTBOX: u16 = 0xF00D;
const ESC_SP_CONTAINER: u16 = 0xF004;

/// `Current User` header token of an unencrypted presentation.
const TOKEN_PLAIN: u32 = 0xE391_C05F;
/// `Current User` header token of an encrypted presentation.
const TOKEN_ENCRYPTED: u32 = 0xF3D1_C4DF;

fn malformed(why: &str) -> PreviewError {
    PreviewError::Malformed(format!("PowerPoint presentation: {why}"))
}

fn usz(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// One record: 8-byte header and body.
#[derive(Clone, Copy)]
struct Rec<'a> {
    /// `recVer`; `0xF` marks a container.
    ver: u8,
    /// `recInstance`.
    inst: u16,
    /// `recType`.
    typ: u16,
    data: &'a [u8],
}

impl<'a> Rec<'a> {
    fn children(&self) -> Recs<'a> {
        Recs { buf: self.data, pos: 0 }
    }
}

/// Iterates the records of a buffer; stops at the first header that does not fit.
struct Recs<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for Recs<'a> {
    type Item = Rec<'a>;

    fn next(&mut self) -> Option<Rec<'a>> {
        let head = slice(self.buf, self.pos, 8)?;
        let ver_inst = u16_le(head, 0)?;
        let typ = u16_le(head, 2)?;
        let len = usz(u32_le(head, 4)?);
        let start = self.pos.checked_add(8)?;
        let end = start.checked_add(len)?;
        let data = self.buf.get(start..end)?;
        self.pos = end;
        Some(Rec { ver: u8::try_from(ver_inst & 0xF).unwrap_or(0), inst: ver_inst >> 4, typ, data })
    }
}

/// The record that starts at `offset`.
fn record_at(buf: &[u8], offset: usize) -> Option<Rec<'_>> {
    Recs { buf, pos: offset }.next()
}

// ---------------------------------------------------------------------------------------------
// Text

/// Paragraph formatting of a run of characters.
#[derive(Debug, Clone, Copy, Default)]
struct PfRun {
    /// Characters covered.
    count: usize,
    indent: u8,
    /// Explicit bullet setting, if the run overrides the default.
    bullet: Option<bool>,
    align: Option<u16>,
}

/// The text of one text box or placeholder.
#[derive(Debug, Clone, Default)]
struct TextItem {
    /// `TextHeaderAtom` type: 0 title, 1 body, 2 notes, 4 other, 5 centre body, 6 centre title…
    ty: u32,
    units: Vec<u16>,
    pf: Vec<PfRun>,
    /// Positions of characters that stand for a field (slide number, date, …) and its label.
    fields: Vec<(usize, &'static str)>,
}

#[derive(Debug, Clone)]
enum Item {
    Text(TextItem),
    /// A shape's text lives in the document's text list at this index.
    Ref(usize),
}

/// Reads the paragraph formatting of a `StyleTextPropAtom`: indent level, bullet and alignment.
///
/// Stops quietly at anything it cannot follow; the text is shown either way.
fn parse_pf(data: &[u8], text_units: usize) -> Vec<PfRun> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut covered = 0usize;
    // The paragraph runs cover the text and its terminating paragraph mark.
    while covered <= text_units && out.len() < MAX_FORMAT_RUNS {
        let (Some(count), Some(indent), Some(mask)) = (
            u32_le(data, pos),
            u16_le(data, pos.saturating_add(4)),
            u32_le(data, pos.saturating_add(6)),
        ) else {
            break;
        };
        pos = pos.saturating_add(10);
        let mut run = PfRun {
            count: usz(count),
            indent: u8::try_from(indent.min(8)).unwrap_or(0),
            ..PfRun::default()
        };
        let mut ok = true;
        let mut take = |n: usize, pos: &mut usize| -> Option<usize> {
            let at = *pos;
            *pos = pos.checked_add(n)?;
            if *pos > data.len() {
                ok = false;
                return None;
            }
            Some(at)
        };
        if mask & 0x0F != 0
            && let Some(at) = take(2, &mut pos)
            && mask & 1 != 0
        {
            run.bullet = Some(u16_le(data, at).unwrap_or(0) & 1 != 0);
        }
        for (bit, size) in [(0x0080u32, 2usize), (0x0010, 2), (0x0040, 2), (0x0020, 4)] {
            if mask & bit != 0 {
                take(size, &mut pos);
            }
        }
        if mask & 0x0800 != 0
            && let Some(at) = take(2, &mut pos)
        {
            run.align = u16_le(data, at);
        }
        for bit in [0x1000u32, 0x2000, 0x4000, 0x0100, 0x0400, 0x8000] {
            if mask & bit != 0 {
                take(2, &mut pos);
            }
        }
        if mask & 0x0010_0000 != 0
            && let Some(at) = take(2, &mut pos)
        {
            let stops = usize::from(u16_le(data, at).unwrap_or(0));
            take(stops.saturating_mul(4), &mut pos);
        }
        if mask & 0x0001_0000 != 0 {
            take(2, &mut pos);
        }
        if mask & 0x000E_0000 != 0 {
            take(2, &mut pos);
        }
        if mask & 0x0020_0000 != 0 {
            take(2, &mut pos);
        }
        if !ok {
            break;
        }
        covered = covered.saturating_add(run.count.max(1));
        out.push(run);
    }
    out
}

/// Collects the text atoms of a text box, or of one entry of a text list.
fn read_items<'a>(recs: impl Iterator<Item = Rec<'a>>) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut ty: Option<u32> = None;
    for r in recs {
        match r.typ {
            RT_TEXT_HEADER => ty = u32_le(r.data, 0),
            RT_TEXT_CHARS => {
                let units: Vec<u16> = r
                    .data
                    .chunks_exact(2)
                    .take(MAX_UNITS)
                    .map(|c| {
                        u16::from_le_bytes([
                            c.first().copied().unwrap_or(0),
                            c.get(1).copied().unwrap_or(0),
                        ])
                    })
                    .collect();
                items.push(Item::Text(TextItem {
                    ty: ty.take().unwrap_or(4),
                    units,
                    pf: Vec::new(),
                    fields: Vec::new(),
                }));
            }
            RT_TEXT_BYTES => {
                let units: Vec<u16> =
                    r.data.iter().take(MAX_UNITS).map(|&b| u16::from(b)).collect();
                items.push(Item::Text(TextItem {
                    ty: ty.take().unwrap_or(4),
                    units,
                    pf: Vec::new(),
                    fields: Vec::new(),
                }));
            }
            RT_STYLE_TEXT => {
                if let Some(Item::Text(t)) = items.last_mut()
                    && t.pf.is_empty()
                {
                    t.pf = parse_pf(r.data, t.units.len());
                }
            }
            RT_OUTLINE_REF => {
                if let Some(i) = u32_le(r.data, 0) {
                    items.push(Item::Ref(usz(i)));
                }
            }
            RT_SLIDE_NUMBER_FIELD
            | RT_DATE_FIELD
            | RT_GENERIC_DATE_FIELD
            | RT_HEADER_FIELD
            | RT_FOOTER_FIELD
            | RT_RTF_DATE_FIELD => {
                let label = match r.typ {
                    RT_SLIDE_NUMBER_FIELD => "‹#›",
                    RT_HEADER_FIELD => "‹header›",
                    RT_FOOTER_FIELD => "‹footer›",
                    _ => "‹date›",
                };
                if let Some(Item::Text(t)) = items.last_mut()
                    && let Some(pos) = u32_le(r.data, 0)
                    && t.fields.len() < MAX_FORMAT_RUNS
                {
                    t.fields.push((usz(pos), label));
                }
            }
            _ => {}
        }
    }
    items
}

/// One entry of a `SlideListWithTexts`: a slide or notes page and its texts.
#[derive(Debug, Clone, Default)]
struct TextGroup {
    /// Persist id of the slide or notes record.
    persist: u32,
    /// `slideId`, which `SlideAtom.notesIdRef` refers to.
    slide_id: u32,
    items: Vec<Item>,
}

type TextList = Vec<TextGroup>;

fn read_text_list(rec: &Rec<'_>) -> TextList {
    let mut groups: Vec<(u32, u32, Vec<Rec<'_>>)> = Vec::new();
    for r in rec.children().take(MAX_RECORDS) {
        if r.typ == RT_SLIDE_PERSIST {
            if groups.len() >= MAX_SLIDES {
                break;
            }
            groups.push((
                u32_le(r.data, 0).unwrap_or(0),
                u32_le(r.data, 12).unwrap_or(0),
                Vec::new(),
            ));
        } else if let Some(g) = groups.last_mut() {
            g.2.push(r);
        }
    }
    groups
        .into_iter()
        .map(|(persist, slide_id, recs)| TextGroup {
            persist,
            slide_id,
            items: read_items(recs.into_iter()),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Slides

/// A shape's text with the shape's flags.
#[derive(Debug, Default)]
struct Shape {
    hidden: bool,
    deleted: bool,
    picture: bool,
    items: Vec<Item>,
}

/// Reads the shapes of a drawing in document order.
fn walk_drawing(data: &[u8], depth: usize, budget: &mut usize, out: &mut Vec<Shape>) {
    if depth > MAX_DEPTH {
        return;
    }
    for r in (Recs { buf: data, pos: 0 }) {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        if r.ver != 0xF {
            continue;
        }
        if r.typ == ESC_SP_CONTAINER {
            out.push(read_shape(&r));
        } else {
            walk_drawing(r.data, depth + 1, budget, out);
        }
    }
}

fn read_shape(sp: &Rec<'_>) -> Shape {
    let mut shape = Shape::default();
    for r in sp.children() {
        match r.typ {
            ESC_SP => {
                // fDeleted
                shape.deleted |= u32_le(r.data, 4).is_some_and(|f| f & 0x08 != 0);
            }
            ESC_OPT => {
                let count = usize::from(r.inst).min(r.data.len() / 6);
                for i in 0..count {
                    let at = i * 6;
                    let (Some(opid), Some(op)) = (u16_le(r.data, at), u32_le(r.data, at + 2))
                    else {
                        break;
                    };
                    match opid & 0x3FFF {
                        // Group-shape booleans: fHidden with its "use" bit.
                        0x03BF => shape.hidden |= op & 0x0002_0002 == 0x0002_0002,
                        // pib: the shape shows a picture.
                        0x0104 => shape.picture = true,
                        _ => {}
                    }
                }
            }
            ESC_CLIENT_TEXTBOX => shape.items.extend(read_items(r.children())),
            _ => {}
        }
    }
    shape
}

#[derive(Debug, Default)]
struct SlideText {
    hidden: bool,
    notes_ref: u32,
    shapes: Vec<Shape>,
}

fn read_slide(rec: &Rec<'_>, budget: &mut usize) -> SlideText {
    let mut s = SlideText::default();
    for r in rec.children() {
        match r.typ {
            RT_SLIDE_ATOM => s.notes_ref = u32_le(r.data, 16).unwrap_or(0),
            // fHidden
            RT_SLIDE_SHOW_INFO => s.hidden = u16_le(r.data, 10).is_some_and(|f| f & 0x0004 != 0),
            RT_PP_DRAWING => walk_drawing(r.data, 0, budget, &mut s.shapes),
            _ => {}
        }
    }
    s
}

fn read_notes(rec: &Rec<'_>, budget: &mut usize) -> Vec<Shape> {
    let mut shapes = Vec::new();
    for r in rec.children() {
        if r.typ == RT_PP_DRAWING {
            walk_drawing(r.data, 0, budget, &mut shapes);
        }
    }
    shapes
}

/// The text of the paragraph `start..end` with field characters replaced by their labels.
/// The flag is set if nothing but fields (and blanks) is in the paragraph.
fn paragraph_text(t: &TextItem, start: usize, end: usize) -> (String, bool) {
    let units = t.units.get(start..end).unwrap_or(&[]);
    if t.fields.is_empty() {
        return (String::from_utf16_lossy(units).replace('\u{B}', "\n"), false);
    }
    let mut out = String::new();
    let mut plain = String::new();
    let mut run: Vec<u16> = Vec::new();
    let flush = |run: &mut Vec<u16>, out: &mut String, plain: &mut String| {
        if !run.is_empty() {
            let s = String::from_utf16_lossy(run).replace('\u{B}', "\n");
            plain.push_str(&s);
            out.push_str(&s);
            run.clear();
        }
    };
    for (i, &u) in units.iter().enumerate() {
        if let Some((_, label)) = t.fields.iter().find(|(p, _)| *p == start + i) {
            flush(&mut run, &mut out, &mut plain);
            out.push_str(label);
        } else {
            run.push(u);
        }
    }
    flush(&mut run, &mut out, &mut plain);
    let only = plain.trim().is_empty();
    (out, only)
}

/// Appends the paragraphs of a text item.
fn text_blocks(t: &TextItem, bits: u16, notes: bool, out: &mut Vec<(bool, Block)>) {
    let units = &t.units;
    let mut start = 0usize;
    // Index of the formatting run that covers `start`, and where it ends.
    let mut run_idx = 0usize;
    let mut run_end = t.pf.first().map_or(0, |r| r.count.max(1));
    while start <= units.len() {
        // A paragraph runs to the next paragraph mark.
        let end = units
            .get(start..)
            .and_then(|rest| rest.iter().position(|&u| u == 0x0D))
            .map_or(units.len(), |p| start + p);
        while run_idx < t.pf.len() && start >= run_end {
            run_idx += 1;
            run_end = run_end.saturating_add(t.pf.get(run_idx).map_or(0, |r| r.count.max(1)));
        }
        let pf = t.pf.get(run_idx).copied();
        let (text, only_fields) = paragraph_text(t, start, end);
        start = end.saturating_add(1);
        // A paragraph that is nothing but a page number or a date carries no content.
        if text.trim().is_empty() || only_fields {
            continue;
        }
        let mut b = ParaBuilder::new();
        b.text(&text, bits, "");
        let is_title = !notes && matches!(t.ty, 0 | 6);
        let st = if notes {
            ParaStyle::Note
        } else {
            match t.ty {
                0 | 6 => ParaStyle::Title,
                5 => ParaStyle::Subtitle,
                _ => ParaStyle::Body,
            }
        };
        let mut para: Paragraph = b.take(st);
        if let Some(a) = pf.and_then(|p| p.align) {
            para.align = match a {
                1 => Align::Center,
                2 => Align::End,
                3 => Align::Justify,
                _ => Align::Start,
            };
        }
        let default_bullet = !notes && matches!(t.ty, 1 | 7 | 8);
        if pf.and_then(|p| p.bullet).unwrap_or(default_bullet) && st == ParaStyle::Body {
            para.marker = "•".to_owned();
            para.level = pf.map_or(1, |p| p.indent.saturating_add(1));
        }
        out.push((is_title, Block::Para(para)));
    }
}

/// Turns the shapes of a slide or notes page into blocks. Text that no shape refers to follows.
fn shape_blocks(
    shapes: &[Shape],
    outline: &[Item],
    notes: bool,
    images: &mut u64,
) -> Vec<(bool, Block)> {
    let mut out: Vec<(bool, Block)> = Vec::new();
    let mut used = vec![false; outline.len()];
    let emit = |shape: Option<&Shape>, t: &TextItem, out: &mut Vec<(bool, Block)>| {
        let mut bits = 0u16;
        if let Some(s) = shape {
            if s.hidden {
                bits |= style::HIDDEN;
            }
            if s.deleted {
                bits |= style::DELETED;
            }
        }
        text_blocks(t, bits, notes, out);
    };
    for shape in shapes {
        for item in &shape.items {
            match item {
                Item::Text(t) => emit(Some(shape), t, &mut out),
                Item::Ref(i) => {
                    if let Some(Item::Text(t)) = outline.get(*i)
                        && let Some(u) = used.get_mut(*i)
                        && !*u
                    {
                        *u = true;
                        emit(Some(shape), t, &mut out);
                    }
                }
            }
        }
        if shape.picture && !notes {
            *images = images.saturating_add(1);
            out.push((
                false,
                Block::Image(ImageInfo {
                    alt: String::new(),
                    media: String::new(),
                    bytes: 0,
                    external: false,
                }),
            ));
        }
    }
    for (item, u) in outline.iter().zip(&used) {
        if !*u && let Item::Text(t) = item {
            emit(None, t, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Edit chain

struct Edits {
    /// Persist id → offset of the record, newest revision wins.
    persist: BTreeMap<u32, usize>,
    /// Persist id of the document container in the newest revision.
    doc_ref: u32,
}

fn read_edits(stream: &[u8], current_edit: usize) -> Result<Option<Edits>, PreviewError> {
    let mut chain: Vec<(usize, u32)> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut offset = current_edit;
    while chain.len() < MAX_EDITS && seen.insert(offset) {
        let Some(r) = record_at(stream, offset) else { break };
        if r.typ != RT_USER_EDIT {
            break;
        }
        // An optional trailing persist id marks a presentation encrypted with a session key.
        if r.data.len() >= 0x20 && u32_le(r.data, 28).unwrap_or(0) != 0 {
            return Err(PreviewError::Encrypted);
        }
        let (Some(last), Some(dir), Some(doc)) =
            (u32_le(r.data, 8), u32_le(r.data, 12), u32_le(r.data, 16))
        else {
            break;
        };
        chain.push((usz(dir), doc));
        if last == 0 {
            break;
        }
        offset = usz(last);
    }
    let Some(&(_, doc_ref)) = chain.first() else { return Ok(None) };
    let mut persist = BTreeMap::new();
    // Oldest first, so that newer revisions replace older ones.
    for &(dir, _) in chain.iter().rev() {
        let Some(r) = record_at(stream, dir) else { continue };
        if r.typ != RT_PERSIST_DIRECTORY {
            continue;
        }
        let mut pos = 0usize;
        while let Some(head) = u32_le(r.data, pos) {
            pos += 4;
            let first = head & 0x000F_FFFF;
            let count = usz(head >> 20);
            for i in 0..count {
                let Some(off) = u32_le(r.data, pos + i * 4) else { break };
                if persist.len() >= MAX_PERSIST {
                    break;
                }
                persist.insert(first.saturating_add(u32::try_from(i).unwrap_or(0)), usz(off));
            }
            pos = pos.saturating_add(count.saturating_mul(4));
        }
    }
    Ok(Some(Edits { persist, doc_ref }))
}

/// A slide with the notes page and text lists that belong to it.
struct Plan<'a> {
    offset: usize,
    slide: Rec<'a>,
    outline: Vec<Item>,
    notes: Option<(Rec<'a>, Vec<Item>)>,
}

#[derive(Default)]
struct DocInfo {
    slide_list: TextList,
    notes_list: TextList,
    links: Vec<(String, String)>,
    embedded: bool,
}

fn utf16_string(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .take(4_096)
        .map(|c| {
            u16::from_le_bytes([c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)])
        })
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn read_document(doc: &Rec<'_>) -> DocInfo {
    let mut info = DocInfo::default();
    for r in doc.children().take(MAX_RECORDS) {
        match r.typ {
            RT_LIST_WITH_TEXTS => match r.inst {
                0 => info.slide_list = read_text_list(&r),
                2 => info.notes_list = read_text_list(&r),
                _ => {}
            },
            RT_EX_OBJ_LIST => {
                for e in r.children().take(MAX_RECORDS) {
                    match e.typ {
                        RT_EX_HYPERLINK => {
                            if info.links.len() >= MAX_LINKS {
                                continue;
                            }
                            let (mut name, mut target, mut location) =
                                (String::new(), String::new(), String::new());
                            for c in e.children() {
                                if c.typ != RT_CSTRING {
                                    continue;
                                }
                                match c.inst {
                                    0 => name = utf16_string(c.data),
                                    1 => target = utf16_string(c.data),
                                    _ => location = utf16_string(c.data),
                                }
                            }
                            if !location.is_empty() && target.is_empty() {
                                target = location;
                            } else if !location.is_empty() {
                                target = format!("{target}#{location}");
                            }
                            if !target.is_empty() || !name.is_empty() {
                                info.links.push((name, target));
                            }
                        }
                        RT_EX_EMBED | RT_EX_CONTROL | RT_EX_OLE_STORAGE => info.embedded = true,
                        _ => {}
                    }
                }
            }
            RT_EX_OLE_STORAGE | RT_EX_EMBED => info.embedded = true,
            _ => {}
        }
    }
    info
}

/// Slides and notes as the newest revision lists them.
fn plan_from_edits<'a>(stream: &'a [u8], edits: &Edits, info: &DocInfo) -> Vec<Plan<'a>> {
    let mut out = Vec::new();
    for group in info.slide_list.iter().take(MAX_SLIDES) {
        let Some(&offset) = edits.persist.get(&group.persist) else { continue };
        let Some(slide) = record_at(stream, offset).filter(|r| r.typ == RT_SLIDE) else { continue };
        let notes_ref = slide
            .children()
            .find(|r| r.typ == RT_SLIDE_ATOM)
            .and_then(|r| u32_le(r.data, 16))
            .unwrap_or(0);
        // `notesIdRef` is the `slideId` of the notes page.
        let notes = (notes_ref != 0)
            .then(|| {
                let g = info.notes_list.iter().find(|g| g.slide_id == notes_ref)?;
                let off = *edits.persist.get(&g.persist)?;
                let rec = record_at(stream, off).filter(|r| r.typ == RT_NOTES)?;
                Some((rec, g.items.clone()))
            })
            .flatten();
        out.push(Plan { offset, slide, outline: group.items.clone(), notes });
    }
    out
}

/// Slides and notes in file order, for files whose edit chain is damaged.
fn plan_from_scan<'a>(stream: &'a [u8], info: &DocInfo) -> Vec<Plan<'a>> {
    let mut slides: Vec<(usize, Rec<'a>)> = Vec::new();
    let mut notes: Vec<Rec<'a>> = Vec::new();
    let mut it = Recs { buf: stream, pos: 0 };
    let mut n = 0usize;
    loop {
        let at = it.pos;
        let Some(r) = it.next() else { break };
        n += 1;
        if n > MAX_RECORDS || slides.len() >= MAX_SLIDES {
            break;
        }
        match r.typ {
            RT_SLIDE => slides.push((at, r)),
            RT_NOTES => notes.push(r),
            _ => {}
        }
    }
    let mut notes_iter = notes.into_iter();
    let mut notes_lists = info.notes_list.iter();
    slides
        .into_iter()
        .enumerate()
        .map(|(i, (offset, slide))| {
            let outline = info.slide_list.get(i).map(|g| g.items.clone()).unwrap_or_default();
            // Without the persist directory the owner of a notes page is unknown: pair by order.
            let has_notes = slide
                .children()
                .find(|r| r.typ == RT_SLIDE_ATOM)
                .and_then(|r| u32_le(r.data, 16))
                .unwrap_or(0)
                != 0;
            let notes = has_notes
                .then(|| {
                    let rec = notes_iter.next()?;
                    let items = notes_lists.next().map(|g| g.items.clone()).unwrap_or_default();
                    Some((rec, items))
                })
                .flatten();
            Plan { offset, slide, outline, notes }
        })
        .collect()
}

/// Offsets of every slide record in the stream, current or not.
fn all_slide_offsets(stream: &[u8]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let mut it = Recs { buf: stream, pos: 0 };
    let mut n = 0usize;
    loop {
        let at = it.pos;
        let Some(r) = it.next() else { break };
        n += 1;
        if n > MAX_RECORDS {
            break;
        }
        if r.typ == RT_SLIDE {
            out.insert(at);
        }
    }
    out
}

/// Extracts a legacy PowerPoint presentation into `sink`.
pub(crate) fn extract(cf: &CompoundFile<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let idx = cf
        .find("PowerPoint Document")
        .ok_or_else(|| malformed("the PowerPoint Document stream is missing"))?;
    let stream = cf
        .read_stream(idx)
        .map_err(|e| PreviewError::Malformed(format!("PowerPoint presentation: {e}")))?;

    // The `Current User` stream says whether the file is encrypted and where the newest
    // revision begins.
    let mut current_edit: Option<usize> = None;
    if let Some(cu) = cf.find("Current User")
        && let Ok(user) = cf.read_stream(cu)
    {
        match u32_le(&user, 12) {
            Some(TOKEN_ENCRYPTED) => return Err(PreviewError::Encrypted),
            Some(TOKEN_PLAIN) => current_edit = u32_le(&user, 16).map(usz),
            _ => {}
        }
    }

    let edits = match current_edit {
        Some(off) => read_edits(&stream, off)?,
        None => None,
    };
    let from_chain = edits
        .as_ref()
        .and_then(|e| e.persist.get(&e.doc_ref).copied())
        .and_then(|off| record_at(&stream, off))
        .filter(|r| r.typ == RT_DOCUMENT);
    let (doc_rec, edits) = match from_chain {
        Some(r) => (Some(r), edits),
        // Damaged or missing chain: the document container is a top-level record.
        None => {
            (Recs { buf: &stream, pos: 0 }.take(MAX_RECORDS).find(|r| r.typ == RT_DOCUMENT), None)
        }
    };
    let info = doc_rec.as_ref().map(read_document).unwrap_or_default();

    let mut plans = match edits.as_ref() {
        Some(e) => plan_from_edits(&stream, e, &info),
        None => Vec::new(),
    };
    let mut best_effort = false;
    if plans.is_empty() {
        plans = plan_from_scan(&stream, &info);
        best_effort = !plans.is_empty();
    }
    if best_effort {
        sink.note(NoticeKind::BestEffort, "damaged presentation structure");
    }

    let mut budget = MAX_RECORDS;
    let mut images = 0u64;
    for plan in &plans {
        if sink.is_full() {
            break;
        }
        let slide = read_slide(&plan.slide, &mut budget);
        let mut blocks = shape_blocks(&slide.shapes, &plan.outline, false, &mut images);
        if let Some((notes_rec, notes_items)) = &plan.notes {
            let shapes = read_notes(notes_rec, &mut budget);
            let notes: Vec<(bool, Block)> = shape_blocks(&shapes, notes_items, true, &mut images)
                .into_iter()
                .filter(|(_, b)| !matches!(b, Block::Image(_)))
                .collect();
            // Titles first, then the rest in document order, then the notes.
            let (titles, rest): (Vec<_>, Vec<_>) = blocks.drain(..).partition(|(t, _)| *t);
            blocks = titles.into_iter().chain(rest).chain(notes).collect();
        } else {
            let (titles, rest): (Vec<_>, Vec<_>) = blocks.drain(..).partition(|(t, _)| *t);
            blocks = titles.into_iter().chain(rest).collect();
        }
        let name = blocks
            .iter()
            .find_map(|(is_title, b)| match b {
                Block::Para(p) if *is_title && !p.is_blank() => {
                    Some(p.text().chars().take(80).collect::<String>())
                }
                _ => None,
            })
            .or_else(|| {
                blocks.iter().find_map(|(_, b)| match b {
                    Block::Para(p) if p.style != ParaStyle::Note && !p.is_blank() => {
                        Some(p.text().chars().take(80).collect::<String>())
                    }
                    _ => None,
                })
            })
            .unwrap_or_default();
        let Some(sec) = sink.open_section(SectionKind::Slide, &name, slide.hidden) else { break };
        for (_, b) in blocks {
            let pushed = match b {
                Block::Image(i) => sink.image(sec, i),
                other => sink.push(sec, other),
            };
            if !pushed {
                break;
            }
        }
    }

    // Slides that older saves left behind.
    if !best_effort {
        let current: BTreeSet<usize> = plans.iter().map(|p| p.offset).collect();
        let stale = all_slide_offsets(&stream).difference(&current).count();
        if stale > 0 {
            sink.note_detail(NoticeKind::Skipped, format!("older revisions of slides: {stale}"));
        }
    }

    if !info.links.is_empty()
        && !sink.is_full()
        && let Some(sec) = sink.open_section(SectionKind::Notes, "Hyperlinks", false)
    {
        for (name, target) in &info.links {
            let mut b = ParaBuilder::new();
            if name.is_empty() || name == target {
                b.text(target, 0, target);
            } else if target.is_empty() {
                b.text(name, 0, "");
            } else {
                b.text(&format!("{name} → {target}"), 0, target);
            }
            if !sink.para(sec, b.take(ParaStyle::Body)) {
                break;
            }
        }
    }

    if info.embedded {
        sink.note(NoticeKind::Unsupported, "embedded objects and macros");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewPolicy;
    use crate::model::{FileFormat, Preview};
    use crate::testkit::{Lcg, fuzz_iterations, ok, paragraphs, run, section_of};
    use arca_container::cfb::writer::CfbBuilder;

    fn rec(ver: u8, inst: u16, typ: u16, data: &[u8]) -> Vec<u8> {
        let mut o = (u16::from(ver) | (inst << 4)).to_le_bytes().to_vec();
        o.extend(typ.to_le_bytes());
        o.extend(u32::try_from(data.len()).expect("len").to_le_bytes());
        o.extend(data);
        o
    }

    fn container(inst: u16, typ: u16, children: &[Vec<u8>]) -> Vec<u8> {
        rec(0xF, inst, typ, &children.concat())
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn header(ty: u32) -> Vec<u8> {
        rec(0, 0, RT_TEXT_HEADER, &ty.to_le_bytes())
    }

    fn chars(s: &str) -> Vec<u8> {
        rec(0, 0, RT_TEXT_CHARS, &utf16(s))
    }

    fn bytes(b: &[u8]) -> Vec<u8> {
        rec(0, 0, RT_TEXT_BYTES, b)
    }

    fn outline_ref(i: u32) -> Vec<u8> {
        rec(0, 0, RT_OUTLINE_REF, &i.to_le_bytes())
    }

    /// A text with its header: the usual `TextHeaderAtom` + `TextCharsAtom` pair.
    fn text(ty: u32, s: &str) -> Vec<Vec<u8>> {
        vec![header(ty), chars(s)]
    }

    fn textbox(items: Vec<Vec<u8>>) -> Vec<u8> {
        container(0, ESC_CLIENT_TEXTBOX, &items)
    }

    fn shape_with(props: &[(u16, u32)], flags: u32, children: Vec<Vec<u8>>) -> Vec<u8> {
        let mut sp = 1u32.to_le_bytes().to_vec();
        sp.extend(flags.to_le_bytes());
        let mut opt = Vec::new();
        for (id, v) in props {
            opt.extend(id.to_le_bytes());
            opt.extend(v.to_le_bytes());
        }
        let mut kids = vec![
            rec(2, 0, ESC_SP, &sp),
            rec(3, u16::try_from(props.len()).expect("n"), ESC_OPT, &opt),
        ];
        kids.extend(children);
        container(0, ESC_SP_CONTAINER, &kids)
    }

    fn shape(items: Vec<Vec<u8>>) -> Vec<u8> {
        shape_with(&[], 0, vec![textbox(items)])
    }

    fn drawing(shapes: Vec<Vec<u8>>) -> Vec<u8> {
        container(0, RT_PP_DRAWING, &[container(0, 0xF002, &[container(0, 0xF003, &shapes)])])
    }

    fn slide(notes_id: u32, hidden: bool, shapes: Vec<Vec<u8>>) -> Vec<u8> {
        let mut atom = vec![0u8; 24];
        atom[16..20].copy_from_slice(&notes_id.to_le_bytes());
        let mut info = vec![0u8; 16];
        if hidden {
            info[10] = 4;
        }
        container(
            0,
            RT_SLIDE,
            &[
                rec(2, 0, RT_SLIDE_ATOM, &atom),
                rec(0, 0, RT_SLIDE_SHOW_INFO, &info),
                drawing(shapes),
            ],
        )
    }

    fn notes_page(shapes: Vec<Vec<u8>>) -> Vec<u8> {
        container(0, RT_NOTES, &[drawing(shapes)])
    }

    fn persist_atom(persist: u32, slide_id: u32) -> Vec<u8> {
        let mut d = persist.to_le_bytes().to_vec();
        d.extend([0u8; 8]);
        d.extend(slide_id.to_le_bytes());
        d.extend([0u8; 4]);
        rec(0, 0, RT_SLIDE_PERSIST, &d)
    }

    /// `(persist id, slide id, text records)` per slide or notes page.
    type Group = (u32, u32, Vec<Vec<u8>>);

    fn text_list(inst: u16, groups: Vec<Group>) -> Vec<u8> {
        let mut kids = Vec::new();
        for (persist, slide_id, items) in groups {
            kids.push(persist_atom(persist, slide_id));
            kids.extend(items);
        }
        container(inst, RT_LIST_WITH_TEXTS, &kids)
    }

    fn cstring(inst: u16, s: &str) -> Vec<u8> {
        rec(0, inst, RT_CSTRING, &utf16(s))
    }

    fn hyperlink(id: u32, name: &str, target: &str) -> Vec<u8> {
        container(
            0,
            RT_EX_HYPERLINK,
            &[rec(0, 0, 0x0FD3, &id.to_le_bytes()), cstring(0, name), cstring(1, target)],
        )
    }

    /// Writes records and revisions the way PowerPoint does: records, then a persist directory
    /// and a `UserEditAtom` that point at them.
    struct Deck {
        out: Vec<u8>,
        last_edit: u32,
    }

    impl Deck {
        fn new() -> Self {
            Self { out: Vec::new(), last_edit: 0 }
        }

        fn put(&mut self, record: &[u8]) -> usize {
            let at = self.out.len();
            self.out.extend(record);
            at
        }

        /// Commits a revision whose persist directory holds `entries` (id, offset).
        fn commit(&mut self, doc_ref: u32, entries: &[(u32, usize)], encrypt_ref: Option<u32>) {
            let mut dir = Vec::new();
            for (id, off) in entries {
                dir.extend((id | (1 << 20)).to_le_bytes());
                dir.extend(u32::try_from(*off).expect("off").to_le_bytes());
            }
            let dir_at = self.put(&rec(0, 0, RT_PERSIST_DIRECTORY, &dir));
            let mut ue = vec![0u8; 8];
            ue.extend(self.last_edit.to_le_bytes());
            ue.extend(u32::try_from(dir_at).expect("dir").to_le_bytes());
            ue.extend(doc_ref.to_le_bytes());
            ue.extend([0u8; 8]);
            if let Some(e) = encrypt_ref {
                ue.extend(e.to_le_bytes());
            }
            let at = self.put(&rec(0, 0, RT_USER_EDIT, &ue));
            self.last_edit = u32::try_from(at).expect("edit");
        }
    }

    fn current_user(token: u32, edit: u32) -> Vec<u8> {
        let mut d = 0x14u32.to_le_bytes().to_vec();
        d.extend(token.to_le_bytes());
        d.extend(edit.to_le_bytes());
        d.extend([0u8; 8]);
        rec(0, 0, 0x0FF6, &d)
    }

    fn file(stream: Vec<u8>, user: Option<Vec<u8>>) -> Vec<u8> {
        let mut b = CfbBuilder::new();
        assert!(b.add_stream("PowerPoint Document", stream).is_ok());
        if let Some(u) = user {
            assert!(b.add_stream("Current User", u).is_ok());
        }
        b.finish().unwrap_or_default()
    }

    /// `(hidden, shapes, notes shapes)` per slide; the notes page has slide id `100 + n`.
    type SlideSpec = (bool, Vec<Vec<u8>>, Option<Vec<Vec<u8>>>);

    fn deck(slides: Vec<SlideSpec>, doc_extra: Vec<Vec<u8>>) -> Vec<u8> {
        let mut d = Deck::new();
        let mut entries = Vec::new();
        let mut slide_groups: Vec<Group> = Vec::new();
        let mut notes_groups: Vec<Group> = Vec::new();
        let mut next = 2u32;
        for (i, (hidden, shapes, notes)) in slides.into_iter().enumerate() {
            let notes_slide_id = notes.as_ref().map_or(0, |_| 100 + u32::try_from(i).expect("i"));
            let sid = next;
            next += 1;
            let at = d.put(&slide(notes_slide_id, hidden, shapes));
            entries.push((sid, at));
            slide_groups.push((sid, 10 + u32::try_from(i).expect("i"), Vec::new()));
            if let Some(n) = notes {
                let nid = next;
                next += 1;
                let at = d.put(&notes_page(n));
                entries.push((nid, at));
                notes_groups.push((nid, notes_slide_id, Vec::new()));
            }
        }
        let mut kids = vec![text_list(0, slide_groups), text_list(2, notes_groups)];
        kids.extend(doc_extra);
        let at = d.put(&container(0, RT_DOCUMENT, &kids));
        entries.push((1, at));
        d.commit(1, &entries, None);
        let cu = current_user(TOKEN_PLAIN, d.last_edit);
        file(d.out, Some(cu))
    }

    /// `(hidden, shapes, text list)` per slide.
    type ListedSlide = (bool, Vec<Vec<u8>>, Vec<Vec<u8>>);

    fn doc_with_lists(slides: Vec<ListedSlide>, notes_lists: Vec<Vec<Vec<u8>>>) -> Vec<u8> {
        // Slides whose placeholder text lives in the document's text list.
        let mut d = Deck::new();
        let mut entries = Vec::new();
        let mut slide_groups: Vec<Group> = Vec::new();
        let mut notes_groups: Vec<Group> = Vec::new();
        let mut next = 2u32;
        for (i, (hidden, shapes, list)) in slides.into_iter().enumerate() {
            let sid = next;
            next += 1;
            let has_notes = notes_lists.get(i).is_some_and(|n| !n.is_empty());
            let nsid = if has_notes { 100 + u32::try_from(i).expect("i") } else { 0 };
            let at = d.put(&slide(nsid, hidden, shapes));
            entries.push((sid, at));
            slide_groups.push((sid, 10 + u32::try_from(i).expect("i"), list));
            if has_notes {
                let nid = next;
                next += 1;
                let at = d.put(&notes_page(vec![shape(vec![outline_ref(0)])]));
                entries.push((nid, at));
                notes_groups.push((nid, nsid, notes_lists.get(i).cloned().unwrap_or_default()));
            }
        }
        let at = d.put(&container(
            0,
            RT_DOCUMENT,
            &[text_list(0, slide_groups), text_list(2, notes_groups)],
        ));
        entries.push((1, at));
        d.commit(1, &entries, None);
        let cu = current_user(TOKEN_PLAIN, d.last_edit);
        file(d.out, Some(cu))
    }

    fn slides_of(p: &Preview) -> Vec<&crate::model::Section> {
        p.sections.iter().filter(|s| s.kind == SectionKind::Slide).collect()
    }

    fn lines(s: &crate::model::Section) -> Vec<(ParaStyle, String)> {
        paragraphs(s).into_iter().map(|p| (p.style, p.text())).collect()
    }

    #[test]
    fn titles_bullets_notes_and_hidden_slides() {
        let mut title = text(0, "Quarterly review");
        title.extend(text(5, "by the team"));
        let mut body = text(1, "First\rNested\rSecond");
        // Paragraph formatting: indent 0, indent 1, indent 0 (each run: count, indent, mask).
        let mut pf = Vec::new();
        for (count, indent) in [(6u32, 0u16), (7, 1), (7, 0)] {
            pf.extend(count.to_le_bytes());
            pf.extend(indent.to_le_bytes());
            pf.extend(0u32.to_le_bytes());
        }
        pf.extend(0u32.to_le_bytes());
        body.push(rec(0, 0, RT_STYLE_TEXT, &pf));
        let p = ok(&deck(
            vec![
                (
                    false,
                    vec![shape(title), shape(body)],
                    Some(vec![shape(text(2, "say this aloud"))]),
                ),
                (true, vec![shape(text(0, "Secret")), shape(text(4, "hidden slide text"))], None),
            ],
            vec![],
        ));
        assert_eq!(p.format, FileFormat::Ppt);
        let s = slides_of(&p);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "Quarterly review");
        assert!(!s[0].hidden && s[1].hidden);
        let l = lines(s[0]);
        assert_eq!(l[0], (ParaStyle::Title, "Quarterly review".into()));
        assert!(l.contains(&(ParaStyle::Subtitle, "by the team".into())));
        assert!(l.contains(&(ParaStyle::Note, "say this aloud".into())));
        let body: Vec<_> =
            paragraphs(s[0]).into_iter().filter(|p| p.style == ParaStyle::Body).collect();
        assert_eq!(
            body.iter().map(|p| (p.marker.as_str(), p.level, p.text())).collect::<Vec<_>>(),
            vec![
                ("•", 1, "First".to_owned()),
                ("•", 2, "Nested".to_owned()),
                ("•", 1, "Second".to_owned())
            ]
        );
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "slides")
        );
        // Notes come last, after the content.
        assert_eq!(l.last().map(|x| x.0), Some(ParaStyle::Note));
    }

    #[test]
    fn hidden_slides_are_omitted_when_the_policy_says_so() {
        let data = deck(
            vec![
                (false, vec![shape(text(0, "Shown"))], None),
                (true, vec![shape(text(0, "Secret"))], None),
            ],
            vec![],
        );
        let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert_eq!(slides_of(&p).len(), 1);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted));
    }

    #[test]
    fn text_in_the_document_list_and_unreferenced_text() {
        // The slide's title shape points at list entry 1; entries 0 and 2 are not referenced.
        let list: Vec<Vec<u8>> =
            [text(4, "orphan text"), text(0, "From the list"), text(1, "also orphan")].concat();
        let p = ok(&doc_with_lists(
            vec![(false, vec![shape(vec![outline_ref(1)])], list)],
            vec![vec![]],
        ));
        let s = slides_of(&p);
        assert_eq!(s.len(), 1);
        let l = lines(s[0]);
        assert_eq!(l[0], (ParaStyle::Title, "From the list".into()));
        let texts: Vec<_> = l.iter().map(|x| x.1.as_str()).collect();
        assert!(texts.contains(&"orphan text") && texts.contains(&"also orphan"), "{texts:?}");
        assert_eq!(s[0].name, "From the list");
    }

    #[test]
    fn notes_are_found_through_the_slide_id() {
        let slide_list = text(0, "Title");
        let notes_list = text(2, "Remember to breathe");
        let p = ok(&doc_with_lists(
            vec![(false, vec![shape(vec![outline_ref(0)])], slide_list)],
            vec![notes_list],
        ));
        let l = lines(slides_of(&p)[0]);
        assert!(l.contains(&(ParaStyle::Note, "Remember to breathe".into())), "{l:?}");
    }

    #[test]
    fn single_byte_text_and_line_breaks() {
        let items = vec![header(4), bytes(b"caf\xE9 \xFC\x0Bline two\rsecond paragraph\r\r")];
        let p = ok(&deck(vec![(false, vec![shape(items)], None)], vec![]));
        let l = lines(slides_of(&p)[0]);
        assert_eq!(l[0].1, "café ü\nline two");
        assert_eq!(l[1].1, "second paragraph");
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn field_characters_get_labels_and_page_numbers_are_dropped() {
        let page = shape(vec![
            header(4),
            chars("Page *"),
            rec(0, 0, RT_SLIDE_NUMBER_FIELD, &[5, 0, 0, 0]),
        ]);
        let only_number =
            shape(vec![header(4), chars("*"), rec(0, 0, RT_SLIDE_NUMBER_FIELD, &[0, 0, 0, 0])]);
        let date = shape(vec![
            header(4),
            chars("Updated *"),
            rec(0, 0, RT_DATE_FIELD, &[8, 0, 0, 0, 0, 0, 0, 0]),
        ]);
        let p = ok(&deck(vec![(false, vec![page, only_number, date], None)], vec![]));
        let l = lines(slides_of(&p)[0]);
        assert_eq!(
            l.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
            vec!["Page ‹#›", "Updated ‹date›"]
        );
    }

    #[test]
    fn hidden_and_deleted_shapes_are_labelled() {
        let hidden =
            shape_with(&[(0x03BF, 0x0002_0002)], 0, vec![textbox(text(4, "invisible text"))]);
        let not_hidden =
            shape_with(&[(0x03BF, 0x0000_0002)], 0, vec![textbox(text(4, "visible text"))]);
        let deleted = shape_with(&[], 0x08, vec![textbox(text(4, "removed text"))]);
        let p = ok(&deck(vec![(false, vec![hidden, not_hidden, deleted], None)], vec![]));
        let ps = paragraphs(slides_of(&p)[0]);
        let bits = |t: &str| ps.iter().find(|x| x.text() == t).map(|x| x.runs[0].style);
        assert_eq!(bits("invisible text").map(|b| b & style::HIDDEN), Some(style::HIDDEN));
        assert_eq!(bits("visible text").map(|b| b & (style::HIDDEN | style::DELETED)), Some(0));
        assert_eq!(bits("removed text").map(|b| b & style::DELETED), Some(style::DELETED));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown));
        let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
        let hidden =
            shape_with(&[(0x03BF, 0x0002_0002)], 0, vec![textbox(text(4, "invisible text"))]);
        let p = crate::preview(
            &deck(vec![(false, vec![hidden, shape(text(4, "shown"))], None)], vec![]),
            &policy,
        )
        .expect("preview");
        let all: String =
            slides_of(&p).iter().flat_map(|s| paragraphs(s)).map(|x| x.text()).collect();
        assert_eq!(all, "shown");
    }

    #[test]
    fn pictures_are_counted() {
        let pic = shape_with(&[(0x4104, 3)], 0, vec![]);
        let p = ok(&deck(vec![(false, vec![pic, shape(text(0, "T"))], None)], vec![]));
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::ImagesNotShown && n.count == 1));
    }

    #[test]
    fn hyperlinks_are_listed_with_their_targets() {
        let links = container(
            0,
            RT_EX_OBJ_LIST,
            &[
                hyperlink(1, "Click me", "https://evil.example/x"),
                hyperlink(2, "", "https://plain.example"),
                rec(0, 0, 0x0FD3, &[0; 4]),
            ],
        );
        let p = ok(&deck(vec![(false, vec![shape(text(0, "Title"))], None)], vec![links]));
        let s = p.sections.iter().find(|s| s.name == "Hyperlinks").expect("links");
        let ps = paragraphs(s);
        assert_eq!(ps[0].runs[0].href, "https://evil.example/x");
        assert!(
            ps[0].text().contains("Click me") && ps[0].text().contains("https://evil.example/x")
        );
        assert_eq!(ps[1].text(), "https://plain.example");
    }

    #[test]
    fn embedded_objects_are_flagged() {
        let extra = container(0, RT_EX_OBJ_LIST, &[container(0, RT_EX_EMBED, &[])]);
        let p = ok(&deck(vec![(false, vec![shape(text(0, "T"))], None)], vec![extra]));
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Unsupported && n.detail.contains("embedded"))
        );
    }

    #[test]
    fn the_newest_revision_wins_and_old_slides_are_counted() {
        let mut d = Deck::new();
        let s1 = d.put(&slide(0, false, vec![shape(text(0, "Original"))]));
        let s2 = d.put(&slide(0, false, vec![shape(text(0, "Second"))]));
        let doc1 = d.put(&container(
            0,
            RT_DOCUMENT,
            &[text_list(0, vec![(2, 10, vec![]), (3, 11, vec![])])],
        ));
        d.commit(1, &[(1, doc1), (2, s1), (3, s2)], None);
        // Second save: slide 2 is edited (new record, same persist id), the document is rewritten.
        let s2b = d.put(&slide(0, false, vec![shape(text(0, "Second, edited"))]));
        let doc2 = d.put(&container(
            0,
            RT_DOCUMENT,
            &[text_list(0, vec![(2, 10, vec![]), (3, 11, vec![])])],
        ));
        d.commit(1, &[(1, doc2), (3, s2b)], None);
        let cu = current_user(TOKEN_PLAIN, d.last_edit);
        let p = ok(&file(d.out, Some(cu)));
        let names: Vec<_> = slides_of(&p).iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, ["Original", "Second, edited"]);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::Skipped
                && n.detail.contains("older revisions")
                && n.detail.ends_with('1')),
            "{:?}",
            p.notices
        );
    }

    #[test]
    fn deleted_slides_are_not_shown_but_counted() {
        let mut d = Deck::new();
        let s1 = d.put(&slide(0, false, vec![shape(text(0, "Kept"))]));
        let _gone = d.put(&slide(0, false, vec![shape(text(0, "Deleted earlier"))]));
        let doc = d.put(&container(0, RT_DOCUMENT, &[text_list(0, vec![(2, 10, vec![])])]));
        d.commit(1, &[(1, doc), (2, s1)], None);
        let cu = current_user(TOKEN_PLAIN, d.last_edit);
        let p = ok(&file(d.out, Some(cu)));
        assert_eq!(slides_of(&p).len(), 1);
        assert_eq!(slides_of(&p)[0].name, "Kept");
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Skipped && n.detail.contains("older revisions"))
        );
    }

    #[test]
    fn encrypted_presentations_are_reported() {
        let mut d = Deck::new();
        let s1 = d.put(&slide(0, false, vec![shape(text(0, "Hi"))]));
        let doc = d.put(&container(0, RT_DOCUMENT, &[text_list(0, vec![(2, 10, vec![])])]));
        d.commit(1, &[(1, doc), (2, s1)], None);
        let encrypted_token = file(d.out.clone(), Some(current_user(TOKEN_ENCRYPTED, d.last_edit)));
        assert_eq!(run(&encrypted_token), Err(PreviewError::Encrypted));

        let mut d = Deck::new();
        let s1 = d.put(&slide(0, false, vec![shape(text(0, "Hi"))]));
        let doc = d.put(&container(0, RT_DOCUMENT, &[text_list(0, vec![(2, 10, vec![])])]));
        d.commit(1, &[(1, doc), (2, s1)], Some(7));
        let session = file(d.out.clone(), Some(current_user(TOKEN_PLAIN, d.last_edit)));
        assert_eq!(run(&session), Err(PreviewError::Encrypted));
    }

    #[test]
    fn a_damaged_edit_chain_falls_back_to_a_scan() {
        let mut d = Deck::new();
        d.put(&slide(0, false, vec![shape(text(0, "Found by scanning"))]));
        d.put(&container(0, RT_DOCUMENT, &[text_list(0, vec![(2, 10, vec![])])]));
        // No Current User stream and no persist directory.
        let p = ok(&file(d.out, None));
        assert_eq!(slides_of(&p).len(), 1);
        assert_eq!(slides_of(&p)[0].name, "Found by scanning");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort));
        // A Current User stream that points nowhere.
        let mut d = Deck::new();
        d.put(&slide(0, false, vec![shape(text(0, "Still found"))]));
        let p = ok(&file(d.out, Some(current_user(TOKEN_PLAIN, 0xFFFF_FF00))));
        assert_eq!(slides_of(&p).len(), 1);
    }

    #[test]
    fn broken_files_fail_cleanly() {
        assert!(matches!(run(&file(Vec::new(), None)), Err(PreviewError::Malformed(_)) | Ok(_)));
        let mut b = CfbBuilder::new();
        assert!(b.add_stream("Current User", vec![0; 30]).is_ok());
        assert!(b.add_stream("PowerPoint Document", vec![0xFF; 100]).is_ok());
        let _ = run(&b.finish().unwrap_or_default());
        // A cycle in the edit chain.
        let mut d = Deck::new();
        let doc = d.put(&container(0, RT_DOCUMENT, &[]));
        let mut ue = vec![0u8; 8];
        ue.extend(u32::try_from(d.out.len()).expect("n").to_le_bytes());
        ue.extend(0u32.to_le_bytes());
        ue.extend(1u32.to_le_bytes());
        ue.extend([0u8; 8]);
        let at = d.put(&rec(0, 0, RT_USER_EDIT, &ue));
        let _ = doc;
        let _ = run(&file(d.out, Some(current_user(TOKEN_PLAIN, u32::try_from(at).expect("at")))));
    }

    #[test]
    fn paragraph_formatting_is_read_defensively() {
        // One run: 5 characters, indent 2, mask = hasBullet | alignment, bullet off, centred.
        let mut d = 5u32.to_le_bytes().to_vec();
        d.extend(2u16.to_le_bytes());
        d.extend((0x0001u32 | 0x0800).to_le_bytes());
        d.extend(0u16.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(1u32.to_le_bytes()); // final run: 1 char
        d.extend(0u16.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        let runs = parse_pf(&d, 5);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[0].count, runs[0].indent, runs[0].bullet, runs[0].align),
            (5, 2, Some(false), Some(1))
        );
        // Every optional field present, then a bullet char: all skipped in the right order.
        let mut full = 3u32.to_le_bytes().to_vec();
        full.extend(1u16.to_le_bytes());
        let mask = 0x0F
            | 0x80
            | 0x10
            | 0x40
            | 0x20
            | 0x800
            | 0x1000
            | 0x2000
            | 0x4000
            | 0x100
            | 0x400
            | 0x8000
            | 0x10_0000
            | 0x1_0000
            | 0xE_0000
            | 0x20_0000;
        full.extend(u32::try_from(mask).expect("mask").to_le_bytes());
        full.extend(1u16.to_le_bytes()); // bulletFlags: bullet
        full.extend([0u8; 2 + 2 + 2 + 4]); // char, font, size, colour
        full.extend(2u16.to_le_bytes()); // alignment: right
        full.extend([0u8; 2 * 6]); // spacing x3, margins x2, tab size
        full.extend(2u16.to_le_bytes()); // two tab stops
        full.extend([0u8; 8]);
        full.extend([0u8; 2 * 3]); // font align, wrap flags, direction
        full.extend(1u32.to_le_bytes());
        full.extend(7u16.to_le_bytes()); // second run: indent 7
        full.extend(0u32.to_le_bytes());
        let runs = parse_pf(&full, 3);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!((runs[0].bullet, runs[0].align, runs[1].indent), (Some(true), Some(2), 7));
        // Truncated and hostile input: no panic, nothing invented.
        assert!(parse_pf(&full[..12], 3).len() <= 1);
        assert!(parse_pf(&[], 3).is_empty());
        let huge = [0xFF; 64];
        let _ = parse_pf(&huge, 1 << 20);
    }

    #[test]
    fn records_are_walked_safely() {
        // A record that claims more bytes than the buffer holds ends the walk.
        let mut data = rec(0, 0, 1, &[1, 2, 3]);
        data.extend([0x00, 0x00, 0x02, 0x00, 0xFF, 0xFF, 0xFF, 0x7F]);
        let all: Vec<_> = (Recs { buf: &data, pos: 0 }).collect();
        assert_eq!(all.len(), 1);
        assert!(record_at(&data, usize::MAX).is_none());
        assert!(record_at(&data, 5).is_none());
    }

    fn sample() -> (Vec<u8>, Vec<u8>) {
        let mut d = Deck::new();
        let s1 =
            d.put(&slide(0, false, vec![shape(text(0, "Fuzz title")), shape(text(1, "a\rb\rc"))]));
        let n = d.put(&notes_page(vec![shape(text(2, "note"))]));
        let s2 = d.put(&slide(100, true, vec![shape(vec![outline_ref(0)])]));
        let links = container(0, RT_EX_OBJ_LIST, &[hyperlink(1, "n", "https://x.example")]);
        let doc = d.put(&container(
            0,
            RT_DOCUMENT,
            &[
                text_list(0, vec![(2, 10, vec![]), (4, 11, text(4, "listed"))]),
                text_list(2, vec![(3, 100, vec![])]),
                links,
            ],
        ));
        d.commit(1, &[(1, doc), (2, s1), (3, n), (4, s2)], None);
        let cu = current_user(TOKEN_PLAIN, d.last_edit);
        (d.out, cu)
    }

    #[test]
    fn the_sample_deck_reads_as_expected() {
        let (stream, cu) = sample();
        let p = ok(&file(stream, Some(cu)));
        assert_eq!(slides_of(&p).len(), 2);
        assert!(section_of(&p, SectionKind::Slide).is_some());
        assert!(slides_of(&p)[1].hidden);
    }

    #[test]
    fn mutated_decks_never_panic() {
        let (stream, cu) = sample();
        let mut rng = Lcg::new(99);
        for i in 0..fuzz_iterations(800) {
            let mut m = stream.clone();
            let mut u = cu.clone();
            rng.mutate(&mut m, 1 + (i % 6));
            if i % 7 == 0 {
                rng.mutate(&mut u, 1);
            }
            if i % 11 == 0 {
                m.truncate(rng.below(m.len()));
            }
            let _ = run(&file(m, Some(u)));
        }
    }

    #[test]
    fn real_presentations_from_the_corpus() {
        let Ok(dir) = std::env::var("ARCA_CORPUS_DIR") else { return };
        let dir = std::path::Path::new(&dir);
        let (Ok(ppt), Ok(pptx)) =
            (std::fs::read(dir.join("slides.ppt")), std::fs::read(dir.join("slides.pptx")))
        else {
            return;
        };
        let (a, b) = (ok(&ppt), ok(&pptx));
        let texts = |p: &Preview| -> Vec<(String, Vec<String>)> {
            slides_of(p)
                .iter()
                .map(|s| (s.name.clone(), paragraphs(s).iter().map(|x| x.text()).collect()))
                .collect()
        };
        assert_eq!(texts(&a), texts(&b));
        // Mutations of the real file's stream.
        let cf = CompoundFile::parse(&ppt, PreviewPolicy::default().cfb).expect("cfb");
        let stream = cf.read_stream(cf.find("PowerPoint Document").expect("stream")).expect("read");
        let cu = cf.read_stream(cf.find("Current User").expect("cu")).expect("read");
        let mut rng = Lcg::new(3);
        for _ in 0..fuzz_iterations(300) {
            let mut m = stream.clone();
            rng.mutate(&mut m, 4);
            let _ = run(&file(m, Some(cu.clone())));
        }
    }
}
