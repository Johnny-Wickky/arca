//! Plain text, delimited text (CSV / TSV) and Markdown.
//!
//! None of these formats can carry active content, but their text is still
//! untrusted: it passes through the same sink as everything else.

use std::cell::Cell as Flag;

use crate::model::{
    Align, Block, Cell, CellKind, FileFormat, ImageInfo, NoticeKind, ParaStyle, Row, SectionKind,
    Table, style,
};
use crate::sink::{GridBuilder, ParaBuilder, Sink};

/// Characters of one Markdown line that are scanned for inline markup; the rest of a longer line
/// is shown as it is. Markup scanning needs the line as characters and several look-ahead
/// tables, so its cost and memory must not follow the length of a hostile line.
const MAX_INLINE_CHARS: usize = 16_384;

/// Lines of `text`, split at `\n`, `\r\n` and a lone `\r` (old Mac files). A lone carriage return
/// is a line break; dropping it would glue two lines into one.
fn lines_of(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        let line = rest.get(..end)?;
        let tail = rest.get(end..).unwrap_or_default();
        rest =
            tail.strip_prefix("\r\n").or_else(|| tail.strip_prefix(['\r', '\n'])).unwrap_or(tail);
        Some(line)
    })
}

/// Lines per block for plain text: keeps list rows small for the UI.
const LINES_PER_BLOCK: usize = 200;
/// Bytes per block for plain text.
const BYTES_PER_BLOCK: usize = 16 << 10;

/// Decoded text input.
pub(crate) struct Decoded {
    /// The text.
    pub(crate) text: String,
    /// Code page assumed for legacy 8-bit text (a guess), `None` for UTF-8 / UTF-16.
    pub(crate) legacy: Option<u16>,
}

/// Decodes text input: UTF-8 (BOM optional), UTF-16 with BOM, or legacy 8-bit text.
pub(crate) fn decode(data: &[u8]) -> Option<Decoded> {
    let unicode = |text: Option<String>| text.map(|text| Decoded { text, legacy: None });
    match data {
        [0xEF, 0xBB, 0xBF, rest @ ..] => {
            unicode(core::str::from_utf8(rest).ok().map(str::to_owned))
        }
        [0xFF, 0xFE, rest @ ..] => unicode(utf16(rest, u16::from_le_bytes)),
        [0xFE, 0xFF, rest @ ..] => unicode(utf16(rest, u16::from_be_bytes)),
        _ => match core::str::from_utf8(data) {
            Ok(t) => unicode(Some(t.to_owned())),
            Err(_) => arca_container::text::decode_legacy_text(data)
                .map(|(cp, text)| Decoded { text, legacy: Some(cp.id()) }),
        },
    }
}

fn utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes.chunks_exact(2).filter_map(|c| <[u8; 2]>::try_from(c).ok()).map(f);
    Some(char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect())
}

/// Extracts a text file.
pub(crate) fn plain(sink: &mut Sink<'_>, text: &str) {
    let Some(sec) = sink.open_section(SectionKind::Body, "", false) else { return };
    let mut chunk = String::new();
    let mut lines = 0usize;
    for line in lines_of(text) {
        chunk.push_str(line);
        chunk.push('\n');
        lines += 1;
        if (lines >= LINES_PER_BLOCK || chunk.len() >= BYTES_PER_BLOCK)
            && !flush_code(sink, sec, &mut chunk, &mut lines)
        {
            return;
        }
    }
    flush_code(sink, sec, &mut chunk, &mut lines);
}

fn flush_code(
    sink: &mut Sink<'_>,
    sec: crate::sink::SectionId,
    chunk: &mut String,
    lines: &mut usize,
) -> bool {
    *lines = 0;
    if chunk.is_empty() {
        return true;
    }
    let body = chunk.trim_end_matches('\n').to_owned();
    chunk.clear();
    let mut b = ParaBuilder::new();
    b.text(&body, 0, "");
    sink.para(sec, b.take(ParaStyle::Code))
}

// ---------------------------------------------------------------------------
// CSV / TSV
// ---------------------------------------------------------------------------

/// Picks `,`, `;` or tab from the first line (outside quotes).
pub(crate) fn detect_delimiter(text: &str, default: char) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in first.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted {
            for (d, n) in &mut counts {
                if *d == c {
                    *n += 1;
                }
            }
        }
    }
    let best = counts.iter().max_by_key(|(_, n)| *n).copied();
    match best {
        Some((d, n)) if n > 0 => d,
        _ => default,
    }
}

