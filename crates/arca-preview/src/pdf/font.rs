//! Fonts: turning the bytes of a shown string into text and into an advance width.
//!
//! No font program is ever parsed — embedded fonts are a classic exploit vector and carry
//! no information the text needs. Text comes from the `ToUnicode` CMap when there is one,
//! otherwise from the font's encoding (base encoding + `/Differences`), and widths from
//! `/Widths` or `/W` (with built-in metrics for the three standard families as a fallback so
//! that word gaps are still recognised).

use arca_container::text::truncate_utf8;

use super::cmap::CMap;
use super::encoding::{BaseEncoding, base_char, glyph_to_string};
use super::file::PdfFile;
use super::lex::{Dict, Obj};

/// Largest `ToUnicode` / `CMap` stream decoded.
const MAX_CMAP_BYTES: usize = 8 << 20;
/// Most `/W` entries kept for a CID font.
const MAX_WIDTH_RANGES: usize = 1 << 17;
/// Most text decoded from one shown string; the rest is cut and reported.
const MAX_SHOWN_TEXT: usize = 1 << 20;

/// Helvetica advance widths (1/1000 em) for U+0020..=U+007E.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Times-Roman advance widths for U+0020..=U+007E.
const TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Simple {
        /// 256 entries: the text each code stands for.
        table: Vec<Option<String>>,
        first_char: i64,
        widths: Vec<f64>,
        missing_width: f64,
        family: Family,
        has_widths: bool,
    },
    Cid {
        /// How codes relate to text without a `ToUnicode` map.
        direct: CidDirect,
        /// Code-space ranges of an embedded encoding CMap.
        encoding: Option<CMap>,
        default_width: f64,
        /// Sorted `(first, last, width)`.
        widths: Vec<(u32, u32, f64)>,
    },
}

/// What a CID code means when the font has no `ToUnicode` CMap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CidDirect {
    /// The code is the Unicode value (UCS-2 CMaps, `Adobe-UCS` ordering).
    Unicode,
    /// Nothing is known.
    Unknown,
}

/// A font as the content interpreter needs it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Font {
    kind: Kind,
    to_unicode: Option<CMap>,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) mono: bool,
    pub(super) serif: bool,
}

/// The result of showing one string.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Shown {
    pub(super) text: String,
    /// Sum of the glyph widths in thousandths of an em.
    pub(super) advance: f64,
    /// Number of glyphs (character codes).
    pub(super) glyphs: usize,
    /// Number of single-byte space codes (word spacing applies to these).
    pub(super) spaces: usize,
    /// Codes that could not be turned into text.
    pub(super) unmapped: usize,
    /// The text was cut at [`MAX_SHOWN_TEXT`].
    pub(super) clipped: bool,
    /// Codes whose `ToUnicode` text differs from what the font's own encoding says the glyph is.
    pub(super) mismatched: usize,
}

fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase().contains(needle)
}

impl Font {
    /// A font used when a resource is missing: Latin text, standard widths.
    pub(super) fn fallback() -> Self {
        Self {
            kind: Kind::Simple {
                table: (0..=255u8)
                    .map(|c| base_char(BaseEncoding::WinAnsi, c).map(String::from))
                    .collect(),
                first_char: 0,
                widths: Vec::new(),
                missing_width: 500.0,
                family: Family::Sans,
                has_widths: false,
            },
            to_unicode: None,
            bold: false,
            italic: false,
            mono: false,
            serif: false,
        }
    }

