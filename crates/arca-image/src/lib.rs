// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-image
//!
//! Decoders for the common raster formats — PNG, JPEG (baseline and progressive), GIF and BMP —
//! written for a hostile input and for the sandbox: safe Rust, no I/O, no clocks, every size
//! checked against a [`Limits`] before memory is reserved.
//!
//! What comes out is deliberately *less* than what went in:
//!
//! * pixels only, as RGBA 8 bit, **reduced** to at most [`Limits::max_out_side`] pixels on the
//!   longest side (the full-size picture never exists in memory);
//! * one frame of an animation;
//! * turned upright by the EXIF orientation;
//! * no colour profile, no metadata, no thumbnail, nothing after the end of the image data.
//!
//! What was dropped is *reported* in [`Facts`] (an embedded GPS position, a camera serial
//! number, an uncropped thumbnail, bytes appended after the image, an animation…), so that the
//! threat X-ray can tell the reader about it.
//!
//! Every decoder also accounts for the bytes of the file: what is picture data, what the format
//! itself defines (headers, tables, markers) and what is metadata of a recognised kind is
//! explained, and everything else — private chunks and markers, extension blocks, gaps, padding
//! that carries values, data after the end of a compressed stream — is counted in
//! [`Facts::unaccounted`]. A hidden payload therefore cannot stay silent by being "just another
//! block".

#![forbid(unsafe_code)]

use core::fmt;

use arca_abi::wire::{Reader, Wire, WireError, Writer};
use arca_abi::{wire_enum, wire_struct};

mod bmp;
mod bytes;
mod exif;
mod gif;
mod jpeg;
mod jpegdct;
mod orient;
mod png;
mod pngenc;
mod raw;
mod scale;

pub use exif::ExifFacts;
pub use pngenc::{EncodeError, EncodeNotes, encode_png, pixels_fingerprint};
pub use raw::RowReducer;

/// A raster format this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// Portable Network Graphics (also APNG, of which the first image is read).
    Png,
    /// JPEG (baseline, extended sequential and progressive Huffman, 8 bit).
    Jpeg,
    /// Graphics Interchange Format (the first frame).
    Gif,
    /// Windows bitmap.
    Bmp,
    /// Plain samples that a document stores without a picture format (see [`RowReducer`]).
    Raw,
}
wire_enum!(ImageFormat { Png = 0, Jpeg = 1, Gif = 2, Bmp = 3, Raw = 4 });

impl ImageFormat {
    /// Short lower-case name (`png`, `jpeg`, …).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Raw => "raw",
        }
    }
}

/// Bits of [`Facts::flags`].
pub mod flag {
    /// The image has transparency.
    pub const HAS_ALPHA: u32 = 1;
    /// More than one frame: only the first is read.
    pub const ANIMATED: u32 = 1 << 1;
    /// Interlaced (PNG, GIF) or progressive (JPEG).
    pub const INTERLACED: u32 = 1 << 2;
    /// An ICC colour profile is embedded (it is not applied).
    pub const ICC_PROFILE: u32 = 1 << 3;
    /// An XMP metadata packet is embedded.
    pub const XMP: u32 = 1 << 4;
    /// A comment is embedded.
    pub const COMMENT: u32 = 1 << 5;
    /// Text chunks are embedded (PNG `tEXt`, `zTXt`, `iTXt`).
    pub const TEXT_CHUNKS: u32 = 1 << 6;
    /// The file is damaged: what could be read is shown, the rest is blank.
    pub const DAMAGED: u32 = 1 << 7;
    /// The picture was reduced: it is smaller than the original.
    pub const DOWNSCALED: u32 = 1 << 8;
    /// The picture was turned upright by its EXIF orientation.
    pub const ORIENTED: u32 = 1 << 9;
    /// A grey-scale image.
    pub const GRAYSCALE: u32 = 1 << 10;
    /// A CMYK image (converted to RGB approximately).
    pub const CMYK: u32 = 1 << 11;
    /// A palette image.
    pub const PALETTE: u32 = 1 << 12;
}

