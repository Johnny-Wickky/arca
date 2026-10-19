// SPDX-License-Identifier: AGPL-3.0-only
//! The secure XML reader: DOCTYPE and entity policy, depth and event limits,
//! UTF-16 transcoding.
#![no_main]

use arca_container::xml::{SecureXmlReader, XmlLimits, decode_xml_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = XmlLimits::default();
    let Ok(text) = decode_xml_bytes(data, &limits) else { return };
    let Ok(mut reader) = SecureXmlReader::new(&text, limits) else { return };
    while let Ok(Some(_)) = reader.next_item() {}
});
