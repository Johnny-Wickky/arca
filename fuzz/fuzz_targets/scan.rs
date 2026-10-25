// SPDX-License-Identifier: AGPL-3.0-only
//! The scanner must never panic, hang or allocate without bound, and its
//! verdict must follow from its findings.
#![no_main]

use arca_xray::{ScanPolicy, Severity, Verdict, report::Category, scan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A small policy keeps each execution fast and bounds libFuzzer's memory;
    // the limits are the same *kinds* as the production ones.
    let policy = ScanPolicy { max_input_bytes: 8 << 20, ..ScanPolicy::default() };
    let r = scan(data, &policy);

    assert_eq!(r.verdict == Verdict::Rejected, !r.rejection.is_empty());
    assert_eq!(r.sha256.len(), 64);
    let reaching = r.findings.iter().any(|f| {
        matches!(f.kind.category(), Category::ActiveContent | Category::RemoteContent)
            && f.severity >= Severity::Medium
    });
    let gap = r.findings.iter().any(|f| f.kind.limits_coverage_at(f.severity));
    if reaching || gap {
        assert_ne!(r.verdict, Verdict::Clean);
    }
    // Deterministic.
    assert_eq!(scan(data, &policy), r);
});
