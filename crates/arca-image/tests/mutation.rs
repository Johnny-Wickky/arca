// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Seeded, structure-aware mutation fuzzing of the image decoders (PNG, JPEG, GIF, BMP and the
//! EXIF reader behind them). No nightly toolchain is needed; the `cargo-fuzz` targets in `fuzz/`
//! cover coverage-guided exploration of the same code.
//!
//! A byte-flipping fuzzer alone mostly dies at the first checksum or marker. This one also
//!
//! * walks the **PNG chunks** and rewrites their fields with boundary values, then repairs the
//!   CRC so that the decoder gets past it (and, a third of the time, does not repair it);
//! * walks the **JPEG markers** and rewrites segment lengths, frame sizes and table bytes;
//! * builds **EXIF** blocks (a TIFF header and a few IFD entries with hostile offsets, counts
//!   and types) and inserts them as the `APP1` segment of a real JPEG.
//!
//! Every mutant must satisfy the invariants of [`check`]: no panic; a decoded picture is exactly
//! `width * height * 4` bytes and no side exceeds `max_out_side` (bounded output); the format
//! the sniffer names is the one the decoder reports; decoding twice gives the same result
//! (determinism); and the picture survives the wire. The run is deterministic: the same
//! `ARCA_FUZZ_SEED` and `ARCA_FUZZ_ITERS` replay the same inputs. A failing input is written to
//! the temporary directory (its path is part of the panic message).
//!
//! | variable           | meaning                                   | default |
//! |--------------------|-------------------------------------------|---------|
//! | `ARCA_FUZZ_ITERS`  | mutants generated per seed file           | 16      |
//! | `ARCA_FUZZ_SEED`   | PRNG seed (decimal or `0x` hex)           | fixed   |

// Test code may assert with panic and index (clippy.toml allows both inside `#[test]` items; these
// helpers live outside them).
#![allow(clippy::panic, clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use arca_abi::wire::{self, DecodeLimits};
use arca_image::{DecodeError, Image, ImageFormat, Limits, decode, encode_png, inspect, sniff};

const DEFAULT_ITERS: u64 = 16;
const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Small limits keep each execution fast and make the "bounded" checks sharp; they are the same
/// *kinds* of limit as the production ones.
const LIMITS: Limits = Limits {
    max_input: 1 << 20,
    max_side: 8192,
    max_stream_pixels: 2_000_000,
    max_buffered_pixels: 1_000_000,
    max_out_side: 256,
};

/// xorshift64* — small, fast, good enough for fuzzing, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // The state must never be zero.
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`0` for `n == 0`).
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        if n == 0 { 0 } else { usize::try_from(self.next() % n).unwrap_or(0) }
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn byte(&mut self) -> u8 {
        u8::try_from(self.next() & 0xFF).unwrap_or(0)
    }

    fn interesting(&mut self) -> u64 {
        INTERESTING[self.below(INTERESTING.len())]
    }
}

/// Values at which integer parsers go wrong.
const INTERESTING: &[u64] = &[
    0,
    1,
    2,
    3,
    0x7F,
    0x80,
    0xFF,
    0x100,
    0x7FFF,
    0x8000,
    0xFFFF,
    0x1_0000,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFE,
    0xFFFF_FFFF,
];

/// Writes `value` (big- or little-endian, `width` bytes) at `at`, if it fits.
fn put(buf: &mut [u8], at: usize, width: usize, big: bool, value: u64) {
    let Some(slot) = buf.get_mut(at..at.saturating_add(width)) else { return };
    let le = value.to_le_bytes();
    for (i, b) in slot.iter_mut().enumerate() {
        let k = if big { width - 1 - i } else { i };
        *b = le.get(k).copied().unwrap_or(0);
    }
}

fn be32(d: &[u8], at: usize) -> Option<usize> {
    let b: [u8; 4] = d.get(at..at.checked_add(4)?)?.try_into().ok()?;
    usize::try_from(u32::from_be_bytes(b)).ok()
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    let b: [u8; 2] = d.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(usize::from(u16::from_be_bytes(b)))
}

// ---- generic byte mutations -----------------------------------------------------------------

