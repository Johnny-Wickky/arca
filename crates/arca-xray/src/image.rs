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

/// Bytes of metadata, of a kind that no flag of the picture names, from which it is reported
/// anyway (a few bytes of such data are routine; hundreds are a place to keep something).
const OTHER_METADATA_THRESHOLD: u64 = 128;

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
    let personal = !(e.artist.is_empty() && e.owner.is_empty() && e.copyright.is_empty())
        || e.serial
        || e.free_text;
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
    if e.free_text {
        who.push("a free-text description or comment".to_owned());
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
    } else if f.exif.is_none() && f.metadata_bytes > OTHER_METADATA_THRESHOLD {
        // Metadata of a kind no flag names (an editor's private records, a suggested palette, a
        // thumbnail inside the JFIF header): no kind, but too much to leave unsaid.
        ctx.report.add(
            ThreatKind::ImageHiddenMetadata,
            location,
            format!("{} bytes of other metadata", f.metadata_bytes),
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
    if facts.unaccounted > 0 {
        // Private blocks, extension data, gaps, padding that carries values, data after the end
        // of a compressed stream: bytes the decoder cannot explain are where a payload hides.
        ctx.report.add(
            ThreatKind::TrailingData,
            location,
            format!(
                "{} bytes of the file are neither picture data nor metadata of a known kind",
                facts.unaccounted
            ),
        );
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

    /// A JPEG: the given segments after SOI, then a one-block frame, a scan and the end marker.
    fn jpeg(segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        for (marker, body) in segments {
            out.extend_from_slice(&[0xFF, *marker]);
            out.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_be_bytes());
            out.extend_from_slice(body);
        }
        out.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
        ]);
        out.extend_from_slice(&[
            0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x12, 0x34,
        ]);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    /// A PNG of one pixel with the given extra chunks before the image data.
    fn png(extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
            out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            let mut crc = arca_digest::Crc32::new();
            crc.update(kind);
            crc.update(body);
            out.extend_from_slice(&crc.finalize().to_be_bytes());
        }
        let mut out = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]);
        for (kind, body) in extra {
            chunk(&mut out, kind, body);
        }
        // A stored zlib stream of one grey pixel: filter 0, value 0x80, Adler-32 0x00820081.
        let idat = [0x78, 0x01, 0x01, 0x02, 0x00, 0xFD, 0xFF, 0x00, 0x80, 0x00, 0x82, 0x00, 0x81];
        chunk(&mut out, b"IDAT", &idat);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_private_block_is_reported_as_bytes_nobody_can_explain() {
        let mut zip = b"PK\x03\x04".to_vec();
        zip.resize(200, 1);
        let r = crate::scan_default(&jpeg(&[(0xFD, zip.clone())]));
        let f = r.findings.iter().find(|f| f.kind == ThreatKind::TrailingData).unwrap();
        assert!(f.detail.starts_with("200 bytes"), "{}", f.detail);
        let r = crate::scan_default(&png(&[(b"prVt", zip)]));
        let f = r.findings.iter().find(|f| f.kind == ThreatKind::TrailingData).unwrap();
        assert!(f.detail.starts_with("212 bytes"), "{}", f.detail);
        // Without it, the same pictures say nothing.
        for clean in [jpeg(&[]), png(&[])] {
            let r = crate::scan_default(&clean);
            assert!(!r.has(ThreatKind::TrailingData), "{:#?}", r.findings);
            assert!(!r.has(ThreatKind::MalformedContainer), "{:#?}", r.findings);
        }
    }

    #[test]
    fn a_pdf_header_inside_a_picture_is_a_polyglot() {
        let r = crate::scan_default(&png(&[(b"tEXt", b"Comment\0%PDF-1.7 hidden".to_vec())]));
        assert!(r.has(ThreatKind::PolyglotFile), "{:#?}", r.findings);
        let r = crate::scan_default(&jpeg(&[(0xFE, b"..%PDF-1.4".to_vec())]));
        assert!(r.has(ThreatKind::PolyglotFile), "{:#?}", r.findings);
    }

    #[test]
    fn other_metadata_is_reported_from_a_few_hundred_bytes() {
        let mut photoshop = b"Photoshop 3.0\0".to_vec();
        photoshop.resize(400, 0);
        let r = crate::scan_default(&jpeg(&[(0xED, photoshop)]));
        let f = r.findings.iter().find(|f| f.kind == ThreatKind::ImageHiddenMetadata).unwrap();
        assert!(f.detail.contains("400 bytes of other metadata"), "{}", f.detail);
        assert!(!r.has(ThreatKind::TrailingData));
    }
}
