//! Stream filters of PDF that can hide text from a lexical scan.
//!
//! `ASCIIHexDecode`, `ASCII85Decode`, `RunLengthDecode`, `LZWDecode` and `FlateDecode` (with the
//! PNG and TIFF predictors) are decoded here, in chains, with every output bounded. Filters that
//! produce pictures (`DCTDecode`, `CCITTFaxDecode`, `JBIG2Decode`, `JPXDecode`) end a chain: what
//! they decode to is pixels, not text. Anything else is reported by the caller as a stream that
//! could not be read.

use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_zlib_with_limit};

use super::is_ws;
use super::syntax::Obj;

/// Why a stream could not be decoded.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    /// The output would exceed the allowed size.
    TooLarge,
    /// A filter or parameter this module does not implement (its name).
    Unsupported(String),
}

/// What one filter does to a stream.
enum Kind {
    Flate,
    Hex,
    A85,
    RunLength,
    Lzw,
    /// Ends the chain: the data is a picture.
    Image,
}

fn kind_of(name: &str) -> Option<Kind> {
    Some(match name {
        "FlateDecode" | "Fl" => Kind::Flate,
        "ASCIIHexDecode" | "AHx" => Kind::Hex,
        "ASCII85Decode" | "A85" => Kind::A85,
        "RunLengthDecode" | "RL" => Kind::RunLength,
        "LZWDecode" | "LZW" => Kind::Lzw,
        "DCTDecode" | "DCT" | "CCITTFaxDecode" | "CCF" | "JBIG2Decode" | "JPXDecode" => Kind::Image,
        _ => return None,
    })
}

/// The filter names of a stream dictionary, or the reason they cannot be known.
pub(super) fn filter_names(dict: &Obj) -> Result<Vec<String>, Failure> {
    match dict.get("Filter") {
        None | Some(Obj::Null) => Ok(Vec::new()),
        Some(Obj::Name(n)) => Ok(vec![n.clone()]),
        Some(Obj::Array(items)) => items
            .iter()
            .map(|o| match o {
                Obj::Name(n) => Ok(n.clone()),
                _ => Err(Failure::Unsupported("an indirect or malformed /Filter entry".to_owned())),
            })
            .collect(),
        Some(_) => Err(Failure::Unsupported("an indirect /Filter".to_owned())),
    }
}

/// The parameter dictionary of filter number `i`, if any.
fn params(dict: &Obj, i: usize, count: usize) -> Result<Option<&Obj>, Failure> {
    let unresolved = || Failure::Unsupported("indirect /DecodeParms".to_owned());
    match dict.get("DecodeParms").or_else(|| dict.get("DP")) {
        None | Some(Obj::Null) => Ok(None),
        Some(d @ Obj::Dict(_)) if count == 1 => Ok(Some(d)),
        Some(Obj::Array(items)) => match items.get(i) {
            None | Some(Obj::Null) => Ok(None),
            Some(d @ Obj::Dict(_)) => Ok(Some(d)),
            Some(_) => Err(unresolved()),
        },
        Some(Obj::Dict(_)) => Ok(None),
        Some(_) => Err(unresolved()),
    }
}

/// Decodes `raw` through the filters of `dict`, at most `cap` bytes out of each step.
///
/// `charge(out, in)` is called for each step so the caller can bill its budget; an `Err` from it
/// ends the decoding with [`Failure::TooLarge`].
pub(super) fn decode_chain(
    raw: &[u8],
    dict: &Obj,
    cap: usize,
    mut charge: impl FnMut(usize, usize) -> Result<(), ()>,
) -> Result<Vec<u8>, Failure> {
    let names = filter_names(dict)?;
    let mut data = raw.to_vec();
    for (i, name) in names.iter().enumerate() {
        let Some(kind) = kind_of(name) else {
            return Err(Failure::Unsupported(format!("the /{name} filter")));
        };
        let p = params(dict, i, names.len())?;
        let before = data.len();
        data = match kind {
            Kind::Image => {
                return if i + 1 == names.len() {
                    Ok(Vec::new())
                } else {
                    Err(Failure::Unsupported(format!("the /{name} filter inside a chain")))
                };
            }
            Kind::Flate => predict(flate(&data, cap)?, p, cap)?,
            Kind::Lzw => {
                let early = p.and_then(|p| p.get("EarlyChange")).and_then(Obj::as_int) != Some(0);
                predict(lzw(&data, cap, early)?, p, cap)?
            }
            Kind::Hex => hex(&data, cap)?,
            Kind::A85 => ascii85(&data, cap)?,
            Kind::RunLength => run_length(&data, cap)?,
        };
        charge(data.len(), before).map_err(|()| Failure::TooLarge)?;
    }
    Ok(data)
}

