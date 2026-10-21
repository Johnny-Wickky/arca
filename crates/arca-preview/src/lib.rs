// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-preview — read-only content extraction
//!
//! Turns the bytes of an office document into a [`Preview`]: sections (body,
//! sheets, slides) made of paragraphs, tables and cell grids of **plain,
//! sanitised text**. Nothing in a document is ever executed, rendered by a
//! third-party engine, fetched or decoded as an image:
//!
//! * macros, OLE objects, ActiveX, scripts, fields and external references are
//!   not interpreted — their *text* is irrelevant to the preview;
//! * pictures become placeholders ([`model::ImageInfo`]);
//! * links are shown as text, never followed;
//! * text hidden by the author (white-on-white, `w:vanish`, hidden sheets and
//!   slides, tracked deletions) is **shown and labelled** instead of hidden;
//! * every string passes through [`sink::Sink`], which removes control and
//!   invisible characters (Trojan-Source bidi overrides, zero-width and Unicode
//!   tag characters) and enforces the output budget.
//!
//! Like `arca-xray` this crate performs no I/O and is built to run inside the
//! `arca-sandbox` WebAssembly guest, where a bug in a parser is contained.
//!
//! ```
//! use arca_preview::{PreviewPolicy, preview};
//!
//! let p = preview(b"Hello\n\nworld", &PreviewPolicy::default()).unwrap();
//! assert_eq!(p.sections.len(), 1);
//! ```

#![forbid(unsafe_code)]

mod bidi;
mod detect;
mod doc;
mod docx;
mod export;
mod listfmt;
pub mod model;
mod msg;
mod numfmt;
mod odf;
mod opc;
mod pdf;
mod plain;
mod ppt;
mod pptx;
mod rtf;
pub mod sink;
mod vsdx;
mod xls;
mod xlsx;
mod xml;

#[cfg(test)]
mod testkit;

use arca_container::cfb::CfbLimits;
use arca_container::xml::XmlLimits;
use arca_container::zip::ZipLimits;

pub use export::{LegacyOptions, sanitize_legacy};
pub use model::{FileFormat, Preview};

/// Resource limits for one preview.
///
/// The output limits are sized so that the wire encoding of a worst-case
/// preview stays well below the sandbox's 64 MiB output cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPolicy {
    /// Largest accepted input.
    pub max_input_bytes: usize,
    /// ZIP limits.
    pub zip: ZipLimits,
    /// OLE2 limits.
    pub cfb: CfbLimits,
    /// XML limits.
    pub xml: XmlLimits,
    /// Total bytes all decompressions may produce.
    pub inflate_budget: u64,
    /// Aggregate compression ratio bound.
    pub aggregate_ratio: u64,
    /// Maximum number of sections (sheets, slides, ...).
    pub max_sections: usize,
    /// Maximum number of blocks in the whole preview.
    pub max_blocks: usize,
    /// Maximum number of text bytes in the whole preview.
    pub max_chars: usize,
    /// Maximum number of items (runs, cells, rows, blocks) in the whole preview.
    pub max_items: usize,
    /// Maximum number of cells in one grid.
    pub max_grid_cells: usize,
    /// Maximum number of columns of a grid.
    pub max_grid_cols: u32,
    /// Maximum number of rows of a grid.
    pub max_grid_rows: u32,
    /// Maximum number of runs in one paragraph.
    pub max_runs_per_paragraph: usize,
    /// Maximum text bytes in one run or cell.
    pub max_cell_bytes: usize,
    /// Maximum bytes of a link target.
    pub max_href_bytes: usize,
    /// Include content the document hides (hidden runs, sheets, slides, rows), labelled as
    /// hidden. When `false` it is left out and counted in a `HiddenOmitted` notice.
    pub reveal_hidden: bool,
}

impl Default for PreviewPolicy {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 << 20,
            zip: ZipLimits::default(),
            cfb: CfbLimits::default(),
            xml: XmlLimits::default(),
            inflate_budget: 1 << 30,
            aggregate_ratio: 100,
            max_sections: 1_000,
            max_blocks: 250_000,
            max_chars: 6 << 20,
            max_items: 1_500_000,
            max_grid_cells: 500_000,
            max_grid_cols: 16_384,
            max_grid_rows: 1_048_576,
            max_runs_per_paragraph: 20_000,
            max_cell_bytes: 64 << 10,
            max_href_bytes: 2_048,
            reveal_hidden: true,
        }
    }
}

/// Why a preview could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    /// The input is larger than [`PreviewPolicy::max_input_bytes`].
    TooLarge(usize),
    /// The format is recognised (or not) but has no preview.
    Unsupported(FileFormat),
    /// The document is password protected.
    Encrypted,
    /// The container or a mandatory part is corrupt, or violates a safety limit.
    Malformed(String),
}

impl core::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLarge(n) => write!(f, "input exceeds the {n} byte limit"),
            Self::Unsupported(fmt) => write!(f, "no preview for {fmt:?}"),
            Self::Encrypted => f.write_str("the document is password protected"),
            Self::Malformed(why) => write!(f, "malformed document: {why}"),
        }
    }
}

impl core::error::Error for PreviewError {}

/// What [`sanitize_pdf`] may be asked to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfSanitizeOptions {
    /// Drop the document information (author, producer, dates); otherwise it is kept.
    pub strip_metadata: bool,
    /// Leave out review comments (notes, highlights, drawings); otherwise their look is kept.
    pub remove_comments: bool,
}

impl Default for PdfSanitizeOptions {
    fn default() -> Self {
        Self { strip_metadata: true, remove_comments: true }
    }
}

/// Builds a sterile copy of a PDF: a new file made only of what the pages need (their content,
/// fonts, pictures and the look of their annotations), with no scripts, actions, links, forms,
/// attachments, signatures, metadata or old revisions. Nothing is decoded or run.
///
/// Returns the new file and the changes made. The caller verifies the result (for the sandbox
/// guest that is `arca_xray::sanitize::sanitize_with`).
///
/// # Errors
/// [`PreviewError::Encrypted`] for a password-protected file; [`PreviewError::Malformed`] when
/// the file has no pages or is too large to rebuild.
pub fn sanitize_pdf(
    data: &[u8],
    options: &PdfSanitizeOptions,
) -> Result<(Vec<u8>, Vec<arca_xray::sanitize::SanitizeAction>), PreviewError> {
    let policy = PreviewPolicy::default();
    if data.len() > policy.max_input_bytes {
        return Err(PreviewError::TooLarge(policy.max_input_bytes));
    }
    pdf::clean(
        data,
        &policy,
        pdf::CleanOptions {
            strip_metadata: options.strip_metadata,
            remove_comments: options.remove_comments,
        },
    )
}

/// Extracts the visible content of `data`.
///
/// The format is determined from content only; the file name is never used.
///
/// # Errors
/// See [`PreviewError`]. Hostile input yields an error or a (possibly
/// truncated) preview with notices — never a panic.
pub fn preview(data: &[u8], policy: &PreviewPolicy) -> Result<Preview, PreviewError> {
    if data.len() > policy.max_input_bytes {
        return Err(PreviewError::TooLarge(policy.max_input_bytes));
    }
    let doc = detect::open(data, policy)?;
    let mut sink = sink::Sink::new(policy);
    let format = detect::extract(doc, &mut sink)?;
    Ok(sink.finish(format))
}
