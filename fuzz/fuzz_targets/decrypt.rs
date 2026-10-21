// SPDX-License-Identifier: AGPL-3.0-only
//! Decryption of password-protected documents (MS-OFFCRYPTO standard and agile, PDF security
//! handler R2–R6). The encryption data is attacker-controlled: lengths, offsets, spin counts and
//! key sizes must be checked, and whatever is produced must be readable by the normal pipeline.
#![no_main]

use arca_preview::{PreviewPolicy, decrypt, is_encrypted_package, preview};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = is_encrypted_package(data);
    // The empty password (tried automatically for PDFs) and the one the fixtures use.
    for password in [&b""[..], "пароль-123".as_bytes()] {
        if let Ok(d) = decrypt(data, password) {
            let policy = PreviewPolicy { max_input_bytes: 8 << 20, ..PreviewPolicy::default() };
            let _ = preview(&d.bytes, &policy);
        }
    }
});
