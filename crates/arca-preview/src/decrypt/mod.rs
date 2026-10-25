//! Opening documents that are protected by a password the reader knows.
//!
//! The file is decrypted *inside the sandbox* into a plain copy that goes through exactly the
//! same X-ray and preview as any other file; the key, the password and the decrypted bytes never
//! leave the guest (only the report and the preview do).
//!
//! * [`office`]: OOXML packages encrypted by Office 2007 ("standard", AES-ECB) and Office 2010+
//!   ("agile", AES-CBC with SHA-1/256/384/512), ECMA-376 / MS-OFFCRYPTO;
//! * [`pdf`]: the PDF standard security handler, revisions 2–6 (RC4 40/128-bit, AES-128, AES-256).
//!
//! Encrypted ODF and the old binary formats (`.doc`, `.xls`, `.ppt` with RC4) are not opened.
//! Password stretching is bounded: a file that asks for more rounds than [`MAX_SPIN`] is refused
//! rather than allowed to spend the sandbox's whole CPU budget on one hash chain.

pub(crate) mod office;
pub(crate) mod pdf;

use std::fmt;

/// Overwrites `buf` with zeros in a way the optimiser may not remove (best effort: a copy the
/// allocator or a reallocation left behind is out of reach without `unsafe`).
pub(crate) fn wipe(buf: &mut [u8]) {
    buf.fill(0);
    std::hint::black_box(buf);
}

/// Bytes of a password or a key, overwritten when dropped.
#[derive(Default)]
pub(crate) struct Secret(Vec<u8>);

impl Secret {
    pub(crate) const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl std::ops::Deref for Secret {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Most hash iterations a file may ask for. Office writes exactly 100 000 (about 1.7·10⁹ units of
/// fuel in the sandbox, within its budget for any file); a file that asks for more would only be
/// stopped by the fuel limit with a less helpful answer, so it is refused up front.
pub(crate) const MAX_SPIN: u32 = 100_000;

/// Why a document could not be decrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecryptError {
    /// The file is not encrypted in a way this module understands.
    NotEncrypted,
    /// The password does not open it.
    WrongPassword,
    /// An encryption scheme that is not supported (named).
    Unsupported(String),
    /// The file asks for more work than is allowed.
    TooExpensive,
    /// The encryption data is damaged.
    Malformed(String),
}

impl fmt::Display for DecryptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEncrypted => f.write_str("the document is not encrypted"),
            Self::WrongPassword => f.write_str("the password is wrong"),
            Self::Unsupported(what) => write!(f, "unsupported encryption: {what}"),
            Self::TooExpensive => f.write_str("the encryption asks for too much work"),
            Self::Malformed(why) => write!(f, "damaged encryption data: {why}"),
        }
    }
}

impl std::error::Error for DecryptError {}

/// The result of opening an encrypted document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decrypted {
    /// The document without its encryption.
    pub bytes: Vec<u8>,
    /// `true` when no password was needed (a PDF protected only against editing, say).
    pub without_password: bool,
}

/// Decrypts `data` with `password` (UTF-8). An Office package or a PDF; anything else is
/// [`DecryptError::NotEncrypted`]. For a PDF the empty password is tried first, whatever is given.
///
/// # Errors
/// See [`DecryptError`].
pub fn decrypt(data: &[u8], password: &[u8]) -> Result<Decrypted, DecryptError> {
    if arca_container::cfb::has_cfb_signature(data) {
        let pw = std::str::from_utf8(password)
            .map_err(|_| DecryptError::Malformed("the password is not valid UTF-8".into()))?;
        return office::decrypt(data, pw).map(|bytes| Decrypted { bytes, without_password: false });
    }
    if data.get(..1024.min(data.len())).is_some_and(|h| h.windows(5).any(|w| w == b"%PDF-")) {
        return pdf::decrypt(data, password);
    }
    Err(DecryptError::NotEncrypted)
}

/// Whether `data` is an encrypted Office package (and so needs a password to be read at all).
#[must_use]
pub fn is_encrypted_package(data: &[u8]) -> bool {
    if !arca_container::cfb::has_cfb_signature(data) {
        return false;
    }
    arca_container::cfb::CompoundFile::parse(data, crate::PreviewPolicy::default().cfb).is_ok_and(
        |cf| cf.find("EncryptedPackage").is_some() && cf.find("EncryptionInfo").is_some(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_overwritten_and_never_printed() {
        let mut b = vec![1u8, 2, 3, 4];
        wipe(&mut b);
        assert_eq!(b, [0, 0, 0, 0]);
        let s = Secret::new(b"hunter2".to_vec());
        assert_eq!(&*s, b"hunter2");
        assert!(!format!("{s:?}").contains("hunter"));
    }
}
