// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! A built-in self-test: a tiny document with a tracking pixel and hidden text goes
//! through the sandbox on the device itself — X-ray, preview and sterilisation — and
//! the results are checked against what they must be.
//!
//! It proves that the native library, the pinned guest module and the interpreter work
//! on *this* phone (ABI, 32/64-bit, memory limits), which no build-time test can. The
//! document is generated here; nothing is read from the device and nothing is fetched
//! (the tracker URL uses a reserved test domain and is only text).

use std::time::Instant;

use arca_abi::request::flags;
use arca_container::zip::writer::{Compression, ZipWriter};
use arca_xray::{ThreatKind, Verdict};

use crate::host::{Handle, Host, Status, op};
use crate::json::JsonWriter;

/// The self-test document: a Word file with a hidden paragraph and a tracking image.
#[must_use]
pub fn sample_document() -> Vec<u8> {
    let files: [(&str, &str); 4] = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Arca self-test</w:t></w:r></w:p><w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden line</w:t></w:r></w:p></w:body></w:document>"#,
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="https://tracker.invalid/pixel.gif" TargetMode="External"/></Relationships>"#,
        ),
    ];
    let mut w = ZipWriter::new();
    for (name, data) in files {
        if w.add(name, data.as_bytes(), Compression::Deflated).is_err() {
            return Vec::new();
        }
    }
    w.finish().unwrap_or_default()
}

struct Step {
    name: &'static str,
    ok: bool,
    millis: u64,
    detail: String,
}

fn load(host: &Host, bytes: &[u8]) -> Result<Handle, Status> {
    let doc = host.doc_new(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    host.doc_append(doc, bytes)?;
    Ok(doc)
}

/// Runs the self-test and returns its JSON report:
/// `{"ok":true,"steps":[{"name":"scan","ok":true,"ms":12,"detail":""},…]}`.
#[must_use]
pub fn run(host: &Host) -> Vec<u8> {
    let mut steps = Vec::new();
    let bytes = sample_document();
    steps.push(Step {
        name: "sample",
        ok: !bytes.is_empty(),
        millis: 0,
        detail: format!("{} bytes", bytes.len()),
    });
    if !bytes.is_empty()
        && let Ok(doc) = load(host, &bytes)
    {
        let job = host.job_new();
        steps.push(scan(host, job, doc));
        steps.push(preview(host, job, doc));
        steps.push(sanitize(host, job, doc));
        host.job_free(job);
        host.doc_free(doc);
    }
    let ok = steps.iter().all(|s| s.ok);
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_bool("ok", ok);
    w.field_str("guestSha256", &host.engine().guest_sha256());
    w.key("steps");
    w.begin_array();
    for s in &steps {
        w.begin_object();
        w.field_str("name", s.name);
        w.field_bool("ok", s.ok);
        w.field_u64("ms", s.millis);
        w.field_str("detail", &s.detail);
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

fn timed(name: &'static str, f: impl FnOnce() -> Result<String, String>) -> Step {
    let started = Instant::now();
    let result = f();
    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(detail) => Step { name, ok: true, millis, detail },
        Err(detail) => Step { name, ok: false, millis, detail },
    }
}

fn describe(status: Status) -> String {
    format!("status {status:?}")
}

fn scan(host: &Host, job: Handle, doc: Handle) -> Step {
    timed("scan", || {
        let outcome = host.run(job, doc, op::SCAN, 0).map_err(describe)?;
        let status = host.outcome_status(outcome).map_err(describe)?;
        if status != Status::Ok {
            host.outcome_free(outcome);
            return Err(describe(status));
        }
        let json = host.report_json(outcome).map_err(describe)?;
        host.outcome_free(outcome);
        let text = String::from_utf8_lossy(&json);
        let found = text.contains(ThreatKind::ExternalImage.id())
            && text.contains(&format!("\"verdict\":\"{:?}\"", Verdict::HiddenTrackers));
        if found {
            Ok("tracker found and blocked".into())
        } else {
            Err("tracker not reported".into())
        }
    })
}

fn preview(host: &Host, job: Handle, doc: Handle) -> Step {
    timed("preview", || {
        let outcome = host.run(job, doc, op::PREVIEW, flags::REVEAL_HIDDEN).map_err(describe)?;
        let status = host.outcome_status(outcome).map_err(describe)?;
        if status != Status::Ok {
            host.outcome_free(outcome);
            return Err(describe(status));
        }
        let items = host.preview_items_json(outcome, 0, 0, 16).map_err(describe)?;
        host.outcome_free(outcome);
        let text = String::from_utf8_lossy(&items);
        if text.contains("Arca self-test") && text.contains("hidden line") {
            Ok("visible and hidden text extracted".into())
        } else {
            Err("expected text missing".into())
        }
    })
}

fn sanitize(host: &Host, job: Handle, doc: Handle) -> Step {
    timed("sanitize", || {
        let outcome = host.run(job, doc, op::SANITIZE, flags::SANITIZE_ALL).map_err(describe)?;
        let status = host.outcome_status(outcome).map_err(describe)?;
        if status != Status::Ok {
            host.outcome_free(outcome);
            return Err(describe(status));
        }
        let mut sterile = Vec::new();
        host.sanitized_write_to(outcome, &mut sterile).map_err(describe)?;
        host.outcome_free(outcome);
        // Re-scan the sterile copy independently of the sanitizer's own verification.
        let again = load(host, &sterile).map_err(describe)?;
        let job2 = host.job_new();
        let rescan = host.run(job2, again, op::SCAN, 0).map_err(describe);
        host.job_free(job2);
        host.doc_free(again);
        let rescan = rescan?;
        let json = host.report_json(rescan).map_err(describe);
        host.outcome_free(rescan);
        let json = json?;
        let text = String::from_utf8_lossy(&json);
        if text.contains(&format!("\"verdict\":\"{:?}\"", Verdict::Clean)) {
            Ok(format!("sterile copy of {} bytes verified clean", sterile.len()))
        } else {
            Err("the sterile copy is not clean".into())
        }
    })
}
