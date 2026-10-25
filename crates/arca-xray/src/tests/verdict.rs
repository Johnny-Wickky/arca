//! Verdict integrity: the verdict must never be weaker than what the scanner
//! actually saw, however the file tries to exhaust the report or hide parts.

use arca_abi::wire::{self, DecodeLimits};

use super::corpus::mark_member_encrypted;
use super::fixtures::*;
use crate::report::{
    ExternalReference, RefKind, ReportBuilder, Severity, TargetClass, ThreatKind, Verdict,
};
use crate::{ScanPolicy, scan, scan_default};

/// `n` distinct external hyperlinks (Info findings) in a relationships part.
fn hyperlink_filler(n: usize) -> Vec<(String, String)> {
    (0..n).map(|i| (format!("rIdF{i}"), format!("https://filler.example/{i}"))).collect()
}

fn rels_with_filler(
    filler: &[(String, String)],
    extra: &[(&'static str, &'static str, &'static str, bool)],
) -> String {
    let mut items: Vec<(&str, &str, &str, bool)> =
        filler.iter().map(|(id, url)| (id.as_str(), "hyperlink", url.as_str(), true)).collect();
    items.extend_from_slice(extra);
    rels(&items)
}

// ---- A-1: finding-limit saturation --------------------------------------

#[test]
fn a1_flood_of_hyperlinks_cannot_hide_dde_in_the_body() {
    let mut o = docx();
    o.set("word/_rels/document.xml.rels", rels_with_filler(&hyperlink_filler(6000), &[]))
        .set("word/document.xml", document(&dde_body()));
    let r = scan_default(&o.build());
    assert_eq!(r.verdict, Verdict::ActiveContentNeutralized, "{:?}", r.max_severity());
    assert!(r.has(ThreatKind::DdeField), "the decisive finding must be listed");
    assert!(r.has(ThreatKind::ScanLimitReached), "truncation notice must never be dropped");
    assert!(r.findings.len() < 5_100, "storage stays bounded: {}", r.findings.len());
}

#[test]
fn a1_flood_in_the_same_rels_part_cannot_hide_remote_template() {
    // The template relationship comes *after* the filler, in the same part.
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels_with_filler(
            &hyperlink_filler(40),
            &[("rIdT", "attachedTemplate", "https://attacker.example/t.dotm", true)],
        ),
    )
    .set(
        "word/document.xml",
        document(&format!(r#"{}<w:p><w:r><w:t>x</w:t></w:r></w:p>"#, para("a"))),
    );
    let policy = ScanPolicy { max_findings: 10, ..ScanPolicy::default() };
    let r = scan(&o.build(), &policy);
    assert!(
        matches!(r.verdict, Verdict::ActiveContentNeutralized | Verdict::HiddenTrackers),
        "{:?}",
        r.verdict
    );
    assert_ne!(r.verdict, Verdict::Clean);
    assert!(r.has(ThreatKind::ScanLimitReached));
}

#[test]
fn a1_first_finding_of_every_class_survives_the_cap() {
    let mut b = ReportBuilder::new(5);
    for i in 0..100 {
        b.add(ThreatKind::ExternalHyperlink, "p", format!("https://x.example/{i}"));
    }
    assert!(b.truncated);
    assert_eq!(b.dropped, 95);
    // The cap is full, yet a new kind is still stored, and so is a more severe
    // instance of an already-capped kind.
    b.add(ThreatKind::DdeField, "body", "DDEAUTO cmd");
    b.add_with(ThreatKind::ExternalHyperlink, Severity::Critical, "p", "https://evil.example/");
    assert!(b.has(ThreatKind::DdeField));
    let facts = b.facts();
    assert!(facts.active);
    let (findings, _) = b.into_parts();
    assert_eq!(findings.len(), 7, "5 stored + 2 first-of-class");
    assert_eq!(findings.first().map(|f| f.severity), Some(Severity::Critical));
}

#[test]
fn a1_dropped_findings_still_feed_the_verdict_facts() {
    let mut b = ReportBuilder::new(1);
    b.add(ThreatKind::Comments, "c", "one");
    // Same class as an existing finding, different detail, over the cap:
    // not stored, but counted.
    b.add(ThreatKind::Comments, "c", "two");
    b.add_with(ThreatKind::MacroButtonField, Severity::Medium, "m", "MACROBUTTON a");
    b.add_with(ThreatKind::MacroButtonField, Severity::Medium, "m", "MACROBUTTON b");
    let facts = b.facts();
    assert!(facts.active, "Medium active content counts even when its entry was dropped");
    assert_eq!(b.dropped, 2);
}

#[test]
fn a1_duplicate_observations_are_not_treated_as_truncation() {
    let mut b = ReportBuilder::new(1);
    b.add(ThreatKind::Comments, "c", "same");
    for _ in 0..100 {
        b.add(ThreatKind::Comments, "c", "same");
    }
    assert!(!b.truncated, "exact duplicates are de-duplicated, not dropped");
}

#[test]
fn a1_auto_load_reference_beyond_the_cap_still_marks_trackers() {
    let mut b = ReportBuilder::new(2);
    for i in 0..4 {
        b.external(ExternalReference {
            target: format!("https://a.example/{i}"),
            kind: RefKind::Hyperlink,
            class: TargetClass::Web,
            location: "p".into(),
            auto_load: false,
        });
    }
    b.external(ExternalReference {
        target: "https://tracker.example/p.gif".into(),
        kind: RefKind::Image,
        class: TargetClass::Web,
        location: "p".into(),
        auto_load: true,
    });
    assert_eq!(b.external_refs.len(), 2);
    assert!(b.facts().tracker);
}

// ---- A-3: "Clean" only if everything was inspected -----------------------

#[test]
fn a3_encrypted_zip_member_is_unverified_not_clean() {
    let mut o = docx();
    o.set("word/embeddings/blob.bin", vec![0x41u8; 64]);
    let mut raw = o.build();
    mark_member_encrypted(&mut raw, "word/embeddings/blob.bin");
    let r = scan_default(&raw);
    assert!(r.has(ThreatKind::EncryptedMember), "{:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::Unverified);
}

#[test]
fn a3_encrypted_pdf_is_unverified_not_clean() {
    let pdf = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n4 0 obj\n<< /Filter /Standard /V 4 /R 4 /O (x) /U (y) /P -1 >>\nendobj\ntrailer\n<< /Root 1 0 R /Encrypt 4 0 R /ID [<00><00>] >>\n%%EOF\n";
    let r = scan_default(pdf);
    assert!(r.has(ThreatKind::EncryptedDocument), "{:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::Unverified);
}

#[test]
fn a3_nesting_limit_reached_is_unverified() {
    let mut doc = docx().build();
    for _ in 0..5 {
        let mut o = docx();
        o.set("word/embeddings/inner.docx", doc);
        doc = o.build();
    }
    let r = scan_default(&doc);
    assert!(r.has(ThreatKind::ScanLimitReached));
    assert_eq!(r.verdict, Verdict::Unverified);
}

#[test]
fn a3_unparsable_part_is_unverified() {
    let mut o = docx();
    // Truncated XML: the parser gives up on this part, so it is not inspected.
    o.set("word/document.xml", "<w:document xmlns:w=\"x\"><w:body><w:p>");
    let r = scan_default(&o.build());
    assert!(r.has(ThreatKind::MalformedContainer), "{:#?}", r.findings);
    assert_eq!(r.verdict, Verdict::Unverified);
}

#[test]
fn a3_active_content_outranks_unverified_and_unverified_outranks_trackers() {
    // Active content + a coverage gap: the stronger statement wins.
    let mut o = docx();
    o.set("word/document.xml", document(&dde_body())).set("word/embeddings/blob.bin", vec![1u8; 8]);
    let mut raw = o.build();
    mark_member_encrypted(&mut raw, "word/embeddings/blob.bin");
    assert_eq!(scan_default(&raw).verdict, Verdict::ActiveContentNeutralized);

    // Tracker + coverage gap: the gap wins, because a tracker verdict would
    // imply the rest of the file was read.
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels(&[("rId9", "image", "https://t.example/p.gif", true)]),
    )
    .set("word/embeddings/blob.bin", vec![1u8; 8]);
    let mut raw = o.build();
    mark_member_encrypted(&mut raw, "word/embeddings/blob.bin");
    assert_eq!(scan_default(&raw).verdict, Verdict::Unverified);
}

#[test]
fn a3_unverified_survives_the_wire() {
    let mut o = docx();
    o.set("word/embeddings/blob.bin", vec![0x41u8; 64]);
    let mut raw = o.build();
    mark_member_encrypted(&mut raw, "word/embeddings/blob.bin");
    let r = scan_default(&raw);
    let bytes = wire::to_bytes(&r).expect("encodes");
    let back: crate::XRayReport =
        wire::from_bytes(&bytes, DecodeLimits::default()).expect("decodes");
    assert_eq!(back.verdict, Verdict::Unverified);
    assert_eq!(back, r);
}

#[test]
fn coverage_gap_kinds_are_exactly_the_unreadable_ones() {
    use ThreatKind as K;
    for kind in [
        K::MalformedContainer,
        K::UnusualCompression,
        K::ScanLimitReached,
        K::EncryptedDocument,
        K::EncryptedMember,
        // The scanner and another reader would see different documents.
        K::ParserDifferential,
        // Bytes the container does not account for, or a second format in the same bytes.
        K::PolyglotFile,
        K::UnreferencedArchiveData,
        K::OrphanedOleEntries,
        K::ObfuscatedName,
    ] {
        assert!(kind.limits_coverage(), "{kind:?}");
        assert!(kind.limits_coverage_at(Severity::Info), "{kind:?}");
    }
    // Informational notes about what *was* read must not demote a file.
    for kind in [K::UnsupportedFormat, K::EmbeddedFont, K::Comments] {
        assert!(!kind.limits_coverage(), "{kind:?}");
        assert!(!kind.limits_coverage_at(Severity::High), "{kind:?}");
    }
}

#[test]
fn trailing_bytes_are_a_gap_unless_they_are_known_to_be_padding() {
    use ThreatKind as K;
    assert!(!K::TrailingData.limits_coverage_at(Severity::Info));
    assert!(K::TrailingData.limits_coverage_at(Severity::Low));
    assert!(!K::ContentTypeMismatch.limits_coverage_at(Severity::Low));
    assert!(K::ContentTypeMismatch.limits_coverage_at(Severity::Medium));
}
