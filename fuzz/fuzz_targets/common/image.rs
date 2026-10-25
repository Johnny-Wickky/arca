// SPDX-License-Identifier: AGPL-3.0-only
//! The invariants shared by the image targets (`image`, `png`, `jpeg`, `gif`, `bmp`, `exif`):
//! the decoders never panic, hang or allocate without bound; a decoded picture is exactly
//! `width * height * 4` bytes and within the output bound; the sniffed and the decoded formats
//! agree; decoding is deterministic; the picture survives the wire; and the sterile PNG written
//! from it reads back with the same size.

use arca_abi::wire::{self, DecodeLimits};
use arca_image::{DecodeError, Image, ImageFormat, Limits, decode, decode_sterile, encode_png, inspect, sniff};

/// Small limits keep each execution fast and libFuzzer's memory bounded; they are the same
/// *kinds* of limit as the production ones.
pub const LIMITS: Limits = Limits {
    max_input: 1 << 20,
    max_side: 8192,
    max_stream_pixels: 2_000_000,
    max_buffered_pixels: 1_000_000,
    max_out_side: 256,
};

pub fn check(data: &[u8]) {
    let first = decode(data, &LIMITS);
    if let Ok(img) = &first {
        assert!(img.width >= 1 && img.height >= 1);
        assert!(img.width <= LIMITS.max_out_side && img.height <= LIMITS.max_out_side);
        let expect = u64::from(img.width) * u64::from(img.height) * 4;
        assert_eq!(u64::try_from(img.rgba.len()).ok(), Some(expect));
        assert_eq!(sniff(data), Some(img.facts.format));
        assert!(img.facts.frames >= 1);
        let bytes = wire::to_bytes(img).expect("a picture encodes");
        let back: Image = wire::from_bytes(&bytes, DecodeLimits::default()).expect("and decodes");
        assert_eq!(&back, img);
        let (png, _) = encode_png(img.width, img.height, &img.rgba).expect("a picture can be written as PNG");
        let again = decode(&png, &LIMITS).expect("the PNG copy is readable");
        assert_eq!(again.facts.format, ImageFormat::Png);
        assert_eq!((again.width, again.height), (img.width, img.height));
    }
    // Deterministic.
    assert_eq!(decode(data, &LIMITS), first);
    // The cheap path never contradicts the full one.
    match (inspect(data, &LIMITS), &first) {
        (Ok(f), Ok(img)) => assert_eq!(f.format, img.facts.format),
        (Err(DecodeError::Unsupported(_)), Ok(_)) => panic!("inspect refuses what decode reads"),
        _ => {}
    }
    // The sterile-copy reader must never panic either (it accepts only our own PNG output).
    let _ = decode_sterile(data, &LIMITS);
}

/// `data` with `magic` in front unless it starts with it already, so the fuzzer spends its
/// effort on the format and not on rediscovering the signature.
#[allow(dead_code)] // only the format-specific targets use it
pub fn with_magic<'a>(magic: &[u8], data: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
    if data.starts_with(magic) {
        return std::borrow::Cow::Borrowed(data);
    }
    let mut v = magic.to_vec();
    v.extend_from_slice(data);
    std::borrow::Cow::Owned(v)
}
