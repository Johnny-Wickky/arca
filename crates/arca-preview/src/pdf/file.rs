//! PDF file structure (ISO 32000-1 §7.5): cross-reference tables and streams, object
//! streams, incremental updates, and reconstruction of damaged files.
//!
//! Everything here treats offsets and sizes as hostile: lookups go through `get()`, chains
//! (`/Prev`, reference-to-reference, object streams) carry visit guards, and every
//! decompression is charged to the shared [`InflateBudget`].

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use arca_container::InflateBudget;

use super::filter::{self, Decoded, FilterError, FilterSpec};
use super::lex::{Dict, Lexer, Obj, Stream, Tok, is_delim, is_ws};
use crate::{PreviewError, PreviewPolicy};

/// Longest reference chain followed.
const MAX_HOPS: usize = 16;
/// Largest object number accepted (guards table allocations).
const MAX_OBJ_NUM: u32 = 16_000_000;
/// Most cross-reference sections followed.
const MAX_SECTIONS: usize = 1_024;
/// Cap on the decoded size of an object stream or xref stream.
const MAX_META_STREAM: usize = 128 << 20;

/// Where an object lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Free,
    /// Byte offset relative to the `%PDF-` header.
    At(usize),
    /// Object stream number and index inside it.
    InStm(u32, u32),
}

/// A decoded object stream.
#[derive(Debug)]
struct ObjStm {
    data: Vec<u8>,
    first: usize,
    /// Object number → (offset relative to `first`, index).
    offsets: HashMap<u32, (usize, u32)>,
}

/// Finds `needle` in `hay` at or after `from`.
pub(super) fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let tail = hay.get(from..)?;
    let mut i = 0usize;
    while let Some(p) = tail.get(i..)?.iter().position(|&b| b == first) {
        let at = i + p;
        if tail.get(at..at + needle.len()) == Some(needle) {
            return Some(from + at);
        }
        i = at + 1;
    }
    None
}

/// An opened PDF file.
pub(super) struct PdfFile<'a> {
    data: &'a [u8],
    base: usize,
    xref: HashMap<u32, Entry>,
    pub(super) trailer: Dict,
    cache: HashMap<u32, Obj>,
    loading: HashSet<u32>,
    objstms: HashMap<u32, Option<Rc<ObjStm>>>,
    budget: InflateBudget,
    rebuilt: bool,
    /// Human-readable notes about things that could not be read (deduplicated).
    pub(super) skipped: Vec<String>,
}

impl<'a> PdfFile<'a> {
    pub(super) fn open(data: &'a [u8], policy: &PreviewPolicy) -> Result<Self, PreviewError> {
        let head = data.get(..data.len().min(1024)).unwrap_or(&[]);
        let base = find(head, b"%PDF-", 0).unwrap_or(0);
        let mut f = Self {
            data,
            base,
            xref: HashMap::new(),
            trailer: Dict::new(),
            cache: HashMap::new(),
            loading: HashSet::new(),
            objstms: HashMap::new(),
            budget: InflateBudget::new(policy.inflate_budget, policy.aggregate_ratio, 16 << 20),
            rebuilt: false,
            skipped: Vec::new(),
        };
        if f.read_xref_chain().is_none() || (!f.encrypted() && !f.root_ok()) {
            f.rebuild();
        }
        if f.encrypted() {
            return Err(PreviewError::Encrypted);
        }
        if !f.root_ok() && !f.rebuilt {
            f.rebuild();
            if f.encrypted() {
                return Err(PreviewError::Encrypted);
            }
        }
        if !f.root_ok() {
            return Err(PreviewError::Malformed("PDF has no readable document catalog".into()));
        }
        Ok(f)
    }

    pub(super) fn encrypted(&self) -> bool {
        self.trailer.contains_key("Encrypt")
    }

    pub(super) const fn rebuilt(&self) -> bool {
        self.rebuilt
    }

    pub(super) fn note(&mut self, what: &str) {
        if !self.skipped.iter().any(|s| s == what) && self.skipped.len() < 32 {
            self.skipped.push(what.to_owned());
        }
    }

    fn root_ok(&mut self) -> bool {
        let root = self.get(&self.trailer.clone(), "Root");
        let Some(d) = root.as_dict() else { return false };
        d.contains_key("Pages")
    }

    // ---- cross-reference data ------------------------------------------------------------

