// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Seeded, structure-aware mutation fuzzing of the Jet / ACE database reader
//! (`arca_container::jet`, behind `.mdb` and `.accdb`). No nightly toolchain is needed; the
//! `cargo-fuzz` target `jet` in `fuzz/` covers coverage-guided exploration of the same code.
//!
//! A database is a sequence of fixed-size pages whose first bytes say what the page is and where
//! its chain continues, so the mutators work on **pages**: they rewrite header fields of a
//! random page with boundary values, copy one page over another (a chain that loops, a table
//! definition that is also a data page), swap pages and cut the file at a page boundary, besides
//! flipping plain bytes.
//!
//! Every mutant must satisfy the invariants of [`walk`]: no panic; the reader opens the file or
//! refuses it cleanly; the number of objects, columns and rows it reports and the size of each
//! value stay within the documented limits (bounded output, whatever the file claims); and
//! reading the same bytes twice gives the same answer (determinism). The run is deterministic:
//! the same `ARCA_FUZZ_SEED` and `ARCA_FUZZ_ITERS` replay the same inputs. A failing input is
//! written to the temporary directory (its path is part of the panic message).
//!
//! | variable           | meaning                                   | default |
//! |--------------------|-------------------------------------------|---------|
//! | `ARCA_FUZZ_ITERS`  | mutants generated per seed file           | 40      |
//! | `ARCA_FUZZ_SEED`   | PRNG seed (decimal or `0x` hex)           | fixed   |

// Test code may assert with panic and index (clippy.toml allows both inside `#[test]` items; these
// helpers live outside them).
#![allow(clippy::panic, clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use arca_container::jet::{Db, OpenError, Value};

const DEFAULT_ITERS: u64 = 40;
const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Most tables read per mutant, and rows per table: keeps one execution fast.
const MAX_TABLES: usize = 6;
const MAX_ROWS: u64 = 1500;
/// The reader's documented caps (see the constants of `jet/mod.rs`).
const CAP_OBJECTS: usize = 100_000;
const CAP_COLUMNS: usize = 255;
/// Longest text value: a 4 MiB long value, at most three UTF-8 bytes per UTF-16 unit pair.
const CAP_TEXT: usize = 3 * (4 << 20);
/// Work units one table scan may spend; the reader's own figure is higher.
const WORK: u64 = 60_000;

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
    0xFF_FFFF,
    0x100_0000,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

/// Writes `value` little-endian (`width` bytes) at `at`, if it fits.
fn put_le(buf: &mut [u8], at: usize, width: usize, value: u64) {
    let Some(slot) = buf.get_mut(at..at.saturating_add(width)) else { return };
    for (b, v) in slot.iter_mut().zip(value.to_le_bytes()) {
        *b = v;
    }
}

/// Page size by file version: Jet 3 pages are 2 KiB, everything newer 4 KiB.
fn page_size(d: &[u8]) -> usize {
    if d.get(0x14) == Some(&0) { 2048 } else { 4096 }
}

