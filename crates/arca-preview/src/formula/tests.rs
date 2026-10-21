use super::*;

/// A one-sheet book from `("A1", "text")` pairs: `=...` is a formula, numbers are numbers,
/// `TRUE`/`FALSE` booleans, the rest text.
fn book(cells: &[(&str, &str)]) -> Book {
    let mut b = Book::new(false);
    b.add(sheet("Sheet1", cells));
    b
}

fn sheet(name: &str, cells: &[(&str, &str)]) -> Sheet {
    let mut map = BTreeMap::new();
    for (at, text) in cells {
        let toks = lex(at).expect("a reference");
        let Some(Tok::Cell(c)) = toks.first().map(|t| t.tok.clone()) else {
            panic!("not a cell: {at}");
        };
        let cell = if let Some(f) = text.strip_prefix('=') {
            Cell { value: Val::Empty, formula: Some(f.to_owned()) }
        } else if let Ok(n) = text.parse::<f64>() {
            Cell { value: Val::Num(n), formula: None }
        } else if *text == "TRUE" || *text == "FALSE" {
            Cell { value: Val::Bool(*text == "TRUE"), formula: None }
        } else {
            Cell { value: Val::Str((*text).to_owned()), formula: None }
        };
        map.insert((c.row, c.col), cell);
    }
    Sheet::new(name, map, true)
}

/// Value of `=expr` in an otherwise empty book plus `cells`.
fn eval_with(cells: &[(&str, &str)], expr: &str) -> Option<Val> {
    let mut all: Vec<(&str, String)> = cells.iter().map(|(a, b)| (*a, (*b).to_owned())).collect();
    all.push(("Z99", format!("={expr}")));
    let refs: Vec<(&str, &str)> = all.iter().map(|(a, b)| (*a, b.as_str())).collect();
    let b = book(&refs);
    b.calculate().values.get(&(0, 99, 25)).cloned()
}

fn eval(expr: &str) -> Option<Val> {
    eval_with(&[], expr)
}

fn n(x: f64) -> Option<Val> {
    Some(Val::Num(x))
}

fn s(x: &str) -> Option<Val> {
    Some(Val::Str(x.to_owned()))
}

fn e(c: ErrCode) -> Option<Val> {
    Some(Val::Err(c))
}

#[test]
fn arithmetic_follows_a_spreadsheet() {
    assert_eq!(eval("1+2*3"), n(7.0));
    assert_eq!(eval("(1+2)*3"), n(9.0));
    assert_eq!(eval("2^3^2"), n(64.0), "left to right");
    assert_eq!(eval("-2^2"), n(4.0), "a minus binds tighter than ^");
    assert_eq!(eval("2^-1"), n(0.5));
    assert_eq!(eval("50%"), n(0.5));
    assert_eq!(eval("200*10%"), n(20.0));
    assert_eq!(eval("10/4"), n(2.5));
    assert_eq!(eval("1/0"), e(ErrCode::Div0));
    assert_eq!(eval("0^0"), e(ErrCode::Num));
    assert_eq!(eval("(-8)^0.5"), e(ErrCode::Num));
    assert_eq!(eval("1E3+.5"), n(1000.5));
    assert_eq!(eval("\"3\"+4"), n(7.0), "text that is a number is read as one");
    assert_eq!(eval("\"abc\"+1"), e(ErrCode::Value));
    assert_eq!(eval("TRUE+TRUE"), n(2.0));
    assert_eq!(eval("1e308*10"), e(ErrCode::Num));
}

#[test]
fn comparisons_and_text() {
    assert_eq!(eval("1<2"), Some(Val::Bool(true)));
    assert_eq!(eval("\"a\"=\"A\""), Some(Val::Bool(true)));
    assert_eq!(eval("\"a\"<\"b\""), Some(Val::Bool(true)));
    assert_eq!(eval("1<\"a\""), Some(Val::Bool(true)), "numbers sort before text");
    assert_eq!(eval("\"a\"<TRUE"), Some(Val::Bool(true)), "text sorts before booleans");
    assert_eq!(eval("1<>1"), Some(Val::Bool(false)));
    assert_eq!(eval("\"a\"&\"b\"&1.5"), s("ab1.5"));
    assert_eq!(eval("\"say \"\"hi\"\"\""), s("say \"hi\""));
    assert_eq!(eval("A1=0"), Some(Val::Bool(true)), "an empty cell is 0");
    assert_eq!(eval("A1=\"\""), Some(Val::Bool(true)), "and also the empty text");
}

