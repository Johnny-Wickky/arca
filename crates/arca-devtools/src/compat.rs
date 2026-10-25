// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! `check-compat`: errors of the compile against the minimum SDK's android.jar may only come
//! from the nested `ApiNN` classes of `ui/Compat.java` (guarded by `SDK_INT`), never from the
//! code that also runs on the oldest supported system.

use std::path::Path;

use crate::util::{is_word, read_text, say};

/// A nested `class ApiNN` and its 1-based line range.
#[derive(Debug, PartialEq, Eq)]
pub struct ApiClass {
    /// First line of the declaration.
    pub start: usize,
    /// Line of the closing brace.
    pub end: usize,
    /// Class name (`Api28`).
    pub name: String,
}

/// The name in `class ApiNN` on `line`, if any.
fn api_class_name(line: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(i) = line.get(from..)?.find("class Api") {
        let at = from + i;
        let before_ok =
            line.get(..at).and_then(|s| s.chars().next_back()).is_none_or(|c| !is_word(c));
        let rest = line.get(at + "class ".len()..)?;
        let digits = rest.get("Api".len()..)?.bytes().take_while(u8::is_ascii_digit).count();
        let end = "Api".len() + digits;
        let after_ok = rest.get(end..).and_then(|s| s.chars().next()).is_none_or(|c| !is_word(c));
        if before_ok && digits > 0 && after_ok {
            return rest.get(..end);
        }
        from = at + 1;
    }
    None
}

/// Line ranges of the nested `ApiNN` classes of `source`.
#[must_use]
pub fn api_classes(source: &str) -> Vec<ApiClass> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(name) = api_class_name(line) else { continue };
        let mut depth = 0i64;
        let mut started = false;
        for (j, l) in lines.iter().enumerate().skip(i) {
            let opens = i64::try_from(l.matches('{').count()).unwrap_or(0);
            let closes = i64::try_from(l.matches('}').count()).unwrap_or(0);
            depth += opens - closes;
            if opens > 0 {
                started = true;
            }
            if started && depth <= 0 {
                out.push(ApiClass { start: i + 1, end: j + 1, name: name.to_owned() });
                break;
            }
        }
    }
    out
}

/// Line numbers of `Compat.java:N: error:` entries in a javac log.
#[must_use]
pub fn error_lines(log: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rest = log;
    while let Some(i) = rest.find("Compat.java:") {
        let tail = rest.get(i + "Compat.java:".len()..).unwrap_or_default();
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty()
            && tail.get(digits.len()..).is_some_and(|t| t.starts_with(": error:"))
            && let Ok(n) = digits.parse()
        {
            out.push(n);
        }
        rest = tail;
    }
    out
}

/// Lines of `log` that are errors in Compat.java but sit outside the `ApiNN` classes.
///
/// Import lines are harmless: a newer class is only loaded where it is used.
#[must_use]
pub fn violations(source: &str, log: &str) -> Vec<usize> {
    let lines: Vec<&str> = source.split('\n').collect();
    let ranges = api_classes(source);
    let mut bad: Vec<usize> = error_lines(log)
        .into_iter()
        .filter(|&n| {
            let is_import = n
                .checked_sub(1)
                .and_then(|i| lines.get(i))
                .is_some_and(|l| l.trim_start().starts_with("import "));
            !is_import && !ranges.iter().any(|r| r.start <= n && n <= r.end)
        })
        .collect();
    bad.sort_unstable();
    bad.dedup();
    bad
}

/// Entry point: `check-compat <Compat.java> <javac-log>`.
///
/// # Errors
/// Unreadable files or errors outside the nested classes.
pub fn run(args: &[String]) -> Result<(), String> {
    let [java, log] = args else {
        return Err("usage: check-compat <Compat.java> <javac.log>".into());
    };
    let source = read_text(Path::new(java))?;
    let log = read_text(Path::new(log))?;
    let bad = violations(&source, &log);
    if !bad.is_empty() {
        return Err(format!(
            "Compat.java: newer APIs outside the nested ApiNN classes at lines {bad:?}"
        ));
    }
    let mut names: Vec<String> = api_classes(&source).into_iter().map(|c| c.name).collect();
    names.sort();
    names.dedup();
    say(&format!("ok: all of them are inside {}", names.join(", ")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "import android.x.New;\nclass Compat {\n  static void a() { old(); }\n  static class Api28 {\n    void b() {\n      newer();\n    }\n  }\n  static class Api33 { void c() { newest(); } }\n  static void d() { old(); }\n}\n";

    #[test]
    fn finds_nested_api_classes_by_brace_matching() {
        let c = api_classes(SRC);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].start, c[0].end, c[0].name.as_str()), (4, 8, "Api28"));
        assert_eq!((c[1].start, c[1].end, c[1].name.as_str()), (9, 9, "Api33"));
        assert!(api_classes("class Apix {}\nclass Api {}\nclass MyApi28 {}").is_empty());
    }

    #[test]
    fn errors_inside_api_classes_and_imports_are_fine() {
        let log = "/x/ui/Compat.java:1: error: cannot find symbol\n/x/ui/Compat.java:6: error: cannot find symbol\n/x/ui/Compat.java:9: error: x\n";
        assert!(violations(SRC, log).is_empty());
    }

    #[test]
    fn errors_in_ordinary_code_are_reported() {
        let log = "/x/ui/Compat.java:3: error: cannot find symbol\nCompat.java:3: error: again\nCompat.java:10: error: z\nOther.java:3: error: q\nCompat.java:5: warning: w\n";
        assert_eq!(violations(SRC, log), [3, 10]);
    }

    #[test]
    fn error_lines_ignores_malformed_entries() {
        assert_eq!(
            error_lines(
                "Compat.java:12: error: a\nCompat.java:x: error: b\nCompat.java:7: warning: c"
            ),
            [12]
        );
    }
}