/// What is known about an image, apart from its pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The format.
    pub format: ImageFormat,
    /// Width of the *original* in pixels.
    pub width: u32,
    /// Height of the *original* in pixels.
    pub height: u32,
    /// Bits per sample (per pixel for palette, grey and BMP).
    pub bits: u8,
    /// See [`flag`].
    pub flags: u32,
    /// Number of frames found (1 for a still image; counting stops at 1000).
    pub frames: u32,
    /// Bytes after the end of the image data.
    pub trailing: u64,
    /// Bytes of metadata (EXIF, XMP, ICC, comments, text chunks) that were not shown.
    pub metadata_bytes: u64,
    /// Bytes of the file that are neither picture data, nor structure the format defines, nor
    /// metadata of a kind this crate recognises, nor trailing data: private chunks and markers,
    /// extension blocks, gaps, padding that carries values, data after the end of a compressed
    /// stream. Whatever the decoder cannot explain is counted here, so that a hidden payload
    /// cannot stay invisible by being "just another block".
    pub unaccounted: u64,
    /// The EXIF block, if any.
    pub exif: Option<ExifFacts>,
}
wire_struct!(Facts {
    format,
    width,
    height,
    bits,
    flags,
    frames,
    trailing,
    metadata_bytes,
    unaccounted,
    exif
});

impl Facts {
    pub(crate) fn new(format: ImageFormat) -> Self {
        Self {
            format,
            width: 0,
            height: 0,
            bits: 8,
            flags: 0,
            frames: 1,
            trailing: 0,
            metadata_bytes: 0,
            unaccounted: 0,
            exif: None,
        }
    }

    /// Whether `bit` of [`flag`] is set.
    #[must_use]
    pub const fn has(&self, bit: u32) -> bool {
        self.flags & bit != 0
    }

    /// Number of pixels of the original.
    #[must_use]
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// A decoded picture: the (reduced) pixels and the facts about the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width of [`Image::rgba`] in pixels.
    pub width: u32,
    /// Height of [`Image::rgba`] in pixels.
    pub height: u32,
    /// `width × height` pixels, four bytes each (straight, not premultiplied, alpha).
    pub rgba: Vec<u8>,
    /// What is known about the original.
    pub facts: Facts,
}

impl Wire for Image {
    fn encode(&self, w: &mut Writer) {
        self.facts.encode(w);
        w.u32(self.width);
        w.u32(self.height);
        w.bytes(&self.rgba);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.enter()?;
        let facts = Facts::decode(r)?;
        let (width, height) = (r.u32()?, r.u32()?);
        let rgba = r.bytes()?;
        r.leave();
        // The host never trusts the size the guest states: the buffer must be exactly it.
        let expect = u64::from(width) * u64::from(height) * 4;
        if width == 0 || height == 0 || u64::try_from(rgba.len()).ok() != Some(expect) {
            return Err(WireError::LengthLimit);
        }
        Ok(Self { width, height, rgba, facts })
    }
}

/// Limits that bound what one image may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest file accepted, in bytes.
    pub max_input: usize,
    /// Largest width or height of the original.
    pub max_side: u32,
    /// Most pixels of an image decoded row by row (baseline JPEG, non-interlaced PNG, BMP, GIF).
    pub max_stream_pixels: u64,
    /// Most pixels of an image of which the whole raster must be held (progressive JPEG,
    /// interlaced PNG or GIF).
    pub max_buffered_pixels: u64,
    /// Longest side of the picture that is returned.
    pub max_out_side: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input: 64 << 20,
            max_side: 30_000,
            max_stream_pixels: 64_000_000,
            max_buffered_pixels: 16_000_000,
            max_out_side: 2048,
        }
    }
}

/// Why an image could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A format or feature that is not read (the text says which).
    Unsupported(String),
    /// The file is damaged beyond use.
    Malformed(String),
    /// The image is beyond a limit.
    TooLarge(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(s) | Self::Malformed(s) | Self::TooLarge(s) => f.write_str(s),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Recognises an image by its first bytes. Never by a name.
#[must_use]
pub fn sniff(data: &[u8]) -> Option<ImageFormat> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(ImageFormat::Png);
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if bmp::looks_like_bmp(data) {
        return Some(ImageFormat::Bmp);
    }
    None
}

