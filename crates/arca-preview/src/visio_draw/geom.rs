//! Geometry sections to paths.
//!
//! The rows of a Geometry section (MoveTo, LineTo, ArcTo, EllipticalArcTo, Ellipse, PolylineTo,
//! NURBSTo, SplineStart/SplineKnot, InfiniteLine and the relative rows of `.vsdx`) are turned into
//! line and cubic Bezier segments in the shape's own coordinates (inches, y up). The meaning of
//! the cells is that of the Visio SDK reference ("Geometry section", one topic per row type):
//!
//! * `ArcTo`: `X`,`Y` end point, `A` the distance from the arc's midpoint to its chord's midpoint
//!   (the bow); a positive bow turns counter-clockwise (the centre lies to the left of the chord
//!   when the arc is travelled from start to end), a negative one clockwise.
//! * `EllipticalArcTo`: `X`,`Y` end, `A`,`B` a point on the arc, `C` the angle of the major axis to
//!   the x axis, `D` the ratio of the major to the minor axis. The ellipse that passes through the
//!   current point, `(A,B)` and `(X,Y)` is found by turning the plane by `-C` and stretching y by
//!   `D` (which makes the ellipse a circle through three points).
//! * `Ellipse`: `X`,`Y` the centre and `(A,B)`, `(C,D)` the ends of two conjugate semi-diameters.
//! * `PolylineTo`: `X`,`Y` the last point; the intermediate points are the numbers of the formula
//!   `POLYLINE(xType, yType, x1, y1, ...)`, relative to the shape's width and height when the type
//!   is 0 and absolute when it is 1.
//! * `NURBSTo`: `X`,`Y` the last control point, `A` the second-to-last knot, `B` the last weight,
//!   `C` the first knot, `D` the first weight and the formula
//!   `NURBS(lastKnot, degree, xType, yType, x1, y1, knot1, weight1, ...)`. The current point is the
//!   first control point. The curve is sampled (rational B-spline, Cox-de Boor), so it is an
//!   approximation by a polyline, and the page says so.
//! * `SplineStart` + `SplineKnot` rows: a B-spline of unit weights; sampled the same way.
//!
//! Which of these layouts match Microsoft's own files was checked against the SDK reference only
//! (not against files made by Visio), so splines are reported as approximated.

use super::model::{Row, RowKind, num};

/// A path segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Seg {
    M(f64, f64),
    L(f64, f64),
    C([f64; 6]),
    Z,
}

/// What was only approximated or cut while building a path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Flags {
    /// A NURBS curve was replaced by a sampled polyline.
    pub(crate) nurbs: bool,
    /// A spline was replaced by a sampled polyline.
    pub(crate) spline: bool,
    /// A polyline or NURBS formula was not a list of plain numbers; a straight line stands in.
    pub(crate) bad_formula: bool,
    /// The degree of a curve was reduced to the supported one.
    pub(crate) degree_capped: bool,
    /// A bound on segments or points cut the path.
    pub(crate) cut: bool,
}

/// Largest coordinate kept (inches).
const MAX_COORD: f64 = 1.0e7;
/// Highest degree of a spline that is evaluated.
const MAX_DEGREE: usize = 8;
/// Most control points of one curve.
const MAX_CONTROLS: usize = 2_000;
/// Most samples of one curve.
const MAX_SAMPLES: usize = 512;
/// Samples per distinct knot span.
const SAMPLES_PER_SPAN: usize = 16;
/// Most formula numbers read.
pub(crate) const MAX_ARGS: usize = 40_000;
const KAPPA: f64 = 0.552_284_749_830_793_4;

fn fin(v: f64) -> f64 {
    if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 }
}

fn g(r: &Row, name: &str) -> f64 {
    fin(num(&r.cells, name).unwrap_or(0.0))
}

/// The accumulating spline of `SplineStart` + `SplineKnot` rows.
struct SplineAcc {
    ctrl: Vec<(f64, f64)>,
    knots: Vec<f64>,
    degree: usize,
    last: f64,
}

