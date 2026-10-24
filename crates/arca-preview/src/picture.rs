//! Pictures: the description of a picture file in the preview, and the decoding of one picture
//! of a document when the reader asks for it.
//!
//! The preview only *describes* pictures ([`crate::model::ImageInfo`], with an `id`). To see one,
//! the app starts a separate sandbox run with that id; [`picture`] finds the picture's bytes in
//! the document, decodes it with `arca-image` (reduced, upright, without metadata) and returns
//! pixels. What the id means belongs to the format of the document and is never interpreted
//! anywhere but here.

use std::borrow::Cow;

use arca_image::{DecodeError, EncodeNotes, Facts, Image, Limits, flag};
use arca_xray::sanitize::{ActionKind, SanitizeAction};

use crate::detect::{self, Doc};
use crate::model::{FileFormat, ImageInfo, SectionKind};
use crate::sink::Sink;
use crate::{PreviewError, PreviewPolicy};

/// Why a picture could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureError {
    /// The document, or the kind of picture in it, is not shown (the text says which).
    Unsupported(String),
    /// The document or the picture is damaged, or violates a safety limit.
    Malformed(String),
    /// The id names no picture of this document.
    Missing,
}

impl core::fmt::Display for PictureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported(s) | Self::Malformed(s) => f.write_str(s),
            Self::Missing => f.write_str("the document has no such picture"),
        }
    }
}

impl core::error::Error for PictureError {}

fn from_decode(e: DecodeError) -> PictureError {
    match e {
        DecodeError::Unsupported(s) => PictureError::Unsupported(s),
        DecodeError::Malformed(s) | DecodeError::TooLarge(s) => PictureError::Malformed(s),
    }
}

pub(crate) fn from_preview(e: PreviewError) -> PictureError {
    match e {
        PreviewError::Unsupported(f) => {
            PictureError::Unsupported(format!("no pictures of {f:?} are shown"))
        }
        PreviewError::Encrypted => {
            PictureError::Unsupported("the document is password protected".into())
        }
        PreviewError::TooLarge(n) => {
            PictureError::Malformed(format!("input exceeds the {n} byte limit"))
        }
        PreviewError::Malformed(s) => PictureError::Malformed(s),
    }
}

/// The preview of a picture file: one page holding the one picture (id 0).
pub(crate) fn extract_file(
    data: &[u8],
    format: FileFormat,
    sink: &mut Sink<'_>,
) -> Result<(), PreviewError> {
    let facts = arca_image::inspect(data, &Limits::default())
        .map_err(|e| PreviewError::Malformed(e.to_string()))?;
    let Some(sec) = sink.open_section(SectionKind::Page, "", false) else { return Ok(()) };
    let media = match format {
        FileFormat::Png => "png",
        FileFormat::Jpeg => "jpeg",
        FileFormat::Gif => "gif",
        _ => "bmp",
    };
    let info = ImageInfo {
        alt: format!("{}×{}", facts.width, facts.height),
        media: media.to_owned(),
        bytes: u64::try_from(data.len()).unwrap_or(u64::MAX),
        external: false,
        id: 0,
    };
    sink.image(sec, info);
    Ok(())
}

/// Decodes picture `id` of the document `data`.
///
/// # Errors
/// [`PictureError`]: the document or picture is not shown, is damaged, or has no such picture.
pub fn picture(data: &[u8], id: u32, policy: &PreviewPolicy) -> Result<Image, PictureError> {
    if data.len() > policy.max_input_bytes {
        return Err(PictureError::Malformed(format!(
            "input exceeds the {} byte limit",
            policy.max_input_bytes
        )));
    }
    let limits = Limits::default();
    let bytes: Cow<'_, [u8]> = match detect::open(data, policy).map_err(from_preview)? {
        Doc::Picture { data, .. } if id == 0 => Cow::Borrowed(data),
        Doc::Picture { .. } => return Err(PictureError::Missing),
        Doc::Rtf(file) => Cow::Owned(
            crate::rtf::picture_bytes(file, usize::try_from(id).unwrap_or(usize::MAX))
                .ok_or(PictureError::Missing)?,
        ),
        Doc::Pdf(file) => {
            return match crate::pdf::picture(file, id, policy)? {
                crate::pdf::Found::Jpeg(jpeg) => {
                    arca_image::decode(&jpeg, &limits).map_err(from_decode)
                }
                crate::pdf::Found::Samples(image) => Ok(image),
            };
        }
        Doc::Opc { mut pkg, .. } | Doc::Odf { mut pkg, .. } => {
            let (_, bytes) =
                pkg.read_by_id(id).map_err(from_preview)?.ok_or(PictureError::Missing)?;
            Cow::Owned(bytes)
        }
        _ => {
            return Err(PictureError::Unsupported(
                "the pictures of this kind of document are not shown yet".into(),
            ));
        }
    };
    arca_image::decode(&bytes, &limits).map_err(from_decode)
}

/// Most pixels a sterile copy of a picture holds, and the most when the reader asks for the full
/// size. A larger picture is reduced by a whole factor (and the copy says so): the sandbox
/// interprets every step of the decoding and the writing, which costs time in proportion.
const COPY_MAX_PIXELS: u64 = 4_000_000;
const COPY_MAX_PIXELS_FULL: u64 = 16_000_000;

/// The longest side a picture of `w × h` may have in a sterile copy: its own if it fits in
/// [`COPY_MAX_PIXELS`], otherwise the largest side whose reduction (the decoders reduce by whole
/// factors) does.
fn copy_side_for(w: u32, h: u32, max_pixels: u64) -> u32 {
    let longest = w.max(h).max(1);
    for k in 1..=longest {
        let side = longest.div_ceil(k);
        let k = longest.div_ceil(side);
        if u64::from(w.div_ceil(k)) * u64::from(h.div_ceil(k)) <= max_pixels {
            return side;
        }
    }
    1
}

