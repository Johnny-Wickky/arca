//! Hostile documents for the hardening pass: each test builds the document that used to be
//! mishandled and pins the behaviour that replaced it.

use crate::model::{Block, Grid, NoticeKind, Preview, SectionKind};
use crate::testkit::{docx_with, ok, opc, rels, section_of, text_of};

fn has_notice(p: &Preview, kind: NoticeKind, detail: &str) -> bool {
    p.notices.iter().any(|n| n.kind == kind && n.detail.contains(detail))
}

fn body_text(p: &Preview) -> String {
    section_of(p, SectionKind::Body).map(text_of).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// C3: namespaces and attribute prefixes
// ---------------------------------------------------------------------------

#[test]
fn a_foreign_namespace_t_is_not_word_text() {
    let body =
        r#"<w:p><w:r><w:t>real</w:t></w:r><w:r><x:t xmlns:x="urn:evil:ns">ghost</x:t></w:r></w:p>"#;
    let p = ok(&docx_with(body, &[]));
    assert_eq!(body_text(&p), "real");
    assert!(has_notice(&p, NoticeKind::Unsupported, "foreign namespace"));
}

#[test]
fn the_end_tag_of_a_foreign_element_does_not_close_a_real_one() {
    // `<t xmlns="urn:evil"/>` is not `w:t`: neither its start nor its end may touch the state of
    // the enclosing real `w:t`.
    let body = r#"<w:p><w:r><w:t>a<t xmlns="urn:evil"/>b</w:t></w:r></w:p>"#;
    let p = ok(&docx_with(body, &[]));
    assert_eq!(body_text(&p), "ab");
    assert!(has_notice(&p, NoticeKind::Unsupported, "foreign namespace"));
}

#[test]
fn the_hyperlink_target_comes_from_the_relationship_attribute_not_a_look_alike() {
    let r = rels(&[
        ("rIdFake", "hyperlink", "https://evil.example/decoy"),
        ("rId5", "hyperlink", "https://good.example/real"),
    ]);
    let body = r#"<w:p><w:hyperlink w:id="rIdFake" r:id="rId5"><w:r><w:t>site</w:t></w:r></w:hyperlink></w:p>"#;
    let p = ok(&docx_with(body, &[("word/_rels/document.xml.rels", r.as_str())]));
    let sec = section_of(&p, SectionKind::Body).expect("body");
    let hrefs: Vec<&str> = crate::testkit::paragraphs(sec)
        .iter()
        .flat_map(|para| para.runs.iter().map(|r| r.href.as_str()))
        .collect();
    assert_eq!(hrefs, ["https://good.example/real"]);
}

#[test]
fn a_relationship_prefix_other_than_r_is_followed_by_its_namespace() {
    let r = rels(&[("rId5", "hyperlink", "https://good.example/real")]);
    let doc =
            r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:rel="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:hyperlink rel:id="rId5"><w:r><w:t>site</w:t></w:r></w:hyperlink></w:p></w:body></w:document>"#.to_owned();
    let data = opc(
        ("word/document.xml", crate::testkit::DOCX_CT),
        &[("word/document.xml", doc.as_str()), ("word/_rels/document.xml.rels", r.as_str())],
    );
    let p = ok(&data);
    let sec = section_of(&p, SectionKind::Body).expect("body");
    let first = crate::testkit::paragraphs(sec)[0].runs[0].href.clone();
    assert_eq!(first, "https://good.example/real");
}

#[test]
fn relationship_attributes_with_a_foreign_prefix_are_not_the_relationship() {
    // `x:Target` comes first and would win a first-match-by-local-name lookup.
    let rel = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships" xmlns:x="urn:evil"><Relationship x:Id="rIdBad" x:Target="https://evil.example/" Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://good.example/" TargetMode="External"/><x:Relationship Id="rId8" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://evil.example/second" TargetMode="External"/></Relationships>"#;
    let body = r#"<w:p><w:hyperlink r:id="rId7"><w:r><w:t>one</w:t></w:r></w:hyperlink><w:hyperlink r:id="rId8"><w:r><w:t>two</w:t></w:r></w:hyperlink></w:p>"#;
    let p = ok(&docx_with(body, &[("word/_rels/document.xml.rels", rel)]));
    let sec = section_of(&p, SectionKind::Body).expect("body");
    let hrefs: Vec<&str> = crate::testkit::paragraphs(sec)
        .iter()
        .flat_map(|para| para.runs.iter().map(|r| r.href.as_str()))
        .collect();
    assert_eq!(hrefs, ["https://good.example/", ""], "the foreign Relationship is not one");
}

fn xlsx_two_sheets(sheet_a_attrs: &str) -> Vec<u8> {
    let ns = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:x="urn:evil""#;
    let wb =
        format!(r#"<workbook {ns}><sheets><sheet name="A" {sheet_a_attrs}/></sheets></workbook>"#);
    let wb_rels = rels(&[
        ("rId11", "worksheet", "worksheets/sheet1.xml"),
        ("rId12", "worksheet", "worksheets/sheet2.xml"),
    ]);
    let sheet = |t: &str| {
        format!(
            r#"<worksheet {ns}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>{t}</t></is></c></row></sheetData></worksheet>"#
        )
    };
    let (s1, s2) = (sheet("REAL"), sheet("DECOY"));
    opc(
        (
            "xl/workbook.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        ),
        &[
            ("xl/workbook.xml", wb.as_str()),
            ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
            ("xl/worksheets/sheet1.xml", s1.as_str()),
            ("xl/worksheets/sheet2.xml", s2.as_str()),
        ],
    )
}

fn first_cell_text(g: &Grid) -> String {
    g.rows.first().and_then(|r| r.cells.first()).map(|c| c.text.clone()).unwrap_or_default()
}

#[test]
fn the_sheet_is_found_by_the_relationship_id_attribute() {
    let p = ok(&xlsx_two_sheets(r#"sheetId="1" x:id="rId12" r:id="rId11""#));
    let sheet = section_of(&p, SectionKind::Sheet).expect("sheet");
    let Some(Block::Grid(g)) = sheet.blocks.first() else { panic!("grid") };
    assert_eq!(first_cell_text(g), "REAL");
}

// ---------------------------------------------------------------------------
// C11: field keywords are case-insensitive
// ---------------------------------------------------------------------------

#[test]
fn hyperlink_field_keyword_is_case_insensitive() {
    for kw in ["HYPERLINK", "hyperlink", "Hyperlink", "HyperLink"] {
        assert_eq!(
            crate::docx::hyperlink_of(&format!(" {kw} \"http://a.example/x\"")).as_deref(),
            Some("http://a.example/x"),
            "{kw}"
        );
    }
}

// ---------------------------------------------------------------------------
// C8: the page view is cleaned like every other text
// ---------------------------------------------------------------------------

mod pdf_tests {
    use crate::PreviewPolicy;
    use crate::model::NoticeKind;
    use crate::page::Prim;
    use crate::pdf::testpdf::PdfBuilder;

    /// A page that shows `(A)` in a font whose ToUnicode maps code `A` to `dest` (hex UTF-16BE).
    fn page_with_map(dest: &str) -> PdfBuilder {
        let mut b = PdfBuilder::simple_text("A");
        b.add_raw(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>",
        );
        let cmap = format!(
            "1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <{dest}> endbfchar"
        );
        b.add_stream(6, "", cmap.as_bytes());
        b
    }

    #[test]
    fn bidi_and_tag_characters_do_not_reach_the_page_view() {
        // U+202E, "A", U+E0041 (a tag letter) and a tab.
        let data = page_with_map("202E0041DB40DC410009").build();
        let m = crate::pdf::render(&data, 0, &PreviewPolicy::default()).expect("page");
        let texts: Vec<&str> = m
            .prims
            .iter()
            .filter_map(
                |p| if let Prim::Text { text, .. } = p { Some(text.as_str()) } else { None },
            )
            .collect();
        assert_eq!(texts, ["A "], "{texts:?}");
        assert!(m.approximate, "text was changed on the way, and the page says so");
    }

    #[test]
    fn a_text_map_that_disagrees_with_the_encoding_is_reported() {
        // The glyph for code 0x41 is an "A" (WinAnsi); the text layer says "B".
        let data = page_with_map("0042").build();
        let p = crate::preview(&data, &PreviewPolicy::default()).expect("preview");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort
            && n.detail.contains("differs from the font encoding")));
        // A ligature expansion is not a disagreement.
        let data = page_with_map("00410042").build();
        let p = crate::preview(&data, &PreviewPolicy::default()).expect("preview");
        assert!(!p.notices.iter().any(|n| n.detail.contains("differs from the font encoding")));
    }

    #[test]
    fn a_text_map_with_huge_destinations_maps_nothing() {
        let big = "0041".repeat(40_000);
        let data = page_with_map(&big).build();
        let p = crate::preview(&data, &PreviewPolicy::default()).expect("preview");
        let all: usize = p
            .sections
            .iter()
            .flat_map(|s| &s.blocks)
            .map(|b| if let crate::model::Block::Para(q) = b { q.text().len() } else { 0 })
            .sum();
        assert!(all < 64, "{all} bytes of text from one code");
    }
}

// ---------------------------------------------------------------------------
// C9 / C6: what the sink removes, cuts and says
// ---------------------------------------------------------------------------

mod sink_tests {
    use crate::PreviewPolicy;
    use crate::model::{
        Block, CellKind, FileFormat, Grid, GridCell, GridRow, NoticeKind, Paragraph, Row, Run,
        SectionKind, Table,
    };
    use crate::sink::{CellLink, CleanCounts, GridBuilder, Sink, clean_text};

    #[test]
    fn invisible_characters_of_the_audit_list_are_removed() {
        let mut c = CleanCounts::default();
        for cp in [
            0x206A, 0x206F, 0x2065, 0xFE00, 0xFE0D, 0x180B, 0x180C, 0x180D, 0x00AD, 0x17B4, 0x17B5,
            0x1D173, 0x1D17A, 0x1BCA0,
        ] {
            let s = format!("a{}b", char::from_u32(cp).expect("a scalar"));
            assert_eq!(clean_text(&s, &mut c), "ab", "{cp:X}");
        }
        // Variation selectors after letters carry bits; after a symbol or a digit they select
        // a presentation and stay.
        assert_eq!(clean_text("a\u{FE0F}b", &mut c), "ab");
        assert_eq!(clean_text("\u{2764}\u{FE0F}", &mut c), "\u{2764}\u{FE0F}");
        assert_eq!(clean_text("1\u{FE0F}\u{20E3}", &mut c), "1\u{FE0F}\u{20E3}");
        assert_eq!(clean_text("\u{2764}\u{FE0F}\u{FE0F}\u{FE01}", &mut c), "\u{2764}\u{FE0F}");
        // A braille blank is a blank that must not pass for nothing.
        assert_eq!(clean_text("a\u{2800}b", &mut c), "a b");
        assert!(c.tags > 0 && c.zero_width > 0);
    }

    fn one_para(sink: &mut Sink<'_>, sec: crate::sink::SectionId, p: Paragraph) {
        assert!(sink.para(sec, p));
    }

    #[test]
    fn names_addresses_and_markers_are_single_line() {
        let policy = PreviewPolicy::default();
        let mut sink = Sink::new(&policy);
        let sec = sink.open_section(SectionKind::Sheet, "Sheet\n1\tx", false).expect("section");
        one_para(
            &mut sink,
            sec,
            Paragraph {
                marker: "1.\n".into(),
                runs: vec![Run { text: "t\nu".into(), style: 0, href: "http://a/\nb\tc".into() }],
                ..Paragraph::default()
            },
        );
        let p = sink.finish(FileFormat::Docx);
        assert_eq!(p.sections[0].name, "Sheet 1 x");
        let Block::Para(para) = &p.sections[0].blocks[0] else { panic!("para") };
        assert_eq!(para.marker, "1. ");
        assert_eq!(para.runs[0].href, "http://a/ b c");
        assert_eq!(para.runs[0].text, "t\nu", "a line break inside text stays");
    }

    #[test]
    fn a_link_address_that_is_too_long_is_dropped_and_said() {
        let policy = PreviewPolicy { max_href_bytes: 16, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&policy);
        let sec = sink.open_section(SectionKind::Body, "", false).expect("section");
        one_para(
            &mut sink,
            sec,
            Paragraph {
                runs: vec![Run {
                    text: "x".into(),
                    style: 0,
                    href: "https://good.example/".to_owned() + &"a".repeat(100) + "/evil",
                }],
                ..Paragraph::default()
            },
        );
        let p = sink.finish(FileFormat::Docx);
        let Block::Para(para) = &p.sections[0].blocks[0] else { panic!("para") };
        assert_eq!(para.runs[0].href, "", "a cut address is a different address");
        assert!(p.stats.truncated);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::Truncated && n.detail.contains("link"))
        );
    }

    #[test]
    fn cut_cell_text_is_reported() {
        let policy = PreviewPolicy { max_cell_bytes: 8, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&policy);
        let sec = sink.open_section(SectionKind::Body, "", false).expect("section");
        one_para(&mut sink, sec, Paragraph::plain("0123456789abcdef"));
        let p = sink.finish(FileFormat::Docx);
        assert!(p.stats.truncated);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::Truncated && n.detail == "text length")
        );
    }

    fn grid_of(rows: u32) -> Grid {
        Grid {
            cols: 1,
            rows: (1..=rows)
                .map(|i| GridRow {
                    index: i,
                    hidden: false,
                    cells: vec![GridCell {
                        col: 0,
                        kind: CellKind::Text,
                        text: "0123456789".into(),
                        href: String::new(),
                    }],
                })
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn a_grid_over_the_budget_is_cut_by_rows_not_dropped() {
        let policy = PreviewPolicy { max_chars: 35, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&policy);
        let sec = sink.open_section(SectionKind::Sheet, "S", false).expect("section");
        assert!(!sink.push(sec, Block::Grid(grid_of(10))), "the budget ran out");
        let p = sink.finish(FileFormat::Xlsx);
        let Block::Grid(g) = &p.sections[0].blocks[0] else { panic!("grid") };
        assert_eq!(g.rows.len(), 3);
        assert!(g.truncated && p.stats.truncated);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Truncated));
    }

    #[test]
    fn a_table_over_the_budget_is_cut_by_rows_not_dropped() {
        let policy = PreviewPolicy { max_chars: 35, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&policy);
        let sec = sink.open_section(SectionKind::Body, "", false).expect("section");
        let row = || Row {
            cells: vec![crate::model::Cell {
                span: 1,
                header: false,
                paras: vec![Paragraph::plain("0123456789")],
            }],
        };
        let table = Table { rows: (0..10).map(|_| row()).collect() };
        assert!(!sink.push(sec, Block::Table(table)));
        let p = sink.finish(FileFormat::Docx);
        let Block::Table(t) = &p.sections[0].blocks[0] else { panic!("table") };
        assert_eq!(t.rows.len(), 3);
    }

    #[test]
    fn links_reach_every_row_of_a_long_range() {
        let mut g = grid_of(1_000);
        let link =
            CellLink { row1: 1, row2: 1_000, col1: 0, col2: 0, href: "https://x.example/".into() };
        assert!(g.attach_links(&[link]));
        assert!(g.rows.iter().all(|r| r.cells[0].href == "https://x.example/"));
        // Past the old limit of 256 rows.
        assert_eq!(g.rows[999].cells[0].href, "https://x.example/");
    }

    #[test]
    fn grid_builder_bounds_the_bytes_and_hides_columns() {
        let policy = PreviewPolicy::default();
        let mut b = GridBuilder::new(&policy, 1_000).with_char_budget(25);
        assert!(b.row(1, false));
        let big = "x".repeat(10);
        assert!(b.cell(0, CellKind::Text, big.clone()));
        assert!(b.cell(1, CellKind::Text, big.clone()));
        assert!(!b.cell(2, CellKind::Text, big), "the byte budget is spent");
        assert!(b.finish().truncated);

        // Hidden columns: left out unless asked for, counted either way.
        let omit = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
        let mut b = GridBuilder::new(&omit, 100);
        b.hide_columns(1, 2);
        assert!(b.row(1, false));
        for c in 0..4 {
            assert!(b.cell(c, CellKind::Text, format!("c{c}")));
        }
        assert_eq!(b.hidden_columns(), 2);
        let cols: Vec<u32> = b.finish().rows[0].cells.iter().map(|c| c.col).collect();
        assert_eq!(cols, [0, 3]);
    }

    #[test]
    fn grid_rows_and_cells_are_kept_in_order_whatever_the_file_says() {
        let policy = PreviewPolicy::default();
        let mut b = GridBuilder::new(&policy, 100);
        assert!(b.row(5, false));
        assert!(b.cell(3, CellKind::Text, "d".into()));
        assert!(b.cell(1, CellKind::Text, "b".into()));
        assert!(b.row(2, false));
        assert!(b.cell(0, CellKind::Text, "a".into()));
        let g = b.finish();
        assert_eq!(g.rows.iter().map(|r| r.index).collect::<Vec<_>>(), [2, 5]);
        assert_eq!(g.rows[1].cells.iter().map(|c| c.col).collect::<Vec<_>>(), [1, 3]);
    }
}

