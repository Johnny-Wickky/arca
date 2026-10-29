// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! `check-resources`: the Android resources and manifest, checked without the Android build
//! tools. What it catches (each of these otherwise shows up late, in CI, or at run time on a
//! device):
//!
//! * every XML file is well-formed (the strict reader of arca-container, which also refuses
//!   DOCTYPE and custom entities);
//! * every name used as `R.string.x` / `R.color.x` / `R.drawable.x` ... in the Java sources
//!   exists, and every name used in XML (`@color/x`, `@drawable/x`, `@string/x`, `@style/x`,
//!   `@xml/x`, `@mipmap/x`) exists;
//! * every string, plurals and string-array of `values/` has a Russian counterpart and the
//!   other way round, with the same format placeholders (`%1$s`, `%2$d`, `%%`), no unescaped
//!   apostrophe or quote, and no unnumbered placeholders next to each other;
//! * every identifier that the engine can report (the Rust enums) has a name in the resources:
//!   threat titles and explanations, formats, categories, severities, metadata keys, reference
//!   kinds and classes, embedded kinds, macro languages, sterile-copy actions, preview notices;
//! * no string is dead (unused and not reachable through the dynamic `Labels` lookups);
//! * `android:` attributes of the XML exist in the minimum SDK's `android.R.attr` (given
//!   `--android-jar`), except the few that are newer on purpose and ignored by older systems;
//! * the manifest: no permission at all is requested, `android:debuggable`
//!   is absent, `android:allowBackup="false"`, and every component declares `android:exported`
//!   explicitly, with only the allow-listed ones exported.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dom::{self, Element};
use crate::util::{is_word, list_dir, parse_args, read_file, read_text, say, walk_files};

/// android: attributes newer than the minimum SDK (26). Older systems ignore unknown
/// attributes, so using them is safe; anything else missing from the minimum SDK is a mistake.
const NEWER_ATTRS: &[&str] = &[
    "localeConfig",
    "enableOnBackInvokedCallback",
    "dataExtractionRules",
    "windowLightNavigationBar",
    "navigationBarColor",
    "useAppZygote",
];

/// Name prefixes that `Labels.java` builds at run time (so those strings are not dead).
const DYNAMIC_PREFIXES: &[&str] = &[
    "threat_",
    "format_",
    "cat_",
    "sev_",
    "meta_",
    "ref_kind_",
    "ref_class_",
    "emb_kind_",
    "macro_lang_",
    "action_",
    "notice_",
];

/// Components that may be exported (reachable by other apps): the launcher / VIEW / SEND
/// activity. Everything else must say `android:exported="false"`.
const EXPORTED_ALLOWED: &[&str] = &[".MainActivity", "org.arca.viewer.MainActivity"];

/// Manifest elements that are components and must declare `android:exported`.
const COMPONENT_TAGS: &[&str] = &["activity", "activity-alias", "service", "receiver", "provider"];

const JAVA_KINDS: &[&str] = &[
    "string", "color", "drawable", "mipmap", "xml", "style", "dimen", "id", "layout", "array",
    "plurals",
];
const XML_KINDS: &[&str] = &["string", "color", "drawable", "mipmap", "xml", "style", "dimen"];

/// Mirror of `org.arca.viewer.core.Names.snake`.
#[must_use]
pub fn snake(camel: &str) -> String {
    let mut out = String::new();
    for ch in camel.chars() {
        if ch.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        }
    }
    out
}

// ---- placeholders and escaping --------------------------------------------------------------

/// A format placeholder: the optional position (`1` of `%1$s`) and the conversion.
pub type Placeholder = (Option<String>, char);

/// The placeholders of `value` (`%%` is not one).
#[must_use]
pub fn placeholders(value: &str) -> Vec<Placeholder> {
    let chars: Vec<char> = value.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c != '%' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let digits_start = j;
        while chars.get(j).is_some_and(char::is_ascii_digit) {
            j += 1;
        }
        let position = if j > digits_start && chars.get(j) == Some(&'$') {
            let p: String = chars.get(digits_start..j).unwrap_or_default().iter().collect();
            j += 1;
            Some(p)
        } else {
            j = digits_start;
            None
        };
        match chars.get(j) {
            Some(&conv) if matches!(conv, 's' | 'd' | 'f') => {
                out.push((position, conv));
                i = j + 1;
            }
            _ if chars.get(i + 1) == Some(&'%') => i += 2,
            _ => i += 1,
        }
    }
    out
}

fn sorted(mut p: Vec<Placeholder>) -> Vec<Placeholder> {
    p.sort();
    p
}

fn unescaped(value: &str, ch: char) -> bool {
    let mut prev = None;
    for c in value.chars() {
        if c == ch && prev != Some('\\') {
            return true;
        }
        prev = Some(c);
    }
    false
}