#[test]
fn references_between_cells_and_sheets() {
    let mut b = Book::new(false);
    b.add(sheet("Data", &[("A1", "10"), ("A2", "20"), ("B1", "=A1*2"), ("B2", "=B1+A2")]));
    b.add(sheet(
        "My Sheet",
        &[("A1", "=Data!B2+1"), ("A2", "='Data'!A1:A2"), ("A3", "=SUM(Data!A1:A2)")],
    ));
    let out = b.calculate();
    assert_eq!(out.values.get(&(0, 1, 1)), Some(&Val::Num(20.0)));
    assert_eq!(out.values.get(&(0, 2, 1)), Some(&Val::Num(40.0)));
    assert_eq!(out.values.get(&(1, 1, 0)), Some(&Val::Num(41.0)));
    assert_eq!(out.values.get(&(1, 3, 0)), Some(&Val::Num(30.0)));
    // A range where a single value is wanted is not guessed at.
    assert!(!out.values.contains_key(&(1, 2, 0)));
    assert_eq!(out.failed, 1);
}

#[test]
fn aggregates_ignore_what_a_spreadsheet_ignores() {
    let cells = [("A1", "1"), ("A2", "2"), ("A3", "text"), ("A4", "TRUE"), ("A6", "4")];
    assert_eq!(eval_with(&cells, "SUM(A1:A6)"), n(7.0));
    assert_eq!(eval_with(&cells, "SUM(A1:A6,10)"), n(17.0));
    assert_eq!(eval_with(&cells, "SUM(TRUE,\"5\")"), n(6.0));
    assert_eq!(eval_with(&cells, "AVERAGE(A1:A6)"), n(7.0 / 3.0));
    assert_eq!(eval_with(&cells, "COUNT(A1:A6)"), n(3.0));
    assert_eq!(eval_with(&cells, "COUNTA(A1:A6)"), n(5.0));
    assert_eq!(eval_with(&cells, "COUNTBLANK(A1:A8)"), n(3.0));
    assert_eq!(eval_with(&cells, "MIN(A1:A6)"), n(1.0));
    assert_eq!(eval_with(&cells, "MAX(A1:A6,9)"), n(9.0));
    assert_eq!(eval_with(&cells, "PRODUCT(A1:A2,A6)"), n(8.0));
    assert_eq!(eval_with(&cells, "MEDIAN(A1:A6)"), n(2.0));
    assert_eq!(eval_with(&cells, "LARGE(A1:A6,2)"), n(2.0));
    assert_eq!(eval_with(&cells, "SMALL(A1:A6,1)"), n(1.0));
    assert_eq!(eval_with(&cells, "SUM(A:A)"), n(7.0), "a whole column stops at the data");
    assert_eq!(eval("AVERAGE(\"a\")"), e(ErrCode::Value));
    // Text in a cell that is pointed at is skipped; typed in, it is refused.
    assert_eq!(eval_with(&cells, "SUM(A3)"), n(0.0));
    assert_eq!(eval_with(&cells, "SUM(A1)"), n(1.0));
    assert_eq!(eval_with(&cells, "COUNT(A1)"), n(1.0));
    assert_eq!(eval_with(&cells, "COUNT(A3)"), n(0.0));
    assert_eq!(eval_with(&cells, "COUNTBLANK(A5)"), n(1.0));
    assert_eq!(eval_with(&cells, "MAX(A3)"), n(0.0));
    assert_eq!(eval_with(&cells, "AND(A4)"), Some(Val::Bool(true)));
    assert_eq!(
        eval("COUNT(1/0,2)"),
        n(1.0),
        "an error argument is not counted, nor does it spread"
    );
    assert_eq!(eval("COUNTA(1/0,2)"), n(2.0));
    assert_eq!(eval_with(&[("A1", "=1/0")], "COUNT(A1:A2)"), n(0.0));
    assert_eq!(eval("NA()"), e(ErrCode::Na));
    assert_eq!(eval("IFNA(NA(),1)"), n(1.0));
    assert_eq!(eval("MAX()"), None, "wrong argument counts are not guessed at");
    assert_eq!(eval_with(&[("A1", "=1/0")], "SUM(A1,1)"), e(ErrCode::Div0));
}

