#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]

//! Tests of the public API: formats, limits, hostile input.

use super::*;
use crate::exif::tests::{V, build};
use crate::testkit::{PngSpec, png};
use crate::testkit_img::bmp;

fn lim() -> Limits {
    Limits::default()
}

fn pix(img: &Image, x: usize, y: usize) -> [u8; 4] {
    let at = (y * img.width as usize + x) * 4;
    [img.rgba[at], img.rgba[at + 1], img.rgba[at + 2], img.rgba[at + 3]]
}

/// A pixel pattern that differs from pixel to pixel in every channel.
fn pattern(x: usize, y: usize, max: u16, ch: usize) -> Vec<u16> {
    (0..ch).map(|c| ((x * 7 + y * 13 + c * 29) as u32 % (u32::from(max) + 1)) as u16).collect()
}

#[test]
fn png_rgb8_with_every_filter() {
    for filter in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
        let spec = PngSpec { w: 37, h: 23, depth: 8, color: 2, filter, ..PngSpec::default() };
        let data = png(&spec, &|x, y| pattern(x, y, 255, 3));
        let img = decode(&data, &lim()).unwrap();
        assert_eq!((img.width, img.height), (37, 23));
        for (x, y) in [(0, 0), (36, 22), (5, 17), (20, 3)] {
            let p = pattern(x, y, 255, 3);
            assert_eq!(
                pix(&img, x, y),
                [p[0] as u8, p[1] as u8, p[2] as u8, 255],
                "{filter:?} {x},{y}"
            );
        }
        assert_eq!(img.facts.trailing, 0);
        assert!(!img.facts.has(flag::DAMAGED));
    }
}

#[test]
fn png_every_colour_type_and_depth() {
    let cases: &[(u8, u8)] = &[
        (0, 1),
        (0, 2),
        (0, 4),
        (0, 8),
        (0, 16),
        (2, 8),
        (2, 16),
        (3, 1),
        (3, 2),
        (3, 4),
        (3, 8),
        (4, 8),
        (4, 16),
        (6, 8),
        (6, 16),
    ];
    for &(color, depth) in cases {
        let max = if depth == 16 { 65_535 } else { (1u16 << depth) - 1 };
        let ch = match color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        };
        let palette: Vec<[u8; 3]> = if color == 3 {
            (0..(1usize << depth).min(256))
                .map(|i| [i as u8, (i * 3) as u8, (255 - i) as u8])
                .collect()
        } else {
            vec![]
        };
        for interlace in [false, true] {
            let spec = PngSpec {
                w: 19,
                h: 11,
                depth,
                color,
                interlace,
                palette: palette.clone(),
                ..PngSpec::default()
            };
            let data = png(&spec, &|x, y| pattern(x, y, max, ch));
            let img = decode(&data, &lim())
                .unwrap_or_else(|e| panic!("{color}/{depth}/{interlace}: {e}"));
            assert_eq!((img.width, img.height), (19, 11), "{color}/{depth}");
            let (x, y) = (11, 7);
            let v = pattern(x, y, max, ch);
            let to8 = |s: u16| -> u8 {
                if depth == 16 {
                    (s >> 8) as u8
                } else if depth == 8 {
                    s as u8
                } else {
                    (u32::from(s) * 255 / u32::from(max)) as u8
                }
            };
            let expect = match color {
                0 => [to8(v[0]), to8(v[0]), to8(v[0]), 255],
                2 => [to8(v[0]), to8(v[1]), to8(v[2]), 255],
                3 => {
                    let p = palette[v[0] as usize];
                    [p[0], p[1], p[2], 255]
                }
                4 => [to8(v[0]), to8(v[0]), to8(v[0]), to8(v[1])],
                _ => [to8(v[0]), to8(v[1]), to8(v[2]), to8(v[3])],
            };
            assert_eq!(
                pix(&img, x, y),
                expect,
                "color {color} depth {depth} interlace {interlace}"
            );
            assert_eq!(img.facts.has(flag::INTERLACED), interlace);
        }
    }
}

