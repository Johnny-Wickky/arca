// SPDX-License-Identifier: AGPL-3.0-only
//! A sterile copy must always be verified sterile, and sterilising twice must
//! change nothing.
#![no_main]

use arca_xray::sanitize::{SanitizeOptions, sanitize};
use arca_xray::{ScanPolicy, Verdict, scan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let policy = ScanPolicy { max_input_bytes: 8 << 20, ..ScanPolicy::default() };
    let options = SanitizeOptions::default();
    let Ok(doc) = sanitize(data, &policy, &options) else { return };

    assert_eq!(doc.verification, scan(&doc.bytes, &policy));
    assert!(matches!(doc.verification.verdict, Verdict::Clean | Verdict::HiddenTrackers));
    let again = sanitize(&doc.bytes, &policy, &options).expect("a sterile copy sterilises again");
    assert_eq!(again.bytes, doc.bytes);
});
