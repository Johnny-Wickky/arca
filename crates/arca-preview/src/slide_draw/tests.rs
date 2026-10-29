//! Tests of the slide drawing: synthetic decks built with the test kit, and the real fixtures.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::too_many_lines)]

use super::*;
use crate::page::{MAX_PRIMS, op};
use crate::testkit::{Lcg, fuzz_iterations, opc, rels, zip};

const P_NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;
const PPTX_CT: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const GROUP_HEAD: &str =
    r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>"#;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Points to EMU.
fn e(pt: f64) -> i64 {
    (pt * 12_700.0).round() as i64
}

/// A deck under construction.
struct Deck {
    slides: Vec<String>,
    layout: String,
    master: String,
    clr_map: String,
    theme_colors: String,
    layout_rels: String,
    master_rels: String,
    slide_rels: String,
    master_bg: String,
    slide_bg: String,
    size: (String, String),
}

impl Deck {
    fn new(slides: &[&str]) -> Self {
        Self {
            slides: slides.iter().map(|s| (*s).to_owned()).collect(),
            layout: String::new(),
            master: String::new(),
            clr_map: r#"<p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>"#.to_owned(),
            theme_colors: r#"<a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2><a:accent1><a:srgbClr val="4F81BD"/></a:accent1><a:accent2><a:srgbClr val="C0504D"/></a:accent2><a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4><a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6><a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink>"#.to_owned(),
            layout_rels: rels(&[("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")]),
            master_rels: rels(&[
                ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                ("rId2", "theme", "../theme/theme1.xml"),
            ]),
            slide_rels: rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")]),
            master_bg: String::new(),
            slide_bg: String::new(),
            size: ("9144000".to_owned(), "6858000".to_owned()),
        }
    }

    fn build(&self, extra: &[(&str, &str)]) -> Vec<u8> {
        let n = self.slides.len();
        let ids: String = (1..=n)
            .map(|i| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + i, i + 10))
            .collect();
        let pres = format!(
            r#"<p:presentation {P_NS}><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx="{}" cy="{}"/></p:presentation>"#,
            self.size.0, self.size.1
        );
        let rl: Vec<(String, String)> =
            (1..=n).map(|i| (format!("rId{}", i + 10), format!("slides/slide{i}.xml"))).collect();
        let mut rl_ref: Vec<(&str, &str, &str)> =
            rl.iter().map(|(a, b)| (a.as_str(), "slide", b.as_str())).collect();
        rl_ref.push(("rId1", "slideMaster", "slideMasters/slideMaster1.xml"));
        let pres_rels = rels(&rl_ref);
        let slide_docs: Vec<String> = self
            .slides
            .iter()
            .map(|body| {
                let hidden = if body.starts_with("HIDDEN") { r#" show="0""# } else { "" };
                let body = body.trim_start_matches("HIDDEN");
                format!(
                    r#"<p:sld {P_NS}{hidden}><p:cSld>{}<p:spTree>{GROUP_HEAD}{body}</p:spTree></p:cSld></p:sld>"#,
                    self.slide_bg
                )
            })
            .collect();
        let layout = format!(
            r#"<p:sldLayout {P_NS}><p:cSld><p:spTree>{GROUP_HEAD}{}</p:spTree></p:cSld></p:sldLayout>"#,
            self.layout
        );
        let master = format!(
            r#"<p:sldMaster {P_NS}><p:cSld>{}<p:spTree>{GROUP_HEAD}{}</p:spTree></p:cSld>{}</p:sldMaster>"#,
            self.master_bg, self.master, self.clr_map
        );
        let theme = format!(
            r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="T"><a:themeElements><a:clrScheme name="C">{}</a:clrScheme></a:themeElements></a:theme>"#,
            self.theme_colors
        );
        let names: Vec<String> = (1..=n).map(|i| format!("ppt/slides/slide{i}.xml")).collect();
        let rel_names: Vec<String> =
            (1..=n).map(|i| format!("ppt/slides/_rels/slide{i}.xml.rels")).collect();
        let mut parts: Vec<(&str, &str)> = vec![
            ("ppt/presentation.xml", pres.as_str()),
            ("ppt/_rels/presentation.xml.rels", pres_rels.as_str()),
            ("ppt/slideLayouts/slideLayout1.xml", layout.as_str()),
            ("ppt/slideLayouts/_rels/slideLayout1.xml.rels", self.layout_rels.as_str()),
            ("ppt/slideMasters/slideMaster1.xml", master.as_str()),
            ("ppt/slideMasters/_rels/slideMaster1.xml.rels", self.master_rels.as_str()),
            ("ppt/theme/theme1.xml", theme.as_str()),
        ];
        for i in 0..n {
            parts.push((names[i].as_str(), slide_docs[i].as_str()));
            parts.push((rel_names[i].as_str(), self.slide_rels.as_str()));
        }
        parts.extend_from_slice(extra);
        opc(("ppt/presentation.xml", PPTX_CT), &parts)
    }
}

/// A shape with a frame in points, a preset and extra `spPr` content.
fn sp(x: f64, y: f64, w: f64, h: f64, prst: &str, extra: &str, text: &str) -> String {
    let tx = if text.is_empty() {
        String::new()
    } else {
        format!(r#"<p:txBody><a:bodyPr/><a:lstStyle/>{text}</p:txBody>"#)
    };
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="s"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm><a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom>{extra}</p:spPr>{tx}</p:sp>"#,
        e(x),
        e(y),
        e(w),
        e(h)
    )
}

const RED: &str = r#"<a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>"#;

fn para(text: &str) -> String {
    format!(r#"<a:p><a:r><a:rPr lang="en-US"/><a:t>{text}</a:t></a:r></a:p>"#)
}

fn render(data: &[u8], i: usize) -> PageModel {
    match render_slide(data, i, &PreviewPolicy::default()) {
        Ok(p) => p,
        Err(err) => panic!("slide {i}: {err}"),
    }
}

/// A policy that accepts the highly repetitive parts the hostile tests build.
fn lax() -> PreviewPolicy {
    let mut p = PreviewPolicy::default();
    p.zip.max_ratio = 1_000_000;
    p.aggregate_ratio = 1_000_000;
    p
}

fn noted(data: &[u8], i: usize) -> Rendered {
    render_slide_noted(data, i, &PreviewPolicy::default()).expect("renders")
}

/// The paths of a page as `(fill, stroke, points)`; points are the move/line end points and curve
/// end points in order.
type PathInfo = (u32, u32, Vec<(f32, f32)>);

fn paths(p: &PageModel) -> Vec<PathInfo> {
    p.prims
        .iter()
        .filter_map(|pr| match pr {
            Prim::Path { ops, fill, stroke, .. } => {
                let mut pts = Vec::new();
                let mut i = 0;
                while i < ops.len() {
                    let (n, take) = match ops[i] {
                        c if c == op::MOVE || c == op::LINE => (3, 1),
                        c if c == op::CURVE => (7, 3),
                        _ => (1, 0),
                    };
                    if take > 0 {
                        pts.push((ops[i + n - 2], ops[i + n - 1]));
                    }
                    i += n;
                }
                Some((*fill, *stroke, pts))
            }
            _ => None,
        })
        .collect()
}

fn texts(p: &PageModel) -> Vec<(String, f32, f32, f32, u8, u32)> {
    p.prims
        .iter()
        .filter_map(|pr| match pr {
            Prim::Text { text, x, y, size, style, color, .. } => {
                Some((text.clone(), *x, *y, *size, *style, *color))
            }
            _ => None,
        })
        .collect()
}

fn near(a: f32, b: f64) -> bool {
    (f64::from(a) - b).abs() < 0.01
}

/// Every number of the page is finite, and the page respects the primitive bound.
fn assert_sane(p: &PageModel) {
    assert!(p.width.is_finite() && p.height.is_finite() && p.width > 0.0 && p.height > 0.0);
    assert!(p.prims.len() <= MAX_PRIMS);
    for pr in &p.prims {
        match pr {
            Prim::Path { ops, width, .. } => {
                assert!(ops.iter().all(|v| v.is_finite()) && width.is_finite());
            }
            Prim::Text { x, y, size, angle, width, .. } => {
                assert!([x, y, size, angle, width].iter().all(|v| v.is_finite()));
            }
            Prim::Image { m, .. } => assert!(m.iter().all(|v| v.is_finite())),
            Prim::Clip { x, y, w, h } => assert!([x, y, w, h].iter().all(|v| v.is_finite())),
            _ => {}
        }
    }
}

/// The bounding box of everything drawn except the page clip, as `(x0, y0, x1, y1)`.
fn bounds(p: &PageModel) -> (f32, f32, f32, f32) {
    let mut b = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut add = |x: f32, y: f32| b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
    for (_, _, pts) in paths(p) {
        for (x, y) in pts {
            add(x, y);
        }
    }
    for t in texts(p) {
        add(t.1, t.2);
    }
    for pr in &p.prims {
        if let Prim::Image { m, .. } = pr {
            for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                add(m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]);
            }
        }
    }
    b
}

