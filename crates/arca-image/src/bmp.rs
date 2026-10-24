//! Windows bitmaps (`BM`): 1, 4, 8, 16, 24 and 32 bits, bit fields, RLE4 and RLE8, bottom-up and
//! top-down. Bitmaps that hold a JPEG or PNG inside (`BI_JPEG`, `BI_PNG`) are not read.

use crate::bytes::{Cursor, to_usize};
use crate::scale::Downscaler;
use crate::{DecodeError, Facts, ImageFormat, Limits, Raw, flag};

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_JPEG: u32 = 4;
const BI_PNG: u32 = 5;
const BI_ALPHABITFIELDS: u32 = 6;

fn bad(msg: &str) -> DecodeError {
    DecodeError::Malformed(format!("BMP: {msg}"))
}

/// Whether the first bytes look like a bitmap file header followed by a known DIB header.
pub(crate) fn looks_like_bmp(d: &[u8]) -> bool {
    if !d.starts_with(b"BM") || d.len() < 26 {
        return false;
    }
    let mut c = Cursor::at(d, 10);
    let (Some(offset), Some(dib)) = (c.u32_le(), c.u32_le()) else { return false };
    matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124) && to_usize(offset) >= 14 + to_usize(dib)
}

/// The pixel array of an uncompressed bitmap: at most `stride × rows` bytes at `offset`.
fn pixel_slice(data: &[u8], offset: usize, stride: usize, rows: usize) -> &[u8] {
    let end = offset.saturating_add(stride.saturating_mul(rows)).min(data.len());
    data.get(offset..end).unwrap_or(&[])
}

/// A colour channel described by a bit mask.
#[derive(Clone, Copy, Default)]
struct Channel {
    shift: u32,
    bits: u32,
}

impl Channel {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Self::default();
        }
        let shift = mask.trailing_zeros();
        let bits = (mask >> shift).trailing_ones();
        Self { shift, bits: bits.min(8) }
    }

    /// The channel of `px` scaled to 8 bits.
    fn get(self, px: u32) -> u8 {
        if self.bits == 0 {
            return 0;
        }
        let v = (px >> self.shift) & ((1u32 << self.bits) - 1);
        u8::try_from(v * 255 / ((1u32 << self.bits) - 1)).unwrap_or(255)
    }
}

/// What a walk over run-length coded pixel data found.
struct RleWalk {
    /// Bytes consumed, up to and including the end-of-bitmap code.
    consumed: usize,
    /// The end-of-bitmap code was reached (not the end of the data first).
    finished: bool,
    /// Bytes of commands that draw nothing at all: runs that start outside the bitmap.
    hidden: u64,
}

/// Walks RLE8 or RLE4 data of a `w × h` bitmap. `put(x, y, value)` is called for every pixel that
/// lands inside the bitmap. The work is bounded: a run is cut at the right edge before it is
/// drawn, so the pixels drawn never exceed `w × h`, and the codes are at most the data.
fn rle_walk(
    pixel_data: &[u8],
    eight: bool,
    w: usize,
    h: usize,
    mut put: impl FnMut(usize, usize, u8),
) -> RleWalk {
    let (mut x, mut y) = (0usize, 0usize);
    let mut p = Cursor::new(pixel_data);
    let mut hidden = 0u64;
    let mut finished = false;
    loop {
        let start = p.pos();
        let (Some(n), Some(v)) = (p.u8(), p.u8()) else { break };
        let visible = x < w && y < h;
        if n > 0 {
            let n = usize::from(n);
            if visible {
                for i in 0..n.min(w - x) {
                    let value = if eight {
                        v
                    } else if i % 2 == 0 {
                        v >> 4
                    } else {
                        v & 15
                    };
                    put(x + i, y, value);
                }
            } else {
                hidden += 2;
            }
            x = x.saturating_add(n);
            continue;
        }
        match v {
            0 => {
                x = 0;
                y = y.saturating_add(1);
            }
            1 => {
                finished = true;
                break;
            }
            2 => {
                let (Some(dx), Some(dy)) = (p.u8(), p.u8()) else { break };
                x = x.saturating_add(usize::from(dx));
                y = y.saturating_add(usize::from(dy));
            }
            count => {
                let count = usize::from(count);
                let bytes = if eight { count } else { count.div_ceil(2) };
                let Some(run) = p.take(bytes) else { break };
                if visible {
                    for i in 0..count.min(w - x) {
                        let value = if eight {
                            run.get(i).copied().unwrap_or(0)
                        } else {
                            let b = run.get(i / 2).copied().unwrap_or(0);
                            if i % 2 == 0 { b >> 4 } else { b & 15 }
                        };
                        put(x + i, y, value);
                    }
                } else {
                    hidden += (p.pos() - start) as u64;
                }
                x = x.saturating_add(count);
                // Absolute runs are padded to an even number of bytes.
                if bytes % 2 == 1 {
                    let _ = p.u8();
                }
            }
        }
    }
    RleWalk { consumed: p.pos(), finished, hidden }
}

