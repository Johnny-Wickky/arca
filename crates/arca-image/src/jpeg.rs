//! JPEG: baseline, extended sequential and progressive Huffman coding, 8 bit, one to four
//! components.
//!
//! The decoder is built for a hostile input: every marker length is checked, every table is
//! validated before use, the entropy reader feeds zeros (and gives up after a bounded number of
//! them) instead of reading past a marker, and the memory it needs is computed from the header
//! and compared with the limits before anything is allocated.
//!
//! A baseline image with all components in one scan is decoded one MCU row at a time and handed
//! to the downscaler straight away. Progressive images (and sequential ones split into several
//! scans) keep their DCT coefficients, which is why they have a lower pixel limit.

use crate::bytes::Cursor;
use crate::scale::Downscaler;
use crate::{DecodeError, Facts, ImageFormat, Limits, Raw, exif, flag};

/// Position in the 8×8 block of the `k`-th coefficient in zigzag order.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Most zero bytes the entropy reader feeds after the end of its data before it gives up.
const MAX_PAD_BYTES: u32 = 4096;

fn natural(k: usize) -> usize {
    usize::from(ZIGZAG.get(k).copied().unwrap_or(63))
}

fn bad(msg: &str) -> DecodeError {
    DecodeError::Malformed(format!("JPEG: {msg}"))
}

// ---- Huffman tables ----------------------------------------------------------------------

const FAST_BITS: u32 = 9;

struct Huff {
    /// `(length << 8) | symbol` for codes of at most [`FAST_BITS`] bits, else 0.
    fast: Vec<u16>,
    maxcode: [i32; 18],
    mincode: [i32; 18],
    valptr: [usize; 18],
    vals: Vec<u8>,
}

impl Huff {
    fn build(counts: &[u8], vals: &[u8]) -> Option<Self> {
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total == 0 || total > 256 || vals.len() < total {
            return None;
        }
        let mut h = Self {
            fast: vec![0; 1 << FAST_BITS],
            maxcode: [-1; 18],
            mincode: [0; 18],
            valptr: [0; 18],
            vals: vals.get(..total)?.to_vec(),
        };
        let (mut code, mut k) = (0i32, 0usize);
        for len in 1..=16usize {
            let n = i32::from(*counts.get(len - 1)?);
            if let Some(p) = h.valptr.get_mut(len) {
                *p = k;
            }
            if let Some(m) = h.mincode.get_mut(len) {
                *m = code;
            }
            for _ in 0..n {
                if code >= (1 << len) {
                    return None;
                }
                if len <= FAST_BITS as usize {
                    let first = usize::try_from(code).ok()? << (FAST_BITS as usize - len);
                    let span = 1usize << (FAST_BITS as usize - len);
                    let entry = u16::try_from(len).ok()? << 8 | u16::from(*vals.get(k)?);
                    for slot in h.fast.get_mut(first..first + span)? {
                        *slot = entry;
                    }
                }
                code += 1;
                k += 1;
            }
            if n > 0
                && let Some(m) = h.maxcode.get_mut(len)
            {
                *m = code - 1;
            }
            code <<= 1;
        }
        Some(h)
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u8> {
        bits.fill();
        let look = bits.peek(FAST_BITS);
        let entry = self.fast.get(look as usize).copied().unwrap_or(0);
        if entry != 0 {
            bits.consume(u32::from(entry >> 8));
            return u8::try_from(entry & 0xFF).ok();
        }
        let code = bits.peek(16);
        for len in (FAST_BITS as usize + 1)..=16 {
            let c = i32::try_from(code >> (16 - len)).ok()?;
            let max = *self.maxcode.get(len)?;
            if max >= 0 && c <= max && c >= *self.mincode.get(len)? {
                let idx =
                    *self.valptr.get(len)? + usize::try_from(c - *self.mincode.get(len)?).ok()?;
                bits.consume(u32::try_from(len).ok()?);
                return self.vals.get(idx).copied();
            }
        }
        None
    }
}

// ---- entropy-coded bit reader --------------------------------------------------------------

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
    marker: bool,
    pad: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Self { d, pos, acc: 0, n: 0, marker: false, pad: 0 }
    }

    fn feed_zero(&mut self) {
        self.acc |= 0;
        self.n += 8;
        self.pad += 1;
    }

    fn fill(&mut self) {
        while self.n <= 24 {
            match self.d.get(self.pos).copied() {
                Some(0xFF) => match self.d.get(self.pos + 1).copied() {
                    Some(0) => {
                        self.pos += 2;
                        self.acc |= 0xFF << (24 - self.n);
                        self.n += 8;
                    }
                    Some(0xFF) => self.pos += 1,
                    _ => {
                        self.marker = true;
                        self.feed_zero();
                    }
                },
                Some(b) => {
                    self.pos += 1;
                    self.acc |= u32::from(b) << (24 - self.n);
                    self.n += 8;
                }
                None => {
                    self.marker = true;
                    self.feed_zero();
                }
            }
        }
    }

    /// The reader has been fed more padding than any valid stream needs.
    fn overrun(&self) -> bool {
        self.pad > MAX_PAD_BYTES
    }

    fn peek(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.acc >> (32 - n) }
    }

    fn consume(&mut self, n: u32) {
        self.acc <<= n;
        self.n = self.n.saturating_sub(n);
    }

    /// `n` (at most 16) bits.
    fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.fill();
        let v = self.peek(n);
        self.consume(n);
        v
    }

    fn bit(&mut self) -> bool {
        self.get(1) != 0
    }

    /// Reads `s` bits and extends the sign (ITU T.81 F.2.2.1).
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = i32::try_from(self.get(s)).unwrap_or(0);
        if v < (1 << (s - 1)) { v - (1 << s) + 1 } else { v }
    }

    /// Moves past the next restart marker. `false` if there is none where it belongs.
    fn restart(&mut self) -> bool {
        self.acc = 0;
        self.n = 0;
        self.pad = 0;
        self.marker = false;
        let mut p = self.pos;
        // Skip entropy bytes that were not consumed (a damaged interval), up to a marker.
        for _ in 0..(1 << 20) {
            let (Some(&a), Some(&b)) = (self.d.get(p), self.d.get(p + 1)) else { return false };
            if a != 0xFF {
                p += 1;
                continue;
            }
            match b {
                0xD0..=0xD7 => {
                    self.pos = p + 2;
                    return true;
                }
                0 => p += 2,
                0xFF => p += 1,
                _ => return false,
            }
        }
        false
    }
}

