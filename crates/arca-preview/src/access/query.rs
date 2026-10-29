//! Saved queries of an Access database, rebuilt as SQL **text for display only**.
//!
//! Access keeps a query as rows of the system table `MSysQueries`, one row per clause part. The
//! layout used here is the one the open-source Jackcess library documents and checks against
//! databases written by Access 97 to 2010 (`QueryImpl`, `QueryFormat` and the per-kind
//! subclasses; written again here from that description, nothing is copied):
//!
//! * `Attribute` says what a row is: 1 the type, 2 a parameter, 3 the flags, 4 the remote
//!   database (`IN '...'`), 5 a table (or, in a union, a whole `SELECT`), 6 a column, 7 a join,
//!   8 `WHERE`, 9 `GROUP BY`, 10 `HAVING`, 11 `ORDER BY`;
//! * `Expression`, `Name1`, `Name2`, `Flag` and `Extra` (the column `LvExtra`) carry the parts,
//!   and which of them mean what depends on the attribute and the kind of query;
//! * the kind comes from the catalog flags of the query (`& 0xF0`): select, cross-tab, delete,
//!   update, append, make-table, data definition, pass-through, union.
//!
//! Only the text is produced. No expression is parsed or evaluated, no sub-query is followed
//! (an expression is printed as the text that is stored), and nothing is run. Anything that does
//! not fit the layout (an unknown join type, two `WHERE` rows, a missing part) makes the whole
//! query [`Outcome::Unsupported`] with the reason, never a guess.

/// Attribute codes of `MSysQueries`.
mod attr {
    pub(super) const TYPE: i64 = 1;
    pub(super) const PARAMETER: i64 = 2;
    pub(super) const FLAGS: i64 = 3;
    pub(super) const REMOTE_DB: i64 = 4;
    pub(super) const TABLE: i64 = 5;
    pub(super) const COLUMN: i64 = 6;
    pub(super) const JOIN: i64 = 7;
    pub(super) const WHERE: i64 = 8;
    pub(super) const GROUP_BY: i64 = 9;
    pub(super) const HAVING: i64 = 10;
    pub(super) const ORDER_BY: i64 = 11;
}

/// Bits of the flag row of a select-like query.
const SELECT_STAR: i64 = 0x01;
const DISTINCT: i64 = 0x02;
const OWNER_ACCESS: i64 = 0x04;
const DISTINCT_ROW: i64 = 0x08;
const TOP: i64 = 0x10;
const PERCENT: i64 = 0x20;
/// Flag bit of a union: set for `UNION`, clear for `UNION ALL`.
const UNION_DISTINCT: i64 = 0x02;
/// Flag bits of the column rows of a cross-tab query.
const CROSSTAB_PIVOT: i64 = 0x01;
const CROSSTAB_NORMAL: i64 = 0x02;
/// Flag bit of a column row of an append query that holds a `VALUES` item.
const APPEND_VALUE: i64 = 0x8000;
/// Names of the two halves of a union in the table rows.
const UNION_PART1: &str = "X7YZ_____1";
const UNION_PART2: &str = "X7YZ_____2";

/// Most rows of one attribute that are used (Access allows 32 tables).
const MAX_PART_ROWS: usize = 4096;
const MAX_TABLES: usize = 64;
const MAX_JOINS: usize = 128;
/// Most bytes of SQL produced for one query.
const MAX_SQL_BYTES: usize = 1 << 20;
/// Most comparisons the join arrangement may make.
const MAX_JOIN_WORK: usize = 4_000_000;

/// One row of `MSysQueries`.
#[derive(Debug, Clone, Default)]
pub(super) struct QueryRow {
    pub(super) attribute: i64,
    pub(super) expression: Option<String>,
    pub(super) flag: Option<i64>,
    pub(super) extra: Option<i64>,
    pub(super) name1: Option<String>,
    pub(super) name2: Option<String>,
    /// The sort key of the row (`Order`); the rows are already read in this order.
    #[allow(dead_code)]
    pub(super) order: Vec<u8>,
}

