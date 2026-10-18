//! Spreadsheet formula triage (OOXML, ODF, CSV).
//!
//! Arca never evaluates `WEBSERVICE`, `CALL`, `REGISTER`, `EXEC`, `RTD`,
//! `IMAGE` or DDE references; this module only recognises them so the X-ray
//! can tell the user what the file would have done in a spreadsheet program.

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
    // DDE: `app|topic!item`, possibly with the topic in single quotes.
    if let Some(bar) = code.find('|') {
        let before = code.get(..bar).unwrap_or("");
        let app: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\'' | '-'))
            .collect();
        let after = code.get(bar + 1..).unwrap_or("");
        if !app.trim_matches('\'').is_empty() && after.contains('!') {
            out.push(Risk::Dde);
        }
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

fn push(v: &mut Vec<Risk>, r: Risk) {
    if !v.contains(&r) {
        v.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
