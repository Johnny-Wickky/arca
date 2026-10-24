#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test-only arithmetic on small values"
)]

//! The PNG writer: what it writes is read back exactly, and is made of nothing else.

use super::*;

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// The chunk types of a PNG file, in order.
fn chunk_kinds(png: &[u8]) -> Vec<String> {
    let mut kinds = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        kinds.push(String::from_utf8_lossy(&png[at + 4..at + 8]).into_owned());
        at += 12 + len;
    }
    assert_eq!(at, png.len(), "no bytes after the last chunk");
    kinds
}

fn big() -> Limits {
    Limits { max_out_side: 30_000, ..Limits::default() }
}

/// Writes `rgba`, reads it back, and returns the file and the colour type it chose.
fn round_trip(w: u32, h: u32, rgba: &[u8]) -> (Vec<u8>, u8) {
    let (png, _) = encode_png(w, h, rgba).unwrap();
    let back = decode(&png, &big()).unwrap();
    assert_eq!((back.width, back.height), (w, h));
    assert!(back.rgba == rgba, "the pixels come back exactly");
    assert_eq!(back.facts.trailing, 0);
    assert_eq!(back.facts.metadata_bytes, 0);
    assert_eq!(chunk_kinds(&png).first().map(String::as_str), Some("IHDR"));
    assert_eq!(chunk_kinds(&png).last().map(String::as_str), Some("IEND"));
    let colour_type = png[8 + 8 + 9];
    (png, colour_type)
}

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// Random RGBA that is never fully transparent (so nothing is cleared on the way).
fn visible(n: usize, seed: u32) -> Vec<u8> {
    let mut v = noise(n, seed);
    for p in v.chunks_exact_mut(4) {
        p[3] |= 1;
    }
    v
}

#[test]
fn every_colour_type_is_written_and_read_back_exactly() {
    for (w, h) in [(1u32, 1u32), (2, 3), (7, 5), (64, 33), (300, 2)] {
        let n = (w * h) as usize;
        // Grey, opaque.
        let g: Vec<u8> = noise(n, w).iter().flat_map(|&v| [v, v, v, 255]).collect();
        assert_eq!(round_trip(w, h, &g).1, 0, "{w}x{h}");
        // Grey with alpha.
        let a = noise(n, h);
        let ga: Vec<u8> =
            noise(n, w + h).iter().zip(&a).flat_map(|(&v, &a)| [v, v, v, a | 1]).collect();
        assert_eq!(round_trip(w, h, &ga).1, 4, "{w}x{h}");
        // Colour, opaque.
        let c: Vec<u8> =
            noise(n * 3, 5).chunks(3).flat_map(|p| [p[0], p[1], p[2] ^ 1, 255]).collect();
        assert_eq!(round_trip(w, h, &c).1, 2, "{w}x{h}");
        // Colour with alpha (never fully transparent, so no colour is cleared).
        let ca: Vec<u8> =
            noise(n * 4, 9).chunks(4).flat_map(|p| [p[0], p[1], p[2] ^ 1, p[3] | 1]).collect();
        assert_eq!(round_trip(w, h, &ca).1, 6, "{w}x{h}");
    }
}

#[test]
fn smooth_and_flat_pictures_use_every_filter_and_still_round_trip() {
    // A gradient (Sub/Up/Paeth territory), flat colour, and vertical stripes.
    let (w, h) = (97u32, 61u32);
    let mut grad = Vec::new();
    let mut stripes = Vec::new();
    for y in 0..h {
        for x in 0..w {
            grad.extend_from_slice(&[(x * 2) as u8, (y * 4) as u8, ((x + y) * 2) as u8, 255]);
            let v = if x % 5 < 2 { 250 } else { 10 };
            stripes.extend_from_slice(&[v, v ^ 0x55, 40, 255]);
        }
    }
    let flat = vec![[12u8, 34, 56, 255]; (w * h) as usize].concat();
    for px in [&grad, &stripes, &flat] {
        let (png, _) = round_trip(w, h, px);
        assert!(png.len() < px.len() / 2, "{} of {}", png.len(), px.len());
    }
}

#[test]
fn only_the_header_the_pixels_and_the_end_are_written() {
    let (png, _) = round_trip(3, 3, &visible(36, 1));
    assert!(chunk_kinds(&png).iter().all(|k| ["IHDR", "IDAT", "IEND"].contains(&k.as_str())));
    // A picture large enough for several IDAT chunks is still only those three kinds.
    let (w, h) = (700u32, 700u32);
    let (png, _) = round_trip(w, h, &visible((w * h * 4) as usize, 3));
    let kinds = chunk_kinds(&png);
    assert!(kinds.iter().filter(|k| *k == "IDAT").count() > 1);
    assert!(kinds.iter().all(|k| ["IHDR", "IDAT", "IEND"].contains(&k.as_str())));
}

#[test]
fn colour_hidden_in_transparent_pixels_is_cleared() {
    let mut px = vec![255u8; 4 * 4];
    px[..4].copy_from_slice(&[10, 20, 30, 0]);
    px[4..8].copy_from_slice(&[0, 0, 0, 0]);
    let (png, notes) = encode_png(2, 2, &px).unwrap();
    assert!(notes.cleared_hidden_colour);
    let back = decode(&png, &big()).unwrap();
    assert_eq!(&back.rgba[..8], &[0; 8]);
    assert_eq!(&back.rgba[8..], &px[8..]);
    // Nothing hidden: nothing to report.
    let (_, notes) = encode_png(2, 2, &[7, 8, 9, 255].repeat(4)).unwrap();
    assert!(!notes.cleared_hidden_colour);
}

#[test]
fn a_wrong_buffer_is_refused() {
    assert!(encode_png(0, 1, &[]).is_err());
    assert!(encode_png(1, 0, &[]).is_err());
    assert!(encode_png(2, 2, &[0; 15]).is_err());
    assert!(encode_png(2, 2, &[0; 17]).is_err());
    assert!(encode_png(u32::MAX, u32::MAX, &[0; 4]).is_err());
}

#[test]
fn the_pictures_of_the_fixtures_come_back_as_clean_files() {
    for name in ["img-420.jpg", "img-444-prog.jpg", "img-gray.jpg", "img-420.ref.png"] {
        let img = decode(&fixture(name), &big()).unwrap();
        let (png, _) = encode_png(img.width, img.height, &img.rgba).unwrap();
        let back = decode(&png, &big()).unwrap();
        assert!(back.rgba == img.rgba, "{name}");
        assert_eq!(back.facts.format, ImageFormat::Png);
        assert_eq!(back.facts.exif, None, "{name}");
        assert_eq!(back.facts.flags & (flag::ICC_PROFILE | flag::XMP | flag::TEXT_CHUNKS), 0);
        assert_eq!(back.facts.trailing, 0, "{name}");
    }
}
