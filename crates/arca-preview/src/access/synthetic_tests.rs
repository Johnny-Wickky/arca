//! Previews of databases built by the test-only writer (`arca-container/src/jet/testdb.rs`):
//! saved queries of every kind, relationships, indexes, complex and calculated columns and the
//! database properties — structures the small fixtures written by Jackcess do not contain.

#![allow(clippy::string_slice, reason = "test code with ASCII text")]

#[path = "../../../arca-container/src/jet/testdb.rs"]
mod testdb;

use crate::model::{Block, NoticeKind, Preview, SectionKind, style};
use testdb::{ColSpec, Cv, IndexSpec, TestDb, int, t, text};

/// A stored row of `MSysQueries`: attribute, expression, flag, extra, name1, name2.
type Qr = (
    u8,
    Option<&'static str>,
    Option<i16>,
    Option<i32>,
    Option<&'static str>,
    Option<&'static str>,
);

const SYSTEM: i64 = -2_147_483_648;

fn q_columns() -> Vec<ColSpec> {
    vec![
        ColSpec::new("Attribute", t::BYTE),
        ColSpec::new("Expression", t::MEMO),
        ColSpec::new("Flag", t::INT),
        ColSpec::new("LvExtra", t::LONG),
        ColSpec::new("Name1", t::TEXT),
        ColSpec::new("Name2", t::TEXT),
        ColSpec::new("ObjectId", t::LONG),
        ColSpec::new("Order", t::BINARY),
    ]
}

fn opt_text(s: Option<&str>) -> Cv {
    s.map_or(Cv::Null, text)
}

fn q_row(id: i64, n: usize, r: &Qr) -> Vec<Cv> {
    vec![
        int(i64::from(r.0)),
        r.1.map_or(Cv::Null, |e| {
            if e.len() > 1500 {
                Cv::Big(e.encode_utf16().flat_map(u16::to_le_bytes).collect())
            } else {
                Cv::Memo(e.to_owned())
            }
        }),
        r.2.map_or(Cv::Null, |f| int(i64::from(f))),
        r.3.map_or(Cv::Null, |f| int(i64::from(f))),
        opt_text(r.4),
        opt_text(r.5),
        int(id),
        Cv::Raw(u16::try_from(n).unwrap().to_be_bytes().to_vec()),
    ]
}

/// A database with a table `Customers`, the given queries (name, catalog flags, rows) and
/// whatever `extra` adds.
fn query_db(queries: &[(&str, i64, Vec<Qr>)], extra: impl FnOnce(&mut TestDb)) -> Vec<u8> {
    let mut tdb = TestDb::new(1);
    tdb.add_table(
        "Customers",
        0,
        &[ColSpec::new("id", t::LONG), ColSpec::new("name", t::TEXT)],
        &[vec![int(1), text("Alice")]],
        &[],
    );
    let mut rows = Vec::new();
    for (i, (name, flags, qrows)) in queries.iter().enumerate() {
        let id = 0x2000 + i64::try_from(i).unwrap();
        tdb.add_object(id, 5, name, *flags);
        for (n, r) in qrows.iter().enumerate() {
            rows.push(q_row(id, n, r));
        }
    }
    tdb.add_table("MSysQueries", SYSTEM, &q_columns(), &rows, &[]);
    extra(&mut tdb);
    tdb.finish()
}

fn preview_bytes(data: &[u8]) -> Preview {
    crate::preview(data, &crate::PreviewPolicy::default()).unwrap()
}

fn grid_lines(b: &Block) -> Vec<String> {
    let Block::Grid(g) = b else { panic!("not a grid") };
    g.rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>().join("|"))
        .collect()
}

/// The text of the first paragraph of the section called `name`, with the style of its run.
fn first_para(p: &Preview, name: &str) -> Option<(String, u16)> {
    let s = p.sections.iter().find(|s| s.name == name)?;
    s.blocks.iter().find_map(|b| match b {
        Block::Para(p) => p.runs.first().map(|r| (r.text.clone(), r.style)),
        _ => None,
    })
}

