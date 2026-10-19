// SPDX-License-Identifier: AGPL-3.0-only
//! OLE compound file parsing and stream reads.
#![no_main]

use arca_container::cfb::{CfbLimits, CompoundFile};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(file) = CompoundFile::parse(data, CfbLimits::default()) else { return };
    for (index, _) in file.streams().take(64) {
        let _ = file.read_stream(index);
    }
});
