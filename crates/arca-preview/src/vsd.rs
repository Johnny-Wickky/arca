//! Binary Visio drawings (`.vsd`): Visio 6 (2000) and the 2003–2010 generation.
//!
//! The file is an OLE2 container whose `VisioDocument` stream is a tree: a trailer points to
//! streams, some of which hold more pointers and some of which hold *chunks* (typed records).
//! Streams can be compressed with a small LZ scheme. A page is a chunk stream: a shape is a
//! chunk, its geometry and text follow it, and a shape list chunk names the shapes a page or a
//! group holds.
//!
//! What a reader needs from a diagram is shown, as for `.vsdx`: the text of every shape, in
//! reading order and with groups kept together, and a marker for pictures. Geometry, connections,
//! shape data, hyperlinks and pictures themselves are not read, and nothing is executed. Masters
//! (the stencil shapes) are not shown: they are the library the page draws from, not the drawing.
//!
//! The structure follows the public descriptions of the format (the HDGF documentation of Apache
//! POI and the VSDump notes it cites); it was checked against the text LibreOffice reads from the
//! same files.

use std::collections::{BTreeMap, BTreeSet};

use arca_container::bytes::{u16_le, u32_le};
use arca_container::cfb::CompoundFile;
use arca_container::text::{CodePage, decode_codepage, utf16le_lossy};

use crate::PreviewError;
use crate::model::{FileFormat, ImageInfo, NoticeKind, Paragraph, Run, SectionKind};
use crate::sink::Sink;

/// The first bytes of the stream.
const SIGNATURE: &[u8] = b"Visio (TM) Drawing\r\n";
/// Pointers in the trailer are at this offset.
const TRAILER_AT: usize = 0x24;
/// Size of a pointer.
const POINTER_SIZE: usize = 18;
/// Size of a chunk header.
const CHUNK_HEADER: usize = 19;

/// Most pointers in one container.
const MAX_POINTERS: usize = 100_000;
/// Most streams opened in one file.
const MAX_STREAMS: usize = 200_000;
/// Most pages shown.
const MAX_PAGES: usize = 4096;
/// Most shapes read from one page.
const MAX_SHAPES: usize = 100_000;
/// Deepest group nesting that is walked.
const MAX_DEPTH: usize = 64;
/// Longest name kept.
const MAX_NAME: usize = 256;

// Pointer types.
const POINTER_TRAILER: i32 = 20;
const POINTER_PAGES: i32 = 0x27;
const POINTER_PAGE: i32 = 0x15;
const POINTER_NAMES: i32 = 0x32;

// Chunk types.
const CHUNK_TEXT: u32 = 0x0e;
const CHUNK_GROUP: u32 = 0x47;
const CHUNK_SHAPE: u32 = 0x48;
const CHUNK_GUIDE: u32 = 0x4d;
const CHUNK_FOREIGN: u32 = 0x4e;
const CHUNK_SHAPE_LIST: u32 = 0x65;
const CHUNK_SHAPE_ID: u32 = 0x83;
const CHUNK_XFORM: u32 = 0x9b;

/// A pointer to a stream.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    ty: i32,
    offset: usize,
    length: usize,
    format: u16,
}

impl Pointer {
    fn read(d: &[u8], at: usize) -> Option<Self> {
        Some(Self {
            ty: i32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?),
            offset: usize::try_from(u32_le(d, at.checked_add(8)?)?).ok()?,
            length: usize::try_from(u32_le(d, at.checked_add(12)?)?).ok()?,
            format: u16_le(d, at.checked_add(16)?)?,
        })
    }

    const fn compressed(self) -> bool {
        self.format & 2 != 0
    }

    const fn has_pointers(self) -> bool {
        self.ty == POINTER_TRAILER
            || (self.format >= 0x1d && self.format < 0x1f)
            || (self.format >= 0x50 && self.format < 0x60)
    }

    const fn has_chunks(self) -> bool {
        self.format >= 0xd0 && self.format < 0xdf
    }
}