/// The kind of a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryKind {
    Select,
    Crosstab,
    Delete,
    Update,
    Append,
    MakeTable,
    DataDefinition,
    PassThrough,
    Union,
}

impl QueryKind {
    /// The kind for the catalog flag bits `0xF0`.
    pub(super) fn from_object_flags(flags: u32) -> Option<Self> {
        Some(match flags & 0xF0 {
            0x00 => Self::Select,
            0x10 => Self::Crosstab,
            0x20 => Self::Delete,
            0x30 => Self::Update,
            0x40 => Self::Append,
            0x50 => Self::MakeTable,
            0x60 => Self::DataDefinition,
            0x70 => Self::PassThrough,
            0x80 => Self::Union,
            _ => return None,
        })
    }

    /// The value of the type row (`Flag` of attribute 1).
    fn row_type(self) -> i64 {
        match self {
            Self::Select => 1,
            Self::MakeTable => 2,
            Self::Append => 3,
            Self::Update => 4,
            Self::Delete => 5,
            Self::Crosstab => 6,
            Self::DataDefinition => 7,
            Self::PassThrough => 8,
            Self::Union => 9,
        }
    }

    fn from_row_type(t: i64) -> Option<Self> {
        [
            Self::Select,
            Self::MakeTable,
            Self::Append,
            Self::Update,
            Self::Delete,
            Self::Crosstab,
            Self::DataDefinition,
            Self::PassThrough,
            Self::Union,
        ]
        .into_iter()
        .find(|k| k.row_type() == t)
    }

    /// The name of the kind in technical vocabulary.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Crosstab => "TRANSFORM",
            Self::Delete => "DELETE",
            Self::Update => "UPDATE",
            Self::Append => "INSERT INTO",
            Self::MakeTable => "SELECT INTO",
            Self::DataDefinition => "DDL",
            Self::PassThrough => "PASS-THROUGH",
            Self::Union => "UNION",
        }
    }
}

/// What the rebuild produced.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The SQL text, and the kind of query.
    Sql(QueryKind, String),
    /// The rows do not fit the documented layout; the reason is a short technical phrase.
    Unsupported(Option<QueryKind>, &'static str),
}

/// Rebuilds the SQL of a query from the catalog flags and the `MSysQueries` rows (in the order
/// the file stores them).
pub(super) fn reconstruct(object_flags: u32, rows: &[QueryRow]) -> Outcome {
    let kind = QueryKind::from_object_flags(object_flags);
    let mut kind = match kind {
        Some(k) => k,
        None => return Outcome::Unsupported(None, "unknown kind of query"),
    };
    // A plain select in the catalog may be another kind according to its type row.
    if kind == QueryKind::Select
        && let Some(t) = rows
            .iter()
            .find(|r| r.attribute == attr::TYPE)
            .and_then(|r| r.flag)
            .and_then(QueryKind::from_row_type)
    {
        kind = t;
    }
    match Builder::new(kind, rows).and_then(|b| b.build()) {
        Ok(sql) => Outcome::Sql(kind, sql),
        Err(why) => Outcome::Unsupported(Some(kind), why),
    }
}

type Res<T> = Result<T, &'static str>;

struct Builder<'a> {
    kind: QueryKind,
    rows: &'a [QueryRow],
    work: usize,
}

/// A table or a join of tables in the `FROM` list.
enum Node {
    Table {
        #[allow(dead_code)]
        key: String,
        text: String,
    },
    Join {
        from: usize,
        to: usize,
        kind: i64,
        on: Vec<String>,
    },
}

