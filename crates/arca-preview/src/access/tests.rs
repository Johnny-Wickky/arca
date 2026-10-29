//! Tests for the Access reader. The reference is Jackcess, an independent implementation:
//! `tools/fixtures/access/DumpDb.java` writes every table of a database as canonical tokens, and
//! [`dump`] writes the same from this reader. The expected `.dump` files sit next to the
//! databases in `tests/fixtures`.

use super::db::{Db, Value};
use super::{date_parts, kind};
use std::fmt::Write as _;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn token(v: &Value) -> String {
    match v {
        Value::Null => "~".into(),
        Value::Bool(b) => format!("b{}", u8::from(*b)),
        Value::Int(i) => format!("i{i}"),
        Value::Single(f) => format!("f{:x}", f.to_bits()),
        Value::Double(f) => format!("d{:x}", f.to_bits()),
        Value::Money(m) => format!("m{m}"),
        Value::Date(d) => match date_parts(*d) {
            Some((y, m, dd, ms)) => format!(
                "t{y:04}-{m:02}-{dd:02} {:02}:{:02}:{:02}.{:03}",
                ms / 3_600_000,
                ms / 60_000 % 60,
                ms / 1000 % 60,
                ms % 1000
            ),
            None => "t?".into(),
        },
        Value::Text(s) => format!("s{}", esc(s)),
        Value::Link(t, a) => format!("s{}#{}#", esc(t), esc(a)),
        Value::Binary(n) => format!("x{n}"),
        Value::Ole(n) => format!("o{n}"),
        Value::Guid(s) => format!("g{s}"),
        Value::Numeric(s) => format!("n{s}"),
        Value::Complex(_) => "c".into(),
        Value::Blob(b) => format!("x{}", b.len()),
        Value::ExtDate(s) => format!("e{s}"),
    }
}

/// Every user table of the database in the format of `DumpDb.java`.
pub(super) fn dump(data: &[u8]) -> String {
    let db = Db::open(data).expect("opens");
    let mut tables: Vec<_> = db
        .catalog()
        .expect("catalog")
        .into_iter()
        .filter(|o| {
            o.kind == kind::TABLE
                && o.flags & super::SYSTEM_FLAGS == 0
                && !o.name.starts_with("MSys")
        })
        .collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for o in tables {
        let t = db.table(o.page).expect("table");
        writeln!(out, "T {}", o.name).unwrap();
        out.push('C');
        for c in &t.columns {
            write!(out, "\t{}:{}", esc(&c.name), c.ty).unwrap();
        }
        out.push('\n');
        db.rows(&t, |vals| {
            out.push('R');
            for v in vals {
                out.push('\t');
                out.push_str(&token(v));
            }
            out.push('\n');
            true
        });
    }
    out
}

/// Databases and their expected dumps in `tests/fixtures`, plus any named by
/// `ARCA_ACCESS_ORACLE_DIR` (a directory of `x.mdb` + `x.mdb.dump` pairs).
fn pairs() -> Vec<(String, Vec<u8>, String)> {
    let mut dirs = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures"
    ))];
    if let Ok(extra) = std::env::var("ARCA_ACCESS_ORACLE_DIR") {
        dirs.push(extra.into());
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let name =
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !(name.ends_with(".mdb") || name.ends_with(".accdb")) {
                continue;
            }
            let Ok(expected) = std::fs::read_to_string(dir.join(format!("{name}.dump"))) else {
                continue;
            };
            out.push((name, std::fs::read(&path).expect("reads"), expected));
        }
    }
    out
}

