//! VBA project extraction per [MS-OVBA].
//!
//! The `dir` stream and every module's source are stored with the MS-OVBA
//! LZ77 variant. This module decompresses them with strict bounds, parses
//! the `dir` records generically (record id + size, with the documented
//! `PROJECTVERSION` exception) and returns the module sources as text for
//! read-only display. Nothing here interprets or executes VBA.

use core::fmt;

use arca_container::bytes::LeReader;
use arca_container::cfb::{CompoundFile, EntryKind};
use arca_container::text::{CodePage, decode_codepage, utf16le_lossy};

use std::collections::BTreeSet;

use crate::macro_analysis;
use crate::report::{MacroLanguage, MacroModule, Severity, ThreatKind};
use crate::{Abort, Ctx};

/// Errors of the decompressor / `dir` parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VbaError {
    BadSignature,
    BadChunk,
    BadCopyToken,
    TooLarge,
    Truncated,
}

impl fmt::Display for VbaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadSignature => "compressed container signature is not 0x01",
            Self::BadChunk => "invalid compressed chunk header",
            Self::BadCopyToken => "copy token points before the chunk start",
            Self::TooLarge => "decompressed data exceeds the limit",
            Self::Truncated => "truncated record",
        })
    }
}

/// Decompresses an MS-OVBA compressed container (2.4.1).
///
/// # Errors
/// [`VbaError::TooLarge`] if the output would exceed `max_out`.
pub(crate) fn decompress(input: &[u8], max_out: usize) -> Result<Vec<u8>, VbaError> {
    match decompress_prefix(input, max_out)? {
        (out, false) => Ok(out),
        (_, true) => Err(VbaError::TooLarge),
    }
}

/// Like [`decompress`] but stops at `max_out` and reports truncation.
pub(crate) fn decompress_prefix(input: &[u8], max_out: usize) -> Result<(Vec<u8>, bool), VbaError> {
    if input.first() != Some(&0x01) {
        return Err(VbaError::BadSignature);
    }
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 1usize;
    while pos < input.len() {
        let header = arca_container::bytes::u16_le(input, pos).ok_or(VbaError::BadChunk)?;
        let size = usize::from(header & 0x0FFF) + 3;
        if (header >> 12) & 0x7 != 0b011 {
            return Err(VbaError::BadChunk);
        }
        let compressed = header & 0x8000 != 0;
        let chunk_end = pos.saturating_add(size).min(input.len());
        let mut p = pos + 2;
        let chunk_start_out = out.len();
        if !compressed {
            let raw =
                input.get(p..p.saturating_add(4096).min(input.len())).ok_or(VbaError::BadChunk)?;
            if out.len().saturating_add(raw.len()) > max_out {
                let room = max_out.saturating_sub(out.len());
                out.extend_from_slice(raw.get(..room).unwrap_or(&[]));
                return Ok((out, true));
            }
            out.extend_from_slice(raw);
            pos = p.saturating_add(4096);
            continue;
        }
        while p < chunk_end {
            let flags = *input.get(p).ok_or(VbaError::Truncated)?;
            p += 1;
            for bit in 0..8 {
                if p >= chunk_end {
                    break;
                }
                if flags & (1 << bit) == 0 {
                    out.push(*input.get(p).ok_or(VbaError::Truncated)?);
                    p += 1;
                } else {
                    let token =
                        arca_container::bytes::u16_le(input, p).ok_or(VbaError::Truncated)?;
                    p += 2;
                    let diff = out.len() - chunk_start_out;
                    if diff == 0 {
                        return Err(VbaError::BadCopyToken);
                    }
                    let mut bit_count = 4u32;
                    while bit_count < 12 && (1usize << bit_count) < diff {
                        bit_count += 1;
                    }
                    let length_mask: u16 = 0xFFFF >> bit_count;
                    let offset_mask: u16 = !length_mask;
                    let length = usize::from(token & length_mask) + 3;
                    let offset = usize::from((token & offset_mask) >> (16 - bit_count)) + 1;
                    if offset > diff {
                        return Err(VbaError::BadCopyToken);
                    }
                    if out.len().saturating_add(length) > max_out {
                        return Ok((out, true));
                    }
                    for _ in 0..length {
                        let b = *out.get(out.len() - offset).ok_or(VbaError::BadCopyToken)?;
                        out.push(b);
                    }
                }
                if out.len() > max_out {
                    out.truncate(max_out);
                    return Ok((out, true));
                }
            }
        }
        pos = chunk_end;
    }
    Ok((out, false))
}

