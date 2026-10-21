//! A sterile copy of a PDF.
//!
//! Instead of cutting dangerous things out of the file, the copy is **built up** from what the
//! pages need: a new catalog, a flat page tree, and for every page its boxes, rotation,
//! resources and content streams, with everything those refer to (fonts, pictures, forms,
//! colour spaces) copied once and renumbered. Nothing else is looked at, so nothing else can
//! come along:
//!
//! * JavaScript, open actions, additional actions, launch/URI/submit/import actions, form
//!   definitions (AcroForm, XFA), embedded files and portfolios, rich media, signatures, the
//!   document outline and named destinations never enter the copy;
//! * annotations are not copied as annotations, but their *appearance* is drawn into the page
//!   (a stamp, a highlight, the text typed into a form field stay visible; links, their
//!   targets and pop-ups go);
//! * the document information, XMP metadata, thumbnails and structure tree are dropped;
//! * old revisions, deleted objects and bytes after the last `%%EOF` are not reachable from the
//!   pages, so they do not survive either.
//!
//! Content streams, fonts and pictures are copied byte for byte (still compressed): nothing is
//! decoded, so nothing in the copy depends on a decoder being correct. The file is written
//! with a classic cross-reference table, in a fixed order, so the result is reproducible.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;

use arca_xray::sanitize::{ActionKind, SanitizeAction};

use super::file::PdfFile;
use super::lex::{Dict, Obj, Stream};
use super::{collect_pages, scan_for_pages};
use crate::{PreviewError, PreviewPolicy};

/// Objects in the copy.
const MAX_OBJECTS: usize = 1_000_000;
/// Pages in the copy.
const MAX_PAGES: usize = 100_000;
/// Annotations whose appearance is drawn on one page.
const MAX_ANNOTS: usize = 500;
/// Nesting of containers written.
const MAX_DEPTH: usize = 48;

/// Keys that never enter the copy, wherever they are found.
const DROP_KEYS: &[&str] = &[
    "A",
    "AA",
    "OpenAction",
    "JS",
    "JavaScript",
    "Launch",
    "URI",
    "Metadata",
    "PieceInfo",
    "Thumb",
    "StructParents",
    "StructTreeRoot",
    "EmbeddedFile",
    "EmbeddedFiles",
    "EF",
    "RF",
    "RichMedia",
    "RichMediaContent",
    "XFA",
    "AcroForm",
    "Annots",
    "Outlines",
    "Names",
    "Dests",
    "Perms",
    "DSS",
    "SpiderInfo",
    "Collection",
    "Threads",
    "Legal",
    "Requirements",
    "AF",
    "Parent",
    "Next",
    "Prev",
    "First",
    "Last",
];

/// Keys of a stream dictionary that point at an external file or are rewritten.
const STREAM_DROP_KEYS: &[&str] = &["F", "FFilter", "FDecodeParms", "DL", "Length"];

/// Action types: a dictionary whose `/S` is one of these is not copied.
const ACTIONS: &[&str] = &[
    "JavaScript",
    "Launch",
    "URI",
    "SubmitForm",
    "ResetForm",
    "ImportData",
    "GoToR",
    "GoToE",
    "Rendition",
    "Movie",
    "Sound",
    "Hide",
    "Thread",
    "RichMediaExecute",
    "GoTo3DView",
    "SetOCGState",
    "Named",
];

/// What the caller may ask to keep.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CleanOptions {
    /// Drop `/Info` (author, producer, dates); otherwise it is copied.
    pub(crate) strip_metadata: bool,
    /// Leave out the appearance of review comments (notes, highlights, drawings).
    pub(crate) remove_comments: bool,
}

fn action(kind: ActionKind, target: impl Into<String>) -> SanitizeAction {
    SanitizeAction { kind, target: arca_container::text::display_safe(&target.into(), 300) }
}

struct Cleaner<'f, 'a> {
    pdf: &'f mut PdfFile<'a>,
    /// Old object number → number in the copy.
    map: HashMap<u32, u32>,
    queue: VecDeque<u32>,
    /// Objects of the copy; index `i` is object `i + 1`.
    objs: Vec<Option<Vec<u8>>>,
    /// Streams that must be Form XObjects (appearance streams drawn into a page).
    forms: HashSet<u32>,
    bytes: usize,
    max_bytes: usize,
    overflow: bool,
}

impl Cleaner<'_, '_> {
    /// Reserves a number for a new object that is filled in later.
    fn reserve(&mut self) -> u32 {
        if self.objs.len() >= MAX_OBJECTS {
            self.overflow = true;
            return 0;
        }
        self.objs.push(None);
        u32::try_from(self.objs.len()).unwrap_or(0)
    }

