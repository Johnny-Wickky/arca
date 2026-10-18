//! `fork_attribution.toml`: the additive attribution of a modified version.
//!
//! The file is parsed with a deliberately tiny, strict TOML subset (tables,
//! arrays of tables, `key = "string"` / `key = 'string'`, comments). Anything
//! else — numbers, booleans, inline tables, unknown keys or tables — is an
//! error with a line number. The same code runs in `build.rs`, so a fork with
//! an invalid file fails to *compile* instead of shipping a broken notice.
//!
//! ```toml
//! [fork]
//! name = "SuperDocs"
//! homepage = "https://superdocs.example"
//! source = "https://github.com/acme/superdocs"
//! modifications = "Adds OCR"            # optional
//!
//! [[fork.maintainers]]
//! name = "ACME Team"
//! url = "https://acme.example"           # optional
//!
//! [[fork.donations]]
//! label = "Open Collective"
//! url = "https://opencollective.com/superdocs"
//! ```

use core::fmt;
use std::collections::BTreeMap;

use super::upstream;

/// A maintainer of the modified version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPerson {
    /// Display name.
    pub name: String,
    /// Optional HTTPS URL.
    pub url: Option<String>,
}

/// A donation channel of the modified version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkDonation {
    /// Short label.
    pub label: String,
    /// HTTPS URL.
    pub url: String,
}

/// The validated attribution of a modified version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkAttribution {
    /// Name of the modified version (must differ from the original, §7(c)).
    pub name: String,
    /// Homepage.
    pub homepage: Option<String>,
    /// Where the fork's Corresponding Source is published (AGPLv3 §13).
    pub source: Option<String>,
    /// Short description of the modifications.
    pub modifications: Option<String>,
    /// Maintainers (at least one).
    pub maintainers: Vec<ForkPerson>,
    /// Donation channels (at most eight).
    pub donations: Vec<ForkDonation>,
}

/// Parse or validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkConfigError {
    /// Syntax error.
    Syntax {
        /// 1-based line.
        line: usize,
        /// Explanation.
        message: String,
    },
    /// A key or table this format does not know.
    Unknown {
        /// 1-based line.
        line: usize,
        /// The offending key or table.
        key: String,
    },
    /// A value is invalid.
    Invalid {
        /// Field path.
        field: String,
        /// Explanation.
        message: String,
    },
    /// The fork presents itself under the original name or authors.
    ImpersonatesUpstream(String),
}

impl fmt::Display for ForkConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, message } => write!(f, "line {line}: {message}"),
            Self::Unknown { line, key } => write!(f, "line {line}: unknown key or table {key:?}"),
            Self::Invalid { field, message } => write!(f, "{field}: {message}"),
            Self::ImpersonatesUpstream(what) => write!(
                f,
                "{what:?} is the name of the original project or its creators; a modified version must use its own \
                 name (LICENSE_ADDITIONAL_TERMS.md, section 5.2; AGPLv3 §7(c))"
            ),
        }
    }
}

impl core::error::Error for ForkConfigError {}

type Table = BTreeMap<String, String>;

#[derive(Default)]
struct Raw {
    declared: bool,
    fork_header: bool,
    fork: Table,
    maintainers: Vec<Table>,
    donations: Vec<Table>,
}

enum Section {
    Fork,
    Maintainer,
    Donation,
}

fn syntax(line: usize, message: &str) -> ForkConfigError {
    ForkConfigError::Syntax { line, message: message.to_owned() }
}

