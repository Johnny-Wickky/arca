//! Classification of reference targets found in documents.

use std::borrow::Cow;

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
    // Windows file sharing: opening it authenticates against the host with the user's NTLM
    // credentials, exactly like a UNC path.
    "smb",
    // Mail and note clients that open the item they are pointed at.
    "ms-outlook",
    "outlook",
    "onenote",
    "ms-onenote",
];

/// Removes ASCII control characters (tab, CR, LF, NUL, ...) anywhere in a
/// target. URL parsers ignore or truncate at them (`java\tscript:`,
/// `ht\ntp://`), so a classifier that did not would call a disguised scheme
/// "relative". Removal can only make a target look *more* like what it is.
fn canonical(target: &str) -> Cow<'_, str> {
    if target.chars().any(|c| c.is_ascii_control()) {
        Cow::Owned(target.chars().filter(|c| !c.is_ascii_control()).collect())
    } else {
        Cow::Borrowed(target)
    }
}

/// `true` if `target` leaves the package or document: a URI with any scheme,
/// a UNC or network-path (`//host/..`) reference, or an absolute drive path.
/// Control characters cannot hide it (see [`canonical`]).
#[must_use]
pub fn leaves_package(target: &str) -> bool {
    classify(target) != TargetClass::Relative
}

/// Splits off a URI scheme (`scheme:`), if the target has one.
#[must_use]
pub fn scheme(target: &str) -> Option<String> {
    let canon = canonical(target);
    let t = canon.trim();
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

/// `canonical` plus percent-decoding, repeated while it changes (`%255C` is `%5C` is `\`), up to
/// three rounds.
fn normalized(target: &str) -> Cow<'_, str> {
    let mut cur = canonical(target);
    for _ in 0..3 {
        if !cur.contains('%') {
            break;
        }
        let decoded = percent_decode(&cur);
        if decoded == *cur {
            break;
        }
        cur = Cow::Owned(canonical(&decoded).into_owned());
    }
    cur
}

/// `true` for a network-path reference: two leading separators, each of which may be a slash or
/// a backslash in any mix (`\host`, `//host`, `\/host`, `/\host`).
fn is_network_path(t: &str) -> bool {
    let b = t.as_bytes();
    matches!(b.first(), Some(b'\\' | b'/')) && matches!(b.get(1), Some(b'\\' | b'/'))
}

/// Classifies a target string.
#[must_use]
pub fn classify(target: &str) -> TargetClass {
    // Escapes are decoded first (`file:///%5C%5Chost` is a UNC path); a reader decodes them
    // before it resolves the target, whatever the scanner thinks of the text.
    let canon = normalized(target);
    let t = canon.trim().trim_matches('"');
    if is_network_path(t) {
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
        "file" => classify_file(t.get(5..).unwrap_or("")),
        "mailto" => TargetClass::Mail,
        other if DANGEROUS_SCHEMES.contains(&other) || other.starts_with("ms-") => {
            TargetClass::DangerousProtocol
        }
        _ => TargetClass::OtherScheme,
    }
}

/// Classifies the body of a `file:` URI (everything after `file:`).
///
/// A `file:` target is remote — and a potential NTLM-leak vector — whenever it
/// resolves to a host. The counting-slashes heuristic used previously treated
/// `file:///\\host\share` and `file:////host/share` as local because of the
/// leading slash run; both actually reach a remote host and leak NTLM. The
/// rule here keys on *where the host is*, not on the slash count:
///
/// * an embedded Windows UNC path after the scheme (`file://\\host`,
///   `file:///\\host`) is always remote;
/// * two leading slashes (`file://host/...`) put the host in the authority;
/// * four or more (`file:////host/...`) are the empty-authority UNC form
///   Windows resolves as `\\host`;
/// * only a hostless path — a drive (`file:///C:/x`), one or three slashes with
///   no host (`file:/x`, `file:///home/u/x`), or `localhost` — is local.
fn classify_file(body: &str) -> TargetClass {
    let after_fwd = body.trim_start_matches('/');
    let fwd = body.len() - after_fwd.len();
    // An embedded UNC path (`\\host\share`) after the scheme is always remote,
    // whatever the number of leading forward slashes before it.
    if after_fwd.starts_with('\\') {
        return TargetClass::Unc;
    }
    if matches!(after_fwd.as_bytes(), [d, b':' | b'|', ..] if d.is_ascii_alphabetic()) {
        return TargetClass::LocalFile;
    }
    let lower = after_fwd.to_ascii_lowercase();
    if lower == "localhost" || lower.starts_with("localhost/") || lower.starts_with("localhost\\") {
        return TargetClass::LocalFile;
    }
    match fwd {
        2 => TargetClass::Unc,           // file://host/...
        n if n >= 4 => TargetClass::Unc, // file:////host/... → //host path = UNC
        _ => TargetClass::LocalFile,     // file:/x, file:///home/u/x → hostless local path
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
    let canon = normalized(target);
    let t = canon.trim();
    let rest = if is_network_path(t) {
        t.get(2..)
    } else {
        t.get(..7).filter(|p| p.eq_ignore_ascii_case("file://")).and_then(|_| t.get(7..))
    }?;
    // Skip any leading slash/backslash run (`file:///\\host`, `file:////host`).
    let rest = rest.trim_start_matches(['\\', '/']);
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
        // Regression (A-2): file: forms that reach a host are UNC, not LocalFile.
        // Leading-slash runs and embedded backslash UNC paths must not disguise them.
        assert_eq!(classify(r"file:///\\evil\share\t.dotm"), TargetClass::Unc);
        assert_eq!(classify("file:////evil/share/t.dotm"), TargetClass::Unc);
        assert_eq!(classify(r"file://\\evil\share\t.dotm"), TargetClass::Unc);
        assert_eq!(classify("FILE://EVIL/share/x"), TargetClass::Unc);
        // Genuinely local file: targets stay local.
        assert_eq!(classify("file:///home/ivan/x.odt"), TargetClass::LocalFile);
        assert_eq!(classify("file:/etc/hosts"), TargetClass::LocalFile);
        assert_eq!(classify("file://localhost/C:/x"), TargetClass::LocalFile);
        assert_eq!(classify("file:///c|/temp/x"), TargetClass::LocalFile);
        assert_eq!(
            classify("ms-msdt:/id PCWDiagnostic /skip force"),
            TargetClass::DangerousProtocol
        );
        assert_eq!(classify("mhtml:http://x/!x-usc:http://y"), TargetClass::DangerousProtocol);
        assert_eq!(classify("mailto:a@b.c"), TargetClass::Mail);
        assert_eq!(classify("media/image1.png"), TargetClass::Relative);
        assert_eq!(classify("../media/image1.png"), TargetClass::Relative);
        assert_eq!(classify("foo:bar"), TargetClass::OtherScheme);
        // Control characters cannot disguise a scheme or a UNC path (F-4 hardening).
        assert_eq!(classify("java\tscript:alert(1)"), TargetClass::DangerousProtocol);
        assert_eq!(classify("ms-ms\ndt:/id x"), TargetClass::DangerousProtocol);
        assert_eq!(classify("ht\ttp://evil.example/"), TargetClass::Web);
        assert_eq!(classify("\\\\\u{0}evil\\share\\x"), TargetClass::Unc);
        assert_eq!(classify("/\t/evil/share/x.dotm"), TargetClass::Unc);
        // A network-path reference (`//host/...`) is remote even without a scheme.
        assert_eq!(classify("//host/share/x.dotm"), TargetClass::Unc);
        assert_eq!(classify("/word/media/image1.png"), TargetClass::Relative);
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