    fn fill(&mut self, num: u32, body: Vec<u8>) {
        self.bytes = self.bytes.saturating_add(body.len());
        if self.bytes > self.max_bytes {
            self.overflow = true;
        }
        if let Some(slot) = usize::try_from(num)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| self.objs.get_mut(i))
        {
            *slot = Some(body);
        }
    }

    /// The number the copy gives to old object `old`; it is copied when the queue gets to it.
    fn number_of(&mut self, old: u32) -> u32 {
        if let Some(n) = self.map.get(&old) {
            return *n;
        }
        let n = self.reserve();
        if n != 0 {
            self.map.insert(old, n);
            self.queue.push_back(old);
        }
        n
    }

    /// Copies every object that was asked for, and everything those refer to.
    fn drain(&mut self) {
        while let Some(old) = self.queue.pop_front() {
            if self.overflow {
                return;
            }
            let Some(new) = self.map.get(&old).copied() else { continue };
            let obj = self.pdf.object(old);
            let mut out = Vec::new();
            match &obj {
                Obj::Stream(s) => self.write_stream(old, s, &mut out),
                other => self.write(other, &mut out, 0),
            }
            self.fill(new, out);
        }
    }

    // ---- writing objects ------------------------------------------------------------------

    fn write(&mut self, o: &Obj, out: &mut Vec<u8>, depth: usize) {
        if depth > MAX_DEPTH {
            out.extend_from_slice(b"null");
            return;
        }
        match o {
            Obj::Null | Obj::Stream(_) => out.extend_from_slice(b"null"),
            Obj::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Obj::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
            Obj::Real(r) => out.extend_from_slice(real(*r).as_bytes()),
            Obj::Str(s) => hex_string(s, out),
            Obj::Name(n) => name(n, out),
            Obj::Arr(items) => {
                out.push(b'[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    self.write(it, out, depth + 1);
                }
                out.push(b']');
            }
            Obj::Dict(d) => self.write_dict(d, out, depth, &[]),
            Obj::Ref(n, _) => {
                let new = self.number_of(*n);
                if new == 0 {
                    out.extend_from_slice(b"null");
                } else {
                    out.extend_from_slice(format!("{new} 0 R").as_bytes());
                }
            }
        }
    }

    fn write_dict(&mut self, d: &Dict, out: &mut Vec<u8>, depth: usize, extra_drop: &[&str]) {
        if is_action(d) {
            out.extend_from_slice(b"null");
            return;
        }
        out.extend_from_slice(b"<<");
        for (k, v) in d {
            if DROP_KEYS.contains(&&**k) || extra_drop.contains(&&**k) {
                continue;
            }
            out.push(b' ');
            name(k, out);
            out.push(b' ');
            self.write(v, out, depth + 1);
        }
        out.extend_from_slice(b" >>");
    }

    fn write_stream(&mut self, old: u32, s: &Rc<Stream>, out: &mut Vec<u8>) {
        let ty = s.dict.get("Type").and_then(Obj::as_name).unwrap_or("");
        if matches!(ty, "EmbeddedFile" | "Metadata") {
            out.extend_from_slice(b"null");
            return;
        }
        let raw = self.pdf.raw(s).into_owned();
        out.extend_from_slice(b"<<");
        for (k, v) in &s.dict {
            if DROP_KEYS.contains(&&**k) || STREAM_DROP_KEYS.contains(&&**k) {
                continue;
            }
            out.push(b' ');
            name(k, out);
            out.push(b' ');
            self.write(v, out, 1);
        }
        if self.forms.contains(&old) {
            if !s.dict.contains_key("Type") {
                out.extend_from_slice(b" /Type /XObject");
            }
            if !s.dict.contains_key("Subtype") {
                out.extend_from_slice(b" /Subtype /Form");
            }
        }
        out.extend_from_slice(format!(" /Length {} >>\nstream\n", raw.len()).as_bytes());
        out.extend_from_slice(&raw);
        out.extend_from_slice(b"\nendstream");
    }
}

fn is_action(d: &Dict) -> bool {
    let s = d.get("S").and_then(Obj::as_name).unwrap_or("");
    let ty = d.get("Type").and_then(Obj::as_name).unwrap_or("");
    ACTIONS.contains(&s) || matches!(ty, "Action" | "Filespec")
}

pub(super) fn real(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_owned();
    }
    let s = format!("{:.6}", v.clamp(-1.0e12, 1.0e12));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" { "0".to_owned() } else { s.to_owned() }
}

pub(super) fn hex_string(s: &[u8], out: &mut Vec<u8>) {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    out.push(b'<');
    for b in s {
        out.push(*H.get(usize::from(b >> 4)).unwrap_or(&b'0'));
        out.push(*H.get(usize::from(b & 15)).unwrap_or(&b'0'));
    }
    out.push(b'>');
}

pub(super) fn name(n: &str, out: &mut Vec<u8>) {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    out.push(b'/');
    for b in n.bytes() {
        let plain = b > 0x20
            && b < 0x7F
            && !matches!(
                b,
                b'#' | b'/' | b'%' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}'
            );
        if plain {
            out.push(b);
        } else {
            out.push(b'#');
            out.push(*H.get(usize::from(b >> 4)).unwrap_or(&b'0'));
            out.push(*H.get(usize::from(b & 15)).unwrap_or(&b'0'));
        }
    }
}