#[test]
fn png_transparency_keys_and_palette_alpha() {
    // Palette with alpha.
    let spec = PngSpec {
        w: 4,
        h: 1,
        depth: 8,
        color: 3,
        palette: vec![[10, 20, 30], [40, 50, 60], [70, 80, 90], [1, 2, 3]],
        trns: vec![0, 128],
        ..PngSpec::default()
    };
    let img = decode(&png(&spec, &|x, _| vec![x as u16]), &lim()).unwrap();
    assert_eq!(pix(&img, 0, 0)[3], 0);
    assert_eq!(pix(&img, 1, 0)[3], 128);
    assert_eq!(pix(&img, 2, 0)[3], 255);
    assert!(img.facts.has(flag::HAS_ALPHA));
    // Grey colour key.
    let spec = PngSpec { w: 3, h: 1, depth: 8, color: 0, trns: vec![0, 7], ..PngSpec::default() };
    let img = decode(&png(&spec, &|x, _| vec![(x * 7) as u16]), &lim()).unwrap();
    assert_eq!((pix(&img, 0, 0)[3], pix(&img, 1, 0)[3], pix(&img, 2, 0)[3]), (255, 0, 255));
    // RGB colour key.
    let spec = PngSpec {
        w: 2,
        h: 1,
        depth: 8,
        color: 2,
        trns: vec![0, 1, 0, 2, 0, 3],
        ..PngSpec::default()
    };
    let img =
        decode(&png(&spec, &|x, _| if x == 0 { vec![1, 2, 3] } else { vec![1, 2, 4] }), &lim())
            .unwrap();
    assert_eq!((pix(&img, 0, 0)[3], pix(&img, 1, 0)[3]), (0, 255));
}

#[test]
fn png_big_images_are_reduced_and_flagged() {
    let spec = PngSpec { w: 4100, h: 100, depth: 8, color: 2, ..PngSpec::default() };
    let data = png(&spec, &|x, _| vec![(x % 256) as u16, 0, 0]);
    let img = decode(&data, &lim()).unwrap();
    assert_eq!(img.width, 1367);
    assert_eq!(img.height, 34);
    assert!(img.facts.has(flag::DOWNSCALED));
    assert_eq!((img.facts.width, img.facts.height), (4100, 100));
}

#[test]
fn png_reports_what_it_carries_and_what_trails() {
    let spec = PngSpec {
        w: 2,
        h: 2,
        depth: 8,
        color: 2,
        extra: vec![
            (*b"tEXt", b"Comment\0hello".to_vec()),
            (*b"iCCP", b"profile\0\0xx".to_vec()),
            (*b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0<x/>".to_vec()),
            (*b"acTL", vec![0, 0, 0, 3, 0, 0, 0, 0]),
        ],
        trailing: b"PK\x03\x04appended".to_vec(),
        ..PngSpec::default()
    };
    let data = png(&spec, &|_, _| vec![1, 2, 3]);
    let f = inspect(&data, &lim()).unwrap();
    assert!(
        f.has(flag::TEXT_CHUNKS)
            && f.has(flag::ICC_PROFILE)
            && f.has(flag::XMP)
            && f.has(flag::ANIMATED)
    );
    assert_eq!(f.frames, 3);
    assert_eq!(f.trailing, 12);
    assert!(f.metadata_bytes > 20);
}

#[test]
fn png_exif_is_read_and_orientation_applied() {
    let exif = build(&[(0x0112, V::Short(6)), (0x010F, V::Ascii("Acme"))], &[], &[], false);
    let spec = PngSpec {
        w: 4,
        h: 2,
        depth: 8,
        color: 2,
        extra: vec![(*b"eXIf", exif)],
        ..PngSpec::default()
    };
    let img =
        decode(&png(&spec, &|x, y| vec![(x * 10) as u16, (y * 10) as u16, 0]), &lim()).unwrap();
    assert!(img.facts.has(flag::ORIENTED));
    assert_eq!((img.width, img.height), (2, 4));
    assert_eq!(img.facts.exif.as_ref().unwrap().make, "Acme");
    // Orientation 6 turns the image clockwise: the old top-left pixel is now top-right.
    assert_eq!(pix(&img, 1, 0)[0], 0);
    assert_eq!(pix(&img, 0, 0)[1], 10);
}

#[test]
fn png_limits_are_enforced_before_memory_is_reserved() {
    // A 1×1 file that claims to be 30001 pixels wide.
    let spec = PngSpec { w: 30_001, h: 1, depth: 8, color: 0, ..PngSpec::default() };
    let data = png(&spec, &|_, _| vec![0]);
    assert!(matches!(decode(&data, &lim()), Err(DecodeError::TooLarge(_))));
    // Two sides within the limit, but too many pixels to stream.
    let small = Limits { max_stream_pixels: 1000, ..lim() };
    let spec = PngSpec { w: 100, h: 100, depth: 8, color: 0, ..PngSpec::default() };
    let data = png(&spec, &|_, _| vec![0]);
    assert!(matches!(decode(&data, &small), Err(DecodeError::TooLarge(_))));
    // Interlaced images need the whole raster and have their own, lower limit.
    let small = Limits { max_buffered_pixels: 1000, ..lim() };
    let spec =
        PngSpec { w: 100, h: 100, depth: 8, color: 0, interlace: true, ..PngSpec::default() };
    let data = png(&spec, &|_, _| vec![0]);
    assert!(matches!(decode(&data, &small), Err(DecodeError::TooLarge(_))));
    // The size of the file itself.
    let tiny = Limits { max_input: 10, ..lim() };
    assert!(matches!(decode(&data, &tiny), Err(DecodeError::TooLarge(_))));
}

#[test]
fn png_decompression_bomb_costs_only_what_the_header_promises() {
    // 8×8 image whose IDAT inflates to megabytes: only the 8 rows are read.
    let spec = PngSpec { w: 8, h: 8, depth: 8, color: 0, ..PngSpec::default() };
    let mut data = png(&spec, &|_, _| vec![5]);
    // Replace the IDAT payload by a bomb.
    let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 8 << 20], 9);
    let idat = data.windows(4).position(|w| w == b"IDAT").unwrap();
    let len = u32::from_be_bytes(data[idat - 4..idat].try_into().unwrap()) as usize;
    let mut rebuilt = data[..idat - 4].to_vec();
    rebuilt.extend_from_slice(&(bomb.len() as u32).to_be_bytes());
    rebuilt.extend_from_slice(b"IDAT");
    rebuilt.extend_from_slice(&bomb);
    let mut h = arca_digest::Crc32::new();
    h.update(b"IDAT");
    h.update(&bomb);
    rebuilt.extend_from_slice(&h.finalize().to_be_bytes());
    rebuilt.extend_from_slice(&data[idat + 4 + len + 4..]);
    data = rebuilt;
    let img = decode(&data, &lim()).unwrap();
    assert_eq!((img.width, img.height), (8, 8));
}

