//! Tests of the SQL rebuild. The row layouts and the expected texts follow the examples the
//! open-source Jackcess library uses to check its own reader (`QueryTest`), several of them
//! taken from queries Access itself wrote; the cases here are written out again.

use super::*;

fn row(
    attribute: i64,
    expr: Option<&str>,
    flag: Option<i64>,
    extra: Option<i64>,
    n1: Option<&str>,
    n2: Option<&str>,
) -> QueryRow {
    QueryRow {
        attribute,
        expression: expr.map(str::to_owned),
        flag,
        extra,
        name1: n1.map(str::to_owned),
        name2: n2.map(str::to_owned),
        order: Vec::new(),
    }
}

fn typ(kind: QueryKind, expr: Option<&str>, name1: Option<&str>) -> QueryRow {
    row(attr::TYPE, expr, Some(kind.row_type()), None, name1, None)
}

fn flagrow(flag: i64) -> QueryRow {
    row(attr::FLAGS, None, Some(flag), None, None, None)
}

fn table(n1: &str, n2: Option<&str>) -> QueryRow {
    row(attr::TABLE, None, None, None, Some(n1), n2)
}

fn col(expr: &str, alias: Option<&str>) -> QueryRow {
    row(attr::COLUMN, Some(expr), None, None, alias, None)
}

fn object_flags(kind: QueryKind) -> u32 {
    match kind {
        QueryKind::Select => 0x00,
        QueryKind::Crosstab => 0x10,
        QueryKind::Delete => 0x20,
        QueryKind::Update => 0x30,
        QueryKind::Append => 0x40,
        QueryKind::MakeTable => 0x50,
        QueryKind::DataDefinition => 0x60,
        QueryKind::PassThrough => 0x70,
        QueryKind::Union => 0x80,
    }
}

fn sql(kind: QueryKind, rows: &[QueryRow]) -> String {
    match reconstruct(object_flags(kind), rows) {
        Outcome::Sql(k, s) => {
            assert_eq!(k, kind);
            s
        }
        Outcome::Unsupported(_, why) => panic!("unsupported: {why}"),
    }
}

fn unsupported(kind: QueryKind, rows: &[QueryRow]) -> &'static str {
    match reconstruct(object_flags(kind), rows) {
        Outcome::Unsupported(_, why) => why,
        Outcome::Sql(_, s) => panic!("rebuilt: {s}"),
    }
}

#[test]
fn union_queries() {
    let mut rows = vec![
        typ(QueryKind::Union, None, None),
        row(attr::TABLE, Some("Select * from Table1"), None, None, None, Some(UNION_PART1)),
        row(attr::TABLE, Some("Select * from Table2"), None, None, None, Some(UNION_PART2)),
        flagrow(3),
    ];
    assert_eq!(sql(QueryKind::Union, &rows), "Select * from Table1\nUNION Select * from Table2;");
    rows[3] = flagrow(1);
    assert_eq!(
        sql(QueryKind::Union, &rows),
        "Select * from Table1\nUNION ALL Select * from Table2;"
    );
    rows.push(row(attr::ORDER_BY, Some("Table1.id"), None, None, None, None));
    assert_eq!(
        sql(QueryKind::Union, &rows),
        "Select * from Table1\nUNION ALL Select * from Table2\nORDER BY Table1.id;"
    );
    // Line breaks inside a part collapse and the part is trimmed.
    rows[1] =
        row(attr::TABLE, Some("  Select a\r\n\r\nfrom T  "), None, None, None, Some(UNION_PART1));
    assert!(sql(QueryKind::Union, &rows).starts_with("Select a\nfrom T\nUNION ALL"));
    // A half that is missing is not made up.
    rows.remove(1);
    assert_eq!(unsupported(QueryKind::Union, &rows), "a union without both of its parts");
    // Without the flag row nobody knows whether it is UNION or UNION ALL.
    let no_flag = [
        typ(QueryKind::Union, None, None),
        row(attr::TABLE, Some("a"), None, None, None, Some(UNION_PART1)),
        row(attr::TABLE, Some("b"), None, None, None, Some(UNION_PART2)),
    ];
    assert_eq!(unsupported(QueryKind::Union, &no_flag), "a union without its flag row");
}