fn section_lines(p: &Preview, name: &str) -> Vec<String> {
    let s =
        p.sections.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no section {name}"));
    s.blocks.iter().find(|b| matches!(b, Block::Grid(_))).map(grid_lines).unwrap_or_default()
}

#[test]
fn saved_queries_of_every_kind_are_shown_as_synthetic_sql() {
    const UNION1: &str = "X7YZ_____1";
    const UNION2: &str = "X7YZ_____2";
    let queries: Vec<(&str, i64, Vec<Qr>)> = vec![
        (
            "qSelect",
            0,
            vec![
                (1, None, Some(1), None, None, None),
                (6, Some("Customers.name"), None, None, Some("Who"), None),
                (5, None, None, None, Some("Customers"), None),
                (8, Some("(((Customers.id)>1))"), None, None, None, None),
                (11, Some("Customers.name"), None, None, Some("D"), None),
            ],
        ),
        (
            "qDelete",
            0x20,
            vec![
                (1, None, Some(5), None, None, None),
                (6, Some("Customers.id"), None, None, None, None),
                (5, None, None, None, Some("Customers"), None),
            ],
        ),
        (
            "qUnion",
            0x80,
            vec![
                (1, None, Some(9), None, None, None),
                (3, None, Some(1), None, None, None),
                (5, Some("Select 1 as a"), None, None, None, Some(UNION1)),
                (5, Some("Select 2 as a"), None, None, None, Some(UNION2)),
            ],
        ),
        (
            "qPass",
            0x70,
            vec![(1, Some("EXEC sp_who\0"), Some(8), None, Some("ODBC;DSN=x;PWD=secret"), None)],
        ),
        ("qDdl", 0x60, vec![(1, Some("DROP TABLE Customers"), Some(7), None, None, None)]),
        (
            "qParam",
            0,
            vec![
                (1, None, Some(1), None, None, None),
                (2, None, Some(10), Some(30), Some("Who"), None),
                (6, Some("Customers.id"), None, None, None, None),
                (5, None, None, None, Some("Customers"), None),
                (8, Some("Customers.name = [Who]"), None, None, None, None),
            ],
        ),
    ];
    let p = preview_bytes(&query_db(&queries, |_| {}));
    let sql = |name: &str| {
        let (text, st) =
            first_para(&p, &format!("Query: {name}")).unwrap_or_else(|| panic!("{name}"));
        // Everything the viewer makes up is marked.
        assert_eq!(st, style::MONO | style::SYNTHETIC, "{name}");
        text
    };
    assert_eq!(
        sql("qSelect"),
        "-- SELECT\nSELECT Customers.name AS Who\nFROM Customers\nWHERE (((Customers.id)>1))\nORDER BY Customers.name DESC;"
    );
    assert_eq!(sql("qDelete"), "-- DELETE\nDELETE Customers.id\nFROM Customers;");
    assert_eq!(sql("qUnion"), "-- UNION\nSelect 1 as a\nUNION ALL Select 2 as a;");
    // The connection string (it may carry a password) is not part of the text.
    assert_eq!(sql("qPass"), "-- PASS-THROUGH\nEXEC sp_who");
    assert_eq!(sql("qDdl"), "-- DDL\nDROP TABLE Customers");
    assert_eq!(
        sql("qParam"),
        "-- SELECT\nPARAMETERS Who Text(30);\nSELECT Customers.id\nFROM Customers\nWHERE Customers.name = [Who];"
    );
    // Rebuilt queries have no grid of stored rows.
    let s = p.sections.iter().find(|s| s.name == "Query: qSelect").unwrap();
    assert!(s.blocks.iter().all(|b| matches!(b, Block::Para(_))));
    assert!(
        p.notices
            .iter()
            .any(|n| n.kind == NoticeKind::BestEffort && n.detail.contains("never run"))
    );
    assert!(!p.notices.iter().any(|n| n.detail.contains("stored rows, not as SQL")));
    assert!(!format!("{p:?}").contains("secret"), "the password of the connection string is shown");
}

