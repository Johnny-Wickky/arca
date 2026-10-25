//! Tests of the PDF pictures (see `picture.rs`).

use arca_image::{Image, flag};

use super::picture::{Found, picture};
use super::testpdf::PdfBuilder;
use crate::PreviewPolicy;
use crate::picture::PictureError;

const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Im1 7 0 R >> >> >>";

/// A one-page PDF whose object 7 is an image XObject with `dict` and `data` (Flate).
fn pdf_with(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::with_content("/Im1 Do");
    b.add_raw(3, PAGE);
    b.add_flate_stream(7, &format!("/Type /XObject /Subtype /Image {dict}"), data);
    b.build()
}

fn samples(pdf: &[u8]) -> Image {
    match picture(pdf, 7, &PreviewPolicy::default()) {
        Ok(Found::Samples(img)) => img,
        Ok(Found::Jpeg(_)) => panic!("expected samples, got a JPEG"),
        Err(e) => panic!("{e}"),
    }
}

fn px(img: &Image, x: usize, y: usize) -> [u8; 4] {
    let at = (y * img.width as usize + x) * 4;
    [img.rgba[at], img.rgba[at + 1], img.rgba[at + 2], img.rgba[at + 3]]
}

#[test]
fn rgb_gray_and_cmyk_samples() {
    let rgb: Vec<u8> = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 10, 20, 30];
    let img =
        samples(&pdf_with("/Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &rgb));
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 1, 1), [10, 20, 30, 255]);
    // Grey, 8 bits, and inverted by a Decode array.
    let img = samples(&pdf_with(
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0, 200],
    ));
    assert_eq!((px(&img, 0, 0)[0], px(&img, 1, 0)[0]), (0, 200));
    let img = samples(&pdf_with(
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]",
        &[0, 200],
    ));
    assert_eq!((px(&img, 0, 0)[0], px(&img, 1, 0)[0]), (255, 55));
    // Grey, 1 bit per sample (rows are padded to whole bytes).
    let img = samples(&pdf_with(
        "/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1",
        &[0b1010_0000, 0b0100_0000],
    ));
    assert_eq!((px(&img, 0, 0)[0], px(&img, 1, 0)[0], px(&img, 2, 0)[0]), (255, 0, 255));
    assert_eq!((px(&img, 0, 1)[0], px(&img, 1, 1)[0]), (0, 255));
    // CMYK: no ink is white, full black ink is black.
    let img = samples(&pdf_with(
        "/Width 2 /Height 1 /ColorSpace /DeviceCMYK /BitsPerComponent 8",
        &[0, 0, 0, 0, 0, 0, 0, 255],
    ));
    assert_eq!((px(&img, 0, 0), px(&img, 1, 0)), ([255, 255, 255, 255], [0, 0, 0, 255]));
    // 16 bits keep the high byte.
    let img = samples(&pdf_with(
        "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16",
        &[0x80, 0x00],
    ));
    assert_eq!(px(&img, 0, 0)[0], 128);
}

#[test]
fn palette_icc_and_masks() {
    // Indexed, 4 bits: two pixels per byte.
    let img = samples(&pdf_with(
        "/Width 2 /Height 1 /ColorSpace [/Indexed /DeviceRGB 2 <FF0000 00FF00 0000FF>] /BitsPerComponent 4",
        &[0x12],
    ));
    assert_eq!((px(&img, 0, 0), px(&img, 1, 0)), ([0, 255, 0, 255], [0, 0, 255, 255]));
    // ICC-based: the number of components decides.
    let mut b = PdfBuilder::with_content("/Im1 Do");
    b.add_raw(3, PAGE);
    b.add_stream(8, "/N 3", b"not a profile");
    b.add_flate_stream(
        7,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace [/ICCBased 8 0 R] /BitsPerComponent 8",
        &[1, 2, 3],
    );
    assert_eq!(px(&samples(&b.build()), 0, 0), [1, 2, 3, 255]);
    // A soft mask gives the alpha channel.
    let mut b = PdfBuilder::with_content("/Im1 Do");
    b.add_raw(3, PAGE);
    b.add_flate_stream(
        8,
        "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[255, 64],
    );
    b.add_flate_stream(
        7,
        "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 8 0 R",
        &[100, 100],
    );
    let img = samples(&b.build());
    assert_eq!((px(&img, 0, 0)[3], px(&img, 1, 0)[3]), (255, 64));
    assert!(img.facts.has(flag::HAS_ALPHA));
    // A stencil mask paints black where the bit is 0.
    let img = samples(&pdf_with(
        "/Width 2 /Height 1 /ImageMask true /BitsPerComponent 1",
        &[0b0100_0000],
    ));
    assert_eq!((px(&img, 0, 0), px(&img, 1, 0)), ([0, 0, 0, 255], [0, 0, 0, 0]));
}