#[test]
fn conditional_aggregates() {
    let cells = [
        ("A1", "apple"),
        ("A2", "Banana"),
        ("A3", "apricot"),
        ("A4", "apple"),
        ("B1", "1"),
        ("B2", "2"),
        ("B3", "3"),
        ("B4", "4"),
    ];
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A4,\"apple\")"), n(2.0));
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A4,\"ap*\")"), n(3.0));
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A4,\"<>apple\")"), n(2.0));
    assert_eq!(eval_with(&cells, "COUNTIF(B1:B4,\">2\")"), n(2.0));
    assert_eq!(eval_with(&cells, "COUNTIF(B1:B4,3)"), n(1.0));
    // Blank cells meet "<>x" and "" — also those past the end of the data — but not 0 or ">1".
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A6,\"<>apple\")"), n(4.0));
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A6,\"\")"), n(2.0));
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A1000,\"\")"), n(996.0));
    assert_eq!(eval_with(&cells, "COUNTIF(A1:A6,0)"), n(0.0));
    let with_gap = [("A1", "x"), ("B1", "1"), ("B2", "10"), ("A3", "y"), ("B3", "100")];
    assert_eq!(
        eval_with(&with_gap, "SUMIF(A1:A3,\"<>x\",B1:B3)"),
        n(110.0),
        "the blank A2 meets it"
    );
    assert_eq!(eval_with(&with_gap, "SUMIF(A1:A3,\"x\",B1:B3)"), n(1.0));
    assert_eq!(eval_with(&cells, "SUMIF(A1:A4,\"apple\",B1:B4)"), n(5.0));
    assert_eq!(eval_with(&cells, "SUMIF(B1:B4,\">=2\")"), n(9.0));
    assert_eq!(eval_with(&cells, "AVERAGEIF(B1:B4,\"<3\")"), n(1.5));
    assert_eq!(eval_with(&cells, "AVERAGEIF(B1:B4,\">9\")"), e(ErrCode::Div0));
    assert_eq!(eval_with(&cells, "SUMPRODUCT(B1:B4,B1:B4)"), n(30.0));
}

#[test]
fn rounding_matches_a_spreadsheet() {
    assert_eq!(eval("ROUND(2.675,2)"), n(2.68), "15-digit normalisation");
    assert_eq!(eval("ROUND(-2.5,0)"), n(-3.0), "half away from zero");
    assert_eq!(eval("ROUND(1234.5678,-2)"), n(1200.0));
    assert_eq!(eval("ROUNDUP(1.1*10,0)"), n(11.0));
    assert_eq!(eval("ROUNDUP(2.01,1)"), n(2.1));
    assert_eq!(eval("ROUNDDOWN(-2.99,1)"), n(-2.9));
    assert_eq!(eval("INT(-1.5)"), n(-2.0));
    assert_eq!(eval("TRUNC(-1.99)"), n(-1.0));
    assert_eq!(eval("TRUNC(5.43219,3)"), n(5.432));
    assert_eq!(eval("MOD(-3,2)"), n(1.0));
    assert_eq!(eval("MOD(3,-2)"), n(-1.0));
    assert_eq!(eval("MOD(1,0)"), e(ErrCode::Div0));
    assert_eq!(eval("ABS(-4)"), n(4.0));
    assert_eq!(eval("SIGN(-4)"), n(-1.0));
    assert_eq!(eval("SQRT(16)"), n(4.0));
    assert_eq!(eval("SQRT(-1)"), e(ErrCode::Num));
    assert_eq!(eval("POWER(2,10)"), n(1024.0));
    assert_eq!(eval("LOG(8,2)"), n(3.0));
    assert_eq!(eval("LOG10(1000)"), n(3.0));
    assert_eq!(eval("LN(0)"), e(ErrCode::Num));
    assert_eq!(eval("ROUND(PI()*3,3)"), n(9.425));
}