impl<'a> Builder<'a> {
    fn new(kind: QueryKind, rows: &'a [QueryRow]) -> Res<Self> {
        if rows.len() > MAX_PART_ROWS {
            return Err("too many rows");
        }
        let b = Self { kind, rows, work: 0 };
        // The type row, when it is there, has to agree with the kind.
        let found = b.of(attr::TYPE).next().and_then(|r| r.flag).unwrap_or(kind.row_type());
        if found != kind.row_type() {
            return Err("the type row disagrees with the kind of query");
        }
        Ok(b)
    }

    fn of(&self, attribute: i64) -> impl Iterator<Item = &'a QueryRow> + use<'a> {
        self.rows.iter().filter(move |r| r.attribute == attribute)
    }

    /// The only row of an attribute: none is fine, two are not.
    fn unique(&self, attribute: i64) -> Res<Option<&'a QueryRow>> {
        let mut it = self.of(attribute);
        let first = it.next();
        if it.next().is_some() {
            return Err("an attribute that should occur once occurs more than once");
        }
        Ok(first)
    }

    fn flag(&self) -> Res<i64> {
        Ok(self.unique(attr::FLAGS)?.and_then(|r| r.flag).unwrap_or(0))
    }

    fn has_flag(&self, mask: i64) -> Res<bool> {
        Ok(self.flag()? & mask != 0)
    }

    fn build(mut self) -> Res<String> {
        let standard = !matches!(self.kind, QueryKind::DataDefinition | QueryKind::PassThrough);
        let mut out = String::new();
        if standard {
            let params = self.parameters()?;
            if !params.is_empty() {
                out.push_str("PARAMETERS ");
                out.push_str(&params.join(", "));
                out.push_str(";\n");
            }
        }
        match self.kind {
            QueryKind::Select | QueryKind::MakeTable => self.select_text(&mut out, true)?,
            QueryKind::Delete => {
                out.push_str("DELETE ");
                self.select_text(&mut out, false)?;
            }
            QueryKind::Crosstab => self.crosstab(&mut out)?,
            QueryKind::Update => self.update(&mut out)?,
            QueryKind::Append => self.append(&mut out)?,
            QueryKind::Union => self.union(&mut out)?,
            QueryKind::DataDefinition | QueryKind::PassThrough => {
                let text = self.unique(attr::TYPE)?.and_then(|r| r.expression.as_deref());
                out.push_str(text.unwrap_or(""));
            }
        }
        if standard {
            if self.has_flag(OWNER_ACCESS)? {
                out.push_str("\nWITH OWNERACCESS OPTION");
            }
            out.push(';');
        }
        if out.len() > MAX_SQL_BYTES {
            return Err("the SQL is too long");
        }
        Ok(clean(&out))
    }

    fn parameters(&self) -> Res<Vec<String>> {
        let mut out = Vec::new();
        for r in self.of(attr::PARAMETER) {
            let t = r.flag.unwrap_or(0);
            let name = match t {
                0 => "Value",
                1 => "Bit",
                2 => "Byte",
                3 => "Short",
                4 => "Long",
                5 => "Currency",
                6 => "IEEESingle",
                7 => "IEEEDouble",
                8 => "DateTime",
                9 => "Binary",
                10 => "Text",
                11 => "LongBinary",
                15 => "Guid",
                _ => return Err("an unknown parameter type"),
            };
            let mut s = format!("{} {name}", r.name1.as_deref().unwrap_or(""));
            if t == 10
                && let Some(n) = r.extra.filter(|n| *n > 0)
            {
                s.push_str(&format!("({n})"));
            }
            out.push(s);
        }
        Ok(out)
    }

    /// The `SELECT ... FROM ... WHERE ... GROUP BY ... HAVING ... ORDER BY ...` text, with or
    /// without the leading `SELECT` keyword and the select type.
    fn select_text(&mut self, out: &mut String, select_prefix: bool) -> Res<()> {
        if select_prefix {
            out.push_str("SELECT ");
            let ty = self.select_type()?;
            if !ty.is_empty() {
                out.push_str(&ty);
                out.push(' ');
            }
        }
        let cols = self.select_columns()?;
        out.push_str(&cols.join(", "));
        if self.kind == QueryKind::MakeTable {
            let t = self.unique(attr::TYPE)?;
            let target =
                t.and_then(|r| r.name1.as_deref()).ok_or("a make-table query without a target")?;
            out.push_str(" INTO ");
            out.push_str(&ident(target));
            remote(
                out,
                t.and_then(|r| r.name2.as_deref()),
                t.and_then(|r| r.expression.as_deref()),
            );
        }
        let from = self.build_from_list()?;
        if !from.is_empty() {
            out.push_str("\nFROM ");
            out.push_str(&from.join(", "));
            self.remote_db(out)?;
        }
        if let Some(w) = self.unique(attr::WHERE)?.and_then(|r| r.expression.as_deref()) {
            out.push_str("\nWHERE ");
            out.push_str(w);
        }
        let groups = self.groupings();
        if !groups.is_empty() {
            out.push_str("\nGROUP BY ");
            out.push_str(&groups.join(", "));
        }
        if let Some(h) = self.unique(attr::HAVING)?.and_then(|r| r.expression.as_deref()) {
            out.push_str("\nHAVING ");
            out.push_str(h);
        }
        let orders = self.orderings();
        if !orders.is_empty() {
            out.push_str("\nORDER BY ");
            out.push_str(&orders.join(", "));
        }
        Ok(())
    }

    /// `DISTINCT`, `DISTINCTROW` and `TOP n [PERCENT]`, in the order SQL wants them.
    fn select_type(&self) -> Res<String> {
        let flag = self.flag()?;
        let mut parts: Vec<String> = Vec::new();
        if flag & DISTINCT != 0 {
            parts.push("DISTINCT".to_owned());
        } else if flag & DISTINCT_ROW != 0 {
            parts.push("DISTINCTROW".to_owned());
        }
        if flag & TOP != 0 {
            let n = self
                .unique(attr::FLAGS)?
                .and_then(|r| r.name1.as_deref())
                .ok_or("TOP without a count")?;
            let mut t = format!("TOP {n}");
            if flag & PERCENT != 0 {
                t.push_str(" PERCENT");
            }
            parts.push(t);
        }
        Ok(parts.join(" "))
    }

    /// Column rows as the kind of query uses them.
    fn column_rows(&self) -> Vec<&'a QueryRow> {
        let all = self.of(attr::COLUMN);
        match self.kind {
            QueryKind::Crosstab => {
                all.filter(|r| r.flag.unwrap_or(0) & CROSSTAB_NORMAL != 0).collect()
            }
            QueryKind::Append => all.filter(|r| r.flag.unwrap_or(0) & APPEND_VALUE == 0).collect(),
            _ => all.collect(),
        }
    }

    fn select_columns(&self) -> Res<Vec<String>> {
        let mut out = Vec::new();
        for r in self.column_rows() {
            let e = r.expression.as_deref().ok_or("a column without an expression")?;
            out.push(format!("{e}{}", alias(r.name1.as_deref())));
        }
        if self.has_flag(SELECT_STAR)? {
            out.push("*".to_owned());
        }
        Ok(out)
    }

    fn groupings(&self) -> Vec<String> {
        self.of(attr::GROUP_BY)
            .filter(|r| {
                self.kind != QueryKind::Crosstab || r.flag.unwrap_or(0) & CROSSTAB_NORMAL != 0
            })
            .map(|r| r.expression.clone().unwrap_or_default())
            .collect()
    }

    fn orderings(&self) -> Vec<String> {
        self.of(attr::ORDER_BY)
            .map(|r| {
                let mut s = r.expression.clone().unwrap_or_default();
                if r.name1.as_deref().is_some_and(|n| n.eq_ignore_ascii_case("D")) {
                    s.push_str(" DESC");
                }
                s
            })
            .collect()
    }

    fn remote_db(&self, out: &mut String) -> Res<()> {
        let r = self.unique(attr::REMOTE_DB)?;
        remote(out, r.and_then(|r| r.name1.as_deref()), r.and_then(|r| r.expression.as_deref()));
        Ok(())
    }

    /// The `FROM` items: tables as stored, and joins arranged the way Access nests them.
    fn build_from_list(&mut self) -> Res<Vec<String>> {
        let mut nodes: Vec<Node> = Vec::new();
        let mut order: Vec<usize> = Vec::new();
        // The lower-cased table keys inside each node.
        let mut keys: Vec<Vec<String>> = Vec::new();
        for (n, r) in self.of(attr::TABLE).enumerate() {
            if n >= MAX_TABLES {
                return Err("too many tables");
            }
            let mut text = String::new();
            if let Some(e) = &r.expression {
                if r.name1.is_none() {
                    return Err("a table row with an expression but no name");
                }
                text.push_str(&quoted(e));
                text.push('.');
            }
            if let Some(n1) = &r.name1 {
                text.push_str(&ident(n1));
            }
            text.push_str(&alias(r.name2.as_deref()));
            let key = r.name2.as_ref().or(r.name1.as_ref()).cloned().unwrap_or_default();
            keys.push(vec![key.to_lowercase()]);
            nodes.push(Node::Table { key, text });
            order.push(nodes.len() - 1);
        }
        for (n, j) in self.of(attr::JOIN).enumerate() {
            if n >= MAX_JOINS {
                return Err("too many joins");
            }
            let from_name = j.name1.clone().unwrap_or_default();
            let to_name = j.name2.clone().unwrap_or_default();
            let (from_l, to_l) = (from_name.to_lowercase(), to_name.to_lowercase());
            let on = j.expression.clone().ok_or("a join without a condition")?;
            let kind = j.flag.unwrap_or(0);
            if !(1..=3).contains(&kind) {
                return Err("an unknown join type");
            }
            let (mut from_ts, mut to_ts): (Option<usize>, Option<usize>) = (None, None);
            let mut removed: Vec<usize> = Vec::new();
            for &idx in &order {
                if from_ts.is_some() && to_ts.is_some() {
                    break;
                }
                self.work += keys.get(idx).map_or(1, Vec::len);
                if self.work > MAX_JOIN_WORK {
                    return Err("the joins are too complicated");
                }
                let has = |name: &str| keys.get(idx).is_some_and(|k| k.iter().any(|x| x == name));
                if from_ts.is_none() && has(&from_l) {
                    from_ts = Some(idx);
                    if to_ts.is_none() && has(&to_l) {
                        to_ts = Some(idx);
                        break;
                    }
                    removed.push(idx);
                } else if to_ts.is_none() && has(&to_l) {
                    to_ts = Some(idx);
                    removed.push(idx);
                }
            }
            order.retain(|i| !removed.contains(i));
            let leaf = |name: &str, nodes: &mut Vec<Node>, keys: &mut Vec<Vec<String>>| {
                nodes.push(Node::Table { key: name.to_owned(), text: ident(name) });
                keys.push(vec![name.to_lowercase()]);
                nodes.len() - 1
            };
            let f = match from_ts {
                Some(i) => i,
                None => leaf(&from_name, &mut nodes, &mut keys),
            };
            let t = match to_ts {
                Some(i) => i,
                None => leaf(&to_name, &mut nodes, &mut keys),
            };
            if f == t {
                // Another condition between tables that are already joined: it is added to
                // that join (latest first) when the join type is the same.
                match nodes.get_mut(f) {
                    Some(Node::Join { kind: k, on: conds, .. }) if *k == kind => {
                        conds.insert(0, on);
                        continue;
                    }
                    _ => return Err("joins of different types between the same tables"),
                }
            }
            let mut joined = keys.get(f).cloned().unwrap_or_default();
            joined.extend(keys.get(t).cloned().unwrap_or_default());
            nodes.push(Node::Join { from: f, to: t, kind, on: vec![on] });
            keys.push(joined);
            order.push(nodes.len() - 1);
        }
        let mut out = Vec::new();
        for &i in &order {
            let mut s = String::new();
            render(&nodes, i, true, &mut s, 0)?;
            out.push(s);
        }
        Ok(out)
    }

    fn crosstab(&mut self, out: &mut String) -> Res<()> {
        let cols = self.of(attr::COLUMN);
        let transform: Vec<&QueryRow> = cols
            .filter(|r| r.flag.unwrap_or(0) & (CROSSTAB_PIVOT | CROSSTAB_NORMAL) == 0)
            .collect();
        if transform.len() > 1 {
            return Err("more than one TRANSFORM row");
        }
        let pivot: Vec<&QueryRow> =
            self.of(attr::COLUMN).filter(|r| r.flag.unwrap_or(0) & CROSSTAB_PIVOT != 0).collect();
        if pivot.len() != 1 {
            return Err("a cross-tab query needs exactly one PIVOT row");
        }
        if let Some(t) = transform.first().and_then(|r| r.expression.as_deref().map(|e| (e, r))) {
            out.push_str("TRANSFORM ");
            out.push_str(t.0);
            out.push_str(&alias(t.1.name1.as_deref()));
            out.push('\n');
        }
        self.select_text(out, true)?;
        out.push_str("\nPIVOT ");
        out.push_str(
            pivot
                .first()
                .and_then(|r| r.expression.as_deref())
                .ok_or("a PIVOT row without an expression")?,
        );
        Ok(())
    }

    fn update(&mut self, out: &mut String) -> Res<()> {
        let tables = self.build_from_list()?;
        out.push_str("UPDATE ");
        out.push_str(&tables.join(", "));
        self.remote_db(out)?;
        let mut sets = Vec::new();
        for r in self.of(attr::COLUMN) {
            let target = r.name2.as_deref().ok_or("an update column without a target")?;
            let e = r.expression.as_deref().ok_or("an update column without a value")?;
            sets.push(format!("{} = {e}", ident(target)));
        }
        out.push_str("\nSET ");
        out.push_str(&sets.join(", "));
        if let Some(w) = self.unique(attr::WHERE)?.and_then(|r| r.expression.as_deref()) {
            out.push_str("\nWHERE ");
            out.push_str(w);
        }
        Ok(())
    }

    fn append(&mut self, out: &mut String) -> Res<()> {
        let t = self.unique(attr::TYPE)?;
        let target =
            t.and_then(|r| r.name1.as_deref()).ok_or("an append query without a target")?;
        out.push_str("INSERT INTO ");
        out.push_str(&ident(target));
        let targets: Vec<String> =
            self.of(attr::COLUMN).filter_map(|r| r.name2.as_deref()).map(ident).collect();
        if !targets.is_empty() {
            out.push_str(&format!(" ({})", targets.join(", ")));
        }
        remote(out, t.and_then(|r| r.name2.as_deref()), t.and_then(|r| r.expression.as_deref()));
        out.push('\n');
        let values: Vec<String> = self
            .of(attr::COLUMN)
            .filter(|r| r.flag.unwrap_or(0) & APPEND_VALUE != 0)
            .map(|r| r.expression.clone().unwrap_or_default())
            .collect();
        if values.is_empty() {
            self.select_text(out, true)
        } else {
            out.push_str(&format!("VALUES ({})", values.join(", ")));
            Ok(())
        }
    }

    fn union(&mut self, out: &mut String) -> Res<()> {
        if self.unique(attr::FLAGS)?.and_then(|r| r.flag).is_none() {
            return Err("a union without its flag row");
        }
        let part = |id: &str| -> Res<String> {
            self.of(attr::TABLE)
                .find(|r| r.name2.as_deref() == Some(id))
                .and_then(|r| r.expression.as_deref())
                .map(union_part)
                .ok_or("a union without both of its parts")
        };
        out.push_str(&part(UNION_PART1)?);
        out.push_str("\nUNION ");
        if !self.has_flag(UNION_DISTINCT)? {
            out.push_str("ALL ");
        }
        out.push_str(&part(UNION_PART2)?);
        let orders = self.orderings();
        if !orders.is_empty() {
            out.push_str("\nORDER BY ");
            out.push_str(&orders.join(", "));
        }
        Ok(())
    }
}