pub(crate) fn decode(data: &[u8], limits: &Limits, pixels: bool) -> Result<Raw, DecodeError> {
    let mut facts = Facts::new(ImageFormat::Bmp);
    let mut c = Cursor::at(data, 10);
    let offset = to_usize(c.u32_le().ok_or_else(|| bad("the header is cut short"))?);
    let dib = to_usize(c.u32_le().ok_or_else(|| bad("the header is cut short"))?);
    // The two reserved words of the file header are zero in every bitmap anyone writes.
    facts.unaccounted +=
        data.get(6..10).map_or(0, |r| r.iter().filter(|&&b| b != 0).count()) as u64;
    let (width, height_raw, bpp, compression, clr_used, mut masks): (
        i64,
        i64,
        u32,
        u32,
        usize,
        [u32; 4],
    );
    if dib == 12 {
        let (Some(w), Some(h), Some(_planes), Some(b)) =
            (c.u16_le(), c.u16_le(), c.u16_le(), c.u16_le())
        else {
            return Err(bad("the header is cut short"));
        };
        (width, height_raw, bpp, compression, clr_used, masks) =
            (i64::from(w), i64::from(h), u32::from(b), BI_RGB, 0, [0; 4]);
    } else {
        let (
            Some(w),
            Some(h),
            Some(_planes),
            Some(b),
            Some(comp),
            Some(_size),
            Some(_x),
            Some(_y),
            Some(used),
            Some(_imp),
        ) = (
            c.i32_le(),
            c.i32_le(),
            c.u16_le(),
            c.u16_le(),
            c.u32_le(),
            c.u32_le(),
            c.u32_le(),
            c.u32_le(),
            c.u32_le(),
            c.u32_le(),
        )
        else {
            return Err(bad("the header is cut short"));
        };
        let mut m = [0u32; 4];
        if dib >= 52 {
            for slot in m.iter_mut().take(if dib >= 56 { 4 } else { 3 }) {
                *slot = c.u32_le().unwrap_or(0);
            }
        }
        (width, height_raw, bpp, compression, clr_used, masks) =
            (i64::from(w), i64::from(h), u32::from(b), comp, to_usize(used), m);
    }
    if compression == BI_JPEG || compression == BI_PNG {
        return Err(DecodeError::Unsupported(
            "a bitmap that holds a JPEG or PNG is not shown".into(),
        ));
    }
    if width <= 0 || height_raw == 0 {
        return Err(bad("the size is zero or negative"));
    }
    let top_down = height_raw < 0;
    let height = height_raw.abs();
    let (w, h) = (
        usize::try_from(width).unwrap_or(usize::MAX),
        usize::try_from(height).unwrap_or(usize::MAX),
    );
    if w > to_usize(limits.max_side) || h > to_usize(limits.max_side) {
        return Err(DecodeError::TooLarge(format!("BMP: {w}×{h} pixels is beyond the limit")));
    }
    let area = (w as u64) * (h as u64);
    let rle = compression == BI_RLE8 || compression == BI_RLE4;
    let cap = if rle { limits.max_buffered_pixels } else { limits.max_stream_pixels };
    if area > cap {
        return Err(DecodeError::TooLarge(format!(
            "BMP: {area} pixels is beyond the limit of {cap}"
        )));
    }
    if !matches!(
        (bpp, compression),
        (1 | 4 | 8, BI_RGB)
            | (4, BI_RLE4)
            | (8, BI_RLE8)
            | (16 | 32, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS)
            | (24, BI_RGB)
    ) {
        return Err(DecodeError::Unsupported(format!(
            "BMP with {bpp} bits and coding {compression} is not shown"
        )));
    }
    facts.width = u32::try_from(w).unwrap_or(u32::MAX);
    facts.height = u32::try_from(h).unwrap_or(u32::MAX);
    facts.bits = u8::try_from(bpp).unwrap_or(8);
    // The palette follows the headers.
    let mut palette: Vec<[u8; 4]> = Vec::new();
    if bpp <= 8 {
        facts.flags |= flag::PALETTE;
        let n = if clr_used > 0 { clr_used.min(1 << bpp) } else { 1 << bpp };
        let entry = if dib == 12 { 3 } else { 4 };
        let mut pc = Cursor::at(data, 14 + dib);
        for _ in 0..n {
            let Some(e) = pc.take(entry) else { break };
            palette.push([
                e.get(2).copied().unwrap_or(0),
                e.get(1).copied().unwrap_or(0),
                e.first().copied().unwrap_or(0),
                255,
            ]);
        }
    }
    // Bytes of one padded row.
    let stride = (w * to_usize(bpp)).div_ceil(32) * 4;
    if bpp == 16 || bpp == 32 {
        if compression == BI_RGB {
            // Without bit fields the fourth byte of a 32-bit pixel is alpha only if the file uses it.
            let alpha_used = bpp == 32
                && pixel_slice(data, offset, stride, h)
                    .chunks_exact(4)
                    .any(|p| p.get(3).is_some_and(|&a| a != 0));
            masks = if bpp == 16 {
                [0x7C00, 0x03E0, 0x001F, 0]
            } else {
                [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, if alpha_used { 0xFF00_0000 } else { 0 }]
            };
        } else if dib < 52 {
            // BI_BITFIELDS stores the three masks right after the 40-byte header.
            let mut mc = Cursor::at(data, 14 + 40);
            masks =
                [mc.u32_le().unwrap_or(0), mc.u32_le().unwrap_or(0), mc.u32_le().unwrap_or(0), 0];
            if compression == BI_ALPHABITFIELDS {
                masks[3] = mc.u32_le().unwrap_or(0);
            }
        }
    }
    if masks[3] != 0 && bpp >= 16 {
        facts.flags |= flag::HAS_ALPHA;
    }
    // Between the headers (with the palette, or the bit masks and the optional colour table)
    // and the pixel array there is nothing: a gap is not part of the picture.
    let masks_len = match (dib, compression) {
        (40, BI_BITFIELDS) => 12,
        (40, BI_ALPHABITFIELDS) => 16,
        _ => 0,
    };
    let table_len = if bpp <= 8 {
        palette.len() * if dib == 12 { 3 } else { 4 }
    } else {
        clr_used.min(256) * if dib == 12 { 3 } else { 4 }
    };
    let header_end =
        14usize.saturating_add(dib).saturating_add(masks_len).saturating_add(table_len);
    facts.unaccounted += offset.min(data.len()).saturating_sub(header_end) as u64;
    let pixel_data = data.get(offset..).unwrap_or(&[]);
    if rle {
        // Walk the run-length codes without drawing: they say where the pixel data ends, and
        // which of them draw nothing at all.
        let walk = rle_walk(pixel_data, compression == BI_RLE8, w, h, |_, _, _| {});
        facts.unaccounted += walk.hidden;
        if walk.finished {
            facts.trailing = (pixel_data.len() - walk.consumed) as u64;
        }
    } else {
        // Trailing data after an uncompressed pixel array.
        let end = offset.saturating_add(stride.saturating_mul(h));
        if end <= data.len() {
            facts.trailing = (data.len() - end) as u64;
        }
        // Padding bytes at the end of a row are zero in every bitmap anyone writes.
        let used = (w * to_usize(bpp)).div_ceil(8);
        if stride > used {
            for r in 0..h {
                let at = offset.saturating_add(r.saturating_mul(stride)).saturating_add(used);
                let Some(pad) = data.get(at..at.saturating_add(stride - used)) else { break };
                facts.unaccounted += pad.iter().filter(|&&b| b != 0).count() as u64;
            }
        }
    }
    if !pixels {
        return Ok(Raw { facts, pixels: None });
    }
    let mut scaler = Downscaler::new(facts.width, facts.height, limits.max_out_side)?;
    let mut damaged = false;
    let mut row_rgba = vec![0u8; w * 4];
    if rle {
        // Decode into an index buffer first: the escapes can jump around.
        let mut idx = vec![0u8; w * h];
        let walk = rle_walk(pixel_data, compression == BI_RLE8, w, h, |x, y, v| {
            if let Some(s) = idx.get_mut(y * w + x) {
                *s = v;
            }
        });
        damaged |= !walk.finished;
        let rows: Vec<usize> = if top_down { (0..h).collect() } else { (0..h).rev().collect() };
        for y in rows {
            for (xx, px) in row_rgba.chunks_exact_mut(4).enumerate() {
                let i = usize::from(idx.get(y * w + xx).copied().unwrap_or(0));
                px.copy_from_slice(&palette.get(i).copied().unwrap_or([0, 0, 0, 255]));
            }
            scaler.push(&row_rgba);
        }
    } else {
        let (r, g, b, a) = (
            Channel::new(masks[0]),
            Channel::new(masks[1]),
            Channel::new(masks[2]),
            Channel::new(masks[3]),
        );
        for out_y in 0..h {
            // Stored order is bottom-up unless the height was negative.
            let stored = if top_down { out_y } else { h - 1 - out_y };
            let Some(line) = pixel_data
                .get(stored * stride..stored * stride + stride)
                .or_else(|| pixel_data.get(stored * stride..).filter(|l| !l.is_empty()))
            else {
                damaged = true;
                row_rgba.iter_mut().for_each(|v| *v = 0);
                scaler.push(&row_rgba);
                continue;
            };
            if line.len() < (w * to_usize(bpp)).div_ceil(8) {
                damaged = true;
            }
            for (x, px) in row_rgba.chunks_exact_mut(4).enumerate() {
                let rgba = match bpp {
                    1 | 4 | 8 => {
                        let per = 8 / to_usize(bpp);
                        let byte = line.get(x / per).copied().unwrap_or(0);
                        let shift = 8 - to_usize(bpp) * (x % per + 1);
                        let i = usize::try_from((u32::from(byte) >> shift) & ((1u32 << bpp) - 1))
                            .unwrap_or(0);
                        palette.get(i).copied().unwrap_or([0, 0, 0, 255])
                    }
                    24 => {
                        let o = x * 3;
                        [
                            line.get(o + 2).copied().unwrap_or(0),
                            line.get(o + 1).copied().unwrap_or(0),
                            line.get(o).copied().unwrap_or(0),
                            255,
                        ]
                    }
                    16 => {
                        let v = u32::from(line.get(x * 2).copied().unwrap_or(0))
                            | u32::from(line.get(x * 2 + 1).copied().unwrap_or(0)) << 8;
                        [r.get(v), g.get(v), b.get(v), if masks[3] != 0 { a.get(v) } else { 255 }]
                    }
                    _ => {
                        let o = x * 4;
                        let v = u32::from(line.get(o).copied().unwrap_or(0))
                            | u32::from(line.get(o + 1).copied().unwrap_or(0)) << 8
                            | u32::from(line.get(o + 2).copied().unwrap_or(0)) << 16
                            | u32::from(line.get(o + 3).copied().unwrap_or(0)) << 24;
                        [r.get(v), g.get(v), b.get(v), if masks[3] != 0 { a.get(v) } else { 255 }]
                    }
                };
                px.copy_from_slice(&rgba);
            }
            scaler.push(&row_rgba);
        }
        // 32 bits without an alpha mask: many writers leave the fourth byte zero.
        // (Handled above: no alpha mask means opaque.)
    }
    if damaged {
        facts.flags |= flag::DAMAGED;
    }
    Ok(Raw { facts, pixels: Some(scaler.finish()) })
}
