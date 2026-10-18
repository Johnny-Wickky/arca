//! The Threat X-Ray report: everything the "Рентген безопасности" HUD shows.
//!
//! All human-facing strings in the report are *technical details* in English
//! (paths, URLs, keywords). Every finding carries a typed [`ThreatKind`]; the
//! UI maps it to a localised title and explanation (RU/EN). This keeps the
//! core free of UI copy and lets translators work without touching Rust.

use std::collections::BTreeSet;

use arca_abi::wire::{Reader, Wire, WireError, Writer};
use arca_abi::{wire_enum, wire_struct};

use crate::format::FileFormat;

/// How dangerous a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Informational; no risk by itself.
    Info,
    /// Low risk (privacy, minor anomalies).
    Low,
    /// Medium risk (trackers, concealment).
    Medium,
    /// High risk (active content that needs user interaction, NTLM leaks).
    High,
    /// Critical (auto-executing code, known exploit primitives).
    Critical,
}
wire_enum!(Severity { Info = 0, Low = 1, Medium = 2, High = 3, Critical = 4 });

/// Broad family of a finding; drives the verdict and HUD grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Code or code-like content that office software would execute.
    ActiveContent,
    /// Content fetched from outside the file (trackers, remote templates).
    RemoteContent,
    /// Content hidden from the reader.
    Concealment,
    /// Personal or organisational metadata.
    PrivacyLeak,
    /// Container irregularities.
    Structural,
    /// Encrypted content that could not be inspected.
    Encryption,
}
wire_enum!(Category { ActiveContent = 0, RemoteContent = 1, Concealment = 2, PrivacyLeak = 3, Structural = 4, Encryption = 5 });

macro_rules! threat_kinds {
    ($( $variant:ident = $tag:literal => ($cat:ident, $sev:ident) ),* $(,)?) => {
        /// Typed finding identifiers. The UI localises these.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum ThreatKind { $(
            #[doc = concat!("`", stringify!($variant), "` — category ", stringify!($cat), ", default severity ", stringify!($sev), ".")]
            $variant,
        )* }

        impl ThreatKind {
            /// Category of the finding.
            #[must_use]
            pub const fn category(self) -> Category {
                match self { $( Self::$variant => Category::$cat, )* }
            }
            /// Default severity of the finding.
            #[must_use]
            pub const fn default_severity(self) -> Severity {
                match self { $( Self::$variant => Severity::$sev, )* }
            }
            /// Stable machine identifier (used for localisation keys).
            #[must_use]
            pub const fn id(self) -> &'static str {
                match self { $( Self::$variant => stringify!($variant), )* }
            }
        }

        impl Wire for ThreatKind {
            fn encode(&self, w: &mut Writer) {
                w.u16(match self { $( Self::$variant => $tag, )* });
            }
            fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
                match r.u16()? {
                    $( $tag => Ok(Self::$variant), )*
                    t => Err(WireError::InvalidTag(t)),
                }
            }
        }
    };
}