// ---- pages -----------------------------------------------------------------------------------

/// A page attribute that may be inherited, as written (references stay references).
fn inherited_raw(pdf: &mut PdfFile<'_>, page: &Dict, key: &str) -> Obj {
    let mut cur = Rc::new(page.clone());
    for _ in 0..32 {
        if let Some(v) = cur.get(key)
            && !v.is_null()
        {
            return v.clone();
        }
        let parent = cur.get("Parent").cloned().unwrap_or_default();
        match pdf.resolve(&parent) {
            Obj::Dict(p) => cur = p,
            _ => break,
        }
    }
    Obj::Null
}

/// Four numbers of a box, if it is one.
fn numbers4(pdf: &mut PdfFile<'_>, o: &Obj) -> Option<[f64; 4]> {
    let arr = pdf.resolve(o);
    let nums: Vec<f64> =
        arr.as_arr()?.iter().map(|x| pdf.resolve(x)).filter_map(|x| x.as_f64()).collect();
    match nums.as_slice() {
        [a, b, c, d] => Some([*a, *b, *c, *d]),
        _ => None,
    }
}

const MARKUP: &[&str] = &[
    "Text",
    "FreeText",
    "Highlight",
    "Underline",
    "Squiggly",
    "StrikeOut",
    "Caret",
    "Line",
    "Square",
    "Circle",
    "Polygon",
    "PolyLine",
    "Ink",
];

/// What happened to the annotations of the whole document.
#[derive(Default)]
struct AnnotStats {
    flattened: u64,
    links: u64,
    comments: u64,
    hidden: u64,
    no_appearance: u64,
    targets: BTreeMap<String, u64>,
}

/// The annotation appearances of a page, to be drawn into it: `(old stream number, matrix)`.
fn annotation_forms(
    pdf: &mut PdfFile<'_>,
    page: &Dict,
    opts: CleanOptions,
    stats: &mut AnnotStats,
) -> Vec<(u32, [f64; 6])> {
    let mut out = Vec::new();
    let annots = pdf.get(page, "Annots");
    let Some(list) = annots.as_arr() else { return out };
    for (i, a) in list.iter().enumerate() {
        if i >= MAX_ANNOTS {
            break;
        }
        let Obj::Dict(d) = pdf.resolve(a) else { continue };
        let subtype = pdf.get(&d, "Subtype").as_name().unwrap_or("").to_owned();
        // Where a link or action leads is reported, then dropped with the annotation.
        let act = pdf.get(&d, "A");
        if let Some(ad) = act.as_dict() {
            let kind = pdf.get(ad, "S").as_name().unwrap_or("").to_owned();
            let target = match pdf.get(ad, "URI") {
                Obj::Str(s) => String::from_utf8_lossy(&s).into_owned(),
                _ => String::new(),
            };
            *stats.targets.entry(format!("{kind} {target}").trim().to_owned()).or_default() += 1;
        }
        if subtype == "Link" {
            stats.links += 1;
            continue;
        }
        if subtype == "Popup" {
            continue;
        }
        let flags = pdf.get(&d, "F").as_int().unwrap_or(0);
        // Hidden (bit 2) and NoView (bit 6) annotations are not shown, so they are not drawn.
        if flags & 0b10 != 0 || flags & 0b10_0000 != 0 {
            stats.hidden += 1;
            continue;
        }
        if opts.remove_comments && MARKUP.contains(&subtype.as_str()) {
            stats.comments += 1;
            continue;
        }
        let Some(rect) = d.get("Rect").and_then(|r| numbers4(pdf, r)) else { continue };
        // The normal appearance; for a field with states (a check box) the current one.
        let ap = pdf.get(&d, "AP");
        let Some(ap) = ap.as_dict() else {
            stats.no_appearance += 1;
            continue;
        };
        let Some(normal_raw) = ap.get("N").cloned() else {
            stats.no_appearance += 1;
            continue;
        };
        let normal_raw = match pdf.resolve(&normal_raw) {
            Obj::Dict(states) => {
                let state = pdf.get(&d, "AS").as_name().unwrap_or("").to_owned();
                states.get(state.as_str()).cloned().unwrap_or_default()
            }
            _ => normal_raw,
        };
        let Obj::Ref(num, _) = normal_raw else {
            stats.no_appearance += 1;
            continue;
        };
        let Obj::Stream(form) = pdf.object(num) else {
            stats.no_appearance += 1;
            continue;
        };
        let bbox =
            form.dict.get("BBox").and_then(|b| numbers4(pdf, b)).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let m = form
            .dict
            .get("Matrix")
            .map(|m| pdf.resolve(m))
            .and_then(|m| {
                let v: Vec<f64> = m.as_arr()?.iter().filter_map(Obj::as_f64).collect();
                <[f64; 6]>::try_from(v).ok()
            })
            .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        out.push((num, appearance_matrix(bbox, m, rect)));
        stats.flattened += 1;
    }
    out
}