fn mutate_bytes(seed: &[u8], others: &[Vec<u8>], rng: &mut Rng) -> Vec<u8> {
    let mut m = seed.to_vec();
    for _ in 0..=rng.below(4) {
        let len = m.len();
        match rng.below(9) {
            0 => {
                if let Some(b) = m.get_mut(rng.below(len)) {
                    *b ^= 1 << rng.below(8);
                }
            }
            1 => {
                let at = rng.below(len);
                let (w, big) = ([1usize, 2, 4][rng.below(3)], rng.chance(2));
                let v = rng.interesting();
                put(&mut m, at, w, big, v);
            }
            2 => m.truncate(rng.below(len + 1)),
            3 => {
                let at = rng.below(len + 1);
                let n = 1 + rng.below(16);
                let fill: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
                m.splice(at..at, fill);
            }
            4 => {
                let at = rng.below(len);
                let n = (1 + rng.below(32)).min(len - at.min(len));
                m.drain(at..at + n);
            }
            5 => {
                // Duplicate a block in place.
                let at = rng.below(len);
                let n = (1 + rng.below(64)).min(len - at.min(len));
                let block: Vec<u8> = m.get(at..at + n).unwrap_or_default().to_vec();
                m.splice(at..at, block);
            }
            6 => {
                // Splice in a piece of another seed (a chunk or segment of a foreign file).
                if !others.is_empty() {
                    let o = &others[rng.below(others.len())];
                    let from = rng.below(o.len());
                    let n = (1 + rng.below(256)).min(o.len() - from.min(o.len()));
                    let at = rng.below(len + 1);
                    m.splice(at..at, o.get(from..from + n).unwrap_or_default().iter().copied());
                }
            }
            7 => {
                let at = rng.below(len);
                let n = (1 + rng.below(8)).min(len - at.min(len));
                let v = rng.byte();
                if let Some(s) = m.get_mut(at..at + n) {
                    s.fill(v);
                }
            }
            _ => {
                if let Some(b) = m.get_mut(rng.below(len)) {
                    *b = rng.byte();
                }
            }
        }
        if m.is_empty() {
            m.push(rng.byte());
        }
    }
    m
}

// ---- PNG ------------------------------------------------------------------------------------

/// `(offset of the length field, data length)` of every chunk that fits in the file.
fn png_chunks(d: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 8;
    while let Some(len) = be32(d, at) {
        if at.saturating_add(12).saturating_add(len) > d.len() {
            break;
        }
        out.push((at, len));
        at += 12 + len;
    }
    out
}

fn fix_png_crc(d: &mut [u8], chunk: (usize, usize)) {
    let (at, len) = chunk;
    let Some(body) = d.get(at + 4..at + 8 + len) else { return };
    let crc = arca_digest::crc32(body);
    put(d, at + 8 + len, 4, true, u64::from(crc));
}

fn mutate_png(seed: &[u8], rng: &mut Rng) -> Option<Vec<u8>> {
    if !seed.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut m = seed.to_vec();
    for _ in 0..=rng.below(3) {
        // The chunk list is recomputed: an earlier step may have dropped a chunk.
        let chunks = png_chunks(&m);
        let chunk = *chunks.get(rng.below(chunks.len()))?;
        let (at, len) = chunk;
        match rng.below(5) {
            0 => {
                // A hostile IHDR: dimensions, depth, colour type, interlace.
                let ihdr = *chunks.first()?;
                let field =
                    [(8usize, 4usize), (12, 4), (16, 1), (17, 1), (18, 1), (19, 1), (20, 1)];
                let (off, w) = field[rng.below(field.len())];
                let v = rng.interesting();
                put(&mut m, ihdr.0 + off, w, true, v);
                fix_png_crc(&mut m, ihdr);
                continue;
            }
            1 => put(&mut m, at, 4, true, rng.interesting()),
            2 => {
                // Bytes inside the chunk data.
                for _ in 0..=rng.below(4) {
                    let i = at + 8 + rng.below(len.max(1));
                    if let Some(b) = m.get_mut(i) {
                        *b = rng.byte();
                    }
                }
            }
            3 => {
                // Another chunk type for the same data.
                for i in 0..4 {
                    if let Some(b) = m.get_mut(at + 4 + i) {
                        *b = b"IHDRPLTEtRNSIDATIENDeXIFtEXtzTXtiTXtacTLfcTLfdATgAMAcHRMsRGBiCCPpHYsbKGDhISTsBIT"
                            [rng.below(20) * 4 + i];
                    }
                }
            }
            _ => {
                // Drop the chunk.
                if at + 12 + len <= m.len() {
                    m.drain(at..at + 12 + len);
                }
                continue;
            }
        }
        if !rng.chance(3) {
            fix_png_crc(&mut m, chunk);
        }
    }
    Some(m)
}

