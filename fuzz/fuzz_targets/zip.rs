// SPDX-License-Identifier: AGPL-3.0-only
//! ZIP parsing and bounded decompression.
#![no_main]

use arca_container::InflateBudget;
use arca_container::zip::{ZipArchive, ZipLimits};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = ZipLimits { max_entry_size: 16 << 20, ..ZipLimits::default() };
    let Ok(archive) = ZipArchive::parse(data, limits) else { return };
    let mut budget = InflateBudget::new(32 << 20, 100, 1 << 20);
    for entry in archive.entries().iter().take(64) {
        let _ = archive.read(entry, &mut budget);
    }
});
