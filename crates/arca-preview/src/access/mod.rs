//! Microsoft Access databases (`.mdb`, `.accdb`): Jet 3 (Access 97), Jet 4 (2000–2003) and ACE
//! (2007 and later).
//!
//! The preview lists the objects of the database and shows each table as a grid of the *stored*
//! values:
//!
//! * the data of a table is read page by page; indexes, relationships and validation rules are
//!   not used;
//! * queries, forms, reports, macros and modules are **listed, never shown or run** — a query is
//!   an SQL text that could reach other files, a macro or module is code;
//! * linked tables are listed; the database they point to is not opened;
//! * memo text and hyperlinks are shown (the address is never followed); OLE objects, binary
//!   values and attachments are replaced by their size;
//! * rows marked deleted are not shown, but their number is reported ([`NoticeKind::DeletedRecords`]):
//!   Access keeps them in the file until the database is compacted;
//! * databases that are encrypted or password protected are reported, not opened.

mod db;

use crate::PreviewError;
use crate::model::{Block, CellKind, FileFormat, NoticeKind, SectionKind};
use crate::numfmt;
use crate::sink::{GridBuilder, Sink};
use db::{Db, Object, OpenError, Table, Value};

/// Object types of the system catalog.
mod kind {
    pub(super) const TABLE: i64 = 1;
    pub(super) const LINKED_ODBC: i64 = 4;
    pub(super) const QUERY: i64 = 5;
    pub(super) const LINKED: i64 = 6;
    pub(super) const FORM: i64 = -32768;
    pub(super) const REPORT: i64 = -32764;
    pub(super) const MACRO: i64 = -32766;
    pub(super) const MODULE: i64 = -32761;
    pub(super) const PAGE: i64 = -32758;
}

/// Catalog flag: the object belongs to the system.
const SYSTEM_FLAGS: u32 = 0x8000_0002;
/// Catalog flag: the object is hidden in the navigation pane.
const HIDDEN_FLAG: u32 = 0x08;

/// Most objects listed.
const MAX_OBJECTS: usize = 4096;

/// Shows the database in `data`.
pub(crate) fn extract(
    data: &[u8],
    _format: FileFormat,
    sink: &mut Sink<'_>,
) -> Result<(), PreviewError> {
    let db = Db::open(data).map_err(|e| match e {
        OpenError::Encrypted => PreviewError::Encrypted,
        OpenError::Malformed(why) => PreviewError::Malformed(why.to_owned()),
    })?;
    let objects = db
        .catalog()
        .ok_or_else(|| PreviewError::Malformed("the system catalog is unreadable".into()))?;

    let user = |o: &Object| {
        order(o.kind) < OTHER
            && o.flags & SYSTEM_FLAGS == 0
            && !o.name.starts_with("MSys")
            && !o.name.starts_with('~')
    };
    let mut shown: Vec<&Object> = objects.iter().filter(|o| user(o)).collect();
    shown.sort_by_key(|o| (order(o.kind), o.name.to_lowercase()));
    shown.truncate(MAX_OBJECTS);

    // The tables first, so the overview can say how many rows each has.
    let mut tables: Vec<(&Object, Option<Table>)> = Vec::new();
    for o in shown.iter().filter(|o| o.kind == kind::TABLE) {
        tables.push((o, db.table(o.page)));
    }

    overview(sink, &shown, &tables);

    let mut deleted_total = 0u64;
    for (o, table) in &tables {
        let hidden = o.flags & HIDDEN_FLAG != 0;
        let Some(table) = table else {
            sink.skipped(&format!("table {}", o.name));
            continue;
        };
        let Some(sec) = sink.open_section(SectionKind::Sheet, &o.name, hidden) else { break };
        let mut grid = GridBuilder::new(sink.policy, sink.items_left());
        let mut row_no = 1u32;
        if grid.row(row_no, false) {
            for (i, c) in table.columns.iter().enumerate() {
                let col = u32::try_from(i).unwrap_or(u32::MAX);
                if !grid.cell(col, CellKind::Text, c.name.clone()) {
                    break;
                }
            }
        }
        let stats = db.rows(table, |vals| {
            row_no = row_no.saturating_add(1);
            if !grid.row(row_no, false) {
                return false;
            }
            for (i, v) in vals.iter().enumerate() {
                let Some((kind, text, href)) = cell(v) else { continue };
                let col = u32::try_from(i).unwrap_or(u32::MAX);
                if !grid.linked_cell(col, kind, text, href) {
                    return false;
                }
            }
            true
        });
        deleted_total = deleted_total.saturating_add(stats.deleted);
        let grid = grid.finish();
        if grid.truncated || stats.stopped {
            sink.truncated("grid");
        }
        sink.push(sec, Block::Grid(grid));
        if sink.is_full() {
            break;
        }
    }
    if deleted_total > 0 {
        sink.count(NoticeKind::DeletedRecords, "", deleted_total);
    }
    Ok(())
}

/// The position of the objects that are not listed.
const OTHER: u8 = 8;

/// Position of a kind of object in the overview.
const fn order(kind: i64) -> u8 {
    match kind {
        kind::TABLE => 0,
        kind::LINKED | kind::LINKED_ODBC => 1,
        kind::QUERY => 2,
        kind::FORM => 3,
        kind::REPORT => 4,
        kind::MACRO => 5,
        kind::MODULE => 6,
        kind::PAGE => 7,
        _ => OTHER,
    }
}