#[test]
fn pass_through_and_data_definition_are_the_stored_text() {
    let rows = [typ(QueryKind::PassThrough, Some("Select * from Table1"), Some("ODBC;"))];
    // No clauses, no semicolon of ours, and the connection string (which may hold a password)
    // is not part of the text.
    assert_eq!(sql(QueryKind::PassThrough, &rows), "Select * from Table1");
    let rows = [typ(QueryKind::DataDefinition, Some("Drop table Table1"), None)];
    assert_eq!(sql(QueryKind::DataDefinition, &rows), "Drop table Table1");
    // Strings written by Access end in a NUL.
    let rows = [typ(QueryKind::DataDefinition, Some("CREATE TABLE T5 (c CHAR);\0"), None)];
    assert_eq!(sql(QueryKind::DataDefinition, &rows), "CREATE TABLE T5 (c CHAR);");
}

#[test]
fn update_queries() {
    let mut rows = vec![
        typ(QueryKind::Update, None, None),
        table("Table1", None),
        row(attr::COLUMN, Some("\"some string\""), None, None, None, Some("Table1.id")),
        row(attr::COLUMN, Some("42"), None, None, None, Some("Table1.col1")),
    ];
    assert_eq!(
        sql(QueryKind::Update, &rows),
        "UPDATE Table1\nSET Table1.id = \"some string\", Table1.col1 = 42;"
    );
    rows.push(row(attr::WHERE, Some("(Table1.col2 < 13)"), None, None, None, None));
    assert_eq!(
        sql(QueryKind::Update, &rows),
        "UPDATE Table1\nSET Table1.id = \"some string\", Table1.col1 = 42\nWHERE (Table1.col2 < 13);"
    );
    // Two WHERE rows are not one query.
    rows.push(row(attr::WHERE, Some("x"), None, None, None, None));
    assert!(unsupported(QueryKind::Update, &rows).contains("more than once"));
}

