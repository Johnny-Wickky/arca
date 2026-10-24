//! Validation of archive member names (anti Zip-Slip / path confusion).
//!
//! Arca never extracts archives to a file system, but member names are
//! still security relevant: OPC relationship targets are resolved against
//! them, and two readers that disagree on what a name means is the classic
//! recipe for a parser-differential bypass. Names are therefore held to a
//! strict canonical form and anything else is rejected, not "repaired".

use core::fmt;

/// Why a member name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRejection {
    /// The name is empty.
    Empty,
    /// The name exceeds the configured maximum length.
    TooLong,
    /// The raw bytes are not valid UTF-8.
    InvalidUtf8,
    /// The name contains NUL, a C0/C1 control character or DEL.
    ControlCharacter,
    /// The name contains a backslash (Windows separator / traversal trick).
    Backslash,
    /// The name starts with `/` (absolute path).
    Absolute,
    /// The name starts with a drive letter such as `C:`.
    DriveLetter,
    /// A path component is `..`.
    ParentTraversal,
    /// A path component is `.`.
    DotComponent,
    /// The name contains an empty component (`a//b`).
    EmptyComponent,
    /// The name contains a Unicode bidirectional override.
    BidiControl,
    /// The name contains a character that draws nothing (zero-width, tag, separator, filler).
    InvisibleCharacter,
    /// A component ends in `.` or a space, which Windows silently drops.
    TrailingDotOrSpace,
    /// The name contains `:` (drive, alternate data stream, URI scheme).
    Colon,
    /// The name contains a percent-encoded dot, slash or backslash (`%2e`, `%2f`, `%5c`).
    PercentEncodedSeparator,
}

impl fmt::Display for NameRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Empty => "empty name",
            Self::TooLong => "name too long",
            Self::InvalidUtf8 => "name is not valid UTF-8",
            Self::ControlCharacter => "name contains a control character",
            Self::Backslash => "name contains a backslash",
            Self::Absolute => "absolute path",
            Self::DriveLetter => "drive-letter path",
            Self::ParentTraversal => "parent directory traversal ('..')",
            Self::DotComponent => "'.' path component",
            Self::EmptyComponent => "empty path component",
            Self::BidiControl => "bidirectional override character",
            Self::InvisibleCharacter => "invisible (format, separator or filler) character",
            Self::TrailingDotOrSpace => "path component ends in a dot or a space",
            Self::Colon => "':' in the name",
            Self::PercentEncodedSeparator => "percent-encoded dot or separator",
        };
        f.write_str(s)
    }
}

/// Validates a member name and returns it as `&str`.
///
/// A trailing `/` marks a directory entry and is permitted.
///
/// # Errors
/// Returns the first [`NameRejection`] that applies.
pub fn validate_member_name(raw: &[u8], max_len: usize) -> Result<&str, NameRejection> {
    if raw.is_empty() {
        return Err(NameRejection::Empty);
    }
    if raw.len() > max_len {
        return Err(NameRejection::TooLong);
    }
    let name = core::str::from_utf8(raw).map_err(|_| NameRejection::InvalidUtf8)?;
    validate_member_str(name)?;
    Ok(name)
}

/// Validates an already decoded member name (see [`validate_member_name`]).
///
/// # Errors
/// Returns the first [`NameRejection`] that applies.
pub fn validate_member_str(name: &str) -> Result<(), NameRejection> {
    if name.is_empty() {
        return Err(NameRejection::Empty);
    }
    for c in name.chars() {
        if c.is_control() {
            return Err(NameRejection::ControlCharacter);
        }
        if c == '\\' {
            return Err(NameRejection::Backslash);
        }
        if crate::text::is_bidi_control(c) {
            return Err(NameRejection::BidiControl);
        }
        if crate::text::is_invisible(c) {
            return Err(NameRejection::InvisibleCharacter);
        }
    }
    if name.starts_with('/') {
        return Err(NameRejection::Absolute);
    }
    let bytes = name.as_bytes();
    if let [first, b':', ..] = bytes
        && first.is_ascii_alphabetic()
    {
        return Err(NameRejection::DriveLetter);
    }
    let body = name.strip_suffix('/').unwrap_or(name);
    if body.is_empty() {
        return Err(NameRejection::EmptyComponent);
    }
    for component in body.split('/') {
        match component {
            "" => return Err(NameRejection::EmptyComponent),
            "." => return Err(NameRejection::DotComponent),
            ".." => return Err(NameRejection::ParentTraversal),
            _ => {}
        }
        if component.ends_with(['.', ' ']) {
            return Err(NameRejection::TrailingDotOrSpace);
        }
    }
    if name.contains(':') {
        return Err(NameRejection::Colon);
    }
    // `%2e%2e/` is `../` to a reader that decodes names (URI-style OPC resolvers do).
    let lower = name.to_ascii_lowercase();
    if ["%2e", "%2f", "%5c"].iter().any(|p| lower.contains(p)) {
        return Err(NameRejection::PercentEncodedSeparator);
    }
    Ok(())
}

/// Case-folds a member name for duplicate detection and lookups.
///
/// OPC part names are ASCII case-insensitive; full Unicode case folding is
/// deliberately not applied so that look-alike names never collapse into one.
#[must_use]
pub fn fold_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_names() {
        for ok in
            ["[Content_Types].xml", "word/document.xml", "_rels/.rels", "customXml/", "a/b/c.bin"]
        {
            assert!(validate_member_name(ok.as_bytes(), 1024).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_traversal_and_friends() {
        let cases: &[(&[u8], NameRejection)] = &[
            (b"", NameRejection::Empty),
            (b"../evil", NameRejection::ParentTraversal),
            (b"word/../../evil", NameRejection::ParentTraversal),
            (b"/etc/passwd", NameRejection::Absolute),
            (b"C:/Windows/x", NameRejection::DriveLetter),
            (b"..\\evil", NameRejection::Backslash),
            (b"a//b", NameRejection::EmptyComponent),
            (b"./a", NameRejection::DotComponent),
            (b"a\0b", NameRejection::ControlCharacter),
            (b"\xff\xfe", NameRejection::InvalidUtf8),
            ("doc\u{202E}lmx.exe".as_bytes(), NameRejection::BidiControl),
            (b"/", NameRejection::Absolute),
            ("a\u{200B}b".as_bytes(), NameRejection::InvisibleCharacter),
            ("a\u{E0041}".as_bytes(), NameRejection::InvisibleCharacter),
            ("a\u{2028}b".as_bytes(), NameRejection::InvisibleCharacter),
            ("a\u{00AD}b".as_bytes(), NameRejection::InvisibleCharacter),
            (b"word/document.xml.", NameRejection::TrailingDotOrSpace),
            (b"word/document.xml ", NameRejection::TrailingDotOrSpace),
            (b"word./document.xml", NameRejection::TrailingDotOrSpace),
            (b"word/doc:stream.xml", NameRejection::Colon),
            (b"%2e%2e/evil", NameRejection::PercentEncodedSeparator),
            (b"word%2Fdocument.xml", NameRejection::PercentEncodedSeparator),
            (b"word%5cdocument.xml", NameRejection::PercentEncodedSeparator),
        ];
        for (raw, expected) in cases {
            assert_eq!(validate_member_name(raw, 1024), Err(*expected), "{raw:?}");
        }
        assert_eq!(validate_member_name(b"abcdef", 3), Err(NameRejection::TooLong));
    }
}
