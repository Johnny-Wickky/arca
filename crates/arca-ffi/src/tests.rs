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
    let doc = doc_new(1024 * 1024);
    if is_init() {
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
    } else {
        assert_eq!(doc, -i64::from(Status::NotReady.code()));
    }
}
