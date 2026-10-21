//! The request a host writes into the guest's input region.
//!
//! ```text
//! offset  size  field
//! 0       4     magic  "ARCQ"
//! 4       2     ABI version (u16, = ABI_VERSION)
//! 6       2     operation (u16, see Op)
//! 8       4     flags (u32, operation specific, unknown bits must be zero)
//! 12      4     password length in bytes (u32, at most MAX_PASSWORD; 0 for none)
//! 16      p     the password, UTF-8 (only if the length is not zero)
//! 16 + p  n     the document, byte for byte
//! ```
//!
//! The header is parsed by the *guest*, which treats the host as trusted but
//! still validates every field: an unknown operation, a non-zero reserved
//! word or an unknown flag is [`status::BAD_REQUEST`](crate::status::BAD_REQUEST),
//! never a guess.

use core::fmt;

use crate::ABI_VERSION;

/// Request magic.
pub const MAGIC: [u8; 4] = *b"ARCQ";
/// Header size in bytes.
pub const HEADER_LEN: usize = 16;
/// Longest password a request may carry.
pub const MAX_PASSWORD: usize = 1024;

/// What the guest is asked to do with the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Threat X-ray; the result is an `XRayReport`.
    Scan,
    /// One-tap sterilisation; the result is a `SanitizedDocument`.
    Sanitize,
    /// Read-only extraction of the visible content; the result is a `Preview`.
    Preview,
    /// The drawing of one page; the result is a `PageModel`. The flags hold the page number
    /// (from 0).
    Render,
}

impl Op {
    /// Wire code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Scan => 1,
            Self::Sanitize => 2,
            Self::Preview => 3,
            Self::Render => 4,
        }
    }

    /// Operation for a wire code.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            1 => Some(Self::Scan),
            2 => Some(Self::Sanitize),
            3 => Some(Self::Preview),
            4 => Some(Self::Render),
            _ => None,
        }
    }

    /// Flag bits this operation understands.
    #[must_use]
    pub const fn known_flags(self) -> u32 {
        match self {
            Self::Scan => 0,
            Self::Sanitize => flags::SANITIZE_ALL,
            Self::Preview => flags::PREVIEW_ALL,
            Self::Render => flags::PAGE_MASK,
        }
    }
}

/// Flag bits.
pub mod flags {
    /// Sanitize: accept all tracked changes.
    pub const ACCEPT_REVISIONS: u32 = 1;
    /// Sanitize: remove comments and their authors.
    pub const REMOVE_COMMENTS: u32 = 2;
    /// Sanitize: remove author / company / timing / path / printer metadata.
    pub const STRIP_METADATA: u32 = 4;
    /// Sanitize: remove custom XML parts.
    pub const REMOVE_CUSTOM_XML: u32 = 8;
    /// Every sanitize flag.
    pub const SANITIZE_ALL: u32 =
        ACCEPT_REVISIONS | REMOVE_COMMENTS | STRIP_METADATA | REMOVE_CUSTOM_XML;
    /// Preview: also extract text that the document hides (hidden runs,
    /// hidden sheets and slides, tiny or invisible text), flagged as hidden.
    pub const REVEAL_HIDDEN: u32 = 1;
    /// Every preview flag.
    pub const PREVIEW_ALL: u32 = REVEAL_HIDDEN;
    /// Render: the bits that hold the page number.
    pub const PAGE_MASK: u32 = 0x00FF_FFFF;
}

/// A parsed request borrowing the document bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    /// Requested operation.
    pub op: Op,
    /// Operation flags (already checked against [`Op::known_flags`]).
    pub flags: u32,
    /// The password the reader gave for an encrypted document (empty if none).
    pub password: &'a [u8],
    /// The document.
    pub document: &'a [u8],
}

/// Why a request header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// Fewer than [`HEADER_LEN`] bytes.
    Truncated,
    /// Wrong magic.
    BadMagic,
    /// The header names another ABI version.
    VersionMismatch(u16),
    /// Unknown operation code.
    UnknownOp(u16),
    /// A flag bit the operation does not define is set.
    UnknownFlags(u32),
    /// The password is longer than [`MAX_PASSWORD`].
    PasswordTooLong,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("request shorter than its header"),
            Self::BadMagic => f.write_str("request magic is wrong"),
            Self::VersionMismatch(v) => write!(f, "request is for ABI version {v}"),
            Self::UnknownOp(c) => write!(f, "unknown operation {c}"),
            Self::UnknownFlags(b) => write!(f, "unknown flag bits {b:#x}"),
            Self::PasswordTooLong => f.write_str("the password is too long"),
        }
    }
}

impl core::error::Error for RequestError {}

/// The header for `op` with `flags`.
#[must_use]
pub fn header(op: Op, flags: u32) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    let version = u16::try_from(ABI_VERSION).unwrap_or(0);
    let (magic, rest) = h.split_at_mut(4);
    magic.copy_from_slice(&MAGIC);
    let (ver, rest) = rest.split_at_mut(2);
    ver.copy_from_slice(&version.to_le_bytes());
    let (code, rest) = rest.split_at_mut(2);
    code.copy_from_slice(&op.code().to_le_bytes());
    let (fl, _password_len) = rest.split_at_mut(4);
    fl.copy_from_slice(&flags.to_le_bytes());
    h
}

