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
    /// Any other single-byte code page from [`crate::codepages`].
    Single(&'static crate::codepages::SingleByte),
    /// A code page this crate has no table for (multi-byte East Asian ones, mostly):
    /// decoded as ASCII with U+FFFD for bytes ≥ 0x80.
    Unsupported(u16),
}

impl CodePage {
    /// Maps a Windows code page identifier.
    #[must_use]
    pub fn from_id(id: u16) -> Self {
        match id {
            65001 => Self::Utf8,
            1200 => Self::Utf16Le,
            1251 => Self::Windows1251,
            1252 | 0 => Self::Windows1252,
            28591 => Self::Latin1,
            other => match crate::codepages::lookup(other) {
                Some(t) => Self::Single(t),
                None => Self::Unsupported(other),
            },
        }
    }

    /// `true` if bytes ≥ 0x80 can be decoded faithfully.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }

    /// The Windows / IANA identifier of this code page.
    #[must_use]
    pub const fn id(self) -> u16 {
        match self {
            Self::Utf8 => 65001,
            Self::Utf16Le => 1200,
            Self::Windows1251 => 1251,
            Self::Windows1252 => 1252,
            Self::Latin1 => 28591,
            Self::Single(t) => t.id,
            Self::Unsupported(id) => id,
        }
    }
}

/// How many leading bytes [`decode_legacy_text`] looks at when guessing.
const LEGACY_SAMPLE: usize = 64 << 10;

/// Guesses the encoding of 8-bit text that is not valid UTF-8 and decodes it.
///
/// This is for old plain-text files (DOS and early-Windows era `.txt`, `.csv`, logs).
/// The guess is deliberately conservative:
///
/// * any control byte other than tab, line feed, form feed and carriage return (a trailing
///   DOS end-of-file marker `0x1A` is tolerated) means "binary" and gives `None`;
/// * Cyrillic code pages (Windows-1251, CP866, KOI8-R) are ranked by how much the decoded
///   high bytes look like running Russian/Ukrainian text, and the winner must clearly look
///   like it;
/// * otherwise Windows-1252 is used if the bytes are mostly ASCII and none of the five
///   undefined code points occurs;
/// * anything else (East Asian double-byte text, binary data that happens to be printable)
///   gives `None`, so the caller reports the file as unidentified instead of showing noise.
///
/// The result is a *guess*: callers should tell the reader which code page was assumed.
#[must_use]
pub fn decode_legacy_text(bytes: &[u8]) -> Option<(CodePage, String)> {
    let sample = bytes.get(..bytes.len().min(LEGACY_SAMPLE)).unwrap_or(bytes);
    let mut high = 0usize;
    let mut ascii = 0usize;
    for (i, &b) in sample.iter().enumerate() {
        match b {
            b'\n' | b'\r' | b'\t' | 0x0C | 0x20..=0x7E => ascii += 1,
            0x1A if i + 1 == bytes.len() => {}
            0x80..=0xFF => high += 1,
            _ => return None,
        }
    }
    if high == 0 {
        return None;
    }
    // Real text has spaces, digits and punctuation between its words.
    if ascii * 10 < sample.len() {
        return None;
    }
    let mut best: Option<(i64, CodePage)> = None;
    for cp in [
        CodePage::Windows1251,
        crate::codepages::lookup(866).map_or(CodePage::Windows1251, CodePage::Single),
        crate::codepages::lookup(20866).map_or(CodePage::Windows1251, CodePage::Single),
    ] {
        let score = cyrillic_score(sample, cp);
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, cp));
        }
    }
    let high = i64::try_from(high).unwrap_or(i64::MAX);
    if let Some((score, cp)) = best {
        // A right guess averages about +26 per byte, a wrong one about zero or below.
        if score >= high * 8 {
            return Some((cp, decode_codepage(bytes, cp)));
        }
    }
    // Western European: no undefined Windows-1252 code points and few accented letters.
    let undefined = sample.iter().any(|b| matches!(b, 0x81 | 0x8D | 0x8F | 0x90 | 0x9D));
    if !undefined && high * 3 <= i64::try_from(sample.len()).unwrap_or(i64::MAX) {
        return Some((CodePage::Windows1252, decode_codepage(bytes, CodePage::Windows1252)));
    }
    None
}

