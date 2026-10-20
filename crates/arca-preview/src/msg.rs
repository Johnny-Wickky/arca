//! Outlook messages (`.msg`): headers, body, links and the list of attachments.
//!
//! A `.msg` is a compound file whose streams are named after MAPI properties
//! (`__substg1.0_<tag><type>`). Only text is read:
//!
//! * the **headers** (subject, sender, recipients, date);
//! * the **body**: plain text if present, else the HTML body reduced to text (scripts, styles
//!   and comments dropped, never rendered), else the compressed RTF body;
//! * **links** found in the HTML body, shown as display-only targets;
//! * the **attachments**: names, sizes and kinds are listed; attachments are never opened,
//!   unpacked or previewed.

use arca_container::bytes::{u16_le, u32_le};
use arca_container::cfb::CompoundFile;
use arca_container::text::{CodePage, decode_codepage, utf16le_until_nul};

use crate::PreviewError;
use crate::model::{NoticeKind, ParaStyle, SectionKind, style};
use crate::sink::{ParaBuilder, Sink};

/// Recipients and attachments listed.
const MAX_ITEMS: usize = 4_096;
/// Bytes of an HTML or RTF body read.
const MAX_BODY: usize = 16 << 20;
/// Links listed.
const MAX_LINKS: usize = 2_048;

const PT_UNICODE: u16 = 0x001F;
const PT_ANSI: u16 = 0x001E;
const PT_BINARY: u16 = 0x0102;
const PT_SYSTIME: u16 = 0x0040;

mod tag {
    pub(super) const SUBJECT: u16 = 0x0037;
    pub(super) const SENDER_NAME: u16 = 0x0C1A;
    pub(super) const SENDER_EMAIL: u16 = 0x0C1F;
    pub(super) const SENDER_SMTP: u16 = 0x5D01;
    pub(super) const DISPLAY_TO: u16 = 0x0E04;
    pub(super) const DISPLAY_CC: u16 = 0x0E03;
    pub(super) const DISPLAY_BCC: u16 = 0x0E02;
    pub(super) const BODY: u16 = 0x1000;
    pub(super) const RTF_COMPRESSED: u16 = 0x1009;
    pub(super) const HTML: u16 = 0x1013;
    pub(super) const SUBMIT_TIME: u16 = 0x0039;
    pub(super) const DELIVERY_TIME: u16 = 0x0E06;
    pub(super) const INTERNET_CPID: u16 = 0x3FDE;
    pub(super) const MESSAGE_CPID: u16 = 0x3FFD;
    pub(super) const DISPLAY_NAME: u16 = 0x3001;
    pub(super) const SMTP_ADDRESS: u16 = 0x39FE;
    pub(super) const EMAIL_ADDRESS: u16 = 0x3003;
    pub(super) const RECIPIENT_TYPE: u16 = 0x0C15;
    pub(super) const ATTACH_LONG_NAME: u16 = 0x3707;
    pub(super) const ATTACH_NAME: u16 = 0x3704;
    pub(super) const ATTACH_DATA: u16 = 0x3701;
    pub(super) const ATTACH_METHOD: u16 = 0x3705;
}

fn malformed(why: &str) -> PreviewError {
    PreviewError::Malformed(format!("Outlook message: {why}"))
}

/// One fixed-size property from a `__properties_version1.0` stream.
#[derive(Debug, Clone, Copy)]
struct Prop {
    id: u16,
    ty: u16,
    value: [u8; 8],
}

/// Properties stored inline: numbers, times, booleans. `header` is 32 bytes for the message
/// itself and 8 for recipients and attachments.
fn props_of(cf: &CompoundFile<'_>, storage: Option<usize>, header: usize) -> Vec<Prop> {
    let name = "__properties_version1.0";
    let idx = match storage {
        Some(s) => cf.child(s, name),
        None => cf.find(name),
    };
    let Some(data) = idx.and_then(|i| cf.read_stream(i).ok()) else { return Vec::new() };
    data.get(header..)
        .unwrap_or(&[])
        .chunks_exact(16)
        .take(1 << 16)
        .filter_map(|e| {
            let ty = u16_le(e, 0)?;
            let id = u16_le(e, 2)?;
            let mut value = [0u8; 8];
            value.copy_from_slice(e.get(8..16)?);
            Some(Prop { id, ty, value })
        })
        .collect()
}

