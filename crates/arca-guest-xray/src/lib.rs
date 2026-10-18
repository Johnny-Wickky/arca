// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-guest-xray
//!
//! The Threat X-Ray packaged as a parser guest implementing
//! [`arca_abi`] version 1. Built for `wasm32-unknown-unknown` it is a module
//! with **zero imports**: it can compute, and nothing else.
//!
//! ```text
//! cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest
//! ```
//!
//! The guest owns two buffers: the input buffer handed out by
//! `arca_input_reserve` and the output envelope produced by `arca_run`.
//! Neither requires `unsafe` code: the guest only reports the *addresses* of
//! buffers it owns; the host reads and writes them through the interpreter's
//! bounds-checked memory API.
//!
//! On the host side, [`decode_report`] turns the sandbox output back into an
//! [`XRayReport`] under strict decoding limits (the payload is untrusted: a
//! compromised guest could have written anything).

#![deny(unsafe_code)]

use std::cell::RefCell;

use arca_abi::envelope::{self, PayloadKind};
use arca_abi::wire::{self, DecodeLimits, WireError};
use arca_abi::{ABI_VERSION, status};
use arca_xray::{ScanPolicy, XRayReport};

#[derive(Default)]
struct State {
    input: Vec<u8>,
    output: Vec<u8>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn addr_of(buf: &[u8]) -> i32 {
    // On wasm32 every address fits in 31 bits because the sandbox caps
    // linear memory far below 2 GiB; 0 signals failure otherwise.
    i32::try_from(buf.as_ptr() as usize).unwrap_or(0)
}

/// Encodes an error message envelope.
fn error_envelope(msg: &str) -> Vec<u8> {
    wire::to_bytes(&msg.to_owned())
        .and_then(|p| envelope::seal(PayloadKind::Error, &p))
        .unwrap_or_default()
}

/// Scans a document and returns `(status, envelope)`. Pure and testable.
#[must_use]
pub fn process(document: &[u8]) -> (i32, Vec<u8>) {
    let report = arca_xray::scan(document, &ScanPolicy::default());
    match wire::to_bytes(&report).and_then(|p| envelope::seal(PayloadKind::XRayReport, &p)) {
        Ok(env) => (status::OK, env),
        Err(_) => (status::ENCODE_FAILED, error_envelope("could not encode the X-ray report")),
    }
}

/// ABI: allocate the input buffer. Returns its address, or 0 on failure.
#[must_use]
pub fn input_reserve(len: i32) -> i32 {
    let Ok(len) = usize::try_from(len) else { return 0 };
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        let mut buf = Vec::new();
        if buf.try_reserve_exact(len).is_err() {
            return 0;
        }
        buf.resize(len, 0);
        st.input = buf;
        st.output.clear();
        if len == 0 { 1 } else { addr_of(&st.input) }
    })
}

/// ABI: process the reserved input. Returns a [`status`] code.
#[must_use]
pub fn run(len: i32) -> i32 {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        let Ok(len) = usize::try_from(len) else { return status::LENGTH_MISMATCH };
        if len != st.input.len() {
            st.output = error_envelope("input length differs from the reserved length");
            return status::LENGTH_MISMATCH;
        }
        // The host wrote the document into `input` between two export calls.
        // `black_box` makes that external write explicit to the optimiser.
        let input = core::mem::take(core::hint::black_box(&mut st.input));
        let (code, out) = process(&input);
        drop(input);
        st.output = out;
        code
    })
}

/// ABI: address of the output envelope.
#[must_use]
pub fn output_ptr() -> i32 {
    STATE.with(|s| addr_of(&s.borrow().output))
}

/// ABI: length of the output envelope.
#[must_use]
pub fn output_len() -> i32 {
    STATE.with(|s| i32::try_from(s.borrow().output.len()).unwrap_or(0))
}

/// Exported symbols of the Wasm guest. This is the only place in the code
/// base where an `unsafe` *attribute* is used; there is no `unsafe` block.
#[cfg(target_arch = "wasm32")]
mod exports {
    #[allow(unsafe_code, reason = "exporting a symbol requires #[unsafe(no_mangle)]")]
    #[unsafe(no_mangle)]
    pub extern "C" fn arca_abi_version() -> i32 {
        super::ABI_VERSION
    }

    #[allow(unsafe_code, reason = "exporting a symbol requires #[unsafe(no_mangle)]")]
    #[unsafe(no_mangle)]
    pub extern "C" fn arca_input_reserve(len: i32) -> i32 {
        super::input_reserve(len)
    }

    #[allow(unsafe_code, reason = "exporting a symbol requires #[unsafe(no_mangle)]")]
    #[unsafe(no_mangle)]
    pub extern "C" fn arca_run(len: i32) -> i32 {
        super::run(len)
    }

