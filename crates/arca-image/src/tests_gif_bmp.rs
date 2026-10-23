#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]

//! GIF and BMP tests.

use super::*;
use crate::testkit_img::{GifSpec, bmp, gif};

fn lim() -> Limits {
    Limits::default()
}

fn pix(img: &Image, x: usize, y: usize) -> [u8; 4] {
    let at = (y * img.width as usize + x) * 4;
    [img.rgba[at], img.rgba[at + 1], img.rgba[at + 2], img.rgba[at + 3]]
}

fn gray_palette(n: usize) -> Vec<[u8; 3]> {
    (0..n)
        .map(|i| [(i * 255 / (n - 1)) as u8, (255 - i * 255 / (n - 1)) as u8, (i * 7) as u8])
        .collect()
}

#[test]
fn gif_decodes_with_and_without_interlace() {
    for (w, h, colors) in [(13usize, 11usize, 4usize), (40, 33, 16), (70, 50, 256)] {
        for interlace in [false, true] {
            let palette = gray_palette(colors);
            let f = |x: usize, y: usize| ((x * 3 + y * 5) % colors) as u8;
            let spec = GifSpec { w, h, palette: palette.clone(), interlace, ..GifSpec::default() };
            let img = decode(&gif(&spec, &f), &lim())
                .unwrap_or_else(|e| panic!("{w}x{h} {colors} {interlace}: {e}"));
            assert_eq!((img.width as usize, img.height as usize), (w, h));
            for (x, y) in [(0, 0), (w - 1, h - 1), (w / 2, h / 3), (5, 2)] {
                let p = palette[f(x, y) as usize];
                assert_eq!(
                    pix(&img, x, y),
                    [p[0], p[1], p[2], 255],
                    "{w}x{h} c{colors} i{interlace} ({x},{y})"
                );
            }
            assert_eq!(img.facts.has(flag::INTERLACED), interlace);
        }
    }
}

#[test]
fn gif_transparency_canvas_animation_and_what_it_carries() {
    let palette = gray_palette(4);
    let spec = GifSpec {
        w: 4,
        h: 3,
        palette: palette.clone(),
        transparent: Some(2),
        canvas: Some((10, 8, 3, 2)),
        extra_frames: 2,
        comment: Some(b"hello there".to_vec()),
        trailing: b"MZ appended".to_vec(),
        ..GifSpec::default()
    };
    let data = gif(&spec, &|x, y| ((x + y) % 4) as u8);
    let img = decode(&data, &lim()).unwrap();
    assert_eq!((img.width, img.height), (10, 8));
    assert_eq!(pix(&img, 0, 0)[3], 0, "outside the frame is transparent");
    assert_eq!(pix(&img, 3, 2)[3], 255, "index 0 is opaque");
    assert_eq!(pix(&img, 5, 2)[3], 0, "index 2 is the transparent colour");
    assert!(
        img.facts.has(flag::ANIMATED)
            && img.facts.has(flag::COMMENT)
            && img.facts.has(flag::HAS_ALPHA)
    );
    assert_eq!(img.facts.frames, 3);
    assert_eq!(img.facts.trailing, 11);
    assert_eq!(img.facts.metadata_bytes, 11);
}

#[test]
fn gif_lzw_survives_a_full_dictionary_and_long_runs() {
    // Enough varied data to fill the 4096-entry table and force clear codes.
    let spec = GifSpec { w: 200, h: 200, palette: gray_palette(256), ..GifSpec::default() };
    let f = |x: usize, y: usize| ((x * x + y * 31 + x * y) % 251) as u8;
    let img = decode(&gif(&spec, &f), &lim()).unwrap();
    assert!(!img.facts.has(flag::DAMAGED));
    let p = gray_palette(256)[f(150, 77) as usize];
    assert_eq!(pix(&img, 150, 77), [p[0], p[1], p[2], 255]);
    // A solid frame (long runs, KwKwK patterns).
    let spec = GifSpec { w: 100, h: 100, palette: gray_palette(2), ..GifSpec::default() };
    let img = decode(&gif(&spec, &|_, _| 1), &lim()).unwrap();
    assert_eq!(pix(&img, 99, 99), [255, 0, 7, 255]);
}

fn lcg(x: &mut u32) {
    *x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
}

