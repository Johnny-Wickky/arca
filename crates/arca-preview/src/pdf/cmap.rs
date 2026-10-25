//! `ToUnicode` CMaps and code-space ranges (Adobe Technical Note #5411).
//!
//! Only the parts needed to turn character codes into text are read: `bfchar`, `bfrange`
//! and `codespacerange`. The CMap is parsed with the ordinary PDF tokenizer — it is a
//! PostScript-like text that is *never executed*.

use std::collections::HashMap;

use super::encoding::glyph_to_string;
use super::lex::{Lexer, Tok};

/// Most mappings kept per CMap.
const MAX_ENTRIES: usize = 1 << 19;
/// Longest text one code may map to, in UTF-16 units. Real maps send a code to a letter or to a
/// ligature's few letters; a map of half a million codes to megabyte strings would multiply
/// into every string shown. A longer destination is not a mapping at all.
const MAX_TARGET_UNITS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// `<lo> <hi> <base>`: the code `lo + k` maps to `base` with its last UTF-16 unit
    /// increased by `k`.
    Base(Vec<u16>),
    /// `<lo> <hi> [<a> <b> ...]`.
    List(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Range {
    lo: u32,
    hi: u32,
    target: Target,
}

/// A parsed CMap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CMap {
    singles: HashMap<u32, String>,
    ranges: Vec<Range>,
    /// `(lo, hi, byte length)`.
    codespaces: Vec<(u32, u32, usize)>,
}

fn code_of(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    Some(bytes.iter().fold(0u32, |acc, &b| acc << 8 | u32::from(b)))
}

/// Text of a destination string: UTF-16BE, or a single byte as Latin-1.
fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    if bytes.len() == 1 {
        return bytes.iter().map(|&b| u16::from(b)).collect();
    }
    bytes.chunks(2).map(|c| c.iter().fold(0u16, |acc, &b| acc << 8 | u16::from(b))).collect()
}

fn units_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The text of a destination string; empty (no mapping) when it is longer than
/// [`MAX_TARGET_UNITS`].
fn target_text(bytes: &[u8]) -> String {
    if bytes.len() > MAX_TARGET_UNITS * 2 {
        return String::new();
    }
    units_to_string(&utf16_units(bytes))
}

impl CMap {
    /// Parses a CMap program.
    pub(super) fn parse(data: &[u8]) -> Self {
        let mut map = Self::default();
        let mut lx = Lexer::new(data, 0);
        let mut entries = 0usize;
        loop {
            let tok = lx.next_tok();
            match tok {
                Tok::Eof => break,
                Tok::Kw(b"begincodespacerange") => {
                    while let (Tok::Str(lo), Tok::Str(hi)) = (lx.next_tok(), lx.next_tok()) {
                        if let (Some(l), Some(h)) = (code_of(&lo), code_of(&hi))
                            && map.codespaces.len() < 256
                        {
                            map.codespaces.push((l, h, lo.len()));
                        }
                    }
                }
                Tok::Kw(b"beginbfchar") => {
                    while let Tok::Str(src) = lx.next_tok() {
                        let dst = match lx.next_tok() {
                            Tok::Str(d) => target_text(&d),
                            Tok::Name(n) => glyph_to_string(&n).unwrap_or_default(),
                            _ => break,
                        };
                        entries += 1;
                        if entries > MAX_ENTRIES {
                            return map;
                        }
                        if let Some(code) = code_of(&src)
                            && !dst.is_empty()
                        {
                            map.singles.insert(code, dst);
                        }
                    }
                }
                Tok::Kw(b"beginbfrange") => {
                    while let Tok::Str(lo) = lx.next_tok() {
                        let Tok::Str(hi) = lx.next_tok() else { break };
                        let target = match lx.next_tok() {
                            Tok::Str(d) if d.len() <= MAX_TARGET_UNITS * 2 => {
                                Target::Base(utf16_units(&d))
                            }
                            // Too long to be a mapping: the range maps nothing.
                            Tok::Str(_) => Target::List(Vec::new()),
                            Tok::ArrOpen => {
                                let mut list = Vec::new();
                                loop {
                                    match lx.next_tok() {
                                        Tok::Str(d) => list.push(target_text(&d)),
                                        Tok::Name(n) => {
                                            list.push(glyph_to_string(&n).unwrap_or_default())
                                        }
                                        Tok::ArrClose | Tok::Eof => break,
                                        _ => {}
                                    }
                                    if list.len() > MAX_ENTRIES {
                                        return map;
                                    }
                                }
                                Target::List(list)
                            }
                            _ => break,
                        };
                        entries += 1;
                        if entries > MAX_ENTRIES {
                            return map;
                        }
                        if let (Some(l), Some(h)) = (code_of(&lo), code_of(&hi))
                            && l <= h
                        {
                            map.ranges.push(Range { lo: l, hi: h, target });
                        }
                    }
                }
                _ => {}
            }
        }
        map.ranges.sort_by_key(|r| r.lo);
        map
    }

    #[cfg(test)]
    pub(super) fn has_mappings(&self) -> bool {
        !self.singles.is_empty() || !self.ranges.is_empty()
    }

    pub(super) fn has_codespaces(&self) -> bool {
        !self.codespaces.is_empty()
    }