    fn find_startxref(&self) -> Option<usize> {
        let tail_start = self.data.len().saturating_sub(2048);
        let mut last = None;
        let mut from = tail_start;
        while let Some(p) = find(self.data, b"startxref", from) {
            last = Some(p);
            from = p + 9;
        }
        let p = last?;
        let mut lx = Lexer::new(self.data, p + 9);
        match lx.next_tok() {
            Tok::Int(n) => usize::try_from(n).ok(),
            _ => None,
        }
    }

    fn read_xref_chain(&mut self) -> Option<()> {
        let mut next = self.find_startxref();
        let mut seen = HashSet::new();
        let mut first = true;
        while let Some(off) = next.take() {
            if !seen.insert(off) || seen.len() > MAX_SECTIONS {
                break;
            }
            let trailer = match self.read_xref_section(off, false) {
                Some(t) => t,
                None if first => return None,
                None => break,
            };
            first = false;
            if let Some(x) =
                trailer.get("XRefStm").and_then(Obj::as_int).and_then(|v| usize::try_from(v).ok())
            {
                let _ = self.read_xref_section(x, true);
            }
            next = trailer.get("Prev").and_then(Obj::as_int).and_then(|v| usize::try_from(v).ok());
            for (k, v) in trailer {
                self.trailer.entry(k).or_insert(v);
            }
        }
        (!self.xref.is_empty()).then_some(())
    }

    fn insert_entry(&mut self, num: u32, e: Entry, override_free: bool) {
        if num > MAX_OBJ_NUM {
            return;
        }
        match self.xref.get(&num) {
            None => {
                self.xref.insert(num, e);
            }
            Some(Entry::Free) if override_free => {
                self.xref.insert(num, e);
            }
            Some(_) => {}
        }
    }

    /// Reads the cross-reference section at `off` (table or stream) and returns its trailer.
    fn read_xref_section(&mut self, off: usize, override_free: bool) -> Option<Dict> {
        let pos = self.base.checked_add(off)?;
        let mut lx = Lexer::new(self.data, pos);
        lx.skip_ws();
        if self.data.get(lx.pos..lx.pos + 4) == Some(b"xref") {
            lx.pos += 4;
            return self.read_xref_table(lx, override_free);
        }
        self.read_xref_stream(pos, override_free)
    }

    fn read_xref_table(&mut self, mut lx: Lexer<'a>, override_free: bool) -> Option<Dict> {
        let mut total = 0usize;
        loop {
            match lx.next_tok() {
                Tok::Int(first) => {
                    let Tok::Int(count) = lx.next_tok() else { return None };
                    let mut first = u32::try_from(first).ok()?;
                    let count = u32::try_from(count).ok()?;
                    for i in 0..count {
                        let Tok::Int(a) = lx.next_tok() else { return None };
                        let Tok::Int(g) = lx.next_tok() else { return None };
                        let Tok::Kw(kind) = lx.next_tok() else { return None };
                        total += 1;
                        if total > MAX_OBJ_NUM as usize {
                            return None;
                        }
                        let is_free = kind == b"f";
                        if i == 0 && first == 1 && is_free && g == 65535 {
                            // Off-by-one subsection header written by some producers.
                            first = 0;
                        }
                        let num = first.checked_add(i)?;
                        if is_free {
                            self.insert_entry(num, Entry::Free, override_free);
                        } else if kind == b"n" {
                            self.insert_entry(
                                num,
                                Entry::At(usize::try_from(a).ok()?),
                                override_free,
                            );
                        } else {
                            return None;
                        }
                    }
                }
                Tok::Kw(b"trailer") => {
                    let o = lx.object(true)?;
                    return o.as_dict().cloned();
                }
                _ => return None,
            }
        }
    }

