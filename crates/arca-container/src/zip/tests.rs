//! Adversarial tests for the ZIP reader.

use super::writer::{Compression, ZipWriter};
use super::*;

/// A raw member description that bypasses every writer-side check.
#[derive(Clone)]
struct Raw {
    name: Vec<u8>,
    payload: Vec<u8>,
    method: u16,
    crc: u32,
    usize_: u32,
    made_by: u16,
    ext_attr: u32,
}

impl Raw {
    fn stored(name: &[u8], data: &[u8]) -> Self {
        Self {
            name: name.to_vec(),
            payload: data.to_vec(),
            method: METHOD_STORED,
            crc: arca_digest::crc32(data),
            usize_: u32::try_from(data.len()).unwrap(),
            made_by: 20,
            ext_attr: 0,
        }
    }

    fn deflated(name: &[u8], data: &[u8]) -> Self {
        let payload = miniz_oxide::deflate::compress_to_vec(data, 6);
        Self {
            name: name.to_vec(),
            payload,
            method: METHOD_DEFLATE,
            crc: arca_digest::crc32(data),
            usize_: u32::try_from(data.len()).unwrap(),
            made_by: 20,
            ext_attr: 0,
        }
    }
}

fn p16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn p32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn local(o: &mut Vec<u8>, r: &Raw) {
    p32(o, 0x0403_4b50);
    p16(o, 20);
    p16(o, 0);
    p16(o, r.method);
    p16(o, 0);
    p16(o, 0x21);
    p32(o, r.crc);
    p32(o, u32::try_from(r.payload.len()).unwrap());
    p32(o, r.usize_);
    p16(o, u16::try_from(r.name.len()).unwrap());
    p16(o, 0);
    o.extend_from_slice(&r.name);
    o.extend_from_slice(&r.payload);
}

fn central(o: &mut Vec<u8>, r: &Raw, offset: u32, csize: u32) {
    p32(o, 0x0201_4b50);
    p16(o, r.made_by);
    p16(o, 20);
    p16(o, 0);
    p16(o, r.method);
    p16(o, 0);
    p16(o, 0x21);
    p32(o, r.crc);
    p32(o, csize);
    p32(o, r.usize_);
    p16(o, u16::try_from(r.name.len()).unwrap());
    p16(o, 0);
    p16(o, 0);
    p16(o, 0);
    p16(o, 0);
    p32(o, r.ext_attr);
    p32(o, offset);
    o.extend_from_slice(&r.name);
}

fn eocd(o: &mut Vec<u8>, count: u16, cd_size: u32, cd_offset: u32) {
    p32(o, 0x0605_4b50);
    p16(o, 0);
    p16(o, 0);
    p16(o, count);
    p16(o, count);
    p32(o, cd_size);
    p32(o, cd_offset);
    p16(o, 0);
}

fn build(prefix: &[u8], members: &[Raw], trailer: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    let mut offsets = Vec::new();
    for m in members {
        offsets.push(u32::try_from(out.len()).unwrap());
        local(&mut out, m);
    }
    let cd_offset = u32::try_from(out.len()).unwrap();
    let mut cd = Vec::new();
    for (m, off) in members.iter().zip(&offsets) {
        central(&mut cd, m, *off, u32::try_from(m.payload.len()).unwrap());
    }
    let cd_size = u32::try_from(cd.len()).unwrap();
    out.extend_from_slice(&cd);
    eocd(&mut out, u16::try_from(members.len()).unwrap(), cd_size, cd_offset);
    out.extend_from_slice(trailer);
    out
}

fn budget() -> InflateBudget {
    InflateBudget::default()
}