#[test]
fn a_query_that_does_not_fit_is_shown_as_its_stored_rows() {
    let queries: Vec<(&str, i64, Vec<Qr>)> = vec![(
        "qOdd",
        0,
        vec![
            (1, None, Some(1), None, None, None),
            (6, Some("x"), None, None, None, None),
            (5, None, None, None, Some("T"), None),
            (8, Some("a = 1"), None, None, None, None),
            (8, Some("b = 2"), None, None, None, None),
        ],
    )];
    let p = preview_bytes(&query_db(&queries, |_| {}));
    let (text, _) = first_para(&p, "Query: qOdd").unwrap();
    assert!(text.starts_with("-- SELECT: not rebuilt as SQL"), "{text}");
    let lines = section_lines(&p, "Query: qOdd");
    assert_eq!(lines[0], "Attribute|Flag|Extra|Name1|Name2|Expression");
    assert_eq!(lines.len(), 1 + 5);
    assert!(lines.contains(&"8|b = 2".to_owned()), "{lines:?}");
    assert!(
        p.notices
            .iter()
            .any(|n| n.kind == NoticeKind::Unsupported && n.detail.contains("stored rows"))
    );
}

#[test]
fn a_query_without_definition_rows_is_skipped_not_invented() {
    let mut tdb = TestDb::new(1);
    tdb.add_object(0x2000, 5, "qGhost", 0);
    let p = preview_bytes(&tdb.finish());
    assert!(p.sections.iter().all(|s| s.name != "Query: qGhost"));
    assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Skipped && n.detail.contains("qGhost")));
}

#[test]
fn forms_reports_macros_and_modules_are_listed_by_name_only() {
    let mut tdb = TestDb::new(1);
    tdb.add_object(0x3001, -32768, "frmMain", 0);
    tdb.add_object(0x3002, -32764, "rptSales", 0);
    tdb.add_object(0x3003, -32766, "AutoExec", 0);
    tdb.add_object(0x3004, -32761, "modUtil", 0);
    tdb.add_object(0x3005, -32758, "pageWeb", 0);
    let p = preview_bytes(&tdb.finish());
    let lines = section_lines(&p, "Database");
    for l in [
        "Form|frmMain",
        "Report|rptSales",
        "Macro|AutoExec",
        "Module|modUtil",
        "Data access page|pageWeb",
    ] {
        assert!(lines.iter().any(|x| x.starts_with(l)), "{l}: {lines:?}");
    }
    // No section is made for them: their content is never shown.
    assert_eq!(p.sections.len(), 1);
    assert!(p.notices.iter().any(|n| n.detail.contains("never run")));
}

#[test]
fn relationships_and_indexes_are_listed() {
    let data = query_db(&[], |tdb| {
        tdb.add_table(
            "Orders",
            0,
            &[ColSpec::new("OrderID", t::LONG), ColSpec::new("Customer", t::LONG)],
            &[],
            &[
                IndexSpec {
                    name: "PrimaryKey".into(),
                    columns: vec![(0, true)],
                    kind: 1,
                    unique: true,
                },
                IndexSpec {
                    name: "CustomersOrders".into(),
                    columns: vec![(1, false)],
                    kind: 2,
                    unique: false,
                },
            ],
        );
        let cols = [
            ColSpec::new("ccolumn", t::LONG),
            ColSpec::new("grbit", t::LONG),
            ColSpec::new("icolumn", t::LONG),
            ColSpec::new("szColumn", t::TEXT),
            ColSpec::new("szObject", t::TEXT),
            ColSpec::new("szReferencedColumn", t::TEXT),
            ColSpec::new("szReferencedObject", t::TEXT),
            ColSpec::new("szRelationship", t::TEXT),
        ];
        let rel = |i: i64, col: &str, rcol: &str| {
            vec![
                int(2),
                int(0x1100),
                int(i),
                text(col),
                text("Orders"),
                text(rcol),
                text("Customers"),
                text("CustomersOrders"),
            ]
        };
        // Stored out of order: the column pairs are sorted by position.
        tdb.add_table(
            "MSysRelationships",
            SYSTEM,
            &cols,
            &[rel(1, "Region", "region"), rel(0, "Customer", "id")],
            &[],
        );
    });
    let p = preview_bytes(&data);
    assert_eq!(
        section_lines(&p, "Indexes"),
        [
            "Table|Index|Kind|Columns",
            "Orders|PrimaryKey|Primary key|OrderID",
            "Orders|CustomersOrders|Relationship|Customer DESC",
        ]
    );
    assert_eq!(
        section_lines(&p, "Relationships"),
        [
            "Relationship|Table|Columns|Related table|Related columns|Attributes",
            "CustomersOrders|Orders|Customer, Region|Customers|id, region|cascade update, cascade delete",
        ]
    );
}