#[test]
fn every_database_reads_like_jackcess() {
    let pairs = pairs();
    assert!(pairs.len() >= FIXTURES.len(), "the databases and their dumps are missing");
    let mut failures = Vec::new();
    for (name, data, expected) in &pairs {
        let got = dump(data);
        if &got != expected {
            let line = got
                .lines()
                .zip(expected.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| got.lines().count().min(expected.lines().count()));
            failures.push(format!(
                "{name}: line {}\n  got      {:?}\n  expected {:?}",
                line + 1,
                got.lines().nth(line).map(|l| l.chars().take(300).collect::<String>()),
                expected.lines().nth(line).map(|l| l.chars().take(300).collect::<String>()),
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn fixture(name: &str) -> Vec<u8> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn preview_of(data: &[u8]) -> Result<crate::model::Preview, crate::PreviewError> {
    crate::preview(data, &crate::PreviewPolicy::default())
}

const FIXTURES: [&str; 2] = ["access-2000.mdb", "access-2016.accdb"];

#[test]
fn tables_become_sheets_and_objects_are_listed() {
    use crate::model::{Block, FileFormat, NoticeKind, SectionKind};
    for name in FIXTURES {
        let p = preview_of(&fixture(name)).unwrap();
        assert_eq!(
            p.format,
            if name.ends_with(".mdb") { FileFormat::AccessJet4 } else { FileFormat::AccessAce },
            "{name}"
        );
        let names: Vec<_> = p.sections.iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (SectionKind::Body, "Database"),
                (SectionKind::Sheet, "Customers"),
                (SectionKind::Sheet, "Everything"),
                (SectionKind::Sheet, "Orders"),
                (SectionKind::Sheet, "Schema"),
            ],
            "{name}"
        );
        // The overview says how many rows each table has.
        let Some(Block::Grid(overview)) = p.sections[0].blocks.first() else { panic!("no grid") };
        let line = |r: &crate::model::GridRow| {
            r.cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>().join("|")
        };
        let lines: Vec<_> = overview.rows.iter().map(line).collect();
        assert!(lines.contains(&"Table|Customers|24".to_owned()), "{name}: {lines:?}");
        assert!(lines.contains(&"Table|Orders|206".to_owned()), "{name}: {lines:?}");
        // The rows deleted from `Orders` are still in the file; they are counted, not shown.
        let deleted =
            p.notices.iter().find(|n| n.kind == NoticeKind::DeletedRecords).map(|n| n.count);
        assert_eq!(deleted, Some(34), "{name}");
        let Some(Block::Grid(orders)) = p.sections[3].blocks.first() else { panic!("no grid") };
        assert_eq!(orders.rows.len(), 1 + 206, "{name}");
        // The schema lists every column with its type; the autonumber key is named.
        let Some(Block::Grid(schema)) = p.sections[4].blocks.first() else { panic!("no grid") };
        let columns: Vec<_> = schema.rows.iter().map(line).collect();
        assert!(
            columns.iter().any(|l| l.starts_with("Customers|ID|AutoNumber|4|")),
            "{name}: {columns:?}"
        );
        assert!(
            columns.iter().any(|l| l.starts_with("Customers|Name|Short Text|")),
            "{name}: {columns:?}"
        );
    }
}

#[test]
fn values_are_shown_the_way_access_shows_them() {
    use crate::model::{Block, CellKind};
    let p = preview_of(&fixture("access-2016.accdb")).unwrap();
    let Some(Block::Grid(g)) = p.sections[1].blocks.first() else { panic!("no grid") };
    let row = |n: u32| g.rows.iter().find(|r| r.index == n).unwrap();
    let cell = |r: &crate::model::GridRow, col: u32| r.cells.iter().find(|c| c.col == col).cloned();
    // Row 2 is the first customer: autonumber, text, a date without a time, currency, yes/no.
    let first = row(2);
    assert_eq!(cell(first, 1).unwrap().text, "Alice Smith");
    assert_eq!(cell(first, 2).unwrap().text, "2020-01-01");
    assert_eq!(cell(first, 2).unwrap().kind, CellKind::Date);
    assert_eq!(cell(first, 3).unwrap().text, "0.05");
    assert_eq!(cell(first, 4).unwrap().text, "FALSE");
    assert_eq!(cell(first, 4).unwrap().kind, CellKind::Bool);
    // A null value is an empty cell; a hyperlink shows its text and keeps the address aside.
    assert!(cell(first, 5).is_none());
    let link = cell(first, 6).unwrap();
    assert_eq!(link.text, "Arca 0");
    assert_eq!(link.href, "https://example.org/c/0#section");
    // Cyrillic text and a long memo that lives on other pages are read whole.
    assert_eq!(cell(row(4), 1).unwrap().text, "Иван Иванов");
    let long = cell(row(5), 5).unwrap().text;
    assert!(long.starts_with("Record 3 paragraph 0: the quick brown fox"));
    assert_eq!(long.chars().count(), 8200);
    // The `Everything` table: every type, nulls and extremes.
    let Some(Block::Grid(all)) = p.sections[2].blocks.first() else { panic!("no grid") };
    let full = all.rows.iter().find(|r| r.index == 2).unwrap();
    let text = |col: u32| cell(full, col).unwrap().text;
    assert_eq!(text(0), "200");
    assert_eq!(text(1), "-1234");
    assert_eq!(text(3), "-12345.6789");
    assert_eq!(text(4), "3.14159");
    assert_eq!(text(6), "06:00:00");
    assert_eq!(text(8), "(binary, 5 bytes)");
    assert_eq!(text(9), "(object, 9000 bytes)");
    assert_eq!(text(11), "{12345678-9ABC-DEF0-1234-56789ABCDEF0}");
    assert_eq!(text(12), "-98765.432");
    // A row whose values are all null still has its yes/no value.
    let nulls = all.rows.iter().find(|r| r.index == 4).unwrap();
    assert_eq!(nulls.cells.len(), 1);
}

/// Flips a bit of the (obfuscated) header at `at`.
fn flipped(mut data: Vec<u8>, at: usize) -> Vec<u8> {
    data[at] ^= 1;
    data
}

#[test]
fn encrypted_and_password_protected_databases_are_reported_not_opened() {
    use crate::PreviewError;
    for name in FIXTURES {
        let data = fixture(name);
        assert!(preview_of(&data).is_ok(), "{name}");
        // The key of the page encryption.
        assert_eq!(preview_of(&flipped(data.clone(), 62)).unwrap_err(), PreviewError::Encrypted);
        // The password stored in the header.
        assert_eq!(preview_of(&flipped(data, 66)).unwrap_err(), PreviewError::Encrypted);
    }
}

#[test]
fn damaged_databases_are_errors_or_partial_never_panics() {
    for name in FIXTURES {
        let data = fixture(name);
        // Cut anywhere: the preview is an error or whatever could be read.
        for len in
            [0, 1, 18, 21, 4096, 4096 * 2, 4096 * 3, 4096 * 3 + 17, data.len() / 2, data.len() - 1]
        {
            let _ = preview_of(&data[..len.min(data.len())]);
        }
        // Random damage anywhere in the file, deterministically.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..300 {
            let mut m = data.clone();
            for _ in 0..1 + next() % 40 {
                let at = usize::try_from(next()).unwrap() % m.len();
                m[at] = u8::try_from(next() % 256).unwrap();
            }
            let _ = preview_of(&m);
        }
        // Damage concentrated on the structures: header, catalog, first table definitions.
        let rounds =
            std::env::var("ARCA_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
        for _ in 0..rounds {
            let mut m = data.clone();
            let page = usize::try_from(next() % 8).unwrap();
            for _ in 0..1 + next() % 12 {
                let at = page * 4096 + usize::try_from(next()).unwrap() % 4096;
                if let Some(b) = m.get_mut(at) {
                    *b = u8::try_from(next() % 256).unwrap();
                }
            }
            let _ = preview_of(&m);
        }
    }
}

#[test]
fn a_small_budget_truncates_the_grid_and_says_so() {
    use crate::model::NoticeKind;
    let policy = crate::PreviewPolicy { max_grid_cells: 100, ..crate::PreviewPolicy::default() };
    let p = crate::preview(&fixture("access-2000.mdb"), &policy).unwrap();
    assert!(p.stats.truncated);
    assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Truncated));
}

#[test]
fn the_relationship_diagram_names_every_table_and_only_one_page_exists() {
    use crate::page::Prim;
    for name in FIXTURES {
        let data = fixture(name);
        let page = crate::render_page(&data, 0, &crate::PreviewPolicy::default()).unwrap();
        assert_eq!(page.pages, 1, "{name}");
        assert!(page.width > 100.0 && page.width < 20_000.0, "{name}: {}", page.width);
        assert!(page.height > 100.0 && page.height < 20_000.0, "{name}: {}", page.height);
        let texts: Vec<&str> = page
            .prims
            .iter()
            .filter_map(
                |p| if let Prim::Text { text, .. } = p { Some(text.as_str()) } else { None },
            )
            .collect();
        for table in ["Customers", "Orders", "Everything"] {
            assert!(texts.contains(&table), "{name}: {table} not drawn in {texts:?}");
        }
        // The key column of a table is listed in its box.
        assert!(texts.contains(&"ID"), "{name}: {texts:?}");
        assert!(
            crate::render_page(&data, 1, &crate::PreviewPolicy::default()).is_err(),
            "{name}: there is no second page"
        );
    }
}
