// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-xray — the "Рентген безопасности" (Threat X-Ray)
//!
//! [`scan`] takes the raw bytes of a file and returns an [`XRayReport`]:
//!
//! 1. **Identification by content** — magic bytes, then container structure
//!    (OPC content types, ODF `mimetype`, OLE2 stream names). The file name
//!    and extension are never consulted.
//! 2. **Structural hardening** — Zip-Slip, Zip bombs (per-member ratio,
//!    lying sizes, overlapping members, shared budget across nesting),
//!    XML `DOCTYPE`/entities, OLE2 chain/tree cycles. Any of these rejects
//!    the file outright.
//! 3. **Active content inventory** — VBA (source decompressed per MS-OVBA,
//!    with auto-exec / suspicious keyword / IOC analysis), Excel 4.0 macro
//!    sheets and `Auto_Open` names, DDE fields and formulas, OLE packages,
//!    exploit-prone OLE classes (Equation Editor, URL moniker, MSCOMCTL),
//!    ActiveX, ribbon callbacks, PowerPoint/Visio program actions, ODF
//!    scripts, RTF objects, PDF JavaScript/launch actions.
//! 4. **Remote content inventory** — every relationship with
//!    `TargetMode="External"`, remote templates, tracking images, frames,
//!    UNC paths (NTLM leaks), abusable protocol handlers, data connections,
//!    `INCLUDEPICTURE`/`INCLUDETEXT` fields.
//! 5. **Forensic metadata** — authors, company, local paths, printers,
//!    editing time and sessions, revision authors, account identifiers.
//!
//! Nothing found is ever executed or fetched. The scanner performs no I/O and
//! is designed to run inside the `arca-sandbox` WebAssembly guest.

#![forbid(unsafe_code)]

mod fields;
pub mod format;
mod formulas;
mod image;
mod macro_analysis;
mod odf;
mod ole;
mod ooxml;
mod pdf;
pub mod report;
mod rtf;
pub mod sanitize;
mod textfmt;
pub mod url;
mod xmlwalk;

use arca_container::InflateBudget;
use arca_container::cfb::CfbLimits;
use arca_container::xml::XmlLimits;
use arca_container::zip::{ZipArchive, ZipError, ZipLimits};

pub use format::{Family, FileFormat};
pub use report::{
    Category, EmbeddedKind, EmbeddedObject, ExternalReference, Finding, MacroLanguage, MacroModule,
    MetaKey, MetadataField, RefKind, ScanStats, Severity, TargetClass, ThreatKind, Verdict,
    XRayReport,
};

use format::Magic;
use report::ReportBuilder;

/// Resource limits for one scan. The defaults are sized for the sandbox's
/// input-proportional memory limit (`arca_sandbox::SandboxPolicy::memory_limit_for`);
/// a part that would not fit there ends the run with a resource error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPolicy {
    /// Largest accepted input.
    pub max_input_bytes: usize,
    /// ZIP limits (ratio 100:1 per the specification).
    pub zip: ZipLimits,
    /// OLE2 limits.
    pub cfb: CfbLimits,
    /// XML limits (depth 64 per the specification).
    pub xml: XmlLimits,
    /// Total bytes all decompressions of one document may produce.
    pub inflate_budget: u64,
    /// Aggregate compression ratio across the whole document.
    pub aggregate_ratio: u64,
    /// Maximum nesting of embedded containers.
    pub max_nesting: usize,
    /// Maximum source size kept per macro module.
    pub max_macro_source_bytes: usize,
    /// Maximum number of findings / metadata entries.
    pub max_findings: usize,
    /// Maximum number of PDF streams decompressed.
    pub max_pdf_streams: usize,
}

impl Default for ScanPolicy {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 << 20,
            zip: ZipLimits::default(),
            cfb: CfbLimits::default(),
            xml: XmlLimits::default(),
            inflate_budget: 1 << 30,
            aggregate_ratio: 100,
            max_nesting: 3,
            max_macro_source_bytes: 1 << 20,
            max_findings: 5_000,
            max_pdf_streams: 4_096,
        }
    }
}

/// Fatal outcome that rejects the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Abort {
    pub(crate) kind: ThreatKind,
    pub(crate) location: String,
    pub(crate) reason: String,
}

impl Abort {
    pub(crate) fn new(kind: ThreatKind, location: &str, reason: impl Into<String>) -> Self {
        Self { kind, location: location.to_owned(), reason: reason.into() }
    }
}