#[test]
fn the_relationship_diagram_draws_a_line_with_both_ends_marked() {
    use crate::page::Prim;
    let data = query_db(&[], |tdb| {
        tdb.add_table(
            "Orders",
            0,
            &[ColSpec::new("OrderID", t::LONG), ColSpec::new("Customer", t::LONG)],
            &[],
            &[IndexSpec {
                name: "PrimaryKey".into(),
                columns: vec![(0, true)],
                kind: 1,
                unique: true,
            }],
        );
        let cols = [
            ColSpec::new("ccolumn", t::LONG),
            ColSpec::new("grbit", t::LONG),
            ColSpec::new("icolumn", t::LONG),
            ColSpec::new("szColumn", t::TEXT),
            ColSpec::new("szObject", t::TEXT),
            ColSpec::new("szReferencedColumn", t::TEXT),
            ColSpec::new("szReferencedObject", t::TEXT),
            ColSpec::new("szRelationship", t::TEXT),
        ];
        let row = vec![
            int(1),
            int(0),
            int(0),
            text("Customer"),
            text("Orders"),
            text("id"),
            text("Customers"),
            text("CustomersOrders"),
        ];
        tdb.add_table("MSysRelationships", SYSTEM, &cols, &[row], &[]);
    });
    let page = crate::render_page(&data, 0, &crate::PreviewPolicy::default()).unwrap();
    let texts: Vec<&str> = page
        .prims
        .iter()
        .filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.as_str()) } else { None })
        .collect();
    for wanted in ["Customers", "Orders", "OrderID", "Customer", "id", "PK", "FK", "∞", "1"] {
        assert!(texts.contains(&wanted), "{wanted} missing from {texts:?}");
    }
    assert!(texts.iter().any(|t| t.starts_with("Relationships: 2 tables, 1 relationships")));
    // One line (a path without a fill) joins the two boxes.
    let lines = page
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::Path { fill: 0, stroke, .. } if *stroke != 0))
        .count();
    assert_eq!(lines, 1);
}

#[test]
fn linked_tables_are_listed_without_their_passwords() {
    let mut tdb = TestDb::new(1);
    tdb.add_link(0x4001, 4, "dbo_Sales", "ODBC;DSN=sales;UID=bob;PWD=hunter2;DATABASE=x", "");
    tdb.add_link(0x4002, 6, "Remote", "", "C:\\data\\other.mdb");
    let p = preview_bytes(&tdb.finish());
    let lines = section_lines(&p, "Database");
    assert!(
        lines.contains(
            &"Linked table (ODBC)|dbo_Sales|ODBC;DSN=sales;UID=bob;PWD=***;DATABASE=x".to_owned()
        ) || lines.contains(
            &"Linked table (ODBC)|dbo_Sales||ODBC;DSN=sales;UID=bob;PWD=***;DATABASE=x".to_owned()
        ),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.ends_with("C:\\data\\other.mdb")), "{lines:?}");
    assert!(!format!("{p:?}").contains("hunter2"));
}

