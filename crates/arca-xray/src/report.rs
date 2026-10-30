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
            /// Every kind, in declaration order (used by the completeness tests).
            pub const ALL: &'static [ThreatKind] = &[ $( Self::$variant, )* ];

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
    ImageHiddenFrames = 215 => (Concealment, Low),
    // ---- privacy ----------------------------------------------------------
    AuthorMetadata = 300 => (PrivacyLeak, Info),
    LocalPathLeak = 301 => (PrivacyLeak, Low),
    PrinterMetadata = 302 => (PrivacyLeak, Low),
    TrackedChanges = 303 => (PrivacyLeak, Low),
    Comments = 304 => (PrivacyLeak, Info),
    PersonalIdentifier = 305 => (PrivacyLeak, Low),
    EditingSessionIds = 306 => (PrivacyLeak, Info),
    CustomProperties = 307 => (PrivacyLeak, Info),
    ImageGpsLocation = 308 => (PrivacyLeak, High),
    ImageCameraIdentity = 309 => (PrivacyLeak, Low),
    ImageEmbeddedThumbnail = 310 => (PrivacyLeak, Low),
    ImageHiddenMetadata = 311 => (PrivacyLeak, Info),
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
    OversizedImage = 412 => (Structural, High),
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

impl ThreatKind {
    /// `true` if the finding means, *whatever its severity*, that part of the file was **not**
    /// (or could not be) inspected: encrypted content, skipped members, parts that failed to
    /// parse, or a scan limit that cut the work short.
    ///
    /// Such a file can never be reported as [`Verdict::Clean`]: absence of
    /// findings in a region the scanner could not read proves nothing, and a
    /// hostile author controls exactly which regions fail to parse.
    ///
    /// Kinds whose meaning depends on how they were raised are decided by
    /// [`limits_coverage_at`](Self::limits_coverage_at).
    #[must_use]
    pub const fn limits_coverage(self) -> bool {
        matches!(
            self,
            Self::MalformedContainer
                | Self::UnusualCompression
                | Self::ScanLimitReached
                | Self::EncryptedDocument
                | Self::EncryptedMember
                // The scanner and another reader would see different documents (a second name
                // for a member, two formats in one package, two ends of one stream): what the
                // other reader sees was not what was inspected.
                | Self::ParserDifferential
                // Bytes the container does not account for, or a second format living in the
                // same bytes, are exactly where a payload hides; they were not inspected.
                | Self::PolyglotFile
                | Self::UnreferencedArchiveData
                // Directory entries nothing links to are never walked by the scanner.
                | Self::OrphanedOleEntries
                // A file that disguises its markup or its names from lexical scans is by that
                // act one the scanner cannot be sure it read the way its consumers will.
                | Self::ObfuscatedName
        )
    }

    /// Like [`limits_coverage`](Self::limits_coverage), for a finding raised with `severity`.
    ///
    /// * `TrailingData` is a coverage gap from `Low` up. The scanner raises it at `Info` only
    ///   for bytes it has verified to be zero padding (or a plain archive comment).
    /// * `ContentTypeMismatch` is a gap from `Medium` up: a part whose declared type,
    ///   relationship type and name disagree was read by the scanner in one way and may be read
    ///   in another.
    #[must_use]
    pub fn limits_coverage_at(self, severity: Severity) -> bool {
        self.limits_coverage()
            || match self {
                Self::TrailingData => severity >= Severity::Low,
                Self::ContentTypeMismatch => severity >= Severity::Medium,
                _ => false,
            }
    }
}

/// Overall assessment shown as the "safety index".
///
/// Precedence when several apply: `Rejected` > `ActiveContentNeutralized` >
/// `Unverified` > `HiddenTrackers` > `Clean`.
///
/// `Clean` is a positive claim: every byte was accounted for and every interpretation the
/// scanner relied on was unambiguous. Anything the scanner could not read, did not interpret,
/// or read in a way another consumer might not (see [`ThreatKind::limits_coverage_at`]) makes
/// the verdict `Unverified` at best.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing active or remote was found, and every part of the file was
    /// inspected (*Чистый*).
    Clean,
    /// Remote resources that would load automatically were found and blocked
    /// (*Содержит скрытые трекеры*).
    HiddenTrackers,
    /// Executable content was found and neutralised (*Обезврежена активная нагрузка*).
    ActiveContentNeutralized,
    /// The file is structurally hostile or not a document; it is not opened.
    Rejected,
    /// No active content was seen, but part of the file could not be inspected
    /// (encrypted, unreadable or beyond a scan limit), so it cannot be called
    /// clean (*Не удалось проверить полностью*).
    Unverified,
}
wire_enum!(Verdict {
    Clean = 0, HiddenTrackers = 1, ActiveContentNeutralized = 2, Rejected = 3, Unverified = 4,
});

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