threat_kinds! {
    // ---- active content ---------------------------------------------------
    VbaMacros = 1 => (ActiveContent, High),
    MacroAutoExec = 2 => (ActiveContent, Critical),
    XlmMacroSheet = 3 => (ActiveContent, Critical),
    XlmAutoOpenName = 4 => (ActiveContent, Critical),
    StarBasicMacros = 5 => (ActiveContent, High),
    EmbeddedScript = 6 => (ActiveContent, High),
    DdeField = 7 => (ActiveContent, Critical),
    DdeFormula = 8 => (ActiveContent, Critical),
    DangerousFormula = 9 => (ActiveContent, High),
    OleObject = 10 => (ActiveContent, Medium),
    OlePackage = 11 => (ActiveContent, High),
    ExploitableOleClass = 12 => (ActiveContent, Critical),
    ActiveXControl = 13 => (ActiveContent, High),
    RibbonCallbacks = 14 => (ActiveContent, Medium),
    ProgramAction = 15 => (ActiveContent, High),
    MacroButtonField = 16 => (ActiveContent, Medium),
    PdfJavaScript = 17 => (ActiveContent, High),
    PdfLaunchAction = 18 => (ActiveContent, Critical),
    PdfAutoAction = 19 => (ActiveContent, Medium),
    PdfEmbeddedFile = 20 => (ActiveContent, Medium),
    PdfRichMedia = 21 => (ActiveContent, High),
    RtfEmbeddedObject = 22 => (ActiveContent, High),
    RtfAutoUpdateObject = 23 => (ActiveContent, Critical),
    EmbeddedExecutable = 24 => (ActiveContent, Critical),
    EmbeddedDocument = 25 => (ActiveContent, Low),
    OdfEventScript = 26 => (ActiveContent, High),
    SignedMacroProject = 27 => (ActiveContent, Info),
    HtmlActiveContent = 28 => (ActiveContent, High),
    CsvFormulaInjection = 29 => (ActiveContent, High),
    AltChunkImport = 30 => (ActiveContent, Medium),
    // ---- remote content ---------------------------------------------------
    RemoteTemplate = 100 => (RemoteContent, Critical),
    ExternalImage = 101 => (RemoteContent, Medium),
    ExternalFrame = 102 => (RemoteContent, High),
    ExternalObjectLink = 103 => (RemoteContent, Critical),
    ExternalWorkbookLink = 104 => (RemoteContent, High),
    ExternalDataConnection = 105 => (RemoteContent, High),
    RemoteFieldInclude = 106 => (RemoteContent, High),
    UncPathReference = 107 => (RemoteContent, High),
    DangerousProtocolUri = 108 => (RemoteContent, Critical),
    ExternalHyperlink = 109 => (RemoteContent, Info),
    XmlStylesheetReference = 110 => (RemoteContent, Medium),
    MailMergeDataSource = 111 => (RemoteContent, High),
    PdfRemoteAction = 112 => (RemoteContent, Medium),
    ExternalResource = 113 => (RemoteContent, Medium),
    HyperlinkBase = 114 => (RemoteContent, Low),
    // ---- concealment ------------------------------------------------------
    HiddenSheet = 200 => (Concealment, Low),
    VeryHiddenSheet = 201 => (Concealment, High),
    HiddenRowsOrColumns = 202 => (Concealment, Info),
    HiddenText = 203 => (Concealment, Low),
    InvisibleText = 204 => (Concealment, Medium),
    HiddenSlide = 205 => (Concealment, Info),
    OrphanedOleEntries = 206 => (Concealment, Medium),
    UnreferencedArchiveData = 207 => (Concealment, Medium),
    PolyglotFile = 208 => (Concealment, High),
    TrailingData = 209 => (Concealment, Low),
    DocumentVariables = 210 => (Concealment, Low),
    CustomXmlData = 211 => (Concealment, Info),
    StaleThumbnail = 212 => (Concealment, Info),
    BidiOverride = 213 => (Concealment, Medium),
    ObfuscatedName = 214 => (Concealment, Medium),
    // ---- privacy ----------------------------------------------------------
    AuthorMetadata = 300 => (PrivacyLeak, Info),
    LocalPathLeak = 301 => (PrivacyLeak, Low),
    PrinterMetadata = 302 => (PrivacyLeak, Low),
    TrackedChanges = 303 => (PrivacyLeak, Low),
    Comments = 304 => (PrivacyLeak, Info),
    PersonalIdentifier = 305 => (PrivacyLeak, Low),
    EditingSessionIds = 306 => (PrivacyLeak, Info),
    CustomProperties = 307 => (PrivacyLeak, Info),
    // ---- structural -------------------------------------------------------
    MalformedContainer = 400 => (Structural, Medium),
    UnusualCompression = 401 => (Structural, Low),
    MacroEnabledContentType = 402 => (Structural, Medium),
    ContentTypeMismatch = 403 => (Structural, Medium),
    DisguisedExecutable = 404 => (Structural, Critical),
    UnsupportedFormat = 405 => (Structural, Info),
    ScanLimitReached = 406 => (Structural, Medium),
    EmbeddedFont = 407 => (Structural, Info),
    CompressionBomb = 408 => (Structural, Critical),
    PathTraversal = 409 => (Structural, Critical),
    ParserDifferential = 410 => (Structural, High),
    XmlEntityAttack = 411 => (Structural, Critical),
    // ---- encryption -------------------------------------------------------
    EncryptedDocument = 500 => (Encryption, Medium),
    EncryptedMember = 501 => (Encryption, Medium),
}

