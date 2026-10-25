//! PDF preview: the visible text of every page, hidden text labelled.
//!
//! A PDF is read structurally — cross-reference data, page tree, content streams, fonts'
//! text mappings — and **never rendered**: no font program, image, colour profile, script
//! or embedded file is decoded or run. The result is a best-effort *reading view*: lines of
//! text are reassembled into paragraphs by position, which is exact for ordinary documents
//! and approximate for complex layouts (multi-column pages, forms, tables), and the
//! preview says so.

mod agl;
mod clean;
mod cmap;
mod content;
mod crypt;
mod decrypt;
mod draw;
mod encoding;
mod file;
mod filter;
mod font;
mod lex;
#[cfg(test)]
pub(crate) mod testpdf;

use std::collections::HashSet;
use std::rc::Rc;

use arca_container::text::{CodePage, decode_codepage, utf16le_lossy};

pub(crate) use self::clean::{CleanOptions, clean};
use self::content::{Item, Line, PageInput, Seg, Shared, run_page};
pub(crate) use self::decrypt::decrypt;
use self::file::PdfFile;
use self::lex::{Dict, Obj};
use crate::PreviewError;
use crate::bidi::{self, Base};
use crate::model::{NoticeKind, ParaStyle, Paragraph, Run, SectionKind, style};
use crate::sink::{ParaBuilder, SectionId, Sink};

/// Content-stream operators executed in one document.
const MAX_OPERATORS: u64 = 40_000_000;
/// Page-tree nodes visited.
const MAX_TREE_NODES: usize = 400_000;
/// Annotations read per page.
const MAX_ANNOTS: usize = 500;

mod picture;
pub(crate) use picture::{Found, picture};

#[cfg(test)]
mod picture_tests;

/// Extracts the text of `data`.
pub(crate) fn extract(data: &[u8], sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    let policy = sink.policy;
    let mut pdf = PdfFile::open(data, policy)?;
    sink.note(NoticeKind::BestEffort, "pdf");
    let trailer = pdf.trailer.clone();
    let root = pdf.get(&trailer, "Root");
    let catalog = root.as_dict().cloned().unwrap_or_default();
    let mut pages = collect_pages(&mut pdf, &catalog, policy.max_sections.saturating_add(1));
    if pages.is_empty() && !pdf.rebuilt() {
        pdf.rebuild();
        let trailer = pdf.trailer.clone();
        let root = pdf.get(&trailer, "Root");
        let catalog = root.as_dict().cloned().unwrap_or_default();
        pages = collect_pages(&mut pdf, &catalog, policy.max_sections.saturating_add(1));
    }
    if pages.is_empty() {
        pages = scan_for_pages(&mut pdf, policy.max_sections.saturating_add(1));
        if !pages.is_empty() {
            sink.note_detail(NoticeKind::BestEffort, "pdf page tree rebuilt".into());
        }
    }
    if pages.is_empty() {
        return Err(PreviewError::Malformed("PDF has no pages".into()));
    }
    if pdf.rebuilt() {
        sink.note_detail(NoticeKind::BestEffort, "pdf structure rebuilt".into());
    }
    if pages.len() > policy.max_sections {
        pages.truncate(policy.max_sections);
        sink.truncated("pages");
    }
    let ocg_off = ocg_off_set(&mut pdf, &catalog);
    let mut shared = Shared::new(ocg_off, MAX_OPERATORS);
    let (mut unmapped, mut damaged, mut cut, mut mismatched) = (0usize, false, false, 0usize);
    for page in &pages {
        if sink.is_full() {
            break;
        }
        let input = page_input(&mut pdf, page);
        let out = run_page(&mut pdf, &mut shared, &input);
        unmapped = unmapped.saturating_add(out.unmapped);
        mismatched = mismatched.saturating_add(out.mismatched);
        damaged |= out.damaged;
        cut |= out.truncated;
        let Some(sec) = sink.open_section(SectionKind::Page, "", false) else { break };
        assemble(sink, sec, out.items, input.visible[2] - input.visible[0]);
        annotations(&mut pdf, sink, sec, page);
        if cut && shared.ops_left == 0 {
            break;
        }
    }
    if unmapped > 0 {
        sink.note(NoticeKind::Unsupported, "pdf fonts without a text mapping");
    }
    if mismatched > 0 {
        // The text is what the font's `ToUnicode` map says; the glyph the reader sees on the
        // page is chosen by the font's encoding, and for these codes the two disagree.
        sink.note(NoticeKind::BestEffort, "pdf text mapping differs from the font encoding");
    }
    if damaged {
        sink.skipped("damaged PDF stream");
    }
    if cut {
        sink.truncated("pdf content limits");
    }
    for s in std::mem::take(&mut pdf.skipped) {
        sink.skipped(&s);
    }
    Ok(())
}

