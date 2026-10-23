//! GIF: the first frame, drawn on a transparent canvas of the logical screen's size.
//!
//! Later frames are only counted (up to a limit): an animation is shown as its first picture and
//! says so. The LZW decoder produces at most as many pixels as the frame declares, however long
//! the compressed data claims to be.

use crate::bytes::{Cursor, to_usize};
use crate::scale::Downscaler;
use crate::{DecodeError, Facts, ImageFormat, Limits, Raw, flag};

/// Most frames counted.
const MAX_FRAMES: u32 = 1000;
/// Most blocks walked in one file.
const MAX_BLOCKS: usize = 1_000_000;

/// Three bytes as a colour.
fn rgb(p: &[u8]) -> Option<[u8; 3]> {
    match p {
        [r, g, b] => Some([*r, *g, *b]),
        _ => None,
    }
}

fn bad(msg: &str) -> DecodeError {
    DecodeError::Malformed(format!("GIF: {msg}"))
}

/// Reads the sub-blocks that follow, up to and including the terminator; returns their
/// concatenated payload (empty if `keep` is false).
fn sub_blocks<'a>(c: &mut Cursor<'a>, out: Option<&mut Vec<u8>>) -> Option<usize> {
    let mut total = 0usize;
    let mut out = out;
    loop {
        let n = usize::from(c.u8()?);
        if n == 0 {
            return Some(total);
        }
        let body = c.take(n)?;
        total += n;
        if let Some(o) = out.as_deref_mut() {
            o.extend_from_slice(body);
        }
    }
}

struct Frame {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    interlaced: bool,
    palette: Vec<[u8; 3]>,
    transparent: Option<u8>,
    min_code: u8,
    data: Vec<u8>,
}

