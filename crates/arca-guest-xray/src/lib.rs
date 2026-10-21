// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-guest-xray
//!
//! The parser guest implementing [`arca_abi`] version 4: Threat X-Ray
//! (`Scan`), one-tap sterilisation (`Sanitize`) and the read-only content
//! preview (`Preview`) in one module. Built for `wasm32-unknown-unknown` it is
//! a module with **zero imports**: it can compute, and nothing else.
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
use arca_abi::request::{self, Op, flags};
use arca_abi::wire::{self, DecodeLimits, WireError};
use arca_abi::{ABI_VERSION, status};
use arca_preview::page::PageModel;
use arca_preview::{Preview, PreviewError, PreviewPolicy};
use arca_xray::sanitize::{SanitizeError, SanitizeOptions, SanitizedDocument};
use arca_xray::{FileFormat, ScanPolicy, XRayReport};

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

/// Answers one request and returns `(status, envelope)`. Pure and testable.
///
/// `input` is the whole request: the 16-byte header, the password if there is one, and the
/// document.
#[must_use]
pub fn process(input: &[u8]) -> (i32, Vec<u8>) {
    let req = match request::parse(input) {
        Ok(r) => r,
        Err(e) => return (status::BAD_REQUEST, error_envelope(&e.to_string())),
    };
    // An encrypted document is opened first, here in the sandbox; everything after that is
    // about the decrypted content, exactly as for a document that never was encrypted.
    let (document, success) = match open_document(req.document, req.password) {
        Ok(opened) => opened,
        Err(refusal) => return refusal,
    };
    let (code, envelope) = match req.op {
        Op::Scan => {
            let mut report = arca_xray::scan(&document, &ScanPolicy::default());
            if success != status::OK {
                // The hash is the one of the file the reader holds, not of the decrypted copy.
                report.sha256 = arca_digest::sha256(req.document).to_string();
            }
            seal(PayloadKind::XRayReport, &report, "could not encode the X-ray report")
        }
        Op::Sanitize => sanitize(&document, req.flags),
        Op::Preview => preview(&document, req.flags),
        Op::Render => render(&document, req.flags),
    };
    if code == status::OK { (success, envelope) } else { (code, envelope) }
}

/// Whether `data` is a PDF that declares an encryption dictionary.
fn is_encrypted_pdf(data: &[u8]) -> bool {
    data.get(..data.len().min(1024)).is_some_and(|h| h.windows(5).any(|w| w == b"%PDF-"))
        && data.windows(8).any(|w| w == b"/Encrypt")
}

/// The bytes to work on and the status of a success, or the answer to give instead.
type Opened<'a> = Result<(std::borrow::Cow<'a, [u8]>, i32), (i32, Vec<u8>)>;

/// Opens an encrypted document. A document that is not encrypted passes through untouched.
fn open_document<'a>(document: &'a [u8], password: &[u8]) -> Opened<'a> {
    use arca_preview::DecryptError;
    let plain = || Ok((std::borrow::Cow::Borrowed(document), status::OK));
    let pdf = is_encrypted_pdf(document);
    let office = !pdf && arca_preview::is_encrypted_package(document);
    if !pdf && !office {
        return plain();
    }
    // An Office package is never tried without a password; a PDF always is, because many are
    // locked only against editing.
    if office && password.is_empty() {
        return plain();
    }
    match arca_preview::decrypt(document, password) {
        Ok(d) => {
            let code = if d.without_password { status::OK_UNLOCKED } else { status::OK_DECRYPTED };
            Ok((std::borrow::Cow::Owned(d.bytes), code))
        }
        // A PDF that needs a password and has none yet is reported as it was before.
        Err(DecryptError::WrongPassword) if password.is_empty() => plain(),
        Err(DecryptError::WrongPassword) => {
            Err((status::WRONG_PASSWORD, error_envelope("the password does not open the document")))
        }
        Err(e @ DecryptError::Unsupported(_)) => {
            Err((status::UNSUPPORTED_FORMAT, error_envelope(&e.to_string())))
        }
        Err(e @ DecryptError::TooExpensive) => {
            Err((status::OPERATION_FAILED, error_envelope(&e.to_string())))
        }
        Err(DecryptError::Malformed(_)) if password.is_empty() => plain(),
        Err(e @ DecryptError::Malformed(_)) => {
            Err((status::DOCUMENT_REJECTED, error_envelope(&e.to_string())))
        }
        Err(DecryptError::NotEncrypted) => plain(),
    }
}

