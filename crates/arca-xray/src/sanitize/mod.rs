//! "Стерилизация в один тап" — One-Tap Sanitize.
//!
//! [`sanitize`] rebuilds an OOXML package (Word, Excel, PowerPoint, Visio) or an
//! OpenDocument package (Writer, Calc, Impress, Draw) without:
//!
//! * macro projects, macro sheets, ribbon callbacks, ActiveX controls,
//!   embedded OLE objects / packages and embedded fonts;
//! * every external relationship (remote templates, tracking images, frames,
//!   external workbooks, hyperlinks), data connections and query tables;
//! * DDE / `INCLUDE*` / `LINK` / `DATABASE` / `MACROBUTTON` fields (their
//!   last rendered result is kept as static text) and DDE / `WEBSERVICE` /
//!   `CALL` / `REGISTER` / `EXEC` / `RTD` / `IMAGE` formulas (the cached
//!   value is kept);
//! * PowerPoint "run program / macro" actions and Visio `RUNADDON`-style
//!   ShapeSheet formulas;
//! * tracked changes (all revisions are accepted), comments and their
//!   authors, revision-save ids, document variables, mail-merge sources;
//! * author, editor, company, manager, editing time, template path, local
//!   paths, printer settings, custom properties, custom XML and thumbnails.
//!
//! Unknown markup is preserved byte-for-byte (span-preserving rewriting). The
//! output is written deterministically (fixed timestamps) and re-scanned; the
//! call fails rather than return a copy that still contains active or remote
//! content.
//!
//! For OpenDocument the same goes for Basic and script libraries, event
//! listeners, DDE links, plug-ins, OLE frames, links that leave the package,
//! thumbnails, signatures and embedded objects; see `odf.rs`.
//!
//! Legacy binary formats (`.doc`, `.xls`, `.ppt`), RTF and PDF are not
//! rebuilt structurally: their safe export path is the flattened PDF produced
//! by the Phase 3 renderer. [`sanitize`] reports them as unsupported.

mod odf;
mod opc;

use core::fmt;

use arca_abi::{wire_enum, wire_struct};

use crate::format::FileFormat;
use crate::report::{Category, Severity, Verdict, XRayReport};
use crate::{ScanPolicy, scan};

/// What the sterilisation removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizeOptions {
    /// Accept all tracked changes (insertions kept, deletions removed).
    pub accept_revisions: bool,
    /// Remove comments and their authors.
    pub remove_comments: bool,
    /// Remove author / company / timing / path / printer metadata.
    pub strip_metadata: bool,
    /// Remove custom XML data parts.
    pub remove_custom_xml: bool,
}

impl Default for SanitizeOptions {
    fn default() -> Self {
        Self {
            accept_revisions: true,
            remove_comments: true,
            strip_metadata: true,
            remove_custom_xml: true,
        }
    }
}

/// Kinds of changes made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    /// A package part was removed.
    RemovedPart,
    /// An external relationship was removed.
    RemovedExternalReference,
    /// An internal relationship to a removed part was removed.
    RemovedRelationship,
    /// A dangerous field was converted to static text.
    NeutralizedField,
    /// A dangerous formula was removed (cached value kept).
    RemovedFormula,
    /// An element (action, control, OLE anchor, sheet ...) was removed.
    RemovedElement,
    /// Tracked changes were accepted.
    AcceptedRevisions,
    /// Comments were removed.
    RemovedComments,
    /// Metadata was removed or blanked.
    StrippedMetadata,
    /// A macro-enabled content type was converted to its macro-free variant.
    ConvertedContentType,
}
wire_enum!(ActionKind {
    RemovedPart = 0, RemovedExternalReference = 1, RemovedRelationship = 2, NeutralizedField = 3,
    RemovedFormula = 4, RemovedElement = 5, AcceptedRevisions = 6, RemovedComments = 7,
    StrippedMetadata = 8, ConvertedContentType = 9,
});

