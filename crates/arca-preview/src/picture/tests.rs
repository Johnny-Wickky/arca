//! The sterile copy of a picture file (`sanitize_picture`): a plain PNG written from the pixels.

use arca_image::{ImageFormat, Limits, flag};
use arca_xray::sanitize::ActionKind;

use super::{PictureError, sanitize_picture};

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/../arca-image/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(kind, data).to_be_bytes());
}

fn crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
    // Bitwise CRC-32 (the test needs no speed).
    let mut crc = 0xFFFF_FFFFu32;
    for &b in kind.iter().chain(data) {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A little-endian TIFF block with a maker name, a serial number, an orientation and a GPS
/// position (10° N, 20.5° E).
fn exif_block() -> Vec<u8> {
    let mut t = b"II*\0".to_vec();
    t.extend_from_slice(&8u32.to_le_bytes());
    let entry = |t: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]| {
        t.extend_from_slice(&tag.to_le_bytes());
        t.extend_from_slice(&typ.to_le_bytes());
        t.extend_from_slice(&count.to_le_bytes());
        t.extend_from_slice(&value);
    };
    // IFD0 at 8: Make, Orientation, GPS pointer. 2 + 3 × 12 + 4 = 42 bytes, so the data follows
    // at 50: the Make string (10 bytes), then the GPS IFD at 60.
    t.extend_from_slice(&3u16.to_le_bytes());
    entry(&mut t, 0x010F, 2, 10, 50u32.to_le_bytes());
    entry(&mut t, 0x0112, 3, 1, [6, 0, 0, 0]);
    entry(&mut t, 0x8825, 4, 1, 60u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(t.len(), 50);
    t.extend_from_slice(b"Acme Cam!\0");
    // GPS IFD at 60: 2 + 4 × 12 + 4 = 54 bytes; the rationals follow at 114 and 138.
    assert_eq!(t.len(), 60);
    t.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut t, 1, 2, 2, *b"N\0\0\0");
    entry(&mut t, 2, 5, 3, 114u32.to_le_bytes());
    entry(&mut t, 3, 2, 2, *b"E\0\0\0");
    entry(&mut t, 4, 5, 3, 138u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(t.len(), 114);
    for (a, b) in [(10u32, 1u32), (0, 1), (0, 1), (20, 1), (30, 1), (0, 1)] {
        t.extend_from_slice(&a.to_le_bytes());
        t.extend_from_slice(&b.to_le_bytes());
    }
    t
}

/// A JPEG with EXIF (position, camera, orientation 6), a comment and a payload after the end.
fn jpeg_with_everything() -> Vec<u8> {
    let base = fixture("img-444.jpg");
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&exif_block());
    let mut data = vec![0xFF, 0xD8, 0xFF, 0xE1];
    data.extend_from_slice(&u16::try_from(app1.len() + 2).unwrap().to_be_bytes());
    data.extend_from_slice(&app1);
    let comment = b"drafted by Alice for the secret project";
    data.extend_from_slice(&[0xFF, 0xFE]);
    data.extend_from_slice(&u16::try_from(comment.len() + 2).unwrap().to_be_bytes());
    data.extend_from_slice(comment);
    data.extend_from_slice(&base[2..]);
    data.extend_from_slice(b"PK\x03\x04 hidden payload");
    data
}

fn targets(actions: &[arca_xray::sanitize::SanitizeAction], kind: ActionKind) -> Vec<&str> {
    actions.iter().filter(|a| a.kind == kind).map(|a| a.target.as_str()).collect()
}

fn same_pixels(a: &arca_image::Image, b: &arca_image::Image) -> bool {
    a.width == b.width && a.height == b.height && a.rgba == b.rgba
}

#[test]
fn a_photo_with_everything_in_it_becomes_a_clean_png() {
    let original = jpeg_with_everything();
    let (png, actions) = sanitize_picture(&original, false).unwrap();
    // A new PNG, with the same pixels (turned upright by the orientation).
    assert_eq!(arca_image::sniff(&png), Some(ImageFormat::Png));
    let before = arca_image::decode(&original, &Limits::default()).unwrap();
    assert!(before.facts.has(flag::ORIENTED));
    let after = arca_image::decode(&png, &Limits::default()).unwrap();
    assert!(same_pixels(&before, &after));
    assert_eq!((after.width, after.height), (37, 61));
    // Nothing of the original is left in it.
    let facts = arca_image::inspect(&png, &Limits::default()).unwrap();
    assert_eq!((facts.exif.is_none(), facts.trailing, facts.metadata_bytes), (true, 0, 0));
    for secret in [&b"Acme"[..], b"Alice", b"hidden payload", b"Exif", b"PK\x03\x04"] {
        assert!(!png.windows(secret.len()).any(|w| w == secret), "{secret:?}");
    }
    // The copy says what it left out.
    let converted = targets(&actions, ActionKind::ConvertedContentType);
    assert!(converted.iter().any(|t| t.starts_with("JPEG → PNG")), "{converted:?}");
    let stripped = targets(&actions, ActionKind::StrippedMetadata);
    assert!(
        stripped.iter().any(|t| t.starts_with("EXIF")
            && t.contains("GPS position")
            && t.contains("camera make and model")),
        "{stripped:?}"
    );
    assert!(stripped.contains(&"comment"), "{stripped:?}");
    assert!(stripped.iter().any(|t| t.starts_with("orientation")), "{stripped:?}");
    let removed = targets(&actions, ActionKind::RemovedElement);
    assert!(removed.iter().any(|t| t.contains("19 bytes after the end")), "{removed:?}");
}