/// Most modules read from one project.
pub(crate) const MAX_MODULES: usize = 4096;

/// One module described in the `dir` stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModuleInfo {
    pub(crate) name: String,
    pub(crate) stream: String,
    pub(crate) offset: u32,
    pub(crate) document: bool,
}

/// The parts of `dir` Arca needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirInfo {
    pub(crate) codepage: u16,
    pub(crate) project: String,
    pub(crate) modules: Vec<ModuleInfo>,
    /// The `dir` stream lists more modules than [`MAX_MODULES`]; the rest were not read.
    pub(crate) truncated: bool,
}

/// Parses a decompressed `dir` stream.
pub(crate) fn parse_dir(dir: &[u8]) -> Result<DirInfo, VbaError> {
    let mut info = DirInfo { codepage: 1252, ..DirInfo::default() };
    let mut r = LeReader::new(dir);
    let mut current: Option<ModuleInfo> = None;
    let cp = |info: &DirInfo| CodePage::from_id(info.codepage);
    while !r.is_empty() {
        let id = r.u16().ok_or(VbaError::Truncated)?;
        if id == 0x0009 {
            // PROJECTVERSION: Reserved(4) + Major(4) + Minor(2); the "size"
            // field does not describe the payload.
            r.skip(10).ok_or(VbaError::Truncated)?;
            continue;
        }
        let size = usize::try_from(r.u32().ok_or(VbaError::Truncated)?)
            .map_err(|_| VbaError::Truncated)?;
        let data = r.bytes(size).ok_or(VbaError::Truncated)?;
        match id {
            0x0003 => {
                if let Some(v) = arca_container::bytes::u16_le(data, 0) {
                    info.codepage = v;
                }
            }
            0x0004 => info.project = decode_codepage(data, cp(&info)),
            0x0019 => {
                if let Some(m) = current.take() {
                    info.modules.push(m);
                }
                current = Some(ModuleInfo {
                    name: decode_codepage(data, cp(&info)),
                    ..ModuleInfo::default()
                });
            }
            0x0047 => {
                if let Some(m) = current.as_mut() {
                    let u = utf16le_lossy(data);
                    if !u.is_empty() {
                        m.name = u;
                    }
                }
            }
            0x001A => {
                if let Some(m) = current.as_mut() {
                    m.stream = decode_codepage(data, cp(&info));
                }
            }
            0x0032 => {
                if let Some(m) = current.as_mut() {
                    let u = utf16le_lossy(data);
                    if !u.is_empty() {
                        m.stream = u;
                    }
                }
            }
            0x0031 => {
                if let (Some(m), Some(off)) =
                    (current.as_mut(), arca_container::bytes::u32_le(data, 0))
                {
                    m.offset = off;
                }
            }
            0x0022 => {
                if let Some(m) = current.as_mut() {
                    m.document = true;
                }
            }
            0x002B => {
                if let Some(m) = current.take() {
                    info.modules.push(m);
                }
            }
            0x0010 => break,
            _ => {}
        }
        if info.modules.len() >= MAX_MODULES {
            info.truncated = true;
            break;
        }
    }
    if let Some(m) = current.take() {
        info.modules.push(m);
    }
    Ok(info)
}

/// Finds every `VBA` storage that carries a `dir` stream.
pub(crate) fn find_projects(cf: &CompoundFile<'_>) -> Vec<usize> {
    cf.entries()
        .iter()
        .enumerate()
        .filter(|(i, e)| {
            e.kind == EntryKind::Storage
                && e.name.eq_ignore_ascii_case("VBA")
                && cf.child(*i, "dir").is_some()
        })
        .map(|(i, _)| i)
        .collect()
}