// ---- frame -------------------------------------------------------------------------------

struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks per row of the (MCU-padded) component plane.
    bw: usize,
    /// Blocks per column of the (MCU-padded) component plane.
    bh: usize,
    /// Blocks per row / column of the component proper (what a non-interleaved scan covers).
    cw: usize,
    ch: usize,
    td: usize,
    ta: usize,
    pred: i32,
    coefs: Vec<i16>,
    plane: Vec<u8>,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    comps: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcus_x: usize,
    mcus_y: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// No scan seen yet.
    Pending,
    /// Rows are emitted as they are decoded.
    Streaming,
    /// Coefficients are collected; rows are emitted after the last scan.
    Buffered,
    /// The picture has been produced.
    Done,
}

struct Scan {
    /// Indices into [`Frame::comps`].
    comps: Vec<usize>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

struct Decoder<'a> {
    d: &'a [u8],
    limits: &'a Limits,
    pixels: bool,
    facts: Facts,
    frame: Option<Frame>,
    qt: [[u16; 64]; 4],
    dc: [Option<Huff>; 4],
    ac: [Option<Huff>; 4],
    restart_interval: usize,
    adobe: Option<u8>,
    mode: Mode,
    scaler: Option<Downscaler>,
    eobrun: u32,
    damaged: bool,
}

fn clamp8(v: i32) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
}

// ---- inverse DCT (the accurate integer algorithm of the IJG library, `jidctint`) ------------
//
// Computed in 64 bits: a hostile file can put any 16-bit value in a coefficient, and an overflow
// in release builds with `overflow-checks` would end the guest. The first pass clamps what goes
// in, so no product can leave the range of an `i64`.

const CONST_BITS: i64 = 13;
const PASS1_BITS: i64 = 2;
const F_0_298: i64 = 2446;
const F_0_390: i64 = 3196;
const F_0_541: i64 = 4433;
const F_0_765: i64 = 6270;
const F_0_899: i64 = 7373;
const F_1_175: i64 = 9633;
const F_1_501: i64 = 12299;
const F_1_847: i64 = 15137;
const F_1_961: i64 = 16069;
const F_2_053: i64 = 16819;
const F_2_562: i64 = 20995;
const F_3_072: i64 = 25172;

fn descale(x: i64, n: i64) -> i64 {
    (x + (1 << (n - 1))) >> n
}

/// One-dimensional inverse DCT of 8 values, scaled up by `2^CONST_BITS` (not yet descaled).
fn idct1d(i: [i64; 8]) -> [i64; 8] {
    let [i0, i1, i2, i3, i4, i5, i6, i7] = i;
    let z1 = (i2 + i6) * F_0_541;
    let t2 = z1 - i6 * F_1_847;
    let t3 = z1 + i2 * F_0_765;
    let t0 = (i0 + i4) << CONST_BITS;
    let t1 = (i0 - i4) << CONST_BITS;
    let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
    let (mut o0, mut o1, mut o2, mut o3) = (i7, i5, i3, i1);
    let (z1, z2, z3, z4) = (o0 + o3, o1 + o2, o0 + o2, o1 + o3);
    let z5 = (z3 + z4) * F_1_175;
    o0 *= F_0_298;
    o1 *= F_2_053;
    o2 *= F_3_072;
    o3 *= F_1_501;
    let (z1, z2) = (-z1 * F_0_899, -z2 * F_2_562);
    let (z3, z4) = (-z3 * F_1_961 + z5, -z4 * F_0_390 + z5);
    o0 += z1 + z3;
    o1 += z2 + z4;
    o2 += z2 + z3;
    o3 += z1 + z4;
    [t10 + o3, t11 + o2, t12 + o1, t13 + o0, t13 - o0, t12 - o1, t11 - o2, t10 - o3]
}

