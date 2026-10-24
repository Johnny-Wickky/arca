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
    // The writer also filled the sizes of the local header, so only the layout can object.
    out[18..22].copy_from_slice(&(cd_offset - 35).to_le_bytes());
    out[22..26].copy_from_slice(&(cd_offset - 35).to_le_bytes());
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

// ---------------------------------------------------------------------------
// Hostile structure: hand-assembled members with full control over both headers
// ---------------------------------------------------------------------------

/// A member whose local header, central header, extras, comment and trailer are free.
#[derive(Clone)]
struct Spec {
    raw: Raw,
    local_flags: u16,
    /// `(crc, compressed, uncompressed)` as written in the local header.
    local_sizes: (u32, u32, u32),
    local_extra: Vec<u8>,
    central_extra: Vec<u8>,
    comment: Vec<u8>,
    /// Bytes written straight after the payload (a data descriptor, or garbage).
    after: Vec<u8>,
    central_csize: u32,
}

impl Spec {
    fn new(raw: Raw) -> Self {
        let csize = u32::try_from(raw.payload.len()).unwrap();
        Self {
            local_sizes: (raw.crc, csize, raw.usize_),
            central_csize: csize,
            raw,
            local_flags: 0,
            local_extra: Vec::new(),
            central_extra: Vec::new(),
            comment: Vec::new(),
            after: Vec::new(),
        }
    }
}

fn extra_field(id: u16, body: &[u8]) -> Vec<u8> {
    let mut e = Vec::new();
    p16(&mut e, id);
    p16(&mut e, u16::try_from(body.len()).unwrap());
    e.extend_from_slice(body);
    e
}

fn unicode_path_field(header_name: &[u8], unicode: &[u8], good_crc: bool) -> Vec<u8> {
    let mut body = vec![1u8];
    let crc = arca_digest::crc32(header_name);
    p32(&mut body, if good_crc { crc } else { !crc });
    body.extend_from_slice(unicode);
    extra_field(0x7075, &body)
}

fn assemble(members: &[Spec], archive_comment: &[u8], trailer: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for m in members {
        offsets.push(u32::try_from(out.len()).unwrap());
        p32(&mut out, 0x0403_4b50);
        p16(&mut out, 20);
        p16(&mut out, m.local_flags);
        p16(&mut out, m.raw.method);
        p16(&mut out, 0);
        p16(&mut out, 0x21);
        p32(&mut out, m.local_sizes.0);
        p32(&mut out, m.local_sizes.1);
        p32(&mut out, m.local_sizes.2);
        p16(&mut out, u16::try_from(m.raw.name.len()).unwrap());
        p16(&mut out, u16::try_from(m.local_extra.len()).unwrap());
        out.extend_from_slice(&m.raw.name);
        out.extend_from_slice(&m.local_extra);
        out.extend_from_slice(&m.raw.payload);
        out.extend_from_slice(&m.after);
    }
    let cd_offset = u32::try_from(out.len()).unwrap();
    let mut cd = Vec::new();
    for (m, off) in members.iter().zip(&offsets) {
        p32(&mut cd, 0x0201_4b50);
        p16(&mut cd, m.raw.made_by);
        p16(&mut cd, 20);
        p16(&mut cd, m.local_flags);
        p16(&mut cd, m.raw.method);
        p16(&mut cd, 0);
        p16(&mut cd, 0x21);
        p32(&mut cd, m.raw.crc);
        p32(&mut cd, m.central_csize);
        p32(&mut cd, m.raw.usize_);
        p16(&mut cd, u16::try_from(m.raw.name.len()).unwrap());
        p16(&mut cd, u16::try_from(m.central_extra.len()).unwrap());
        p16(&mut cd, u16::try_from(m.comment.len()).unwrap());
        p16(&mut cd, 0);
        p16(&mut cd, 0);
        p32(&mut cd, m.raw.ext_attr);
        p32(&mut cd, *off);
        cd.extend_from_slice(&m.raw.name);
        cd.extend_from_slice(&m.central_extra);
        cd.extend_from_slice(&m.comment);
    }
    let cd_size = u32::try_from(cd.len()).unwrap();
    out.extend_from_slice(&cd);
    p32(&mut out, 0x0605_4b50);
    p16(&mut out, 0);
    p16(&mut out, 0);
    p16(&mut out, u16::try_from(members.len()).unwrap());
    p16(&mut out, u16::try_from(members.len()).unwrap());
    p32(&mut out, cd_size);
    p32(&mut out, cd_offset);
    p16(&mut out, u16::try_from(archive_comment.len()).unwrap());
    out.extend_from_slice(archive_comment);
    out.extend_from_slice(trailer);
    out
}