// ---- the real fixtures ---------------------------------------------------------------------

#[test]
fn every_slide_of_every_fixture_renders_inside_its_page() {
    for (name, slides, text_min) in
        [("slides.pptx", 2, 5), ("deck-rich.pptx", 6, 20), ("slides.odp", 2, 7)]
    {
        let data = fixture(name);
        let policy = PreviewPolicy::default();
        assert_eq!(slide_count(&data, &policy).unwrap(), slides, "{name}");
        let mut text_total = 0;
        for i in 0..slides {
            let r = noted(&data, i);
            let p = &r.page;
            assert_sane(p);
            assert_eq!((p.index as usize, p.pages as usize), (i, slides), "{name}");
            assert!(!p.truncated, "{name} {i}");
            assert!(p.prims.len() > 2, "{name} {i}: something is drawn");
            let (x0, y0, x1, y1) = bounds(p);
            assert!(
                x0 >= -1.0 && y0 >= -1.0 && x1 <= p.width + 1.0 && y1 <= p.height + 1.0,
                "{name} {i}: {x0} {y0} {x1} {y1} in {}x{}",
                p.width,
                p.height
            );
            text_total += texts(p).len();
            // The page survives the wire.
            let bytes = arca_abi::wire::to_bytes(p).unwrap();
            let back: PageModel =
                arca_abi::wire::from_bytes(&bytes, arca_abi::wire::DecodeLimits::default())
                    .unwrap();
            assert_eq!(&back, p);
        }
        assert!(text_total >= text_min, "{name}: {text_total} text runs");
    }
}

#[test]
fn the_fixture_slides_have_their_text_and_sizes() {
    let data = fixture("slides.pptx");
    let p = render(&data, 0);
    // 10080625 × 5670550 EMU.
    assert!(
        (f64::from(p.width) - 793.75).abs() < 0.01 && (f64::from(p.height) - 446.5).abs() < 0.01
    );
    let t = texts(&p);
    assert!(t.iter().any(|t| t.0 == "Заголовок первого слайда"));
    assert!(t.iter().any(|t| t.0 == "Пункт один") && t.iter().any(|t| t.0 == "Пункт два"));
    // The two text boxes are filled with #729fcf.
    assert_eq!(paths(&p).iter().filter(|p| p.0 == 0xFF72_9FCF).count(), 2);

    let odp = render(&fixture("slides.odp"), 0);
    assert!((f64::from(odp.width) - 28.0 * 72.0 / 2.54).abs() < 0.01);
    assert!(texts(&odp).iter().any(|t| t.0 == "Пункт один"));

    let deck = fixture("deck-rich.pptx");
    let title = render(&deck, 0);
    assert!(texts(&title).iter().any(|t| t.0 == "Квартальный обзор 2026"));
    // The title placeholder is centred on a 720 pt wide page: its position comes from the master.
    let t = texts(&title).into_iter().find(|t| t.0 == "Квартальный обзор 2026").unwrap();
    assert!(t.1 > 100.0 && t.1 < 300.0, "x = {}", t.1);
    // Slide 4 is a table of 3 × 3 cells with a styled header row.
    let table = render(&deck, 3);
    assert!(texts(&table).iter().filter(|t| t.0.starts_with('R')).count() == 9);
    assert!(paths(&table).iter().any(|p| p.0 == 0xFF4F_81BD), "header fill");
    // Slide 6 holds a picture: a placeholder with the id of its member.
    let pic = render(&deck, 5);
    let ids: Vec<u32> = pic
        .prims
        .iter()
        .filter_map(|p| if let Prim::Image { id, .. } = p { Some(*id) } else { None })
        .collect();
    assert_eq!(ids.len(), 1);
    assert_ne!(ids[0], crate::model::IMAGE_NONE);
}

#[test]
fn other_formats_and_missing_slides_are_refused() {
    let policy = PreviewPolicy::default();
    assert!(matches!(
        render_slide(&fixture("report.docx"), 0, &policy),
        Err(PreviewError::Unsupported(_))
    ));
    assert!(matches!(
        render_slide(&fixture("slides.ppt"), 0, &policy),
        Err(PreviewError::Unsupported(_))
    ));
    for f in ["slides.pptx", "slides.odp"] {
        assert!(matches!(render_slide(&fixture(f), 2, &policy), Err(PreviewError::Malformed(_))));
    }
    assert!(matches!(
        render_slide(&fixture("slides.pptx"), usize::MAX, &policy),
        Err(PreviewError::Malformed(_))
    ));
}

// ---- geometry ------------------------------------------------------------------------------

