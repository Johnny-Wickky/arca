//! The `detail` strings of the preview's notices are an interface: the apps turn each of them
//! into a sentence in the reader's language (Android: `NoticeDetails.java` and the
//! `notice_detail_*` strings). A detail that the Rust code starts to emit without the apps
//! hearing of it would reach the reader as raw English, so the list of emitted details is
//! committed in `android/core-test/notice_details.txt` and this test fails when the code and the
//! list differ. The Android core test fails when a listed detail has no localized text.
//!
//! The scan reads the sources of this crate: every call of `truncated`, `stop`, `note`,
//! `note_detail`, `count` and `skipped` whose detail is a string literal (or a `format!` whose
//! template is one; `{..}` becomes `{}`), plus the few details that are built another way and
//! are listed in [`EXTRAS`]. Details made of document data (names of parts, sheets, pages) are
//! not literals and are not listed: they use the generic text of their kind, as do the notices
//! without a detail and the machine breakdown that comes with `InvisibleRemoved`.
//!
//! To refresh the list after a legitimate change, run this test: its failure message prints the
//! lines to add and to remove.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Details that are not written as an argument of one of the scanned calls, with the file that
/// holds them (the test checks that the literal is still there).
const EXTRAS: &[(&str, &str, &str)] = &[
    ("Truncated", "items", "sink.rs"),
    ("*", "sheets", "sink.rs"),
    ("*", "slides", "sink.rs"),
    ("*", "sections", "sink.rs"),
    ("Unsupported", "elements in a foreign namespace were ignored", "sink.rs"),
    ("Skipped", "PDF objects beyond the first 2000000", "pdf/file.rs"),
    ("Skipped", "[Content_Types].xml", "opc.rs"),
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> =
        fs::read_dir(dir).expect("read src").map(|e| e.expect("entry").path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        if p.is_dir() {
            if !name.starts_with("tests") && !name.ends_with("_tests") {
                rust_files(&p, out);
            }
        } else if name.ends_with(".rs")
            && !name.starts_with("tests")
            && !name.ends_with("_tests.rs")
            && name != "testkit.rs"
            && name != "testpdf.rs"
        {
            out.push(p);
        }
    }
}

/// The source of a file without comments and without the test module at its end.
fn production_source(path: &Path) -> String {
    let text = fs::read_to_string(path).expect("read source");
    let mut out = String::new();
    let lines: Vec<&str> = text.lines().collect();
    for (n, line) in lines.iter().enumerate() {
        // The test module at the end of the file: `#[cfg(test)]` before an inline `mod ... {`.
        if line.starts_with("#[cfg(test)]")
            && lines
                .get(n + 1)
                .is_some_and(|l| l.starts_with("mod ") && l.trim_end().ends_with('{'))
        {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Index of the end of the string literal that starts at `i` (a `"`), and its text.
fn string_at(b: &[u8], i: usize) -> Option<(usize, String)> {
    let mut j = i + 1;
    let mut s = Vec::new();
    while j < b.len() {
        match b[j] {
            b'"' => return Some((j + 1, String::from_utf8_lossy(&s).into_owned())),
            b'\\' => {
                j += 1;
                match b.get(j) {
                    Some(b'n') => s.push(b'\n'),
                    Some(b'"') => s.push(b'"'),
                    Some(b'\\') => s.push(b'\\'),
                    Some(b'\n') => {
                        while b.get(j + 1).is_some_and(|c| c.is_ascii_whitespace()) {
                            j += 1;
                        }
                    }
                    Some(c) => {
                        s.push(b'\\');
                        s.push(*c);
                    }
                    None => return None,
                }
                j += 1;
            }
            c => {
                s.push(c);
                j += 1;
            }
        }
    }
    None
}

/// The top-level arguments (as source text) of the call whose `(` is at `open`.
fn arguments(b: &[u8], open: usize) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' => match string_at(b, i) {
                Some((end, _)) => {
                    i = end;
                    continue;
                }
                None => return args,
            },
            b'\'' => {
                // A character literal such as '"' or '\n'; a lifetime has no closing quote.
                if b.get(i + 1) == Some(&b'\\') {
                    if let Some(k) = b[i + 2..].iter().position(|c| *c == b'\'') {
                        i += k + 3;
                        continue;
                    }
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 3;
                    continue;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    args.push(String::from_utf8_lossy(&b[start..i]).trim().to_owned());
                    return args;
                }
            }
            b',' if depth == 1 => {
                args.push(String::from_utf8_lossy(&b[start..i]).trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    args
}

/// The detail an argument spells: a string literal or the template of a `format!`.
fn literal(arg: &str) -> Option<String> {
    let arg = arg.trim().trim_start_matches('&').trim();
    let arg = arg.strip_prefix("format!(").map_or(arg, str::trim_start);
    let b = arg.as_bytes();
    if b.first() != Some(&b'"') {
        return None;
    }
    let (_, text) = string_at(b, 0)?;
    // `{name}` and `{0}` are values filled in at run time.
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '{' if !inside => {
                inside = true;
                out.push_str("{}");
            }
            '}' if inside => inside = false,
            _ if inside => {}
            c => out.push(c),
        }
    }
    Some(out)
}

/// Index of the first `needle` in `hay` at or after `from`.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|k| k + from)
}