/// One observation of the X-ray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What was found.
    pub kind: ThreatKind,
    /// Effective severity (may differ from the default).
    pub severity: Severity,
    /// Where (archive member, OLE stream path, PDF object ...).
    pub location: String,
    /// Technical detail (URL, keyword, field instruction ...), display-safe.
    pub detail: String,
    /// `true` if Arca blocks/never executes this content (always the
    /// case for active and remote content).
    pub neutralized: bool,
}
wire_struct!(Finding { kind, severity, location, detail, neutralized });

/// Overall assessment shown as the "safety index".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing active or remote was found (*Чистый*).
    Clean,
    /// Remote resources that would load automatically were found and blocked
    /// (*Содержит скрытые трекеры*).
    HiddenTrackers,
    /// Executable content was found and neutralised (*Обезврежена активная нагрузка*).
    ActiveContentNeutralized,
    /// The file is structurally hostile or not a document; it is not opened.
    Rejected,
}
wire_enum!(Verdict { Clean = 0, HiddenTrackers = 1, ActiveContentNeutralized = 2, Rejected = 3 });

/// Source language of an extracted macro module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroLanguage {
    /// Visual Basic for Applications.
    Vba,
    /// Excel 4.0 macro sheet formulas.
    Xlm,
    /// LibreOffice/StarOffice Basic.
    StarBasic,
    /// Python script inside an ODF package.
    Python,
    /// JavaScript (ODF scripts, PDF).
    JavaScript,
    /// BeanShell script inside an ODF package.
    BeanShell,
}
wire_enum!(MacroLanguage { Vba = 0, Xlm = 1, StarBasic = 2, Python = 3, JavaScript = 4, BeanShell = 5 });

/// A macro module extracted for safe, read-only display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroModule {
    /// Language.
    pub language: MacroLanguage,
    /// Location of the project/stream.
    pub location: String,
    /// Module name.
    pub name: String,
    /// Decompressed source text (never executed).
    pub source: String,
    /// `true` if the source was cut at the configured limit.
    pub truncated: bool,
    /// Auto-execution entry points found (e.g. `AutoOpen`, `Workbook_Open`).
    pub auto_exec: Vec<String>,
    /// Suspicious keywords found (e.g. `Shell`, `CreateObject`).
    pub suspicious: Vec<String>,
    /// Indicators of compromise (URLs, IPs, executable names).
    pub iocs: Vec<String>,
}
wire_struct!(MacroModule {
    language,
    location,
    name,
    source,
    truncated,
    auto_exec,
    suspicious,
    iocs
});

/// What a reference to an outside resource is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// Attached / remote template.
    Template,
    /// Image or other media.
    Image,
    /// HTML frame or sub-document.
    Frame,
    /// Clickable hyperlink.
    Hyperlink,
    /// OLE link / linked object.
    ObjectLink,
    /// External workbook / DDE link.
    WorkbookLink,
    /// Data connection or query.
    DataConnection,
    /// Field code (INCLUDEPICTURE, INCLUDETEXT, IMPORT ...).
    Field,
    /// XML stylesheet processing instruction.
    Stylesheet,
    /// PDF action target.
    PdfAction,
    /// Anything else.
    Other,
}
wire_enum!(RefKind {
    Template = 0, Image = 1, Frame = 2, Hyperlink = 3, ObjectLink = 4, WorkbookLink = 5,
    DataConnection = 6, Field = 7, Stylesheet = 8, PdfAction = 9, Other = 10,
});

/// Network class of a reference target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetClass {
    /// `http://` or `https://`.
    Web,
    /// UNC / SMB / WebDAV path (NTLM hash leak).
    Unc,
    /// Local file path on the sender's machine.
    LocalFile,
    /// Protocol handler known to be abusable (`ms-msdt:`, `search-ms:` ...).
    DangerousProtocol,
    /// `mailto:`.
    Mail,
    /// Any other scheme.
    OtherScheme,
    /// Relative or unclassifiable target.
    Relative,
}
wire_enum!(TargetClass { Web = 0, Unc = 1, LocalFile = 2, DangerousProtocol = 3, Mail = 4, OtherScheme = 5, Relative = 6 });

/// A blocked reference to a resource outside the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalReference {
    /// The target as written in the file (display-safe).
    pub target: String,
    /// Purpose.
    pub kind: RefKind,
    /// Network class.
    pub class: TargetClass,
    /// Where it was found.
    pub location: String,
    /// `true` if office software would fetch it without user interaction.
    pub auto_load: bool,
}
wire_struct!(ExternalReference { target, kind, class, location, auto_load });

