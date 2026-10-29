//! Tests of the sheet pages: sizes, tiling, merged cells, hidden rows and columns, pagination,
//! agreement with the text preview, and hostile documents.

use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};

use super::*;
use crate::model::{Block, NoticeKind, Preview, SectionKind};
use crate::page::{MAX_PRIMS, Prim, op, to_svg};
use crate::testkit::{ok, opc, rels, zip};

const SS_NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const XLSX_CT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";

// Default A4 page: 595.28 x 841.89 points, margins 50.4 / 54 / 50.4 / 54.
const ML: f32 = 50.4;
const MT: f32 = 54.0;
/// How far a fill reaches over the gridline at its edge.
const BLEED: f32 = 0.3;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn plain() -> PreviewPolicy {
    PreviewPolicy::default()
}

fn omit_hidden() -> PreviewPolicy {
    PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() }
}

/// `<styleSheet>` with the given fills and cell formats (font 0 and border 0 are plain).
fn styles_xml(fills: &str, borders: &str, xfs: &str) -> String {
    format!(
        r#"<styleSheet {SS_NS}><fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><i/><sz val="14"/><color rgb="FF0000FF"/><name val="Courier New"/></font></fonts><fills>{fills}</fills><borders><border><left/><right/><top/><bottom/><diagonal/></border>{borders}</borders><cellXfs>{xfs}</cellXfs></styleSheet>"#
    )
}

fn solid(rgb: &str) -> String {
    format!(
        r#"<fill><patternFill patternType="solid"><fgColor rgb="FF{rgb}"/><bgColor indexed="64"/></patternFill></fill>"#
    )
}

/// Fills 0 (none), 1 (gray125), 2 red, 3 green, 4 blue; formats 0 plain, 1..3 filled.
fn colour_styles() -> String {
    let fills = format!(
        r#"<fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill>{}{}{}"#,
        solid("FF0000"),
        solid("00FF00"),
        solid("0000FF")
    );
    styles_xml(
        &fills,
        "",
        r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="0" fillId="2" borderId="0"/><xf numFmtId="0" fontId="0" fillId="3" borderId="0"/><xf numFmtId="0" fontId="0" fillId="4" borderId="0"/>"#,
    )
}