/// Scan state shared by all format scanners.
pub(crate) struct Ctx<'p> {
    pub(crate) policy: &'p ScanPolicy,
    pub(crate) budget: InflateBudget,
    pub(crate) report: ReportBuilder,
    pub(crate) depth: usize,
    /// Format of the top-level file once identified (kept if it is later rejected).
    pub(crate) top_format: FileFormat,
}

impl Ctx<'_> {
    /// Records the identified format of the container at the current depth.
    pub(crate) fn identified(&mut self, f: FileFormat) {
        if self.depth == 0 {
            self.top_format = f;
        }
    }

    /// Joins a location prefix and a member name.
    pub(crate) fn join(prefix: &str, name: &str) -> String {
        if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") }
    }

    /// Scans a nested container (embedded document, OLE object, ...).
    ///
    /// Structural attacks inside nested containers still reject the whole
    /// document; plain corruption only produces a finding.
    pub(crate) fn scan_nested(&mut self, data: &[u8], location: &str) -> Result<FileFormat, Abort> {
        if self.depth >= self.policy.max_nesting {
            self.report.add(
                ThreatKind::ScanLimitReached,
                location,
                "maximum nesting depth reached; content not inspected",
            );
            return Ok(FileFormat::Unknown);
        }
        self.depth += 1;
        self.report.stats.max_nesting = self.report.stats.max_nesting.max(self.depth as u64);
        let result = dispatch(self, data, location);
        self.depth -= 1;
        match result {
            Ok(f) => Ok(f),
            Err(a) if is_fatal_kind(a.kind) => Err(a),
            Err(a) => {
                self.report.add(ThreatKind::MalformedContainer, &a.location, a.reason);
                Ok(FileFormat::Unknown)
            }
        }
    }

    /// Reads a ZIP member, translating container errors into findings or aborts.
    pub(crate) fn read_member(
        &mut self,
        zip: &ZipArchive<'_>,
        entry: &arca_container::zip::ZipEntry,
        location: &str,
    ) -> Result<Option<Vec<u8>>, Abort> {
        let before = self.budget.produced();
        let result = zip.read(entry, &mut self.budget);
        self.report.stats.bytes_inflated = self
            .report
            .stats
            .bytes_inflated
            .saturating_add(self.budget.produced().saturating_sub(before));
        match result {
            Ok(data) => Ok(Some(data)),
            Err(ZipError::Encrypted(_)) => {
                self.report.add(
                    ThreatKind::EncryptedMember,
                    location,
                    "encrypted archive member was not inspected",
                );
                Ok(None)
            }
            Err(ZipError::UnsupportedCompression { method, .. }) => {
                self.report.add(
                    ThreatKind::UnusualCompression,
                    location,
                    format!("compression method {method} is not supported; member skipped"),
                );
                Ok(None)
            }
            Err(e) => Err(zip_abort(&e, location)),
        }
    }
}

/// Kinds that reject the document even when found in nested content.
pub(crate) const fn is_fatal_kind(kind: ThreatKind) -> bool {
    matches!(
        kind,
        ThreatKind::CompressionBomb
            | ThreatKind::PathTraversal
            | ThreatKind::ParserDifferential
            | ThreatKind::XmlEntityAttack
            | ThreatKind::DisguisedExecutable
    )
}

/// Maps a ZIP error to an abort with the appropriate threat kind.
pub(crate) fn zip_abort(e: &ZipError, location: &str) -> Abort {
    use arca_container::zip::name::NameRejection as N;
    let kind = match e {
        ZipError::CompressionBomb { .. }
        | ZipError::Budget(_)
        | ZipError::SizeMismatch(_)
        | ZipError::EntryTooLarge { .. }
        | ZipError::TooManyEntries { .. } => ThreatKind::CompressionBomb,
        ZipError::InvalidName {
            reason: N::ParentTraversal | N::Absolute | N::DriveLetter | N::Backslash,
            ..
        }
        | ZipError::Symlink(_) => ThreatKind::PathTraversal,
        ZipError::DuplicateName(_)
        | ZipError::LocalHeaderMismatch(_)
        | ZipError::OverlappingEntries(..)
        | ZipError::CentralDirectoryMismatch
        | ZipError::InvalidName { .. } => ThreatKind::ParserDifferential,
        _ => ThreatKind::MalformedContainer,
    };
    Abort::new(kind, location, e.to_string())
}

/// Scans a file with the default policy.
#[must_use]
pub fn scan_default(data: &[u8]) -> XRayReport {
    scan(data, &ScanPolicy::default())
}

