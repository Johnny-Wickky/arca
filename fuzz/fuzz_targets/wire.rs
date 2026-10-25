// SPDX-License-Identifier: AGPL-3.0-only
//! The host decodes what the (untrusted) guest returns: every byte string must
//! decode or fail cleanly, and whatever decodes must re-encode to something
//! that decodes to the same value. The decoder's allocation budget is exercised too: whatever
//! decodes under a small budget decodes to the same value under every larger one, and whatever
//! decodes under tight limits decodes to the same value under the default ones.
#![no_main]

use arca_abi::envelope::{PayloadKind, open};
use arca_abi::wire::{self, DecodeLimits};
use arca_image::Image;
use arca_preview::Preview;
use arca_preview::page::PageModel;
use arca_xray::XRayReport;
use arca_xray::sanitize::SanitizedDocument;
use libfuzzer_sys::fuzz_target;

fn roundtrip<T: wire::Wire + PartialEq + core::fmt::Debug>(payload: &[u8]) {
    let limits = DecodeLimits::default();
    if let Ok(value) = wire::from_bytes::<T>(payload, limits) {
        let bytes = wire::to_bytes(&value).expect("a decoded value re-encodes");
        let back: T = wire::from_bytes(&bytes, limits).expect("re-encoded bytes decode");
        assert_eq!(back, value);
    }
}

fn decode_with<T: wire::Wire>(
    payload: &[u8],
    limits: DecodeLimits,
    budget: usize,
) -> Result<T, wire::WireError> {
    let mut r = wire::Reader::new(payload, limits).with_total_budget(budget);
    let v = T::decode(&mut r)?;
    r.finish()?;
    Ok(v)
}

/// Decoding with a budget: monotonic in the budget and in the limits, and never a panic.
fn budgeted<T: wire::Wire + PartialEq + core::fmt::Debug>(payload: &[u8]) {
    let full = DecodeLimits::default();
    let mut first_ok: Option<T> = None;
    for budget in [64, 1 << 10, 1 << 16, 1 << 22, wire::DEFAULT_TOTAL_BYTES] {
        let r = decode_with::<T>(payload, full, budget);
        if let Some(smaller) = &first_ok {
            // A larger budget cannot refuse what a smaller one accepted, nor change the value.
            assert_eq!(r.as_ref().ok(), Some(smaller));
        } else if let Ok(v) = r {
            first_ok = Some(v);
        }
    }
    let tight = DecodeLimits { max_string_bytes: 256, max_items: 16, max_depth: 4 };
    if let Ok(v) = decode_with::<T>(payload, tight, wire::DEFAULT_TOTAL_BYTES) {
        assert_eq!(decode_with::<T>(payload, full, wire::DEFAULT_TOTAL_BYTES).ok(), Some(v));
    }
}

fn both<T: wire::Wire + PartialEq + core::fmt::Debug>(payload: &[u8]) {
    roundtrip::<T>(payload);
    budgeted::<T>(payload);
}

fuzz_target!(|data: &[u8]| {
    // Straight into the typed decoders ...
    both::<XRayReport>(data);
    both::<SanitizedDocument>(data);
    both::<Preview>(data);
    both::<PageModel>(data);
    both::<Image>(data);
    both::<String>(data);
    // ... and through the envelope, as the host does.
    if let Ok((kind, payload)) = open(data) {
        match kind {
            PayloadKind::XRayReport => both::<XRayReport>(payload),
            PayloadKind::SanitizedDocument => both::<SanitizedDocument>(payload),
            PayloadKind::Preview => both::<Preview>(payload),
            PayloadKind::Page => both::<PageModel>(payload),
            PayloadKind::Image => both::<Image>(payload),
            PayloadKind::Error => both::<String>(payload),
        }
    }
});
