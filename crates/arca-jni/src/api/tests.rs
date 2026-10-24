// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Tests of the safe API layer. They need the guest module embedded at build time
//! (`ARCA_GUEST_WASM`); CI sets it together with `ARCA_REQUIRE_E2E=1`.

use std::io::Cursor;

use super::*;

fn ready() -> bool {
    if !has_guest() {
        assert!(
            std::env::var_os("ARCA_REQUIRE_E2E").is_none_or(|v| v.is_empty() || v == "0"),
            "ARCA_REQUIRE_E2E is set but the library was built without ARCA_GUEST_WASM"
        );
        #[allow(clippy::print_stderr)] // a skipped test says so
        {
            eprintln!("skipped: built without ARCA_GUEST_WASM");
        }
        return false;
    }
    assert_eq!(init(0, 0, 0, 0), 0);
    true
}

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("bytes")).expect("utf-8")
}

#[test]
fn without_a_guest_the_library_refuses_to_start() {
    if has_guest() {
        return;
    }
    assert_eq!(init(0, 0, 0, 0), Status::NotReady.code());
    assert_eq!(doc_new(10), -i64::from(Status::NotReady.code()));
    assert!(info().is_none());
}

#[test]
fn the_embedded_guest_matches_the_digest_recorded_at_build_time() {
    if !has_guest() {
        return;
    }
    assert_eq!(arca_engine::guest_digest_of(guest::GUEST_WASM), GUEST_SHA256);
    assert_eq!(GUEST_SHA256.len(), 64);
    assert!(text(info()).contains(GUEST_SHA256));
}

#[test]
fn full_flow_through_the_api() {
    if !ready() {
        return;
    }
    let doc = doc_new(1 << 20);
    assert!(doc > 0);
    let sample = selftest::sample_document();
    // In two chunks, as the Java side may deliver it.
    let (a, b) = sample.split_at(sample.len() / 2);
    assert_eq!(doc_append(doc, a), 0);
    assert_eq!(doc_append(doc, b), 0);
    assert_eq!(doc_len(doc), i64::try_from(sample.len()).unwrap());

    let job = job_new();
    assert!(job > 0);
    let scan = run(job, doc, 1, 0);
    assert!(scan > 0, "{scan}");
    assert_eq!(outcome_status(scan), 0);
    let report = text(report(scan));
    assert!(report.contains("\"verdict\":\"HiddenTrackers\""), "{report}");
    assert!(report.contains("ExternalImage"));
    assert!(text(outcome_info(scan)).contains("\"statusName\":\"Ok\""));

    let preview = run(job, doc, 3, 1);
    assert_eq!(outcome_status(preview), 0);
    assert!(text(preview_meta(preview)).contains("\"sections\""));
    let items = text(preview_items(preview, 0, 0, 10));
    assert!(items.contains("Arca self-test") && items.contains("hidden line"));
    assert!(preview_items(preview, -1, 0, 10).is_none());
    assert!(preview_items(preview, 0, -5, 10).is_none());
    assert!(preview_items(preview, 99, 0, 10).is_none());
    assert!(report_of_preview(preview).is_none(), "a preview is not a report");

    let sanitize = run(job, doc, 2, 15);
    assert_eq!(outcome_status(sanitize), 0);
    assert!(text(sanitized_info(sanitize)).contains("\"verdict\":\"Clean\""));
    let mut sterile = Vec::new();
    let n = sanitized_write(sanitize, &mut sterile);
    assert_eq!(n, i64::try_from(sterile.len()).unwrap());
    assert!(sterile.starts_with(b"PK"));

    for o in [scan, preview, sanitize] {
        outcome_free(o);
        assert_eq!(outcome_status(o), -Status::BadHandle.code(), "freed handle");
    }
    job_free(job);
    doc_free(doc);
    assert_eq!(doc_len(doc), -i64::from(Status::BadHandle.code()));
}

fn report_of_preview(preview: i64) -> Option<Vec<u8>> {
    report(preview)
}

