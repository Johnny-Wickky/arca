//! Text decoding helpers for legacy binary formats.
//!
//! Binary Office formats store strings as UTF-16LE or as 8-bit text in a
//! Windows code page (VBA source, OLE property sets, BIFF names). Only the
//! code pages that matter for Arca' audience are decoded natively; any
//! other code page degrades to a lossy, clearly marked ASCII rendering instead
//! of failing the scan.

/// Decodes UTF-16LE bytes, stopping at the first NUL code unit.
///
/// Unpaired surrogates and a trailing odd byte become U+FFFD.
#[must_use]
pub fn utf16le_until_nul(bytes: &[u8]) -> String {
    let units = bytes
        .chunks_exact(2)
        .map(|p| {
            u16::from_le_bytes([p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0)])
        })
        .take_while(|&u| u != 0);
    char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// Decodes all UTF-16LE code units in `bytes` (NUL is kept as U+0000).
#[must_use]
pub fn utf16le_lossy(bytes: &[u8]) -> String {
    let units = bytes.chunks_exact(2).map(|p| {
        u16::from_le_bytes([p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0)])
    });
    char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// Windows code pages recognised by [`decode_codepage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePage {
    /// UTF-8 (65001).
    Utf8,
    /// UTF-16LE (1200).
    Utf16Le,
    /// Windows-1251, Cyrillic.
    Windows1251,
    /// Windows-1252, Western European.
    Windows1252,
    /// ISO-8859-1 / Latin-1 (28591).
    Latin1,
    /// Any other code page: decoded as ASCII with U+FFFD for bytes ≥ 0x80.
    Unsupported(u16),
}

impl CodePage {
    /// Maps a Windows code page identifier.
    #[must_use]
    pub const fn from_id(id: u16) -> Self {
        match id {
            65001 => Self::Utf8,
            1200 => Self::Utf16Le,
            1251 => Self::Windows1251,
            1252 | 0 => Self::Windows1252,
            28591 => Self::Latin1,
            other => Self::Unsupported(other),
        }
    }
}

/// Decodes `bytes` in the given code page. Never fails.
#[must_use]
pub fn decode_codepage(bytes: &[u8], cp: CodePage) -> String {
    match cp {
        CodePage::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        CodePage::Utf16Le => utf16le_lossy(bytes),
        CodePage::Windows1251 => bytes.iter().map(|&b| cp1251(b)).collect(),
        CodePage::Windows1252 => bytes.iter().map(|&b| cp1252(b)).collect(),
        CodePage::Latin1 => bytes.iter().map(|&b| char::from(b)).collect(),
        CodePage::Unsupported(_) => bytes
            .iter()
            .map(|&b| if b < 0x80 { char::from(b) } else { char::REPLACEMENT_CHARACTER })
            .collect(),
    }
}

/// Windows-1251 upper half 0x80..=0xBF (0xC0..=0xFF map to U+0410..U+044F).
const CP1251_80_BF: [u16; 64] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039,
    0x040A, 0x040C, 0x040B, 0x040F, 0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0xFFFD, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F, 0x00A0, 0x040E, 0x045E, 0x0408,
    0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407,
    0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB,
    0x0458, 0x0405, 0x0455, 0x0457,
];

/// Windows-1252 range 0x80..=0x9F (the rest is identical to Latin-1).
const CP1252_80_9F: [u16; 32] = [
    0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0xFFFD, 0x017D, 0xFFFD, 0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0xFFFD, 0x017E, 0x0178,
];

fn bmp(code: u16) -> char {
    char::from_u32(u32::from(code)).unwrap_or(char::REPLACEMENT_CHARACTER)
}

fn cp1251(b: u8) -> char {
    match b {
        0x00..=0x7F => char::from(b),
        0x80..=0xBF => bmp(CP1251_80_BF.get(usize::from(b - 0x80)).copied().unwrap_or(0xFFFD)),
        0xC0..=0xFF => bmp(0x0410 + u16::from(b - 0xC0)),
    }
}

fn cp1252(b: u8) -> char {
    match b {
        0x80..=0x9F => bmp(CP1252_80_9F.get(usize::from(b - 0x80)).copied().unwrap_or(0xFFFD)),
        _ => char::from(b),
    }
}

/// Renders a string for display: control characters become `\xNN`, the
/// result is truncated to `max_chars` characters (with an ellipsis).
#[must_use]
pub fn display_safe(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (count, c) in s.chars().enumerate() {
        if count >= max_chars {
            out.push('…');
            break;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            out.push_str(&format!("\\x{:02X}", u32::from(c)));
        } else if is_bidi_control(c) {
            // Trojan-Source style reordering characters are made visible.
            out.push_str(&format!("\\u{{{:04X}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// Unicode bidirectional formatting characters (CVE-2021-42574 class).
#[must_use]
pub const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

/// Truncates `s` to at most `max_bytes` bytes on a character boundary.
#[must_use]
pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyrillic_cp1251() {
        // "Привет" in Windows-1251
        let bytes = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(decode_codepage(&bytes, CodePage::Windows1251), "Привет");
        assert_eq!(decode_codepage(&[0xA8, 0xB8, 0xB9], CodePage::Windows1251), "Ёё№");
    }

    #[test]
    fn western_cp1252() {
        assert_eq!(decode_codepage(&[0x80, 0x93, 0xE9], CodePage::Windows1252), "€“é");
    }

    #[test]
    fn utf16_helpers() {
        let bytes = [0x41, 0x00, 0x42, 0x00, 0x00, 0x00, 0x43, 0x00];
        assert_eq!(utf16le_until_nul(&bytes), "AB");
        assert_eq!(utf16le_lossy(&bytes[..4]), "AB");
    }

    #[test]
    fn display_safe_escapes_controls_and_bidi() {
        assert_eq!(display_safe("\u{5}Summary", 50), "\\x05Summary");
        assert_eq!(display_safe("a\u{202E}b", 50), "a\\u{202E}b");
        assert_eq!(display_safe("abcdef", 3), "abc…");
    }

    #[test]
    fn truncate_on_boundary() {
        assert_eq!(truncate_utf8("ПРИВЕТ", 3), "П");
        assert_eq!(truncate_utf8("abc", 10), "abc");
    }
}