#[test]
fn logic_is_lazy() {
    assert_eq!(eval("IF(1>0,\"y\",\"n\")"), s("y"));
    assert_eq!(eval("IF(0,\"y\",\"n\")"), s("n"));
    assert_eq!(eval("IF(0,1)"), Some(Val::Bool(false)));
    assert_eq!(eval("IF(TRUE,1,1/0)"), n(1.0), "the untaken branch is not evaluated");
    assert_eq!(eval("IF(1,,2)"), n(0.0));
    assert_eq!(eval("IFERROR(1/0,\"oops\")"), s("oops"));
    assert_eq!(eval("IFERROR(5,\"oops\")"), n(5.0));
    assert_eq!(eval("IFNA(#N/A,7)"), n(7.0));
    assert_eq!(eval("IFNA(1/0,7)"), e(ErrCode::Div0));
    assert_eq!(eval("AND(1,TRUE,2>1)"), Some(Val::Bool(true)));
    assert_eq!(eval("AND(1,0)"), Some(Val::Bool(false)));
    assert_eq!(eval("OR(0,0,1)"), Some(Val::Bool(true)));
    assert_eq!(eval("XOR(1,1)"), Some(Val::Bool(false)));
    assert_eq!(eval("NOT(0)"), Some(Val::Bool(true)));
    assert_eq!(eval("AND(\"x\")"), e(ErrCode::Value));
    assert_eq!(eval("CHOOSE(2,\"a\",\"b\",\"c\")"), s("b"));
    assert_eq!(eval("CHOOSE(4,\"a\",\"b\",\"c\")"), e(ErrCode::Value));
    assert_eq!(eval("TRUE()"), Some(Val::Bool(true)));
    assert_eq!(eval("ISNUMBER(1)"), Some(Val::Bool(true)));
    assert_eq!(eval("ISTEXT(1)"), Some(Val::Bool(false)));
    assert_eq!(eval("ISERROR(1/0)"), Some(Val::Bool(true)));
    assert_eq!(eval("ISNA(#N/A)"), Some(Val::Bool(true)));
    assert_eq!(eval("ISBLANK(B5)"), Some(Val::Bool(true)));
}

#[test]
fn text_functions() {
    assert_eq!(eval("LEN(\"héllo\")"), n(5.0));
    assert_eq!(eval("UPPER(\"straße\")"), s("STRASSE"));
    assert_eq!(eval("LOWER(\"ПРИВЕТ\")"), s("привет"));
    assert_eq!(eval("TRIM(\"  a   b  \")"), s("a b"));
    assert_eq!(eval("PROPER(\"hELLO wORLD-foo\")"), s("Hello World-Foo"));
    assert_eq!(eval("LEFT(\"abcdef\",2)"), s("ab"));
    assert_eq!(eval("LEFT(\"abc\")"), s("a"));
    assert_eq!(eval("RIGHT(\"abcdef\",3)"), s("def"));
    assert_eq!(eval("MID(\"abcdef\",2,3)"), s("bcd"));
    assert_eq!(eval("MID(\"abc\",9,3)"), s(""));
    assert_eq!(eval("LEFT(\"abc\",-1)"), e(ErrCode::Value));
    assert_eq!(eval("CONCAT(\"a\",1,TRUE)"), s("a1TRUE"));
    assert_eq!(eval("CONCATENATE(\"a\",\"b\")"), s("ab"));
    assert_eq!(eval("_xlfn.CONCAT(\"a\",\"b\")"), s("ab"));
    assert_eq!(eval("TEXTJOIN(\", \",TRUE,\"a\",\"\",\"b\")"), s("a, b"));
    assert_eq!(eval("EXACT(\"a\",\"A\")"), Some(Val::Bool(false)));
    assert_eq!(eval("REPT(\"ab\",3)"), s("ababab"));
    assert_eq!(eval("REPT(\"ab\",100000)"), e(ErrCode::Value));
    assert_eq!(eval("SUBSTITUTE(\"a-b-c\",\"-\",\"+\")"), s("a+b+c"));
    assert_eq!(eval("SUBSTITUTE(\"a-b-c\",\"-\",\"+\",2)"), s("a-b+c"));
    assert_eq!(eval("FIND(\"b\",\"abcb\")"), n(2.0));
    assert_eq!(eval("FIND(\"b\",\"abcb\",3)"), n(4.0));
    assert_eq!(eval("FIND(\"B\",\"abc\")"), e(ErrCode::Value));
    assert_eq!(eval("SEARCH(\"B\",\"abc\")"), n(2.0));
    assert_eq!(eval("SEARCH(\"a*\",\"abc\")"), None, "wildcards in SEARCH are not guessed at");
    assert_eq!(eval("VALUE(\"12.5\")"), n(12.5));
    assert_eq!(eval("VALUE(\"x\")"), e(ErrCode::Value));
    assert_eq!(eval("VALUE(\"2020-01-01\")"), None, "dates written as text are not guessed at");
    assert_eq!(eval("TEXT(0.256,\"0.0%\")"), s("25.6%"));
    assert_eq!(eval("TEXT(1234.5,\"#,##0.00\")"), s("1,234.50"));
    assert_eq!(eval("TEXT(DATE(2024,3,5),\"yyyy-mm-dd\")"), s("2024-03-05"));
}

