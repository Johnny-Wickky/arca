//! Microsoft Access databases (`.mdb`, `.accdb`): Jet 3 (Access 97), Jet 4 (2000–2003) and ACE
//! (2007 and later).
//!
//! The preview lists the objects of the database and shows each table as a grid of the *stored*
//! values:
//!
//! * the data of a table is read page by page; the indexes are listed (names and columns) but not
//!   used, and validation rules are not applied;
//! * queries are shown as SQL text rebuilt from the rows Access stores for them, marked
//!   [`crate::model::style::SYNTHETIC`] (see [`query`]); when a query does not fit the documented
//!   layout it is shown as the stored rows instead. No query is ever run, no expression is
//!   evaluated. Forms, reports, macros, modules and data access pages are **listed by name only**
//!   (a macro or module is code);
//! * relationships and indexes are listed;
//! * linked tables are listed; the database they point to is not opened;
//! * memo text and hyperlinks are shown (the address is never followed); of an OLE object or a
//!   binary value only the size is shown; attachments are shown by name and stored size, never
//!   extracted; multi-value fields show their values; a calculated column shows the result Access
//!   stored, which is not recalculated;
//! * rows marked deleted are not shown, but their number is reported ([`NoticeKind::DeletedRecords`]):
//!   Access keeps them in the file until the database is compacted;
//! * databases that are encrypted or password protected are reported, not opened.

mod db;
mod query;

use crate::PreviewError;
use crate::model::{Block, CellKind, FileFormat, NoticeKind, Paragraph, Run, SectionKind, style};
use crate::numfmt;
use crate::sink::{GridBuilder, Sink};
use db::{
    ComplexData, ComplexItem, ComplexKind, Db, IndexKind, Object, OpenError, PropValue, Table,
    Value, ty,
};
use query::{Outcome, QueryKind, QueryRow, reconstruct};
use std::collections::BTreeMap;

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
/// Most rows of `MSysQueries` read.
const MAX_QUERY_ROWS: usize = 200_000;
/// Most rows kept for one query.
const MAX_QUERY_ROWS_PER_OBJECT: usize = 4096;
/// Most bytes of query text kept in all.
const MAX_QUERY_BYTES: usize = 4 << 20;
/// Most characters of one text field of a query row; a longer one makes the query unreadable
/// (it is never cut, because a cut statement would be a different statement).
const MAX_QUERY_FIELD_CHARS: usize = 64 * 1024;
/// Most relationships listed.
const MAX_RELATIONSHIPS: usize = 4096;
/// Most rows of `MSysRelationships` read.
const MAX_RELATIONSHIP_ROWS: usize = 100_000;
/// Most values shown in one cell of a complex field.
const MAX_COMPLEX_IN_CELL: usize = 1000;

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
    let (objects, catalog_stats) = db
        .catalog_checked()
        .ok_or_else(|| PreviewError::Malformed("the system catalog is unreadable".into()))?;
    if catalog_stats.stopped {
        sink.truncated("catalog");
    }

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
    let mut calculated = false;
    let mut complex_stopped = false;
    for (o, table) in &tables {
        let hidden = o.flags & HIDDEN_FLAG != 0;
        let Some(table) = table else {
            sink.skipped(&format!("table {}", o.name));
            continue;
        };
        let Some(sec) = sink.open_section(SectionKind::Sheet, &o.name, hidden) else { break };
        calculated |= table.columns.iter().any(db::Column::is_calculated);
        // The values of the complex columns, read once for the whole table.
        let complex: Vec<Option<ComplexData>> = table
            .columns
            .iter()
            .map(|c| {
                if c.ty == ty::COMPLEX { db.complex_data(&objects, o.page, c) } else { None }
            })
            .collect();
        complex_stopped |= complex.iter().flatten().any(|d| d.stopped);
        let mut grid =
            GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
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
                let shown = match v {
                    Value::Complex(key) => complex_cell(complex.get(i).and_then(Option::as_ref), *key),
                    other => cell(other),
                };
                let Some((kind, text, href)) = shown else { continue };
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
    if complex_stopped {
        sink.truncated("complex values");
    }
    if calculated {
        sink.note(
            NoticeKind::FormulasNotEvaluated,
            "access: calculated columns show the result Access stored; their expressions are not evaluated",
        );
    }

    indexes_section(sink, &tables);
    relationships_section(sink, &db, &objects);
    queries_section(sink, &db, &objects, &shown);
    startup_notes(sink, &db);

    if deleted_total > 0 {
        sink.count(NoticeKind::DeletedRecords, "", deleted_total);
    }
    Ok(())
}

