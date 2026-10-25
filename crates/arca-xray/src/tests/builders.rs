//! Builders of hostile files for the hardening tests, on top of the fixtures: every byte of every
//! sample is made here, in the test that uses it.

use arca_container::cfb::writer::CfbBuilder;

use super::fixtures::*;

// ---- OOXML ------------------------------------------------------------------------------------

/// `<w:fldChar>` run of the given type.
pub(crate) fn fld_char(kind: &str) -> String {
    format!("<w:r><w:fldChar w:fldCharType=\"{kind}\"/></w:r>")
}

/// `<w:instrText>` run.
pub(crate) fn instr(text: &str) -> String {
    format!("<w:r><w:instrText xml:space=\"preserve\">{text}</w:instrText></w:r>")
}

/// A complete field with a cached result.
pub(crate) fn field(instruction: &str) -> String {
    format!(
        "{}{}{}<w:r><w:t>result</w:t></w:r>{}",
        fld_char("begin"),
        instr(instruction),
        fld_char("separate"),
        fld_char("end")
    )
}

/// A field whose instruction is `before`, then a nested field with instruction `inner`, then
/// `after`.
pub(crate) fn nested_field(before: &str, inner: &str, after: &str) -> String {
    format!(
        "<w:p>{}{}{}{}{}{}{}</w:p>",
        fld_char("begin"),
        if before.is_empty() { String::new() } else { instr(before) },
        field(inner),
        instr(after),
        fld_char("separate"),
        "<w:r><w:t>x</w:t></w:r>",
        fld_char("end")
    )
}

/// A `.docx` whose body is `body`.
pub(crate) fn docx_body(body: &str) -> Opc {
    let mut o = docx();
    o.set("word/document.xml", document(body));
    o
}

/// A `.docx` whose main part has been renamed to `name` (content type and relationship follow).
pub(crate) fn docx_main_part_named(name: &str, body: &str) -> Opc {
    let mut o = Opc::default();
    o.set("[Content_Types].xml", content_types(&[(&format!("/{name}"), CT_DOCX)]))
        .set("_rels/.rels", rels(&[("rId1", "officeDocument", name, false)]))
        .set(name, document(body));
    o
}

// ---- ZIP --------------------------------------------------------------------------------------

/// One entry of a raw (stored) ZIP archive.
pub(crate) struct Raw<'a> {
    pub(crate) name: &'a str,
    pub(crate) data: &'a [u8],
    pub(crate) made_by: u16,
    pub(crate) ext_attr: u32,
    pub(crate) central_extra: Vec<u8>,
}

impl<'a> Raw<'a> {
    pub(crate) fn new(name: &'a str, data: &'a [u8]) -> Self {
        Self { name, data, made_by: 20, ext_attr: 0, central_extra: Vec::new() }
    }
}

fn p16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn p32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

/// A stored ZIP archive with full control of the headers `ZipWriter` never writes oddly.
pub(crate) fn raw_zip(entries: &[Raw<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for e in entries {
        offsets.push(u32::try_from(out.len()).unwrap());
        p32(&mut out, 0x0403_4b50);
        p16(&mut out, 20);
        p16(&mut out, 0);
        p16(&mut out, 0);
        p16(&mut out, 0);
        p16(&mut out, 0x21);
        p32(&mut out, arca_digest::crc32(e.data));
        p32(&mut out, u32::try_from(e.data.len()).unwrap());
        p32(&mut out, u32::try_from(e.data.len()).unwrap());
        p16(&mut out, u16::try_from(e.name.len()).unwrap());
        p16(&mut out, 0);
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(e.data);
    }
    let cd_start = out.len();
    for (e, off) in entries.iter().zip(&offsets) {
        p32(&mut out, 0x0201_4b50);
        p16(&mut out, e.made_by);
        p16(&mut out, 20);
        p16(&mut out, 0);
        p16(&mut out, 0);
        p16(&mut out, 0);
        p16(&mut out, 0x21);
        p32(&mut out, arca_digest::crc32(e.data));
        p32(&mut out, u32::try_from(e.data.len()).unwrap());
        p32(&mut out, u32::try_from(e.data.len()).unwrap());
        p16(&mut out, u16::try_from(e.name.len()).unwrap());
        p16(&mut out, u16::try_from(e.central_extra.len()).unwrap());
        p16(&mut out, 0);
        p16(&mut out, 0);
        p16(&mut out, 0);
        p32(&mut out, e.ext_attr);
        p32(&mut out, *off);
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&e.central_extra);
    }
    let cd_size = u32::try_from(out.len() - cd_start).unwrap();
    p32(&mut out, 0x0605_4b50);
    p16(&mut out, 0);
    p16(&mut out, 0);
    p16(&mut out, u16::try_from(entries.len()).unwrap());
    p16(&mut out, u16::try_from(entries.len()).unwrap());
    p32(&mut out, cd_size);
    p32(&mut out, u32::try_from(cd_start).unwrap());
    p16(&mut out, 0);
    out
}

