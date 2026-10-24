// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Embeds the WebAssembly guest into the library and records its SHA-256.
//!
//! `ARCA_GUEST_WASM` names the built `arca_guest_xray.wasm`. The module is compiled into
//! the library (`include_bytes!`), so the guest that runs is always the one the
//! library was built with, and the digest the app shows can be compared with the one in the
//! release notes. Without the variable the library builds with an empty module and refuses
//! to initialise: useful for `cargo check`, never for a release (CI sets the variable).

#![allow(clippy::print_stdout, reason = "the cargo build-script protocol is stdout")]

use std::path::PathBuf;

fn main() -> Result<(), String> {
    println!("cargo::rerun-if-env-changed=ARCA_GUEST_WASM");
    println!("cargo::rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let source = match std::env::var_os("ARCA_GUEST_WASM") {
        Some(path) if !path.is_empty() => {
            let path = std::fs::canonicalize(&path)
                .map_err(|e| format!("ARCA_GUEST_WASM={path:?}: {e}"))?;
            println!("cargo::rerun-if-changed={}", path.display());
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if bytes.get(..4) != Some(b"\0asm") {
                return Err(format!("{} is not a WebAssembly module", path.display()));
            }
            let digest = arca_digest::sha256(&bytes);
            format!(
                "/// The embedded guest module.\npub static GUEST_WASM: &[u8] = include_bytes!({path:?});\n\
                 /// Its SHA-256 (lowercase hex), computed by the build script.\npub const GUEST_SHA256: &str = \"{digest}\";\n"
            )
        }
        _ => "/// No guest module was embedded.\npub static GUEST_WASM: &[u8] = &[];\n\
              /// Empty: no guest module was embedded.\npub const GUEST_SHA256: &str = \"\";\n"
            .to_owned(),
    };
    std::fs::write(out.join("guest.rs"), source).map_err(|e| e.to_string())
}