/// Dequantises `coefs` (natural order) and writes the 8×8 block of samples to `out`.
#[allow(
    clippy::indexing_slicing,
    reason = "every index below is `row * 8 + col` with row and col bounded by 8, into arrays of 64"
)]
fn idct(coefs: &[i16], q: &[u16; 64], out: &mut [u8; 64]) {
    let mut ws = [0i64; 64];
    let c = |i: usize| {
        (i64::from(coefs.get(i).copied().unwrap_or(0)) * i64::from(q[i])).clamp(-32_768, 32_767)
    };
    for col in 0..8 {
        let g = [
            c(col),
            c(col + 8),
            c(col + 16),
            c(col + 24),
            c(col + 32),
            c(col + 40),
            c(col + 48),
            c(col + 56),
        ];
        if g[1..].iter().all(|&v| v == 0) {
            let dc = g[0] << PASS1_BITS;
            for row in 0..8 {
                ws[row * 8 + col] = dc;
            }
            continue;
        }
        let o = idct1d(g);
        for row in 0..8 {
            ws[row * 8 + col] = descale(o[row], CONST_BITS - PASS1_BITS);
        }
    }
    for row in 0..8 {
        let r: [i64; 8] = [
            ws[row * 8],
            ws[row * 8 + 1],
            ws[row * 8 + 2],
            ws[row * 8 + 3],
            ws[row * 8 + 4],
            ws[row * 8 + 5],
            ws[row * 8 + 6],
            ws[row * 8 + 7],
        ];
        let o = idct1d(r);
        for col in 0..8 {
            let v = descale(o[col], CONST_BITS + PASS1_BITS + 3) + 128;
            out[row * 8 + col] = u8::try_from(v.clamp(0, 255)).unwrap_or(0);
        }
    }
}

// ---- decoding of blocks ------------------------------------------------------------------

/// Sequential (baseline) block. Writes raw coefficients in natural order.
fn decode_block_seq(
    bits: &mut Bits<'_>,
    dc: &Huff,
    ac: &Huff,
    pred: &mut i32,
    out: &mut [i16],
) -> bool {
    out.iter_mut().for_each(|c| *c = 0);
    let Some(t) = dc.decode(bits) else { return false };
    if t > 16 {
        return false;
    }
    *pred = pred.wrapping_add(bits.receive_extend(u32::from(t)));
    if let Some(c) = out.get_mut(0) {
        *c = i16::try_from((*pred).clamp(-32768, 32767)).unwrap_or(0);
    }
    let mut k = 1usize;
    while k < 64 {
        let Some(rs) = ac.decode(bits) else { return false };
        let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r == 15 {
                k += 16;
                continue;
            }
            break;
        }
        k += r;
        if k > 63 {
            return false;
        }
        let v = bits.receive_extend(s);
        if let Some(c) = out.get_mut(natural(k)) {
            *c = i16::try_from(v.clamp(-32768, 32767)).unwrap_or(0);
        }
        k += 1;
    }
    !bits.overrun()
}

impl Decoder<'_> {
    /// One block of a progressive scan.
    fn decode_block_prog(
        &mut self,
        bits: &mut Bits<'_>,
        scan: &Scan,
        ci: usize,
        at: usize,
    ) -> bool {
        let Some(frame) = self.frame.as_mut() else { return false };
        let Some(comp) = frame.comps.get_mut(ci) else { return false };
        let (td, ta) = (comp.td, comp.ta);
        let Some(block) = comp.coefs.get_mut(at..at + 64) else { return false };
        if scan.ss == 0 {
            // DC scan.
            if scan.ah == 0 {
                let Some(Some(dc)) = self.dc.get(td) else { return false };
                let Some(t) = dc.decode(bits) else { return false };
                if t > 16 {
                    return false;
                }
                comp.pred = comp.pred.wrapping_add(bits.receive_extend(u32::from(t)));
                if let Some(c) = block.get_mut(0) {
                    *c = i16::try_from((comp.pred << scan.al).clamp(-32768, 32767)).unwrap_or(0);
                }
            } else if bits.bit()
                && let Some(c) = block.get_mut(0)
            {
                *c |= 1i16 << scan.al;
            }
            return !bits.overrun();
        }
        let Some(Some(ac)) = self.ac.get(ta) else { return false };
        if scan.ah == 0 {
            // AC first pass.
            if self.eobrun > 0 {
                self.eobrun -= 1;
                return true;
            }
            let mut k = scan.ss;
            while k <= scan.se {
                let Some(rs) = ac.decode(bits) else { return false };
                let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
                if s != 0 {
                    k += r;
                    if k > 63 {
                        return false;
                    }
                    let v = bits.receive_extend(s) << scan.al;
                    if let Some(c) = block.get_mut(natural(k)) {
                        *c = i16::try_from(v.clamp(-32768, 32767)).unwrap_or(0);
                    }
                    k += 1;
                } else if r == 15 {
                    k += 16;
                } else {
                    self.eobrun = 1u32 << r;
                    if r > 0 {
                        self.eobrun += bits.get(u32::try_from(r).unwrap_or(0));
                    }
                    self.eobrun -= 1;
                    break;
                }
            }
            return !bits.overrun();
        }
        // AC refinement pass (ITU T.81 G.1.2.3).
        let p1 = 1i16 << scan.al;
        let m1 = -1i16 << scan.al;
        let mut k = scan.ss;
        let refine = |bits: &mut Bits<'_>, c: &mut i16| {
            if *c != 0 && bits.bit() && *c & p1 == 0 {
                *c = if *c >= 0 { c.wrapping_add(p1) } else { c.wrapping_add(m1) };
            }
        };
        if self.eobrun == 0 {
            while k <= scan.se {
                let Some(rs) = ac.decode(bits) else { return false };
                let (mut r, s) = (i32::from(rs >> 4), rs & 15);
                let mut value = 0i16;
                if s != 0 {
                    value = if bits.bit() { p1 } else { m1 };
                } else if r != 15 {
                    self.eobrun = 1u32 << r;
                    if r > 0 {
                        self.eobrun += bits.get(u32::try_from(r).unwrap_or(0));
                    }
                    break;
                }
                loop {
                    let Some(c) = block.get_mut(natural(k)) else { return false };
                    if *c != 0 {
                        refine(bits, c);
                    } else {
                        r -= 1;
                        if r < 0 {
                            break;
                        }
                    }
                    k += 1;
                    if k > scan.se {
                        break;
                    }
                }
                if value != 0
                    && k <= scan.se
                    && let Some(c) = block.get_mut(natural(k))
                {
                    *c = value;
                }
                k += 1;
            }
        }
        if self.eobrun > 0 {
            while k <= scan.se {
                if let Some(c) = block.get_mut(natural(k)) {
                    refine(bits, c);
                }
                k += 1;
            }
            self.eobrun -= 1;
        }
        !bits.overrun()
    }
}

