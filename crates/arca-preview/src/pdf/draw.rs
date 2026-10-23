//! What a page draws, recorded while its content stream is interpreted.
//!
//! The interpreter calls into [`Rec`] as it meets path, colour, clip, text and picture operators.
//! Everything is first kept in the page's own coordinates (points, y up); [`Rec::finish`] maps it
//! to the page as it is shown (origin at the top left, y down, the page's rotation applied).

use super::content::Mat;
use crate::page::{MAX_PATH_NUMBERS, MAX_PRIMS, PageModel, Prim, argb, op};

/// A colour as red, green, blue in 0..=1.
pub(super) type Rgb = [f64; 3];

/// Black.
pub(super) const BLACK: Rgb = [0.0, 0.0, 0.0];

/// Converts CMYK to RGB the simple way (what most viewers do without a colour profile).
pub(super) fn cmyk(c: f64, m: f64, y: f64, k: f64) -> Rgb {
    let k = 1.0 - k.clamp(0.0, 1.0);
    [(1.0 - c.clamp(0.0, 1.0)) * k, (1.0 - m.clamp(0.0, 1.0)) * k, (1.0 - y.clamp(0.0, 1.0)) * k]
}

/// The recorder.
#[derive(Debug, Default)]
pub(super) struct Rec {
    pub(super) prims: Vec<Prim>,
    /// The path under construction: `op` codes and coordinates in page space.
    path: Vec<f32>,
    cur: [f64; 2],
    start: [f64; 2],
    open: bool,
    /// Bounding box of the path under construction: `[x0, y0, x1, y1]`.
    bbox: Option<[f64; 4]>,
    /// A clip operator was seen; the next painting (or end) operator applies it.
    pub(super) clip_pending: bool,
    even_odd_clip: bool,
    /// The page was cut short by a limit.
    pub(super) full: bool,
    /// Something was only approximated (a gradient, a pattern).
    pub(super) approximate: bool,
}

#[allow(clippy::cast_possible_truncation)] // a coordinate on a page fits an f32
fn f(v: f64) -> f32 {
    v as f32
}

impl Rec {
    pub(super) fn push(&mut self, p: Prim) {
        if self.prims.len() >= MAX_PRIMS {
            self.full = true;
        } else {
            self.prims.push(p);
        }
    }

