//! Word field-code analysis (shared by OOXML, RTF and Word binary scanners).
//!
//! Field instructions are the vector behind DDE command execution
//! (`DDEAUTO cmd "/c calc"`), tracking pixels (`INCLUDEPICTURE "http://..." \d`),
//! remote content (`INCLUDETEXT`, `IMPORT`), OLE links (`LINK`), database
//! connections (`DATABASE`) and macro triggers (`MACROBUTTON`).

use crate::report::{RefKind, Severity, ThreatKind};
use crate::{Ctx, ooxml};

/// Splits a field instruction into arguments (quotes and doubled backslashes
/// are interpreted the way Word does).
pub(crate) fn split_args(instr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = instr.chars().peekable();
    while out.len() < 64 {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        if first == '"' || first == '\u{201C}' || first == '\u{201D}' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' | '\u{201C}' | '\u{201D}' => break,
                    '\\' => {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        arg.push('\\');
                    }
                    _ => arg.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                chars.next();
                // `\\` is an escaped backslash; `\d` stays a switch.
                if c == '\\' && chars.peek() == Some(&'\\') {
                    chars.next();
                }
                arg.push(c);
            }
        }
        out.push(arg);
    }
    out
}

/// Decodes `QUOTE 68 68 69` style obfuscation into text, if it is one.
pub(crate) fn decode_quote_codes(instr: &str) -> Option<String> {
    let args = split_args(instr);
    let (kw, rest) = args.split_first()?;
    if !kw.eq_ignore_ascii_case("QUOTE") || rest.is_empty() {
        return None;
    }
    let mut out = String::new();
    for a in rest {
        let n: u32 = a.parse().ok()?;
        out.push(char::from_u32(n)?);
    }
    Some(out)
}

/// Analyses one complete field instruction.
pub(crate) fn analyze(ctx: &mut Ctx<'_>, instr: &str, loc: &str) {
    let args = split_args(instr);
    let Some(keyword) = args.first() else { return };
    let kw = keyword.trim_start_matches('\\').to_ascii_uppercase();
    let display = arca_container::text::display_safe(instr.trim(), 300);
    match kw.as_str() {
        "DDE" | "DDEAUTO" => {
            let sev = if kw == "DDEAUTO" { Severity::Critical } else { Severity::High };
            ctx.report.add_with(ThreatKind::DdeField, sev, loc, display);
        }
        "INCLUDEPICTURE" | "INCLUDETEXT" | "IMPORT" | "INCLUDE" => {
            if let Some(target) = args.get(1) {
                let kind = if kw == "INCLUDEPICTURE" || kw == "IMPORT" {
                    RefKind::Image
                } else {
                    RefKind::Field
                };
                ooxml::record_external(ctx, target, kind, true, loc);
                if crate::url::is_network(crate::url::classify(target)) {
                    ctx.report.add(ThreatKind::RemoteFieldInclude, loc, display);
                }
            }
        }
        "LINK" => {
            if let Some(target) = args.get(2) {
                ooxml::record_external(ctx, target, RefKind::ObjectLink, true, loc);
            }
            ctx.report.add_with(ThreatKind::ExternalObjectLink, Severity::Medium, loc, display);
        }
        "DATABASE" => {
            ctx.report.add(ThreatKind::ExternalDataConnection, loc, display);
        }
        "MACROBUTTON" => {
            ctx.report.add(ThreatKind::MacroButtonField, loc, display);
        }
        "HYPERLINK" => {
            if let Some(target) = args.iter().skip(1).find(|a| !a.starts_with('\\')) {
                ooxml::record_external(ctx, target, RefKind::Hyperlink, false, loc);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_splitting() {
        assert_eq!(
            split_args(r#" DDEAUTO c:\\windows\\system32\\cmd.exe "/k calc.exe"  "#),
            vec![
                "DDEAUTO".to_owned(),
                r"c:\windows\system32\cmd.exe".to_owned(),
                "/k calc.exe".to_owned()
            ]
        );
        assert_eq!(
            split_args(r#"INCLUDEPICTURE "http://t.example/p.gif" \d"#),
            vec![
                "INCLUDEPICTURE".to_owned(),
                "http://t.example/p.gif".to_owned(),
                "\\d".to_owned()
            ]
        );
    }

    #[test]
    fn quote_obfuscation() {
        assert_eq!(decode_quote_codes("QUOTE 68 68 69 65 85 84 79").as_deref(), Some("DDEAUTO"));
        assert_eq!(decode_quote_codes("QUOTE abc"), None);
        assert_eq!(decode_quote_codes("PAGE"), None);
    }
}
