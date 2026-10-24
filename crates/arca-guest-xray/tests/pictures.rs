#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test code")]

//! Pictures through the guest's request/response path (without the sandbox): the preview names
//! them, an `Image` request decodes one of them.

use arca_abi::envelope::{self, PayloadKind};
use arca_abi::request::{self, Op};
use arca_abi::status;
use arca_guest_xray::{decode_image, decode_preview, decode_report, process};
use arca_preview::model::{Block, IMAGE_NONE, ImageInfo};
use arca_xray::{FileFormat, ThreatKind};

fn fixture(path: &str) -> Vec<u8> {
    let p = format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn picture_file(name: &str) -> Vec<u8> {
    fixture(&format!("crates/arca-image/tests/fixtures/{name}"))
}

fn ask(op: Op, flags: u32, doc: &[u8]) -> (i32, Vec<u8>) {
    let mut v = request::header(op, flags).to_vec();
    v.extend_from_slice(doc);
    process(&v)
}

fn images_of(doc: &[u8]) -> Vec<ImageInfo> {
    let (code, env) = ask(Op::Preview, 0, doc);
    assert_eq!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    let preview = decode_preview(kind, payload).unwrap();
    preview
        .sections
        .iter()
        .flat_map(|s| s.blocks.iter())
        .filter_map(|b| if let Block::Image(i) = b { Some(i.clone()) } else { None })
        .collect()
}

#[test]
fn a_picture_file_is_scanned_previewed_and_decoded() {
    let jpeg = picture_file("img-420.jpg");
    // X-ray: recognised by content, nothing to report on a plain picture.
    let (code, env) = ask(Op::Scan, 0, &jpeg);
    assert_eq!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    let report = decode_report(kind, payload).unwrap();
    assert_eq!(report.format, FileFormat::Jpeg);
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    // Preview: one picture, id 0.
    let images = images_of(&jpeg);
    assert_eq!(images.len(), 1);
    assert_eq!(
        (images[0].id, images[0].media.as_str(), images[0].alt.as_str()),
        (0, "jpeg", "61×37")
    );
    // Image: pixels.
    let (code, env) = ask(Op::Image, 0, &jpeg);
    assert_eq!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    assert_eq!(kind, PayloadKind::Image);
    let img = decode_image(kind, payload).unwrap();
    assert_eq!((img.width, img.height), (61, 37));
    assert_eq!(img.rgba.len(), 61 * 37 * 4);
    // The same for a PNG.
    let png = picture_file("img-420.ref.png");
    let (_, env) = ask(Op::Image, 0, &png);
    let (kind, payload) = envelope::open(&env).unwrap();
    assert_eq!(decode_image(kind, payload).unwrap().width, 61);
}

#[test]
fn bytes_after_a_picture_are_a_polyglot_finding() {
    let mut doc = picture_file("img-444.jpg");
    doc.extend_from_slice(b"PK\x03\x04 pretend archive");
    let (_, env) = ask(Op::Scan, 0, &doc);
    let (kind, payload) = envelope::open(&env).unwrap();
    let report = decode_report(kind, payload).unwrap();
    assert!(
        report.findings.iter().any(|f| f.kind == ThreatKind::PolyglotFile),
        "{:?}",
        report.findings
    );
    // Plain trailing bytes are a lesser finding.
    let mut doc = picture_file("img-444.jpg");
    doc.extend_from_slice(&[0u8; 40]);
    let (_, env) = ask(Op::Scan, 0, &doc);
    let (kind, payload) = envelope::open(&env).unwrap();
    let report = decode_report(kind, payload).unwrap();
    assert!(report.findings.iter().any(|f| f.kind == ThreatKind::TrailingData));
}

#[test]
fn a_picture_inside_a_document_is_found_by_its_id() {
    let deck = fixture("tests/fixtures/deck-rich.pptx");
    let images = images_of(&deck);
    let first = images.iter().find(|i| i.media == "png").expect("the deck holds a PNG");
    assert_ne!(first.id, IMAGE_NONE);
    let (code, env) = ask(Op::Image, first.id, &deck);
    assert_eq!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    let img = decode_image(kind, payload).unwrap();
    assert!(img.width >= 1 && img.height >= 1);
    // Any other number is refused, and so is a member that is not a picture.
    let (code, _) = ask(Op::Image, 9_999, &deck);
    assert_eq!(code, status::DOCUMENT_REJECTED);
    let (code, _) = ask(Op::Image, 0, &deck);
    assert_ne!(code, status::OK, "member 0 is not a picture");
}

#[test]
fn pictures_of_unsupported_kinds_are_refused_by_name() {
    // A PDF has pictures, but they are not served yet through this operation for plain text files.
    let (code, env) = ask(Op::Image, 0, b"just some text");
    assert_ne!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    assert!(decode_image(kind, payload).is_err());
    let (code, _) = ask(Op::Image, 0, b"RIFF\0\0\0\0WEBPVP8 xxxxxxxx");
    assert_eq!(code, status::UNSUPPORTED_FORMAT);
}

#[test]
fn hostile_picture_payloads_are_refused_by_the_host_decoder() {
    assert!(decode_image(PayloadKind::Image, &[0xFF; 64]).is_err());
    assert!(decode_image(PayloadKind::Image, &[]).is_err());
    let img = picture_file("img-420.jpg");
    let (_, env) = ask(Op::Image, 0, &img);
    let (kind, payload) = envelope::open(&env).unwrap();
    let payload = payload.to_vec();
    for i in (0..payload.len().min(400)).chain([payload.len() - 1]) {
        let mut m = payload.clone();
        m[i] ^= 0xFF;
        let _ = decode_image(kind, &m);
        let _ = decode_image(kind, &payload[..i]);
    }
}

/// A one-page PDF (no cross-reference table: the reader rebuilds it) that draws object 5, a
/// 2 × 1 RGB picture (red, green).
fn pdf_with_a_picture() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>\nendobj\n");
    pdf.extend_from_slice(
        b"4 0 obj\n<< /Length 28 >>\nstream\nq 80 0 0 40 10 10 cm /Im1 Do Q\nendstream\nendobj\n",
    );
    pdf.extend_from_slice(b"5 0 obj\n<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 6 >>\nstream\n");
    pdf.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
    pdf.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Root 1 0 R /Size 6 >>\n%%EOF\n");
    pdf
}

