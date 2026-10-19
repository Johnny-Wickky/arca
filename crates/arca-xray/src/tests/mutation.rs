//! Seeded, structure-aware mutation fuzzing of the whole scan / sanitize
//! pipeline.
//!
//! A byte-flipping fuzzer alone mostly produces archives that die at the first
//! CRC check. This one also
//!
//! * rewrites **container header fields** (ZIP local / central / end records,
//!   CFB header) with boundary values, which is where length-confusion bugs live;
//! * unpacks a valid seed, mutates the **decompressed XML / text of one
//!   member** (DOCTYPEs, entity references, unbalanced tags, deep nesting,
//!   huge attributes, truncation) and repacks it, so the XML scanners are
//!   exercised on inputs that survive the container layer.
//!
//! Every mutant must satisfy the invariants of [`check`]. The run is
//! deterministic: the same `ARCA_FUZZ_SEED` and `ARCA_FUZZ_ITERS` replay the
//! same inputs. A failing input is written to the temporary directory (its
//! path is part of the panic message) so it can be replayed and minimised.
//!
//! | variable           | meaning                                   | default |
//! |--------------------|-------------------------------------------|---------|
//! | `ARCA_FUZZ_ITERS`  | mutants generated per seed file           | 40      |
//! | `ARCA_FUZZ_SEED`   | PRNG seed (decimal or `0x` hex)           | fixed   |
//!
//! The scheduled `fuzz` workflow runs this test with a large iteration count in
//! release mode; the `cargo-fuzz` targets in `fuzz/` cover coverage-guided
//! exploration.

use std::panic::{AssertUnwindSafe, catch_unwind};

use arca_abi::wire::{self, DecodeLimits};
use arca_container::InflateBudget;
use arca_container::zip::writer::{Compression, ZipWriter};
use arca_container::zip::{ZipArchive, ZipLimits};

use super::corpus;
use crate::report::{Verdict, XRayReport};
use crate::sanitize::{SanitizeOptions, sanitize};
use crate::{ScanPolicy, scan_default};

const DEFAULT_ITERS: u64 = 40;
const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

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

    /// Uniform in `0..n` (`n > 0`; `0` for `n == 0`).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }
}

/// Values at which integer parsers go wrong.
const INTERESTING: &[u64] = &[
    0,
    1,
    2,
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
    0x1_0000_0000,
    u64::MAX,
];

fn put_le(buf: &mut [u8], at: usize, width: usize, value: u64) {
    let bytes = value.to_le_bytes();
    for (k, b) in bytes.iter().take(width).enumerate() {
        if let Some(slot) = buf.get_mut(at + k) {
            *slot = *b;
        }
    }
}

fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return Vec::new();
    }
    (0..=haystack.len() - needle.len())
        .filter(|&i| haystack.get(i..i + needle.len()) == Some(needle))
        .collect()
}

/// `(signature, field offset, width)` of the ZIP fields worth attacking.
const ZIP_FIELDS: &[(&[u8; 4], usize, usize)] = &[
    // local file header
    (b"PK\x03\x04", 4, 2),  // version needed
    (b"PK\x03\x04", 6, 2),  // flags
    (b"PK\x03\x04", 8, 2),  // method
    (b"PK\x03\x04", 14, 4), // crc32
    (b"PK\x03\x04", 18, 4), // compressed size
    (b"PK\x03\x04", 22, 4), // uncompressed size
    (b"PK\x03\x04", 26, 2), // name length
    (b"PK\x03\x04", 28, 2), // extra length
    // central directory header
    (b"PK\x01\x02", 8, 2),  // flags
    (b"PK\x01\x02", 10, 2), // method
    (b"PK\x01\x02", 16, 4), // crc32
    (b"PK\x01\x02", 20, 4), // compressed size
    (b"PK\x01\x02", 24, 4), // uncompressed size
    (b"PK\x01\x02", 28, 2), // name length
    (b"PK\x01\x02", 30, 2), // extra length
    (b"PK\x01\x02", 32, 2), // comment length
    (b"PK\x01\x02", 42, 4), // local header offset
    // end of central directory
    (b"PK\x05\x06", 8, 2),  // entries on this disk
    (b"PK\x05\x06", 10, 2), // total entries
    (b"PK\x05\x06", 12, 4), // central directory size
    (b"PK\x05\x06", 16, 4), // central directory offset
    (b"PK\x05\x06", 20, 2), // comment length
];