/// One change made by the sterilisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeAction {
    /// What was done.
    pub kind: ActionKind,
    /// Where / to what (part name, target, field instruction ...).
    pub target: String,
}
wire_struct!(SanitizeAction { kind, target });

/// Result of a successful sterilisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedDocument {
    /// Format of the produced file (macro-enabled inputs become macro-free).
    pub format: FileFormat,
    /// The sterile file.
    pub bytes: Vec<u8>,
    /// Changes made, in order.
    pub actions: Vec<SanitizeAction>,
    /// X-ray of the produced file (verification).
    pub verification: XRayReport,
}
wire_struct!(SanitizedDocument { format, bytes, actions, verification });

/// Why a sterile copy could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SanitizeError {
    /// The input itself was rejected by the X-ray (hostile structure).
    Rejected(String),
    /// The format cannot be rebuilt structurally (use the flattened PDF export).
    Unsupported(FileFormat),
    /// The package could not be rebuilt.
    Rebuild(String),
    /// The rebuilt file still contains active or remote content.
    Verification(String),
}

impl fmt::Display for SanitizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(r) => write!(f, "input rejected: {r}"),
            Self::Unsupported(fmt) => {
                write!(f, "{fmt:?} cannot be rebuilt; export a flattened PDF instead")
            }
            Self::Rebuild(r) => write!(f, "rebuild failed: {r}"),
            Self::Verification(r) => write!(f, "verification failed: {r}"),
        }
    }
}

impl core::error::Error for SanitizeError {}

/// Produces a sterile copy of an OOXML or OpenDocument document.
///
/// # Errors
/// See [`SanitizeError`].
pub fn sanitize(
    data: &[u8],
    policy: &ScanPolicy,
    options: &SanitizeOptions,
) -> Result<SanitizedDocument, SanitizeError> {
    let before = scan(data, policy);
    if before.verdict == Verdict::Rejected {
        return Err(SanitizeError::Rejected(before.rejection));
    }
    let is_odf = matches!(
        before.format,
        FileFormat::Odt
            | FileFormat::Ods
            | FileFormat::Odp
            | FileFormat::Odg
            | FileFormat::OdfOther
    );
    let supported = is_odf
        || (before.format.is_opc()
            && !matches!(before.format, FileFormat::Xlsb | FileFormat::Xps | FileFormat::OpcOther));
    if !supported {
        return Err(SanitizeError::Unsupported(before.format));
    }
    let (bytes, format, actions) = if is_odf {
        odf::rebuild(data, policy, options, before.format)?
    } else {
        opc::rebuild(data, policy, options, before.format)?
    };
    let verification = scan(&bytes, policy);
    if verification.verdict == Verdict::Rejected {
        return Err(SanitizeError::Verification(format!(
            "rebuilt file rejected: {}",
            verification.rejection
        )));
    }
    // A "sterile" copy that still contains parts the scanner could not read
    // (encrypted, unparsable, beyond a limit) is not verified sterile.
    if verification.verdict == Verdict::Unverified {
        let gaps: Vec<String> = verification
            .findings
            .iter()
            .filter(|f| f.kind.limits_coverage())
            .map(|f| format!("{:?} at {}", f.kind, f.location))
            .collect();
        return Err(SanitizeError::Verification(format!(
            "rebuilt file could not be fully inspected: {}",
            gaps.join("; ")
        )));
    }
    let residual: Vec<String> = verification
        .findings
        .iter()
        .filter(|f| {
            (f.kind.category() == Category::ActiveContent && f.severity >= Severity::Medium)
                || (f.kind.category() == Category::RemoteContent && f.severity >= Severity::Medium)
        })
        .map(|f| format!("{:?} at {}", f.kind, f.location))
        .collect();
    if !residual.is_empty() {
        return Err(SanitizeError::Verification(residual.join("; ")));
    }
    Ok(SanitizedDocument { format, bytes, actions, verification })
}

#[cfg(test)]
mod tests;
