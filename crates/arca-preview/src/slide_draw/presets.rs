//! Preset shapes as paths, in the shape's own coordinates (`0..w`, `0..h`).
//!
//! A small table of the presets slides use all the time (ISO 29500 `presetShapeDefinitions` for
//! the ones listed here, with their default adjust values). Anything else is not drawn from this
//! table: the caller outlines the frame and says the slide is approximate.

use super::Seg;

/// Bézier constant for a quarter of a circle.
const KAPPA: f64 = 0.552_284_749_830_793_4;

/// A preset shape.
pub(super) struct Preset {
    /// The outline.
    pub(super) segs: Vec<Seg>,
    /// A line: it is stroked, never filled.
    pub(super) open: bool,
    /// The preset is only a rough likeness of the named shape.
    pub(super) rough: bool,
}

fn closed(segs: Vec<Seg>) -> Preset {
    Preset { segs, open: false, rough: false }
}

fn rough(segs: Vec<Seg>) -> Preset {
    Preset { segs, open: false, rough: true }
}

/// A rectangle.
pub(super) fn rect(w: f64, h: f64) -> Vec<Seg> {
    poly(&[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)], true)
}

/// A polygon or polyline through `pts`.
pub(super) fn poly(pts: &[(f64, f64)], close: bool) -> Vec<Seg> {
    let mut out = Vec::with_capacity(pts.len() + 1);
    for (i, (x, y)) in pts.iter().enumerate() {
        out.push(if i == 0 { Seg::M(*x, *y) } else { Seg::L(*x, *y) });
    }
    if close && !pts.is_empty() {
        out.push(Seg::Z);
    }
    out
}

/// An ellipse inscribed in `0..w`, `0..h`.
pub(super) fn ellipse(w: f64, h: f64) -> Vec<Seg> {
    let (rx, ry) = (w / 2.0, h / 2.0);
    let (cx, cy) = (rx, ry);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    vec![
        Seg::M(cx + rx, cy),
        Seg::C(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry),
        Seg::C(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy),
        Seg::C(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry),
        Seg::C(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy),
        Seg::Z,
    ]
}

/// A rectangle with rounded corners of radius `r`.
pub(super) fn round_rect(w: f64, h: f64, r: f64) -> Vec<Seg> {
    let r = r.clamp(0.0, w.min(h) / 2.0);
    if r <= 0.0 {
        return rect(w, h);
    }
    let k = r * KAPPA;
    vec![
        Seg::M(r, 0.0),
        Seg::L(w - r, 0.0),
        Seg::C(w - r + k, 0.0, w, r - k, w, r),
        Seg::L(w, h - r),
        Seg::C(w, h - r + k, w - r + k, h, w - r, h),
        Seg::L(r, h),
        Seg::C(r - k, h, 0.0, h - r + k, 0.0, h - r),
        Seg::L(0.0, r),
        Seg::C(0.0, r - k, r - k, 0.0, r, 0.0),
        Seg::Z,
    ]
}

/// Scales the points so that their bounding box is exactly `0..w`, `0..h`.
fn fit(pts: &[(f64, f64)], w: f64, h: f64) -> Vec<(f64, f64)> {
    let x0 = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let x1 = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y0 = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let y1 = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let (dx, dy) = ((x1 - x0).max(1e-9), (y1 - y0).max(1e-9));
    pts.iter().map(|(x, y)| ((x - x0) / dx * w, (y - y0) / dy * h)).collect()
}

/// An arrow of length `len` along its axis and thickness `thick` across it, pointing along +x:
/// the points as `(along, across)`.
fn arrow_points(len: f64, thick: f64, shaft: f64, head: f64) -> Vec<(f64, f64)> {
    let y1 = thick * (1.0 - shaft) / 2.0;
    let x1 = (len - head).max(0.0);
    vec![
        (0.0, y1),
        (x1, y1),
        (x1, 0.0),
        (len, thick / 2.0),
        (x1, thick),
        (x1, thick - y1),
        (0.0, thick - y1),
    ]
}