#[test]
fn dates() {
    assert_eq!(eval("DATE(2024,1,1)"), n(45292.0));
    assert_eq!(eval("DATE(2024,14,1)"), eval("DATE(2025,2,1)"), "months overflow");
    assert_eq!(eval("DATE(2024,3,0)"), eval("DATE(2024,2,29)"), "days underflow");
    assert_eq!(eval("DATE(24,1,1)"), eval("DATE(1924,1,1)"));
    assert_eq!(eval("YEAR(45292)"), n(2024.0));
    assert_eq!(eval("MONTH(45292)"), n(1.0));
    assert_eq!(eval("DAY(45322)"), n(31.0));
    assert_eq!(eval("WEEKDAY(45292)"), n(2.0), "a Monday, Sunday = 1");
    assert_eq!(eval("WEEKDAY(45292,2)"), n(1.0));
    assert_eq!(eval("WEEKDAY(45292,3)"), n(0.0));
    assert_eq!(eval("DATE(2024,2,29)-DATE(2024,1,1)"), n(59.0));
    assert_eq!(eval("DATE(1900,1,1)"), None, "the two odd months of 1900 are not guessed at");
    assert_eq!(eval("YEAR(-1)"), e(ErrCode::Num));
}

#[test]
fn lookups() {
    let cells = [
        ("A1", "10"),
        ("B1", "ten"),
        ("A2", "20"),
        ("B2", "twenty"),
        ("A3", "30"),
        ("B3", "thirty"),
        ("D1", "x"),
        ("E1", "y"),
        ("F1", "z"),
    ];
    assert_eq!(eval_with(&cells, "VLOOKUP(20,A1:B3,2,FALSE)"), s("twenty"));
    assert_eq!(eval_with(&cells, "VLOOKUP(25,A1:B3,2)"), s("twenty"), "approximate");
    assert_eq!(eval_with(&cells, "VLOOKUP(5,A1:B3,2)"), e(ErrCode::Na));
    assert_eq!(eval_with(&cells, "VLOOKUP(25,A1:B3,2,FALSE)"), e(ErrCode::Na));
    assert_eq!(eval_with(&cells, "VLOOKUP(20,A1:B3,3,FALSE)"), e(ErrCode::Ref));
    assert_eq!(eval_with(&cells, "VLOOKUP(30,A:B,2,0)"), s("thirty"));
    assert_eq!(eval_with(&cells, "HLOOKUP(\"y\",D1:F1,1,FALSE)"), s("y"));
    assert_eq!(eval_with(&cells, "MATCH(30,A1:A3,0)"), n(3.0));
    assert_eq!(eval_with(&cells, "MATCH(25,A1:A3,1)"), n(2.0));
    assert_eq!(eval_with(&cells, "MATCH(\"Z\",D1:F1,0)"), n(3.0));
    assert_eq!(eval_with(&cells, "MATCH(99,A1:A3,0)"), e(ErrCode::Na));
    assert_eq!(eval_with(&cells, "INDEX(B1:B3,2)"), s("twenty"));
    assert_eq!(eval_with(&cells, "INDEX(A1:B3,3,2)"), s("thirty"));
    assert_eq!(eval_with(&cells, "INDEX(D1:F1,2)"), s("y"));
    assert_eq!(eval_with(&cells, "INDEX(A1:B3,4,1)"), e(ErrCode::Ref));
    assert_eq!(eval_with(&cells, "INDEX(B1:B3,MATCH(30,A1:A3,0))"), s("thirty"));
    assert_eq!(eval_with(&cells, "ROWS(A1:B3)"), n(3.0));
    assert_eq!(eval_with(&cells, "COLUMNS(A1:B3)"), n(2.0));
}