// ---- the decoder -------------------------------------------------------------------------

impl<'a> Decoder<'a> {
    fn new(d: &'a [u8], limits: &'a Limits, pixels: bool) -> Self {
        Self {
            d,
            limits,
            pixels,
            facts: Facts::new(ImageFormat::Jpeg),
            frame: None,
            qt: [[1; 64]; 4],
            dc: [None, None, None, None],
            ac: [None, None, None, None],
            restart_interval: 0,
            adobe: None,
            mode: Mode::Pending,
            scaler: None,
            eobrun: 0,
            damaged: false,
        }
    }

    fn parse_sof(&mut self, seg: &[u8], progressive: bool) -> Result<(), DecodeError> {
        if self.frame.is_some() {
            return Err(bad("a second frame header"));
        }
        let mut c = Cursor::new(seg);
        let (Some(prec), Some(h), Some(w), Some(n)) = (c.u8(), c.u16_be(), c.u16_be(), c.u8())
        else {
            return Err(bad("the frame header is cut short"));
        };
        if prec != 8 {
            return Err(DecodeError::Unsupported(format!(
                "JPEG with {prec} bits per sample is not shown"
            )));
        }
        if w == 0 || h == 0 {
            return Err(bad("the size is zero or defined after the data"));
        }
        let (width, height) = (usize::from(w), usize::from(h));
        if u32::from(w) > self.limits.max_side || u32::from(h) > self.limits.max_side {
            return Err(DecodeError::TooLarge(format!("JPEG: {w}×{h} pixels is beyond the limit")));
        }
        if !matches!(n, 1 | 3 | 4) {
            return Err(DecodeError::Unsupported(format!("JPEG with {n} components is not shown")));
        }
        let mut comps = Vec::new();
        for _ in 0..n {
            let (Some(id), Some(hv), Some(tq)) = (c.u8(), c.u8(), c.u8()) else {
                return Err(bad("the frame header is cut short"));
            };
            let (hs, vs) = (usize::from(hv >> 4), usize::from(hv & 15));
            if !(1..=4).contains(&hs) || !(1..=4).contains(&vs) || tq > 3 {
                return Err(bad("an impossible sampling factor or table"));
            }
            comps.push(Component {
                id,
                h: hs,
                v: vs,
                tq: usize::from(tq),
                bw: 0,
                bh: 0,
                cw: 0,
                ch: 0,
                td: 0,
                ta: 0,
                pred: 0,
                coefs: Vec::new(),
                plane: Vec::new(),
            });
        }
        if comps.len() == 1 {
            // A single component is never subsampled: its MCU is one block.
            if let Some(c0) = comps.first_mut() {
                c0.h = 1;
                c0.v = 1;
            }
        }
        let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
        let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
        let mcus_x = width.div_ceil(8 * hmax);
        let mcus_y = height.div_ceil(8 * vmax);
        for comp in &mut comps {
            comp.bw = mcus_x * comp.h;
            comp.bh = mcus_y * comp.v;
            comp.cw = (width * comp.h).div_ceil(hmax).div_ceil(8);
            comp.ch = (height * comp.v).div_ceil(vmax).div_ceil(8);
        }
        self.facts.width = u32::from(w);
        self.facts.height = u32::from(h);
        self.facts.bits = 8;
        if progressive {
            self.facts.flags |= flag::INTERLACED;
        }
        match comps.len() {
            1 => self.facts.flags |= flag::GRAYSCALE,
            4 => self.facts.flags |= flag::CMYK,
            _ => {}
        }
        self.frame = Some(Frame { width, height, progressive, comps, hmax, vmax, mcus_x, mcus_y });
        Ok(())
    }