// ---------------------------------------------------------------------------
// C6 / C7: plain text, CSV, Markdown, number formats, spreadsheets
// ---------------------------------------------------------------------------

mod text_tests {
    use std::time::{Duration, Instant};

    use crate::PreviewPolicy;
    use crate::model::{Block, CellKind, FileFormat, NoticeKind, Preview};
    use crate::sink::Sink;

    fn run(format: FileFormat, text: &str) -> Preview {
        let policy = PreviewPolicy::default();
        let mut sink = Sink::new(&policy);
        crate::plain::extract(&mut sink, text, format);
        sink.finish(format)
    }

    fn rows(p: &Preview) -> Vec<Vec<String>> {
        let Some(Block::Grid(g)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no grid")
        };
        g.rows.iter().map(|r| r.cells.iter().map(|c| c.text.clone()).collect()).collect()
    }

    #[test]
    fn an_unclosed_csv_quote_is_reported() {
        let p = run(FileFormat::Csv, "a,b\n\"never closed,1\n2,3\n");
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort
            && n.detail.contains("quoted field is never closed")));
        // A closed quote is not.
        let p = run(FileFormat::Csv, "a,b\n\"closed, fine\",1\n");
        assert!(!p.notices.iter().any(|n| n.detail.contains("never closed")));
    }

    #[test]
    fn a_lone_carriage_return_ends_a_line() {
        let p = run(FileFormat::Csv, "a,b\r1,2\r3,4");
        assert_eq!(rows(&p), [["a", "b"], ["1", "2"], ["3", "4"]]);
        let p = run(FileFormat::Csv, "a,b\r\n1,\"x\ry\"\r\n");
        assert_eq!(rows(&p), [vec!["a", "b"], vec!["1", "x\ny"]]);
        let p = run(FileFormat::PlainText, "one\rtwo\r\nthree\nfour");
        let Some(Block::Para(para)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no para")
        };
        assert_eq!(para.text(), "one\ntwo\nthree\nfour");
        let p = run(FileFormat::Markdown, "# T\rline one\rline two");
        assert_eq!(p.sections[0].blocks.len(), 2, "the heading ends at the carriage return");
    }

    #[test]
    fn a_markdown_line_of_brackets_is_linear() {
        for line in ["[".repeat(300_000), "[a](".repeat(80_000), "`a ``b ```c ".repeat(30_000)] {
            let t = Instant::now();
            let p = run(FileFormat::Markdown, &line);
            assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
            assert!(p.notices.iter().any(|n| n.detail.contains("very long lines")));
            // The text is all still there.
            let Some(Block::Para(para)) = p.sections.first().and_then(|s| s.blocks.first()) else {
                panic!("no para")
            };
            assert!(para.text().chars().count() >= 30_000);
        }
        // A normal link still works.
        let p = run(FileFormat::Markdown, "see [the docs](https://a.example/x) now");
        let Some(Block::Para(para)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no para")
        };
        assert!(para.runs.iter().any(|r| r.href == "https://a.example/x" && r.text == "the docs"));
    }

    #[test]
    fn a_format_code_of_a_million_placeholders_is_not_interpreted() {
        let t = Instant::now();
        for code in ["0".repeat(1_000_000), "0,".repeat(100_000), "#".repeat(300)] {
            let f = crate::numfmt::parse(&code);
            assert_eq!(f.number(1234.5, false), ("1234.5".to_owned(), CellKind::Number));
        }
        // Forty placeholders is still a format.
        let f = crate::numfmt::parse(&"0".repeat(20));
        assert_eq!(f.number(5.0, false).0, format!("{}5", "0".repeat(19)));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }
}

