//! Classification of reference targets found in documents.

use crate::report::TargetClass;

/// Protocol handlers with a history of abuse from documents
/// (Follina `ms-msdt:` CVE-2022-30190, `search-ms:`, Office URI schemes,
/// `mhtml:`/`mk:` for CVE-2021-40444-style chains, ...).
const DANGEROUS_SCHEMES: &[&str] = &[
    "ms-msdt",
    "search-ms",
    "search",
    "ms-search",
    "ms-officecmd",
    "ms-word",
    "ms-excel",
    "ms-powerpoint",
    "ms-visio",
    "ms-access",
    "ms-publisher",
    "ms-project",
    "ms-infopath",
    "ms-spd",
    "ms-settings",
    "ms-appinstaller",
    "ms-cxh",
    "ms-cxh-full",
    "ms-its",
    "its",
    "mk",
    "mhtml",
    "hcp",
    "jar",
    "javascript",
    "vbscript",
    "shell",
    "cmd",
    "data",
    "ms-excel-addin",
    "ms-officeapp",
    "ms-calculator",
    "ms-launch",
    "ldap",
    "res",
    "vbefile",
    "jscript",
    "wscript",
    "msdt",
];

/// Splits off a URI scheme (`scheme:`), if the target has one.
#[must_use]
pub fn scheme(target: &str) -> Option<String> {
    let t = target.trim();
    let (s, _) = t.split_once(':')?;
    // A single letter before ':' is a drive letter, not a scheme.
    if s.len() < 2 {
        return None;
    }
    let mut chars = s.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

/// Classifies a target string.
#[must_use]
pub fn classify(target: &str) -> TargetClass {
    let t = target.trim().trim_matches('"');
    if t.starts_with("\\\\") || t.starts_with("//") {
        return TargetClass::Unc;
    }
    let bytes = t.as_bytes();
    if let [d, b':', sep, ..] = bytes
        && d.is_ascii_alphabetic()
        && (*sep == b'\\' || *sep == b'/')
    {
        return TargetClass::LocalFile;
    }
    let Some(s) = scheme(t) else { return TargetClass::Relative };
    match s.as_str() {
        "http" | "https" => {
            // WebDAV-over-HTTP to a raw IP or with credentials is still Web.
            TargetClass::Web
        }
        "file" => {
            let rest = t.get(5..).unwrap_or("").trim_start_matches('/');
            // file:///C:/... is local; file://host/share is UNC.
            let after_slashes = t.get(5..).unwrap_or("");
            let slash_count = after_slashes.chars().take_while(|c| *c == '/' || *c == '\\').count();
            let rb = rest.as_bytes();
            let is_drive = matches!(rb, [d, b':' | b'|', ..] if d.is_ascii_alphabetic());
            if slash_count >= 3 || is_drive || rest.to_ascii_lowercase().starts_with("localhost/") {
                TargetClass::LocalFile
            } else {
                TargetClass::Unc
            }
        }
        "mailto" => TargetClass::Mail,
        other if DANGEROUS_SCHEMES.contains(&other) => TargetClass::DangerousProtocol,
        _ => TargetClass::OtherScheme,
    }
}

/// `true` for classes that touch the network when resolved.
#[must_use]
pub const fn is_network(c: TargetClass) -> bool {
    matches!(
        c,
        TargetClass::Web
            | TargetClass::Unc
            | TargetClass::DangerousProtocol
            | TargetClass::OtherScheme
    )
}

/// Extracts a user-name segment from Windows profile paths
/// (`C:\Users\<name>\...`, `/Users/<name>/...`, `/home/<name>/...`).
#[must_use]
pub fn profile_user(path: &str) -> Option<String> {
    let norm = path.replace('\\', "/");
    let lower = norm.to_ascii_lowercase();
    for marker in ["/users/", "/documents and settings/", "/home/"] {
        if let Some(i) = lower.find(marker) {
            let rest = norm.get(i + marker.len()..)?;
            let user = rest.split('/').next()?;
            if !user.is_empty()
                && !user.eq_ignore_ascii_case("public")
                && !user.eq_ignore_ascii_case("default")
            {
                return Some(user.to_owned());
            }
        }
    }
    None
}

/// Extracts the host of a UNC or `file://host/` target.
#[must_use]
pub fn unc_host(target: &str) -> Option<String> {
    let t = target.trim();
    let rest = t.strip_prefix("\\\\").or_else(|| t.strip_prefix("//")).or_else(|| {
        t.get(..7).filter(|p| p.eq_ignore_ascii_case("file://")).and_then(|_| t.get(7..))
    })?;
    let host = rest.split(['\\', '/']).next()?;
    let host = host.split('@').next().unwrap_or(host);
    if host.is_empty() { None } else { Some(host.to_owned()) }
}

/// Decodes `%XX` escapes (invalid escapes are kept verbatim).
#[must_use]
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) =
                (bytes.get(i + 1).and_then(|c| hex(*c)), bytes.get(i + 2).and_then(|c| hex(*c)))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Finds `http(s)://` URLs in free text (macro source, field codes, PDF).