/// `(offset, width)` of the OLE compound file header fields.
const CFB_FIELDS: &[(usize, usize)] = &[
    (24, 2), // minor version
    (26, 2), // major version
    (28, 2), // byte order
    (30, 2), // sector shift
    (32, 2), // mini sector shift
    (40, 4), // directory sector count
    (44, 4), // FAT sector count
    (48, 4), // first directory sector
    (56, 4), // mini stream cutoff
    (60, 4), // first mini FAT sector
    (64, 4), // mini FAT sector count
    (68, 4), // first DIFAT sector
    (72, 4), // DIFAT sector count
    (76, 4), // first DIFAT entry
];

const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Text fragments that stress XML and field-code handling.
const XML_SNIPPETS: &[&str] = &[
    r#"<!DOCTYPE x [<!ENTITY a "b">]>"#,
    r#"<!DOCTYPE x SYSTEM "http://198.51.100.1/x.dtd">"#,
    "&a;",
    "&#0;",
    "&#x10FFFF;",
    "&#xD800;",
    "<![CDATA[",
    "]]>",
    "<?xml-stylesheet href=\"http://198.51.100.1/s.xsl\"?>",
    "<a:b xmlns:a=\"urn:x\" xmlns:a=\"urn:y\"/>",
    "<w:fldSimple w:instr=\"DDEAUTO c:\\\\windows\\\\system32\\\\cmd.exe\">",
    "<f>HYPERLINK(\"http://198.51.100.1\")</f>",
    "<f>cmd|' /C calc'!A0</f>",
    "<![if x]>",
    "<!--",
    "-->",
    "\u{0}",
    "\u{202E}",
    "\u{FEFF}",
    "</x>",
    "<x",
];

fn mutate_bytes(seed: &[u8], others: &[Vec<u8>], rng: &mut Rng) -> Vec<u8> {
    let mut m = seed.to_vec();
    for _ in 0..=rng.below(3) {
        match rng.below(12) {
            0 => {
                // add to a few bytes
                for _ in 0..=rng.below(7) {
                    let i = rng.below(m.len());
                    if let Some(b) = m.get_mut(i) {
                        *b = b.wrapping_add(1 + rng.below(255) as u8);
                    }
                }
            }
            1 => {
                // flip single bits
                for _ in 0..=rng.below(15) {
                    let i = rng.below(m.len());
                    let bit = 1u8 << rng.below(8);
                    if let Some(b) = m.get_mut(i) {
                        *b ^= bit;
                    }
                }
            }
            2 => {
                let cut = rng.below(m.len());
                m.truncate(cut);
            }
            3 => {
                let n = rng.below(64);
                let fill = if rng.chance(2) { 0 } else { rng.next().to_le_bytes()[0] };
                m.extend(std::iter::repeat_n(fill, n));
            }
            4 => {
                // duplicate a block in place
                let start = rng.below(m.len());
                let len = (1 + rng.below(256)).min(m.len().saturating_sub(start));
                if let Some(block) = m.get(start..start + len).map(<[u8]>::to_vec) {
                    let at = rng.below(m.len() + 1);
                    m.splice(at..at, block);
                }
            }
            5 => {
                // delete a block
                let start = rng.below(m.len());
                let len = (1 + rng.below(256)).min(m.len().saturating_sub(start));
                m.drain(start..start + len);
            }
            6 => {
                // overwrite an integer with a boundary value
                let at = rng.below(m.len());
                let width = *rng.pick(&[1usize, 2, 4, 8]).unwrap_or(&4);
                put_le(&mut m, at, width, *rng.pick(INTERESTING).unwrap_or(&0));
            }
            7 => {
                // splice a region of another seed over this one
                if let Some(other) = rng.pick(others) {
                    let from = rng.below(other.len());
                    let len = (1 + rng.below(512)).min(other.len().saturating_sub(from));
                    let at = rng.below(m.len());
                    if let Some(block) = other.get(from..from + len) {
                        let end = (at + block.len()).min(m.len());
                        m.splice(at..end, block.iter().copied());
                    }
                }
            }
            8 | 9 => mutate_zip_header(&mut m, rng),
            10 => mutate_cfb_header(&mut m, rng),
            _ => {
                // insert a text fragment (useful for XML stored without deflate)
                if let Some(s) = rng.pick(XML_SNIPPETS) {
                    let at = rng.below(m.len() + 1);
                    m.splice(at..at, s.bytes());
                }
            }
        }
    }
    m
}

