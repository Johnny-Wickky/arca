//! Stream filters (ISO 32000-1 §7.4): Flate, LZW, ASCII85, ASCIIHex, RunLength and the
//! PNG/TIFF predictors. Image codecs (DCT, JPX, CCITT, JBIG2) are *not* decoded: pixels
//! are never needed and every image decoder is a proven attack surface.

use arca_container::InflateBudget;
use arca_container::inflate::{Stop, inflate_auto};

/// One entry of a stream's filter chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FilterSpec {
    /// Canonical filter name (abbreviations are expanded).
    pub(super) name: String,
    pub(super) predictor: i64,
    pub(super) colors: i64,
    pub(super) bpc: i64,
    pub(super) columns: i64,
    pub(super) early_change: i64,
}

impl FilterSpec {
    pub(super) fn new(name: &str) -> Self {
        let canonical = match name {
            "Fl" => "FlateDecode",
            "LZW" => "LZWDecode",
            "A85" => "ASCII85Decode",
            "AHx" => "ASCIIHexDecode",
            "RL" => "RunLengthDecode",
            "CCF" => "CCITTFaxDecode",
            "DCT" => "DCTDecode",
            other => other,
        };
        Self {
            name: canonical.to_owned(),
            predictor: 1,
            colors: 1,
            bpc: 8,
            columns: 1,
            early_change: 1,
        }
    }
}

/// Why a stream could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FilterError {
    /// A filter this reader deliberately does not implement (images, encryption).
    Unsupported(String),
    /// The shared decompression budget was exhausted.
    Budget,
}

/// A decoded stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Decoded {
    pub(super) data: Vec<u8>,
    /// The data ends early because the encoded stream is damaged.
    pub(super) damaged: bool,
    /// The data was cut at the output limit.
    pub(super) capped: bool,
}

/// Applies the filter chain to `raw`.
pub(super) fn apply(
    raw: &[u8],
    specs: &[FilterSpec],
    max_out: usize,
    budget: &mut InflateBudget,
) -> Result<Decoded, FilterError> {
    if specs.is_empty() {
        let capped = raw.len() > max_out;
        return Ok(Decoded {
            data: raw.get(..raw.len().min(max_out)).unwrap_or(&[]).to_vec(),
            damaged: false,
            capped,
        });
    }
    let mut cur: Vec<u8> = raw.to_vec();
    let mut out = Decoded::default();
    for spec in specs {
        let mut step = decode_one(&cur, spec, max_out, budget)?;
        if spec.predictor > 1 && matches!(spec.name.as_str(), "FlateDecode" | "LZWDecode") {
            step.data = predict(step.data, spec);
        }
        out.damaged |= step.damaged;
        out.capped |= step.capped;
        cur = step.data;
    }
    out.data = cur;
    Ok(out)
}

fn decode_one(
    input: &[u8],
    spec: &FilterSpec,
    max_out: usize,
    budget: &mut InflateBudget,
) -> Result<Decoded, FilterError> {
    if budget.remaining() == 0 {
        return Err(FilterError::Budget);
    }
    let limit = max_out.min(usize::try_from(budget.remaining()).unwrap_or(usize::MAX));
    let (data, damaged, capped, consumed) = match spec.name.as_str() {
        "FlateDecode" => {
            let r = inflate_auto(input, limit);
            (r.data, r.stop == Stop::Damaged, r.stop == Stop::Capped, r.consumed)
        }
        "LZWDecode" => {
            let (d, dmg, cap) = lzw(input, limit, spec.early_change != 0);
            (d, dmg, cap, input.len())
        }
        "ASCII85Decode" => {
            let (d, cap) = ascii85(input, limit);
            (d, false, cap, input.len())
        }
        "ASCIIHexDecode" => {
            let (d, cap) = ascii_hex(input, limit);
            (d, false, cap, input.len())
        }
        "RunLengthDecode" => {
            let (d, cap) = run_length(input, limit);
            (d, false, cap, input.len())
        }
        other => return Err(FilterError::Unsupported(other.to_owned())),
    };
    let produced = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let used = u64::try_from(consumed).unwrap_or(u64::MAX);
    budget.charge(produced, used).map_err(|_| FilterError::Budget)?;
    Ok(Decoded { data, damaged, capped })
}