    /// Text for `code`, if mapped.
    pub(super) fn lookup(&self, code: u32) -> Option<String> {
        if let Some(s) = self.singles.get(&code) {
            return Some(s.clone());
        }
        // Ranges are sorted by start; those that contain `code` start at or before it.
        let upto = self.ranges.partition_point(|r| r.lo <= code);
        for r in self.ranges.get(..upto)?.iter().rev().take(8) {
            if code > r.hi {
                continue;
            }
            let k = code - r.lo;
            match &r.target {
                Target::Base(units) => {
                    let mut u = units.clone();
                    let last = u.last_mut()?;
                    *last = last.wrapping_add(u16::try_from(k & 0xFFFF).unwrap_or(0));
                    return Some(units_to_string(&u));
                }
                Target::List(list) => {
                    return list.get(usize::try_from(k).ok()?).filter(|s| !s.is_empty()).cloned();
                }
            }
        }
        None
    }

    /// Splits `bytes` into character codes `(code, length)`.
    ///
    /// With code-space ranges, a code is the shortest prefix inside any range; bytes that
    /// match nothing are consumed `fallback` at a time (the shortest range length when
    /// there are ranges). Without them every code is `fallback` bytes long.
    pub(super) fn codes(&self, bytes: &[u8], fallback: usize) -> Vec<(u32, usize)> {
        let mut out = Vec::with_capacity(bytes.len() / fallback.max(1));
        let shortest = self.codespaces.iter().map(|c| c.2).min().unwrap_or(fallback).max(1);
        let mut i = 0usize;
        while i < bytes.len() {
            let mut found = None;
            if !self.codespaces.is_empty() {
                for n in 1..=4usize {
                    let Some(chunk) = bytes.get(i..i + n) else { break };
                    let Some(code) = code_of(chunk) else { break };
                    if self
                        .codespaces
                        .iter()
                        .any(|&(lo, hi, len)| len == n && (lo..=hi).contains(&code))
                    {
                        found = Some((code, n));
                        break;
                    }
                }
            }
            let (code, n) = match found {
                Some(f) => f,
                None => {
                    let n = if self.codespaces.is_empty() { fallback } else { shortest }.max(1);
                    let chunk = bytes.get(i..).unwrap_or(&[]);
                    let chunk = chunk.get(..n.min(chunk.len())).unwrap_or(&[]);
                    (code_of(chunk).unwrap_or(0), chunk.len().max(1))
                }
            };
            out.push((code, n));
            i += n;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"/CIDInit /ProcSet findresource begin 12 dict begin begincmap
/CMapName /Adobe-Identity-UCS def
1 begincodespacerange <0000> <FFFF> endcodespacerange
3 beginbfchar <0003> <0020> <0024> <0041> <0025> <00660069> endbfchar
2 beginbfrange <0030> <0039> <0030> <0040> <0042> [<0410> <0411> <0412>] endbfrange
endcmap CMapName currentdict /CMap defineresource pop end end";

    #[test]
    fn parses_chars_ranges_and_codespaces() {
        let m = CMap::parse(SAMPLE.as_bytes());
        assert!(m.has_mappings() && m.has_codespaces());
        assert_eq!(m.lookup(3).as_deref(), Some(" "));
        assert_eq!(m.lookup(0x24).as_deref(), Some("A"));
        assert_eq!(m.lookup(0x25).as_deref(), Some("fi"));
        assert_eq!(m.lookup(0x35).as_deref(), Some("5"));
        assert_eq!(m.lookup(0x41).as_deref(), Some("Б"));
        assert_eq!(m.lookup(0x43), None);
        assert_eq!(m.lookup(0x2F), None);
    }

    #[test]
    fn splits_codes_by_codespace() {
        let m = CMap::parse(SAMPLE.as_bytes());
        assert_eq!(m.codes(&[0, 3, 0, 0x24, 7], 2), vec![(3, 2), (0x24, 2), (7, 1)]);
        let mixed = CMap::parse(
            b"2 begincodespacerange <00> <7F> <8140> <9FFC> endcodespacerange".as_slice(),
        );
        assert_eq!(
            mixed.codes(&[0x41, 0x81, 0x40, 0x42], 2),
            vec![(0x41, 1), (0x8140, 2), (0x42, 1)]
        );
        let none = CMap::default();
        assert_eq!(none.codes(&[1, 2, 3], 1), vec![(1, 1), (2, 1), (3, 1)]);
        assert_eq!(none.codes(&[1, 2, 3], 2), vec![(0x102, 2), (3, 1)]);
    }

    #[test]
    fn surrogates_and_overlong_destinations() {
        let m = CMap::parse(
            b"1 beginbfchar <0001> <D83DDE00> endbfchar 1 beginbfchar <0002> <D83D> endbfchar"
                .as_slice(),
        );
        assert_eq!(m.lookup(1).as_deref(), Some("😀"));
        assert_eq!(m.lookup(2).as_deref(), Some("\u{FFFD}"));
    }

    #[test]
    fn ranges_may_not_wrap_or_invert() {
        let m = CMap::parse(b"1 beginbfrange <0010> <0005> <0041> endbfrange 1 beginbfrange <0000> <FFFF> <0041> endbfrange".as_slice());
        assert_eq!(m.lookup(0x10).as_deref(), Some("Q"));
    }

    #[test]
    fn garbage_never_panics() {
        let mut x = 5u32;
        for len in 0..300 {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    *b"<>[] 0123456789ABCDEFbeginbfchar\n"
                        .get(((x >> 16) % 33) as usize)
                        .unwrap_or(&b' ')
                })
                .collect();
            let m = CMap::parse(&junk);
            let _ = m.lookup(1);
            let _ = m.codes(&junk, 2);
        }
    }
}