/// An end-of-central-directory record on its own (22 bytes, no comment).
fn lone_eocd() -> Vec<u8> {
    let mut e = Vec::new();
    eocd(&mut e, 0, 0, 0);
    e
}

#[test]
fn a_second_end_record_hidden_in_an_entry_comment_is_refused() {
    // The record at the end of the file is a decoy; a reader that stops at the signature
    // in the comment sees a different central directory.
    let mut m = Spec::new(Raw::stored(b"a.xml", b"x"));
    m.comment = lone_eocd();
    let bytes = assemble(&[m], &[], &[]);
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::AmbiguousEndOfCentralDirectory)
    ));
}

#[test]
fn an_end_record_signature_after_the_real_one_is_refused() {
    // The tail record claims a comment that does not fit, so the real record stays the
    // chosen one, but a reader that scans from the front would meet the other first.
    let mut fake = lone_eocd();
    fake[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
    let bytes = assemble(&[Spec::new(Raw::stored(b"a.xml", b"x"))], &[], &fake);
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::AmbiguousEndOfCentralDirectory)
    ));
    // The same bytes inside the archive comment of a well-formed record.
    let bytes = assemble(&[Spec::new(Raw::stored(b"a.xml", b"x"))], &fake, &[]);
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::AmbiguousEndOfCentralDirectory)
    ));
}

#[test]
fn a_stored_nested_archive_is_not_a_second_end_record() {
    let inner = assemble(&[Spec::new(Raw::stored(b"in.xml", b"inner"))], &[], &[]);
    let bytes = assemble(&[Spec::new(Raw::stored(b"nested.zip", &inner))], &[], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    let e = zip.entry("nested.zip").unwrap();
    assert_eq!(zip.read(e, &mut budget()).unwrap(), inner);
}

#[test]
fn bytes_that_merely_spell_the_signature_are_not_a_second_end_record() {
    // A CRC or an offset can equal "PK\x05\x06"; without the shape of a record it is data.
    let mut m = Spec::new(Raw::stored(b"a.xml", b"x"));
    m.comment = vec![b'P', b'K', 5, 6, 1, 1, 1, 1];
    let bytes = assemble(&[m], &[], &[]);
    assert!(ZipArchive::parse(&bytes, ZipLimits::default()).is_ok());
}

#[test]
fn entry_comments_are_reported() {
    let mut m = Spec::new(Raw::stored(b"a.xml", b"x"));
    m.comment = b"note".to_vec();
    let bytes = assemble(&[m], &[], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(zip.anomalies().iter().any(|a| matches!(a, ZipAnomaly::ArchiveComment { bytes: 4 })));
}

#[test]
fn unread_bytes_inside_a_deflate_range_are_refused() {
    let mut raw = Raw::deflated(b"a.xml", &[b'x'; 500]);
    raw.payload.extend_from_slice(b"HIDDEN-PAYLOAD");
    let bytes = assemble(&[Spec::new(raw)], &[], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(matches!(
        zip.read(&zip.entries()[0], &mut budget()),
        Err(ZipError::CompressedSizeMismatch(_))
    ));
}

fn descriptor(crc: u32, csize: u32, usize_: u32, signed: bool) -> Vec<u8> {
    let mut d = Vec::new();
    if signed {
        p32(&mut d, 0x0807_4b50);
    }
    p32(&mut d, crc);
    p32(&mut d, csize);
    p32(&mut d, usize_);
    d
}

#[test]
fn a_matching_data_descriptor_is_accepted_and_part_of_the_member() {
    for signed in [true, false] {
        let raw = Raw::stored(b"a.xml", b"hello");
        let mut m = Spec::new(raw.clone());
        m.local_flags = 8;
        m.local_sizes = (0, 0, 0);
        m.after = descriptor(raw.crc, 5, 5, signed);
        let bytes = assemble(&[m], &[], &[]);
        let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
        assert!(zip.anomalies().is_empty(), "{:?}", zip.anomalies());
        assert_eq!(zip.read(&zip.entries()[0], &mut budget()).unwrap(), b"hello");
    }
}

#[test]
fn a_lying_data_descriptor_is_refused() {
    let raw = Raw::stored(b"a.xml", b"hello");
    for (crc, c, u) in [(raw.crc ^ 1, 5, 5), (raw.crc, 6, 5), (raw.crc, 5, 99)] {
        let mut m = Spec::new(raw.clone());
        m.local_flags = 8;
        m.local_sizes = (0, 0, 0);
        m.after = descriptor(crc, c, u, true);
        let bytes = assemble(&[m], &[], &[]);
        assert!(matches!(
            ZipArchive::parse(&bytes, ZipLimits::default()),
            Err(ZipError::LocalHeaderMismatch(_))
        ));
    }
    // Flagged, but neither a descriptor nor sizes in the local header.
    let mut m = Spec::new(raw);
    m.local_flags = 8;
    m.local_sizes = (0, 0, 0);
    let bytes = assemble(&[m], &[], &[]);
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::LocalHeaderMismatch(_))
    ));
}