fn mutate(seed: &[u8], others: &[Vec<u8>], rng: &mut Rng) -> Vec<u8> {
    let mut m = seed.to_vec();
    let ps = page_size(&m);
    for _ in 0..=rng.below(4) {
        let pages = (m.len() / ps).max(1);
        // Early pages (header, catalog, table definitions) matter most.
        let page = if rng.chance(2) { rng.below(pages.min(64)) } else { rng.below(pages) };
        let base = page * ps;
        let near = |rng: &mut Rng| base + if rng.chance(4) { rng.below(ps) } else { rng.below(96) };
        match rng.below(10) {
            0 => {
                let at = near(rng);
                if let Some(b) = m.get_mut(at) {
                    *b ^= 1 << rng.below(8);
                }
            }
            1 | 2 => {
                let (at, w) = (near(rng), [1usize, 2, 3, 4][rng.below(4)]);
                let v = rng.interesting();
                put_le(&mut m, at, w, v);
            }
            3 => {
                // Copy a page over another: loops in chains, type confusion.
                let from = rng.below(pages) * ps;
                let to = base;
                if let Some(src) = m.get(from..from + ps).map(<[u8]>::to_vec)
                    && let Some(dst) = m.get_mut(to..to + ps)
                {
                    dst.copy_from_slice(&src);
                }
            }
            4 => {
                // Swap two pages.
                let other = rng.below(pages) * ps;
                if base != other && base + ps <= m.len() && other + ps <= m.len() {
                    for i in 0..ps {
                        m.swap(base + i, other + i);
                    }
                }
            }
            5 => m.truncate(
                (rng.below(pages) + 1) * ps - if rng.chance(3) { rng.below(ps) } else { 0 },
            ),
            6 => {
                // The file header: signature, version, code page, key.
                let at = rng.below(0x80);
                if let Some(b) = m.get_mut(at) {
                    *b = rng.byte();
                }
            }
            7 => {
                // A piece of another database over this one.
                if let Some(o) = others.get(rng.below(others.len().max(1))) {
                    let from = rng.below(o.len());
                    let n = (1 + rng.below(512)).min(o.len() - from);
                    let at = near(rng);
                    for i in 0..n {
                        if let (Some(d), Some(s)) = (m.get_mut(at + i), o.get(from + i)) {
                            *d = *s;
                        }
                    }
                }
            }
            8 => {
                // Row offsets live right after the data page header: scramble a run of them.
                let at = base + 14 + rng.below(64);
                for i in 0..2 * (1 + rng.below(8)) {
                    if let Some(b) = m.get_mut(at + i) {
                        *b = rng.byte();
                    }
                }
            }
            _ => {
                let at = near(rng);
                if let Some(b) = m.get_mut(at) {
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

fn seeds() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    ["access-2000.mdb", "access-2016.accdb"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            ((*name).to_owned(), data)
        })
        .collect()
}

// ---- the invariants -------------------------------------------------------------------------

/// What reading a database gave: compared between two runs to check determinism.
#[derive(Debug, PartialEq, Eq)]
enum Walk {
    Refused(String),
    Read { objects: usize, tables: usize, rows: u64, deleted: u64, stopped: bool, digest: u64 },
}

fn mix(digest: &mut u64, v: u64) {
    *digest = digest.rotate_left(5) ^ v.wrapping_mul(0x9E37_79B9_7F4A_7C15);
}

fn walk(data: &[u8], what: &str) -> Walk {
    let db = match Db::open(data) {
        Ok(db) => db.with_work_limit(WORK),
        Err(OpenError::Encrypted) => return Walk::Refused("encrypted".into()),
        Err(OpenError::Malformed(why)) => return Walk::Refused(why.into()),
    };
    let Some((objects, cat)) = db.catalog_checked() else {
        return Walk::Read {
            objects: 0,
            tables: 0,
            rows: 0,
            deleted: 0,
            stopped: false,
            digest: 0,
        };
    };
    assert!(objects.len() <= CAP_OBJECTS, "{what}: {} objects", objects.len());
    let (mut tables, mut rows, mut deleted, mut digest) = (0usize, 0u64, cat.deleted, 0u64);
    let mut stopped = cat.stopped;
    for obj in objects.iter().filter(|o| o.kind == 1).take(MAX_TABLES) {
        mix(&mut digest, u64::from(obj.page));
        let Some(table) = db.table(obj.page) else { continue };
        tables += 1;
        assert!(table.columns.len() <= CAP_COLUMNS, "{what}: {} columns", table.columns.len());
        let mut seen = 0u64;
        let stats = db.rows(&table, |vals| {
            seen += 1;
            assert!(vals.len() <= CAP_COLUMNS, "{what}: a row of {} values", vals.len());
            for v in vals {
                match v {
                    Value::Text(s) | Value::Numeric(s) | Value::ExtDate(s) | Value::Guid(s) => {
                        assert!(s.len() <= CAP_TEXT, "{what}: a value of {} bytes", s.len());
                        mix(&mut digest, s.len() as u64);
                    }
                    Value::Int(i) | Value::Money(i) => {
                        mix(&mut digest, u64::from_le_bytes(i.to_le_bytes()))
                    }
                    Value::Binary(n) | Value::Ole(n) => mix(&mut digest, *n as u64),
                    _ => mix(&mut digest, 1),
                }
            }
            seen < MAX_ROWS
        });
        rows += seen;
        deleted += stats.deleted;
        stopped |= stats.stopped;
    }
    Walk::Read { objects: objects.len(), tables, rows, deleted, stopped, digest }
}

fn check(data: &[u8], what: &str) {
    let first = walk(data, what);
    assert_eq!(walk(data, what), first, "{what}: reading is not deterministic");
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
    panic!("jet mutation fuzz failure ({what}); {saved}\n{detail}");
}

fn env_u64(name: &str, default: u64) -> u64 {
    let Ok(text) = std::env::var(name) else { return default };
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.unwrap_or_else(|_| panic!("{name}={text:?} is not a number"))
}

#[test]
fn jet_mutation_fuzz_never_panics() {
    let iters = env_u64("ARCA_FUZZ_ITERS", DEFAULT_ITERS);
    let seed = env_u64("ARCA_FUZZ_SEED", DEFAULT_SEED);
    let seeds = seeds();
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();

    let mut rng = Rng::new(seed);
    for (name, body) in &seeds {
        check_or_save(body, &format!("{name} (seed)"));
        for i in 0..iters {
            check_or_save(&mutate(body, &bodies, &mut rng), &format!("{name} #{i}"));
        }
    }
}

#[test]
fn the_seeds_are_real_databases_with_rows() {
    // Otherwise the mutators would be exercising the refusal path only.
    for (name, body) in seeds() {
        match walk(&body, &name) {
            Walk::Read { tables, rows, .. } => {
                assert!(tables > 0 && rows > 0, "{name}: {tables} tables, {rows} rows")
            }
            Walk::Refused(why) => panic!("{name}: refused: {why}"),
        }
    }
}

#[test]
fn the_prng_and_mutators_are_deterministic() {
    let seeds = seeds();
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    let run = || {
        let mut rng = Rng::new(7);
        (0..12)
            .map(|i| mutate(&seeds[i % seeds.len()].1, &bodies, &mut rng))
            .map(|m| arca_digest::sha256(&m).to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn most_mutants_still_open_so_the_mutators_reach_the_row_reader() {
    let seeds = seeds();
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    let mut rng = Rng::new(11);
    let (mut opened, mut tried) = (0, 0);
    for (_, body) in &seeds {
        for _ in 0..20 {
            tried += 1;
            opened += usize::from(Db::open(&mutate(body, &bodies, &mut rng)).is_ok());
        }
    }
    assert!(opened * 2 > tried, "only {opened} of {tried} mutants open");
}
