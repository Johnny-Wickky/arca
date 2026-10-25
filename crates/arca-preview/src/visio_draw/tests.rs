//! Tests of the Visio page drawing: synthetic `.vsdx` packages built from the element and cell
//! names of the Visio SDK reference, with exact expected coordinates, and hostile inputs.
//!
//! The binary format has its own tests (`vsd/tests.rs`), built with a test-only chunk writer.

use super::{render_visio_page, render_visio_page_noted, visio_page_count};
use crate::page::{PageModel, Prim, op};
use crate::testkit::{opc, rels};
use crate::{PreviewError, PreviewPolicy};

const VSDX_CT: &str = "application/vnd.ms-visio.drawing.main+xml";
const NS: &str = r#"xmlns="http://schemas.microsoft.com/office/visio/2012/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

/// One page of a test drawing.
pub(super) struct TestPage<'a> {
    pub(super) attrs: &'a str,
    pub(super) sheet: &'a str,
    pub(super) shapes: &'a str,
    pub(super) connects: &'a str,
}

pub(super) fn page<'a>(shapes: &'a str) -> TestPage<'a> {
    TestPage { attrs: "", sheet: "", shapes, connects: "" }
}

/// A drawing: pages, extra content of `document.xml` (colours, style sheets) and masters
/// `(id, xml of the master's shapes)`.
pub(super) fn drawing(pages: &[TestPage<'_>], document: &str, masters: &[(u32, &str)]) -> Vec<u8> {
    let mut list = String::new();
    let mut page_rels = Vec::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        let n = i + 1;
        let sheet = if p.sheet.is_empty() {
            r#"<Cell N="PageWidth" V="8.5"/><Cell N="PageHeight" V="11"/>"#
        } else {
            p.sheet
        };
        list.push_str(&format!(
            r#"<Page ID="{n}" Name="P{n}" NameU="P{n}" {}><PageSheet>{sheet}</PageSheet><Rel r:id="rId{n}"/></Page>"#,
            p.attrs
        ));
        page_rels.push((format!("rId{n}"), format!("page{n}.xml")));
        parts.push((
            format!("visio/pages/page{n}.xml"),
            format!(
                r#"<PageContents {NS}><Shapes>{}</Shapes><Connects>{}</Connects></PageContents>"#,
                p.shapes, p.connects
            ),
        ));
    }
    parts.push((
        "visio/pages/pages.xml".into(),
        format!(r#"<Pages {NS}>{list}</Pages>"#),
    ));
    let pr: Vec<(&str, &str, &str)> =
        page_rels.iter().map(|(a, b)| (a.as_str(), "page", b.as_str())).collect();
    parts.push(("visio/pages/_rels/pages.xml.rels".into(), rels(&pr)));
    let mut doc_rels = vec![("rId1", "pages", "pages/pages.xml")];
    if !masters.is_empty() {
        doc_rels.push(("rId2", "masters", "masters/masters.xml"));
        let mut mlist = String::new();
        let mut mrels = Vec::new();
        for (id, xml) in masters {
            mlist.push_str(&format!(
                r#"<Master ID="{id}" NameU="M{id}"><Rel r:id="rId{id}"/></Master>"#
            ));
            mrels.push((format!("rId{id}"), format!("master{id}.xml")));
            parts.push((
                format!("visio/masters/master{id}.xml"),
                format!(r#"<MasterContents {NS}><Shapes>{xml}</Shapes></MasterContents>"#),
            ));
        }
        parts.push(("visio/masters/masters.xml".into(), format!(r#"<Masters {NS}>{mlist}</Masters>"#)));
        let mr: Vec<(&str, &str, &str)> =
            mrels.iter().map(|(a, b)| (a.as_str(), "master", b.as_str())).collect();
        parts.push(("visio/masters/_rels/masters.xml.rels".into(), rels(&mr)));
    }
    parts.push(("visio/_rels/document.xml.rels".into(), rels(&doc_rels)));
    parts.push(("visio/document.xml".into(), format!(r#"<VisioDocument {NS}>{document}</VisioDocument>"#)));
    let refs: Vec<(&str, &str)> = parts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    opc(("visio/document.xml", VSDX_CT), &refs)
}

pub(super) fn one(shapes: &str) -> Vec<u8> {
    drawing(&[page(shapes)], "", &[])
}

pub(super) fn render(data: &[u8]) -> PageModel {
    match render_visio_page(data, 0, &PreviewPolicy::default()) {
        Ok(p) => p,
        Err(e) => panic!("render failed: {e}"),
    }
}

/// A path as points `(x, y)` of its move, line and curve-end operations.
pub(super) fn points(ops: &[f32]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&code) = ops.get(i) {
        if code == op::MOVE || code == op::LINE {
            out.push((ops[i + 1], ops[i + 2]));
            i += 3;
        } else if code == op::CURVE {
            out.push((ops[i + 5], ops[i + 6]));
            i += 7;
        } else {
            i += 1;
        }
    }
    out
}

fn paths(p: &PageModel) -> Vec<(&Vec<f32>, u32, u32, f32)> {
    p.prims
        .iter()
        .filter_map(|q| match q {
            Prim::Path { ops, fill, stroke, width, .. } => Some((ops, *fill, *stroke, *width)),
            _ => None,
        })
        .collect()
}

fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.02 && (a.1 - b.1).abs() < 0.02
}

/// A shape of 2 x 1 inches with its local pin at (1, 0.5) placed at (3, 4); the rectangle is a
/// closed path of MoveTo and LineTo rows. `extra` adds cells.
fn rect(extra: &str) -> String {
    format!(
        r##"<Shape ID="1" Type="Shape"><Cell N="PinX" V="3"/><Cell N="PinY" V="4"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="LocPinX" V="1"/><Cell N="LocPinY" V="0.5"/>{extra}
        <Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.02"/><Cell N="FillPattern" V="1"/><Cell N="FillForegnd" V="#ff0000"/>
        <Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row>
          <Row T="LineTo" IX="2"><Cell N="X" V="2"/><Cell N="Y" V="0"/></Row>
          <Row T="LineTo" IX="3"><Cell N="X" V="2"/><Cell N="Y" V="1"/></Row>
          <Row T="LineTo" IX="4"><Cell N="X" V="0"/><Cell N="Y" V="1"/></Row>
          <Row T="LineTo" IX="5"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row></Section></Shape>"##
    )
}

// ---- placement ----------------------------------------------------------------------------------

#[test]
fn a_shape_is_placed_by_its_pin_and_local_pin_with_exact_coordinates() {
    let p = render(&one(&rect("")));
    assert_eq!((p.width, p.height), (612.0, 792.0));
    let all = paths(&p);
    assert_eq!(all.len(), 1);
    let (ops, fill, stroke, width) = all[0];
    // Local (0,0) is the pin (3,4) minus (1, 0.5): (2, 3.5) inches; y runs down from the top.
    let pts = points(ops);
    let want = [(2.0, 3.5), (4.0, 3.5), (4.0, 4.5), (2.0, 4.5), (2.0, 3.5)];
    assert_eq!(pts.len(), 5);
    for (got, w) in pts.iter().zip(want) {
        assert!(close(*got, (w.0 * 72.0, (11.0 - w.1) * 72.0)), "{got:?} vs {w:?}");
    }
    assert_eq!(fill, 0xFFFF_0000);
    assert_eq!(stroke, 0xFF00_0000);
    assert!((width - 1.44).abs() < 1e-4, "{width}");
    assert!(!p.approximate && !p.truncated);
}

#[test]
fn rotation_is_counter_clockwise_about_the_pin() {
    // 90 degrees: local (2, 0) is (1, -0.5) from the local pin; turned counter-clockwise it is
    // (0.5, 1), so the page point is (3.5, 5).
    let p = render(&one(&rect(r#"<Cell N="Angle" V="1.5707963267948966"/>"#)));
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[1], (3.5 * 72.0, (11.0 - 5.0) * 72.0)), "{:?}", pts[1]);
    // (0,0): (-1,-0.5) -> (0.5,-1) -> (3.5, 3).
    assert!(close(pts[0], (3.5 * 72.0, (11.0 - 3.0) * 72.0)), "{:?}", pts[0]);
}

#[test]
fn flips_negate_the_coordinate_about_the_local_pin() {
    // FlipX: local (2,0) -> (1,-0.5) -> (-1,-0.5) -> page (2, 3.5).
    let p = render(&one(&rect(r#"<Cell N="FlipX" V="1"/>"#)));
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[1], (2.0 * 72.0, (11.0 - 3.5) * 72.0)), "{:?}", pts[1]);
    // FlipY: local (2,0) -> (1,-0.5) -> (1, 0.5) -> page (4, 4.5).
    let p = render(&one(&rect(r#"<Cell N="FlipY" V="1"/>"#)));
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[1], (4.0 * 72.0, (11.0 - 4.5) * 72.0)), "{:?}", pts[1]);
    // Both flips and a half turn.
    let p = render(&one(&rect(r#"<Cell N="FlipX" V="1"/><Cell N="FlipY" V="1"/><Cell N="Angle" V="3.141592653589793"/>"#)));
    let pts = points(paths(&p)[0].0);
    // (1,-0.5) -> (-1, 0.5) -> turned by pi: (1, -0.5) -> (4, 3.5).
    assert!(close(pts[1], (4.0 * 72.0, (11.0 - 3.5) * 72.0)), "{:?}", pts[1]);
}

#[test]
fn groups_place_their_children_in_the_group_coordinates() {
    let child = r#"<Shape ID="2" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/><Cell N="Width" V="1"/><Cell N="Height" V="1"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.01"/>
        <Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row></Section></Shape>"#;
    let group = format!(
        r#"<Shape ID="1" Type="Group"><Cell N="PinX" V="5"/><Cell N="PinY" V="5"/><Cell N="Width" V="4"/><Cell N="Height" V="4"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><Cell N="Angle" V="1.5707963267948966"/><Shapes>{child}</Shapes></Shape>"#
    );
    let p = render(&one(&group));
    let pts = points(paths(&p)[0].0);
    // The child's (0,0) is at (1,1) in the group; the group turns by 90 degrees about its local
    // origin (the pin): (1,1) -> (-1, 1) -> (4, 6); (1,0)+(1,1) = (2,1) -> (-1, 2) -> (4, 7).
    assert!(close(pts[0], (4.0 * 72.0, (11.0 - 6.0) * 72.0)), "{:?}", pts[0]);
    assert!(close(pts[1], (4.0 * 72.0, (11.0 - 7.0) * 72.0)), "{:?}", pts[1]);
}

#[test]
fn the_page_scale_turns_drawing_units_into_paper_inches() {
    // A drawing at 1:2 (PageScale 1, DrawingScale 2): 4 drawing inches are 2 paper inches.
    let sheet = r#"<Cell N="PageWidth" V="8.5"/><Cell N="PageHeight" V="11"/><Cell N="PageScale" V="1"/><Cell N="DrawingScale" V="2"/>"#;
    let shapes = rect("");
    let data = drawing(
        &[TestPage { attrs: "", sheet, shapes: &shapes, connects: "" }],
        "",
        &[],
    );
    let p = render(&data);
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[0], (1.0 * 72.0, (11.0 - 1.75) * 72.0)), "{:?}", pts[0]);
    // The line weight is paper units: not scaled.
    assert!((paths(&p)[0].3 - 1.44).abs() < 1e-4);
}

// ---- geometry rows ------------------------------------------------------------------------------

fn geometry(rows: &str, cells: &str) -> String {
    format!(
        r#"<Shape ID="1" Type="Shape"><Cell N="PinX" V="0"/><Cell N="PinY" V="0"/><Cell N="Width" V="4"/><Cell N="Height" V="2"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.01"/>{cells}<Section N="Geometry" IX="0">{rows}</Section></Shape>"#
    )
}

#[test]
fn arcs_ellipses_and_polylines_are_drawn_exactly() {
    // ArcTo: a half circle from (0,0) to (2,0) with the bow equal to the radius, counter-clockwise
    // (positive bow): it passes through (1,-1).
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="1"/></Row><Row T="ArcTo" IX="2"><Cell N="X" V="2"/><Cell N="Y" V="1"/><Cell N="A" V="1"/></Row>"#;
    let p = render(&one(&geometry(rows, "")));
    let pts = points(paths(&p)[0].0);
    assert_eq!(pts.len(), 3, "a move and two quarter circles");
    assert!(close(pts[1], (1.0 * 72.0, (11.0 - 0.0) * 72.0)), "{:?}", pts[1]);
    assert!(close(pts[2], (2.0 * 72.0, (11.0 - 1.0) * 72.0)), "{:?}", pts[2]);
    // A negative bow turns the other way: through (1, 2).
    let rows = rows.replace(r#"N="A" V="1""#, r#"N="A" V="-1""#);
    let p = render(&one(&geometry(&rows, "")));
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[1], (1.0 * 72.0, (11.0 - 2.0) * 72.0)), "{:?}", pts[1]);

    // EllipticalArcTo: through the control point (1, 1) from (0,0) to (2,0), a circle (ratio 1).
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="EllipticalArcTo" IX="2"><Cell N="X" V="2"/><Cell N="Y" V="0"/><Cell N="A" V="1"/><Cell N="B" V="1"/><Cell N="C" V="0"/><Cell N="D" V="1"/></Row>"#;
    let p = render(&one(&geometry(rows, "")));
    let pts = points(paths(&p)[0].0);
    assert!(close(*pts.last().unwrap(), (2.0 * 72.0, 11.0 * 72.0)), "{pts:?}");
    assert!(pts.iter().any(|q| close(*q, (72.0, (11.0 - 1.0) * 72.0))), "{pts:?}");

    // Ellipse: centre (2,1), a point at (3,1) and one at (2,2): a circle of radius 1.
    let rows = r#"<Row T="Ellipse" IX="1"><Cell N="X" V="2"/><Cell N="Y" V="1"/><Cell N="A" V="3"/><Cell N="B" V="1"/><Cell N="C" V="2"/><Cell N="D" V="2"/></Row>"#;
    let p = render(&one(&geometry(rows, "<Cell N=\"FillPattern\" V=\"1\"/>")));
    let (ops, fill, ..) = paths(&p)[0];
    assert_eq!(fill >> 24, 0xFF, "a closed ellipse is filled");
    let pts = points(ops);
    assert_eq!(pts.len(), 5);
    assert!(close(pts[0], (3.0 * 72.0, (11.0 - 1.0) * 72.0)));
    assert!(close(pts[1], (2.0 * 72.0, (11.0 - 2.0) * 72.0)));
    assert!(close(pts[2], (1.0 * 72.0, (11.0 - 1.0) * 72.0)));
    assert!(close(pts[3], (2.0 * 72.0, (11.0 - 0.0) * 72.0)));

    // PolylineTo: the points are relative to the shape (type 0): x * 4, y * 2.
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="PolylineTo" IX="2"><Cell N="X" V="4"/><Cell N="Y" V="0"/><Cell N="A" V="0" F="POLYLINE(0, 0, 0.25, 0.5, 0.5, 1)"/></Row>"#;
    let p = render(&one(&geometry(rows, "")));
    let pts = points(paths(&p)[0].0);
    assert_eq!(pts.len(), 4);
    assert!(close(pts[1], (1.0 * 72.0, (11.0 - 1.0) * 72.0)));
    assert!(close(pts[2], (2.0 * 72.0, (11.0 - 2.0) * 72.0)));
    assert!(close(pts[3], (4.0 * 72.0, 11.0 * 72.0)));
    assert!(!p.approximate);
}

#[test]
fn relative_rows_scale_by_the_shape_size() {
    let rows = r#"<Row T="RelMoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="0.5"/></Row><Row T="RelCubBezTo" IX="3"><Cell N="X" V="1"/><Cell N="Y" V="1"/><Cell N="A" V="1"/><Cell N="B" V="0.5"/><Cell N="C" V="1"/><Cell N="D" V="1"/></Row><Row T="RelQuadBezTo" IX="4"><Cell N="X" V="0"/><Cell N="Y" V="1"/><Cell N="A" V="0.5"/><Cell N="B" V="1"/></Row><Row T="RelEllipticalArcTo" IX="5"><Cell N="X" V="0"/><Cell N="Y" V="0"/><Cell N="A" V="-0.1"/><Cell N="B" V="0.5"/><Cell N="C" V="0"/><Cell N="D" V="1"/></Row>"#;
    let p = render(&one(&geometry(rows, "")));
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[1], (4.0 * 72.0, (11.0 - 1.0) * 72.0)), "{pts:?}");
    assert!(close(pts[2], (4.0 * 72.0, (11.0 - 2.0) * 72.0)), "{pts:?}");
    assert!(close(pts[3], (0.0, (11.0 - 2.0) * 72.0)), "{pts:?}");
    assert!(pts.len() >= 5);
}

#[test]
fn infinite_lines_become_long_lines_and_never_fill() {
    let rows = r#"<Row T="InfiniteLine" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="1"/><Cell N="A" V="1"/><Cell N="B" V="1"/></Row>"#;
    let p = render(&one(&geometry(rows, "")));
    let (ops, fill, ..) = paths(&p)[0];
    assert_eq!(fill, 0);
    let pts = points(ops);
    assert_eq!(pts.len(), 2);
    assert!((pts[0].1 - pts[1].1).abs() < 0.01, "horizontal: {pts:?}");
    assert!(pts[1].0 - pts[0].0 > 612.0, "reaches across the page: {pts:?}");
}

#[test]
fn nurbs_and_splines_are_sampled_and_reported_as_approximations() {
    // A cubic curve through four control points with clamped knots (0 0 0 0 1 1 1 1).
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="NURBSTo" IX="2"><Cell N="X" V="4"/><Cell N="Y" V="0"/><Cell N="A" V="1"/><Cell N="B" V="1"/><Cell N="C" V="0"/><Cell N="D" V="1"/><Cell N="E" V="0" F="NURBS(1, 3, 1, 1, 1, 2, 0, 1, 3, 2, 0, 1)"/></Row>"#;
    let rendered = render_visio_page_noted(&one(&geometry(rows, "")), 0, &PreviewPolicy::default()).unwrap();
    let p = &rendered.page;
    let pts = points(paths(p)[0].0);
    assert!(pts.len() > 8, "sampled: {}", pts.len());
    assert!(close(pts[0], (0.0, 11.0 * 72.0)), "{pts:?}");
    assert!(close(*pts.last().unwrap(), (4.0 * 72.0, 11.0 * 72.0)));
    // The curve bulges up (towards y = 2) and stays inside the control polygon.
    assert!(pts.iter().all(|q| q.0 >= -0.1 && q.0 <= 4.0 * 72.0 + 0.1 && q.1 <= 11.0 * 72.0 + 0.1));
    assert!(pts.iter().any(|q| q.1 < 11.0 * 72.0 - 20.0), "{pts:?}");
    assert!(p.approximate);
    assert!(rendered.notes.iter().any(|n| n.contains("NURBS")));

    // A spline: start, second control point and knots; same treatment.
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="SplineStart" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="2"/><Cell N="A" V="0"/><Cell N="B" V="0"/><Cell N="C" V="1"/><Cell N="D" V="2"/></Row><Row T="SplineKnot" IX="3"><Cell N="X" V="3"/><Cell N="Y" V="2"/><Cell N="A" V="0.5"/></Row><Row T="SplineKnot" IX="4"><Cell N="X" V="4"/><Cell N="Y" V="0"/><Cell N="A" V="1"/></Row>"#;
    let rendered = render_visio_page_noted(&one(&geometry(rows, "")), 0, &PreviewPolicy::default()).unwrap();
    assert!(rendered.page.approximate);
    assert!(rendered.notes.iter().any(|n| n.contains("splines")));
    assert!(points(paths(&rendered.page)[0].0).len() > 8);
}

#[test]
fn a_formula_that_is_not_plain_numbers_is_a_line_and_said_so() {
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="PolylineTo" IX="2"><Cell N="X" V="4"/><Cell N="Y" V="0"/><Cell N="A" V="0" F="POLYLINE(0, 0, Width*0.5, Height)"/></Row>"#;
    let r = render_visio_page_noted(&one(&geometry(rows, "")), 0, &PreviewPolicy::default()).unwrap();
    assert_eq!(points(paths(&r.page)[0].0).len(), 2);
    assert!(r.page.approximate);
}

// ---- inheritance --------------------------------------------------------------------------------

#[test]
fn an_instance_inherits_geometry_from_its_master_and_style_from_the_style_sheets() {
    let master = r#"<Shape ID="5" Type="Shape" LineStyle="3" FillStyle="3"><Cell N="Width" V="2"/><Cell N="Height" V="1"/>
        <Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="2"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="3"><Cell N="X" V="2"/><Cell N="Y" V="1"/></Row><Row T="LineTo" IX="4"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row></Section></Shape>"#;
    // Style 3 inherits the line colour from style 2 (via its LineStyle) and sets its own fill.
    let styles = r##"<StyleSheets>
        <StyleSheet ID="2"><Cell N="LineColor" V="#00ff00"/><Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.03"/></StyleSheet>
        <StyleSheet ID="3" LineStyle="2" FillStyle="2"><Cell N="FillPattern" V="1"/><Cell N="FillForegnd" V="3"/></StyleSheet></StyleSheets>"##;
    let instance = r#"<Shape ID="1" Type="Shape" Master="7"><Cell N="PinX" V="3"/><Cell N="PinY" V="4"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/></Shape>"#;
    let data = drawing(&[page(instance)], styles, &[(7, master)]);
    let p = render(&data);
    let (ops, fill, stroke, width) = paths(&p)[0];
    // The geometry and the size come from the master shape; the pin from the instance.
    let pts = points(ops);
    assert!(close(pts[1], (5.0 * 72.0, (11.0 - 4.0) * 72.0)), "{pts:?}");
    // Colour index 3 of the default table is green; the stroke colour comes through two styles.
    assert_eq!(fill, 0xFF00_FF00);
    assert_eq!(stroke, 0xFF00_FF00);
    assert!((width - 2.16).abs() < 1e-4, "{width}");

    // The instance's own cells win.
    let instance = r##"<Shape ID="1" Type="Shape" Master="7"><Cell N="PinX" V="3"/><Cell N="PinY" V="4"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><Cell N="LineColor" V="#0000ff"/></Shape>"##;
    let p = render(&drawing(&[page(instance)], styles, &[(7, master)]));
    assert_eq!(paths(&p)[0].2, 0xFF00_00FF);
}

#[test]
fn children_of_an_instance_find_their_master_shapes_by_id() {
    let master = r#"<Shape ID="1" Type="Group"><Cell N="Width" V="2"/><Cell N="Height" V="2"/><Shapes>
        <Shape ID="2" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/><Cell N="Width" V="1"/><Cell N="Height" V="1"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.01"/>
        <Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row></Section></Shape></Shapes></Shape>"#;
    let instance = r#"<Shape ID="10" Type="Group" Master="4"><Cell N="PinX" V="2"/><Cell N="PinY" V="2"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><Shapes><Shape ID="11" Type="Shape" MasterShape="2"/></Shapes></Shape>"#;
    let p = render(&drawing(&[page(instance)], "", &[(4, master)]));
    let all = paths(&p);
    assert_eq!(all.len(), 1);
    let pts = points(all[0].0);
    // The child's pin (1,1) comes from its master shape, plus the group's pin (2,2).
    assert!(close(pts[0], (3.0 * 72.0, (11.0 - 3.0) * 72.0)), "{pts:?}");
}

#[test]
fn master_and_style_cycles_end() {
    let master = r#"<Shape ID="1" Type="Shape" Master="1" LineStyle="1"><Cell N="Width" V="1"/><Cell N="Height" V="1"/><Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row></Section></Shape>"#;
    let styles = r#"<StyleSheets><StyleSheet ID="1" LineStyle="2" FillStyle="1"><Cell N="LinePattern" V="1"/></StyleSheet><StyleSheet ID="2" LineStyle="1" FillStyle="2"/></StyleSheets>"#;
    let instance = r#"<Shape ID="1" Type="Shape" Master="1" LineStyle="2"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/></Shape>"#;
    let p = render(&drawing(&[page(instance)], styles, &[(1, master)]));
    assert_eq!(paths(&p).len(), 1);
}

// ---- styles, text, visibility, effects ----------------------------------------------------------

#[test]
fn colour_indices_use_the_document_table_and_patterns_are_flat_colours() {
    let doc = r##"<Colors><ColorEntry IX="0" RGB="#102030"/><ColorEntry IX="1" RGB="#405060"/></Colors>"##;
    let shape = rect("")
        .replace(r##"<Cell N="FillForegnd" V="#ff0000"/>"##, r#"<Cell N="FillForegnd" V="1"/><Cell N="FillBkgnd" V="0"/>"#)
        .replace(r#"<Cell N="FillPattern" V="1"/>"#, r#"<Cell N="FillPattern" V="5"/>"#);
    let r = render_visio_page_noted(&drawing(&[page(&shape)], doc, &[]), 0, &PreviewPolicy::default()).unwrap();
    // Pattern 5 is a hatch: drawn as the mix of the two colours, and reported.
    assert_eq!(paths(&r.page)[0].1, 0xFF28_3848);
    assert!(r.page.approximate);
    assert!(r.notes.iter().any(|n| n.contains("fill patterns")));
}

#[test]
fn text_is_drawn_with_size_style_colour_and_alignment() {
    let chars = r##"<Section N="Character"><Row IX="0"><Cell N="Size" V="0.16666666666666666"/><Cell N="Style" V="1"/><Cell N="Color" V="#0000ff"/></Row></Section>"##;
    let shape = rect(&format!("{chars}")).replace("</Shape>", r#"<Text><cp IX="0"/>Hi</Text></Shape>"#);
    let p = render(&drawing(&[page(&shape)], "", &[]));
    let texts: Vec<_> = p
        .prims
        .iter()
        .filter_map(|q| match q {
            Prim::Text { x, y, size, angle, width, style, color, text } => {
                Some((*x, *y, *size, *angle, *width, *style, *color, text.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1);
    let (x, y, size, angle, width, style, color, text) = texts[0].clone();
    assert_eq!(text, "Hi");
    assert!((size - 12.0).abs() < 0.01);
    assert_eq!(style, crate::page::style::BOLD);
    assert_eq!(color, 0xFF00_00FF);
    assert!(angle.abs() < 1e-6);
    // Centred in the 2 x 1 inch box at (3, 4): the run is centred on x = 216.
    assert!((x + width / 2.0 - 216.0).abs() < 0.1, "{x} {width}");
    // The single line is centred vertically: its baseline is a little below the centre.
    assert!(y > (11.0 - 4.0) * 72.0 && y < (11.0 - 4.0) * 72.0 + 12.0, "{y}");
}

#[test]
fn text_follows_the_rotation_of_the_shape_and_wraps_to_the_block() {
    let shape = rect(r#"<Cell N="Angle" V="1.5707963267948966"/>"#)
        .replace("</Shape>", r#"<Text>alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu</Text></Shape>"#);
    let p = render(&drawing(&[page(&shape)], "", &[]));
    let texts: Vec<_> = p
        .prims
        .iter()
        .filter_map(|q| match q {
            Prim::Text { angle, text, .. } => Some((*angle, text.clone())),
            _ => None,
        })
        .collect();
    assert!(texts.len() > 1, "wrapped into several lines: {texts:?}");
    // A quarter turn counter-clockwise reads upwards: -90 degrees on the screen.
    assert!(texts.iter().all(|(a, _)| (a + core::f32::consts::FRAC_PI_2).abs() < 1e-4));
}

#[test]
fn hidden_layers_follow_the_policy() {
    let sheet = r#"<Cell N="PageWidth" V="8.5"/><Cell N="PageHeight" V="11"/><Section N="Layer"><Row IX="0"><Cell N="Name" V="a"/><Cell N="Visible" V="1"/></Row><Row IX="1"><Cell N="Name" V="b"/><Cell N="Visible" V="0"/></Row></Section>"#;
    let shape = rect(r#"<Section N="LayerMem"><Row IX="0"><Cell N="LayerMember" V="1"/></Row></Section>"#);
    let data = drawing(&[TestPage { attrs: "", sheet, shapes: &shape, connects: "" }], "", &[]);
    let shown = render_visio_page_noted(&data, 0, &PreviewPolicy::default()).unwrap();
    assert_eq!(shown.hidden_shown, 1);
    let (_, fill, stroke, _) = paths(&shown.page)[0];
    assert!(fill >> 24 < 0xFF && stroke >> 24 < 0xFF, "drawn faintly: {fill:x} {stroke:x}");
    let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
    let hidden = render_visio_page_noted(&data, 0, &policy).unwrap();
    assert_eq!(hidden.hidden_omitted, 1);
    assert!(paths(&hidden.page).is_empty());
    // A shape on a visible layer is drawn either way.
    let shape = rect(r#"<Section N="LayerMem"><Row IX="0"><Cell N="LayerMember" V="0;1"/></Row></Section>"#);
    let data = drawing(&[TestPage { attrs: "", sheet, shapes: &shape, connects: "" }], "", &[]);
    assert_eq!(paths(&render_visio_page_noted(&data, 0, &policy).unwrap().page).len(), 1);
}

#[test]
fn three_d_effects_are_reported_as_not_drawn() {
    let shape = rect(r#"<Cell N="BevelTopType" V="3"/><Cell N="BevelTopWidth" V="0.1"/>"#);
    let r = render_visio_page_noted(&one(&shape), 0, &PreviewPolicy::default()).unwrap();
    assert!(r.page.approximate);
    assert!(r.notes.iter().any(|n| n.contains("3-D effects") && n.contains("not drawn")), "{:?}", r.notes);
    // The shape itself is still drawn; a zero bevel is no effect.
    assert_eq!(paths(&r.page).len(), 1);
    let none = rect(r#"<Cell N="BevelTopType" V="0"/>"#);
    let r = render_visio_page_noted(&one(&none), 0, &PreviewPolicy::default()).unwrap();
    assert!(!r.page.approximate);
}

#[test]
fn arrowheads_dashes_and_pictures_are_marked() {
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="1"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="4"/><Cell N="Y" V="1"/></Row>"#;
    let shape = geometry(rows, r#"<Cell N="EndArrow" V="13"/>"#)
        .replace(r#"<Cell N="LinePattern" V="1"/>"#, r#"<Cell N="LinePattern" V="2"/>"#);
    let r = render_visio_page_noted(&one(&shape), 0, &PreviewPolicy::default()).unwrap();
    assert_eq!(paths(&r.page).len(), 2, "the line and the arrowhead");
    assert!(r.notes.iter().any(|n| n.contains("arrowheads")));
    assert!(r.notes.iter().any(|n| n.contains("dashed")));
    let pic = r#"<Shape ID="1" Type="Foreign"><Cell N="PinX" V="2"/><Cell N="PinY" V="2"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="LocPinX" V="0"/><Cell N="LocPinY" V="0"/><ForeignData ForeignType="Bitmap"><Rel r:id="rId9"/></ForeignData></Shape>"#;
    let p = render(&one(pic));
    assert!(p.prims.iter().any(|q| matches!(q, Prim::Image { label, id, .. } if label == "picture (bitmap)" && *id == crate::model::IMAGE_NONE)));
}

#[test]
fn connectors_are_drawn_from_their_own_geometry_or_as_a_straight_line() {
    let own = geometry(
        r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="3"/><Cell N="Y" V="0"/></Row>"#,
        r#"<Cell N="BeginX" V="0"/><Cell N="BeginY" V="0"/><Cell N="EndX" V="3"/><Cell N="EndY" V="0"/>"#,
    );
    let p = render(&one(&own));
    assert_eq!(points(paths(&p)[0].0).len(), 2);
    // No geometry at all: the line from begin to end, in page coordinates.
    let bare = r#"<Shape ID="9" Type="Shape"><Cell N="BeginX" V="1"/><Cell N="BeginY" V="1"/><Cell N="EndX" V="3"/><Cell N="EndY" V="2"/><Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.01"/></Shape>"#;
    let p = render(&one(bare));
    let pts = points(paths(&p)[0].0);
    assert!(close(pts[0], (72.0, (11.0 - 1.0) * 72.0)) && close(pts[1], (216.0, (11.0 - 2.0) * 72.0)), "{pts:?}");
}

// ---- pages --------------------------------------------------------------------------------------

#[test]
fn the_page_count_is_the_number_of_foreground_pages_and_backgrounds_lie_under() {
    let back = rect("");
    let front = r#"<Shape ID="1" Type="Shape"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/><Cell N="Width" V="1"/><Cell N="Height" V="1"/><Cell N="LinePattern" V="1"/><Cell N="LineWeight" V="0.01"/><Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row></Section></Shape>"#;
    let data = drawing(
        &[
            TestPage { attrs: r#"Background="1""#, sheet: "", shapes: &back, connects: "" },
            TestPage { attrs: r#"BackPage="1""#, sheet: "", shapes: front, connects: "" },
            TestPage { attrs: "", sheet: "", shapes: front, connects: "" },
        ],
        "",
        &[],
    );
    let policy = PreviewPolicy::default();
    assert_eq!(visio_page_count(&data, &policy).unwrap(), 2);
    let first = render_visio_page(&data, 0, &policy).unwrap();
    assert_eq!((first.index, first.pages), (0, 2));
    assert_eq!(paths(&first).len(), 2, "the background page and the page");
    let second = render_visio_page(&data, 1, &policy).unwrap();
    assert_eq!(paths(&second).len(), 1, "no background page");
    assert!(matches!(render_visio_page(&data, 2, &policy), Err(PreviewError::Malformed(_))));
}

#[test]
fn other_formats_are_refused() {
    let policy = PreviewPolicy::default();
    assert!(matches!(visio_page_count(b"plain", &policy), Err(PreviewError::Unsupported(_))));
    assert!(matches!(render_visio_page(b"plain", 0, &policy), Err(PreviewError::Unsupported(_))));
}

#[test]
fn the_public_page_entry_point_draws_visio_drawings() {
    let data = one(&rect(""));
    let p = crate::render_page(&data, 0, &PreviewPolicy::default()).unwrap();
    assert_eq!(p.pages, 1);
    assert!(!p.prims.is_empty());
}

// ---- hostile input ------------------------------------------------------------------------------

#[test]
fn hostile_numbers_formulas_and_counts_stay_bounded() {
    // NaN, infinity and huge values in cells; a formula that names its own cell.
    let rows = r#"<Row T="MoveTo" IX="1"><Cell N="X" V="NaN"/><Cell N="Y" V="inf"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="1e308"/><Cell N="Y" V="-1e308"/></Row><Row T="ArcTo" IX="3"><Cell N="X" V="1e300"/><Cell N="Y" V="1"/><Cell N="A" V="1e300"/></Row><Row T="EllipticalArcTo" IX="4"><Cell N="X" V="1"/><Cell N="Y" V="1"/><Cell N="A" V="1e300"/><Cell N="B" V="NaN"/><Cell N="C" V="1e300"/><Cell N="D" V="0"/></Row><Row T="Ellipse" IX="5"><Cell N="X" V="1e300"/><Cell N="Y" V="1"/><Cell N="A" V="1"/><Cell N="B" V="1"/><Cell N="C" V="1e300"/><Cell N="D" V="1e300"/></Row>"#;
    let cells = r#"<Cell N="LineWeight" V="1e300" F="LineWeight*2"/><Cell N="Angle" V="1e300"/>"#;
    let shape = geometry(rows, cells).replace(r#"<Cell N="Width" V="4"/>"#, r#"<Cell N="Width" V="4" F="Width"/>"#);
    let p = render(&one(&shape));
    for q in &p.prims {
        if let Prim::Path { ops, width, .. } = q {
            assert!(ops.iter().all(|v| v.is_finite()) && width.is_finite());
        }
    }
    // A NURBS with an absurd number of knots and a degree beyond the limit.
    let nums: String = (0..50_000).map(|i| format!(", {i}, {i}, {i}, 1")).collect();
    let rows = format!(
        r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="NURBSTo" IX="2"><Cell N="X" V="4"/><Cell N="Y" V="0"/><Cell N="A" V="1"/><Cell N="B" V="1"/><Cell N="C" V="0"/><Cell N="D" V="1"/><Cell N="E" V="0" F="NURBS(1, 25, 1, 1{nums})"/></Row>"#
    );
    let p = render(&one(&geometry(&rows, "")));
    assert!(p.prims.len() <= crate::page::MAX_PRIMS);
    let few: String = (0..30).map(|i| format!(", {i}, 1, {i}, 1")).collect();
    let rows = format!(
        r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="NURBSTo" IX="2"><Cell N="X" V="4"/><Cell N="Y" V="0"/><Cell N="A" V="1"/><Cell N="B" V="1"/><Cell N="C" V="0"/><Cell N="D" V="1"/><Cell N="E" V="0" F="NURBS(29, 25, 1, 1{few})"/></Row>"#
    );
    let r = render_visio_page_noted(&one(&geometry(&rows, "")), 0, &PreviewPolicy::default()).unwrap();
    assert!(r.notes.iter().any(|n| n.contains("degree above 8")));
    assert!(points(paths(&r.page)[0].0).len() <= 600);
}

#[test]
fn a_flood_of_shapes_and_deep_groups_are_cut_not_followed() {
    let flood: String = (0..130_000)
        .map(|i| format!(r#"<Shape ID="{i}"><Cell N="PinX" V="1"/><Cell N="PinY" V="1"/></Shape>"#))
        .collect();
    let policy = PreviewPolicy { aggregate_ratio: 100_000, ..PreviewPolicy::default() };
    let p = render_visio_page(&one(&flood), 0, &policy).unwrap();
    assert!(p.truncated);
    let lines: String = (0..30_000)
        .map(|i| geometry(r#"<Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row>"#, "").replace("ID=\"1\"", &format!("ID=\"{i}\"")))
        .collect();
    let p = render_visio_page(&one(&lines), 0, &policy).unwrap();
    assert!(p.truncated, "more shapes than the page budget");
    assert!(p.prims.len() <= 20_000);
    // Nesting beyond the depth limit.
    let mut deep = String::new();
    for i in 0..200 {
        deep.push_str(&format!(r#"<Shape ID="{i}" Type="Group"><Cell N="PinX" V="0.01"/><Shapes>"#));
    }
    deep.push_str(&rect(""));
    for _ in 0..200 {
        deep.push_str("</Shapes></Shape>");
    }
    let p = render(&one(&deep));
    assert!(p.truncated || paths(&p).is_empty() || paths(&p).len() == 1);
    // A page cut in the middle of an element.
    let data = one(&rect(""));
    let _ = render_visio_page(&data[..data.len() / 2], 0, &PreviewPolicy::default());
}

#[test]
fn mutated_packages_never_panic() {
    use crate::testkit::{Lcg, fuzz_iterations};
    let base = one(&rect(r#"<Text>x</Text>"#));
    let mut rng = Lcg::new(5);
    for _ in 0..fuzz_iterations(200) {
        let mut m = base.clone();
        rng.mutate(&mut m, 4);
        let _ = render_visio_page(&m, 0, &PreviewPolicy::default());
        let _ = visio_page_count(&m, &PreviewPolicy::default());
    }
}