/// What a macro module is for, judged from its source text alone (it is never run).
///
/// This is a static judgement of what the code *says it does*, not a guarantee: a hostile author
/// can hide intent. It therefore never changes the verdict (a file with a macro is always at least
/// [`Verdict::ActiveContentNeutralized`]); it only lets the report say what the code does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MacroIntent {
    /// Works on the document's own content only: no programs, files, network, registry or other
    /// components were seen, and nothing looks concealed.
    Benign,
    /// Reads its surroundings, starts by itself, hides its text or uses other components, but no
    /// program start, download, file or registry write was seen.
    Suspicious,
    /// Starts programs, downloads or writes files, injects code or modifies the system.
    Malicious,
}
wire_enum!(MacroIntent { Benign = 0, Suspicious = 1, Malicious = 2 });

impl MacroIntent {
    /// The severity of the finding that reports a macro of this intent.
    ///
    /// A macro is active content whatever it says it does, and the verdict treats active content of
    /// `Medium` severity or more as "neutralised", never "clean": a benign-looking macro keeps the
    /// floor of `Medium`, so that a misjudged module can never make a file look clean.
    #[must_use]
    pub const fn macro_severity(self) -> Severity {
        match self {
            Self::Benign => Severity::Medium,
            Self::Suspicious => Severity::High,
            Self::Malicious => Severity::Critical,
        }
    }
}

/// One thing a macro does (or says it does), found in its source. The UI turns the kind into
/// text in the user's language; the core carries no prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BehaviorKind {
    // ---- ordinary work on the document (benign) --------------------------------------------
    /// Counts rows, columns, pages, words or paragraphs.
    CountsContent,
    /// Formats text or cells (fonts, colours, number formats, borders, widths).
    FormatsContent,
    /// Reads or writes cells and ranges of the workbook.
    EditsCells,
    /// Reads or writes the text of the document.
    EditsText,
    // ---- suspicious ------------------------------------------------------------------------
    /// Starts by itself when the file is opened, closed or changed.
    AutoRuns,
    /// Reads environment variables.
    ReadsEnvironment,
    /// Asks for the version of the operating system or the application.
    ChecksSystemVersion,
    /// Reads local paths, folders or the user's name.
    ReadsLocalPaths,
    /// Creates or gets other components (`CreateObject`, `GetObject`).
    UsesComObject,
    /// Builds text from pieces or encodes it so that it cannot be read as written.
    ObfuscatedCode,
    /// Runs a procedure or text it names at run time (`Application.Run`, `Eval`, `OnTime`).
    RunsByName,
    /// Declares functions of system libraries.
    CallsNativeApi,
    /// Sends key presses to other windows.
    SendsKeys,
    /// Reads the registry.
    ReadsRegistry,
    /// Keeps data in places readers do not look at (document variables and properties).
    HiddenPayloadStore,
    /// Contains a web address or an IP address.
    ContainsAddress,
    /// Names a program or script file.
    MentionsProgram,
    /// Is an Excel 4.0 macro sheet, which runs formulas as code.
    XlmMacro,
    /// The source was cut at the limit, so the rest was not read.
    NotFullyRead,
    /// Opens an address in the browser or sends data from the document.
    OpensAddress,
    // ---- malicious -------------------------------------------------------------------------
    /// Starts a program or a command shell.
    RunsProgram,
    /// Downloads a file or data from the network.
    DownloadsFile,
    /// Writes or copies files.
    WritesFiles,
    /// Deletes files.
    DeletesFiles,
    /// Writes to the registry.
    WritesRegistry,
    /// Allocates executable memory or starts threads in it (shellcode).
    InjectsCode,
    /// Creates a scheduled task or another way to start again later.
    ScheduledTask,
    /// Changes its own code or the macro project at run time.
    SelfModifiesCode,
    /// Compiled code is present but the readable source is empty or does not match.
    SourceMismatch,
    /// Calls functions of a native library from an Excel 4.0 macro sheet.
    CallsNativeCode,
    /// Uses a technique known from exploits of document readers (heap spray, overflow of a
    /// built-in function).
    ExploitPrimitive,
}
wire_enum!(BehaviorKind {
    CountsContent = 0, FormatsContent = 1, EditsCells = 2, EditsText = 3,
    AutoRuns = 10, ReadsEnvironment = 11, ChecksSystemVersion = 12, ReadsLocalPaths = 13,
    UsesComObject = 14, ObfuscatedCode = 15, RunsByName = 16, CallsNativeApi = 17, SendsKeys = 18,
    ReadsRegistry = 19, HiddenPayloadStore = 20, ContainsAddress = 21, MentionsProgram = 22,
    XlmMacro = 23, NotFullyRead = 24, OpensAddress = 25,
    RunsProgram = 40, DownloadsFile = 41, WritesFiles = 42, DeletesFiles = 43, WritesRegistry = 44,
    InjectsCode = 45, ScheduledTask = 46, SelfModifiesCode = 47, SourceMismatch = 48,
    CallsNativeCode = 49, ExploitPrimitive = 50,
});

