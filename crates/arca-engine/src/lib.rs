// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-engine — what the apps talk to
//!
//! The Rust core exposes three operations on a document — **X-ray**, **preview** and
//! **sterilise** — and all of them run inside the `arca-sandbox` WebAssembly guest. This
//! crate is the safe host-side layer between that sandbox and a user interface:
//!
//! * [`Engine`] applies device-appropriate limits (memory, wall-clock time, input size),
//!   runs one operation, and turns every failure into a [`Status`];
//! * [`Host`] is the API an app calls: documents, jobs and results are *handles*
//!   (plain counters, never pointers), results are read back as JSON in small pages, and
//!   the document bytes never have to enter the app's garbage-collected heap;
//! * [`render`] is the single place where typed results become JSON;
//! * [`selftest`] runs a built-in document through the whole pipeline on the device.
//!
//! The crate contains no `unsafe` code. The JNI bridge (`arca-jni`) is a thin shell that
//! converts Java arguments and calls the methods of [`Host`].

#![forbid(unsafe_code)]

mod host;
pub mod json;
pub mod render;
pub mod schedule;
pub mod selftest;
pub mod stats;
pub mod support;

pub use host::{Engine, EngineConfig, Handle, Host, PreparedGuest, RunOutput, Status, guest_digest_of, op};
pub use render::{Counters, PreviewIndex};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_hardening;