fn int_prop(props: &[Prop], id: u16) -> Option<u32> {
    props
        .iter()
        .find(|p| p.id == id)
        .map(|p| u32::from_le_bytes([p.value[0], p.value[1], p.value[2], p.value[3]]))
}

/// The raw stream of a property, with the property type that was found.
fn raw_prop(
    cf: &CompoundFile<'_>,
    storage: Option<usize>,
    id: u16,
    types: &[u16],
) -> Option<(u16, Vec<u8>)> {
    for &ty in types {
        let name = format!("__substg1.0_{id:04X}{ty:04X}");
        let idx = match storage {
            Some(s) => cf.child(s, &name),
            None => cf.find(&name),
        };
        if let Some(i) = idx
            && let Ok(d) = cf.read_stream(i)
        {
            return Some((ty, d));
        }
    }
    None
}

fn codepage_of(cpid: Option<u32>) -> CodePage {
    match cpid {
        Some(65001) => CodePage::Utf8,
        Some(1251) => CodePage::Windows1251,
        _ => CodePage::Windows1252,
    }
}

fn text_prop(
    cf: &CompoundFile<'_>,
    storage: Option<usize>,
    id: u16,
    cp: CodePage,
) -> Option<String> {
    let (ty, data) = raw_prop(cf, storage, id, &[PT_UNICODE, PT_ANSI])?;
    let text = if ty == PT_UNICODE {
        utf16le_until_nul(&data)
    } else {
        decode_codepage(data.split(|&b| b == 0).next().unwrap_or(&data), cp)
    };
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// `YYYY-MM-DD HH:MM:SS UTC` for a Windows `FILETIME`.
fn filetime(value: [u8; 8]) -> Option<String> {
    let ticks = u64::from_le_bytes(value);
    if ticks == 0 {
        return None;
    }
    let secs = i64::try_from(ticks / 10_000_000).ok()?;
    // 1601-01-01 to 1970-01-01.
    let unix = secs.checked_sub(11_644_473_600)?;
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    // Civil date from a day count (proleptic Gregorian).
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    if !(1601..=9999).contains(&y) {
        return None;
    }
    Some(format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    ))
}

// ---------------------------------------------------------------------------------------------
// Compressed RTF (RTF-LZ, "LZFu")

/// The dictionary that compressed RTF starts with.
const LZ_DICT: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