/// The position of the objects that are not listed.
const OTHER: u8 = 8;

fn int_of(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}

fn text_of(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::Text(s)) => Some(s),
        _ => None,
    }
}

/// Short text for a notice: control characters removed, at most 80 characters.
fn short(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(80).collect()
}

/// The saved rows of one query.
#[derive(Default)]
struct Collected {
    rows: Vec<QueryRow>,
    /// Some of the rows could not be kept whole (a limit was reached), so the statement cannot be
    /// rebuilt.
    incomplete: bool,
}

/// Reads the rows of `MSysQueries` of the queries in `wanted`, in the order the file stores them.
fn read_query_rows(
    sink: &mut Sink<'_>,
    db: &Db<'_>,
    q_table: &Table,
    wanted: &std::collections::BTreeSet<i64>,
) -> BTreeMap<i64, Collected> {
    let mut out: BTreeMap<i64, Collected> = BTreeMap::new();
    let col = |name: &str| q_table.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name));
    let (c_obj, c_attr, c_expr, c_n1, c_n2, c_flag, c_extra, c_ord) = (
        col("ObjectId"),
        col("Attribute"),
        col("Expression"),
        col("Name1"),
        col("Name2"),
        col("Flag"),
        col("LvExtra"),
        col("Order"),
    );
    let mut seen_rows = 0usize;
    let mut kept_bytes = 0usize;
    let mut limit_hit = false;
    let stats = db.rows_raw(q_table, |vals| {
        seen_rows += 1;
        if seen_rows > MAX_QUERY_ROWS {
            limit_hit = true;
            return false;
        }
        let Some(object_id) = c_obj.and_then(|i| int_of(vals.get(i))) else { return true };
        if !wanted.contains(&object_id) {
            return true;
        }
        let entry = out.entry(object_id).or_default();
        if entry.incomplete {
            return true;
        }
        let mut oversize = false;
        let mut field = |i: Option<usize>| -> Option<String> {
            let s = i.and_then(|i| text_of(vals.get(i)))?;
            // Counted in bytes: a character is at least one.
            if s.len() > MAX_QUERY_FIELD_CHARS * 4 || s.chars().count() > MAX_QUERY_FIELD_CHARS {
                oversize = true;
                return None;
            }
            Some(s.to_owned())
        };
        let row = QueryRow {
            attribute: c_attr.and_then(|i| int_of(vals.get(i))).unwrap_or(0),
            expression: field(c_expr),
            flag: c_flag.and_then(|i| int_of(vals.get(i))),
            extra: c_extra.and_then(|i| int_of(vals.get(i))),
            name1: field(c_n1),
            name2: field(c_n2),
            order: match c_ord.and_then(|i| vals.get(i)) {
                Some(Value::Blob(b)) => b.clone(),
                _ => Vec::new(),
            },
        };
        if oversize {
            entry.incomplete = true;
            return true;
        }
        kept_bytes = kept_bytes
            .saturating_add(row.expression.as_ref().map_or(0, String::len))
            .saturating_add(row.name1.as_ref().map_or(0, String::len))
            .saturating_add(row.name2.as_ref().map_or(0, String::len));
        if kept_bytes > MAX_QUERY_BYTES {
            limit_hit = true;
            return false;
        }
        if entry.rows.len() >= MAX_QUERY_ROWS_PER_OBJECT {
            entry.incomplete = true;
            return true;
        }
        entry.rows.push(row);
        true
    });
    if limit_hit || stats.stopped {
        sink.truncated("queries");
        // Rows after the stopping point are missing: no query is complete any more.
        for c in out.values_mut() {
            c.incomplete = true;
        }
    }
    out
}