    #[allow(unsafe_code, reason = "exporting a symbol requires #[unsafe(no_mangle)]")]
    #[unsafe(no_mangle)]
    pub extern "C" fn arca_output_ptr() -> i32 {
        super::output_ptr()
    }

    #[allow(unsafe_code, reason = "exporting a symbol requires #[unsafe(no_mangle)]")]
    #[unsafe(no_mangle)]
    pub extern "C" fn arca_output_len() -> i32 {
        super::output_len()
    }
}

/// The ABI version this guest implements.
#[must_use]
pub const fn abi_version() -> i32 {
    ABI_VERSION
}

/// Host-side failure to interpret a guest result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The guest reported an error message.
    Guest(String),
    /// The payload is of an unexpected kind.
    UnexpectedKind(PayloadKind),
    /// The payload is malformed.
    Wire(WireError),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Guest(m) => write!(f, "guest error: {m}"),
            Self::UnexpectedKind(k) => write!(f, "unexpected payload kind {k:?}"),
            Self::Wire(e) => write!(f, "malformed payload: {e}"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Decodes the payload returned by the sandbox into a report.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode_report(kind: PayloadKind, payload: &[u8]) -> Result<XRayReport, DecodeError> {
    let limits = DecodeLimits { max_string_bytes: 8 << 20, max_items: 1 << 20, max_depth: 8 };
    match kind {
        PayloadKind::XRayReport => wire::from_bytes(payload, limits).map_err(DecodeError::Wire),
        PayloadKind::Error => Err(DecodeError::Guest(
            wire::from_bytes::<String>(payload, limits).map_err(DecodeError::Wire)?,
        )),
        other => Err(DecodeError::UnexpectedKind(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_docx() -> Vec<u8> {
        use arca_container::zip::writer::{Compression, ZipWriter};
        // A minimal OPC package with an external tracking image.
        let files: [(&str, &str); 4] = [
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>hi</w:t></w:r></w:p></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="https://t.example/p.gif" TargetMode="External"/></Relationships>"#,
            ),
        ];
        let mut w = ZipWriter::new();
        for (n, d) in files {
            w.add(n, d.as_bytes(), Compression::Deflated).unwrap();
        }
        w.finish().unwrap()
    }

    #[test]
    fn process_matches_direct_scan_after_the_wire() {
        let doc = tiny_docx();
        let (code, env) = process(&doc);
        assert_eq!(code, status::OK);
        let (kind, payload) = envelope::open(&env).unwrap();
        let report = decode_report(kind, payload).unwrap();
        assert_eq!(report, arca_xray::scan_default(&doc));
        assert_eq!(report.verdict, arca_xray::Verdict::HiddenTrackers);
    }

    #[test]
    fn abi_state_machine() {
        let doc = tiny_docx();
        // Native addresses do not fit the 32-bit ABI; only wasm32 hands them out.
        let ptr = input_reserve(i32::try_from(doc.len()).unwrap());
        assert!(ptr >= 0);
        assert_eq!(STATE.with(|s| s.borrow().input.len()), doc.len());
        // Simulate the host writing into guest memory.
        STATE.with(|s| s.borrow_mut().input.copy_from_slice(&doc));
        assert_eq!(run(i32::try_from(doc.len()).unwrap()), status::OK);
        let len = usize::try_from(output_len()).unwrap();
        assert!(len > envelope::HEADER_LEN);
        assert!(output_ptr() >= 0);
        let out = STATE.with(|s| s.borrow().output.clone());
        let (kind, payload) = envelope::open(&out).unwrap();
        assert!(decode_report(kind, payload).is_ok());
    }

    #[test]
    fn length_mismatch_and_negative_lengths() {
        assert_eq!(input_reserve(-1), 0);
        let _ = input_reserve(10);
        assert_eq!(STATE.with(|s| s.borrow().input.len()), 10);
        assert_eq!(run(11), status::LENGTH_MISMATCH);
        let out = STATE.with(|s| s.borrow().output.clone());
        let (kind, payload) = envelope::open(&out).unwrap();
        assert!(matches!(decode_report(kind, payload), Err(DecodeError::Guest(_))));
        assert_eq!(run(-3), status::LENGTH_MISMATCH);
    }

    #[test]
    fn hostile_payloads_are_rejected_by_the_host_decoder() {
        assert!(decode_report(PayloadKind::XRayReport, &[0xFF; 64]).is_err());
        assert!(decode_report(PayloadKind::SanitizedDocument, &[]).is_err());
        assert_eq!(abi_version(), 1);
    }
}
