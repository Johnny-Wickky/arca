//! Binary Visio drawings (`.vsd`): Visio 6 (2000) and the 2003–2010 generation.
//!
//! The file is an OLE2 container whose `VisioDocument` stream is a tree: a trailer points to
//! streams, some of which hold more pointers and some of which hold *chunks* (typed records).
//! Streams can be compressed with a small LZ scheme. A pointer is 18 bytes: type (u32), 4 bytes
//! that are not used, offset (u32), length (u32), format (u16). The format tells what the stream
//! holds: its high nibble is 5 for a stream of pointers, 8, 0xC or 0xD for a stream of chunks and
//! 0, 4 or 5 for a single record (a "blob" whose type is the pointer's type); bit 1 says the
//! stream is compressed; bit 0 of a page pointer says it is a foreground page.
//!
//! A chunk has a 19-byte header (type u32, id u32, list u32, data length u32, level u16, one more
//! byte) followed by the data, an optional trailer of 8 bytes and, in 2003 and later files, an
//! optional separator of 4. Inside the data, a number is a tag byte followed by a little-endian
//! `f64`; the layouts of the chunks (shape, XForm, Line, Fill, Geometry rows, text, character and
//! paragraph formats, the colour table, style sheets) are read by [`read`]. They are the ones
//! the open-source readers of the format use (libvisio and the HDGF notes of Apache POI); the
//! format is not published by Microsoft, so **none of it is verified against files made by
//! Visio**, and the page says so ([`crate::visio_draw`]).
//!
//! The drawing is read into the shape model shared with `.vsdx` and shown as a page by
//! `visio_draw`; the reading view shows the text of every shape in reading order with groups kept
//! together. Nothing is executed.

use std::collections::{BTreeMap, BTreeSet};

use arca_container::bytes::{u16_le, u32_le};
use arca_container::cfb::CompoundFile;
use arca_container::text::{CodePage, decode_codepage, utf16le_lossy};

use crate::model::{FileFormat, NoticeKind};
use crate::sink::Sink;
use crate::visio_draw::model::{Doc, Kind, PageDef, Sheet};
use crate::visio_draw::{Want, text as draw_text};
use crate::{PreviewError, PreviewPolicy};

mod read;

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
/// Most pages listed.
const MAX_PAGES: usize = 4096;
/// Longest name kept.
const MAX_NAME: usize = 256;
/// Deepest nesting of pointer containers followed.
const MAX_LEVEL: u32 = 24;
/// Most chunks read from one stream.
const MAX_CHUNKS: usize = 2_000_000;

// Pointer types.
const POINTER_TRAILER: i32 = 20;
pub(crate) const POINTER_PAGE: i32 = 0x15;
const POINTER_NAMES: i32 = 0x32;
const POINTER_NAME_IDX: i32 = 0xc9;
const POINTER_NAME_IDX123: i32 = 0x34;
const POINTER_FONTFACES: i32 = 0xd8;
const POINTER_COLORS: i32 = 0x16;
const CHUNK_NAME2: u32 = 0x33;

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

    /// The stream holds more pointers.
    const fn has_pointers(self) -> bool {
        self.ty == POINTER_TRAILER
            || (self.format >= 0x1d && self.format < 0x1f)
            || (self.format >= 0x50 && self.format < 0x60)
    }

    /// The stream holds chunks.
    const fn has_chunks(self) -> bool {
        matches!(self.format >> 4, 0x8 | 0xc | 0xd)
    }

    /// The stream is one record.
    const fn is_blob(self) -> bool {
        matches!(self.format >> 4, 0x0 | 0x4 | 0x5)
    }

    /// A page pointer of a foreground page.
    const fn foreground(self) -> bool {
        self.format & 1 != 0
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
pub(crate) struct Chunk<'a> {
    pub(crate) ty: u32,
    pub(crate) id: u32,
    /// The level the chunk says it is at, plus the level of the stream that holds it.
    pub(crate) level: u32,
    pub(crate) body: &'a [u8],
}