fn seal<T: wire::Wire>(kind: PayloadKind, value: &T, what: &str) -> (i32, Vec<u8>) {
    match wire::to_bytes(value).and_then(|p| envelope::seal(kind, &p)) {
        Ok(env) => (status::OK, env),
        Err(_) => (status::ENCODE_FAILED, error_envelope(what)),
    }
}

fn sanitize(document: &[u8], request_flags: u32) -> (i32, Vec<u8>) {
    let options = SanitizeOptions {
        accept_revisions: request_flags & flags::ACCEPT_REVISIONS != 0,
        remove_comments: request_flags & flags::REMOVE_COMMENTS != 0,
        strip_metadata: request_flags & flags::STRIP_METADATA != 0,
        remove_custom_xml: request_flags & flags::REMOVE_CUSTOM_XML != 0,
    };
    let result = match arca_xray::sanitize::sanitize(document, &ScanPolicy::default(), &options) {
        // A PDF is rebuilt by the preview crate, which has the PDF parser; the result goes
        // through the same verification as every other sterile copy.
        Err(SanitizeError::Unsupported(FileFormat::Pdf)) => {
            return sanitize_pdf(document, &options);
        }
        // The binary Office formats and RTF come out as OOXML, written from what the viewer
        // reads of them (a copy that has never had a macro, an OLE object or a field in it).
        Err(SanitizeError::Unsupported(
            FileFormat::Doc | FileFormat::Xls | FileFormat::Ppt | FileFormat::Rtf,
        )) => {
            return sanitize_legacy(document, &options);
        }
        other => other,
    };
    sanitize_result(result)
}

fn sanitize_pdf(document: &[u8], options: &SanitizeOptions) -> (i32, Vec<u8>) {
    let pdf_options = arca_preview::PdfSanitizeOptions {
        strip_metadata: options.strip_metadata,
        remove_comments: options.remove_comments,
    };
    let mut encrypted = false;
    let result = arca_xray::sanitize::sanitize_with(document, &ScanPolicy::default(), |data| {
        arca_preview::sanitize_pdf(data, &pdf_options)
            .map(|(bytes, actions)| (bytes, FileFormat::Pdf, actions))
            .map_err(|e| {
                encrypted = matches!(e, PreviewError::Encrypted);
                SanitizeError::Rebuild(e.to_string())
            })
    });
    if encrypted {
        return (status::ENCRYPTED, error_envelope("the document is password protected"));
    }
    sanitize_result(result)
}

fn sanitize_legacy(document: &[u8], options: &SanitizeOptions) -> (i32, Vec<u8>) {
    let legacy = arca_preview::LegacyOptions {
        accept_revisions: options.accept_revisions,
        remove_comments: options.remove_comments,
    };
    let mut encrypted = false;
    let result = arca_xray::sanitize::sanitize_with(document, &ScanPolicy::default(), |data| {
        arca_preview::sanitize_legacy(data, &legacy).map_err(|e| {
            encrypted = matches!(e, PreviewError::Encrypted);
            SanitizeError::Rebuild(e.to_string())
        })
    });
    if encrypted {
        return (status::ENCRYPTED, error_envelope("the document is password protected"));
    }
    sanitize_result(result)
}

