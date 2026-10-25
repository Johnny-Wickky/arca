// SPDX-License-Identifier: AGPL-3.0-only
//! PDF, the format with the most hostile structure: the scanner (`arca-xray`), the reading view
//! and page renderer (`arca-preview`) and the sterile-copy writer. The input gets a `%PDF-`
//! header when it lacks one, so the fuzzer works on objects, streams, xref tables and filters
//! and not on rediscovering the signature. Hostile input must give an error or a bounded result
//! — never a panic, a hang or unbounded memory — and the results must be deterministic.
#![no_main]

use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};
use arca_preview::{PdfSanitizeOptions, Preview, PreviewPolicy, preview, render_page, sanitize_pdf};
use arca_xray::{ScanPolicy, Verdict, scan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut pdf = Vec::with_capacity(data.len() + 8);
    if !data.starts_with(b"%PDF-") {
        pdf.extend_from_slice(b"%PDF-1.7\n");
    }
    pdf.extend_from_slice(data);

    // The scanner: the verdict follows from the findings, and the answer is deterministic.
    let policy = ScanPolicy { max_input_bytes: 8 << 20, ..ScanPolicy::default() };
    let report = scan(&pdf, &policy);
    assert_eq!(report.verdict == Verdict::Rejected, !report.rejection.is_empty());
    if report.findings.iter().any(|f| f.kind.limits_coverage_at(f.severity)) {
        assert_ne!(report.verdict, Verdict::Clean);
    }
    assert_eq!(scan(&pdf, &policy), report);

    // The reading view: deterministic, and it survives the wire.
    let pp = PreviewPolicy { max_input_bytes: 8 << 20, ..PreviewPolicy::default() };
    let first = preview(&pdf, &pp);
    if let Ok(p) = &first {
        assert_eq!(preview(&pdf, &pp).as_ref(), Ok(p));
        let bytes = to_bytes(p).expect("a preview encodes");
        let back: Preview = from_bytes(&bytes, DecodeLimits::default()).expect("and decodes");
        assert_eq!(&back, p);
    }
    // The page renderer, for the first pages.
    for index in 0..3 {
        let a = render_page(&pdf, index, &pp);
        assert_eq!(render_page(&pdf, index, &pp), a);
    }

    // A sterile copy that is produced must itself be readable.
    if let Ok((copy, _)) = sanitize_pdf(&pdf, &PdfSanitizeOptions::default()) {
        assert!(preview(&copy, &pp).is_ok(), "the PDF copy cannot be read back");
    }
});