/// One section per saved query: the SQL rebuilt from its rows, or the rows themselves.
fn queries_section(sink: &mut Sink<'_>, db: &Db<'_>, objects: &[Object], shown: &[&Object]) {
    let queries: Vec<&&Object> = shown.iter().filter(|o| o.kind == kind::QUERY).collect();
    if queries.is_empty() {
        return;
    }
    let wanted: std::collections::BTreeSet<i64> = queries.iter().map(|o| o.id).collect();
    let q_table = objects
        .iter()
        .find(|o| o.kind == kind::TABLE && o.name.eq_ignore_ascii_case("MSysQueries"))
        .and_then(|o| db.table(o.page));
    let mut collected = match q_table.as_ref() {
        Some(t) => read_query_rows(sink, db, t, &wanted),
        None => BTreeMap::new(),
    };

    let (mut reconstructed, mut as_stored) = (false, false);
    for o in queries {
        if sink.is_full() {
            break;
        }
        let Some(found) = collected.remove(&o.id) else {
            // Nothing is made up for a query whose definition is not in the file.
            sink.skipped(&format!("query {}: no definition rows", o.name));
            continue;
        };
        let hidden = o.flags & HIDDEN_FLAG != 0;
        let Some(sec) =
            sink.open_section(SectionKind::Sheet, &format!("Query: {}", o.name), hidden)
        else {
            break;
        };
        let outcome = if found.incomplete {
            Outcome::Unsupported(
                QueryKind::from_object_flags(o.flags),
                "the definition is larger than the limits allow",
            )
        } else {
            reconstruct(o.flags, &found.rows)
        };
        let (text, show_rows) = match outcome {
            Outcome::Sql(kind, sql) => {
                reconstructed = true;
                (format!("-- {}\n{sql}", kind.label()), false)
            }
            Outcome::Unsupported(kind, why) => {
                as_stored = true;
                let label = kind
                    .map_or_else(|| format!("TYPE {:#X}", o.flags & 0xF0), |k| k.label().to_owned());
                (format!("-- {label}: not rebuilt as SQL ({why}); the stored rows follow"), true)
            }
        };
        // The viewer's own words are marked as such (`SYNTHETIC`); the rows below are the file's.
        let mut p = Paragraph::default();
        p.runs.push(Run { text, style: style::MONO | style::SYNTHETIC, href: String::new() });
        sink.push(sec, Block::Para(p));
        if show_rows {
            let rows_grid = stored_rows(sink, &found.rows);
            sink.push(sec, Block::Grid(rows_grid));
        }
    }
    if reconstructed {
        sink.note(
            NoticeKind::BestEffort,
            "access: query SQL is rebuilt from the stored rows for display; it is equivalent to, not necessarily identical to, the text Access shows, and is never run",
        );
    }
    if as_stored {
        sink.note(
            NoticeKind::Unsupported,
            "access: queries that do not fit the documented layout are shown as their stored rows, not as SQL",
        );
    }
}

