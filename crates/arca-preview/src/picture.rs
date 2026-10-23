//! Pictures: the description of a picture file in the preview, and the decoding of one picture
//! of a document when the reader asks for it.
//!
//! The preview only *describes* pictures ([`crate::model::ImageInfo`], with an `id`). To see one,
//! the app starts a separate sandbox run with that id; [`picture`] finds the picture's bytes in
//! the document, decodes it with `arca-image` (reduced, upright, without metadata) and returns
//! pixels. What the id means belongs to the format of the document and is never interpreted
//! anywhere but here.

use std::borrow::Cow;

use arca_image::{DecodeError, Image, Limits};

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

fn from_preview(e: PreviewError) -> PictureError {
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