/// Whether `value` has a `%` followed by something that is not part of a placeholder.
fn stray_percent(value: &str) -> bool {
    let stripped = value.replace("%%", "");
    let mut chars = stripped.chars();
    while let Some(c) = chars.next() {
        if c == '%'
            && let Some(n) = chars.clone().next()
            && !(n == '%' || n.is_ascii_digit() || matches!(n, '$' | 's' | 'd' | 'f'))
        {
            return true;
        }
    }
    false
}

fn check_escaping(locale: &str, name: &str, value: &str, problems: &mut Vec<String>) {
    // In an Android string resource an apostrophe or a double quote must be escaped, and a
    // leading @ or ? must be escaped too.
    if unescaped(value, '\'') {
        problems.push(format!(
            "values[{locale}]/{name}: unescaped apostrophe (use \\' or the typographic \u{2019})"
        ));
    }
    if unescaped(value, '"') {
        problems.push(format!("values[{locale}]/{name}: unescaped double quote"));
    }
    if value.starts_with(['@', '?']) {
        problems.push(format!("values[{locale}]/{name}: starts with @ or ? (escape it)"));
    }
    if stray_percent(value) {
        problems
            .push(format!("values[{locale}]/{name}: stray % that is not a placeholder (write %%)"));
    }
    let ph = placeholders(value);
    if ph.len() > 1 && ph.iter().any(|p| p.0.is_none()) {
        problems.push(format!(
            "values[{locale}]/{name}: several placeholders must be positional (%1$s, %2$d)"
        ));
    }
}

// ---- reading the resources ------------------------------------------------------------------

/// One locale's value resources.
#[derive(Debug, Default)]
struct Values {
    /// name -> (text, translatable)
    strings: BTreeMap<String, (String, bool)>,
    /// name -> (translatable, [(quantity, text)])
    plurals: BTreeMap<String, (bool, Vec<(String, String)>)>,
    /// name -> (translatable, items)
    arrays: BTreeMap<String, (bool, Vec<String>)>,
}