/// ISO 32000-1 §12.5.5, algorithm 8.1: the matrix that puts the transformed appearance box
/// onto the annotation rectangle.
fn appearance_matrix(bbox: [f64; 4], m: [f64; 6], rect: [f64; 4]) -> [f64; 6] {
    let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[0], bbox[3]), (bbox[2], bbox[3])];
    let (mut llx, mut lly, mut urx, mut ury) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        let tx = m[0] * x + m[2] * y + m[4];
        let ty = m[1] * x + m[3] * y + m[5];
        llx = llx.min(tx);
        urx = urx.max(tx);
        lly = lly.min(ty);
        ury = ury.max(ty);
    }
    let (rx0, rx1) = (rect[0].min(rect[2]), rect[0].max(rect[2]));
    let (ry0, ry1) = (rect[1].min(rect[3]), rect[1].max(rect[3]));
    let (bw, bh) = (urx - llx, ury - lly);
    let sx = if bw.abs() > 1e-9 { (rx1 - rx0) / bw } else { 1.0 };
    let sy = if bh.abs() > 1e-9 { (ry1 - ry0) / bh } else { 1.0 };
    [sx, 0.0, 0.0, sy, rx0 - llx * sx, ry0 - lly * sy]
}

/// Writes one page and returns its number.
fn copy_page(
    c: &mut Cleaner<'_, '_>,
    page: &Dict,
    parent: u32,
    opts: CleanOptions,
    stats: &mut AnnotStats,
) -> u32 {
    let num = c.reserve();
    if num == 0 {
        return 0;
    }
    let mut body = Vec::new();
    body.extend_from_slice(format!("<< /Type /Page /Parent {parent} 0 R").as_bytes());
    for key in ["MediaBox", "CropBox", "BleedBox", "TrimBox", "ArtBox", "Rotate", "UserUnit"] {
        let v = inherited_raw(c.pdf, page, key);
        if !v.is_null() {
            body.push(b' ');
            name(key, &mut body);
            body.push(b' ');
            c.write(&v, &mut body, 1);
        }
    }

    // Annotations first: they add to the contents and to the resources.
    let forms = annotation_forms(c.pdf, page, opts, stats);

    // Resources: a copy of the dictionary, with the appearance forms added.
    let resources = match inherited_raw(c.pdf, page, "Resources") {
        Obj::Null => Obj::Dict(Rc::new(Dict::new())),
        other => c.pdf.resolve(&other),
    };
    let mut resources = resources.as_dict().cloned().unwrap_or_default();
    let mut extra_stream = Vec::new();
    if !forms.is_empty() {
        let mut xobjects = match resources.get("XObject") {
            Some(x) => c.pdf.resolve(x).as_dict().cloned().unwrap_or_default(),
            None => Dict::new(),
        };
        for (i, (old, m)) in forms.iter().enumerate() {
            let mut key = format!("ArcaAnnot{i}");
            while xobjects.contains_key(key.as_str()) {
                key.push('x');
            }
            c.forms.insert(*old);
            xobjects.insert(Rc::from(key.as_str()), Obj::Ref(*old, 0));
            extra_stream.extend_from_slice(
                format!(
                    "q {} {} {} {} {} {} cm /{key} Do Q\n",
                    real(m[0]),
                    real(m[1]),
                    real(m[2]),
                    real(m[3]),
                    real(m[4]),
                    real(m[5])
                )
                .as_bytes(),
            );
        }
        resources.insert(Rc::from("XObject"), Obj::Dict(Rc::new(xobjects)));
    }
    body.extend_from_slice(b" /Resources ");
    c.write_dict(&resources, &mut body, 1, &[]);

    // Contents: the original streams, then the drawn annotations.
    let mut parts: Vec<Obj> = match page.get("Contents").cloned().unwrap_or_default() {
        r @ Obj::Ref(..) => match c.pdf.resolve(&r) {
            Obj::Arr(a) => a.to_vec(),
            _ => vec![r],
        },
        Obj::Arr(a) => a.to_vec(),
        _ => Vec::new(),
    };
    parts.truncate(100_000);
    body.extend_from_slice(b" /Contents [");
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            body.push(b' ');
        }
        c.write(p, &mut body, 1);
    }
    if !extra_stream.is_empty() {
        let n = c.reserve();
        if n != 0 {
            let mut s = format!("<< /Length {} >>\nstream\n", extra_stream.len()).into_bytes();
            s.extend_from_slice(&extra_stream);
            s.extend_from_slice(b"\nendstream");
            c.fill(n, s);
            if !parts.is_empty() {
                body.push(b' ');
            }
            body.extend_from_slice(format!("{n} 0 R").as_bytes());
        }
    }
    body.push(b']');

    // The page group (transparency, colour space of the page), if it is a plain dictionary.
    if let Some(Obj::Dict(g)) = page.get("Group").map(|g| c.pdf.resolve(g)) {
        body.extend_from_slice(b" /Group ");
        c.write_dict(&g, &mut body, 1, &[]);
    }
    body.extend_from_slice(b" >>");
    c.fill(num, body);
    num
}