/// Splits a chunk stream into chunks. `version` decides whether chunks are followed by a
/// separator.
pub(crate) fn chunks(data: &[u8], version: u16, max: usize) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + CHUNK_HEADER <= data.len() && out.len() < max {
        let (Some(ty), Some(id), Some(unknown1), Some(len), Some(level)) = (
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
        let has_separator = version > 6 && separator(ty, level, unknown3, has_trailer);
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
        out.push(Chunk { ty, id, level: u32::from(level), body });
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
fn separator(ty: u32, level: u16, unknown3: u8, has_trailer: bool) -> bool {
    match ty {
        0x1f | 0xc9 => return false,
        0x69 => return true,
        0xa9 | 0xaa | 0xb4 | 0xb6 | 0xc4 if level == 2 && unknown3 == 0x54 => return true,
        _ => {}
    }
    (level == 2 && unknown3 == 0x55) || (level == 3 && unknown3 != 0x50) || has_trailer
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
            // A stored stream is copied, so it is charged like an inflated one.
            if raw.len() > self.budget {
                return None;
            }
            self.budget -= raw.len();
            Some(Content { data: raw.to_vec(), start: 0 })
        }
    }

    /// The pointers a container stream holds with their index, and the order the list gives.
    fn pointers(&self, content: &[u8]) -> (Vec<(u32, Pointer)>, Vec<u32>) {
        let mut out = Vec::new();
        let mut order = Vec::new();
        let Some(at) = u32_le(content, 0).and_then(|v| usize::try_from(v).ok()) else {
            return (out, order);
        };
        let Some(count) = u32_le(content, at).and_then(|v| usize::try_from(v).ok()) else {
            return (out, order);
        };
        let count = count.min(MAX_POINTERS);
        for i in 0..count {
            let Some(p) = at
                .checked_add(8)
                .and_then(|b| b.checked_add(i * POINTER_SIZE))
                .and_then(|o| Pointer::read(content, o))
            else {
                break;
            };
            if p.ty != 0 {
                out.push((u32::try_from(i).unwrap_or(u32::MAX), p));
            }
        }
        // The list of indices after the pointers says in which order they are visited.
        let list_size = at
            .checked_sub(4)
            .and_then(|o| u32_le(content, o))
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0);
        if list_size > 1 {
            let first = at.saturating_add(8).saturating_add(count.saturating_mul(POINTER_SIZE));
            for i in 0..list_size.min(MAX_POINTERS) {
                match first.checked_add(i * 4).and_then(|o| u32_le(content, o)) {
                    Some(v) => order.push(v),
                    None => break,
                }
            }
        }
        (out, order)
    }
}

/// What the walk of the stream tree reports.
pub(crate) enum Ev<'a> {
    /// A page, the styles, the stencils or one stencil page starts.
    Begin { ty: i32, idx: u32, name: Option<&'a str>, foreground: bool },
    /// It ends.
    End { ty: i32 },
    /// A chunk (or the single record of a blob stream).
    Chunk(&'a Chunk<'a>),
}

/// A page whose content is read later.
struct PageRef {
    ptr: Pointer,
    idx: u32,
    level: u32,
    name: Option<String>,
}

/// Walks the tree of streams.
struct Walk<'v, 'd> {
    v: &'v mut Visio<'d>,
    /// Offsets of the streams visited (a stream is read once, however many pointers reach it).
    seen: BTreeSet<usize>,
    /// Pages are noted, not read.
    defer: bool,
    pages: Vec<PageRef>,
    /// The first name table met: pages in containers without one are named from it.
    fallback: Vec<String>,
}

/// The text of a `NAME2` entry: `[level][text]`, or `[size][level][text]` when the size is that of
/// the rest of the entry; UTF-16 (2003 and later) or one byte per character (Visio 6).
fn name_entry(d: &[u8], version: u16) -> String {
    let skip = match u32_le(d, 0).and_then(|s| usize::try_from(s).ok()) {
        Some(size) if size > 0 && size.checked_add(4) == Some(d.len()) => 8,
        _ => 4,
    };
    let raw = d.get(skip..).unwrap_or(&[]);
    let s = if version > 6 {
        let end = raw.chunks_exact(2).position(|u| u == [0, 0]).map_or(raw.len() & !1, |i| i * 2);
        utf16le_lossy(raw.get(..end).unwrap_or(&[]))
    } else {
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        decode_codepage(raw.get(..end).unwrap_or(&[]), CodePage::Windows1252)
    };
    s.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect()
}