/// Forensic metadata keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetaKey {
    /// Title.
    Title,
    /// Subject.
    Subject,
    /// Author / creator.
    Author,
    /// Last modified by.
    LastModifiedBy,
    /// Company.
    Company,
    /// Manager.
    Manager,
    /// Producing application.
    Application,
    /// Application version.
    AppVersion,
    /// Template name or path.
    Template,
    /// Revision number.
    Revision,
    /// Total editing time in minutes.
    TotalEditMinutes,
    /// Creation timestamp (ISO 8601).
    Created,
    /// Last saved timestamp.
    Modified,
    /// Last printed timestamp.
    LastPrinted,
    /// Keywords.
    Keywords,
    /// Description / comments.
    Description,
    /// Category.
    Category,
    /// Custom document property (`name` holds the property name).
    CustomProperty,
    /// Local file-system path of the sender.
    LocalPath,
    /// Printer name.
    Printer,
    /// Person (revision author, commenter, co-author, account id).
    Person,
    /// Number of distinct editing sessions (Word rsids).
    EditingSessions,
    /// Hyperlink base URL.
    HyperlinkBase,
    /// Generator string (ODF).
    Generator,
    /// Language.
    Language,
    /// Content status.
    ContentStatus,
    /// Machine / host name.
    Machine,
    /// Document security / protection flags.
    Security,
    /// Number of editing cycles (ODF).
    EditingCycles,
}
wire_enum!(MetaKey {
    Title = 0, Subject = 1, Author = 2, LastModifiedBy = 3, Company = 4, Manager = 5, Application = 6,
    AppVersion = 7, Template = 8, Revision = 9, TotalEditMinutes = 10, Created = 11, Modified = 12,
    LastPrinted = 13, Keywords = 14, Description = 15, Category = 16, CustomProperty = 17, LocalPath = 18,
    Printer = 19, Person = 20, EditingSessions = 21, HyperlinkBase = 22, Generator = 23, Language = 24,
    ContentStatus = 25, Machine = 26, Security = 27, EditingCycles = 28,
});

/// One metadata value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataField {
    /// Key.
    pub key: MetaKey,
    /// Property name for [`MetaKey::CustomProperty`], otherwise empty.
    pub name: String,
    /// Value (display-safe).
    pub value: String,
    /// Where it was found.
    pub source: String,
}
wire_struct!(MetadataField { key, name, value, source });

/// Kind of an embedded object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedKind {
    /// Generic OLE object.
    OleObject,
    /// OLE Packager object wrapping an arbitrary file.
    Package,
    /// A nested office document.
    Document,
    /// An executable or script file.
    Executable,
    /// ActiveX control.
    ActiveX,
    /// Embedded font.
    Font,
    /// Anything else.
    Other,
}
wire_enum!(EmbeddedKind { OleObject = 0, Package = 1, Document = 2, Executable = 3, ActiveX = 4, Font = 5, Other = 6 });

/// An embedded object inventory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedObject {
    /// Location inside the file.
    pub location: String,
    /// Kind.
    pub kind: EmbeddedKind,
    /// OLE class name / ProgID if known.
    pub class_name: String,
    /// CLSID if known.
    pub clsid: String,
    /// Original file name (OLE Packager), if any.
    pub file_name: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the object bytes (for threat-intel lookups by the user).
    pub sha256: String,
}
wire_struct!(EmbeddedObject { location, kind, class_name, clsid, file_name, size, sha256 });

/// Work counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Container members / streams examined.
    pub parts_scanned: u64,
    /// XML events processed.
    pub xml_events: u64,
    /// Bytes produced by decompression.
    pub bytes_inflated: u64,
    /// Deepest nesting of embedded containers.
    pub max_nesting: u64,
}
wire_struct!(ScanStats { parts_scanned, xml_events, bytes_inflated, max_nesting });

/// The complete X-ray of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XRayReport {
    /// Identified format (from magic bytes and structure, never the extension).
    pub format: FileFormat,
    /// Overall verdict.
    pub verdict: Verdict,
    /// Reason the file was rejected (empty unless `verdict == Rejected`).
    pub rejection: String,
    /// SHA-256 of the input, lowercase hex.
    pub sha256: String,
    /// Findings, most severe first.
    pub findings: Vec<Finding>,
    /// Extracted macro sources.
    pub macros: Vec<MacroModule>,
    /// Blocked external references.
    pub external_refs: Vec<ExternalReference>,
    /// Forensic metadata.
    pub metadata: Vec<MetadataField>,
    /// Embedded objects.
    pub embedded: Vec<EmbeddedObject>,
    /// Counters.
    pub stats: ScanStats,
}
wire_struct!(XRayReport {
    format,
    verdict,
    rejection,
    sha256,
    findings,
    macros,
    external_refs,
    metadata,
    embedded,
    stats
});

