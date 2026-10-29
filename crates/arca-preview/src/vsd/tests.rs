//! Tests for binary Visio. The compressed vector is a real stream; the rest are drawings built
//! here from the structure the reader expects (uncompressed streams, so no compressor is needed).

use super::{chunks, decompress};
use crate::model::{Block, Preview, SectionKind};
use crate::{PreviewError, PreviewPolicy, preview};
use arca_container::cfb::writer::CfbBuilder as CfbWriter;
use arca_container::crypto::Hash;

/// A real compressed chunk stream (163 bytes) of a Visio 2003 drawing.
const COMPRESSED: &str = "5531ebf0fff2f003ebf028e8f3151cebf00cdcff00f6f1f6f1e6f55501ebf002e7f44f220844260261554101e8f3f2f1550fffff4e05906a09e0fb2901e6f568220801010217005418ebf0047d0f8b0aeaf155bce3f82b1602552d0209af0dae7c0e1f0608e9f2c989081e422602512d01bf0a19052d02482a04003d0f2800580f6913f6f1771db10f900fa8a20fb40fe1fa9be3f84516025110fa0f082f1a2f2c2f00";

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn a_real_stream_decompresses_and_splits_into_chunks() {
    let out = decompress(&unhex(COMPRESSED), 1 << 20).unwrap();
    assert_eq!(out.len(), 594);
    let sum: String = Hash::Sha256.digest(&[&out]).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(sum, "9de58b81f7cf9ba5c3363427a30a4f5001c2f8a9a060e7d1cc5fbe31037ef409");
    let parts = chunks(&out, 11, 100);
    let types: Vec<u32> = parts.iter().map(|c| c.ty).collect();
    assert_eq!(types, [0x31, 0x4f, 0x68, 0xbc, 0xc9, 0x48, 0x68, 0x9b]);
    // The chunks account for every byte.
    assert_eq!(parts.last().map(|c| c.id), Some(0));
}

#[test]
fn decompression_is_bounded_and_survives_truncation() {
    let data = unhex(COMPRESSED);
    assert!(decompress(&data, 100).is_none(), "the output limit holds");
    for len in 0..data.len() {
        let _ = decompress(&data[..len], 1 << 20);
    }
    // A reference into an empty window yields zero bytes, not a panic.
    assert_eq!(decompress(&[0x00, 0x00, 0x00], 100).unwrap(), vec![0u8; 3]);
}

// ---------------------------------------------------------------------------------------------
// Building drawings
// ---------------------------------------------------------------------------------------------

fn pointer(ty: i32, offset: usize, len: usize, format: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&ty.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
    p.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
    p.extend_from_slice(&format.to_le_bytes());
    p
}

/// A container stream holding `children`.
fn container(children: &[Vec<u8>]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&4u32.to_le_bytes());
    c.extend_from_slice(&u32::try_from(children.len()).unwrap().to_le_bytes());
    c.extend_from_slice(&[0; 4]);
    for p in children {
        c.extend_from_slice(p);
    }
    c
}

/// One chunk. `trailer` and `separator` follow the rules of the version.
fn chunk(version: u16, ty: u32, id: u32, body: &[u8]) -> Vec<u8> {
    let trailer = matches!(ty, 0x2c | 0x65 | 0x66 | 0x69 | 0x6a | 0x6b | 0x70 | 0x71);
    let separator = version > 6 && (ty == 0x69 || trailer);
    let mut c = Vec::new();
    c.extend_from_slice(&ty.to_le_bytes());
    c.extend_from_slice(&id.to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes());
    c.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    c.extend_from_slice(&[0, 0, 0]);
    c.extend_from_slice(body);
    if trailer {
        c.extend_from_slice(&[0; 8]);
    }
    if separator {
        c.extend_from_slice(&[0; 4]);
    }
    c
}