#[test]
fn png_truncated_files_show_what_they_have() {
    let spec = PngSpec { w: 64, h: 64, depth: 8, color: 2, filter: Some(0), ..PngSpec::default() };
    let data = png(&spec, &|x, y| vec![(x * 3) as u16, (y * 3) as u16, 9]);
    let idat = data.windows(4).position(|w| w == b"IDAT").unwrap();
    let cut = &data[..idat + 4 + 2000];
    let img = decode(cut, &lim()).unwrap();
    assert!(img.facts.has(flag::DAMAGED));
    assert_eq!((img.width, img.height), (64, 64));
    assert_eq!(img.rgba.len(), 64 * 64 * 4);
    assert_eq!(pix(&img, 3, 0), [9, 0, 9, 255]);
}

#[test]
fn sniffing_never_goes_by_a_name() {
    assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxxxx"), Some(ImageFormat::Png));
    assert_eq!(sniff(b"\xFF\xD8\xFF\xE0"), Some(ImageFormat::Jpeg));
    assert_eq!(sniff(b"GIF89a.."), Some(ImageFormat::Gif));
    assert_eq!(sniff(b"GIF87a.."), Some(ImageFormat::Gif));
    assert_eq!(sniff(b"hello"), None);
    assert_eq!(sniff(b""), None);
    assert_eq!(sniff_unsupported(b"RIFF\0\0\0\0WEBPVP8 "), Some("WebP"));
    assert_eq!(sniff_unsupported(b"II*\0\x08\0\0\0"), Some("TIFF"));
    assert_eq!(sniff_unsupported(b"\0\0\0\x18ftypavif"), Some("AVIF"));
    assert!(
        matches!(decode(b"RIFF\0\0\0\0WEBPVP8 ", &lim()), Err(DecodeError::Unsupported(m)) if m.contains("WebP"))
    );
    assert!(matches!(decode(b"<svg onload=alert(1)/>", &lim()), Err(DecodeError::Unsupported(_))));
}

#[test]
fn image_wire_roundtrip_and_hostile_sizes() {
    use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};
    let spec = PngSpec { w: 5, h: 3, depth: 8, color: 6, ..PngSpec::default() };
    let img = decode(&png(&spec, &|x, y| pattern(x, y, 255, 4)), &lim()).unwrap();
    let bytes = to_bytes(&img).unwrap();
    let limits = DecodeLimits { max_string_bytes: 1 << 20, max_items: 1 << 16, max_depth: 8 };
    assert_eq!(from_bytes::<Image>(&bytes, limits).unwrap(), img);
    // A guest that lies about the size is refused.
    let mut lying = img.clone();
    lying.width = 6;
    let bytes = to_bytes(&lying).unwrap();
    assert!(from_bytes::<Image>(&bytes, limits).is_err());
    // Truncation anywhere is an error, never a panic.
    let good = to_bytes(&img).unwrap();
    for cut in 0..good.len() {
        assert!(from_bytes::<Image>(&good[..cut], limits).is_err());
    }
}