/// An Info-ZIP Unicode Path extra field naming `unicode` for the member called `header_name`.
pub(crate) fn unicode_path_extra(header_name: &str, unicode: &str) -> Vec<u8> {
    let mut body = vec![1u8];
    p32(&mut body, arca_digest::crc32(header_name.as_bytes()));
    body.extend_from_slice(unicode.as_bytes());
    let mut e = Vec::new();
    p16(&mut e, 0x7075);
    p16(&mut e, u16::try_from(body.len()).unwrap());
    e.extend_from_slice(&body);
    e
}

// ---- PDF --------------------------------------------------------------------------------------

/// A PDF with the given extra objects (written verbatim) and an `/OpenAction`-free catalogue.
pub(crate) fn pdf_with(objects: &[u8]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n".to_vec();
    pdf.extend_from_slice(b"2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n");
    pdf.extend_from_slice(objects);
    pdf.extend_from_slice(b"\ntrailer << /Root 1 0 R >>\n%%EOF\n");
    pdf
}

/// The JavaScript action every PDF test hides in a stream.
pub(crate) const JS_ACTION: &[u8] = b"<< /Type /Action /S /JavaScript /JS (app.alert\\(1\\);) >>";

/// `3 0 obj` whose stream is `data` with the dictionary entries `dict` (`/Length` is added).
pub(crate) fn stream_object(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("3 0 obj << {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream\nendobj\n");
    o
}

pub(crate) fn hex_encode(data: &[u8]) -> Vec<u8> {
    let mut s: String = data.iter().map(|b| format!("{b:02x}")).collect();
    s.push('>');
    s.into_bytes()
}

// ---- RTF --------------------------------------------------------------------------------------

/// An RTF document with the given body.
pub(crate) fn rtf(body: &str) -> Vec<u8> {
    format!("{{\\rtf1\\ansi {body}}}").into_bytes()
}

// ---- Word binary -------------------------------------------------------------------------------

