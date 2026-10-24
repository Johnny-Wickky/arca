//! Row-by-row reduction of the decoded image to a bounded size.
//!
//! Decoders hand over one RGBA row at a time, top to bottom; the downscaler averages
//! `k × k` boxes (alpha-weighted, so a transparent pixel does not darken its neighbours) and
//! keeps only the reduced image. The full-size image therefore never exists in memory, which is
//! what makes a 60-megapixel photograph affordable inside the sandbox.

use crate::DecodeError;
use crate::bytes::to_usize;

/// Collects the rows of one image into a possibly reduced RGBA buffer.
#[derive(Debug)]
pub(crate) struct Downscaler {
    src_w: usize,
    src_h: usize,
    k: usize,
    ow: usize,
    oh: usize,
    /// Per output column: sums of `a`, `r·a`, `g·a`, `b·a` over the current group of rows.
    acc: Vec<u64>,
    rows_in_group: usize,
    rows_seen: usize,
    out: Vec<u8>,
}

impl Downscaler {
    /// A downscaler for an image of `src_w × src_h` whose longest side may not exceed `max_side`.
    pub(crate) fn new(src_w: u32, src_h: u32, max_side: u32) -> Result<Self, DecodeError> {
        let (w, h) = (to_usize(src_w), to_usize(src_h));
        if w == 0 || h == 0 {
            return Err(DecodeError::Malformed("the image has no pixels".into()));
        }
        let longest = w.max(h);
        let max_side = to_usize(max_side.max(1));
        let k = longest.div_ceil(max_side).max(1);
        let ow = w.div_ceil(k);
        let oh = h.div_ceil(k);
        let bytes = ow
            .checked_mul(oh)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| DecodeError::TooLarge("the reduced image is too large".into()))?;
        let mut out = Vec::new();
        out.try_reserve_exact(bytes)
            .map_err(|_| DecodeError::TooLarge("not enough memory for the image".into()))?;
        let mut acc = Vec::new();
        if k > 1 {
            acc.try_reserve_exact(ow * 4)
                .map_err(|_| DecodeError::TooLarge("not enough memory for the image".into()))?;
            acc.resize(ow * 4, 0);
        }
        Ok(Self { src_w: w, src_h: h, k, ow, oh, acc, rows_in_group: 0, rows_seen: 0, out })
    }

    /// Size of the reduced image.
    pub(crate) fn output_size(&self) -> (u32, u32) {
        (u32::try_from(self.ow).unwrap_or(u32::MAX), u32::try_from(self.oh).unwrap_or(u32::MAX))
    }

    /// Takes the next source row (`src_w` pixels of RGBA). Surplus rows are ignored.
    pub(crate) fn push(&mut self, row: &[u8]) {
        if self.rows_seen >= self.src_h {
            return;
        }
        self.rows_seen += 1;
        let Some(row) = row.get(..self.src_w * 4) else { return };
        if self.k == 1 {
            self.out.extend_from_slice(row);
            return;
        }
        for (x, px) in row.chunks_exact(4).enumerate() {
            let (r, g, b, a) = match px {
                [r, g, b, a] => (u64::from(*r), u64::from(*g), u64::from(*b), u64::from(*a)),
                _ => continue,
            };
            let at = x / self.k * 4;
            if let Some(cell) = self.acc.get_mut(at..at + 4)
                && let [sa, sr, sg, sb] = cell
            {
                *sa += a;
                *sr += r * a;
                *sg += g * a;
                *sb += b * a;
            }
        }
        self.rows_in_group += 1;
        if self.rows_in_group == self.k {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.rows_in_group == 0 {
            return;
        }
        let rows = self.rows_in_group;
        for ox in 0..self.ow {
            let first = ox * self.k;
            let cols = self.k.min(self.src_w.saturating_sub(first));
            let n = u64::try_from(rows * cols).unwrap_or(u64::MAX).max(1);
            let (sa, sr, sg, sb) = match self.acc.get(ox * 4..ox * 4 + 4) {
                Some([a, r, g, b]) => (*a, *r, *g, *b),
                _ => (0, 0, 0, 0),
            };
            if sa == 0 {
                self.out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let avg = |s: u64| u8::try_from((s + sa / 2) / sa).unwrap_or(255);
                let alpha = u8::try_from((sa + n / 2) / n).unwrap_or(255);
                self.out.extend_from_slice(&[avg(sr), avg(sg), avg(sb), alpha]);
            }
        }
        self.acc.iter_mut().for_each(|v| *v = 0);
        self.rows_in_group = 0;
    }

    /// The reduced image: width, height and RGBA bytes. Missing rows (a truncated file) are
    /// filled with transparent black so the buffer always matches the stated size.
    pub(crate) fn finish(mut self) -> (u32, u32, Vec<u8>) {
        if self.k > 1 {
            self.flush();
        }
        let want = self.ow * self.oh * 4;
        self.out.resize(want, 0);
        let (w, h) = self.output_size();
        (w, h, self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Vec<Vec<u8>> {
        (0..h).map(|y| (0..w).flat_map(|x| f(x, y)).collect()).collect()
    }

    #[test]
    fn small_images_pass_through() {
        let mut d = Downscaler::new(3, 2, 2048).unwrap();
        for r in rows(3, 2, |x, y| [u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 7, 255]) {
            d.push(&r);
        }
        let (w, h, px) = d.finish();
        assert_eq!((w, h, px.len()), (3, 2, 24));
        assert_eq!(&px[..4], &[0, 0, 7, 255]);
    }

    #[test]
    fn large_images_are_averaged_in_boxes() {
        let mut d = Downscaler::new(8, 8, 4).unwrap();
        for r in rows(8, 8, |x, _| if x % 2 == 0 { [0, 0, 0, 255] } else { [200, 100, 50, 255] }) {
            d.push(&r);
        }
        let (w, h, px) = d.finish();
        assert_eq!((w, h), (4, 4));
        assert_eq!(&px[..4], &[100, 50, 25, 255]);
    }

    #[test]
    fn transparent_pixels_do_not_darken_the_average() {
        let mut d = Downscaler::new(2, 2, 1).unwrap();
        for r in rows(2, 2, |x, _| if x == 0 { [255, 0, 0, 255] } else { [0, 0, 0, 0] }) {
            d.push(&r);
        }
        let (w, h, px) = d.finish();
        assert_eq!((w, h), (1, 1));
        assert_eq!(&px[..3], &[255, 0, 0]);
        assert!(px[3] > 100 && px[3] < 160);
    }

    #[test]
    fn odd_sizes_and_missing_rows_keep_the_buffer_consistent() {
        let mut d = Downscaler::new(5, 5, 2).unwrap();
        for r in rows(5, 3, |_, _| [9, 9, 9, 255]) {
            d.push(&r);
        }
        let (w, h, px) = d.finish();
        assert_eq!((w, h), (2, 2));
        assert_eq!(px.len(), 16);
    }

    #[test]
    fn zero_sized_images_are_refused() {
        assert!(Downscaler::new(0, 5, 10).is_err());
        assert!(Downscaler::new(5, 0, 10).is_err());
    }

    #[test]
    fn boxes_of_hundreds_of_pixels_a_side_do_not_overflow() {
        // k = 334: the sum of 334 × 334 pixels of 255 × 255 does not fit in 32 bits.
        let mut d = Downscaler::new(1000, 1000, 3).unwrap();
        let row = [255u8; 4000];
        for _ in 0..1000 {
            d.push(&row);
        }
        let (w, h, px) = d.finish();
        assert_eq!((w, h), (3, 3));
        assert!(px.chunks(4).all(|p| p == [255, 255, 255, 255]), "{px:?}");
    }
}