fn read_values(
    res_dir: &Path,
    problems: &mut Vec<String>,
) -> Result<BTreeMap<String, Values>, String> {
    let mut result: BTreeMap<String, Values> = BTreeMap::new();
    for (entry, dir) in list_dir(res_dir)? {
        if !entry.starts_with("values") || !dir.is_dir() {
            continue;
        }
        let locale = if entry == "values" {
            "en".to_owned()
        } else {
            entry.get("values-".len()..).unwrap_or_default().to_owned()
        };
        if entry.contains("night") || entry.matches('-').count() > 1 {
            continue;
        }
        let values = result.entry(locale).or_default();
        for (fname, path) in list_dir(&dir)? {
            if !fname.ends_with(".xml") {
                continue;
            }
            // A malformed file is reported once, by the well-formedness pass of `check_inner`.
            let Ok(root) = dom::parse(&read_file(&path)?) else { continue };
            for el in &root.children {
                let Some(name) = el.attr("name") else { continue };
                let translatable = el.attr("translatable") != Some("false");
                match el.name.as_str() {
                    "string" => {
                        if values
                            .strings
                            .insert(name.to_owned(), (el.text.clone(), translatable))
                            .is_some()
                        {
                            problems.push(format!(
                                "{}: duplicate string '{name}' in {entry}/",
                                path.display()
                            ));
                        }
                    }
                    "plurals" => {
                        let items = el
                            .children
                            .iter()
                            .filter(|i| i.name == "item")
                            .map(|i| {
                                (i.attr("quantity").unwrap_or_default().to_owned(), i.text.clone())
                            })
                            .collect();
                        values.plurals.insert(name.to_owned(), (translatable, items));
                    }
                    "string-array" => {
                        let items = el
                            .children
                            .iter()
                            .filter(|i| i.name == "item")
                            .map(|i| i.text.clone())
                            .collect();
                        values.arrays.insert(name.to_owned(), (translatable, items));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(result)
}

/// All resource names by type, as defined by the `res/` tree.
fn resource_names(res_dir: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (entry, dir) in list_dir(res_dir)? {
        if !dir.is_dir() {
            continue;
        }
        let kind = entry.split('-').next().unwrap_or_default().to_owned();
        for (fname, fpath) in list_dir(&dir)? {
            if kind == "values" {
                if !fname.ends_with(".xml") {
                    continue;
                }
                let Ok(root) = dom::parse(&read_file(&fpath)?) else { continue };
                for el in &root.children {
                    let Some(n) = el.attr("name") else { continue };
                    let (t, n) = match el.name.as_str() {
                        "item" => match el.attr("type") {
                            Some(t) => (t.to_owned(), n.to_owned()),
                            None => continue,
                        },
                        "style" => ("style".to_owned(), n.replace('.', "_")),
                        other => (other.to_owned(), n.to_owned()),
                    };
                    names.entry(t).or_default().insert(n);
                }
            } else {
                names
                    .entry(kind.clone())
                    .or_default()
                    .insert(fname.split('.').next().unwrap_or_default().to_owned());
            }
        }
    }
    Ok(names)
}

// ---- Rust enums -----------------------------------------------------------------------------

/// Variant names of the enum whose declaration starts with `header` (which ends in `{`).
fn enum_variants(
    path: &Path,
    header: &str,
    problems: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => {
            problems.push(e);
            return Ok(Vec::new());
        }
    };
    let Some(at) = text.find(header) else {
        problems.push(format!("{}: enum matching /{header}/ not found", path.display()));
        return Ok(Vec::new());
    };
    let start = at + header.len() - 1;
    let mut depth = 0i64;
    let mut end = start;
    for (i, c) in text.get(start..).unwrap_or_default().char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = text.get(start + 1..end).unwrap_or_default();
    let mut out = Vec::new();
    for line in body.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with("//") || s.starts_with('#') {
            continue;
        }
        let ident: String = s.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let next = s.get(ident.len()..).and_then(|r| r.chars().next());
        if ident.starts_with(|c: char| c.is_ascii_uppercase()) && next.is_none_or(|c| !is_word(c)) {
            out.push(ident);
        }
    }
    Ok(out)
}

/// `Name = 3 => (a, b)` lines of the `threats!` table: the variant name.
fn threat_line(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.len() == line.len() {
        return None;
    }
    let ident_len = t.chars().take_while(|c| c.is_ascii_alphanumeric()).count();
    let ident = t.get(..ident_len)?;
    if !ident.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let rest = t.get(ident_len..)?.strip_prefix(" = ")?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = rest.get(digits..)?.strip_prefix(" => (")?;
    let first = rest.chars().take_while(|&c| is_word(c)).count();
    if first == 0 {
        return None;
    }
    let rest = rest.get(first..)?.strip_prefix(", ")?;
    let second = rest.chars().take_while(|&c| is_word(c)).count();
    if second == 0 || !rest.get(second..)?.starts_with(')') {
        return None;
    }
    Some(ident)
}

fn threat_kinds(path: &Path, problems: &mut Vec<String>) -> Result<Vec<String>, String> {
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => {
            problems.push(e);
            return Ok(Vec::new());
        }
    };
    let kinds: Vec<String> = text.lines().filter_map(threat_line).map(str::to_owned).collect();
    if kinds.is_empty() {
        problems.push(format!("{}: no threat kinds found", path.display()));
    }
    Ok(kinds)
}

// ---- Java and XML references ----------------------------------------------------------------

/// `(kind, name)` of every `R.kind.name` in `text`.
#[must_use]
pub fn java_refs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices("R.") {
        if text.get(..i).and_then(|s| s.chars().next_back()).is_some_and(is_word) {
            continue;
        }
        let rest = text.get(i + 2..).unwrap_or_default();
        let kind: String = rest.chars().take_while(|&c| is_word(c)).collect();
        if !JAVA_KINDS.contains(&kind.as_str()) {
            continue;
        }
        let Some(after) = rest.get(kind.len()..).and_then(|r| r.strip_prefix('.')) else {
            continue;
        };
        let name: String = after.chars().take_while(|&c| is_word(c)).collect();
        if !name.is_empty() {
            out.push((kind, name));
        }
    }
    out
}

/// `(kind, name)` of every `@kind/name` in `text` for the given kinds (`@android:...` and
/// `@+id` are not references to this app's resources).
#[must_use]
pub fn xml_refs(text: &str, kinds: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices('@') {
        let rest = text.get(i + 1..).unwrap_or_default();
        let kind: String = rest.chars().take_while(char::is_ascii_lowercase).collect();
        if !kinds.contains(&kind.as_str()) {
            continue;
        }
        let Some(after) = rest.get(kind.len()..).and_then(|r| r.strip_prefix('/')) else {
            continue;
        };
        let name: String = after.chars().take_while(|&c| is_word(c) || c == '.').collect();
        if !name.is_empty() {
            out.push((kind, name));
        }
    }
    out
}

/// Names of `android:` attributes used in `text` (`android:name=` and `<item name="android:x"`).
#[must_use]
pub fn android_attrs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices("android:") {
        if text.get(..i).and_then(|s| s.chars().next_back()).is_some_and(is_word) {
            continue;
        }
        let rest = text.get(i + "android:".len()..).unwrap_or_default();
        let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
        if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        let after = rest.get(name.len()..).unwrap_or_default().trim_start();
        let item =
            text.get(..i).is_some_and(|s| s.ends_with("<item name=\"")) && after.starts_with('"');
        if after.starts_with('=') || item {
            out.push(name);
        }
    }
    out
}

