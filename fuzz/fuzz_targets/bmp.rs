// SPDX-License-Identifier: AGPL-3.0-only
//! The BMP decoder (bit depths, RLE, bitfields, palettes, top-down and bottom-up) behind the image invariants of common/image.rs.
#![no_main]

#[path = "common/image.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    common::check(&common::with_magic(b"BM", data));
});