#[test]
fn gif_limits_truncation_and_garbage() {
    let spec = GifSpec { w: 30, h: 20, palette: gray_palette(4), ..GifSpec::default() };
    let data = gif(&spec, &|x, y| ((x + y) % 4) as u8);
    // Claims 30000 × 30000.
    let mut huge = data.clone();
    huge[6..8].copy_from_slice(&30_000u16.to_le_bytes());
    huge[8..10].copy_from_slice(&30_000u16.to_le_bytes());
    assert!(matches!(decode(&huge, &lim()), Err(DecodeError::TooLarge(_))));
    // Cut in the middle of the image data: shows what is there.
    let cut = &data[..data.len() - 20];
    let img = decode(cut, &lim()).unwrap();
    assert!(img.facts.has(flag::DAMAGED));
    assert_eq!(img.rgba.len(), 30 * 20 * 4);
    // Mutations.
    let mut x = 0xC0FF_EE11u32;
    for _ in 0..3000 {
        let mut d = data.clone();
        for _ in 0..1 + (x >> 29) as usize {
            lcg(&mut x);
            let i = (x >> 8) as usize % d.len();
            d[i] = (x >> 24) as u8;
        }
        let l = Limits {
            max_stream_pixels: 1 << 22,
            max_buffered_pixels: 1 << 22,
            max_side: 4096,
            ..lim()
        };
        if let Ok(img) = decode(&d, &l) {
            assert_eq!(img.rgba.len(), img.width as usize * img.height as usize * 4);
        }
    }
}

fn rows_of(w: usize, h: usize, pack: impl Fn(usize, usize) -> Vec<u8>) -> Vec<Vec<u8>> {
    (0..h).map(|y| (0..w).flat_map(|x| pack(x, y)).collect()).collect()
}

#[test]
fn bmp_24_and_32_bit_bottom_up_and_top_down() {
    let (w, h) = (9usize, 6usize);
    let rgb = |x: usize, y: usize| [(x * 20) as u8, (y * 40) as u8, 77u8];
    for top_down in [false, true] {
        let hs = if top_down { -(h as i32) } else { h as i32 };
        let rows = rows_of(w, h, |x, y| {
            let p = rgb(x, y);
            vec![p[2], p[1], p[0]]
        });
        let img = decode(&bmp(w as i32, hs, 24, 0, &[], None, &rows), &lim()).unwrap();
        assert_eq!((img.width as usize, img.height as usize), (w, h));
        assert_eq!(pix(&img, 4, 1), [80, 40, 77, 255], "top_down {top_down}");
        assert_eq!(pix(&img, 8, 5), [160, 200, 77, 255]);
        // 32 bits, no alpha mask: opaque even though the fourth byte is zero.
        let rows = rows_of(w, h, |x, y| {
            let p = rgb(x, y);
            vec![p[2], p[1], p[0], 0]
        });
        let img = decode(&bmp(w as i32, hs, 32, 0, &[], None, &rows), &lim()).unwrap();
        assert_eq!(pix(&img, 4, 1), [80, 40, 77, 255]);
    }
}

#[test]
fn bmp_palette_depths_and_16_bit_masks() {
    let (w, h) = (11usize, 4usize);
    // 8 bits.
    let palette: Vec<[u8; 3]> = (0..16).map(|i| [i as u8 * 16, 255 - i as u8 * 16, 9]).collect();
    let rows = rows_of(w, h, |x, y| vec![((x + y) % 16) as u8]);
    let img = decode(&bmp(w as i32, h as i32, 8, 0, &palette, None, &rows), &lim()).unwrap();
    assert_eq!(pix(&img, 3, 2), [80, 175, 9, 255]);
    assert!(img.facts.has(flag::PALETTE));
    // 4 bits (two pixels per byte) and 1 bit.
    let rows4: Vec<Vec<u8>> = (0..h)
        .map(|y| {
            (0..w.div_ceil(2))
                .map(|i| ((((i * 2 + y) % 16) << 4) | ((i * 2 + 1 + y) % 16)) as u8)
                .collect()
        })
        .collect();
    let img = decode(&bmp(w as i32, h as i32, 4, 0, &palette, None, &rows4), &lim()).unwrap();
    assert_eq!(pix(&img, 2, 1), [48, 207, 9, 255]);
    assert_eq!(pix(&img, 3, 1), [64, 191, 9, 255]);
    let pal2 = vec![[0u8, 0, 0], [255, 255, 255]];
    let rows1: Vec<Vec<u8>> = (0..h).map(|_| vec![0b1010_1010, 0b1010_0000]).collect();
    let img = decode(&bmp(w as i32, h as i32, 1, 0, &pal2, None, &rows1), &lim()).unwrap();
    assert_eq!((pix(&img, 0, 0)[0], pix(&img, 1, 0)[0], pix(&img, 2, 0)[0]), (255, 0, 255));
    // 16 bits: 5-5-5 by default, 5-6-5 with bit fields.
    let rows = rows_of(w, h, |_, _| 0b0_11111_00000_01111u16.to_le_bytes().to_vec());
    let img = decode(&bmp(w as i32, h as i32, 16, 0, &[], None, &rows), &lim()).unwrap();
    assert_eq!(pix(&img, 0, 0), [255, 0, 123, 255]);
    let rows = rows_of(w, h, |_, _| 0b1111_1000_0000_1111_u16.to_le_bytes().to_vec());
    let masks = Some([0xF800, 0x07E0, 0x001F, 0]);
    let img = decode(&bmp(w as i32, h as i32, 16, 3, &[], masks, &rows), &lim()).unwrap();
    assert_eq!(pix(&img, 0, 0), [255, 0, 123, 255]);
}