/// A workbook: `(name, worksheet inner xml, hidden)` per sheet.
fn xlsx_book(sheets: &[(&str, &str, bool)], styles: &str, theme: Option<&str>) -> Vec<u8> {
    let list: String = sheets
        .iter()
        .enumerate()
        .map(|(i, (name, _, hidden))| {
            format!(
                r#"<sheet name="{name}" sheetId="{}" r:id="rId{}"{}/>"#,
                i + 1,
                i + 10,
                if *hidden { r#" state="hidden""# } else { "" }
            )
        })
        .collect();
    let wb = format!(r#"<workbook {SS_NS}><sheets>{list}</sheets></workbook>"#);
    let mut rel_list: Vec<(String, &str, String)> = (0..sheets.len())
        .map(|i| (format!("rId{}", i + 10), "worksheet", format!("worksheets/sheet{}.xml", i + 1)))
        .collect();
    rel_list.push(("rId2".into(), "styles", "styles.xml".into()));
    if theme.is_some() {
        rel_list.push(("rId3".into(), "theme", "theme/theme1.xml".into()));
    }
    let rl: Vec<(&str, &str, &str)> =
        rel_list.iter().map(|(a, b, c)| (a.as_str(), *b, c.as_str())).collect();
    let wb_rels = rels(&rl);
    let bodies: Vec<String> = sheets
        .iter()
        .map(|(_, inner, _)| format!(r#"<worksheet {SS_NS}>{inner}</worksheet>"#))
        .collect();
    let names: Vec<String> =
        (0..sheets.len()).map(|i| format!("xl/worksheets/sheet{}.xml", i + 1)).collect();
    let mut parts: Vec<(&str, &str)> = vec![
        ("xl/workbook.xml", wb.as_str()),
        ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
        ("xl/styles.xml", styles),
    ];
    if let Some(t) = theme {
        parts.push(("xl/theme/theme1.xml", t));
    }
    for (n, b) in names.iter().zip(&bodies) {
        parts.push((n.as_str(), b.as_str()));
    }
    opc(("xl/workbook.xml", XLSX_CT), &parts)
}

fn xlsx1(inner: &str, styles: &str) -> Vec<u8> {
    xlsx_book(&[("S", inner, false)], styles, None)
}

fn text_cell(r: &str, s: u32, text: &str) -> String {
    format!(r#"<c r="{r}" s="{s}" t="inlineStr"><is><t>{text}</t></is></c>"#)
}

fn col_name(mut i: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + u8::try_from(i % 26).unwrap_or(0));
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn render(data: &[u8], sheet: usize, page: usize) -> SheetPage {
    render_with(data, sheet, page, &plain())
}

fn render_with(data: &[u8], sheet: usize, page: usize, policy: &PreviewPolicy) -> SheetPage {
    match render_sheet_page(data, sheet, page, policy) {
        Ok(p) => {
            check_page(&p.page);
            p
        }
        Err(e) => panic!("render failed: {e}"),
    }
}

fn texts(p: &PageModel) -> Vec<String> {
    p.prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Bounding boxes `(x0, y0, x1, y1, fill)` of the filled paths.
fn fills(p: &PageModel) -> Vec<(f32, f32, f32, f32, u32)> {
    let mut out = Vec::new();
    for prim in &p.prims {
        if let Prim::Path { ops, fill, .. } = prim
            && fill >> 24 != 0
        {
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            let mut i = 0;
            while i < ops.len() {
                let code = ops.get(i).copied().unwrap_or(-1.0);
                if code == op::MOVE || code == op::LINE {
                    let x = ops.get(i + 1).copied().unwrap_or(0.0);
                    let y = ops.get(i + 2).copied().unwrap_or(0.0);
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                    i += 3;
                } else {
                    i += 1;
                }
            }
            out.push((x0, y0, x1, y1, *fill));
        }
    }
    out
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.02
}

/// Every number is finite, clips are balanced, the page is bounded.
fn check_page(p: &PageModel) {
    assert!(p.width.is_finite() && p.height.is_finite() && p.width > 0.0 && p.height > 0.0);
    assert!(p.prims.len() <= MAX_PRIMS);
    let mut depth = 0i32;
    for prim in &p.prims {
        match prim {
            Prim::Path { ops, width, .. } => {
                assert!(ops.iter().all(|v| v.is_finite()) && width.is_finite());
            }
            Prim::Text { x, y, size, width, .. } => {
                assert!([x, y, size, width].iter().all(|v| v.is_finite()));
            }
            Prim::Clip { x, y, w, h } => assert!([x, y, w, h].iter().all(|v| v.is_finite())),
            Prim::Save => depth += 1,
            Prim::Restore => depth -= 1,
            Prim::Image { .. } => {}
        }
        assert!(depth >= 0);
    }
    assert_eq!(depth, 0, "saves and restores balance");
    assert!(!to_svg(p).is_empty());
    let bytes = to_bytes(p).unwrap_or_default();
    let back: Result<PageModel, _> = from_bytes(&bytes, DecodeLimits::default());
    assert_eq!(back.as_ref().ok(), Some(p), "the page survives the wire");
}

fn has_notice(n: &[Notice], kind: NoticeKind, detail: &str) -> bool {
    n.iter().any(|x| x.kind == kind && x.detail.contains(detail))
}

// ---------------------------------------------------------------------------
// Sizes and tiling
// ---------------------------------------------------------------------------

#[test]
fn widths_and_heights_become_points_and_cells_tile() {
    let sheet = r#"<sheetFormatPr defaultRowHeight="15"/>
        <cols><col min="1" max="1" width="10" customWidth="1"/><col min="3" max="3" width="20" customWidth="1"/></cols>
        <sheetData>
        <row r="1" ht="30" customHeight="1"><c r="A1" s="1"/><c r="B1" s="2"/><c r="C1" s="3"/></row>
        <row r="2"><c r="A2" s="2"/><c r="B2" s="3"/><c r="C2" s="1"/></row>
        </sheetData>"#;
    let data = xlsx1(sheet, &colour_styles());
    let page = render(&data, 0, 0);
    assert_eq!(page.page.pages, 1);
    let r = fills(&page.page);
    assert_eq!(r.len(), 6, "{r:?}");
    // Width 10 is 70 pixels, that is 52.5 points; the default column is 64 pixels, 48 points;
    // width 20 is 140 pixels, 105 points. Row 1 is 30 points, row 2 the default 15.
    let widths = [52.5, 48.0, 105.0];
    let mut x = ML;
    for (k, w) in widths.iter().enumerate() {
        let (x0, y0, x1, y1, _) = r[k];
        assert!(near(x0, x - BLEED) && near(x1, x + w + BLEED), "column {k}: {x0} {x1}");
        assert!(near(y0, MT - BLEED) && near(y1, MT + 30.0 + BLEED), "row 1: {y0} {y1}");
        x += w;
    }
    let mut x = ML;
    for (k, w) in widths.iter().enumerate() {
        let (x0, y0, x1, y1, _) = r[3 + k];
        assert!(near(x0, x - BLEED) && near(x1, x + w + BLEED));
        assert!(near(y0, MT + 30.0 - BLEED) && near(y1, MT + 45.0 + BLEED), "row 2: {y0} {y1}");
        x += w;
    }
    // No gaps and no overlaps beyond the bleed.
    for k in 0..2 {
        assert!(near(r[k].2 - r[k + 1].0, 2.0 * BLEED));
        assert!(near(r[3 + k].2 - r[4 + k].0, 2.0 * BLEED));
    }
    for k in 0..3 {
        assert!(near(r[k].3 - r[3 + k].1, 2.0 * BLEED));
    }
    // Colours.
    assert_eq!(r[0].4, 0xFFFF_0000);
    assert_eq!(r[1].4, 0xFF00_FF00);
    assert_eq!(r[2].4, 0xFF00_00FF);
}

#[test]
fn neighbouring_cells_of_one_colour_become_one_rectangle() {
    let sheet = r#"<sheetData><row r="1"><c r="A1" s="1"/><c r="B1" s="1"/><c r="C1" s="1"/></row></sheetData>"#;
    let page = render(&xlsx1(sheet, &colour_styles()), 0, 0);
    let r = fills(&page.page);
    assert_eq!(r.len(), 1);
    assert!(near(r[0].2 - r[0].0, 3.0 * 48.0 + 2.0 * BLEED));
}

#[test]
fn a_merged_range_spans_its_cells_and_hides_the_text_under_it() {
    let sheet = format!(
        r#"<cols><col min="1" max="1" width="10" customWidth="1"/><col min="3" max="3" width="20" customWidth="1"/></cols>
        <sheetData>
        <row r="1" ht="30" customHeight="1">{}{}</row>
        <row r="2">{}{}</row>
        </sheetData>
        <mergeCells count="2"><mergeCell ref="A1:C2"/><mergeCell ref="E1:F1"/></mergeCells>"#,
        text_cell("A1", 1, "anchor"),
        text_cell("B1", 0, "covered1"),
        text_cell("C2", 0, "covered2"),
        text_cell("E1", 0, "plain merge")
    );
    let page = render(&xlsx1(&sheet, &colour_styles()), 0, 0);
    let r = fills(&page.page);
    assert_eq!(r.len(), 2, "{r:?}");
    let red = r.iter().find(|f| f.4 == 0xFFFF_0000).expect("the merged fill");
    assert!(near(red.2 - red.0, 52.5 + 48.0 + 105.0 + 2.0 * BLEED), "{red:?}");
    assert!(near(red.3 - red.1, 45.0 + 2.0 * BLEED), "{red:?}");
    // A merged range without a fill is white, so that no gridline shows inside it.
    assert!(r.iter().any(|f| f.4 == 0xFFFF_FFFF));
    let t = texts(&page.page);
    assert_eq!(t.iter().filter(|s| s.as_str() == "anchor").count(), 1);
    assert!(t.iter().any(|s| s == "plain merge"));
    assert!(!t.iter().any(|s| s.starts_with("covered")), "{t:?}");
}

#[test]
fn hidden_rows_and_columns_are_skipped_and_counted_unless_revealed() {
    let sheet = format!(
        r#"<cols><col min="2" max="2" width="10" hidden="1" customWidth="1"/></cols>
        <sheetData>
        <row r="1">{}{}{}</row>
        <row r="2" hidden="1">{}</row>
        <row r="3">{}</row>
        </sheetData>"#,
        text_cell("A1", 0, "a1"),
        text_cell("B1", 0, "hidden_col"),
        text_cell("C1", 0, "c1"),
        text_cell("A2", 0, "hidden_row"),
        text_cell("C3", 0, "c3")
    );
    let data = xlsx1(&sheet, &colour_styles());
    let hid = render_with(&data, 0, 0, &omit_hidden());
    let t = texts(&hid.page);
    assert!(
        t.contains(&"a1".to_owned())
            && t.contains(&"c1".to_owned())
            && t.contains(&"c3".to_owned())
    );
    assert!(!t.iter().any(|s| s.starts_with("hidden")), "{t:?}");
    assert!(
        hid.notices
            .iter()
            .any(|n| n.kind == NoticeKind::HiddenOmitted && n.detail == "rows" && n.count == 1),
        "{:?}",
        hid.notices
    );
    assert!(
        hid.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted && n.detail == "columns")
    );
    // Row 3 follows row 1 directly: the hidden row 2 takes no room.
    let y_of = |page: &PageModel, what: &str| {
        page.prims.iter().find_map(|p| match p {
            Prim::Text { text, y, .. } if text == what => Some(*y),
            _ => None,
        })
    };
    let (y1, y3) = (y_of(&hid.page, "c1").expect("c1"), y_of(&hid.page, "c3").expect("c3"));
    assert!(near(y3 - y1, 15.0), "{y1} {y3}");
    // Asked for, they are shown and said to be hidden.
    let shown = render(&data, 0, 0);
    let t = texts(&shown.page);
    assert!(t.iter().any(|s| s == "hidden_col") && t.iter().any(|s| s == "hidden_row"));
    assert!(shown.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "rows"));
    let (y1, y3) = (y_of(&shown.page, "c1").expect("c1"), y_of(&shown.page, "c3").expect("c3"));
    assert!(near(y3 - y1, 30.0));
}

#[test]
fn hidden_sheets_are_listed_only_when_revealed() {
    let data = xlsx_book(
        &[
            ("One", r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#, false),
            ("Two", r#"<sheetData><row r="1"><c r="A1"><v>2</v></c></row></sheetData>"#, true),
            ("Three", r#"<sheetData><row r="1"><c r="A1"><v>3</v></c></row></sheetData>"#, false),
        ],
        &colour_styles(),
        None,
    );
    let all = sheet_list(&data, &plain()).expect("list");
    assert_eq!(all.sheets.len(), 3);
    assert!(all.sheets.get(1).is_some_and(|s| s.hidden && s.name == "Two"));
    let some = sheet_list(&data, &omit_hidden()).expect("list");
    assert_eq!(some.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["One", "Three"]);
    assert_eq!(some.hidden_omitted, 1);
    // Sheets are addressed by their position in the list the policy gives.
    let third = render_with(&data, 1, 0, &omit_hidden());
    assert_eq!(texts(&third.page), ["3"]);
    let third = render(&data, 2, 0);
    assert_eq!(texts(&third.page), ["3"]);
    assert!(render_sheet_page(&data, 2, 0, &omit_hidden()).is_err());
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn tall_sheet(rows: u32, cols: u32, setup: &str) -> Vec<u8> {
    let mut s = String::from("<sheetData>");
    for r in 1..=rows {
        s.push_str(&format!(r#"<row r="{r}">"#));
        for c in 0..cols {
            let name = format!("{}{r}", col_name(c));
            s.push_str(&format!(
                r#"<c r="{name}" t="inlineStr"><is><t>R{r}C{}</t></is></c>"#,
                c + 1
            ));
        }
        s.push_str("</row>");
    }
    s.push_str("</sheetData>");
    s.push_str(setup);
    xlsx1(&s, &colour_styles())
}

#[test]
fn the_used_range_is_split_into_pages_by_the_paper() {
    // 48 rows of 15 points fit the 733.89 points between the margins of an A4 page.
    let data = tall_sheet(100, 1, "");
    assert_eq!(page_count(&data, 0, &plain()).expect("count"), 3);
    let second = render(&data, 0, 1);
    assert_eq!((second.page.index, second.page.pages), (1, 3));
    let t = texts(&second.page);
    assert_eq!(t.first().map(String::as_str), Some("R49C1"));
    assert_eq!(t.len(), 48);
    assert_eq!(texts(&render(&data, 0, 2).page).len(), 4);
    assert!(render_sheet_page(&data, 0, 3, &plain()).is_err());

    // Landscape: 487 points of height, 32 rows.
    let land = tall_sheet(100, 1, r#"<pageSetup orientation="landscape"/>"#);
    assert_eq!(page_count(&land, 0, &plain()).expect("count"), 4);
    let p = render(&land, 0, 0);
    assert!(p.page.width > p.page.height);
    // Scale 50 %: twice the rows per page.
    let half = tall_sheet(100, 1, r#"<pageSetup scale="50"/>"#);
    assert_eq!(page_count(&half, 0, &plain()).expect("count"), 2);
    // Fit to one page.
    let fit = xlsx1(
        &format!(
            r#"<sheetPr><pageSetUpPr fitToPage="1"/></sheetPr>{}"#,
            r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row><row r="200"><c r="A200"><v>1</v></c></row></sheetData><pageSetup fitToWidth="1" fitToHeight="1"/>"#
        ),
        &colour_styles(),
    );
    assert_eq!(page_count(&fit, 0, &plain()).expect("count"), 1);
    // Letter paper (code 1) is 792 points tall: 49 rows fit.
    let letter = tall_sheet(100, 1, r#"<pageSetup paperSize="1"/>"#);
    assert_eq!(page_count(&letter, 0, &plain()).expect("count"), 3);
    let p = render(&letter, 0, 0);
    assert!(near(p.page.width, 612.0) && near(p.page.height, 792.0));
}

#[test]
fn pages_go_down_and_then_over_unless_the_sheet_says_otherwise() {
    // 10 columns of 48 points fit the 494 points between the margins: 25 columns are 3 bands.
    let data = tall_sheet(60, 25, "");
    assert_eq!(page_count(&data, 0, &plain()).expect("count"), 6);
    let has =
        |page: usize, what: &str| texts(&render(&data, 0, page).page).iter().any(|t| t == what);
    assert!(has(0, "R1C1") && has(0, "R48C10"));
    assert!(has(1, "R49C1") && !has(1, "R1C1"));
    assert!(has(2, "R1C11"));
    assert!(has(5, "R60C25"));
    let over = tall_sheet(60, 25, r#"<pageSetup pageOrder="overThenDown"/>"#);
    let t = texts(&render(&over, 0, 1).page);
    assert!(t.iter().any(|t| t == "R1C11"), "{t:?}");
}

#[test]
fn manual_page_breaks_start_a_new_page() {
    let data = tall_sheet(
        30,
        1,
        r#"<rowBreaks count="1" manualBreakCount="1"><brk id="10" max="16383" man="1"/></rowBreaks>"#,
    );
    assert_eq!(page_count(&data, 0, &plain()).expect("count"), 2);
    let t = texts(&render(&data, 0, 1).page);
    assert_eq!(t.first().map(String::as_str), Some("R11C1"));
}

// ---------------------------------------------------------------------------
// Looks
// ---------------------------------------------------------------------------

#[test]
fn fonts_alignment_and_wrapping() {
    let fills = r#"<fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill>"#;
    let xfs = r#"<xf fontId="0" fillId="0" borderId="0"/><xf fontId="1" fillId="0" borderId="0"/><xf fontId="0" fillId="0" borderId="0"><alignment horizontal="center"/></xf><xf fontId="0" fillId="0" borderId="0"><alignment wrapText="1" vertical="top"/></xf>"#;
    let sheet = format!(
        r#"<cols><col min="1" max="1" width="10" customWidth="1"/><col min="2" max="2" width="8" customWidth="1"/></cols>
        <sheetData>
        <row r="1">{}<c r="C1"><v>1234</v></c></row>
        <row r="2">{}</row>
        <row r="3">{}</row>
        </sheetData>"#,
        text_cell("A1", 1, "bold"),
        text_cell("A2", 2, "mid"),
        text_cell("B3", 3, "alpha beta gamma delta")
    );
    let page = render(&xlsx1(&sheet, &styles_xml(fills, "", xfs)), 0, 0);
    let by = |what: &str| {
        page.page
            .prims
            .iter()
            .find_map(|p| match p {
                Prim::Text { x, size, width, style, color, text, .. } if text == what => {
                    Some((*x, *size, *width, *style, *color))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {what}"))
    };
    // Font: bold, italic, monospaced ("Courier New"), 14 points, blue.
    let (x, size, _, style, color) = by("bold");
    assert!(near(x, ML + 2.0), "left aligned with padding: {x}");
    assert!(near(size, 14.0) && color == 0xFF00_00FF);
    assert_eq!(
        style,
        crate::page::style::BOLD | crate::page::style::ITALIC | crate::page::style::MONO
    );
    // Centred text.
    let (x, _, w, _, _) = by("mid");
    assert!(near(x + w / 2.0, ML + 52.5 / 2.0), "{x} {w}");
    // A number is right aligned.
    let (x, _, w, _, _) = by("1234");
    assert!(near(x + w, ML + 52.5 + 42.0 + 48.0 - 2.0), "{x} {w}");
    // Wrapped text: several lines whose words are the cell's words, and a taller row.
    let lines: Vec<String> = texts(&page.page)
        .into_iter()
        .filter(|t| ["alpha", "beta", "gamma", "delta"].iter().any(|w| t.contains(w)))
        .collect();
    assert!(lines.len() >= 3, "{lines:?}");
    assert_eq!(lines.join(" "), "alpha beta gamma delta");
}

#[test]
fn a_wrapped_cell_makes_its_row_as_tall_as_its_text() {
    let xfs = r#"<xf fontId="0" fillId="0" borderId="0"/><xf fontId="0" fillId="2" borderId="0"><alignment wrapText="1"/></xf>"#;
    let fills = format!(
        r#"<fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill>{}"#,
        solid("FFFF00")
    );
    let sheet = format!(
        r#"<cols><col min="1" max="1" width="5" customWidth="1"/></cols><sheetData><row r="1">{}</row></sheetData>"#,
        text_cell("A1", 1, "alpha beta gamma delta epsilon")
    );
    let page = render(&xlsx1(&sheet, &styles_xml(&fills, "", xfs)), 0, 0);
    let r = fills_of(&page.page);
    assert_eq!(r.len(), 1);
    assert!(r[0].3 - r[0].1 > 15.0 * 2.0, "{:?}", r[0]);
}

fn fills_of(p: &PageModel) -> Vec<(f32, f32, f32, f32, u32)> {
    fills(p)
}

#[test]
fn borders_are_lines_with_a_width_and_unsupported_styles_are_said() {
    let borders = r#"<border><left style="thin"><color rgb="FF000000"/></left><right style="thin"><color rgb="FF000000"/></right><top style="medium"><color rgb="FFFF0000"/></top><bottom style="dashed"><color rgb="FF000000"/></bottom><diagonal/></border>"#;
    let fills = r#"<fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill>"#;
    let xfs = r#"<xf fontId="0" fillId="0" borderId="0"/><xf fontId="0" fillId="0" borderId="1"/>"#;
    let sheet = format!(r#"<sheetData><row r="1">{}</row></sheetData>"#, text_cell("A1", 1, "x"));
    let page = render(&xlsx1(&sheet, &styles_xml(fills, borders, xfs)), 0, 0);
    let strokes: Vec<(f32, u32)> = page
        .page
        .prims
        .iter()
        .filter_map(|p| match p {
            Prim::Path { stroke, width, fill, .. } if fill >> 24 == 0 && *stroke != 0xFFD0_D0D0 => {
                Some((*width, *stroke))
            }
            _ => None,
        })
        .collect();
    assert!(strokes.contains(&(1.5, 0xFFFF_0000)), "{strokes:?}");
    assert!(strokes.contains(&(0.75, 0xFF00_0000)), "{strokes:?}");
    assert!(has_notice(
        &page.notices,
        NoticeKind::Unsupported,
        "dashed, dotted and double borders"
    ));
}

#[test]
fn what_is_not_drawn_is_named() {
    let sheet = r#"<sheetViews><sheetView showGridLines="0"><pane xSplit="1" ySplit="1" state="frozen"/></sheetView></sheetViews>
        <sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>
        <conditionalFormatting sqref="A1"><cfRule type="dataBar" priority="1"><dataBar><cfvo type="min"/><cfvo type="max"/></dataBar></cfRule></conditionalFormatting>
        <drawing r:id="rId9"/>"#;
    let page = render(&xlsx1(sheet, &colour_styles()), 0, 0);
    for what in ["frozen panes", "conditional formatting", "data bars", "drawings, charts"] {
        assert!(
            has_notice(&page.notices, NoticeKind::Unsupported, what),
            "{what}: {:?}",
            page.notices
        );
    }
    // The author turned the gridlines off: none are drawn.
    assert!(!page.page.prims.iter().any(|p| matches!(p, Prim::Path { stroke: 0xFFD0_D0D0, .. })));
    let with = render(
        &xlsx1(
            r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#,
            &colour_styles(),
        ),
        0,
        0,
    );
    assert!(with.page.prims.iter().any(|p| matches!(p, Prim::Path { stroke: 0xFFD0_D0D0, .. })));
}

// ---------------------------------------------------------------------------
// Text agrees with the text preview
// ---------------------------------------------------------------------------

fn letters(strings: impl IntoIterator<Item = String>) -> Vec<char> {
    let mut v: Vec<char> = strings
        .into_iter()
        .flat_map(|s| s.chars().collect::<Vec<_>>())
        .filter(|c| !c.is_whitespace())
        .collect();
    v.sort_unstable();
    v
}

fn preview_sheets(p: &Preview) -> Vec<Vec<String>> {
    p.sections
        .iter()
        .filter(|s| s.kind == SectionKind::Sheet)
        .map(|s| {
            s.blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Grid(g) => Some(g),
                    _ => None,
                })
                .flat_map(|g| g.rows.iter().flat_map(|r| r.cells.iter().map(|c| c.text.clone())))
                .collect()
        })
        .collect()
}

#[test]
fn the_text_of_every_fixture_equals_the_text_preview() {
    for name in ["sheet.xlsx", "sheet.ods", "sheet.xls", "formulas.xlsx"] {
        let data = fixture(name);
        let list = sheet_list(&data, &plain()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!list.sheets.is_empty(), "{name}");
        let sheets = preview_sheets(&ok(&data));
        assert_eq!(sheets.len(), list.sheets.len(), "{name}");
        for (i, expected) in sheets.iter().enumerate() {
            let n = page_count(&data, i, &plain()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(n >= 1);
            let mut got = Vec::new();
            for p in 0..n {
                let page = render(&data, i, p as usize);
                assert_eq!((page.page.index, page.page.pages), (p, n));
                got.extend(texts(&page.page));
            }
            assert_eq!(
                letters(got),
                letters(expected.iter().cloned()),
                "{name} sheet {i}: the page and the preview disagree"
            );
        }
    }
}

#[test]
fn fixtures_that_are_not_sheets_or_are_encrypted_are_refused() {
    for name in ["report.docx", "report.pdf", "slides.pptx", "sheet.csv"] {
        let data = fixture(name);
        assert!(sheet_list(&data, &plain()).is_err(), "{name}");
        assert!(render_sheet_page(&data, 0, 0, &plain()).is_err(), "{name}");
    }
    for name in ["enc-agile.xlsx", "enc-standard.docx"] {
        assert!(sheet_list(&fixture(name), &plain()).is_err(), "{name}");
    }
    let data = fixture("sheet.xlsx");
    assert!(matches!(render_sheet_page(&data, 99, 0, &plain()), Err(PreviewError::Malformed(_))));
    assert!(matches!(render_sheet_page(&data, 0, 99, &plain()), Err(PreviewError::Malformed(_))));
}

#[test]
fn the_fixture_sheets_have_a_look() {
    // The LibreOffice-made fixtures have a header with a fill, sizes and widths.
    for name in ["sheet.xlsx", "sheet.ods", "sheet.xls"] {
        let data = fixture(name);
        let page = render(&data, 0, 0);
        assert!(!page.page.prims.is_empty(), "{name}");
        assert!(!texts(&page.page).is_empty(), "{name}");
        // Opening the SVG for a look is a developer's job (see the example); here it must exist.
        assert!(to_svg(&page.page).contains("<text"), "{name}");
    }
}

#[test]
fn text_that_could_attack_the_reader_is_cleaned() {
    let sheet = format!(
        r#"<sheetData><row r="1">{}</row></sheetData>"#,
        text_cell("A1", 0, "a&#x202E;b&#x200B;c&#x2066;d")
    );
    let page = render(&xlsx1(&sheet, &colour_styles()), 0, 0);
    let t = texts(&page.page);
    assert_eq!(t, ["abcd"]);
}

// ---------------------------------------------------------------------------
// Hostile documents
// ---------------------------------------------------------------------------

#[test]
fn the_whole_sheet_as_used_range_is_bounded() {
    let sheet = r#"<sheetData><row r="1"><c r="A1" s="1"><v>1</v></c></row><row r="1048576"><c r="XFD1048576" s="1"/></row></sheetData>
        <mergeCells count="1"><mergeCell ref="B2:XFC1048575"/></mergeCells>"#;
    let data = xlsx1(sheet, &colour_styles());
    let n = page_count(&data, 0, &plain()).expect("count");
    assert_eq!(n as usize, model::MAX_PAGES);
    for p in [0usize, 1, 4_999] {
        let page = render(&data, 0, p);
        assert!(page.page.truncated, "the sheet is cut at the page limit");
        assert!(has_notice(&page.notices, NoticeKind::Truncated, "sheet pages"));
    }
    assert!(render_sheet_page(&data, 0, 5_000, &plain()).is_err());
}

#[test]
fn a_million_merged_ranges_are_cut() {
    let mut s = String::from(
        r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><mergeCells count="1000000">"#,
    );
    for n in 1..=1_000_000u32 {
        s.push_str(&format!(r#"<mergeCell ref="A{n}:B{n}"/>"#));
    }
    s.push_str("</mergeCells>");
    let data = xlsx1(&s, &colour_styles());
    let page = render(&data, 0, 0);
    assert!(page.page.truncated);
    assert!(has_notice(&page.notices, NoticeKind::Truncated, "sheet content"));
}

#[test]
fn absurd_sizes_fall_back_to_sane_ones() {
    let sheet = format!(
        r#"<cols><col min="1" max="1" width="NaN" customWidth="1"/><col min="2" max="2" width="-5" customWidth="1"/>
        <col min="3" max="3" width="1e30" customWidth="1"/><col min="4" max="4" width="inf" customWidth="1"/></cols>
        <sheetData>
        <row r="1" ht="1e30" customHeight="1">{}</row>
        <row r="2" ht="-3" customHeight="1">{}</row>
        <row r="3" ht="NaN" customHeight="1">{}</row>
        <row r="4" ht="0" customHeight="1">{}</row>
        </sheetData>"#,
        text_cell("A1", 1, "x")
            + &text_cell("B1", 2, "x")
            + &text_cell("C1", 3, "x")
            + &text_cell("D1", 1, "x"),
        ["A2", "B2"].map(|r| text_cell(r, 2, "x")).concat(),
        ["A3"].map(|r| text_cell(r, 3, "x")).concat(),
        ["A4"].map(|r| text_cell(r, 1, "x")).concat()
    );
    let page = render(&xlsx1(&sheet, &colour_styles()), 0, 0);
    let r = fills(&page.page);
    assert!(!r.is_empty());
    for (x0, y0, x1, y1, _) in &r {
        assert!(x1 - x0 <= 1401.0 && y1 - y0 <= 410.5, "{x0} {y0} {x1} {y1}");
    }
    // Row 1 is the largest a row can be; the zero-height row 4 has no cells to show.
    assert!(r.iter().any(|f| near(f.3 - f.1, 409.5 + 0.6)));
    // Not-a-number and negative sizes are the defaults.
    assert!(r.iter().any(|f| near(f.2 - f.0, 48.0 + 0.6)));
    // The 1400-point column does not fit a page: it has a page to itself, cut at the paper.
    assert!(page_count(&xlsx1(&sheet, &colour_styles()), 0, &plain()).expect("count") >= 2);
    let wide = render(&xlsx1(&sheet, &colour_styles()), 0, 1);
    assert!(
        fills(&wide.page).iter().any(|f| near(f.2 - f.0, 494.48 + 0.6)),
        "{:?}",
        fills(&wide.page)
    );
    assert!(page_count(&xlsx1(&sheet, &colour_styles()), 0, &plain()).expect("count") >= 1);
}

#[test]
fn two_hundred_thousand_styles_are_bounded() {
    let mut xfs = String::from(
        r#"<xf fontId="0" fillId="0" borderId="0"/><xf fontId="0" fillId="2" borderId="0"/>"#,
    );
    for i in 0..200_000 {
        xfs.push_str(&format!(
            r#"<xf numFmtId="{}" fontId="0" fillId="2" borderId="0"/>"#,
            1000 + i * 7
        ));
    }
    let fills = format!(
        r#"<fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill>{}"#,
        solid("FF0000")
    );
    let sheet =
        r#"<sheetData><row r="1"><c r="A1" s="1"/><c r="B1" s="150000"/></row></sheetData>"#;
    let lenient = PreviewPolicy { aggregate_ratio: 1_000_000, ..plain() };
    let page = render_with(&xlsx1(sheet, &styles_xml(&fills, "", &xfs)), 0, 0, &lenient);
    // The format kept is painted; the one past the limit falls back to the plain format.
    assert_eq!(fills_of(&page.page).len(), 1);
    assert!(has_notice(&page.notices, NoticeKind::Unsupported, "more cell formats"));
}

#[test]
fn circular_theme_colours_do_not_loop() {
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="x">
        <a:dk1><a:schemeClr val="lt1"/></a:dk1><a:lt1><a:schemeClr val="dk1"/></a:lt1>
        <a:dk2><a:srgbClr val="112233"/></a:dk2><a:lt2><a:schemeClr val="accent1"/></a:lt2>
        <a:accent1><a:schemeClr val="accent2"/></a:accent1><a:accent2><a:schemeClr val="accent1"/></a:accent2>
        </a:clrScheme></a:themeElements></a:theme>"#;
    let fills = r#"<fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor theme="3"/></patternFill></fill><fill><patternFill patternType="solid"><fgColor theme="1"/></patternFill></fill>"#;
    let xfs = r#"<xf fontId="0" fillId="0" borderId="0"/><xf fontId="0" fillId="2" borderId="0"/><xf fontId="0" fillId="3" borderId="0"/>"#;
    let sheet = r#"<sheetData><row r="1"><c r="A1" s="1"/><c r="B1" s="2"/></row></sheetData>"#;
    let data = xlsx_book(&[("S", sheet, false)], &styles_xml(fills, "", xfs), Some(theme));
    let page = render(&data, 0, 0);
    let r = fills_of(&page.page);
    assert_eq!(r.len(), 2);
    // The scheme colour that is given directly is used; the ones that point at each other fall
    // back to the defaults.
    assert_eq!(r[0].4, 0xFF11_2233);
    assert_eq!(r[1].4, 0xFF00_0000);
}

// ---------------------------------------------------------------------------
// ODS
// ---------------------------------------------------------------------------

const ODS_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink""#;

fn ods(auto: &str, body: &str, styles_auto: &str) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {ODS_NS}><office:automatic-styles>{auto}</office:automatic-styles><office:body><office:spreadsheet>{body}</office:spreadsheet></office:body></office:document-content>"#
    );
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {ODS_NS}><office:automatic-styles>{styles_auto}</office:automatic-styles></office:document-styles>"#
    );
    zip(&[
        ("mimetype", b"application/vnd.oasis.opendocument.spreadsheet"),
        ("content.xml", content.as_bytes()),
        ("styles.xml", styles.as_bytes()),
    ])
}

const ODS_STYLES: &str = r##"
    <style:style style:name="co1" style:family="table-column"><style:table-column-properties style:column-width="2.54cm"/></style:style>
    <style:style style:name="co2" style:family="table-column"><style:table-column-properties style:column-width="5.08cm"/></style:style>
    <style:style style:name="ro1" style:family="table-row"><style:table-row-properties style:row-height="1.27cm"/></style:style>
    <style:style style:name="ce1" style:family="table-cell"><style:table-cell-properties fo:background-color="#ff0000" fo:border="1pt solid #000000"/></style:style>
    <style:style style:name="ce2" style:family="table-cell" style:parent-style-name="ce1"><style:table-cell-properties fo:background-color="#00ff00" fo:wrap-option="wrap"/><style:text-properties fo:font-weight="bold" fo:font-size="12pt" fo:color="#0000ff"/><style:paragraph-properties fo:text-align="end"/></style:style>
"##;

#[test]
fn an_ods_sheet_has_its_widths_heights_fills_and_borders() {
    let body = r#"<table:table table:name="T">
        <table:table-column table:style-name="co1"/><table:table-column table:style-name="co2" table:number-columns-repeated="2"/>
        <table:table-row table:style-name="ro1">
          <table:table-cell table:style-name="ce1" office:value-type="string"><text:p>A</text:p></table:table-cell>
          <table:table-cell table:style-name="ce2" office:value-type="string"><text:p>B</text:p></table:table-cell>
          <table:table-cell table:number-columns-spanned="1" office:value-type="string"><text:p>C</text:p></table:table-cell>
        </table:table-row></table:table>"#;
    let data = ods(ODS_STYLES, body, "");
    let page = render(&data, 0, 0);
    let r = fills(&page.page);
    assert_eq!(r.len(), 2, "{r:?}");
    // 2.54 cm is 72 points, 5.08 cm is 144; the row is 1.27 cm, 36 points.
    assert!(near(r[0].0, ML - BLEED) && near(r[0].2, ML + 72.0 + BLEED));
    assert!(near(r[1].0, ML + 72.0 - BLEED) && near(r[1].2, ML + 72.0 + 144.0 + BLEED));
    assert!(near(r[0].1, MT - BLEED) && near(r[0].3, MT + 36.0 + BLEED));
    assert_eq!((r[0].4, r[1].4), (0xFFFF_0000, 0xFF00_FF00));
    // The border of "ce1" is a line 1 point wide; "ce2" inherits it and is bold, blue, 12 points,
    // right aligned.
    assert!(page.page.prims.iter().any(
        |p| matches!(p, Prim::Path { stroke: 0xFF00_0000, width, .. } if (*width - 1.0).abs() < 0.01)
    ));
    let b = page.page.prims.iter().find_map(|p| match p {
        Prim::Text { text, size, color, style, x, width, .. } if text == "B" => {
            Some((*size, *color, *style, *x + *width))
        }
        _ => None,
    });
    let (size, color, style, right) = b.expect("B");
    assert!(near(size, 12.0) && color == 0xFF00_00FF && style & crate::page::style::BOLD != 0);
    assert!(near(right, ML + 72.0 + 144.0 - 2.0), "{right}");
}

#[test]
fn ods_merges_hidden_columns_and_repeats() {
    let body = r#"<table:table table:name="T">
        <table:table-column table:style-name="co1"/><table:table-column table:style-name="co1" table:visibility="collapse"/><table:table-column table:style-name="co1" table:number-columns-repeated="3"/>
        <table:table-row table:style-name="ro1">
          <table:table-cell table:style-name="ce1" table:number-columns-spanned="3" table:number-rows-spanned="2" office:value-type="string"><text:p>M</text:p></table:table-cell>
          <table:covered-table-cell/><table:covered-table-cell/>
          <table:table-cell office:value-type="string"><text:p>D</text:p></table:table-cell>
        </table:table-row>
        <table:table-row table:style-name="ro1"><table:covered-table-cell table:number-columns-repeated="3"/></table:table-row>
        <table:table-row table:number-rows-repeated="1048000"><table:table-cell table:style-name="ce1" table:number-columns-repeated="1024"/></table:table-row>
        </table:table>
        <table:table table:name="Second"><table:table-row><table:table-cell office:value-type="string"><text:p>second</text:p></table:table-cell></table:table-row></table:table>"#;
    let data = ods(ODS_STYLES, body, "");
    let list = sheet_list(&data, &plain()).expect("list");
    assert_eq!(list.sheets.len(), 2);
    let page = render_with(&data, 0, 0, &omit_hidden());
    let r = fills(&page.page);
    // The merge covers columns 0..2 of which column 1 is hidden: 72 + 72 points wide, two rows
    // of 36 points; the repeated styled rows below the data are not part of the used range.
    let m = r.iter().find(|f| f.4 == 0xFFFF_0000).expect("merge fill");
    assert!(near(m.2 - m.0, 144.0 + 2.0 * BLEED), "{m:?}");
    assert!(near(m.3 - m.1, 72.0 + 2.0 * BLEED), "{m:?}");
    assert!(
        has_notice(&page.notices, NoticeKind::HiddenOmitted, "columns")
            || page.notices.iter().all(|n| n.kind != NoticeKind::HiddenShown)
    );
    assert_eq!(page_count(&data, 0, &plain()).expect("count"), 1);
    assert!(texts(&page.page).contains(&"M".to_owned()));
    assert_eq!(texts(&render(&data, 1, 0).page), ["second"]);
}

#[test]
fn a_million_repeated_ods_rows_stay_ranges() {
    let body = r#"<table:table table:name="T">
        <table:table-column table:style-name="co1" table:number-columns-repeated="16384"/>
        <table:table-row table:style-name="ro1" table:number-rows-repeated="1048576"><table:table-cell table:style-name="ce1" table:number-columns-repeated="16384"/></table:table-row>
        <table:table-row><table:table-cell office:value-type="string"><text:p>end</text:p></table:table-cell></table:table-row>
        </table:table>"#;
    let data = ods(ODS_STYLES, body, "");
    // The text is in row 1048577 of the table, outside any sheet; the sheet is still bounded.
    let n = page_count(&data, 0, &plain());
    assert!(n.is_ok());
    let page = render(&data, 0, 0);
    assert!(page.page.prims.len() <= MAX_PRIMS);
}

// ---------------------------------------------------------------------------
// XLS
// ---------------------------------------------------------------------------

fn biff_rec(id: u16, data: &[u8]) -> Vec<u8> {
    let mut o = id.to_le_bytes().to_vec();
    o.extend(u16::try_from(data.len()).unwrap_or(0).to_le_bytes());
    o.extend(data);
    o
}

fn biff_bof(dt: u16) -> Vec<u8> {
    let mut d = 0x0600u16.to_le_bytes().to_vec();
    d.extend(dt.to_le_bytes());
    d.extend([0u8; 12]);
    biff_rec(0x0809, &d)
}

fn biff_xf(fill_icv: u16, align: u8) -> Vec<u8> {
    let mut d = vec![0u8; 20];
    d[6] = align;
    if fill_icv != 0 {
        d[14..18].copy_from_slice(&(1u32 << 26).to_le_bytes());
        d[18..20].copy_from_slice(&fill_icv.to_le_bytes());
    }
    biff_rec(0x00E0, &d)
}

fn xls_file() -> Vec<u8> {
    let font = {
        let mut d = Vec::new();
        d.extend(240u16.to_le_bytes()); // 12 points
        d.extend(0u16.to_le_bytes());
        d.extend(0x7FFFu16.to_le_bytes());
        d.extend(700u16.to_le_bytes()); // bold
        d.extend([0u8; 4]);
        d.extend([0u8, 0u8]);
        d.push(5);
        d.push(0);
        d.extend(b"Arial");
        biff_rec(0x0031, &d)
    };
    // Row 1 is 30 points tall; column A is 10 characters wide; A1 holds 42 on a red
    // background, centred; B1:C2 are merged.
    let sheet: Vec<Vec<u8>> = {
        let mut colinfo = Vec::new();
        for v in [0u16, 0, 2560, 0, 0, 0] {
            colinfo.extend(v.to_le_bytes());
        }
        let mut row = Vec::new();
        for v in [0u16, 0, 1, 600, 0, 0, 0x40, 0] {
            row.extend(v.to_le_bytes());
        }
        let mut cell = Vec::new();
        for v in [0u16, 0, 1] {
            cell.extend(v.to_le_bytes());
        }
        cell.extend(42f64.to_le_bytes());
        let mut merge = 1u16.to_le_bytes().to_vec();
        for v in [0u16, 1, 1, 2] {
            merge.extend(v.to_le_bytes());
        }
        vec![
            biff_rec(0x007D, &colinfo),
            biff_rec(0x0208, &row),
            biff_rec(0x0203, &cell),
            biff_rec(0x00E5, &merge),
        ]
    };
    let globals = |offset: u32| -> Vec<u8> {
        let mut g = biff_bof(5);
        g.extend(&font);
        g.extend(biff_xf(0, 0));
        // Format 1: red (palette entry 10), centred horizontally.
        g.extend(biff_xf(10, 0x02));
        let mut bs = offset.to_le_bytes().to_vec();
        bs.extend([0u8, 0u8, 1u8, 0u8, b'S']);
        g.extend(biff_rec(0x0085, &bs));
        g.extend(biff_rec(0x000A, &[]));
        g
    };
    let probe = globals(0);
    let offset = u32::try_from(probe.len()).unwrap_or(0);
    let mut stream = globals(offset);
    stream.extend(biff_bof(0x10));
    for r in &sheet {
        stream.extend(r);
    }
    stream.extend(biff_rec(0x000A, &[]));
    let mut b = arca_container::cfb::writer::CfbBuilder::new();
    assert!(b.add_stream("Workbook", stream).is_ok());
    b.finish().unwrap_or_default()
}

#[test]
fn an_xls_sheet_has_its_widths_heights_fills_and_merges() {
    let data = xls_file();
    let list = sheet_list(&data, &plain()).expect("list");
    assert_eq!(list.sheets.len(), 1);
    let page = render(&data, 0, 0);
    let r = fills(&page.page);
    assert_eq!(r.len(), 2, "{r:?}");
    // 2560/256 = 10 characters: 70 pixels, 52.5 points. Row 1 is 600 twips, 30 points.
    assert!(near(r[0].0, ML - BLEED) && near(r[0].2, ML + 52.5 + BLEED), "{:?}", r[0]);
    assert!(near(r[0].1, MT - BLEED) && near(r[0].3, MT + 30.0 + BLEED), "{:?}", r[0]);
    assert_eq!(r[0].4, 0xFFFF_0000);
    let t = page.page.prims.iter().find_map(|p| match p {
        Prim::Text { text, x, width, .. } if text == "42" => Some(*x + *width / 2.0),
        _ => None,
    });
    assert!(near(t.expect("42"), ML + 52.5 / 2.0));
}

// ---------------------------------------------------------------------------
// Mutations never panic
// ---------------------------------------------------------------------------

#[test]
fn damaged_workbooks_give_errors_or_pages_never_panics() {
    use crate::testkit::{Lcg, fuzz_iterations};
    let sources = [fixture("sheet.xlsx"), fixture("sheet.ods"), fixture("sheet.xls"), xls_file()];
    let mut rng = Lcg::new(0x5EE7);
    for _ in 0..fuzz_iterations(40) {
        for src in &sources {
            let mut data = src.clone();
            let count = 1 + rng.below(8);
            rng.mutate(&mut data, count);
            if let Ok(n) = page_count(&data, 0, &plain()) {
                let _ = render_sheet_page(&data, 0, rng.below(n as usize + 1), &plain());
            }
            let _ = sheet_list(&data, &omit_hidden());
        }
    }
}

#[test]
fn the_page_entry_point_draws_workbook_pages_in_sheet_order() {
    let rows = format!("<row r=\"1\">{}</row>", text_cell("A1", 0, "Hello"));
    let second = format!("<row r=\"1\">{}</row>", text_cell("A1", 0, "World"));
    let data = xlsx_book(&[("One", &rows, false), ("Two", &second, false)], "", None);
    let policy = plain();
    let first = crate::render_page(&data, 0, &policy).unwrap();
    assert!(texts(&first).iter().any(|t| t == "Hello"));
    assert_eq!(first.index, 0);
    assert_eq!(first.pages, 2);
    let last = crate::render_page(&data, 1, &policy).unwrap();
    assert!(texts(&last).iter().any(|t| t == "World"));
    assert_eq!(last.index, 1);
    assert!(crate::render_page(&data, 2, &policy).is_err());
}