#[test]
fn rotation_and_flip_move_the_corners_where_a_viewer_does() {
    // A 100 × 50 rectangle at (100, 100): its centre is (150, 125).
    let plain = sp(100.0, 100.0, 100.0, 50.0, "rect", RED, "");
    let rot = plain.replace("<a:xfrm>", r#"<a:xfrm rot="5400000">"#);
    let flip_h = plain.replace("<a:xfrm>", r#"<a:xfrm flipH="1">"#);
    let flip_v = plain.replace("<a:xfrm>", r#"<a:xfrm flipV="1">"#);
    let both = plain.replace("<a:xfrm>", r#"<a:xfrm rot="10800000" flipH="1">"#);
    let data = Deck::new(&[&plain, &rot, &flip_h, &flip_v, &both]).build(&[]);
    let first = |i: usize| paths(&render(&data, i))[0].2.clone();
    let corner = |pts: &[(f32, f32)], k: usize| (f64::from(pts[k].0), f64::from(pts[k].1));
    let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01;
    // Unrotated: (0,0) -> (100,100), (w,0) -> (200,100).
    let p0 = first(0);
    assert!(close(corner(&p0, 0), (100.0, 100.0)) && close(corner(&p0, 1), (200.0, 100.0)));
    // A quarter turn clockwise about the centre: the top left corner goes to the top right, at
    // (175, 75); the top right corner to the bottom right, (175, 175).
    let p1 = first(1);
    assert!(close(corner(&p1, 0), (175.0, 75.0)), "{:?}", p1[0]);
    assert!(close(corner(&p1, 1), (175.0, 175.0)), "{:?}", p1[1]);
    // Flipped horizontally: the first corner is now at the right.
    let p2 = first(2);
    assert!(close(corner(&p2, 0), (200.0, 100.0)) && close(corner(&p2, 1), (100.0, 100.0)));
    // Flipped vertically: the first corner is at the bottom left.
    let p3 = first(3);
    assert!(close(corner(&p3, 0), (100.0, 150.0)) && close(corner(&p3, 2), (200.0, 100.0)));
    // Half a turn and a flip is a vertical flip.
    let p4 = first(4);
    assert!(close(corner(&p4, 0), (100.0, 150.0)), "{:?}", p4[0]);
}

#[test]
fn groups_compose_their_transforms() {
    // A group at (100, 100), 200 × 200, whose children live in a 100 × 100 space: scale 2.
    let child = sp(10.0, 10.0, 20.0, 20.0, "rect", RED, "");
    let group = format!(
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="3" name="g"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/><a:chOff x="0" y="0"/><a:chExt cx="{}" cy="{}"/></a:xfrm></p:grpSpPr>{child}</p:grpSp>"#,
        e(100.0),
        e(100.0),
        e(200.0),
        e(200.0),
        e(100.0),
        e(100.0)
    );
    let data = Deck::new(&[&group]).build(&[]);
    let pts = &paths(&render(&data, 0))[0].2;
    assert!(near(pts[0].0, 120.0) && near(pts[0].1, 120.0), "{:?}", pts[0]);
    assert!(near(pts[2].0, 160.0) && near(pts[2].1, 160.0), "{:?}", pts[2]);

    // A group inside a group: scale 2 then a shift of (50, 0), with a non-zero child origin.
    let inner = format!(
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="4" name="i"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="{}" y="0"/><a:ext cx="{}" cy="{}"/><a:chOff x="{}" y="{}"/><a:chExt cx="{}" cy="{}"/></a:xfrm></p:grpSpPr>{child}</p:grpSp>"#,
        e(50.0),
        e(50.0),
        e(50.0),
        e(10.0),
        e(10.0),
        e(25.0),
        e(25.0)
    );
    let outer = group.replace(&child, &inner);
    let data = Deck::new(&[&outer]).build(&[]);
    let pts = &paths(&render(&data, 0))[0].2;
    // Inner: child (10,10) -> ((10-10)*2+50, (10-10)*2+0) = (50, 0); outer: x2+100 -> (200, 100).
    assert!(near(pts[0].0, 200.0) && near(pts[0].1, 100.0), "{:?}", pts[0]);
    // The child is 20 wide: inner scale 2 (20 -> 40), outer scale 2 (-> 80).
    assert!(near(pts[1].0, 280.0), "{:?}", pts[1]);
}

#[test]
fn custom_geometry_is_scaled_by_its_own_path_size() {
    let g = r#"<a:custGeom><a:avLst/><a:pathLst><a:path w="100" h="100"><a:moveTo><a:pt x="0" y="0"/></a:moveTo><a:lnTo><a:pt x="100" y="0"/></a:lnTo><a:cubicBezTo><a:pt x="100" y="50"/><a:pt x="50" y="100"/><a:pt x="0" y="100"/></a:cubicBezTo><a:close/></a:path></a:pathLst></a:custGeom>"#;
    let shape = sp(10.0, 20.0, 200.0, 200.0, "rect", RED, "")
        .replace(r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#, g);
    let data = Deck::new(&[&shape]).build(&[]);
    let r = noted(&data, 0);
    let p = &paths(&r.page)[0];
    assert_eq!(p.0, 0xFFFF_0000);
    // moveTo, lnTo, curve end, in points: (10,20), (210,20), (10,220).
    assert!(near(p.2[0].0, 10.0) && near(p.2[0].1, 20.0));
    assert!(near(p.2[1].0, 210.0) && near(p.2[1].1, 20.0));
    assert!(near(p.2[2].0, 10.0) && near(p.2[2].1, 220.0));
    assert!(!r.page.approximate);
    // Guide names in a path are not evaluated, and the page says so.
    let guided = g.replace(r#"<a:pt x="100" y="0"/>"#, r#"<a:pt x="r" y="0"/>"#);
    let shape = sp(10.0, 20.0, 200.0, 200.0, "rect", RED, "")
        .replace(r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#, &guided);
    let r = noted(&Deck::new(&[&shape]).build(&[]), 0);
    assert!(r.page.approximate);
}

#[test]
fn presets_stay_inside_their_frames_and_unknown_ones_are_outlined() {
    let names = [
        "rect",
        "roundRect",
        "ellipse",
        "triangle",
        "rtTriangle",
        "diamond",
        "parallelogram",
        "trapezoid",
        "hexagon",
        "octagon",
        "pentagon",
        "star5",
        "homePlate",
        "chevron",
        "rightArrow",
        "leftArrow",
        "upArrow",
        "downArrow",
        "plus",
        "line",
        "straightConnector1",
        "bentConnector3",
        "flowChartProcess",
        "flowChartDecision",
        "flowChartTerminator",
    ];
    let body: String = names.iter().map(|n| sp(100.0, 100.0, 300.0, 200.0, n, RED, "")).collect();
    let r = noted(&Deck::new(&[&body]).build(&[]), 0);
    assert!(!r.page.approximate);
    let (x0, y0, x1, y1) = bounds(&r.page);
    assert!(x0 >= 99.99 && y0 >= 99.99 && x1 <= 400.01 && y1 <= 300.01, "{x0} {y0} {x1} {y1}");

    let odd = sp(100.0, 100.0, 300.0, 200.0, "cloudCallout", RED, "");
    let r = noted(&Deck::new(&[&odd]).build(&[]), 0);
    assert!(r.page.approximate);
    let p = &paths(&r.page);
    assert_eq!(p.len(), 1);
    // The frame is outlined, not filled.
    assert_eq!(p[0].0, 0);
    assert_ne!(p[0].1, 0);
    assert_eq!(p[0].2.len(), 4);
}

// ---- colours and styles --------------------------------------------------------------------

#[test]
fn theme_colours_resolve_through_the_colour_map_and_transforms() {
    let fill = |c: &str| format!("<a:solidFill>{c}</a:solidFill>");
    let accent = fill(r#"<a:schemeClr val="accent1"/>"#);
    let darker = fill(r#"<a:schemeClr val="accent1"><a:lumMod val="50000"/></a:schemeClr>"#);
    let lighter = fill(
        r#"<a:schemeClr val="accent1"><a:lumMod val="40000"/><a:lumOff val="60000"/></a:schemeClr>"#,
    );
    let tx1 = fill(r#"<a:schemeClr val="tx1"/>"#);
    let bg2 = fill(r#"<a:schemeClr val="bg2"/>"#);
    let alpha = fill(r#"<a:srgbClr val="FF0000"><a:alpha val="50000"/></a:srgbClr>"#);
    let red_half = fill(r#"<a:srgbClr val="FF0000"><a:lumMod val="50000"/></a:srgbClr>"#);
    let sys = fill(r#"<a:sysClr val="window" lastClr="FFFFFF"/>"#);
    let body: String = [&accent, &darker, &lighter, &tx1, &bg2, &alpha, &red_half, &sys]
        .iter()
        .map(|f| sp(0.0, 0.0, 10.0, 10.0, "rect", f, ""))
        .collect();
    let mut d = Deck::new(&[&body]);
    let data = d.build(&[]);
    let fills: Vec<u32> = paths(&render(&data, 0)).iter().map(|p| p.0).collect();
    let chan = |c: u32, shift: u32| i64::from((c >> shift) & 255);
    let close = |c: u32, want: u32| {
        [16, 8, 0].iter().all(|s| (chan(c, *s) - chan(want, *s)).abs() <= 1)
            && c >> 24 == want >> 24
    };
    assert_eq!(fills[0], 0xFF4F_81BD);
    // lumMod 50%: the lightness of 4F81BD (about 0.52) halved, hue and saturation kept.
    assert!(
        close(fills[1], 0xFF26_4062) || (chan(fills[1], 0) < chan(fills[0], 0)),
        "{:08x}",
        fills[1]
    );
    assert!(chan(fills[1], 16) < chan(fills[0], 16) && chan(fills[1], 8) < chan(fills[0], 8));
    // 40% of the lightness plus 60%: lighter than the accent.
    assert!(chan(fills[2], 16) > chan(fills[0], 16) && chan(fills[2], 0) > chan(fills[0], 0));
    assert_eq!(fills[3], 0xFF00_0000, "tx1 is dk1");
    assert_eq!(fills[4], 0xFFEE_ECE1, "bg2 is lt2");
    assert_eq!(fills[5], 0x80FF_0000);
    assert!(close(fills[6], 0xFF80_0000), "{:08x}", fills[6]);
    assert_eq!(fills[7], 0xFFFF_FFFF);

    // A dark colour map swaps the roles.
    d.clr_map = r#"<p:clrMap bg1="dk1" tx1="lt1" bg2="dk2" tx2="lt2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>"#.to_owned();
    let fills: Vec<u32> = paths(&render(&d.build(&[]), 0)).iter().map(|p| p.0).collect();
    assert_eq!(fills[3], 0xFFFF_FFFF, "tx1 is lt1 now");
    assert_eq!(fills[4], 0xFF1F_497D, "bg2 is dk2 now");
}

#[test]
fn styles_gradients_and_dashes() {
    // A shape with only a style: the fill and the line come from the theme references.
    let styled = sp(0.0, 0.0, 50.0, 50.0, "rect", "", "").replace(
        "</p:spPr>",
        r#"</p:spPr><p:style><a:lnRef idx="0"><a:schemeClr val="accent1"/></a:lnRef><a:fillRef idx="1"><a:schemeClr val="accent2"/></a:fillRef><a:effectRef idx="0"><a:schemeClr val="accent1"/></a:effectRef><a:fontRef idx="minor"><a:schemeClr val="lt1"/></a:fontRef></p:style>"#,
    );
    let theme_fill = r#"<a:fmtScheme name="x"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst></a:fmtScheme>"#;
    let mut d = Deck::new(&[&styled]);
    d.theme_colors.push_str("</a:clrScheme>");
    d.theme_colors.push_str(theme_fill);
    d.theme_colors.push_str("<a:clrScheme name=\"unused\">");
    let r = noted(&d.build(&[]), 0);
    assert_eq!(paths(&r.page)[0].0, 0xFFC0_504D);

    let grad = r#"<a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="00FF00"/></a:gs><a:gs pos="100000"><a:srgbClr val="0000FF"/></a:gs></a:gsLst></a:gradFill>"#;
    let r = noted(&Deck::new(&[&sp(0.0, 0.0, 9.0, 9.0, "rect", grad, "")]).build(&[]), 0);
    assert_eq!(paths(&r.page)[0].0, 0xFF00_FF00);
    assert!(r.page.approximate && r.notes.iter().any(|n| n.contains("gradient")));

    let dashed = r#"<a:ln w="25400"><a:solidFill><a:srgbClr val="0000FF"/></a:solidFill><a:prstDash val="dash"/></a:ln>"#;
    let r = noted(&Deck::new(&[&sp(0.0, 0.0, 9.0, 9.0, "rect", dashed, "")]).build(&[]), 0);
    assert!(r.page.approximate && r.notes.iter().any(|n| n.contains("dashed")));
    match &r.page.prims[2] {
        Prim::Path { stroke, width, fill, .. } => {
            assert_eq!((*stroke, *fill), (0xFF00_00FF, 0));
            assert!((width - 2.0).abs() < 1e-6);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn slide_backgrounds_come_from_the_master() {
    let mut d = Deck::new(&[&sp(0.0, 0.0, 9.0, 9.0, "rect", RED, "")]);
    // No background: the shape is the only path.
    assert_eq!(paths(&render(&d.build(&[]), 0)).len(), 1);
    d.master_bg =
        r#"<p:bg><p:bgPr><a:solidFill><a:srgbClr val="112233"/></a:solidFill></p:bgPr></p:bg>"#
            .to_owned();
    let p = render(&d.build(&[]), 0);
    let ps = paths(&p);
    assert_eq!(ps.len(), 2);
    assert_eq!(ps[0].0, 0xFF11_2233);
    assert!(near(ps[0].2[2].0, 720.0) && near(ps[0].2[2].1, 540.0));
}

/// The pictures of a page as `(matrix, id)`, in drawing order.
fn images(p: &PageModel) -> Vec<([f32; 6], u32)> {
    p.prims
        .iter()
        .filter_map(|pr| match pr {
            Prim::Image { m, id, .. } => Some((*m, *id)),
            _ => None,
        })
        .collect()
}

const PIC_BG: &str = r#"<p:bg><p:bgPr><a:blipFill dpi="0" rotWithShape="1"><a:blip r:embed="rId7"/><a:srcRect/><a:stretch><a:fillRect/></a:stretch></a:blipFill><a:effectLst/></p:bgPr></p:bg>"#;

fn picture_rels(extra: &[(&str, &str, &str)]) -> String {
    let mut all = vec![("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")];
    all.extend_from_slice(extra);
    rels(&all)
}

#[test]
fn a_picture_background_of_the_slide_is_drawn_under_the_shapes() {
    let mut d = Deck::new(&[&sp(0.0, 0.0, 9.0, 9.0, "rect", RED, "")]);
    d.slide_bg = PIC_BG.to_owned();
    d.slide_rels = picture_rels(&[("rId7", "image", "../media/back.png")]);
    let r = noted(&d.build(&[("ppt/media/back.png", "PNG!")]), 0);
    let imgs = images(&r.page);
    assert_eq!(imgs.len(), 1);
    assert_ne!(imgs[0].1, crate::model::IMAGE_NONE);
    // It covers the whole slide (the unit square maps to 0..720 × 0..540, the picture's bottom
    // left corner being the unit origin) ...
    let m = imgs[0].0;
    assert!(
        near(m[0], 720.0) && near(m[3], -540.0) && near(m[4], 0.0) && near(m[5], 540.0),
        "{m:?}"
    );
    // ... and comes before the shape, so the shape stays on top of it.
    let first_path = r.page.prims.iter().position(|p| matches!(p, Prim::Path { .. })).unwrap();
    let first_image = r.page.prims.iter().position(|p| matches!(p, Prim::Image { .. })).unwrap();
    assert!(first_image < first_path);
    assert!(!r.page.approximate, "a picture background is no longer approximated");
}

#[test]
fn a_picture_background_comes_from_the_master_and_a_missing_one_is_noted() {
    let mut d = Deck::new(&[&sp(0.0, 0.0, 9.0, 9.0, "rect", RED, "")]);
    d.master_bg = PIC_BG.to_owned();
    d.master_rels = rels(&[
        ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
        ("rId2", "theme", "../theme/theme1.xml"),
        ("rId7", "image", "../media/back.png"),
    ]);
    let with = noted(&d.build(&[("ppt/media/back.png", "PNG!")]), 0);
    let imgs = images(&with.page);
    assert_eq!(imgs.len(), 1);
    assert_ne!(imgs[0].1, crate::model::IMAGE_NONE);
    // The relationship names a picture that is not in the package: nothing to draw, and the page
    // says so.
    let without = noted(&d.build(&[]), 0);
    assert!(images(&without.page).is_empty());
    assert!(without.page.approximate);
}

#[test]
fn a_shape_filled_with_a_picture_draws_the_picture_in_its_frame() {
    let fill =
        r#"<a:blipFill><a:blip r:embed="rId7"/><a:stretch><a:fillRect/></a:stretch></a:blipFill>"#;
    let label = para("on top");
    let shape = sp(100.0, 50.0, 200.0, 100.0, "rect", fill, &label);
    let mut d = Deck::new(&[&shape]);
    d.slide_rels = picture_rels(&[("rId7", "image", "../media/fill.png")]);
    let r = noted(&d.build(&[("ppt/media/fill.png", "PNG!")]), 0);
    let imgs = images(&r.page);
    assert_eq!(imgs.len(), 1);
    let m = imgs[0].0;
    assert!(
        near(m[4], 100.0) && near(m[5], 150.0) && near(m[0], 200.0) && near(m[3], -100.0),
        "{m:?}"
    );
    assert!(!r.page.approximate);
    // The picture is under the text.
    let image_at = r.page.prims.iter().position(|p| matches!(p, Prim::Image { .. })).unwrap();
    let text_at = r.page.prims.iter().position(|p| matches!(p, Prim::Text { .. })).unwrap();
    assert!(image_at < text_at);
    // A picture of an ellipse is stretched over its frame, and the page says it is approximate.
    let round = sp(100.0, 50.0, 200.0, 100.0, "ellipse", fill, "");
    let mut d = Deck::new(&[&round]);
    d.slide_rels = picture_rels(&[("rId7", "image", "../media/fill.png")]);
    let r = noted(&d.build(&[("ppt/media/fill.png", "PNG!")]), 0);
    assert_eq!(images(&r.page).len(), 1);
    assert!(r.page.approximate);
}

// ---- text ----------------------------------------------------------------------------------

#[test]
fn text_is_placed_by_the_insets_and_styled_by_its_runs() {
    let body = para("Hello");
    let shape = sp(72.0, 72.0, 200.0, 100.0, "rect", "", &body);
    let p = render(&Deck::new(&[&shape]).build(&[]), 0);
    let t = texts(&p);
    assert_eq!(t.len(), 1);
    // Left inset 7.2 pt; baseline: top inset 3.6 + line pitch 21.6 - 0.2 * 18.
    assert!(near(t[0].1, 79.2) && near(t[0].2, 72.0 + 3.6 + 18.0), "{:?}", t[0]);
    assert!(near(t[0].3, 18.0));
    assert_eq!(t[0].5, 0xFF00_0000);

    let styled = r#"<a:p><a:pPr algn="ctr"/><a:r><a:rPr sz="2400" b="1" i="1"><a:solidFill><a:srgbClr val="00AA00"/></a:solidFill><a:latin typeface="Courier New"/></a:rPr><a:t>Mid</a:t></a:r></a:p>"#;
    let shape = sp(0.0, 0.0, 200.0, 100.0, "rect", "", styled);
    let t = texts(&render(&Deck::new(&[&shape]).build(&[]), 0));
    assert!(near(t[0].3, 24.0));
    assert_eq!(t[0].4, pstyle::BOLD | pstyle::ITALIC | pstyle::MONO);
    assert_eq!(t[0].5, 0xFF00_AA00);
    // Centred: 3 mono characters of 0.6 em at 24 pt = 43.2 wide in 185.6 of room.
    assert!(near(t[0].1, 7.2 + (185.6 - 43.2) / 2.0), "{}", t[0].1);
}

#[test]
fn text_wraps_by_estimated_widths_and_anchors() {
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
    let shape = sp(0.0, 0.0, 120.0, 400.0, "rect", "", &para(long));
    let t = texts(&render(&Deck::new(&[&shape]).build(&[]), 0));
    assert!(t.len() >= 4, "{} lines", t.len());
    assert!(t.windows(2).all(|w| w[1].2 > w[0].2), "lines go down");
    let joined: Vec<&str> = t.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(joined.join(" "), long);
    // No line is wider than the room (105.6 pt) by the estimate.
    let st = RunStyle::plain(18.0, 0xFF00_0000);
    assert!(t.iter().all(|t| text_width(&t.0, &st) <= 105.7), "{joined:?}");

    // With wrap off the text stays on one line.
    let nowrap = sp(0.0, 0.0, 120.0, 400.0, "rect", "", &para(long))
        .replace("<a:bodyPr/>", r#"<a:bodyPr wrap="none"/>"#);
    assert_eq!(texts(&render(&Deck::new(&[&nowrap]).build(&[]), 0)).len(), 1);

    // A word wider than the line is cut where the line ends.
    let word = sp(0.0, 0.0, 60.0, 400.0, "rect", "", &para("Supercalifragilisticexpialidocious"));
    let t = texts(&render(&Deck::new(&[&word]).build(&[]), 0));
    assert!(
        t.len() >= 3
            && t.iter().map(|t| t.0.as_str()).collect::<String>()
                == "Supercalifragilisticexpialidocious"
    );

    // Anchored at the bottom the baseline is near the bottom edge; centred it is in the middle.
    let bottom = sp(0.0, 0.0, 200.0, 100.0, "rect", "", &para("x"))
        .replace("<a:bodyPr/>", r#"<a:bodyPr anchor="b"/>"#);
    let ctr = sp(0.0, 0.0, 200.0, 100.0, "rect", "", &para("x"))
        .replace("<a:bodyPr/>", r#"<a:bodyPr anchor="ctr"/>"#);
    let y_of = |s: &str| texts(&render(&Deck::new(&[s]).build(&[]), 0))[0].2;
    assert!(near(y_of(&bottom), 92.8), "{}", y_of(&bottom));
    assert!(near(y_of(&ctr), 57.2), "{}", y_of(&ctr));
}

#[test]
fn text_is_cleaned_and_bullets_and_slide_numbers_are_drawn() {
    let evil = "a\u{202E}b\u{200B}c\u{E0041}d";
    let r = noted(&Deck::new(&[&sp(0.0, 0.0, 200.0, 50.0, "rect", "", &para(evil))]).build(&[]), 0);
    let t = texts(&r.page);
    assert_eq!(t[0].0, "abcd");
    assert!(r.page.approximate);

    let bullets = r#"<a:p><a:pPr marL="285750" indent="-285750"><a:buChar char="–"/></a:pPr><a:r><a:t>one</a:t></a:r></a:p><a:p><a:pPr marL="285750" indent="-285750"><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>two</a:t></a:r></a:p><a:p><a:pPr marL="285750" indent="-285750"><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>three</a:t></a:r></a:p><a:p><a:r><a:t>Slide </a:t></a:r><a:fld id="{1}" type="slidenum"><a:t>‹#›</a:t></a:fld></a:p>"#;
    let t = texts(&render(
        &Deck::new(&["", &sp(0.0, 0.0, 300.0, 200.0, "rect", "", bullets)]).build(&[]),
        1,
    ));
    let all: Vec<&str> = t.iter().map(|t| t.0.as_str()).collect();
    assert!(all.contains(&"–") && all.contains(&"1.") && all.contains(&"2."), "{all:?}");
    assert!(all.contains(&"Slide 2"), "{all:?}");
    // The hanging bullet sits left of the text.
    let bx = t.iter().find(|t| t.0 == "–").unwrap().1;
    let tx = t.iter().find(|t| t.0 == "one").unwrap().1;
    assert!(bx < tx);
}

#[test]
fn placeholders_take_position_and_text_style_from_layout_and_master() {
    let slide = r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Heading</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="b"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Point</a:t></a:r></a:p></p:txBody></p:sp>"#;
    let mut d = Deck::new(&[slide]);
    // The layout positions the title; the master positions the body and sets the text styles.
    d.layout = format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm></p:spPr><p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle/><a:p><a:r><a:t>Click to edit</a:t></a:r></a:p></p:txBody></p:sp>"#,
        e(50.0),
        e(20.0),
        e(600.0),
        e(80.0)
    );
    d.master = format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="b"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Master text</a:t></a:r></a:p></p:txBody></p:sp>"#,
        e(60.0),
        e(150.0),
        e(500.0),
        e(300.0)
    );
    let master_tx = r#"<p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:defRPr sz="4000" b="1"/></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr marL="342900" indent="-342900"><a:buChar char="•"/><a:defRPr sz="2800"/></a:lvl1pPr></p:bodyStyle></p:txStyles>"#;
    d.clr_map.push_str(master_tx);
    let r = noted(&d.build(&[]), 0);
    let t = texts(&r.page);
    // The prompt texts of the layout and the master are not drawn.
    assert!(t.iter().all(|t| t.0 != "Click to edit" && t.0 != "Master text"));
    let h = t.iter().find(|t| t.0 == "Heading").unwrap();
    assert!(near(h.1, 50.0 + 7.2), "{h:?}");
    assert!(near(h.3, 40.0) && h.4 == pstyle::BOLD, "{h:?}");
    assert!(h.2 > 20.0 && h.2 < 100.0, "centred in its 80 pt box: {h:?}");
    let b = t.iter().find(|t| t.0 == "Point").unwrap();
    assert!(near(b.3, 28.0));
    assert!(near(b.1, 60.0 + 7.2 + 27.0), "{b:?}");
    assert!(t.iter().any(|t| t.0 == "•"), "the bullet of the master");
    assert!(!r.page.approximate);
}

#[test]
fn tables_pictures_and_charts_become_cells_and_placeholders() {
    let tbl = |extra: &str| {
        format!(
            r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="t"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr/><a:tblGrid><a:gridCol w="{}"/><a:gridCol w="{}"/></a:tblGrid>{extra}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
            e(100.0),
            e(100.0),
            e(300.0),
            e(100.0),
            e(100.0),
            e(200.0)
        )
    };
    let cell = |t: &str, pr: &str| {
        format!(
            r#"<a:tc><a:txBody><a:bodyPr/><a:lstStyle/>{}</a:txBody><a:tcPr{pr}</a:tc>"#,
            para(t)
        )
    };
    let rows = format!(
        r#"<a:tr h="{}">{}{}</a:tr><a:tr h="{}">{}{}</a:tr>"#,
        e(30.0),
        cell("A1", &format!(r#">{RED}</a:tcPr>"#)),
        cell("B1", "/>"),
        e(30.0),
        cell("A2", "/>"),
        cell(
            "B2",
            r#"><a:lnB w="12700"><a:solidFill><a:srgbClr val="0000FF"/></a:solidFill></a:lnB></a:tcPr>"#
        )
    );
    let r = noted(&Deck::new(&[&tbl(&rows)]).build(&[]), 0);
    let t = texts(&r.page);
    assert_eq!(t.len(), 4);
    let a1 = t.iter().find(|t| t.0 == "A1").unwrap();
    let b1 = t.iter().find(|t| t.0 == "B1").unwrap();
    let a2 = t.iter().find(|t| t.0 == "A2").unwrap();
    assert!(near(a1.1, 107.2) && near(b1.1, 207.2), "{a1:?} {b1:?}");
    assert!(a2.2 > a1.2 + 25.0, "second row below the first");
    let p = paths(&r.page);
    assert_eq!(p.iter().filter(|p| p.0 == 0xFFFF_0000).count(), 1, "one filled cell");
    assert_eq!(p.iter().filter(|p| p.1 == 0xFF00_00FF).count(), 1, "one border");

    // A merged cell spans its columns.
    let merged = format!(
        r#"<a:tr h="{}"><a:tc gridSpan="2"><a:txBody><a:bodyPr/><a:lstStyle/>{}</a:txBody><a:tcPr>{RED}</a:tcPr></a:tc><a:tc hMerge="1"><a:txBody><a:bodyPr/><a:p/></a:txBody><a:tcPr/></a:tc></a:tr>"#,
        e(30.0),
        para("wide")
    );
    let p = paths(&render(&Deck::new(&[&tbl(&merged)]).build(&[]), 0));
    let f = p.iter().find(|p| p.0 == 0xFFFF_0000).unwrap();
    assert!(near(f.2[1].0, 400.0), "{:?}", f.2);

    // Pictures: the id of the member; a missing or external one has none. Charts and objects
    // are labelled placeholders.
    let pic = |embed: &str| {
        format!(
            r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="Pic" descr="A red square"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{embed}"/></p:blipFill><p:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm></p:spPr></p:pic>"#,
            e(10.0),
            e(20.0),
            e(100.0),
            e(50.0)
        )
    };
    let chart = r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="6" name="c"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="1270000" cy="635000"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"/></a:graphic></p:graphicFrame>"#;
    let body = format!("{}{}{chart}", pic("rId7"), pic("rId99"));
    let mut d = Deck::new(&[&body]);
    d.slide_rels = rels(&[
        ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
        ("rId7", "image", "../media/image1.png"),
    ]);
    let data_deck = d.build(&[("ppt/media/image1.png", "PNG!")]);
    let p = render(&data_deck, 0);
    let imgs: Vec<(&[f32; 6], &str, u32)> = p
        .prims
        .iter()
        .filter_map(|pr| match pr {
            Prim::Image { m, label, id } => Some((m, label.as_str(), *id)),
            _ => None,
        })
        .collect();
    assert_eq!(imgs.len(), 3);
    assert_ne!(imgs[0].2, crate::model::IMAGE_NONE);
    assert_eq!(imgs[0].1, "A red square");
    assert_eq!(imgs[1].2, crate::model::IMAGE_NONE);
    assert_eq!((imgs[2].1, imgs[2].2), ("chart", crate::model::IMAGE_NONE));
    // The unit square maps to the frame: bottom left of the picture is the unit origin.
    let m = imgs[0].0;
    assert!(
        near(m[4], 10.0) && near(m[5], 70.0) && near(m[0], 100.0) && near(m[3], -50.0),
        "{m:?}"
    );
}

/// A copy of a package with one member's content replaced.
fn replace_part(pkg: &[u8], name: &str, content: &str) -> Vec<u8> {
    let zip_in = arca_container::zip::ZipArchive::parse(pkg, PreviewPolicy::default().zip).unwrap();
    let mut budget = arca_container::InflateBudget::new(1 << 28, 1000, 16 << 20);
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in zip_in.entries() {
        let data = if entry.name() == name {
            content.as_bytes().to_vec()
        } else {
            zip_in.read(entry, &mut budget).unwrap()
        };
        members.push((entry.name().to_owned(), data));
    }
    let refs: Vec<(&str, &[u8])> =
        members.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    zip(&refs)
}

// ---- hidden content ------------------------------------------------------------------------

#[test]
fn smartart_is_drawn_from_its_cached_drawing() {
    let frame = format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="d"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" r:dm="rId5" r:lo="rId6" r:qs="rId7" r:cs="rId8"/></a:graphicData></a:graphic></p:graphicFrame>"#,
        e(100.0),
        e(50.0),
        e(300.0),
        e(200.0)
    );
    let mut deck = Deck::new(&[frame.as_str()]);
    deck.slide_rels = rels(&[
        ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
        ("rId5", "diagramData", "../diagrams/data1.xml"),
        ("rId9", "diagramDrawing", "../diagrams/drawing1.xml"),
    ]);
    let shape = |x: f64, y: f64, text: &str| {
        format!(
            r#"<dsp:sp modelId="{{1}}"><dsp:nvSpPr><dsp:cNvPr id="0" name=""/><dsp:cNvSpPr/></dsp:nvSpPr><dsp:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></dsp:spPr><dsp:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr sz="1400"/><a:t>{text}</a:t></a:r></a:p></dsp:txBody></dsp:sp>"#,
            e(x),
            e(y),
            e(120.0),
            e(60.0)
        )
    };
    let drawing = format!(
        r#"<dsp:drawing xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dsp:spTree><dsp:nvGrpSpPr><dsp:cNvPr id="0" name=""/><dsp:cNvGrpSpPr/></dsp:nvGrpSpPr><dsp:grpSpPr/>{}{}</dsp:spTree></dsp:drawing>"#,
        shape(0.0, 0.0, "Планирование"),
        shape(150.0, 100.0, "Execution")
    );
    let data = build_diagram_data();
    let pkg = deck.build(&[
        ("ppt/diagrams/data1.xml", data.as_str()),
        ("ppt/diagrams/drawing1.xml", drawing.as_str()),
    ]);
    let p = render(&pkg, 0);
    let t = texts(&p);
    let find = |s: &str| t.iter().find(|(text, ..)| text.contains(s)).cloned();
    let first = find("Планирование").expect("first node text drawn");
    let second = find("Execution").expect("second node text drawn");
    // The nodes sit where the drawing puts them, relative to the frame: 100 pt and 50 pt in.
    assert!(first.1 >= 100.0 - 1.0 && first.2 >= 50.0 - 1.0, "{first:?}");
    assert!(second.1 > first.1 && second.2 > first.2, "{first:?} {second:?}");
    assert_sane(&p);
}

fn build_diagram_data() -> String {
    r#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:ptLst/></dgm:dataModel>"#
        .to_owned()
}

#[test]
fn hidden_slides_and_shapes_follow_the_policy() {
    let shown = sp(0.0, 0.0, 10.0, 10.0, "rect", RED, "");
    let hidden_shape =
        sp(20.0, 0.0, 10.0, 10.0, "rect", RED, "").replace(r#"name="s""#, r#"name="h" hidden="1""#);
    let both = format!("{shown}{hidden_shape}");
    let data = Deck::new(&[&both, &format!("HIDDEN{shown}")]).build(&[]);
    let reveal = PreviewPolicy::default();
    let conceal = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
    assert_eq!(slide_count(&data, &reveal).unwrap(), 2);
    assert_eq!(slide_count(&data, &conceal).unwrap(), 1);
    // Revealed, the hidden shape is drawn faded; concealed, it is left out.
    let p = render_slide(&data, 0, &reveal).unwrap();
    let fills: Vec<u32> = paths(&p).iter().map(|p| p.0).collect();
    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0], 0xFFFF_0000);
    assert!(fills[1] >> 24 < 0xFF && fills[1] >> 24 > 0, "{:08x}", fills[1]);
    let p = render_slide(&data, 0, &conceal).unwrap();
    assert_eq!(paths(&p).len(), 1);
    assert_eq!(p.pages, 1);
    assert!(matches!(render_slide(&data, 1, &conceal), Err(PreviewError::Malformed(_))));
}

// ---- hostile input -------------------------------------------------------------------------

#[test]
fn a_hundred_thousand_shapes_are_bounded() {
    let one = r#"<p:sp><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12700" cy="12700"/></a:xfrm><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></p:spPr></p:sp>"#;
    let body = one.repeat(100_000);
    let data = Deck::new(&[&body]).build(&[]);
    let p = render_slide(&data, 0, &lax()).unwrap();
    assert_sane(&p);
    assert!(p.truncated, "the page says it was cut");
    assert!(p.prims.len() <= MAX_PRIMS);
}

#[test]
fn path_numbers_are_bounded_per_primitive_and_per_page() {
    let segs: Vec<Seg> = (0..100_000).map(|i| Seg::L(f64::from(i), 1.0)).collect();
    let mut cv = Canvas::new();
    cv.path(&segs, &Mat::ID, 0xFF00_0000, 0, 1.0, false);
    assert!(cv.full, "one path of 300000 numbers is cut");
    match cv.prims.first() {
        Some(Prim::Path { ops, .. }) => assert!(ops.len() <= MAX_PATH_NUMBERS),
        other => panic!("{other:?}"),
    }
    let mut cv = Canvas::new();
    let segs: Vec<Seg> = (0..60_000).map(|i| Seg::L(f64::from(i), 1.0)).collect();
    for _ in 0..40 {
        cv.path(&segs, &Mat::ID, 0xFF00_0000, 0, 1.0, false);
    }
    let total: usize =
        cv.prims.iter().map(|p| if let Prim::Path { ops, .. } = p { ops.len() } else { 0 }).sum();
    assert!(cv.full && total <= MAX_PAGE_NUMBERS);
}

#[test]
fn deep_groups_stop_at_the_limit() {
    let leaf = sp(0.0, 0.0, 10.0, 10.0, "rect", RED, "");
    let nest = |depth: usize| {
        let mut s = leaf.clone();
        for _ in 0..depth {
            s = format!(
                r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="3" name="g"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{s}</p:grpSp>"#
            );
        }
        s
    };
    let ok = render(&Deck::new(&[&nest(5)]).build(&[]), 0);
    assert_eq!(paths(&ok).len(), 1);
    assert!(!ok.truncated);
    let deep = render(&Deck::new(&[&nest(45)]).build(&[]), 0);
    assert_sane(&deep);
    assert!(deep.truncated);
    assert_eq!(paths(&deep).len(), 0, "the leaf is below the limit");
    // Deeper than the XML nesting limit: the slide cannot be read at all, and that is an error.
    let beyond = Deck::new(&[&nest(80)]).build(&[]);
    let _ = render_slide(&beyond, 0, &PreviewPolicy::default());
}

#[test]
fn absurd_numbers_never_reach_the_page() {
    let weird = [
        ("9223372036854775807", "9223372036854775807"),
        ("-9223372036854775808", "99999999999999999999999"),
        ("NaN", "inf"),
        ("1e999", "-1e999"),
        ("", "abc"),
        ("-1", "-1"),
    ];
    for (a, b) in weird {
        let shape = format!(
            r#"<p:sp><p:spPr><a:xfrm rot="{a}"><a:off x="{a}" y="{b}"/><a:ext cx="{b}" cy="{a}"/></a:xfrm>{RED}<a:ln w="{a}"><a:solidFill><a:srgbClr val="00FF00"/></a:solidFill></a:ln></p:spPr><p:txBody><a:bodyPr lIns="{a}" tIns="{b}"/><a:lstStyle/><a:p><a:pPr marL="{a}" indent="{b}"/><a:r><a:rPr sz="{a}"/><a:t>text</a:t></a:r></a:p></p:txBody></p:sp>"#
        );
        let mut d = Deck::new(&[&shape]);
        d.size = (a.to_owned(), b.to_owned());
        let r = render_slide_noted(&d.build(&[]), 0, &PreviewPolicy::default()).unwrap();
        assert_sane(&r.page);
        // A page size that is not believed is the default one, and the page says so.
        assert!(r.page.width <= MAX_SIDE as f32);
    }
    // The size of the page: huge or tiny values fall back.
    let mut d = Deck::new(&[&sp(0.0, 0.0, 10.0, 10.0, "rect", RED, "")]);
    d.size = ("999999999999".to_owned(), "1".to_owned());
    let r = noted(&d.build(&[]), 0);
    assert_eq!((r.page.width, r.page.height), (720.0, 540.0));
    assert!(r.page.approximate);
}

#[test]
fn cyclic_layout_and_master_relationships_terminate() {
    let shape = sp(0.0, 0.0, 10.0, 10.0, "rect", RED, "");
    let mut d = Deck::new(&[&shape]);
    // The layout names itself as its master; the master names the layout as its theme and itself
    // as a layout.
    d.layout_rels = rels(&[("rId1", "slideMaster", "slideLayout1.xml")]);
    d.master_rels =
        rels(&[("rId1", "slideLayout", "slideMaster1.xml"), ("rId2", "theme", "slideMaster1.xml")]);
    let p = render(&d.build(&[]), 0);
    assert_eq!(paths(&p).len(), 1);
    // The slide names itself as its layout.
    let data = Deck::new(&[&shape]).build(&[]);
    let data = replace_part(
        &data,
        "ppt/slides/_rels/slide1.xml.rels",
        &rels(&[("rId1", "slideLayout", "slide1.xml")]),
    );
    assert_eq!(paths(&render(&data, 0)).len(), 1);
    // A layout that is not a layout, and a missing master.
    let mut d = Deck::new(&[&shape]);
    d.layout_rels = rels(&[("rId1", "slideMaster", "../slides/slide1.xml")]);
    assert_eq!(paths(&render(&d.build(&[]), 0)).len(), 1);
}

#[test]
fn damaged_slides_are_drawn_as_far_as_they_go_and_never_panic() {
    let shape = sp(0.0, 0.0, 100.0, 50.0, "rect", RED, &para("Hi"));
    let data = Deck::new(&[&shape]).build(&[]);
    let d = Deck::new(&[&shape]);
    let good = d.slides[0].clone();
    let slide = format!(
        r#"<p:sld {P_NS}><p:cSld><p:spTree>{GROUP_HEAD}{good}</p:spTree></p:cSld></p:sld>"#
    );
    // Cut the slide in the middle of its second shape.
    let cut = format!("{}{}", slide.split("</p:spTree>").next().unwrap(), "<p:sp><p:spPr><a:xfrm>");
    let broken = replace_part(&data, "ppt/slides/slide1.xml", &cut);
    let r = noted(&broken, 0);
    assert_eq!(paths(&r.page).len(), 1);
    assert!(r.page.approximate);

    // Random damage to every part: an error or a page, never a panic.
    let mut rng = Lcg::new(7);
    for _ in 0..fuzz_iterations(300) {
        let mut b = slide.clone().into_bytes();
        let k = 1 + rng.below(6);
        rng.mutate(&mut b, k);
        if let Ok(s) = String::from_utf8(b) {
            let m = replace_part(&data, "ppt/slides/slide1.xml", &s);
            if let Ok(p) = render_slide(&m, 0, &PreviewPolicy::default()) {
                assert_sane(&p);
            }
        }
    }
    let mut rng = Lcg::new(11);
    for name in ["slides.pptx", "deck-rich.pptx", "slides.odp"] {
        let base = fixture(name);
        for _ in 0..fuzz_iterations(60) {
            let mut b = base.clone();
            let k = 1 + rng.below(8);
            rng.mutate(&mut b, k);
            for i in 0..2 {
                if let Ok(p) = render_slide(
                    &b,
                    i,
                    &PreviewPolicy { max_input_bytes: 1 << 26, ..PreviewPolicy::default() },
                ) {
                    assert_sane(&p);
                }
            }
        }
    }
}

// ---- OpenDocument --------------------------------------------------------------------------

const ODF_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0""#;

fn odp(auto: &str, pages: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0"?><office:document-content {ODF_NS}><office:automatic-styles>{auto}</office:automatic-styles><office:body><office:presentation>{pages}</office:presentation></office:body></office:document-content>"#
    );
    let styles = format!(
        r#"<?xml version="1.0"?><office:document-styles {ODF_NS}><office:automatic-styles><style:page-layout style:name="PM1"><style:page-layout-properties fo:page-width="20cm" fo:page-height="10cm"/></style:page-layout></office:automatic-styles><office:master-styles><style:master-page style:name="Default" style:page-layout-name="PM1"/></office:master-styles></office:document-styles>"#
    );
    let mut parts: Vec<(&str, &[u8])> = vec![
        ("mimetype", b"application/vnd.oasis.opendocument.presentation"),
        ("content.xml", content.as_bytes()),
        ("styles.xml", styles.as_bytes()),
    ];
    parts.extend_from_slice(extra);
    zip(&parts)
}

const CM: f64 = 72.0 / 2.54;

#[test]
fn odp_shapes_transforms_text_and_hidden_pages() {
    let auto = r##"<style:style style:name="gr1" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#00ff00" draw:stroke="solid" svg:stroke-color="#0000ff" svg:stroke-width="0.1cm"/></style:style><style:style style:name="T1" style:family="text"><style:text-properties fo:font-size="24pt" fo:font-weight="bold" fo:color="#ff0000"/></style:style><style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties presentation:visibility="hidden"/></style:style>"##;
    let pages = r#"<draw:page draw:name="p1" draw:master-page-name="Default"><draw:rect draw:style-name="gr1" svg:x="1cm" svg:y="1cm" svg:width="12cm" svg:height="2cm"><text:p>Plain <text:span text:style-name="T1">loud</text:span> text</text:p></draw:rect><draw:rect draw:style-name="gr1" svg:width="4cm" svg:height="2cm" draw:transform="rotate (1.5707963267949) translate (10cm 5cm)"/><draw:g><draw:ellipse draw:style-name="gr1" svg:x="0cm" svg:y="0cm" svg:width="2cm" svg:height="2cm"/><draw:line svg:x1="0cm" svg:y1="0cm" svg:x2="3cm" svg:y2="1cm"/></draw:g><draw:frame svg:x="2cm" svg:y="6cm" svg:width="5cm" svg:height="2cm"><draw:image xlink:href="Pictures/a.png"><text:p/></draw:image></draw:frame></draw:page><draw:page draw:name="p2" draw:style-name="dp2" draw:master-page-name="Default"><draw:rect svg:x="0cm" svg:y="0cm" svg:width="1cm" svg:height="1cm"/></draw:page>"#;
    let data = odp(auto, pages, &[("Pictures/a.png", b"PNG!")]);
    let reveal = PreviewPolicy::default();
    let conceal = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
    assert_eq!(slide_count(&data, &reveal).unwrap(), 2);
    assert_eq!(slide_count(&data, &conceal).unwrap(), 1);
    let r = render_slide_noted(&data, 0, &reveal).unwrap();
    let p = &r.page;
    assert_sane(p);
    assert!(near(p.width, 20.0 * CM) && near(p.height, 10.0 * CM));
    let ps = paths(p);
    // First rectangle: filled green, stroked blue, at (1cm, 1cm).
    assert_eq!((ps[0].0, ps[0].1), (0xFF00_FF00, 0xFF00_00FF));
    assert!(near(ps[0].2[0].0, CM) && near(ps[0].2[0].1, CM));
    assert!(near(ps[0].2[2].0, 13.0 * CM) && near(ps[0].2[2].1, 3.0 * CM));
    // Second: a quarter turn, then moved to (10cm, 5cm): the first corner lands there, the second
    // (the old right end of the top edge) 4cm further down.
    assert!(near(ps[1].2[0].0, 10.0 * CM) && near(ps[1].2[0].1, 5.0 * CM), "{:?}", ps[1].2[0]);
    assert!(near(ps[1].2[1].0, 10.0 * CM) && near(ps[1].2[1].1, 9.0 * CM), "{:?}", ps[1].2[1]);
    // The line has no fill, the ellipse is filled.
    assert!(ps.iter().any(|p| p.0 == 0 && p.1 == 0xFF00_0000 && p.2.len() == 2));
    // Text: three runs, the middle one big, bold and red.
    let t = texts(p);
    assert_eq!(t.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["Plain ", "loud", " text"]);
    assert!(near(t[1].3, 24.0) && t[1].4 == pstyle::BOLD && t[1].5 == 0xFFFF_0000);
    assert!(near(t[0].3, 18.0) && t[0].5 == 0xFF00_0000);
    // The picture: its member's number.
    let id = p
        .prims
        .iter()
        .find_map(|pr| if let Prim::Image { id, .. } = pr { Some(*id) } else { None });
    assert!(id.is_some_and(|i| i != crate::model::IMAGE_NONE));
    assert!(!r.page.approximate);
    // The hidden page is a page only when hidden content is shown.
    assert!(render_slide(&data, 1, &reveal).is_ok());
    assert!(matches!(render_slide(&data, 1, &conceal), Err(PreviewError::Malformed(_))));
}

#[test]
fn odp_tables_lists_and_paths() {
    let auto = r##"<style:style style:name="co1" style:family="table-column"><style:table-column-properties style:column-width="5cm"/></style:style><style:style style:name="ce1" style:family="table-cell"><style:table-cell-properties fo:background-color="#ff0000" fo:border="0.05pt solid #0000ff"/></style:style><text:list-style style:name="L1"><text:list-level-style-bullet text:level="1" text:bullet-char="•"><style:list-level-properties text:space-before="0cm" text:min-label-width="0.6cm"/></text:list-level-style-bullet><text:list-level-style-number text:level="2" style:num-format="1" style:num-suffix="."><style:list-level-properties text:space-before="0.6cm" text:min-label-width="0.6cm"/></text:list-level-style-number></text:list-style>"##;
    let pages = r#"<draw:page draw:master-page-name="Default"><draw:frame svg:x="1cm" svg:y="1cm" svg:width="10cm" svg:height="3cm"><table:table><table:table-column table:style-name="co1" table:number-columns-repeated="2"/><table:table-row><table:table-cell table:style-name="ce1"><text:p>A</text:p></table:table-cell><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row></table:table></draw:frame><draw:frame svg:x="1cm" svg:y="5cm" svg:width="10cm" svg:height="4cm"><draw:text-box><text:list text:style-name="L1"><text:list-item><text:p>top</text:p><text:list><text:list-item><text:p>sub</text:p></text:list-item></text:list></text:list-item></text:list></draw:text-box></draw:frame><draw:path svg:x="12cm" svg:y="1cm" svg:width="4cm" svg:height="4cm" svg:viewBox="0 0 100 100" svg:d="M 0 0 L 100 0 L 100 100 Z"/><draw:custom-shape svg:x="12cm" svg:y="5cm" svg:width="4cm" svg:height="4cm"><draw:enhanced-geometry draw:type="mystery"/></draw:custom-shape></draw:page>"#;
    let r = render_slide_noted(&odp(auto, pages, &[]), 0, &PreviewPolicy::default()).unwrap();
    let t = texts(&r.page);
    let by = |s: &str| t.iter().find(|t| t.0 == s).unwrap_or_else(|| panic!("{s}: {t:?}"));
    // Two columns of 5 cm: B starts 5 cm right of A.
    assert!((by("B").1 - by("A").1 - (5.0 * CM) as f32).abs() < 0.1);
    let ps = paths(&r.page);
    assert!(ps.iter().any(|p| p.0 == 0xFFFF_0000), "the red cell");
    assert!(ps.iter().any(|p| p.1 == 0xFF00_00FF), "its border");
    // Bullet and numbered sub-list.
    assert!(t.iter().any(|t| t.0 == "•") && t.iter().any(|t| t.0 == "1."));
    assert!(by("sub").1 > by("top").1, "the sub-list is indented");
    // The path (three corners and a close) and the unknown shape (outlined, and said so).
    assert!(ps.iter().any(|p| p.2.len() == 3));
    assert!(r.page.approximate);
    assert!(r.notes.iter().any(|n| n.contains("preset table")));
}

#[test]
fn odp_hostile_input_is_bounded() {
    let many = r#"<draw:rect svg:x="0cm" svg:y="0cm" svg:width="1cm" svg:height="1cm" draw:style-name="gr"/>"#.repeat(60_000);
    let auto = r##"<style:style style:name="gr" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#00ff00"/></style:style>"##;
    let p = render_slide(&odp(auto, &format!("<draw:page>{many}</draw:page>"), &[]), 0, &lax())
        .unwrap();
    assert_sane(&p);
    assert!(p.truncated);
    // Deep groups.
    let mut g = r#"<draw:rect svg:x="0cm" svg:y="0cm" svg:width="1cm" svg:height="1cm" draw:style-name="gr"/>"#.to_owned();
    for _ in 0..40 {
        g = format!("<draw:g>{g}</draw:g>");
    }
    let p = render(&odp(auto, &format!("<draw:page>{g}</draw:page>"), &[]), 0);
    assert!(p.truncated && paths(&p).is_empty());
    // Absurd numbers and a cycle of style parents.
    let auto = r##"<style:style style:name="a" style:family="graphic" style:parent-style-name="b"/><style:style style:name="b" style:family="graphic" style:parent-style-name="a"><style:graphic-properties draw:fill="solid" draw:fill-color="#00ff00" svg:stroke-width="1e999cm"/></style:style>"##;
    let weird = r#"<draw:page><draw:rect draw:style-name="a" svg:x="NaNcm" svg:y="99999999999cm" svg:width="1e999cm" svg:height="-5cm" draw:transform="rotate (inf) translate (NaN 1e400) scale (0)"><text:p>x</text:p></draw:rect><draw:polygon svg:width="1cm" svg:height="1cm" svg:viewBox="0 0 0 0" draw:points="1,2 3,4"/><draw:path svg:width="1cm" svg:height="1cm" svg:viewBox="0 0 10 10" svg:d="M1e999 -1e999 L NaN NaN C 1 2 3 Q"/></draw:page>"#;
    let p = render(&odp(auto, weird, &[]), 0);
    assert_sane(&p);
}