    fn parse_dht(&mut self, seg: &[u8]) -> Result<(), DecodeError> {
        let mut c = Cursor::new(seg);
        while c.remaining() > 0 {
            let tc_th = c.u8().ok_or_else(|| bad("a cut-short Huffman table"))?;
            let counts = c.take(16).ok_or_else(|| bad("a cut-short Huffman table"))?;
            let total: usize = counts.iter().map(|&n| usize::from(n)).sum();
            let vals = c.take(total).ok_or_else(|| bad("a cut-short Huffman table"))?;
            let (class, id) = (tc_th >> 4, usize::from(tc_th & 15));
            let table = Huff::build(counts, vals).ok_or_else(|| bad("an invalid Huffman table"))?;
            let slot = match class {
                0 => self.dc.get_mut(id),
                1 => self.ac.get_mut(id),
                _ => None,
            };
            *slot.ok_or_else(|| bad("a Huffman table with an impossible number"))? = Some(table);
        }
        Ok(())
    }

    fn parse_dqt(&mut self, seg: &[u8]) -> Result<(), DecodeError> {
        let mut c = Cursor::new(seg);
        while c.remaining() > 0 {
            let pq_tq = c.u8().ok_or_else(|| bad("a cut-short quantisation table"))?;
            let (wide, id) = (pq_tq >> 4 != 0, usize::from(pq_tq & 15));
            if id > 3 {
                return Err(bad("a quantisation table with an impossible number"));
            }
            for k in 0..64 {
                let v = if wide { c.u16_be() } else { c.u8().map(u16::from) }
                    .ok_or_else(|| bad("a cut-short quantisation table"))?;
                if let Some(slot) = self.qt.get_mut(id).and_then(|t| t.get_mut(natural(k))) {
                    *slot = v.max(1);
                }
            }
        }
        Ok(())
    }

    fn parse_app(&mut self, marker: u8, seg: &[u8]) {
        self.facts.metadata_bytes += seg.len() as u64;
        match marker {
            0xE1 if seg.starts_with(b"Exif\0\0") => {
                if self.facts.exif.is_none() {
                    self.facts.exif = seg.get(6..).and_then(exif::parse);
                }
            }
            0xE1 if seg.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => {
                self.facts.flags |= flag::XMP
            }
            0xE2 if seg.starts_with(b"ICC_PROFILE\0") => self.facts.flags |= flag::ICC_PROFILE,
            0xEE if seg.starts_with(b"Adobe") => {
                self.adobe = seg.get(11).copied();
                // The Adobe segment is part of the colour description, not metadata to report.
                self.facts.metadata_bytes -= seg.len() as u64;
            }
            0xE0 if seg.starts_with(b"JFIF\0") => self.facts.metadata_bytes -= seg.len() as u64,
            _ => {}
        }
    }

    /// Reads the scan header at `seg`; returns the scan description.
    fn parse_sos(&mut self, seg: &[u8]) -> Result<Scan, DecodeError> {
        let frame = self.frame.as_mut().ok_or_else(|| bad("a scan before the frame header"))?;
        let mut c = Cursor::new(seg);
        let ns = usize::from(c.u8().ok_or_else(|| bad("a cut-short scan header"))?);
        if ns == 0 || ns > 4 || ns > frame.comps.len() {
            return Err(bad("a scan with an impossible number of components"));
        }
        let mut comps = Vec::new();
        for _ in 0..ns {
            let (Some(cs), Some(t)) = (c.u8(), c.u8()) else {
                return Err(bad("a cut-short scan header"));
            };
            let idx = frame
                .comps
                .iter()
                .position(|k| k.id == cs)
                .ok_or_else(|| bad("a scan names a component the frame does not have"))?;
            if comps.contains(&idx) {
                return Err(bad("a component twice in one scan"));
            }
            if let Some(k) = frame.comps.get_mut(idx) {
                k.td = usize::from(t >> 4);
                k.ta = usize::from(t & 15);
                if k.td > 3 || k.ta > 3 {
                    return Err(bad("a Huffman table with an impossible number"));
                }
            }
            comps.push(idx);
        }
        let (Some(ss), Some(se), Some(a)) = (c.u8(), c.u8(), c.u8()) else {
            return Err(bad("a cut-short scan header"));
        };
        let scan = if frame.progressive {
            let (ss, se, ah, al) =
                (usize::from(ss), usize::from(se), u32::from(a >> 4), u32::from(a & 15));
            let dc_ok = ss == 0 && se == 0;
            let ac_ok = ss >= 1 && ss <= se && se <= 63 && comps.len() == 1;
            if !(dc_ok || ac_ok) || al > 13 || ah > 13 {
                return Err(bad("a progressive scan with impossible parameters"));
            }
            Scan { comps, ss, se, ah, al }
        } else {
            Scan { comps, ss: 0, se: 63, ah: 0, al: 0 }
        };
        Ok(scan)
    }

