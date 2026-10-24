// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The one picture file Arca writes: a plain PNG made only of the pixels.
//!
//! A sterile copy of a picture is not the original with parts cut out, it is a *new file*: the
//! pixels the sandbox decoded, written by this module as a signature, one header, the pixel data
//! and an end marker. No colour profile, no text, no metadata, no ancillary chunk of any kind,
//! nothing appended. What a system decoder is shown cannot have been shaped by the original's
//! structure.

use core::fmt;

use miniz_oxide::deflate::compress_to_vec_zlib;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Pixel data goes into chunks of this size (any size is valid; this one is kind to readers).
const IDAT_CHUNK: usize = 1 << 20;
/// 1 is fast and poor, 10 is slow and good; the sandbox pays for every step in fuel.
const LEVEL: u8 = 2;

/// Why a picture could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(&'static str);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl core::error::Error for EncodeError {}

/// What was changed in the pixels on the way (the caller reports it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncodeNotes {
    /// Fully transparent pixels had colour values; they were set to black. (Nothing shows them,
    /// but data can be hidden there.)
    pub cleared_hidden_colour: bool,
}

/// Writes `rgba` (straight alpha, `width × height × 4` bytes) as a PNG: grey, grey with alpha,
/// RGB or RGBA, whichever holds the pixels exactly and is the smallest; 8 bits per sample.
///
/// # Errors
/// [`EncodeError`] if the buffer is not `width × height × 4` bytes or the size is zero.
pub fn encode_png(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(Vec<u8>, EncodeNotes), EncodeError> {
    let w = usize::try_from(width).map_err(|_| EncodeError("the picture is too wide"))?;
    let h = usize::try_from(height).map_err(|_| EncodeError("the picture is too tall"))?;
    let expect = w.checked_mul(h).and_then(|n| n.checked_mul(4));
    if w == 0 || h == 0 || expect != Some(rgba.len()) {
        return Err(EncodeError("the pixel buffer does not match the size"));
    }
    // Hidden colour in pixels that are fully transparent is dropped first, so that it cannot
    // make the picture look "coloured" or "with alpha" either.
    let mut notes = EncodeNotes::default();
    let mut owned: Option<Vec<u8>> = None;
    if rgba.chunks_exact(4).any(|p| p.get(3) == Some(&0) && p.iter().take(3).any(|&c| c != 0)) {
        let mut copy = rgba.to_vec();
        for p in copy.chunks_exact_mut(4) {
            if p.get(3) == Some(&0) {
                p.fill(0);
            }
        }
        notes.cleared_hidden_colour = true;
        owned = Some(copy);
    }
    let pixels: &[u8] = owned.as_deref().unwrap_or(rgba);

    let (mut opaque, mut grey) = (true, true);
    for p in pixels.chunks_exact(4) {
        if let [r, g, b, a] = *p {
            opaque &= a == 255;
            grey &= r == g && g == b;
        }
    }
    // (PNG colour type, samples per pixel)
    let (colour_type, bpp) = match (grey, opaque) {
        (true, true) => (0u8, 1usize),
        (true, false) => (4, 2),
        (false, true) => (2, 3),
        (false, false) => (6, 4),
    };
    let stride = w.checked_mul(bpp).ok_or(EncodeError("the picture is too wide"))?;

    // The scan lines, each with the filter that makes it most compressible.
    let packed = pack_row_fn(grey, opaque);
    let mut raw: Vec<u8> = Vec::new();
    raw.try_reserve_exact(
        h.checked_mul(stride + 1).ok_or(EncodeError("the picture is too large"))?,
    )
    .map_err(|_| EncodeError("not enough memory for the picture"))?;
    let mut prev = vec![0u8; stride];
    let mut cur: Vec<u8> = Vec::with_capacity(stride);
    let mut candidate: Vec<u8> = Vec::with_capacity(stride);
    let mut best: Vec<u8> = Vec::with_capacity(stride);
    for row in pixels.chunks_exact(w * 4) {
        cur.clear();
        packed(row, &mut cur);
        let mut best_kind = 0u8;
        let mut best_score = u64::MAX;
        for kind in 0..=4u8 {
            candidate.clear();
            filter(kind, bpp, &cur, &prev, &mut candidate);
            let score = score(&candidate);
            if score < best_score {
                best_score = score;
                best_kind = kind;
                core::mem::swap(&mut best, &mut candidate);
            }
        }
        raw.push(best_kind);
        raw.extend_from_slice(&best);
        core::mem::swap(&mut prev, &mut cur);
    }
    drop((prev, cur, candidate, best, owned));

    let compressed = compress_to_vec_zlib(&raw, LEVEL);
    drop(raw);

    let mut out = Vec::with_capacity(compressed.len() + 128);
    out.extend_from_slice(&SIGNATURE);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 8 bits per sample, the colour type, deflate, adaptive filtering, no interlace.
    header.extend_from_slice(&[8, colour_type, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    for part in compressed.chunks(IDAT_CHUNK) {
        chunk(&mut out, b"IDAT", part);
    }
    chunk(&mut out, b"IEND", &[]);
    Ok((out, notes))
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    let mut crc = arca_digest::crc32::Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

type PackRow = fn(&[u8], &mut Vec<u8>);

/// How one row of RGBA pixels becomes the samples of the chosen colour type.
fn pack_row_fn(grey: bool, opaque: bool) -> PackRow {
    match (grey, opaque) {
        (true, true) => |row, out| out.extend(row.chunks_exact(4).filter_map(|p| p.first())),
        (true, false) => |row, out| {
            for p in row.chunks_exact(4) {
                if let [g, _, _, a] = *p {
                    out.extend_from_slice(&[g, a]);
                }
            }
        },
        (false, true) => |row, out| {
            for p in row.chunks_exact(4) {
                out.extend(p.iter().take(3));
            }
        },
        (false, false) => |row, out| out.extend_from_slice(row),
    }
}

/// Applies the PNG filter `kind` (0 none, 1 sub, 2 up, 3 average, 4 Paeth) to a row.
fn filter(kind: u8, bpp: usize, cur: &[u8], prev: &[u8], out: &mut Vec<u8>) {
    let head = bpp.min(cur.len());
    match kind {
        1 => {
            out.extend_from_slice(cur.get(..head).unwrap_or(&[]));
            out.extend(cur.iter().skip(bpp).zip(cur).map(|(c, a)| c.wrapping_sub(*a)));
        }
        2 => out.extend(cur.iter().zip(prev).map(|(c, b)| c.wrapping_sub(*b))),
        3 => {
            out.extend(cur.iter().take(head).zip(prev).map(|(c, b)| c.wrapping_sub(b >> 1)));
            out.extend(
                cur.iter()
                    .skip(bpp)
                    .zip(cur)
                    .zip(prev.iter().skip(bpp))
                    .map(|((c, a), b)| c.wrapping_sub((a & b) + ((a ^ b) >> 1))),
            );
        }
        4 => {
            out.extend(cur.iter().take(head).zip(prev).map(|(c, b)| c.wrapping_sub(*b)));
            out.extend(
                cur.iter()
                    .skip(bpp)
                    .zip(cur)
                    .zip(prev.iter().skip(bpp).zip(prev))
                    .map(|((c, a), (b, d))| c.wrapping_sub(paeth(*a, *b, *d))),
            );
        }
        _ => out.extend_from_slice(cur),
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

/// The usual heuristic for choosing a filter: the smaller the sum of the bytes as signed numbers,
/// the better the row will compress.
fn score(filtered: &[u8]) -> u64 {
    filtered.iter().map(|&b| u64::from(i8::from_ne_bytes([b]).unsigned_abs())).sum()
}

/// A SHA-256 over the size and the pixels, with the colour of fully transparent pixels left out
/// (the writer clears it): two pictures that look the same have the same fingerprint. It is what
/// lets the sterile copy be compared with the picture without holding both in memory.
#[must_use]
pub fn pixels_fingerprint(width: u32, height: u32, rgba: &[u8]) -> [u8; 32] {
    let mut h = arca_digest::sha256::Sha256::new();
    h.update(&width.to_be_bytes());
    h.update(&height.to_be_bytes());
    let mut block = [0u8; 4096];
    for part in rgba.chunks(block.len()) {
        let used = block.get_mut(..part.len()).unwrap_or(&mut []);
        used.copy_from_slice(part);
        for p in used.chunks_exact_mut(4) {
            if p.get(3) == Some(&0) {
                p.fill(0);
            }
        }
        h.update(used);
    }
    h.finalize().0
}