#[test]
fn a_png_with_text_and_trailing_data_is_written_anew() {
    let mut png = fixture("img-420.ref.png");
    // Text chunks go before IEND, the payload after it.
    let iend = png.len() - 12;
    let mut with_text = png[..iend].to_vec();
    chunk(&mut with_text, b"tEXt", b"Author\0Mallory");
    chunk(&mut with_text, b"tEXt", b"Comment\0meet at the usual place");
    with_text.extend_from_slice(&png[iend..]);
    with_text.extend_from_slice(b"MZ\x90\0 not a program");
    png = with_text;
    let (copy, actions) = sanitize_picture(&png, false).unwrap();
    let before = arca_image::decode(&png, &Limits::default()).unwrap();
    let after = arca_image::decode(&copy, &Limits::default()).unwrap();
    assert!(same_pixels(&before, &after));
    for secret in [&b"Mallory"[..], b"usual place", b"MZ\x90"] {
        assert!(!copy.windows(secret.len()).any(|w| w == secret), "{secret:?}");
    }
    assert!(targets(&actions, ActionKind::StrippedMetadata).contains(&"text chunks"));
    assert!(
        targets(&actions, ActionKind::RemovedElement).iter().any(|t| t.contains("after the end"))
    );
}

#[test]
fn every_fixture_picture_comes_back_with_the_same_pixels() {
    for name in [
        "img-420.jpg",
        "img-411-prog.jpg",
        "img-444-rst.jpg",
        "img-gray.jpg",
        "img-cmyk.jpg",
        "img-ycck.jpg",
        "img-java-prog.jpg",
        "img-420.ref.png",
        "img-gray.ref.png",
    ] {
        let original = fixture(name);
        let (copy, actions) = sanitize_picture(&original, false).unwrap();
        let before = arca_image::decode(&original, &Limits::default()).unwrap();
        let after = arca_image::decode(&copy, &Limits::default()).unwrap();
        assert!(same_pixels(&before, &after), "{name}");
        assert!(!actions.is_empty(), "{name}: the copy always says what it is");
        // Writing the copy again changes nothing: it is already what the sandbox would write.
        let (again, _) = sanitize_picture(&copy, false).unwrap();
        assert!(again == copy, "{name}: the copy is stable");
    }
}

#[test]
fn a_picture_beyond_the_size_of_a_copy_is_reduced_and_says_so() {
    // 61 × 37 = 2 257 pixels: with room for 1 000, a factor of two (31 × 19) is the first fit.
    let side = super::copy_side_for(61, 37, 1_000);
    assert_eq!(side, 31);
    let original = fixture("img-420.ref.png");
    let (copy, actions) = super::sanitize_picture_within(&original, 1_000).unwrap();
    let after = arca_image::decode(&copy, &Limits::default()).unwrap();
    assert_eq!((after.width, after.height), (31, 19));
    let removed = targets(&actions, ActionKind::RemovedElement);
    assert!(removed.iter().any(|t| t.contains("reduced from 61×37 to 31×19")), "{removed:?}");
    // Room enough: nothing is reduced.
    assert_eq!(super::copy_side_for(61, 37, 1 << 40), 61);
    assert_eq!(super::copy_side_for(4000, 3000, super::COPY_MAX_PIXELS_FULL), 4000);
    assert!(super::copy_side_for(8000, 6000, super::COPY_MAX_PIXELS_FULL) <= 4000);
    assert!(super::copy_side_for(30_000, 30_000, super::COPY_MAX_PIXELS_FULL) <= 4_000);
    // By default a 12-megapixel photograph is halved (3 megapixels), an 8000 × 6000 one quartered.
    assert_eq!(super::copy_side_for(4000, 3000, super::COPY_MAX_PIXELS), 2000);
    assert_eq!(super::copy_side_for(8000, 6000, super::COPY_MAX_PIXELS), 2000);
    let original = fixture("img-420.jpg");
    assert!(
        sanitize_picture(&original, true).unwrap().0
            == sanitize_picture(&original, false).unwrap().0
    );
}

#[test]
fn what_is_not_a_picture_is_refused() {
    for (data, wanted) in
        [(&b"just some text"[..], "not a picture"), (&b"RIFF\0\0\0\0WEBPVP8 xxxxxxxx"[..], "WebP")]
    {
        match sanitize_picture(data, false) {
            Err(PictureError::Unsupported(m)) => assert!(m.contains(wanted), "{m}"),
            other => panic!("{other:?}"),
        }
    }
    // A picture cut off in the middle: what could be read is kept, the rest is blank, and the
    // copy says so.
    let jpeg = fixture("img-java-base.jpg");
    let cut = &jpeg[..jpeg.len() * 6 / 10];
    let (copy, actions) = sanitize_picture(cut, false).unwrap();
    assert_eq!(arca_image::sniff(&copy), Some(ImageFormat::Png));
    assert!(targets(&actions, ActionKind::RemovedElement).iter().any(|t| t.contains("damaged")));
}
