//! PNG (and the first image of an APNG).
//!
//! Chunks are walked once with every length checked; the compressed data is inflated through a
//! 32 KiB window, so a "1 × 1 megabyte" decompression bomb costs nothing beyond the bytes of
//! the rows the header promises. Rows are unfiltered and expanded to RGBA one at a time and
//! handed to the downscaler.

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

use crate::bytes::{Cursor, to_usize};
use crate::scale::Downscaler;
use crate::{DecodeError, Facts, ImageFormat, Limits, Raw, exif, flag};

/// Most chunks walked in one file.
const MAX_CHUNKS: usize = 200_000;
/// Window of the inflater (the DEFLATE dictionary size).
const WINDOW: usize = 32 * 1024;
/// Most bytes of inflated data past the last row that are counted before the inflater gives up
/// (a stream that keeps producing output after the picture is a channel, not a picture).
const TAIL_BUDGET: u64 = 1 << 20;
/// Largest inflated size for which the headers-only reading (`inspect`) inflates the data to find
/// where the zlib stream really ends. Beyond it the check is left to the full decoder.
const INSPECT_INFLATE_CAP: u64 = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    Gray,
    Rgb,
    Palette,
    GrayAlpha,
    Rgba,
}

impl Color {
    fn channels(self) -> usize {
        match self {
            Self::Gray | Self::Palette => 1,
            Self::GrayAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }
}

struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color: Color,
    interlaced: bool,
}

impl Header {
    fn bits_per_pixel(&self) -> usize {
        self.color.channels() * usize::from(self.depth)
    }

    /// Bytes of one row of `width` pixels (without the filter byte).
    fn stride(&self, width: usize) -> usize {
        (width * self.bits_per_pixel()).div_ceil(8)
    }

    /// Bytes per pixel for the filters (at least 1).
    fn bpp(&self) -> usize {
        self.bits_per_pixel().div_ceil(8).max(1)
    }
}

fn parse_header(d: &[u8]) -> Result<Header, DecodeError> {
    let bad = |m: &str| DecodeError::Malformed(format!("PNG: {m}"));
    let mut c = Cursor::new(d);
    let (Some(width), Some(height), Some(depth), Some(ct), Some(comp), Some(filt), Some(il)) =
        (c.u32_be(), c.u32_be(), c.u8(), c.u8(), c.u8(), c.u8(), c.u8())
    else {
        return Err(bad("the header is cut short"));
    };
    if width == 0 || height == 0 || width > 0x7FFF_FFFF || height > 0x7FFF_FFFF {
        return Err(bad("the size is zero or out of range"));
    }
    if comp != 0 || filt != 0 || il > 1 {
        return Err(DecodeError::Unsupported(
            "PNG: unknown compression, filter or interlace method".into(),
        ));
    }
    let color = match (ct, depth) {
        (0, 1 | 2 | 4 | 8 | 16) => Color::Gray,
        (2, 8 | 16) => Color::Rgb,
        (3, 1 | 2 | 4 | 8) => Color::Palette,
        (4, 8 | 16) => Color::GrayAlpha,
        (6, 8 | 16) => Color::Rgba,
        _ => return Err(bad("an impossible colour type and depth")),
    };
    Ok(Header { width, height, depth, color, interlaced: il == 1 })
}

/// Transparency information of the file.
#[derive(Default)]
struct Trns {
    /// Palette alphas.
    palette: Vec<u8>,
    /// Colour key for grey (one value) or RGB (three), as 16-bit samples.
    key: Option<[u16; 3]>,
}

struct Parsed<'a> {
    header: Header,
    facts: Facts,
    palette: Vec<[u8; 3]>,
    trns: Trns,
    idat: Vec<&'a [u8]>,
}