fn sanitize_result(result: Result<SanitizedDocument, SanitizeError>) -> (i32, Vec<u8>) {
    match result {
        Ok(doc) => {
            seal(PayloadKind::SanitizedDocument, &doc, "could not encode the sterile document")
        }
        Err(e @ SanitizeError::Unsupported(_)) => {
            (status::UNSUPPORTED_FORMAT, error_envelope(&e.to_string()))
        }
        Err(e @ SanitizeError::Rejected(_)) => {
            (status::DOCUMENT_REJECTED, error_envelope(&e.to_string()))
        }
        Err(e) => (status::OPERATION_FAILED, error_envelope(&e.to_string())),
    }
}

fn render(document: &[u8], request_flags: u32) -> (i32, Vec<u8>) {
    let page = usize::try_from(request_flags & flags::PAGE_MASK).unwrap_or(usize::MAX);
    match arca_preview::render_page(document, page, &PreviewPolicy::default()) {
        Ok(p) => seal(PayloadKind::Page, &p, "could not encode the page"),
        Err(e @ PreviewError::Unsupported(_)) => {
            (status::UNSUPPORTED_FORMAT, error_envelope(&e.to_string()))
        }
        Err(PreviewError::Encrypted) => {
            (status::ENCRYPTED, error_envelope("the document is password protected"))
        }
        Err(e @ (PreviewError::Malformed(_) | PreviewError::TooLarge(_))) => {
            (status::DOCUMENT_REJECTED, error_envelope(&e.to_string()))
        }
    }
}

fn preview(document: &[u8], request_flags: u32) -> (i32, Vec<u8>) {
    let policy = PreviewPolicy {
        reveal_hidden: request_flags & flags::REVEAL_HIDDEN != 0,
        ..PreviewPolicy::default()
    };
    match arca_preview::preview(document, &policy) {
        Ok(p) => seal(PayloadKind::Preview, &p, "could not encode the preview"),
        Err(e @ PreviewError::Unsupported(_)) => {
            (status::UNSUPPORTED_FORMAT, error_envelope(&e.to_string()))
        }
        Err(PreviewError::Encrypted) => {
            (status::ENCRYPTED, error_envelope("the document is password protected"))
        }
        Err(e @ (PreviewError::Malformed(_) | PreviewError::TooLarge(_))) => {
            (status::DOCUMENT_REJECTED, error_envelope(&e.to_string()))
        }
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

/// Decoding limits for each kind of result. The payload is untrusted: a
/// compromised guest could have written anything.
const fn limits_for(kind: PayloadKind) -> DecodeLimits {
    match kind {
        PayloadKind::Preview => {
            DecodeLimits { max_string_bytes: 8 << 20, max_items: 4 << 20, max_depth: 32 }
        }
        PayloadKind::SanitizedDocument => {
            DecodeLimits { max_string_bytes: 1 << 30, max_items: 1 << 20, max_depth: 16 }
        }
        PayloadKind::Page => {
            DecodeLimits { max_string_bytes: 8 << 20, max_items: 1 << 20, max_depth: 8 }
        }
        PayloadKind::XRayReport | PayloadKind::Error => {
            DecodeLimits { max_string_bytes: 8 << 20, max_items: 1 << 20, max_depth: 8 }
        }
    }
}

/// A decoded guest result.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// Result of [`Op::Scan`].
    Report(Box<XRayReport>),
    /// Result of [`Op::Sanitize`].
    Sanitized(Box<SanitizedDocument>),
    /// Result of [`Op::Preview`].
    Preview(Box<Preview>),
    /// Result of [`Op::Render`].
    Page(Box<PageModel>),
}

/// Decodes the payload returned by the sandbox.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode(kind: PayloadKind, payload: &[u8]) -> Result<Output, DecodeError> {
    let limits = limits_for(kind);
    match kind {
        PayloadKind::XRayReport => wire::from_bytes(payload, limits)
            .map(|r| Output::Report(Box::new(r)))
            .map_err(DecodeError::Wire),
        PayloadKind::SanitizedDocument => wire::from_bytes(payload, limits)
            .map(|r| Output::Sanitized(Box::new(r)))
            .map_err(DecodeError::Wire),
        PayloadKind::Preview => wire::from_bytes(payload, limits)
            .map(|r| Output::Preview(Box::new(r)))
            .map_err(DecodeError::Wire),
        PayloadKind::Page => wire::from_bytes(payload, limits)
            .map(|r| Output::Page(Box::new(r)))
            .map_err(DecodeError::Wire),
        PayloadKind::Error => Err(DecodeError::Guest(
            wire::from_bytes::<String>(payload, limits).map_err(DecodeError::Wire)?,
        )),
    }
}