/// The changes a sterile copy made to a picture, for the reader.
fn copy_actions(f: &Facts, size: (u32, u32), notes: EncodeNotes) -> Vec<SanitizeAction> {
    let mut actions = Vec::new();
    let mut add = |kind: ActionKind, target: String| actions.push(SanitizeAction { kind, target });
    add(
        ActionKind::ConvertedContentType,
        format!("{} → PNG, written anew from the pixels", f.format.name().to_ascii_uppercase()),
    );
    if let Some(e) = &f.exif {
        let mut parts: Vec<&str> = Vec::new();
        if e.has_position {
            parts.push("GPS position");
        } else if e.gps {
            parts.push("GPS data");
        }
        if e.serial {
            parts.push("serial number");
        }
        if !e.make.is_empty() || !e.model.is_empty() {
            parts.push("camera make and model");
        }
        if !e.datetime.is_empty() {
            parts.push("date and time");
        }
        if !e.software.is_empty() {
            parts.push("software");
        }
        if !e.artist.is_empty() || !e.owner.is_empty() || !e.copyright.is_empty() {
            parts.push("author, owner, copyright");
        }
        if e.maker_note {
            parts.push("maker note");
        }
        let target = if parts.is_empty() {
            "EXIF".to_owned()
        } else {
            format!("EXIF: {}", parts.join(", "))
        };
        add(ActionKind::StrippedMetadata, target);
        if e.thumbnail {
            add(ActionKind::RemovedElement, "embedded thumbnail".into());
        }
    }
    if f.has(flag::ICC_PROFILE) {
        add(ActionKind::StrippedMetadata, "ICC colour profile (colours are shown as sRGB)".into());
    }
    if f.has(flag::XMP) {
        add(ActionKind::StrippedMetadata, "XMP".into());
    }
    if f.has(flag::COMMENT) {
        add(ActionKind::StrippedMetadata, "comment".into());
    }
    if f.has(flag::TEXT_CHUNKS) {
        add(ActionKind::StrippedMetadata, "text chunks".into());
    }
    if f.trailing > 0 {
        add(ActionKind::RemovedElement, format!("{} bytes after the end of the image", f.trailing));
    }
    if f.frames > 1 || f.has(flag::ANIMATED) {
        add(
            ActionKind::RemovedElement,
            format!("animation: only the first of {} frames is kept", f.frames),
        );
    }
    if f.has(flag::ORIENTED) {
        add(ActionKind::StrippedMetadata, "orientation (the picture was turned upright)".into());
    }
    if f.has(flag::CMYK) {
        add(ActionKind::ConvertedContentType, "CMYK → RGB (approximate colours)".into());
    }
    if f.has(flag::DOWNSCALED) {
        add(
            ActionKind::RemovedElement,
            format!("pixels: reduced from {}×{} to {}×{}", f.width, f.height, size.0, size.1),
        );
    }
    if f.has(flag::DAMAGED) {
        add(ActionKind::RemovedElement, "damaged part of the image (blank in the copy)".into());
    }
    if notes.cleared_hidden_colour {
        add(ActionKind::RemovedElement, "colour values of fully transparent pixels".into());
    }
    actions
}

/// Builds the sterile copy of a picture file: the pixels the sandbox decoded, written anew as a
/// plain PNG (see `arca_image::encode_png`) — no metadata, no thumbnail, no other frames, nothing
/// after the image, and none of the original's structure. The copy is read back and compared
/// with the pixels before it is returned.
///
/// A picture of more than 4 megapixels (16 with `full_size`) is reduced first.
///
/// Returns the new file and what was left out of it. The caller verifies the result (for the
/// sandbox guest that is `arca_xray::sanitize::sanitize_with`).
///
/// # Errors
/// [`PictureError`]: not a picture this crate reads, or damaged beyond use, or too large.
pub fn sanitize_picture(
    data: &[u8],
    full_size: bool,
) -> Result<(Vec<u8>, Vec<SanitizeAction>), PictureError> {
    sanitize_picture_within(data, if full_size { COPY_MAX_PIXELS_FULL } else { COPY_MAX_PIXELS })
}

fn sanitize_picture_within(
    data: &[u8],
    max_pixels: u64,
) -> Result<(Vec<u8>, Vec<SanitizeAction>), PictureError> {
    let limits = Limits::default();
    let facts = arca_image::inspect(data, &limits).map_err(from_decode)?;
    let side = copy_side_for(facts.width, facts.height, max_pixels);
    let Image { width, height, rgba, facts } =
        arca_image::decode(data, &Limits { max_out_side: side, ..limits }).map_err(from_decode)?;
    let (png, notes) = arca_image::encode_png(width, height, &rgba)
        .map_err(|e| PictureError::Malformed(e.to_string()))?;
    // What a reader of the copy will see must be what the sandbox decoded. Only a fingerprint of
    // the pixels is kept, so that the picture and its copy are never in memory together.
    let wanted = arca_image::pixels_fingerprint(width, height, &rgba);
    drop(rgba);
    let back =
        arca_image::decode(&png, &Limits { max_out_side: width.max(height), ..Limits::default() })
            .map_err(from_decode)?;
    if arca_image::pixels_fingerprint(back.width, back.height, &back.rgba) != wanted {
        return Err(PictureError::Malformed("the sterile copy does not match the picture".into()));
    }
    drop(back);
    let actions = copy_actions(&facts, (width, height), notes);
    Ok((png, actions))
}

#[cfg(test)]
mod tests;