// ---- the file --------------------------------------------------------------------------------

/// Builds the sterile copy of `data`. Returns the new file and the changes made.
///
/// # Errors
/// [`PreviewError::Encrypted`] for a password-protected file, [`PreviewError::Malformed`] when
/// no page can be found or a limit is exceeded.
pub(crate) fn clean(
    data: &[u8],
    policy: &PreviewPolicy,
    opts: CleanOptions,
) -> Result<(Vec<u8>, Vec<SanitizeAction>), PreviewError> {
    let mut pdf = PdfFile::open(data, policy)?;
    let trailer = pdf.trailer.clone();
    let root = pdf.get(&trailer, "Root");
    let catalog = root.as_dict().cloned().unwrap_or_default();
    let mut pages = collect_pages(&mut pdf, &catalog, MAX_PAGES + 1);
    if pages.is_empty() && !pdf.rebuilt() {
        pdf.rebuild();
        let trailer = pdf.trailer.clone();
        let root = pdf.get(&trailer, "Root");
        let cat = root.as_dict().cloned().unwrap_or_default();
        pages = collect_pages(&mut pdf, &cat, MAX_PAGES + 1);
    }
    if pages.is_empty() {
        pages = scan_for_pages(&mut pdf, MAX_PAGES + 1);
    }
    if pages.is_empty() {
        return Err(PreviewError::Malformed("PDF has no pages".into()));
    }
    if pages.len() > MAX_PAGES {
        return Err(PreviewError::Malformed("PDF has too many pages to rebuild".into()));
    }

    let mut actions: Vec<SanitizeAction> = Vec::new();
    note_catalog(&mut pdf, &catalog, &trailer, opts, &mut actions);

    let object_count = pdf.object_numbers().len();
    let mut c = Cleaner {
        pdf: &mut pdf,
        map: HashMap::new(),
        queue: VecDeque::new(),
        objs: Vec::new(),
        forms: HashSet::new(),
        bytes: 0,
        max_bytes: policy.max_input_bytes.saturating_mul(2).saturating_add(1 << 20),
        overflow: false,
    };
    let catalog_num = c.reserve();
    let pages_num = c.reserve();
    let mut stats = AnnotStats::default();
    let mut kids = Vec::new();
    for page in &pages {
        let n = copy_page(&mut c, page, pages_num, opts, &mut stats);
        if n != 0 {
            kids.push(n);
        }
        // Fonts, pictures and forms are copied as the pages ask for them.
        c.drain();
        if c.overflow {
            return Err(PreviewError::Malformed("the PDF is too large to rebuild".into()));
        }
    }
    let kids_text: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
    c.fill(catalog_num, format!("<< /Type /Catalog /Pages {pages_num} 0 R >>").into_bytes());
    c.fill(
        pages_num,
        format!("<< /Type /Pages /Count {} /Kids [{}] >>", kids.len(), kids_text.join(" "))
            .into_bytes(),
    );

    // Optional-content settings keep hidden layers hidden.
    let mut catalog_extra = Vec::new();
    if let Some(oc) = catalog.get("OCProperties") {
        let mut body = Vec::new();
        c.write(oc, &mut body, 1);
        catalog_extra = body;
        c.drain();
    }
    if !catalog_extra.is_empty() {
        let mut body =
            format!("<< /Type /Catalog /Pages {pages_num} 0 R /OCProperties ").into_bytes();
        body.extend_from_slice(&catalog_extra);
        body.extend_from_slice(b" >>");
        c.fill(catalog_num, body);
    }
    // The document information is kept only when asked for.
    let info_num = match trailer.get("Info") {
        Some(Obj::Ref(n, _)) if !opts.strip_metadata => Some(c.number_of(*n)).filter(|m| *m != 0),
        _ => None,
    };
    c.drain();
    if c.overflow {
        return Err(PreviewError::Malformed("the PDF is too large to rebuild".into()));
    }

    note_annotations(&stats, &mut actions);
    let kept = c.map.len();
    let unreachable = object_count.saturating_sub(kept);
    if unreachable > 0 {
        actions.push(action(
            ActionKind::RemovedElement,
            format!("{unreachable} object(s) not used by any page (old revisions, deleted or hidden data)"),
        ));
    }
    let bytes = serialize(&c.objs, catalog_num, info_num).ok_or_else(|| {
        PreviewError::Malformed("an object of the rebuilt PDF could not be written".into())
    })?;
    Ok((bytes, actions))
}