/// How many bytes of a chunk of `len` bytes the format, or a kind of metadata this crate
/// recognises, accounts for; `None` for a kind that is not known at all. The rest of a known
/// chunk and the whole of an unknown one cannot be explained and are reported as unaccounted.
fn explained(kind: &[u8; 4], len: usize, color: Option<Color>) -> Option<usize> {
    let up_to = |n: usize| Some(len.min(n));
    match kind {
        b"IHDR" => up_to(13),
        b"IEND" => Some(0),
        b"gAMA" | b"cICP" => up_to(4),
        b"cHRM" => up_to(32),
        b"sRGB" => up_to(1),
        b"sBIT" => up_to(4),
        b"bKGD" => up_to(6),
        b"pHYs" | b"oFFs" => up_to(9),
        b"tIME" => up_to(7),
        b"mDCV" => up_to(24),
        b"cLLI" => up_to(8),
        b"acTL" => up_to(8),
        b"fcTL" => up_to(26),
        b"hIST" => up_to(512),
        b"tRNS" => match color {
            Some(Color::Palette) => up_to(256),
            Some(Color::Gray) => up_to(2),
            Some(Color::Rgb) => up_to(6),
            _ => Some(0),
        },
        b"PLTE" => Some(if len.is_multiple_of(3) && len <= 768 { len } else { 0 }),
        b"IDAT" | b"fdAT" | b"iCCP" | b"eXIf" | b"tEXt" | b"zTXt" | b"iTXt" | b"sPLT" | b"pCAL"
        | b"sCAL" => Some(len),
        _ => None,
    }
}

fn walk_chunks<'a>(data: &'a [u8], limits: &Limits) -> Result<Parsed<'a>, DecodeError> {
    let mut c = Cursor::at(data, 8);
    let mut header: Option<Header> = None;
    let mut facts = Facts::new(ImageFormat::Png);
    let mut palette = Vec::new();
    let mut trns = Trns::default();
    let mut idat: Vec<&[u8]> = Vec::new();
    let mut seen_iend = false;
    let mut after_idat = false;
    let mut damaged = false;
    let mut end_pos = data.len();
    for _ in 0..MAX_CHUNKS {
        let Some(len) = c.u32_be() else { break };
        let Some(kind) = c.take(4) else { break };
        let kind: [u8; 4] = [
            kind.first().copied().unwrap_or(0),
            kind.get(1).copied().unwrap_or(0),
            kind.get(2).copied().unwrap_or(0),
            kind.get(3).copied().unwrap_or(0),
        ];
        let len = to_usize(len);
        let Some(body) = c.take(len) else {
            // A cut-off file: whatever image data is there is still shown.
            if &kind == b"IDAT" && header.is_some() && !after_idat {
                idat.push(c.rest());
            }
            damaged = true;
            break;
        };
        let crc_ok = c.u32_be().is_some_and(|crc| {
            let mut h = arca_digest::crc32::Crc32::new();
            h.update(&kind);
            h.update(body);
            h.finalize() == crc
        });
        if !crc_ok {
            damaged = true;
        }
        let known = match &kind {
            // A second header is not one the format defines.
            b"IHDR" if header.is_some() => None,
            _ => explained(&kind, len, header.as_ref().map(|h| h.color)),
        };
        facts.unaccounted += match known {
            Some(n) => len.saturating_sub(n),
            None => len.saturating_add(12),
        } as u64;
        match &kind {
            b"IHDR" if header.is_none() => {
                let h = parse_header(body)?;
                facts.width = h.width;
                facts.height = h.height;
                facts.bits = h.depth;
                if h.interlaced {
                    facts.flags |= flag::INTERLACED;
                }
                match h.color {
                    Color::Gray | Color::GrayAlpha => facts.flags |= flag::GRAYSCALE,
                    Color::Palette => facts.flags |= flag::PALETTE,
                    _ => {}
                }
                if matches!(h.color, Color::GrayAlpha | Color::Rgba) {
                    facts.flags |= flag::HAS_ALPHA;
                }
                header = Some(h);
            }
            _ if header.is_none() => {
                return Err(DecodeError::Malformed("PNG: the header chunk is missing".into()));
            }
            b"PLTE" if body.len() % 3 == 0 && body.len() <= 768 => {
                palette = body
                    .chunks_exact(3)
                    .filter_map(|p| match p {
                        [r, g, b] => Some([*r, *g, *b]),
                        _ => None,
                    })
                    .collect();
            }
            b"tRNS" => {
                let color = header.as_ref().map(|h| h.color);
                match color {
                    Some(Color::Palette) => trns.palette = body.to_vec(),
                    Some(Color::Gray) => {
                        let mut r = Cursor::new(body);
                        trns.key = r.u16_be().map(|g| [g, 0, 0]);
                    }
                    Some(Color::Rgb) => {
                        let mut r = Cursor::new(body);
                        trns.key = match (r.u16_be(), r.u16_be(), r.u16_be()) {
                            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
                            _ => None,
                        };
                    }
                    _ => {}
                }
                if trns.key.is_some() || !trns.palette.is_empty() {
                    facts.flags |= flag::HAS_ALPHA;
                }
            }
            b"IDAT" => {
                if after_idat && !idat.is_empty() {
                    // IDAT chunks must be consecutive; a later one is ignored, so what it
                    // carries is not picture data.
                    damaged = true;
                    facts.unaccounted += len as u64;
                } else {
                    idat.push(body);
                }
            }
            b"IEND" => {
                seen_iend = true;
                end_pos = c.pos();
                break;
            }
            b"acTL" => {
                facts.flags |= flag::ANIMATED;
                let frames = Cursor::new(body).u32_be().unwrap_or(1);
                facts.frames = frames.clamp(1, 1000);
            }
            b"iCCP" => {
                facts.flags |= flag::ICC_PROFILE;
                facts.metadata_bytes += len as u64;
            }
            b"eXIf" => {
                facts.metadata_bytes += len as u64;
                if let Some(found) = exif::parse(body) {
                    exif::merge_into(&mut facts.exif, found);
                }
            }
            b"tEXt" | b"zTXt" | b"iTXt" => {
                facts.flags |= flag::TEXT_CHUNKS;
                facts.metadata_bytes += len as u64;
                if body.starts_with(b"XML:com.adobe.xmp") {
                    facts.flags |= flag::XMP;
                }
            }
            // Suggested palettes and calibration records: data nobody shows.
            b"sPLT" | b"pCAL" | b"sCAL" => facts.metadata_bytes += len as u64,
            _ => {}
        }
        if &kind != b"IDAT" && !idat.is_empty() {
            after_idat = true;
        }
    }
    let header =
        header.ok_or_else(|| DecodeError::Malformed("PNG: the header chunk is missing".into()))?;
    if !seen_iend {
        damaged = true;
    } else {
        facts.trailing = (data.len() - end_pos) as u64;
    }
    if damaged {
        facts.flags |= flag::DAMAGED;
    }
    if header.width > limits.max_side || header.height > limits.max_side {
        return Err(DecodeError::TooLarge(format!(
            "PNG: {}×{} pixels is beyond the limit of {} on a side",
            header.width, header.height, limits.max_side
        )));
    }
    let pixels = u64::from(header.width) * u64::from(header.height);
    let cap = if header.interlaced { limits.max_buffered_pixels } else { limits.max_stream_pixels };
    if pixels > cap {
        return Err(DecodeError::TooLarge(format!(
            "PNG: {pixels} pixels is beyond the limit of {cap}"
        )));
    }
    Ok(Parsed { header, facts, palette, trns, idat })
}