/// The header for `op` with `flags`, followed by `password`: everything that goes in front of
/// the document. `None` if the password is longer than [`MAX_PASSWORD`].
#[must_use]
pub fn prefix(op: Op, flags: u32, password: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    if password.len() > MAX_PASSWORD {
        return None;
    }
    let mut h = header(op, flags);
    let len = u32::try_from(password.len()).ok()?;
    h.get_mut(12..16)?.copy_from_slice(&len.to_le_bytes());
    let mut out = h.to_vec();
    out.extend_from_slice(password);
    Some(out)
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at.checked_add(2)?)?.try_into().ok().map(u16::from_le_bytes)
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_le_bytes)
}

/// Parses and validates a request.
///
/// # Errors
/// See [`RequestError`].
pub fn parse(input: &[u8]) -> Result<Request<'_>, RequestError> {
    let rest = input.get(HEADER_LEN..).ok_or(RequestError::Truncated)?;
    if input.get(..4) != Some(&MAGIC[..]) {
        return Err(RequestError::BadMagic);
    }
    let version = u16_at(input, 4).ok_or(RequestError::Truncated)?;
    if i32::from(version) != ABI_VERSION {
        return Err(RequestError::VersionMismatch(version));
    }
    let code = u16_at(input, 6).ok_or(RequestError::Truncated)?;
    let op = Op::from_code(code).ok_or(RequestError::UnknownOp(code))?;
    let flags = u32_at(input, 8).ok_or(RequestError::Truncated)?;
    let unknown = flags & !op.known_flags();
    if unknown != 0 {
        return Err(RequestError::UnknownFlags(unknown));
    }
    let password_len =
        usize::try_from(u32_at(input, 12).ok_or(RequestError::Truncated)?).unwrap_or(usize::MAX);
    if password_len > MAX_PASSWORD {
        return Err(RequestError::PasswordTooLong);
    }
    let password = rest.get(..password_len).ok_or(RequestError::Truncated)?;
    let document = rest.get(password_len..).ok_or(RequestError::Truncated)?;
    Ok(Request { op, flags, password, document })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(op: Op, flags: u32, doc: &[u8]) -> alloc::vec::Vec<u8> {
        let mut v = header(op, flags).to_vec();
        v.extend_from_slice(doc);
        v
    }

    #[test]
    fn roundtrip_for_every_operation() {
        for op in [Op::Scan, Op::Sanitize, Op::Preview] {
            let r = request(op, op.known_flags(), b"payload");
            let parsed = parse(&r).unwrap();
            assert_eq!(parsed.op, op);
            assert_eq!(parsed.flags, op.known_flags());
            assert_eq!(parsed.document, b"payload");
        }
        let empty = request(Op::Scan, 0, b"");
        assert_eq!(parse(&empty).unwrap().document, b"");
    }

    #[test]
    fn every_truncation_is_refused() {
        let r = request(Op::Preview, 1, b"x");
        for cut in 0..HEADER_LEN {
            assert!(parse(&r[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn hostile_headers_are_refused() {
        let good = request(Op::Scan, 0, b"doc");
        let mut bad = good.clone();
        bad[0] ^= 0xFF;
        assert_eq!(parse(&bad), Err(RequestError::BadMagic));

        let mut bad = good.clone();
        bad[4] = 9;
        assert_eq!(parse(&bad), Err(RequestError::VersionMismatch(9)));

        let mut bad = good.clone();
        bad[6] = 77;
        assert_eq!(parse(&bad), Err(RequestError::UnknownOp(77)));

        // A scan takes no flags; a sanitize takes only its own.
        assert_eq!(parse(&request(Op::Scan, 1, b"")), Err(RequestError::UnknownFlags(1)));
        let r = request(Op::Sanitize, flags::SANITIZE_ALL | 0x100, b"");
        assert_eq!(parse(&r), Err(RequestError::UnknownFlags(0x100)));

        // A password longer than the limit, or longer than the request.
        let mut bad = good.clone();
        bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&bad), Err(RequestError::PasswordTooLong));
        let mut bad = good;
        bad[12] = 100;
        assert_eq!(parse(&bad), Err(RequestError::Truncated));
    }

    #[test]
    fn a_password_travels_in_front_of_the_document() {
        let mut r = prefix(Op::Preview, 0, "пароль".as_bytes()).unwrap();
        r.extend_from_slice(b"the document");
        let parsed = parse(&r).unwrap();
        assert_eq!(parsed.password, "пароль".as_bytes());
        assert_eq!(parsed.document, b"the document");
        assert!(prefix(Op::Scan, 0, &[b'x'; MAX_PASSWORD + 1]).is_none());
        let plain = request(Op::Scan, 0, b"d");
        let none = parse(&plain).unwrap();
        assert_eq!(none.password, b"");
        assert_eq!(none.document, b"d");
    }

    #[test]
    fn the_document_is_not_interpreted() {
        // Bytes that look like a second header are document content.
        let inner = request(Op::Sanitize, 0, b"");
        let outer = request(Op::Scan, 0, &inner);
        let parsed = parse(&outer).unwrap();
        assert_eq!(parsed.op, Op::Scan);
        assert_eq!(parsed.document, &inner[..]);
    }
}
