// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-abi — the contract between the sandbox host and parser guests
//!
//! A parser guest is a `wasm32-unknown-unknown` module with **zero imports**.
//! It exports exactly the following items (version 5):
//!
//! | export                         | signature           | meaning |
//! |--------------------------------|---------------------|---------|
//! | `memory`                       | linear memory       | the guest's only memory |
//! | `arca_abi_version`            | `() -> i32`         | must return [`ABI_VERSION`] |
//! | `arca_input_reserve`          | `(len: i32) -> i32` | guest allocates `len` bytes, returns their address (0 = failure) |
//! | `arca_run`                    | `(len: i32) -> i32` | processes the reserved input, returns a [`status`] code |
//! | `arca_output_ptr`             | `() -> i32`         | address of the result envelope |
//! | `arca_output_len`             | `() -> i32`         | length of the result envelope |
//!
//! Protocol: the host calls `arca_abi_version`, then `arca_input_reserve`,
//! writes a [`request`] (a 16-byte header naming the operation followed by the
//! document bytes) into the returned region, calls `arca_run`, and copies
//! `arca_output_len` bytes from `arca_output_ptr` out of the guest memory after
//! bounds-checking them. The result is an [`envelope`] wrapping a payload
//! encoded with the [`wire`] codec.
//!
//! Version 2 added the request header: a guest now serves several operations
//! (scan, sterilise, preview) instead of one. Version 3 lets the header carry a
//! password, which the guest uses to open an encrypted document inside the
//! sandbox. Later versions extended the operation set (see [`request`]); the
//! current one is [`ABI_VERSION`]. A guest of another version is refused by the
//! sandbox because its `arca_abi_version` differs.
//!
//! Nothing in the ABI requires `unsafe` code on either side: the host uses
//! the interpreter's checked memory accessors and the guest only hands out
//! addresses of buffers it owns.

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;

pub mod envelope;
pub mod request;
pub mod wire;

/// Version of the guest ABI described in this crate.
pub const ABI_VERSION: i32 = 6;

/// Names of the items a guest must export.
pub mod exports {
    /// Linear memory export.
    pub const MEMORY: &str = "memory";
    /// `() -> i32`
    pub const ABI_VERSION: &str = "arca_abi_version";
    /// `(len: i32) -> i32`
    pub const INPUT_RESERVE: &str = "arca_input_reserve";
    /// `(len: i32) -> i32`
    pub const RUN: &str = "arca_run";
    /// `() -> i32`
    pub const OUTPUT_PTR: &str = "arca_output_ptr";
    /// `() -> i32`
    pub const OUTPUT_LEN: &str = "arca_output_len";
}

/// Status codes returned by `arca_run`.
pub mod status {
    /// The output envelope holds a successful result.
    pub const OK: i32 = 0;
    /// `arca_run` was called without a reserved input.
    pub const NO_INPUT: i32 = 1;
    /// The length passed to `arca_run` differs from the reserved length.
    pub const LENGTH_MISMATCH: i32 = 2;
    /// The guest could not encode its result.
    pub const ENCODE_FAILED: i32 = 3;
    /// The guest rejected the document as damaged, hostile or beyond a safety
    /// limit; the envelope holds an error payload.
    pub const DOCUMENT_REJECTED: i32 = 4;
    /// The request header is malformed or names an unknown operation.
    pub const BAD_REQUEST: i32 = 5;
    /// The format cannot be sterilised structurally (the payload names it).
    pub const UNSUPPORTED_FORMAT: i32 = 6;
    /// The operation failed after the document was accepted (rebuild or
    /// verification failure, resource limit of the extractor ...).
    pub const OPERATION_FAILED: i32 = 7;
    /// The document is password protected / encrypted: nothing can be read
    /// from it (the X-ray still reports what is visible from outside).
    pub const ENCRYPTED: i32 = 8;
    /// The document is encrypted and the password given does not open it.
    pub const WRONG_PASSWORD: i32 = 9;
    /// Success: the document was encrypted and the password opened it; the result is about the
    /// decrypted content.
    pub const OK_DECRYPTED: i32 = 10;
    /// Success: the document was encrypted but needed no password (a PDF that only restricts
    /// editing); the result is about the decrypted content.
    pub const OK_UNLOCKED: i32 = 11;

    /// Whether `code` means that the output envelope holds a successful result.
    #[must_use]
    pub const fn is_success(code: i32) -> bool {
        matches!(code, OK | OK_DECRYPTED | OK_UNLOCKED)
    }
}