mod sheet_tests {
    use crate::PreviewPolicy;
    use crate::model::{Block, NoticeKind, SectionKind};
    use crate::testkit::{ok, opc, rels, section_of};

    const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
    const CT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";

    pub(super) fn xlsx(sheet: &str, sst: &str) -> Vec<u8> {
        let wb = format!(
            r#"<workbook {NS}><sheets><sheet name="S" sheetId="1" r:id="rId11"/></sheets></workbook>"#
        );
        let wb_rels = rels(&[
            ("rId11", "worksheet", "worksheets/sheet1.xml"),
            ("rId1", "sharedStrings", "sharedStrings.xml"),
        ]);
        let s1 = format!(r#"<worksheet {NS}>{sheet}</worksheet>"#);
        opc(
            ("xl/workbook.xml", CT),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/sharedStrings.xml", sst),
                ("xl/worksheets/sheet1.xml", s1.as_str()),
            ],
        )
    }

    #[test]
    fn hidden_columns_are_counted_and_left_out_when_not_asked_for() {
        let sheet = r#"<cols><col min="2" max="3" width="9" hidden="1"/></cols><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c><c r="B1" t="inlineStr"><is><t>b</t></is></c><c r="C1" t="inlineStr"><is><t>c</t></is></c><c r="D1" t="inlineStr"><is><t>d</t></is></c></row></sheetData>"#;
        let data = xlsx(sheet, "<sst/>");
        let p = ok(&data);
        let cells = |p: &crate::model::Preview| -> Vec<String> {
            let sec = section_of(p, SectionKind::Sheet).expect("sheet");
            let Some(Block::Grid(g)) = sec.blocks.first() else { panic!("grid") };
            g.rows[0].cells.iter().map(|c| c.text.clone()).collect()
        };
        assert_eq!(cells(&p), ["a", "b", "c", "d"]);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown
                && n.detail == "columns"
                && n.count == 2)
        );
        let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert_eq!(cells(&p), ["a", "d"]);
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted
                && n.detail == "columns"
                && n.count == 2)
        );
    }

    #[test]
    fn a_few_huge_shared_strings_referenced_by_many_cells_do_not_multiply() {
        // 20 strings of 60 KB, 4000 cells that refer to them: 240 MB if every cell kept a copy,
        // against a text budget of 1 MB.
        // (Letters that do not compress, or the archive itself is refused as a bomb.)
        let mut rng = crate::testkit::Lcg::new(7);
        let sst: String = (0..20)
            .map(|_| {
                let big: String = (0..60_000)
                    .map(|_| char::from(b'a' + u8::try_from(rng.below(26)).unwrap_or(0)))
                    .collect();
                format!("<si><t>{big}</t></si>")
            })
            .collect();
        let sst = format!("<sst {NS}>{sst}</sst>");
        let rows: String = (0..4000)
            .map(|i| {
                format!(
                    r#"<row r="{r}"><c r="A{r}" t="s"><v>{v}</v></c></row>"#,
                    r = i + 1,
                    v = i % 20
                )
            })
            .collect();
        let sheet = format!("<sheetData>{rows}</sheetData>");
        let data = xlsx(&sheet, &sst);
        let policy = PreviewPolicy { max_chars: 1 << 20, ..PreviewPolicy::default() };
        let p = crate::preview(&data, &policy).expect("preview");
        assert!(p.stats.chars <= 1 << 20, "{} bytes kept", p.stats.chars);
        // The first rows are shown, not nothing.
        let sec = section_of(&p, SectionKind::Sheet).expect("sheet");
        let Some(Block::Grid(g)) = sec.blocks.first() else { panic!("grid") };
        assert!((10..=20).contains(&g.rows.len()), "{} rows", g.rows.len());
        assert!(p.stats.truncated);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::Truncated));
    }
}