/// What a zlib stream holds after the last row of the picture.
struct Tail {
    /// Bytes that are not picture data: inflated output past the rows, and input past the end of
    /// the stream.
    unaccounted: u64,
    /// The stream ended properly (and, in strict mode, with the right Adler-32 checksum).
    clean: bool,
}

/// Streaming inflater over a zlib stream that is split into slices (the IDAT chunks). The chunks
/// are fed one after the other; they are never joined into one buffer.
struct Inflater<'a> {
    state: Box<DecompressorOxide>,
    window: Vec<u8>,
    pos: usize,
    ready: core::ops::Range<usize>,
    parts: &'a [&'a [u8]],
    part: usize,
    in_pos: usize,
    finished: bool,
    /// The stream ended with `Done` (and, in strict mode, a matching checksum).
    complete: bool,
    /// Verify the Adler-32 checksum (the viewer does not; the check of a sterile copy does).
    strict: bool,
}

impl<'a> Inflater<'a> {
    fn new(parts: &'a [&'a [u8]], strict: bool) -> Self {
        Self {
            state: Box::default(),
            window: vec![0; WINDOW],
            pos: 0,
            ready: 0..0,
            parts,
            part: 0,
            in_pos: 0,
            finished: false,
            complete: false,
            strict,
        }
    }

    /// Bytes of input that have not been consumed.
    fn unread(&self) -> u64 {
        let mut n = 0u64;
        for (i, p) in self.parts.iter().enumerate().skip(self.part) {
            let used = if i == self.part { self.in_pos } else { 0 };
            n += p.len().saturating_sub(used) as u64;
        }
        n
    }

    /// Runs the inflater once. Fresh output lands in `ready`; every call consumes input, writes
    /// output or finishes the stream, so a loop over it ends.
    fn step(&mut self) {
        while self.parts.get(self.part).is_some_and(|p| self.in_pos >= p.len()) {
            self.part += 1;
            self.in_pos = 0;
        }
        let rest = self.parts.get(self.part).and_then(|p| p.get(self.in_pos..)).unwrap_or(&[]);
        let more = self.parts.get(self.part + 1..).is_some_and(|r| r.iter().any(|p| !p.is_empty()));
        let mut flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER;
        if !self.strict {
            flags |= inflate_flags::TINFL_FLAG_IGNORE_ADLER32;
        }
        if more {
            flags |= inflate_flags::TINFL_FLAG_HAS_MORE_INPUT;
        }
        let (status, used, wrote) =
            decompress(&mut self.state, rest, &mut self.window, self.pos, flags);
        self.in_pos += used;
        self.ready = self.pos..self.pos + wrote;
        self.pos = (self.pos + wrote) & (WINDOW - 1);
        match status {
            TINFLStatus::Done => {
                self.finished = true;
                self.complete = true;
            }
            TINFLStatus::HasMoreOutput => {}
            TINFLStatus::NeedsMoreInput if more => {}
            _ => self.finished = true,
        }
        if used == 0 && wrote == 0 {
            self.finished = true;
        }
    }

    /// Makes sure there is output to read, or reports that the stream ended or is damaged first.
    fn fill_ready(&mut self) -> Result<(), ()> {
        while self.ready.is_empty() {
            if self.finished {
                return Err(());
            }
            self.step();
        }
        Ok(())
    }

    /// Fills `dst` completely, or reports that the stream ended or is damaged first.
    fn read_exact(&mut self, dst: &mut [u8]) -> Result<(), ()> {
        let mut filled = 0;
        while filled < dst.len() {
            self.fill_ready()?;
            let avail = self.window.get(self.ready.clone()).unwrap_or(&[]);
            let n = avail.len().min(dst.len() - filled);
            if let (Some(from), Some(to)) = (avail.get(..n), dst.get_mut(filled..filled + n)) {
                to.copy_from_slice(from);
            }
            self.ready.start += n;
            filled += n;
        }
        Ok(())
    }

    /// Skips `n` bytes of output.
    fn skip(&mut self, mut n: u64) -> Result<(), ()> {
        while n > 0 {
            self.fill_ready()?;
            let take = u64::try_from(self.ready.len()).unwrap_or(u64::MAX).min(n);
            self.ready.start += usize::try_from(take).unwrap_or(0);
            n -= take;
        }
        Ok(())
    }

    /// After the last row: drains the stream (up to [`TAIL_BUDGET`]) and counts what is left.
    fn tail(&mut self) -> Tail {
        let mut extra = self.ready.len() as u64;
        self.ready = 0..0;
        while !self.finished && extra <= TAIL_BUDGET {
            self.step();
            extra += self.ready.len() as u64;
            self.ready = 0..0;
        }
        Tail { unaccounted: extra + self.unread(), clean: self.complete }
    }
}