fn ascii_hex(input: &[u8], limit: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in input {
        let v = match b {
            b'>' => break,
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        match hi.take() {
            Some(h) => {
                if out.len() >= limit {
                    return (out, true);
                }
                out.push(h << 4 | v);
            }
            None => hi = Some(v),
        }
    }
    if let Some(h) = hi
        && out.len() < limit
    {
        out.push(h << 4);
    }
    (out, false)
}

/// Converts `count` (2..=5) ASCII85 digits into `count - 1` bytes.
fn a85_group(group: &[u8; 5], count: usize, out: &mut Vec<u8>) {
    let mut v: u32 = 0;
    for i in 0..5 {
        let d = if i < count { group.get(i).copied().unwrap_or(84) } else { 84 };
        v = v.wrapping_mul(85).wrapping_add(u32::from(d));
    }
    let keep = count.saturating_sub(1).min(4);
    out.extend(v.to_be_bytes().iter().take(keep));
}

fn ascii85(input: &[u8], limit: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let mut body = input;
    while let Some((&c, rest)) = body.split_first() {
        if matches!(c, b' ' | b'\n' | b'\r' | b'\t' | 0 | 12) {
            body = rest;
        } else {
            break;
        }
    }
    if body.starts_with(b"<~") {
        body = body.get(2..).unwrap_or(&[]);
    }
    for &c in body {
        match c {
            b'~' => break,
            b'z' if n == 0 => {
                if out.len().saturating_add(4) > limit {
                    return (out, true);
                }
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            b'!'..=b'u' => {
                if let Some(slot) = group.get_mut(n) {
                    *slot = c - b'!';
                }
                n += 1;
                if n == 5 {
                    if out.len().saturating_add(4) > limit {
                        return (out, true);
                    }
                    a85_group(&group, 5, &mut out);
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n >= 2 {
        a85_group(&group, n, &mut out);
    }
    let capped = out.len() > limit;
    out.truncate(limit);
    (out, capped)
}

fn run_length(input: &[u8], limit: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&n) = input.get(i) {
        i += 1;
        match n {
            128 => break,
            0..=127 => {
                let len = usize::from(n) + 1;
                let chunk = input.get(i..).unwrap_or(&[]);
                let chunk = chunk.get(..len.min(chunk.len())).unwrap_or(&[]);
                if out.len().saturating_add(chunk.len()) > limit {
                    return (out, true);
                }
                out.extend_from_slice(chunk);
                i = i.saturating_add(len);
            }
            129..=255 => {
                let Some(&b) = input.get(i) else { break };
                i += 1;
                let len = 257 - usize::from(n);
                if out.len().saturating_add(len) > limit {
                    return (out, true);
                }
                out.resize(out.len() + len, b);
            }
        }
    }
    (out, false)
}

/// The dictionary of an LZW decoder.
struct LzwTable {
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    length: Vec<u32>,
}

impl LzwTable {
    fn new() -> Self {
        let mut t = Self {
            prefix: Vec::with_capacity(4096),
            suffix: Vec::with_capacity(4096),
            length: Vec::with_capacity(4096),
        };
        t.reset();
        t
    }

    fn reset(&mut self) {
        self.prefix.clear();
        self.suffix.clear();
        self.length.clear();
        for b in 0..=255u8 {
            self.prefix.push(u16::MAX);
            self.suffix.push(b);
            self.length.push(1);
        }
        // 256 (clear) and 257 (end) hold no string.
        for _ in 0..2 {
            self.prefix.push(u16::MAX);
            self.suffix.push(0);
            self.length.push(0);
        }
    }

    fn len(&self) -> usize {
        self.prefix.len()
    }

    fn string_len(&self, code: u16) -> usize {
        self.length.get(usize::from(code)).map_or(0, |l| usize::try_from(*l).unwrap_or(0))
    }

    fn add(&mut self, prefix: u16, byte: u8) {
        if self.prefix.len() < 4096 {
            let len = self.length.get(usize::from(prefix)).copied().unwrap_or(0);
            self.prefix.push(prefix);
            self.suffix.push(byte);
            self.length.push(len.saturating_add(1));
        }
    }

    /// Appends the string of `code` to `out`; returns its first byte.
    fn write(&self, code: u16, out: &mut Vec<u8>, scratch: &mut Vec<u8>) -> Option<u8> {
        scratch.clear();
        let mut c = code;
        for _ in 0..self.string_len(code) {
            scratch.push(*self.suffix.get(usize::from(c))?);
            c = *self.prefix.get(usize::from(c))?;
        }
        scratch.reverse();
        let first = scratch.first().copied();
        out.extend_from_slice(scratch);
        first
    }
}

/// LZW as used by PDF: MSB-first codes of 9..=12 bits, clear = 256, end = 257.
/// Returns `(data, damaged, capped)`.
fn lzw(input: &[u8], limit: usize, early_change: bool) -> (Vec<u8>, bool, bool) {
    const CLEAR: u16 = 256;
    const END: u16 = 257;
    let mut table = LzwTable::new();
    let mut out: Vec<u8> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut bits: u32 = 0;
    let mut nbits: u32 = 0;
    let mut width: u32 = 9;
    let mut prev: Option<u16> = None;
    let mut bytes = input.iter();
    loop {
        while nbits < width {
            let Some(&b) = bytes.next() else { return (out, true, false) };
            bits = bits << 8 | u32::from(b);
            nbits += 8;
        }
        let code = u16::try_from((bits >> (nbits - width)) & ((1 << width) - 1)).unwrap_or(END);
        nbits -= width;
        bits &= (1u32 << nbits).wrapping_sub(1);
        match code {
            END => return (out, false, false),
            CLEAR => {
                table.reset();
                width = 9;
                prev = None;
                continue;
            }
            _ => {}
        }
        let next_code = u16::try_from(table.len()).unwrap_or(u16::MAX);
        let first_byte = if table.string_len(code) > 0 {
            if out.len().saturating_add(table.string_len(code)) > limit {
                return (out, false, true);
            }
            match table.write(code, &mut out, &mut scratch) {
                Some(f) => f,
                None => return (out, true, false),
            }
        } else if code == next_code {
            // The "KwKwK" case: previous string followed by its own first byte.
            let Some(p) = prev else { return (out, true, false) };
            if out.len().saturating_add(table.string_len(p)).saturating_add(1) > limit {
                return (out, false, true);
            }
            let Some(f) = table.write(p, &mut out, &mut scratch) else {
                return (out, true, false);
            };
            out.push(f);
            f
        } else {
            return (out, true, false);
        };
        if let Some(p) = prev {
            table.add(p, first_byte);
        }
        prev = Some(code);
        let used = table.len() + usize::from(early_change);
        width = match used {
            0..=511 => 9,
            512..=1023 => 10,
            1024..=2047 => 11,
            _ => 12,
        };
    }
}

fn clamp_params(spec: &FilterSpec) -> Option<(usize, usize, usize)> {
    let colors = usize::try_from(spec.colors).ok().filter(|c| (1..=32).contains(c))?;
    let bpc = usize::try_from(spec.bpc).ok().filter(|b| matches!(b, 1 | 2 | 4 | 8 | 16))?;
    let columns = usize::try_from(spec.columns).ok().filter(|c| (1..=(1 << 20)).contains(c))?;
    Some((colors, bpc, columns))
}

/// Undoes the PNG (10..=15) or TIFF (2) predictor. Unsupported combinations return the
/// data unchanged.
fn predict(data: Vec<u8>, spec: &FilterSpec) -> Vec<u8> {
    let Some((colors, bpc, columns)) = clamp_params(spec) else { return data };
    let row_bytes = (colors * bpc * columns).div_ceil(8);
    let bpp = (colors * bpc).div_ceil(8).max(1);
    if spec.predictor == 2 {
        if bpc != 8 {
            return data;
        }
        let mut out = data;
        for row in out.chunks_mut(row_bytes) {
            for i in colors..row.len() {
                let left = row.get(i - colors).copied().unwrap_or(0);
                if let Some(v) = row.get_mut(i) {
                    *v = v.wrapping_add(left);
                }
            }
        }
        return out;
    }
    if spec.predictor < 10 {
        return data;
    }
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    // The row above; beyond its end (or before the first row) everything reads as zero, so it
    // never needs to be padded to `row_bytes`, which a file may declare as tens of megabytes.
    let mut prev: Vec<u8> = Vec::new();
    for chunk in data.chunks(row_bytes + 1) {
        let Some((&filter, body)) = chunk.split_first() else { break };
        let mut cur: Vec<u8> = body.to_vec();
        for i in 0..cur.len() {
            let a = if i >= bpp { cur.get(i - bpp).copied().unwrap_or(0) } else { 0 };
            let b = prev.get(i).copied().unwrap_or(0);
            let c = if i >= bpp { prev.get(i - bpp).copied().unwrap_or(0) } else { 0 };
            let add = match filter {
                1 => a,
                2 => b,
                3 => u8::try_from((u16::from(a) + u16::from(b)) / 2).unwrap_or(0),
                4 => paeth(a, b, c),
                _ => 0,
            };
            if let Some(v) = cur.get_mut(i) {
                *v = v.wrapping_add(add);
            }
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i32::from(a), i32::from(b), i32::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::compress_to_vec_zlib;

    fn budget() -> InflateBudget {
        InflateBudget::new(1 << 30, 10_000, 1 << 30)
    }

    fn run(data: &[u8], names: &[&str]) -> Decoded {
        let specs: Vec<FilterSpec> = names.iter().map(|n| FilterSpec::new(n)).collect();
        apply(data, &specs, 1 << 20, &mut budget()).unwrap_or_default()
    }

    #[test]
    fn ascii_hex_and_85() {
        assert_eq!(run(b"48 65 6c6C 6f>", &["AHx"]).data, b"Hello");
        assert_eq!(run(b"4865 6c6c 6", &["ASCIIHexDecode"]).data, b"Hell`");
        assert_eq!(run(b"<~87cURD]i,\"Ebo80~>", &["A85"]).data, b"Hello World!");
        assert_eq!(run(b"z~>", &["A85"]).data, [0, 0, 0, 0]);
        assert_eq!(run(b"87cURDZ~>", &["A85"]).data, b"Hello");
    }

    #[test]
    fn run_length() {
        let enc = [2u8, b'a', b'b', b'c', 254, b'x', 128, 9, 9];
        assert_eq!(run(&enc, &["RL"]).data, b"abcxxx");
        assert_eq!(run(&[5, b'a', b'b'], &["RL"]).data, b"ab");
    }

    #[test]
    fn lzw_spec_example() {
        // Example from ISO 32000-1 §7.4.4.2.
        let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(run(&enc, &["LZW"]).data, [45, 45, 45, 45, 45, 65, 45, 45, 45, 66]);
    }

    #[test]
    fn flate_chain_and_png_predictor() {
        // Two rows of 3 bytes, the second with "Up" prediction.
        let raw = [0u8, 1, 2, 3, 2, 1, 1, 1];
        let z = compress_to_vec_zlib(&raw, 6);
        let mut spec = FilterSpec::new("FlateDecode");
        spec.predictor = 12;
        spec.columns = 3;
        let d = apply(&z, &[spec], 1 << 20, &mut budget()).unwrap_or_default();
        assert_eq!(d.data, [1, 2, 3, 2, 3, 4]);
        let hex: String = z.iter().map(|b| format!("{b:02x}")).collect();
        let d = run(format!("{hex}>").as_bytes(), &["AHx", "Fl"]);
        assert_eq!(d.data, raw);
    }

    #[test]
    fn a_predictor_row_of_sixty_megabytes_is_not_allocated_per_row() {
        // 32 colours x 16 bits x a million columns: a row the stream does not have.
        let mut s = FilterSpec::new("FlateDecode");
        s.predictor = 12;
        s.colors = 32;
        s.bpc = 16;
        s.columns = 1 << 20;
        let mut rows = Vec::new();
        for _ in 0..300 {
            rows.extend([0u8, 1, 2, 3, 4]);
        }
        let z = compress_to_vec_zlib(&rows, 6);
        let started = std::time::Instant::now();
        let d = apply(&z, &[s], 1 << 20, &mut budget()).unwrap_or_default();
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
        // The one (short) row is reconstructed; nothing was padded to 64 MiB per row.
        assert_eq!(d.data.len(), 1499);
    }

    #[test]
    fn flate_bomb_is_capped_and_charged() {
        let z = compress_to_vec_zlib(&vec![0u8; 8 << 20], 9);
        let specs = [FilterSpec::new("Fl")];
        let d = apply(&z, &specs, 1 << 20, &mut budget()).unwrap_or_default();
        assert!(d.capped && d.data.len() == 1 << 20);
        // A budget smaller than the stream cuts the output; the next stream is refused.
        let mut tight = InflateBudget::new(1000, 10_000, 1 << 30);
        let d = apply(&z, &specs, 1 << 20, &mut tight).unwrap_or_default();
        assert!(d.capped && d.data.len() == 1000);
        assert_eq!(apply(&z, &specs, 1 << 20, &mut tight), Err(FilterError::Budget));
        let mut ratio = InflateBudget::new(1 << 30, 100, 1 << 10);
        assert_eq!(apply(&z, &specs, 1 << 20, &mut ratio), Err(FilterError::Budget));
    }

    #[test]
    fn image_filters_are_refused() {
        let r = apply(b"\xff\xd8", &[FilterSpec::new("DCT")], 100, &mut budget());
        assert_eq!(r, Err(FilterError::Unsupported("DCTDecode".into())));
        let r = apply(b"x", &[FilterSpec::new("Crypt")], 100, &mut budget());
        assert!(matches!(r, Err(FilterError::Unsupported(_))));
    }

    #[test]
    fn no_filter_copies_with_cap() {
        let d = apply(b"abcdef", &[], 4, &mut budget()).unwrap_or_default();
        assert_eq!((d.data.as_slice(), d.capped), (b"abcd".as_slice(), true));
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut x = 99u32;
        for len in 0..400usize {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    x.to_be_bytes().first().copied().unwrap_or(0)
                })
                .collect();
            for name in
                ["FlateDecode", "LZWDecode", "ASCII85Decode", "ASCIIHexDecode", "RunLengthDecode"]
            {
                let mut s = FilterSpec::new(name);
                for predictor in [1, 2, 12, 15] {
                    s.predictor = predictor;
                    s.columns = 7;
                    s.colors = 3;
                    let _ = apply(&junk, std::slice::from_ref(&s), 4096, &mut budget());
                }
            }
        }
        // Absurd predictor parameters are ignored instead of allocating.
        let mut s = FilterSpec::new("FlateDecode");
        s.predictor = 12;
        s.columns = i64::MAX;
        s.colors = i64::MAX;
        let z = compress_to_vec_zlib(b"abc", 6);
        let d = apply(&z, &[s], 100, &mut budget()).unwrap_or_default();
        assert_eq!(d.data, b"abc");
    }
}