/// Relative frequency, in tenths of a percent, of the letters of Russian (and the extra
/// Ukrainian ones) in running text.
const LETTER_FREQ: [(char, i64); 37] = [
    ('о', 110),
    ('е', 85),
    ('а', 80),
    ('и', 74),
    ('н', 67),
    ('т', 63),
    ('с', 55),
    ('р', 47),
    ('в', 45),
    ('л', 43),
    ('к', 35),
    ('п', 32),
    ('м', 30),
    ('д', 30),
    ('у', 26),
    ('я', 20),
    ('ы', 19),
    ('ь', 17),
    ('г', 17),
    ('з', 17),
    ('б', 16),
    ('ч', 15),
    ('й', 12),
    ('х', 10),
    ('ж', 9),
    ('ш', 7),
    ('ю', 6),
    ('ц', 5),
    ('щ', 4),
    ('э', 3),
    ('ф', 3),
    ('ъ', 0),
    ('ё', 0),
    ('і', 45),
    ('ї', 7),
    ('є', 6),
    ('ґ', 1),
];

/// Rates how much `sample` looks like Russian / Ukrainian text when read in `cp`: the sum,
/// over the bytes ≥ 0x80, of the letter's frequency minus 3 % (so a permuted alphabet scores
/// about zero and the right one clearly positive) — and a heavy penalty for anything else
/// (box drawing, accented Latin, symbols). Case does not matter, so all-capitals text works.
fn cyrillic_score(sample: &[u8], cp: CodePage) -> i64 {
    let mut score = 0i64;
    for &b in sample.iter().filter(|b| **b >= 0x80) {
        let c = decode_codepage(&[b], cp).chars().next().unwrap_or(char::REPLACEMENT_CHARACTER);
        let lower = c.to_lowercase().next().unwrap_or(c);
        score += match LETTER_FREQ.iter().find(|(l, _)| *l == lower) {
            Some((_, f)) => f - 30,
            None => -50,
        };
    }
    score
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
        CodePage::Single(t) => bytes
            .iter()
            .map(|&b| match b.checked_sub(0x80).and_then(|i| t.high.get(usize::from(i))) {
                Some(&cp) => bmp(cp),
                None => char::from(b),
            })
            .collect(),
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

/// Renders a string as one line of visible characters: whatever a document puts into a name,
/// a target or an instruction cannot forge a second line or hide behind a blank. Control
/// characters (line breaks and tabs included) become `\xNN`, characters that reorder text or
/// draw nothing become `\u{XXXX}`.
///
/// The result is truncated to `max_chars` characters (with an ellipsis).
#[must_use]
pub fn display_safe(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (count, c) in s.chars().enumerate() {
        if count >= max_chars {
            out.push('…');
            break;
        }
        if c.is_control() {
            out.push_str(&format!("\\x{:02X}", u32::from(c)));
        } else if is_bidi_control(c) || is_invisible(c) {
            // Trojan-Source style reordering and "blank" characters are made visible.
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

/// Characters that draw nothing (or only a blank) yet are not whitespace: format controls
/// (Unicode category Cf: soft hyphen, zero-width space and joiners, word joiner, invisible
/// operators, deprecated format characters, tags, the byte order mark), the line and
/// paragraph separators, variation selectors, and the fillers and the blank pattern that
/// pass for a space. The bidirectional controls are included.
#[must_use]
pub const fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{08E2}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
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
    fn display_safe_is_one_visible_line() {
        // A line break would let a document forge a second line of a report.
        assert_eq!(display_safe("a\nb\r\tc", 50), "a\\x0Ab\\x0D\\x09c");
        // Zero-width and tag characters, the byte order mark and the separators are shown.
        assert_eq!(
            display_safe("a\u{200B}b\u{FEFF}c\u{E0041}d\u{2028}e\u{00AD}f\u{FE0F}", 50),
            "a\\u{200B}b\\u{FEFF}c\\u{E0041}d\\u{2028}e\\u{00AD}f\\u{FE0F}"
        );
        assert_eq!(display_safe("плюс ünï", 50), "плюс ünï");
    }

    #[test]
    fn truncate_on_boundary() {
        assert_eq!(truncate_utf8("ПРИВЕТ", 3), "П");
        assert_eq!(truncate_utf8("abc", 10), "abc");
    }

    /// Encodes `text` with `table` (a Windows / DOS / KOI8 single-byte code page).
    fn encode(text: &str, cp: CodePage) -> Vec<u8> {
        (0..=255u8)
            .map(|b| (decode_codepage(&[b], cp), b))
            .filter(|(c, _)| c.chars().count() == 1)
            .fold(std::collections::HashMap::new(), |mut m, (c, b)| {
                m.entry(c.chars().next().unwrap_or(' ')).or_insert(b);
                m
            })
            .pipe(|m| text.chars().map(|c| *m.get(&c).unwrap_or(&b'?')).collect())
    }

    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    impl<T> Pipe for T {}

    const RUSSIAN: &str = "Отчёт о проделанной работе за квартал.\nВсе показатели в норме, \
        замечаний нет. Следующая проверка состоится в начале октября.\r\n";

    #[test]
    fn legacy_cyrillic_encodings_are_told_apart() {
        for (id, cp) in [
            (1251u16, CodePage::Windows1251),
            (866, CodePage::from_id(866)),
            (20866, CodePage::from_id(20866)),
        ] {
            let bytes = encode(RUSSIAN, cp);
            let (guess, text) = decode_legacy_text(&bytes).expect("text");
            assert_eq!(guess.id(), id, "wrong code page for {id}");
            assert_eq!(text, RUSSIAN, "{id}");
        }
    }

    #[test]
    fn legacy_all_caps_cyrillic_is_still_cp1251() {
        let bytes = encode("ВНИМАНИЕ! ОПАСНАЯ ЗОНА. ПРОХОД ЗАПРЕЩЁН.", CodePage::Windows1251);
        assert_eq!(decode_legacy_text(&bytes).map(|(c, _)| c.id()), Some(1251));
    }

    #[test]
    fn legacy_western_text_is_windows_1252() {
        let bytes =
            encode("Café crème brûlée costs €4 – très bon, señor Müller.\n", CodePage::Windows1252);
        let (cp, text) = decode_legacy_text(&bytes).expect("text");
        assert_eq!(cp.id(), 1252);
        assert!(text.contains("Café") && text.contains('€') && text.contains("Müller"));
    }

    #[test]
    fn legacy_rejects_binary_and_unconvincing_data() {
        // Control bytes.
        assert!(decode_legacy_text(b"abc\x00def \xE9").is_none());
        assert!(decode_legacy_text(b"abc\x07def \xE9").is_none());
        // Pure ASCII is not "legacy" (the caller handles it as UTF-8).
        assert!(decode_legacy_text(b"hello").is_none());
        // Mostly high bytes without ASCII structure (double-byte East Asian text, noise).
        assert!(decode_legacy_text(&[0xB0, 0xA1, 0xB0, 0xA2, 0xB1, 0xA3, 0xC4, 0xE3]).is_none());
        // Undefined Windows-1252 code points.
        assert!(decode_legacy_text(b"(c) \xA9 2020 \xAE \xB0 \x81").is_none());
        // A DOS end-of-file marker at the very end is fine, in the middle it is not.
        assert!(decode_legacy_text(b"caf\xE9 au lait \x1A").is_some());
        assert!(decode_legacy_text(b"caf\xE9 \x1A au lait").is_none());
    }
}