#[test]
fn select_columns_flags_and_clauses() {
    let mut rows = vec![typ(QueryKind::Select, None, None), table("Table1", None), flagrow(1)];
    assert_eq!(sql(QueryKind::Select, &rows), "SELECT *\nFROM Table1;");
    rows.push(col("Table1.id", None));
    rows.push(col("Table1.col", Some("Some.Alias")));
    assert_eq!(
        sql(QueryKind::Select, &rows),
        "SELECT Table1.id, Table1.col AS [Some.Alias], *\nFROM Table1;"
    );
    // DISTINCT, DISTINCTROW, owner access.
    rows[2] = flagrow(3);
    assert!(sql(QueryKind::Select, &rows).starts_with("SELECT DISTINCT Table1.id"));
    rows[2] = flagrow(9);
    assert!(sql(QueryKind::Select, &rows).starts_with("SELECT DISTINCTROW Table1.id"));
    rows[2] = flagrow(7);
    assert_eq!(
        sql(QueryKind::Select, &rows),
        "SELECT DISTINCT Table1.id, Table1.col AS [Some.Alias], *\nFROM Table1\nWITH OWNERACCESS OPTION;"
    );
    // TOP n PERCENT; DISTINCT and TOP together are both written (Access allows both).
    rows[2] = row(attr::FLAGS, None, Some(49), None, Some("5"), None);
    assert!(sql(QueryKind::Select, &rows).starts_with("SELECT TOP 5 PERCENT Table1.id"));
    rows[2] = row(attr::FLAGS, None, Some(0x13), None, Some("5"), None);
    assert!(sql(QueryKind::Select, &rows).starts_with("SELECT DISTINCT TOP 5 Table1.id"));
    rows[2] = row(attr::FLAGS, None, Some(0x11), None, None, None);
    assert_eq!(unsupported(QueryKind::Select, &rows), "TOP without a count");
    rows[2] = flagrow(0);
    // Parameters.
    let params = [
        row(attr::PARAMETER, None, Some(3), None, Some("INT_VAL"), None),
        row(attr::PARAMETER, None, Some(10), Some(50), Some("TextVal"), None),
        row(attr::PARAMETER, None, Some(0), Some(50), Some("[Some Value]"), None),
    ];
    let mut with_params = params.to_vec();
    with_params.extend(rows.iter().cloned());
    assert_eq!(
        sql(QueryKind::Select, &with_params),
        "PARAMETERS INT_VAL Short, TextVal Text(50), [Some Value] Value;\nSELECT Table1.id, Table1.col AS [Some.Alias]\nFROM Table1;"
    );
    with_params.push(row(attr::PARAMETER, None, Some(-1), None, Some("BadVal"), None));
    assert_eq!(unsupported(QueryKind::Select, &with_params), "an unknown parameter type");
    // Tables with aliases and a sub-query source.
    rows.push(table("Table2", Some("Another Table")));
    rows.push(row(
        attr::TABLE,
        Some("Select val from Table3"),
        None,
        None,
        Some("val"),
        Some("Table3Val"),
    ));
    assert_eq!(
        sql(QueryKind::Select, &rows),
        "SELECT Table1.id, Table1.col AS [Some.Alias]\nFROM Table1, Table2 AS [Another Table], [Select val from Table3].val AS Table3Val;"
    );
    // Remote database.
    let mut remote_rows = rows.clone();
    remote_rows.push(row(attr::REMOTE_DB, None, Some(2), None, Some("other_db.mdb"), None));
    assert!(sql(QueryKind::Select, &remote_rows).ends_with("AS Table3Val IN 'other_db.mdb';"));
    let last = remote_rows.len() - 1;
    remote_rows[last] =
        row(attr::REMOTE_DB, Some("MDB_FILE;"), Some(2), None, Some("other_db.mdb"), None);
    assert!(sql(QueryKind::Select, &remote_rows).ends_with("IN 'other_db.mdb' [MDB_FILE;];"));
    remote_rows[last] = row(attr::REMOTE_DB, Some("MDB_FILE;"), Some(2), None, None, None);
    assert!(sql(QueryKind::Select, &remote_rows).ends_with("IN '' [MDB_FILE;];"));
    // WHERE, GROUP BY, HAVING, ORDER BY.
    rows.push(row(attr::WHERE, Some("(Table1.col2 < 13)"), None, None, None, None));
    rows.push(row(attr::GROUP_BY, Some("Table1.id"), None, None, None, None));
    rows.push(row(attr::GROUP_BY, Some("SUM(Table1.val)"), None, None, None, None));
    rows.push(row(attr::HAVING, Some("(SUM(Table1.val) = 500)"), None, None, None, None));
    rows.push(row(attr::ORDER_BY, Some("Table1.id"), None, None, None, None));
    rows.push(row(attr::ORDER_BY, Some("Table2.val"), None, None, Some("D"), None));
    assert_eq!(
        sql(QueryKind::Select, &rows),
        "SELECT Table1.id, Table1.col AS [Some.Alias]\n\
         FROM Table1, Table2 AS [Another Table], [Select val from Table3].val AS Table3Val\n\
         WHERE (Table1.col2 < 13)\n\
         GROUP BY Table1.id, SUM(Table1.val)\n\
         HAVING (SUM(Table1.val) = 500)\n\
         ORDER BY Table1.id, Table2.val DESC;"
    );
}

