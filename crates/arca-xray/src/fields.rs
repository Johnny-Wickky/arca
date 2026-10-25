//! Word field-code analysis (shared by OOXML, RTF and Word binary scanners).
//!
//! Field instructions are the vector behind DDE command execution
//! (`DDEAUTO cmd "/c calc"`), tracking pixels (`INCLUDEPICTURE "http://..." \d`),
//! remote content (`INCLUDETEXT`, `IMPORT`), OLE links (`LINK`), database
//! connections (`DATABASE`) and macro triggers (`MACROBUTTON`).
//!
//! Fields nest, and a nested field's *result* becomes part of the enclosing instruction, so
//! `{ {QUOTE 68 68 69} cmd }` is a DDE field whose keyword no text of the file spells out.
//! [`Tracker`] follows the nesting for every scanner (and the sanitizer), so that all of them
//! see the instruction Word would see.

use crate::report::{RefKind, Severity, ThreatKind};
use crate::{Ctx, ooxml};

/// The arguments of a field instruction, each with whether it was written in quotes.
fn split_tokens(instr: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut chars = instr.chars().peekable();
    while out.len() < 64 {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        let quoted = first == '"' || first == '\u{201C}' || first == '\u{201D}';
        if quoted {
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
        out.push((arg, quoted));
    }
    out
}

/// Splits a field instruction into arguments (quotes and doubled backslashes
/// are interpreted the way Word does).
pub(crate) fn split_args(instr: &str) -> Vec<String> {
    split_tokens(instr).into_iter().map(|(a, _)| a).collect()
}

/// The text a `QUOTE` field produces: quoted arguments are copied, unquoted numbers are
/// character codes (`QUOTE 68 68 69` is "DDE"), other unquoted words are copied too.
pub(crate) fn decode_quote_codes(instr: &str) -> Option<String> {
    let tokens = split_tokens(instr);
    let ((kw, _), rest) = tokens.split_first()?;
    if !kw.eq_ignore_ascii_case("QUOTE") || rest.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut last_literal = false;
    for (a, quoted) in rest {
        match a.parse::<u32>().ok().and_then(char::from_u32) {
            Some(c) if !quoted => {
                out.push(c);
                last_literal = false;
            }
            _ => {
                if last_literal {
                    out.push(' ');
                }
                out.push_str(a);
                last_literal = true;
            }
        }
    }
    Some(out)
}

/// `true` for a field switch (`\l`, `\m`, `\d`): a backslash and one letter. A longer argument
/// that starts with a backslash is a path — a UNC path among them.
pub(crate) fn is_switch(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('\\')
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.next().is_none()
}

/// What a finished nested field does to the instruction that encloses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nested {
    /// A `QUOTE` field: its text was appended to the enclosing instruction.
    Merged,
    /// Another field, standing where the enclosing instruction's *keyword* is: what the
    /// enclosing field is cannot be told without evaluating the nested one.
    Keyword,
    /// Another field in an argument position: harmless to the keyword.
    Argument,
}

/// Folds the finished nested field `child` into the instruction `parent` of its enclosing
/// field.
pub(crate) fn nest(parent: &mut String, child: &str) -> Nested {
    if let Some(decoded) = decode_quote_codes(child) {
        parent.push_str(&decoded);
        return Nested::Merged;
    }
    // Still inside the first word of the instruction (no whitespace before or after it): the
    // keyword is being built out of the nested field.
    if parent.trim_start().contains(char::is_whitespace) {
        Nested::Argument
    } else {
        Nested::Keyword
    }
}

/// A field that has ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finished {
    /// Position among all fields, in the order they began.
    pub(crate) index: usize,
    /// The instruction, with nested `QUOTE` results folded in.
    pub(crate) instr: String,
    /// The keyword was supplied by a nested field that could not be evaluated.
    pub(crate) computed_keyword: bool,
    /// This was a `QUOTE` nested in another field: its text is part of that field's instruction.
    pub(crate) merged: bool,
}

struct Open {
    index: usize,
    instr: String,
    in_instr: bool,
    computed_keyword: bool,
}

/// Follows `begin` / `separate` / `end` markers of nested fields.
#[derive(Default)]
pub(crate) struct Tracker {
    stack: Vec<Open>,
    begun: usize,
    /// Fields beyond the depth limit whose markers are being skipped.
    overflow: usize,
    /// `true` once a field nested deeper than [`Tracker::MAX_DEPTH`] was seen.
    pub(crate) too_deep: bool,
}

impl Tracker {
    /// Deepest nesting followed.
    pub(crate) const MAX_DEPTH: usize = 64;
    /// Longest instruction kept.
    const MAX_INSTR: usize = 8192;