#[test]
fn jpeg_streams_pass_through_and_other_codecs_are_named() {
    let path = format!("{}/../arca-image/tests/fixtures/img-420.jpg", env!("CARGO_MANIFEST_DIR"));
    let jpeg = std::fs::read(path).unwrap();
    let mut b = PdfBuilder::with_content("/Im1 Do");
    b.add_raw(3, PAGE);
    b.add_stream(
        7,
        "/Type /XObject /Subtype /Image /Width 61 /Height 37 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        &jpeg,
    );
    let pdf = b.build();
    let img = crate::picture(&pdf, 7, &PreviewPolicy::default()).unwrap();
    assert_eq!((img.width, img.height), (61, 37));
    // JPEG 2000 and CCITT are refused by name.
    for (filter, name) in [("JPXDecode", "JPEG 2000"), ("CCITTFaxDecode", "CCITT")] {
        let mut b = PdfBuilder::with_content("/Im1 Do");
        b.add_raw(3, PAGE);
        b.add_stream(
            7,
            &format!("/Type /XObject /Subtype /Image /Width 8 /Height 8 /BitsPerComponent 8 /Filter /{filter}"),
            b"xx",
        );
        match picture(&b.build(), 7, &PreviewPolicy::default()) {
            Err(PictureError::Unsupported(m)) => assert!(m.contains(name), "{m}"),
            other => panic!("{:?}", other.err()),
        }
    }
}

#[test]
fn what_is_not_a_picture_or_not_usable_is_refused() {
    let pdf = pdf_with("/Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[0; 12]);
    // Objects 2 (the pages), 4 (the content) and 99 (nothing) are not pictures.
    for n in [2u32, 99, 4] {
        assert!(
            matches!(picture(&pdf, n, &PreviewPolicy::default()), Err(PictureError::Missing)),
            "{n}"
        );
    }
    // An unknown colour space, a bit depth that does not exist, sizes that are not usable.
    for dict in [
        "/Width 2 /Height 2 /ColorSpace [/Lab << >>] /BitsPerComponent 8",
        "/Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 3",
        "/Width 0 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        "/Width 99999 /Height 99999 /ColorSpace /DeviceRGB /BitsPerComponent 8",
    ] {
        assert!(
            picture(&pdf_with(dict, &[0; 12]), 7, &PreviewPolicy::default()).is_err(),
            "{dict}"
        );
    }
    // Fewer samples than the header promises: what is there is shown, the rest is blank.
    let img = samples(&pdf_with(
        "/Width 2 /Height 4 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[9, 9],
    ));
    assert_eq!((img.width, img.height), (2, 4));
    assert_eq!(px(&img, 0, 0)[3], 255);
    assert_eq!(px(&img, 0, 3)[3], 0);
}

#[test]
fn the_preview_gives_pictures_their_object_numbers() {
    let pdf = pdf_with("/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[7]);
    let p = crate::testkit::ok(&pdf);
    let ids: Vec<u32> = p
        .sections
        .iter()
        .flat_map(|s| s.blocks.iter())
        .filter_map(|b| if let crate::model::Block::Image(i) = b { Some(i.id) } else { None })
        .collect();
    assert_eq!(ids, [7]);
}

#[test]
fn a_soft_mask_is_sampled_not_expanded() {
    let gray = "/Type /XObject /Subtype /Image /ColorSpace /DeviceGray /BitsPerComponent 8";
    let masked = |mask: &str, mask_data: &[u8], w: u32, h: u32, data: &[u8]| {
        let mut b = PdfBuilder::with_content("/Im1 Do");
        b.add_raw(3, PAGE);
        b.add_flate_stream(8, &format!("{gray} {mask}"), mask_data);
        b.add_flate_stream(7, &format!("{gray} /Width {w} /Height {h} /SMask 8 0 R"), data);
        b.build()
    };
    // One alpha value for every pixel of a larger picture.
    let img = samples(&masked("/Width 1 /Height 1", &[64], 8, 8, &[200; 64]));
    assert!((0..8).all(|i| px(&img, i, i)[3] == 64));
    // A 2 × 2 mask over 4 × 4: each mask pixel covers a 2 × 2 square.
    let img = samples(&masked("/Width 2 /Height 2", &[0, 90, 180, 255], 4, 4, &[7; 16]));
    assert_eq!(
        [
            px(&img, 0, 0)[3],
            px(&img, 1, 1)[3],
            px(&img, 2, 0)[3],
            px(&img, 0, 2)[3],
            px(&img, 3, 3)[3]
        ],
        [0, 0, 90, 180, 255]
    );
    // A picture of 30000 × 30000 pixels with a 1 × 1 mask is refused by its own size, before
    // anything is made for the mask (a byte per pixel would be 900 MB).
    let huge = masked("/Width 1 /Height 1", &[1], 30_000, 30_000, &[0; 16]);
    match picture(&huge, 7, &PreviewPolicy::default()) {
        Err(PictureError::Malformed(why)) => assert!(why.contains("pixels"), "{why}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("a 900 megapixel picture was accepted"),
    }
}