impl XRayReport {
    /// Highest severity among all findings.
    #[must_use]
    pub fn max_severity(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }

    /// `true` if any finding of `kind` exists.
    #[must_use]
    pub fn has(&self, kind: ThreatKind) -> bool {
        self.findings.iter().any(|f| f.kind == kind)
    }

    /// Findings of `kind`.
    pub fn of_kind(&self, kind: ThreatKind) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.kind == kind)
    }

    /// First metadata value for `key`.
    #[must_use]
    pub fn meta(&self, key: MetaKey) -> Option<&str> {
        self.metadata.iter().find(|m| m.key == key).map(|m| m.value.as_str())
    }
}

/// Accumulates findings with de-duplication and caps.
#[derive(Debug)]
pub(crate) struct ReportBuilder {
    findings: Vec<Finding>,
    seen: BTreeSet<(ThreatKind, String, String)>,
    pub(crate) macros: Vec<MacroModule>,
    pub(crate) external_refs: Vec<ExternalReference>,
    pub(crate) metadata: Vec<MetadataField>,
    meta_seen: BTreeSet<(MetaKey, String, String)>,
    pub(crate) embedded: Vec<EmbeddedObject>,
    pub(crate) stats: ScanStats,
    max_findings: usize,
    pub(crate) truncated: bool,
}

impl ReportBuilder {
    pub(crate) fn new(max_findings: usize) -> Self {
        Self {
            findings: Vec::new(),
            seen: BTreeSet::new(),
            macros: Vec::new(),
            external_refs: Vec::new(),
            metadata: Vec::new(),
            meta_seen: BTreeSet::new(),
            embedded: Vec::new(),
            stats: ScanStats::default(),
            max_findings,
            truncated: false,
        }
    }

    /// Records a finding with its default severity.
    pub(crate) fn add(&mut self, kind: ThreatKind, location: &str, detail: impl Into<String>) {
        self.add_with(kind, kind.default_severity(), location, detail);
    }

    /// Records a finding with an explicit severity.
    pub(crate) fn add_with(
        &mut self,
        kind: ThreatKind,
        severity: Severity,
        location: &str,
        detail: impl Into<String>,
    ) {
        let detail = arca_container::text::display_safe(&detail.into(), 512);
        let location = arca_container::text::display_safe(location, 256);
        if !self.seen.insert((kind, location.clone(), detail.clone())) {
            return;
        }
        if self.findings.len() >= self.max_findings {
            self.truncated = true;
            return;
        }
        let neutralized =
            matches!(kind.category(), Category::ActiveContent | Category::RemoteContent);
        self.findings.push(Finding { kind, severity, location, detail, neutralized });
    }

    pub(crate) fn meta(&mut self, key: MetaKey, name: &str, value: &str, source: &str) {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        let value = arca_container::text::display_safe(value, 1024);
        if !self.meta_seen.insert((key, name.to_owned(), value.clone())) {
            return;
        }
        if self.metadata.len() >= self.max_findings {
            self.truncated = true;
            return;
        }
        self.metadata.push(MetadataField {
            key,
            name: arca_container::text::display_safe(name, 256),
            value,
            source: source.to_owned(),
        });
    }

    pub(crate) fn external(&mut self, r: ExternalReference) {
        if self.external_refs.len() >= self.max_findings {
            self.truncated = true;
            return;
        }
        if !self.external_refs.contains(&r) {
            self.external_refs.push(r);
        }
    }

    pub(crate) fn has_category(&self, cat: Category, min: Severity) -> bool {
        self.findings.iter().any(|f| f.kind.category() == cat && f.severity >= min)
    }

    pub(crate) fn has(&self, kind: ThreatKind) -> bool {
        self.findings.iter().any(|f| f.kind == kind)
    }

    pub(crate) fn into_parts(mut self) -> (Vec<Finding>, Self) {
        let mut findings = core::mem::take(&mut self.findings);
        findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.kind.cmp(&b.kind)));
        (findings, self)
    }
}