    /// Allocates what the mode needs.
    fn prepare(&mut self, mode: Mode) -> Result<(), DecodeError> {
        let limits = *self.limits;
        let frame = self.frame.as_mut().ok_or_else(|| bad("no frame"))?;
        let pixels = u64::try_from(frame.width * frame.height).unwrap_or(u64::MAX);
        let cap = if mode == Mode::Buffered {
            limits.max_buffered_pixels
        } else {
            limits.max_stream_pixels
        };
        if pixels > cap {
            return Err(DecodeError::TooLarge(format!(
                "JPEG: {pixels} pixels is beyond the limit of {cap}"
            )));
        }
        let oom = || DecodeError::TooLarge("not enough memory for the image".into());
        let mut coef_bytes = 0u64;
        for comp in &frame.comps {
            coef_bytes += (comp.bw * comp.bh * 64 * 2) as u64;
        }
        if mode == Mode::Buffered && coef_bytes > limits.max_buffered_pixels.saturating_mul(3) {
            return Err(DecodeError::TooLarge(format!(
                "JPEG: the coefficients of this image need {coef_bytes} bytes of memory"
            )));
        }
        for comp in &mut frame.comps {
            if mode == Mode::Buffered {
                let n = comp.bw * comp.bh * 64;
                comp.coefs.try_reserve_exact(n).map_err(|_| oom())?;
                comp.coefs.resize(n, 0);
            }
            let n = comp.bw * 8 * comp.v * 8;
            comp.plane.try_reserve_exact(n).map_err(|_| oom())?;
            comp.plane.resize(n, 0);
        }
        self.scaler = Some(Downscaler::new(
            u32::try_from(frame.width).unwrap_or(u32::MAX),
            u32::try_from(frame.height).unwrap_or(u32::MAX),
            limits.max_out_side,
        )?);
        self.mode = mode;
        Ok(())
    }

