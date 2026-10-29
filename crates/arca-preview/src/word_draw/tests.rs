use super::*;
use crate::testkit::{docx, docx_with, rels};

fn texts(page: &PageModel) -> String {
    page.prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn doc(body: &str) -> PageModel {
    render_text_page(&docx(body), 0, &PreviewPolicy::default()).expect("a page")
}

#[test]
fn a_short_document_is_one_a4_page_with_its_text_on_it() {
    let page = doc(r#"<w:p><w:r><w:t>Hello page view</w:t></w:r></w:p>"#);
    assert_eq!((page.width, page.height, page.pages, page.index), (595.0, 842.0, 1, 0));
    assert!(texts(&page).contains("Hello page view"), "{}", texts(&page));
    assert!(page.approximate);
    for p in &page.prims {
        if let Prim::Text { x, y, width, .. } = p {
            assert!(*x >= MARGIN - 0.5 && x + width <= A4.0 - MARGIN + 1.0, "x {x} width {width}");
            assert!(*y > 0.0 && *y < A4.1);
        }
    }
}

#[test]
fn a_long_document_runs_on_to_more_pages_and_every_page_can_be_asked_for() {
    let body: String = (0..160)
        .map(|i| {
            format!(
                "<w:p><w:r><w:t>Paragraph number {i} has some words in it to fill a line or so.</w:t></w:r></w:p>"
            )
        })
        .collect();
    let data = docx(&body);
    let first = render_text_page(&data, 0, &PreviewPolicy::default()).unwrap();
    assert!(first.pages >= 3, "{} pages", first.pages);
    let last = render_text_page(
        &data,
        usize::try_from(first.pages).unwrap() - 1,
        &PreviewPolicy::default(),
    )
    .unwrap();
    assert!(texts(&last).contains("Paragraph number 159"));
    assert!(render_text_page(&data, 1000, &PreviewPolicy::default()).is_err(), "no such page");
}

#[test]
fn colours_sizes_highlights_and_rules_are_drawn() {
    let styles = format!(
        r#"<w:styles {}><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
        <w:style w:type="paragraph" w:styleId="Note"><w:name w:val="Note"/><w:pPr><w:pBdr><w:left w:val="single" w:color="3366CC"/></w:pBdr><w:ind w:left="720"/></w:pPr></w:style></w:styles>"#,
        crate::testkit::W_NS
    );
    let r = rels(&[("rId1", "styles", "styles.xml")]);
    let data = docx_with(
        r#"<w:p><w:pPr><w:pStyle w:val="Note"/></w:pPr><w:r><w:rPr><w:color w:val="CC0000"/><w:sz w:val="44"/></w:rPr><w:t>Big red</w:t></w:r><w:r><w:rPr><w:highlight w:val="yellow"/></w:rPr><w:t> marked</w:t></w:r></w:p>
           <w:p><w:r><w:rPr><w:color w:val="FFFFFF"/></w:rPr><w:t>white on white</w:t></w:r></w:p>"#,
        &[("word/styles.xml", styles.as_str()), ("word/_rels/document.xml.rels", r.as_str())],
    );
    let page = render_text_page(&data, 0, &PreviewPolicy::default()).unwrap();
    let red = page.prims.iter().find_map(|p| match p {
        Prim::Text { text, size, color, x, .. } if text.starts_with("Big red") => {
            Some((*size, *color, *x))
        }
        _ => None,
    });
    let (size, color, x) = red.expect("the red run");
    assert_eq!(color, 0xFFCC_0000);
    assert!((size - 22.0).abs() < 0.1, "44 half-points are 22 pt, got {size}");
    assert!((x - (MARGIN + 36.0)).abs() < 0.5, "indented by 36 pt, got {x}");
    assert!(
        page.prims.iter().any(|p| matches!(p, Prim::Path { fill, .. } if *fill == 0xFFFF_FF00)),
        "the highlight is a yellow rectangle"
    );
    assert!(
        page.prims.iter().any(|p| matches!(p, Prim::Path { fill, .. } if *fill == 0xFF33_66CC)),
        "the rule down the margin"
    );
    let white = page.prims.iter().find_map(|p| match p {
        Prim::Text { text, color, .. } if text.contains("white on white") => Some(*color),
        _ => None,
    });
    assert_eq!(white, Some(INK), "white text is shown in the viewer's colour, never hidden");
}

#[test]
fn a_table_is_drawn_as_cells_with_their_text() {
    let page = doc(
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>left</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>right</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    let t = texts(&page);
    assert!(t.contains("left") && t.contains("right"), "{t}");
    let cells = page
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::Path { stroke, .. } if *stroke == 0xFF99_9999))
        .count();
    assert_eq!(cells, 2);
}

#[test]
fn a_word_wider_than_the_line_is_cut_not_overflowed() {
    let page = doc(&format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "W".repeat(200)));
    for p in &page.prims {
        if let Prim::Text { x, width, .. } = p {
            assert!(x + width <= A4.0 - MARGIN + 1.0, "x {x} width {width}");
        }
    }
}