#[test]
fn unreferenced_bytes_between_members_are_reported_even_when_small() {
    let mut a = Spec::new(Raw::stored(b"a.xml", b"aaaa"));
    a.after = vec![0xEE; 6]; // not a descriptor, nothing accounts for it
    let bytes = assemble(&[a, Spec::new(Raw::stored(b"b.xml", b"bbbb"))], &[], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(
        zip.anomalies().iter().any(|a| matches!(a, ZipAnomaly::UnreferencedData { bytes: 6 })),
        "{:?}",
        zip.anomalies()
    );
}

fn zip64_sizes(uncompressed: u64, compressed: u64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&uncompressed.to_le_bytes());
    body.extend_from_slice(&compressed.to_le_bytes());
    extra_field(0x0001, &body)
}

#[test]
fn zip64_markers_in_the_local_header_are_checked_against_the_central_sizes() {
    let raw = Raw::stored(b"a.xml", b"hello");
    let marked = |extra: Vec<u8>| {
        let mut m = Spec::new(raw.clone());
        m.local_sizes = (raw.crc, u32::MAX, u32::MAX);
        m.local_extra = extra;
        assemble(&[m], &[], &[])
    };
    // Honest sizes in the local ZIP64 extra field.
    let ok = marked(zip64_sizes(5, 5));
    assert!(ZipArchive::parse(&ok, ZipLimits::default()).is_ok());
    // Lying sizes, or none at all.
    for extra in [zip64_sizes(5, 6), zip64_sizes(500, 5), Vec::new()] {
        assert!(matches!(
            ZipArchive::parse(&marked(extra), ZipLimits::default()),
            Err(ZipError::LocalHeaderMismatch(_))
        ));
    }
}

#[test]
fn a_directory_entry_cannot_carry_data() {
    let mut raw = Raw::stored(b"dir/", b"");
    raw.payload = vec![0x55; 64];
    let bytes = assemble(&[Spec::new(raw)], &[], &[]);
    assert!(matches!(
        ZipArchive::parse(&bytes, ZipLimits::default()),
        Err(ZipError::LocalHeaderMismatch(_))
    ));
    // An empty deflate stream (two bytes) is what some writers emit.
    let mut raw = Raw::deflated(b"dir/", b"");
    raw.usize_ = 0;
    let bytes = assemble(&[Spec::new(raw)], &[], &[]);
    assert!(ZipArchive::parse(&bytes, ZipLimits::default()).is_ok());
}

#[test]
fn a_unicode_path_extra_that_renames_the_member_is_refused() {
    let raw = Raw::stored(b"word/document.xml", b"x");
    let renamed = |central: bool, good_crc: bool| {
        let mut m = Spec::new(raw.clone());
        let field = unicode_path_field(b"word/document.xml", b"word/evil.bin", good_crc);
        if central {
            m.central_extra = field;
        } else {
            m.local_extra = field;
        }
        assemble(&[m], &[], &[])
    };
    for central in [true, false] {
        assert!(matches!(
            ZipArchive::parse(&renamed(central, true), ZipLimits::default()),
            Err(ZipError::UnicodePathMismatch(_))
        ));
    }
    // A field whose CRC is not the header name's belongs to a stale name: readers drop it.
    assert!(ZipArchive::parse(&renamed(true, false), ZipLimits::default()).is_ok());
    // The same name in the field is only reported.
    let mut m = Spec::new(raw);
    m.central_extra = unicode_path_field(b"word/document.xml", b"word/document.xml", true);
    let bytes = assemble(&[m], &[], &[]);
    let zip = ZipArchive::parse(&bytes, ZipLimits::default()).unwrap();
    assert!(zip.anomalies().iter().any(|a| matches!(a, ZipAnomaly::UnicodePathExtra(_))));
}