fn flate(data: &[u8], cap: usize) -> Result<Vec<u8>, Failure> {
    match decompress_to_vec_zlib_with_limit(data, cap) {
        Ok(d) => Ok(d),
        Err(e) if e.status == TINFLStatus::HasMoreOutput => Err(Failure::TooLarge),
        // Truncated and corrupt streams are common; what decoded is still inspected.
        Err(e) => Ok(e.output),
    }
}

fn hex(data: &[u8], cap: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let Some(v) = (match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }) else {
            continue;
        };
        match hi.take() {
            Some(h) => out.push((h << 4) | v),
            None => hi = Some(v),
        }
        if out.len() > cap {
            return Err(Failure::TooLarge);
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    Ok(out)
}

fn ascii85(data: &[u8], cap: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let flush = |group: &[u8; 5], n: usize, out: &mut Vec<u8>| {
        // A short final group is padded with 'u' and yields n - 1 bytes.
        let mut padded = *group;
        for slot in padded.iter_mut().skip(n) {
            *slot = 84;
        }
        let value =
            padded.iter().fold(0u32, |acc, &d| acc.wrapping_mul(85).wrapping_add(u32::from(d)));
        let bytes = value.to_be_bytes();
        out.extend_from_slice(bytes.get(..n.saturating_sub(1)).unwrap_or(&[]));
    };
    let mut iter = data.iter().copied().peekable();
    // An optional "<~" prefix is tolerated.
    if iter.peek() == Some(&b'<') {
        let mut probe = iter.clone();
        probe.next();
        if probe.peek() == Some(&b'~') {
            iter.next();
            iter.next();
        }
    }
    for b in iter {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                if let Some(slot) = group.get_mut(n) {
                    *slot = b - b'!';
                }
                n += 1;
                if n == 5 {
                    flush(&group, 5, &mut out);
                    n = 0;
                }
            }
            b if is_ws(b) => {}
            _ => break,
        }
        if out.len() > cap {
            return Err(Failure::TooLarge);
        }
    }
    if n > 1 {
        flush(&group, n, &mut out);
    }
    Ok(out)
}

fn run_length(data: &[u8], cap: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&len) = data.get(i) {
        i += 1;
        match len {
            128 => break,
            0..=127 => {
                let n = usize::from(len) + 1;
                out.extend_from_slice(data.get(i..i + n).unwrap_or(data.get(i..).unwrap_or(&[])));
                i += n;
            }
            _ => {
                let n = 257 - usize::from(len);
                if let Some(&b) = data.get(i) {
                    out.extend(core::iter::repeat_n(b, n));
                }
                i += 1;
            }
        }
        if out.len() > cap {
            return Err(Failure::TooLarge);
        }
    }
    Ok(out)
}

/// PDF LZW: codes of 9 to 12 bits, most significant bit first; 256 clears the table, 257 ends
/// the data.
fn lzw(data: &[u8], cap: usize, early_change: bool) -> Result<Vec<u8>, Failure> {
    const CLEAR: usize = 256;
    const EOD: usize = 257;
    // Each table entry is (previous code, last byte, length).
    let mut table: Vec<(u16, u8, u32)> = Vec::with_capacity(4096);
    let reset = |table: &mut Vec<(u16, u8, u32)>| {
        table.clear();
        for b in 0..=255u8 {
            table.push((0, b, 1));
        }
        // 256 and 257 are placeholders for the two control codes.
        table.push((0, 0, 0));
        table.push((0, 0, 0));
    };
    reset(&mut table);
    let early = usize::from(early_change);
    let mut out: Vec<u8> = Vec::new();
    let mut bits = 9usize;
    let mut acc = 0u32;
    let mut have = 0usize;
    let mut prev: Option<usize> = None;
    let mut scratch: Vec<u8> = Vec::new();
    for &byte in data {
        acc = (acc << 8) | u32::from(byte);
        have += 8;
        while have >= bits {
            have -= bits;
            let code = usize::try_from((acc >> have) & ((1 << bits) - 1)).unwrap_or(0);
            acc &= (1u32 << have) - 1;
            match code {
                CLEAR => {
                    reset(&mut table);
                    bits = 9;
                    prev = None;
                    continue;
                }
                EOD => return Ok(out),
                _ => {}
            }
            // Expand `code` (or, for the one code not yet defined, previous + first byte).
            scratch.clear();
            let known = code < table.len();
            let start = if known { code } else { prev.unwrap_or(0) };
            let mut cur = start;
            let mut guard = 0usize;
            while let Some(&(p, b, len)) = table.get(cur) {
                scratch.push(b);
                guard += 1;
                if len <= 1 || guard > 4096 {
                    break;
                }
                cur = usize::from(p);
            }
            scratch.reverse();
            if !known {
                let first = scratch.first().copied().unwrap_or(0);
                scratch.push(first);
            }
            if let Some(pc) = prev
                && table.len() < 4096
            {
                let first = scratch.first().copied().unwrap_or(0);
                let len = table.get(pc).map_or(1, |e| e.2) + 1;
                table.push((u16::try_from(pc).unwrap_or(0), first, len));
            }
            out.extend_from_slice(&scratch);
            if out.len() > cap {
                return Err(Failure::TooLarge);
            }
            prev = Some(code.min(table.len().saturating_sub(1)));
            let size = table.len() + early;
            bits = match size {
                0..=511 => 9,
                512..=1023 => 10,
                1024..=2047 => 11,
                _ => 12,
            };
        }
    }
    Ok(out)
}