#[test]
fn invalid_arguments_are_statuses() {
    if !ready() {
        return;
    }
    let bad = |s: Status| -i64::from(s.code());
    assert_eq!(doc_new(-1), bad(Status::BadArgument));
    assert_eq!(doc_len(0), bad(Status::BadHandle));
    assert_eq!(doc_len(-4), bad(Status::BadHandle));
    assert_eq!(doc_append(123_456_789, b"x"), Status::BadHandle.code());
    let doc = doc_new(8);
    let job = job_new();
    assert_eq!(run(job, doc, 99, 0), bad(Status::BadArgument), "unknown operation");
    assert_eq!(run(job, doc, -1, 0), bad(Status::BadArgument));
    assert_eq!(run(job, doc, 1, -1), bad(Status::BadArgument));
    assert_eq!(run(job, doc, 1, 1), bad(Status::BadArgument), "a scan takes no flags");
    assert_eq!(run(0, doc, 1, 0), bad(Status::BadHandle));
    assert_eq!(doc_append(doc, &[0u8; 9]), Status::TooLarge.code());
    job_free(job);
    doc_free(doc);
}

#[test]
fn a_file_descriptor_style_reader_fills_the_document_and_a_writer_receives_the_copy() {
    if !ready() {
        return;
    }
    let sample = selftest::sample_document();
    let doc = doc_new(1 << 20);
    assert_eq!(
        doc_read(doc, &mut Cursor::new(sample.clone())),
        i64::try_from(sample.len()).unwrap()
    );
    let job = job_new();
    let o = run(job, doc, 2, 15);
    assert_eq!(outcome_status(o), 0);
    let mut out = Vec::new();
    assert!(sanitized_write(o, &mut out) > 0);
    // A failing sink is an I/O status, not a crash.
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(sanitized_write(o, &mut Broken), -i64::from(Status::Io.code()));
    struct BrokenRead;
    impl Read for BrokenRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("gone"))
        }
    }
    let d2 = doc_new(100);
    assert_eq!(doc_read(d2, &mut BrokenRead), -i64::from(Status::Io.code()));
    outcome_free(o);
    job_free(job);
    doc_free(doc);
    doc_free(d2);
}

#[test]
fn cancel_stops_a_run_that_has_not_started_yet() {
    if !ready() {
        return;
    }
    let doc = doc_new(1 << 20);
    assert_eq!(doc_append(doc, &selftest::sample_document()), 0);
    let job = job_new();
    job_cancel(job);
    let o = run(job, doc, 1, 0);
    assert_eq!(outcome_status(o), Status::Cancelled.code());
    outcome_free(o);
    job_free(job);
    doc_free(doc);
}

#[test]
fn self_test_and_static_views() {
    if !ready() {
        return;
    }
    let report = text(self_test());
    assert!(report.starts_with("{\"ok\":true"), "{report}");
    assert!(text(Some(about(0))).contains("Создатели оригинального ядра Arca"));
    assert!(text(Some(about(1))).contains("Creators of the original Arca core"));
    assert!(text(Some(about(-5))).contains("\"cards\""), "unknown language falls back to English");
    let rec = passport_update(&[], 20_000, 3, 1);
    assert_eq!(rec.len(), 36);
    assert!(text(Some(passport(1, &rec, 20_002))).contains("\"documents\":1"));
    assert!(license_text(0).is_some());
    assert!(license_text(7).is_none());
    assert!(license_text(-1).is_none());
    assert!(text(Some(release_info())).contains("\"blockers\""));
}

#[test]
fn megabyte_figures_from_the_device_never_overflow() {
    assert_eq!(mib_to_bytes(0, 7), 7);
    assert_eq!(mib_to_bytes(-3, 7), 7);
    assert_eq!(mib_to_bytes(1, 7), 1 << 20);
    // The largest figure an `i32` can carry is 2 GiB worth of MiB: 2^31 << 20 does not fit
    // a 32-bit `usize`. It saturates (the engine clamps to its own ceiling), it never wraps.
    let huge = mib_to_bytes(i32::MAX, 7);
    assert!(huge >= usize::try_from(u32::MAX).unwrap_or(usize::MAX).min(1 << 31));
}

#[test]
fn a_module_that_is_not_the_recorded_one_is_refused() {
    use arca_engine::{Engine, EngineConfig};
    let wasm = vec![0u8, 1, 2, 3];
    let digest = arca_engine::guest_digest_of(&wasm);
    assert!(Engine::pinned(wasm.clone(), &digest, EngineConfig::default()).is_ok());
    let other = arca_engine::guest_digest_of(b"another module");
    assert_eq!(Engine::pinned(wasm, &other, EngineConfig::default()).unwrap_err(), Status::Fault);
}
