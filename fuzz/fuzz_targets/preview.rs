// SPDX-License-Identifier: AGPL-3.0-only
//! Every reading-view parser (DOCX/XLSX/PPTX, DOC/XLS/PPT, ODF, RTF, PDF, MSG, VSDX, text) behind
//! the format detector, and the two sterile-copy writers built on it. Hostile input must give an
//! error or a bounded preview — never a panic, a hang or unbounded memory — and the result must
//! be deterministic and survive the wire.
#![no_main]

use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};
use arca_preview::{LegacyOptions, PdfSanitizeOptions, Preview, PreviewPolicy, preview};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let policy = PreviewPolicy { max_input_bytes: 8 << 20, ..PreviewPolicy::default() };
    let first = preview(data, &policy);
    if let Ok(p) = &first {
        // Deterministic.
        assert_eq!(preview(data, &policy).as_ref(), Ok(p));
        // The host decodes this from an untrusted guest: it must round-trip.
        let bytes = to_bytes(p).expect("a preview encodes");
        let back: Preview = from_bytes(&bytes, DecodeLimits::default()).expect("and decodes");
        assert_eq!(&back, p);
    }

    // A sterile copy that is produced must itself be readable.
    if let Ok((copy, _, _)) = arca_preview::sanitize_legacy(data, &LegacyOptions::default()) {
        assert!(preview(&copy, &policy).is_ok(), "the legacy copy cannot be read back");
    }
    if let Ok((copy, _)) = arca_preview::sanitize_pdf(data, &PdfSanitizeOptions::default()) {
        assert!(preview(&copy, &policy).is_ok(), "the PDF copy cannot be read back");
    }
});
