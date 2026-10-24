// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-container
//!
//! Hardened readers for the three container formats every office document is
//! built from:
//!
//! | module  | container                         | used by                                  |
//! |---------|-----------------------------------|------------------------------------------|
//! | [`zip`] | ZIP / OPC packages / ODF packages | `.docx .xlsx .pptx .vsdx .odt .ods .odp` |
//! | [`cfb`] | OLE2 Compound File Binary (MS-CFB)| `.doc .xls .ppt .vsd .msg`, `vbaProject.bin`, OLE objects |
//! | [`xml`] | XML 1.0 (pull parser, no DTD)     | every XML part of OPC/ODF                |
//!
//! Every reader treats its input as hostile:
//!
//! * all offsets are bounds-checked (`get()` instead of indexing; checked arithmetic);
//! * every chain/tree walk is cycle-checked and length-limited;
//! * every allocation is bounded by an explicit, caller-visible limit;
//! * decompression is metered against a shared [`budget::InflateBudget`]
//!   so nested archives cannot multiply their footprint;
//! * XML `DOCTYPE` declarations are refused outright, which removes XXE and
//!   entity-expansion ("billion laughs") attacks at the root.
//!
//! The crate performs no I/O, spawns no threads and reads no clocks, so the
//! very same code runs inside the `wasm32-unknown-unknown` parser sandbox.

#![forbid(unsafe_code)]

pub mod budget;
pub mod bytes;
pub mod cfb;
pub mod codepages;
pub mod crypto;
pub mod inflate;
pub mod jet;
pub mod text;
pub mod xml;
pub mod zip;

pub use budget::{BudgetExceeded, InflateBudget};