/// The stored rows of a query as a grid: what the file says, whatever the viewer makes of it.
fn stored_rows(sink: &Sink<'_>, rows: &[QueryRow]) -> crate::model::Grid {
    let mut grid =
        GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
    let mut line = 1u32;
    let mut put = |grid: &mut GridBuilder, cells: [String; 6], kinds: [CellKind; 6]| {
        if !grid.row(line, false) {
            return false;
        }
        line = line.saturating_add(1);
        for (i, (text, kind)) in cells.into_iter().zip(kinds).enumerate() {
            if !grid.cell(u32::try_from(i).unwrap_or(0), kind, text) {
                return false;
            }
        }
        true
    };
    // The column names of `MSysQueries` itself.
    let head = ["Attribute", "Flag", "Extra", "Name1", "Name2", "Expression"].map(str::to_owned);
    let mut ok = put(&mut grid, head, [CellKind::Text; 6]);
    let num = |n: Option<i64>| n.map_or_else(String::new, |n| n.to_string());
    for r in rows {
        if !ok {
            break;
        }
        ok = put(
            &mut grid,
            [
                r.attribute.to_string(),
                num(r.flag),
                num(r.extra),
                r.name1.clone().unwrap_or_default(),
                r.name2.clone().unwrap_or_default(),
                r.expression.clone().unwrap_or_default(),
            ],
            [
                CellKind::Number,
                CellKind::Number,
                CellKind::Number,
                CellKind::Text,
                CellKind::Text,
                CellKind::Text,
            ],
        );
    }
    grid.finish()
}

/// A list of the indexes of all tables: name, kind, columns.
fn indexes_section(sink: &mut Sink<'_>, tables: &[(&Object, Option<Table>)]) {
    if !tables.iter().any(|(_, t)| t.as_ref().is_some_and(|t| !t.indexes.is_empty())) {
        return;
    }
    let Some(sec) = sink.open_section(SectionKind::Sheet, "Indexes", false) else { return };
    let mut grid =
        GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
    let mut line = 1u32;
    let mut ok = grid.row(line, false);
    for (i, head) in ["Table", "Index", "Kind", "Columns"].into_iter().enumerate() {
        ok = ok && grid.cell(u32::try_from(i).unwrap_or(0), CellKind::Text, head.to_owned());
    }
    'tables: for (o, t) in tables {
        let Some(t) = t else { continue };
        for ix in &t.indexes {
            if !ok {
                break 'tables;
            }
            line = line.saturating_add(1);
            let kind = match (ix.kind, ix.unique) {
                (IndexKind::Primary, _) => "Primary key",
                (IndexKind::Foreign, _) => "Relationship",
                (_, true) => "Unique",
                (IndexKind::Other(_), false) => "Index (other)",
                (IndexKind::Normal, false) => "Index",
            };
            let cols: Vec<String> = ix
                .columns
                .iter()
                .map(|(n, asc)| if *asc { n.clone() } else { format!("{n} DESC") })
                .collect();
            ok = grid.row(line, false)
                && grid.cell(0, CellKind::Text, o.name.clone())
                && grid.cell(1, CellKind::Text, ix.name.clone())
                && grid.cell(2, CellKind::Text, kind.to_owned())
                && grid.cell(3, CellKind::Text, cols.join(", "));
        }
    }
    let grid = grid.finish();
    if grid.truncated || !ok {
        sink.truncated("indexes");
    }
    sink.push(sec, Block::Grid(grid));
}

/// The bits of `grbit` in `MSysRelationships` (the layout the Jackcess library documents).
fn relationship_attributes(grbit: i64) -> String {
    let mut parts = Vec::new();
    for (bit, text) in [
        (0x1, "one-to-one"),
        (0x2, "no referential integrity"),
        (0x100, "cascade update"),
        (0x1000, "cascade delete"),
        (0x2000, "set null on delete"),
        (0x0100_0000, "left outer join"),
        (0x0200_0000, "right outer join"),
    ] {
        if grbit & bit != 0 {
            parts.push(text);
        }
    }
    parts.join(", ")
}