/// Draws page `index` (from 0) of `data`: the page as a list of things to draw.
pub(crate) fn render(
    data: &[u8],
    index: usize,
    policy: &crate::PreviewPolicy,
) -> Result<crate::page::PageModel, PreviewError> {
    let mut pdf = PdfFile::open(data, policy)?;
    let limit = policy.max_sections.saturating_add(1);
    let pages = {
        let trailer = pdf.trailer.clone();
        let root = pdf.get(&trailer, "Root");
        let catalog = root.as_dict().cloned().unwrap_or_default();
        let mut pages = collect_pages(&mut pdf, &catalog, limit);
        if pages.is_empty() && !pdf.rebuilt() {
            pdf.rebuild();
            let trailer = pdf.trailer.clone();
            let root = pdf.get(&trailer, "Root");
            let catalog = root.as_dict().cloned().unwrap_or_default();
            pages = collect_pages(&mut pdf, &catalog, limit);
        }
        if pages.is_empty() {
            pages = scan_for_pages(&mut pdf, limit);
        }
        pages
    };
    let trailer = pdf.trailer.clone();
    let root = pdf.get(&trailer, "Root");
    let catalog = root.as_dict().cloned().unwrap_or_default();
    let Some(page) = pages.get(index) else {
        return Err(PreviewError::Malformed("the document has no such page".into()));
    };
    let mut input = page_input(&mut pdf, page);
    input.record = true;
    let rotate = match inherited(&mut pdf, page, "Rotate") {
        Obj::Int(n) => n,
        _ => 0,
    };
    let mut shared = Shared::new(ocg_off_set(&mut pdf, &catalog), MAX_OPERATORS);
    let out = run_page(&mut pdf, &mut shared, &input);
    let (view, width, height) = draw::view_of(input.visible, rotate);
    let mut model = out
        .drawn
        .ok_or_else(|| PreviewError::Malformed("the page could not be drawn".into()))?
        .finish(
            view,
            width,
            height,
            u32::try_from(index).unwrap_or(u32::MAX),
            u32::try_from(pages.len()).unwrap_or(u32::MAX),
        );
    model.truncated |= out.truncated;
    model.approximate |= out.damaged || out.unmapped > 0 || out.mismatched > 0;
    // The page model bypasses the sink, so the text it carries is cleaned here: the reader draws
    // it with a system font, and a bidi override or a tag character in it would work there as in
    // any other text. What is removed makes the page approximate.
    model.approximate |= clean_page_text(&mut model.prims);
    Ok(model)
}

/// Removes control, bidirectional and invisible characters from the text and picture labels of a
/// page. Returns `true` if anything was removed or replaced.
fn clean_page_text(prims: &mut [crate::page::Prim]) -> bool {
    let mut counts = crate::sink::CleanCounts::default();
    let mut changed = false;
    let mut tidy = |s: &mut String| {
        let cleaned = crate::sink::clean_text(s, &mut counts);
        let flat = cleaned.replace(['\n', '\t'], " ");
        if flat != *s {
            changed = true;
            *s = flat;
        }
    };
    for p in prims {
        match p {
            crate::page::Prim::Text { text, .. } => tidy(text),
            crate::page::Prim::Image { label, .. } => tidy(label),
            _ => {}
        }
    }
    changed
}

// ---- page tree ---------------------------------------------------------------------------

fn collect_pages(pdf: &mut PdfFile<'_>, catalog: &Dict, limit: usize) -> Vec<Rc<Dict>> {
    let mut pages = Vec::new();
    let mut visited: HashSet<u32> = HashSet::new();
    let mut stack: Vec<Obj> = Vec::new();
    if let Some(n) = catalog.get("Pages") {
        stack.push(n.clone());
    }
    let mut nodes = 0usize;
    while let Some(node) = stack.pop() {
        nodes += 1;
        if nodes > MAX_TREE_NODES || pages.len() >= limit {
            break;
        }
        if let Obj::Ref(n, _) = &node
            && !visited.insert(*n)
        {
            continue;
        }
        let resolved = pdf.resolve(&node);
        let Obj::Dict(dict) = resolved else { continue };
        let kids = pdf.get(&dict, "Kids");
        let ty = pdf.get(&dict, "Type");
        match (kids.as_arr(), ty.as_name()) {
            (Some(kids), t) if t != Some("Page") => {
                for k in kids.iter().rev() {
                    stack.push(k.clone());
                }
            }
            _ => pages.push(dict),
        }
    }
    pages
}

/// Fallback when the page tree is unusable: every object that says it is a page.
fn scan_for_pages(pdf: &mut PdfFile<'_>, limit: usize) -> Vec<Rc<Dict>> {
    let mut pages = Vec::new();
    for (i, n) in pdf.object_numbers().into_iter().enumerate() {
        if i > MAX_TREE_NODES || pages.len() >= limit {
            break;
        }
        if let Obj::Dict(d) = pdf.object(n)
            && pdf.get(&d, "Type").as_name() == Some("Page")
        {
            pages.push(d);
        }
    }
    pages
}

/// An inheritable page attribute (looked up through `/Parent`).
fn inherited(pdf: &mut PdfFile<'_>, page: &Dict, key: &str) -> Obj {
    let mut cur = Rc::new(page.clone());
    for _ in 0..32 {
        let v = pdf.get(&cur, key);
        if !v.is_null() {
            return v;
        }
        match pdf.get(&cur, "Parent") {
            Obj::Dict(p) => cur = p,
            _ => break,
        }
    }
    Obj::Null
}

fn rect(pdf: &mut PdfFile<'_>, page: &Dict, key: &str) -> Option<[f64; 4]> {
    let v = inherited(pdf, page, key);
    let nums: Vec<f64> =
        v.as_arr()?.iter().map(|o| pdf.resolve(o)).filter_map(|o| o.as_f64()).collect();
    match nums.as_slice() {
        [a, b, c, d] if (c - a).abs() > 1.0 && (d - b).abs() > 1.0 => {
            Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)])
        }
        _ => None,
    }
}