fn jar_attr_names(jar: &Path) -> Option<BTreeSet<String>> {
    let out = Command::new("javap")
        .arg("-cp")
        .arg(jar)
        .args(["-public", "android.R$attr"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .filter_map(|l| l.trim().strip_prefix("public static final int "))
            .filter_map(|r| r.strip_suffix(';'))
            .map(str::to_owned)
            .collect(),
    )
}

/// The text without `<!-- ... -->` comments (they may mention what is absent).
#[must_use]
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<!--") {
        out.push_str(rest.get(..i).unwrap_or_default());
        match rest.get(i + 4..).and_then(|r| r.find("-->").map(|j| (r, j))) {
            Some((r, j)) => rest = r.get(j + 3..).unwrap_or_default(),
            None => return out + rest.get(i..).unwrap_or_default(),
        }
    }
    out + rest
}

// ---- the manifest ---------------------------------------------------------------------------

/// Manifest policy: permissions, network, backup, debuggable, exported components.
pub fn check_manifest(root: &Element, text_without_comments: &str, problems: &mut Vec<String>) {
    // Parsed, not grepped: every uses-permission* and permission* element counts, whatever the
    // quoting, attribute order or suffix (uses-permission-sdk-23, uses-permission-sdk-m, ...).
    for el in root.descendants() {
        let tag = el.name.as_str();
        if tag.starts_with("uses-permission") || tag.starts_with("permission") {
            let p = el.attr("android:name").unwrap_or("(none)");
            problems.push(format!(
                "AndroidManifest.xml declares <{tag} {p}>; Arca requests no permission at all"
            ));
        }
        if el.attr("android:debuggable").is_some() {
            problems.push(format!(
                "AndroidManifest.xml: <{tag}> sets android:debuggable (a release must not)"
            ));
        }
        if COMPONENT_TAGS.contains(&tag) {
            let name = el.attr("android:name").unwrap_or("(unnamed)");
            match el.attr("android:exported") {
                None => problems.push(format!("AndroidManifest.xml: <{tag} {name}> must set android:exported explicitly")),
                Some("true") if !EXPORTED_ALLOWED.contains(&name) => problems.push(format!(
                    "AndroidManifest.xml: <{tag} {name}> is exported but is not on the allow-list {EXPORTED_ALLOWED:?}"
                )),
                Some("true" | "false") => {}
                Some(other) => problems.push(format!("AndroidManifest.xml: <{tag} {name}> has android:exported=\"{other}\"")),
            }
        }
    }
    match root.descendants().into_iter().find(|e| e.name == "application") {
        None => problems.push("AndroidManifest.xml: no <application> element".to_owned()),
        Some(app) => {
            if app.attr("android:allowBackup") != Some("false") {
                problems.push(
                    "AndroidManifest.xml: <application> must set android:allowBackup=\"false\""
                        .to_owned(),
                );
            }
        }
    }
    if text_without_comments.contains("INTERNET") {
        problems.push("AndroidManifest.xml mentions INTERNET".to_owned());
    }
}

// ---- the string tables ----------------------------------------------------------------------

fn check_translations(en: &Values, ru: &Values, problems: &mut Vec<String>) {
    for (name, (value, translatable)) in &en.strings {
        check_escaping("en", name, value, problems);
        if *translatable && !ru.strings.contains_key(name) {
            problems.push(format!("values-ru/: missing translation of '{name}'"));
        }
        if let Some((rv, _)) = ru.strings.get(name).filter(|_| *translatable) {
            let (a, b) = (sorted(placeholders(value)), sorted(placeholders(rv)));
            if a != b {
                problems
                    .push(format!("'{name}': placeholders differ between en {a:?} and ru {b:?}"));
            }
        }
    }
    for (name, (value, _)) in &ru.strings {
        check_escaping("ru", name, value, problems);
        match en.strings.get(name) {
            None => problems.push(format!("values-ru/: '{name}' does not exist in values/")),
            Some((_, false)) => problems
                .push(format!("values-ru/: '{name}' is marked translatable=false in values/")),
            Some(_) => {}
        }
    }
    for (name, (value, translatable)) in &en.strings {
        if *translatable && value.trim().is_empty() {
            problems.push(format!("values/{name}: empty"));
        }
    }
    check_plurals(en, ru, problems);
    check_arrays(en, ru, problems);
}

fn check_plurals(en: &Values, ru: &Values, problems: &mut Vec<String>) {
    for (name, (translatable, items)) in &en.plurals {
        for (q, text) in items {
            check_escaping("en", &format!("{name}[{q}]"), text, problems);
        }
        if !*translatable {
            continue;
        }
        let Some((_, ru_items)) = ru.plurals.get(name) else {
            problems.push(format!("values-ru/: missing translation of plurals '{name}'"));
            continue;
        };
        let other = |list: &[(String, String)]| {
            list.iter().find(|(q, _)| q == "other").map(|(_, t)| sorted(placeholders(t)))
        };
        match (other(items), other(ru_items)) {
            (Some(a), Some(b)) if a != b => {
                problems.push(format!(
                    "plurals '{name}': placeholders of 'other' differ between en {a:?} and ru {b:?}"
                ));
            }
            (Some(_), Some(_)) => {}
            _ => problems.push(format!("plurals '{name}': every language needs an 'other' item")),
        }
    }
    for (name, (_, items)) in &ru.plurals {
        for (q, text) in items {
            check_escaping("ru", &format!("{name}[{q}]"), text, problems);
        }
        if !en.plurals.contains_key(name) {
            problems.push(format!("values-ru/: plurals '{name}' does not exist in values/"));
        }
    }
}