/// Builds the segments of one Geometry section.
pub(crate) struct Builder<'a> {
    w: f64,
    h: f64,
    pub(crate) segs: Vec<Seg>,
    cur: (f64, f64),
    start: (f64, f64),
    open: bool,
    pub(crate) flags: Flags,
    /// Segments the whole page may still take.
    left: &'a mut usize,
    spline: Option<SplineAcc>,
    /// Half the length of the lines that stand for infinite ones.
    reach: f64,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(w: f64, h: f64, reach: f64, left: &'a mut usize) -> Self {
        Self {
            w: fin(w),
            h: fin(h),
            segs: Vec::new(),
            cur: (0.0, 0.0),
            start: (0.0, 0.0),
            open: false,
            flags: Flags::default(),
            left,
            spline: None,
            reach,
        }
    }

    fn room(&mut self) -> bool {
        if *self.left == 0 {
            self.flags.cut = true;
            return false;
        }
        *self.left -= 1;
        true
    }

    fn move_to(&mut self, x: f64, y: f64) {
        if self.room() {
            self.segs.push(Seg::M(x, y));
        }
        self.cur = (x, y);
        self.start = (x, y);
        self.open = true;
    }

    fn line_to(&mut self, x: f64, y: f64) {
        if !self.open {
            self.move_to(x, y);
            return;
        }
        if self.room() {
            self.segs.push(Seg::L(x, y));
        }
        self.cur = (x, y);
    }

    fn curve_to(&mut self, c: [f64; 6]) {
        if !self.open {
            self.move_to(c[0], c[1]);
        }
        if self.room() {
            self.segs.push(Seg::C(c));
        }
        self.cur = (c[4], c[5]);
    }

    fn close(&mut self) {
        if self.open && self.room() {
            self.segs.push(Seg::Z);
        }
        self.cur = self.start;
        self.open = false;
    }

    /// Adds one row.
    pub(crate) fn row(&mut self, r: &Row) {
        if !matches!(r.kind, RowKind::SplineKnot) {
            self.flush_spline();
        }
        if r.del {
            return;
        }
        let (x, y) = (g(r, "X"), g(r, "Y"));
        let (w, h) = (self.w, self.h);
        match r.kind {
            RowKind::MoveTo => self.move_to(x, y),
            RowKind::LineTo => self.line_to(x, y),
            RowKind::RelMoveTo => self.move_to(x * w, y * h),
            RowKind::RelLineTo => self.line_to(x * w, y * h),
            RowKind::ArcTo => self.arc_to(x, y, g(r, "A")),
            RowKind::EllipticalArcTo => {
                self.elliptical_arc_to(x, y, g(r, "A"), g(r, "B"), g(r, "C"), g(r, "D"));
            }
            RowKind::RelEllipticalArcTo => {
                self.elliptical_arc_to(
                    x * w,
                    y * h,
                    g(r, "A") * w,
                    g(r, "B") * h,
                    g(r, "C"),
                    g(r, "D"),
                );
            }
            RowKind::RelCubBezTo => {
                let (a, b, c, d) = (g(r, "A") * w, g(r, "B") * h, g(r, "C") * w, g(r, "D") * h);
                self.curve_to([a, b, c, d, x * w, y * h]);
            }
            RowKind::RelQuadBezTo => {
                let (p0x, p0y) = self.cur;
                let (qx, qy) = (g(r, "A") * w, g(r, "B") * h);
                let (ex, ey) = (x * w, y * h);
                // A quadratic Bezier as a cubic one.
                self.curve_to([
                    p0x + 2.0 / 3.0 * (qx - p0x),
                    p0y + 2.0 / 3.0 * (qy - p0y),
                    ex + 2.0 / 3.0 * (qx - ex),
                    ey + 2.0 / 3.0 * (qy - ey),
                    ex,
                    ey,
                ]);
            }
            RowKind::Ellipse => self.ellipse(x, y, (g(r, "A"), g(r, "B")), (g(r, "C"), g(r, "D"))),
            RowKind::InfiniteLine => self.infinite_line((x, y), (g(r, "A"), g(r, "B"))),
            RowKind::PolylineTo => self.polyline_to(r, x, y),
            RowKind::NurbsTo => self.nurbs_to(r, x, y),
            RowKind::SplineStart => {
                let degree = num(&r.cells, "D").unwrap_or(3.0).clamp(1.0, 25.0);
                self.spline = Some(SplineAcc {
                    ctrl: vec![self.cur, (x, y)],
                    knots: vec![g(r, "B"), g(r, "A")],
                    degree: float_to_usize(degree),
                    last: g(r, "C"),
                });
            }
            RowKind::SplineKnot => {
                let a = g(r, "A");
                if let Some(s) = self.spline.as_mut() {
                    if s.ctrl.len() < MAX_CONTROLS {
                        s.ctrl.push((x, y));
                        s.knots.push(a);
                    } else {
                        self.flags.cut = true;
                    }
                }
            }
        }
    }

    /// Ends the section: a pending spline is drawn.
    pub(crate) fn finish(&mut self) {
        self.flush_spline();
    }

    fn flush_spline(&mut self) {
        let Some(s) = self.spline.take() else { return };
        let mut knots = s.knots;
        knots.push(s.last);
        let weights = vec![1.0; s.ctrl.len()];
        self.flags.spline = true;
        self.sample(&s.ctrl, knots, &weights, s.degree);
    }

    // ---- arcs ----

    fn arc_to(&mut self, x: f64, y: f64, bow: f64) {
        let (x0, y0) = self.cur;
        let (dx, dy) = (x - x0, y - y0);
        let chord = dx.hypot(dy);
        if !self.open || bow == 0.0 || chord < 1e-12 {
            self.line_to(x, y);
            return;
        }
        let s = bow.abs();
        let r = (chord * chord / 4.0 + s * s) / (2.0 * s);
        // Distance from the chord to the centre, towards the arc's far side.
        let h = r - s;
        let half = (chord / 2.0).atan2(h);
        let sweep = 2.0 * half;
        // Unit normal to the left of the chord direction.
        let (nx, ny) = (-dy / chord, dx / chord);
        let (mx, my) = ((x0 + x) / 2.0, (y0 + y) / 2.0);
        // Counter-clockwise (positive bow): the centre is on the left, `h` away from the chord.
        let sign = if bow > 0.0 { 1.0 } else { -1.0 };
        let (cx, cy) = (mx + sign * nx * h, my + sign * ny * h);
        let a0 = (y0 - cy).atan2(x0 - cx);
        self.circle_arc((cx, cy), r, a0, sign * sweep, (x, y));
    }

    /// Adds the arc from angle `a0` over `sweep` of the circle, ending exactly at `end`.
    fn circle_arc(&mut self, c: (f64, f64), r: f64, a0: f64, sweep: f64, end: (f64, f64)) {
        let n = float_to_usize((sweep.abs() / core::f64::consts::FRAC_PI_2).ceil()).clamp(1, 8);
        let step = sweep / usize_to_f64(n);
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        for i in 0..n {
            let (b0, b1) = (a0 + step * usize_to_f64(i), a0 + step * usize_to_f64(i + 1));
            let (c0, s0, c1, s1) = (b0.cos(), b0.sin(), b1.cos(), b1.sin());
            let p1 = (c.0 + r * (c0 - k * s0), c.1 + r * (s0 + k * c0));
            let p2 = (c.0 + r * (c1 + k * s1), c.1 + r * (s1 - k * c1));
            let p3 = if i + 1 == n { end } else { (c.0 + r * c1, c.1 + r * s1) };
            self.curve_to([p1.0, p1.1, p2.0, p2.1, p3.0, p3.1]);
        }
    }

    fn elliptical_arc_to(&mut self, x: f64, y: f64, a: f64, b: f64, angle: f64, ratio: f64) {
        let (x0, y0) = self.cur;
        let ratio = if ratio.is_finite() && ratio > 0.0 && ratio <= 1000.0 { ratio } else { 1.0 };
        let angle = if angle.is_finite() { angle } else { 0.0 };
        let (c, s) = (angle.cos(), angle.sin());
        // The plane turned by `-angle` and stretched in y by `ratio`: the ellipse is a circle.
        let fwd = |px: f64, py: f64| (px * c + py * s, ratio * (py * c - px * s));
        let (p1, p2, p3) = (fwd(x0, y0), fwd(a, b), fwd(x, y));
        let det = 2.0 * (p1.0 * (p2.1 - p3.1) + p2.0 * (p3.1 - p1.1) + p3.0 * (p1.1 - p2.1));
        if !self.open || det.abs() < 1e-12 {
            self.line_to(x, y);
            return;
        }
        let sq = |p: (f64, f64)| p.0 * p.0 + p.1 * p.1;
        let (s1, s2, s3) = (sq(p1), sq(p2), sq(p3));
        let ux = (s1 * (p2.1 - p3.1) + s2 * (p3.1 - p1.1) + s3 * (p1.1 - p2.1)) / det;
        let uy = (s1 * (p3.0 - p2.0) + s2 * (p1.0 - p3.0) + s3 * (p2.0 - p1.0)) / det;
        let r = (p1.0 - ux).hypot(p1.1 - uy);
        if !r.is_finite() || r < 1e-12 {
            self.line_to(x, y);
            return;
        }
        let ang = |p: (f64, f64)| (p.1 - uy).atan2(p.0 - ux);
        let (t1, t2, t3) = (ang(p1), ang(p2), ang(p3));
        let tau = core::f64::consts::TAU;
        let ccw_mid = (t2 - t1).rem_euclid(tau);
        let ccw_end = (t3 - t1).rem_euclid(tau);
        let sweep = if ccw_mid <= ccw_end { ccw_end } else { ccw_end - tau };
        // Segments in the circle plane, mapped back by the inverse of `fwd`.
        let back = |q: (f64, f64)| {
            let yy = q.1 / ratio;
            (q.0 * c - yy * s, q.0 * s + yy * c)
        };
        let n = float_to_usize((sweep.abs() / core::f64::consts::FRAC_PI_2).ceil()).clamp(1, 8);
        let step = sweep / usize_to_f64(n);
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        for i in 0..n {
            let (b0, b1) = (t1 + step * usize_to_f64(i), t1 + step * usize_to_f64(i + 1));
            let (c0, s0, c1, s1) = (b0.cos(), b0.sin(), b1.cos(), b1.sin());
            let q1 = back((ux + r * (c0 - k * s0), uy + r * (s0 + k * c0)));
            let q2 = back((ux + r * (c1 + k * s1), uy + r * (s1 - k * c1)));
            let q3 = if i + 1 == n { (x, y) } else { back((ux + r * c1, uy + r * s1)) };
            self.curve_to([q1.0, q1.1, q2.0, q2.1, q3.0, q3.1]);
        }
    }

    fn ellipse(&mut self, cx: f64, cy: f64, u: (f64, f64), v: (f64, f64)) {
        // Conjugate semi-diameters: the ellipse is the image of the unit circle under (u-c, v-c).
        let (ux, uy) = (u.0 - cx, u.1 - cy);
        let (vx, vy) = (v.0 - cx, v.1 - cy);
        let pt = |t: f64, s: f64| (cx + ux * t + vx * s, cy + uy * t + vy * s);
        let (p0, p1, p2, p3) = (pt(1.0, 0.0), pt(0.0, 1.0), pt(-1.0, 0.0), pt(0.0, -1.0));
        self.move_to(p0.0, p0.1);
        let quarter = |a: (f64, f64), ta: (f64, f64), b: (f64, f64), tb: (f64, f64)| {
            [
                a.0 + ta.0 * KAPPA,
                a.1 + ta.1 * KAPPA,
                b.0 - tb.0 * KAPPA,
                b.1 - tb.1 * KAPPA,
                b.0,
                b.1,
            ]
        };
        // Tangent at angle t is d/dt (cos t * u + sin t * v).
        let (t0, t1) = ((vx, vy), (-ux, -uy));
        let (t2, t3) = ((-vx, -vy), (ux, uy));
        self.curve_to(quarter(p0, t0, p1, t1));
        self.curve_to(quarter(p1, t1, p2, t2));
        self.curve_to(quarter(p2, t2, p3, t3));
        self.curve_to(quarter(p3, t3, p0, t0));
        self.close();
    }

    fn infinite_line(&mut self, p: (f64, f64), q: (f64, f64)) {
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let len = dx.hypot(dy);
        if len < 1e-12 {
            return;
        }
        let (ux, uy) = (dx / len * self.reach, dy / len * self.reach);
        self.move_to(p.0 - ux, p.1 - uy);
        self.line_to(p.0 + ux, p.1 + uy);
        self.open = false;
    }

    // ---- polylines and splines ----

    fn polyline_to(&mut self, r: &Row, x: f64, y: f64) {
        let Some(args) = r.args.as_ref().filter(|a| a.len() >= 2) else {
            self.flags.bad_formula = r.args.is_none();
            self.line_to(x, y);
            return;
        };
        let (xt, yt) = (args.first().copied().unwrap_or(1.0), args.get(1).copied().unwrap_or(1.0));
        let (sx, sy) = (if xt == 0.0 { self.w } else { 1.0 }, if yt == 0.0 { self.h } else { 1.0 });
        let mut it = args.iter().skip(2);
        let mut taken = 0usize;
        while let (Some(px), Some(py)) = (it.next(), it.next()) {
            if taken >= MAX_CONTROLS * 5 {
                self.flags.cut = true;
                break;
            }
            taken += 1;
            self.line_to(fin(*px) * sx, fin(*py) * sy);
        }
        self.line_to(x, y);
    }

    fn nurbs_to(&mut self, r: &Row, x: f64, y: f64) {
        let Some(args) = r.args.as_ref().filter(|a| a.len() >= 4) else {
            self.flags.bad_formula = true;
            self.line_to(x, y);
            return;
        };
        let last_knot = args.first().copied().unwrap_or(1.0);
        let degree = float_to_usize(args.get(1).copied().unwrap_or(3.0).clamp(1.0, 25.0));
        let (xt, yt) = (args.get(2).copied().unwrap_or(1.0), args.get(3).copied().unwrap_or(1.0));
        let (sx, sy) = (if xt == 0.0 { self.w } else { 1.0 }, if yt == 0.0 { self.h } else { 1.0 });
        let mut ctrl = vec![self.cur];
        let mut knots = vec![g(r, "C")];
        let mut weights = vec![num(&r.cells, "D").filter(|w| w.is_finite()).unwrap_or(1.0)];
        let mut it = args.iter().skip(4);
        while let (Some(px), Some(py), Some(k), Some(wt)) =
            (it.next(), it.next(), it.next(), it.next())
        {
            if ctrl.len() >= MAX_CONTROLS {
                self.flags.cut = true;
                break;
            }
            ctrl.push((fin(*px) * sx, fin(*py) * sy));
            knots.push(fin(*k));
            weights.push(*wt);
        }
        ctrl.push((x, y));
        knots.push(g(r, "A"));
        knots.push(fin(last_knot));
        weights.push(num(&r.cells, "B").filter(|w| w.is_finite()).unwrap_or(1.0));
        self.flags.nurbs = true;
        if !self.open {
            self.move_to(self.cur.0, self.cur.1);
        }
        self.sample(&ctrl, knots, &weights, degree);
        self.line_to(x, y);
    }

    /// Samples a rational B-spline into line segments. The knots are made non-decreasing and are
    /// padded with the last knot to `controls + degree + 1`; a curve whose knots leave no
    /// parameter range is drawn as the polyline of its control points.
    fn sample(&mut self, ctrl: &[(f64, f64)], mut knots: Vec<f64>, weights: &[f64], degree: usize) {
        let mut degree = degree;
        if degree > MAX_DEGREE {
            degree = MAX_DEGREE;
            self.flags.degree_capped = true;
        }
        let n = ctrl.len();
        if n < 2 {
            return;
        }
        let want = n + degree + 1;
        for i in 1..knots.len() {
            let prev = knots.get(i - 1).copied().unwrap_or(0.0);
            if let Some(k) = knots.get_mut(i)
                && *k < prev
            {
                *k = prev;
            }
        }
        let last = knots.last().copied().unwrap_or(0.0);
        while knots.len() < want {
            knots.push(last);
        }
        knots.truncate(want);
        let lo = knots.get(degree).copied().unwrap_or(0.0);
        let hi = knots.get(n).copied().unwrap_or(0.0);
        if hi - lo <= 1e-12 || degree >= n {
            for p in ctrl.iter().skip(1) {
                self.line_to(p.0, p.1);
            }
            return;
        }
        let spans = knots.windows(2).filter(|w| match (w.first(), w.get(1)) {
            (Some(a), Some(b)) => (b - a).abs() > 1e-12,
            _ => false,
        });
        let spans = spans.count().max(1);
        let samples = (SAMPLES_PER_SPAN * spans).clamp(8, MAX_SAMPLES);
        for i in 1..=samples {
            let t = lo + (hi - lo) * usize_to_f64(i) / usize_to_f64(samples);
            let span = find_span(&knots, n, degree, t);
            let basis = basis_funs(&knots, span, degree, t);
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for (j, b) in basis.iter().enumerate() {
                let idx = span + j - degree;
                let (Some(p), w) = (ctrl.get(idx), weights.get(idx).copied().unwrap_or(1.0)) else {
                    continue;
                };
                let w = if w.is_finite() && w > 0.0 { w } else { 1.0 };
                sx += b * w * p.0;
                sy += b * w * p.1;
                sw += b * w;
            }
            if sw.abs() > 1e-12 {
                self.line_to(sx / sw, sy / sw);
            }
        }
    }
}