/// Decodes the payload returned by the sandbox into a report.
///
/// # Errors
/// See [`DecodeError`]; any kind other than a report is
/// [`DecodeError::UnexpectedKind`].
pub fn decode_report(kind: PayloadKind, payload: &[u8]) -> Result<XRayReport, DecodeError> {
    match decode(kind, payload)? {
        Output::Report(r) => Ok(*r),
        Output::Sanitized(_) => Err(DecodeError::UnexpectedKind(PayloadKind::SanitizedDocument)),
        Output::Preview(_) => Err(DecodeError::UnexpectedKind(PayloadKind::Preview)),
        Output::Page(_) => Err(DecodeError::UnexpectedKind(PayloadKind::Page)),
    }
}

/// Decodes the payload returned by the sandbox into a preview.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode_preview(kind: PayloadKind, payload: &[u8]) -> Result<Preview, DecodeError> {
    match decode(kind, payload)? {
        Output::Preview(p) => Ok(*p),
        Output::Report(_) => Err(DecodeError::UnexpectedKind(PayloadKind::XRayReport)),
        Output::Sanitized(_) => Err(DecodeError::UnexpectedKind(PayloadKind::SanitizedDocument)),
        Output::Page(_) => Err(DecodeError::UnexpectedKind(PayloadKind::Page)),
    }
}

/// Decodes the payload returned by the sandbox into a sterile document.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode_sanitized(
    kind: PayloadKind,
    payload: &[u8],
) -> Result<SanitizedDocument, DecodeError> {
    match decode(kind, payload)? {
        Output::Sanitized(d) => Ok(*d),
        Output::Report(_) => Err(DecodeError::UnexpectedKind(PayloadKind::XRayReport)),
        Output::Preview(_) => Err(DecodeError::UnexpectedKind(PayloadKind::Preview)),
        Output::Page(_) => Err(DecodeError::UnexpectedKind(PayloadKind::Page)),
    }
}

