// SPDX-License-Identifier: AGPL-3.0-only
//! The JPEG decoder (baseline, extended and progressive Huffman, restart intervals, CMYK/YCCK, APPn/COM segments) behind the image invariants of common/image.rs.
#![no_main]

#[path = "common/image.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    common::check(&common::with_magic(b"\xFF\xD8\xFF", data));
});