/// Reverses the row filter in place. `prev` is the unfiltered previous row (zeros for the first).
fn unfilter(filter: u8, bpp: usize, cur: &mut [u8], prev: &[u8]) -> bool {
    match filter {
        0 => {}
        1 => {
            let mut left = [0u8; 8];
            for px in cur.chunks_mut(bpp) {
                for (c, l) in px.iter_mut().zip(left.iter()) {
                    *c = c.wrapping_add(*l);
                }
                for (l, c) in left.iter_mut().zip(px.iter()) {
                    *l = *c;
                }
            }
        }
        2 => {
            for (c, p) in cur.iter_mut().zip(prev.iter()) {
                *c = c.wrapping_add(*p);
            }
        }
        3 => {
            let mut left = [0u8; 8];
            for (px, up) in cur.chunks_mut(bpp).zip(prev.chunks(bpp)) {
                for ((c, l), u) in px.iter_mut().zip(left.iter()).zip(up.iter()) {
                    let avg = (u16::from(*l) + u16::from(*u)) / 2;
                    *c = c.wrapping_add(u8::try_from(avg).unwrap_or(255));
                }
                for (l, c) in left.iter_mut().zip(px.iter()) {
                    *l = *c;
                }
            }
        }
        4 => {
            let mut left = [0u8; 8];
            let mut upleft = [0u8; 8];
            for (px, up) in cur.chunks_mut(bpp).zip(prev.chunks(bpp)) {
                for (i, c) in px.iter_mut().enumerate() {
                    let a = i16::from(left.get(i).copied().unwrap_or(0));
                    let b = i16::from(up.get(i).copied().unwrap_or(0));
                    let cc = i16::from(upleft.get(i).copied().unwrap_or(0));
                    let p = a + b - cc;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - cc).abs());
                    let pred = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        cc
                    };
                    *c = c.wrapping_add(u8::try_from(pred).unwrap_or(0));
                }
                for (l, c) in left.iter_mut().zip(px.iter()) {
                    *l = *c;
                }
                for (l, c) in upleft.iter_mut().zip(up.iter()) {
                    *l = *c;
                }
            }
        }
        _ => return false,
    }
    true
}

