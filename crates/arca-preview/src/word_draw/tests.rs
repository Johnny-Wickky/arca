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