#[must_use]
pub fn find_urls(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut start = 0usize;
    while out.len() < max {
        let Some(rel) = lower.get(start..).and_then(|s| s.find("http")) else { break };
        let at = start + rel;
        let tail = lower.get(at..).unwrap_or("");
        let skip = if tail.starts_with("https://") {
            8
        } else if tail.starts_with("http://") {
            7
        } else {
            start = at + 4;
            continue;
        };
        let end = text
            .get(at..)
            .unwrap_or("")
            .char_indices()
            .find(|(_, c)| {
                c.is_whitespace()
                    || matches!(c, '"' | '\'' | '<' | '>' | ')' | '(' | '`' | '\\' | ',')
            })
            .map_or(text.len(), |(i, _)| at + i);
        if end > at + skip
            && let Some(url) = text.get(at..end)
            && !out.iter().any(|u| u == url)
        {
            out.push(url.to_owned());
        }
        start = end.max(at + skip);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("https://evil.example/t.png"), TargetClass::Web);
        assert_eq!(classify("\\\\10.0.0.1\\share\\x.dotm"), TargetClass::Unc);
        assert_eq!(classify("file://attacker/share/t.dotm"), TargetClass::Unc);
        assert_eq!(
            classify("file:///C:/Users/ivan/AppData/Roaming/Microsoft/Templates/Normal.dotm"),
            TargetClass::LocalFile
        );
        assert_eq!(classify("C:\\Users\\ivan\\Desktop\\a.xlsx"), TargetClass::LocalFile);
        assert_eq!(
            classify("ms-msdt:/id PCWDiagnostic /skip force"),
            TargetClass::DangerousProtocol
        );
        assert_eq!(classify("mhtml:http://x/!x-usc:http://y"), TargetClass::DangerousProtocol);
        assert_eq!(classify("mailto:a@b.c"), TargetClass::Mail);
        assert_eq!(classify("media/image1.png"), TargetClass::Relative);
        assert_eq!(classify("../media/image1.png"), TargetClass::Relative);
        assert_eq!(classify("foo:bar"), TargetClass::OtherScheme);
    }

    #[test]
    fn helpers() {
        assert_eq!(profile_user("C:\\Users\\Ivan.Petrov\\Documents\\"), Some("Ivan.Petrov".into()));
        assert_eq!(profile_user("/home/alice/x.odt"), Some("alice".into()));
        assert_eq!(unc_host("\\\\fileserver01\\docs\\a"), Some("fileserver01".into()));
        assert_eq!(unc_host("file://evil.example/x"), Some("evil.example".into()));
        assert_eq!(percent_decode("a%20b%2"), "a b%2");
        assert_eq!(
            find_urls("x = \"http://a.example/p\" & 'https://b.example/q?x=1'", 10),
            vec!["http://a.example/p".to_owned(), "https://b.example/q?x=1".to_owned()]
        );
    }
}