    /// Converts the samples of MCU row `my` (already in the component planes) to RGBA rows.
    fn emit_row(&mut self, my: usize) {
        let adobe = self.adobe;
        let (Some(frame), Some(scaler)) = (self.frame.as_ref(), self.scaler.as_mut()) else {
            return;
        };
        let rows_here = (frame.vmax * 8).min(frame.height.saturating_sub(my * frame.vmax * 8));
        let n = frame.comps.len();
        let ids: Vec<u8> = frame.comps.iter().map(|c| c.id).collect();
        let ycc = match n {
            3 => match adobe {
                Some(0) => false,
                Some(_) => true,
                None => ids != *b"RGB",
            },
            4 => adobe == Some(2),
            _ => false,
        };
        let xmaps: Vec<Vec<usize>> = frame
            .comps
            .iter()
            .map(|c| (0..frame.width).map(|x| x * c.h / frame.hmax).collect())
            .collect();
        let mut row = vec![0u8; frame.width * 4];
        for y in 0..rows_here {
            let mut lines: Vec<&[u8]> = Vec::with_capacity(n);
            for c in &frame.comps {
                let stride = c.bw * 8;
                let cy = y * c.v / frame.vmax;
                lines.push(c.plane.get(cy * stride..(cy + 1) * stride).unwrap_or(&[]));
            }
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let s = |i: usize| -> i32 {
                    let xm = xmaps.get(i).and_then(|m| m.get(x)).copied().unwrap_or(0);
                    i32::from(lines.get(i).and_then(|l| l.get(xm)).copied().unwrap_or(0))
                };
                let rgb = match n {
                    1 => {
                        let g = clamp8(s(0));
                        [g, g, g]
                    }
                    3 => {
                        if ycc {
                            ycc_to_rgb(s(0), s(1), s(2))
                        } else {
                            [clamp8(s(0)), clamp8(s(1)), clamp8(s(2))]
                        }
                    }
                    _ => {
                        let (c, m, yy, k) = if ycc {
                            let [r, g, b] = ycc_to_rgb(s(0), s(1), s(2));
                            (255 - i32::from(r), 255 - i32::from(g), 255 - i32::from(b), s(3))
                        } else {
                            (s(0), s(1), s(2), s(3))
                        };
                        if adobe.is_some() {
                            // Adobe stores CMYK inverted: 255 means "no ink".
                            [clamp8(c * k / 255), clamp8(m * k / 255), clamp8(yy * k / 255)]
                        } else {
                            [
                                clamp8((255 - c) * (255 - k) / 255),
                                clamp8((255 - m) * (255 - k) / 255),
                                clamp8((255 - yy) * (255 - k) / 255),
                            ]
                        }
                    }
                };
                if let [r, g, b, a] = px {
                    (*r, *g, *b, *a) = (rgb[0], rgb[1], rgb[2], 255);
                }
            }
            scaler.push(&row);
        }
    }

    /// Decodes the entropy-coded data of one scan that starts at `pos`.
    fn decode_scan(&mut self, scan: &Scan, pos: usize) -> Result<(), DecodeError> {
        let mut bits = Bits::new(self.d, pos);
        let streaming = self.mode == Mode::Streaming;
        let ri = self.restart_interval;
        for &ci in &scan.comps {
            if let Some(c) = self.frame.as_mut().and_then(|f| f.comps.get_mut(ci)) {
                c.pred = 0;
            }
        }
        self.eobrun = 0;
        let (progressive, mcus_x, mcus_y, ncomp_frame) = {
            let f = self.frame.as_ref().ok_or_else(|| bad("no frame"))?;
            (f.progressive, f.mcus_x, f.mcus_y, f.comps.len())
        };
        let mut unit = 0usize;
        // The quantisation tables only matter when blocks are turned into samples.
        let qts = self.qt;
        let mut tmp = [0i16; 64];
        let mut samples = [0u8; 64];
        let mut ok = true;

        // Decodes one block of component `ci` at block position (bx, by).
        macro_rules! block {
            ($ci:expr, $bx:expr, $by:expr) => {{
                let (ci, bx, by): (usize, usize, usize) = ($ci, $bx, $by);
                let bw = self.frame.as_ref().and_then(|f| f.comps.get(ci)).map_or(0, |c| c.bw);
                if streaming {
                    let (td, ta) = self
                        .frame
                        .as_ref()
                        .and_then(|f| f.comps.get(ci))
                        .map_or((0, 0), |c| (c.td, c.ta));
                    let mut pred =
                        self.frame.as_ref().and_then(|f| f.comps.get(ci)).map_or(0, |c| c.pred);
                    let good = match (self.dc.get(td), self.ac.get(ta)) {
                        (Some(Some(dc)), Some(Some(ac))) => {
                            decode_block_seq(&mut bits, dc, ac, &mut pred, &mut tmp)
                        }
                        _ => false,
                    };
                    if !good {
                        ok = false;
                    }
                    if let Some(c) = self.frame.as_mut().and_then(|f| f.comps.get_mut(ci)) {
                        c.pred = pred;
                        if good {
                            let q = qts.get(c.tq).copied().unwrap_or([1; 64]);
                            idct(&tmp, &q, &mut samples);
                            let stride = c.bw * 8;
                            // The block's row inside the MCU row.
                            let row0 = (by % c.v) * 8;
                            for (r, line) in samples.chunks_exact(8).enumerate() {
                                let at = (row0 + r) * stride + bx * 8;
                                if let Some(dst) = c.plane.get_mut(at..at + 8) {
                                    dst.copy_from_slice(line);
                                }
                            }
                        }
                    }
                } else if progressive {
                    if !self.decode_block_prog(&mut bits, scan, ci, (by * bw + bx) * 64) {
                        ok = false;
                    }
                } else {
                    let (td, ta) = self
                        .frame
                        .as_ref()
                        .and_then(|f| f.comps.get(ci))
                        .map_or((0, 0), |c| (c.td, c.ta));
                    let mut pred =
                        self.frame.as_ref().and_then(|f| f.comps.get(ci)).map_or(0, |c| c.pred);
                    let good = match (self.dc.get(td), self.ac.get(ta)) {
                        (Some(Some(dc)), Some(Some(ac))) => {
                            decode_block_seq(&mut bits, dc, ac, &mut pred, &mut tmp)
                        }
                        _ => false,
                    };
                    if let Some(c) = self.frame.as_mut().and_then(|f| f.comps.get_mut(ci)) {
                        c.pred = pred;
                        if good {
                            if let Some(dst) =
                                c.coefs.get_mut((by * bw + bx) * 64..(by * bw + bx) * 64 + 64)
                            {
                                dst.copy_from_slice(&tmp);
                            }
                        }
                    }
                    if !good {
                        ok = false;
                    }
                }
            }};
        }

        macro_rules! restart_check {
            () => {
                if ri > 0 && unit > 0 && unit % ri == 0 {
                    if !bits.restart() {
                        ok = false;
                    }
                    for &ci in &scan.comps {
                        if let Some(c) = self.frame.as_mut().and_then(|f| f.comps.get_mut(ci)) {
                            c.pred = 0;
                        }
                    }
                    self.eobrun = 0;
                }
            };
        }

        if scan.comps.len() == 1 && !(streaming && ncomp_frame == 1) {
            // A non-interleaved scan: the blocks of one component in raster order.
            let ci = scan.comps.first().copied().unwrap_or(0);
            let (cw, ch) =
                self.frame.as_ref().and_then(|f| f.comps.get(ci)).map_or((0, 0), |c| (c.cw, c.ch));
            'blocks: for by in 0..ch {
                for bx in 0..cw {
                    restart_check!();
                    block!(ci, bx, by);
                    unit += 1;
                    if !ok {
                        break 'blocks;
                    }
                }
            }
        } else {
            'mcus: for my in 0..mcus_y {
                for mx in 0..mcus_x {
                    restart_check!();
                    for &ci in &scan.comps {
                        let (h, v) = self
                            .frame
                            .as_ref()
                            .and_then(|f| f.comps.get(ci))
                            .map_or((1, 1), |c| (c.h, c.v));
                        for vy in 0..v {
                            for hx in 0..h {
                                block!(ci, mx * h + hx, my * v + vy);
                            }
                        }
                    }
                    unit += 1;
                    if !ok {
                        break 'mcus;
                    }
                }
                if streaming {
                    self.emit_row(my);
                }
            }
        }
        if !ok {
            self.damaged = true;
        }
        Ok(())
    }

    /// After the last scan: converts every MCU row from the stored coefficients.
    fn finish_buffered(&mut self) {
        let qts = self.qt;
        let mut samples = [0u8; 64];
        let mcus_y = self.frame.as_ref().map_or(0, |f| f.mcus_y);
        for my in 0..mcus_y {
            if let Some(frame) = self.frame.as_mut() {
                for c in &mut frame.comps {
                    let q = qts.get(c.tq).copied().unwrap_or([1; 64]);
                    let stride = c.bw * 8;
                    for vy in 0..c.v {
                        let by = my * c.v + vy;
                        for bx in 0..c.bw {
                            let at = (by * c.bw + bx) * 64;
                            let Some(block) = c.coefs.get(at..at + 64) else { continue };
                            idct(block, &q, &mut samples);
                            for (r, line) in samples.chunks_exact(8).enumerate() {
                                let o = (vy * 8 + r) * stride + bx * 8;
                                if let Some(dst) = c.plane.get_mut(o..o + 8) {
                                    dst.copy_from_slice(line);
                                }
                            }
                        }
                    }
                }
            }
            self.emit_row(my);
        }
        self.mode = Mode::Done;
    }
}

