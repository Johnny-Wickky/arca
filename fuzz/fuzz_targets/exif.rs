// SPDX-License-Identifier: AGPL-3.0-only
//! The EXIF reader, reached the way a file reaches it: as the `APP1` segment of a JPEG or the
//! `eXIf` chunk of a PNG. The first input byte picks the container, the rest is the TIFF block.
//! The invariants are those of common/image.rs, and a block that the reader accepts must not
//! change the picture's size bound.
#![no_main]

#[path = "common/image.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

/// A real, small JPEG; the Exif segment goes right after its SOI marker.
const JPEG: &[u8] = include_bytes!("../../crates/arca-image/tests/fixtures/img-420.jpg");

fn png_with_exif(tiff: &[u8]) -> Option<Vec<u8>> {
    let (png, _) = arca_image::encode_png(2, 2, &[200, 30, 30, 255, 30, 200, 30, 255, 30, 30, 200, 255, 90, 90, 90, 255]).ok()?;
    // Insert before IDAT: signature (8) + IHDR chunk (25) is where the next chunk starts.
    let at = 8 + 25;
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&u32::try_from(tiff.len()).ok()?.to_be_bytes());
    chunk.extend_from_slice(b"eXIf");
    chunk.extend_from_slice(tiff);
    let crc = arca_digest::crc32(chunk.get(4..)?);
    chunk.extend_from_slice(&crc.to_be_bytes());
    let mut out = png.get(..at)?.to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(png.get(at..)?);
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let Some((&which, tiff)) = data.split_first() else { return };
    if tiff.len() > 60_000 {
        return;
    }
    let file = if which & 1 == 0 {
        let mut seg = vec![0xFF, 0xE1, 0, 0];
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(tiff);
        let Ok(len) = u16::try_from(seg.len() - 2) else { return };
        seg[2..4].copy_from_slice(&len.to_be_bytes());
        let mut f = JPEG[..2].to_vec();
        f.extend_from_slice(&seg);
        f.extend_from_slice(&JPEG[2..]);
        f
    } else {
        match png_with_exif(tiff) {
            Some(f) => f,
            None => return,
        }
    };
    common::check(&file);
});