#[test]
fn writer_roundtrip_is_readable_and_deterministic() {
    let make = || {
        let mut w = ZipWriter::new();
        w.add("[Content_Types].xml", b"<Types/>", Compression::Deflated).unwrap();
        w.add(
            "word/document.xml",
            &b"<w:document>hello</w:document>".repeat(50),
            Compression::Deflated,
        )
        .unwrap();
        w.add("mimetype", b"application/x", Compression::Stored).unwrap();
        w.finish().unwrap()
    };
    let bytes = make();
    assert_eq!(bytes, make(), "writer output must be reproducible");
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert_eq!(zip.entries().len(), 3);
    assert!(zip.anomalies().is_empty(), "{:?}", zip.anomalies());
    let mut b = budget();
    let doc = zip.read_by_name("WORD/Document.XML", &mut b).unwrap().unwrap();
    assert_eq!(doc, b"<w:document>hello</w:document>".repeat(50));
    assert_eq!(zip.read_by_name("mimetype", &mut b).unwrap().unwrap(), b"application/x");
    assert!(zip.read_by_name("absent", &mut b).unwrap().is_none());
}

#[test]
fn writer_rejects_bad_and_duplicate_names() {
    let mut w = ZipWriter::new();
    assert!(w.add("../x", b"", Compression::Stored).is_err());
    w.add("a.xml", b"", Compression::Stored).unwrap();
    assert!(matches!(w.add("A.XML", b"", Compression::Stored), Err(ZipError::DuplicateName(_))));
}

#[test]
fn empty_archive_parses() {
    let bytes = build(&[], &[], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(zip.entries().is_empty());
}

#[test]
fn rejects_path_traversal_variants() {
    for evil in [
        &b"../../etc/passwd"[..],
        b"/abs",
        b"..\\..\\win.ini",
        b"C:/boot.ini",
        b"a/../../b",
        b"ok/\x00.xml",
    ] {
        let bytes = build(&[], &[Raw::stored(evil, b"x")], &[]);
        let err = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap_err();
        assert!(matches!(err, ZipError::InvalidName { .. }), "{evil:?}: {err}");
    }
}

#[test]
fn rejects_case_insensitive_duplicates() {
    let bytes = build(
        &[],
        &[Raw::stored(b"word/document.xml", b"a"), Raw::stored(b"Word/Document.xml", b"b")],
        &[],
    );
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::DuplicateName(_))
    ));
}

#[test]
fn rejects_unix_symlinks() {
    let mut link = Raw::stored(b"word/link", b"/etc/passwd");
    link.made_by = (3 << 8) | 20;
    link.ext_attr = 0o120_777 << 16;
    let bytes = build(&[], &[link], &[]);
    assert!(matches!(ZipArchive::parse(&bytes, ZipLimits::default()), Err(ZipError::Symlink(_))));
}