mod formula_tests {
    use std::time::{Duration, Instant};

    use super::sheet_tests::xlsx;
    use crate::model::{Block, NoticeKind, SectionKind};
    use crate::testkit::{ok, section_of};

    fn grid_texts(p: &crate::model::Preview) -> Vec<String> {
        let sec = section_of(p, SectionKind::Sheet).expect("sheet");
        let Some(Block::Grid(g)) = sec.blocks.first() else { panic!("grid") };
        g.rows.iter().flat_map(|r| r.cells.iter().map(|c| c.text.clone())).collect()
    }

    #[test]
    fn a_shared_formula_bomb_costs_nothing_per_cell() {
        // One master of 300 KB and twenty thousand cells that share it: each would have been
        // lexed (and its length counted) again.
        let master = "1+".repeat(150_000) + "1";
        let mut rows = format!(
            r#"<row r="1"><c r="A1"><f t="shared" ref="A1:A20001" si="0">{master}</f><v>1</v></c></row>"#
        );
        for r in 2..=20_001 {
            rows.push_str(&format!(r#"<row r="{r}"><c r="A{r}"><f t="shared" si="0"/></c></row>"#));
        }
        let t = Instant::now();
        let p = ok(&xlsx(&format!("<sheetData>{rows}</sheetData>"), "<sst/>"));
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        assert!(
            p.notices.iter().any(|n| n.kind == NoticeKind::Unsupported
                && n.detail.contains("shared formulas too long"))
        );
    }

    #[test]
    fn a_lookup_key_longer_than_excel_allows_is_value_error() {
        let key = "a".repeat(300);
        let sheet = format!(
            r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c><c r="B1"><f>MATCH("{key}",A1:A3,0)</f></c><c r="C1"><f>COUNTIF(A1:A3,"{key}")</f></c><c r="D1"><f>MATCH("a",A1:A3,0)</f></c></row></sheetData>"#
        );
        let p = ok(&xlsx(&sheet, "<sst/>"));
        assert_eq!(grid_texts(&p), ["a", "#VALUE!", "#VALUE!", "1"]);
    }
}

// ---------------------------------------------------------------------------
// C10 / C11: sterile copies keep what the reader should see; field keywords
// ---------------------------------------------------------------------------

mod copy_tests {
    use crate::model::{Block, SectionKind};
    use crate::testkit::{docx, ok, paragraphs, section_of, text_of};
    use crate::{LegacyOptions, PreviewPolicy, sanitize_legacy};

    fn all_text(data: &[u8]) -> String {
        let p = crate::preview(data, &PreviewPolicy::default()).expect("preview");
        p.sections
            .iter()
            .map(|s| {
                s.blocks
                    .iter()
                    .filter_map(|b| match b {
                        Block::Para(q) => Some(q.text()),
                        Block::Grid(g) => Some(
                            g.rows
                                .iter()
                                .flat_map(|r| r.cells.iter().map(|c| c.text.clone()))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_sterile_copy_shows_where_each_link_pointed() {
        // Mixed-case keyword: Word links it, so the copy must say where it went.
        let rtf = br#"{\rtf1\ansi See {\field{\*\fldinst{HyperLink "http://evil.example/login"}}{\fldrslt{the portal}}} now.\par}"#;
        let (out, _, actions) = sanitize_legacy(rtf, &LegacyOptions::default()).expect("a copy");
        let text = all_text(&out);
        assert!(text.contains("the portal (http://evil.example/login)"), "{text}");
        assert!(actions.iter().any(|a| a.target.contains("hyperlink")));
        // The words are not repeated when they are the address.
        let rtf = br#"{\rtf1\ansi {\field{\*\fldinst{HYPERLINK "http://same.example/"}}{\fldrslt{http://same.example/}}}\par}"#;
        let (out, _, _) = sanitize_legacy(rtf, &LegacyOptions::default()).expect("a copy");
        let text = all_text(&out);
        assert_eq!(text.matches("http://same.example/").count(), 1, "{text}");
    }

    #[test]
    fn the_address_is_shown_unless_it_is_the_words_or_an_internal_jump() {
        use crate::export::visible_address;
        assert_eq!(
            visible_address("click", "https://evil.example/x").as_deref(),
            Some(" (https://evil.example/x)")
        );
        assert_eq!(visible_address("https://a.example/", "https://a.example/"), None);
        assert_eq!(visible_address("see below", "#section2"), None);
        assert_eq!(visible_address("x", ""), None);
    }

    #[test]
    fn docx_hyperlink_fields_match_the_keyword_in_any_case() {
        for kw in ["HYPERLINK", "hyperlink", "Hyperlink", "HyPeRlInK"] {
            let body = format!(
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> {kw} "http://a.example/x" </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>link</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#
            );
            let p = ok(&docx(&body));
            let sec = section_of(&p, SectionKind::Body).expect("body");
            let hrefs: Vec<&str> = paragraphs(sec)
                .iter()
                .flat_map(|q| q.runs.iter().map(|r| r.href.as_str()))
                .collect();
            assert_eq!(hrefs, ["http://a.example/x"], "{kw}");
            assert_eq!(text_of(sec), "link");
        }
    }

    #[test]
    fn rtf_hyperlink_fields_match_the_keyword_in_any_case() {
        let rtf =
            br#"{\rtf1\ansi {\field{\*\fldinst{hYpErLiNk "http://r.example/"}}{\fldrslt{x}}}\par}"#;
        let p = ok(rtf);
        let sec = section_of(&p, SectionKind::Body).expect("body");
        let hrefs: Vec<&str> =
            paragraphs(sec).iter().flat_map(|q| q.runs.iter().map(|r| r.href.as_str())).collect();
        assert_eq!(hrefs, ["http://r.example/"]);
    }
}

mod unlisted_tests {
    use crate::model::NoticeKind;
    use crate::testkit::{ok, opc, rels};

    fn said(p: &crate::model::Preview, what: &str) -> bool {
        p.notices.iter().any(|n| n.kind == NoticeKind::Skipped && n.detail.contains(what))
    }

    #[test]
    fn worksheet_parts_the_workbook_does_not_list_are_reported() {
        let ns = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        let wb = format!(
            r#"<workbook {ns}><sheets><sheet name="A" sheetId="1" r:id="rId11"/></sheets></workbook>"#
        );
        let wb_rels = rels(&[("rId11", "worksheet", "worksheets/sheet1.xml")]);
        let sheet = format!(r#"<worksheet {ns}><sheetData/></worksheet>"#);
        let ct = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
        let with_extra = opc(
            ("xl/workbook.xml", ct),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/worksheets/sheet1.xml", sheet.as_str()),
                ("xl/worksheets/Sheet2.xml", sheet.as_str()),
                ("xl/worksheets/sheet3.xml", sheet.as_str()),
            ],
        );
        let p = ok(&with_extra);
        assert!(said(&p, "2 worksheet part(s)"), "{:?}", p.notices);
        let without = opc(
            ("xl/workbook.xml", ct),
            &[
                ("xl/workbook.xml", wb.as_str()),
                ("xl/_rels/workbook.xml.rels", wb_rels.as_str()),
                ("xl/worksheets/sheet1.xml", sheet.as_str()),
            ],
        );
        assert!(!said(&ok(&without), "worksheet part"));
    }

    #[test]
    fn slide_parts_the_presentation_does_not_list_are_reported() {
        let ns = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
        let pres = format!(
            r#"<p:presentation {ns}><p:sldIdLst><p:sldId id="256" r:id="rId11"/></p:sldIdLst></p:presentation>"#
        );
        let pres_rels = rels(&[("rId11", "slide", "slides/slide1.xml")]);
        let slide = format!(r#"<p:sld {ns}><p:cSld><p:spTree/></p:cSld></p:sld>"#);
        let ct =
            "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
        let data = opc(
            ("ppt/presentation.xml", ct),
            &[
                ("ppt/presentation.xml", pres.as_str()),
                ("ppt/_rels/presentation.xml.rels", pres_rels.as_str()),
                ("ppt/slides/slide1.xml", slide.as_str()),
                ("ppt/slides/slide2.xml", slide.as_str()),
            ],
        );
        let p = ok(&data);
        assert!(said(&p, "1 slide part(s)"), "{:?}", p.notices);
    }
}