/// Decompresses a stream (the Visio variant of LZ77 with a 4096-byte window): a flag byte tells,
/// bit by bit from the lowest, whether the next item is a literal byte (bit set) or a reference
/// of two bytes — 12 bits of position and 4 of length (plus three) into the window. Stops at
/// `limit` bytes of output.
pub(crate) fn decompress(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut window = [0u8; 4096];
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(data.len().saturating_mul(2).min(limit));
    let mut it = data.iter().copied();
    'outer: while let Some(flag) = it.next() {
        for bit in 0..8 {
            if out.len() >= limit {
                return None;
            }
            if flag >> bit & 1 == 1 {
                let Some(b) = it.next() else { break 'outer };
                if let Some(w) = window.get_mut(pos & 4095) {
                    *w = b;
                }
                pos += 1;
                out.push(b);
            } else {
                let (Some(p1), Some(p2)) = (it.next(), it.next()) else { break 'outer };
                let len = usize::from(p2 & 0x0F) + 3;
                let mut ptr = usize::from(p1) + (usize::from(p2 & 0xF0) << 4);
                ptr = if ptr > 4078 { ptr - 4078 } else { ptr + 18 };
                for i in 0..len {
                    let b = window.get((ptr + i) & 4095).copied().unwrap_or(0);
                    if let Some(w) = window.get_mut((pos + i) & 4095) {
                        *w = b;
                    }
                    out.push(b);
                }
                pos += len;
            }
        }
    }
    Some(out)
}

/// A chunk of a chunk stream.
struct Chunk<'a> {
    ty: u32,
    id: u32,
    body: &'a [u8],
}

/// Splits a chunk stream into chunks. `version` decides whether chunks are followed by a
/// separator.
fn chunks(data: &[u8], version: u16, max: usize) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + CHUNK_HEADER <= data.len() && out.len() < max {
        let (Some(ty), Some(id), Some(unknown1), Some(len), Some(unknown2)) = (
            u32_le(data, pos),
            u32_le(data, pos + 4),
            u32_le(data, pos + 8),
            u32_le(data, pos + 12),
            u16_le(data, pos + 16),
        ) else {
            break;
        };
        let unknown3 = data.get(pos + 18).copied().unwrap_or(0);
        let has_trailer =
            matches!(ty, 0x2c | 0x65 | 0x66 | 0x69 | 0x6a | 0x6b | 0x70 | 0x71) || unknown1 != 0;
        let has_separator = version > 6 && separator(ty, unknown2, unknown3, has_trailer);
        let mut len = usize::try_from(len).unwrap_or(usize::MAX);
        let room = data.len() - pos - CHUNK_HEADER;
        if len > room {
            // The chunk runs past the data: what is there is the chunk, less its trailer and
            // separator.
            len = room;
            if has_trailer {
                len = len.saturating_sub(8);
            }
            if has_separator {
                len = len.saturating_sub(4);
            }
        }
        let body_at = pos + CHUNK_HEADER;
        let Some(body) = data.get(body_at..body_at + len) else { break };
        out.push(Chunk { ty, id, body });
        let mut end = body_at + len;
        if has_trailer && end + 8 <= data.len() {
            end += 8;
        }
        if has_separator && end + 4 <= data.len() {
            end += 4;
        }
        pos = end;
    }
    out
}

/// Whether a chunk of a 2003+ file is followed by a four-byte separator.
fn separator(ty: u32, unknown2: u16, unknown3: u8, has_trailer: bool) -> bool {
    match ty {
        0x1f | 0xc9 => return false,
        0x69 => return true,
        0xa9 | 0xaa | 0xb4 | 0xb6 if unknown2 == 2 && unknown3 == 0x54 => return true,
        _ => {}
    }
    (unknown2 == 2 && unknown3 == 0x55) || (unknown2 == 3 && unknown3 != 0x50) || has_trailer
}

/// The opened `VisioDocument` stream.
struct Visio<'a> {
    doc: &'a [u8],
    version: u16,
    /// Bytes of decompressed data still allowed.
    budget: usize,
    /// Streams opened so far.
    opened: usize,
}