#[test]
fn append_queries() {
    let values = |n1: Option<&str>, n2: Option<&str>| {
        let rows = [
            row(attr::TYPE, None, Some(3), None, Some("Table2"), None),
            row(attr::COLUMN, Some("54"), Some(APPEND_VALUE), None, None, n1),
            row(attr::COLUMN, Some("'hello'"), Some(APPEND_VALUE), None, None, n2),
        ];
        sql(QueryKind::Append, &rows)
    };
    assert_eq!(values(None, None), "INSERT INTO Table2\nVALUES (54, 'hello');");
    assert_eq!(
        values(Some("ID"), Some("Field 3")),
        "INSERT INTO Table2 (ID, [Field 3])\nVALUES (54, 'hello');"
    );
    // The select form: target names come from Name2 of the column rows.
    let rows = [
        row(attr::TYPE, None, Some(3), None, Some("Table3"), None),
        row(attr::COLUMN, Some("[Table1].[col2]"), None, None, None, Some("col2")),
        row(attr::COLUMN, Some("[Table2].[col3]"), None, None, None, Some("col3")),
        table("Table1", None),
        table("Table2", None),
        row(
            attr::JOIN,
            Some("[Table1].[col1]=[Table2].[col1]"),
            Some(1),
            None,
            Some("Table1"),
            Some("Table2"),
        ),
    ];
    assert_eq!(
        sql(QueryKind::Append, &rows),
        "INSERT INTO Table3 (col2, col3)\nSELECT [Table1].[col2], [Table2].[col3]\nFROM Table1 INNER JOIN Table2 ON [Table1].[col1]=[Table2].[col1];"
    );
}

#[test]
fn make_table_and_delete() {
    let rows = [
        row(attr::TYPE, None, Some(2), None, Some("Table4"), None),
        col("Max(Table2.col1)", Some("MaxOfcol1")),
        col("Table2.col2", None),
        table("Table2", None),
        table("Table1", None),
        row(attr::JOIN, Some("Table2.col1 = Table1.col2"), Some(1), None, Some("Table2"), Some("Table1")),
        row(attr::GROUP_BY, Some("Table2.col2"), None, None, None, None),
        row(attr::HAVING, Some("(((Max(Table2.col1))=\"buzz\"))"), None, None, None, None),
    ];
    assert_eq!(
        sql(QueryKind::MakeTable, &rows),
        "SELECT Max(Table2.col1) AS MaxOfcol1, Table2.col2 INTO Table4\nFROM Table2 INNER JOIN Table1 ON Table2.col1 = Table1.col2\nGROUP BY Table2.col2\nHAVING (((Max(Table2.col1))=\"buzz\"));"
    );
    // A make-table query into another database.
    let mut other = rows.to_vec();
    other[0] = row(attr::TYPE, Some("Jet;"), Some(2), None, Some("My Table"), Some("c:\\x.mdb"));
    assert!(sql(QueryKind::MakeTable, &other).contains("INTO [My Table] IN 'c:\\x.mdb' [Jet;]\nFROM"));
    let rows = [
        typ(QueryKind::Delete, None, None),
        col("Table1.col1", None),
        col("Table1.col2", None),
        table("Table1", None),
        row(attr::WHERE, Some("(((Table1.col1)>\"blah\"))"), None, None, None, None),
    ];
    assert_eq!(
        sql(QueryKind::Delete, &rows),
        "DELETE Table1.col1, Table1.col2\nFROM Table1\nWHERE (((Table1.col1)>\"blah\"));"
    );
}

