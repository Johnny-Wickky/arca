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