    /// Builds a font from its dictionary.
    pub(super) fn load(pdf: &mut PdfFile<'_>, dict: &Dict) -> Self {
        let subtype = pdf.get(dict, "Subtype").as_name().unwrap_or("").to_owned();
        let base_name = pdf.get(dict, "BaseFont").as_name().unwrap_or("").to_owned();
        let name = strip_subset(&base_name).to_owned();
        let to_unicode = {
            let tu = pdf.get(dict, "ToUnicode");
            tu.as_stream().and_then(|s| {
                let s = s.clone();
                pdf.stream_data(&s, MAX_CMAP_BYTES).ok().map(|d| CMap::parse(&d.data))
            })
        };
        let (descriptor_src, kind) = if subtype == "Type0" {
            let desc = pdf.get_array(dict, "DescendantFonts");
            let cid = desc.first().and_then(Obj::as_dict).cloned().unwrap_or_default();
            let kind = load_cid(pdf, dict, &cid);
            (cid, kind)
        } else {
            (dict.clone(), load_simple(pdf, dict, &subtype, &name))
        };
        let descriptor = pdf.get(&descriptor_src, "FontDescriptor");
        let (mut bold, mut italic) = (
            ["bold", "black", "heavy", "semibold", "demi"].iter().any(|k| contains_ci(&name, k)),
            ["italic", "oblique", "slanted"].iter().any(|k| contains_ci(&name, k)),
        );
        if let Some(d) = descriptor.as_dict() {
            let flags = pdf.get(d, "Flags").as_int().unwrap_or(0);
            italic |= flags & 64 != 0;
            bold |= flags & (1 << 18) != 0;
            bold |= pdf.get(d, "FontWeight").as_int().unwrap_or(400) >= 700;
            italic |= pdf.get(d, "ItalicAngle").as_f64().is_some_and(|a| a != 0.0);
        }
        let mono = family_of(&name) == Family::Mono;
        let mut serif = family_of(&name) == Family::Serif;
        if let Some(d) = descriptor.as_dict() {
            // The descriptor's flags say it too (bit 2: serif).
            serif |= pdf.get(d, "Flags").as_int().unwrap_or(0) & 2 != 0 && !mono;
        }
        Self { kind, to_unicode, bold, italic, mono, serif }
    }

    /// `true` if codes are two bytes wide by default.
    #[cfg(test)]
    pub(super) const fn is_cid(&self) -> bool {
        matches!(self.kind, Kind::Cid { .. })
    }

    /// Decodes a shown string.
    pub(super) fn show(&self, bytes: &[u8]) -> Shown {
        let mut out = Shown::default();
        match &self.kind {
            Kind::Simple { table, first_char, widths, missing_width, family, has_widths } => {
                for &code in bytes {
                    if out.text.len() > MAX_SHOWN_TEXT {
                        out.clipped = true;
                        break;
                    }
                    out.glyphs += 1;
                    let from_map = self.to_unicode.as_ref().and_then(|m| m.lookup(u32::from(code)));
                    let from_table = table.get(usize::from(code)).cloned().flatten();
                    if let (Some(a), Some(b)) = (&from_map, &from_table)
                        && differs_in_meaning(a, b)
                    {
                        out.mismatched += 1;
                    }
                    let mapped = from_map.or(from_table);
                    let width = simple_width(
                        code,
                        mapped.as_deref(),
                        *first_char,
                        widths,
                        *missing_width,
                        *family,
                        *has_widths,
                    );
                    out.advance += width;
                    if code == 0x20 {
                        out.spaces += 1;
                    }
                    match mapped {
                        Some(t) => out.text.push_str(&t),
                        None if code >= 0x20 => {
                            out.unmapped += 1;
                            out.text.push(char::REPLACEMENT_CHARACTER);
                        }
                        None => {}
                    }
                }
            }
            Kind::Cid { direct, encoding, default_width, widths } => {
                let fixed = 2;
                let codes = match encoding {
                    Some(e) if e.has_codespaces() => e.codes(bytes, fixed),
                    _ => match &self.to_unicode {
                        Some(m) if m.has_codespaces() && encoding.is_none() => {
                            m.codes(bytes, fixed)
                        }
                        _ => CMap::default().codes(bytes, fixed),
                    },
                };
                for (code, _len) in codes {
                    if out.text.len() > MAX_SHOWN_TEXT {
                        out.clipped = true;
                        break;
                    }
                    out.glyphs += 1;
                    out.advance += cid_width(widths, code, *default_width);
                    let mapped =
                        self.to_unicode.as_ref().and_then(|m| m.lookup(code)).or_else(|| {
                            match direct {
                                CidDirect::Unicode => char::from_u32(code).map(String::from),
                                CidDirect::Unknown => None,
                            }
                        });
                    match mapped {
                        Some(t) => out.text.push_str(&t),
                        None => {
                            out.unmapped += 1;
                            out.text.push(char::REPLACEMENT_CHARACTER);
                        }
                    }
                }
            }
        }
        out
    }
}

