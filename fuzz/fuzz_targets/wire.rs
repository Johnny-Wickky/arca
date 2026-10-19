// SPDX-License-Identifier: AGPL-3.0-only
//! The host decodes what the (untrusted) guest returns: every byte string must
//! decode or fail cleanly, and whatever decodes must re-encode to something
//! that decodes to the same value.
#![no_main]

use arca_abi::envelope::{PayloadKind, open};
use arca_abi::wire::{self, DecodeLimits};
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

fuzz_target!(|data: &[u8]| {
    // Straight into the typed decoders ...
    roundtrip::<XRayReport>(data);
    roundtrip::<SanitizedDocument>(data);
    roundtrip::<String>(data);
    // ... and through the envelope, as the host does.
    if let Ok((kind, payload)) = open(data) {
        match kind {
            PayloadKind::XRayReport => roundtrip::<XRayReport>(payload),
            PayloadKind::SanitizedDocument => roundtrip::<SanitizedDocument>(payload),
            PayloadKind::Error => roundtrip::<String>(payload),
        }
    }
});