impl BehaviorKind {
    /// The intent this behaviour points to: what a module that does it, and nothing worse, is.
    #[must_use]
    pub const fn intent(self) -> MacroIntent {
        match self {
            Self::CountsContent | Self::FormatsContent | Self::EditsCells | Self::EditsText => {
                MacroIntent::Benign
            }
            Self::AutoRuns
            | Self::ReadsEnvironment
            | Self::ChecksSystemVersion
            | Self::ReadsLocalPaths
            | Self::UsesComObject
            | Self::ObfuscatedCode
            | Self::RunsByName
            | Self::CallsNativeApi
            | Self::SendsKeys
            | Self::ReadsRegistry
            | Self::HiddenPayloadStore
            | Self::ContainsAddress
            | Self::MentionsProgram
            | Self::XlmMacro
            | Self::NotFullyRead
            | Self::OpensAddress => MacroIntent::Suspicious,
            Self::RunsProgram
            | Self::DownloadsFile
            | Self::WritesFiles
            | Self::DeletesFiles
            | Self::WritesRegistry
            | Self::InjectsCode
            | Self::ScheduledTask
            | Self::SelfModifiesCode
            | Self::SourceMismatch
            | Self::CallsNativeCode
            | Self::ExploitPrimitive => MacroIntent::Malicious,
        }
    }
}

/// A behaviour of a macro module: what, in which procedure, and the line that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroBehavior {
    /// What it does.
    pub kind: BehaviorKind,
    /// The procedure it is in (empty: outside any procedure).
    pub procedure: String,
    /// The line of source that shows it, trimmed, display-safe.
    pub detail: String,
}
wire_struct!(MacroBehavior { kind, procedure, detail });

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
    /// What the module does, judged from its source.
    pub intent: MacroIntent,
    /// What it does, one entry per behaviour and procedure, the most serious first.
    pub behaviors: Vec<MacroBehavior>,
    /// Names of its procedures (functions), in the order they are written.
    pub procedures: Vec<String>,
}
wire_struct!(MacroModule {
    language,
    location,
    name,
    source,
    truncated,
    auto_exec,
    suspicious,
    iocs,
    intent,
    behaviors,
    procedures
});

/// What a reference to an outside resource is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

/// Inputs of the verdict, accumulated for **every** distinct observation —
/// whether or not the matching [`Finding`] still fits under the storage cap.
///
/// The verdict must never depend on what was *stored*: an attacker who floods
/// the report with thousands of low-severity findings (hidden rows, comments)
/// would otherwise push the one decisive finding past the cap and obtain
/// `Clean` for a malicious file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Executable content or an attack payload was observed.
    pub(crate) active: bool,
    /// A resource that office software would fetch without user interaction
    /// was observed.
    pub(crate) tracker: bool,
    /// Part of the file was not (or could not be) inspected.
    pub(crate) coverage_gap: bool,
}