fn property_list(name: &str, value: &str) -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let n = utf16(name);
    let mut names = u16::try_from(n.len()).unwrap().to_le_bytes().to_vec();
    names.extend(n);
    let val = utf16(value);
    let mut entry = u16::try_from(8 + val.len()).unwrap().to_le_bytes().to_vec();
    entry.extend([0, 0x0A, 0, 0]);
    entry.extend(u16::try_from(val.len()).unwrap().to_le_bytes());
    entry.extend(&val);
    let mut values = 4u32.to_le_bytes().to_vec();
    values.extend(entry);
    let block = |kind: u16, body: &[u8]| {
        let mut v = u32::try_from(body.len() + 6).unwrap().to_le_bytes().to_vec();
        v.extend(kind.to_le_bytes());
        v.extend(body);
        v
    };
    let mut list = b"MR2\0".to_vec();
    list.extend(block(0x80, &names));
    list.extend(block(0, &values));
    list
}

#[test]
fn the_startup_form_is_reported_never_opened() {
    let mut tdb = TestDb::new(1);
    tdb.add_object_with_props(0x5000, 3, "MSysDb", property_list("StartUpForm", "frmSwitchboard"));
    let p = preview_bytes(&tdb.finish());
    assert!(
        p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported
            && n.detail.contains("startup form (frmSwitchboard)")
            && n.detail.contains("never opened")),
        "{:?}",
        p.notices
    );
}

fn attachments_db() -> Vec<u8> {
    let mut tdb = TestDb::new(3);
    let att_type = tdb.add_table(
        "MSysComplexType_Attachment",
        SYSTEM,
        &[
            ColSpec::new("FileURL", t::MEMO),
            ColSpec::new("FileName", t::TEXT),
            ColSpec::new("FileType", t::TEXT),
            ColSpec::new("FileData", t::OLE),
            ColSpec::new("FileTimeStamp", t::DATE),
            ColSpec::new("FileFlags", t::LONG),
        ],
        &[],
        &[],
    );
    let att = |id: i64, fk: i64, name: &str, len: usize| {
        vec![
            int(id),
            int(fk),
            Cv::Null,
            text(name),
            text("txt"),
            Cv::Long(vec![9; len]),
            Cv::Null,
            int(0),
        ]
    };
    let att_flat = tdb.add_table(
        "f_Files",
        SYSTEM,
        &[
            ColSpec::new("_Id", t::LONG).flags(0x04),
            ColSpec::new("Files_FK", t::LONG).ext(0x08),
            ColSpec::new("FileURL", t::MEMO),
            ColSpec::new("FileName", t::TEXT),
            ColSpec::new("FileType", t::TEXT),
            ColSpec::new("FileData", t::OLE),
            ColSpec::new("FileTimeStamp", t::DATE),
            ColSpec::new("FileFlags", t::LONG),
        ],
        &[att(1, 7, "a.txt", 300), att(2, 7, "b.txt", 10)],
        &[],
    );
    let mv_type =
        tdb.add_table("MSysComplexType_Text", SYSTEM, &[ColSpec::new("Value", t::TEXT)], &[], &[]);
    let mv_flat = tdb.add_table(
        "f_Tags",
        SYSTEM,
        &[
            ColSpec::new("_Id", t::LONG).flags(0x04),
            ColSpec::new("Tags_FK", t::LONG).ext(0x08),
            ColSpec::new("Value", t::TEXT),
        ],
        &[vec![int(1), int(21), text("red")], vec![int(2), int(21), text("blue")]],
        &[],
    );
    let main = tdb.add_table(
        "Docs",
        0,
        &[
            ColSpec::new("id", t::LONG),
            ColSpec::new("Files", t::COMPLEX).complex(101),
            ColSpec::new("Tags", t::COMPLEX).complex(102),
            ColSpec::new("Lost", t::COMPLEX).complex(999),
        ],
        &[vec![int(1), int(7), int(21), int(5)], vec![int(2), int(8), Cv::Null, Cv::Null]],
        &[],
    );
    let reg = |id: i64, ty: usize, flat: usize, name: &str| {
        vec![
            int(id),
            int(i64::try_from(ty).unwrap()),
            int(i64::try_from(main).unwrap()),
            int(i64::try_from(flat).unwrap()),
            text(name),
        ]
    };
    tdb.add_table(
        "MSysComplexColumns",
        SYSTEM,
        &[
            ColSpec::new("ComplexID", t::LONG),
            ColSpec::new("ComplexTypeObjectID", t::LONG),
            ColSpec::new("ConceptualTableID", t::LONG),
            ColSpec::new("FlatTableID", t::LONG),
            ColSpec::new("ColumnName", t::TEXT),
        ],
        &[reg(101, att_type, att_flat, "Files"), reg(102, mv_type, mv_flat, "Tags")],
        &[],
    );
    tdb.finish()
}

