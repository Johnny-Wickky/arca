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
use crate::model::{Block, CellKind, FileFormat, NoticeKind, Paragraph, Run, SectionKind, style};
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

    // Extract SQL queries from MSysQueries if present (Issue #47)
    let msys_queries_obj = objects.iter().find(|o| o.name == "MSysQueries");
    let msys_queries_table = msys_queries_obj.and_then(|o| db.table(o.page));
    let mut queries_map: std::collections::BTreeMap<i64, Vec<QueryRow>> = std::collections::BTreeMap::new();
    if let Some(q_table) = &msys_queries_table {
        let col = |name: &str| q_table.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name));
        let (c_obj, c_attr, c_expr, c_n1, c_n2, c_ord, c_flag) = (
            col("ObjectId"),
            col("Attribute"),
            col("Expression"),
            col("Name1"),
            col("Name2"),
            col("Order"),
            col("Flag"),
        );
        let int_val = |vals: &[Value], i: Option<usize>| -> i64 {
            i.and_then(|idx| vals.get(idx)).and_then(|v| match v {
                Value::Int(n) => Some(*n),
                _ => None,
            }).unwrap_or(0)
        };
        let str_val = |vals: &[Value], i: Option<usize>| -> String {
            i.and_then(|idx| vals.get(idx)).and_then(|v| match v {
                Value::Text(s) => Some(s.clone()),
                _ => None,
            }).unwrap_or_default()
        };
        db.rows(q_table, |vals| {
            let object_id = int_val(vals, c_obj);
            let attribute = int_val(vals, c_attr);
            let expression = str_val(vals, c_expr);
            let name1 = str_val(vals, c_n1);
            let name2 = str_val(vals, c_n2);
            let order = int_val(vals, c_ord);
            let flag = int_val(vals, c_flag);
            queries_map.entry(object_id).or_default().push(QueryRow {
                attribute,
                expression,
                name1,
                name2,
                order,
                flag,
            });
            true
        });
    }

    for o in shown.iter().filter(|o| o.kind == kind::QUERY) {
        if sink.is_full() {
            break;
        }
        let hidden = o.flags & HIDDEN_FLAG != 0;
        let Some(sec) = sink.open_section(SectionKind::Sheet, &format!("Query: {}", o.name), hidden) else { break };
        let sql_text = if let Some(mut rows) = queries_map.remove(&o.id) {
            format_query(&mut rows)
        } else {
            String::from("-- (query definition not found in MSysQueries)")
        };
        let mut p = Paragraph::default();
        p.runs.push(Run { text: sql_text, style: style::PLAIN });
        sink.push(sec, Block::Para(p));
    }

    if deleted_total > 0 {
        sink.count(NoticeKind::DeletedRecords, "", deleted_total);
    }
    Ok(())
}

/// The position of the objects that are not listed.
const OTHER: u8 = 8;

/// Position of a kind of object in the overview.

#[derive(Debug, Clone, Default)]
struct QueryRow {
    attribute: i64,
    expression: String,
    name1: String,
    name2: String,
    order: i64,
    flag: i64,
}

fn format_query(rows: &mut [QueryRow]) -> String {
    rows.sort_by_key(|r| (r.attribute, r.order));
    let mut select_parts = Vec::new();
    let mut from_parts = Vec::new();
    let mut where_parts = Vec::new();
    let mut group_parts = Vec::new();
    let mut having_parts = Vec::new();
    let mut order_parts = Vec::new();

    for r in rows.iter() {
        match r.attribute {
            // 1: SELECT
            1 => {
                let part = if !r.expression.is_empty() {
                    if !r.name1.is_empty() && r.name1 != r.expression {
                        format!("{} AS [{}]", r.expression, r.name1)
                    } else {
                        r.expression.clone()
                    }
                } else if !r.name1.is_empty() {
                    format!("[{}]", r.name1)
                } else {
                    String::new()
                };
                if !part.is_empty() {
                    select_parts.push(part);
                }
            }
            // 3: FROM / tables
            3 => {
                let part = if !r.name1.is_empty() {
                    if !r.name2.is_empty() {
                        format!("[{}].[{}]", r.name2, r.name1)
                    } else {
                        format!("[{}]", r.name1)
                    }
                } else if !r.expression.is_empty() {
                    r.expression.clone()
                } else {
                    String::new()
                };
                if !part.is_empty() {
                    from_parts.push(part);
                }
            }
            // 5: WHERE
            5 => {
                if !r.expression.is_empty() {
                    where_parts.push(r.expression.clone());
                }
            }
            // 6: GROUP BY
            6 => {
                if !r.expression.is_empty() {
                    group_parts.push(r.expression.clone());
                }
            }
            // 7: HAVING
            7 => {
                if !r.expression.is_empty() {
                    having_parts.push(r.expression.clone());
                }
            }
            // 8: ORDER BY
            8 => {
                let part = if !r.expression.is_empty() {
                    if r.flag & 1 != 0 {
                        format!("{} DESC", r.expression)
                    } else {
                        r.expression.clone()
                    }
                } else {
                    String::new()
                };
                if !part.is_empty() {
                    order_parts.push(part);
                }
            }
            _ => {
                if !r.expression.is_empty() && select_parts.is_empty() && from_parts.is_empty() {
                    return r.expression.clone();
                }
            }
        }
    }

    if select_parts.is_empty() && from_parts.is_empty() {
        return String::from("-- (empty or binary query definition)");
    }

    let mut sql = String::from("SELECT ");
    sql.push_str(&select_parts.join(", "));
    if !from_parts.is_empty() {
        sql.push_str("\nFROM ");
        sql.push_str(&from_parts.join(", "));
    }
    if !where_parts.is_empty() {
        sql.push_str("\nWHERE ");
        sql.push_str(&where_parts.join(" AND "));
    }
    if !group_parts.is_empty() {
        sql.push_str("\nGROUP BY ");
        sql.push_str(&group_parts.join(", "));
    }
    if !having_parts.is_empty() {
        sql.push_str("\nHAVING ");
        sql.push_str(&having_parts.join(" AND "));
    }
    if !order_parts.is_empty() {
        sql.push_str("\nORDER BY ");
        sql.push_str(&order_parts.join(", "));
    }
    sql.push(';');
    sql
}

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
        "forms, reports, macros and modules are listed, not shown or run; queries are displayed as safe read-only SQL text",
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
