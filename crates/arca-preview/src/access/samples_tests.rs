//! Checks against databases that Access itself wrote.
//!
//! The test databases of the open-source Jackcess library (Apache-2.0) are real files from
//! Access 97 to 2010, and its tests state what every saved query of `queryTest*.mdb` must read
//! as. They are not part of this repository: set `ARCA_ACCESS_SAMPLES` to a directory holding
//! `queryTestV1997.mdb`, `queryTestV2000.mdb`, `queryTestV2003.mdb`, `queryTestV2007.accdb`,
//! `queryTestV2010.accdb` (and any other databases, which are only read). Without the variable
//! the tests do nothing. `ARCA_ACCESS_SAMPLES_OUT` names a file that receives the text of every
//! preview, for looking at.

use crate::model::{Block, Preview};
use std::fmt::Write as _;

/// The SQL of each saved query of `queryTest*`, as Jackcess's `QueryTest.testReadQueries` states
/// it (Access wrote these queries). The two strings Jackcess keeps with a trailing NUL end here
/// without it, which is how they are displayed.
const EXPECTED: [(&str, &str); 9] = [
    (
        "SelectQuery",
        "SELECT DISTINCT Table1.*, Table2.col1, Table2.col2, Table3.col3\n\
         FROM (Table1 LEFT JOIN Table3 ON Table1.col1 = Table3.col1) INNER JOIN Table2 ON (Table3.col1 = Table2.col2) AND (Table3.col1 = Table2.col1)\n\
         WHERE (((Table2.col2)=\"foo\" Or (Table2.col2) In (\"buzz\",\"bazz\")))\n\
         ORDER BY Table2.col1;",
    ),
    (
        "DeleteQuery",
        "DELETE Table1.col1, Table1.col2, Table1.col3\n\
         FROM Table1\n\
         WHERE (((Table1.col1)>\"blah\"));",
    ),
    (
        "AppendQuery",
        "INSERT INTO Table3 (col2, col2, col3)\n\
         SELECT [Table1].[col2], [Table2].[col2], [Table2].[col3]\n\
         FROM Table3, Table1 INNER JOIN Table2 ON [Table1].[col1]=[Table2].[col1];",
    ),
    (
        "UpdateQuery",
        "PARAMETERS User Name Text;\n\
         UPDATE Table1\n\
         SET Table1.col1 = \"foo\", Table1.col2 = [Table2].[col3], [Table2].[col1] = [User Name]\n\
         WHERE ((([Table2].[col1]) Is Not Null));",
    ),
    (
        "MakeTableQuery",
        "SELECT Max(Table2.col1) AS MaxOfcol1, Table2.col2, Table3.col2 INTO Table4\n\
         FROM (Table2 INNER JOIN Table1 ON Table2.col1 = Table1.col2) RIGHT JOIN Table3 ON Table1.col2 = Table3.col3\n\
         GROUP BY Table2.col2, Table3.col2\n\
         HAVING (((Max(Table2.col1))=\"buzz\") AND ((Table2.col2)<>\"blah\"));",
    ),
    (
        "CrosstabQuery",
        "TRANSFORM Count([Table2].[col2]) AS CountOfcol2\n\
         SELECT Table2_1.col1, [Table2].[col3], Avg(Table2_1.col2) AS AvgOfcol2\n\
         FROM (Table1 INNER JOIN Table2 ON [Table1].[col1]=[Table2].[col1]) INNER JOIN Table2 AS Table2_1 ON [Table2].[col1]=Table2_1.col3\n\
         WHERE ((([Table1].[col1])>\"10\") And ((Table2_1.col1) Is Not Null) And ((Avg(Table2_1.col2))>\"10\"))\n\
         GROUP BY Table2_1.col1, [Table2].[col3]\n\
         ORDER BY [Table2].[col3]\n\
         PIVOT [Table1].[col1];",
    ),
    (
        "UnionQuery",
        "Select Table1.col1, Table1.col2\n\
         where Table1.col1 = \"foo\"\n\
         UNION\n\
         Select Table2.col1, Table2.col2\n\
         UNION ALL Select Table3.col1, Table3.col2\n\
         where Table3.col3 > \"blah\";",
    ),
    ("PassthroughQuery", "ALTER TABLE Table4 DROP COLUMN col5;"),
    ("DataDefinitionQuery", "CREATE TABLE Table5 (col1 CHAR, col2 CHAR);"),
];

fn text_of(p: &Preview) -> String {
    let mut out = String::new();
    for s in &p.sections {
        let _ = writeln!(out, "## {:?} {}{}", s.kind, s.name, if s.hidden { " (hidden)" } else { "" });
        for b in &s.blocks {
            match b {
                Block::Para(para) => {
                    for r in &para.runs {
                        let _ = writeln!(out, "{}", r.text);
                    }
                }
                Block::Grid(g) => {
                    for row in g.rows.iter().take(40) {
                        let cells: Vec<String> = row
                            .cells
                            .iter()
                            .map(|c| c.text.chars().take(120).collect::<String>())
                            .collect();
                        let _ = writeln!(out, "| {}", cells.join(" | "));
                    }
                    if g.rows.len() > 40 {
                        let _ = writeln!(out, "| ... {} rows", g.rows.len());
                    }
                }
                _ => {}
            }
        }
    }
    for n in &p.notices {
        let _ = writeln!(out, "NOTICE {:?} {} {}", n.kind, n.count, n.detail);
    }
    out
}

fn query_sql(p: &Preview, name: &str) -> Option<String> {
    let s = p.sections.iter().find(|s| s.name == format!("Query: {name}"))?;
    let text = s.blocks.iter().find_map(|b| match b {
        Block::Para(p) => p.runs.first().map(|r| r.text.clone()),
        _ => None,
    })?;
    // Drop the "-- KIND" line the preview puts above the statement.
    text.split_once('\n').map(|(_, sql)| sql.to_owned())
}

#[test]
fn queries_written_by_access_read_as_jackcess_says() {
    let Ok(dir) = std::env::var("ARCA_ACCESS_SAMPLES") else { return };
    let dir = std::path::PathBuf::from(dir);
    let mut report = String::new();
    let mut checked = 0usize;
    let mut query_files = 0usize;
    let mut failures = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir).expect("samples directory").flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.ends_with(".mdb") || name.ends_with(".accdb")) {
            continue;
        }
        let data = std::fs::read(e.path()).expect("reads");
        let p = match crate::preview(&data, &crate::PreviewPolicy::default()) {
            Ok(p) => p,
            Err(err) => {
                let _ = writeln!(report, "#### {name}: {err:?}\n");
                continue;
            }
        };
        let _ = write!(report, "#### {name}\n{}\n", text_of(&p));
        if name.starts_with("queryTest") {
            query_files += 1;
            for (query, expected) in EXPECTED {
                match query_sql(&p, query) {
                    Some(got) if got == expected => checked += 1,
                    other => failures.push(format!("{name}: {query}\n  got      {other:?}\n  expected {expected:?}")),
                }
            }
        }
    }
    if let Ok(out) = std::env::var("ARCA_ACCESS_SAMPLES_OUT") {
        std::fs::write(out, &report).expect("writes");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(checked, query_files * EXPECTED.len(), "not every saved query was found");
}
