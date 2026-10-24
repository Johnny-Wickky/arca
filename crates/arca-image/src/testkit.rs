#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]

//! Encoders used only by the tests: they write the images the decoders are checked against.

use miniz_oxide::deflate::compress_to_vec_zlib;

pub(crate) fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut h = arca_digest::Crc32::new();
    h.update(kind);
    h.update(body);
    out.extend_from_slice(&h.finalize().to_be_bytes());
}

/// What a test PNG looks like.
#[derive(Clone, Default)]
pub(crate) struct PngSpec {
    pub w: usize,
    pub h: usize,
    pub depth: u8,
    /// PNG colour type: 0 grey, 2 RGB, 3 palette, 4 grey+alpha, 6 RGBA.
    pub color: u8,
    pub interlace: bool,
    pub palette: Vec<[u8; 3]>,
    pub trns: Vec<u8>,
    pub extra: Vec<([u8; 4], Vec<u8>)>,
    pub trailing: Vec<u8>,
    /// `Some(f)` forces filter type `f` for every row; `None` cycles through all five.
    pub filter: Option<u8>,
}

fn channels(color: u8) -> usize {
    match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        _ => 4,
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
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

/// Filters `raw` (an unfiltered row) with filter `f`.
fn filter_row(f: u8, bpp: usize, raw: &[u8], prev: &[u8]) -> Vec<u8> {
    let left = |i: usize| if i >= bpp { raw[i - bpp] } else { 0 };
    let upleft = |i: usize| if i >= bpp { prev[i - bpp] } else { 0 };
    raw.iter()
        .enumerate()
        .map(|(i, &x)| match f {
            0 => x,
            1 => x.wrapping_sub(left(i)),
            2 => x.wrapping_sub(prev[i]),
            3 => x.wrapping_sub(((u16::from(left(i)) + u16::from(prev[i])) / 2) as u8),
            _ => x.wrapping_sub(paeth(left(i), prev[i], upleft(i))),
        })
        .collect()
}

/// Packs the samples of one row (`values`, already in `0..2^depth`) into bytes.
fn pack(values: &[u16], depth: u8) -> Vec<u8> {
    match depth {
        8 => values.iter().map(|&v| v as u8).collect(),
        16 => values.iter().flat_map(|&v| v.to_be_bytes()).collect(),
        d => {
            let per = 8 / d as usize;
            values
                .chunks(per)
                .map(|c| {
                    let mut b = 0u8;
                    for (i, &v) in c.iter().enumerate() {
                        b |= (v as u8) << (8 - d as usize * (i + 1));
                    }
                    b
                })
                .collect()
        }
    }
}

/// Writes a PNG; `px(x, y)` returns the channel values of a pixel, each in `0..2^depth`.
pub(crate) fn png(spec: &PngSpec, px: &dyn Fn(usize, usize) -> Vec<u16>) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(spec.w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(spec.h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[spec.depth, spec.color, 0, 0, u8::from(spec.interlace)]);
    chunk(&mut out, b"IHDR", &ihdr);
    if !spec.palette.is_empty() {
        chunk(&mut out, b"PLTE", &spec.palette.concat());
    }
    if !spec.trns.is_empty() {
        chunk(&mut out, b"tRNS", &spec.trns);
    }
    for (k, b) in &spec.extra {
        chunk(&mut out, k, b);
    }
    let bpp = (channels(spec.color) * spec.depth as usize).div_ceil(8).max(1);
    let passes: Vec<(usize, usize, usize, usize)> = if spec.interlace {
        vec![
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
    } else {
        vec![(0, 0, 1, 1)]
    };
    let mut raw = Vec::new();
    for (x0, y0, dx, dy) in passes {
        let pw = spec.w.saturating_sub(x0).div_ceil(dx);
        let ph = spec.h.saturating_sub(y0).div_ceil(dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let mut prev: Vec<u8> = Vec::new();
        for r in 0..ph {
            let y = y0 + r * dy;
            let values: Vec<u16> = (0..pw).flat_map(|i| px(x0 + i * dx, y)).collect();
            let row = pack(&values, spec.depth);
            if prev.is_empty() {
                prev = vec![0; row.len()];
            }
            let f = spec.filter.unwrap_or((r % 5) as u8);
            raw.push(f);
            raw.extend(filter_row(f, bpp, &row, &prev));
            prev = row;
        }
    }
    chunk(&mut out, b"IDAT", &compress_to_vec_zlib(&raw, 6));
    chunk(&mut out, b"IEND", &[]);
    out.extend_from_slice(&spec.trailing);
    out
}
