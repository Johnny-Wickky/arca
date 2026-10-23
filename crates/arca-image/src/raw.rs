//! Pictures a document stores as plain samples (a PDF image without a codec): the caller turns
//! them into RGBA rows, this reduces them like any other picture while they are produced.

use crate::scale::Downscaler;
use crate::{DecodeError, Facts, Image, ImageFormat, Limits, flag};

/// Collects the rows of a picture of `width × height` into a reduced RGBA [`Image`].
#[derive(Debug)]
pub struct RowReducer {
    scaler: Downscaler,
    width: u32,
    height: u32,
}

impl RowReducer {
    /// A reducer for a picture of this size.
    ///
    /// # Errors
    /// [`DecodeError::TooLarge`] if a side or the number of pixels is beyond `limits`;
    /// [`DecodeError::Malformed`] for a picture without pixels.
    pub fn new(width: u32, height: u32, limits: &Limits) -> Result<Self, DecodeError> {
        if width > limits.max_side || height > limits.max_side {
            return Err(DecodeError::TooLarge(format!(
                "{width}×{height} pixels is beyond the limit of {} on a side",
                limits.max_side
            )));
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > limits.max_stream_pixels {
            return Err(DecodeError::TooLarge(format!(
                "{pixels} pixels is beyond the limit of {}",
                limits.max_stream_pixels
            )));
        }
        Ok(Self { scaler: Downscaler::new(width, height, limits.max_out_side)?, width, height })
    }

    /// The next row, top to bottom: `width` pixels of RGBA.
    pub fn push_row(&mut self, rgba: &[u8]) {
        self.scaler.push(rgba);
    }

    /// The reduced picture. Rows that were never pushed are transparent.
    #[must_use]
    pub fn finish(self, has_alpha: bool) -> Image {
        let (w, h, rgba) = self.scaler.finish();
        let mut facts = Facts::new(ImageFormat::Raw);
        facts.width = self.width;
        facts.height = self.height;
        if has_alpha {
            facts.flags |= flag::HAS_ALPHA;
        }
        if w < self.width || h < self.height {
            facts.flags |= flag::DOWNSCALED;
        }
        Image { width: w, height: h, rgba, facts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_reduced_and_limits_hold() {
        let limits = Limits { max_out_side: 4, ..Limits::default() };
        let mut r = RowReducer::new(8, 8, &limits).unwrap();
        for _ in 0..8 {
            r.push_row(&[10u8, 20, 30, 255].repeat(8));
        }
        let img = r.finish(false);
        assert_eq!((img.width, img.height, img.rgba.len()), (4, 4, 64));
        assert!(img.facts.has(flag::DOWNSCALED));
        assert_eq!(&img.rgba[..4], &[10, 20, 30, 255]);
        assert!(RowReducer::new(0, 5, &limits).is_err());
        assert!(matches!(RowReducer::new(40_000, 1, &limits), Err(DecodeError::TooLarge(_))));
        let small = Limits { max_stream_pixels: 100, ..Limits::default() };
        assert!(matches!(RowReducer::new(11, 10, &small), Err(DecodeError::TooLarge(_))));
    }
}