/// Extracts all modules of the project rooted at storage `vba_index`.
///
/// Every decompression is charged to the document budget, a stream is decompressed at most once
/// however many modules name it, and the sources kept are capped in total, so a project of
/// thousands of modules cannot multiply the work or the memory of one stream.
pub(crate) fn extract(
    ctx: &mut Ctx<'_>,
    cf: &CompoundFile<'_>,
    vba_index: usize,
    location: &str,
) -> Result<(), Abort> {
    let base = cf.entries().get(vba_index).map(|e| e.path.clone()).unwrap_or_default();
    let loc = Ctx::join(location, &base);
    let Some(dir_idx) = cf.child(vba_index, "dir") else { return Ok(()) };
    let dir_raw = match cf.read_stream(dir_idx) {
        Ok(d) => d,
        Err(e) => {
            ctx.report.add(ThreatKind::MalformedContainer, &loc, format!("VBA dir stream: {e}"));
            ctx.report.add(
                ThreatKind::VbaMacros,
                &loc,
                "VBA project present (unreadable dir stream)",
            );
            return Ok(());
        }
    };
    let info = match decompress(&dir_raw, 4 << 20).and_then(|d| parse_dir(&d)) {
        Ok(i) => i,
        Err(e) => {
            ctx.report.add(ThreatKind::MalformedContainer, &loc, format!("VBA dir stream: {e}"));
            ctx.report.add(
                ThreatKind::VbaMacros,
                &loc,
                "VBA project present (undecodable dir stream)",
            );
            return Ok(());
        }
    };
    if info.truncated {
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            &loc,
            format!("the project lists more than {MAX_MODULES} modules; the rest were not read"),
        );
    }
    let cp = CodePage::from_id(info.codepage);
    let limit = ctx.policy.max_macro_source_bytes;
    // Source text kept and analysed across all modules of the project.
    let mut remaining_total = limit.saturating_mul(8);
    let mut skipped = 0usize;
    let mut shared = 0usize;
    let mut seen_streams: BTreeSet<usize> = BTreeSet::new();
    let mut names = Vec::new();
    for m in &info.modules {
        let Some(stream_idx) = cf.child(vba_index, &m.stream) else {
            ctx.report.add(
                ThreatKind::MalformedContainer,
                &loc,
                format!("module stream {:?} is missing", m.stream),
            );
            continue;
        };
        if !seen_streams.insert(stream_idx) {
            shared += 1;
            continue;
        }
        if remaining_total == 0 {
            skipped += 1;
            continue;
        }
        let raw = match cf.read_stream(stream_idx) {
            Ok(r) => r,
            Err(e) => {
                ctx.report.add(
                    ThreatKind::MalformedContainer,
                    &loc,
                    format!("module {:?}: {e}", m.name),
                );
                continue;
            }
        };
        let offset = usize::try_from(m.offset).unwrap_or(usize::MAX);
        let compressed = raw.get(offset..).unwrap_or(&[]);
        let room = limit.min(remaining_total);
        let (source, truncated) = match decompress_prefix(compressed, room) {
            Ok((s, truncated)) => {
                ctx.budget
                    .charge(s.len() as u64, compressed.len() as u64)
                    .map_err(|e| Abort::new(ThreatKind::CompressionBomb, &loc, e.to_string()))?;
                remaining_total = remaining_total.saturating_sub(s.len());
                (decode_codepage(&s, cp), truncated)
            }
            Err(e) => {
                ctx.report.add(
                    ThreatKind::MalformedContainer,
                    &loc,
                    format!("module {:?} source: {e}", m.name),
                );
                continue;
            }
        };
        let analysis = macro_analysis::analyze(&source, MacroLanguage::Vba);
        // VBA stomping: p-code present but the source has no procedures.
        let has_code = source.lines().any(|l| {
            let t = l.trim_start().to_ascii_lowercase();
            !t.is_empty()
                && !t.starts_with("attribute ")
                && !t.starts_with('\'')
                && !t.starts_with("option ")
        });
        if !has_code && offset > 2048 {
            ctx.report.add_with(
                ThreatKind::VbaMacros,
                Severity::Critical,
                &loc,
                format!("module {:?}: compiled p-code ({offset} bytes) without matching source — possible VBA stomping", m.name),
            );
        }
        if !analysis.auto_exec.is_empty() {
            ctx.report.add(
                ThreatKind::MacroAutoExec,
                &loc,
                format!("{}: {}", m.name, analysis.auto_exec.join(", ")),
            );
        }
        names.push(m.name.clone());
        ctx.report.macros.push(MacroModule {
            language: MacroLanguage::Vba,
            location: loc.clone(),
            name: m.name.clone(),
            source,
            truncated,
            auto_exec: analysis.auto_exec,
            suspicious: analysis.suspicious,
            iocs: analysis.iocs,
        });
    }
    if shared > 0 {
        ctx.report.add(
            ThreatKind::ParserDifferential,
            &loc,
            format!("{shared} module(s) name a stream that another module already uses; it was read once"),
        );
    }
    if skipped > 0 {
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            &loc,
            format!("{skipped} module(s) were not read: the macro source of the project exceeds the analysis limit"),
        );
    }
    let project =
        if info.project.is_empty() { String::from("VBAProject") } else { info.project.clone() };
    ctx.report.add(
        ThreatKind::VbaMacros,
        &loc,
        format!("VBA project {project:?} with {} module(s): {}", names.len(), names.join(", ")),
    );
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Literal-only MS-OVBA compression (valid per the format): every token is
    /// a literal, chunks hold at most 4096 decompressed bytes.
    ///
    /// Full 4096-byte chunks use the raw chunk form (as Office does when
    /// compression does not pay off); a trailing partial chunk uses literal
    /// tokens, split so that no chunk exceeds the 4098-byte maximum.
    pub(crate) fn compress_literal(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x01];
        for chunk in data.chunks(4096) {
            if chunk.len() == 4096 {
                out.extend_from_slice(&0x3FFFu16.to_le_bytes());
                out.extend_from_slice(chunk);
                continue;
            }
            for piece in chunk.chunks(3640) {
                let mut body = Vec::new();
                for group in piece.chunks(8) {
                    body.push(0x00);
                    body.extend_from_slice(group);
                }
                let size = u16::try_from(body.len() + 2 - 3).unwrap();
                let header = 0x8000 | 0x3000 | size;
                out.extend_from_slice(&header.to_le_bytes());
                out.extend_from_slice(&body);
            }
        }
        out
    }

    #[test]
    fn spec_example_decompresses() {
        // MS-OVBA 3.2.2 "Normal Compression Example".
        let compressed: [u8; 51] = [
            0x01, 0x2F, 0xB0, 0x00, 0x23, 0x61, 0x61, 0x61, 0x62, 0x63, 0x64, 0x65, 0x82, 0x66,
            0x00, 0x70, 0x61, 0x67, 0x68, 0x69, 0x6A, 0x01, 0x38, 0x08, 0x61, 0x6B, 0x6C, 0x00,
            0x30, 0x6D, 0x6E, 0x6F, 0x70, 0x06, 0x71, 0x02, 0x70, 0x04, 0x10, 0x72, 0x73, 0x74,
            0x75, 0x76, 0x10, 0x77, 0x78, 0x79, 0x7A, 0x00, 0x3C,
        ];
        let out = decompress(&compressed, 1 << 20).unwrap();
        assert_eq!(out, b"#aaabcdefaaaaghijaaaaaklaaamnopqaaaaaaaaaaaarstuvwxyzaaa".to_vec());
    }

    #[test]
    fn literal_roundtrip_and_limits() {
        let src = b"Sub AutoOpen()\r\n  Shell \"calc\"\r\nEnd Sub\r\n".repeat(200);
        let c = compress_literal(&src);
        assert_eq!(decompress(&c, 1 << 20).unwrap(), src);
        assert_eq!(decompress(&c, 100), Err(VbaError::TooLarge));
        assert_eq!(decompress(&[0x02], 10), Err(VbaError::BadSignature));
    }

    #[test]
    fn hostile_copy_token_is_rejected() {
        // Flag byte says "copy token" for the very first token of a chunk.
        let evil = [0x01, 0x03, 0xB0, 0x01, 0xFF, 0xFF];
        assert_eq!(decompress(&evil, 1 << 20), Err(VbaError::BadCopyToken));
    }

    #[test]
    fn decompressor_never_panics_on_noise() {
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut buf = vec![0x01];
            for _ in 0..64 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                buf.push(seed.to_le_bytes()[2]);
            }
            let _ = decompress(&buf, 1 << 16);
        }
    }
}