#[test]
fn crosstab_queries() {
    let rows = [
        typ(QueryKind::Crosstab, None, None),
        row(attr::COLUMN, Some("Count([Table2].[col2])"), Some(0), None, Some("CountOfcol2"), None),
        row(attr::COLUMN, Some("Table2_1.col1"), Some(2), None, None, None),
        row(attr::COLUMN, Some("[Table2].[col3]"), Some(2), None, None, None),
        row(attr::COLUMN, Some("[Table1].[col1]"), Some(1), None, None, None),
        table("Table1", None),
        table("Table2", None),
        row(attr::GROUP_BY, Some("Table2_1.col1"), Some(2), None, None, None),
        row(attr::GROUP_BY, Some("[Table1].[col1]"), Some(1), None, None, None),
        row(attr::ORDER_BY, Some("[Table2].[col3]"), None, None, None, None),
        row(
            attr::JOIN,
            Some("[Table1].[col1]=[Table2].[col1]"),
            Some(1),
            None,
            Some("Table1"),
            Some("Table2"),
        ),
    ];
    assert_eq!(
        sql(QueryKind::Crosstab, &rows),
        "TRANSFORM Count([Table2].[col2]) AS CountOfcol2\nSELECT Table2_1.col1, [Table2].[col3]\nFROM Table1 INNER JOIN Table2 ON [Table1].[col1]=[Table2].[col1]\nGROUP BY Table2_1.col1\nORDER BY [Table2].[col3]\nPIVOT [Table1].[col1];"
    );
    // No PIVOT row: not a cross-tab query.
    let broken: Vec<QueryRow> = rows
        .iter()
        .filter(|r| r.flag != Some(1) || r.attribute != attr::COLUMN)
        .cloned()
        .collect();
    assert_eq!(
        unsupported(QueryKind::Crosstab, &broken),
        "a cross-tab query needs exactly one PIVOT row"
    );
}

#[test]
fn the_type_row_must_agree_with_the_catalog() {
    let rows = [row(attr::TYPE, None, Some(9), None, None, None), table("T", None), flagrow(1)];
    // The catalog says select, the type row says union: the type row wins when the catalog flags
    // are zero, and then the union is incomplete.
    match reconstruct(0, &rows) {
        Outcome::Unsupported(Some(QueryKind::Union), _) => {}
        other => panic!("{other:?}"),
    }
    // A delete in the catalog contradicts a select type row.
    let rows = [typ(QueryKind::Select, None, None), table("T", None), flagrow(1)];
    assert!(matches!(reconstruct(0x20, &rows), Outcome::Unsupported(Some(QueryKind::Delete), _)));
    assert!(matches!(reconstruct(0x90, &rows), Outcome::Unsupported(None, _)));
}

fn join_rows(tables: usize, joins: &[(usize, usize, i64)]) -> Vec<QueryRow> {
    let mut rows = vec![typ(QueryKind::Select, None, None), flagrow(1)];
    for i in 1..=tables {
        rows.push(table(&format!("Table{i}"), None));
    }
    for (i, (from, to, kind)) in joins.iter().enumerate() {
        let i = i * 3;
        rows.push(row(
            attr::JOIN,
            Some(&format!("Table{from}.f{i} = Table{to}.f{i}")),
            Some(*kind),
            None,
            Some(&format!("Table{from}")),
            Some(&format!("Table{to}")),
        ));
    }
    rows
}

fn from_of(rows: &[QueryRow]) -> String {
    let s = sql(QueryKind::Select, rows);
    s.strip_prefix("SELECT *\nFROM ").and_then(|s| s.strip_suffix(';')).unwrap().to_owned()
}