fn page_input(pdf: &mut PdfFile<'_>, page: &Dict) -> PageInput {
    let media = rect(pdf, page, "MediaBox").unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let visible = match rect(pdf, page, "CropBox") {
        Some(c) => {
            let v =
                [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])];
            if v[2] - v[0] > 1.0 && v[3] - v[1] > 1.0 { v } else { media }
        }
        None => media,
    };
    let resources = match inherited(pdf, page, "Resources") {
        Obj::Dict(d) => d,
        _ => Rc::new(Dict::new()),
    };
    let contents = match pdf.get(page, "Contents") {
        Obj::Stream(s) => vec![s],
        Obj::Arr(items) => items
            .iter()
            .filter_map(|o| match pdf.resolve(o) {
                Obj::Stream(s) => Some(s),
                _ => None,
            })
            .take(10_000)
            .collect(),
        _ => Vec::new(),
    };
    PageInput { resources, contents, visible, record: false }
}

/// Optional-content groups that are switched off in the default configuration.
fn ocg_off_set(pdf: &mut PdfFile<'_>, catalog: &Dict) -> HashSet<u32> {
    let mut off = HashSet::new();
    let props = pdf.get(catalog, "OCProperties");
    let Some(props) = props.as_dict().cloned() else { return off };
    let default = pdf.get(&props, "D");
    let Some(d) = default.as_dict().cloned() else { return off };
    let refs = |o: &Obj| -> Vec<u32> {
        o.as_arr().map(|a| a.iter().filter_map(Obj::ref_num).collect()).unwrap_or_default()
    };
    let base_off = pdf.get(&d, "BaseState").as_name() == Some("OFF");
    if base_off {
        let all = props.get("OCGs").map(|o| pdf.resolve(o)).unwrap_or_default();
        off.extend(refs(&all));
        let on = d.get("ON").map(|o| pdf.resolve(o)).unwrap_or_default();
        for n in refs(&on) {
            off.remove(&n);
        }
    } else {
        let list = d.get("OFF").map(|o| pdf.resolve(o)).unwrap_or_default();
        off.extend(refs(&list));
    }
    off
}

// ---- layout ----------------------------------------------------------------------------------

/// `true` for text that begins a list item or a numbered paragraph.
fn starts_item(text: &str) -> bool {
    let t = text.trim_start();
    let mut chars = t.chars();
    match chars.next() {
        Some('•' | '◦' | '▪' | '●' | '■' | '–' | '—' | '-' | '*' | '·' | '○' | '►' | '✓') => {
            chars.next().is_some_and(char::is_whitespace)
        }
        Some(c) if c.is_ascii_digit() => {
            let rest: String = chars.skip_while(char::is_ascii_digit).take(2).collect();
            let mut r = rest.chars();
            matches!(r.next(), Some('.' | ')')) && r.next().is_some_and(char::is_whitespace)
        }
        _ => false,
    }
}

/// The most common font size, weighted by amount of text.
fn body_size(lines: &[&Line]) -> f64 {
    let mut sizes: Vec<(f64, usize)> = lines
        .iter()
        .map(|l| (l.size, l.segs.iter().map(|s| s.text.chars().count()).sum::<usize>()))
        .collect();
    sizes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: usize = sizes.iter().map(|s| s.1).sum();
    let mut acc = 0usize;
    for (size, n) in &sizes {
        acc += n;
        if acc * 2 >= total {
            return *size;
        }
    }
    sizes.last().map_or(12.0, |s| s.0)
}

fn heading_of(line: &Line, body: f64) -> ParaStyle {
    let chars: usize = line.segs.iter().map(|s| s.text.chars().count()).sum();
    if chars == 0 || chars > 160 || body <= 0.0 {
        return ParaStyle::Body;
    }
    let ratio = line.size / body;
    let bold = line.segs.iter().all(|s| s.style & style::BOLD != 0);
    if ratio >= 1.6 {
        ParaStyle::Heading1
    } else if ratio >= 1.3 {
        ParaStyle::Heading2
    } else if ratio >= 1.12 && bold {
        ParaStyle::Heading3
    } else {
        ParaStyle::Body
    }
}

fn push_para(sink: &mut Sink<'_>, sec: SectionId, pb: &mut ParaBuilder, st: ParaStyle) -> bool {
    if pb.is_empty() {
        return true;
    }
    let p = pb.take(st);
    sink.para(sec, p)
}