fn mutate_zip_header(m: &mut [u8], rng: &mut Rng) {
    let &(sig, offset, width) = rng.pick(ZIP_FIELDS).unwrap_or(&(b"PK\x05\x06", 10, 2));
    let hits = find_all(m, sig);
    if let Some(&base) = rng.pick(&hits) {
        let value = match rng.below(3) {
            0 => *rng.pick(INTERESTING).unwrap_or(&0),
            1 => rng.next(),
            _ => rng.next() & 0xFF,
        };
        put_le(m, base + offset, width, value);
    }
}

fn mutate_cfb_header(m: &mut [u8], rng: &mut Rng) {
    if m.get(..8) != Some(&CFB_MAGIC[..]) {
        return;
    }
    let &(offset, width) = rng.pick(CFB_FIELDS).unwrap_or(&(48, 4));
    put_le(m, offset, width, *rng.pick(INTERESTING).unwrap_or(&0));
}

/// Mutates the decompressed content of one member of a valid ZIP seed and
/// repacks it. `None` if the seed is not a readable archive.
fn mutate_member(seed: &[u8], rng: &mut Rng) -> Option<Vec<u8>> {
    let archive = ZipArchive::parse(seed, ZipLimits::default()).ok()?;
    let policy = ScanPolicy::default();
    let mut budget = InflateBudget::new(64 << 20, policy.aggregate_ratio, 16 << 20);
    let mut members: Vec<(String, Vec<u8>, bool)> = Vec::new();
    for entry in archive.entries() {
        if entry.is_dir() {
            continue;
        }
        let data = archive.read(entry, &mut budget).ok()?;
        members.push((entry.name().to_owned(), data, entry.method() == 0));
    }
    let victim = rng.below(members.len());
    let (_, data, _) = members.get_mut(victim)?;
    match rng.below(7) {
        0 => {
            let cut = rng.below(data.len());
            data.truncate(cut);
        }
        1 => {
            let at = rng.below(data.len() + 1);
            let snippet = rng.pick(XML_SNIPPETS)?;
            data.splice(at..at, snippet.bytes());
        }
        2 => {
            // deep nesting
            let depth = 1 + rng.below(400);
            let open = "<n>".repeat(depth);
            let close = "</n>".repeat(depth);
            let at = rng.below(data.len() + 1);
            data.splice(at..at, open.bytes().chain(close.bytes()));
        }
        3 => {
            // oversized attribute / text
            let big = "A".repeat(1 + rng.below(70_000));
            let at = rng.below(data.len() + 1);
            data.splice(at..at, format!("<q v=\"{big}\"/>").bytes());
        }
        4 => {
            for _ in 0..=rng.below(5) {
                let i = rng.below(data.len());
                if let Some(b) = data.get_mut(i) {
                    *b = b.wrapping_add(1 + rng.below(255) as u8);
                }
            }
        }
        5 => {
            // a UTF-16 byte-order mark in front of UTF-8 text
            let bom: &[u8] = if rng.chance(2) { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] };
            data.splice(0..0, bom.iter().copied());
        }
        _ => {
            data.clear();
        }
    }
    let mut z = ZipWriter::new();
    for (name, data, stored) in &members {
        let method =
            if *stored || rng.chance(4) { Compression::Stored } else { Compression::Deflated };
        z.add(name, data, method).ok()?;
    }
    z.finish().ok()
}