#[test]
fn joins_nest_the_way_access_writes_them() {
    assert_eq!(
        from_of(&join_rows(10, &[(1, 2, 1), (2, 3, 1), (3, 4, 1)])),
        "Table5, Table6, Table7, Table8, Table9, Table10, ((Table1 INNER JOIN Table2 ON Table1.f0 = Table2.f0) INNER JOIN Table3 ON Table2.f3 = Table3.f3) INNER JOIN Table4 ON Table3.f6 = Table4.f6"
    );
    // The same pair joined twice: the second condition is added in front.
    assert_eq!(
        from_of(&join_rows(10, &[(1, 2, 1), (2, 1, 1)])),
        "Table3, Table4, Table5, Table6, Table7, Table8, Table9, Table10, Table1 INNER JOIN Table2 ON (Table2.f3 = Table1.f3) AND (Table1.f0 = Table2.f0)"
    );
    // The same pair with two join types cannot be written.
    assert_eq!(
        unsupported(QueryKind::Select, &join_rows(10, &[(1, 2, 1), (2, 1, 2)])),
        "joins of different types between the same tables"
    );
    assert_eq!(
        from_of(&join_rows(10, &[(1, 2, 1), (3, 4, 1), (5, 6, 1), (7, 8, 1), (2, 3, 1)])),
        "Table9, Table10, Table5 INNER JOIN Table6 ON Table5.f6 = Table6.f6, Table7 INNER JOIN Table8 ON Table7.f9 = Table8.f9, (Table1 INNER JOIN Table2 ON Table1.f0 = Table2.f0) INNER JOIN (Table3 INNER JOIN Table4 ON Table3.f3 = Table4.f3) ON Table2.f12 = Table3.f12"
    );
    assert_eq!(
        from_of(&join_rows(
            10,
            &[(1, 2, 1), (3, 4, 1), (5, 6, 1), (7, 8, 1), (2, 3, 1), (5, 8, 1)]
        )),
        "Table9, Table10, (Table1 INNER JOIN Table2 ON Table1.f0 = Table2.f0) INNER JOIN (Table3 INNER JOIN Table4 ON Table3.f3 = Table4.f3) ON Table2.f12 = Table3.f12, (Table5 INNER JOIN Table6 ON Table5.f6 = Table6.f6) INNER JOIN (Table7 INNER JOIN Table8 ON Table7.f9 = Table8.f9) ON Table5.f15 = Table8.f15"
    );
    assert_eq!(
        from_of(&join_rows(
            10,
            &[
                (1, 2, 1),
                (1, 3, 1),
                (6, 3, 1),
                (1, 9, 2),
                (10, 9, 3),
                (7, 8, 3),
                (1, 8, 2),
                (1, 4, 2),
                (5, 4, 3)
            ]
        )),
        "Table5 RIGHT JOIN (((Table10 RIGHT JOIN ((Table6 INNER JOIN ((Table1 INNER JOIN Table2 ON Table1.f0 = Table2.f0) INNER JOIN Table3 ON Table1.f3 = Table3.f3) ON Table6.f6 = Table3.f6) LEFT JOIN Table9 ON Table1.f9 = Table9.f9) ON Table10.f12 = Table9.f12) LEFT JOIN (Table7 RIGHT JOIN Table8 ON Table7.f15 = Table8.f15) ON Table1.f18 = Table8.f18) LEFT JOIN Table4 ON Table1.f21 = Table4.f21) ON Table5.f24 = Table4.f24"
    );
    assert_eq!(
        from_of(&join_rows(
            10,
            &[
                (1, 2, 1),
                (1, 3, 1),
                (1, 9, 2),
                (1, 8, 2),
                (1, 4, 2),
                (6, 3, 1),
                (5, 4, 3),
                (7, 8, 3),
                (10, 9, 3)
            ]
        )),
        "Table10 RIGHT JOIN (Table7 RIGHT JOIN (Table5 RIGHT JOIN (Table6 INNER JOIN (((((Table1 INNER JOIN Table2 ON Table1.f0 = Table2.f0) INNER JOIN Table3 ON Table1.f3 = Table3.f3) LEFT JOIN Table9 ON Table1.f6 = Table9.f6) LEFT JOIN Table8 ON Table1.f9 = Table8.f9) LEFT JOIN Table4 ON Table1.f12 = Table4.f12) ON Table6.f15 = Table3.f15) ON Table5.f18 = Table4.f18) ON Table7.f21 = Table8.f21) ON Table10.f24 = Table9.f24"
    );
    // A join that mentions a table the query does not list adds it as a bare name.
    let mut rows = join_rows(0, &[]);
    rows.push(table("Table1", None));
    rows.push(row(
        attr::JOIN,
        Some("(Table1.id = Table3Val.id)"),
        Some(1),
        None,
        Some("BogusTable"),
        Some("Table3Val"),
    ));
    assert_eq!(from_of(&rows), "Table1, BogusTable INNER JOIN Table3Val ON (Table1.id = Table3Val.id)");
    // An unknown join type and a missing condition are refused.
    let bad = join_rows(2, &[(1, 2, 5)]);
    assert_eq!(unsupported(QueryKind::Select, &bad), "an unknown join type");
    let mut bad = join_rows(2, &[(1, 2, 1)]);
    bad.last_mut().unwrap().expression = None;
    assert_eq!(unsupported(QueryKind::Select, &bad), "a join without a condition");
}