/// Records what the catalog and the trailer held that the copy does not.
fn note_catalog(
    pdf: &mut PdfFile<'_>,
    catalog: &Dict,
    trailer: &Dict,
    opts: CleanOptions,
    actions: &mut Vec<SanitizeAction>,
) {
    let removed = |key: &str, text: &str, actions: &mut Vec<SanitizeAction>| {
        if catalog.contains_key(key) {
            actions.push(action(ActionKind::RemovedElement, text));
        }
    };
    removed("OpenAction", "automatic action on opening (/OpenAction)", actions);
    removed("AA", "document-level additional actions (/AA)", actions);
    removed("AcroForm", "interactive form (AcroForm)", actions);
    removed("Outlines", "bookmarks (outline)", actions);
    removed("Dests", "named destinations", actions);
    removed("Perms", "permissions and signatures (/Perms)", actions);
    removed("DSS", "document security store (/DSS)", actions);
    removed("Collection", "portfolio (/Collection)", actions);
    removed("StructTreeRoot", "tagged-structure tree", actions);
    removed("Threads", "article threads", actions);
    removed("URI", "base URI", actions);
    if catalog.contains_key("Metadata") {
        actions.push(action(ActionKind::StrippedMetadata, "XMP metadata"));
    }
    if opts.strip_metadata && trailer.contains_key("Info") {
        actions.push(action(
            ActionKind::StrippedMetadata,
            "document information (author, producer, dates)",
        ));
    }
    let names = pdf.get(catalog, "Names");
    if let Some(n) = names.as_dict() {
        if n.contains_key("JavaScript") {
            actions.push(action(
                ActionKind::RemovedElement,
                "document-level JavaScript (/Names /JavaScript)",
            ));
        }
        if n.contains_key("EmbeddedFiles") {
            actions
                .push(action(ActionKind::RemovedElement, "embedded files (/Names /EmbeddedFiles)"));
        }
        if n.keys().any(|k| !matches!(&**k, "JavaScript" | "EmbeddedFiles")) {
            actions.push(action(
                ActionKind::RemovedElement,
                "name trees (destinations, pages, templates)",
            ));
        }
    }
}

fn note_annotations(stats: &AnnotStats, actions: &mut Vec<SanitizeAction>) {
    for (target, n) in &stats.targets {
        actions.push(action(ActionKind::RemovedExternalReference, format!("{target} (x{n})")));
    }
    if stats.links > 0 {
        actions.push(action(
            ActionKind::RemovedElement,
            format!("{} link annotation(s)", stats.links),
        ));
    }
    if stats.flattened > 0 {
        actions.push(action(
            ActionKind::RemovedElement,
            format!(
                "{} annotation(s) and form field(s) turned into plain page content",
                stats.flattened
            ),
        ));
    }
    if stats.comments > 0 {
        actions.push(action(
            ActionKind::RemovedComments,
            format!("{} comment annotation(s)", stats.comments),
        ));
    }
    if stats.hidden > 0 {
        actions.push(action(
            ActionKind::RemovedElement,
            format!("{} hidden annotation(s)", stats.hidden),
        ));
    }
    if stats.no_appearance > 0 {
        actions.push(action(
            ActionKind::RemovedElement,
            format!("{} annotation(s) without an appearance", stats.no_appearance),
        ));
    }
}