/// Scans `data` and returns the complete X-ray. Never panics, never fails:
/// hostile input yields a report with [`Verdict::Rejected`].
#[must_use]
pub fn scan(data: &[u8], policy: &ScanPolicy) -> XRayReport {
    let sha256 = arca_digest::sha256(data).to_string();
    let mut ctx = Ctx {
        policy,
        budget: InflateBudget::new(policy.inflate_budget, policy.aggregate_ratio, 16 << 20),
        report: ReportBuilder::new(policy.max_findings),
        depth: 0,
        top_format: FileFormat::Unknown,
    };

    let outcome = if data.len() > policy.max_input_bytes {
        Err(Abort::new(
            ThreatKind::ScanLimitReached,
            "",
            format!(
                "input of {} bytes exceeds the {} byte limit",
                data.len(),
                policy.max_input_bytes
            ),
        ))
    } else {
        dispatch(&mut ctx, data, "")
    };

    let (format, rejection) = match outcome {
        Ok(f) => (f, None),
        Err(abort) => {
            ctx.report.add_with(
                abort.kind,
                Severity::High.max(abort.kind.default_severity()),
                &abort.location,
                abort.reason.clone(),
            );
            (ctx.top_format, Some(abort.reason))
        }
    };
    if ctx.report.truncated {
        let dropped = ctx.report.dropped;
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            "",
            format!(
                "report limit reached: {dropped} item(s) are counted in the verdict but not listed"
            ),
        );
    }
    finish(ctx, format, rejection, sha256)
}

fn finish(
    ctx: Ctx<'_>,
    format: FileFormat,
    rejection: Option<String>,
    sha256: String,
) -> XRayReport {
    let unsupported = matches!(
        format,
        FileFormat::Unknown
            | FileFormat::Zip
            | FileFormat::Html
            | FileFormat::Executable
            | FileFormat::Msi
            | FileFormat::Empty
    );
    // The verdict is computed from facts accumulated over *every* observation,
    // never from the (capped) stored list: see `report::Facts`.
    let facts = ctx.report.facts();
    let (verdict, reason) = match rejection {
        Some(r) => (Verdict::Rejected, r),
        None if unsupported => {
            let why = match format {
                FileFormat::Executable | FileFormat::Msi => "executable content is never opened",
                FileFormat::Html => {
                    "HTML/HTA/MHT files are active-content containers and are not opened"
                }
                FileFormat::Empty => "the file is empty",
                FileFormat::Zip => "generic archives are not documents",
                _ => "unrecognised format",
            };
            (Verdict::Rejected, why.to_owned())
        }
        None if facts.active => (Verdict::ActiveContentNeutralized, String::new()),
        None if facts.coverage_gap => (Verdict::Unverified, String::new()),
        None if facts.tracker => (Verdict::HiddenTrackers, String::new()),
        None => (Verdict::Clean, String::new()),
    };
    let (findings, rest) = ctx.report.into_parts();
    XRayReport {
        format,
        verdict,
        rejection: reason,
        sha256,
        findings,
        macros: rest.macros,
        external_refs: rest.external_refs,
        metadata: rest.metadata,
        embedded: rest.embedded,
        stats: rest.stats,
    }
}

/// Identifies `data` and runs the matching scanner.
pub(crate) fn dispatch(
    ctx: &mut Ctx<'_>,
    data: &[u8],
    location: &str,
) -> Result<FileFormat, Abort> {
    ctx.report.stats.parts_scanned = ctx.report.stats.parts_scanned.saturating_add(1);
    match format::sniff(data) {
        Magic::Empty => Ok(FileFormat::Empty),
        Magic::Executable(what) => {
            if ctx.depth == 0 {
                Err(Abort::new(
                    ThreatKind::DisguisedExecutable,
                    location,
                    format!("{what} disguised as a document"),
                ))
            } else {
                ctx.report.add(ThreatKind::EmbeddedExecutable, location, what);
                Ok(FileFormat::Executable)
            }
        }
        Magic::Zip => scan_zip(ctx, data, location),
        Magic::Ole2 => ole::scan(ctx, data, location),
        Magic::Pdf => pdf::scan(ctx, data, location),
        Magic::Image(format) => image::scan(ctx, data, location, format),
        Magic::Rtf => rtf::scan(ctx, data, location),
        Magic::Jet(fmt) => {
            ole::scan_access(ctx, data, location);
            Ok(fmt)
        }
        Magic::Text => textfmt::scan(ctx, data, location),
        Magic::Unknown => {
            check_polyglot(ctx, data, location, false);
            Ok(FileFormat::Unknown)
        }
    }
}