/// Decompresses a `PR_RTF_COMPRESSED` value, producing at most `limit` bytes.
fn decompress_rtf(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let raw_size = usize::try_from(u32_le(data, 4)?).ok()?;
    let kind = u32_le(data, 8)?;
    let body = data.get(16..)?;
    if kind == 0x414C_454D {
        // Stored uncompressed.
        return Some(body.get(..raw_size.min(body.len()).min(limit))?.to_vec());
    }
    if kind != 0x7546_5A4C {
        return None;
    }
    let want = raw_size.min(limit);
    let mut dict = [0u8; 4_096];
    dict.get_mut(..LZ_DICT.len())?.copy_from_slice(LZ_DICT);
    let mut write = LZ_DICT.len();
    let mut out: Vec<u8> = Vec::with_capacity(want.min(1 << 20));
    let mut pos = 0usize;
    'outer: while out.len() < want {
        let control = *body.get(pos)?;
        pos += 1;
        for bit in 0..8 {
            if out.len() >= want {
                break 'outer;
            }
            if control >> bit & 1 == 0 {
                let Some(&b) = body.get(pos) else { break 'outer };
                pos += 1;
                out.push(b);
                *dict.get_mut(write)? = b;
                write = (write + 1) & 0xFFF;
            } else {
                let (Some(&hi), Some(&lo)) = (body.get(pos), body.get(pos + 1)) else {
                    break 'outer;
                };
                pos += 2;
                let offset = (usize::from(hi) << 4) | (usize::from(lo) >> 4);
                let len = usize::from(lo & 0xF) + 2;
                if offset == write {
                    break 'outer;
                }
                let mut from = offset;
                for _ in 0..len {
                    let b = *dict.get(from)?;
                    out.push(b);
                    *dict.get_mut(write)? = b;
                    write = (write + 1) & 0xFFF;
                    from = (from + 1) & 0xFFF;
                    if out.len() >= want {
                        break 'outer;
                    }
                }
            }
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// HTML reduced to text

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{A0}'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Replaces character references (`&amp;`, `&#65;`, …) in an attribute value.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or(""));
        let tail = rest.get(at + 1..).unwrap_or("");
        let semi = tail.char_indices().take(10).find(|&(_, k)| k == ';').map(|(p, _)| p);
        match semi.and_then(|p| tail.get(..p).and_then(entity).map(|c| (p, c))) {
            Some((p, c)) => {
                out.push(c);
                rest = tail.get(p + 1..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in the inside of a tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower.get(from..)?.find(name) {
        let at = from + rel;
        let before_ok = lower
            .get(..at)
            .and_then(|s| s.chars().next_back())
            .is_none_or(|c| c.is_ascii_whitespace() || c == '"' || c == '\'');
        let rest = tag.get(at + name.len()..)?.trim_start();
        if before_ok && let Some(value) = rest.strip_prefix('=') {
            let value = value.trim_start();
            return Some(match value.chars().next() {
                Some(q @ ('"' | '\'')) => value.get(1..)?.split(q).next()?.to_owned(),
                _ => value.split(|c: char| c.is_ascii_whitespace() || c == '>').next()?.to_owned(),
            });
        }
        from = at + name.len();
    }
    None
}

/// Reduces HTML to text. Nothing is rendered or fetched; links are collected, not followed.
fn html_text(html: &str, links: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut chars = html.char_indices().peekable();
    let mut skip_until: Option<&'static str> = None;
    let mut pending_space = false;
    while let Some((i, c)) = chars.next() {
        if let Some(end) = skip_until {
            if c == '<'
                && html.get(i + 1..).is_some_and(|r| {
                    r.len() >= end.len()
                        && r.as_bytes()
                            .get(..end.len())
                            .is_some_and(|b| b.eq_ignore_ascii_case(end.as_bytes()))
                })
            {
                skip_until = None;
                for (_, k) in chars.by_ref() {
                    if k == '>' {
                        break;
                    }
                }
            }
            continue;
        }
        match c {
            '<' => {
                if html.get(i..).is_some_and(|r| r.starts_with("<!--")) {
                    let end = html
                        .get(i + 4..)
                        .and_then(|r| r.find("-->"))
                        .map_or(html.len(), |p| i + 4 + p + 3);
                    while chars.peek().is_some_and(|&(j, _)| j < end) {
                        chars.next();
                    }
                    continue;
                }
                let end =
                    html.get(i + 1..).and_then(|r| r.find('>')).map_or(html.len(), |p| i + 1 + p);
                let inside = html.get(i + 1..end).unwrap_or("");
                while chars.peek().is_some_and(|&(j, _)| j <= end) {
                    chars.next();
                }
                let name: String = inside
                    .trim_start_matches('/')
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_lowercase();
                match name.as_str() {
                    "script" if !inside.starts_with('/') => skip_until = Some("/script"),
                    "style" if !inside.starts_with('/') => skip_until = Some("/style"),
                    "br" | "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "table" | "blockquote" | "pre" | "hr" => {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        pending_space = false;
                    }
                    "td" | "th" if !inside.starts_with('/') => pending_space = true,
                    "a" if !inside.starts_with('/') => {
                        if let Some(h) = attribute(inside, "href")
                            && links.len() < MAX_LINKS
                        {
                            let h = unescape(h.trim());
                            if !h.is_empty() && !links.contains(&h) {
                                links.push(h);
                            }
                        }
                    }
                    _ => {}
                }
            }
            '&' => {
                let tail = html.get(i + 1..).unwrap_or("");
                let semi = tail.char_indices().take(10).find(|&(_, k)| k == ';').map(|(p, _)| p);
                if let Some(p) = semi
                    && let Some(ch) = tail.get(..p).and_then(entity)
                {
                    if pending_space && !out.ends_with(['\n', ' ']) {
                        out.push(' ');
                    }
                    pending_space = false;
                    out.push(ch);
                    while chars.peek().is_some_and(|&(j, _)| j <= i + p + 1) {
                        chars.next();
                    }
                } else {
                    out.push('&');
                }
            }
            c if c.is_whitespace() => pending_space = true,
            c => {
                if pending_space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
                    out.push(' ');
                }
                pending_space = false;
                out.push(c);
            }
        }
        if out.len() > MAX_BODY {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------

fn header_line(sink: &mut Sink<'_>, sec: crate::sink::SectionId, label: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    let mut b = ParaBuilder::new();
    b.text(&format!("{label}: "), style::BOLD, "");
    b.text(value, 0, "");
    sink.para(sec, b.take(ParaStyle::Body));
}

fn address(name: Option<&str>, email: Option<&str>) -> String {
    match (name, email) {
        (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
        (Some(n), _) => n.to_owned(),
        (None, Some(e)) => e.to_owned(),
        (None, None) => String::new(),
    }
}

/// Extracts an Outlook message into `sink`.
pub(crate) fn extract(cf: &CompoundFile<'_>, sink: &mut Sink<'_>) -> Result<(), PreviewError> {
    if cf.find("__properties_version1.0").is_none() {
        return Err(malformed("the property stream is missing"));
    }
    let props = props_of(cf, None, 32);
    let cp = codepage_of(
        int_prop(&props, tag::INTERNET_CPID).or_else(|| int_prop(&props, tag::MESSAGE_CPID)),
    );
    let get = |id: u16| text_prop(cf, None, id, cp);

    // Recipients and attachments live in numbered sub-storages of the root.
    let mut recipients: Vec<(u32, String)> = Vec::new();
    let mut attachments: Vec<usize> = Vec::new();
    for (i, e) in cf.entries().iter().enumerate() {
        if e.parent != Some(0) {
            continue;
        }
        if e.name.starts_with("__recip_version1.0_") && recipients.len() < MAX_ITEMS {
            let rp = props_of(cf, Some(i), 8);
            let name = text_prop(cf, Some(i), tag::DISPLAY_NAME, cp);
            let mail = text_prop(cf, Some(i), tag::SMTP_ADDRESS, cp)
                .or_else(|| text_prop(cf, Some(i), tag::EMAIL_ADDRESS, cp));
            let who = address(name.as_deref(), mail.as_deref());
            if !who.is_empty() {
                recipients.push((int_prop(&rp, tag::RECIPIENT_TYPE).unwrap_or(1), who));
            }
        } else if e.name.starts_with("__attach_version1.0_") && attachments.len() < MAX_ITEMS {
            attachments.push(i);
        }
    }

    let Some(sec) = sink.open_section(SectionKind::Body, "", false) else { return Ok(()) };
    if let Some(subject) = get(tag::SUBJECT) {
        let mut b = ParaBuilder::new();
        b.text(&subject, 0, "");
        sink.para(sec, b.take(ParaStyle::Title));
    }
    let sender = address(
        get(tag::SENDER_NAME).as_deref(),
        get(tag::SENDER_SMTP).or_else(|| get(tag::SENDER_EMAIL)).as_deref(),
    );
    header_line(sink, sec, "From", &sender);
    for (kind, label, fallback) in
        [(1u32, "To", tag::DISPLAY_TO), (2, "Cc", tag::DISPLAY_CC), (3, "Bcc", tag::DISPLAY_BCC)]
    {
        let listed: Vec<&str> =
            recipients.iter().filter(|(k, _)| *k == kind).map(|(_, w)| w.as_str()).collect();
        let value =
            if listed.is_empty() { get(fallback).unwrap_or_default() } else { listed.join("; ") };
        header_line(sink, sec, label, &value);
    }
    let date = props
        .iter()
        .find(|p| p.id == tag::SUBMIT_TIME && p.ty == PT_SYSTIME)
        .or_else(|| props.iter().find(|p| p.id == tag::DELIVERY_TIME && p.ty == PT_SYSTIME))
        .and_then(|p| filetime(p.value));
    header_line(sink, sec, "Date", &date.unwrap_or_default());
    sink.push(sec, crate::model::Block::Rule);

    // Body: plain text, else HTML as text, else compressed RTF.
    let mut links: Vec<String> = Vec::new();
    let mut body_shown = false;
    if let Some(text) = get(tag::BODY) {
        for line in text.replace("\r\n", "\n").split('\n') {
            let mut b = ParaBuilder::new();
            b.text(line, 0, "");
            if !sink.para(sec, b.take(ParaStyle::Body)) {
                break;
            }
        }
        body_shown = true;
    }
    if let Some((ty, data)) = raw_prop(cf, None, tag::HTML, &[PT_BINARY, PT_UNICODE, PT_ANSI]) {
        let data = data.get(..MAX_BODY.min(data.len())).unwrap_or(&[]);
        let html =
            if ty == PT_UNICODE { utf16le_until_nul(data) } else { decode_codepage(data, cp) };
        let text = html_text(&html, &mut links);
        if !body_shown {
            for line in text.split('\n') {
                if line.trim().is_empty() {
                    continue;
                }
                let mut b = ParaBuilder::new();
                b.text(line.trim(), 0, "");
                if !sink.para(sec, b.take(ParaStyle::Body)) {
                    break;
                }
            }
            body_shown = !text.trim().is_empty();
        }
    }
    if !body_shown && let Some((_, data)) = raw_prop(cf, None, tag::RTF_COMPRESSED, &[PT_BINARY]) {
        match decompress_rtf(&data, MAX_BODY) {
            Some(rtf) if crate::rtf::extract(&rtf, sink).is_ok() => {}
            _ => sink.skipped("rtf body"),
        }
    }

    if !links.is_empty()
        && let Some(ls) = sink.open_section(SectionKind::Notes, "Links", false)
    {
        for l in &links {
            let mut b = ParaBuilder::new();
            b.text(l, 0, l);
            if !sink.para(ls, b.take(ParaStyle::Body)) {
                break;
            }
        }
    }

    if !attachments.is_empty()
        && let Some(asec) = sink.open_section(SectionKind::Notes, "Attachments", false)
    {
        for &a in &attachments {
            let ap = props_of(cf, Some(a), 8);
            let name = text_prop(cf, Some(a), tag::ATTACH_LONG_NAME, cp)
                .or_else(|| text_prop(cf, Some(a), tag::ATTACH_NAME, cp))
                .or_else(|| text_prop(cf, Some(a), tag::DISPLAY_NAME, cp))
                .unwrap_or_else(|| "(unnamed)".to_owned());
            let size = [PT_BINARY, 0x000D]
                .iter()
                .find_map(|&ty| {
                    cf.child(a, &format!("__substg1.0_{:04X}{ty:04X}", tag::ATTACH_DATA))
                })
                .and_then(|i| cf.entries().get(i))
                .map(|e| e.size);
            let kind = match int_prop(&ap, tag::ATTACH_METHOD) {
                Some(1) | None => "",
                Some(5) => ", embedded message",
                Some(6) => ", embedded object",
                Some(_) => ", link",
            };
            let mut b = ParaBuilder::new();
            match size {
                Some(n) => b.text(&format!("{name} ({n} bytes{kind})"), 0, ""),
                None => b.text(&format!("{name} (no data{kind})"), 0, ""),
            }
            if !sink.para(asec, b.take(ParaStyle::Body)) {
                break;
            }
        }
        sink.note(NoticeKind::Unsupported, "attachments are listed, not opened");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileFormat, Preview};
    use crate::testkit::{Lcg, fuzz_iterations, ok, paragraphs, run, section_of};
    use arca_container::cfb::writer::CfbBuilder;

    const NO_CLSID: [u8; 16] = [0; 16];

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn prop_entry(id: u16, ty: u16, value: [u8; 8]) -> Vec<u8> {
        let mut e = ty.to_le_bytes().to_vec();
        e.extend(id.to_le_bytes());
        e.extend(6u32.to_le_bytes());
        e.extend(value);
        e
    }

    fn filetime_of(unix: u64) -> [u8; 8] {
        ((unix + 11_644_473_600) * 10_000_000).to_le_bytes()
    }

    #[derive(Default)]
    struct Msg {
        root: Vec<(String, Vec<u8>)>,
        props: Vec<Vec<u8>>,
        recipients: Vec<Vec<(String, Vec<u8>)>>,
        attachments: Vec<Vec<(String, Vec<u8>)>>,
    }

    impl Msg {
        fn text(mut self, id: u16, s: &str) -> Self {
            self.root.push((format!("__substg1.0_{id:04X}001F"), utf16(s)));
            self
        }

        fn ansi(mut self, id: u16, b: &[u8]) -> Self {
            self.root.push((format!("__substg1.0_{id:04X}001E"), [b, &[0]].concat()));
            self
        }

        fn binary(mut self, id: u16, b: &[u8]) -> Self {
            self.root.push((format!("__substg1.0_{id:04X}0102"), b.to_vec()));
            self
        }

        fn prop(mut self, e: Vec<u8>) -> Self {
            self.props.push(e);
            self
        }

        fn build(&self) -> Vec<u8> {
            let mut b = CfbBuilder::new();
            let mut header = vec![0u8; 32];
            header[8..12]
                .copy_from_slice(&u32::try_from(self.recipients.len()).expect("n").to_le_bytes());
            let mut props = header;
            for e in &self.props {
                props.extend(e);
            }
            assert!(b.add_stream("__properties_version1.0", props).is_ok());
            for (n, d) in &self.root {
                assert!(b.add_stream(n, d.clone()).is_ok());
            }
            for (i, r) in self.recipients.iter().enumerate() {
                let dir = format!("__recip_version1.0_#{i:08X}");
                assert!(b.add_storage(&dir, NO_CLSID).is_ok());
                for (n, d) in r {
                    assert!(b.add_stream(&format!("{dir}/{n}"), d.clone()).is_ok());
                }
            }
            for (i, a) in self.attachments.iter().enumerate() {
                let dir = format!("__attach_version1.0_#{i:08X}");
                assert!(b.add_storage(&dir, NO_CLSID).is_ok());
                for (n, d) in a {
                    assert!(b.add_stream(&format!("{dir}/{n}"), d.clone()).is_ok());
                }
            }
            b.finish().unwrap_or_default()
        }
    }

    fn recipient(kind: u32, name: &str, mail: &str) -> Vec<(String, Vec<u8>)> {
        let mut props = vec![0u8; 8];
        props.extend(prop_entry(
            0x0C15,
            0x0003,
            [kind.to_le_bytes().as_slice(), &[0; 4]].concat().try_into().expect("8"),
        ));
        vec![
            ("__properties_version1.0".to_owned(), props),
            ("__substg1.0_3001001F".to_owned(), utf16(name)),
            ("__substg1.0_39FE001F".to_owned(), utf16(mail)),
        ]
    }

    fn attachment(name: &str, size: usize, method: u32) -> Vec<(String, Vec<u8>)> {
        let mut props = vec![0u8; 8];
        props.extend(prop_entry(
            0x3705,
            0x0003,
            [method.to_le_bytes().as_slice(), &[0; 4]].concat().try_into().expect("8"),
        ));
        vec![
            ("__properties_version1.0".to_owned(), props),
            ("__substg1.0_3707001F".to_owned(), utf16(name)),
            ("__substg1.0_37010102".to_owned(), vec![0x4D; size]),
        ]
    }

    fn texts(p: &Preview) -> Vec<String> {
        p.sections.iter().flat_map(paragraphs).map(|x| x.text()).collect()
    }

    #[test]
    fn dictionary_has_the_specified_size() {
        assert_eq!(LZ_DICT.len(), 207);
    }

    #[test]
    fn headers_body_recipients_and_attachments() {
        let mut m = Msg::default()
            .text(0x0037, "Invoice overdue")
            .text(0x0C1A, "Mallory")
            .text(0x5D01, "mallory@evil.example")
            .text(0x1000, "Hello,\r\nopen the attachment.\r\n\r\nBye")
            .prop(prop_entry(0x0039, 0x0040, filetime_of(1_709_210_096)));
        m.recipients.push(recipient(1, "Alice", "alice@example.org"));
        m.recipients.push(recipient(2, "Bob", "bob@example.org"));
        m.attachments.push(attachment("invoice.pdf.exe", 1234, 1));
        m.attachments.push(attachment("fwd.msg", 10, 5));
        let p = ok(&m.build());
        assert_eq!(p.format, FileFormat::Msg);
        let all = texts(&p);
        assert!(all.contains(&"Invoice overdue".to_owned()));
        assert!(all.contains(&"From: Mallory <mallory@evil.example>".to_owned()), "{all:?}");
        assert!(all.contains(&"To: Alice <alice@example.org>".to_owned()));
        assert!(all.contains(&"Cc: Bob <bob@example.org>".to_owned()));
        assert!(all.contains(&"Date: 2024-02-29 12:34:56 UTC".to_owned()));
        assert!(all.contains(&"open the attachment.".to_owned()));
        let att = p.sections.iter().find(|s| s.name == "Attachments").expect("attachments");
        let lines: Vec<_> = paragraphs(att).iter().map(|x| x.text()).collect();
        assert_eq!(lines, ["invoice.pdf.exe (1234 bytes)", "fwd.msg (10 bytes, embedded message)"]);
        assert!(
            p.notices
                .iter()
                .any(|n| n.kind == NoticeKind::Unsupported && n.detail.contains("attachments"))
        );
    }

    #[test]
    fn display_fields_are_used_without_recipient_storages() {
        let m = Msg::default()
            .text(0x0037, "S")
            .text(0x0E04, "Alice; Bob")
            .text(0x0E03, "Carol")
            .text(0x1000, "x");
        let all = texts(&ok(&m.build()));
        assert!(
            all.contains(&"To: Alice; Bob".to_owned()) && all.contains(&"Cc: Carol".to_owned()),
            "{all:?}"
        );
    }

    #[test]
    fn html_bodies_become_text_and_links_are_listed() {
        let html = "<html><head><style>p{color:red}</style></head><body><script>alert(1)</script><!-- hidden -->\
            <p>Dear &quot;user&quot;,</p><p>Click <a href=\"https://evil.example/login?a=1&amp;b=2\">here</a> &amp; win&nbsp;&#8364;5<br>Thanks</p>\
            <table><tr><td>A</td><td>B</td></tr></table></body></html>";
        let m = Msg::default().text(0x0037, "Win").binary(0x1013, html.as_bytes());
        let p = ok(&m.build());
        let all = texts(&p);
        assert!(all.contains(&"Dear \"user\",".to_owned()), "{all:?}");
        assert!(
            all.iter().any(|l| l.starts_with("Click here & win") && l.contains('€')),
            "{all:?}"
        );
        assert!(all.contains(&"Thanks".to_owned()));
        assert!(all.contains(&"A B".to_owned()), "{all:?}");
        assert!(
            !all.iter()
                .any(|l| l.contains("alert") || l.contains("color:red") || l.contains("hidden"))
        );
        let links = p.sections.iter().find(|s| s.name == "Links").expect("links");
        assert_eq!(paragraphs(links)[0].runs[0].href, "https://evil.example/login?a=1&b=2");
    }

    #[test]
    fn plain_text_wins_over_html_but_links_are_still_listed() {
        let m =
            Msg::default().text(0x1000, "plain body").binary(0x1013, b"<a href=x>html body</a>");
        let p = ok(&m.build());
        let all = texts(&p);
        assert!(all.contains(&"plain body".to_owned()));
        assert!(!all.iter().any(|l| l.contains("html body")));
        assert!(p.sections.iter().any(|s| s.name == "Links"));
    }

    #[test]
    fn single_byte_strings_use_the_message_code_page() {
        let m = Msg::default().ansi(0x0037, b"\xCF\xF0\xE8\xE2\xE5\xF2").prop(prop_entry(
            0x3FDE,
            0x0003,
            [0xE3, 0x04, 0, 0, 0, 0, 0, 0],
        ));
        assert!(texts(&ok(&m.build())).contains(&"Привет".to_owned()));
        let m = Msg::default().ansi(0x0037, b"caf\xE9");
        assert!(texts(&ok(&m.build())).contains(&"café".to_owned()));
    }

    #[test]
    fn file_times() {
        assert_eq!(
            filetime(filetime_of(1_609_459_200)).as_deref(),
            Some("2021-01-01 00:00:00 UTC")
        );
        assert_eq!(
            filetime(filetime_of(1_709_210_096)).as_deref(),
            Some("2024-02-29 12:34:56 UTC")
        );
        assert_eq!(filetime(filetime_of(0)).as_deref(), Some("1970-01-01 00:00:00 UTC"));
        assert_eq!(filetime([0; 8]), None);
        assert_eq!(filetime([0xFF; 8]), None);
    }

    /// Hand-made compressed RTF: literals and one reference into the initial dictionary.
    fn lzfu(literal_a: &[u8], reference: (usize, usize), literal_b: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut flags = Vec::new();
        for &b in literal_a {
            flags.push((false, vec![b]));
        }
        let (off, len) = reference;
        flags.push((
            true,
            vec![
                u8::try_from(off >> 4).expect("hi"),
                u8::try_from(((off & 0xF) << 4) | (len - 2)).expect("lo"),
            ],
        ));
        for &b in literal_b {
            flags.push((false, vec![b]));
        }
        let end = LZ_DICT.len() + literal_a.len() + len + literal_b.len();
        flags.push((
            true,
            vec![
                u8::try_from((end & 0xFFF) >> 4).expect("hi"),
                u8::try_from((end & 0xF) << 4).expect("lo"),
            ],
        ));
        for chunk in flags.chunks(8) {
            let mut control = 0u8;
            for (i, (is_ref, _)) in chunk.iter().enumerate() {
                if *is_ref {
                    control |= 1 << i;
                }
            }
            body.push(control);
            for (_, bytes) in chunk {
                body.extend(bytes);
            }
        }
        let raw = literal_a.len() + len + literal_b.len();
        let mut out = u32::try_from(body.len() + 12).expect("n").to_le_bytes().to_vec();
        out.extend(u32::try_from(raw).expect("n").to_le_bytes());
        out.extend(0x7546_5A4Cu32.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn compressed_rtf_is_decompressed() {
        // "{\\rtf1\\ansi" is the dictionary's first 11 bytes.
        let data = lzfu(b"", (0, 11), b"\\deff0 Hi there}");
        let out = decompress_rtf(&data, 1 << 20).expect("decompressed");
        assert_eq!(String::from_utf8_lossy(&out), "{\\rtf1\\ansi\\deff0 Hi there}");
        // The size limit applies.
        assert_eq!(decompress_rtf(&data, 5).map(|v| v.len()), Some(5));
        // Stored, not compressed.
        let mut stored = 0u32.to_le_bytes().to_vec();
        stored.extend(5u32.to_le_bytes());
        stored.extend(0x414C_454Du32.to_le_bytes());
        stored.extend(0u32.to_le_bytes());
        stored.extend(b"12345678");
        assert_eq!(decompress_rtf(&stored, 100).as_deref(), Some(&b"12345"[..]));
        // Garbage.
        assert!(decompress_rtf(&[1, 2, 3], 100).is_none());
        let mut bad = stored.clone();
        bad[8] = 1;
        assert!(decompress_rtf(&bad, 100).is_none());
        let mut huge = data.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let _ = decompress_rtf(&huge, 1 << 20);
    }

    #[test]
    fn a_compressed_rtf_body_is_shown() {
        let rtf = lzfu(b"", (0, 11), b"\\deff0 Body from RTF\\par}");
        let m = Msg::default().text(0x0037, "Rtf only").binary(0x1009, &rtf);
        let all = texts(&ok(&m.build()));
        assert!(all.iter().any(|l| l.contains("Body from RTF")), "{all:?}");
    }

    #[test]
    fn html_text_details() {
        let mut links = Vec::new();
        assert_eq!(
            html_text("a&lt;b&gt;&amp;c&#65;&#x42;&bogus;&", &mut links),
            "a<b>&cAB&bogus;&"
        );
        assert_eq!(html_text("one   two\n\tthree", &mut links), "one two three");
        assert_eq!(html_text("x<br/>y<p>z</p>", &mut links).trim(), "x\ny\nz");
        assert_eq!(html_text("<!-- unterminated", &mut links), "");
        assert_eq!(html_text("a<script>b", &mut links), "a");
        assert_eq!(html_text("a<b", &mut links), "a");
        assert_eq!(
            html_text(
                "<A HREF='http://x.example/'>l</A><a href=y>m</a><a name=z>n</a>",
                &mut links
            ),
            "lmn"
        );
        assert_eq!(links, ["http://x.example/", "y"]);
        assert_eq!(attribute("a href = \"v\" title=t", "href").as_deref(), Some("v"));
        assert_eq!(attribute("a data-href=\"no\"", "href"), None);
    }

    #[test]
    fn broken_messages_fail_cleanly() {
        let mut b = CfbBuilder::new();
        assert!(b.add_stream("__substg1.0_0037001F", utf16("x")).is_ok());
        assert!(matches!(
            run(&b.finish().unwrap_or_default()),
            Err(PreviewError::Malformed(_)) | Ok(_)
        ));
        // No properties stream: not detected as a message either way, but never a panic.
        let m = Msg::default().text(0x0037, "S");
        let mut data = m.build();
        data.truncate(data.len() / 2);
        let _ = run(&data);
    }

    #[test]
    fn mutated_messages_never_panic() {
        let mut m = Msg::default()
            .text(0x0037, "S")
            .text(0x1000, "body")
            .binary(0x1013, b"<p>x</p><a href=y>z</a>")
            .binary(0x1009, &lzfu(b"", (0, 11), b"}"))
            .prop(prop_entry(0x0039, 0x0040, filetime_of(1_700_000_000)));
        m.recipients.push(recipient(1, "A", "a@b"));
        m.attachments.push(attachment("f.bin", 20, 1));
        let data = m.build();
        let mut rng = Lcg::new(31);
        for i in 0..fuzz_iterations(600) {
            let mut d = data.clone();
            rng.mutate(&mut d, 1 + (i % 5));
            let _ = run(&d);
        }
        assert!(section_of(&ok(&data), SectionKind::Body).is_some());
    }
}