/// A list of the relationships between tables.
fn relationships_section(sink: &mut Sink<'_>, db: &Db<'_>, objects: &[Object]) {
    let Some(table) = objects
        .iter()
        .find(|o| o.kind == kind::TABLE && o.name.eq_ignore_ascii_case("MSysRelationships"))
        .and_then(|o| db.table(o.page))
    else {
        return;
    };
    let col = |n: &str| table.columns.iter().position(|c| c.name.eq_ignore_ascii_case(n));
    let (Some(c_name), Some(c_obj), Some(c_col), Some(c_ref_obj), Some(c_ref_col)) = (
        col("szRelationship"),
        col("szObject"),
        col("szColumn"),
        col("szReferencedObject"),
        col("szReferencedColumn"),
    ) else {
        return;
    };
    let (c_flags, c_index) = (col("grbit"), col("icolumn"));
    // (name, table, related table, flags) with the column pairs by position.
    struct Rel {
        table: String,
        related: String,
        flags: i64,
        columns: Vec<(i64, String, String)>,
    }
    let mut rels: BTreeMap<String, Rel> = BTreeMap::new();
    let mut seen = 0usize;
    let stats = db.rows(&table, |v| {
        seen += 1;
        if seen > MAX_RELATIONSHIP_ROWS {
            return false;
        }
        let name = text_of(v.get(c_name)).unwrap_or("").to_owned();
        if rels.len() >= MAX_RELATIONSHIPS && !rels.contains_key(&name) {
            return true;
        }
        let rel = rels.entry(name).or_insert_with(|| Rel {
            table: text_of(v.get(c_obj)).unwrap_or("").to_owned(),
            related: text_of(v.get(c_ref_obj)).unwrap_or("").to_owned(),
            flags: c_flags.and_then(|i| int_of(v.get(i))).unwrap_or(0),
            columns: Vec::new(),
        });
        if rel.columns.len() < 64 {
            rel.columns.push((
                c_index.and_then(|i| int_of(v.get(i))).unwrap_or(0),
                text_of(v.get(c_col)).unwrap_or("").to_owned(),
                text_of(v.get(c_ref_col)).unwrap_or("").to_owned(),
            ));
        }
        true
    });
    if rels.is_empty() {
        return;
    }
    let Some(sec) = sink.open_section(SectionKind::Sheet, "Relationships", false) else { return };
    let mut grid =
        GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
    let mut line = 1u32;
    let mut ok = grid.row(line, false);
    for (i, head) in
        ["Relationship", "Table", "Columns", "Related table", "Related columns", "Attributes"]
            .into_iter()
            .enumerate()
    {
        ok = ok && grid.cell(u32::try_from(i).unwrap_or(0), CellKind::Text, head.to_owned());
    }
    for (name, rel) in &mut rels {
        if !ok {
            break;
        }
        rel.columns.sort_by_key(|c| c.0);
        let own: Vec<&str> = rel.columns.iter().map(|c| c.1.as_str()).collect();
        let other: Vec<&str> = rel.columns.iter().map(|c| c.2.as_str()).collect();
        line = line.saturating_add(1);
        ok = grid.row(line, false)
            && grid.cell(0, CellKind::Text, name.clone())
            && grid.cell(1, CellKind::Text, rel.table.clone())
            && grid.cell(2, CellKind::Text, own.join(", "))
            && grid.cell(3, CellKind::Text, rel.related.clone())
            && grid.cell(4, CellKind::Text, other.join(", "))
            && grid.cell(5, CellKind::Text, relationship_attributes(rel.flags));
    }
    let grid = grid.finish();
    if grid.truncated || !ok || stats.stopped {
        sink.truncated("relationships");
    }
    sink.push(sec, Block::Grid(grid));
}