// ---- JPEG and EXIF --------------------------------------------------------------------------

/// Offsets of the `FF xx` markers that start a segment with a length field.
fn jpeg_segments(d: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 2;
    while d.get(at) == Some(&0xFF) {
        let marker = d.get(at + 1).copied().unwrap_or(0);
        if matches!(marker, 0x00 | 0x01 | 0xD0..=0xD9) {
            at += 2;
            continue;
        }
        out.push(at);
        match be16(d, at + 2) {
            Some(n) if n >= 2 => at += 2 + n,
            _ => break,
        }
    }
    out
}

fn mutate_jpeg(seed: &[u8], rng: &mut Rng) -> Option<Vec<u8>> {
    if !seed.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut m = seed.to_vec();
    let segs = jpeg_segments(&m);
    for _ in 0..=rng.below(3) {
        let at = *segs.get(rng.below(segs.len()))?;
        match rng.below(4) {
            0 => put(&mut m, at + 2, 2, true, rng.interesting()),
            1 => {
                // Frame header: precision, height, width, component count.
                let (off, w) = [(4usize, 1usize), (5, 2), (7, 2), (9, 1)][rng.below(4)];
                put(&mut m, at + off, w, true, rng.interesting());
            }
            2 => {
                let n = be16(&m, at + 2).unwrap_or(2);
                let i = at + 4 + rng.below(n.saturating_sub(2).max(1));
                if let Some(b) = m.get_mut(i) {
                    *b = rng.byte();
                }
            }
            _ => {
                // Another marker for the same segment.
                if let Some(b) = m.get_mut(at + 1) {
                    *b = [0xC0, 0xC1, 0xC2, 0xC4, 0xDA, 0xDB, 0xDD, 0xE0, 0xE1, 0xEE, 0xFE]
                        [rng.below(11)];
                }
            }
        }
    }
    Some(m)
}

/// A TIFF block with a few IFD entries whose offsets, counts and types are hostile.
fn hostile_tiff(rng: &mut Rng) -> Vec<u8> {
    let big = rng.chance(2);
    let mut t = if big { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
    let ifd_at = if rng.chance(4) { rng.interesting() } else { 8 };
    t.extend_from_slice(&if big {
        u32::try_from(ifd_at).unwrap_or(8).to_be_bytes()
    } else {
        u32::try_from(ifd_at).unwrap_or(8).to_le_bytes()
    });
    let entries = rng.below(10);
    let count = if rng.chance(8) { rng.interesting() } else { entries as u64 };
    t.extend_from_slice(&if big {
        u16::try_from(count & 0xFFFF).unwrap_or(0).to_be_bytes()
    } else {
        u16::try_from(count & 0xFFFF).unwrap_or(0).to_le_bytes()
    });
    for _ in 0..entries {
        // Tags the reader cares about come up often: orientation, make, model, software, date,
        // the Exif and GPS sub-IFD pointers, the interoperability pointer.
        let tag = [0x0112, 0x010F, 0x0110, 0x0131, 0x0132, 0x8769, 0x8825, 0xA005, 0x9003, 0x013B]
            [rng.below(10)];
        let ty = if rng.chance(3) { rng.interesting() } else { 1 + rng.below(12) as u64 };
        for (v, w) in
            [(tag, 2usize), (ty & 0xFFFF, 2), (rng.interesting(), 4), (rng.interesting(), 4)]
        {
            let mut field = vec![0u8; w];
            put(&mut field, 0, w, big, v);
            t.extend_from_slice(&field);
        }
    }
    t.extend_from_slice(&[0; 4]);
    for _ in 0..rng.below(64) {
        t.push(rng.byte());
    }
    t
}

/// Inserts an `APP1` Exif segment with a hostile TIFF block right after the SOI of a JPEG.
fn mutate_exif(seed: &[u8], rng: &mut Rng) -> Option<Vec<u8>> {
    if !seed.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let tiff = hostile_tiff(rng);
    let mut seg = vec![0xFF, 0xE1, 0, 0];
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    let declared = if rng.chance(6) { rng.interesting() } else { (seg.len() - 2) as u64 };
    put(&mut seg, 2, 2, true, declared);
    let mut m = seed.to_vec();
    m.splice(2..2, seg);
    Some(m)
}

// ---- seeds ----------------------------------------------------------------------------------

/// A 2x2, 24-bit BMP.
fn tiny_bmp() -> Vec<u8> {
    let mut b = b"BM".to_vec();
    b.extend_from_slice(&70u32.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&[0x13, 0x0B, 0, 0, 0x13, 0x0B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for row in 0..2u8 {
        b.extend_from_slice(&[200 * row, 30, 90, 10, 220, 40 * row, 0, 0]);
    }
    b
}

/// A 2x1 GIF with a two-colour palette.
fn tiny_gif() -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend_from_slice(&[2, 0, 1, 0, 0xF0, 0, 0]);
    g.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 2, 0, 1, 0, 0]);
    g.extend_from_slice(&[2, 2, 0x44, 0x0A, 0, 0x3B]);
    g
}

