// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Unit tests of the safe API layer.

use arca_engine::Status;

use crate::api::*;

#[test]
fn uninitialized_state_returns_not_ready() {
    if !is_init() && !has_guest() {
        assert_eq!(init(0, 0, 0, 0), Status::NotReady.code());
        assert_eq!(doc_new(10), -i64::from(Status::NotReady.code()));
        assert!(info().is_none());
    }
}

#[test]
fn invalid_handles_produce_status_bad_handle() {
    // An initialised host (of this thread) answers for the handle, not for readiness.
    test_host::install();
    assert_eq!(doc_len(9999), -i64::from(Status::BadHandle.code()));
    assert_eq!(outcome_status(9999), -Status::BadHandle.code());
    assert_eq!(report(9999), Err(Status::BadHandle));
    assert_eq!(preview_meta(9999), Err(Status::BadHandle));
    assert_eq!(sanitized_bytes(9999), Err(Status::BadHandle));
}

#[test]
fn about_and_attribution_screens_produce_valid_json() {
    let json_ru = String::from_utf8(about(0)).expect("valid utf-8");
    assert!(json_ru.contains("\"appName\""));
    assert!(json_ru.contains("\"fingerprint\""));
    assert!(json_ru.contains("\"cards\""));

    let json_en = String::from_utf8(about(1)).expect("valid utf-8");
    assert!(json_en.contains("\"appName\""));

    let lic = String::from_utf8(license_text(0).expect("agpl text")).expect("valid utf-8");
    assert!(lic.contains("GNU AFFERO GENERAL PUBLIC LICENSE"));
}

#[test]
fn passport_flow() {
    // Fresh record
    let json = String::from_utf8(passport(0, &[], 100)).expect("valid utf-8");
    assert!(json.contains("\"title\""));
    assert!(json.contains("\"documents\":0"));

    // Update record
    let updated_record = passport_update(&[], 100, 2, 1);
    assert_eq!(updated_record.len(), 36);

    // Feed updated record
    let json_after = String::from_utf8(passport(0, &updated_record, 100)).expect("valid utf-8");
    assert!(json_after.contains("\"documents\":1"));
    assert!(json_after.contains("\"trackers\":2"));
    assert!(json_after.contains("\"active\":1"));
}

#[test]
fn document_and_job_lifecycle() {
    test_host::install();
    let doc = doc_new(1024 * 1024);
    assert!(doc > 0);
    let chunk = b"Hello, Arca!";
    assert_eq!(doc_append(doc, chunk), Status::Ok.code());
    assert_eq!(doc_len(doc), i64::try_from(chunk.len()).unwrap_or(-1));

    let job = job_new();
    assert!(job > 0);
    job_cancel(job);
    job_free(job);

    doc_free(doc);
    assert_eq!(doc_len(doc), -i64::from(Status::BadHandle.code()));
}

#[test]
fn non_positive_handles_are_bad_handles() {
    test_host::install();
    assert_eq!(doc_len(0), -i64::from(Status::BadHandle.code()));
    assert_eq!(doc_len(-5), -i64::from(Status::BadHandle.code()));
    assert_eq!(doc_append(i64::MIN, b"x"), Status::BadHandle.code());
}

#[test]
fn statistics_and_reminder_rules_work_without_a_guest() {
    // Feed three outcomes, read the totals, fold the old counters in, drop the blob.
    let mut blob = stats_record_scan(&[], 20_000, 0, 0, 0);
    blob = stats_record_scan(&blob, 20_000, 2, 1, 4);
    blob = stats_record_event(&blob, 20_001, 1);
    let json = String::from_utf8(stats_summary(&blob, 20_001)).expect("utf-8");
    assert!(json.contains("\"week\":{\"opened\":2,\"clean\":1,\"trackers\":1,\"active\":4"), "{json}");
    assert!(json.contains("\"sterile\":1"), "{json}");
    // Hostile blobs and arguments are empty statistics, never a failure.
    let junk = stats_summary(&[0xff; 300], -1);
    assert!(String::from_utf8(junk).expect("utf-8").contains("\"empty\":true"));
    let folded = stats_fold_legacy(&[], &[], 5, 2, 20_000);
    assert!(String::from_utf8(stats_summary(&folded, 20_000)).expect("utf-8").contains("\"all\":{\"opened\":5"));
    let pass = String::from_utf8(stats_passport(1, &blob, 20_001)).expect("utf-8");
    assert!(pass.contains("\"documents\":2"), "{pass}");

    // Reminder rules.
    let now = 1_772_323_200_000_i64;
    let t = reminder_plan(now, 3_600_000, 7);
    assert!(t > now + 6 * 86_400_000);
    assert_eq!(reminder_refine(t, 3_600_000, 3_600_000), t);
    let fire = String::from_utf8(reminder_on_fire(now + 13 * 3_600_000, 0, 0, 1, 1, 0)).expect("utf-8");
    assert!(fire.starts_with("{\"action\":\"show\""), "{fire}");
    let start = String::from_utf8(reminder_on_start(0, now, 0, 1, 0)).expect("utf-8");
    assert!(start.contains("\"plan\""), "{start}");
    assert_eq!(reminder_item_count(), 38);
    assert_eq!(reminder_pick(0, 0, 0) >> 8, 0);
}