    /// A field begins. Returns its index, or `None` beyond the depth limit.
    pub(crate) fn begin(&mut self) -> Option<usize> {
        if self.stack.len() >= Self::MAX_DEPTH {
            self.overflow += 1;
            self.too_deep = true;
            return None;
        }
        let index = self.begun;
        self.begun += 1;
        self.stack.push(Open {
            index,
            instr: String::new(),
            in_instr: true,
            computed_keyword: false,
        });
        Some(index)
    }

    /// Instruction text of the innermost field (ignored once its result has started).
    pub(crate) fn push_text(&mut self, text: &str) {
        if self.overflow > 0 {
            return;
        }
        if let Some(f) = self.stack.last_mut()
            && f.in_instr
            && f.instr.len() < Self::MAX_INSTR
        {
            f.instr.push_str(text);
        }
    }

    /// The instruction of the innermost field ends and its result begins.
    pub(crate) fn separate(&mut self) {
        if self.overflow == 0
            && let Some(f) = self.stack.last_mut()
        {
            f.in_instr = false;
        }
    }

    /// `true` while the innermost open field is still reading its instruction.
    pub(crate) fn in_instruction(&self) -> bool {
        self.overflow == 0 && self.stack.last().is_some_and(|f| f.in_instr)
    }

    /// The innermost field ends.
    pub(crate) fn end(&mut self) -> Option<Finished> {
        if self.overflow > 0 {
            self.overflow -= 1;
            return None;
        }
        let f = self.stack.pop()?;
        let mut merged = false;
        if let Some(parent) = self.stack.last_mut()
            && parent.in_instr
        {
            match nest(&mut parent.instr, &f.instr) {
                Nested::Merged => merged = true,
                Nested::Keyword => parent.computed_keyword = true,
                Nested::Argument => {}
            }
        }
        Some(Finished {
            index: f.index,
            instr: f.instr,
            computed_keyword: f.computed_keyword,
            merged,
        })
    }

    /// Fields still open at the end of the text, innermost first.
    pub(crate) fn finish(&mut self) -> Vec<Finished> {
        self.overflow = 0;
        let mut out = Vec::new();
        while let Some(f) = self.end() {
            out.push(f);
        }
        out
    }
}

/// Analyses a finished field: its instruction, and the fact that its keyword is computed.
pub(crate) fn analyze_finished(ctx: &mut Ctx<'_>, f: &Finished, loc: &str) {
    if f.merged {
        return;
    }
    if f.computed_keyword {
        ctx.report.add_with(
            ThreatKind::DdeField,
            Severity::High,
            loc,
            format!(
                "the keyword of this field is produced by a nested field, so what it does cannot be told without evaluating it: {}",
                arca_container::text::display_safe(f.instr.trim(), 200)
            ),
        );
    }
    analyze(ctx, &f.instr, loc);
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
            if let Some(target) = args.iter().skip(1).find(|a| !is_switch(a)) {
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
        assert_eq!(decode_quote_codes("QUOTE abc").as_deref(), Some("abc"));
        assert_eq!(decode_quote_codes("QUOTE \"DDEAUTO\"").as_deref(), Some("DDEAUTO"));
        assert_eq!(decode_quote_codes("QUOTE"), None);
        assert_eq!(decode_quote_codes("PAGE"), None);
    }

    #[test]
    fn nested_fields_are_folded_into_the_enclosing_instruction() {
        let mut t = Tracker::default();
        t.begin();
        t.push_text(" DD");
        t.begin();
        t.push_text("QUOTE \"EAUTO\"");
        t.separate();
        let inner = t.end().unwrap();
        assert!(inner.merged);
        t.push_text(" cmd");
        t.separate();
        let outer = t.end().unwrap();
        assert_eq!(outer.instr, " DDEAUTO cmd");
        assert!(!outer.computed_keyword);
    }

    #[test]
    fn a_field_in_the_keyword_position_makes_the_keyword_computed() {
        let mut t = Tracker::default();
        t.begin();
        t.begin();
        t.push_text("REF x");
        t.end();
        t.push_text(" cmd");
        assert!(t.end().unwrap().computed_keyword);
        // In an argument position it does not.
        t.begin();
        t.push_text(" IF ");
        t.begin();
        t.push_text("PAGE");
        t.end();
        assert!(!t.end().unwrap().computed_keyword);
    }

    #[test]
    fn fields_nested_beyond_the_limit_are_counted_not_followed() {
        let mut t = Tracker::default();
        for _ in 0..Tracker::MAX_DEPTH + 5 {
            t.begin();
        }
        assert!(t.too_deep);
        assert_eq!(t.finish().len(), Tracker::MAX_DEPTH);
    }

    #[test]
    fn switches_are_told_from_paths() {
        assert!(is_switch(r"\l"));
        assert!(!is_switch(r"\\host\share"));
        assert!(!is_switch(r"\attacker"));
    }
}