/// Everything needed to turn the samples of a row into RGBA.
struct Expander<'a> {
    header: &'a Header,
    palette: &'a [[u8; 3]],
    trns: &'a Trns,
}

impl Expander<'_> {
    /// The `i`-th sample of `line` (`depth` bits), as its integer value.
    fn sample(&self, line: &[u8], i: usize) -> u16 {
        match self.header.depth {
            8 => u16::from(line.get(i).copied().unwrap_or(0)),
            16 => {
                u16::from(line.get(i * 2).copied().unwrap_or(0)) << 8
                    | u16::from(line.get(i * 2 + 1).copied().unwrap_or(0))
            }
            d => {
                let d = usize::from(d);
                let bit = i * d;
                let byte = line.get(bit / 8).copied().unwrap_or(0);
                let shift = 8 - d - (bit % 8);
                u16::from((byte >> shift) & ((1u8 << d) - 1))
            }
        }
    }

    /// Scales an integer sample of `depth` bits to 8 bits.
    fn to8(&self, v: u16) -> u8 {
        match self.header.depth {
            8 => u8::try_from(v).unwrap_or(255),
            16 => u8::try_from(v >> 8).unwrap_or(255),
            d => {
                let max = (1u32 << d) - 1;
                u8::try_from(u32::from(v) * 255 / max).unwrap_or(255)
            }
        }
    }

    /// Expands `width` pixels of `line` into `out` (RGBA).
    fn expand(&self, line: &[u8], width: usize, out: &mut [u8]) {
        for (x, px) in out.chunks_exact_mut(4).take(width).enumerate() {
            let (r, g, b, a) = match self.header.color {
                Color::Gray => {
                    let v = self.sample(line, x);
                    let g = self.to8(v);
                    let a = if self.trns.key.is_some_and(|k| k[0] == v) { 0 } else { 255 };
                    (g, g, g, a)
                }
                Color::GrayAlpha => {
                    let g = self.to8(self.sample(line, x * 2));
                    (g, g, g, self.to8(self.sample(line, x * 2 + 1)))
                }
                Color::Rgb => {
                    let (r, g, b) = (
                        self.sample(line, x * 3),
                        self.sample(line, x * 3 + 1),
                        self.sample(line, x * 3 + 2),
                    );
                    let a = if self.trns.key == Some([r, g, b]) { 0 } else { 255 };
                    (self.to8(r), self.to8(g), self.to8(b), a)
                }
                Color::Rgba => (
                    self.to8(self.sample(line, x * 4)),
                    self.to8(self.sample(line, x * 4 + 1)),
                    self.to8(self.sample(line, x * 4 + 2)),
                    self.to8(self.sample(line, x * 4 + 3)),
                ),
                Color::Palette => {
                    let i = usize::from(self.sample(line, x));
                    let [r, g, b] = self.palette.get(i).copied().unwrap_or([0, 0, 0]);
                    (r, g, b, self.trns.palette.get(i).copied().unwrap_or(255))
                }
            };
            if let [pr, pg, pb, pa] = px {
                (*pr, *pg, *pb, *pa) = (r, g, b, a);
            }
        }
    }
}

/// Adam7 passes: (x start, y start, x step, y step).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Bytes of inflated data the header promises: the filter byte and the packed samples of every
/// row of every pass.
fn raw_size(h: &Header) -> u64 {
    let (w, ht) = (to_usize(h.width), to_usize(h.height));
    if !h.interlaced {
        return (1 + h.stride(w)) as u64 * ht as u64;
    }
    ADAM7
        .iter()
        .map(|&(x0, y0, dx, dy)| {
            let pw = w.saturating_sub(x0).div_ceil(dx);
            let ph = ht.saturating_sub(y0).div_ceil(dy);
            if pw == 0 || ph == 0 { 0 } else { (1 + h.stride(pw)) as u64 * ph as u64 }
        })
        .sum()
}