/// Writes the header, the objects, the cross-reference table and the trailer.
fn serialize(objs: &[Option<Vec<u8>>], root: u32, info: Option<u32>) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body.as_ref()?);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    let info_text = info.map(|n| format!(" /Info {n} 0 R")).unwrap_or_default();
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {root} 0 R{info_text} >>\nstartxref\n{xref_at}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::super::testpdf::PdfBuilder;
    use super::*;
    use crate::testkit::{Lcg, fuzz_iterations};
    use crate::{PreviewPolicy, preview};

    fn opts() -> CleanOptions {
        CleanOptions { strip_metadata: true, remove_comments: true }
    }

    fn text_of(pdf: &[u8]) -> String {
        let p = preview(pdf, &PreviewPolicy::default()).expect("a preview");
        crate::testkit::paragraphs(p.sections.first().expect("page"))
            .iter()
            .map(|p| p.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A page with a link, a form field that has an appearance, a hidden annotation and a comment,
    /// plus every kind of document-level trouble.
    fn hostile() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.add_raw(
            1,
            "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert\\('open'\\)) >> \
             /AA << /WC << /S /JavaScript /JS (x) >> >> /Names << /JavaScript << /Names [(a) 11 0 R] >> /EmbeddedFiles << /Names [(f) 12 0 R] >> >> \
             /AcroForm << /Fields [8 0 R] /XFA 13 0 R >> /Outlines 14 0 R /Metadata 15 0 R /Lang (en) \
             /OCProperties << /OCGs [16 0 R] /D << /OFF [16 0 R] >> >> >>",
        );
        b.add_raw(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R 7 0 R 8 0 R 9 0 R 10 0 R] \
             /AA << /O << /S /JavaScript /JS (page) >> >> /Metadata 15 0 R /Thumb 17 0 R /StructParents 0 >>",
        );
        b.add_stream(4, "", b"BT /F1 12 Tf 72 700 Td (Hello page) Tj ET");
        b.add_raw(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        // A link to the web and a launch action.
        b.add_raw(6, "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /A << /S /URI /URI (https://phish.example/x) >> >>");
        b.add_raw(
            7,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /A << /S /Launch /F (cmd.exe) >> >>",
        );
        // A text field with a value and a java-script format action.
        b.add_raw(
            8,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (typed) /Rect [100 600 200 620] \
             /AP << /N 18 0 R >> /AA << /F << /S /JavaScript /JS (format) >> >> >>",
        );
        // Hidden annotation (flag 2): never drawn.
        b.add_raw(
            9,
            "<< /Type /Annot /Subtype /Stamp /F 2 /Rect [0 0 10 10] /AP << /N 18 0 R >> >>",
        );
        // A comment: left out when comments are removed.
        b.add_raw(10, "<< /Type /Annot /Subtype /Highlight /Contents (secret remark) /Rect [10 10 60 30] /AP << /N 19 0 R >> >>");
        b.add_raw(11, "<< /S /JavaScript /JS (doc js) >>");
        b.add_raw(12, "<< /Type /Filespec /F (evil.exe) /EF << /F 20 0 R >> >>");
        b.add_stream(20, "/Type /EmbeddedFile", b"MZ-attachment");
        b.add_raw(13, "<< /Length 0 >>");
        b.add_raw(14, "<< /Type /Outlines /First 21 0 R >>");
        b.add_raw(21, "<< /Title (bookmark) /A << /S /GoToR /F (other.pdf) >> >>");
        b.add_stream(15, "/Type /Metadata /Subtype /XML", b"<x:xmpmeta>XMPSECRET</x:xmpmeta>");
        b.add_raw(16, "<< /Type /OCG /Name (Layer) >>");
        b.add_stream(17, "/Type /XObject /Subtype /Image /Width 1 /Height 1", b"\x00");
        // Appearance of the field: the typed text, drawn in a 100 x 20 box.
        b.add_stream(
            18,
            "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >>",
            b"BT /F1 10 Tf 2 6 Td (FIELDVALUE) Tj ET",
        );
        b.add_stream(
            19,
            "/Type /XObject /Subtype /Form /BBox [0 0 50 20] /Resources << /Font << /F1 5 0 R >> >>",
            b"BT /F1 10 Tf 2 6 Td (COMMENTTEXT) Tj ET",
        );
        b.add_raw(22, "<< /Author (Eve) /Producer (HiddenProducer) >>");
        // Never reachable from a page: old data left in the file.
        b.add_raw(30, "<< /Hidden (UNREACHABLE_SECRET) >>");
        b.with_trailer_entry("/Info 22 0 R").build()
    }

    /// Whether `needle` is in `hay` as plain text or as the hexadecimal string the copy writes.
    fn has(hay: &[u8], needle: &str) -> bool {
        let hex: String = needle.bytes().map(|b| format!("{b:02X}")).collect();
        [needle, hex.as_str()].iter().any(|n| hay.windows(n.len()).any(|w| w == n.as_bytes()))
    }

    #[test]
    fn a_hostile_pdf_is_rebuilt_from_what_the_pages_need() {
        let input = hostile();
        let (out, actions) = clean(&input, &PreviewPolicy::default(), opts()).expect("a copy");
        for gone in [
            "JavaScript",
            "/JS",
            "OpenAction",
            "/URI",
            "Launch",
            "/AA",
            "AcroForm",
            "/Names",
            "/Annots",
            "XFA",
            "Outlines",
            "Metadata",
            "Thumb",
            "Filespec",
            "EmbeddedFile",
            "StructParents",
            "Eve",
            "HiddenProducer",
            "UNREACHABLE_SECRET",
            "XMPSECRET",
            "phish",
            "cmd.exe",
            "evil.exe",
            "COMMENTTEXT",
            "GoToR",
            "/Info",
        ] {
            assert!(!has(&out, gone), "{gone} survived:\n{}", String::from_utf8_lossy(&out));
        }
        // What the pages need: the text, the field's look (drawn into the page) and the layer settings.
        let text = text_of(&out);
        assert!(text.contains("Hello page"), "{text}");
        assert!(text.contains("FIELDVALUE"), "the typed text is page content now: {text}");
        assert!(!text.contains("COMMENTTEXT"), "comments are left out when asked");
        assert!(has(&out, "OCProperties"), "hidden layers stay hidden");
        assert!(has(&out, "cm /ArcaAnnot0 Do Q"));
        assert!(out.starts_with(b"%PDF-1.7"));

        let kinds: Vec<ActionKind> = actions.iter().map(|a| a.kind).collect();
        for k in [
            ActionKind::RemovedElement,
            ActionKind::RemovedExternalReference,
            ActionKind::RemovedComments,
            ActionKind::StrippedMetadata,
        ] {
            assert!(kinds.contains(&k), "missing {k:?}: {actions:#?}");
        }
        let all: String = actions.iter().map(|a| a.target.as_str()).collect::<Vec<_>>().join("\n");
        for what in [
            "/OpenAction",
            "AcroForm",
            "bookmarks",
            "JavaScript",
            "embedded files",
            "URI https://phish.example/x",
            "Launch",
            "not used by any page",
        ] {
            assert!(all.contains(what), "no action mentions {what}:\n{all}");
        }

        // Deterministic, and a sterile file stays as it is.
        let (again, _) = clean(&input, &PreviewPolicy::default(), opts()).expect("a copy");
        assert_eq!(again, out);
        let (twice, _) =
            clean(&out, &PreviewPolicy::default(), opts()).expect("a copy of the copy");
        assert_eq!(twice, out, "cleaning a clean file changes nothing");
    }

    #[test]
    fn options_keep_the_comments_and_the_information_when_asked() {
        let keep = CleanOptions { strip_metadata: false, remove_comments: false };
        let (out, _) = clean(&hostile(), &PreviewPolicy::default(), keep).expect("a copy");
        let text = text_of(&out);
        assert!(text.contains("COMMENTTEXT"), "{text}");
        assert!(has(&out, "Eve"), "the information dictionary is kept on request");
        // Active content goes whatever is asked.
        for gone in ["JavaScript", "OpenAction", "Launch", "/URI", "AcroForm"] {
            assert!(!has(&out, gone), "{gone} survived");
        }
    }

    #[test]
    fn shared_objects_are_copied_once_and_the_page_boxes_are_inherited() {
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(
            2,
            "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 /MediaBox [0 0 300 400] /Rotate 90 /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.add_raw(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>");
        b.add_raw(6, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 100 100] >>");
        b.add_stream(4, "", b"BT /F1 12 Tf 10 10 Td (Shared) Tj ET");
        b.add_raw(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let (out, _) = clean(&b.build(), &PreviewPolicy::default(), opts()).expect("a copy");
        let p = preview(&out, &PreviewPolicy::default()).expect("preview");
        assert_eq!(p.sections.len(), 2);
        // catalog, pages, 2 pages, the content stream and the font: each once.
        let objects = out.windows(5).filter(|w| w == b" obj\n").count();
        assert_eq!(objects, 6, "{}", String::from_utf8_lossy(&out));
        assert!(
            has(&out, "/MediaBox [0 0 300 400]")
                && has(&out, "/MediaBox [0 0 100 100]")
                && has(&out, "/Rotate 90")
        );
    }

    #[test]
    fn the_appearance_matrix_puts_the_box_onto_the_rectangle() {
        // Same size: a move.
        assert_eq!(
            appearance_matrix(
                [0.0, 0.0, 100.0, 20.0],
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                [200.0, 300.0, 300.0, 320.0]
            ),
            [1.0, 0.0, 0.0, 1.0, 200.0, 300.0]
        );
        // Smaller box: scaled up.
        assert_eq!(
            appearance_matrix(
                [0.0, 0.0, 50.0, 10.0],
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                [100.0, 100.0, 200.0, 120.0]
            ),
            [2.0, 0.0, 0.0, 2.0, 100.0, 100.0]
        );
        // The form's own matrix is taken into account (a rotation by 90 degrees).
        let m = appearance_matrix(
            [0.0, 0.0, 10.0, 20.0],
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 20.0, 10.0],
        );
        assert!(
            (m[0] - 1.0).abs() < 1e-9 && (m[3] - 1.0).abs() < 1e-9 && (m[4] - 20.0).abs() < 1e-9,
            "{m:?}"
        );
        // A degenerate box and a rectangle given backwards do not divide by zero.
        let m = appearance_matrix(
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            [50.0, 50.0, 10.0, 10.0],
        );
        assert!(m.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn encrypted_empty_and_broken_files_are_refused_cleanly() {
        let enc = PdfBuilder::simple_text("x").with_trailer_entry("/Encrypt 9 0 R").build();
        assert!(matches!(
            clean(&enc, &PreviewPolicy::default(), opts()),
            Err(PreviewError::Encrypted)
        ));
        assert!(clean(b"", &PreviewPolicy::default(), opts()).is_err());
        assert!(clean(b"%PDF-1.7\n", &PreviewPolicy::default(), opts()).is_err());
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        assert!(clean(&b.build(), &PreviewPolicy::default(), opts()).is_err(), "no pages");
    }

    #[test]
    fn loops_and_deep_nesting_are_bounded() {
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(2, "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>");
        // A page whose resources point at themselves and nest deeply.
        b.add_raw(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources 7 0 R >>");
        b.add_stream(4, "", b"q Q");
        b.add_raw(7, "<< /Self 7 0 R /Deep [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]] >>");
        let (out, _) = clean(&b.build(), &PreviewPolicy::default(), opts()).expect("a copy");
        assert!(preview(&out, &PreviewPolicy::default()).is_ok());
    }

    #[test]
    fn mutated_files_never_panic() {
        let base = hostile();
        let mut rng = Lcg::new(23);
        for _ in 0..fuzz_iterations(400) {
            let mut m = base.clone();
            rng.mutate(&mut m, 6);
            let _ = clean(&m, &PreviewPolicy::default(), opts());
        }
        for cut in (0..base.len()).step_by(37) {
            let _ = clean(base.get(..cut).unwrap_or_default(), &PreviewPolicy::default(), opts());
        }
    }
}
