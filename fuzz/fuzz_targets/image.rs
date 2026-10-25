// SPDX-License-Identifier: AGPL-3.0-only
//! The image entry point as the engine calls it: any bytes, format found by sniffing. Never a
//! panic, a hang or unbounded memory; see common/image.rs for the invariants.
#![no_main]

#[path = "common/image.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    common::check(data);
});