fn check_arrays(en: &Values, ru: &Values, problems: &mut Vec<String>) {
    for (name, (translatable, items)) in &en.arrays {
        for (i, text) in items.iter().enumerate() {
            check_escaping("en", &format!("{name}[{i}]"), text, problems);
        }
        if !*translatable {
            continue;
        }
        let Some((_, ru_items)) = ru.arrays.get(name) else {
            problems.push(format!("values-ru/: missing translation of string-array '{name}'"));
            continue;
        };
        if items.len() != ru_items.len() {
            problems.push(format!(
                "string-array '{name}': {} items in en, {} in ru",
                items.len(),
                ru_items.len()
            ));
            continue;
        }
        for (i, (a, b)) in items.iter().zip(ru_items).enumerate() {
            let (pa, pb) = (sorted(placeholders(a)), sorted(placeholders(b)));
            if pa != pb {
                problems.push(format!("string-array '{name}'[{i}]: placeholders differ between en {pa:?} and ru {pb:?}"));
            }
        }
    }
    for (name, (_, items)) in &ru.arrays {
        for (i, text) in items.iter().enumerate() {
            check_escaping("ru", &format!("{name}[{i}]"), text, problems);
        }
        if !en.arrays.contains_key(name) {
            problems.push(format!("values-ru/: string-array '{name}' does not exist in values/"));
        }
    }
}

// ---- the whole check ------------------------------------------------------------------------

/// What a successful check covered.
#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    /// Strings in `values/`.
    pub strings: usize,
    /// Engine identifiers that have names.
    pub identifiers: usize,
    /// Resources defined by `res/`.
    pub resources: usize,
}

fn files_with_extension(dir: &Path, ext: &str) -> Result<Vec<PathBuf>, String> {
    Ok(walk_files(dir)?.into_iter().filter(|p| p.extension().is_some_and(|e| e == ext)).collect())
}

