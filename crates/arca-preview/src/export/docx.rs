//! A new `.docx` from the body of a legacy Word document or an RTF file.

use super::{LegacyOptions, Package, Tally, esc, runs_of};
use crate::PreviewError;
use crate::model::{
    Align, Block, ImageInfo, ParaStyle, Paragraph, Preview, SectionKind, Table, style,
};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

fn styles_xml() -> String {
    let heading = |n: u32, size: u32| {
        format!(
            r#"<w:style w:type="paragraph" w:styleId="Heading{n}"><w:name w:val="heading {n}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="80"/><w:outlineLvl w:val="{}"/></w:pPr><w:rPr><w:b/><w:sz w:val="{size}"/></w:rPr></w:style>"#,
            n - 1
        )
    };
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {W}><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri" w:eastAsia="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="264" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>"#
    );
    for (n, size) in [(1, 36), (2, 30), (3, 26), (4, 24), (5, 22), (6, 22)] {
        s.push_str(&heading(n, size));
    }
    s.push_str(r#"<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="240"/></w:pPr><w:rPr><w:b/><w:sz w:val="52"/></w:rPr></w:style>"#);
    s.push_str(r#"<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:rPr><w:color w:val="595959"/><w:sz w:val="28"/></w:rPr></w:style>"#);
    s.push_str(r#"<w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="Quote"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:ind w:left="720"/></w:pPr><w:rPr><w:i/></w:rPr></w:style>"#);
    s.push_str(r#"<w:style w:type="paragraph" w:styleId="Note"><w:name w:val="Note"/><w:basedOn w:val="Normal"/><w:qFormat/><w:rPr><w:color w:val="595959"/><w:sz w:val="20"/></w:rPr></w:style>"#);
    s.push_str(r#"<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>"#);
    s.push_str(r#"<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style>"#);
    s.push_str("</w:styles>");
    s
}

/// Text of a run as WordprocessingML: tabs and line breaks become elements.
fn text_elements(text: &str) -> String {
    let mut out = String::new();
    let mut piece = String::new();
    let flush = |piece: &mut String, out: &mut String| {
        if !piece.is_empty() {
            out.push_str(&format!(r#"<w:t xml:space="preserve">{}</w:t>"#, esc(piece)));
            piece.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\t' => {
                flush(&mut piece, &mut out);
                out.push_str("<w:tab/>");
            }
            '\n' => {
                flush(&mut piece, &mut out);
                out.push_str("<w:br/>");
            }
            c => piece.push(c),
        }
    }
    flush(&mut piece, &mut out);
    out
}

fn run_xml(text: &str, st: u16, extra_italic: bool) -> String {
    let mut props = String::new();
    if st & style::MONO != 0 {
        props.push_str(
            r#"<w:rFonts w:ascii="Courier New" w:hAnsi="Courier New" w:cs="Courier New"/>"#,
        );
    }
    if st & style::BOLD != 0 {
        props.push_str("<w:b/>");
    }
    if st & style::ITALIC != 0 || extra_italic {
        props.push_str("<w:i/>");
    }
    if st & style::STRIKE != 0 {
        props.push_str("<w:strike/>");
    }
    if st & style::UNDERLINE != 0 {
        props.push_str(r#"<w:u w:val="single"/>"#);
    }
    if st & style::SUPER != 0 {
        props.push_str(r#"<w:vertAlign w:val="superscript"/>"#);
    } else if st & style::SUB != 0 {
        props.push_str(r#"<w:vertAlign w:val="subscript"/>"#);
    }
    let rpr = if props.is_empty() { String::new() } else { format!("<w:rPr>{props}</w:rPr>") };
    format!("<w:r>{rpr}{}</w:r>", text_elements(text))
}

fn style_id(s: ParaStyle) -> Option<&'static str> {
    Some(match s {
        ParaStyle::Title => "Title",
        ParaStyle::Subtitle => "Subtitle",
        ParaStyle::Heading1 => "Heading1",
        ParaStyle::Heading2 => "Heading2",
        ParaStyle::Heading3 => "Heading3",
        ParaStyle::Heading4 => "Heading4",
        ParaStyle::Heading5 => "Heading5",
        ParaStyle::Heading6 => "Heading6",
        ParaStyle::Quote => "Quote",
        ParaStyle::Note | ParaStyle::Comment | ParaStyle::Footnote | ParaStyle::HeaderFooter => {
            "Note"
        }
        ParaStyle::Body | ParaStyle::Code => return None,
    })
}

fn paragraph_xml(p: &Paragraph, opts: &LegacyOptions, tally: &mut Tally) -> String {
    let (runs, links) = runs_of(p, opts);
    tally.links += links;
    let mut ppr = String::new();
    if let Some(id) = style_id(p.style) {
        ppr.push_str(&format!(r#"<w:pStyle w:val="{id}"/>"#));
    }
    if p.level > 0 || !p.marker.is_empty() {
        let left = 360 + 360 * u32::from(p.level.max(1));
        ppr.push_str(&format!(r#"<w:ind w:left="{left}" w:hanging="360"/>"#));
    }
    match p.align {
        Align::Center => ppr.push_str(r#"<w:jc w:val="center"/>"#),
        Align::End => ppr.push_str(r#"<w:jc w:val="right"/>"#),
        Align::Justify => ppr.push_str(r#"<w:jc w:val="both"/>"#),
        Align::Start => {}
    }
    let mono = p.style == ParaStyle::Code;
    let mut body = String::new();
    if !p.marker.is_empty() {
        body.push_str(&run_xml(&format!("{}\t", p.marker), 0, false));
    }
    for (text, st) in &runs {
        body.push_str(&run_xml(text, if mono { st | style::MONO } else { *st }, false));
    }
    let ppr = if ppr.is_empty() { String::new() } else { format!("<w:pPr>{ppr}</w:pPr>") };
    format!("<w:p>{ppr}{body}</w:p>")
}

fn table_xml(t: &Table, opts: &LegacyOptions, tally: &mut Tally) -> String {
    let columns = t
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| usize::from(c.span.max(1))).sum::<usize>())
        .max()
        .unwrap_or(1)
        .clamp(1, 63);
    let width = 9000 / columns;
    let mut out = String::from(
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid>"#,
    );
    for _ in 0..columns {
        out.push_str(&format!(r#"<w:gridCol w:w="{width}"/>"#));
    }
    out.push_str("</w:tblGrid>");
    for row in &t.rows {
        out.push_str("<w:tr>");
        for cell in &row.cells {
            let span = usize::from(cell.span.max(1)).min(columns);
            let span_xml =
                if span > 1 { format!(r#"<w:gridSpan w:val="{span}"/>"#) } else { String::new() };
            out.push_str(&format!(
                r#"<w:tc><w:tcPr><w:tcW w:w="{}" w:type="dxa"/>{span_xml}</w:tcPr>"#,
                width * span
            ));
            let mut any = false;
            for p in &cell.paras {
                any = true;
                let mut p = p.clone();
                if cell.header {
                    for r in &mut p.runs {
                        r.style |= style::BOLD;
                    }
                }
                out.push_str(&paragraph_xml(&p, opts, tally));
            }
            if !any {
                out.push_str("<w:p/>");
            }
            out.push_str("</w:tc>");
        }
        out.push_str("</w:tr>");
    }
    // A table needs a paragraph after it; it also keeps neighbouring tables apart.
    out.push_str("</w:tbl><w:p/>");
    out
}

fn image_xml(i: &ImageInfo, tally: &mut Tally) -> String {
    tally.pictures += 1;
    let what = if i.alt.trim().is_empty() { i.media.clone() } else { i.alt.clone() };
    let label = if what.is_empty() { "[picture]".to_owned() } else { format!("[picture: {what}]") };
    format!("<w:p>{}</w:p>", run_xml(&label, 0, true))
}

fn blocks_xml(blocks: &[Block], opts: &LegacyOptions, tally: &mut Tally, out: &mut String) {
    for b in blocks {
        match b {
            Block::Para(p) => out.push_str(&paragraph_xml(p, opts, tally)),
            Block::Table(t) => out.push_str(&table_xml(t, opts, tally)),
            Block::Image(i) => out.push_str(&image_xml(i, tally)),
            Block::Rule => out.push_str(r#"<w:p><w:r><w:br w:type="page"/></w:r></w:p>"#),
            // A spreadsheet grid in a text document does not occur; its rows would be lost.
            Block::Grid(g) => {
                for row in &g.rows {
                    let text: Vec<&str> = row.cells.iter().map(|c| c.text.as_str()).collect();
                    out.push_str(&paragraph_xml(&Paragraph::plain(&text.join("\t")), opts, tally));
                }
            }
        }
    }
}

/// Writes the package.
pub(super) fn write(
    p: &Preview,
    opts: &LegacyOptions,
    tally: &mut Tally,
) -> Result<Vec<u8>, PreviewError> {
    let mut body = String::new();
    // The body first, then notes, headers and (unless removed) comments under their own headings.
    let order = [
        (SectionKind::Body, ""),
        (SectionKind::Page, ""),
        (SectionKind::Notes, "Notes"),
        (SectionKind::Headers, "Headers and footers"),
        (SectionKind::Comments, "Comments"),
    ];
    for (kind, title) in order {
        if kind == SectionKind::Comments && opts.remove_comments {
            continue;
        }
        for sec in p.sections.iter().filter(|s| s.kind == kind) {
            if !title.is_empty() && !sec.blocks.is_empty() {
                let name = if sec.name.is_empty() { title } else { sec.name.as_str() };
                let h = Paragraph { style: ParaStyle::Heading2, ..Paragraph::plain(name) };
                body.push_str(&paragraph_xml(&h, opts, tally));
            }
            blocks_xml(&sec.blocks, opts, tally, &mut body);
        }
    }
    if body.is_empty() {
        body.push_str("<w:p/>");
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="709" w:footer="709" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let mut pkg = Package::new();
    pkg.add("[Content_Types].xml", CONTENT_TYPES)?;
    pkg.add("_rels/.rels", ROOT_RELS)?;
    pkg.add("word/document.xml", &document)?;
    pkg.add("word/_rels/document.xml.rels", DOC_RELS)?;
    pkg.add("word/styles.xml", &styles_xml())?;
    pkg.finish()
}