fn ycc_to_rgb(y: i32, cb: i32, cr: i32) -> [u8; 3] {
    let (cb, cr) = (cb - 128, cr - 128);
    [
        clamp8(y + ((91_881 * cr + 32_768) >> 16)),
        clamp8(y - ((22_554 * cb + 46_802 * cr + 32_768) >> 16)),
        clamp8(y + ((116_130 * cb + 32_768) >> 16)),
    ]
}

/// The position of the next marker at or after `from`: `0xFF` followed by something that is not
/// a stuffed zero, a fill byte or a restart marker.
fn next_marker(d: &[u8], from: usize) -> Option<usize> {
    let mut p = from;
    while let Some(&b) = d.get(p) {
        if b == 0xFF {
            match d.get(p + 1).copied() {
                Some(0 | 0xD0..=0xD7) => p += 2,
                Some(0xFF) => p += 1,
                Some(_) => return Some(p),
                None => return None,
            }
        } else {
            p += 1;
        }
    }
    None
}

pub(crate) fn decode(data: &[u8], limits: &Limits, pixels: bool) -> Result<Raw, DecodeError> {
    let mut dec = Decoder::new(data, limits, pixels);
    let mut pos = 2usize;
    let mut saw_eoi = false;
    let mut end = data.len();
    let mut scans = 0usize;
    // A hard stop for files made of nothing but markers.
    for _ in 0..100_000 {
        // Find the next marker.
        let Some(m_at) = next_marker(data, pos) else { break };
        let Some(&marker) = data.get(m_at + 1) else { break };
        pos = m_at + 2;
        match marker {
            0xD9 => {
                saw_eoi = true;
                end = pos;
                break;
            }
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            _ => {}
        }
        let mut c = Cursor::at(data, pos);
        let Some(len) = c.u16_be().map(usize::from) else { break };
        if len < 2 {
            return Err(bad("a segment shorter than its own length field"));
        }
        let Some(seg) = c.take(len - 2) else {
            dec.damaged = true;
            break;
        };
        pos = c.pos();
        match marker {
            0xC0 | 0xC1 => dec.parse_sof(seg, false)?,
            0xC2 => dec.parse_sof(seg, true)?,
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(DecodeError::Unsupported(
                    "JPEG with lossless, hierarchical or arithmetic coding is not shown".into(),
                ));
            }
            0xC4 => dec.parse_dht(seg)?,
            0xDB => dec.parse_dqt(seg)?,
            0xDD => {
                dec.restart_interval = Cursor::new(seg).u16_be().map_or(0, usize::from);
            }
            0xE0..=0xEF => dec.parse_app(marker, seg),
            0xFE => {
                dec.facts.flags |= flag::COMMENT;
                dec.facts.metadata_bytes += seg.len() as u64;
            }
            0xDA => {
                scans += 1;
                let scan = dec.parse_sos(seg)?;
                if dec.pixels && dec.mode != Mode::Done {
                    if dec.mode == Mode::Pending {
                        let (prog, ncomp) = dec
                            .frame
                            .as_ref()
                            .map_or((false, 0), |f| (f.progressive, f.comps.len()));
                        let streaming = !prog && scan.comps.len() == ncomp;
                        dec.prepare(if streaming { Mode::Streaming } else { Mode::Buffered })?;
                    }
                    dec.decode_scan(&scan, pos)?;
                    if dec.mode == Mode::Streaming {
                        dec.mode = Mode::Done;
                    }
                }
                // Skip the entropy-coded data to the next marker.
                match next_marker(data, pos) {
                    Some(p) => pos = p,
                    None => {
                        dec.damaged = true;
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if dec.frame.is_none() {
        return Err(bad("there is no frame header"));
    }
    if scans == 0 {
        return Err(bad("there is no image data"));
    }
    if !saw_eoi {
        dec.damaged = true;
    } else {
        dec.facts.trailing = (data.len() - end) as u64;
    }
    if dec.mode == Mode::Buffered {
        dec.finish_buffered();
    }
    if dec.damaged {
        dec.facts.flags |= flag::DAMAGED;
    }
    let facts = dec.facts.clone();
    if !pixels {
        return Ok(Raw { facts, pixels: None });
    }
    let out =
        dec.scaler.take().map(Downscaler::finish).ok_or_else(|| bad("there is no image data"))?;
    Ok(Raw { facts, pixels: Some(out) })
}