fn text_chunk(version: u16, id: u32, s: &str) -> Vec<u8> {
    let mut body = vec![0u8; 8];
    if version > 6 {
        body.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
    } else {
        body.extend(s.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')));
    }
    chunk(version, 0x0e, id, &body)
}

fn xform(version: u16, x: f64, y: f64) -> Vec<u8> {
    let mut body = vec![0x40];
    body.extend_from_slice(&x.to_le_bytes());
    body.push(0x40);
    body.extend_from_slice(&y.to_le_bytes());
    body.extend_from_slice(&[0; 40]);
    chunk(version, 0x9b, 0, &body)
}

fn shape_list(version: u16, id: u32, ids: &[u32]) -> Vec<u8> {
    let mut c = chunk(version, 0x65, id, &[0u8; 24]);
    for i in ids {
        let mut body = i.to_le_bytes().to_vec();
        body.extend_from_slice(&[0x48, 0, 0, 0, 0, 0]);
        c.extend(chunk(version, 0x83, 0, &body));
    }
    c
}

fn name_entry(version: u16, s: &str) -> Vec<u8> {
    let mut text: Vec<u8> = if version > 6 {
        s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    } else {
        s.bytes().chain([0]).collect()
    };
    let mut e = Vec::new();
    // [size][level][text] for the long form.
    e.extend_from_slice(&u32::try_from(text.len() + 4).unwrap().to_le_bytes());
    e.extend_from_slice(&1u32.to_le_bytes());
    e.append(&mut text);
    e
}

/// A stream that hangs off the trailer: pointer type, content, format.
struct Extra {
    ty: i32,
    data: Vec<u8>,
    format: u16,
}

/// A page of the drawing: its chunk stream and whether it is a foreground page.
struct TestPage {
    data: Vec<u8>,
    foreground: bool,
}

/// A drawing with the given pages (each a chunk stream), names, extra streams of the trailer and
/// stencils (each a chunk stream of one stencil page).
fn drawing_ex(
    version: u16,
    pages: &[TestPage],
    names: &[&str],
    extras: &[Extra],
    stencils: &[Vec<u8>],
) -> Vec<u8> {
    let mut doc = vec![0u8; 0x24 + 18];
    doc[..20].copy_from_slice(b"Visio (TM) Drawing\r\n");
    doc[0x1a..0x1c].copy_from_slice(&version.to_le_bytes());
    let put = |doc: &mut Vec<u8>, data: &[u8]| {
        let at = doc.len();
        doc.extend_from_slice(data);
        (at, data.len())
    };
    // Names.
    let mut name_ptrs = Vec::new();
    for n in names {
        let (at, len) = put(&mut doc, &name_entry(version, n));
        name_ptrs.push(pointer(0x33, at, len, 0x45));
    }
    let (at, len) = put(&mut doc, &container(&name_ptrs));
    let names_ptr = pointer(0x32, at, len, 0x54);
    // Pages (0xd4 and 0xd5: chunk streams; bit 0 marks a foreground page).
    let mut page_ptrs = Vec::new();
    for p in pages {
        let (at, len) = put(&mut doc, &p.data);
        let format = if p.foreground { 0xd5 } else { 0xd4 };
        page_ptrs.push(pointer(0x15, at, len, format));
    }
    let (at, len) = put(&mut doc, &container(&page_ptrs));
    let pages_ptr = pointer(0x27, at, len, 0x54);
    let mut top = vec![names_ptr, pages_ptr];
    for e in extras {
        let (at, len) = put(&mut doc, &e.data);
        top.push(pointer(e.ty, at, len, e.format));
    }
    if !stencils.is_empty() {
        let mut ptrs = Vec::new();
        for s in stencils {
            let (at, len) = put(&mut doc, s);
            ptrs.push(pointer(0x1e, at, len, 0xd4));
        }
        let (at, len) = put(&mut doc, &container(&ptrs));
        top.push(pointer(0x1d, at, len, 0x54));
    }
    // The trailer.
    let (at, len) = put(&mut doc, &container(&top));
    let trailer = pointer(20, at, len, 0x50);
    doc[0x24..0x24 + 18].copy_from_slice(&trailer);
    let size = u32::try_from(doc.len()).unwrap();
    doc[0x1c..0x20].copy_from_slice(&size.to_le_bytes());

    let mut w = CfbWriter::new();
    w.add_stream("VisioDocument", doc).unwrap();
    w.finish().unwrap()
}

/// A drawing with the given pages (each a chunk stream) and names.
fn drawing(version: u16, pages: &[Vec<u8>], names: &[&str]) -> Vec<u8> {
    let pages: Vec<TestPage> =
        pages.iter().map(|p| TestPage { data: p.clone(), foreground: false }).collect();
    drawing_ex(version, &pages, names, &[], &[])
}

fn run(data: &[u8]) -> Result<Preview, PreviewError> {
    preview(data, &PreviewPolicy::default())
}

fn paragraphs(p: &Preview, section: usize) -> Vec<(u8, String)> {
    p.sections[section]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para(para) => {
                Some((para.level, para.runs.iter().map(|r| r.text.as_str()).collect::<String>()))
            }
            _ => None,
        })
        .collect()
}

/// A page: a group "Header" holding two shapes, and a loose shape lower down.
fn page(version: u16) -> Vec<u8> {
    let mut p = chunk(version, 0x15, u32::MAX, &[0u8; 80]);
    p.extend(shape_list(version, 4, &[1, 4]));
    // The group, at the top of the page.
    p.extend(chunk(version, 0x47, 1, &[0u8; 20]));
    p.extend(xform(version, 5.0, 9.0));
    p.extend(shape_list(version, 7, &[2, 3]));
    p.extend(chunk(version, 0x48, 2, &[0u8; 20]));
    p.extend(xform(version, 6.0, 1.0));
    p.extend(text_chunk(version, 5, "second child\n"));
    p.extend(chunk(version, 0x48, 3, &[0u8; 20]));
    p.extend(xform(version, 1.0, 1.0));
    p.extend(text_chunk(version, 6, "first child\n"));
    // A loose shape below the group (listed second, but also lower on the page).
    p.extend(chunk(version, 0x48, 4, &[0u8; 20]));
    p.extend(xform(version, 3.0, 2.0));
    p.extend(text_chunk(version, 7, "Привет, мир\nline two\n"));
    p
}

#[test]
fn text_is_shown_per_page_in_reading_order_with_groups_kept_together() {
    for version in [11u16, 6] {
        let names = ["Guide", "TheDoc", "Overview"];
        let data = drawing(version, &[page(version), page(version)], &names);
        let p = run(&data).unwrap();
        assert_eq!(p.format, crate::model::FileFormat::Vsd);
        let titles: Vec<_> = p.sections.iter().map(|s| (s.kind, s.name.as_str())).collect();
        // The second page has no name in the table and gets a default one.
        assert_eq!(titles, [(SectionKind::Page, "Overview"), (SectionKind::Page, "Page-2")]);
        // v6 text is one byte per character: the Cyrillic line cannot be written there.
        let want: Vec<(u8, &str)> = if version > 6 {
            vec![(1, "first child"), (1, "second child"), (0, "Привет, мир"), (0, "line two")]
        } else {
            vec![(1, "first child"), (1, "second child"), (0, "??????, ???"), (0, "line two")]
        };
        let got = paragraphs(&p, 0);
        assert_eq!(
            got.iter().map(|(l, t)| (*l, t.as_str())).collect::<Vec<_>>(),
            want,
            "v{version}"
        );
        // The group itself has no text; its children follow it. The loose shape sits lower on
        // the page than the group, so it comes after the group's children.
        assert_eq!(paragraphs(&p, 1).len(), 4);
    }
}