/// `true` if two texts for the same code are different letters, not merely one character and
/// its expansion (a ligature, a presentation form, a no-break space).
fn differs_in_meaning(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let single = |s: &str| {
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    match (single(a), single(b)) {
        (Some(x), Some(y)) => {
            // Compatibility pairs the encodings legitimately disagree on.
            let blank = |c: char| c.is_whitespace() || c == '\u{ad}' || c == '-';
            !(blank(x) && blank(y)) && !x.is_control() && !y.is_control()
        }
        // One side is several letters: a ligature or an expansion; not judged.
        _ => false,
    }
}

fn simple_width(
    code: u8,
    mapped: Option<&str>,
    first_char: i64,
    widths: &[f64],
    missing: f64,
    family: Family,
    has_widths: bool,
) -> f64 {
    if has_widths {
        let idx = i64::from(code) - first_char;
        if let Some(w) = usize::try_from(idx).ok().and_then(|i| widths.get(i)) {
            return *w;
        }
        return missing;
    }
    let ch = mapped.and_then(|t| t.chars().next());
    match (family, ch) {
        (Family::Mono, _) => 600.0,
        (Family::Sans, Some(c @ ' '..='~')) => lookup95(&HELVETICA, c),
        (Family::Serif, Some(c @ ' '..='~')) => lookup95(&TIMES, c),
        (Family::Sans, _) => 556.0,
        (Family::Serif, _) => 500.0,
    }
}

fn lookup95(table: &[u16; 95], c: char) -> f64 {
    let idx = usize::try_from(u32::from(c).saturating_sub(0x20)).unwrap_or(0);
    f64::from(table.get(idx).copied().unwrap_or(500))
}

fn cid_width(widths: &[(u32, u32, f64)], code: u32, default: f64) -> f64 {
    let upto = widths.partition_point(|r| r.0 <= code);
    match upto.checked_sub(1).and_then(|i| widths.get(i)) {
        Some(&(_, last, w)) if code <= last => w,
        _ => default,
    }
}

fn family_of(name: &str) -> Family {
    if ["courier", "mono", "consolas", "typewriter", "lucidaconsole", "cmtt"]
        .iter()
        .any(|k| contains_ci(name, k))
    {
        Family::Mono
    } else if [
        "times", "serif", "georgia", "garamond", "palatino", "bookman", "cmr", "minion", "cambria",
    ]
    .iter()
    .any(|k| contains_ci(name, k))
        && !contains_ci(name, "sans")
    {
        Family::Serif
    } else {
        Family::Sans
    }
}

fn load_simple(pdf: &mut PdfFile<'_>, dict: &Dict, subtype: &str, name: &str) -> Kind {
    let descriptor = pdf.get(dict, "FontDescriptor");
    let flags =
        descriptor.as_dict().map(|d| pdf.get(d, "Flags").as_int().unwrap_or(0)).unwrap_or(0);
    let symbolic = flags & 4 != 0 && flags & 32 == 0;
    let enc = pdf.get(dict, "Encoding");
    let mut base: Option<BaseEncoding> = match &enc {
        Obj::Name(n) => BaseEncoding::from_name(n),
        Obj::Dict(d) => pdf.get(d, "BaseEncoding").as_name().and_then(BaseEncoding::from_name),
        _ => None,
    };
    if base.is_none() {
        base = Some(if contains_ci(name, "zapfdingbats") || contains_ci(name, "dingbats") {
            BaseEncoding::Dingbats
        } else if name == "Symbol" || contains_ci(name, "symbol") && !contains_ci(name, "symbolmt-")
        {
            BaseEncoding::Symbol
        } else if subtype == "TrueType" || (symbolic && subtype != "Type1") {
            BaseEncoding::WinAnsi
        } else {
            BaseEncoding::Standard
        });
    }
    let base = base.unwrap_or(BaseEncoding::Standard);
    let mut table: Vec<Option<String>> =
        (0..=255u8).map(|c| base_char(base, c).map(String::from)).collect();
    if let Obj::Dict(d) = &enc {
        let diffs = pdf.get_array(d, "Differences");
        let mut code: i64 = 0;
        for item in &diffs {
            match item {
                Obj::Int(n) => code = *n,
                Obj::Real(r) => code = super::lex::to_i64(*r),
                Obj::Name(n) => {
                    if let Some(slot) = usize::try_from(code).ok().and_then(|c| table.get_mut(c)) {
                        *slot = glyph_to_string(n);
                    }
                    code = code.saturating_add(1);
                }
                _ => {}
            }
        }
    }
    let first_char = pdf.get(dict, "FirstChar").as_int().unwrap_or(0);
    let scale = if subtype == "Type3" {
        let m = pdf.get_numbers(dict, "FontMatrix");
        m.first().copied().filter(|v| *v != 0.0).map_or(1.0, |v| v * 1000.0)
    } else {
        1.0
    };
    let widths: Vec<f64> = pdf
        .get_array(dict, "Widths")
        .iter()
        .take(256)
        .map(|o| o.as_f64().unwrap_or(0.0) * scale)
        .collect();
    let missing_width = descriptor
        .as_dict()
        .and_then(|d| pdf.get(d, "MissingWidth").as_f64())
        .unwrap_or(if widths.is_empty() { 500.0 } else { 0.0 });
    Kind::Simple {
        table,
        first_char,
        has_widths: !widths.is_empty(),
        widths,
        missing_width,
        family: family_of(name),
    }
}

fn load_cid(pdf: &mut PdfFile<'_>, font: &Dict, cid: &Dict) -> Kind {
    let default_width = pdf.get(cid, "DW").as_f64().unwrap_or(1000.0);
    let mut widths: Vec<(u32, u32, f64)> = Vec::new();
    let w = pdf.get_array(cid, "W");
    let mut i = 0usize;
    while let Some(first) = w.get(i).and_then(Obj::as_int) {
        let Ok(first) = u32::try_from(first) else { break };
        match w.get(i + 1) {
            Some(Obj::Arr(list)) => {
                for (k, v) in list.iter().enumerate() {
                    let v = pdf.resolve(v).as_f64().unwrap_or(default_width);
                    let c = first.saturating_add(u32::try_from(k).unwrap_or(0));
                    widths.push((c, c, v));
                    if widths.len() > MAX_WIDTH_RANGES {
                        break;
                    }
                }
                i += 2;
            }
            Some(o) => {
                let Some(last) = o.as_int().and_then(|v| u32::try_from(v).ok()) else { break };
                let Some(v) = w.get(i + 2).and_then(Obj::as_f64) else { break };
                widths.push((first, last.max(first), v));
                i += 3;
            }
            None => break,
        }
        if widths.len() > MAX_WIDTH_RANGES {
            break;
        }
    }
    widths.sort_by_key(|r| r.0);
    let enc = pdf.get(font, "Encoding");
    let mut direct = CidDirect::Unknown;
    let mut encoding = None;
    match &enc {
        Obj::Name(n) => {
            if n.contains("UCS2") || n.contains("UTF16") {
                direct = CidDirect::Unicode;
            }
        }
        Obj::Stream(s) => {
            let s = s.clone();
            if let Ok(d) = pdf.stream_data(&s, MAX_CMAP_BYTES) {
                encoding = Some(CMap::parse(&d.data));
            }
        }
        _ => {}
    }
    let info = pdf.get(cid, "CIDSystemInfo");
    if let Some(d) = info.as_dict()
        && let Some(o) = pdf.get(d, "Ordering").as_str()
        && truncate_utf8(&String::from_utf8_lossy(o), 16) == "UCS"
    {
        direct = CidDirect::Unicode;
    }
    Kind::Cid { direct, encoding, default_width, widths }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewPolicy;
    use crate::pdf::testpdf::PdfBuilder;

    fn load(extra: &[(u32, &str)]) -> Font {
        let mut b = PdfBuilder::simple_text("x");
        for (n, body) in extra {
            b.add_raw(*n, body);
        }
        let data = b.build();
        let mut pdf = PdfFile::open(&data, &PreviewPolicy::default()).expect("open");
        let d = pdf.object(10);
        let dict = d.as_dict().cloned().expect("font dict");
        Font::load(&mut pdf, &dict)
    }

    fn load_with_streams(extra: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Font {
        let mut b = PdfBuilder::simple_text("x");
        for (n, body) in extra {
            b.add_raw(*n, body);
        }
        for (n, dict, data) in streams {
            b.add_stream(*n, dict, data);
        }
        let data = b.build();
        let mut pdf = PdfFile::open(&data, &PreviewPolicy::default()).expect("open");
        let d = pdf.object(10);
        let dict = d.as_dict().cloned().expect("font dict");
        Font::load(&mut pdf, &dict)
    }

    #[test]
    fn winansi_helvetica_with_standard_metrics() {
        let f = load(&[(
            10,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>",
        )]);
        let s = f.show(b"Hi \xE9");
        assert_eq!(s.text, "Hi é");
        assert_eq!((s.glyphs, s.spaces, s.unmapped), (4, 1, 0));
        assert!((s.advance - (722.0 + 222.0 + 278.0 + 556.0)).abs() < 1e-6, "{}", s.advance);
        assert!(f.bold && !f.italic);
    }

    #[test]
    fn differences_and_widths() {
        let f = load(&[(
            10,
            "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+CMR10 /FirstChar 65 /Widths [700 710] /Encoding << /Type /Encoding /Differences [65 /Gamma /fi 200 /afii10017] >> >>",
        )]);
        let s = f.show(&[65, 66, 200, 67]);
        assert_eq!(s.text, "Γ\u{FB01}АC");
        assert_eq!(s.unmapped, 0);
        // Codes outside /Widths take the (absent) MissingWidth, i.e. 0.
        assert!((s.advance - 1410.0).abs() < 1e-6, "{}", s.advance);
    }

    #[test]
    fn to_unicode_wins_over_encoding() {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfrange <41> <43> <0410> endbfrange";
        let f = load_with_streams(
            &[(
                10,
                "<< /Type /Font /Subtype /TrueType /BaseFont /Arial /Encoding /WinAnsiEncoding /ToUnicode 11 0 R >>",
            )],
            &[(11, "", cmap)],
        );
        assert_eq!(f.show(b"ABCD").text, "АБВD");
    }

    #[test]
    fn type0_identity_with_to_unicode_and_w_array() {
        let cmap = b"1 begincodespacerange <0000> <FFFF> endcodespacerange 2 beginbfchar <0003> <0020> <0024> <041F> endbfchar";
        let f = load_with_streams(
            &[
                (
                    10,
                    "<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+Arial-BoldMT /Encoding /Identity-H /DescendantFonts [12 0 R] /ToUnicode 11 0 R >>",
                ),
                (
                    12,
                    "<< /Type /Font /Subtype /CIDFontType2 /DW 800 /W [3 [250] 36 40 600] /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
                ),
            ],
            &[(11, "", cmap)],
        );
        assert!(f.is_cid() && f.bold);
        let s = f.show(&[0, 0x24, 0, 3, 0, 0x25, 0]);
        // 0x25 and the odd tail byte have no mapping and show as U+FFFD.
        assert_eq!(s.text, "П \u{FFFD}\u{FFFD}");
        assert_eq!((s.glyphs, s.unmapped, s.spaces), (4, 2, 0));
        assert!((s.advance - (600.0 + 250.0 + 600.0 + 800.0)).abs() < 1e-6, "{}", s.advance);
    }

    #[test]
    fn ucs2_cmap_names_and_ucs_ordering_are_direct() {
        let f = load(&[
            (
                10,
                "<< /Type /Font /Subtype /Type0 /BaseFont /MSung /Encoding /UniCNS-UCS2-H /DescendantFonts [12 0 R] >>",
            ),
            (
                12,
                "<< /Type /Font /Subtype /CIDFontType0 /CIDSystemInfo << /Registry (Adobe) /Ordering (CNS1) /Supplement 0 >> >>",
            ),
        ]);
        assert_eq!(f.show(&[0x4E, 0x2D, 0x65, 0x87]).text, "中文");
        let g = load(&[
            (
                10,
                "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [12 0 R] >>",
            ),
            (
                12,
                "<< /Type /Font /Subtype /CIDFontType2 /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> >>",
            ),
        ]);
        assert_eq!(g.show(&[0x04, 0x10]).text, "А");
        let h = load(&[
            (
                10,
                "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [12 0 R] >>",
            ),
            (
                12,
                "<< /Type /Font /Subtype /CIDFontType2 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
            ),
        ]);
        let s = h.show(&[0x04, 0x10]);
        assert_eq!((s.text.as_str(), s.unmapped), ("\u{FFFD}", 1));
    }

    #[test]
    fn symbol_and_dingbats() {
        let s = load(&[(10, "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>")]);
        assert_eq!(s.show(b"abc").text, "αβχ");
        let z = load(&[(10, "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>")]);
        assert_eq!(z.show(b"l").text, "•");
    }

    #[test]
    fn style_from_descriptor_flags() {
        let f = load(&[
            (10, "<< /Type /Font /Subtype /TrueType /BaseFont /Foo /FontDescriptor 11 0 R >>"),
            (11, "<< /Type /FontDescriptor /FontName /Foo /Flags 262208 /ItalicAngle 0 >>"),
        ]);
        assert!(f.bold && f.italic);
    }

    #[test]
    fn fallback_font_and_hostile_dicts() {
        let f = Font::fallback();
        assert_eq!(f.show(b"ok").text, "ok");
        let weird = load(&[(
            10,
            "<< /Subtype /Type0 /DescendantFonts [10 0 R] /Encoding 10 0 R /ToUnicode 10 0 R /W [1 2 3 4 5 6 7 8 9] >>",
        )]);
        let _ = weird.show(b"abc");
        let big = load(&[(
            10,
            "<< /Subtype /Type1 /FirstChar -99999999999 /Widths [1 2 3] /Encoding << /Differences [99999999999 /a -5 /b 255 /c /d /e] >> >>",
        )]);
        let _ = big.show(&[0, 1, 255, 254]);
    }
}
