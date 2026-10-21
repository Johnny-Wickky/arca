//! A new `.pptx` from the slides of a legacy PowerPoint presentation.
//!
//! Every slide gets a title and a text body from the standard layout; the slide's text keeps its
//! order, levels and character formatting. Tables (which the legacy reader gives as paragraphs
//! anyway), pictures and speaker notes become text lines.

use super::{LegacyOptions, Package, Tally, esc, runs_of};
use crate::PreviewError;
use crate::model::{Block, ParaStyle, Paragraph, Preview, SectionKind, style};

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;

const GROUP: &str = r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#;

fn rels(items: &[(String, &str, String)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (id, ty, target) in items {
        s.push_str(&format!(
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{ty}" Target="{target}"/>"#
        ));
    }
    s.push_str("</Relationships>");
    s
}

fn placeholder(id: u32, name: &str, ph: &str, [x, y, cx, cy]: [u32; 4], body: &str) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph {ph}/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/>{body}</p:txBody></p:sp>"#
    )
}

fn master() -> String {
    let title = placeholder(
        2,
        "Title Placeholder 1",
        r#"type="title""#,
        [457_200, 274_638, 8_229_600, 1_143_000],
        "<a:p><a:endParaRPr lang=\"en-US\"/></a:p>",
    );
    let body = placeholder(
        3,
        "Text Placeholder 2",
        r#"type="body" idx="1""#,
        [457_200, 1_600_200, 8_229_600, 4_525_963],
        "<a:p><a:endParaRPr lang=\"en-US\"/></a:p>",
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldMaster {NS}><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>{GROUP}{title}{body}</p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:defRPr sz="3200" b="1"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mj-lt"/></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr marL="342900" indent="-342900"><a:buFont typeface="Arial"/><a:buChar char="&#8226;"/><a:defRPr sz="2400"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl1pPr><a:lvl2pPr marL="742950" indent="-285750"><a:buFont typeface="Arial"/><a:buChar char="&#8211;"/><a:defRPr sz="2000"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl2pPr><a:lvl3pPr marL="1143000" indent="-228600"><a:buFont typeface="Arial"/><a:buChar char="&#8226;"/><a:defRPr sz="1800"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl3pPr></p:bodyStyle><p:otherStyle><a:defPPr><a:defRPr lang="en-US"/></a:defPPr></p:otherStyle></p:txStyles></p:sldMaster>"#
    )
}

fn layout() -> String {
    let title = placeholder(
        2,
        "Title 1",
        r#"type="title""#,
        [457_200, 274_638, 8_229_600, 1_143_000],
        "<a:p><a:endParaRPr lang=\"en-US\"/></a:p>",
    );
    let body = placeholder(
        3,
        "Content Placeholder 2",
        r#"idx="1""#,
        [457_200, 1_600_200, 8_229_600, 4_525_963],
        "<a:p><a:endParaRPr lang=\"en-US\"/></a:p>",
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldLayout {NS} type="obj" preserve="1"><p:cSld name="Title and Content"><p:spTree>{GROUP}{title}{body}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#
    )
}