/// LZW decoding of GIF image data into at most `want` palette indices.
fn lzw(min_code: u8, data: &[u8], want: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    if out.try_reserve_exact(want).is_err() {
        return (out, false);
    }
    let clear = 1usize << min_code;
    let eoi = clear + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut stack: Vec<u8> = Vec::with_capacity(4097);
    let mut size = u32::from(min_code) + 1;
    let mut next = eoi + 1;
    let mut prev: Option<usize> = None;
    let (mut acc, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut ok = true;
    while out.len() < want {
        while nbits < size {
            let Some(&b) = data.get(pos) else {
                return (out, false);
            };
            pos += 1;
            acc |= u32::from(b) << nbits;
            nbits += 8;
        }
        let code = to_usize(acc & ((1 << size) - 1));
        acc >>= size;
        nbits -= size;
        if code == clear {
            size = u32::from(min_code) + 1;
            next = eoi + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            return (out, false);
        }
        let first = match prev {
            None => {
                if code >= clear {
                    ok = false;
                    break;
                }
                out.push(u8::try_from(code).unwrap_or(0));
                prev = Some(code);
                continue;
            }
            Some(_) => code,
        };
        // Decode the string for `code` (or, for the KwKwK case, for `prev` plus its first byte).
        let p = prev.unwrap_or(0);
        stack.clear();
        let mut cur = if first < next {
            first
        } else if first == next && next < 4096 {
            p
        } else {
            ok = false;
            break;
        };
        for _ in 0..4097 {
            if cur < clear {
                stack.push(u8::try_from(cur).unwrap_or(0));
                break;
            }
            stack.push(suffix.get(cur).copied().unwrap_or(0));
            cur = usize::from(prefix.get(cur).copied().unwrap_or(0));
        }
        let first_byte = stack.last().copied().unwrap_or(0);
        if first == next {
            // KwKwK: the string is that of `prev` followed by its own first byte.
            out.extend(stack.iter().rev());
            out.push(first_byte);
        } else {
            out.extend(stack.iter().rev());
        }
        if next < 4096 {
            if let (Some(pf), Some(sf)) = (prefix.get_mut(next), suffix.get_mut(next)) {
                *pf = u16::try_from(p).unwrap_or(0);
                *sf = first_byte;
            }
            next += 1;
            if next == (1 << size) && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    out.truncate(want);
    (out, ok)
}

pub(crate) fn decode(data: &[u8], limits: &Limits, pixels: bool) -> Result<Raw, DecodeError> {
    let mut facts = Facts::new(ImageFormat::Gif);
    let mut c = Cursor::at(data, 6);
    let (Some(w), Some(h), Some(packed), Some(_bg), Some(_aspect)) =
        (c.u16_le(), c.u16_le(), c.u8(), c.u8(), c.u8())
    else {
        return Err(bad("the header is cut short"));
    };
    let global: Vec<[u8; 3]> = if packed & 0x80 != 0 {
        let n = 3usize << (usize::from(packed & 7) + 1);
        c.take(n)
            .ok_or_else(|| bad("the colour table is cut short"))?
            .chunks_exact(3)
            .filter_map(rgb)
            .collect()
    } else {
        Vec::new()
    };
    let mut canvas = (usize::from(w), usize::from(h));
    let mut transparent: Option<u8> = None;
    let mut frame: Option<Frame> = None;
    let mut frames = 0u32;
    let mut saw_trailer = false;
    let mut end = data.len();
    let mut damaged = false;
    for _ in 0..MAX_BLOCKS {
        let Some(kind) = c.u8() else {
            damaged = true;
            break;
        };
        match kind {
            0x3B => {
                saw_trailer = true;
                end = c.pos();
                break;
            }
            0x21 => {
                let Some(label) = c.u8() else {
                    damaged = true;
                    break;
                };
                match label {
                    0xF9 => {
                        let mut body = Vec::new();
                        if sub_blocks(&mut c, Some(&mut body)).is_none() {
                            damaged = true;
                            break;
                        }
                        transparent = if body.first().is_some_and(|f| f & 1 != 0) {
                            body.get(3).copied()
                        } else {
                            None
                        };
                    }
                    0xFE | 0xFF => {
                        let mut body = Vec::new();
                        if sub_blocks(&mut c, Some(&mut body)).is_none() {
                            damaged = true;
                            break;
                        }
                        facts.metadata_bytes += body.len() as u64;
                        if label == 0xFE {
                            facts.flags |= flag::COMMENT;
                        } else if body.starts_with(b"XMP DataXMP") {
                            facts.flags |= flag::XMP;
                        } else if body.starts_with(b"ICCRGBG1012") {
                            facts.flags |= flag::ICC_PROFILE;
                        } else {
                            // Animation extensions (NETSCAPE, ANIMEXTS) are not metadata.
                            facts.metadata_bytes -= body.len() as u64;
                        }
                    }
                    _ => {
                        if sub_blocks(&mut c, None).is_none() {
                            damaged = true;
                            break;
                        }
                    }
                }
            }
            0x2C => {
                let (Some(left), Some(top), Some(fw), Some(fh), Some(p)) =
                    (c.u16_le(), c.u16_le(), c.u16_le(), c.u16_le(), c.u8())
                else {
                    damaged = true;
                    break;
                };
                let local: Option<Vec<[u8; 3]>> = if p & 0x80 != 0 {
                    let n = 3usize << (usize::from(p & 7) + 1);
                    match c.take(n) {
                        Some(t) => Some(t.chunks_exact(3).filter_map(rgb).collect()),
                        None => {
                            damaged = true;
                            break;
                        }
                    }
                } else {
                    None
                };
                let Some(min_code) = c.u8() else {
                    damaged = true;
                    break;
                };
                frames += 1;
                let mut payload = Vec::new();
                let first = frames == 1;
                let walked =
                    sub_blocks(&mut c, if first && pixels { Some(&mut payload) } else { None });
                if walked.is_none() {
                    damaged = true;
                }
                if first {
                    if !(2..=11).contains(&min_code) {
                        return Err(bad("an impossible LZW code size"));
                    }
                    frame = Some(Frame {
                        left: usize::from(left),
                        top: usize::from(top),
                        width: usize::from(fw),
                        height: usize::from(fh),
                        interlaced: p & 0x40 != 0,
                        palette: local.unwrap_or_else(|| global.clone()),
                        transparent,
                        min_code,
                        data: payload,
                    });
                    if p & 0x40 != 0 {
                        facts.flags |= flag::INTERLACED;
                    }
                    if transparent.is_some() {
                        facts.flags |= flag::HAS_ALPHA;
                    }
                }
                if walked.is_none() || frames >= MAX_FRAMES {
                    break;
                }
            }
            _ => {
                damaged = true;
                break;
            }
        }
    }
    let Some(fr) = frame else {
        return Err(bad("there is no image"));
    };
    if canvas.0 == 0 || canvas.1 == 0 {
        canvas = (fr.left + fr.width, fr.top + fr.height);
    }
    if canvas.0 == 0 || canvas.1 == 0 {
        return Err(bad("the size is zero"));
    }
    let (cw, ch) =
        (u32::try_from(canvas.0).unwrap_or(u32::MAX), u32::try_from(canvas.1).unwrap_or(u32::MAX));
    if cw > limits.max_side || ch > limits.max_side {
        return Err(DecodeError::TooLarge(format!("GIF: {cw}×{ch} pixels is beyond the limit")));
    }
    let area = u64::from(cw) * u64::from(ch);
    let cap = if fr.interlaced { limits.max_buffered_pixels } else { limits.max_stream_pixels };
    if area > cap {
        return Err(DecodeError::TooLarge(format!(
            "GIF: {area} pixels is beyond the limit of {cap}"
        )));
    }
    facts.width = cw;
    facts.height = ch;
    facts.bits = 8;
    facts.flags |= flag::PALETTE;
    facts.frames = frames.max(1);
    if frames > 1 {
        facts.flags |= flag::ANIMATED;
    }
    if saw_trailer {
        facts.trailing = (data.len() - end) as u64;
    } else {
        damaged = true;
    }
    if !pixels {
        if damaged {
            facts.flags |= flag::DAMAGED;
        }
        return Ok(Raw { facts, pixels: None });
    }
    let mut scaler = Downscaler::new(cw, ch, limits.max_out_side)?;
    // Frame rectangle clipped to the canvas.
    let fw = fr.width.min(canvas.0.saturating_sub(fr.left));
    let fh = fr.height.min(canvas.1.saturating_sub(fr.top));
    let want = fr.width * fr.height;
    let (indices, complete) = lzw(fr.min_code, &fr.data, want);
    if !complete && indices.len() < want {
        damaged = true;
    }
    // Row order of the frame as stored.
    let mut order: Vec<usize> = Vec::new();
    if fr.interlaced {
        for (start, step) in [(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
            order.extend((start..fr.height).step_by(step));
        }
    } else {
        order.extend(0..fr.height);
    }
    // Where each stored row ends up: `stored_at[y]` is the index of the stored row for image row y.
    let mut stored_at = vec![usize::MAX; fr.height];
    for (stored, &y) in order.iter().enumerate() {
        if let Some(s) = stored_at.get_mut(y) {
            *s = stored;
        }
    }
    let blank = vec![0u8; canvas.0 * 4];
    let mut row = vec![0u8; canvas.0 * 4];
    for y in 0..canvas.1 {
        if y < fr.top || y >= fr.top + fh {
            scaler.push(&blank);
            continue;
        }
        row.iter_mut().for_each(|b| *b = 0);
        let stored = stored_at.get(y - fr.top).copied().unwrap_or(usize::MAX);
        if let Some(src) = indices.get(
            stored.saturating_mul(fr.width)
                ..(stored.saturating_mul(fr.width)).saturating_add(fr.width),
        ) && stored != usize::MAX
        {
            for (x, &i) in src.iter().take(fw).enumerate() {
                if Some(i) == fr.transparent {
                    continue;
                }
                let [r, g, b] = fr.palette.get(usize::from(i)).copied().unwrap_or([0, 0, 0]);
                if let Some(px) = row.get_mut((fr.left + x) * 4..(fr.left + x) * 4 + 4) {
                    px.copy_from_slice(&[r, g, b, 255]);
                }
            }
        }
        scaler.push(&row);
    }
    if damaged {
        facts.flags |= flag::DAMAGED;
    }
    Ok(Raw { facts, pixels: Some(scaler.finish()) })
}