fn parse_string(raw: &str, line: usize) -> Result<(String, &str), ForkConfigError> {
    let mut chars = raw.char_indices();
    let Some((_, quote)) = chars.next() else { return Err(syntax(line, "expected a string")) };
    if quote == '\'' {
        let body = raw.get(1..).unwrap_or("");
        let end = body.find('\'').ok_or_else(|| syntax(line, "unterminated literal string"))?;
        return Ok((body.get(..end).unwrap_or("").to_owned(), body.get(end + 1..).unwrap_or("")));
    }
    if quote != '"' {
        return Err(syntax(line, "only quoted strings are supported"));
    }
    let mut out = String::new();
    let mut escaped = false;
    let mut end_at = None;
    let mut unicode: Option<(u32, usize)> = None;
    for (i, c) in chars {
        if let Some((acc, left)) = unicode.take() {
            let d = c.to_digit(16).ok_or_else(|| syntax(line, "invalid \\u escape"))?;
            let acc = acc * 16 + d;
            if left == 1 {
                out.push(char::from_u32(acc).ok_or_else(|| syntax(line, "invalid \\u escape"))?);
            } else {
                unicode = Some((acc, left - 1));
            }
            continue;
        }
        if escaped {
            match c {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'u' => unicode = Some((0, 4)),
                _ => return Err(syntax(line, "unsupported escape")),
            }
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => {
                end_at = Some(i);
                break;
            }
            _ => out.push(c),
        }
    }
    let end = end_at.ok_or_else(|| syntax(line, "unterminated string"))?;
    Ok((out, raw.get(end + 1..).unwrap_or("")))
}

fn parse_raw(src: &str) -> Result<Raw, ForkConfigError> {
    let mut raw = Raw::default();
    let mut section: Option<Section> = None;
    for (idx, full) in src.lines().enumerate() {
        let line = idx + 1;
        let t = full.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        // Table headers may carry a trailing comment; table names never contain '#'.
        let header =
            if t.starts_with('[') { t.split('#').next().unwrap_or("").trim_end() } else { "" };
        if let Some(name) = header.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
            raw.declared = true;
            section = Some(match name.trim() {
                "fork.maintainers" => {
                    raw.maintainers.push(Table::new());
                    Section::Maintainer
                }
                "fork.donations" => {
                    raw.donations.push(Table::new());
                    Section::Donation
                }
                other => return Err(ForkConfigError::Unknown { line, key: other.to_owned() }),
            });
            continue;
        }
        if let Some(name) = header.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            match name.trim() {
                "fork" if !raw.fork_header => {
                    raw.declared = true;
                    raw.fork_header = true;
                    section = Some(Section::Fork);
                }
                "fork" => return Err(syntax(line, "duplicate [fork] table")),
                other => return Err(ForkConfigError::Unknown { line, key: other.to_owned() }),
            }
            continue;
        }
        let (key, value) =
            t.split_once('=').ok_or_else(|| syntax(line, "expected `key = \"value\"`"))?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(syntax(line, "keys must be bare ASCII identifiers"));
        }
        let (value, rest) = parse_string(value.trim(), line)?;
        let rest = rest.trim();
        if !rest.is_empty() && !rest.starts_with('#') {
            return Err(syntax(line, "unexpected characters after the value"));
        }
        let (table, allowed): (&mut Table, &[&str]) = match section {
            Some(Section::Fork) => {
                (&mut raw.fork, &["name", "homepage", "source", "modifications"])
            }
            Some(Section::Maintainer) => match raw.maintainers.last_mut() {
                Some(t) => (t, &["name", "url"]),
                None => return Err(syntax(line, "key outside a table")),
            },
            Some(Section::Donation) => match raw.donations.last_mut() {
                Some(t) => (t, &["label", "url"]),
                None => return Err(syntax(line, "key outside a table")),
            },
            None => return Err(syntax(line, "key outside a table")),
        };
        if !allowed.contains(&key) {
            return Err(ForkConfigError::Unknown { line, key: key.to_owned() });
        }
        if table.insert(key.to_owned(), value).is_some() {
            return Err(syntax(line, "duplicate key"));
        }
    }
    Ok(raw)
}

fn normalized(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase()
}

fn check_text(field: &str, value: &str, max: usize) -> Result<String, ForkConfigError> {
    let v = value.trim();
    let invalid =
        |m: &str| ForkConfigError::Invalid { field: field.to_owned(), message: m.to_owned() };
    if v.is_empty() {
        return Err(invalid("must not be empty"));
    }
    if v.chars().count() > max {
        return Err(invalid(&format!("longer than {max} characters")));
    }
    if v.chars().any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')) {
        return Err(invalid("contains control or bidirectional override characters"));
    }
    Ok(v.to_owned())
}