fn synthetic_png() -> Vec<u8> {
    let (w, h) = (16u32, 12u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[
                u8::try_from(x * 16).unwrap_or(0),
                u8::try_from(y * 20).unwrap_or(0),
                90,
                if x % 5 == 0 { 128 } else { 255 },
            ]);
        }
    }
    encode_png(w, h, &px).map(|(png, _)| png).unwrap_or_default()
}

fn seeds() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jpg" || x == "png"))
        .collect();
    names.sort();
    let mut out: Vec<(String, Vec<u8>)> = names
        .into_iter()
        .map(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (name, std::fs::read(&p).unwrap_or_default())
        })
        .collect();
    out.push(("tiny.bmp".into(), tiny_bmp()));
    out.push(("tiny.gif".into(), tiny_gif()));
    out.push(("synthetic.png".into(), synthetic_png()));
    out
}

// ---- the invariants -------------------------------------------------------------------------

fn check(data: &[u8], what: &str) {
    let first = decode(data, &LIMITS);
    if let Ok(img) = &first {
        assert!(img.width >= 1 && img.height >= 1, "{what}: empty picture");
        assert!(
            img.width <= LIMITS.max_out_side && img.height <= LIMITS.max_out_side,
            "{what}: {}x{} exceeds the output bound",
            img.width,
            img.height
        );
        assert_eq!(
            img.rgba.len(),
            usize::try_from(u64::from(img.width) * u64::from(img.height) * 4).unwrap_or(usize::MAX),
            "{what}: the pixel buffer does not match the size"
        );
        assert_eq!(
            sniff(data),
            Some(img.facts.format),
            "{what}: sniffed and decoded formats differ"
        );
        assert!(img.facts.frames >= 1, "{what}: no frames");
        // The host decodes this from an untrusted guest: it must round-trip.
        let bytes = wire::to_bytes(img).unwrap_or_else(|e| panic!("{what}: encode failed: {e}"));
        let back: Image = wire::from_bytes(&bytes, DecodeLimits::default())
            .unwrap_or_else(|e| panic!("{what}: decode failed: {e}"));
        assert_eq!(&back, img, "{what}: wire round trip changed the picture");
        // And the sterile PNG written from it must read back to the same pixels.
        let (png, _) = encode_png(img.width, img.height, &img.rgba)
            .unwrap_or_else(|e| panic!("{what}: encode_png failed: {e}"));
        let again = decode(&png, &LIMITS)
            .unwrap_or_else(|e| panic!("{what}: the PNG copy is unreadable: {e}"));
        assert_eq!(again.facts.format, ImageFormat::Png);
        assert_eq!(
            (again.width, again.height),
            (img.width, img.height),
            "{what}: the copy changed the size"
        );
    }
    // Deterministic: the same bytes always give the same answer.
    assert_eq!(decode(data, &LIMITS), first, "{what}: decode is not deterministic");
    // The cheap path never contradicts the full one.
    match (inspect(data, &LIMITS), &first) {
        (Ok(f), Ok(img)) => assert_eq!(f.format, img.facts.format, "{what}: inspect disagrees"),
        (Err(DecodeError::Unsupported(_)), Ok(_)) => {
            panic!("{what}: inspect refuses what decode reads")
        }
        _ => {}
    }
}

/// Runs [`check`]; on failure saves the input and re-raises with its path.
fn check_or_save(data: &[u8], what: &str) {
    let Err(payload) = catch_unwind(AssertUnwindSafe(|| check(data, what))) else { return };
    let digest: String = arca_digest::sha256(data).to_string().chars().take(16).collect();
    let path = std::env::temp_dir().join(format!("arca-fuzz-failure-{digest}.bin"));
    let saved = match std::fs::write(&path, data) {
        Ok(()) => format!("input saved to {}", path.display()),
        Err(e) => format!("input could not be saved: {e}"),
    };
    let detail = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    panic!("image mutation fuzz failure ({what}); {saved}\n{detail}");
}