/// Puts the text of a line that holds Hebrew or Arabic into reading order.
///
/// A content stream lists glyphs left to right as drawn, which for those scripts is backwards.
/// Parts separated by a tab (table cells, columns) keep their places on the page and are
/// reordered one by one.
fn to_reading_order(line: &mut Line, base: Base) {
    if !line.segs.iter().any(|s| bidi::has_rtl(&s.text)) {
        return;
    }
    // Characters with their styles, split at the tabs that the layout step inserted.
    let mut parts: Vec<Vec<(char, u16)>> = vec![Vec::new()];
    for seg in &line.segs {
        for c in seg.text.chars() {
            if c == '\t' {
                parts.push(Vec::new());
            } else if let Some(last) = parts.last_mut() {
                last.push((c, seg.style));
            }
        }
    }
    let mut segs: Vec<Seg> = Vec::new();
    let mut push = |c: char, st: u16| match segs.last_mut() {
        Some(last) if last.style == st => last.text.push(c),
        _ => segs.push(Seg { text: c.to_string(), style: st }),
    };
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            push('\t', 0);
        }
        let visual: Vec<char> = part.iter().map(|(c, _)| *c).collect();
        let logical = bidi::to_logical(&visual, base);
        // The reordered text has the same length; each character takes the style of the one
        // at its place before, which keeps whole words in one style (the usual case).
        for (k, c) in logical.into_iter().enumerate() {
            push(c, part.get(k).map_or(0, |(_, st)| *st));
        }
    }
    line.segs = segs;
}

/// Reads right-to-left lines in the right order. Which way a line runs is judged from the margin
/// it is flush with (text in a right-to-left paragraph is flush right); a line that fills the
/// column, or sits in the middle, is judged by its letters.
fn fix_reading_order(items: &mut [Item]) {
    let (mut left, mut right) = (f64::INFINITY, f64::NEG_INFINITY);
    for item in items.iter() {
        if let Item::Line(l) = item {
            left = left.min(l.a0.min(l.a1));
            right = right.max(l.a0.max(l.a1));
        }
    }
    let tolerance = 0.03 * (right - left).max(1.0);
    for item in items.iter_mut() {
        if let Item::Line(l) = item {
            let flush_left = (l.a0.min(l.a1) - left).abs() <= tolerance;
            let flush_right = (right - l.a0.max(l.a1)).abs() <= tolerance;
            let base = match (flush_left, flush_right) {
                (true, false) => Base::Ltr,
                (false, true) => Base::Rtl,
                _ => Base::Auto,
            };
            to_reading_order(l, base);
        }
    }
}

/// Turns the lines and pictures of a page into paragraphs.
fn assemble(sink: &mut Sink<'_>, sec: SectionId, items: Vec<Item>, page_width: f64) {
    let mut items = items;
    fix_reading_order(&mut items);
    let lines: Vec<&Line> =
        items.iter().filter_map(|i| if let Item::Line(l) = i { Some(l) } else { None }).collect();
    // A line "reaches the margin" if it is nearly as long as the longest line, which itself
    // must be a plausible text column (short lines never count as wrapped).
    let max_width = lines.iter().map(|l| l.a1 - l.a0).fold(0.0, f64::max).max(0.35 * page_width);
    let body = body_size(&lines);
    let mut pb = ParaBuilder::new();
    let mut para_style = ParaStyle::Body;
    let mut prev: Option<&Line> = None;
    for item in &items {
        match item {
            Item::Image(info) => {
                if !push_para(sink, sec, &mut pb, para_style) || !sink.image(sec, info.clone()) {
                    return;
                }
                prev = None;
            }
            Item::Line(line) => {
                let text = line.text();
                let heading = heading_of(line, body);
                let mut separator = "";
                let mut new_para = true;
                if let Some(p) = prev {
                    let drop = p.across - line.across;
                    let size = p.size.max(line.size);
                    let broke = drop > 1.75 * size || drop < -0.3 * size;
                    let same_kind = heading_of(p, body) == heading;
                    new_para = broke || !same_kind || starts_item(&text);
                    if !new_para {
                        let ptext = p.text();
                        let wrapped = (p.a1 - p.a0) >= 0.7 * max_width
                            && (p.size - line.size).abs() <= 0.12 * p.size
                            && !ptext.contains('\t')
                            && !text.contains('\t')
                            && heading == ParaStyle::Body;
                        separator = if wrapped { " " } else { "\n" };
                    }
                }
                if new_para {
                    if !push_para(sink, sec, &mut pb, para_style) {
                        return;
                    }
                    para_style = heading;
                } else {
                    pb.text(separator, 0, "");
                }
                for seg in &line.segs {
                    pb.text(&seg.text, seg.style, "");
                }
                prev = Some(line);
            }
        }
    }
    push_para(sink, sec, &mut pb, para_style);
}

// ---- annotations ---------------------------------------------------------------------------------

/// Decodes a PDF text string (UTF-16 with BOM, UTF-8 with BOM, else PDFDocEncoding ≈ cp1252).
fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let swapped: Vec<u8> =
            rest.chunks(2).flat_map(|c| c.iter().rev().copied().collect::<Vec<_>>()).collect();
        return utf16le_lossy(&swapped);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16le_lossy(rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    decode_codepage(bytes, CodePage::Windows1252)
}