/// The knot span `k` (`degree <= k < n`) with `knots[k] <= t < knots[k+1]` (the last span for the
/// end of the range).
fn find_span(knots: &[f64], n: usize, degree: usize, t: f64) -> usize {
    let mut k = degree;
    while k + 1 < n {
        let next = knots.get(k + 1).copied().unwrap_or(f64::INFINITY);
        if t < next {
            break;
        }
        k += 1;
    }
    k
}

/// The `degree + 1` non-zero basis functions at `t` in span `span` (The NURBS Book, A2.2).
fn basis_funs(knots: &[f64], span: usize, degree: usize, t: f64) -> Vec<f64> {
    let mut n = vec![0.0; degree + 1];
    let mut left = vec![0.0; degree + 1];
    let mut right = vec![0.0; degree + 1];
    if let Some(first) = n.first_mut() {
        *first = 1.0;
    }
    for j in 1..=degree {
        let kl = knots.get((span + 1).wrapping_sub(j)).copied().unwrap_or(0.0);
        let kr = knots.get(span + j).copied().unwrap_or(0.0);
        if let Some(l) = left.get_mut(j) {
            *l = t - kl;
        }
        if let Some(r) = right.get_mut(j) {
            *r = kr - t;
        }
        let mut saved = 0.0;
        for r in 0..j {
            let rr = right.get(r + 1).copied().unwrap_or(0.0);
            let ll = left.get(j - r).copied().unwrap_or(0.0);
            let denom = rr + ll;
            let temp =
                if denom.abs() < 1e-14 { 0.0 } else { n.get(r).copied().unwrap_or(0.0) / denom };
            if let Some(slot) = n.get_mut(r) {
                *slot = saved + rr * temp;
            }
            saved = ll * temp;
        }
        if let Some(slot) = n.get_mut(j) {
            *slot = saved;
        }
    }
    n
}

#[allow(clippy::cast_precision_loss)] // counts are far below 2^52
fn usize_to_f64(v: usize) -> f64 {
    v as f64
}

/// A non-negative whole number from a float already clamped to a small range.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn float_to_usize(v: f64) -> usize {
    if v.is_finite() && v >= 0.0 { v.min(1.0e6) as usize } else { 0 }
}