/// The preset `name` for a frame of `w × h`; `adj` are the first two adjust values (in 1/100000).
/// `None` for a name this table does not know.
#[allow(clippy::too_many_lines)] // one arm per preset
pub(super) fn preset(name: &str, w: f64, h: f64, adj: [Option<f64>; 2]) -> Option<Preset> {
    let a = |i: usize, default: f64, max: f64| -> f64 {
        let v = adj.get(i).copied().flatten().unwrap_or(default);
        (if v.is_finite() { v } else { default } / 100_000.0).clamp(0.0, max)
    };
    let ss = w.min(h);
    Some(match name {
        "rect"
        | "flowChartProcess"
        | "flowChartPredefinedProcess"
        | "flowChartInternalStorage"
        | "textBox" => closed(rect(w, h)),
        "roundRect" | "flowChartAlternateProcess" => {
            closed(round_rect(w, h, ss * a(0, 16_667.0, 0.5)))
        }
        "flowChartTerminator" => closed(round_rect(w, h, ss / 2.0)),
        "ellipse" | "flowChartConnector" | "flowChartSummingJunction" | "flowChartOr" => {
            closed(ellipse(w, h))
        }
        "triangle" => closed(poly(&[(0.0, h), (w * a(0, 50_000.0, 1.0), 0.0), (w, h)], true)),
        "rtTriangle" => closed(poly(&[(0.0, 0.0), (0.0, h), (w, h)], true)),
        "diamond" | "flowChartDecision" => {
            closed(poly(&[(w / 2.0, 0.0), (w, h / 2.0), (w / 2.0, h), (0.0, h / 2.0)], true))
        }
        "parallelogram" | "flowChartInputOutput" => {
            let x = ss * a(0, 25_000.0, if ss > 0.0 { w / ss } else { 1.0 });
            closed(poly(&[(0.0, h), (x, 0.0), (w, 0.0), (w - x, h)], true))
        }
        "trapezoid" | "flowChartManualOperation" => {
            let x = ss * a(0, 25_000.0, 0.5 * if ss > 0.0 { w / ss } else { 1.0 });
            closed(poly(&[(0.0, h), (x, 0.0), (w - x, 0.0), (w, h)], true))
        }
        "hexagon" | "flowChartPreparation" => {
            let x = ss * a(0, 25_000.0, 0.5 * if ss > 0.0 { w / ss } else { 1.0 });
            closed(poly(
                &[(0.0, h / 2.0), (x, 0.0), (w - x, 0.0), (w, h / 2.0), (w - x, h), (x, h)],
                true,
            ))
        }
        "octagon" => {
            let x = ss * a(0, 29_289.0, 0.5);
            closed(poly(
                &[
                    (0.0, x),
                    (x, 0.0),
                    (w - x, 0.0),
                    (w, x),
                    (w, h - x),
                    (w - x, h),
                    (x, h),
                    (0.0, h - x),
                ],
                true,
            ))
        }
        "pentagon" => {
            let pts: Vec<(f64, f64)> = (0..5)
                .map(|i| {
                    let t = (f64::from(i) * 72.0 - 90.0).to_radians();
                    (t.cos(), t.sin())
                })
                .collect();
            closed(poly(&fit(&pts, w, h), true))
        }
        "star5" => {
            let pts: Vec<(f64, f64)> = (0..10)
                .map(|i| {
                    let t = (f64::from(i) * 36.0 - 90.0).to_radians();
                    let r = if i % 2 == 0 {
                        1.0
                    } else {
                        (a(0, 19_098.0, 0.5) / 0.190_98 * 0.381_966).clamp(0.05, 0.95)
                    };
                    (r * t.cos(), r * t.sin())
                })
                .collect();
            closed(poly(&fit(&pts, w, h), true))
        }
        "homePlate" | "flowChartOffpageConnector" => {
            let x = w - ss * a(0, 50_000.0, if ss > 0.0 { w / ss } else { 1.0 });
            closed(poly(&[(0.0, 0.0), (x, 0.0), (w, h / 2.0), (x, h), (0.0, h)], true))
        }
        "chevron" => {
            let x = ss * a(0, 50_000.0, if ss > 0.0 { w / ss } else { 1.0 });
            closed(poly(
                &[(0.0, 0.0), (w - x, 0.0), (w, h / 2.0), (w - x, h), (0.0, h), (x, h / 2.0)],
                true,
            ))
        }
        "rightArrow" | "leftArrow" => {
            let head = ss * a(1, 50_000.0, if ss > 0.0 { w / ss } else { 1.0 });
            let pts = arrow_points(w, h, a(0, 50_000.0, 1.0), head);
            let flip = name == "leftArrow";
            let pts: Vec<(f64, f64)> =
                pts.iter().map(|(l, t)| (if flip { w - l } else { *l }, *t)).collect();
            closed(poly(&pts, true))
        }
        "upArrow" | "downArrow" => {
            let head = ss * a(1, 50_000.0, if ss > 0.0 { h / ss } else { 1.0 });
            let pts = arrow_points(h, w, a(0, 50_000.0, 1.0), head);
            let up = name == "upArrow";
            let pts: Vec<(f64, f64)> =
                pts.iter().map(|(l, t)| (*t, if up { h - l } else { *l })).collect();
            closed(poly(&pts, true))
        }
        "plus" | "mathPlus" => {
            let x = ss * a(0, 25_000.0, 0.5);
            closed(poly(
                &[
                    (x, 0.0),
                    (w - x, 0.0),
                    (w - x, x),
                    (w, x),
                    (w, h - x),
                    (w - x, h - x),
                    (w - x, h),
                    (x, h),
                    (x, h - x),
                    (0.0, h - x),
                    (0.0, x),
                    (x, x),
                ],
                true,
            ))
        }
        "line" | "straightConnector1" | "lineInv" => {
            let inv = name == "lineInv";
            Preset {
                segs: vec![
                    Seg::M(0.0, if inv { h } else { 0.0 }),
                    Seg::L(w, if inv { 0.0 } else { h }),
                ],
                open: true,
                rough: false,
            }
        }
        "bentConnector2" => {
            Preset { segs: poly(&[(0.0, 0.0), (w, 0.0), (w, h)], false), open: true, rough: false }
        }
        "bentConnector3" => {
            let x = w * a(0, 50_000.0, 10.0);
            Preset {
                segs: poly(&[(0.0, 0.0), (x, 0.0), (x, h), (w, h)], false),
                open: true,
                rough: false,
            }
        }
        // Other connectors: their path depends on adjust values and routing this table does not
        // model; the straight line between the ends is the closest cheap likeness.
        n if n.starts_with("bentConnector") || n.starts_with("curvedConnector") => {
            Preset { segs: vec![Seg::M(0.0, 0.0), Seg::L(w, h)], open: true, rough: true }
        }
        // A cylinder, a block of text with a folded corner and the like: the frame is the
        // likeness.
        "can"
        | "cube"
        | "foldedCorner"
        | "flowChartDocument"
        | "flowChartMultidocument"
        | "flowChartMagneticDisk"
        | "snip1Rect"
        | "snip2SameRect"
        | "round1Rect"
        | "round2SameRect" => rough(rect(w, h)),
        _ => return None,
    })
}

