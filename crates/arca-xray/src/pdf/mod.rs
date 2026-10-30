//! PDF triage (lexical, in the spirit of `pdfid`, plus Flate stream inspection).
//!
//! A full PDF object parser belongs to the Phase 3 renderer. For the X-ray,
//! every name token is normalised (`/J#61vaScript` → `/JavaScript`) and
//! counted in the raw file *and* inside every Flate-compressed stream
//! (object streams routinely hide `/JavaScript` and `/OpenAction`).
//! Decompression is charged against the document budget.

mod filters;
mod syntax;

use crate::format::FileFormat;
use crate::macro_analysis;
use crate::report::{
    MacroIntent, MacroLanguage, MacroModule, MetaKey, RefKind, Severity, ThreatKind,
};
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

/// PDF white space: NUL, tab, line feed, form feed, carriage return and space. (Rust's own
/// definition leaves out NUL, which is how `/JavaScript\0/S` slipped past a lexical scan.)
pub(super) fn is_ws(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

pub(super) fn is_delim(b: u8) -> bool {
    is_ws(b) || matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

pub(super) fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Returns `(normalised name, was_obfuscated, end index)` for a name at `i` (pointing at '/').
pub(super) fn read_name(data: &[u8], i: usize) -> (String, bool, usize) {
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
                while data.get(j).is_some_and(|&b| is_ws(b)) {
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
        while data.get(j).is_some_and(|&b| is_ws(b)) {
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

/// Decodes the streams of the file and scans what comes out.
///
/// Stream boundaries follow the dictionary: `/Length` (direct, or an indirect integer object)
/// says where the data ends and the keyword `endstream` must follow. A reader that trusts the
/// length and a reader that looks for the keyword see different streams when they disagree, so
/// the disagreement is reported and both extents are scanned.
fn streams(
    ctx: &mut Ctx<'_>,
    data: &[u8],
    loc: &str,
    counts: &mut Counts,
    scripts: &mut Vec<String>,
    uris: &mut Vec<String>,
    unreadable: &mut Vec<String>,
) -> Result<(), Abort> {
    // Raw bytes handed to decoders, over all streams and both extents: bounded by the file.
    let feed_limit = data.len().saturating_mul(4).saturating_add(1 << 20);
    let mut fed = 0usize;
    let lengths = syntax::integer_objects(data);
    let mut count = 0usize;
    let mut differential = 0usize;
    let mut from = 0usize;
    let mut previous_end = 0usize;
    'streams: while let Some(rel) = data.get(from..).and_then(|d| memfind(d, b"stream")) {
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
        // The dictionary is whatever follows the last `obj` keyword in front of the keyword,
        // within a bounded window that never reaches back into the previous stream.
        let lo = previous_end.max(at.saturating_sub(16 << 10));
        let dict = data.get(lo..at).and_then(syntax::dict_before_stream);
        // The extent a scanner for the keyword sees, and the one the length claims.
        let Some(end_rel) = data.get(start..).and_then(|d| memfind(d, b"endstream")) else {
            ctx.report.add(
                ThreatKind::MalformedContainer,
                loc,
                "a PDF stream has no endstream keyword; the rest of the file was not scanned for streams",
            );
            break;
        };
        let keyword_end = start + end_rel;
        previous_end = keyword_end + 9;
        from = previous_end;
        let claimed = dict.as_ref().and_then(|d| match d.get("Length") {
            Some(syntax::Obj::Int(n)) => usize::try_from(*n).ok(),
            Some(syntax::Obj::Ref(num)) => {
                lengths.get(num).copied().flatten().and_then(|n| usize::try_from(n).ok())
            }
            _ => None,
        });
        let length_end = claimed.and_then(|n| start.checked_add(n)).filter(|&e| {
            // A usable length is followed (after optional white space) by `endstream`.
            let mut j = e;
            while data.get(j).is_some_and(|&b| is_ws(b)) {
                j += 1;
            }
            e <= data.len() && data.get(j..j + 9) == Some(b"endstream")
        });
        let mut extents = vec![keyword_end];
        if let Some(e) = length_end {
            // Equal up to the end-of-line before the keyword: no disagreement.
            let same = e <= keyword_end
                && data.get(e..keyword_end).is_some_and(|g| g.iter().all(|&b| is_ws(b)));
            if !same {
                differential += 1;
                extents.push(e);
            }
        }
        let Some(dict) = dict else {
            unreadable.push("a dictionary that could not be parsed".to_owned());
            continue;
        };
        if filters::filter_names(&dict).is_ok_and(|names| names.is_empty()) {
            continue;
        }
        for end in extents {
            count += 1;
            if count > ctx.policy.max_pdf_streams {
                ctx.report.add(
                    ThreatKind::ScanLimitReached,
                    loc,
                    "PDF stream limit reached; remaining streams not inspected",
                );
                break 'streams;
            }
            let raw = data.get(start..end).unwrap_or(&[]);
            fed = fed.saturating_add(raw.len());
            if fed > feed_limit {
                ctx.report.add(
                    ThreatKind::ScanLimitReached,
                    loc,
                    "the PDF streams overlap so much that decoding them all would cost more than the file is worth; the rest was not inspected",
                );
                break 'streams;
            }
            // Each stream may expand at most to what the budget still allows, and never past
            // `STREAM_CAP`.
            let cap = usize::try_from(ctx.budget.remaining().min(STREAM_CAP as u64))
                .unwrap_or(STREAM_CAP);
            let mut produced = 0u64;
            let result = filters::decode_chain(raw, &dict, cap, |out, input| {
                produced = produced.saturating_add(out as u64);
                ctx.budget.charge(out as u64, input as u64).map_err(|_| ())
            });
            ctx.report.stats.bytes_inflated =
                ctx.report.stats.bytes_inflated.saturating_add(produced);
            match result {
                Ok(decoded) => {
                    count_names(&decoded, counts);
                    scripts.extend(values_after(&decoded, b"/JS", 16, 1 << 20));
                    uris.extend(values_after(&decoded, b"/URI", 64, 4096));
                }
                Err(filters::Failure::TooLarge) => {
                    return Err(Abort::new(
                        ThreatKind::CompressionBomb,
                        loc,
                        "a PDF stream expands beyond the decompression budget",
                    ));
                }
                Err(filters::Failure::Unsupported(what)) => {
                    if !unreadable.contains(&what) && unreadable.len() < 16 {
                        unreadable.push(what);
                    }
                }
            }
        }
    }
    if differential > 0 {
        ctx.report.add(
            ThreatKind::ParserDifferential,
            loc,
            format!(
                "{differential} PDF stream(s) have a /Length that disagrees with the position of endstream; readers that trust the length and readers that look for the keyword see different data (both were scanned)"
            ),
        );
    }
    Ok(())
}

/// The most one decoded PDF stream may hold when it is scanned.
///
/// The decoder doubles its buffer as it grows, and in the linear memory of the sandbox the old
/// buffer and the new one exist together for a moment: a stream allowed 64 MiB needs 96 MiB at the
/// step from 32 to 64, which is the whole memory limit of a scan, so a decompression bomb ended as
/// "the guest ran out of memory" instead of "this is a bomb". At 32 MiB the peak is 48 MiB. A stream
/// that decodes to more is treated as a bomb (a scan of ordinary text, scripts and actions never
/// needs that much; a lossless picture of that size is not read for its text anyway).
const STREAM_CAP: usize = 32 << 20;

/// Scans a PDF.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) -> Result<FileFormat, Abort> {
    ctx.identified(FileFormat::Pdf);
    crate::check_polyglot(ctx, data, loc, true);
    let mut counts = Counts::default();
    count_names(data, &mut counts);
    let mut scripts: Vec<String> = values_after(data, b"/JS", 64, 1 << 20);
    let mut uris: Vec<String> = values_after(data, b"/URI", 256, 4096);

    // Streams: every filter chain is decoded (or reported as unreadable) and its text scanned.
    let mut unreadable: Vec<String> = Vec::new();
    streams(ctx, data, loc, &mut counts, &mut scripts, &mut uris, &mut unreadable)?;
    for why in &unreadable {
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            loc,
            format!("a PDF stream uses {why}; its content was not inspected"),
        );
    }

    // What the scripts that could be read do sets the severity of the finding that reports scripts;
    // a script reference whose text was not found (`scripts` empty) keeps the default.
    let limit = ctx.policy.max_macro_source_bytes;
    let analyses: Vec<macro_analysis::Analysis> = scripts
        .iter()
        .take(32)
        .map(|js| macro_analysis::analyze_module(js, MacroLanguage::JavaScript, js.len() > limit))
        .collect();
    // Every script was read and every one is ordinary form scripting (number formats, sums).
    let scripts_benign = !analyses.is_empty()
        && analyses.len() == scripts.len()
        && analyses.iter().all(|a| a.intent == MacroIntent::Benign);
    if counts.javascript > 0 {
        let severity = if analyses.is_empty() || analyses.len() < scripts.len() {
            ThreatKind::PdfJavaScript.default_severity()
        } else {
            analyses
                .iter()
                .map(|a| a.intent)
                .max()
                .map_or(ThreatKind::PdfJavaScript.default_severity(), |i| i.macro_severity())
        };
        ctx.report.add_with(
            ThreatKind::PdfJavaScript,
            severity,
            loc,
            format!("{} JavaScript reference(s)", counts.javascript),
        );
    }
    for (n, (js, analysis)) in scripts.iter().zip(analyses).enumerate() {
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
            intent: analysis.intent,
            behaviors: analysis.behaviors,
            procedures: analysis.procedures,
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
        // An automatic action only matters if there is something to run. Form scripting that only
        // formats and adds numbers is "run" too, but not worth a High: Medium is the floor that keeps
        // the file from being called clean.
        let sev = if counts.launch > 0 || (counts.javascript > 0 && !scripts_benign) {
            Severity::High
        } else if counts.javascript
            + counts.submit_form
            + counts.import_data
            + counts.goto_r
            + counts.goto_e
            + counts.uri
            > 0
        {
            // (Script that is only form scripting, or an action that reaches outside.)
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