fn theme() -> String {
    let fill = r#"<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#;
    let line = |w: u32| {
        format!(r#"<a:ln w="{w}"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>"#)
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2><a:accent1><a:srgbClr val="4F81BD"/></a:accent1><a:accent2><a:srgbClr val="C0504D"/></a:accent2><a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4><a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6><a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst>{fill}{fill}{fill}</a:fillStyleLst><a:lnStyleLst>{}{}{}</a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst>{fill}{fill}{fill}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
        line(9525),
        line(25400),
        line(38100)
    )
}

/// A DrawingML paragraph.
fn para_xml(p: &Paragraph, opts: &LegacyOptions, tally: &mut Tally, italic: bool) -> String {
    let (runs, links) = runs_of(p, opts);
    tally.links += links;
    let lvl = u32::from(p.level.min(8));
    let ppr = if lvl > 0 { format!(r#"<a:pPr lvl="{lvl}"/>"#) } else { String::new() };
    let mut out = format!("<a:p>{ppr}");
    let mut run = |text: &str, st: u16| {
        // A line break inside a paragraph is an element of its own.
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                out.push_str("<a:br><a:rPr lang=\"en-US\"/></a:br>");
            }
            if part.is_empty() {
                continue;
            }
            let mut attrs = String::from(r#" lang="en-US""#);
            if st & style::BOLD != 0 {
                attrs.push_str(r#" b="1""#);
            }
            if st & style::ITALIC != 0 || italic {
                attrs.push_str(r#" i="1""#);
            }
            if st & style::UNDERLINE != 0 {
                attrs.push_str(r#" u="sng""#);
            }
            if st & style::STRIKE != 0 {
                attrs.push_str(r#" strike="sngStrike""#);
            }
            if st & style::SUPER != 0 {
                attrs.push_str(r#" baseline="30000""#);
            } else if st & style::SUB != 0 {
                attrs.push_str(r#" baseline="-25000""#);
            }
            out.push_str(&format!("<a:r><a:rPr{attrs}/><a:t>{}</a:t></a:r>", esc(part)));
        }
    };
    if !p.marker.is_empty() && p.marker != "•" {
        run(&format!("{} ", p.marker), 0);
    }
    for (text, st) in &runs {
        run(text, *st);
    }
    out.push_str("</a:p>");
    out
}

struct Slide {
    title: String,
    body: String,
}

fn slide_of(
    sec: &crate::model::Section,
    number: usize,
    opts: &LegacyOptions,
    tally: &mut Tally,
) -> Slide {
    let mut title: Option<String> = None;
    let mut first_text = true;
    let mut body = String::new();
    let mut notes = String::new();
    for b in &sec.blocks {
        match b {
            Block::Para(p) => {
                // The title is the first line of text if it is marked as one or is the name the
                // reader gave the slide (legacy files do not mark titles in the text).
                let is_title = first_text
                    && !p.is_blank()
                    && (matches!(p.style, ParaStyle::Title | ParaStyle::Heading1)
                        || p.text().trim() == sec.name.trim());
                if !p.is_blank() && !matches!(p.style, ParaStyle::Note | ParaStyle::Comment) {
                    first_text = false;
                }
                if is_title && title.is_none() {
                    title = Some(p.text().trim().to_owned());
                } else if matches!(p.style, ParaStyle::Note | ParaStyle::Comment) {
                    if !(opts.remove_comments && p.style == ParaStyle::Comment) {
                        notes.push_str(&para_xml(p, opts, tally, true));
                    }
                } else {
                    body.push_str(&para_xml(p, opts, tally, false));
                }
            }
            Block::Table(t) => {
                for row in &t.rows {
                    let cells: Vec<String> = row
                        .cells
                        .iter()
                        .map(|c| c.paras.iter().map(Paragraph::text).collect::<Vec<_>>().join(" "))
                        .collect();
                    body.push_str(&para_xml(
                        &Paragraph::plain(&cells.join(" │ ")),
                        opts,
                        tally,
                        false,
                    ));
                }
            }
            Block::Image(i) => {
                tally.pictures += 1;
                let what = if i.alt.trim().is_empty() { i.media.clone() } else { i.alt.clone() };
                let label = if what.is_empty() {
                    "[picture]".to_owned()
                } else {
                    format!("[picture: {what}]")
                };
                body.push_str(&para_xml(&Paragraph::plain(&label), opts, tally, true));
            }
            Block::Rule | Block::Grid(_) => {}
        }
    }
    if !notes.is_empty() {
        body.push_str(&para_xml(&Paragraph::plain("Speaker notes:"), opts, tally, true));
        body.push_str(&notes);
    }
    let title = title.filter(|t| !t.is_empty()).unwrap_or_else(|| {
        if sec.name.trim().is_empty() {
            format!("Slide {number}")
        } else {
            sec.name.trim().to_owned()
        }
    });
    Slide { title, body }
}

/// Writes the package.
pub(super) fn write(
    p: &Preview,
    opts: &LegacyOptions,
    tally: &mut Tally,
) -> Result<Vec<u8>, PreviewError> {
    // Slides first; whatever else the reader listed (hyperlinks, comments) follows on slides of
    // its own, named after the list.
    let slides: Vec<Slide> = p
        .sections
        .iter()
        .filter(|s| match s.kind {
            SectionKind::Slide => true,
            SectionKind::Comments => !opts.remove_comments && !s.blocks.is_empty(),
            SectionKind::Notes => !s.blocks.is_empty(),
            _ => false,
        })
        .enumerate()
        .map(|(i, s)| slide_of(s, i + 1, opts, tally))
        .collect();
    // A presentation needs at least one slide to be a presentation.
    let slides = if slides.is_empty() {
        vec![Slide { title: "Slide 1".into(), body: String::new() }]
    } else {
        slides
    };

    let mut content_types = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#,
    );
    let mut pres_rels =
        vec![("rId1".to_owned(), "slideMaster", "slideMasters/slideMaster1.xml".to_owned())];
    let mut id_list = String::new();
    for i in 0..slides.len() {
        let n = i + 1;
        content_types.push_str(&format!(
            r#"<Override PartName="/ppt/slides/slide{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#
        ));
        pres_rels.push((format!("rId{}", n + 1), "slide", format!("slides/slide{n}.xml")));
        id_list.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + n, n + 1));
    }
    pres_rels.push((format!("rId{}", slides.len() + 2), "theme", "theme/theme1.xml".to_owned()));
    content_types.push_str("</Types>");
    let presentation = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation {NS}><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{id_list}</p:sldIdLst><p:sldSz cx="9144000" cy="6858000" type="screen4x3"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    );

    let mut pkg = Package::new();
    pkg.add("[Content_Types].xml", &content_types)?;
    pkg.add("_rels/.rels", ROOT_RELS)?;
    pkg.add("ppt/presentation.xml", &presentation)?;
    pkg.add("ppt/_rels/presentation.xml.rels", &rels(&pres_rels))?;
    pkg.add("ppt/slideMasters/slideMaster1.xml", &master())?;
    pkg.add(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        &rels(&[
            ("rId1".into(), "slideLayout", "../slideLayouts/slideLayout1.xml".into()),
            ("rId2".into(), "theme", "../theme/theme1.xml".into()),
        ]),
    )?;
    pkg.add("ppt/slideLayouts/slideLayout1.xml", &layout())?;
    pkg.add(
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        &rels(&[("rId1".into(), "slideMaster", "../slideMasters/slideMaster1.xml".into())]),
    )?;
    pkg.add("ppt/theme/theme1.xml", &theme())?;
    for (i, s) in slides.iter().enumerate() {
        let n = i + 1;
        let title = placeholder(
            2,
            "Title 1",
            r#"type="title""#,
            [457_200, 274_638, 8_229_600, 1_143_000],
            &format!(r#"<a:p><a:r><a:rPr lang="en-US"/><a:t>{}</a:t></a:r></a:p>"#, esc(&s.title)),
        );
        let body_text = if s.body.is_empty() {
            r#"<a:p><a:endParaRPr lang="en-US"/></a:p>"#.to_owned()
        } else {
            s.body.clone()
        };
        let body = placeholder(
            3,
            "Content Placeholder 2",
            r#"idx="1""#,
            [457_200, 1_600_200, 8_229_600, 4_525_963],
            &body_text,
        );
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld {NS}><p:cSld><p:spTree>{GROUP}{title}{body}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#
        );
        pkg.add(&format!("ppt/slides/slide{n}.xml"), &xml)?;
        pkg.add(
            &format!("ppt/slides/_rels/slide{n}.xml.rels"),
            &rels(&[("rId1".into(), "slideLayout", "../slideLayouts/slideLayout1.xml".into())]),
        )?;
    }
    pkg.finish()
}