/// Appends the arc of an ellipse with radii `wr`, `hr` that starts at the current point `from`
/// at the visual angle `st` and sweeps `sw` (both in degrees, positive clockwise on the screen).
/// Returns the end point.
pub(super) fn arc_to(
    out: &mut Vec<Seg>,
    from: (f64, f64),
    wr: f64,
    hr: f64,
    st: f64,
    sw: f64,
) -> (f64, f64) {
    let sw = if sw.is_finite() { sw.clamp(-360.0, 360.0) } else { 0.0 };
    if !(wr.is_finite() && hr.is_finite()) || wr.abs() < 1e-9 || hr.abs() < 1e-9 || sw == 0.0 {
        return from;
    }
    let (wr, hr) = (wr.abs(), hr.abs());
    // The parametric angle of the point at visual angle `theta`.
    let param = |theta_deg: f64| {
        let t = theta_deg.to_radians();
        (wr * t.sin()).atan2(hr * t.cos())
    };
    let t0 = param(st);
    let center = (from.0 - wr * t0.cos(), from.1 - hr * t0.sin());
    let pieces = (sw.abs() / 90.0).ceil().clamp(1.0, 4.0);
    let step = sw / pieces;
    let mut ta = t0;
    let mut end = from;
    for i in 0..4_u8 {
        if f64::from(i) >= pieces {
            break;
        }
        let b = st + step * f64::from(i + 1);
        let a_vis = st + step * f64::from(i);
        // The parametric span of this piece, unwrapped into the direction of the sweep.
        let mut dt = param(b) - param(a_vis);
        if step > 0.0 {
            while dt <= 0.0 {
                dt += std::f64::consts::TAU;
            }
            while dt > std::f64::consts::PI {
                dt -= std::f64::consts::TAU;
            }
        } else {
            while dt >= 0.0 {
                dt -= std::f64::consts::TAU;
            }
            while dt < -std::f64::consts::PI {
                dt += std::f64::consts::TAU;
            }
        }
        let k = 4.0 / 3.0 * (dt / 4.0).tan();
        let tb = ta + dt;
        let p = |t: f64| (center.0 + wr * t.cos(), center.1 + hr * t.sin());
        let d = |t: f64| (-wr * t.sin(), hr * t.cos());
        let (pa, pb) = (p(ta), p(tb));
        let (da, db) = (d(ta), d(tb));
        out.push(Seg::C(
            pa.0 + k * da.0,
            pa.1 + k * da.1,
            pb.0 - k * db.0,
            pb.1 - k * db.1,
            pb.0,
            pb.1,
        ));
        end = pb;
        ta = tb;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(segs: &[Seg]) -> (f64, f64, f64, f64) {
        let mut b = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut add = |x: f64, y: f64| {
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        };
        for s in segs {
            match *s {
                Seg::M(x, y) | Seg::L(x, y) => add(x, y),
                Seg::C(a, b2, c, d, e, f) => {
                    add(a, b2);
                    add(c, d);
                    add(e, f);
                }
                Seg::Z => {}
            }
        }
        b
    }

    #[test]
    fn every_preset_stays_inside_its_frame() {
        for name in [
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
            "bentConnector3",
            "curvedConnector3",
            "flowChartTerminator",
        ] {
            let p = preset(name, 200.0, 100.0, [None, None]).unwrap_or_else(|| panic!("{name}"));
            let (x0, y0, x1, y1) = bounds(&p.segs);
            assert!(
                x0 >= -1e-6 && y0 >= -1e-6 && x1 <= 200.0 + 1e-6 && y1 <= 100.0 + 1e-6,
                "{name}: {x0} {y0} {x1} {y1}"
            );
        }
        assert!(preset("noSuchPreset", 10.0, 10.0, [None, None]).is_none());
    }

    #[test]
    fn extreme_adjust_values_stay_finite_and_inside() {
        for adj in [f64::NAN, f64::INFINITY, -5.0e9, 5.0e9] {
            for name in ["roundRect", "triangle", "parallelogram", "rightArrow", "star5", "plus"] {
                let p = preset(name, 80.0, 40.0, [Some(adj), Some(adj)]).expect("known");
                let (x0, y0, x1, y1) = bounds(&p.segs);
                assert!(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite());
                assert!(x0 >= -1e-6 && y0 >= -1e-6 && x1 <= 80.0 + 1e-6 && y1 <= 40.0 + 1e-6);
            }
        }
    }

    #[test]
    fn a_quarter_arc_ends_where_the_circle_does() {
        // Start at the rightmost point of a circle of radius 10 around (0, 0); sweep a quarter
        // clockwise on the screen (y down): the end is the bottom point.
        let mut out = Vec::new();
        let end = arc_to(&mut out, (10.0, 0.0), 10.0, 10.0, 0.0, 90.0);
        assert!((end.0).abs() < 1e-9 && (end.1 - 10.0).abs() < 1e-9, "{end:?}");
        assert_eq!(out.len(), 1);
        // A full circle comes back to the start.
        let mut out = Vec::new();
        let end = arc_to(&mut out, (10.0, 0.0), 10.0, 10.0, 0.0, 360.0);
        assert!((end.0 - 10.0).abs() < 1e-9 && end.1.abs() < 1e-9, "{end:?}");
    }
}