fn annotations(pdf: &mut PdfFile<'_>, sink: &mut Sink<'_>, sec: SectionId, page: &Dict) {
    let annots = pdf.get_array(page, "Annots");
    for a in annots.iter().take(MAX_ANNOTS) {
        let Some(d) = a.as_dict().cloned() else { continue };
        let subtype = pdf.get(&d, "Subtype").as_name().unwrap_or("").to_owned();
        match subtype.as_str() {
            "Link" => {
                let action = pdf.get(&d, "A");
                let Some(ad) = action.as_dict().cloned() else { continue };
                if pdf.get(&ad, "S").as_name() != Some("URI") {
                    continue;
                }
                let uri = pdf.get(&ad, "URI");
                let Some(u) = uri.as_str().map(text_string) else { continue };
                if u.trim().is_empty() {
                    continue;
                }
                let run = Run { text: u.clone(), style: 0, href: u };
                if !sink.para(sec, Paragraph { runs: vec![run], ..Paragraph::default() }) {
                    return;
                }
            }
            "Text" | "FreeText" | "Highlight" | "Underline" | "StrikeOut" | "Squiggly"
            | "Stamp" | "Caret" => {
                let contents = pdf.get(&d, "Contents");
                let Some(t) = contents.as_str().map(text_string) else { continue };
                if t.trim().is_empty() {
                    continue;
                }
                let p = Paragraph { style: ParaStyle::Comment, ..Paragraph::plain(&t) };
                if !sink.para(sec, p) {
                    return;
                }
            }
            "Widget" => {
                let value = pdf.get(&d, "V");
                let v = match &value {
                    Obj::Str(s) => text_string(s),
                    Obj::Name(n) => n.to_string(),
                    _ => continue,
                };
                if v.trim().is_empty() || v == "Off" {
                    continue;
                }
                let name = pdf.get(&d, "T").as_str().map(text_string).unwrap_or_default();
                let line = if name.is_empty() { v } else { format!("{name}: {v}") };
                let p = Paragraph { style: ParaStyle::Comment, ..Paragraph::plain(&line) };
                if !sink.para(sec, p) {
                    return;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewPolicy;
    use crate::model::Block;
    use crate::pdf::testpdf::PdfBuilder;
    use crate::testkit::{ok, paragraphs, run, section_of, text_of};

    fn pages_text(p: &crate::model::Preview) -> Vec<String> {
        p.sections.iter().filter(|s| s.kind == SectionKind::Page).map(text_of).collect()
    }

    #[test]
    fn hello_world() {
        let p = ok(&PdfBuilder::simple_text("Hello, PDF").build());
        assert_eq!(p.format, crate::FileFormat::Pdf);
        assert_eq!(pages_text(&p), ["Hello, PDF"]);
        assert!(p.notices.iter().any(|n| n.kind == NoticeKind::BestEffort && n.detail == "pdf"));
    }

    #[test]
    fn modern_layout_and_compressed_content() {
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> >>");
        b.add_raw(3, "<< /Type /Page /Parent 2 0 R /Contents [4 0 R 6 0 R] >>");
        b.add_flate_stream(4, "", b"BT /F1 12 Tf 72 700 Td (first");
        b.add_flate_stream(6, "", b") Tj ET BT 72 680 Td (second) Tj ET");
        b.add_raw(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>");
        for data in [b.build(), b.build_modern()] {
            let p = ok(&data);
            assert_eq!(pages_text(&p), ["first\nsecond"]);
        }
    }

    #[test]
    fn paragraphs_are_reflowed_but_lists_and_tables_are_not() {
        let mut c = String::from("BT /F1 12 Tf 14 TL 72 700 Td ");
        // A wrapped paragraph: three full-width lines and a short last one.
        for (i, l) in [
            "The quick brown fox jumps over the lazy dog and keeps running",
            "through the forest until the evening comes and everyone",
            "goes home to rest after a very long day of walking",
            "The end.",
        ]
        .iter()
        .enumerate()
        {
            match i {
                0 => {}
                3 => c.push_str("0 -28 Td "),
                _ => c.push_str("T* "),
            }
            c.push_str(&format!("({l}) Tj "));
        }
        // A bullet list and a tab-separated row after a blank line.
        c.push_str("0 -30 Td (- first item) Tj T* (- second item) Tj ET");
        let p = ok(&PdfBuilder::with_content(&c).build());
        let sec = section_of(&p, SectionKind::Page).expect("page");
        let paras: Vec<String> = paragraphs(sec).iter().map(|p| p.text()).collect();
        assert_eq!(
            paras,
            [
                "The quick brown fox jumps over the lazy dog and keeps running through the forest until the evening comes and everyone goes home to rest after a very long day of walking",
                "The end.",
                "- first item",
                "- second item",
            ]
        );
    }

    #[test]
    fn right_to_left_lines_are_read_in_logical_order() {
        use super::content::Seg;
        let line = |text: &str, a0: f64, a1: f64| {
            Item::Line(Line {
                segs: vec![Seg { text: text.to_owned(), style: 0 }],
                a0,
                a1,
                across: 0.0,
                size: 12.0,
            })
        };
        let text = |i: &Item| if let Item::Line(l) = i { l.text() } else { String::new() };
        let reversed = |s: &str| s.chars().rev().collect::<String>();
        let mut items = vec![
            // Flush left with a Hebrew word in it: a left-to-right paragraph.
            line(&format!("x {} y", reversed("עברית")), 0.0, 120.0),
            // Flush right, short: a right-to-left paragraph; drawn backwards.
            line(&reversed("שלום עולם"), 420.0, 500.0),
            // The same picture as the first line, but flush right.
            line(&format!("x {} y", reversed("עברית")), 380.0, 500.0),
            // The full-width line fixes both margins.
            line("a long line of ordinary Latin text across the whole column", 0.0, 500.0),
            // A table row: the cells stay where they are on the page, each is turned round.
            line(&format!("{}\t{}", reversed("שלום"), reversed("עולם")), 0.0, 500.0),
            // Nothing right to left: untouched.
            line("plain text, 123", 0.0, 90.0),
        ];
        fix_reading_order(&mut items);
        let t: Vec<String> = items.iter().map(text).collect();
        assert_eq!(t[0], "x עברית y");
        assert_eq!(t[1], "שלום עולם");
        assert_eq!(t[2], "y עברית x", "flush right means a right-to-left paragraph");
        assert_eq!(t[3], "a long line of ordinary Latin text across the whole column");
        assert_eq!(t[4], "שלום\tעולם", "cells keep their order, each reads correctly");
        assert_eq!(t[5], "plain text, 123");
    }

    #[test]
    fn styles_survive_reordering() {
        use super::content::Seg;
        let mut l = Line {
            segs: vec![
                Seg { text: "ab ".into(), style: 0 },
                Seg { text: "םולש".into(), style: style::BOLD },
            ],
            a0: 0.0,
            a1: 50.0,
            across: 0.0,
            size: 12.0,
        };
        to_reading_order(&mut l, Base::Ltr);
        assert_eq!(l.text(), "ab שלום");
        assert!(l.segs.iter().any(|s| s.style == style::BOLD && s.text.contains('ש')));
    }

    #[test]
    fn headings_by_size() {
        let c = "BT /F1 24 Tf 72 740 Td (Big Title) Tj /F1 11 Tf 0 -40 Td (body text one) Tj T* (body text two) Tj ET";
        let p = ok(&PdfBuilder::with_content(c).build());
        let sec = section_of(&p, SectionKind::Page).expect("page");
        let styles: Vec<ParaStyle> = paragraphs(sec).iter().map(|p| p.style).collect();
        assert_eq!(styles.first(), Some(&ParaStyle::Heading1));
        assert_eq!(styles.get(1), Some(&ParaStyle::Body));
    }

    #[test]
    fn hidden_text_is_shown_and_counted() {
        let c = "BT /F1 12 Tf 72 700 Td (public) Tj ET 1 g BT /F1 12 Tf 72 600 Td (white secret) Tj ET 0 g BT 3 Tr 72 500 Td (invisible secret) Tj ET";
        let p = ok(&PdfBuilder::with_content(c).build());
        let sec = section_of(&p, SectionKind::Page).expect("page");
        let hidden: Vec<String> = paragraphs(sec)
            .iter()
            .flat_map(|p| p.runs.iter())
            .filter(|r| r.style & style::HIDDEN != 0)
            .map(|r| r.text.clone())
            .collect();
        assert_eq!(hidden, ["white secret", "invisible secret"]);
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "text" && n.count == 2)
        );
        // Without a request for hidden content they are left out and counted.
        let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
        let q = crate::preview(&PdfBuilder::with_content(c).build(), &policy).expect("preview");
        assert_eq!(pages_text(&q), ["public"]);
        assert!(q.notices.iter().any(|n| n.kind == NoticeKind::HiddenOmitted && n.count == 2));
    }

    #[test]
    fn images_links_and_annotations() {
        let mut b = PdfBuilder::with_content("/Im1 Do BT /F1 12 Tf 72 700 Td (text) Tj ET");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> /Annots [7 0 R 8 0 R 9 0 R] >>",
        );
        b.add_stream(6, "/Type /XObject /Subtype /Image /Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0u8; 100]);
        b.add_raw(7, "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /URI /URI (http://evil.example/login) >> >>");
        b.add_raw(8, "<< /Type /Annot /Subtype /Text /Contents (\\376\\377\\000R\\000e\\000v\\000i\\000e\\000w) >>");
        b.add_raw(9, "<< /Type /Annot /Subtype /Widget /T (name) /V (Alice) >>");
        let p = ok(&b.build());
        let sec = section_of(&p, SectionKind::Page).expect("page");
        assert!(matches!(sec.blocks.first(), Some(Block::Image(i)) if i.alt == "10×10"));
        let paras = paragraphs(sec);
        let link =
            paras.iter().flat_map(|p| p.runs.iter()).find(|r| !r.href.is_empty()).expect("link");
        assert_eq!(link.href, "http://evil.example/login");
        let comments: Vec<String> =
            paras.iter().filter(|p| p.style == ParaStyle::Comment).map(|p| p.text()).collect();
        assert_eq!(comments, ["Review", "name: Alice"]);
        // The picture can be opened (its id is its object number), so it is not counted as left out.
        assert!(matches!(sec.blocks.first(), Some(Block::Image(i)) if i.id == 6));
        assert!(!p.notices.iter().any(|n| n.kind == NoticeKind::ImagesNotShown));
    }

    #[test]
    fn page_tree_variants() {
        // Nested /Pages nodes, inherited resources, a loop in /Kids.
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(2, "<< /Type /Pages /Kids [10 0 R 3 0 R 2 0 R] /Count 2 /Resources << /Font << /F1 5 0 R >> >> >>");
        b.add_raw(10, "<< /Type /Pages /Parent 2 0 R /Kids [11 0 R] /Count 1 >>");
        b.add_raw(11, "<< /Type /Page /Parent 10 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>");
        b.add_raw(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 12 0 R >>");
        b.add_stream(4, "", b"BT /F1 12 Tf 72 700 Td (one) Tj ET");
        b.add_stream(12, "", b"BT /F1 12 Tf 72 700 Td (two) Tj ET");
        b.add_raw(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let p = ok(&b.build());
        assert_eq!(pages_text(&p), ["one", "two"]);
    }

    #[test]
    fn broken_page_tree_falls_back_to_scanning() {
        let mut b = PdfBuilder::simple_text("rescued");
        b.add_raw(2, "<< /Type /Pages /Kids [99 0 R] /Count 1 >>");
        let p = ok(&b.build());
        assert_eq!(pages_text(&p), ["rescued"]);
        assert!(p.notices.iter().any(|n| n.detail.contains("page tree")));
    }

    #[test]
    fn encrypted_and_empty_documents() {
        let enc = PdfBuilder::simple_text("x").with_trailer_entry("/Encrypt 9 0 R").build();
        assert_eq!(run(&enc).err(), Some(PreviewError::Encrypted));
        assert!(matches!(run(b"%PDF-1.4\n%%EOF"), Err(PreviewError::Malformed(_))));
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        assert!(matches!(run(&b.build()), Err(PreviewError::Malformed(_))));
    }

    #[test]
    fn page_limit_is_reported() {
        let mut b = PdfBuilder::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..30).map(|i| format!("{} 0 R ", 10 + i)).collect();
        b.add_raw(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 30 /Resources << >> /MediaBox [0 0 100 100] >>"));
        for i in 0..30u32 {
            b.add_raw(10 + i, "<< /Type /Page /Parent 2 0 R >>");
        }
        let policy = PreviewPolicy { max_sections: 5, ..PreviewPolicy::default() };
        let p = crate::preview(&b.build(), &policy).expect("preview");
        assert_eq!(p.sections.len(), 5);
        assert!(p.stats.truncated);
    }

    #[test]
    fn invisible_characters_in_text_are_removed() {
        let p =
            ok(&PdfBuilder::with_content("BT /F1 12 Tf 72 700 Td (a\\342\\200\\256b) Tj ET")
                .build());
        // U+202E is not a WinAnsi character; the three bytes are three separate glyphs.
        assert!(!pages_text(&p).is_empty());
    }

    #[test]
    fn mutated_documents_never_panic() {
        let seeds = [
            PdfBuilder::simple_text("Hello").build(),
            PdfBuilder::simple_text("Hello").build_modern(),
        ];
        for base in &seeds {
            for i in 0..base.len() {
                for mask in [0x01u8, 0x40, 0xFF] {
                    let mut m = base.clone();
                    if let Some(b) = m.get_mut(i) {
                        *b ^= mask;
                    }
                    let _ = run(&m);
                }
            }
            for cut in (0..base.len()).step_by(7) {
                let _ = run(base.get(..cut).unwrap_or(&[]));
            }
        }
    }

    // ---- drawing a page -------------------------------------------------------------------

    use crate::page::{PageModel, Prim, op as pathop};

    fn drawn(b: &PdfBuilder) -> PageModel {
        render(&b.build(), 0, &PreviewPolicy::default()).expect("renders")
    }

    fn path_at(m: &PageModel, i: usize) -> (&[f32], u32, u32, f32) {
        match m.prims.get(i) {
            Some(Prim::Path { ops, fill, stroke, width, .. }) => (ops, *fill, *stroke, *width),
            other => panic!("not a path: {other:?}"),
        }
    }

    #[test]
    fn a_filled_rectangle_lands_where_a_viewer_puts_it() {
        let m = drawn(&PdfBuilder::with_content("1 0 0 rg 72 600 100 50 re f"));
        assert_eq!((m.width, m.height, m.pages, m.index), (612.0, 792.0, 1, 0));
        assert_eq!(m.prims.len(), 1);
        let (ops, fill, stroke, _) = path_at(&m, 0);
        // The page's y axis is flipped: 792 - 600 = 192 is the bottom edge of the rectangle,
        // and 792 - 650 = 142 its top edge.
        assert_eq!(
            ops,
            [
                pathop::MOVE,
                72.0,
                192.0,
                pathop::LINE,
                172.0,
                192.0,
                pathop::LINE,
                172.0,
                142.0,
                pathop::LINE,
                72.0,
                142.0,
                pathop::CLOSE
            ]
        );
        assert_eq!(fill, 0xFFFF_0000);
        assert_eq!(stroke, 0);
    }

    #[test]
    fn strokes_colours_widths_and_curves_are_recorded() {
        let m = drawn(&PdfBuilder::with_content(
            "0 0 1 RG 3 w 10 10 m 20 20 l 30 10 40 20 50 10 c S 0.5 g 0 0 10 10 re B",
        ));
        let (ops, fill, stroke, width) = path_at(&m, 0);
        assert_eq!(fill, 0);
        assert_eq!(stroke, 0xFF00_00FF);
        assert!((width - 3.0).abs() < 1e-6);
        assert_eq!(ops.iter().filter(|v| **v == pathop::CURVE).count(), 1);
        // The second shape is filled grey and stroked blue.
        let (_, fill, stroke, _) = path_at(&m, 1);
        assert_eq!(fill, 0xFF80_8080);
        assert_eq!(stroke, 0xFF00_00FF);
    }

    #[test]
    fn clips_and_saves_are_recorded_in_order() {
        let m = drawn(&PdfBuilder::with_content(
            "q 10 10 100 100 re W n 0 g 0 0 612 792 re f Q 1 g 0 0 5 5 re f",
        ));
        assert!(matches!(m.prims[0], Prim::Save));
        match m.prims[1] {
            Prim::Clip { x, y, w, h } => assert_eq!((x, y, w, h), (10.0, 682.0, 100.0, 100.0)),
            ref other => panic!("{other:?}"),
        }
        assert!(matches!(m.prims[2], Prim::Path { .. }));
        assert!(matches!(m.prims[3], Prim::Restore));
        assert!(matches!(m.prims[4], Prim::Path { fill: 0xFFFF_FFFF, .. }));
    }

    #[test]
    fn text_keeps_its_position_size_colour_and_width() {
        let m =
            drawn(&PdfBuilder::with_content("0 0.5 0 rg BT /F1 20 Tf 100 700 Td (Hello) Tj ET"));
        let Some(Prim::Text { x, y, size, angle, width, color, text, .. }) = m.prims.first() else {
            panic!("{:?}", m.prims)
        };
        assert_eq!(text, "Hello");
        assert_eq!((*x, *y, *size), (100.0, 92.0, 20.0));
        assert!(angle.abs() < 1e-6);
        // Helvetica: H 722, e 556, l 222, l 222, o 556 thousandths of an em.
        assert!((width - 2.278 * 20.0).abs() < 0.01, "{width}");
        assert_eq!(*color, 0xFF00_8000);
    }

    #[test]
    fn rotated_text_and_pages_turn_together() {
        // Text going up the page: angle -90° on the shown page.
        let m = drawn(&PdfBuilder::with_content("BT /F1 12 Tf 0 1 -1 0 100 100 Tm (up) Tj ET"));
        let Some(Prim::Text { angle, .. }) = m.prims.first() else { panic!() };
        assert!((angle + core::f32::consts::FRAC_PI_2).abs() < 1e-5, "{angle}");
        // A page rotated by 90°: the shown page is 792 wide and 612 tall, and the text, which
        // read left to right, now runs down.
        let mut b = PdfBuilder::with_content("BT /F1 12 Tf 72 700 Td (turned) Tj ET");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /Rotate 90 /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        let m = drawn(&b);
        assert_eq!((m.width, m.height), (792.0, 612.0));
        let Some(Prim::Text { x, y, angle, .. }) = m.prims.first() else { panic!() };
        assert_eq!((*x, *y), (700.0, 72.0));
        assert!((angle - core::f32::consts::FRAC_PI_2).abs() < 1e-5, "{angle}");
    }

    #[test]
    fn hidden_text_and_hidden_layers_are_not_drawn() {
        let m = drawn(&PdfBuilder::with_content(
            "BT /F1 12 Tf 3 Tr 72 700 Td (invisible) Tj ET BT /F1 12 Tf 0 Tr 72 650 Td (seen) Tj ET",
        ));
        let words: Vec<_> = m
            .prims
            .iter()
            .filter_map(
                |p| if let Prim::Text { text, .. } = p { Some(text.as_str()) } else { None },
            )
            .collect();
        assert_eq!(words, ["seen"]);
    }

    #[test]
    fn pictures_become_placeholders_with_their_matrix() {
        let mut b = PdfBuilder::with_content("q 200 0 0 100 50 500 cm /Im1 Do Q");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Im1 6 0 R >> >> >>",
        );
        b.add_stream(6, "/Type /XObject /Subtype /Image /Width 640 /Height 480 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[0u8; 8]);
        let m = drawn(&b);
        let img = m.prims.iter().find_map(|p| match p {
            Prim::Image { m, label, .. } => Some((*m, label.clone())),
            _ => None,
        });
        let (matrix, label) = img.expect("an image");
        assert_eq!(label, "640×480");
        // The unit square becomes 200 × 100 points; y is flipped (and so is the picture's own y).
        assert_eq!(matrix, [200.0, 0.0, 0.0, -100.0, 50.0, 292.0]);
        assert!(m.approximate);
    }

    #[test]
    fn a_page_that_does_not_exist_and_other_formats_are_refused() {
        let data = PdfBuilder::simple_text("x").build();
        assert!(matches!(
            render(&data, 1, &PreviewPolicy::default()),
            Err(PreviewError::Malformed(_))
        ));
        assert!(matches!(
            crate::render_page(b"hello", 0, &PreviewPolicy::default()),
            Err(PreviewError::Unsupported(_))
        ));
    }

    #[test]
    fn hostile_content_stays_bounded_and_never_panics() {
        // A flood of paths, a huge path, unbalanced Q, nonsense numbers.
        let flood = "0 0 5 5 re f ".repeat(200_000);
        let m = drawn(&PdfBuilder::with_content(&flood));
        assert!(m.prims.len() <= crate::page::MAX_PRIMS);
        let long = format!("0 0 m {} f", "1 1 l ".repeat(300_000));
        let m = drawn(&PdfBuilder::with_content(&long));
        assert!(m.truncated || m.prims.len() <= 1);
        let m = drawn(&PdfBuilder::with_content(
            "Q Q Q 1e400 0 0 1e400 0 0 cm 0 0 1e300 1e300 re f nan inf m l c",
        ));
        let _ = m.prims.len();
    }
}
