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

/// A drawing with the given pages (each a chunk stream) and names.
fn drawing(version: u16, pages: &[Vec<u8>], names: &[&str]) -> Vec<u8> {
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
    // Pages.
    let mut page_ptrs = Vec::new();
    for p in pages {
        let (at, len) = put(&mut doc, p);
        page_ptrs.push(pointer(0x15, at, len, 0xd4));
    }
    let (at, len) = put(&mut doc, &container(&page_ptrs));
    let pages_ptr = pointer(0x27, at, len, 0x54);
    // The trailer.
    let (at, len) = put(&mut doc, &container(&[names_ptr, pages_ptr]));
    let trailer = pointer(20, at, len, 0x50);
    doc[0x24..0x24 + 18].copy_from_slice(&trailer);
    let size = u32::try_from(doc.len()).unwrap();
    doc[0x1c..0x20].copy_from_slice(&size.to_le_bytes());

    let mut w = CfbWriter::new();
    w.add_stream("VisioDocument", doc).unwrap();
    w.finish().unwrap()
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