/// The invariants every mutant must satisfy. Panics on a violation.
fn check(data: &[u8], what: &str, deep: bool) {
    let policy = ScanPolicy::default();
    let r = scan_default(data);
    corpus::assert_verdict_invariants(&r, what);

    // The report must survive the sandbox boundary.
    let bytes = wire::to_bytes(&r).unwrap_or_else(|e| panic!("{what}: encode failed: {e}"));
    let back: XRayReport = wire::from_bytes(&bytes, DecodeLimits::default())
        .unwrap_or_else(|e| panic!("{what}: decode failed: {e}"));
    assert_eq!(back, r, "{what}: wire round trip changed the report");

    if deep {
        // Determinism: the same bytes always give the same report.
        assert_eq!(scan_default(data), r, "{what}: scan is not deterministic");
    }

    let Ok(doc) = sanitize(data, &policy, &SanitizeOptions::default()) else { return };

    // A sterile copy is verified by an independent scan of the produced bytes.
    assert_eq!(doc.verification, scan_default(&doc.bytes), "{what}: stale verification");
    assert!(
        matches!(doc.verification.verdict, Verdict::Clean | Verdict::HiddenTrackers),
        "{what}: sterile copy has verdict {:?}: {:#?}",
        doc.verification.verdict,
        doc.verification.findings
    );
    corpus::assert_verdict_invariants(&doc.verification, what);
    if deep {
        // Sterilising is idempotent: a sterile file is a fixed point.
        let again = sanitize(&doc.bytes, &policy, &SanitizeOptions::default())
            .unwrap_or_else(|e| panic!("{what}: sterile copy cannot be sterilised again: {e}"));
        assert_eq!(again.bytes, doc.bytes, "{what}: sanitize is not idempotent");
    }
}

/// Runs [`check`]; on failure saves the input and re-raises with its path.
fn check_or_save(data: &[u8], what: &str, deep: bool) {
    let Err(payload) = catch_unwind(AssertUnwindSafe(|| check(data, what, deep))) else { return };
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
    panic!("mutation fuzz failure ({what}); {saved}\n{detail}");
}

fn env_u64(name: &str, default: u64) -> u64 {
    let Ok(text) = std::env::var(name) else { return default };
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.unwrap_or_else(|_| panic!("{name}={text:?} is not a number"))
}

fn seeds() -> Vec<(String, Vec<u8>)> {
    corpus::samples()
        .into_iter()
        // 17 MiB of zeros: covered by its own test, wasteful to mutate.
        .filter(|s| s.name != "oversized-input")
        .map(|s| (s.name.to_owned(), (s.build)()))
        .collect()
}

#[test]
fn mutation_fuzz_never_panics() {
    let iters = env_u64("ARCA_FUZZ_ITERS", DEFAULT_ITERS);
    let seed = env_u64("ARCA_FUZZ_SEED", DEFAULT_SEED);
    let seeds = seeds();
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    assert!(seeds.len() > 50, "the seed corpus has shrunk: {}", seeds.len());

    let mut rng = Rng::new(seed);
    for (name, body) in &seeds {
        // The unmodified seed must pass every check, in deep mode.
        check_or_save(body, &format!("{name} (seed)"), true);
        for i in 0..iters {
            let deep = i % 8 == 0;
            let structural = if rng.chance(3) { mutate_member(body, &mut rng) } else { None };
            let mutant = structural.unwrap_or_else(|| mutate_bytes(body, &bodies, &mut rng));
            check_or_save(&mutant, &format!("{name} #{i}"), deep);
        }
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
            .take(20)
            .map(|(_, b)| mutate_bytes(b, &bodies, &mut rng))
            .map(|m| arca_digest::sha256(&m).to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn member_mutation_yields_readable_archives() {
    // At least some structural mutants must survive the container layer;
    // otherwise `mutate_member` would silently degrade into the byte fuzzer.
    let mut rng = Rng::new(11);
    let mut readable = 0usize;
    let mut tried = 0usize;
    for (_, body) in seeds().iter().filter(|(_, b)| b.starts_with(b"PK")) {
        for _ in 0..8 {
            tried += 1;
            if let Some(m) = mutate_member(body, &mut rng)
                && ZipArchive::parse(&m, ZipLimits::default()).is_ok()
            {
                readable += 1;
            }
        }
    }
    assert!(tried > 100, "too few ZIP seeds: {tried}");
    assert!(readable * 2 > tried, "only {readable}/{tried} structural mutants are readable ZIPs");
}