impl Facts {
    fn observe(&mut self, kind: ThreatKind, severity: Severity) {
        let category = kind.category();
        // Critical remote content (remote template injection, URL-moniker OLE
        // links, abusable protocol handlers) is an attack payload, not a tracker.
        self.active |= (category == Category::ActiveContent && severity >= Severity::Medium)
            || (category == Category::RemoteContent && severity >= Severity::Critical);
        // Any remote-content finding of Medium or higher severity (that is not
        // already an attack payload) means the file reaches out to the network
        // or to a UNC host; "Clean" would contradict the finding list.
        self.tracker |= matches!(
            kind,
            ThreatKind::RemoteTemplate | ThreatKind::ExternalImage | ThreatKind::ExternalFrame
        ) || (category == Category::RemoteContent && severity >= Severity::Medium);
        self.coverage_gap |= kind.limits_coverage_at(severity);
    }
}

/// Accumulates findings with de-duplication and caps.
///
/// Storage policy: up to `max_findings` findings are kept in arrival order; in
/// addition, the **first finding of every `(kind, severity)` class is always
/// kept** (at most `ThreatKind × Severity` extra entries). A flood of cheap
/// findings therefore can neither hide the *kind* of a later, more serious
/// observation from the list nor push out the truncation notice. The verdict
/// itself is computed from [`Facts`], which no cap can affect.
#[derive(Debug)]
pub(crate) struct ReportBuilder {
    findings: Vec<Finding>,
    seen: BTreeSet<(ThreatKind, String, String)>,
    classes: BTreeSet<(ThreatKind, Severity)>,
    pub(crate) macros: Vec<MacroModule>,
    pub(crate) external_refs: Vec<ExternalReference>,
    external_seen: BTreeSet<ExternalReference>,
    pub(crate) metadata: Vec<MetadataField>,
    meta_seen: BTreeSet<(MetaKey, String, String)>,
    pub(crate) embedded: Vec<EmbeddedObject>,
    pub(crate) stats: ScanStats,
    facts: Facts,
    max_findings: usize,
    pub(crate) truncated: bool,
    /// Distinct observations that were counted in the verdict but not stored.
    pub(crate) dropped: u64,
}

impl ReportBuilder {
    pub(crate) fn new(max_findings: usize) -> Self {
        Self {
            findings: Vec::new(),
            seen: BTreeSet::new(),
            classes: BTreeSet::new(),
            macros: Vec::new(),
            external_refs: Vec::new(),
            external_seen: BTreeSet::new(),
            metadata: Vec::new(),
            meta_seen: BTreeSet::new(),
            embedded: Vec::new(),
            stats: ScanStats::default(),
            facts: Facts::default(),
            max_findings,
            truncated: false,
            dropped: 0,
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
        // Verdict facts first: independent of de-duplication and of every cap.
        self.facts.observe(kind, severity);
        let detail = arca_container::text::display_safe(&detail.into(), 512);
        let location = arca_container::text::display_safe(location, 256);
        let key = (kind, location, detail);
        if self.seen.contains(&key) {
            return;
        }
        let first_of_class = self.classes.insert((kind, severity));
        if self.findings.len() >= self.max_findings && !first_of_class {
            // Not remembered in `seen`: memory stays bounded by what is stored.
            self.truncated = true;
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        let (kind, location, detail) = key;
        self.seen.insert((kind, location.clone(), detail.clone()));
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
        if self.meta_seen.contains(&(key, name.to_owned(), value.clone())) {
            return;
        }
        if self.metadata.len() >= self.max_findings {
            self.truncated = true;
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.meta_seen.insert((key, name.to_owned(), value.clone()));
        self.metadata.push(MetadataField {
            key,
            name: arca_container::text::display_safe(name, 256),
            value,
            source: source.to_owned(),
        });
    }

    pub(crate) fn external(&mut self, r: ExternalReference) {
        if r.auto_load && crate::url::is_network(r.class) {
            self.facts.tracker = true;
        }
        if self.external_seen.contains(&r) {
            return;
        }
        if self.external_refs.len() >= self.max_findings {
            self.truncated = true;
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.external_seen.insert(r.clone());
        self.external_refs.push(r);
    }

    /// Verdict inputs; unaffected by any storage cap.
    pub(crate) const fn facts(&self) -> Facts {
        self.facts
    }

    /// `true` if a finding of `kind` is stored. Reliable for every kind: the
    /// first finding of each class is always kept.
    pub(crate) fn has(&self, kind: ThreatKind) -> bool {
        self.findings.iter().any(|f| f.kind == kind)
    }

    pub(crate) fn into_parts(mut self) -> (Vec<Finding>, Self) {
        let mut findings = core::mem::take(&mut self.findings);
        findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.kind.cmp(&b.kind)));
        (findings, self)
    }
}