    fn grow(&mut self, x: f64, y: f64) {
        self.bbox = Some(match self.bbox {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }

    fn room(&mut self, numbers: usize) -> bool {
        if self.path.len().saturating_add(numbers) > MAX_PATH_NUMBERS {
            self.full = true;
            return false;
        }
        true
    }

    pub(super) fn move_to(&mut self, x: f64, y: f64) {
        if !self.room(3) {
            return;
        }
        self.path.extend([op::MOVE, f(x), f(y)]);
        self.cur = [x, y];
        self.start = [x, y];
        self.open = true;
        self.grow(x, y);
    }

    pub(super) fn line_to(&mut self, x: f64, y: f64) {
        if !self.open {
            self.move_to(x, y);
            return;
        }
        if !self.room(3) {
            return;
        }
        self.path.extend([op::LINE, f(x), f(y)]);
        self.cur = [x, y];
        self.grow(x, y);
    }

    pub(super) fn curve_to(&mut self, p1: [f64; 2], p2: [f64; 2], p3: [f64; 2]) {
        if !self.open {
            self.move_to(p1[0], p1[1]);
        }
        if !self.room(7) {
            return;
        }
        self.path.extend([op::CURVE, f(p1[0]), f(p1[1]), f(p2[0]), f(p2[1]), f(p3[0]), f(p3[1])]);
        self.cur = p3;
        for p in [p1, p2, p3] {
            self.grow(p[0], p[1]);
        }
    }

    pub(super) fn close(&mut self) {
        if self.open && self.room(1) {
            self.path.push(op::CLOSE);
            self.cur = self.start;
        }
    }

    /// The current point (for curves that start from it).
    pub(super) const fn current(&self) -> [f64; 2] {
        self.cur
    }

    /// A rectangle given by its four corners in page space.
    pub(super) fn rect(&mut self, corners: [[f64; 2]; 4]) {
        self.move_to(corners[0][0], corners[0][1]);
        for c in &corners[1..] {
            self.line_to(c[0], c[1]);
        }
        self.close();
    }

    pub(super) fn has_path(&self) -> bool {
        !self.path.is_empty()
    }

    /// Paints the path under construction.
    pub(super) fn paint(&mut self, fill: u32, stroke: u32, width: f64, even_odd: bool) {
        if self.has_path() && (fill >> 24 != 0 || stroke >> 24 != 0) {
            let ops = self.path.clone();
            self.push(Prim::Path { ops, fill, stroke, width: f(width), even_odd });
        }
    }

    /// Marks the next painting or path-ending operator as one that also clips.
    pub(super) fn will_clip(&mut self, even_odd: bool) {
        self.clip_pending = true;
        self.even_odd_clip = even_odd;
    }

    /// Ends the path: applies a pending clip (as the path's bounding box) and forgets the path.
    pub(super) fn end_path(&mut self) {
        if self.clip_pending {
            self.clip_pending = false;
            if let Some([x0, y0, x1, y1]) = self.bbox {
                self.push(Prim::Clip { x: f(x0), y: f(y0), w: f(x1 - x0), h: f(y1 - y0) });
            }
        }
        let _ = self.even_odd_clip;
        self.path.clear();
        self.bbox = None;
        self.open = false;
    }

    /// Turns the recorded page space into the page as it is shown.
    ///
    /// `view` maps page space to the shown page (origin top left, y down).
    pub(super) fn finish(
        self,
        view: Mat,
        width: f64,
        height: f64,
        index: u32,
        pages: u32,
    ) -> PageModel {
        let mut prims = Vec::with_capacity(self.prims.len());
        for p in self.prims {
            prims.push(match p {
                Prim::Path { mut ops, fill, stroke, width, even_odd } => {
                    let mut i = 0;
                    while i < ops.len() {
                        let n = match ops.get(i).copied() {
                            Some(c) if c == op::MOVE || c == op::LINE => 1,
                            Some(c) if c == op::CURVE => 3,
                            _ => 0,
                        };
                        for k in 0..n {
                            let at = i + 1 + 2 * k;
                            if let (Some(&x), Some(&y)) = (ops.get(at), ops.get(at + 1)) {
                                let (nx, ny) = view.apply(f64::from(x), f64::from(y));
                                if let Some(s) = ops.get_mut(at) {
                                    *s = f(nx);
                                }
                                if let Some(s) = ops.get_mut(at + 1) {
                                    *s = f(ny);
                                }
                            }
                        }
                        i += 1 + 2 * n;
                    }
                    Prim::Path { ops, fill, stroke, width, even_odd }
                }
                Prim::Text { x, y, size, angle, width, style, color, text } => {
                    let (nx, ny) = view.apply(f64::from(x), f64::from(y));
                    // `angle` was recorded as the direction in page space.
                    let (ux, uy) = (f64::from(angle).cos(), f64::from(angle).sin());
                    let (vx, vy) = (ux * view.a + uy * view.c, ux * view.b + uy * view.d);
                    Prim::Text {
                        x: f(nx),
                        y: f(ny),
                        size,
                        angle: f(vy.atan2(vx)),
                        width,
                        style,
                        color,
                        text,
                    }
                }
                Prim::Image { m, label, id } => {
                    let img = Mat {
                        a: f64::from(m[0]),
                        b: f64::from(m[1]),
                        c: f64::from(m[2]),
                        d: f64::from(m[3]),
                        e: f64::from(m[4]),
                        f: f64::from(m[5]),
                    }
                    .then(view);
                    Prim::Image {
                        m: [f(img.a), f(img.b), f(img.c), f(img.d), f(img.e), f(img.f)],
                        label,
                        id,
                    }
                }
                Prim::Clip { x, y, w, h } => {
                    let corners = [
                        view.apply(f64::from(x), f64::from(y)),
                        view.apply(f64::from(x + w), f64::from(y)),
                        view.apply(f64::from(x), f64::from(y + h)),
                        view.apply(f64::from(x + w), f64::from(y + h)),
                    ];
                    let xs = corners.iter().map(|c| c.0);
                    let ys = corners.iter().map(|c| c.1);
                    let (x0, x1) = (
                        xs.clone().fold(f64::INFINITY, f64::min),
                        xs.fold(f64::NEG_INFINITY, f64::max),
                    );
                    let (y0, y1) = (
                        ys.clone().fold(f64::INFINITY, f64::min),
                        ys.fold(f64::NEG_INFINITY, f64::max),
                    );
                    Prim::Clip { x: f(x0), y: f(y0), w: f(x1 - x0), h: f(y1 - y0) }
                }
                other => other,
            });
        }
        PageModel {
            width: f(width),
            height: f(height),
            index,
            pages,
            prims,
            truncated: self.full,
            approximate: self.approximate,
        }
    }
}

/// The transform from page space to the page as shown, for a page box and a rotation in degrees
/// (a multiple of 90), and the size of the shown page.
pub(super) fn view_of(visible: [f64; 4], rotate: i64) -> (Mat, f64, f64) {
    let [x0, y0, x1, y1] = visible;
    let (w, h) = ((x1 - x0).abs(), (y1 - y0).abs());
    let (x0, y0, x1, y1) = (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
    match rotate.rem_euclid(360) {
        90 => (Mat { a: 0.0, b: 1.0, c: 1.0, d: 0.0, e: -y0, f: -x0 }, h, w),
        180 => (Mat { a: -1.0, b: 0.0, c: 0.0, d: 1.0, e: x1, f: -y0 }, w, h),
        270 => (Mat { a: 0.0, b: -1.0, c: -1.0, d: 0.0, e: y1, f: x1 }, h, w),
        _ => (Mat { a: 1.0, b: 0.0, c: 0.0, d: -1.0, e: -x0, f: y1 }, w, h),
    }
}

/// A colour with an opacity as `0xAARRGGBB`.
pub(super) fn color(rgb: Rgb, alpha: f64) -> u32 {
    argb(rgb[0], rgb[1], rgb[2], alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_rotations_put_the_corners_where_a_viewer_does() {
        let page = [10.0, 20.0, 110.0, 220.0]; // 100 wide, 200 tall
        let bottom_left = (10.0, 20.0);
        let top_left = (10.0, 220.0);
        for (rotate, bl, tl, w, h) in [
            (0, (0.0, 200.0), (0.0, 0.0), 100.0, 200.0),
            (90, (0.0, 0.0), (200.0, 0.0), 200.0, 100.0),
            (180, (100.0, 0.0), (100.0, 200.0), 100.0, 200.0),
            (270, (200.0, 100.0), (0.0, 100.0), 200.0, 100.0),
            (-90, (200.0, 100.0), (0.0, 100.0), 200.0, 100.0),
        ] {
            let (m, ww, hh) = view_of(page, rotate);
            assert_eq!((ww, hh), (w, h), "size at {rotate}");
            let a = m.apply(bottom_left.0, bottom_left.1);
            let b = m.apply(top_left.0, top_left.1);
            assert!(
                (a.0 - bl.0).abs() < 1e-9 && (a.1 - bl.1).abs() < 1e-9,
                "bottom left at {rotate}: {a:?}"
            );
            assert!(
                (b.0 - tl.0).abs() < 1e-9 && (b.1 - tl.1).abs() < 1e-9,
                "top left at {rotate}: {b:?}"
            );
        }
    }

    #[test]
    fn cmyk_is_converted_without_a_profile() {
        assert_eq!(cmyk(0.0, 0.0, 0.0, 0.0), [1.0, 1.0, 1.0]);
        assert_eq!(cmyk(0.0, 0.0, 0.0, 1.0), [0.0, 0.0, 0.0]);
        assert_eq!(cmyk(1.0, 0.0, 0.0, 0.0), [0.0, 1.0, 1.0]);
    }
}