/// The name of a kind of object in the overview.
const fn kind_name(kind: i64) -> &'static str {
    match kind {
        kind::TABLE => "Table",
        kind::LINKED => "Linked table",
        kind::LINKED_ODBC => "Linked table (ODBC)",
        kind::QUERY => "Query",
        kind::FORM => "Form",
        kind::REPORT => "Report",
        kind::MACRO => "Macro",
        kind::MODULE => "Module",
        kind::PAGE => "Data access page",
        _ => "Object",
    }
}

/// Lists the objects of the database.
fn overview(sink: &mut Sink<'_>, objects: &[&Object], tables: &[(&Object, Option<Table>)]) {
    let Some(sec) = sink.open_section(SectionKind::Body, "Database", false) else { return };
    let mut grid = GridBuilder::new(sink.policy, sink.items_left());
    let mut row = 1u32;
    let mut full = !grid.row(row, false);
    for (col, head) in ["Object", "Name", "Rows"].into_iter().enumerate() {
        if full {
            break;
        }
        full = !grid.cell(u32::try_from(col).unwrap_or(0), CellKind::Text, head.to_owned());
    }
    let mut hidden_listed = 0u64;
    let mut code = false;
    for o in objects {
        if full {
            break;
        }
        row = row.saturating_add(1);
        if !grid.row(row, o.flags & HIDDEN_FLAG != 0) {
            break;
        }
        hidden_listed += u64::from(o.flags & HIDDEN_FLAG != 0);
        code |= matches!(o.kind, kind::MACRO | kind::MODULE);
        let rows = tables
            .iter()
            .find(|(t, _)| std::ptr::eq(*t, *o))
            .and_then(|(_, t)| t.as_ref())
            .map(|t| t.row_count.to_string());
        let ok = grid.cell(0, CellKind::Text, kind_name(o.kind).to_owned())
            && grid.cell(1, CellKind::Text, o.name.clone())
            && rows.is_none_or(|r| grid.cell(2, CellKind::Number, r));
        if !ok {
            break;
        }
    }
    let grid = grid.finish();
    if grid.truncated {
        sink.truncated("objects");
    }
    sink.push(sec, Block::Grid(grid));
    sink.note(
        NoticeKind::Unsupported,
        "queries, forms, reports, macros and modules are listed, not shown or run",
    );
    if code {
        sink.note(NoticeKind::Unsupported, "macros and modules are code and are never run");
    }
    if hidden_listed > 0 {
        sink.count(NoticeKind::HiddenShown, "objects", hidden_listed);
    }
}

/// How a value is shown: its kind, the text and the address of a link.
fn cell(v: &Value) -> Option<(CellKind, String, String)> {
    let plain = |kind, text: String| Some((kind, text, String::new()));
    match v {
        Value::Null => None,
        Value::Bool(b) => plain(CellKind::Bool, if *b { "TRUE" } else { "FALSE" }.to_owned()),
        Value::Int(i) => plain(CellKind::Number, i.to_string()),
        Value::Single(f) => {
            // The shortest text that reads back as the same single-precision number.
            let shortest = f.to_string().parse::<f64>().ok()?;
            plain(CellKind::Number, numfmt::general(shortest))
        }
        Value::Double(f) => {
            if f.is_finite() {
                plain(CellKind::Number, numfmt::general(*f))
            } else {
                plain(CellKind::Error, "#NUM!".to_owned())
            }
        }
        Value::Money(m) => plain(CellKind::Number, money(*m)),
        Value::Date(d) => plain(CellKind::Date, date(*d)),
        Value::Text(s) => plain(CellKind::Text, s.clone()),
        Value::Link(text, address) => Some((CellKind::Text, text.clone(), address.clone())),
        Value::Binary(n) => plain(CellKind::Text, format!("(binary, {n} bytes)")),
        Value::Ole(n) => plain(CellKind::Text, format!("(object, {n} bytes)")),
        Value::Guid(s) | Value::Numeric(s) => plain(CellKind::Text, s.clone()),
        Value::Complex => plain(CellKind::Text, "(attachments or multiple values)".to_owned()),
        Value::ExtDate(s) => plain(CellKind::Date, s.clone()),
    }
}

/// Currency with at least two decimals: `-1234.50`.
fn money(m: i64) -> String {
    let neg = m < 0;
    let abs = m.unsigned_abs();
    let (int, frac) = (abs / 10_000, abs % 10_000);
    let mut frac = format!("{frac:04}");
    while frac.len() > 2 && frac.ends_with('0') {
        frac.pop();
    }
    format!("{}{int}.{frac}", if neg { "-" } else { "" })
}

/// Splits a date from days since 1899-12-30 into year, month, day and milliseconds of the day.
/// Like Access, the time is a positive fraction of the day even for dates before the start.
fn date_parts(v: f64) -> Option<(i64, u32, u32, i64)> {
    if !v.is_finite() || v.abs() > 3.0e6 {
        return None;
    }
    // Whole days (towards zero) and the time of day, in milliseconds.
    #[allow(clippy::cast_possible_truncation)]
    let days = v.trunc() as i64;
    #[allow(clippy::cast_possible_truncation)]
    let ms = (v.abs().fract() * 86_400_000.0).round() as i64;
    let days = days.saturating_add(ms / 86_400_000);
    let (y, m, d) = numfmt::civil_from_days(days.saturating_sub(25_569));
    Some((y, m, d, ms % 86_400_000))
}

/// A date and time from days since 1899-12-30: the date, the time, or both.
fn date(v: f64) -> String {
    let Some((y, m, d, ms)) = date_parts(v) else { return "#NUM!".to_owned() };
    let (h, mi, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    if ms == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else if v.trunc() == 0.0 {
        format!("{h:02}:{mi:02}:{s:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests;