impl Walk<'_, '_> {
    /// Visits the pointers of a container: first the name tables (which name the pages and
    /// stencils among them), then the rest in the order the list gives, then the remainder.
    fn container(&mut self, body: &[u8], level: u32, f: &mut dyn FnMut(Ev<'_>)) {
        if level > MAX_LEVEL {
            return;
        }
        let (ptrs, order) = self.v.pointers(body);
        let version = self.v.version;
        let mut by_id: BTreeMap<u32, String> = BTreeMap::new();
        let mut listed: Vec<String> = Vec::new();
        for (idx, p) in ptrs.iter().filter(|(_, p)| p.ty == POINTER_NAMES) {
            self.stream(*p, *idx, level + 1, None, &mut |ev| {
                if let Ev::Chunk(c) = ev
                    && c.ty == CHUNK_NAME2
                {
                    let n = name_entry(c.body, version);
                    by_id.insert(c.id, n.clone());
                    listed.push(n);
                }
            });
        }
        let mut element: BTreeMap<u32, String> = BTreeMap::new();
        for (idx, p) in ptrs
            .iter()
            .filter(|(_, p)| p.ty == POINTER_NAME_IDX || p.ty == POINTER_NAME_IDX123)
        {
            self.stream(*p, *idx, level + 1, None, &mut |ev| {
                let Ev::Chunk(c) = ev else { return };
                for (name_id, element_id) in name_pairs(c) {
                    if let Some(n) = by_id.get(&name_id) {
                        element.insert(element_id, n.clone());
                    }
                }
            });
        }
        if self.fallback.is_empty() {
            self.fallback.clone_from(&listed);
        }
        let fallback = self.fallback.clone();
        let mut done: BTreeSet<u32> = BTreeSet::new();
        let mut visit = |w: &mut Self, idx: u32, p: Pointer, k: usize| {
            if p.ty == POINTER_NAMES
                || p.ty == POINTER_NAME_IDX
                || p.ty == POINTER_NAME_IDX123
                || p.ty == POINTER_FONTFACES
            {
                return;
            }
            // Without a name index, the names are those of the table in order: the first is the
            // document's, then come the pages.
            let name = element
                .get(&idx)
                .cloned()
                .or_else(|| {
                    (p.ty == POINTER_PAGE)
                        .then(|| listed.get(k + 2).or_else(|| fallback.get(k + 2)).cloned())
                        .flatten()
                });
            w.stream(p, idx, level + 1, name.as_deref(), f);
        };
        let by_index: BTreeMap<u32, Pointer> = ptrs.iter().map(|(i, p)| (*i, *p)).collect();
        let mut page_no = 0usize;
        for j in order {
            if let Some(p) = by_index.get(&j)
                && done.insert(j)
            {
                let k = if p.ty == POINTER_PAGE { page_no } else { 0 };
                if p.ty == POINTER_PAGE {
                    page_no += 1;
                }
                visit(self, j, *p, k);
            }
        }
        for (idx, p) in &ptrs {
            if done.insert(*idx) {
                let k = if p.ty == POINTER_PAGE { page_no } else { 0 };
                if p.ty == POINTER_PAGE {
                    page_no += 1;
                }
                visit(self, *idx, *p, k);
            }
        }
    }

    /// Visits one stream.
    fn stream(
        &mut self,
        p: Pointer,
        idx: u32,
        level: u32,
        name: Option<&str>,
        f: &mut dyn FnMut(Ev<'_>),
    ) {
        if level > MAX_LEVEL {
            return;
        }
        if p.ty == POINTER_PAGE && self.defer {
            if self.pages.len() <= MAX_PAGES {
                self.pages.push(PageRef { ptr: p, idx, level, name: name.map(str::to_owned) });
            }
            return;
        }
        if !self.seen.insert(p.offset) {
            return;
        }
        let Some(content) = self.v.open(p) else { return };
        let shift = if p.compressed() { 4 } else { 0 };
        let scoped = matches!(p.ty, 0x15 | 0x1a | 0x1d | 0x1e);
        if scoped {
            f(Ev::Begin { ty: p.ty, idx, name, foreground: p.foreground() });
        }
        if p.has_chunks() {
            for mut c in chunks(&content.data, self.v.version, MAX_CHUNKS) {
                c.level = c.level.saturating_add(level + 1);
                f(Ev::Chunk(&c));
            }
        } else if p.is_blob() || p.has_pointers() {
            if p.is_blob() {
                let c = Chunk {
                    ty: u32::try_from(p.ty).unwrap_or(0),
                    id: idx,
                    level: level + 1,
                    body: content.data.get(shift..).unwrap_or(&[]),
                };
                f(Ev::Chunk(&c));
            }
            if (p.format >> 4 == 5 && p.ty != POINTER_COLORS) || p.has_pointers() {
                self.container(content.body(), level + 1, f);
            }
        }
        if scoped {
            f(Ev::End { ty: p.ty });
        }
    }
}

/// The `(name id, element id)` pairs of a name index chunk: records of 13 bytes `[id][id][element]
/// [1]` after a count (type 0xC9), or pairs of ids (type 0x34).
fn name_pairs(c: &Chunk<'_>) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    if c.ty == 0xc9 {
        let count = u32_le(c.body, 0).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
        let count = count.min(c.body.len() / 13);
        for i in 0..count {
            let at = 4 + i * 13;
            if let (Some(n), Some(e)) = (u32_le(c.body, at), u32_le(c.body, at + 8)) {
                out.push((n, e));
            }
        }
    } else if c.ty == 0x34 {
        for i in 0..c.body.len() / 8 {
            if let (Some(n), Some(e)) = (u32_le(c.body, i * 8), u32_le(c.body, i * 8 + 4)) {
                out.push((n, e));
            }
        }
    }
    out
}

/// The text of a text chunk: after eight bytes of header, UTF-16 (2003 and later) or one byte
/// per character (Visio 6), up to the end of the chunk or the first zero.
pub(crate) fn text(body: &[u8], version: u16) -> String {
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

/// Opens the stream and reads the drawing into the shape model.
///
/// `want` says which pages are read in full: the page list (names, foreground or background) is
/// always complete.
///
/// # Errors
/// [`PreviewError::Malformed`] when the stream is not a Visio drawing or lists no pages,
/// [`PreviewError::Unsupported`] for Visio 5 and older.
pub(crate) fn read_doc(
    cf: &CompoundFile<'_>,
    policy: &PreviewPolicy,
    want: Want,
) -> Result<Doc, PreviewError> {
    let index = cf
        .find("VisioDocument")
        .ok_or_else(|| PreviewError::Malformed("the drawing has no VisioDocument stream".into()))?;
    let data = cf
        .read_stream(index)
        .map_err(|e| PreviewError::Malformed(format!("VisioDocument: {e}")))?;
    if !data.starts_with(SIGNATURE) {
        return Err(PreviewError::Malformed("not a Visio drawing".into()));
    }
    let version =
        u16_le(&data, 0x1a).ok_or_else(|| PreviewError::Malformed("truncated header".into()))?;
    if version < 6 {
        return Err(PreviewError::Unsupported(FileFormat::Vsd));
    }
    let mut v = Visio {
        doc: &data,
        version,
        budget: usize::try_from(policy.inflate_budget).unwrap_or(usize::MAX),
        opened: 0,
    };
    let trailer = Pointer::read(&data, TRAILER_AT)
        .filter(|p| p.ty == POINTER_TRAILER)
        .ok_or_else(|| PreviewError::Malformed("the trailer is missing".into()))?;
    let trailer_content = v
        .open(trailer)
        .ok_or_else(|| PreviewError::Malformed("the trailer is unreadable".into()))?;

    let mut reader = read::Reader::new(version);
    let mut walk = Walk { v: &mut v, seen: BTreeSet::new(), defer: true, pages: Vec::new(), fallback: Vec::new() };
    walk.seen.insert(trailer.offset);
    walk.container(trailer_content.body(), 0, &mut |ev| reader.event(&ev));
    let refs = std::mem::take(&mut walk.pages);
    walk.defer = false;
    if refs.is_empty() {
        return Err(PreviewError::Malformed("the drawing lists no pages".into()));
    }
    let mut doc = Doc::default();
    // A page pointer says if the page is a foreground one; if none does, all are.
    let any_fg = refs.iter().any(|r| r.ptr.foreground());
    let mut foreground_no = 0usize;
    let mut wanted: Vec<bool> = Vec::new();
    for r in &refs {
        let fg = !any_fg || r.ptr.foreground();
        let load = match want {
            Want::Count => false,
            Want::All => true,
            Want::Page(n) => fg && foreground_no == n,
        };
        if fg {
            foreground_no += 1;
        }
        wanted.push(load);
        doc.pages.push(PageDef {
            id: r.idx,
            name: r.name.clone().unwrap_or_default(),
            background: !fg,
            ..PageDef::default()
        });
    }
    doc.pages_cut = refs.len() > MAX_PAGES;
    let mut back: Option<u32> = None;
    for (i, r) in refs.iter().enumerate() {
        if !wanted.get(i).copied().unwrap_or(false) {
            continue;
        }
        if let Some(def) = read_page(&mut walk, &mut reader, r, doc.pages.get(i)) {
            back = back.or(def.back_page);
            if let Some(slot) = doc.pages.get_mut(i) {
                *slot = def;
            }
        }
    }
    // The background page of the page asked for is read too.
    if let (Want::Page(_), Some(id)) = (want, back)
        && let Some(i) = refs.iter().position(|r| r.idx == id)
        && !wanted.get(i).copied().unwrap_or(true)
        && let Some(r) = refs.get(i)
        && let Some(def) = read_page(&mut walk, &mut reader, r, doc.pages.get(i))
        && let Some(slot) = doc.pages.get_mut(i)
    {
        *slot = def;
    }
    let (masters, styles, colors, unverified) = reader.take_shared();
    doc.masters = masters;
    doc.styles = styles;
    doc.colors = colors;
    doc.unverified = unverified;
    doc.unverified("the layouts of binary Visio follow the open-source descriptions of the format");
    // Colours and styles are known only once everything has been read: pages were read after
    // them (the first walk read all streams but the pages), so nothing is missing.
    Ok(doc)
}

/// Reads one page: its stream, and its content as the reader builds it.
fn read_page(
    walk: &mut Walk<'_, '_>,
    reader: &mut read::Reader,
    r: &PageRef,
    meta: Option<&PageDef>,
) -> Option<PageDef> {
    walk.seen.remove(&r.ptr.offset);
    walk.stream(r.ptr, r.idx, r.level, r.name.as_deref(), &mut |ev| reader.event(&ev));
    let mut def = reader.take_page()?;
    if let Some(m) = meta {
        def.id = m.id;
        def.name.clone_from(&m.name);
        def.background = m.background;
    }
    def.loaded = true;
    Some(def)
}

/// Shows the text of the pages of the drawing.
pub(crate) fn extract(cf: &CompoundFile<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let doc = read_doc(cf, sink.policy, Want::All)?;
    if doc.pages.len() > MAX_PAGES {
        sink.truncated("pages");
    }
    for p in doc.pages.iter().filter(|p| !p.loaded) {
        sink.skipped(&p.name);
    }
    draw_text::emit_doc(&doc, sink);
    if doc.pages.iter().any(|p| p.truncated) {
        sink.truncated("shapes");
    }
    if doc.pages.iter().any(|p| has_pictures(&p.shapes)) {
        sink.note(NoticeKind::Unsupported, "pictures and embedded objects");
    }
    for what in &doc.unverified {
        sink.note(NoticeKind::BestEffort, what);
    }
    sink.note(NoticeKind::BestEffort, "binary Visio");
    Ok(())
}

fn has_pictures(shapes: &[Sheet]) -> bool {
    shapes.iter().any(|s| s.kind == Kind::Foreign || has_pictures(&s.kids))
}

#[cfg(test)]
mod tests;