#[test]
fn a_page_without_text_is_dropped_and_a_drawing_without_pages_is_an_error() {
    let empty = chunk(11, 0x15, u32::MAX, &[0u8; 80]);
    let p = run(&drawing(11, &[empty, page(11)], &[])).unwrap();
    assert_eq!(p.sections.len(), 1);
    assert_eq!(p.sections[0].name, "Page-2");
    assert!(matches!(run(&drawing(11, &[], &[])), Err(PreviewError::Malformed(_))));
}

#[test]
fn a_page_that_does_not_list_its_shapes_is_shown_in_file_order() {
    let mut p = chunk(11, 0x15, u32::MAX, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 1, &[0u8; 20]));
    p.extend(text_chunk(11, 0, "one"));
    p.extend(chunk(11, 0x48, 2, &[0u8; 20]));
    p.extend(text_chunk(11, 0, "two"));
    let out = run(&drawing(11, &[p], &[])).unwrap();
    let got: Vec<_> = paragraphs(&out, 0).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, ["one", "two"]);
}

#[test]
fn old_and_foreign_files_are_refused_cleanly() {
    let mut doc = drawing(11, &[page(11)], &[]);
    // Not a Visio stream at all.
    let mut w = CfbWriter::new();
    w.add_stream("VisioDocument", b"hello".to_vec()).unwrap();
    assert!(matches!(run(&w.finish().unwrap()), Err(PreviewError::Malformed(_))));
    // Visio 5.
    let mut w = CfbWriter::new();
    let mut d = vec![0u8; 100];
    d[..20].copy_from_slice(b"Visio (TM) Drawing\r\n");
    d[0x1a] = 5;
    w.add_stream("VisioDocument", d).unwrap();
    assert!(matches!(run(&w.finish().unwrap()), Err(PreviewError::Unsupported(_))));
    doc.truncate(doc.len() / 2);
    let _ = run(&doc);
}