/// Reports what the database says should happen when it opens. Nothing of it is done.
fn startup_notes(sink: &mut Sink<'_>, db: &Db<'_>) {
    let Some(props) = db.properties_of("MSysDb") else { return };
    for p in props.iter().filter(|p| p.map_kind == 0 && p.scope.is_empty()) {
        let PropValue::Text(value) = &p.value else { continue };
        if value.is_empty() {
            continue;
        }
        let what = if p.name.eq_ignore_ascii_case("StartUpForm") {
            "startup form"
        } else if p.name.eq_ignore_ascii_case("StartUpMenuBar") {
            "startup menu bar"
        } else {
            continue;
        };
        sink.note_detail(
            NoticeKind::Unsupported,
            format!("access: the database names a {what} ({}); it is never opened or run", short(value)),
        );
    }
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

/// A connection string without the values of its passwords.
fn without_passwords(connect: &str) -> String {
    connect
        .split(';')
        .map(|part| {
            let key = part.split('=').next().unwrap_or("").trim().to_ascii_lowercase();
            if part.contains('=') && matches!(key.as_str(), "pwd" | "password" | "dbq pwd") {
                format!("{}=***", part.split('=').next().unwrap_or(""))
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Lists the objects of the database.
fn overview(sink: &mut Sink<'_>, objects: &[&Object], tables: &[(&Object, Option<Table>)]) {
    let Some(sec) = sink.open_section(SectionKind::Body, "Database", false) else { return };
    let mut grid =
        GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
    let mut row = 1u32;
    let mut full = !grid.row(row, false);
    for (col, head) in ["Object", "Name", "Rows", "Source"].into_iter().enumerate() {
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
        // Where a linked table gets its rows from (never opened or contacted).
        let source = if matches!(o.kind, kind::LINKED | kind::LINKED_ODBC) {
            let target = if o.database.is_empty() { &o.connect } else { &o.database };
            Some(without_passwords(target))
        } else {
            None
        };
        let ok = grid.cell(0, CellKind::Text, kind_name(o.kind).to_owned())
            && grid.cell(1, CellKind::Text, o.name.clone())
            && rows.is_none_or(|r| grid.cell(2, CellKind::Number, r))
            && source.is_none_or(|s| s.is_empty() || grid.cell(3, CellKind::Text, s));
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
        "forms, reports, macros, modules and data access pages are listed by name, not shown or run; queries are shown as SQL text that is never run",
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
        Value::Blob(b) => plain(CellKind::Text, format!("(binary, {} bytes)", b.len())),
        Value::Guid(s) | Value::Numeric(s) => plain(CellKind::Text, s.clone()),
        Value::Complex(k) => plain(CellKind::Text, format!("(complex field, key {k})")),
        Value::ExtDate(s) => plain(CellKind::Date, s.clone()),
    }
}

/// How the values of a complex field are shown in one cell. Attachments are named with their
/// stored size and never read; multi-value fields list their values.
fn complex_cell(data: Option<&ComplexData>, key: i64) -> Option<(CellKind, String, String)> {
    let text = |t: String| Some((CellKind::Text, t, String::new()));
    let Some(data) = data else {
        return text("(complex field; its values are not in the file)".to_owned());
    };
    if data.kind == ComplexKind::Unsupported {
        return text("(complex field of an unsupported kind)".to_owned());
    }
    let items = data.items.get(&key)?;
    let mut parts: Vec<String> = Vec::new();
    for item in items.iter().take(MAX_COMPLEX_IN_CELL) {
        match item {
            ComplexItem::Attachment { name, size } => {
                let name = if name.is_empty() { "(unnamed)" } else { name.as_str() };
                parts.push(format!("{name} ({size} bytes)"));
            }
            ComplexItem::Value(v) => {
                if let Some((_, t, _)) = cell(v) {
                    parts.push(t);
                }
            }
            ComplexItem::Version => {}
        }
    }
    if data.kind == ComplexKind::VersionHistory {
        return text(format!("({} versions of this text are kept; only the current one is shown)", items.len()));
    }
    if items.len() > MAX_COMPLEX_IN_CELL {
        parts.push(format!("... ({} more)", items.len() - MAX_COMPLEX_IN_CELL));
    }
    text(parts.join("; "))
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
#[cfg(test)]
mod synthetic_tests;
