//! Plain-text family: TXT, CSV/TSV, Markdown, flat XML and HTML.

use arca_container::xml::{self, XmlLimits};

use crate::format::{FileFormat, classify_text};
use crate::report::{RefKind, Severity, ThreatKind};
use crate::xmlwalk::{self, NoopVisitor};
use crate::{Abort, Ctx, formulas, has_bidi, ooxml};

/// Scans text content.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) -> Result<FileFormat, Abort> {
    let limits = XmlLimits { max_input_bytes: ctx.policy.max_input_bytes, ..ctx.policy.xml };
    let text = match xml::decode_xml_bytes(data, &limits) {
        Ok(t) => t,
        Err(_) => {
            // Not strictly UTF-8: degrade gracefully for detection purposes.
            std::borrow::Cow::Owned(String::from_utf8_lossy(data).into_owned())
        }
    };
    let format = classify_text(&text);
    ctx.identified(format);
    if has_bidi(&text) {
        ctx.report.add(ThreatKind::BidiOverride, loc, "text contains bidirectional override characters (content may display differently than it reads)");
    }
    match format {
        FileFormat::Csv | FileFormat::Tsv => csv(ctx, &text, format == FileFormat::Tsv, loc),
        FileFormat::Markdown => markdown(ctx, &text, loc),
        FileFormat::Html => html(ctx, &text, loc),
        FileFormat::FlatXml => {
            xmlwalk::walk(ctx, text.as_bytes(), loc, &mut NoopVisitor)?;
            let lower = text.to_ascii_lowercase();
            if lower.contains("<w:bindata")
                || lower.contains("editdata.mso")
                || lower.contains("vbaproject")
            {
                ctx.report.add(
                    ThreatKind::VbaMacros,
                    loc,
                    "flat XML document embeds binary data / a macro project",
                );
            }
        }
        _ => {}
    }
    Ok(format)
}

fn csv(ctx: &mut Ctx<'_>, text: &str, tabs: bool, loc: &str) {
    let mut dangerous = 0usize;
    let mut plain = 0usize;
    let mut sample = String::new();
    for (line_no, line) in text.lines().enumerate().take(5_000_000) {
        let cells: Box<dyn Iterator<Item = &str>> =
            if tabs { Box::new(line.split('\t')) } else { Box::new(line.split([',', ';'])) };
        for cell in cells {
            let c = cell.trim().trim_matches('"').trim_start();
            let Some(first) = c.chars().next() else { continue };
            if !matches!(first, '=' | '+' | '-' | '@' | '\t' | '\r') {
                continue;
            }
            let body = c.trim_start_matches(['=', '+', '-', '@']);
            if body
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit() || ch == '.' || ch.is_whitespace())
                && !body.contains('|')
            {
                // Negative numbers and similar.
                continue;
            }
            formulas::record_urls(ctx, body, loc);
            if formulas::assess(body).is_empty() {
                plain += 1;
            } else {
                dangerous += 1;
                if sample.is_empty() {
                    sample = format!(
                        "line {}: {}",
                        line_no + 1,
                        arca_container::text::display_safe(c, 120)
                    );
                }
            }
        }
    }
    if dangerous > 0 {
        ctx.report.add(
            ThreatKind::CsvFormulaInjection,
            loc,
            format!("{dangerous} cell(s) with DDE / command formulas, e.g. {sample}"),
        );
    }
    if plain > 0 {
        ctx.report.add_with(
            ThreatKind::CsvFormulaInjection,
            Severity::Low,
            loc,
            format!(
                "{plain} cell(s) start with a formula character (shown as text, never evaluated)"
            ),
        );
    }
}

fn markdown(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    let mut images = 0usize;
    let mut i = 0usize;
    while let Some(rel) = text.get(i..).and_then(|t| t.find("![")) {
        let at = i + rel;
        i = at + 2;
        let Some(open) = text.get(at..).and_then(|t| t.find("](")).map(|p| at + p + 2) else {
            continue;
        };
        let Some(close) = text.get(open..).and_then(|t| t.find(')')).map(|p| open + p) else {
            continue;
        };
        let target = text.get(open..close).unwrap_or("").split_whitespace().next().unwrap_or("");
        if crate::url::scheme(target).is_some() && images < 256 {
            images += 1;
            ooxml::record_external(ctx, target, RefKind::Image, true, loc);
        }
        i = close.max(i);
    }
    html_tags(ctx, text, loc);
}

fn html(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    html_tags(ctx, text, loc);
}

fn html_tags(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    let lower = text.to_ascii_lowercase();
    for (needle, what) in [
        ("<script", "<script> element"),
        ("<iframe", "<iframe> element"),
        ("<object", "<object> element"),
        ("<embed", "<embed> element"),
        ("<hta:application", "HTA application declaration"),
        ("javascript:", "javascript: URL"),
        ("vbscript:", "vbscript: URL"),
        (" onload=", "onload handler"),
        (" onerror=", "onerror handler"),
        ("<meta http-equiv=\"refresh\"", "meta refresh redirect"),
    ] {
        if lower.contains(needle) {
            ctx.report.add(ThreatKind::HtmlActiveContent, loc, what);
        }
    }
    let mut from = 0usize;
    let mut count = 0usize;
    while let Some(rel) = lower.get(from..).and_then(|t| t.find("<img")) {
        let at = from + rel;
        from = at + 4;
        let tag_end = lower.get(at..).and_then(|t| t.find('>')).map_or(lower.len(), |p| at + p);
        let tag = text.get(at..tag_end).unwrap_or("");
        if let Some(src) = xmlwalk::pseudo_attr(tag, "src")
            && crate::url::leaves_package(&src)
            && count < 256
        {
            count += 1;
            ooxml::record_external(ctx, &src, RefKind::Image, true, loc);
        }
    }
}
