//! PDF triage (lexical, in the spirit of `pdfid`, plus Flate stream inspection).
//!
//! A full PDF object parser belongs to the Phase 3 renderer. For the X-ray,
//! every name token is normalised (`/J#61vaScript` → `/JavaScript`) and
//! counted in the raw file *and* inside every Flate-compressed stream
//! (object streams routinely hide `/JavaScript` and `/OpenAction`).
//! Decompression is charged against the document budget.

use crate::format::FileFormat;
use crate::macro_analysis;
use crate::report::{MacroLanguage, MacroModule, MetaKey, RefKind, Severity, ThreatKind};
use crate::{Abort, Ctx, memfind, ooxml};

#[derive(Debug, Default)]
struct Counts {
    javascript: u32,
    /// `/OpenAction` whose value is an action (dictionary or reference).
    open_action: u32,
    /// `/OpenAction [page /XYZ ...]` — a plain destination, benign.
    open_destination: u32,
    aa: u32,
    launch: u32,
    embedded_file: u32,
    rich_media: u32,
    xfa: u32,
    submit_form: u32,
    import_data: u32,
    goto_r: u32,
    goto_e: u32,
    encrypt: u32,
    jbig2: u32,
    obfuscated: u32,
    uri: u32,
}

fn is_delim(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Returns `(normalised name, was_obfuscated, end index)` for a name at `i` (pointing at '/').
fn read_name(data: &[u8], i: usize) -> (String, bool, usize) {
    let mut j = i + 1;
    let mut out = Vec::new();
    let mut obf = false;
    while let Some(&b) = data.get(j) {
        if is_delim(b) || out.len() > 127 {
            break;
        }
        if b == b'#'
            && let (Some(h), Some(l)) =
                (data.get(j + 1).and_then(|c| hexval(*c)), data.get(j + 2).and_then(|c| hexval(*c)))
        {
            out.push((h << 4) | l);
            obf = true;
            j += 3;
            continue;
        }
        out.push(b);
        j += 1;
    }
    (String::from_utf8_lossy(&out).into_owned(), obf, j)
}

fn count_names(data: &[u8], c: &mut Counts) {
    let mut i = 0usize;
    while let Some(rel) = data.get(i..).and_then(|d| d.iter().position(|&b| b == b'/')) {
        let at = i + rel;
        let (name, obf, end) = read_name(data, at);
        let hit = match name.as_str() {
            "JavaScript" | "JS" => {
                c.javascript += 1;
                true
            }
            "OpenAction" => {
                let mut j = end;
                while data.get(j).is_some_and(u8::is_ascii_whitespace) {
                    j += 1;
                }
                if data.get(j) == Some(&b'[') {
                    c.open_destination += 1;
                } else {
                    c.open_action += 1;
                }
                true
            }
            "AA" => {
                c.aa += 1;
                true
            }
            "Launch" => {
                c.launch += 1;
                true
            }
            "EmbeddedFile" | "EmbeddedFiles" => {
                c.embedded_file += 1;
                true
            }
            "RichMedia" | "Movie" | "Sound" | "Rendition" | "Flash" => {
                c.rich_media += 1;
                true
            }
            "XFA" => {
                c.xfa += 1;
                true
            }
            "SubmitForm" => {
                c.submit_form += 1;
                true
            }
            "ImportData" => {
                c.import_data += 1;
                true
            }
            "GoToR" => {
                c.goto_r += 1;
                true
            }
            "GoToE" => {
                c.goto_e += 1;
                true
            }
            "Encrypt" => {
                c.encrypt += 1;
                true
            }
            "JBIG2Decode" => {
                c.jbig2 += 1;
                true
            }
            "URI" => {
                c.uri += 1;
                true
            }
            _ => false,
        };
        if hit && obf {
            c.obfuscated += 1;
        }
        i = end.max(at + 1);
    }
}

/// Parses a PDF literal string starting at `i` (pointing at '(').
fn literal_string(data: &[u8], i: usize, max: usize) -> Option<String> {
    let mut j = i + 1;
    let mut depth = 1u32;
    let mut out = Vec::new();
    while let Some(&b) = data.get(j) {
        if out.len() >= max {
            break;
        }
        match b {
            b'\\' => {
                let n = *data.get(j + 1)?;
                j += 2;
                match n {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'0'..=b'7' => {
                        let mut v = u32::from(n - b'0');
                        for _ in 0..2 {
                            match data.get(j) {
                                Some(&d @ b'0'..=b'7') => {
                                    v = v * 8 + u32::from(d - b'0');
                                    j += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(u8::try_from(v & 0xFF).unwrap_or(0));
                    }
                    b'\r' | b'\n' => {}
                    other => out.push(other),
                }
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        out.push(b);
        j += 1;
    }
    Some(decode_pdf_text(&out))
}

fn hex_string(data: &[u8], i: usize, max: usize) -> Option<String> {
    let mut j = i + 1;
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    while let Some(&b) = data.get(j) {
        if b == b'>' || out.len() >= max {
            break;
        }
        if let Some(v) = hexval(b) {
            match hi.take() {
                Some(h) => out.push((h << 4) | v),
                None => hi = Some(v),
            }
        }
        j += 1;
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    Some(decode_pdf_text(&out))
}

/// PDF text strings are PDFDocEncoding or UTF-16BE with BOM.
fn decode_pdf_text(raw: &[u8]) -> String {
    if let [0xFE, 0xFF, rest @ ..] = raw {
        let units = rest
            .chunks_exact(2)
            .filter_map(|c| <[u8; 2]>::try_from(c).ok())
            .map(u16::from_be_bytes);
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    raw.iter().map(|&b| char::from(b)).collect()
}

/// Returns the string value following `key` (e.g. `/Author`) at each occurrence.
fn values_after(data: &[u8], key: &[u8], max_items: usize, max_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while out.len() < max_items {
        let Some(rel) = data.get(from..).and_then(|d| memfind(d, key)) else { break };
        let mut j = from + rel + key.len();
        if data.get(j).is_some_and(|b| !is_delim(*b)) {
            from = j;
            continue;
        }
        while data.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let v = match data.get(j) {
            Some(b'(') => literal_string(data, j, max_len),
            Some(b'<') if data.get(j + 1) != Some(&b'<') => hex_string(data, j, max_len),
            _ => None,
        };
        if let Some(v) = v {
            out.push(v);
        }
        from = j.max(from + rel + 1);
    }
    out
}

/// Scans a PDF.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) -> Result<FileFormat, Abort> {
    ctx.identified(FileFormat::Pdf);
    crate::check_polyglot(ctx, data, loc, true);
    let mut counts = Counts::default();
    count_names(data, &mut counts);
    let mut scripts: Vec<String> = values_after(data, b"/JS", 64, 1 << 20);
    let mut uris: Vec<String> = values_after(data, b"/URI", 256, 4096);

    // Flate streams.
    let mut streams = 0usize;
    let mut from = 0usize;
    while let Some(rel) = data.get(from..).and_then(|d| memfind(d, b"stream")) {
        let at = from + rel;
        from = at + 6;
        if data.get(at.saturating_sub(3)..at) == Some(b"end") {
            continue;
        }
        let mut start = at + 6;
        match (data.get(start), data.get(start + 1)) {
            (Some(b'\r'), Some(b'\n')) => start += 2,
            (Some(b'\n' | b'\r'), _) => start += 1,
            _ => continue,
        }
        let dict = data.get(at.saturating_sub(1024)..at).unwrap_or(&[]);
        let dict_start = dict.windows(2).rposition(|w| w == b"<<").unwrap_or(0);
        let dict = dict.get(dict_start..).unwrap_or(&[]);
        let flate = memfind(dict, b"/FlateDecode").is_some()
            || memfind(dict, b"/Fl ").is_some()
            || memfind(dict, b"/Fl]").is_some();
        let Some(end_rel) = data.get(start..).and_then(|d| memfind(d, b"endstream")) else { break };
        let raw = data.get(start..start + end_rel).unwrap_or(&[]);
        from = start + end_rel + 9;
        if !flate {
            continue;
        }
        streams += 1;
        if streams > ctx.policy.max_pdf_streams {
            ctx.report.add(
                ThreatKind::ScanLimitReached,
                loc,
                "PDF stream limit reached; remaining streams not inspected",
            );
            break;
        }
        // Each stream may expand at most to what the budget still allows.
        let cap = usize::try_from(ctx.budget.remaining().min(64 << 20)).unwrap_or(64 << 20);
        let decoded = match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(raw, cap) {
            Ok(d) => d,
            Err(e) if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
                return Err(Abort::new(
                    ThreatKind::CompressionBomb,
                    loc,
                    "a PDF stream expands beyond the decompression budget",
                ));
            }
            // Corrupt or truncated streams are common in the wild; the
            // bytes that did decode are still inspected.
            Err(e) => e.output,
        };
        if let Err(e) = ctx.budget.charge(decoded.len() as u64, raw.len() as u64) {
            return Err(Abort::new(ThreatKind::CompressionBomb, loc, e.to_string()));
        }
        ctx.report.stats.bytes_inflated =
            ctx.report.stats.bytes_inflated.saturating_add(decoded.len() as u64);
        count_names(&decoded, &mut counts);
        scripts.extend(values_after(&decoded, b"/JS", 16, 1 << 20));
        uris.extend(values_after(&decoded, b"/URI", 64, 4096));
    }

    if counts.javascript > 0 {
        ctx.report.add(
            ThreatKind::PdfJavaScript,
            loc,
            format!("{} JavaScript reference(s)", counts.javascript),
        );
    }
    for (n, js) in scripts.iter().enumerate().take(32) {
        let analysis = macro_analysis::analyze(js, MacroLanguage::JavaScript);
        ctx.report.macros.push(MacroModule {
            language: MacroLanguage::JavaScript,
            location: loc.to_owned(),
            name: format!("PDF script #{}", n + 1),
            source: arca_container::text::truncate_utf8(js, ctx.policy.max_macro_source_bytes)
                .to_owned(),
            truncated: js.len() > ctx.policy.max_macro_source_bytes,
            auto_exec: analysis.auto_exec,
            suspicious: analysis.suspicious,
            iocs: analysis.iocs,
        });
    }
    if counts.launch > 0 {
        let targets = values_after(data, b"/F", 4, 512);
        ctx.report.add(
            ThreatKind::PdfLaunchAction,
            loc,
            format!("/Launch action(s): {}", targets.join(", ")),
        );
    }
    if counts.open_action + counts.aa > 0 {
        // An automatic action only matters if there is something to run.
        let sev = if counts.javascript > 0 || counts.launch > 0 {
            Severity::High
        } else if counts.submit_form
            + counts.import_data
            + counts.goto_r
            + counts.goto_e
            + counts.uri
            > 0
        {
            Severity::Medium
        } else {
            Severity::Low
        };
        ctx.report.add_with(
            ThreatKind::PdfAutoAction,
            sev,
            loc,
            format!(
                "{} /OpenAction, {} /AA (actions run on open / page view)",
                counts.open_action, counts.aa
            ),
        );
    }
    if counts.embedded_file > 0 {
        let names = values_after(data, b"/UF", 16, 260);
        ctx.report.add(
            ThreatKind::PdfEmbeddedFile,
            loc,
            format!("embedded file(s) {}", names.join(", ")),
        );
        for n in names {
            if let Some((_, ext)) = n.rsplit_once('.')
                && crate::ole::is_dangerous_extension(ext)
            {
                ctx.report.add(ThreatKind::EmbeddedExecutable, loc, format!("attached file {n:?}"));
            }
        }
    }
    if counts.rich_media > 0 {
        ctx.report.add(
            ThreatKind::PdfRichMedia,
            loc,
            format!("{} rich media / Flash / movie object(s)", counts.rich_media),
        );
    }
    if counts.xfa > 0 {
        ctx.report.add_with(
            ThreatKind::PdfJavaScript,
            Severity::Medium,
            loc,
            "XFA form (scriptable, large attack surface)",
        );
    }
    for (n, what) in [
        (counts.submit_form, "/SubmitForm"),
        (counts.import_data, "/ImportData"),
        (counts.goto_r, "/GoToR"),
        (counts.goto_e, "/GoToE"),
    ] {
        if n > 0 {
            ctx.report.add(ThreatKind::PdfRemoteAction, loc, format!("{n} {what} action(s)"));
        }
    }
    for u in uris.iter().take(256) {
        ooxml::record_external(ctx, u, RefKind::PdfAction, false, loc);
    }
    if counts.encrypt > 0 {
        ctx.report.add(
            ThreatKind::EncryptedDocument,
            loc,
            "PDF is encrypted; encrypted streams were not inspected",
        );
    }
    if counts.jbig2 > 0 {
        ctx.report.add_with(ThreatKind::UnsupportedFormat, Severity::Low, loc, "JBIG2 images (a historically exploited decoder) are rendered only by the sandboxed decoder");
    }
    if counts.obfuscated > 0 {
        ctx.report.add(
            ThreatKind::ObfuscatedName,
            loc,
            format!(
                "{} security-relevant name(s) hex-obfuscated (e.g. /J#61vaScript)",
                counts.obfuscated
            ),
        );
    }
    let eofs = data.windows(5).filter(|w| *w == b"%%EOF").count();
    if eofs > 1 {
        ctx.report.add(
            ThreatKind::TrackedChanges,
            loc,
            format!(
                "{} incremental updates: earlier revisions of the document are recoverable",
                eofs - 1
            ),
        );
    }
    for (key, mk) in [
        (&b"/Author"[..], MetaKey::Author),
        (b"/Creator", MetaKey::Application),
        (b"/Producer", MetaKey::AppVersion),
        (b"/Title", MetaKey::Title),
        (b"/Subject", MetaKey::Subject),
        (b"/Keywords", MetaKey::Keywords),
        (b"/CreationDate", MetaKey::Created),
        (b"/ModDate", MetaKey::Modified),
    ] {
        for v in values_after(data, key, 4, 1024) {
            ctx.report.meta(mk, "", &v, loc);
        }
    }
    if ctx.report.metadata.iter().any(|m| m.key == MetaKey::Author) {
        ctx.report.add(
            ThreatKind::AuthorMetadata,
            loc,
            "author name stored in the document information dictionary",
        );
    }
    Ok(FileFormat::Pdf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_normalisation() {
        let mut c = Counts::default();
        count_names(b"<< /J#61vaScript (app.alert(1)) /OpenAction 3 0 R /AA << >> >>", &mut c);
        assert_eq!(c.javascript, 1);
        assert_eq!(c.obfuscated, 1);
        assert_eq!(c.open_action, 1);
        assert_eq!(c.aa, 1);
        let mut c = Counts::default();
        count_names(b"/JSX /AAA /Launcher", &mut c);
        assert_eq!((c.javascript, c.aa, c.launch), (0, 0, 0), "prefixes must not match");
    }

    #[test]
    fn strings() {
        assert_eq!(
            literal_string(b"(a\\(b\\)c (nested) \\101)", 0, 100).as_deref(),
            Some("a(b)c (nested) A")
        );
        assert_eq!(hex_string(b"<48656c6c6f>", 0, 100).as_deref(), Some("Hello"));
        assert_eq!(decode_pdf_text(&[0xFE, 0xFF, 0x04, 0x1F]), "П");
        assert_eq!(
            values_after(b"/Author (Ivan) /AuthorX (no) /Author<4142>", b"/Author", 10, 100),
            vec!["Ivan".to_owned(), "AB".to_owned()]
        );
    }
}
