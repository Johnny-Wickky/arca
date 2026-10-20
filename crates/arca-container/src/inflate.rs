//! Bounded, best-effort DEFLATE / zlib decompression for streams of unknown size.
//!
//! ZIP entries declare their size and are inflated by `zip` into a buffer of exactly that
//! size. PDF streams, in contrast, carry no trustworthy length of the decoded data, and a
//! damaged or hostile file is expected to hold truncated or corrupt streams. This module
//! therefore decodes *as much as possible* under a hard output cap and reports why it
//! stopped instead of failing: a reader that shows the first 40 pages of a damaged PDF
//! is more useful than one that shows nothing.

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

/// Why [`inflate`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The stream ended properly.
    Complete,
    /// The input ended before the stream did, or the data is corrupt after this point.
    Damaged,
    /// The output cap was reached; the data is cut.
    Capped,
}

/// Result of [`inflate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inflated {
    /// Everything decoded before the decoder stopped.
    pub data: Vec<u8>,
    /// Number of input bytes consumed.
    pub consumed: usize,
    /// Why decoding stopped.
    pub stop: Stop,
}

/// Initial output buffer: small inputs are usually small outputs.
const INITIAL_MIN: usize = 4 << 10;

/// Decodes `input` as a zlib stream (RFC 1950); with `zlib == false` as raw DEFLATE.
///
/// At most `max_out` bytes are produced. The Adler-32 checksum is not verified: the aim is
/// to show what is readable. Never panics, never allocates more than `max_out` plus a small
/// constant.
#[must_use]
pub fn inflate(input: &[u8], max_out: usize, zlib: bool) -> Inflated {
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
        | inflate_flags::TINFL_FLAG_IGNORE_ADLER32
        | if zlib { inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER } else { 0 };
    let mut out: Vec<u8> = Vec::new();
    let first = input.len().saturating_mul(3).max(INITIAL_MIN).min(max_out);
    if out.try_reserve_exact(first).is_err() {
        return Inflated { data: Vec::new(), consumed: 0, stop: Stop::Capped };
    }
    out.resize(first, 0);
    let mut state = Box::<DecompressorOxide>::default();
    let mut rest = input;
    let mut produced = 0usize;
    let stop = loop {
        let (status, used, wrote) = decompress(&mut state, rest, &mut out, produced, flags);
        produced = produced.saturating_add(wrote).min(out.len());
        rest = rest.get(used..).unwrap_or(&[]);
        match status {
            TINFLStatus::Done => break Stop::Complete,
            TINFLStatus::HasMoreOutput => {
                if out.len() >= max_out {
                    break Stop::Capped;
                }
                let target = out.len().saturating_mul(2).min(max_out);
                let extra = target.saturating_sub(out.len());
                if out.try_reserve_exact(extra).is_err() {
                    break Stop::Capped;
                }
                out.resize(target, 0);
            }
            _ => break Stop::Damaged,
        }
    };
    out.truncate(produced);
    Inflated { data: out, consumed: input.len().saturating_sub(rest.len()), stop }
}

/// Decodes a zlib stream; if it has no valid zlib header, tries raw DEFLATE.
///
/// Some producers omit the two-byte header. The attempt is made only when the header
/// check fails, so a valid stream is never decoded twice.
#[must_use]
pub fn inflate_auto(input: &[u8], max_out: usize) -> Inflated {
    let header_ok = match (input.first(), input.get(1)) {
        (Some(&cmf), Some(&flg)) => {
            cmf & 0x0F == 8 && (u16::from(cmf) * 256 + u16::from(flg)) % 31 == 0
        }
        _ => false,
    };
    if header_ok {
        return inflate(input, max_out, true);
    }
    inflate(input, max_out, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::compress_to_vec_zlib;

    #[test]
    fn roundtrip_zlib_and_raw() {
        let text: Vec<u8> = (0..20_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let z = compress_to_vec_zlib(&text, 6);
        let r = inflate(&z, 1 << 20, true);
        assert_eq!((r.stop, r.data.as_slice()), (Stop::Complete, text.as_slice()));
        assert_eq!(r.consumed, z.len());
        let raw = z.get(2..z.len().saturating_sub(4)).unwrap_or(&[]);
        let r = inflate(raw, 1 << 20, false);
        assert_eq!((r.stop, r.data.as_slice()), (Stop::Complete, text.as_slice()));
        let r = inflate_auto(raw, 1 << 20);
        assert_eq!((r.stop, r.data.as_slice()), (Stop::Complete, text.as_slice()));
        let r = inflate_auto(&z, 1 << 20);
        assert_eq!((r.stop, r.data.as_slice()), (Stop::Complete, text.as_slice()));
    }

    #[test]
    fn output_is_capped_exactly() {
        let bomb = compress_to_vec_zlib(&vec![0u8; 5 << 20], 9);
        assert!(bomb.len() < 10_000);
        for cap in [0usize, 1, 100, 4_096, 4_097, 1 << 20] {
            let r = inflate(&bomb, cap, true);
            assert!(r.data.len() <= cap, "cap {cap}");
            if cap > 0 {
                assert_eq!(r.stop, Stop::Capped, "cap {cap}");
                assert_eq!(r.data.len(), cap, "cap {cap}");
            }
        }
    }

    #[test]
    fn truncated_stream_yields_prefix() {
        let text: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let z = compress_to_vec_zlib(&text, 6);
        let cut = z.get(..z.len() / 2).unwrap_or(&[]);
        let r = inflate(cut, 1 << 20, true);
        assert_eq!(r.stop, Stop::Damaged);
        assert!(!r.data.is_empty() && text.starts_with(&r.data));
    }

    #[test]
    fn garbage_never_panics() {
        let mut x = 0x1234_5678u32;
        for len in 0..300usize {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (x >> 24) as u8
                })
                .collect();
            for zlib in [true, false] {
                let r = inflate(&junk, 4096, zlib);
                assert!(r.data.len() <= 4096 && r.consumed <= junk.len());
            }
            let _ = inflate_auto(&junk, 4096);
        }
    }

    #[test]
    fn empty_input() {
        let r = inflate(&[], 100, true);
        assert_eq!((r.data.len(), r.stop), (0, Stop::Damaged));
    }
}
