// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-abi — the contract between the sandbox host and parser guests
//!
//! A parser guest is a `wasm32-unknown-unknown` module with **zero imports**.
//! It exports exactly the following items (version 1):
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
//! writes the document bytes into the returned region, calls `arca_run`,
//! and copies `arca_output_len` bytes from `arca_output_ptr` out of the
//! guest memory after bounds-checking them. The result is an [`envelope`]
//! wrapping a payload encoded with the [`wire`] codec.
//!
//! Nothing in the ABI requires `unsafe` code on either side: the host uses
//! the interpreter's checked memory accessors and the guest only hands out
//! addresses of buffers it owns.

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;

pub mod envelope;
pub mod wire;

/// Version of the guest ABI described in this crate.
pub const ABI_VERSION: i32 = 1;

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
    /// The guest rejected the document; the envelope holds an error payload.
    pub const DOCUMENT_REJECTED: i32 = 4;
}