#[test]
fn attachments_are_named_with_their_size_and_multi_values_are_listed() {
    let p = preview_bytes(&attachments_db());
    let lines = section_lines(&p, "Docs");
    assert_eq!(lines[0], "id|Files|Tags|Lost");
    // Attachments: name and stored size only. Values: all of them. A complex column the
    // registry does not know says so instead of showing a number.
    assert_eq!(
        lines[1],
        "1|a.txt (300 bytes); b.txt (10 bytes)|red; blue|(complex field; its values are not in the file)"
    );
    // A key without values is an empty cell.
    assert_eq!(lines[2], "2");
}

#[test]
fn a_calculated_column_is_reported_as_not_evaluated() {
    let wrapped = |data: &[u8]| {
        let mut v = vec![0u8; 16];
        v.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend(data);
        v.resize(39, 0);
        v
    };
    let mut tdb = TestDb::new(3);
    tdb.add_table(
        "Calc",
        0,
        &[ColSpec::new("a", t::LONG), ColSpec::new("b", t::LONG).ext(0xC0).length(39)],
        &[vec![int(2), Cv::Raw(wrapped(&4i32.to_le_bytes()))]],
        &[],
    );
    let p = preview_bytes(&tdb.finish());
    assert_eq!(section_lines(&p, "Calc"), ["a|b", "2|4"]);
    assert!(p.notices.iter().any(|n| n.kind == NoticeKind::FormulasNotEvaluated));
}

#[test]
fn a_query_with_more_rows_than_the_limit_is_shown_as_stored() {
    let rows: Vec<Qr> = (0..super::MAX_QUERY_ROWS_PER_OBJECT + 50)
        .map(|_| (6u8, Some("x"), None, None, None, None))
        .collect();
    let queries: Vec<(&str, i64, Vec<Qr>)> = vec![("qBig", 0, rows)];
    let p = preview_bytes(&query_db(&queries, |_| {}));
    let (text, _) = first_para(&p, "Query: qBig").unwrap();
    assert!(text.contains("not rebuilt as SQL"), "{}", &text[..text.len().min(200)]);
    let section = p.sections.iter().find(|s| s.name == "Query: qBig").unwrap();
    assert!(section.kind == SectionKind::Sheet);
}

#[test]
fn an_oversized_expression_is_never_cut() {
    let big: &'static str =
        Box::leak("x".repeat(super::MAX_QUERY_FIELD_CHARS + 10).into_boxed_str());
    let queries: Vec<(&str, i64, Vec<Qr>)> = vec![(
        "qHuge",
        0,
        vec![
            (1, None, Some(1), None, None, None),
            (6, Some(big), None, None, None, None),
            (5, None, None, None, Some("T"), None),
        ],
    )];
    let p = preview_bytes(&query_db(&queries, |_| {}));
    let (text, _) = first_para(&p, "Query: qHuge").unwrap();
    assert!(text.contains("not rebuilt as SQL"), "{}", &text[..text.len().min(200)]);
    assert!(!format!("{p:?}").contains(&big[..1000]));
}
