// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-jni — the JNI bridge of the Android app
//!
//! `libarca_jni.so` is the only native code the app loads. It embeds the pinned
//! WebAssembly guest and exposes [`arca_engine::Host`] to Java as a flat set of `static
//! native` methods (`org.arca.viewer.Native`). The crate is split so that almost nothing
//! needs `unsafe`:
//!
//! * [`api`] — the whole logic, written in safe Rust over plain Rust types and tested
//!   natively without a JVM;
//! * `ffi` — the JNI entry points: unwrapping Java arrays and file descriptors, calling
//!   [`api`], wrapping the result. It is the *only* file of the workspace outside the
//!   guest's export table that contains `unsafe` code; `tools/check-unsafe.sh` enforces
//!   that, and the JNI tests run on a real JVM with `-Xcheck:jni`.
//!
//! Document bytes can travel from a file descriptor straight into the engine, so a large
//! file never has to exist on the Java heap, and every handle is a counter, not a pointer.

#![deny(unsafe_code)]

pub mod api;
mod ffi;
