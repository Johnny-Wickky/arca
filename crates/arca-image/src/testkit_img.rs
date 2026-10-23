#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]

//! Encoders for GIF and BMP files, used only by the tests.

use std::collections::HashMap;

/// LZW-compresses `indices` the way GIF does (variable code size, clear and end codes).
pub(crate) fn lzw_encode(min_code: u8, indices: &[u8]) -> Vec<u8> {
    let clear = 1u32 << min_code;
    let eoi = clear + 1;
    let mut out: Vec<u8> = Vec::new();
    let (mut acc, mut nbits) = (0u32, 0u32);
    let mut put = |code: u32, size: u32, out: &mut Vec<u8>| {
        acc |= code << nbits;
        nbits += size;
        while nbits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            nbits -= 8;
        }
    };
    let mut size = u32::from(min_code) + 1;
    let mut table: HashMap<(u32, u8), u32> = HashMap::new();
    let mut next = eoi + 1;
    put(clear, size, &mut out);
    let mut iter = indices.iter();
    let Some(&first) = iter.next() else {
        put(eoi, size, &mut out);
        if nbits > 0 {
            out.push(acc as u8);
        }
        return out;
    };
    let mut cur = u32::from(first);
    for &b in iter {
        if let Some(&c) = table.get(&(cur, b)) {
            cur = c;
        } else {
            put(cur, size, &mut out);
            if next < 4096 {
                table.insert((cur, b), next);
                next += 1;
                if next > (1 << size) && size < 12 {
                    size += 1;
                }
            } else {
                put(clear, size, &mut out);
                table.clear();
                size = u32::from(min_code) + 1;
                next = eoi + 1;
            }
            cur = u32::from(b);
        }
    }
    put(cur, size, &mut out);
    put(eoi, size, &mut out);
    if nbits > 0 {
        out.push(acc as u8);
    }
    out
}

fn sub_blocks(data: &[u8], out: &mut Vec<u8>) {
    for chunk in data.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
}

/// What a test GIF looks like.
#[derive(Clone, Default)]
pub(crate) struct GifSpec {
    pub w: usize,
    pub h: usize,
    pub palette: Vec<[u8; 3]>,
    pub transparent: Option<u8>,
    pub interlace: bool,
    /// Extra frames after the first (each the same size, all index 0).
    pub extra_frames: usize,
    pub comment: Option<Vec<u8>>,
    pub trailing: Vec<u8>,
    /// Frame offset on a larger canvas: (canvas w, canvas h, left, top).
    pub canvas: Option<(usize, usize, usize, usize)>,
}

pub(crate) fn gif(spec: &GifSpec, px: &dyn Fn(usize, usize) -> u8) -> Vec<u8> {
    let (cw, ch, left, top) = spec.canvas.unwrap_or((spec.w, spec.h, 0, 0));
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&(cw as u16).to_le_bytes());
    out.extend_from_slice(&(ch as u16).to_le_bytes());
    let bits = (spec.palette.len().max(2) as f64).log2().ceil() as u8;
    let table_len = 1usize << bits;
    out.extend_from_slice(&[0x80 | 0x70 | (bits - 1), 0, 0]);
    for i in 0..table_len {
        out.extend_from_slice(&spec.palette.get(i).copied().unwrap_or([0, 0, 0]));
    }
    if let Some(c) = &spec.comment {
        out.extend_from_slice(&[0x21, 0xFE]);
        sub_blocks(c, &mut out);
    }
    if spec.extra_frames > 0 {
        out.extend_from_slice(&[0x21, 0xFF, 11]);
        out.extend_from_slice(b"NETSCAPE2.0");
        sub_blocks(&[1, 0, 0], &mut out);
    }
    let min_code = bits.max(2);
    let frame = |out: &mut Vec<u8>, f: &dyn Fn(usize, usize) -> u8, gce: bool| {
        if gce {
            let flags = u8::from(spec.transparent.is_some());
            out.extend_from_slice(&[0x21, 0xF9, 4, flags, 0, 0, spec.transparent.unwrap_or(0), 0]);
        }
        out.push(0x2C);
        for v in [left, top, spec.w, spec.h] {
            out.extend_from_slice(&(v as u16).to_le_bytes());
        }
        out.push(if spec.interlace { 0x40 } else { 0 });
        out.push(min_code);
        let mut order: Vec<usize> = Vec::new();
        if spec.interlace {
            for (s, step) in [(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
                order.extend((s..spec.h).step_by(step));
            }
        } else {
            order.extend(0..spec.h);
        }
        let indices: Vec<u8> = order
            .iter()
            .flat_map(|&y| (0..spec.w).map(move |x| (x, y)))
            .map(|(x, y)| f(x, y))
            .collect();
        sub_blocks(&lzw_encode(min_code, &indices), out);
    };
    frame(&mut out, px, true);
    for _ in 0..spec.extra_frames {
        frame(&mut out, &|_, _| 0, false);
    }
    out.push(0x3B);
    out.extend_from_slice(&spec.trailing);
    out
}

/// A BMP with a 40-byte header. `rows` are top to bottom, already packed (without padding).
pub(crate) fn bmp(
    w: i32,
    h_signed: i32,
    bpp: u16,
    compression: u32,
    palette: &[[u8; 3]],
    masks: Option<[u32; 4]>,
    rows: &[Vec<u8>],
) -> Vec<u8> {
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    let mut pixels = Vec::new();
    let order: Vec<&Vec<u8>> =
        if h_signed < 0 { rows.iter().collect() } else { rows.iter().rev().collect() };
    for r in order {
        let mut line = r.clone();
        line.resize(stride, 0);
        pixels.extend(line);
    }
    let dib = if masks.is_some() && compression == 3 { 40 + 12 } else { 40 };
    let pal_len = palette.len() * 4;
    let offset = 14 + dib + pal_len;
    let mut out = b"BM".to_vec();
    out.extend_from_slice(&((offset + pixels.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h_signed.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&bpp.to_le_bytes());
    out.extend_from_slice(&compression.to_le_bytes());
    out.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    if let (Some(m), 3) = (masks, compression) {
        for v in &m[..3] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    for p in palette {
        out.extend_from_slice(&[p[2], p[1], p[0], 0]);
    }
    out.extend_from_slice(&pixels);
    out
}
