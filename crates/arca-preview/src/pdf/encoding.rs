//! Character encodings of simple (single-byte) PDF fonts and glyph names (ISO 32000-1
//! Annex D, Adobe Glyph List conventions).

use super::agl::NAMES;

/// A predefined base encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BaseEncoding {
    /// `WinAnsiEncoding` (Windows-1252).
    WinAnsi,
    /// `MacRomanEncoding`.
    MacRoman,
    /// `StandardEncoding` (Type 1 default).
    Standard,
    /// The built-in encoding of the Symbol font.
    Symbol,
    /// The built-in encoding of ZapfDingbats: every glyph is shown as a bullet.
    Dingbats,
}

impl BaseEncoding {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "WinAnsiEncoding" => Some(Self::WinAnsi),
            "MacRomanEncoding" => Some(Self::MacRoman),
            "StandardEncoding" => Some(Self::Standard),
            _ => None,
        }
    }
}

/// MacRoman 0x80..=0xFF as the PDF specification defines it.
const MAC_ROMAN_HIGH: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{A0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄¤‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{F8FF}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";

/// StandardEncoding differences from ASCII/Latin-1 in 0xA1..=0xFB.
const STANDARD_HIGH: &[(u8, char)] = &[
    (0xA1, '¡'),
    (0xA2, '¢'),
    (0xA3, '£'),
    (0xA4, '⁄'),
    (0xA5, '¥'),
    (0xA6, 'ƒ'),
    (0xA7, '§'),
    (0xA8, '¤'),
    (0xA9, '\''),
    (0xAA, '“'),
    (0xAB, '«'),
    (0xAC, '‹'),
    (0xAD, '›'),
    (0xAE, 'ﬁ'),
    (0xAF, 'ﬂ'),
    (0xB1, '–'),
    (0xB2, '†'),
    (0xB3, '‡'),
    (0xB4, '·'),
    (0xB6, '¶'),
    (0xB7, '•'),
    (0xB8, '‚'),
    (0xB9, '„'),
    (0xBA, '”'),
    (0xBB, '»'),
    (0xBC, '…'),
    (0xBD, '‰'),
    (0xBF, '¿'),
    (0xC1, '`'),
    (0xC2, '´'),
    (0xC3, 'ˆ'),
    (0xC4, '˜'),
    (0xC5, '¯'),
    (0xC6, '˘'),
    (0xC7, '˙'),
    (0xC8, '¨'),
    (0xCA, '˚'),
    (0xCB, '¸'),
    (0xCD, '˝'),
    (0xCE, '˛'),
    (0xCF, 'ˇ'),
    (0xD0, '—'),
    (0xE1, 'Æ'),
    (0xE3, 'ª'),
    (0xE8, 'Ł'),
    (0xE9, 'Ø'),
    (0xEA, 'Œ'),
    (0xEB, 'º'),
    (0xF1, 'æ'),
    (0xF5, 'ı'),
    (0xF8, 'ł'),
    (0xF9, 'ø'),
    (0xFA, 'œ'),
    (0xFB, 'ß'),
];

/// Greek letters of the Symbol font for 0x41..=0x5A and 0x61..=0x7A.
const SYMBOL_UPPER: &str = "ΑΒΧΔΕΦΓΗΙϑΚΛΜΝΟΠΘΡΣΤΥςΩΞΨΖ";
const SYMBOL_LOWER: &str = "αβχδεφγηιϕκλμνοπθρστυϖωξψζ";

const SYMBOL_HIGH: &[(u8, char)] = &[
    (0xA3, '≤'),
    (0xA5, '∞'),
    (0xAB, '↔'),
    (0xAC, '←'),
    (0xAD, '↑'),
    (0xAE, '→'),
    (0xAF, '↓'),
    (0xB0, '°'),
    (0xB1, '±'),
    (0xB3, '≥'),
    (0xB4, '×'),
    (0xB5, '∝'),
    (0xB6, '∂'),
    (0xB7, '•'),
    (0xB8, '÷'),
    (0xB9, '≠'),
    (0xBA, '≡'),
    (0xBB, '≈'),
    (0xBC, '…'),
    (0xC5, '⊕'),
    (0xC7, '∩'),
    (0xC8, '∪'),
    (0xCE, '∈'),
    (0xD1, '∇'),
    (0xD5, '∏'),
    (0xD6, '√'),
    (0xD7, '⋅'),
    (0xD8, '¬'),
    (0xD9, '∧'),
    (0xDA, '∨'),
    (0xE5, '∑'),
    (0xF2, '∫'),
];

/// The character a base encoding assigns to `code`.
pub(super) fn base_char(enc: BaseEncoding, code: u8) -> Option<char> {
    match enc {
        BaseEncoding::WinAnsi => match code {
            0x00..=0x1F => None,
            0x7F | 0x81 | 0x8D | 0x8F | 0x90 | 0x9D => Some('•'),
            0xA0 => Some(' '),
            0xAD => Some('-'),
            0x80..=0x9F => arca_container::text::decode_codepage(
                &[code],
                arca_container::text::CodePage::Windows1252,
            )
            .chars()
            .next(),
            _ => Some(char::from(code)),
        },
        BaseEncoding::MacRoman => match code {
            0x00..=0x1F | 0x7F => None,
            0x20..=0x7E => Some(char::from(code)),
            _ => MAC_ROMAN_HIGH.chars().nth(usize::from(code - 0x80)).filter(|c| *c != '\u{F8FF}'),
        },
        BaseEncoding::Standard => match code {
            0x27 => Some('’'),
            0x60 => Some('‘'),
            0x20..=0x7E => Some(char::from(code)),
            _ => STANDARD_HIGH.iter().find(|(c, _)| *c == code).map(|(_, ch)| *ch),
        },
        BaseEncoding::Symbol => match code {
            0x20 => Some(' '),
            0x41..=0x5A => SYMBOL_UPPER.chars().nth(usize::from(code - 0x41)),
            0x61..=0x7A => SYMBOL_LOWER.chars().nth(usize::from(code - 0x61)),
            0x2D => Some('−'),
            0x30..=0x39 | 0x21 | 0x23 | 0x25 | 0x26 | 0x28..=0x2F | 0x3A..=0x3F => {
                Some(char::from(code))
            }
            _ => SYMBOL_HIGH.iter().find(|(c, _)| *c == code).map(|(_, ch)| *ch),
        },
        BaseEncoding::Dingbats => (code > 0x20).then_some('•'),
    }
}

fn lookup_name(name: &str) -> Option<u32> {
    NAMES.binary_search_by(|(n, _)| (*n).cmp(name)).ok().and_then(|i| NAMES.get(i)).map(|(_, c)| *c)
}

fn hex_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// Maps a glyph name to text following the Adobe Glyph List specification: suffixes after
/// a period are dropped, underscores join components, `uniXXXX` and `uXXXX[XX]` are
/// Unicode values. Names that denote glyph indices (`g12`, `cid5`) have no text.
pub(super) fn glyph_to_string(name: &str) -> Option<String> {
    let base = name.split('.').next().unwrap_or("");
    if base.is_empty() {
        return None;
    }
    let mut out = String::new();
    for part in base.split('_') {
        out.push_str(&component(part)?);
    }
    (!out.is_empty()).then_some(out)
}

fn component(part: &str) -> Option<String> {
    if let Some(cp) = lookup_name(part) {
        return char::from_u32(cp).map(String::from);
    }
    if let Some(hex) = part.strip_prefix("uni")
        && hex.len() >= 4
        && hex.len() % 4 == 0
        && hex.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    {
        let mut s = String::new();
        for chunk in hex.as_bytes().chunks(4) {
            let v = hex_u32(std::str::from_utf8(chunk).ok()?)?;
            s.push(char::from_u32(v)?);
        }
        return Some(s);
    }
    if let Some(hex) = part.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    {
        return char::from_u32(hex_u32(hex)?).map(String::from);
    }
    // Ligature names that are not in the table's single-code-point form.
    match part {
        "f_f" | "ff" => Some("ff".into()),
        "f_i" | "fi" => Some("fi".into()),
        "f_l" | "fl" => Some("fl".into()),
        "f_f_i" | "ffi" => Some("ffi".into()),
        "f_f_l" | "ffl" => Some("ffl".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        for w in NAMES.windows(2) {
            if let [a, b] = w {
                assert!(a.0 < b.0, "{} !< {}", a.0, b.0);
            }
        }
    }

    #[test]
    fn common_names() {
        let g = |n| glyph_to_string(n);
        assert_eq!(g("A").as_deref(), Some("A"));
        assert_eq!(g("space").as_deref(), Some(" "));
        assert_eq!(g("eacute").as_deref(), Some("é"));
        assert_eq!(g("Ccedilla").as_deref(), Some("Ç"));
        assert_eq!(g("Scaron").as_deref(), Some("Š"));
        assert_eq!(g("germandbls").as_deref(), Some("ß"));
        assert_eq!(g("quoteright").as_deref(), Some("’"));
        assert_eq!(g("emdash").as_deref(), Some("—"));
        assert_eq!(g("Gamma").as_deref(), Some("Γ"));
        assert_eq!(g("alpha").as_deref(), Some("α"));
        assert_eq!(g("sigma1").as_deref(), Some("ς"));
        assert_eq!(g("afii10017").as_deref(), Some("А"));
        assert_eq!(g("afii10023").as_deref(), Some("Ё"));
        assert_eq!(g("afii10071").as_deref(), Some("ё"));
        assert_eq!(g("afii10097").as_deref(), Some("я"));
        assert_eq!(g("fi").as_deref(), Some("\u{FB01}"));
        assert_eq!(g("Euro").as_deref(), Some("€"));
    }

    #[test]
    fn cyrillic_alphabet_is_complete() {
        let upper: String =
            (10017..=10049).filter_map(|n| glyph_to_string(&format!("afii{n}"))).collect();
        assert_eq!(upper, "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ");
        let lower: String =
            (10065..=10097).filter_map(|n| glyph_to_string(&format!("afii{n}"))).collect();
        assert_eq!(lower, "абвгдеёжзийклмнопрстуфхцчшщъыьэюя");
    }

    #[test]
    fn uni_and_composite_names() {
        let g = |n| glyph_to_string(n);
        assert_eq!(g("uni0416").as_deref(), Some("Ж"));
        assert_eq!(g("uni00410042").as_deref(), Some("AB"));
        assert_eq!(g("u1F600").as_deref(), Some("😀"));
        assert_eq!(g("a.sc").as_deref(), Some("a"));
        assert_eq!(g("f_f_i").as_deref(), Some("ffi"));
        assert_eq!(g("T_h").as_deref(), Some("Th"));
        assert_eq!(g("uni00zz"), None);
        assert_eq!(g("uniD800"), None);
        assert_eq!(g("g12"), None);
        assert_eq!(g("cid1234"), None);
        assert_eq!(g(".notdef"), None);
        assert_eq!(g(""), None);
    }

    #[test]
    fn base_encodings() {
        use BaseEncoding::*;
        assert_eq!(base_char(WinAnsi, b'A'), Some('A'));
        assert_eq!(base_char(WinAnsi, 0xE9), Some('é'));
        assert_eq!(base_char(WinAnsi, 0x80), Some('€'));
        assert_eq!(base_char(WinAnsi, 0x93), Some('“'));
        assert_eq!(base_char(WinAnsi, 0xA0), Some(' '));
        assert_eq!(base_char(WinAnsi, 0x05), None);
        assert_eq!(base_char(MacRoman, 0x8E), Some('é'));
        assert_eq!(base_char(MacRoman, 0xD2), Some('“'));
        assert_eq!(base_char(MacRoman, 0xDB), Some('¤'));
        assert_eq!(base_char(MacRoman, 0xF0), None);
        assert_eq!(base_char(Standard, 0x27), Some('’'));
        assert_eq!(base_char(Standard, 0xAE), Some('ﬁ'));
        assert_eq!(base_char(Standard, 0xFB), Some('ß'));
        assert_eq!(base_char(Symbol, b'a'), Some('α'));
        assert_eq!(base_char(Symbol, b'W'), Some('Ω'));
        assert_eq!(base_char(Symbol, 0xE5), Some('∑'));
        assert_eq!(base_char(Dingbats, b'l'), Some('•'));
        assert_eq!(MAC_ROMAN_HIGH.chars().count(), 128);
        assert_eq!(SYMBOL_UPPER.chars().count(), 26);
        assert_eq!(SYMBOL_LOWER.chars().count(), 26);
    }
}
