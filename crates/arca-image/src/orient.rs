//! EXIF orientation: turns the reduced RGBA image so that it is upright.

/// Applies EXIF orientation `o` (2..=8; anything else leaves the image alone).
/// Returns the new width, height and pixels.
pub(crate) fn apply(o: u8, w: u32, h: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    if !(2..=8).contains(&o) {
        return (w, h, rgba);
    }
    let (wu, hu) = (crate::bytes::to_usize(w), crate::bytes::to_usize(h));
    if wu.checked_mul(hu).and_then(|n| n.checked_mul(4)) != Some(rgba.len()) {
        return (w, h, rgba);
    }
    let swap = o >= 5;
    let (nw, nh) = if swap { (hu, wu) } else { (wu, hu) };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..hu {
        for x in 0..wu {
            // Destination coordinates of the source pixel (x, y).
            let (dx, dy) = match o {
                2 => (wu - 1 - x, y),
                3 => (wu - 1 - x, hu - 1 - y),
                4 => (x, hu - 1 - y),
                5 => (y, x),
                6 => (hu - 1 - y, x),
                7 => (hu - 1 - y, wu - 1 - x),
                _ => (y, wu - 1 - x),
            };
            let s = (y * wu + x) * 4;
            let d = (dy * nw + dx) * 4;
            if let (Some(src), Some(dst)) = (rgba.get(s..s + 4), out.get_mut(d..d + 4)) {
                dst.copy_from_slice(src);
            }
        }
    }
    (u32::try_from(nw).unwrap_or(w), u32::try_from(nh).unwrap_or(h), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_2x1_image_turns_like_exif_says() {
        // Two pixels: A B.
        let img = vec![1, 1, 1, 255, 2, 2, 2, 255];
        let first = |o| {
            let (w, h, px) = apply(o, 2, 1, img.clone());
            (w, h, px.iter().step_by(4).copied().collect::<Vec<_>>())
        };
        assert_eq!(first(1), (2, 1, vec![1, 2]));
        assert_eq!(first(2), (2, 1, vec![2, 1]));
        assert_eq!(first(3), (2, 1, vec![2, 1]));
        assert_eq!(first(4), (2, 1, vec![1, 2]));
        assert_eq!(first(5), (1, 2, vec![1, 2]));
        assert_eq!(first(6), (1, 2, vec![1, 2]));
        assert_eq!(first(7), (1, 2, vec![2, 1]));
        assert_eq!(first(8), (1, 2, vec![2, 1]));
    }

    #[test]
    fn a_wrong_buffer_is_returned_unchanged() {
        let (w, h, px) = apply(6, 3, 3, vec![0; 5]);
        assert_eq!((w, h, px.len()), (3, 3, 5));
    }
}
