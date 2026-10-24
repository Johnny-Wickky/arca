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
/// Largest frame (in pixels) whose LZW data the headers-only reading runs through to find where
/// it ends. A larger one is checked when it is decoded.
const INSPECT_LZW_CAP: u64 = 32_000_000;

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

/// The result of reading the LZW data of a frame.
struct Lzw {
    /// The palette indices (empty when only the extent of the data was asked for).
    out: Vec<u8>,
    /// Fewer than the wanted number of indices could be read.
    short: bool,
    /// Bytes of the data consumed, up to and including the end-of-information code (all of it if
    /// that code was not found).
    used: usize,
}

/// LZW decoding of GIF image data into at most `want` palette indices; with `store` false the
/// indices are only counted, which is all it takes to learn where the data ends.
fn lzw(min_code: u8, data: &[u8], want: usize, store: bool) -> Lzw {
    let mut out = Vec::new();
    if store && out.try_reserve_exact(want).is_err() {
        return Lzw { out, short: true, used: data.len() };
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
    let mut produced = 0usize;
    // The next code, or `None` when the data has run out.
    macro_rules! code {
        () => {{
            let mut got = true;
            while nbits < size {
                let Some(&b) = data.get(pos) else {
                    got = false;
                    break;
                };
                pos += 1;
                acc |= u32::from(b) << nbits;
                nbits += 8;
            }
            if got {
                let c = to_usize(acc & ((1 << size) - 1));
                acc >>= size;
                nbits -= size;
                Some(c)
            } else {
                None
            }
        }};
    }
    // Where the data ends if the code just read was its last: whole unread bytes in the
    // accumulator are given back.
    macro_rules! consumed {
        () => {
            pos - (nbits / 8) as usize
        };
    }
    while produced < want {
        let Some(code) = code!() else {
            return Lzw { out, short: true, used: data.len() };
        };
        if code == clear {
            size = u32::from(min_code) + 1;
            next = eoi + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            return Lzw { out, short: true, used: consumed!() };
        }
        let Some(p) = prev else {
            if code >= clear {
                return Lzw { out, short: true, used: data.len() };
            }
            if store {
                out.push(u8::try_from(code).unwrap_or(0));
            }
            produced += 1;
            prev = Some(code);
            continue;
        };
        // Decode the string for `code` (or, for the KwKwK case, for `prev` plus its first byte).
        stack.clear();
        let mut cur = if code < next {
            code
        } else if code == next && next < 4096 {
            p
        } else {
            return Lzw { out, short: true, used: data.len() };
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
        // For KwKwK the string is that of `prev` followed by its own first byte.
        let kwkwk = code == next;
        if store {
            out.extend(stack.iter().rev());
            if kwkwk {
                out.push(first_byte);
            }
        }
        produced += stack.len() + usize::from(kwkwk);
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
    // All pixels are there. A well-formed stream ends with the end-of-information code (maybe
    // after a clear code); what follows it is not part of the picture.
    for _ in 0..4 {
        let before = consumed!();
        match code!() {
            Some(c) if c == clear => size = u32::from(min_code) + 1,
            Some(c) if c == eoi => return Lzw { out, short: false, used: consumed!() },
            Some(_) => return Lzw { out, short: false, used: before },
            None => break,
        }
    }
    Lzw { out, short: false, used: data.len() }
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
    let mut last_round = false;
    for round in 0..MAX_BLOCKS {
        last_round = round + 1 == MAX_BLOCKS;
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
                        // The graphic control block is four bytes: flags, delay, colour index.
                        facts.unaccounted += body.len().saturating_sub(4) as u64;
                    }
                    0xFE | 0xFF => {
                        let mut body = Vec::new();
                        if sub_blocks(&mut c, Some(&mut body)).is_none() {
                            damaged = true;
                            break;
                        }
                        let len = body.len() as u64;
                        if label == 0xFE {
                            facts.flags |= flag::COMMENT;
                            facts.metadata_bytes += len;
                        } else if body.starts_with(b"XMP DataXMP") {
                            facts.flags |= flag::XMP;
                            facts.metadata_bytes += len;
                        } else if body.starts_with(b"ICCRGBG1012") {
                            facts.flags |= flag::ICC_PROFILE;
                            facts.metadata_bytes += len;
                        } else if body.starts_with(b"NETSCAPE2.0")
                            || body.starts_with(b"ANIMEXTS1.0")
                        {
                            // The animation extensions are the loop count: an 11 byte name and
                            // three bytes of data.
                            facts.unaccounted += len.saturating_sub(14);
                        } else {
                            // An application the format has no use for here.
                            facts.unaccounted += len;
                        }
                    }
                    _ => {
                        // Plain text, and any extension this decoder does not know: nothing in
                        // it is shown, and nothing in it can be explained.
                        let Some(total) = sub_blocks(&mut c, None) else {
                            damaged = true;
                            break;
                        };
                        facts.unaccounted += total as u64;
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
                frames = frames.saturating_add(1);
                let mut payload = Vec::new();
                let first = frames == 1;
                // Only the first frame is read; the others are walked to find where they end.
                let walked = sub_blocks(&mut c, if first { Some(&mut payload) } else { None });
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
                if walked.is_none() {
                    break;
                }
            }
            _ => {
                damaged = true;
                break;
            }
        }
    }
    if last_round && !saw_trailer {
        // A file of nothing but blocks: the walk was cut off, and what is left is unexamined.
        facts.unaccounted += data.len().saturating_sub(c.pos()) as u64;
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
    // The frame is checked on its own: it may be far larger than the canvas it is drawn on, and
    // its indices are held (or at least counted) whatever part of them shows.
    if fr.width > to_usize(limits.max_side) || fr.height > to_usize(limits.max_side) {
        return Err(DecodeError::TooLarge(format!(
            "GIF: a frame of {}×{} pixels is beyond the limit",
            fr.width, fr.height
        )));
    }
    let frame_area = fr.width as u64 * fr.height as u64;
    if frame_area > cap {
        return Err(DecodeError::TooLarge(format!(
            "GIF: a frame of {frame_area} pixels is beyond the limit of {cap}"
        )));
    }
    facts.width = cw;
    facts.height = ch;
    facts.bits = 8;
    facts.flags |= flag::PALETTE;
    facts.frames = frames.clamp(1, MAX_FRAMES);
    if frames > 1 {
        facts.flags |= flag::ANIMATED;
    }
    if saw_trailer {
        facts.trailing = (data.len() - end) as u64;
    } else {
        damaged = true;
    }
    let want = fr.width * fr.height;
    if !pixels {
        // Only the extent of the data: where the LZW stream ends, and what follows it.
        if frame_area <= INSPECT_LZW_CAP {
            let read = lzw(fr.min_code, &fr.data, want, false);
            damaged |= read.short;
            facts.unaccounted += fr.data.len().saturating_sub(read.used) as u64;
        }
        if damaged {
            facts.flags |= flag::DAMAGED;
        }
        return Ok(Raw { facts, pixels: None });
    }
    let mut scaler = Downscaler::new(cw, ch, limits.max_out_side)?;
    // Frame rectangle clipped to the canvas.
    let fw = fr.width.min(canvas.0.saturating_sub(fr.left));
    let fh = fr.height.min(canvas.1.saturating_sub(fr.top));
    let read = lzw(fr.min_code, &fr.data, want, true);
    damaged |= read.short;
    facts.unaccounted += fr.data.len().saturating_sub(read.used) as u64;
    let indices = read.out;
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
