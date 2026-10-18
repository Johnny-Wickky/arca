// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Dependency-free digests used across Arca.
//!
//! * [`mod@sha256`] — FIPS 180-4 SHA-256. Used to pin the exact bytes of every
//!   WebAssembly parser module that the sandbox is allowed to execute and to
//!   fingerprint the upstream attribution notice.
//! * [`mod@crc32`] — IEEE 802.3 CRC-32 as required by the ZIP format. Integrity
//!   only; it is *not* a security primitive.
//!
//! Both are implemented without dependencies so that the trust base of the
//! sandbox host stays minimal and auditable. SHA-256 is verified against the
//! FIPS 180-2 / NIST CAVP vectors in the unit tests below.

#![forbid(unsafe_code)]
#![no_std]

pub mod crc32;
pub mod sha256;

pub use crc32::{Crc32, crc32};
pub use sha256::{Digest, DigestParseError, Sha256, sha256};