#[test]
fn mutated_pngs_never_panic() {
    let spec = PngSpec { w: 23, h: 17, depth: 8, color: 6, interlace: true, ..PngSpec::default() };
    let base = png(&spec, &|x, y| pattern(x, y, 255, 4));
    let mut x = 0x9E37_79B9u32;
    for _ in 0..4000 {
        let mut d = base.clone();
        for _ in 0..1 + (x >> 29) as usize {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = (x >> 8) as usize % d.len();
            d[i] = (x >> 24) as u8;
        }
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        if x & 7 == 0 {
            d.truncate((x >> 8) as usize % (d.len() + 1));
        }
        let small = Limits {
            max_stream_pixels: 1 << 20,
            max_buffered_pixels: 1 << 20,
            max_side: 4096,
            ..lim()
        };
        if let Ok(img) = decode(&d, &small) {
            assert_eq!(img.rgba.len(), img.width as usize * img.height as usize * 4);
        }
        let _ = inspect(&d, &small);
    }
}

// ---- JPEG against reference decodes ---------------------------------------------------------

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Mean and largest absolute difference of the RGB channels of two same-size pictures.
fn diff(a: &Image, b: &Image) -> (f64, u32) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut max, mut n) = (0u64, 0u32, 0u64);
    for (p, q) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        for c in 0..3 {
            let d = (i32::from(p[c]) - i32::from(q[c])).unsigned_abs();
            sum += u64::from(d);
            max = max.max(d);
            n += 1;
        }
    }
    (sum as f64 / n as f64, max)
}