#[test]
fn names_are_bracketed_the_way_access_does() {
    assert_eq!(ident("Table1"), "Table1");
    assert_eq!(ident("My Table"), "[My Table]");
    assert_eq!(ident("db.My Table"), "db.[My Table]");
    assert_eq!(ident("[Already]"), "[Already]");
    assert_eq!(ident("Таблица"), "[Таблица]");
    assert_eq!(ident("a."), "a");
    assert_eq!(ident(""), "");
    assert_eq!(alias(Some("x y")), " AS [x y]");
    assert_eq!(alias(Some("[x y]")), " AS [x y]");
    assert_eq!(alias(None), "");
}

#[test]
fn hostile_rows_end_in_a_refusal_not_in_a_hang() {
    // Many tables, many joins, deep nesting.
    let many: Vec<(usize, usize, i64)> = (1..=MAX_JOINS + 5).map(|i| (i, i + 1, 1)).collect();
    let rows = join_rows(MAX_JOINS + 8, &many);
    assert!(matches!(reconstruct(0, &rows), Outcome::Unsupported(..)));
    // A chain of joins within the limits nests and finishes.
    let chain: Vec<(usize, usize, i64)> = (1..40).map(|i| (i, i + 1, 1)).collect();
    let out = sql(QueryKind::Select, &join_rows(40, &chain));
    assert!(out.starts_with("SELECT *\nFROM ") && out.len() < 20_000);
    // A star of joins on one table.
    let star: Vec<(usize, usize, i64)> = (2..60).map(|i| (1, i, 2)).collect();
    let _ = reconstruct(0, &join_rows(60, &star));
    // Huge expressions are copied once, not multiplied.
    let big = "x".repeat(300_000);
    let rows = [typ(QueryKind::Select, None, None), col(&big, None), table("T", None)];
    match reconstruct(0, &rows) {
        Outcome::Sql(_, s) => assert!(s.len() < 400_000),
        Outcome::Unsupported(..) => {}
    }
}

/// A small deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[test]
fn random_rows_never_panic_and_stay_bounded() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let names = ["T1", "T2", "T3", "a b", "", "X7YZ_____1", "X7YZ_____2", "[q]", "t.c"];
    for _ in 0..4000 {
        let n = rng.below(24);
        let mut rows = Vec::new();
        for _ in 0..n {
            let mut pick = |rng: &mut Rng| {
                if rng.below(4) == 0 {
                    None
                } else {
                    names.get(usize::try_from(rng.below(names.len() as u64)).unwrap()).map(|s| (*s).to_owned())
                }
            };
            rows.push(QueryRow {
                attribute: i64::try_from(rng.below(14)).unwrap() - 1,
                expression: pick(&mut rng),
                flag: if rng.below(3) == 0 {
                    None
                } else {
                    Some(i64::try_from(rng.below(0x10000)).unwrap() - 0x8000)
                },
                extra: if rng.below(3) == 0 { None } else { Some(i64::try_from(rng.below(100)).unwrap()) },
                name1: pick(&mut rng),
                name2: pick(&mut rng),
                order: Vec::new(),
            });
        }
        let flags = u32::try_from(rng.below(0x100)).unwrap();
        if let Outcome::Sql(_, s) = reconstruct(flags, &rows) {
            assert!(s.len() <= MAX_SQL_BYTES);
            assert!(!s.contains('\0'));
        }
    }
}