/// Splits delimited text into records (RFC 4180 quoting; a record ends at `\r\n`, `\n` or a lone
/// `\r`).
///
/// Calls `row` for every record; stops when it returns `false`. Returns `true` if the text ended
/// inside a quoted field, whose quote was never closed: everything after it is one field.
fn records(text: &str, delim: char, mut row: impl FnMut(&[String]) -> bool) -> bool {
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else if c == '\r' {
                // A line break inside a quoted field is kept as one.
                if chars.peek() != Some(&'\n') {
                    field.push('\n');
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == delim => fields.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    continue;
                }
                fields.push(std::mem::take(&mut field));
                if !row(&fields) {
                    return false;
                }
                fields.clear();
                any = false;
            }
            c => field.push(c),
        }
    }
    if any {
        fields.push(field);
        row(&fields);
    }
    quoted
}

fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.len() > 40 {
        return false;
    }
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    if body.starts_with('0') && body.len() > 1 && !body.starts_with("0.") && !body.starts_with("0,")
    {
        return false; // identifiers such as 007
    }
    t.replace(',', ".").parse::<f64>().is_ok_and(f64::is_finite)
}

/// Extracts a CSV / TSV file as a grid.
pub(crate) fn delimited(sink: &mut Sink<'_>, text: &str, delim: char) {
    let Some(sec) = sink.open_section(SectionKind::Sheet, "", false) else { return };
    let mut grid =
        GridBuilder::new(sink.policy, sink.items_left()).with_char_budget(sink.chars_left());
    let mut row_no = 0u32;
    let open_quote = records(text, delim, |fields| {
        row_no = row_no.saturating_add(1);
        if fields.iter().all(|f| f.trim().is_empty()) {
            return true;
        }
        if !grid.row(row_no, false) {
            return false;
        }
        for (i, f) in fields.iter().enumerate() {
            let col = u32::try_from(i).unwrap_or(u32::MAX);
            let kind = if looks_numeric(f) { CellKind::Number } else { CellKind::Text };
            if !grid.cell(col, kind, f.clone()) {
                return false;
            }
        }
        true
    });
    let g = grid.finish();
    if g.truncated {
        sink.truncated("grid");
    }
    if open_quote {
        sink.note(NoticeKind::BestEffort, "a quoted field is never closed: the rest is one cell");
    }
    sink.push(sec, Block::Grid(g));
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

/// Extracts a Markdown file. HTML in the source is shown literally.
pub(crate) fn markdown(sink: &mut Sink<'_>, text: &str) {
    let Some(sec) = sink.open_section(SectionKind::Body, "", false) else { return };
    let lines: Vec<&str> = lines_of(text).collect();
    let cut = Flag::new(false);
    markdown_lines(sink, sec, &lines, &cut);
    if cut.get() {
        sink.note(NoticeKind::BestEffort, "markdown: markup of very long lines is not interpreted");
    }
}

fn markdown_lines(
    sink: &mut Sink<'_>,
    sec: crate::sink::SectionId,
    lines: &[&str],
    cut: &Flag<bool>,
) {
    let mut i = 0usize;
    let mut para: Vec<&str> = Vec::new();
    while let Some(&line) = lines.get(i) {
        if sink.is_full() {
            return;
        }
        let trimmed = line.trim_start();
        let indent = line.len().saturating_sub(trimmed.len());

        // Fenced code block.
        if let Some(fence) = fence_of(trimmed) {
            if !flush_para(sink, sec, &mut para, cut) {
                return;
            }
            let mut code = String::new();
            i += 1;
            while let Some(&l) = lines.get(i) {
                if l.trim_start().starts_with(fence) {
                    break;
                }
                code.push_str(l);
                code.push('\n');
                i += 1;
            }
            i += 1;
            let mut b = ParaBuilder::new();
            b.text(code.trim_end_matches('\n'), 0, "");
            if !sink.para(sec, b.take(ParaStyle::Code)) {
                return;
            }
            continue;
        }

        if trimmed.is_empty() {
            if !flush_para(sink, sec, &mut para, cut) {
                return;
            }
            i += 1;
            continue;
        }

        if is_rule(trimmed) {
            if !flush_para(sink, sec, &mut para, cut) || !sink.push(sec, Block::Rule) {
                return;
            }
            i += 1;
            continue;
        }

        if let Some((level, rest)) = heading_of(trimmed) {
            if !flush_para(sink, sec, &mut para, cut) {
                return;
            }
            let mut b = ParaBuilder::new();
            inline(&mut b, rest.trim().trim_end_matches('#').trim_end(), 0, cut);
            let mut p = b.take(ParaStyle::heading(level));
            p.align = Align::Start;
            if !sink.para(sec, p) {
                return;
            }
            i += 1;
            continue;
        }

        // Table: a row with pipes followed by a separator row.
        if trimmed.contains('|') && lines.get(i + 1).is_some_and(|n| is_table_separator(n.trim())) {
            if !flush_para(sink, sec, &mut para, cut) {
                return;
            }
            let mut rows = Vec::new();
            let mut first = true;
            let mut j = i;
            while let Some(&l) = lines.get(j) {
                let t = l.trim();
                if t.is_empty() || !t.contains('|') {
                    break;
                }
                if !(j == i + 1 && is_table_separator(t)) {
                    rows.push(Row {
                        cells: split_table_row(t)
                            .into_iter()
                            .map(|c| {
                                let mut b = ParaBuilder::new();
                                inline(&mut b, c.trim(), if first { style::BOLD } else { 0 }, cut);
                                Cell {
                                    span: 1,
                                    header: first,
                                    paras: vec![b.take(ParaStyle::Body)],
                                }
                            })
                            .collect(),
                    });
                    first = false;
                }
                j += 1;
            }
            i = j;
            if !sink.push(sec, Block::Table(Table { rows })) {
                return;
            }
            continue;
        }

        // Quote.
        if let Some(rest) = trimmed.strip_prefix('>') {
            if !flush_para(sink, sec, &mut para, cut) {
                return;
            }
            let mut b = ParaBuilder::new();
            inline(&mut b, rest.trim_start(), 0, cut);
            if !sink.para(sec, b.take(ParaStyle::Quote)) {
                return;
            }
            i += 1;
            continue;
        }

        // List item.
        if let Some((marker, rest)) = list_item(trimmed) {
            if !flush_para(sink, sec, &mut para, cut) {
                return;
            }
            let mut b = ParaBuilder::new();
            inline(&mut b, rest, 0, cut);
            let mut p = b.take(ParaStyle::Body);
            p.marker = marker;
            p.level = u8::try_from((indent / 2).min(6) + 1).unwrap_or(1);
            if !sink.para(sec, p) {
                return;
            }
            i += 1;
            continue;
        }

        para.push(line);
        i += 1;
    }
    flush_para(sink, sec, &mut para, cut);
}

fn fence_of(t: &str) -> Option<&'static str> {
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn is_rule(t: &str) -> bool {
    let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && ["-", "*", "_"].iter().any(|m| compact.chars().all(|c| m.starts_with(c)))
}

fn heading_of(t: &str) -> Option<(u8, &str)> {
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = t.get(hashes..)?;
        if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
            return Some((u8::try_from(hashes).unwrap_or(6), rest));
        }
    }
    None
}