/// A `.doc` whose text is `pieces` (each an 8-bit piece stored at a place of its own, so that
/// text spanning two pieces is not contiguous in the stream), with the attached template
/// `template` if non-empty.
pub(crate) fn doc_with_pieces(pieces: &[&[u8]], template: &str) -> Vec<u8> {
    let mut word = vec![0u8; 0x400];
    word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    word[0x0A..0x0C].copy_from_slice(&0x0200u16.to_le_bytes()); // 1Table
    word[152..154].copy_from_slice(&34u16.to_le_bytes()); // cbRgFcLcb
    let mut places = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        let at = word.len().max(0x400 * (i + 1));
        word.resize(at, 0);
        places.push(at);
        word.extend_from_slice(p);
        word.resize(word.len() + 0x400, b' '); // padding between pieces
    }
    // Piece table (CLX): one Pcdt with n pieces.
    let n = pieces.len();
    let mut plc = Vec::new();
    let mut cp = 0u32;
    plc.extend_from_slice(&cp.to_le_bytes());
    for p in pieces {
        cp += u32::try_from(p.len()).unwrap();
        plc.extend_from_slice(&cp.to_le_bytes());
    }
    for at in &places {
        plc.extend_from_slice(&0u16.to_le_bytes());
        let fc = (u32::try_from(*at).unwrap() * 2) | 0x4000_0000;
        plc.extend_from_slice(&fc.to_le_bytes());
        plc.extend_from_slice(&0u16.to_le_bytes());
    }
    assert_eq!(plc.len(), 4 * (n + 1) + 8 * n);
    let mut table = vec![0x02];
    table.extend_from_slice(&u32::try_from(plc.len()).unwrap().to_le_bytes());
    table.extend_from_slice(&plc);
    let clx_len = table.len();
    // Associated strings: [file next, attached template].
    let assoc_at = table.len();
    table.extend_from_slice(&0xFFFFu16.to_le_bytes());
    table.extend_from_slice(&2u16.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    let units: Vec<u16> = template.encode_utf16().collect();
    table.extend_from_slice(&u16::try_from(units.len()).unwrap().to_le_bytes());
    for u in units {
        table.extend_from_slice(&u.to_le_bytes());
    }
    let assoc_len = table.len() - assoc_at;
    let pair = |word: &mut Vec<u8>, index: usize, fc: usize, lcb: usize| {
        let at = 154 + index * 8;
        word[at..at + 4].copy_from_slice(&u32::try_from(fc).unwrap().to_le_bytes());
        word[at + 4..at + 8].copy_from_slice(&u32::try_from(lcb).unwrap().to_le_bytes());
    };
    pair(&mut word, 33, 0, clx_len);
    pair(&mut word, 32, assoc_at, assoc_len);
    let mut b = CfbBuilder::new();
    b.add_stream("WordDocument", word).unwrap();
    b.add_stream("1Table", table).unwrap();
    b.finish().unwrap()
}

/// A compound file with the given root-level streams.
pub(crate) fn cfb_with(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut b = CfbBuilder::new();
    for (name, data) in streams {
        b.add_stream(name, data.clone()).unwrap();
    }
    b.finish().unwrap()
}

// ---- encoders for PDF filters and flat XML ------------------------------------------------------

pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// ASCII85 (with the `~>` terminator).
pub(crate) fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(buf);
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = u8::try_from(v % 85).unwrap() + b'!';
            v /= 85;
        }
        out.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    out.extend_from_slice(b"~>");
    out
}

/// RunLength: literal runs of at most 128 bytes, then the end marker.
pub(crate) fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(128) {
        out.push(u8::try_from(chunk.len() - 1).unwrap());
        out.extend_from_slice(chunk);
    }
    out.push(128);
    out
}

/// LZW with literal codes only (valid, and short inputs never leave 9-bit codes).
pub(crate) fn lzw_literals(data: &[u8]) -> Vec<u8> {
    assert!(data.len() < 240, "9-bit codes only");
    let mut bits: Vec<bool> = Vec::new();
    let mut push = |code: u16| (0..9).rev().for_each(|i| bits.push(code >> i & 1 == 1));
    push(256);
    for &b in data {
        push(u16::from(b));
    }
    push(257);
    bits.chunks(8)
        .map(|c| c.iter().enumerate().fold(0u8, |acc, (i, &b)| acc | (u8::from(b) << (7 - i))))
        .collect()
}

pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(char::from(T[usize::try_from((n >> (18 - 6 * i)) & 63).unwrap()]));
            } else {
                s.push('=');
            }
        }
    }
    s
}

// ---- ODF --------------------------------------------------------------------------------------

/// An OpenDocument package: `mimetype` first, a manifest, and the given members.
pub(crate) fn odf_zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    use arca_container::zip::writer::{Compression, ZipWriter};
    let mut w = ZipWriter::new();
    w.add("mimetype", b"application/vnd.oasis.opendocument.text", Compression::Stored).unwrap();
    w.add(
        "META-INF/manifest.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"/>"#,
        Compression::Deflated,
    )
    .unwrap();
    for (name, data) in members {
        w.add(name, data, Compression::Deflated).unwrap();
    }
    w.finish().unwrap()
}