#[test]
fn an_empty_document_is_one_empty_page() {
    let page = doc("");
    assert_eq!(page.pages, 1);
    assert!(!page.truncated);
}

#[test]
fn the_page_limit_is_enforced() {
    let body: String =
        (0..40_000).map(|_| "<w:p><w:r><w:t>x</w:t></w:r></w:p>".to_owned()).collect();
    let page = render_text_page(&docx(&body), 0, &PreviewPolicy::default());
    if let Ok(p) = page {
        assert!(usize::try_from(p.pages).unwrap() <= MAX_PAGES + 1);
    }
}

#[test]
fn the_page_view_is_reached_through_render_page_for_word_and_rich_text() {
    let policy = PreviewPolicy::default();
    let word = docx(r#"<w:p><w:r><w:t>through the front door</w:t></w:r></w:p>"#);
    let page = crate::render_page(&word, 0, &policy).expect("a docx page");
    assert!(texts(&page).contains("through the front door"));
    let rtf = br"{\rtf1\ansi\deff0{\colortbl;\red200\green0\blue0;}\cf1 red words\par plain words}"
        .to_vec();
    let page = crate::render_page(&rtf, 0, &policy).expect("an rtf page");
    let t = texts(&page);
    assert!(t.contains("red words") && t.contains("plain words"), "{t}");
}

fn sect(extra: &str) -> String {
    format!(
        r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840" w:orient="landscape"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>{extra}</w:sectPr>"#
    )
}

fn text_at(page: &PageModel) -> Vec<(String, f32, f32, f32)> {
    page.prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { text, x, y, width, .. } => Some((text.clone(), *x, *y, *width)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_sheet_margins_header_footer_and_page_number_follow_the_document() {
    let header = format!(
        r#"<w:hdr {}><w:p><w:r><w:t>Running head</w:t></w:r></w:p></w:hdr>"#,
        crate::testkit::W_NS
    );
    let footer = format!(
        r#"<w:ftr {}><w:p><w:r><w:t>Running foot</w:t></w:r></w:p></w:ftr>"#,
        crate::testkit::W_NS
    );
    let r = rels(&[("rH", "header", "header1.xml"), ("rF", "footer", "footer1.xml")]);
    let body = format!(
        r#"<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{}"#,
        sect(r#"<w:headerReference w:type="default" r:id="rH"/><w:footerReference w:type="default" r:id="rF"/>"#)
    );
    let data = docx_with(
        &body,
        &[
            ("word/header1.xml", header.as_str()),
            ("word/footer1.xml", footer.as_str()),
            ("word/_rels/document.xml.rels", r.as_str()),
        ],
    );
    let page = render_text_page(&data, 0, &PreviewPolicy::default()).unwrap();
    assert_eq!((page.width, page.height), (792.0, 612.0), "Letter, landscape");
    let all = text_at(&page);
    let find = |t: &str| all.iter().find(|(s, ..)| s.contains(t)).cloned().expect(t);
    let (_, bx, by, _) = find("Body text");
    assert!((bx - 72.0).abs() < 0.5, "left margin of one inch, got {bx}");
    let (_, _, hy, _) = find("Running head");
    let (_, _, fy, _) = find("Running foot");
    let (_, nx, ny, nw) = find("1");
    assert!(hy < 72.0 && hy < by, "the header sits in the top margin ({hy})");
    assert!(fy > 612.0 - 72.0, "the footer sits in the bottom margin ({fy})");
    assert!((nx + nw - (792.0 - 72.0)).abs() < 1.0 && (ny - fy).abs() < 1.0, "number at the right of the footer line");
}

#[test]
fn a_page_without_footer_gets_its_number_centred_and_each_page_its_own() {
    let body: String = (0..120)
        .map(|i| format!("<w:p><w:r><w:t>Line number {i} of a long document.</w:t></w:r></w:p>"))
        .collect();
    let data = docx(&body);
    let p0 = render_text_page(&data, 0, &PreviewPolicy::default()).unwrap();
    let p1 = render_text_page(&data, 1, &PreviewPolicy::default()).unwrap();
    let number = |p: &PageModel| {
        text_at(p).into_iter().filter(|(_, _, y, _)| *y > 842.0 - 56.0).map(|(t, x, ..)| (t, x)).collect::<Vec<_>>()
    };
    let n0 = number(&p0);
    let n1 = number(&p1);
    assert_eq!(n0.len(), 1);
    assert_eq!((n0[0].0.as_str(), n1[0].0.as_str()), ("1", "2"));
    assert!((n0[0].1 - 297.5).abs() < 8.0, "centred, got {}", n0[0].1);
}

#[test]
fn text_runs_down_the_first_column_and_on_into_the_second() {
    let body: String = (0..60)
        .map(|i| format!("<w:p><w:r><w:t>Row {i} in the columns.</w:t></w:r></w:p>"))
        .collect();
    let two = format!("{body}{}", sect(r#"<w:cols w:num="2" w:space="720"/>"#).replace(r#" w:orient="landscape""#, ""));
    let one = format!("{body}{}", sect("").replace(r#" w:orient="landscape""#, ""));
    let policy = PreviewPolicy::default();
    let p_two = render_text_page(&docx(&two), 0, &policy).unwrap();
    let p_one = render_text_page(&docx(&one), 0, &policy).unwrap();
    assert!(p_two.pages < p_one.pages, "two columns hold more per page: {} vs {}", p_two.pages, p_one.pages);
    let xs: Vec<f32> = text_at(&p_two).into_iter().filter(|(t, ..)| t.starts_with("Row")).map(|(_, x, ..)| x).collect();
    let right = xs.iter().copied().fold(0.0f32, f32::max);
    let left = xs.iter().copied().fold(f32::MAX, f32::min);
    // 612 pt sheet, 72 pt margins, 36 pt gap: columns of 216 pt.
    assert!((left - 72.0).abs() < 0.5 && (right - (72.0 + 216.0 + 36.0)).abs() < 0.5, "{left} {right}");
}

#[test]
fn tab_stops_align_text_and_fill_with_leader_dots() {
    let body = r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="8000"/></w:tabs></w:pPr><w:r><w:t>Chapter one</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>7</w:t></w:r></w:p>
        <w:p><w:r><w:t>a</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>b</w:t></w:r></w:p>"#;
    let page = doc(body);
    let all = text_at(&page);
    let dots = all.iter().find(|(t, ..)| t.starts_with("..")).expect("a leader");
    let seven = all.iter().find(|(t, ..)| t == "7").expect("the number");
    // The right stop is 400 pt from the left edge of the text (56 pt): the 7 ends there.
    assert!((seven.1 + seven.3 - (MARGIN + 400.0)).abs() < 1.0, "ends at {}", seven.1 + seven.3);
    assert!(dots.1 > MARGIN + 60.0 && dots.1 + dots.3 <= seven.1 + 0.5);
    // Without stops a tab goes to the next half inch.
    let b = all.iter().find(|(t, ..)| t == "b").expect("b");
    assert!((b.1 - (MARGIN + 36.0)).abs() < 0.5, "b at {}", b.1);
}

#[test]
fn a_picture_is_drawn_at_the_size_the_document_gives_it() {
    let r = rels(&[("rId1", "image", "media/a.png")]);
    let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="2540000" cy="1270000"/><wp:docPr id="1" name="Pic"/><a:graphic><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let data = docx_with(body, &[("word/media/a.png", "abc"), ("word/_rels/document.xml.rels", r.as_str())]);
    let page = render_text_page(&data, 0, &PreviewPolicy::default()).unwrap();
    let img = page.prims.iter().find_map(|p| match p {
        Prim::Image { m, id, .. } => Some((m[0], m[3], *id)),
        _ => None,
    });
    let (w, h, id) = img.expect("a picture");
    assert!((w - 200.0).abs() < 0.5 && (h - 100.0).abs() < 0.5, "{w}x{h}");
    assert_ne!(id, crate::model::IMAGE_NONE, "it can be asked for");
}