#[test]
fn the_pictures_of_a_pdf_are_found_by_their_object_numbers() {
    let pdf = pdf_with_a_picture();
    let images = images_of(&pdf);
    assert_eq!(images.len(), 1, "{images:?}");
    assert_eq!(images[0].id, 5);
    let (code, env) = ask(Op::Image, 5, &pdf);
    assert_eq!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    let img = decode_image(kind, payload).unwrap();
    assert_eq!((img.width, img.height), (2, 1));
    assert_eq!(&img.rgba, &[255, 0, 0, 255, 0, 255, 0, 255]);
    // The page drawing carries the same id, so that a tap on the picture can ask for it.
    let (code, env) = ask(Op::Render, 0, &pdf);
    assert_eq!(code, status::OK);
    let (kind, payload) = envelope::open(&env).unwrap();
    let page = arca_guest_xray::decode_page(kind, payload).unwrap();
    assert!(page.prims.iter().any(|p| matches!(p, arca_preview::page::Prim::Image { id: 5, .. })));
    // An object that is not a picture is refused.
    for n in [1u32, 4, 99] {
        let (code, _) = ask(Op::Image, n, &pdf);
        assert_ne!(code, status::OK, "object {n}");
    }
}

#[test]
fn the_pictures_of_an_rtf_document_are_hex_or_binary() {
    let png = picture_file("img-420.ref.png");
    let jpeg = picture_file("img-444.jpg");
    let digits: Vec<String> = png.iter().map(|b| format!("{b:02x}")).collect();
    let hex: String = digits.chunks(40).map(|c| c.concat()).collect::<Vec<_>>().join("\r\n");
    let mut rtf = Vec::new();
    rtf.extend_from_slice(b"{\\rtf1\\ansi before {\\pict\\pngblip\\picw61\\pich37 {\\*\\blipuid 0123456789abcdef0123456789abcdef}");
    rtf.extend_from_slice(hex.as_bytes());
    rtf.extend_from_slice(b"} middle {\\pict\\jpegblip\\bin");
    rtf.extend_from_slice(format!("{} ", jpeg.len()).as_bytes());
    rtf.extend_from_slice(&jpeg);
    rtf.extend_from_slice(b"} text {\\pict\\emfblip 0102030405060708} after}");
    let images = images_of(&rtf);
    assert_eq!(images.len(), 3, "{images:?}");
    assert!(images.iter().all(|i| i.id != IMAGE_NONE));
    for (info, wanted) in images.iter().zip([Some(61u32), Some(61), None]) {
        let (code, env) = ask(Op::Image, info.id, &rtf);
        let (kind, payload) = envelope::open(&env).unwrap();
        match wanted {
            Some(w) => {
                assert_eq!(code, status::OK, "{info:?}");
                assert_eq!(decode_image(kind, payload).unwrap().width, w);
            }
            None => assert_eq!(code, status::UNSUPPORTED_FORMAT, "an EMF is named, not shown"),
        }
    }
    // An offset that is not a picture group is refused.
    let (code, _) = ask(Op::Image, 3, &rtf);
    assert_ne!(code, status::OK);
}