/// The width and height in pixels that the header of a PNG, JPEG, GIF or BMP states, without
/// decoding a single pixel. `None` for anything else, for a damaged header, or for a size of zero.
/// What the header says is a claim; [`inspect`] and [`decode`] check it against their limits.
#[must_use]
pub fn sniff_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| -> Option<u32> {
        data.get(at..at.checked_add(4)?).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let le32 = |at: usize| -> Option<u32> {
        data.get(at..at.checked_add(4)?).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let le16 = |at: usize| -> Option<u32> {
        data.get(at..at.checked_add(2)?).map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])))
    };
    let be16 = |at: usize| -> Option<u32> {
        data.get(at..at.checked_add(2)?).map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let dims = match sniff(data)? {
        ImageFormat::Png => (be32(16)?, be32(20)?),
        ImageFormat::Gif => (le16(6)?, le16(8)?),
        ImageFormat::Bmp => {
            if le32(14)? == 12 {
                (le16(18)?, le16(20)?)
            } else {
                // The height of a bottom-up bitmap is stored negated.
                let h = i32::from_le_bytes(le32(22)?.to_le_bytes());
                (le32(18)?, h.unsigned_abs())
            }
        }
        ImageFormat::Jpeg => {
            let mut at = 2usize;
            let mut found = None;
            // Walk the marker segments up to the first frame header.
            for _ in 0..4096 {
                if *data.get(at)? != 0xFF {
                    return None;
                }
                let marker = *data.get(at.checked_add(1)?)?;
                match marker {
                    0xFF => {
                        at += 1;
                        continue;
                    }
                    0xD8 | 0x01 | 0xD0..=0xD7 => {
                        at += 2;
                        continue;
                    }
                    0xD9 | 0xDA => return None,
                    0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                        found = Some((be16(at.checked_add(7)?)?, be16(at.checked_add(5)?)?));
                        break;
                    }
                    _ => {}
                }
                at = at
                    .checked_add(2)?
                    .checked_add(usize::try_from(be16(at.checked_add(2)?)?).ok()?)?;
            }
            found?
        }
        ImageFormat::Raw => return None,
    };
    if dims.0 == 0 || dims.1 == 0 { None } else { Some(dims) }
}

/// What the first bytes say about a format this crate does not read, if anything.
#[must_use]
pub fn sniff_unsupported(data: &[u8]) -> Option<&'static str> {
    let head = data.get(..data.len().min(32)).unwrap_or(&[]);
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return Some("WebP");
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return Some("TIFF");
    }
    if head.starts_with(&[0, 0, 1, 0]) {
        return Some("ICO");
    }
    if head.get(4..8) == Some(b"ftyp") {
        let brand = head.get(8..12).unwrap_or(&[]);
        return Some(match brand {
            b"avif" | b"avis" => "AVIF",
            b"heic" | b"heix" | b"hevc" | b"mif1" | b"msf1" => "HEIC",
            _ => return None,
        });
    }
    if head.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) || head.get(4..8) == Some(b"jP  ") {
        return Some("JPEG 2000");
    }
    if head.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) {
        return Some("WMF");
    }
    if head.get(40..44) == Some(b" EMF") {
        return Some("EMF");
    }
    None
}

/// What the decoders hand back to [`decode`] and [`inspect`].
pub(crate) struct Raw {
    pub(crate) facts: Facts,
    /// Width, height and RGBA of the reduced image; `None` when only the facts were asked for.
    pub(crate) pixels: Option<(u32, u32, Vec<u8>)>,
}