/// `content.xml` of a text document with the given body markup.
pub(crate) fn odf_content(body: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:script="urn:oasis:names:tc:opendocument:xmlns:script:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"><office:body><office:text>{body}</office:text></office:body></office:document-content>"#
    )
    .into_bytes()
}

// ---- BIFF -------------------------------------------------------------------------------------

/// One BIFF record.
pub(crate) fn biff_record(id: u16, body: &[u8]) -> Vec<u8> {
    let mut r = id.to_le_bytes().to_vec();
    r.extend_from_slice(&u16::try_from(body.len()).unwrap().to_le_bytes());
    r.extend_from_slice(body);
    r
}

/// A workbook-globals stream: BOF, then the given records.
pub(crate) fn biff_workbook(records: &[Vec<u8>]) -> Vec<u8> {
    let mut s = biff_record(0x0809, &[0x00, 0x06, 0x05, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]);
    for r in records {
        s.extend_from_slice(r);
    }
    s
}

/// A BOUNDSHEET record for a sheet of the given type (`0x01` is an Excel 4.0 macro sheet).
pub(crate) fn biff_sheet(kind: u8, name: &str) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 0, 0, kind, u8::try_from(name.len()).unwrap(), 0];
    body.extend_from_slice(name.as_bytes());
    biff_record(0x0085, &body)
}

/// A SUPBOOK record whose virtual path is `path` (8-bit characters).
pub(crate) fn biff_supbook(path: &str) -> Vec<u8> {
    let mut body = vec![1, 0];
    body.extend_from_slice(&u16::try_from(path.chars().count()).unwrap().to_le_bytes());
    body.push(0);
    body.extend(path.chars().map(|c| u8::try_from(u32::from(c)).unwrap()));
    biff_record(0x01AE, &body)
}

// ---- VBA --------------------------------------------------------------------------------------

/// A `vbaProject.bin` whose `dir` lists `modules` modules, all of which name the same stream.
pub(crate) fn vba_project_with_shared_stream(modules: usize, source: &[u8]) -> Vec<u8> {
    use crate::ole::vba::tests::compress_literal;
    let mut dir = Vec::new();
    rec(&mut dir, 0x0001, &1u32.to_le_bytes());
    rec(&mut dir, 0x0003, &1252u16.to_le_bytes());
    rec(&mut dir, 0x0004, b"VBAProject");
    for i in 0..modules {
        rec(&mut dir, 0x0019, format!("M{i}").as_bytes());
        rec(&mut dir, 0x001A, b"Shared");
        rec(&mut dir, 0x0031, &0u32.to_le_bytes());
        rec(&mut dir, 0x0021, b"");
        rec(&mut dir, 0x002B, b"");
    }
    rec(&mut dir, 0x0010, b"");
    let mut b = CfbBuilder::new();
    b.add_stream("VBA/dir", compress_literal(&dir)).unwrap();
    b.add_stream("VBA/Shared", compress_literal(source)).unwrap();
    b.add_stream("PROJECT", b"ID=\"{00000000-0000-0000-0000-000000000000}\"\r\n".to_vec()).unwrap();
    b.finish().unwrap()
}

// ---- Outlook message ---------------------------------------------------------------------------

/// An Outlook `.msg` with one attachment called `name` holding `data`.
pub(crate) fn msg_with_attachment(name: &str, data: &[u8], html_body: Option<&str>) -> Vec<u8> {
    let mut b = CfbBuilder::new();
    b.add_stream("__properties_version1.0", vec![0u8; 32]).unwrap();
    b.add_storage("__attach_version1.0_#00000000", [0u8; 16]).unwrap();
    b.add_stream("__attach_version1.0_#00000000/__substg1.0_37010102", data.to_vec()).unwrap();
    let utf16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    b.add_stream("__attach_version1.0_#00000000/__substg1.0_3707001F", utf16).unwrap();
    if let Some(html) = html_body {
        b.add_stream("__substg1.0_10130102", html.as_bytes().to_vec()).unwrap();
    }
    b.finish().unwrap()
}