fn kind_of(arg: &str) -> String {
    arg.trim().strip_prefix("NoticeKind::").map_or_else(|| "*".to_owned(), str::to_owned)
}

fn emitted() -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_files(&src_dir(), &mut files);
    let mut set = BTreeSet::new();
    for path in files {
        let text = production_source(&path);
        let b = text.as_bytes();
        for (name, form) in [
            (".truncated(", 'T'),
            (".stop(", 'T'),
            (".note(", 'N'),
            (".note_detail(", 'N'),
            (".count(", 'N'),
            (".skipped(", 'S'),
        ] {
            let mut from = 0;
            while let Some(k) = find(b, name.as_bytes(), from) {
                let open = k + name.len() - 1;
                from = open + 1;
                let args = arguments(b, open);
                let found = match form {
                    'T' => {
                        args.first().and_then(|a| literal(a)).map(|d| ("Truncated".to_owned(), d))
                    }
                    'S' => args.first().and_then(|a| literal(a)).map(|d| ("Skipped".to_owned(), d)),
                    _ => match args.as_slice() {
                        // `pdf.note("what")`: a part that could not be read.
                        [only] if name == ".note(" => {
                            literal(only).map(|d| ("Skipped".to_owned(), d))
                        }
                        [kind, detail, ..] if name != ".count(" || detail.starts_with('"') => {
                            literal(detail).map(|d| (kind_of(kind), d))
                        }
                        _ => None,
                    },
                };
                if let Some((kind, detail)) = found.filter(|(_, d)| !d.is_empty()) {
                    set.insert(format!("{kind}|{detail}"));
                }
            }
        }
    }
    for (kind, detail, file) in EXTRAS {
        let source = production_source(&src_dir().join(file));
        assert!(
            source.contains(detail.split("{}").next().unwrap_or_default()),
            "{file} no longer holds the detail {detail:?}: update EXTRAS in tests_notice_details.rs"
        );
        set.insert(format!("{kind}|{detail}"));
    }
    set
}

fn committed() -> BTreeSet<String> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../android/core-test/notice_details.txt");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("android/core-test/notice_details.txt: {e}"));
    text.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_details_the_code_emits_are_the_ones_the_apps_know() {
    let code = emitted();
    let file = committed();
    let new: Vec<&String> = code.difference(&file).collect();
    let stale: Vec<&String> = file.difference(&code).collect();
    assert!(
        new.is_empty() && stale.is_empty(),
        "notice details differ from android/core-test/notice_details.txt\n\
         emitted but not listed (add them there and give them RU and EN texts in the app):\n{}\n\
         listed but not emitted (remove them):\n{}",
        new.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n"),
        stale.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n"),
    );
}

#[test]
fn the_scan_sees_the_calls_it_is_meant_to_see() {
    // A guard against the scanner going blind (a refactor of the Sink API, say): these must
    // always be found.
    let code = emitted();
    for must in [
        "Truncated|grid",
        "Unsupported|charts",
        "BestEffort|pdf",
        "*|columns",
        "HiddenShown|objects",
        "BestEffort|text-encoding=cp{}",
    ] {
        assert!(code.contains(must), "the scan lost {must:?}");
    }
}