/// The content of a stream: the 4-byte block header of a compressed stream, then the rest.
struct Content {
    data: Vec<u8>,
    /// Where the content proper starts (after the block header of a compressed stream).
    start: usize,
}

impl Content {
    fn body(&self) -> &[u8] {
        self.data.get(self.start..).unwrap_or(&[])
    }
}

impl Visio<'_> {
    fn open(&mut self, p: Pointer) -> Option<Content> {
        self.opened += 1;
        if self.opened > MAX_STREAMS {
            return None;
        }
        let raw = self.doc.get(p.offset..p.offset.checked_add(p.length)?)?;
        if p.compressed() {
            let data = decompress(raw, self.budget)?;
            self.budget = self.budget.saturating_sub(data.len());
            if data.len() < 4 {
                return None;
            }
            Some(Content { data, start: 4 })
        } else {
            Some(Content { data: raw.to_vec(), start: 0 })
        }
    }

    /// The pointers a container stream holds.
    fn children(&self, content: &[u8]) -> Vec<Pointer> {
        let mut out = Vec::new();
        let Some(at) = u32_le(content, 0).and_then(|v| usize::try_from(v).ok()) else {
            return out;
        };
        let Some(count) = u32_le(content, at).and_then(|v| usize::try_from(v).ok()) else {
            return out;
        };
        for i in 0..count.min(MAX_POINTERS) {
            let Some(p) = at
                .checked_add(8)
                .and_then(|b| b.checked_add(i * POINTER_SIZE))
                .and_then(|o| Pointer::read(content, o))
            else {
                break;
            };
            out.push(p);
        }
        out
    }
}

/// A shape read from a page.
#[derive(Default)]
struct Shape {
    ty: u32,
    pin: Option<(f64, f64)>,
    text: String,
    /// Ids of the shapes a group holds.
    kids: Vec<u32>,
}

/// The shapes of a page.
#[derive(Default)]
struct Page {
    shapes: BTreeMap<u32, Shape>,
    /// Top-level shape ids, as the page lists them.
    roots: Vec<u32>,
    /// Shapes in the order they appear.
    order: Vec<u32>,
}

/// Where the shape ids that follow a shape list belong.
enum Owner {
    Page,
    Shape(u32),
}