    fn read_xref_stream(&mut self, pos: usize, override_free: bool) -> Option<Dict> {
        let obj = self.parse_at_pos(pos, None)?;
        let stream = obj.as_stream()?.clone();
        let w: Vec<usize> = stream
            .dict
            .get("W")?
            .as_arr()?
            .iter()
            .filter_map(|o| o.as_int().and_then(|v| usize::try_from(v).ok()))
            .collect();
        let (&w0, &w1, &w2) = (w.first()?, w.get(1)?, w.get(2)?);
        if w0 > 8 || w1 > 8 || w2 > 8 || w0 + w1 + w2 == 0 {
            return None;
        }
        let size = stream.dict.get("Size").and_then(Obj::as_int).unwrap_or(0);
        let index: Vec<i64> = match stream.dict.get("Index").and_then(|o| o.as_arr()) {
            Some(a) => a.iter().filter_map(Obj::as_int).collect(),
            None => vec![0, size],
        };
        let decoded = self.stream_data(&stream, MAX_META_STREAM).ok()?;
        let row = w0 + w1 + w2;
        let mut cursor = 0usize;
        let field = |bytes: &[u8]| bytes.iter().fold(0u64, |acc, &b| acc << 8 | u64::from(b));
        for pair in index.chunks(2) {
            let (&start, &count) = (pair.first()?, pair.get(1)?);
            let (start, count) = (u32::try_from(start).ok()?, u32::try_from(count).ok()?);
            for i in 0..count {
                let Some(rec) = decoded.data.get(cursor..cursor + row) else { break };
                cursor += row;
                let t = if w0 == 0 { 1 } else { field(rec.get(..w0)?) };
                let a = field(rec.get(w0..w0 + w1)?);
                let b = field(rec.get(w0 + w1..)?);
                let num = start.checked_add(i)?;
                let entry = match t {
                    0 => Entry::Free,
                    1 => Entry::At(usize::try_from(a).ok()?),
                    2 => Entry::InStm(u32::try_from(a).ok()?, u32::try_from(b).ok()?),
                    _ => continue,
                };
                self.insert_entry(num, entry, override_free);
            }
        }
        Some(stream.dict.clone())
    }

    // ---- reconstruction ------------------------------------------------------------------

    /// Rebuilds the cross-reference data by scanning the file for `N G obj` headers.
    pub(super) fn rebuild(&mut self) {
        self.rebuilt = true;
        self.xref.clear();
        self.cache.clear();
        self.objstms.clear();
        self.loading.clear();
        let data = self.data;
        let mut objstms: Vec<(u32, usize)> = Vec::new();
        let mut xref_streams: Vec<usize> = Vec::new();
        let mut catalogs: Vec<u32> = Vec::new();
        let mut from = self.base;
        while let Some(p) = find(data, b"obj", from) {
            from = p + 3;
            if p == 0 || data.get(p - 1) == Some(&b'd') {
                continue; // "endobj"
            }
            let Some((num, gen_ok, start)) = obj_header_before(data, p) else { continue };
            let _ = gen_ok;
            if num > MAX_OBJ_NUM {
                continue;
            }
            let rel = start.saturating_sub(self.base);
            self.xref.insert(num, Entry::At(rel));
            let window = data.get(p + 3..(p + 3 + 400).min(data.len())).unwrap_or(&[]);
            if find(window, b"/ObjStm", 0).is_some() {
                objstms.push((num, rel));
            }
            if find(window, b"/XRef", 0).is_some() {
                xref_streams.push(start);
            }
            if find(window, b"/Catalog", 0).is_some() {
                catalogs.push(num);
            }
        }
        // Trailers, in file order; cross-reference streams double as trailers.
        let mut trailers: Vec<(usize, Dict)> = Vec::new();
        let mut from = self.base;
        while let Some(p) = find(data, b"trailer", from) {
            from = p + 7;
            let mut lx = Lexer::new(data, p + 7);
            if let Some(Obj::Dict(d)) = lx.object(true) {
                trailers.push((p, (*d).clone()));
            }
        }
        for start in xref_streams {
            if let Some(obj) = self.parse_at_pos(start, None)
                && let Some(d) = obj.as_dict()
            {
                trailers.push((start, d.clone()));
            }
        }
        trailers.sort_by_key(|(p, _)| *p);
        self.trailer = Dict::new();
        for (_, t) in trailers {
            for key in ["Root", "Encrypt", "Info", "ID"] {
                if let Some(v) = t.get(key) {
                    self.trailer.insert(Rc::from(key), v.clone());
                }
            }
        }
        // Objects stored in object streams (unless a later plain copy exists).
        for (num, rel) in objstms {
            let Some(os) = self.objstm(num) else { continue };
            let mut inner: Vec<(u32, u32)> =
                os.offsets.iter().map(|(n, (_, i))| (*n, *i)).collect();
            inner.sort_unstable();
            for (n, idx) in inner {
                let replace = match self.xref.get(&n) {
                    None | Some(Entry::Free | Entry::InStm(..)) => true,
                    Some(Entry::At(o)) => *o < rel,
                };
                if replace && n <= MAX_OBJ_NUM {
                    self.xref.insert(n, Entry::InStm(num, idx));
                }
            }
        }
        if !self.root_ok()
            && let Some(&c) = catalogs.last()
        {
            self.trailer.insert(Rc::from("Root"), Obj::Ref(c, 0));
        }
    }

