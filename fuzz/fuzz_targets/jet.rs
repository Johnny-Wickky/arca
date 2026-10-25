// SPDX-License-Identifier: AGPL-3.0-only
//! The Jet / ACE database reader (`.mdb`, `.accdb`): open, catalog, table definitions and rows,
//! all of which follow page numbers and offsets written by the (hostile) author of the file.
//! Never a panic, a hang or unbounded output, and reading the same bytes twice gives the same
//! answer. The deterministic in-tree counterpart is `crates/arca-container/tests/jet_mutation.rs`.
#![no_main]

use arca_container::jet::{Db, OpenError, Value};
use libfuzzer_sys::fuzz_target;

/// Most tables read, rows per table and file size: keeps one execution fast.
const MAX_TABLES: usize = 6;
const MAX_ROWS: u64 = 1500;
const MAX_FILE: usize = 4 << 20;
/// The reader's documented caps.
const CAP_OBJECTS: usize = 100_000;
const CAP_COLUMNS: usize = 255;
const CAP_TEXT: usize = 3 * (4 << 20);
/// Work units one table scan may spend (the reader's own figure is higher).
const WORK: u64 = 60_000;

#[derive(Debug, PartialEq, Eq)]
enum Walk {
    Refused(String),
    Read { objects: usize, tables: usize, rows: u64, deleted: u64, stopped: bool, digest: u64 },
}

fn walk(data: &[u8]) -> Walk {
    let db = match Db::open(data) {
        Ok(db) => db.with_work_limit(WORK),
        Err(OpenError::Encrypted) => return Walk::Refused("encrypted".into()),
        Err(OpenError::Malformed(why)) => return Walk::Refused(why.into()),
    };
    let Some((objects, cat)) = db.catalog_checked() else {
        return Walk::Read { objects: 0, tables: 0, rows: 0, deleted: 0, stopped: false, digest: 0 };
    };
    assert!(objects.len() <= CAP_OBJECTS);
    let (mut tables, mut rows, mut deleted, mut digest) = (0usize, 0u64, cat.deleted, 0u64);
    let mut stopped = cat.stopped;
    for obj in objects.iter().filter(|o| o.kind == 1).take(MAX_TABLES) {
        let Some(table) = db.table(obj.page) else { continue };
        tables += 1;
        assert!(table.columns.len() <= CAP_COLUMNS);
        let mut seen = 0u64;
        let stats = db.rows(&table, |vals| {
            seen += 1;
            assert!(vals.len() <= CAP_COLUMNS);
            for v in vals {
                if let Value::Text(s) | Value::Numeric(s) | Value::ExtDate(s) | Value::Guid(s) = v {
                    assert!(s.len() <= CAP_TEXT);
                    digest = digest.rotate_left(5) ^ (s.len() as u64);
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

fuzz_target!(|data: &[u8]| {
    let mut d = data[..data.len().min(MAX_FILE)].to_vec();
    // Make the input look like a database, so that the fuzzer works on pages and not on
    // rediscovering the signature: two pages at least, the signature, a version the reader knows.
    let jet = d.get(4..19) == Some(&b"Standard Jet DB"[..]);
    let ace = d.get(4..19) == Some(&b"Standard ACE DB"[..]);
    if !jet && !ace {
        if d.len() < 8192 {
            d.resize(8192, 0);
        }
        let sig: &[u8] = if d[0] & 1 == 0 { b"Standard Jet DB" } else { b"Standard ACE DB" };
        d[4..19].copy_from_slice(sig);
        d[0x14] %= 7;
    }
    let first = walk(&d);
    assert_eq!(walk(&d), first);
});
