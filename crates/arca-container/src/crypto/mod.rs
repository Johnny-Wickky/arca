// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The few cryptographic primitives that opening a password-protected document needs, written
//! out in safe Rust so that they run unchanged in the `wasm32-unknown-unknown` sandbox:
//!
//! | module | primitive | used for |
//! |---|---|---|
//! | [`md5`] | MD5 | PDF standard security handler (revisions 2–4) |
//! | [`sha1`] | SHA-1 | ECMA-376 "standard" encryption, PDF revision 3 owner/user checks |
//! | `arca_digest::sha256` | SHA-256 | agile encryption, PDF revisions 5–6 |
//! | [`sha512`] | SHA-384, SHA-512 | agile encryption, PDF revision 6 |
//! | [`aes`] | AES-128/192/256 | everything above |
//! | [`rc4`] | RC4 | PDF revisions 2–4 |
//!
//! These are **decryption-side** tools for files whose owner knows the password. None of them
//! is offered for protecting anything: there is no random source in the sandbox and none is
//! wanted. Every one is checked against published test vectors and against an independent
//! implementation (see the tests).

pub mod aes;
pub mod md5;
pub mod rc4;
pub mod sha1;
pub mod sha512;

/// The hash functions of the formats above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    /// MD5, 16 bytes.
    Md5,
    /// SHA-1, 20 bytes.
    Sha1,
    /// SHA-256, 32 bytes.
    Sha256,
    /// SHA-384, 48 bytes.
    Sha384,
    /// SHA-512, 64 bytes.
    Sha512,
}

impl Hash {
    /// Length of the digest in bytes.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    /// The digest of the concatenation of `parts`, written to the start of `out`; returns its
    /// length. For loops that hash millions of times: nothing is allocated.
    pub fn digest_into(self, parts: &[&[u8]], out: &mut [u8; 64]) -> usize {
        match self {
            Self::Sha1 => {
                let mut h = sha1::Sha1::new();
                parts.iter().for_each(|p| h.update(p));
                let d = h.finalize();
                out.get_mut(..20).map_or(0, |o| {
                    o.copy_from_slice(&d);
                    20
                })
            }
            Self::Sha384 | Self::Sha512 => {
                let mut h = if self == Self::Sha384 {
                    sha512::Sha512::new384()
                } else {
                    sha512::Sha512::new()
                };
                parts.iter().for_each(|p| h.update(p));
                h.finalize_into(out)
            }
            Self::Md5 | Self::Sha256 => {
                let d = self.digest(parts);
                out.get_mut(..d.len()).map_or(0, |o| {
                    o.copy_from_slice(&d);
                    d.len()
                })
            }
        }
    }

    /// The digest of the concatenation of `parts`.
    #[must_use]
    pub fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        match self {
            Self::Md5 => {
                let mut h = md5::Md5::new();
                parts.iter().for_each(|p| h.update(p));
                h.finalize().to_vec()
            }
            Self::Sha1 => {
                let mut h = sha1::Sha1::new();
                parts.iter().for_each(|p| h.update(p));
                h.finalize().to_vec()
            }
            Self::Sha256 => {
                let mut h = arca_digest::Sha256::new();
                parts.iter().for_each(|p| h.update(p));
                h.finalize().0.to_vec()
            }
            Self::Sha384 => {
                let mut h = sha512::Sha512::new384();
                parts.iter().for_each(|p| h.update(p));
                h.finalize()
            }
            Self::Sha512 => {
                let mut h = sha512::Sha512::new();
                parts.iter().for_each(|p| h.update(p));
                h.finalize()
            }
        }
    }
}

/// Compares two byte strings without stopping at the first difference.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests;