    /// Object numbers in ascending order.
    pub(super) fn object_numbers(&self) -> Vec<u32> {
        let mut v: Vec<u32> =
            self.xref.iter().filter(|(_, e)| **e != Entry::Free).map(|(n, _)| *n).collect();
        v.sort_unstable();
        v
    }

    // ---- objects ---------------------------------------------------------------------------

    /// Loads object `num` (cached). Missing or unreadable objects are `Null`.
    pub(super) fn object(&mut self, num: u32) -> Obj {
        if let Some(o) = self.cache.get(&num) {
            return o.clone();
        }
        if !self.loading.insert(num) {
            return Obj::Null;
        }
        let o = self.load(num);
        self.loading.remove(&num);
        self.cache.insert(num, o.clone());
        o
    }

    fn load(&mut self, num: u32) -> Obj {
        match self.xref.get(&num).copied() {
            None | Some(Entry::Free) => Obj::Null,
            Some(Entry::At(off)) => {
                let Some(pos) = self.base.checked_add(off) else { return Obj::Null };
                self.parse_at_pos(pos, Some(num)).unwrap_or(Obj::Null)
            }
            Some(Entry::InStm(stm, _)) => {
                let Some(os) = self.objstm(stm) else { return Obj::Null };
                let Some(&(off, _)) = os.offsets.get(&num) else { return Obj::Null };
                let mut lx = Lexer::new(&os.data, os.first.saturating_add(off));
                match lx.object(true) {
                    Some(o) => o,
                    None => Obj::Null,
                }
            }
        }
    }

    /// Parses `N G obj ... endobj` at absolute position `pos`. With `expect`, the object
    /// number must match.
    fn parse_at_pos(&mut self, pos: usize, expect: Option<u32>) -> Option<Obj> {
        let data = self.data;
        let mut lx = Lexer::new(data, pos);
        let Tok::Int(n) = lx.next_tok() else { return None };
        let Tok::Int(_) = lx.next_tok() else { return None };
        if lx.next_tok() != Tok::Kw(b"obj") {
            return None;
        }
        if let Some(e) = expect
            && u32::try_from(n).ok() != Some(e)
        {
            return None;
        }
        let Some(obj) = lx.object(true) else { return Some(Obj::Null) };
        let Obj::Dict(dict) = &obj else { return Some(obj) };
        if lx.next_tok() != Tok::Kw(b"stream") {
            return Some(obj);
        }
        let mut start = lx.pos;
        match data.get(start) {
            Some(b'\r') => {
                start += 1;
                if data.get(start) == Some(&b'\n') {
                    start += 1;
                }
            }
            Some(b'\n') => start += 1,
            _ => {}
        }
        let end = self.stream_end(dict, start);
        Some(Obj::Stream(Rc::new(Stream { dict: (**dict).clone(), start, end })))
    }

    /// Finds the end of a stream's raw data: by `/Length` if it is consistent, else by
    /// scanning for `endstream`.
    fn stream_end(&mut self, dict: &Dict, start: usize) -> usize {
        let data = self.data;
        let declared = match dict.get("Length") {
            Some(Obj::Int(n)) => usize::try_from(*n).ok(),
            Some(Obj::Ref(r, _)) => {
                let r = *r;
                self.object(r).as_int().and_then(|n| usize::try_from(n).ok())
            }
            _ => None,
        };
        if let Some(len) = declared
            && let Some(end) = start.checked_add(len)
            && end <= data.len()
        {
            let mut lx = Lexer::new(data, end);
            if lx.next_tok() == Tok::Kw(b"endstream") {
                return end;
            }
        }
        let mut end = match find(data, b"endstream", start) {
            Some(p) => p,
            None => find(data, b"endobj", start).unwrap_or(data.len()),
        };
        // Drop the end-of-line that precedes the keyword.
        if end > start && data.get(end - 1) == Some(&b'\n') {
            end -= 1;
        }
        if end > start && data.get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
        end.max(start)
    }