fn check_input(data: &[u8], limits: &Limits) -> Result<ImageFormat, DecodeError> {
    if data.len() > limits.max_input {
        return Err(DecodeError::TooLarge(format!(
            "the image file is {} bytes, the limit is {}",
            data.len(),
            limits.max_input
        )));
    }
    sniff(data).ok_or_else(|| match sniff_unsupported(data) {
        Some(name) => DecodeError::Unsupported(format!("{name} images are not shown")),
        None => DecodeError::Unsupported("not a picture format that Arca shows".into()),
    })
}

fn run(data: &[u8], limits: &Limits, pixels: bool) -> Result<Raw, DecodeError> {
    match check_input(data, limits)? {
        ImageFormat::Png => png::decode(data, limits, pixels, false),
        ImageFormat::Jpeg => jpeg::decode(data, limits, pixels),
        ImageFormat::Gif => gif::decode(data, limits, pixels),
        ImageFormat::Bmp => bmp::decode(data, limits, pixels),
        ImageFormat::Raw => {
            Err(DecodeError::Unsupported("raw samples are not a file format".into()))
        }
    }
}

/// Reads the facts of an image — its size, format and what it carries besides pixels — without
/// decoding the pixels.
///
/// # Errors
/// [`DecodeError`] if the file is not a readable image or is beyond a limit.
pub fn inspect(data: &[u8], limits: &Limits) -> Result<Facts, DecodeError> {
    run(data, limits, false).map(|r| r.facts)
}

/// Decodes an image into a reduced, upright RGBA picture.
///
/// # Errors
/// [`DecodeError`] if the file is not a readable image or is beyond a limit.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    let Raw { mut facts, pixels } = run(data, limits, true)?;
    let (mut w, mut h, mut rgba) =
        pixels.ok_or_else(|| DecodeError::Malformed("the image has no pixels".into()))?;
    let orientation = facts.exif.as_ref().map_or(1, |e| e.orientation);
    if orientation != 1 {
        let (nw, nh, px) = orient::apply(orientation, w, h, rgba);
        (w, h, rgba) = (nw, nh, px);
        facts.flags |= flag::ORIENTED;
    }
    let (sw, sh) = if (5..=8).contains(&orientation) {
        (facts.height, facts.width)
    } else {
        (facts.width, facts.height)
    };
    if w < sw || h < sh {
        facts.flags |= flag::DOWNSCALED;
    }
    Ok(Image { width: w, height: h, rgba, facts })
}

/// Reads back a PNG that [`encode_png`] wrote — a sterile copy — and checks that it is nothing
/// but its pixels: every chunk checksum and the zlib checksum are verified, and the file must be
/// undamaged, without a byte after its end, without metadata or unaccounted bytes, of one frame.
///
/// # Errors
/// [`DecodeError::Malformed`] naming the first thing that is not as an encoded copy must be, or
/// whatever [`decode`] reports.
pub fn decode_sterile(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    if check_input(data, limits)? != ImageFormat::Png {
        return Err(DecodeError::Malformed("the sterile copy is not a PNG".into()));
    }
    let Raw { facts, pixels } = png::decode(data, limits, true, true)?;
    let unclean =
        |what: &str| DecodeError::Malformed(format!("the sterile copy is not clean: {what}"));
    if facts.has(flag::DAMAGED) {
        return Err(unclean("it is damaged (a checksum or the data does not match)"));
    }
    if facts.trailing != 0 || facts.unaccounted != 0 {
        return Err(unclean("it holds bytes that are not its pixels"));
    }
    if facts.metadata_bytes != 0 || facts.exif.is_some() {
        return Err(unclean("it carries metadata"));
    }
    if facts.frames != 1 || facts.has(flag::ANIMATED | flag::INTERLACED) {
        return Err(unclean("it is animated or interlaced"));
    }
    let (width, height, rgba) =
        pixels.ok_or_else(|| DecodeError::Malformed("the image has no pixels".into()))?;
    Ok(Image { width, height, rgba, facts })
}

#[cfg(test)]
mod testkit;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod testkit_img;

#[cfg(test)]
mod tests_gif_bmp;

#[cfg(test)]
mod tests_enc;

#[cfg(test)]
mod tests_reduced;

#[cfg(test)]
mod tests_hidden;