fn scan_zip(ctx: &mut Ctx<'_>, data: &[u8], location: &str) -> Result<FileFormat, Abort> {
    let zip = ZipArchive::parse(data, ctx.policy.zip).map_err(|e| zip_abort(&e, location))?;
    for anomaly in zip.anomalies() {
        use arca_container::zip::ZipAnomaly as A;
        match anomaly {
            A::PrefixData { bytes } => ctx.report.add(
                ThreatKind::PolyglotFile,
                location,
                format!("{bytes} bytes precede the archive (self-extractor or polyglot)"),
            ),
            A::TrailingData { bytes } => ctx.report.add(
                ThreatKind::TrailingData,
                location,
                format!("{bytes} bytes follow the archive"),
            ),
            A::UnreferencedData { bytes } => ctx.report.add(
                ThreatKind::UnreferencedArchiveData,
                location,
                format!("{bytes} bytes between members are not referenced by any header"),
            ),
            A::ArchiveComment { bytes } => ctx.report.add_with(
                ThreatKind::TrailingData,
                Severity::Info,
                location,
                format!("archive comment of {bytes} bytes"),
            ),
            A::UnicodePathExtra(n) => ctx.report.add(
                ThreatKind::ParserDifferential,
                &Ctx::join(location, n),
                "Info-ZIP Unicode path extra field may name this member differently in other tools",
            ),
            A::EncryptedMember(n) => ctx.report.add(
                ThreatKind::EncryptedMember,
                &Ctx::join(location, n),
                "encrypted member",
            ),
            A::UnusualCompression { name, method } => ctx.report.add(
                ThreatKind::UnusualCompression,
                &Ctx::join(location, name),
                format!("method {method}"),
            ),
            A::Zip64 => {}
        }
    }
    if zip.entry("[Content_Types].xml").is_some() {
        return ooxml::scan(ctx, &zip, location);
    }
    if zip.entry("mimetype").is_some() || zip.entry("META-INF/manifest.xml").is_some() {
        return odf::scan(ctx, &zip, location);
    }
    // A generic archive: inventory executables, do not open.
    for e in zip.entries() {
        if let Some(ext) = e.extension()
            && ole::is_dangerous_extension(&ext)
        {
            ctx.report.add(
                ThreatKind::EmbeddedExecutable,
                &Ctx::join(location, e.name()),
                format!(".{ext} file inside archive"),
            );
        }
    }
    Ok(FileFormat::Zip)
}

/// Looks for a second format hidden in the same bytes.
pub(crate) fn check_polyglot(ctx: &mut Ctx<'_>, data: &[u8], location: &str, is_pdf: bool) {
    let head = data.get(..data.len().min(1024)).unwrap_or(data);
    if !is_pdf && memfind(head, b"%PDF-").is_some_and(|p| p > 0) {
        ctx.report.add(
            ThreatKind::PolyglotFile,
            location,
            "a PDF header is embedded near the start of the file",
        );
    }
    if !arca_container::zip::has_zip_signature(data)
        && let Some(base) = appended_zip_offset(data)
    {
        ctx.report.add(
            ThreatKind::PolyglotFile,
            location,
            format!("a ZIP archive is appended at offset {base}"),
        );
    }
}

/// Locates a ZIP archive whose end-of-central-directory record sits at the
/// end of `data` but whose offsets are relative to a later start (simple
/// concatenation polyglots). Returns the archive's start offset.
fn appended_zip_offset(data: &[u8]) -> Option<usize> {
    use arca_container::bytes::{u16_le, u32_le};
    let tail_start = data.len().saturating_sub(65_557);
    let tail = data.get(tail_start..)?;
    let rel = tail.windows(4).rposition(|w| w == b"PK\x05\x06")?;
    let eocd = tail_start + rel;
    let cd_size = usize::try_from(u32_le(data, eocd + 12)?).ok()?;
    let cd_offset = usize::try_from(u32_le(data, eocd + 16)?).ok()?;
    let entries = u16_le(data, eocd + 10)?;
    let cd_start = eocd.checked_sub(cd_size)?;
    let base = cd_start.checked_sub(cd_offset)?;
    let local_ok = u32_le(data, base) == Some(0x0403_4b50);
    let central_ok = entries == 0 || u32_le(data, cd_start) == Some(0x0201_4b50);
    (base > 0 && local_ok && central_ok).then_some(base)
}

/// `true` if `s` contains Unicode bidirectional override characters.
pub(crate) fn has_bidi(s: &str) -> bool {
    s.chars().any(arca_container::text::is_bidi_control)
}

/// Naive substring search (inputs are bounded by the caller).
pub(crate) fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests;
