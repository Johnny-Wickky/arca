//! Picture files: what a PNG, JPEG, GIF or BMP carries besides its pixels.
//!
//! Only the headers are read (`arca_image::inspect`): the pixels of a picture are decoded later,
//! in a separate sandbox run, and only when the reader asks for them. What matters here is what
//! the file *says about its owner or hides*: where a photograph was taken, which camera made it,
//! the uncropped thumbnail an editing program left behind, comments and metadata packets, frames
//! of an animation that are never shown, and any bytes after the end of the picture (a ZIP, a PDF
//! or a program appended to a picture is the oldest polyglot trick there is).

use arca_image::{DecodeError, ExifFacts, Facts, Limits, flag};

use crate::report::{Severity, ThreatKind};
use crate::{Abort, Ctx, FileFormat};

/// The first bytes of what is appended after a picture, if they identify something dangerous.
fn appended_kind(tail: &[u8]) -> Option<&'static str> {
    let starts = |m: &[u8]| tail.starts_with(m);
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        Some("a ZIP archive")
    } else if starts(b"%PDF-") {
        Some("a PDF document")
    } else if starts(b"MZ") {
        Some("a Windows program")
    } else if starts(b"\x7FELF") {
        Some("a Linux program")
    } else if starts(b"Rar!") || starts(b"7z\xBC\xAF") {
        Some("a compressed archive")
    } else if starts(b"\xD0\xCF\x11\xE0") {
        Some("an Office document")
    } else if starts(b"{\\rt") {
        Some("an RTF document")
    } else if starts(b"#!")
        || starts(b"<?php")
        || tail.get(..8).is_some_and(|h| h.eq_ignore_ascii_case(b"<script>"))
    {
        Some("a script")
    } else {
        None
    }
}

fn exif_findings(ctx: &mut Ctx<'_>, location: &str, e: &ExifFacts) {
    if e.gps {
        let detail = if e.has_position {
            format!(
                "taken at {:.5}, {:.5} (latitude, longitude)",
                e.lat_micro as f64 / 1e6,
                e.lon_micro as f64 / 1e6
            )
        } else {
            "a GPS block is present".to_owned()
        };
        ctx.report.add(ThreatKind::ImageGpsLocation, location, detail);
    }
    let mut who = Vec::new();
    for (label, v) in
        [("camera", format!("{} {}", e.make, e.model)), ("software", e.software.clone())]
    {
        if !v.trim().is_empty() {
            who.push(format!("{label}: {}", v.trim()));
        }
    }
    let personal =
        !(e.artist.is_empty() && e.owner.is_empty() && e.copyright.is_empty()) || e.serial;
    for (label, v) in
        [("photographer", &e.artist), ("owner", &e.owner), ("copyright", &e.copyright)]
    {
        if !v.is_empty() {
            who.push(format!("{label}: {v}"));
        }
    }
    if e.serial {
        who.push("a serial number".to_owned());
    }
    if !e.datetime.is_empty() {
        who.push(format!("date: {}", e.datetime));
    }
    if !who.is_empty() {
        let severity = if personal { Severity::Low } else { Severity::Info };
        ctx.report.add_with(ThreatKind::ImageCameraIdentity, severity, location, who.join("; "));
    }
    if e.thumbnail {
        ctx.report.add(
            ThreatKind::ImageEmbeddedThumbnail,
            location,
            "a small copy of the picture is embedded (editing programs leave the original, uncropped one in it)",
        );
    }
}

fn other_metadata(ctx: &mut Ctx<'_>, location: &str, f: &Facts) {
    let mut parts = Vec::new();
    for (bit, what) in [
        (flag::XMP, "an XMP packet"),
        (flag::COMMENT, "a comment"),
        (flag::TEXT_CHUNKS, "text chunks"),
        (flag::ICC_PROFILE, "a colour profile"),
    ] {
        if f.has(bit) {
            parts.push(what);
        }
    }
    if f.exif.as_ref().is_some_and(|e| e.maker_note) {
        parts.push("a maker note");
    }
    if !parts.is_empty() {
        ctx.report.add(
            ThreatKind::ImageHiddenMetadata,
            location,
            format!("{} ({} bytes in all)", parts.join(", "), f.metadata_bytes),
        );
    }
}

/// Scans a picture file.
pub(crate) fn scan(
    ctx: &mut Ctx<'_>,
    data: &[u8],
    location: &str,
    format: FileFormat,
) -> Result<FileFormat, Abort> {
    let limits = Limits::default();
    let facts = match arca_image::inspect(data, &limits) {
        Ok(f) => f,
        Err(DecodeError::TooLarge(why)) => {
            return Err(Abort::new(ThreatKind::OversizedImage, location, why));
        }
        Err(DecodeError::Malformed(why)) => {
            return Err(Abort::new(ThreatKind::MalformedContainer, location, why));
        }
        Err(DecodeError::Unsupported(why)) => {
            ctx.report.add(ThreatKind::UnsupportedFormat, location, why);
            return Ok(format);
        }
    };
    if facts.trailing > 0 {
        let n = usize::try_from(facts.trailing).unwrap_or(usize::MAX);
        let tail = data.get(data.len().saturating_sub(n)..).unwrap_or(&[]);
        match appended_kind(tail) {
            Some(what) => ctx.report.add(
                ThreatKind::PolyglotFile,
                location,
                format!("{n} bytes after the end of the picture are {what}"),
            ),
            None => ctx.report.add(
                ThreatKind::TrailingData,
                location,
                format!("{n} bytes follow the end of the picture"),
            ),
        }
    }
    if let Some(e) = &facts.exif {
        exif_findings(ctx, location, e);
    }
    other_metadata(ctx, location, &facts);
    if facts.has(flag::ANIMATED) && facts.frames > 1 {
        ctx.report.add(
            ThreatKind::ImageHiddenFrames,
            location,
            format!("{} frames; only the first is shown", facts.frames),
        );
    }
    if facts.has(flag::DAMAGED) {
        ctx.report.add(
            ThreatKind::MalformedContainer,
            location,
            "the picture is damaged; what could be read is shown and the rest is blank",
        );
    }
    Ok(format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appended_payloads_are_named() {
        assert_eq!(appended_kind(b"PK\x03\x04rest"), Some("a ZIP archive"));
        assert_eq!(appended_kind(b"%PDF-1.7"), Some("a PDF document"));
        assert_eq!(appended_kind(b"MZ\x90\0"), Some("a Windows program"));
        assert_eq!(appended_kind(b"<SCRIPT>alert(1)"), Some("a script"));
        assert_eq!(appended_kind(b"\0\0\0\0padding"), None);
        assert_eq!(appended_kind(b""), None);
    }
}