/// Validates an HTTPS URL (no credentials, plausible host).
pub fn check_https(field: &str, url: &str) -> Result<String, ForkConfigError> {
    let invalid =
        |m: &str| ForkConfigError::Invalid { field: field.to_owned(), message: m.to_owned() };
    let u = url.trim();
    if u.len() > 200 {
        return Err(invalid("URL longer than 200 characters"));
    }
    let rest = u.strip_prefix("https://").ok_or_else(|| invalid("URL must start with https://"))?;
    if u.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '<' || c == '>') {
        return Err(invalid("URL contains whitespace or forbidden characters"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(invalid("URL must not contain credentials"));
    }
    let host = authority.split(':').next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    let ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !l.starts_with('-')
        });
    if !ok {
        return Err(invalid("URL host is not a valid domain name"));
    }
    Ok(u.to_owned())
}

/// Parses and validates a fork attribution file.
///
/// A file without any table (only comments and blank lines — the state in
/// which the original project ships it) means "not a fork": `Ok(None)`.
///
/// # Errors
/// See [`ForkConfigError`].
pub fn parse(src: &str) -> Result<Option<ForkAttribution>, ForkConfigError> {
    let raw = parse_raw(src)?;
    if !raw.declared {
        return Ok(None);
    }
    let name = check_text("fork.name", raw.fork.get("name").map_or("", String::as_str), 64)?;
    let upstream_names: Vec<String> = core::iter::once(upstream::UPSTREAM_PROJECT)
        .chain(upstream::UPSTREAM_AUTHORS.iter().map(|a| a.name))
        .map(normalized)
        .collect();
    if upstream_names.contains(&normalized(&name)) {
        return Err(ForkConfigError::ImpersonatesUpstream(name));
    }
    let homepage = raw.fork.get("homepage").map(|u| check_https("fork.homepage", u)).transpose()?;
    let source = raw.fork.get("source").map(|u| check_https("fork.source", u)).transpose()?;
    let modifications = raw
        .fork
        .get("modifications")
        .map(|m| check_text("fork.modifications", m, 280))
        .transpose()?;

    if raw.maintainers.is_empty() {
        return Err(ForkConfigError::Invalid {
            field: "fork.maintainers".into(),
            message: "at least one maintainer is required".into(),
        });
    }
    if raw.maintainers.len() > 16 {
        return Err(ForkConfigError::Invalid {
            field: "fork.maintainers".into(),
            message: "at most 16 maintainers".into(),
        });
    }
    let mut maintainers = Vec::new();
    for (i, m) in raw.maintainers.iter().enumerate() {
        let field = format!("fork.maintainers[{i}]");
        let name =
            check_text(&format!("{field}.name"), m.get("name").map_or("", String::as_str), 80)?;
        if upstream_names.contains(&normalized(&name)) {
            return Err(ForkConfigError::ImpersonatesUpstream(name));
        }
        let url = m.get("url").map(|u| check_https(&format!("{field}.url"), u)).transpose()?;
        maintainers.push(ForkPerson { name, url });
    }
    if raw.donations.len() > 8 {
        return Err(ForkConfigError::Invalid {
            field: "fork.donations".into(),
            message: "at most 8 donation links".into(),
        });
    }
    let mut donations = Vec::new();
    for (i, d) in raw.donations.iter().enumerate() {
        let field = format!("fork.donations[{i}]");
        let label =
            check_text(&format!("{field}.label"), d.get("label").map_or("", String::as_str), 40)?;
        let url = check_https(&format!("{field}.url"), d.get("url").map_or("", String::as_str))?;
        if upstream::UPSTREAM_DONATION_LINKS.iter().any(|l| l.url == url) {
            return Err(ForkConfigError::Invalid {
                field,
                message: "repeats an upstream donation link; it is already shown".into(),
            });
        }
        donations.push(ForkDonation { label, url });
    }
    Ok(Some(ForkAttribution { name, homepage, source, modifications, maintainers, donations }))
}
