// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-ffi — the C ABI bridge for iOS and native clients
//!
//! `libarca_ffi.a` is the native library linked into the iOS app (and macOS/generic C clients).
//! It embeds the pinned WebAssembly guest and exposes [`arca_engine::Host`] as a set of standard
//! C functions (`arca_*`). The crate is split so that almost nothing needs `unsafe`:
//!
//! * [`api`] — the whole logic, written in safe Rust over plain Rust types and tested
//!   natively without a C compiler;
//! * `ffi` — the C ABI entry points: validating pointers and lengths, unwrapping C arguments,
//!   calling [`api`] inside `catch_unwind`, and writing to [`ffi::ArcaBuffer`]. It is the *only*
//!   file of this crate that contains `unsafe` code; `tools/check-unsafe.sh` enforces that.
//!
//! Document bytes can travel directly from a file descriptor or path into the engine, so large
//! files never have to be copied through foreign managed heaps, and every handle is a counter,
//! never a raw pointer.

#![deny(unsafe_code)]

pub mod api;
mod ffi;

#[cfg(test)]
mod tests;