#[test]
fn hostile_structures_stay_bounded() {
    // A container that points to itself, a count far beyond the data, and a chunk that claims
    // more bytes than there are.
    let mut doc = vec![0u8; 0x24 + 18];
    doc[..20].copy_from_slice(b"Visio (TM) Drawing\r\n");
    doc[0x1a] = 11;
    let self_ptr = pointer(0x27, 0x24 + 18, 100, 0x54);
    let mut body = container(&[self_ptr.clone(), self_ptr.clone()]);
    body[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let at = doc.len();
    doc.extend_from_slice(&body);
    let trailer = pointer(20, at, body.len(), 0x50);
    doc[0x24..0x24 + 18].copy_from_slice(&trailer);
    let mut w = CfbWriter::new();
    w.add_stream("VisioDocument", doc).unwrap();
    assert!(run(&w.finish().unwrap()).is_err());

    let mut p = chunk(11, 0x15, u32::MAX, &[0u8; 80]);
    p.extend_from_slice(&0x48u32.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p.extend_from_slice(&u32::MAX.to_le_bytes());
    p.extend_from_slice(&[0, 0, 0]);
    p.extend(text_chunk(11, 0, "cut off"));
    let _ = run(&drawing(11, &[p], &[]));
}

#[test]
fn damaged_drawings_never_panic() {
    let base = drawing(11, &[page(11), page(11)], &["a", "b", "c"]);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let mut m = base.clone();
        for _ in 0..1 + next() % 20 {
            let at = usize::try_from(next()).unwrap() % m.len();
            m[at] = u8::try_from(next() % 256).unwrap();
        }
        let _ = run(&m);
    }
}

/// Real Visio files (from the Apache POI test set; not kept in this repository) and the text
/// LibreOffice reads from them with libvisio. Set `ARCA_VSD_DIR` to a directory that holds them.
#[test]
fn real_files_read_like_libvisio() {
    let Ok(dir) = std::env::var("ARCA_VSD_DIR") else { return };
    let expected: [(&str, &[&str]); 4] = [
        ("testVISIO.vsd", &["Test View", "I am a test view", "Some random text, on a page"]),
        (
            "Test_Visio-Some_Random_Text.vsd",
            &["Test View", "I am a test view", "Some random text, on a page"],
        ),
        ("v6-non-utf16le.vsd", &["PropertySheet", "PropertySheetField"]),
        ("44594.vsd", &[]),
    ];
    for (name, want) in expected {
        let path = std::path::Path::new(&dir).join(name);
        let Ok(data) = std::fs::read(&path) else { continue };
        let p = run(&data).unwrap();
        let got: Vec<String> =
            (0..p.sections.len()).flat_map(|i| paragraphs(&p, i)).map(|(_, t)| t).collect();
        assert_eq!(got, want, "{name}");
        if !want.is_empty() {
            assert_eq!(p.sections[0].name, "Page-1", "{name}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Chunk layouts (see the table in `read.rs`): a test-only writer that follows them, and the
// drawing and reading tests built on it.
// ---------------------------------------------------------------------------------------------

const NONE: u32 = u32::MAX;

/// A number: a tag byte and a little-endian `f64`.
fn num(v: f64) -> Vec<u8> {
    let mut b = vec![0x40];
    b.extend_from_slice(&v.to_le_bytes());
    b
}

fn nums(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| num(*x)).collect()
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// The data of a shape chunk: parent at 10, master page at 18, master shape at 26, fill style at
/// 34, line style at 42, text style at 50.
fn shape_body(parent: u32, master: u32, master_shape: u32, styles: (u32, u32, u32)) -> Vec<u8> {
    let mut b = vec![0u8; 54];
    put_u32(&mut b, 10, parent);
    put_u32(&mut b, 18, master);
    put_u32(&mut b, 26, master_shape);
    put_u32(&mut b, 34, styles.0);
    put_u32(&mut b, 42, styles.1);
    put_u32(&mut b, 50, styles.2);
    b
}

/// XForm: PinX, PinY, Width, Height, LocPinX, LocPinY, Angle, then the flip bytes at 63, 64.
fn xform_body(pin: (f64, f64), size: (f64, f64), loc: (f64, f64), angle: f64, flips: (u8, u8)) -> Vec<u8> {
    let mut b = nums(&[pin.0, pin.1, size.0, size.1, loc.0, loc.1, angle]);
    b.extend([flips.0, flips.1]);
    b
}

/// Line: weight, RGBA at 10, pattern at 14, rounding at 15 (as a number), arrows at 25 and 26.
fn line_body(weight: f64, rgb: u32, pattern: u8, arrows: (u8, u8)) -> Vec<u8> {
    let mut b = vec![0u8; 28];
    b[..9].copy_from_slice(&num(weight));
    b[9] = 0x40;
    b[10] = u8::try_from(rgb >> 16 & 0xff).unwrap();
    b[11] = u8::try_from(rgb >> 8 & 0xff).unwrap();
    b[12] = u8::try_from(rgb & 0xff).unwrap();
    b[14] = pattern;
    b[15] = 0x40;
    b[25] = arrows.0;
    b[26] = arrows.1;
    b
}

/// Fill: index and RGBA of the foreground at 0, of the background at 5, pattern at 10.
fn fill_body(fg: Option<u32>, fg_index: u8, bg: Option<u32>, bg_index: u8, pattern: u8) -> Vec<u8> {
    let mut b = vec![0u8; 22];
    let rgba = |b: &mut [u8], at: usize, c: Option<u32>| {
        if let Some(c) = c {
            b[at] = u8::try_from(c >> 16 & 0xff).unwrap();
            b[at + 1] = u8::try_from(c >> 8 & 0xff).unwrap();
            b[at + 2] = u8::try_from(c & 0xff).unwrap();
        }
    };
    b[0] = fg_index;
    rgba(&mut b, 1, fg);
    b[5] = bg_index;
    rgba(&mut b, 6, bg);
    b[10] = pattern;
    b
}

fn page_props(width: f64, height: f64) -> Vec<u8> {
    let mut b = nums(&[width, height, 0.0, 0.0, 1.0]);
    b.push(0);
    b.extend_from_slice(&1.0f64.to_le_bytes());
    b
}

fn rows(version: u16, geometry: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = chunk(version, 0x6c, 0, &[]);
    out.extend(chunk(version, 0x89, 0, &[0]));
    for (i, (ty, body)) in geometry.iter().enumerate() {
        out.extend(chunk(version, *ty, u32::try_from(i).unwrap(), body));
    }
    out
}

fn move_to(x: f64, y: f64) -> (u32, Vec<u8>) {
    (0x8a, nums(&[x, y]))
}

fn line_to(x: f64, y: f64) -> (u32, Vec<u8>) {
    (0x8b, nums(&[x, y]))
}

/// A page with a rectangle of 2 x 1 inches whose local pin (1, 0.5) is at (3, 4), as in the
/// `.vsdx` tests.
fn rect_page(version: u16, style: (u32, u32, u32), extra: &[u8]) -> Vec<u8> {
    let mut p = chunk(version, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(version, 0x92, 0, &page_props(8.5, 11.0)));
    p.extend(chunk(version, 0x48, 1, &shape_body(0, NONE, NONE, style)));
    p.extend(chunk(version, 0x9b, 0, &xform_body((3.0, 4.0), (2.0, 1.0), (1.0, 0.5), 0.0, (0, 0))));
    p.extend_from_slice(extra);
    p.extend(rows(
        version,
        &[move_to(0.0, 0.0), line_to(2.0, 0.0), line_to(2.0, 1.0), line_to(0.0, 1.0), line_to(0.0, 0.0)],
    ));
    p
}

fn fg(data: Vec<u8>) -> TestPage {
    TestPage { data, foreground: true }
}

fn render(data: &[u8]) -> crate::page::PageModel {
    crate::visio_draw::render_visio_page(data, 0, &PreviewPolicy::default()).unwrap()
}

fn path_points(p: &crate::page::PageModel) -> Vec<Vec<(f32, f32)>> {
    p.prims
        .iter()
        .filter_map(|q| match q {
            crate::page::Prim::Path { ops, .. } => Some(crate::visio_draw::tests::points(ops)),
            _ => None,
        })
        .collect()
}

fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.02 && (a.1 - b.1).abs() < 0.02
}

#[test]
fn a_binary_page_is_drawn_from_xform_line_fill_and_geometry() {
    for version in [11u16, 6] {
        let mut extra = chunk(version, 0x85, 0, &line_body(0.02, 0x0000_00ff, 1, (0, 0)));
        extra.extend(chunk(version, 0x86, 0, &fill_body(Some(0xff0000), 0, Some(0xffffff), 1, 1)));
        let data = drawing_ex(version, &[fg(rect_page(version, (NONE, NONE, NONE), &extra))], &[], &[], &[]);
        let policy = PreviewPolicy::default();
        assert_eq!(crate::visio_draw::visio_page_count(&data, &policy).unwrap(), 1);
        let p = render(&data);
        assert_eq!((p.width, p.height), (612.0, 792.0), "v{version}");
        let paths = path_points(&p);
        assert_eq!(paths.len(), 1);
        let want = [(2.0, 3.5), (4.0, 3.5), (4.0, 4.5), (2.0, 4.5), (2.0, 3.5)];
        for (got, w) in paths[0].iter().zip(want) {
            assert!(close(*got, (w.0 * 72.0, (11.0 - w.1) * 72.0)), "{got:?} vs {w:?}");
        }
        let crate::page::Prim::Path { fill, stroke, width, .. } = &p.prims[0] else { panic!() };
        assert_eq!((*fill, *stroke), (0xFFFF_0000, 0xFF00_00FF));
        assert!((width - 1.44).abs() < 1e-4);
        // The layouts of binary files are not verified: the page says so.
        assert!(p.approximate);
    }
}

#[test]
fn rotation_and_flips_of_a_binary_shape() {
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(
        11,
        0x9b,
        0,
        &xform_body((3.0, 4.0), (2.0, 1.0), (1.0, 0.5), std::f64::consts::FRAC_PI_2, (0, 0)),
    ));
    p.extend(chunk(11, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
    p.extend(rows(11, &[move_to(0.0, 0.0), line_to(2.0, 0.0)]));
    let pts = path_points(&render(&drawing_ex(11, &[fg(p)], &[], &[], &[])));
    // Local (2,0) is (1,-0.5) from the pin; a quarter turn counter-clockwise: (0.5, 1) + (3,4).
    assert!(close(pts[0][1], (3.5 * 72.0, (11.0 - 5.0) * 72.0)), "{:?}", pts[0]);
    // The same shape flipped in x: (-1,-0.5) + (3,4).
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((3.0, 4.0), (2.0, 1.0), (1.0, 0.5), 0.0, (1, 0))));
    p.extend(chunk(11, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
    p.extend(rows(11, &[move_to(0.0, 0.0), line_to(2.0, 0.0)]));
    let pts = path_points(&render(&drawing_ex(11, &[fg(p)], &[], &[], &[])));
    assert!(close(pts[0][1], (2.0 * 72.0, (11.0 - 3.5) * 72.0)), "{:?}", pts[0]);
}

#[test]
fn groups_are_built_from_the_parent_field_and_the_shape_lists() {
    // Child 2 names the group 1 as its parent; child 3 is listed by the group.
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x47, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((5.0, 5.0), (4.0, 4.0), (0.0, 0.0), 0.0, (0, 0))));
    p.extend(shape_list(11, 9, &[3]));
    for (id, parent) in [(2u32, 1u32), (3, 0)] {
        p.extend(chunk(11, 0x48, id, &shape_body(parent, NONE, NONE, (NONE, NONE, NONE))));
        p.extend(chunk(11, 0x9b, 0, &xform_body((1.0, 1.0), (1.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
        p.extend(chunk(11, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
        p.extend(rows(11, &[move_to(0.0, 0.0), line_to(1.0, 0.0)]));
    }
    let paths = path_points(&render(&drawing_ex(11, &[fg(p)], &[], &[], &[])));
    assert_eq!(paths.len(), 2);
    // Both children are placed in the group's coordinates: (1,1) + the group's pin (5,5).
    for pts in &paths {
        assert!(close(pts[0], (6.0 * 72.0, (11.0 - 6.0) * 72.0)), "{pts:?}");
    }
}

#[test]
fn masters_styles_and_colours_are_inherited() {
    // The colour table: index 3 is green here.
    let mut table = vec![0u8, 0, 4, 0];
    for c in [[0u8, 0, 0, 0], [255, 255, 255, 0], [9, 9, 9, 0], [0, 255, 0, 0]] {
        table.extend(c);
    }
    let colours = Extra { ty: 0x16, data: table, format: 0x44 };
    // Style 5 sets a fill by colour index and a line; style 6 takes its line from style 5.
    let mut styles = chunk(11, 0x4a, 5, &shape_body(0, 0, 0, (NONE, NONE, NONE))[..54]);
    styles.extend(chunk(11, 0x86, 0, &fill_body(None, 3, None, 1, 1)));
    styles.extend(chunk(11, 0x85, 0, &line_body(0.03, 0x00_00_80, 1, (0, 0))));
    let mut parent = shape_body(0, 0, 0, (NONE, NONE, NONE));
    put_u32(&mut parent, 34, 5);
    put_u32(&mut parent, 42, 5);
    styles.extend(chunk(11, 0x4a, 6, &parent));
    let styles = Extra { ty: 0x1a, data: styles, format: 0xd4 };
    // The stencil page (master 0) holds a shape with the geometry.
    let mut stencil = chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (6, 6, NONE)));
    stencil.extend(chunk(11, 0x9b, 0, &xform_body((0.0, 0.0), (2.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    stencil.extend(rows(
        11,
        &[move_to(0.0, 0.0), line_to(2.0, 0.0), line_to(2.0, 1.0), line_to(0.0, 0.0)],
    ));
    // The page shape is an instance of it, with nothing of its own but a pin.
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 7, &shape_body(0, 0, 1, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((3.0, 4.0), (2.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    let data = drawing_ex(11, &[fg(p)], &[], &[colours, styles], &[stencil]);
    let page = render(&data);
    let crate::page::Prim::Path { fill, stroke, width, .. } = &page.prims[0] else {
        panic!("{:?}", page.prims)
    };
    assert_eq!(*fill, 0xFF00_FF00, "colour index 3 through style 6 -> style 5");
    assert_eq!(*stroke, 0xFF00_0080);
    assert!((width - 2.16).abs() < 1e-4, "{width}");
    let pts = path_points(&page);
    assert!(close(pts[0][1], (5.0 * 72.0, (11.0 - 4.0) * 72.0)), "{:?}", pts[0]);
}

#[test]
fn arcs_ellipses_splines_and_nurbs_of_binary_files() {
    let ellipse = (0x8f, nums(&[2.0, 1.0, 3.0, 1.0, 2.0, 2.0]));
    let arc = (0x8c, nums(&[2.0, 1.0, 1.0]));
    let elliptical = (0x90, nums(&[2.0, 0.0, 1.0, 1.0, 0.0, 1.0]));
    let mk = |rows_: &[(u32, Vec<u8>)]| {
        let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
        p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
        p.extend(chunk(11, 0x9b, 0, &xform_body((0.0, 0.0), (4.0, 2.0), (0.0, 0.0), 0.0, (0, 0))));
        p.extend(chunk(11, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
        p.extend(rows(11, rows_));
        render(&drawing_ex(11, &[fg(p)], &[], &[], &[]))
    };
    let p = mk(&[ellipse]);
    let pts = &path_points(&p)[0];
    assert_eq!(pts.len(), 5);
    assert!(close(pts[0], (3.0 * 72.0, (11.0 - 1.0) * 72.0)) && close(pts[1], (2.0 * 72.0, (11.0 - 2.0) * 72.0)));
    let p = mk(&[move_to(0.0, 1.0), arc]);
    assert!(close(path_points(&p)[0][1], (72.0, 11.0 * 72.0)), "{:?}", path_points(&p));
    let p = mk(&[move_to(0.0, 0.0), elliptical]);
    assert!(close(*path_points(&p)[0].last().unwrap(), (2.0 * 72.0, 11.0 * 72.0)));

    // A spline: SplineStart (x, y, then second, first and last knot and the degree) and knots.
    let mut start = nums(&[1.0, 2.0]);
    for k in [0.0f64, 0.0, 1.0] {
        start.extend(k.to_le_bytes());
    }
    start.push(2);
    let mut knot = nums(&[3.0, 2.0]);
    knot.extend(0.5f64.to_le_bytes());
    let mut knot2 = nums(&[4.0, 0.0]);
    knot2.extend(1.0f64.to_le_bytes());
    let p = mk(&[move_to(0.0, 0.0), (0xa5, start), (0xa6, knot), (0xa6, knot2)]);
    assert!(path_points(&p)[0].len() > 8);
    assert!(p.approximate);

    // NURBSTo with its formula in a block: the same curve as in the `.vsdx` test.
    let nurbs = |use_data: Option<u32>| {
        let mut b = nums(&[4.0, 0.0]);
        for v in [0.0f64, 1.0, 0.0, 1.0] {
            b.extend(v.to_le_bytes());
        }
        b.push(0x40); // 50
        match use_data {
            Some(id) => {
                b.push(0x8a);
                b.extend([0, 0, 0]);
                b.extend(id.to_le_bytes());
            }
            None => {
                b.push(0);
                b.extend([0u8; 9]);
                // The block of cell 6: [length][1 byte][cell][0x8a][last knot][degree][x, y type]
                // [count] and four numbers per point.
                let mut blk = vec![0u8, 6, 0x8a];
                blk.extend(1.0f64.to_le_bytes());
                blk.extend(3u16.to_le_bytes());
                blk.extend([1, 1]);
                blk.extend(2u32.to_le_bytes());
                for pt in [[1.0f64, 2.0, 0.0, 1.0], [3.0, 2.0, 0.0, 1.0]] {
                    for v in pt {
                        blk.extend(v.to_le_bytes());
                    }
                }
                let len = u32::try_from(blk.len() + 4).unwrap();
                b.extend(len.to_le_bytes());
                b.extend(blk);
            }
        }
        (0xc3, b)
    };
    let p = mk(&[move_to(0.0, 0.0), nurbs(None)]);
    let pts = &path_points(&p)[0];
    assert!(pts.len() > 8, "{}", pts.len());
    assert!(close(*pts.last().unwrap(), (4.0 * 72.0, 11.0 * 72.0)));
    assert!(pts.iter().any(|q| q.1 < 11.0 * 72.0 - 20.0), "bulges up");
    // The same curve through a shape-data chunk (kind 0x82).
    let mut sd = vec![0x82u8];
    sd.extend([0u8; 15]);
    sd.extend(1.0f64.to_le_bytes());
    sd.extend(3u16.to_le_bytes());
    sd.extend([1, 1]);
    sd.extend(2u32.to_le_bytes());
    for pt in [[1.0f64, 2.0, 0.0, 1.0], [3.0, 2.0, 0.0, 1.0]] {
        for v in pt {
            sd.extend(v.to_le_bytes());
        }
    }
    let mut p2 = chunk(11, 0x15, NONE, &[0u8; 80]);
    p2.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p2.extend(chunk(11, 0x9b, 0, &xform_body((0.0, 0.0), (4.0, 2.0), (0.0, 0.0), 0.0, (0, 0))));
    p2.extend(chunk(11, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
    p2.extend(rows(11, &[move_to(0.0, 0.0), nurbs(Some(77))]));
    p2.extend(chunk(11, 0xd1, 77, &sd));
    let by_data = render(&drawing_ex(11, &[fg(p2)], &[], &[], &[]));
    assert_eq!(path_points(&by_data)[0], *pts, "data chunk and formula block give one curve");
}

/// A Misc chunk with the formula tokens of a connector glued to shapes `begin` and `end`.
fn misc_glue(begin: u32, end: u32) -> Vec<u8> {
    let mut b = vec![0u8; 45];
    for id in [begin, end] {
        let mut blk = vec![0u8; 4];
        blk.extend([2, 0, 0x74]);
        blk.extend(0x6000_004eu32.to_le_bytes());
        blk.extend(id.to_le_bytes());
        blk.push(0x7a);
        blk.extend(0x4000_0073u32.to_le_bytes());
        let len = u32::try_from(blk.len()).unwrap();
        blk[..4].copy_from_slice(&len.to_le_bytes());
        b.extend(blk);
    }
    b
}

#[test]
fn connections_shape_data_and_hyperlinks_are_read_and_marked_unverified() {
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((1.0, 9.0), (1.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    p.extend(text_chunk(11, 0, "Start"));
    p.extend(chunk(11, 0x48, 2, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((1.0, 5.0), (1.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    p.extend(text_chunk(11, 0, "Finish"));
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    p.extend(chunk(11, 0xb6, 3, &utf16("Cost\0\0Twelve\0\0")));
    p.extend(chunk(11, 0xc4, 4, &utf16("http://example.org/page\0\0")));
    // The connector: glued to shape 1 at its begin and to shape 2 at its end.
    p.extend(chunk(11, 0x48, 3, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((1.0, 7.0), (0.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    p.extend(chunk(11, 0xa4, 0, &misc_glue(1, 2)));
    let p = run(&drawing(11, &[p], &[])).unwrap();
    let lines: Vec<(String, u16, String)> = p.sections[0]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para(q) => Some((
                q.text(),
                q.runs.first().map_or(0, |r| r.style),
                q.runs.last().map_or(String::new(), |r| r.href.clone()),
            )),
            _ => None,
        })
        .collect();
    let texts: Vec<&str> = lines.iter().map(|(t, ..)| t.as_str()).collect();
    assert_eq!(&texts[..2], ["Start", "Finish"], "{texts:?}");
    // Shape data as "label: value"; the hyperlink as plain text that links nowhere.
    assert!(texts.contains(&"Cost: Twelve"), "{texts:?}");
    let link = lines.iter().find(|(t, ..)| t == "Hyperlink: http://example.org/page").expect("link text");
    assert!(link.2.is_empty(), "never a live link");
    // The connection is made up by the viewer and marked so.
    let conn = lines.iter().find(|(t, ..)| t == "Start → Finish").expect("connection");
    assert_eq!(conn.1 & crate::model::style::SYNTHETIC, crate::model::style::SYNTHETIC);
    // And what is not verified is said.
    let said = |what: &str| {
        p.notices
            .iter()
            .any(|n| n.kind == crate::model::NoticeKind::BestEffort && n.detail.contains(what))
    };
    assert!(said("connections"), "{:?}", p.notices);
    assert!(said("shape data"));
    assert!(said("hyperlinks"));
    assert!(said("follow the open-source descriptions"));
}

#[test]
fn a_shapes_own_text_that_reads_like_a_connection_stays_plain() {
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(text_chunk(11, 0, "One → Two"));
    let out = run(&drawing(11, &[p], &[])).unwrap();
    let Block::Para(q) = &out.sections[0].blocks[0] else { panic!() };
    assert_eq!(q.runs[0].style, 0);
}

#[test]
fn page_names_come_from_the_name_index_and_background_pages_lie_under() {
    // Names live in the container that holds the pages: a name table and an index that maps the
    // pointer index of each page to a name.
    let version = 11u16;
    let mut doc = vec![0u8; 0x24 + 18];
    doc[..20].copy_from_slice(b"Visio (TM) Drawing\r\n");
    doc[0x1a..0x1c].copy_from_slice(&version.to_le_bytes());
    let put = |doc: &mut Vec<u8>, data: &[u8]| {
        let at = doc.len();
        doc.extend_from_slice(data);
        (at, data.len())
    };
    // Pointer 0: names, 1: index, 2: background page, 3: page.
    let mut names = Vec::new();
    for n in ["Back", "Main"] {
        let (at, len) = put(&mut doc, &name_entry(version, n));
        names.push(pointer(0x33, at, len, 0x45));
    }
    let (at, len) = put(&mut doc, &container(&names));
    let names_ptr = pointer(0x32, at, len, 0x54);
    // Index records of 13 bytes: name id twice, element id, one byte. Names are ids 0 and 1.
    let mut idx = 2u32.to_le_bytes().to_vec();
    for (name, element) in [(0u32, 2u32), (1, 3)] {
        idx.extend(name.to_le_bytes());
        idx.extend(name.to_le_bytes());
        idx.extend(element.to_le_bytes());
        idx.push(0);
    }
    let (at, len) = put(&mut doc, &idx);
    let idx_ptr = pointer(0xc9, at, len, 0x44);
    let mut back = rect_page(version, (NONE, NONE, NONE), &chunk(version, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
    back.extend(text_chunk(version, 0, "b"));
    let mut front = chunk(version, 0x15, NONE, &{
        let mut b = vec![0u8; 80];
        put_u32(&mut b, 8, 2);
        b
    });
    front.extend(chunk(version, 0x48, 5, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    front.extend(chunk(version, 0x9b, 0, &xform_body((1.0, 1.0), (1.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    front.extend(chunk(version, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
    front.extend(rows(version, &[move_to(0.0, 0.0), line_to(1.0, 1.0)]));
    front.extend(text_chunk(version, 0, "f"));
    let (at, len) = put(&mut doc, &back);
    let back_ptr = pointer(0x15, at, len, 0xd4);
    let (at, len) = put(&mut doc, &front);
    let front_ptr = pointer(0x15, at, len, 0xd5);
    let (at, len) = put(&mut doc, &container(&[names_ptr, idx_ptr, back_ptr, front_ptr]));
    let pages_ptr = pointer(0x27, at, len, 0x54);
    let (at, len) = put(&mut doc, &container(&[pages_ptr]));
    doc[0x24..0x24 + 18].copy_from_slice(&pointer(20, at, len, 0x50));
    let mut w = CfbWriter::new();
    w.add_stream("VisioDocument", doc).unwrap();
    let data = w.finish().unwrap();
    let policy = PreviewPolicy::default();
    assert_eq!(crate::visio_draw::visio_page_count(&data, &policy).unwrap(), 1);
    let page = render(&data);
    assert_eq!(path_points(&page).len(), 2, "the background page and the page");
    let p = run(&data).unwrap();
    let titles: Vec<_> = p.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(titles, ["Back (background)", "Main"]);
}

#[test]
fn a_binary_page_hides_hidden_layers_by_policy() {
    // Layer 1 is not visible; the shape belongs to it (membership text "1").
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    let mut layer0 = vec![0u8; 16];
    layer0[8] = 0xff;
    layer0[14] = 1;
    let mut layer1 = layer0.clone();
    layer1[14] = 0;
    p.extend(chunk(11, 0xa8, 0, &layer0));
    p.extend(chunk(11, 0xa8, 1, &layer1));
    p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    p.extend(chunk(11, 0x9b, 0, &xform_body((1.0, 1.0), (1.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    p.extend(chunk(11, 0x85, 0, &line_body(0.01, 0, 1, (0, 0))));
    let mut mem = vec![0u8; 13];
    mem.push(1);
    mem.extend("1".encode_utf16().flat_map(u16::to_le_bytes));
    p.extend(chunk(11, 0xa7, 0, &mem));
    p.extend(rows(11, &[move_to(0.0, 0.0), line_to(1.0, 1.0)]));
    let data = drawing_ex(11, &[fg(p)], &[], &[], &[]);
    let shown = crate::visio_draw::render_visio_page_noted(&data, 0, &PreviewPolicy::default()).unwrap();
    assert_eq!(shown.hidden_shown, 1);
    let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
    let hidden = crate::visio_draw::render_visio_page_noted(&data, 0, &policy).unwrap();
    assert_eq!((hidden.hidden_omitted, path_points(&hidden.page).len()), (1, 0));
}

#[test]
fn hostile_binary_drawings_stay_bounded() {
    // A flood of shapes, a chain of parents far deeper than the limit, a cycle of parents, and
    // numbers that are not numbers.
    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    for i in 1..=120_000u32 {
        p.extend(chunk(11, 0x48, i, &shape_body(i - 1, NONE, NONE, (NONE, NONE, NONE))));
        p.extend(chunk(11, 0x9b, 0, &xform_body((1.0, 1.0), (1.0, 1.0), (0.0, 0.0), 0.0, (0, 0))));
    }
    let data = drawing_ex(11, &[fg(p)], &[], &[], &[]);
    let page = render(&data);
    assert!(page.truncated);
    let p = run(&data).unwrap();
    assert!(p.stats.truncated);

    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    for (id, parent) in [(1u32, 2u32), (2, 1), (3, 3)] {
        p.extend(chunk(11, 0x48, id, &shape_body(parent, 4, id, (id, id, id))));
        p.extend(text_chunk(11, 0, "cycle"));
    }
    let p = run(&drawing(11, &[p], &[])).unwrap();
    assert_eq!(p.sections[0].blocks.len(), 3, "every shape is shown once");

    let mut p = chunk(11, 0x15, NONE, &[0u8; 80]);
    p.extend(chunk(11, 0x48, 1, &shape_body(0, NONE, NONE, (NONE, NONE, NONE))));
    let bad = nums(&[f64::NAN, f64::INFINITY, 1e308, -1e308, 1e300, 1e300, 1e300]);
    p.extend(chunk(11, 0x9b, 0, &bad));
    p.extend(chunk(11, 0x85, 0, &line_body(f64::NAN, 0, 1, (0, 0))));
    let mut nurbs = nums(&[1.0, 1.0]);
    for v in [f64::NAN, 1e300, -1e300, f64::INFINITY] {
        nurbs.extend(v.to_le_bytes());
    }
    nurbs.push(0x40);
    nurbs.push(0);
    nurbs.extend([0u8; 9]);
    // A block that claims a huge repetition count and one whose length is zero.
    let mut blk = vec![0u8, 6, 0x8a];
    blk.extend(f64::NAN.to_le_bytes());
    blk.extend(u16::MAX.to_le_bytes());
    blk.extend([1, 1]);
    blk.extend(u32::MAX.to_le_bytes());
    nurbs.extend(u32::try_from(blk.len() + 4).unwrap().to_le_bytes());
    nurbs.extend(blk);
    nurbs.extend(0u32.to_le_bytes());
    p.extend(rows(11, &[move_to(0.0, 0.0), (0xc3, nurbs.clone()), (0xc1, nurbs), (0x90, nums(&[f64::NAN; 6]))]));
    let page = render(&drawing_ex(11, &[fg(p)], &[], &[], &[]));
    for q in &page.prims {
        if let crate::page::Prim::Path { ops, .. } = q {
            assert!(ops.iter().all(|v| v.is_finite()));
        }
    }
}

#[test]
fn damaged_binary_drawings_never_panic() {
    let mut extra = chunk(11, 0x85, 0, &line_body(0.02, 0xff, 1, (1, 1)));
    extra.extend(chunk(11, 0x86, 0, &fill_body(Some(0xff0000), 0, None, 0, 1)));
    let page = rect_page(11, (3, 3, 3), &extra);
    let colours = Extra { ty: 0x16, data: vec![0, 0, 1, 0, 1, 2, 3, 0], format: 0x44 };
    let base = drawing_ex(11, &[fg(page)], &["a", "b", "c"], &[colours], &[rect_page(11, (NONE, NONE, NONE), &[])]);
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..1500 {
        let mut m = base.clone();
        for _ in 0..1 + next() % 12 {
            let at = usize::try_from(next()).unwrap() % m.len();
            m[at] = u8::try_from(next() % 256).unwrap();
        }
        let _ = run(&m);
        let _ = crate::visio_draw::render_visio_page(&m, 0, &PreviewPolicy::default());
    }
}

#[test]
fn a_drawing_without_those_chunks_does_not_claim_to_have_skipped_them() {
    let p = run(&drawing(11, &[page(11)], &[])).unwrap();
    let said = |what: &str| p.notices.iter().any(|n| n.detail.contains(what));
    assert!(!said("connections") && !said("shape data") && !said("hyperlinks"));
}

#[test]
fn stored_streams_are_charged_to_the_budget_like_inflated_ones() {
    let data = drawing(11, &[page(11)], &[]);
    assert!(run(&data).is_ok());
    let tight = PreviewPolicy { inflate_budget: 64, ..PreviewPolicy::default() };
    assert!(preview(&data, &tight).is_err(), "copies of stored streams must not be free");
}