fn env_u64(name: &str, default: u64) -> u64 {
    let Ok(text) = std::env::var(name) else { return default };
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.unwrap_or_else(|_| panic!("{name}={text:?} is not a number"))
}

fn mutant(seed: &[u8], bodies: &[Vec<u8>], rng: &mut Rng) -> Vec<u8> {
    let structural = match rng.below(10) {
        0..=2 => mutate_png(seed, rng),
        3..=5 => mutate_jpeg(seed, rng),
        6 | 7 => mutate_exif(seed, rng),
        _ => None,
    };
    structural.unwrap_or_else(|| mutate_bytes(seed, bodies, rng))
}

#[test]
fn image_mutation_fuzz_never_panics() {
    let iters = env_u64("ARCA_FUZZ_ITERS", DEFAULT_ITERS);
    let seed = env_u64("ARCA_FUZZ_SEED", DEFAULT_SEED);
    let seeds = seeds();
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    assert!(seeds.len() > 40, "the seed corpus has shrunk: {}", seeds.len());

    let mut rng = Rng::new(seed);
    for (name, body) in &seeds {
        check_or_save(body, &format!("{name} (seed)"));
        for i in 0..iters {
            check_or_save(&mutant(body, &bodies, &mut rng), &format!("{name} #{i}"));
        }
    }
}

#[test]
fn the_seeds_themselves_are_readable_pictures() {
    // Otherwise the mutators would be exercising error paths only.
    for (name, body) in seeds() {
        if name.ends_with(".jpg") && name.contains("cmyk") {
            continue;
        }
        let r = decode(&body, &LIMITS);
        assert!(r.is_ok(), "{name}: {:?}", r.err());
    }
}

#[test]
fn the_prng_and_mutators_are_deterministic() {
    let seeds = seeds();
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    let run = || {
        let mut rng = Rng::new(7);
        seeds
            .iter()
            .map(|(_, b)| mutant(b, &bodies, &mut rng))
            .map(|m| arca_digest::sha256(&m).to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn structural_mutators_produce_real_changes_and_exif_blocks() {
    let mut rng = Rng::new(11);
    let seeds = seeds();
    let (mut png, mut jpeg, mut exif) = (0, 0, 0);
    for (_, body) in &seeds {
        for _ in 0..4 {
            png += usize::from(mutate_png(body, &mut rng).is_some_and(|m| &m != body));
            jpeg += usize::from(mutate_jpeg(body, &mut rng).is_some_and(|m| &m != body));
            exif += usize::from(mutate_exif(body, &mut rng).is_some_and(|m| m.len() > body.len()));
        }
    }
    assert!(png > 20 && jpeg > 20 && exif > 20, "png {png}, jpeg {jpeg}, exif {exif}");
}

#[test]
fn bounded_output_holds_for_a_huge_declared_size() {
    // A PNG header that claims 2^31 x 2^31 must be refused, not allocated.
    let mut d = synthetic_png();
    put(&mut d, 16, 4, true, 0x7FFF_FFFF);
    put(&mut d, 20, 4, true, 0x7FFF_FFFF);
    let chunk = (8usize, 13usize);
    fix_png_crc(&mut d, chunk);
    assert!(matches!(
        decode(&d, &LIMITS),
        Err(DecodeError::TooLarge(_) | DecodeError::Malformed(_))
    ));
}

#[test]
fn exif_mutants_reach_the_exif_reader() {
    // Otherwise `mutate_exif` would only ever test the "unknown segment" path of the JPEG reader.
    let mut rng = Rng::new(13);
    let (mut tried, mut with_exif) = (0, 0);
    for (name, body) in seeds().iter().filter(|(n, _)| n.ends_with(".jpg")) {
        for _ in 0..6 {
            let Some(m) = mutate_exif(body, &mut rng) else { continue };
            tried += 1;
            with_exif += usize::from(inspect(&m, &LIMITS).is_ok_and(|f| f.exif.is_some()));
        }
        assert!(tried > 0, "{name}");
    }
    assert!(
        with_exif * 10 > tried,
        "only {with_exif} of {tried} EXIF mutants reach the EXIF reader"
    );
}