fn is_table_separator(t: &str) -> bool {
    t.contains('-')
        && t.contains('|')
        && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

fn split_table_row(t: &str) -> Vec<&str> {
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').collect()
}

fn list_item(t: &str) -> Option<(String, &str)> {
    let mut chars = t.chars();
    let first = chars.next()?;
    if matches!(first, '-' | '*' | '+') {
        let rest = t.get(1..)?;
        if rest.starts_with(' ') || rest.starts_with('\t') {
            // Task list items keep their check box as text.
            let body = rest.trim_start();
            return Some(("•".to_owned(), body));
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let after = t.get(digits..)?;
        if let Some(rest) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            let number = t.get(..digits)?;
            return Some((format!("{number}."), rest.trim_start()));
        }
    }
    None
}

fn flush_para(
    sink: &mut Sink<'_>,
    sec: crate::sink::SectionId,
    para: &mut Vec<&str>,
    cut: &Flag<bool>,
) -> bool {
    if para.is_empty() {
        return true;
    }
    let mut b = ParaBuilder::new();
    let last = para.len().saturating_sub(1);
    for (k, l) in para.iter().enumerate() {
        let hard = l.ends_with("  ") || l.ends_with('\\');
        let body = l.trim().trim_end_matches('\\');
        inline(&mut b, body, 0, cut);
        if k != last {
            b.text(if hard { "\n" } else { " " }, 0, "");
        }
    }
    para.clear();
    sink.para(sec, b.take(ParaStyle::Body))
}

/// Inline Markdown: `**bold**`, `*italic*`, `` `code` ``, `~~strike~~`, links, images.
///
/// Only the first [`MAX_INLINE_CHARS`] characters are scanned for markup; the rest of a longer
/// line is appended as it is and `cut` is set so that the caller can say so.
fn inline(b: &mut ParaBuilder, s: &str, base: u16, cut: &Flag<bool>) {
    let split = if s.len() <= MAX_INLINE_CHARS {
        s.len()
    } else {
        s.char_indices().nth(MAX_INLINE_CHARS).map_or(s.len(), |(i, _)| i)
    };
    let (head, tail) = (s.get(..split).unwrap_or(s), s.get(split..).unwrap_or_default());
    inline_head(b, head, base);
    if !tail.is_empty() {
        cut.set(true);
        b.text(tail, base, "");
    }
}

/// Look-ahead tables for the links of one line, built in one pass so that a line of nothing but
/// `[` or `[a](` costs the same per character as any other.
struct LinkTables {
    /// For each `[`: the index of its matching `]`.
    close_of: Vec<Option<usize>>,
    /// For each position: the first `)` at or after it (`usize::MAX` if none); one entry longer
    /// than the line.
    next_rparen: Vec<usize>,
}

impl LinkTables {
    fn new(chars: &[char]) -> Self {
        let mut close_of = vec![None; chars.len()];
        let mut open: Vec<usize> = Vec::new();
        for (k, &c) in chars.iter().enumerate() {
            match c {
                '[' => open.push(k),
                ']' => {
                    if let Some(o) = open.pop()
                        && let Some(slot) = close_of.get_mut(o)
                    {
                        *slot = Some(k);
                    }
                }
                _ => {}
            }
        }
        let mut next_rparen = vec![usize::MAX; chars.len().saturating_add(1)];
        let mut next = usize::MAX;
        for (k, &c) in chars.iter().enumerate().rev() {
            if c == ')' {
                next = k;
            }
            if let Some(slot) = next_rparen.get_mut(k) {
                *slot = next;
            }
        }
        Self { close_of, next_rparen }
    }
}

fn inline_head(b: &mut ParaBuilder, s: &str, base: u16) {
    let chars: Vec<char> = s.chars().collect();
    let links = chars.contains(&'[').then(|| LinkTables::new(&chars));
    // Backtick run lengths for which no closing run exists: asking again would scan the whole
    // line again.
    let mut unclosed: Vec<usize> = Vec::new();
    let mut st = base;
    let mut lit = String::new();
    let mut i = 0usize;
    let at = |k: usize| chars.get(k).copied();
    macro_rules! flush {
        () => {
            if !lit.is_empty() {
                b.text(&lit, st, "");
                lit.clear();
            }
        };
    }
    while let Some(c) = at(i) {
        match c {
            '\\' if at(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                if let Some(n) = at(i + 1) {
                    lit.push(n);
                }
                i += 2;
            }
            '`' => {
                let ticks = (i..chars.len()).take_while(|&k| at(k) == Some('`')).count();
                let close = if unclosed.contains(&ticks) {
                    None
                } else {
                    let found = find_run(&chars, i + ticks, '`', ticks);
                    if found.is_none() {
                        unclosed.push(ticks);
                    }
                    found
                };
                if let Some(end) = close {
                    flush!();
                    let code: String =
                        chars.get(i + ticks..end).unwrap_or_default().iter().collect();
                    b.text(code.trim(), st | style::MONO, "");
                    i = end + ticks;
                } else {
                    lit.extend(std::iter::repeat_n('`', ticks));
                    i += ticks;
                }
            }
            '*' | '_' => {
                let double = at(i + 1) == Some(c);
                let n = if double { 2 } else { 1 };
                let prev = if i == 0 { None } else { at(i - 1) };
                let next = at(i + n);
                let flag = if double { style::BOLD } else { style::ITALIC };
                let active = st & flag != 0 && base & flag == 0;
                let intraword = c == '_'
                    && prev.is_some_and(char::is_alphanumeric)
                    && next.is_some_and(char::is_alphanumeric);
                let opens = !active && next.is_some_and(|x| !x.is_whitespace());
                let closes = active && prev.is_some_and(|x| !x.is_whitespace());
                if !intraword && (opens || closes) {
                    flush!();
                    st ^= flag;
                } else {
                    lit.extend(std::iter::repeat_n(c, n));
                }
                i += n;
            }
            '~' if at(i + 1) == Some('~') => {
                flush!();
                st ^= style::STRIKE;
                i += 2;
            }
            '!' if at(i + 1) == Some('[') => {
                if let Some((label, _target, end)) = link_at(&chars, links.as_ref(), i + 1) {
                    flush!();
                    b.text(&format!("[{label}]"), st | style::ITALIC, "");
                    i = end;
                } else {
                    lit.push('!');
                    i += 1;
                }
            }
            '[' => {
                if let Some((label, target, end)) = link_at(&chars, links.as_ref(), i) {
                    flush!();
                    b.text(&label, st | style::UNDERLINE, &target);
                    i = end;
                } else {
                    lit.push('[');
                    i += 1;
                }
            }
            c => {
                lit.push(c);
                i += 1;
            }
        }
    }
    flush!();
}

fn find_run(chars: &[char], from: usize, ch: char, len: usize) -> Option<usize> {
    let mut k = from;
    while k < chars.len() {
        if chars.get(k) == Some(&ch) {
            let run = (k..chars.len()).take_while(|&j| chars.get(j) == Some(&ch)).count();
            if run == len {
                return Some(k);
            }
            k += run;
        } else {
            k += 1;
        }
    }
    None
}

/// Parses `[label](target)` starting at `chars[start] == '['`.
fn link_at(
    chars: &[char],
    tables: Option<&LinkTables>,
    start: usize,
) -> Option<(String, String, usize)> {
    let tables = tables?;
    let close = tables.close_of.get(start).copied().flatten()?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let rparen = *tables.next_rparen.get(close + 2)?;
    if rparen == usize::MAX {
        return None;
    }
    let label: String = chars.get(start + 1..close)?.iter().collect();
    let target: String = chars.get(close + 2..rparen)?.iter().collect();
    let target = target.split_whitespace().next().unwrap_or("").to_owned();
    Some((label, target, rparen + 1))
}

/// A picture placeholder for formats where text embeds an image reference.
#[allow(dead_code)]
pub(crate) fn image_placeholder(alt: &str) -> Block {
    Block::Image(ImageInfo { alt: alt.to_owned(), ..ImageInfo::default() })
}

/// Which extractor applies to a text format.
pub(crate) fn extract(sink: &mut Sink<'_>, text: &str, format: FileFormat) {
    match format {
        FileFormat::Csv => delimited(sink, text, detect_delimiter(text, ',')),
        FileFormat::Tsv => delimited(sink, text, '\t'),
        FileFormat::Markdown => markdown(sink, text),
        _ => plain(sink, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewPolicy;
    use crate::model::Paragraph;
    use crate::model::{Preview, Run};

    fn run(format: FileFormat, text: &str) -> Preview {
        let policy = PreviewPolicy::default();
        let mut sink = Sink::new(&policy);
        extract(&mut sink, text, format);
        sink.finish(format)
    }

    fn paras(p: &Preview) -> Vec<&Paragraph> {
        p.sections
            .iter()
            .flat_map(|s| s.blocks.iter())
            .filter_map(|b| if let Block::Para(p) = b { Some(p) } else { None })
            .collect()
    }

    fn unicode(data: &[u8]) -> Option<String> {
        decode(data).filter(|d| d.legacy.is_none()).map(|d| d.text)
    }

    #[test]
    fn decoding() {
        assert_eq!(unicode(b"abc").as_deref(), Some("abc"));
        assert_eq!(unicode(&[0xEF, 0xBB, 0xBF, b'x']).as_deref(), Some("x"));
        let mut u16le = vec![0xFF, 0xFE];
        for u in "Привет".encode_utf16() {
            u16le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(unicode(&u16le).as_deref(), Some("Привет"));
        let mut u16be = vec![0xFE, 0xFF];
        for u in "ab".encode_utf16() {
            u16be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(unicode(&u16be).as_deref(), Some("ab"));
        assert!(decode(&[0xC3, 0x28]).is_none());
        assert!(decode(&[0xFF, 0xFE, 0x41]).is_none());
        // Invalid UTF-16 / UTF-8 BOM content is never reinterpreted as legacy text.
        assert!(decode(&[0xEF, 0xBB, 0xBF, 0xC3, 0x28]).is_none());
    }

    #[test]
    fn legacy_eight_bit_text_is_decoded_and_labelled() {
        // "Привет, мир! Это тестовый файл." in Windows-1251.
        let bytes = [
            0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, b',', b' ', 0xEC, 0xE8, 0xF0, b'!', b' ', 0xDD,
            0xF2, 0xEE, b' ', 0xF2, 0xE5, 0xF1, 0xF2, 0xEE, 0xE2, 0xFB, 0xE9, b' ', 0xF4, 0xE0,
            0xE9, 0xEB, b'.',
        ];
        let d = decode(&bytes).expect("decoded");
        assert_eq!(d.text, "Привет, мир! Это тестовый файл.");
        assert_eq!(d.legacy, Some(1251));
        // Binary data is still refused.
        assert!(decode(&[0x00, 0x01, 0xFF, 0xFE, 0x02]).is_none());
    }

    #[test]
    fn plain_text_is_chunked_and_mono() {
        let text = (0..450).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let p = run(FileFormat::PlainText, &text);
        let ps = paras(&p);
        assert_eq!(ps.len(), 3);
        assert!(ps.iter().all(|p| p.style == ParaStyle::Code));
        assert!(ps.first().is_some_and(|p| p.text().starts_with("line 0\nline 1")));
        assert!(ps.last().is_some_and(|p| p.text().ends_with("line 449")));
    }

    #[test]
    fn plain_text_budget_truncates() {
        let policy = PreviewPolicy { max_chars: 1000, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&policy);
        let text = "x".repeat(100) + "\n";
        plain(&mut sink, &text.repeat(5000));
        let p = sink.finish(FileFormat::PlainText);
        assert!(p.stats.truncated);
        assert!(p.stats.chars <= 1000);
    }

    #[test]
    fn csv_quotes_and_delimiters() {
        let p = run(FileFormat::Csv, "name;note\n\"Иванов; И.\";\"say \"\"hi\"\"\"\n007;12,5\n");
        let Some(Block::Grid(g)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no grid")
        };
        assert_eq!(g.cols, 2);
        assert_eq!(g.rows.len(), 3);
        let cell = |r: usize, c: usize| {
            g.rows.get(r).and_then(|row| row.cells.get(c)).map(|c| (c.text.as_str(), c.kind))
        };
        assert_eq!(cell(1, 0), Some(("Иванов; И.", CellKind::Text)));
        assert_eq!(cell(1, 1), Some(("say \"hi\"", CellKind::Text)));
        assert_eq!(cell(2, 0), Some(("007", CellKind::Text)));
        assert_eq!(cell(2, 1), Some(("12,5", CellKind::Number)));
    }

    #[test]
    fn csv_embedded_newlines_blank_rows_and_crlf() {
        let p = run(FileFormat::Csv, "a,b\r\n\r\n\"x\ny\",2\r\n");
        let Some(Block::Grid(g)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no grid")
        };
        assert_eq!(g.rows.len(), 2);
        assert_eq!(g.rows.get(1).map(|r| r.index), Some(3));
        assert_eq!(
            g.rows.get(1).and_then(|r| r.cells.first()).map(|c| c.text.as_str()),
            Some("x\ny")
        );
    }

    #[test]
    fn tsv_and_delimiter_detection() {
        assert_eq!(detect_delimiter("a;b;c\n1;2;3", ','), ';');
        assert_eq!(detect_delimiter("a\tb\n", ','), '\t');
        assert_eq!(detect_delimiter("\"a,b\";c", ','), ';');
        assert_eq!(detect_delimiter("single", ','), ',');
        let p = run(FileFormat::Tsv, "a\tb\n1\t2\n");
        let Some(Block::Grid(g)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no grid")
        };
        assert_eq!(g.rows.len(), 2);
    }

    #[test]
    fn csv_row_limits_truncate() {
        let policy = PreviewPolicy { max_grid_cells: 10, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&policy);
        delimited(&mut sink, &"1,2,3\n".repeat(100), ',');
        let p = sink.finish(FileFormat::Csv);
        let Some(Block::Grid(g)) = p.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no grid")
        };
        assert!(g.truncated);
        assert!(g.rows.iter().map(|r| r.cells.len()).sum::<usize>() <= 10);
        assert!(p.stats.truncated);
    }

    #[test]
    fn markdown_blocks() {
        let md = "# Title\n\nSome *emph* and **strong** and `code` text\nsecond line\n\n\
                  - one\n- two\n  - nested\n1. first\n\n> quoted\n\n```\nlet x = 1;\n```\n\n---\n\
                  | h1 | h2 |\n|----|----|\n| a | b |\n";
        let p = run(FileFormat::Markdown, md);
        let blocks = &p.sections.first().map(|s| &s.blocks).cloned().unwrap_or_default();
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                Block::Para(p) => match p.style {
                    ParaStyle::Heading1 => "h1",
                    ParaStyle::Quote => "quote",
                    ParaStyle::Code => "code",
                    _ if !p.marker.is_empty() => "li",
                    _ => "p",
                },
                Block::Table(_) => "table",
                Block::Rule => "rule",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["h1", "p", "li", "li", "li", "li", "quote", "code", "rule", "table"]);
        let Some(Block::Para(p1)) = blocks.get(1) else { panic!("no para") };
        assert_eq!(p1.text(), "Some emph and strong and code text second line");
        let styles: Vec<(String, u16)> =
            p1.runs.iter().map(|r| (r.text.clone(), r.style)).collect();
        assert!(styles.contains(&("emph".to_owned(), style::ITALIC)));
        assert!(styles.contains(&("strong".to_owned(), style::BOLD)));
        assert!(styles.contains(&("code".to_owned(), style::MONO)));
        let Some(Block::Para(nested)) = blocks.get(4) else { panic!("no nested") };
        assert_eq!(nested.level, 2);
        let Some(Block::Para(num)) = blocks.get(5) else { panic!("no numbered") };
        assert_eq!(num.marker, "1.");
        let Some(Block::Table(t)) = blocks.last() else { panic!("no table") };
        assert_eq!(t.rows.len(), 2);
        assert!(t.rows.first().is_some_and(|r| r.cells.iter().all(|c| c.header)));
    }

    #[test]
    fn markdown_links_are_text_only_and_literals_survive() {
        let p = run(
            FileFormat::Markdown,
            "[click](https://evil.example/x \"t\") and ![pic](http://t.example/p.png) \
             2 * 3 = 6, snake_case_name, \\*escaped\\*, <script>alert(1)</script>",
        );
        let ps = paras(&p);
        let Some(para) = ps.first() else { panic!("no para") };
        let link: Vec<&Run> = para.runs.iter().filter(|r| !r.href.is_empty()).collect();
        assert_eq!(link.len(), 1);
        assert_eq!(link.first().map(|r| r.href.as_str()), Some("https://evil.example/x"));
        let text = para.text();
        assert!(text.contains("[pic]"));
        assert!(!text.contains("t.example"), "image targets are not exposed as links");
        assert!(text.contains("2 * 3 = 6"));
        assert!(text.contains("snake_case_name"));
        assert!(text.contains("*escaped*"));
        assert!(text.contains("<script>alert(1)</script>"), "HTML stays inert text");
    }

    #[test]
    fn markdown_never_panics_on_garbage() {
        let samples = [
            "",
            "#",
            "# ",
            "```",
            "~~~\n",
            "|",
            "|-|\n|-|",
            "[",
            "[]()",
            "![",
            "**",
            "_ _",
            "`",
            "``` ` ```",
            "1.",
            "- ",
            "> ",
            "\\",
            "[a](",
            "[a]( b",
            "***bold italic***",
            "a  \nb",
            "[[[[]]]](x)",
            "~~",
            "\u{0}\u{1}",
        ];
        for s in samples {
            let _ = run(FileFormat::Markdown, s);
            let _ = run(FileFormat::Csv, s);
            let _ = run(FileFormat::PlainText, s);
        }
    }
}