#[test]
fn a_picture_file_gets_a_sterile_copy_that_is_a_plain_png() {
    use arca_abi::request::flags;
    use arca_guest_xray::decode_sanitized;
    use arca_xray::Verdict;
    use arca_xray::sanitize::ActionKind;

    let mut jpeg = picture_file("img-420.jpg");
    jpeg.extend_from_slice(b"PK\x03\x04 hidden payload");
    for request_flags in [flags::SANITIZE_ALL, flags::SANITIZE_ALL | flags::FULL_SIZE] {
        let (code, env) = ask(Op::Sanitize, request_flags, &jpeg);
        assert_eq!(code, status::OK);
        let (kind, payload) = envelope::open(&env).unwrap();
        assert_eq!(kind, PayloadKind::SanitizedDocument);
        let copy = decode_sanitized(kind, payload).unwrap();
        // A PNG that was written from the pixels, inspected again, and found clean.
        assert_eq!(copy.format, FileFormat::Png);
        assert!(copy.bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
        assert_eq!(copy.verification.verdict, Verdict::Clean);
        assert!(copy.verification.findings.is_empty(), "{:?}", copy.verification.findings);
        assert!(!copy.bytes.windows(14).any(|w| w == b"hidden payload"));
        assert!(
            copy.actions.iter().any(|a| a.kind == ActionKind::ConvertedContentType
                && a.target.starts_with("JPEG → PNG")),
            "{:?}",
            copy.actions
        );
        assert!(
            copy.actions
                .iter()
                .any(|a| a.kind == ActionKind::RemovedElement && a.target.contains("after the end")),
            "{:?}",
            copy.actions
        );
        // The copy shows the same picture as the original.
        let pixels = |doc: &[u8]| {
            let (code, env) = ask(Op::Image, 0, doc);
            assert_eq!(code, status::OK);
            let (kind, payload) = envelope::open(&env).unwrap();
            decode_image(kind, payload).unwrap()
        };
        let (a, b) = (pixels(&jpeg), pixels(&copy.bytes));
        assert_eq!((a.width, a.height, &a.rgba), (b.width, b.height, &b.rgba));
    }
    // The same for every kind of picture file; a file that only claims to be one is refused.
    for name in ["img-420.ref.png", "img-java-prog.jpg", "img-cmyk.jpg"] {
        let (code, _) = ask(Op::Sanitize, flags::SANITIZE_ALL, &picture_file(name));
        assert_eq!(code, status::OK, "{name}");
    }
    let mut broken = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    broken.extend_from_slice(&[0xFF; 64]);
    let (code, _) = ask(Op::Sanitize, flags::SANITIZE_ALL, &broken);
    assert_ne!(code, status::OK);
}