/// The headers-only reading: inflates the picture data without keeping it, to learn where the
/// zlib stream really ends and what trails it. (Only for pictures whose inflated size is within
/// [`INSPECT_INFLATE_CAP`]; a larger one is checked when it is decoded.)
fn probe_stream(header: &Header, idat: &[&[u8]], facts: &mut Facts) {
    let raw = raw_size(header);
    if idat.is_empty() || raw > INSPECT_INFLATE_CAP {
        return;
    }
    let mut inflater = Inflater::new(idat, false);
    if inflater.skip(raw).is_err() {
        facts.flags |= flag::DAMAGED;
        return;
    }
    facts.unaccounted += inflater.tail().unaccounted;
}

pub(crate) fn decode(
    data: &[u8],
    limits: &Limits,
    pixels: bool,
    strict: bool,
) -> Result<Raw, DecodeError> {
    let Parsed { header, mut facts, palette, trns, idat } = walk_chunks(data, limits)?;
    if !pixels {
        probe_stream(&header, &idat, &mut facts);
        return Ok(Raw { facts, pixels: None });
    }
    if idat.is_empty() {
        return Err(DecodeError::Malformed("PNG: there is no image data".into()));
    }
    if header.color == Color::Palette && palette.is_empty() {
        return Err(DecodeError::Malformed("PNG: a palette image without a palette".into()));
    }
    let (w, h) = (to_usize(header.width), to_usize(header.height));
    let mut scaler = Downscaler::new(header.width, header.height, limits.max_out_side)?;
    let mut inflater = Inflater::new(&idat, strict);
    let ex = Expander { header: &header, palette: &palette, trns: &trns };
    let bpp = header.bpp();
    let mut ok = true;

    if header.interlaced {
        let mut full = Vec::new();
        full.try_reserve_exact(w * h * 4)
            .map_err(|_| DecodeError::TooLarge("not enough memory for the image".into()))?;
        full.resize(w * h * 4, 0);
        'passes: for (x0, y0, dx, dy) in ADAM7 {
            let pw = w.saturating_sub(x0).div_ceil(dx);
            let ph = h.saturating_sub(y0).div_ceil(dy);
            if pw == 0 || ph == 0 {
                continue;
            }
            let stride = header.stride(pw);
            let mut prev = vec![0u8; stride];
            let mut cur = vec![0u8; stride];
            let mut rgba = vec![0u8; pw * 4];
            for py in 0..ph {
                let mut f = [0u8; 1];
                if inflater.read_exact(&mut f).is_err() || inflater.read_exact(&mut cur).is_err() {
                    ok = false;
                    break 'passes;
                }
                if !unfilter(f[0], bpp, &mut cur, &prev) {
                    ok = false;
                    break 'passes;
                }
                ex.expand(&cur, pw, &mut rgba);
                let y = y0 + py * dy;
                for (px, src) in rgba.chunks_exact(4).enumerate() {
                    let x = x0 + px * dx;
                    let at = (y * w + x) * 4;
                    if let Some(dst) = full.get_mut(at..at + 4) {
                        dst.copy_from_slice(src);
                    }
                }
                core::mem::swap(&mut prev, &mut cur);
            }
        }
        for row in full.chunks_exact(w * 4) {
            scaler.push(row);
        }
    } else {
        let stride = header.stride(w);
        let mut prev = vec![0u8; stride];
        let mut cur = vec![0u8; stride];
        let mut rgba = vec![0u8; w * 4];
        for _ in 0..h {
            let mut f = [0u8; 1];
            if inflater.read_exact(&mut f).is_err() || inflater.read_exact(&mut cur).is_err() {
                ok = false;
                break;
            }
            if !unfilter(f[0], bpp, &mut cur, &prev) {
                ok = false;
                break;
            }
            ex.expand(&cur, w, &mut rgba);
            scaler.push(&rgba);
            core::mem::swap(&mut prev, &mut cur);
        }
    }
    if ok {
        let tail = inflater.tail();
        facts.unaccounted += tail.unaccounted;
        if strict && !tail.clean {
            ok = false;
        }
    }
    if !ok {
        facts.flags |= flag::DAMAGED;
    }
    Ok(Raw { facts, pixels: Some(scaler.finish()) })
}