#[test]
fn jpeg_matches_the_reference_decoder_for_every_layout() {
    let mut names: Vec<String> =
        std::fs::read_dir(format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| {
                n.starts_with("img-")
                    && n.ends_with(".jpg")
                    && !n.contains("cmyk")
                    && !n.contains("ycck")
            })
            .collect();
    names.sort();
    assert!(names.len() >= 20, "{names:?}");
    for name in names {
        let jpg = decode(&fixture(&name), &lim()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let reference = decode(&fixture(&name.replace(".jpg", ".ref.png")), &lim()).unwrap();
        let (mean, max) = diff(&jpg, &reference);
        // 4:4:4 has no chroma upsampling, so it must be (almost) exact; subsampled layouts differ
        // from "fancy" upsampling at colour edges.
        let exact = name.contains("444") || name.contains("gray");
        let (mean_limit, max_limit) = if exact { (1.0, 12) } else { (14.0, 255) };
        assert!(mean <= mean_limit && max <= max_limit, "{name}: mean {mean:.2} max {max}");
        // Subsampled chroma is spread with plain replication here and with "fancy" filtering by
        // the reference, so colours differ at sharp edges; the brightness must not.
        let luma = |i: &Image| -> Vec<i32> {
            i.rgba
                .chunks_exact(4)
                .map(|p| {
                    (299 * i32::from(p[0]) + 587 * i32::from(p[1]) + 114 * i32::from(p[2])) / 1000
                })
                .collect()
        };
        let ld: f64 = luma(&jpg)
            .iter()
            .zip(luma(&reference))
            .map(|(a, b)| f64::from((a - b).abs()))
            .sum::<f64>()
            / f64::from(jpg.width * jpg.height);
        assert!(ld <= if exact { 0.8 } else { 3.5 }, "{name}: luma mean {ld:.2}");
        if !name.contains("java") {
            assert_eq!((jpg.width, jpg.height), (61, 37), "{name}");
        }
        assert!(!jpg.facts.has(flag::DAMAGED), "{name}");
        assert_eq!(jpg.facts.trailing, 0, "{name}");
        assert_eq!(jpg.facts.has(flag::INTERLACED), name.contains("prog"), "{name}");
        assert_eq!(jpg.facts.has(flag::GRAYSCALE), name.contains("gray"), "{name}");
    }
}

#[test]
fn jpeg_scan_scripts_restarts_and_tables_do_not_change_the_picture() {
    // The same quantised coefficients, coded differently, must give the same pixels.
    let pairs: Vec<(String, String)> = [
        ("img-java-prog.jpg", "img-java-base.jpg"),
        ("img-444-prog.jpg", "img-444.jpg"),
        ("img-420-prog.jpg", "img-420.jpg"),
        ("img-422-prog.jpg", "img-422.jpg"),
        ("img-440-prog.jpg", "img-440.jpg"),
        ("img-411-prog.jpg", "img-411.jpg"),
        ("img-gray-prog.jpg", "img-gray.jpg"),
        ("img-444-rst.jpg", "img-444.jpg"),
        ("img-420-rst.jpg", "img-420.jpg"),
        ("img-411-rst.jpg", "img-411.jpg"),
        ("img-444-opt.jpg", "img-444.jpg"),
        ("img-420-opt.jpg", "img-420.jpg"),
    ]
    .iter()
    .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
    .collect();
    for (a, b) in pairs {
        let (ia, ib) =
            (decode(&fixture(&a), &lim()).unwrap(), decode(&fixture(&b), &lim()).unwrap());
        assert_eq!(ia.rgba, ib.rgba, "{a} and {b} must decode to the same pixels");
    }
}

#[test]
fn jpeg_cmyk_and_ycck_become_rgb() {
    // Adobe CMYK (stored inverted): a full-cyan band at the top, a black bar on the left,
    // the rest is white paper.
    for name in ["img-cmyk.jpg", "img-ycck.jpg"] {
        let img = decode(&fixture(name), &lim()).unwrap();
        assert!(img.facts.has(flag::CMYK), "{name}");
        assert_eq!((img.width, img.height), (40, 24));
        let paper = pix(&img, 30, 20);
        assert!(paper[0] > 235 && paper[1] > 235 && paper[2] > 235, "{name}: paper {paper:?}");
        let black = pix(&img, 3, 20);
        assert!(black[0] < 30 && black[1] < 30 && black[2] < 30, "{name}: black {black:?}");
        let cyan = pix(&img, 20, 3);
        assert!(cyan[0] < 60 && cyan[1] > 170 && cyan[2] > 170, "{name}: cyan {cyan:?}");
    }
}

#[test]
fn jpeg_exif_orientation_and_trailing_data() {
    // Build a JPEG with an EXIF APP1 (orientation 6, GPS) and bytes appended after EOI.
    let base = fixture("img-444.jpg");
    let exif = build(
        &[(0x0112, V::Short(6))],
        &[],
        &[
            (1, V::Ascii("N")),
            (2, V::Rationals(&[(10, 1), (0, 1), (0, 1)])),
            (3, V::Ascii("E")),
            (4, V::Rationals(&[(20, 1), (30, 1), (0, 1)])),
        ],
        true,
    );
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&exif);
    let mut data = vec![0xFF, 0xD8, 0xFF, 0xE1];
    data.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    data.extend_from_slice(&app1);
    data.extend_from_slice(&base[2..]);
    data.extend_from_slice(b"PK\x03\x04 hidden payload");
    let img = decode(&data, &lim()).unwrap();
    assert!(img.facts.has(flag::ORIENTED));
    assert_eq!((img.width, img.height), (37, 61));
    assert_eq!(img.facts.trailing, 19);
    let e = img.facts.exif.as_ref().unwrap();
    assert!(e.gps && e.has_position && e.thumbnail);
    assert_eq!((e.lat_micro, e.lon_micro), (10_000_000, 20_500_000));
    assert!(img.facts.metadata_bytes as usize >= app1.len());
    // `inspect` reports the same without decoding the pixels.
    let f = inspect(&data, &lim()).unwrap();
    assert_eq!((f.width, f.height, f.trailing), (61, 37, 19));
    assert_eq!(f.exif, img.facts.exif);
}