fn f64_at(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Reads the shapes of a page from its chunks.
fn read_page(data: &[u8], version: u16) -> Page {
    let mut page = Page::default();
    let mut current: Option<u32> = None;
    let mut owner: Option<Owner> = None;
    for c in chunks(data, version, 2_000_000) {
        if c.ty != CHUNK_SHAPE_ID {
            owner = None;
        }
        match c.ty {
            CHUNK_GROUP | CHUNK_SHAPE | CHUNK_GUIDE | CHUNK_FOREIGN => {
                if page.shapes.len() >= MAX_SHAPES {
                    break;
                }
                page.order.push(c.id);
                page.shapes.entry(c.id).or_default().ty = c.ty;
                current = Some(c.id);
            }
            CHUNK_SHAPE_LIST => {
                owner = Some(current.map_or(Owner::Page, Owner::Shape));
            }
            CHUNK_SHAPE_ID => {
                let Some(id) = u32_le(c.body, 0) else { continue };
                match &owner {
                    Some(Owner::Page) => page.roots.push(id),
                    Some(Owner::Shape(s)) => {
                        if let Some(shape) = page.shapes.get_mut(s) {
                            shape.kids.push(id);
                        }
                    }
                    None => {}
                }
            }
            CHUNK_XFORM => {
                if let (Some(shape), Some(x), Some(y)) = (
                    current.and_then(|id| page.shapes.get_mut(&id)),
                    f64_at(c.body, 1),
                    f64_at(c.body, 10),
                ) && x.is_finite()
                    && y.is_finite()
                {
                    shape.pin = Some((x, y));
                }
            }
            CHUNK_TEXT => {
                if let Some(shape) = current.and_then(|id| page.shapes.get_mut(&id)) {
                    shape.text = text(c.body, version);
                }
            }
            _ => {}
        }
    }
    page
}

/// The text of a text chunk: after eight bytes of header, UTF-16 (2003 and later) or one byte
/// per character (Visio 6), up to the end of the chunk or the first zero.
fn text(body: &[u8], version: u16) -> String {
    let raw = body.get(8..).unwrap_or(&[]);
    let s = if version > 6 {
        let end = raw.chunks_exact(2).position(|u| u == [0, 0]).map_or(raw.len() & !1, |i| i * 2);
        utf16le_lossy(raw.get(..end).unwrap_or(&[]))
    } else {
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        decode_codepage(raw.get(..end).unwrap_or(&[]), CodePage::Windows1252)
    };
    // Control characters stand for fields and line breaks; keep the breaks, drop the rest.
    s.chars()
        .filter_map(|c| match c {
            '\r' | '\n' | '\u{b}' | '\u{2028}' => Some('\n'),
            '\t' => Some(c),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// The name table: the text of each entry. Entry 1 is the document, the next ones the pages.
fn names(v: &mut Visio<'_>, p: Pointer) -> Vec<String> {
    let mut out = Vec::new();
    let Some(content) = v.open(p) else { return out };
    for child in v.children(content.body()) {
        let Some(c) = v.open(child) else {
            out.push(String::new());
            continue;
        };
        let d = c.body();
        // An entry is [size][level][text] when `size` is the rest of the entry, else [level][text].
        let skip = match u32_le(d, 0).and_then(|s| usize::try_from(s).ok()) {
            Some(size) if size > 0 && size + 4 == d.len() => 8,
            _ => 4,
        };
        let raw = d.get(skip..).unwrap_or(&[]);
        let s = if v.version > 6 {
            let end =
                raw.chunks_exact(2).position(|u| u == [0, 0]).map_or(raw.len() & !1, |i| i * 2);
            utf16le_lossy(raw.get(..end).unwrap_or(&[]))
        } else {
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            decode_codepage(raw.get(..end).unwrap_or(&[]), CodePage::Windows1252)
        };
        out.push(s.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect());
        if out.len() > MAX_PAGES + 4 {
            break;
        }
    }
    out
}

/// Reading order: top to bottom (Visio's y axis points up), then left to right.
fn sorted(page: &Page, ids: &[u32]) -> Vec<u32> {
    let mut v: Vec<u32> = ids.iter().copied().filter(|id| page.shapes.contains_key(id)).collect();
    #[allow(clippy::cast_possible_truncation)]
    let key = |id: &u32| match page.shapes.get(id).and_then(|s| s.pin) {
        Some((x, y)) => {
            (0u8, ((y * 20.0).round() as i64).saturating_neg(), (x * 20.0).round() as i64, *id)
        }
        None => (1u8, 0, 0, *id),
    };
    v.sort_by_key(key);
    v
}

/// Writes the shapes `ids` (and what they hold) into section `sec`. `ordered` puts them in reading
/// order; otherwise they stay as they are (their order in the file).
fn emit(
    page: &Page,
    ids: &[u32],
    sec: crate::sink::SectionId,
    sink: &mut Sink<'_>,
    depth: usize,
    seen: &mut BTreeSet<u32>,
    ordered: bool,
) -> bool {
    let ids = if ordered { sorted(page, ids) } else { ids.to_vec() };
    for id in ids {
        // A shape listed twice (or a group that lists itself) is shown once.
        if !seen.insert(id) {
            continue;
        }
        let Some(shape) = page.shapes.get(&id) else { continue };
        let level = u8::try_from(depth.min(6)).unwrap_or(6);
        for line in shape.text.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
            let p = Paragraph {
                level,
                runs: vec![Run { text: line.to_owned(), style: 0, href: String::new() }],
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                return false;
            }
        }
        if shape.ty == CHUNK_FOREIGN {
            let alt = shape.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let info = ImageInfo {
                alt: alt.chars().take(60).collect(),
                media: String::new(),
                bytes: 0,
                external: false,
            };
            if !sink.image(sec, info) {
                return false;
            }
        }
        if depth < MAX_DEPTH
            && !shape.kids.is_empty()
            && !emit(page, &shape.kids, sec, sink, depth + 1, seen, true)
        {
            return false;
        }
    }
    true
}

/// Shows the text of the pages of the drawing.
pub(crate) fn extract(cf: &CompoundFile<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let index = cf
        .find("VisioDocument")
        .ok_or_else(|| PreviewError::Malformed("the drawing has no VisioDocument stream".into()))?;
    let doc = cf
        .read_stream(index)
        .map_err(|e| PreviewError::Malformed(format!("VisioDocument: {e}")))?;
    if !doc.starts_with(SIGNATURE) {
        return Err(PreviewError::Malformed("not a Visio drawing".into()));
    }
    let version =
        u16_le(&doc, 0x1a).ok_or_else(|| PreviewError::Malformed("truncated header".into()))?;
    if version < 6 {
        return Err(PreviewError::Unsupported(FileFormat::Vsd));
    }
    let mut v = Visio {
        doc: &doc,
        version,
        budget: usize::try_from(sink.policy.inflate_budget).unwrap_or(usize::MAX),
        opened: 0,
    };
    let trailer = Pointer::read(&doc, TRAILER_AT)
        .filter(|p| p.ty == POINTER_TRAILER)
        .ok_or_else(|| PreviewError::Malformed("the trailer is missing".into()))?;
    let trailer_content = v
        .open(trailer)
        .ok_or_else(|| PreviewError::Malformed("the trailer is unreadable".into()))?;
    let top = v.children(trailer_content.body());

    // The name table.
    let table = top.iter().find(|p| p.ty == POINTER_NAMES && p.has_pointers()).copied();
    let names = table.map(|p| names(&mut v, p)).unwrap_or_default();

    // The pages.
    let mut pages: Vec<Pointer> = Vec::new();
    for p in top.iter().filter(|p| p.ty == POINTER_PAGES && p.has_pointers()) {
        let Some(c) = v.open(*p) else { continue };
        pages.extend(v.children(c.body()).into_iter().filter(|c| c.ty == POINTER_PAGE));
    }
    if pages.is_empty() {
        return Err(PreviewError::Malformed("the drawing lists no pages".into()));
    }

    let mut pictures = false;
    for (k, ptr) in pages.iter().take(MAX_PAGES).enumerate() {
        if sink.is_full() {
            break;
        }
        let name = names
            .get(k + 2)
            .filter(|n| !n.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Page-{}", k + 1));
        if !ptr.has_chunks() {
            sink.skipped(&name);
            continue;
        }
        let Some(content) = v.open(*ptr) else {
            sink.skipped(&name);
            continue;
        };
        // The block header of a compressed stream belongs to the first chunk.
        let page = read_page(&content.data, version);
        pictures |= page.shapes.values().any(|s| s.ty == CHUNK_FOREIGN);
        let Some(sec) = sink.open_section(SectionKind::Page, &name, false) else { break };
        let mut seen = BTreeSet::new();
        let ok = if page.roots.is_empty() {
            // The page does not list its shapes: show them in the order of the file.
            emit(&page, &page.order, sec, sink, 0, &mut seen, false)
        } else {
            // Shapes that no list reaches are still shown, after the listed ones.
            emit(&page, &page.roots, sec, sink, 0, &mut seen, true) && {
                let rest: Vec<u32> =
                    page.order.iter().copied().filter(|id| !seen.contains(id)).collect();
                emit(&page, &rest, sec, sink, 0, &mut seen, false)
            }
        };
        sink.drop_if_empty(sec);
        if !ok {
            break;
        }
    }
    if pictures {
        sink.note(NoticeKind::Unsupported, "pictures and embedded objects");
    }
    sink.note(
        NoticeKind::Unsupported,
        "drawing geometry, connections, shape data, hyperlinks and masters",
    );
    sink.note(NoticeKind::BestEffort, "binary Visio");
    Ok(())
}

#[cfg(test)]
mod tests;