#[test]
fn chains_cycles_and_unknowns_are_left_alone() {
    // A long running total, written bottom-up so that the natural order is the worst one.
    let mut cells: Vec<(String, String)> = vec![("A1".into(), "1".into())];
    for i in 2..=400 {
        cells.push((format!("A{i}"), format!("=A{}+1", i - 1)));
    }
    let refs: Vec<(&str, &str)> = cells.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let out = book(&refs).calculate();
    assert_eq!(out.values.get(&(0, 400, 0)), Some(&Val::Num(400.0)), "{} failed", out.failed);

    // The same chain in reverse order of rows: formulas that refer downwards.
    let mut cells: Vec<(String, String)> = vec![("A300".into(), "1".into())];
    for i in 1..300 {
        cells.push((format!("A{i}"), format!("=A{}+1", i + 1)));
    }
    let refs: Vec<(&str, &str)> = cells.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let out = book(&refs).calculate();
    assert_eq!(out.values.get(&(0, 1, 0)), Some(&Val::Num(300.0)), "{} failed", out.failed);

    // A circle, and everything that depends on it.
    let out = book(&[("A1", "=B1+1"), ("B1", "=A1+1"), ("C1", "=A1*2"), ("D1", "=5")]).calculate();
    assert_eq!(out.values.len(), 1);
    assert_eq!(out.values.get(&(0, 1, 3)), Some(&Val::Num(5.0)));
    assert_eq!(out.failed, 3);

    // Things a spreadsheet would do differently or that need the outside world.
    for f in [
        "TODAY()",
        "NOW()",
        "RAND()",
        "INDIRECT(\"A1\")",
        "WEBSERVICE(\"http://example.com\")",
        "HYPERLINK(\"http://example.com\",\"x\")",
        "CALL(\"a\",\"b\")",
        "NoSuchName",
        "[1]Sheet1!A1",
        "Table1[Col]",
        "{1,2,3}",
        "A1:A3 B1:B3",
        "'Missing'!A1",
        "A1:A3",
        "SUM(",
        "1+",
        "\"unterminated",
    ] {
        assert_eq!(eval(f), None, "{f} must not be calculated");
    }
}

#[test]
fn budgets_hold() {
    // A formula of every kind of nesting the parser limits.
    let deep = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    assert_eq!(eval(&deep), None);
    let deep = format!("{}1", "-".repeat(200));
    assert_eq!(eval(&deep), None);
    let deep = format!("{}1{}", "SUM(".repeat(100), ")".repeat(100));
    assert_eq!(eval(&deep), None);
    assert_eq!(eval(&"1+".repeat(3000)), None, "too long");
    // Many cells over many ranges cost steps; the budget ends it.
    let mut cells: Vec<(String, String)> = Vec::new();
    for i in 1..=2000 {
        cells.push((format!("A{i}"), i.to_string()));
    }
    for i in 1..=200 {
        cells.push((format!("B{i}"), "=SUM(A1:A2000)+SUM(A1:A2000)".into()));
    }
    let refs: Vec<(&str, &str)> = cells.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let out = book(&refs).calculate();
    assert!(out.values.len() < 200, "{} calculated", out.values.len());
    assert!(out.failed > 0);
}

#[test]
fn shared_formulas_move_their_references() {
    assert_eq!(shifted("A1*2", 2, 0).as_deref(), Some("A3*2"));
    assert_eq!(shifted("$A$1+B1", 1, 1).as_deref(), Some("$A$1+C2"));
    assert_eq!(shifted("SUM($A1:B$2)", 3, 2).as_deref(), Some("SUM($A4:D$2)"));
    assert_eq!(shifted("Sheet2!C3", 1, 0).as_deref(), Some("Sheet2!C4"));
    assert_eq!(shifted("'My Sheet'!C3+\"A1\"", 0, 1).as_deref(), Some("'My Sheet'!D3+\"A1\""));
    assert_eq!(shifted("A1", -1, 0), None, "off the sheet");
    assert_eq!(shifted("A1", 0, -1), None);
    assert_eq!(shifted("SUM(A:A)", 1, 0), None, "whole columns are not guessed at");
    assert_eq!(shifted("ROUND(1.5,0)", 5, 5).as_deref(), Some("ROUND(1.5,0)"));
    assert_eq!(column_name(0), "A");
    assert_eq!(column_name(25), "Z");
    assert_eq!(column_name(26), "AA");
    assert_eq!(column_name(16383), "XFD");
}

