// SPDX-License-Identifier: AGPL-3.0-only
//! The PNG decoder (chunks, filters, interlacing, palettes, APNG, ancillary metadata) behind the image invariants of common/image.rs.
#![no_main]

#[path = "common/image.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    common::check(&common::with_magic(b"\x89PNG\r\n\x1a\n", data));
});