#[test]
fn bmp_rle8_and_limits_and_trailing() {
    // RLE8: a run of 5 x index 1, end of line, absolute run of 3, end of line, end of bitmap.
    let palette = vec![[0u8, 0, 0], [200, 100, 50], [1, 2, 3], [4, 5, 6]];
    let mut data = bmp(5, 2, 8, 1, &palette, None, &[]);
    let offset = u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize;
    data.truncate(offset);
    data.extend_from_slice(&[5, 1, 0, 0, 0, 3, 2, 3, 2, 0, 0, 1, 0, 1]);
    let img = decode(&data, &lim()).unwrap();
    // Bottom-up: the first coded row is the bottom row.
    assert_eq!(pix(&img, 0, 1), [200, 100, 50, 255]);
    assert_eq!(pix(&img, 0, 0), [1, 2, 3, 255]);
    assert_eq!(pix(&img, 1, 0), [4, 5, 6, 255]);
    // Impossible sizes and a lying header.
    let rows = rows_of(2, 2, |_, _| vec![1, 2, 3]);
    let ok = bmp(2, 2, 24, 0, &[], None, &rows);
    let mut huge = ok.clone();
    huge[18..22].copy_from_slice(&40_000i32.to_le_bytes());
    assert!(matches!(decode(&huge, &lim()), Err(DecodeError::TooLarge(_))));
    let mut zero = ok.clone();
    zero[18..22].copy_from_slice(&0i32.to_le_bytes());
    assert!(decode(&zero, &lim()).is_err());
    // A header claiming more rows than the file holds: blank, flagged, no huge allocation.
    let mut tall = ok.clone();
    tall[22..26].copy_from_slice(&20_000i32.to_le_bytes());
    let img = decode(&tall, &lim()).unwrap();
    assert!(img.facts.has(flag::DAMAGED));
    // Trailing data after the pixel array.
    let mut t = ok;
    t.extend_from_slice(b"%PDF-1.4 hidden");
    assert_eq!(inspect(&t, &lim()).unwrap().trailing, 15);
    // A bitmap that holds a JPEG is refused by name.
    let mut jpeg_in_bmp = t.clone();
    jpeg_in_bmp[30..34].copy_from_slice(&4u32.to_le_bytes());
    assert!(matches!(decode(&jpeg_in_bmp, &lim()), Err(DecodeError::Unsupported(_))));
}

#[test]
fn mutated_bmps_never_panic() {
    let palette: Vec<[u8; 3]> = (0..16).map(|i| [i as u8 * 16, 0, 9]).collect();
    let rows = rows_of(17, 9, |x, y| vec![((x + y) % 16) as u8]);
    let base = bmp(17, 9, 8, 0, &palette, None, &rows);
    let mut x = 0xBEEF_0042u32;
    for _ in 0..4000 {
        let mut d = base.clone();
        for _ in 0..1 + (x >> 29) as usize {
            lcg(&mut x);
            let i = (x >> 8) as usize % d.len();
            d[i] = (x >> 24) as u8;
        }
        lcg(&mut x);
        if x & 7 == 0 {
            d.truncate((x >> 8) as usize % (d.len() + 1));
        }
        let l = Limits {
            max_stream_pixels: 1 << 22,
            max_buffered_pixels: 1 << 22,
            max_side: 4096,
            ..lim()
        };
        if let Ok(img) = decode(&d, &l) {
            assert_eq!(img.rgba.len(), img.width as usize * img.height as usize * 4);
        }
    }
}