/// Undoes the predictor of a Flate or LZW stream.
fn predict(data: Vec<u8>, params: Option<&Obj>, cap: usize) -> Result<Vec<u8>, Failure> {
    let Some(p) = params else { return Ok(data) };
    let int = |key: &str, default: i64| p.get(key).and_then(Obj::as_int).unwrap_or(default);
    let predictor = int("Predictor", 1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = usize::try_from(int("Colors", 1)).unwrap_or(0);
    let bpc = usize::try_from(int("BitsPerComponent", 8)).unwrap_or(0);
    let columns = usize::try_from(int("Columns", 1)).unwrap_or(0);
    let row = colors.saturating_mul(bpc).saturating_mul(columns).div_ceil(8);
    if row == 0 || row > (1 << 20) {
        return Err(Failure::Unsupported("a predictor with implausible parameters".to_owned()));
    }
    let bpp = (colors * bpc).div_ceil(8).max(1);
    match predictor {
        2 => {
            if bpc != 8 {
                return Err(Failure::Unsupported("the TIFF predictor below 8 bits".to_owned()));
            }
            let mut out = data;
            for line in out.chunks_mut(row) {
                for i in colors..line.len() {
                    let left = line.get(i - colors).copied().unwrap_or(0);
                    if let Some(b) = line.get_mut(i) {
                        *b = b.wrapping_add(left);
                    }
                }
            }
            Ok(out)
        }
        10..=15 => {
            let mut out: Vec<u8> = Vec::with_capacity(data.len());
            let mut prev = vec![0u8; row];
            let mut cur = vec![0u8; row];
            for line in data.chunks(row + 1) {
                let (&filter, body) = line.split_first().unwrap_or((&0, &[]));
                cur.fill(0);
                for (i, &x) in body.iter().enumerate() {
                    let left = if i >= bpp { cur.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                    let up = prev.get(i).copied().unwrap_or(0);
                    let up_left =
                        if i >= bpp { prev.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                    let v = match filter {
                        1 => x.wrapping_add(left),
                        2 => x.wrapping_add(up),
                        3 => x.wrapping_add(
                            u8::try_from((u16::from(left) + u16::from(up)) / 2).unwrap_or(0),
                        ),
                        4 => x.wrapping_add(paeth(left, up, up_left)),
                        _ => x,
                    };
                    if let Some(slot) = cur.get_mut(i) {
                        *slot = v;
                    }
                }
                out.extend_from_slice(cur.get(..body.len()).unwrap_or(&[]));
                core::mem::swap(&mut prev, &mut cur);
                if out.len() > cap {
                    return Err(Failure::TooLarge);
                }
            }
            Ok(out)
        }
        other => Err(Failure::Unsupported(format!("the predictor {other}"))),
    }
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

    #[test]
    fn ascii_filters() {
        assert_eq!(hex(b"48 65 6C6c 6F>", 100), Ok(b"Hello".to_vec()));
        assert_eq!(hex(b"4>", 100), Ok(vec![0x40]));
        // "Man " in ASCII85 is 9jqo^.
        assert_eq!(ascii85(b"9jqo^~>", 100), Ok(b"Man ".to_vec()));
        assert_eq!(ascii85(b"<~9jqo^~>", 100), Ok(b"Man ".to_vec()));
        assert_eq!(run_length(&[2, b'a', b'b', b'c', 254, b'x', 128], 100), Ok(b"abcxxx".to_vec()));
        assert_eq!(hex(&[b'a'; 10], 3), Err(Failure::TooLarge));
    }

    #[test]
    fn lzw_spec_example() {
        // PDF 32000-1, 7.4.4.2: 45 45 45 45 45 65 45 45 45 66 encodes as 80 0B 60 50 22 0C 0C 85 01.
        let out = lzw(&[0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01], 100, true);
        assert_eq!(out, Ok(vec![45, 45, 45, 45, 45, 65, 45, 45, 45, 66]));
    }

    #[test]
    fn png_up_predictor() {
        let dict = Obj::Dict(vec![
            ("Predictor".to_owned(), Obj::Int(12)),
            ("Columns".to_owned(), Obj::Int(3)),
        ]);
        // Two rows with the "Up" filter: the second adds the first.
        let data = vec![2, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(predict(data, Some(&dict), 100), Ok(vec![1, 2, 3, 2, 3, 4]));
    }
}