#[test]
fn nothing_panics_on_junk() {
    let junk = [
        "",
        " ",
        "((",
        "))",
        ",,",
        "#",
        "#DIV/0",
        "1..2",
        "..",
        "$",
        "$$A$1",
        "A1:",
        ":A1",
        "A1:B",
        "1:",
        "1:a",
        "IF(,,)",
        "IF(",
        "SUM(,)",
        "@A1",
        "A1!",
        "'a",
        "'a'",
        "'a'!",
        "!!",
        "1e",
        "1e+",
        "Лист1!A1",
        "\u{0}",
        "A1+++++++++1",
        "A1&&B1",
        "\"\"\"",
        "1%%%%",
        "ROW()",
        "AA1:ZZ9",
        "XFE1",
        "A1048577",
        "A0",
        "A00001",
        "LOG(0)",
        "REPT(\"x\",1E308)",
        "MID(\"a\",1E308,1E308)",
        "ROUND(1E308,15)",
        "ROUND(1,99)",
        "DATE(1E308,1,1)",
        "INDEX(A1:A2,1E308)",
        "CHOOSE(1E308,1)",
        "LEFT(\"a\",1E308)",
        "VLOOKUP(1,A1:B2,1E308)",
        "SUM(A1:XFD1048576)",
        "2^1E308",
        "-(-(-1))",
    ];
    for f in junk {
        let _ = eval(f);
        let _ = shifted(f, 1, 1);
    }
}

#[test]
fn error_names_round_trip() {
    for c in ErrCode::ALL {
        assert_eq!(ErrCode::from_text(c.text()), Some(c));
        assert_eq!(eval(c.text()), Some(Val::Err(c)));
    }
}

/// Formulas stitched together from the pieces a lexer has to tell apart, with the cells they can
/// refer to also being formulas: nothing may panic, loop or overflow the stack.
#[test]
fn random_formulas_never_panic() {
    use crate::testkit::{Lcg, fuzz_iterations};
    let pieces = [
        "A1",
        "B2",
        "$C$3",
        "D4:E5",
        "A:A",
        "1:2",
        "Sheet1!A1",
        "'Sheet1'!B2",
        "'x''y'!C3",
        "SUM(",
        "IF(",
        "VLOOKUP(",
        "INDEX(",
        "MATCH(",
        "COUNTIF(",
        "TEXT(",
        "REPT(",
        "(",
        ")",
        ",",
        ":",
        "+",
        "-",
        "*",
        "/",
        "^",
        "&",
        "=",
        "<>",
        "<=",
        ">=",
        "<",
        ">",
        "%",
        "1",
        "0",
        "-1",
        "1E308",
        "1E-308",
        ".5",
        "1.",
        "\"a\"",
        "\"\"",
        "\"x\"\"y\"",
        "TRUE",
        "FALSE",
        "#N/A",
        "#REF!",
        "NA()",
        " ",
        "Лист1!A1",
        "é",
        "{",
        "}",
        ";",
        "@",
        "[",
        "]",
        "#",
        "$",
        "!",
        "'",
    ];
    let mut rng = Lcg::new(7);
    for _ in 0..fuzz_iterations(1500) {
        let n = 1 + rng.below(12);
        let f: String = (0..n).map(|_| pieces[rng.below(pieces.len())]).collect();
        // Both alone and as the formula of cells that refer to each other.
        let mut b = Book::new(false);
        let mut cells = BTreeMap::new();
        for (i, at) in [(1, 0), (2, 1), (3, 2), (4, 3), (5, 4)].into_iter().enumerate() {
            let g: String = if i == 0 {
                f.clone()
            } else {
                (0..1 + rng.below(6)).map(|_| pieces[rng.below(pieces.len())]).collect()
            };
            cells.insert(at, Cell { value: Val::Empty, formula: Some(g) });
        }
        cells.insert((1, 5), Cell { value: Val::Num(3.0), formula: None });
        b.add(Sheet::new("Sheet1", cells, true));
        let _ = b.calculate();
        let _ = shifted(&f, 1, 1);
    }
}