/// Decodes the payload returned by the sandbox into the drawing of a page.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode_page(kind: PayloadKind, payload: &[u8]) -> Result<PageModel, DecodeError> {
    match decode(kind, payload)? {
        Output::Page(p) => Ok(*p),
        Output::Report(_) => Err(DecodeError::UnexpectedKind(PayloadKind::XRayReport)),
        Output::Sanitized(_) => Err(DecodeError::UnexpectedKind(PayloadKind::SanitizedDocument)),
        Output::Preview(_) => Err(DecodeError::UnexpectedKind(PayloadKind::Preview)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arca_xray::Verdict;

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
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>hi</w:t></w:r></w:p><w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>secret</w:t></w:r></w:p></w:body></w:document>"#,
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

    fn request_for(op: Op, flags: u32, doc: &[u8]) -> Vec<u8> {
        let mut v = request::header(op, flags).to_vec();
        v.extend_from_slice(doc);
        v
    }

    fn open(env: &[u8]) -> (PayloadKind, &[u8]) {
        envelope::open(env).unwrap()
    }

    #[test]
    fn scan_matches_direct_scan_after_the_wire() {
        let doc = tiny_docx();
        let (code, env) = process(&request_for(Op::Scan, 0, &doc));
        assert_eq!(code, status::OK);
        let (kind, payload) = open(&env);
        let report = decode_report(kind, payload).unwrap();
        assert_eq!(report, arca_xray::scan_default(&doc));
        assert_eq!(report.verdict, Verdict::HiddenTrackers);
    }

    #[test]
    fn preview_matches_direct_preview_after_the_wire() {
        let doc = tiny_docx();
        for reveal in [true, false] {
            let flags = if reveal { flags::REVEAL_HIDDEN } else { 0 };
            let (code, env) = process(&request_for(Op::Preview, flags, &doc));
            assert_eq!(code, status::OK);
            let (kind, payload) = open(&env);
            assert_eq!(kind, PayloadKind::Preview);
            let got = decode_preview(kind, payload).unwrap();
            let policy = PreviewPolicy { reveal_hidden: reveal, ..PreviewPolicy::default() };
            assert_eq!(got, arca_preview::preview(&doc, &policy).unwrap());
            let all: String = got
                .sections
                .iter()
                .flat_map(|s| s.blocks.iter())
                .filter_map(|b| {
                    if let arca_preview::model::Block::Para(p) = b { Some(p.text()) } else { None }
                })
                .collect::<Vec<_>>()
                .join("|");
            assert_eq!(all.contains("secret"), reveal, "{all}");
            assert!(all.contains("hi"));
        }
    }

    #[test]
    fn sanitize_removes_the_tracker_and_verifies() {
        let doc = tiny_docx();
        let (code, env) = process(&request_for(Op::Sanitize, flags::SANITIZE_ALL, &doc));
        assert_eq!(code, status::OK);
        let (kind, payload) = open(&env);
        let sd = decode_sanitized(kind, payload).unwrap();
        assert!(!sd.bytes.is_empty());
        assert_eq!(sd.verification.verdict, Verdict::Clean);
        assert!(!sd.actions.is_empty());
        // A different payload kind is not mistaken for another.
        assert!(matches!(decode_report(kind, payload), Err(DecodeError::UnexpectedKind(_))));
        assert!(matches!(decode_preview(kind, payload), Err(DecodeError::UnexpectedKind(_))));
    }

    fn pdf_with_script() -> Vec<u8> {
        // Catalog with an open action that runs JavaScript, one page of text.
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert\\(1\\)) >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            "<< /Length 40 >>\nstream\nBT /F1 12 Tf 10 100 Td (Sterile text) Tj ET\nendstream",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .bytes(),
        );
        out
    }

    #[test]
    fn a_pdf_gets_a_sterile_copy_too() {
        let doc = pdf_with_script();
        let before = process(&request_for(Op::Scan, 0, &doc));
        assert_eq!(before.0, status::OK);
        let (code, env) = process(&request_for(Op::Sanitize, flags::SANITIZE_ALL, &doc));
        assert_eq!(code, status::OK);
        let (kind, payload) = open(&env);
        let sd = decode_sanitized(kind, payload).unwrap();
        assert_eq!(sd.format, FileFormat::Pdf);
        assert!(sd.bytes.starts_with(b"%PDF-"));
        assert!(!sd.bytes.windows(10).any(|w| w == b"JavaScript"));
        assert_eq!(sd.verification.verdict, Verdict::Clean);
        assert!(sd.actions.iter().any(|a| a.target.contains("OpenAction")));
        // The copy is itself a document the viewer can show.
        let (code, env) = process(&request_for(Op::Preview, 0, &sd.bytes));
        assert_eq!(code, status::OK);
        let (kind, payload) = open(&env);
        let p = decode_preview(kind, payload).unwrap();
        assert!(format!("{:?}", p.sections).contains("Sterile text"));
        // A password-protected PDF says so instead of failing.
        let mut enc = pdf_with_script();
        let at = enc.windows(8).position(|w| w == b"/Size 6 ").unwrap();
        enc.splice(at..at, b"/Encrypt 9 0 R ".iter().copied());
        let (code, _) = process(&request_for(Op::Sanitize, flags::SANITIZE_ALL, &enc));
        assert!(code == status::ENCRYPTED || code == status::DOCUMENT_REJECTED, "{code}");
    }

    #[test]
    fn legacy_office_and_rtf_get_an_ooxml_copy() {
        let rtf: &[u8] = br"{\rtf1\ansi Before {\object\objemb{\*\objclass Equation.3}{\*\objdata 0105000002000000}}After}";
        let doc =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/report.doc"))
                .unwrap();
        for (input, format) in [(rtf, FileFormat::Docx), (doc.as_slice(), FileFormat::Docx)] {
            let (code, env) = process(&request_for(Op::Sanitize, flags::SANITIZE_ALL, input));
            assert_eq!(code, status::OK);
            let (kind, payload) = open(&env);
            let sd = decode_sanitized(kind, payload).unwrap();
            assert_eq!(sd.format, format);
            assert!(sd.bytes.starts_with(b"PK"));
            assert_eq!(sd.verification.verdict, Verdict::Clean);
            assert!(!sd.actions.is_empty());
            let (code, env) = process(&request_for(Op::Preview, 0, &sd.bytes));
            assert_eq!(code, status::OK);
            let (kind, payload) = open(&env);
            assert!(decode_preview(kind, payload).is_ok());
        }
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/").to_owned() + name,
        )
        .unwrap()
    }

    fn with_password(op: Op, flags: u32, password: &str, doc: &[u8]) -> Vec<u8> {
        let mut v = request::prefix(op, flags, password.as_bytes()).unwrap();
        v.extend_from_slice(doc);
        v
    }

    #[test]
    fn an_encrypted_document_opens_inside_the_sandbox_with_its_password() {
        let enc = fixture("enc-agile.docx");
        // Without a password: reported as encrypted, as ever.
        let (code, env) = process(&request_for(Op::Scan, 0, &enc));
        assert_eq!(code, status::OK);
        let (kind, payload) = open(&env);
        let r = decode_report(kind, payload).unwrap();
        assert_eq!(r.format, FileFormat::EncryptedOoxml);
        let (code, _) = process(&request_for(Op::Preview, 0, &enc));
        assert_eq!(code, status::ENCRYPTED);
        // With the wrong one.
        let (code, _) = process(&with_password(Op::Preview, 0, "nope", &enc));
        assert_eq!(code, status::WRONG_PASSWORD);
        // With the right one: the X-ray, the preview and the sterile copy are about the content.
        let (code, env) = process(&with_password(Op::Scan, 0, "пароль-123", &enc));
        assert_eq!(code, status::OK_DECRYPTED);
        let (kind, payload) = open(&env);
        let r = decode_report(kind, payload).unwrap();
        assert_eq!(r.format, FileFormat::Docx);
        assert_eq!(r.sha256, arca_digest::sha256(&enc).to_string(), "the hash is the file's");
        let (code, env) = process(&with_password(Op::Preview, 0, "пароль-123", &enc));
        assert_eq!(code, status::OK_DECRYPTED);
        let (kind, payload) = open(&env);
        let p = decode_preview(kind, payload).unwrap();
        assert!(format!("{:?}", p.sections).contains("Квартальный отчёт"));
        let (code, env) =
            process(&with_password(Op::Sanitize, flags::SANITIZE_ALL, "пароль-123", &enc));
        assert_eq!(code, status::OK_DECRYPTED);
        let (kind, payload) = open(&env);
        let sd = decode_sanitized(kind, payload).unwrap();
        assert_eq!(sd.format, FileFormat::Docx);
        assert_eq!(sd.verification.verdict, Verdict::Clean);
        // A password given for a file that needs none changes nothing.
        let (code, _) = process(&with_password(Op::Preview, 0, "x", &fixture("report.docx")));
        assert_eq!(code, status::OK);
    }

    #[test]
    fn a_pdf_locked_only_against_editing_opens_by_itself() {
        let (code, env) = process(&request_for(Op::Preview, 0, &fixture("enc-open.pdf")));
        assert_eq!(code, status::OK_UNLOCKED);
        let (kind, payload) = open(&env);
        assert!(
            format!("{:?}", decode_preview(kind, payload).unwrap().sections).contains("Шаг два")
        );
        // One that needs a password says so, and then opens with it.
        let locked = fixture("enc-aes-256.pdf");
        assert_eq!(process(&request_for(Op::Preview, 0, &locked)).0, status::ENCRYPTED);
        assert_eq!(
            process(&with_password(Op::Preview, 0, "wrong", &locked)).0,
            status::WRONG_PASSWORD
        );
        assert_eq!(
            process(&with_password(Op::Preview, 0, "user-256", &locked)).0,
            status::OK_DECRYPTED
        );
        let (code, env) =
            process(&with_password(Op::Sanitize, flags::SANITIZE_ALL, "user-256", &locked));
        assert_eq!(code, status::OK_DECRYPTED);
        let (kind, payload) = open(&env);
        assert_eq!(decode_sanitized(kind, payload).unwrap().format, FileFormat::Pdf);
    }

    #[test]
    fn unsupported_and_failed_operations_have_their_own_statuses() {
        // Plain text cannot be rebuilt structurally.
        let (code, env) = process(&request_for(Op::Sanitize, flags::SANITIZE_ALL, b"just text"));
        assert_eq!(code, status::UNSUPPORTED_FORMAT);
        let (kind, payload) = open(&env);
        assert!(matches!(decode(kind, payload), Err(DecodeError::Guest(_))));
        // An executable has nothing to preview.
        let (code, _) = process(&request_for(Op::Preview, 0, b"MZ\x90\x00\x03\x00\x00\x00"));
        assert_eq!(code, status::UNSUPPORTED_FORMAT);
        // A garbled archive is a rejected document, not a trap.
        let mut bad = tiny_docx();
        bad.truncate(bad.len() / 2);
        let (code, _) = process(&request_for(Op::Preview, 0, &bad));
        assert_eq!(code, status::DOCUMENT_REJECTED);
    }

    #[test]
    fn malformed_requests_are_refused_not_guessed() {
        for (input, why) in [
            (Vec::new(), "empty"),
            (tiny_docx(), "a bare document is not a request"),
            (request_for(Op::Scan, 1, b"x"), "unknown flag"),
        ] {
            let (code, env) = process(&input);
            assert_eq!(code, status::BAD_REQUEST, "{why}");
            let (kind, payload) = open(&env);
            assert!(matches!(decode(kind, payload), Err(DecodeError::Guest(_))), "{why}");
        }
        let mut wrong_version = request_for(Op::Scan, 0, b"x");
        wrong_version[4] = 1;
        assert_eq!(process(&wrong_version).0, status::BAD_REQUEST);
        // A password length that runs past the request, or past the limit.
        let mut password = request_for(Op::Scan, 0, b"x");
        password[12] = 9;
        assert_eq!(process(&password).0, status::BAD_REQUEST);
        password[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(process(&password).0, status::BAD_REQUEST);
    }

    #[test]
    fn abi_state_machine() {
        let req = request_for(Op::Scan, 0, &tiny_docx());
        // Native addresses do not fit the 32-bit ABI; only wasm32 hands them out.
        let ptr = input_reserve(i32::try_from(req.len()).unwrap());
        assert!(ptr >= 0);
        assert_eq!(STATE.with(|s| s.borrow().input.len()), req.len());
        // Simulate the host writing into guest memory.
        STATE.with(|s| s.borrow_mut().input.copy_from_slice(&req));
        assert_eq!(run(i32::try_from(req.len()).unwrap()), status::OK);
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
        assert!(decode_preview(PayloadKind::Preview, &[0xFF; 64]).is_err());
        assert!(decode_preview(PayloadKind::Preview, &[]).is_err());
        assert!(decode(PayloadKind::Error, &[0xFF; 8]).is_err());
        assert_eq!(abi_version(), 4);
    }

    #[test]
    fn corrupted_previews_never_panic_the_host_decoder() {
        let doc = tiny_docx();
        let (_, env) = process(&request_for(Op::Preview, flags::REVEAL_HIDDEN, &doc));
        let (kind, payload) = envelope::open(&env).unwrap();
        let payload = payload.to_vec();
        for i in 0..payload.len() {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut m = payload.clone();
                m[i] ^= mask;
                let _ = decode_preview(kind, &m);
            }
            let _ = decode_preview(kind, &payload[..i]);
        }
    }
}