#[test]
fn jpeg_limits_and_hostile_headers() {
    let base = fixture("img-420.jpg");
    // Patch the frame header: SOF0 is `FF C0 len(2) 08 h(2) w(2) ...`.
    let sof = base.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    let patch = |h: u16, w: u16| {
        let mut d = base.clone();
        d[sof + 5..sof + 7].copy_from_slice(&h.to_be_bytes());
        d[sof + 7..sof + 9].copy_from_slice(&w.to_be_bytes());
        d
    };
    // 65535 × 65535 from a 2 kB file.
    assert!(matches!(decode(&patch(65_535, 65_535), &lim()), Err(DecodeError::TooLarge(_))));
    // Within the side limit but far more pixels than the file can hold: must not allocate by
    // the header and must come back (blank where the data ran out).
    let l = Limits { max_stream_pixels: 4_000_000, ..lim() };
    let img = decode(&patch(2000, 2000), &l).unwrap();
    assert!(img.facts.has(flag::DAMAGED));
    // Over the pixel limit.
    assert!(matches!(
        decode(&patch(2000, 2001), &Limits { max_stream_pixels: 4_000_000, ..lim() }),
        Err(DecodeError::TooLarge(_))
    ));
    // Zero size.
    assert!(decode(&patch(0, 10), &lim()).is_err());
    // A progressive image over the (lower) buffered limit.
    let prog = fixture("img-420-prog.jpg");
    let sof = prog.windows(2).position(|w| w == [0xFF, 0xC2]).unwrap();
    let mut d = prog.clone();
    d[sof + 5..sof + 7].copy_from_slice(&5000u16.to_be_bytes());
    d[sof + 7..sof + 9].copy_from_slice(&5000u16.to_be_bytes());
    assert!(matches!(decode(&d, &lim()), Err(DecodeError::TooLarge(_))));
}

#[test]
fn mutated_jpegs_never_panic() {
    for name in [
        "img-420.jpg",
        "img-444-prog.jpg",
        "img-java-prog.jpg",
        "img-411-rst.jpg",
        "img-gray.jpg",
        "img-cmyk.jpg",
    ] {
        let base = fixture(name);
        let mut x = 0x1234_5678u32 ^ name.len() as u32;
        for _ in 0..1500 {
            let mut d = base.clone();
            for _ in 0..1 + (x >> 29) as usize {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = (x >> 8) as usize % d.len();
                d[i] = (x >> 24) as u8;
            }
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if x & 3 == 0 {
                d.truncate((x >> 8) as usize % (d.len() + 1));
            }
            let l = Limits {
                max_stream_pixels: 1 << 22,
                max_buffered_pixels: 1 << 22,
                max_side: 8192,
                ..lim()
            };
            if let Ok(img) = decode(&d, &l) {
                assert_eq!(img.rgba.len(), img.width as usize * img.height as usize * 4, "{name}");
            }
            let _ = inspect(&d, &l);
        }
    }
}

#[test]
fn headers_state_the_size_without_decoding() {
    // PNG: signature, IHDR length, "IHDR", width, height.
    let mut png_head = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    png_head.extend_from_slice(b"IHDR");
    png_head.extend_from_slice(&640u32.to_be_bytes());
    png_head.extend_from_slice(&480u32.to_be_bytes());
    assert_eq!(sniff_dimensions(&png_head), Some((640, 480)));

    let mut gif_head = b"GIF89a".to_vec();
    gif_head.extend_from_slice(&300u16.to_le_bytes());
    gif_head.extend_from_slice(&200u16.to_le_bytes());
    assert_eq!(sniff_dimensions(&gif_head), Some((300, 200)));

    // JPEG: a JFIF segment first, then the frame header (height before width).
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
    jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    jpeg.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8]);
    jpeg.extend_from_slice(&120u16.to_be_bytes());
    jpeg.extend_from_slice(&90u16.to_be_bytes());
    assert_eq!(sniff_dimensions(&jpeg), Some((90, 120)));

    // BMP: a bottom-up bitmap stores a positive height, a top-down one a negative height.
    let rows = vec![vec![0u8; 3]; 2];
    let up = bmp(1, 2, 24, 0, &[], None, &rows);
    let down = bmp(1, -2, 24, 0, &[], None, &rows);
    assert_eq!(sniff_dimensions(&up), Some((1, 2)));
    assert_eq!(sniff_dimensions(&down), Some((1, 2)));
}

#[test]
fn a_damaged_or_foreign_header_gives_no_size() {
    assert_eq!(sniff_dimensions(&[]), None);
    assert_eq!(sniff_dimensions(b"not a picture at all"), None);
    assert_eq!(sniff_dimensions(&[0xFF, 0xD8, 0xFF]), None, "a JPEG without a frame header");
    assert_eq!(sniff_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 1]), None, "a segment past the end");
    let mut zero = b"GIF89a".to_vec();
    zero.extend_from_slice(&[0, 0, 5, 0]);
    assert_eq!(sniff_dimensions(&zero), None, "a zero width is no size");
    for cut in 0..40 {
        let mut h = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        h.extend_from_slice(b"IHDR\0\0\0\x10\0\0\0\x10");
        h.truncate(cut);
        let _ = sniff_dimensions(&h);
    }
}