    fn objstm(&mut self, num: u32) -> Option<Rc<ObjStm>> {
        if let Some(c) = self.objstms.get(&num) {
            return c.clone();
        }
        // Mark as failed first: an object stream may not (indirectly) contain itself.
        self.objstms.insert(num, None);
        let built = self.build_objstm(num).map(Rc::new);
        self.objstms.insert(num, built.clone());
        built
    }

    fn build_objstm(&mut self, num: u32) -> Option<ObjStm> {
        let obj = self.object(num);
        let s = obj.as_stream()?.clone();
        let n = usize::try_from(s.dict.get("N").and_then(Obj::as_int)?).ok()?;
        let first = usize::try_from(s.dict.get("First").and_then(Obj::as_int)?).ok()?;
        let decoded = match self.stream_data(&s, MAX_META_STREAM) {
            Ok(d) => d,
            Err(_) => {
                self.note("object stream");
                return None;
            }
        };
        let mut offsets = HashMap::new();
        let mut lx = Lexer::new(&decoded.data, 0);
        for idx in 0..n.min(decoded.data.len() / 2 + 1) {
            let (Tok::Int(on), Tok::Int(off)) = (lx.next_tok(), lx.next_tok()) else { break };
            let (Ok(on), Ok(off)) = (u32::try_from(on), usize::try_from(off)) else { continue };
            offsets.entry(on).or_insert((off, u32::try_from(idx).unwrap_or(u32::MAX)));
        }
        Some(ObjStm { data: decoded.data, first, offsets })
    }

    /// Follows references (at most [`MAX_HOPS`]).
    pub(super) fn resolve(&mut self, o: &Obj) -> Obj {
        let mut cur = o.clone();
        for _ in 0..MAX_HOPS {
            match cur {
                Obj::Ref(n, _) => cur = self.object(n),
                other => return other,
            }
        }
        Obj::Null
    }

    /// The resolved value of `key` in `d`, `Null` when absent.
    pub(super) fn get(&mut self, d: &Dict, key: &str) -> Obj {
        match d.get(key) {
            Some(o) => {
                let o = o.clone();
                self.resolve(&o)
            }
            None => Obj::Null,
        }
    }

    /// Elements of an array value, each resolved.
    pub(super) fn get_array(&mut self, d: &Dict, key: &str) -> Vec<Obj> {
        let v = self.get(d, key);
        match v.as_arr() {
            Some(a) => a.iter().map(|o| self.resolve(o)).collect(),
            None => Vec::new(),
        }
    }

    /// Numbers of an array value (`MediaBox` and the like).
    pub(super) fn get_numbers(&mut self, d: &Dict, key: &str) -> Vec<f64> {
        self.get_array(d, key).iter().filter_map(Obj::as_f64).collect()
    }

    // ---- streams -------------------------------------------------------------------------------