#[test]
fn ratio_bomb_is_refused_before_inflation() {
    let zeros = vec![0u8; 10 << 20];
    let bytes = build(&[], &[Raw::deflated(b"bomb.xml", &zeros)], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    let mut b = budget();
    let err = zip.read(&zip.entries()[0], &mut b).unwrap_err();
    assert!(matches!(err, ZipError::CompressionBomb { .. }), "{err}");
    assert_eq!(b.produced(), 0, "nothing may be charged for a refused member");
}

#[test]
fn small_repetitive_parts_within_grace_are_allowed() {
    let zeros = vec![b'a'; 512 << 10];
    let bytes = build(&[], &[Raw::deflated(b"styles.xml", &zeros)], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    let out = zip.read(&zip.entries()[0], &mut budget()).unwrap();
    assert_eq!(out.len(), 512 << 10);
}

#[test]
fn lying_uncompressed_size_is_detected() {
    let mut m = Raw::deflated(b"liar.xml", &vec![b'z'; 4096]);
    m.usize_ = 100;
    let bytes = build(&[], &[m], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(matches!(zip.read(&zip.entries()[0], &mut budget()), Err(ZipError::SizeMismatch(_))));
}

#[test]
fn overstated_uncompressed_size_is_detected() {
    // The header promises more bytes than the stream delivers.
    let mut m = Raw::deflated(b"short.xml", &[b'z'; 100]);
    m.usize_ = 4096;
    let bytes = build(&[], &[m], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(matches!(zip.read(&zip.entries()[0], &mut budget()), Err(ZipError::SizeMismatch(_))));
}

#[test]
fn truncated_deflate_stream_is_corrupt_not_a_size_error() {
    let noise: Vec<u8> =
        (0..2048u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let mut m = Raw::deflated(b"cut.xml", &noise);
    let cut = m.payload.len() / 2;
    m.payload.truncate(cut);
    let bytes = build(&[], &[m], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(matches!(zip.read(&zip.entries()[0], &mut budget()), Err(ZipError::Inflate(_))));
}

#[test]
fn empty_deflated_member_is_readable() {
    let bytes = build(&[], &[Raw::deflated(b"empty.xml", b"")], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(zip.read(&zip.entries()[0], &mut budget()).unwrap().is_empty());
}

#[test]
fn crc_mismatch_is_detected() {
    let mut m = Raw::stored(b"a.xml", b"hello");
    m.crc ^= 1;
    let bytes = build(&[], &[m], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(matches!(zip.read(&zip.entries()[0], &mut budget()), Err(ZipError::CrcMismatch(_))));
}

#[test]
fn overlapping_members_are_rejected() {
    // Entry "b" is written normally; entry "a" claims a local header at
    // offset 0 whose data range swallows b's header (Fifield-style overlap).
    let b = Raw::stored(b"b.xml", b"BBBB");
    let mut out = Vec::new();
    let a = Raw::stored(b"a.xml", b"");
    local(&mut out, &a);
    let b_off = u32::try_from(out.len()).unwrap();
    local(&mut out, &b);
    let cd_offset = u32::try_from(out.len()).unwrap();
    let mut cd = Vec::new();
    // a's compressed size covers b's whole local record.
    let mut a_big = a.clone();
    a_big.usize_ = cd_offset - 35;
    central(&mut cd, &a_big, 0, cd_offset - 35);
    central(&mut cd, &b, b_off, 4);
    let cd_size = u32::try_from(cd.len()).unwrap();
    out.extend_from_slice(&cd);
    eocd(&mut out, 2, cd_size, cd_offset);
    let err = ZipArchive::parse(&out, ZipLimits::default()).unwrap_err();
    // Local sizes of "a" disagree with the central directory, or the ranges
    // overlap; both are fatal.
    assert!(
        matches!(err, ZipError::OverlappingEntries(..) | ZipError::LocalHeaderMismatch(_)),
        "{err}"
    );
}

#[test]
fn overlap_with_data_descriptor_style_headers_is_rejected() {
    // Same as above but the local header of "a" uses a data descriptor so
    // only the layout check can catch the overlap.
    let b = Raw::stored(b"b.xml", b"BBBB");
    let mut out = Vec::new();
    p32(&mut out, 0x0403_4b50);
    p16(&mut out, 20);
    p16(&mut out, FLAG_DATA_DESCRIPTOR);
    p16(&mut out, METHOD_STORED);
    p16(&mut out, 0);
    p16(&mut out, 0x21);
    p32(&mut out, 0);
    p32(&mut out, 0);
    p32(&mut out, 0);
    p16(&mut out, 5);
    p16(&mut out, 0);
    out.extend_from_slice(b"a.xml");
    let b_off = u32::try_from(out.len()).unwrap();
    local(&mut out, &b);
    let cd_offset = u32::try_from(out.len()).unwrap();
    let mut cd = Vec::new();
    let a = Raw { usize_: cd_offset - 35, ..Raw::stored(b"a.xml", b"") };
    central(&mut cd, &a, 0, cd_offset - 35);
    central(&mut cd, &b, b_off, 4);
    let cd_size = u32::try_from(cd.len()).unwrap();
    out.extend_from_slice(&cd);
    eocd(&mut out, 2, cd_size, cd_offset);
    let err = ZipArchive::parse(&out, ZipLimits::default()).unwrap_err();
    assert!(matches!(err, ZipError::OverlappingEntries(..)), "{err}");
}

#[test]
fn local_and_central_names_must_agree() {
    let m = Raw::stored(b"good.xml", b"x");
    let mut out = Vec::new();
    local(&mut out, &Raw::stored(b"evil.xml", b"x"));
    let cd_offset = u32::try_from(out.len()).unwrap();
    let mut cd = Vec::new();
    central(&mut cd, &m, 0, 1);
    let cd_size = u32::try_from(cd.len()).unwrap();
    out.extend_from_slice(&cd);
    eocd(&mut out, 1, cd_size, cd_offset);
    assert!(matches!(
        ZipArchive::parse(&out, ZipLimits::default()),
        Err(ZipError::LocalHeaderMismatch(_))
    ));
}

#[test]
fn declared_entry_count_must_match() {
    let mut bytes = build(&[], &[Raw::stored(b"a", b"1"), Raw::stored(b"b", b"2")], &[]);
    // Patch the EOCD entry counts from 2 to 1.
    let n = bytes.len();
    bytes[n - 12] = 1;
    bytes[n - 14] = 1;
    assert!(ZipArchive::parse(&bytes, ZipLimits::default()).is_err());
}

#[test]
fn shifted_central_directory_is_rejected() {
    let mut bytes = build(&[], &[Raw::stored(b"a", b"1")], &[]);
    let n = bytes.len();
    // Decrease the declared cd_offset by one.
    bytes[n - 6] = bytes[n - 6].wrapping_sub(1);
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::CentralDirectoryMismatch | ZipError::InvalidCentralHeader { .. })
    ));
}

#[test]
fn prefix_and_trailing_data_are_reported() {
    let bytes = build(b"%PDF-1.7 polyglot prefix", &[Raw::stored(b"a.xml", b"x")], b"TRAILER");
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(zip.anomalies().iter().any(|a| matches!(a, ZipAnomaly::PrefixData { bytes: 24 })));
    assert!(zip.anomalies().iter().any(|a| matches!(a, ZipAnomaly::TrailingData { bytes: 7 })));
}

#[test]
fn truncated_archives_never_panic() {
    let bytes = build(
        &[],
        &[Raw::deflated(b"word/document.xml", &b"abc".repeat(1000)), Raw::stored(b"b", b"x")],
        &[],
    );
    for cut in 0..bytes.len() {
        let _ = ZipArchive::parse(&bytes[..cut], ZipLimits::default());
    }
    // Every single-byte corruption must be handled without panicking.
    for i in 0..bytes.len() {
        let mut c = bytes.clone();
        c[i] ^= 0xA5;
        if let Ok(zip) = ZipArchive::parse(&c, ZipLimits::default()) {
            for e in zip.entries() {
                let _ = zip.read(e, &mut budget());
            }
        }
    }
}

#[test]
fn budget_is_shared_across_members() {
    let data = vec![b'q'; 600 << 10];
    let bytes = build(&[], &[Raw::deflated(b"a", &data), Raw::deflated(b"b", &data)], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    let mut small = InflateBudget::new(1 << 20, 1_000_000, 0);
    assert!(zip.read(&zip.entries()[0], &mut small).is_ok());
    assert!(matches!(zip.read(&zip.entries()[1], &mut small), Err(ZipError::Budget(_))));
}

#[test]
fn entry_count_limit() {
    let members: Vec<Raw> =
        (0..10).map(|i| Raw::stored(format!("f{i}").as_bytes(), b"x")).collect();
    let bytes = build(&[], &members, &[]);
    let limits = ZipLimits { max_entries: 5, ..ZipLimits::default() };
    assert!(matches!(ZipArchive::parse(&bytes, limits), Err(ZipError::TooManyEntries { .. })));
}