/// Appends ` IN 'path' [type]` when the remote database row has either part.
fn remote(out: &mut String, path: Option<&str>, kind: Option<&str>) {
    if path.is_some() || kind.is_some() {
        out.push_str(" IN '");
        out.push_str(path.unwrap_or(""));
        out.push('\'');
        if let Some(k) = kind {
            out.push_str(&format!(" [{k}]"));
        }
    }
}

/// A union part: trimmed, with every run of line breaks as one `\n`.
fn union_part(s: &str) -> String {
    let mut out = String::new();
    let mut in_break = false;
    for c in s.trim().chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                out.push('\n');
            }
            in_break = true;
        } else {
            in_break = false;
            out.push(c);
        }
    }
    out
}

/// Writes node `i`; a join that is not at the top level is parenthesised.
fn render(nodes: &[Node], i: usize, top: bool, out: &mut String, depth: usize) -> Res<()> {
    if depth > MAX_JOINS + 2 || out.len() > MAX_SQL_BYTES {
        return Err("the joins are nested too deeply");
    }
    match nodes.get(i).ok_or("a join refers to a missing table")? {
        Node::Table { text, .. } => out.push_str(text),
        Node::Join { from, to, kind, on } => {
            if !top {
                out.push('(');
            }
            render(nodes, *from, false, out, depth + 1)?;
            out.push_str(match kind {
                1 => " INNER JOIN ",
                2 => " LEFT JOIN ",
                3 => " RIGHT JOIN ",
                _ => return Err("an unknown join type"),
            });
            render(nodes, *to, false, out, depth + 1)?;
            out.push_str(" ON ");
            if on.len() > 1 {
                out.push('(');
            }
            out.push_str(&on.join(") AND ("));
            if on.len() > 1 {
                out.push(')');
            }
            if !top {
                out.push(')');
            }
        }
    }
    Ok(())
}

