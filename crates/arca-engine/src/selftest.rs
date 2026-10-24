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

/// What the self-test holds in the host, released on every path out of a step (an early `?`
/// included): a failing step must not leave documents, jobs or results behind.
enum Held {
    Doc,
    Job,
    Outcome,
}

struct Guard<'a> {
    host: &'a Host,
    handle: Handle,
    what: Held,
}

impl<'a> Guard<'a> {
    const fn new(host: &'a Host, handle: Handle, what: Held) -> Self {
        Self { host, handle, what }
    }

    const fn id(&self) -> Handle {
        self.handle
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        match self.what {
            Held::Doc => self.host.doc_free(self.handle),
            Held::Job => self.host.job_free(self.handle),
            Held::Outcome => self.host.outcome_free(self.handle),
        };
    }
}

fn load<'a>(host: &'a Host, bytes: &[u8]) -> Result<Guard<'a>, Status> {
    let doc =
        Guard::new(host, host.doc_new(u64::try_from(bytes.len()).unwrap_or(u64::MAX)), Held::Doc);
    host.doc_append(doc.id(), bytes)?;
    Ok(doc)
}

/// Runs `op` and returns the result if it finished with [`Status::Ok`].
fn run_ok<'a>(
    host: &'a Host,
    job: &Guard<'_>,
    doc: &Guard<'_>,
    op: u16,
    flags: u32,
) -> Result<Guard<'a>, String> {
    let outcome =
        Guard::new(host, host.run(job.id(), doc.id(), op, flags).map_err(describe)?, Held::Outcome);
    match host.outcome_status(outcome.id()).map_err(describe)? {
        Status::Ok => Ok(outcome),
        other => Err(describe(other)),
    }
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
        let job = Guard::new(host, host.job_new(), Held::Job);
        steps.push(scan(host, &job, &doc));
        steps.push(preview(host, &job, &doc));
        steps.push(sanitize(host, &job, &doc));
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

fn scan(host: &Host, job: &Guard<'_>, doc: &Guard<'_>) -> Step {
    timed("scan", || {
        let outcome = run_ok(host, job, doc, op::SCAN, 0)?;
        let json = host.report_json(outcome.id()).map_err(describe)?;
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

fn preview(host: &Host, job: &Guard<'_>, doc: &Guard<'_>) -> Step {
    timed("preview", || {
        let outcome = run_ok(host, job, doc, op::PREVIEW, flags::REVEAL_HIDDEN)?;
        let items = host.preview_items_json(outcome.id(), 0, 0, 16).map_err(describe)?;
        let text = String::from_utf8_lossy(&items);
        if text.contains("Arca self-test") && text.contains("hidden line") {
            Ok("visible and hidden text extracted".into())
        } else {
            Err("expected text missing".into())
        }
    })
}

fn sanitize(host: &Host, job: &Guard<'_>, doc: &Guard<'_>) -> Step {
    timed("sanitize", || {
        let outcome = run_ok(host, job, doc, op::SANITIZE, flags::SANITIZE_ALL)?;
        let mut sterile = Vec::new();
        host.sanitized_write_to(outcome.id(), &mut sterile).map_err(describe)?;
        drop(outcome);
        // Re-scan the sterile copy independently of the sanitizer's own verification.
        let again = load(host, &sterile).map_err(describe)?;
        let job2 = Guard::new(host, host.job_new(), Held::Job);
        let rescan = run_ok(host, &job2, &again, op::SCAN, 0)?;
        let json = host.report_json(rescan.id()).map_err(describe)?;
        let text = String::from_utf8_lossy(&json);
        if text.contains(&format!("\"verdict\":\"{:?}\"", Verdict::Clean)) {
            Ok(format!("sterile copy of {} bytes verified clean", sterile.len()))
        } else {
            Err("the sterile copy is not clean".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, EngineConfig};

    #[test]
    fn a_step_that_leaves_early_still_releases_what_it_held() {
        let host = Host::new(Engine::new(vec![0u8; 8], EngineConfig::default()));
        // `?` out of the middle of a step: the guards go with the stack frame.
        let leaving = |host: &Host| -> Result<(), Status> {
            let doc = load(host, b"abc")?;
            let _job = Guard::new(host, host.job_new(), Held::Job);
            // A handle that does not exist: the call fails, the step returns.
            host.doc_append(doc.id() + 1000, b"x")?;
            Ok(())
        };
        assert_eq!(leaving(&host), Err(Status::BadHandle));
        assert_eq!(host.live_objects(), (0, 0, 0));
    }

    #[test]
    fn the_self_test_leaves_nothing_behind_even_when_every_step_fails() {
        // Over a module that is not a guest every run ends in a fault: the steps fail.
        let host = Host::new(Engine::new(vec![0u8; 8], EngineConfig::default()));
        let report = String::from_utf8(run(&host)).unwrap();
        assert!(report.contains("\"ok\":false"), "{report}");
        assert_eq!(host.live_objects(), (0, 0, 0));
    }
}