fn rel(path: &Path, base: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Runs every check.
///
/// # Errors
/// All problems found (an unreadable tree is reported as one problem).
pub fn check(
    android_dir: &Path,
    repo_root: &Path,
    android_jar: Option<&Path>,
) -> Result<Summary, Vec<String>> {
    let mut problems = Vec::new();
    match check_inner(android_dir, repo_root, android_jar, &mut problems) {
        Ok(summary) if problems.is_empty() => Ok(summary),
        Ok(_) => Err(problems),
        Err(e) => {
            problems.push(e);
            Err(problems)
        }
    }
}

fn check_inner(
    android_dir: &Path,
    root: &Path,
    android_jar: Option<&Path>,
    problems: &mut Vec<String>,
) -> Result<Summary, String> {
    let main_dir = android_dir.join("app/src/main");
    let res_dir = main_dir.join("res");
    let java_dir = main_dir.join("java");

    // 0. every XML file is well-formed
    let xml_files = files_with_extension(&main_dir, "xml")?;
    for path in &xml_files {
        if let Err(e) = dom::parse(&read_file(path)?) {
            problems.push(format!("{}: not well-formed XML: {e}", rel(path, &main_dir)));
        }
    }

    let values = read_values(&res_dir, problems)?;
    let empty = Values::default();
    let en = values.get("en").unwrap_or(&empty);
    let ru = values.get("ru").unwrap_or(&empty);
    if en.strings.is_empty() {
        problems.push("values/ has no strings".to_owned());
    }

    // 1. translations match
    check_translations(en, ru, problems);

    // 2. the engine's identifiers all have names
    let rs = root.join("crates");
    let report = rs.join("arca-xray/src/report.rs");
    let mut expected: BTreeMap<String, String> = BTreeMap::new();
    for kind in threat_kinds(&report, problems)? {
        for suffix in ["title", "text"] {
            expected
                .insert(format!("threat_{}_{suffix}", snake(&kind)), format!("ThreatKind::{kind}"));
        }
    }
    let groups: [(&str, PathBuf, &str); 10] = [
        ("format_", rs.join("arca-xray/src/format.rs"), "pub enum FileFormat {"),
        ("cat_", report.clone(), "pub enum Category {"),
        ("sev_", report.clone(), "pub enum Severity {"),
        ("meta_", report.clone(), "pub enum MetaKey {"),
        ("ref_kind_", report.clone(), "pub enum RefKind {"),
        ("ref_class_", report.clone(), "pub enum TargetClass {"),
        ("emb_kind_", report.clone(), "pub enum EmbeddedKind {"),
        ("macro_lang_", report.clone(), "pub enum MacroLanguage {"),
        ("action_", rs.join("arca-xray/src/sanitize/mod.rs"), "pub enum ActionKind {"),
        ("notice_", rs.join("arca-preview/src/model.rs"), "pub enum NoticeKind {"),
    ];
    for (prefix, path, header) in &groups {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for variant in enum_variants(path, header, problems)? {
            expected.insert(format!("{prefix}{}", snake(&variant)), format!("{file}::{variant}"));
        }
    }
    for (name, origin) in &expected {
        if !en.strings.contains_key(name) {
            problems.push(format!("values/: missing '{name}' (for {origin})"));
        }
        if !ru.strings.contains_key(name) {
            problems.push(format!("values-ru/: missing '{name}' (for {origin})"));
        }
    }

    // 3. references resolve
    let defined = resource_names(&res_dir)?;
    let is_defined = |kind: &str, name: &str| defined.get(kind).is_some_and(|s| s.contains(name));
    let mut used_strings: BTreeSet<String> = BTreeSet::new();
    let mut java: BTreeMap<(String, String), String> = BTreeMap::new();
    for path in files_with_extension(&java_dir, "java")? {
        for r in java_refs(&read_text(&path)?) {
            java.entry(r).or_insert_with(|| rel(&path, &java_dir));
        }
    }
    for ((kind, name), where_) in &java {
        if kind == "string" {
            used_strings.insert(name.clone());
        }
        // R.id is created by `@+id`; Java may only use ids that XML declares.
        if !is_defined(kind, name) {
            problems.push(format!("R.{kind}.{name} used in {where_} but not defined in res/"));
        }
    }
    let mut xml: BTreeMap<(String, String), String> = BTreeMap::new();
    for path in &xml_files {
        for (kind, name) in xml_refs(&read_text(path)?, XML_KINDS) {
            let name = if kind == "style" { name.replace('.', "_") } else { name };
            xml.entry((kind, name)).or_insert_with(|| rel(path, &main_dir));
        }
    }
    for ((kind, name), where_) in &xml {
        if kind == "string" {
            used_strings.insert(name.clone());
        }
        if !is_defined(kind, name) {
            problems.push(format!("@{kind}/{name} used in {where_} but not defined in res/"));
        }
    }
    let manifest_path = main_dir.join("AndroidManifest.xml");
    let manifest_text = strip_comments(&read_text(&manifest_path)?);
    for (kind, name) in xml_refs(&manifest_text, &["string", "mipmap", "style", "xml", "drawable"])
    {
        let key = name.replace('.', "_");
        if kind == "string" {
            used_strings.insert(key.clone());
        }
        if !is_defined(&kind, &key) {
            problems
                .push(format!("AndroidManifest.xml refers to @{kind}/{name} which is not defined"));
        }
    }

    // 4. dead strings
    for name in en.strings.keys() {
        if used_strings.contains(name)
            || DYNAMIC_PREFIXES.iter().any(|p| name.starts_with(p))
            || name == "app_name"
        {
            continue;
        }
        problems.push(format!("values/{name}: never used"));
    }

    // 5. android: attributes exist in the minimum SDK
    if let Some(jar) = android_jar {
        match jar_attr_names(jar) {
            None => problems.push(format!(
                "could not read android.R$attr from {} (is javap installed?)",
                jar.display()
            )),
            Some(known) => {
                let mut used: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                for path in &xml_files {
                    for attr in android_attrs(&read_text(path)?) {
                        used.entry(attr).or_default().insert(rel(path, &main_dir));
                    }
                }
                for (attr, files) in &used {
                    if !known.contains(attr) && !NEWER_ATTRS.contains(&attr.as_str()) {
                        let first = files.iter().next().map_or("?", String::as_str);
                        problems.push(format!(
                            "android:{attr} (in {first}) does not exist in the minimum SDK and is not in NEWER_ATTRS"
                        ));
                    }
                }
            }
        }
    }

    // 6. manifest policy
    match dom::parse(&read_file(&manifest_path)?) {
        Ok(manifest) => check_manifest(&manifest, &manifest_text, problems),
        Err(e) => problems.push(format!("AndroidManifest.xml: not well-formed XML: {e}")),
    }

    Ok(Summary {
        strings: en.strings.len(),
        identifiers: expected.len(),
        resources: defined.values().map(BTreeSet::len).sum(),
    })
}

/// Entry point: `check-resources <android-dir> [--android-jar PATH] [--repo-root DIR]`.
///
/// `<android-dir>` is the repository's `android/` directory; the Rust enums are read from
/// `<repo-root>/crates` (default: the parent of `<android-dir>`).
///
/// # Errors
/// Bad arguments or any problem found.
pub fn run(args: &[String]) -> Result<(), String> {
    let p = parse_args(args, &["android-jar", "repo-root"], &[])?;
    let [android] = p.pos.as_slice() else {
        return Err(
            "usage: check-resources <android-dir> [--android-jar PATH] [--repo-root DIR]".into()
        );
    };
    let android = std::fs::canonicalize(android).map_err(|e| format!("{android}: {e}"))?;
    let root = match p.opt("repo-root") {
        Some(r) => PathBuf::from(r),
        None => android.parent().map(Path::to_path_buf).ok_or("android-dir has no parent")?,
    };
    match check(&android, &root, p.opt("android-jar").map(Path::new)) {
        Ok(s) => {
            say(&format!(
                "check-resources: ok - {} strings in 2 languages, {} engine identifiers named, {} resources",
                s.strings, s.identifiers, s.resources
            ));
            Ok(())
        }
        Err(problems) => {
            let mut msg = format!("check-resources: {} problem(s)", problems.len());
            for p in problems.iter().take(200) {
                msg.push_str("\n  - ");
                msg.push_str(p);
            }
            if problems.len() > 200 {
                msg.push_str(&format!("\n  ... and {} more", problems.len() - 200));
            }
            Err(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TempDir;

    fn ph(s: &str) -> Vec<(String, char)> {
        placeholders(s).into_iter().map(|(p, c)| (p.unwrap_or_default(), c)).collect()
    }

    #[test]
    fn snake_matches_the_java_helper() {
        assert_eq!(snake("MacroEnabled"), "macro_enabled");
        assert_eq!(snake("Zip64"), "zip64");
        assert_eq!(snake("OLE2Stream"), "o_l_e2_stream");
        assert_eq!(snake("a-b"), "ab");
    }

    #[test]
    fn placeholders_are_found_and_percent_percent_is_not_one() {
        assert_eq!(ph("%1$s of %2$d"), [("1".into(), 's'), ("2".into(), 'd')]);
        assert_eq!(ph("100%% of %s"), [(String::new(), 's')]);
        assert_eq!(ph("%%s"), []);
        assert_eq!(ph("%1$ and %q"), []);
        assert_eq!(ph("%f"), [(String::new(), 'f')]);
    }

    fn escaping(s: &str) -> Vec<String> {
        let mut p = Vec::new();
        check_escaping("en", "n", s, &mut p);
        p
    }

    #[test]
    fn escaping_rules() {
        assert!(escaping("it\\'s fine \\\"ok\\\" 100%% %1$s").is_empty());
        assert!(escaping("it's")[0].contains("apostrophe"));
        assert!(escaping("say \"hi\"")[0].contains("double quote"));
        assert!(escaping("@home")[0].contains("starts with"));
        assert!(escaping("50% off")[0].contains("stray %"));
        assert!(escaping("100%")[..].is_empty());
        assert!(escaping("%s and %d")[0].contains("positional"));
        assert!(escaping("%1$s and %2$d").is_empty());
    }

    #[test]
    fn enum_variants_skip_comments_attributes_and_fields() {
        let t = TempDir::new("enum");
        t.write("e.rs", "pub enum A {\n    /// doc\n    #[x]\n    One,\n    Two = 2,\n    Three(u8),\n    // c\n}\npub enum B { X }\n");
        let mut p = Vec::new();
        assert_eq!(
            enum_variants(&t.0.join("e.rs"), "pub enum A {", &mut p).unwrap(),
            ["One", "Two", "Three"]
        );
        assert!(p.is_empty());
        assert!(enum_variants(&t.0.join("e.rs"), "pub enum Z {", &mut p).unwrap().is_empty());
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn threat_table_lines() {
        assert_eq!(threat_line("    MacroPresent = 12 => (High, Macro),"), Some("MacroPresent"));
        assert_eq!(threat_line("MacroPresent = 12 => (High, Macro)"), None);
        assert_eq!(threat_line("    lower = 1 => (A, B)"), None);
        assert_eq!(threat_line("    X = 1 => (A B)"), None);
        assert_eq!(threat_line("    X = => (A, B)"), None);
    }

    #[test]
    fn java_and_xml_references() {
        let j = java_refs(
            "x(R.string.a_b); foo.R.color.c; MyR.string.no; R.styleable.s; R.id.\nR.layout.main",
        );
        assert_eq!(
            j,
            [
                ("string".into(), "a_b".into()),
                ("color".into(), "c".into()),
                ("layout".into(), "main".into())
            ]
        );
        let x = xml_refs(
            "@string/a @android:color/white @+id/q @style/Theme.Arca @drawable/",
            XML_KINDS,
        );
        assert_eq!(x, [("string".into(), "a".into()), ("style".into(), "Theme.Arca".into())]);
    }

    #[test]
    fn android_attribute_names() {
        let a = android_attrs(
            "<a android:name=\"x\" android:textSize = \"1\" xmlns:android=\"u\"/><item name=\"android:windowBackground\">@c</item>",
        );
        assert_eq!(a, ["name", "textSize", "windowBackground"]);
    }

    #[test]
    fn comments_are_stripped() {
        assert_eq!(strip_comments("a<!-- INTERNET -->b<!--x-->c"), "abc");
        assert_eq!(strip_comments("a<!-- open"), "a<!-- open");
    }

    fn manifest_problems(xml: &str) -> Vec<String> {
        let root = dom::parse(xml.as_bytes()).unwrap();
        let mut p = Vec::new();
        check_manifest(&root, &strip_comments(xml), &mut p);
        p
    }

    const GOOD: &str = "<manifest xmlns:android=\"u\"><application android:allowBackup=\"false\"><activity android:name=\".MainActivity\" android:exported=\"true\"/><receiver android:name=\".R\" android:exported=\"false\"/></application></manifest>";

    #[test]
    fn a_good_manifest_passes() {
        assert!(manifest_problems(GOOD).is_empty());
    }

    #[test]
    fn manifest_policy_violations_are_reported() {
        let bad = |from: &str, to: &str| manifest_problems(&GOOD.replace(from, to));
        assert!(
            bad(
                "<application ",
                "<uses-permission android:name=\"android.permission.CAMERA\"/><application "
            )[0]
            .contains("CAMERA")
        );
        assert!(
            bad("<application ", "<uses-permission-sdk-23 android:name=\"x\"/><application ")[0]
                .contains("uses-permission-sdk-23")
        );
        assert!(
            bad(
                "<application ",
                "<uses-permission android:name=\"android.permission.INTERNET\"/><application "
            )
            .iter()
            .any(|m| m.contains("INTERNET"))
        );
        assert!(
            bad("android:allowBackup=\"false\"", "android:allowBackup=\"true\"")[0]
                .contains("allowBackup")
        );
        assert!(
            bad("<application ", "<application android:debuggable=\"false\" ")[0]
                .contains("debuggable")
        );
        assert!(bad(" android:exported=\"false\"", "")[0].contains("exported explicitly"));
        assert!(
            bad("\".R\" android:exported=\"false\"", "\".R\" android:exported=\"true\"")[0]
                .contains("allow-list")
        );
        assert!(manifest_problems("<manifest/>")[0].contains("no <application>"));
    }

    #[test]
    fn translation_parity_covers_strings_plurals_and_arrays() {
        let mut en = Values::default();
        let mut ru = Values::default();
        en.strings.insert("a".into(), ("%1$s x".into(), true));
        ru.strings.insert("a".into(), ("%1$d x".into(), true));
        en.strings.insert("b".into(), ("b".into(), true));
        en.plurals.insert("p".into(), (true, vec![("other".into(), "%d files".into())]));
        ru.plurals.insert("p".into(), (true, vec![("other".into(), "files".into())]));
        en.arrays.insert("r".into(), (true, vec!["1".into(), "2".into()]));
        ru.arrays.insert("r".into(), (true, vec!["1".into()]));
        ru.strings.insert("c".into(), ("c".into(), true));
        let mut p = Vec::new();
        check_translations(&en, &ru, &mut p);
        let all = p.join("\n");
        assert!(all.contains("placeholders differ between en"));
        assert!(all.contains("missing translation of 'b'"));
        assert!(all.contains("'c' does not exist in values/"));
        assert!(all.contains("plurals 'p': placeholders of 'other' differ"));
        assert!(all.contains("string-array 'r': 2 items in en, 1 in ru"));
    }

    #[test]
    fn a_small_tree_passes_and_a_dead_string_is_found() {
        let t = TempDir::new("res");
        let app = "android/app/src/main";
        t.write(&format!("{app}/AndroidManifest.xml"), GOOD);
        t.write(&format!("{app}/res/values/strings.xml"), "<resources><string name=\"app_name\">A</string><string name=\"used\">U</string><string name=\"dead\">D</string></resources>");
        t.write(&format!("{app}/res/values-ru/strings.xml"), "<resources><string name=\"app_name\">\u{410}</string><string name=\"used\">\u{423}</string><string name=\"dead\">\u{414}</string></resources>");
        t.write(&format!("{app}/java/X.java"), "class X { int a = R.string.used; }");
        t.write("crates/arca-xray/src/report.rs", "");
        let android = t.0.join("android");
        let err = check(&android, &t.0, None).unwrap_err();
        assert!(err.iter().any(|m| m == "values/dead: never used"), "{err:?}");
        assert!(err.iter().any(|m| m.contains("no threat kinds")), "{err:?}");
    }

    #[test]
    fn a_missing_name_and_malformed_xml_are_problems() {
        let t = TempDir::new("res2");
        let app = "android/app/src/main";
        t.write(&format!("{app}/AndroidManifest.xml"), GOOD);
        t.write(
            &format!("{app}/res/values/strings.xml"),
            "<resources><string name=\"app_name\">A</string>",
        );
        t.write(&format!("{app}/java/X.java"), "class X { int a = R.string.nothing; }");
        let err = check(&t.0.join("android"), &t.0, None).unwrap_err();
        assert!(err.iter().any(|m| m.contains("not well-formed XML")), "{err:?}");
        assert!(err.iter().any(|m| m.contains("R.string.nothing")), "{err:?}");
    }
}