/// Whether Access would have to bracket the name: it holds anything but ASCII letters, digits
/// and the underscore.
fn needs_brackets(s: &str) -> bool {
    s.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
}

fn is_bracketed(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('[') && s.ends_with(']')
}

fn quoted(s: &str) -> String {
    if is_bracketed(s) { s.to_owned() } else { format!("[{s}]") }
}

/// A possibly dotted name (`table.column`), each part bracketed when it needs to be.
fn ident(s: &str) -> String {
    let mut parts: Vec<&str> = if s.contains('.') { s.split('.').collect() } else { vec![s] };
    while parts.len() > 1 && parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    if s.contains('.') && parts.len() == 1 && parts.first().is_some_and(|p| p.is_empty()) {
        parts.clear();
    }
    parts
        .into_iter()
        .map(|p| if needs_brackets(p) { quoted(p) } else { p.to_owned() })
        .collect::<Vec<_>>()
        .join(".")
}

/// ` AS name` (bracketed when needed), or nothing.
fn alias(a: Option<&str>) -> String {
    match a {
        Some(a) if needs_brackets(a) => format!(" AS {}", quoted(a)),
        Some(a) => format!(" AS {a}"),
        None => String::new(),
    }
}

/// Removes characters that must not reach a display: NUL (pass-through and data definition
/// strings end with one) and other control characters except line breaks and tabs.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

#[cfg(test)]
mod tests;