    fn filter_specs(&mut self, dict: &Dict) -> Vec<FilterSpec> {
        let filter = self.get(dict, "Filter");
        let names: Vec<String> = match &filter {
            Obj::Name(n) => vec![n.to_string()],
            Obj::Arr(a) => {
                a.iter().filter_map(|o| self.resolve(o).as_name().map(str::to_owned)).collect()
            }
            _ => Vec::new(),
        };
        let parms = self.get(dict, "DecodeParms");
        let parm_dicts: Vec<Option<Rc<Dict>>> = match &parms {
            Obj::Dict(d) => vec![Some(d.clone())],
            Obj::Arr(a) => a
                .iter()
                .map(|o| match self.resolve(o) {
                    Obj::Dict(d) => Some(d),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let mut spec = FilterSpec::new(name);
            if let Some(Some(p)) = parm_dicts.get(i) {
                let p = p.clone();
                let num =
                    |f: &mut Self, k: &str, default: i64| f.get(&p, k).as_int().unwrap_or(default);
                spec.predictor = num(self, "Predictor", 1);
                spec.colors = num(self, "Colors", 1);
                spec.bpc = num(self, "BitsPerComponent", 8);
                spec.columns = num(self, "Columns", 1);
                spec.early_change = num(self, "EarlyChange", 1);
            }
            out.push(spec);
        }
        out
    }

    /// Decodes a stream, capped at `max` bytes and charged to the document's budget.
    pub(super) fn stream_data(&mut self, s: &Stream, max: usize) -> Result<Decoded, FilterError> {
        let specs = self.filter_specs(&s.dict);
        let raw = self.data.get(s.start..s.end).unwrap_or(&[]);
        filter::apply(raw, &specs, max, &mut self.budget)
    }

    /// Raw (encoded) length of a stream, for picture placeholders.
    pub(super) fn raw_len(&self, s: &Stream) -> usize {
        s.end.saturating_sub(s.start)
    }
}

/// Parses `N G` backwards from the `obj` keyword at `p`. Returns the object number, whether
/// the generation parsed, and the offset of the first digit of `N`.
fn obj_header_before(data: &[u8], p: usize) -> Option<(u32, bool, usize)> {
    let mut j = p;
    while j > 0 && data.get(j - 1).is_some_and(|&b| is_ws(b)) {
        j -= 1;
    }
    let gen_end = j;
    while j > 0 && data.get(j - 1).is_some_and(u8::is_ascii_digit) {
        j -= 1;
    }
    if j == gen_end || gen_end - j > 5 {
        return None;
    }
    let gen_start = j;
    let ws_end = j;
    while j > 0 && data.get(j - 1).is_some_and(|&b| is_ws(b)) {
        j -= 1;
    }
    if j == ws_end {
        return None;
    }
    let num_end = j;
    while j > 0 && data.get(j - 1).is_some_and(u8::is_ascii_digit) {
        j -= 1;
    }
    if j == num_end || num_end - j > 10 {
        return None;
    }
    // The number must start a token.
    if j > 0 && data.get(j - 1).is_some_and(|&b| !is_ws(b) && !is_delim(b)) {
        return None;
    }
    let num: u32 = std::str::from_utf8(data.get(j..num_end)?).ok()?.parse().ok()?;
    Some((num, gen_start < gen_end, j))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::testpdf::PdfBuilder;

    fn open(data: &[u8]) -> Result<PdfFile<'_>, PreviewError> {
        PdfFile::open(data, &PreviewPolicy::default())
    }

    fn page_count(f: &mut PdfFile<'_>) -> usize {
        let root = f.get(&f.trailer.clone(), "Root");
        let pages = f.get(root.as_dict().unwrap_or(&Dict::new()), "Pages");
        f.get_array(pages.as_dict().unwrap_or(&Dict::new()), "Kids").len()
    }

    #[test]
    fn classic_table() {
        let data = PdfBuilder::simple_text("Hello").build();
        let mut f = open(&data).expect("open");
        assert!(!f.rebuilt());
        assert_eq!(page_count(&mut f), 1);
    }

    #[test]
    fn xref_stream_with_object_streams() {
        let data = PdfBuilder::simple_text("Hello").build_modern();
        let mut f = open(&data).expect("open");
        assert!(!f.rebuilt(), "modern layout must be read without rebuilding");
        assert_eq!(page_count(&mut f), 1);
        assert!(f.objstms.values().any(Option::is_some), "an object stream was used");
    }

    #[test]
    fn damaged_startxref_is_rebuilt() {
        for build in [PdfBuilder::build as fn(&PdfBuilder) -> Vec<u8>, PdfBuilder::build_modern] {
            let mut data = build(&PdfBuilder::simple_text("Hello"));
            // Corrupt the offset behind startxref.
            let p = find(&data, b"startxref", 0).expect("startxref");
            for b in data.iter_mut().skip(p + 10).take(4) {
                *b = b'9';
            }
            let mut f = open(&data).expect("open");
            assert!(f.rebuilt());
            assert_eq!(page_count(&mut f), 1);
        }
    }

    #[test]
    fn truncated_file_is_rebuilt() {
        let data = PdfBuilder::simple_text("Hello").build();
        let cut = data.get(..data.len() - 120).expect("cut");
        let mut f = open(cut).expect("open");
        assert!(f.rebuilt());
        assert_eq!(page_count(&mut f), 1);
    }

    #[test]
    fn junk_before_header_shifts_offsets() {
        let mut data = b"GARBAGE\r\n".to_vec();
        data.extend(PdfBuilder::simple_text("Hello").build());
        let mut f = open(&data).expect("open");
        assert_eq!(page_count(&mut f), 1);
    }

    #[test]
    fn incremental_update_wins() {
        let b = PdfBuilder::simple_text("Hello");
        let mut data = b.build();
        // Append a new version of object 4 (the content stream) and a second xref section.
        let prev = find(&data, b"startxref", 0).map(|p| p + 10).expect("startxref");
        let prev_off: usize = std::str::from_utf8(data.get(prev..prev + 8).expect("digits"))
            .expect("utf8")
            .split_whitespace()
            .next()
            .expect("token")
            .parse()
            .expect("number");
        let off = data.len();
        data.extend(b"4 0 obj\n<< /Length 9 >>\nstream\nBT ET q Q\nendstream\nendobj\n");
        let xref_off = data.len();
        data.extend(format!("xref\n4 1\n{off:010} 00000 n \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {prev_off} >>\nstartxref\n{xref_off}\n%%EOF\n").as_bytes());
        let mut f = open(&data).expect("open");
        assert!(!f.rebuilt());
        let o = f.object(4);
        let s = o.as_stream().expect("stream").clone();
        let d = f.stream_data(&s, 1000).expect("decode");
        assert_eq!(d.data, b"BT ET q Q");
    }

    #[test]
    fn free_entries_hide_old_objects() {
        let data = PdfBuilder::simple_text("Hello").build();
        let mut f = open(&data).expect("open");
        f.xref.insert(4, Entry::Free);
        f.cache.clear();
        assert!(f.object(4).is_null());
    }

    #[test]
    fn wrong_stream_length_is_repaired() {
        let data = PdfBuilder::simple_text("Hello").with_length_lie().build();
        let mut f = open(&data).expect("open");
        let o = f.object(4);
        let s = o.as_stream().expect("stream").clone();
        let d = f.stream_data(&s, 1 << 20).expect("decode");
        assert!(d.data.starts_with(b"BT"), "{:?}", String::from_utf8_lossy(&d.data));
        assert!(d.data.ends_with(b"ET"));
    }

    #[test]
    fn encrypted_is_reported() {
        let data = PdfBuilder::simple_text("Hello").with_trailer_entry("/Encrypt 9 0 R").build();
        assert_eq!(open(&data).err(), Some(PreviewError::Encrypted));
    }

    #[test]
    fn reference_loops_and_self_containing_streams_terminate() {
        let mut b = PdfBuilder::simple_text("Hello");
        b.add_raw(7, "7 0 R");
        b.add_raw(8, "9 0 R");
        b.add_raw(9, "8 0 R");
        let data = b.build();
        let mut f = open(&data).expect("open");
        assert!(f.resolve(&Obj::Ref(7, 0)).is_null());
        assert!(f.resolve(&Obj::Ref(8, 0)).is_null());
    }

    #[test]
    fn prev_loop_terminates() {
        let mut data = PdfBuilder::simple_text("Hello").build();
        let p = find(&data, b"trailer", 0).expect("trailer");
        // Make /Prev point at the table itself.
        let table = find(&data, b"xref", 0).expect("xref");
        let patched = format!("trailer\n<< /Size 6 /Root 1 0 R /Prev {table} >>\n");
        let end = find(&data, b"startxref", p).expect("startxref");
        data.splice(p..end, patched.into_bytes());
        // startxref now points to a wrong offset; whatever happens, it must terminate.
        let _ = open(&data);
    }

    #[test]
    fn no_catalog_is_malformed() {
        assert!(matches!(
            open(b"%PDF-1.4\n1 0 obj\n<< >>\nendobj\n"),
            Err(PreviewError::Malformed(_))
        ));
        assert!(matches!(open(b"%PDF-1.4\n"), Err(PreviewError::Malformed(_))));
    }

    #[test]
    fn mutated_files_never_panic() {
        for base in [
            PdfBuilder::simple_text("Hello").build(),
            PdfBuilder::simple_text("Hello").build_modern(),
        ] {
            for i in 0..base.len() {
                for mask in [0x01u8, 0x80, 0xFF] {
                    let mut m = base.clone();
                    if let Some(b) = m.get_mut(i) {
                        *b ^= mask;
                    }
                    if let Ok(mut f) = open(&m) {
                        let _ = page_count(&mut f);
                    }
                }
            }
        }
    }
}
