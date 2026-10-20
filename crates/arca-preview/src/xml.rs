//! A lean, hardened XML pull tokenizer for the content extractors.
//!
//! The scanner in `arca-xray` runs every part through [`arca_container::xml::SecureXmlReader`],
//! which validates a lot per event and costs thousands of interpreter fuel units for each of them
//! once compiled to WebAssembly. The extractors only need to *read text and a few attributes* of
//! parts the scanner has already vetted, so they use this tokenizer instead: one pass over the
//! bytes, no allocation per event, the same hard limits.
//!
//! What it enforces, and why:
//!
//! * `DOCTYPE` is refused — there are no DTDs, no external entities, no entity expansion.
//! * Only the five predefined entities and numeric character references resolve.
//! * Nesting depth, attribute count, name length and event count are bounded.
//! * End tags must match, exactly one root element, nothing but whitespace around it.
//!
//! What it does not do: namespace resolution (names are compared by local part, as the scanner
//! does), validation of the character set of names beyond a basic sanity check, or duplicate
//! attribute detection (the scanner already rejects those documents).

use std::borrow::Cow;

pub(crate) use arca_container::xml::{XmlError, XmlLimits};

/// A start or empty-element tag.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tag<'a> {
    /// Qualified name as written.
    pub(crate) name: &'a str,
    /// The raw attribute area (between the name and `>` / `/>`), already validated.
    attrs: &'a str,
}

impl<'a> Tag<'a> {
    /// Local part of the name (text after the last `:`).
    pub(crate) fn local(&self) -> &'a str {
        local_part(self.name)
    }

    /// Value of the first attribute whose local name is `local`, entity-decoded.
    pub(crate) fn attr(&self, local: &str) -> Option<Cow<'a, str>> {
        self.attrs().find(|(n, _)| local_part(n) == local).and_then(|(_, v)| unescape(v).ok())
    }

    /// Value of the attribute with exactly this qualified name.
    pub(crate) fn attr_q(&self, qname: &str) -> Option<Cow<'a, str>> {
        self.attrs().find(|(n, _)| *n == qname).and_then(|(_, v)| unescape(v).ok())
    }

    /// `true` if the attribute is present and its value is not an "off" literal.
    ///
    /// OOXML toggles (`<w:b/>`, `<w:b w:val="1"/>`, `<w:b w:val="false"/>`) all go through this.
    pub(crate) fn flag(&self, local: &str) -> bool {
        match self.attr(local) {
            None => true,
            Some(v) => !matches!(v.trim(), "0" | "false" | "off" | "none"),
        }
    }

    /// Iterator over `(qualified name, raw value)` pairs.
    pub(crate) fn attrs(&self) -> Attrs<'a> {
        Attrs { rest: self.attrs }
    }
}

/// Attribute iterator; the area was validated when the tag was tokenized.
#[derive(Debug, Clone)]
pub(crate) struct Attrs<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Attrs<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.rest.as_bytes();
        let start = skip_ws(bytes, 0);
        if start >= bytes.len() {
            return None;
        }
        let eq = bytes.iter().skip(start).position(|&b| b == b'=')? + start;
        let name = self.rest.get(start..eq)?.trim_end();
        let q = skip_ws(bytes, eq + 1);
        let quote = *bytes.get(q)?;
        let close = bytes.iter().skip(q + 1).position(|&b| b == quote)? + q + 1;
        let value = self.rest.get(q + 1..close)?;
        self.rest = self.rest.get(close + 1..).unwrap_or("");
        Some((name, value))
    }
}

/// One token.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Ev<'a> {
    /// `<name ...>`
    Start(Tag<'a>),
    /// `<name .../>`
    Empty(Tag<'a>),
    /// `</name>` (qualified name as written).
    End(&'a str),
    /// Character data between tags, **not yet entity-decoded** (see [`unescape`]).
    Text(&'a str),
    /// A CDATA section's content (never entity-decoded).
    CData(&'a str),
}

/// Pull tokenizer over one document held in memory.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    src: &'a str,
    pos: usize,
    stack: Vec<&'a str>,
    pending_end: Option<&'a str>,
    limits: XmlLimits,
    events: u64,
    seen_root: bool,
    root_closed: bool,
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

#[inline]
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(|&b| is_ws(b)) {
        i += 1;
    }
    i
}

#[inline]
fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

#[inline]
fn is_name_char(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-' || b == b'.'
}

/// Text after the last `:`.
pub(crate) fn local_part(name: &str) -> &str {
    name.bytes().rposition(|b| b == b':').and_then(|i| name.get(i + 1..)).unwrap_or(name)
}

fn find_from(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes.get(from..)?.iter().position(|&b| b == needle).map(|i| i + from)
}

fn find_seq(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut at = from;
    while let Some(i) = find_from(bytes, at, first) {
        if bytes.get(i..i + needle.len()) == Some(needle) {
            return Some(i);
        }
        at = i + 1;
    }
    None
}

fn malformed(why: &str) -> XmlError {
    XmlError::Malformed(why.to_owned())
}

impl<'a> Reader<'a> {
    /// Creates a tokenizer over `src`.
    ///
    /// # Errors
    /// The input exceeds [`XmlLimits::max_input_bytes`].
    pub(crate) fn new(src: &'a str, limits: &XmlLimits) -> Result<Self, XmlError> {
        if src.len() > limits.max_input_bytes {
            return Err(XmlError::InputTooLarge(limits.max_input_bytes));
        }
        Ok(Self {
            src,
            pos: 0,
            stack: Vec::new(),
            pending_end: None,
            limits: *limits,
            events: 0,
            seen_root: false,
            root_closed: false,
        })
    }

    /// Number of currently open elements.
    pub(crate) fn depth(&self) -> usize {
        self.stack.len()
    }

    fn count_event(&mut self) -> Result<(), XmlError> {
        self.events = self.events.saturating_add(1);
        if self.events > self.limits.max_events {
            return Err(XmlError::TooManyEvents(self.limits.max_events));
        }
        Ok(())
    }

    /// The next token, `Ok(None)` at the end of a complete document.
    ///
    /// # Errors
    /// Any policy violation or well-formedness error.
    pub(crate) fn next(&mut self) -> Result<Option<Ev<'a>>, XmlError> {
        if let Some(name) = self.pending_end.take() {
            self.count_event()?;
            return Ok(Some(Ev::End(name)));
        }
        let bytes = self.src.as_bytes();
        loop {
            let Some(&first) = bytes.get(self.pos) else {
                if !self.stack.is_empty() {
                    return Err(malformed("unexpected end of document"));
                }
                if !self.seen_root {
                    return Err(XmlError::NoRoot);
                }
                return Ok(None);
            };
            if first != b'<' {
                let end = find_from(bytes, self.pos, b'<').unwrap_or(bytes.len());
                let raw = self.src.get(self.pos..end).ok_or_else(|| malformed("bad text span"))?;
                self.pos = end;
                if self.stack.is_empty() {
                    if raw.bytes().all(is_ws) {
                        continue;
                    }
                    return Err(if self.root_closed {
                        XmlError::ContentAfterRoot
                    } else {
                        malformed("text before the root element")
                    });
                }
                self.count_event()?;
                return Ok(Some(Ev::Text(raw)));
            }
            match bytes.get(self.pos + 1).copied() {
                Some(b'/') => return self.end_tag(bytes).map(Some),
                Some(b'!') => {
                    if let Some(ev) = self.bang(bytes)? {
                        return Ok(Some(ev));
                    }
                }
                Some(b'?') => {
                    let end = find_seq(bytes, self.pos + 2, b"?>")
                        .ok_or_else(|| malformed("unterminated processing instruction"))?;
                    self.pos = end + 2;
                }
                Some(_) => return self.start_tag(bytes).map(Some),
                None => return Err(malformed("'<' at the end of the document")),
            }
        }
    }

    fn end_tag(&mut self, bytes: &[u8]) -> Result<Ev<'a>, XmlError> {
        let close = find_from(bytes, self.pos + 2, b'>')
            .ok_or_else(|| malformed("unterminated end tag"))?;
        let name = self
            .src
            .get(self.pos + 2..close)
            .ok_or_else(|| malformed("bad end tag span"))?
            .trim_end_matches(is_xml_ws);
        let open = self.stack.pop().ok_or_else(|| malformed("end tag without a start tag"))?;
        if open != name {
            return Err(malformed("end tag does not match its start tag"));
        }
        self.pos = close + 1;
        if self.stack.is_empty() {
            self.root_closed = true;
        }
        self.count_event()?;
        Ok(Ev::End(name))
    }

    /// Comments are skipped (`Ok(None)`), CDATA is returned, DOCTYPE is refused.
    fn bang(&mut self, bytes: &[u8]) -> Result<Option<Ev<'a>>, XmlError> {
        let rest = bytes.get(self.pos + 2..).unwrap_or_default();
        if rest.starts_with(b"--") {
            let end = find_seq(bytes, self.pos + 4, b"-->")
                .ok_or_else(|| malformed("unterminated comment"))?;
            self.pos = end + 3;
            return Ok(None);
        }
        if rest.starts_with(b"[CDATA[") {
            if self.stack.is_empty() {
                return Err(malformed("CDATA outside the root element"));
            }
            let from = self.pos + 9;
            let end = find_seq(bytes, from, b"]]>")
                .ok_or_else(|| malformed("unterminated CDATA section"))?;
            let text = self.src.get(from..end).ok_or_else(|| malformed("bad CDATA span"))?;
            self.pos = end + 3;
            self.count_event()?;
            return Ok(Some(Ev::CData(text)));
        }
        if rest.get(..7).is_some_and(|h| h.eq_ignore_ascii_case(b"DOCTYPE")) {
            return Err(XmlError::DoctypeForbidden);
        }
        Err(malformed("unsupported '<!' construct"))
    }

    fn start_tag(&mut self, bytes: &[u8]) -> Result<Ev<'a>, XmlError> {
        let name_start = self.pos + 1;
        if !bytes.get(name_start).copied().is_some_and(is_name_start) {
            return Err(malformed("element name expected after '<'"));
        }
        let mut i = name_start;
        while let Some(&b) = bytes.get(i) {
            if is_name_char(b) {
                i += 1;
            } else {
                break;
            }
        }
        if i - name_start > self.limits.max_name_len {
            return Err(XmlError::NameTooLong);
        }
        let name = self.src.get(name_start..i).ok_or_else(|| malformed("bad name span"))?;

        // Attribute area: validated here, parsed lazily by `Tag::attrs`.
        let attr_start = i;
        let mut count = 0usize;
        let (attr_end, empty) = loop {
            i = skip_ws(bytes, i);
            match bytes.get(i).copied() {
                None => return Err(malformed("unterminated start tag")),
                Some(b'>') => break (i, false),
                Some(b'/') => {
                    if bytes.get(i + 1) == Some(&b'>') {
                        break (i, true);
                    }
                    return Err(malformed("stray '/' in a start tag"));
                }
                Some(b) if is_name_start(b) => {
                    let n0 = i;
                    while bytes.get(i).copied().is_some_and(is_name_char) {
                        i += 1;
                    }
                    if i - n0 > self.limits.max_name_len {
                        return Err(XmlError::NameTooLong);
                    }
                    i = skip_ws(bytes, i);
                    if bytes.get(i) != Some(&b'=') {
                        return Err(malformed("attribute without a value"));
                    }
                    i = skip_ws(bytes, i + 1);
                    let quote = match bytes.get(i).copied() {
                        Some(q @ (b'"' | b'\'')) => q,
                        _ => return Err(malformed("unquoted attribute value")),
                    };
                    let close = find_from(bytes, i + 1, quote)
                        .ok_or_else(|| malformed("unterminated attribute value"))?;
                    if bytes.get(i + 1..close).is_some_and(|v| v.contains(&b'<')) {
                        return Err(malformed("'<' in an attribute value"));
                    }
                    i = close + 1;
                    count += 1;
                    if count > self.limits.max_attributes {
                        return Err(XmlError::TooManyAttributes(self.limits.max_attributes));
                    }
                    // Whitespace between attributes is required by XML; being lenient here only
                    // widens what is *accepted*, never what is executed.
                }
                Some(_) => return Err(malformed("unexpected character in a start tag")),
            }
        };
        let attrs = self.src.get(attr_start..attr_end).ok_or_else(|| malformed("bad attr span"))?;
        let tag = Tag { name, attrs };
        self.pos = if empty { attr_end + 2 } else { attr_end + 1 };

        if self.stack.is_empty() {
            if self.root_closed || self.seen_root {
                return Err(XmlError::ContentAfterRoot);
            }
            self.seen_root = true;
        }
        if self.stack.len() + 1 > self.limits.max_depth {
            return Err(XmlError::DepthExceeded(self.limits.max_depth));
        }
        self.count_event()?;
        if empty {
            if self.stack.is_empty() {
                self.root_closed = true;
            }
            self.pending_end = Some(name);
            Ok(Ev::Empty(tag))
        } else {
            self.stack.push(name);
            Ok(Ev::Start(tag))
        }
    }
}

/// What a [`Handler`] wants done with the children of an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Descend normally.
    Continue,
    /// Ignore everything inside this element.
    SkipChildren,
}

/// Callbacks for [`drive`].
pub(crate) trait Handler {
    /// A start tag (`empty` for `<x/>`; the matching [`Handler::end`] still follows).
    fn start(&mut self, tag: &Tag<'_>, empty: bool) -> Flow;
    /// An end tag, by local name.
    fn end(&mut self, local: &str);
    /// Character data (`raw` is not entity-decoded unless `cdata` is false and the
    /// handler calls [`unescape`]; CDATA content is never decoded).
    ///
    /// # Errors
    /// A malformed entity reference aborts the part.
    fn text(&mut self, raw: &str, cdata: bool) -> Result<(), XmlError>;
    /// `true` once the handler wants no more events (a budget is exhausted).
    fn done(&self) -> bool {
        false
    }
}

/// Runs `handler` over a whole document.
///
/// Elements for which the handler returns [`Flow::SkipChildren`] are skipped
/// in a single pass without calling the handler for their content.
///
/// # Errors
/// Well-formedness or policy violations; events already delivered stay delivered.
pub(crate) fn drive(
    src: &str,
    limits: &XmlLimits,
    handler: &mut impl Handler,
) -> Result<(), XmlError> {
    let mut rd = Reader::new(src, limits)?;
    let mut skip_to: Option<(usize, &str)> = None;
    while let Some(ev) = rd.next()? {
        if let Some((depth, name)) = skip_to {
            if matches!(ev, Ev::End(_)) && rd.depth() < depth {
                skip_to = None;
                handler.end(local_part(name));
            }
            continue;
        }
        match ev {
            Ev::Start(t) => {
                if handler.start(&t, false) == Flow::SkipChildren {
                    skip_to = Some((rd.depth(), t.name));
                }
            }
            Ev::Empty(t) => {
                let _ = handler.start(&t, true);
            }
            Ev::End(name) => handler.end(local_part(name)),
            Ev::Text(raw) => handler.text(raw, false)?,
            Ev::CData(raw) => handler.text(raw, true)?,
        }
        if handler.done() {
            break;
        }
    }
    Ok(())
}

fn is_xml_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

/// Resolves the predefined entities and numeric character references in `raw`.
///
/// # Errors
/// Any other entity name, a malformed reference, or a character XML 1.0 does not allow.
pub(crate) fn unescape(raw: &str) -> Result<Cow<'_, str>, XmlError> {
    if !raw.as_bytes().contains(&b'&') {
        return Ok(Cow::Borrowed(raw));
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i + 1..).unwrap_or_default();
        let end = after.find(';').ok_or_else(|| malformed("unterminated entity reference"))?;
        let name = after.get(..end).unwrap_or_default();
        match name {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = if let Some(hex) = name.strip_prefix("#x").or(name.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok().filter(|_| hex.len() <= 8)
                } else if let Some(dec) = name.strip_prefix('#') {
                    dec.parse::<u32>().ok().filter(|_| dec.len() <= 10)
                } else {
                    return Err(XmlError::UndefinedEntity(
                        arca_container::text::display_safe(name, 64).to_string(),
                    ));
                };
                let c = code
                    .and_then(char::from_u32)
                    .filter(|&c| xml_char(c))
                    .ok_or_else(|| malformed("invalid character reference"))?;
                out.push(c);
            }
        }
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(Cow::Owned(out))
}

/// Decodes raw part bytes (UTF-8 or BOM-marked UTF-16) to text.
///
/// # Errors
/// Invalid encodings and oversized input.
pub(crate) fn decode<'a>(bytes: &'a [u8], limits: &XmlLimits) -> Result<Cow<'a, str>, XmlError> {
    arca_container::xml::decode_xml_bytes(bytes, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arca_container::xml::SecureXmlReader;

    fn limits() -> XmlLimits {
        XmlLimits::default()
    }

    /// Flattens a document into comparable lines.
    fn lite(src: &str) -> Result<Vec<String>, XmlError> {
        let mut r = Reader::new(src, &limits())?;
        let mut out = Vec::new();
        while let Some(ev) = r.next()? {
            out.push(match ev {
                Ev::Start(t) => format!("S {}", t.local()),
                Ev::Empty(t) => format!("E {}", t.local()),
                Ev::End(n) => format!("X {}", local_part(n)),
                Ev::Text(t) => format!("T {}", unescape(t)?),
                Ev::CData(t) => format!("T {t}"),
            });
        }
        Ok(out)
    }

    #[test]
    fn tokenizes_a_small_document() {
        let got =
            lite(r#"<?xml version="1.0"?><!-- c --><a x="1"><b/><c>t&amp;u</c></a>"#).unwrap();
        // Empty elements are reported as `Empty` followed by their `End`.
        assert_eq!(got, ["S a", "E b", "X b", "S c", "T t&u", "X c", "X a"]);
    }

    #[test]
    fn attributes_are_found_by_local_name_and_decoded() {
        let src = r#"<w:p w:val="a&lt;b" xml:space='preserve' data-x = "5"/>"#;
        let mut r = Reader::new(src, &limits()).unwrap();
        let Some(Ev::Empty(t)) = r.next().unwrap() else { panic!("no tag") };
        assert_eq!(t.local(), "p");
        assert_eq!(t.attr("val").as_deref(), Some("a<b"));
        assert_eq!(t.attr("space").as_deref(), Some("preserve"));
        assert_eq!(t.attr_q("w:val").as_deref(), Some("a<b"));
        assert_eq!(t.attr("data-x").as_deref(), Some("5"));
        assert_eq!(t.attr("missing"), None);
        assert_eq!(t.attrs().count(), 3);
    }

    #[test]
    fn gt_inside_attribute_values_does_not_end_the_tag() {
        let got = lite(r#"<a title="x>y" other='>'>t</a>"#).unwrap();
        assert_eq!(got, ["S a", "T t", "X a"]);
    }

    #[test]
    fn flag_semantics() {
        let src = r#"<r><b/><b val="0"/><b val="false"/><b val="1"/><b val="none"/></r>"#;
        let mut r = Reader::new(src, &limits()).unwrap();
        let mut flags = Vec::new();
        while let Some(ev) = r.next().unwrap() {
            if let Ev::Empty(t) = ev {
                flags.push(t.flag("val"));
            }
        }
        assert_eq!(flags, [true, false, false, true, false]);
    }

    #[test]
    fn cdata_is_not_decoded() {
        let got = lite("<a><![CDATA[x < y &amp; z]]></a>").unwrap();
        assert_eq!(got, ["S a", "T x < y &amp; z", "X a"]);
    }

    #[test]
    fn doctype_and_custom_entities_are_refused() {
        let xxe = r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/passwd">]><d>&x;</d>"#;
        assert_eq!(lite(xxe), Err(XmlError::DoctypeForbidden));
        assert_eq!(lite("<!doctype a><a/>"), Err(XmlError::DoctypeForbidden));
        assert!(matches!(lite("<a>&x;</a>"), Err(XmlError::UndefinedEntity(_))));
        // The classic expansion attack never gets to expand anything.
        let laughs = r#"<!DOCTYPE l [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;&a;">]><l>&b;</l>"#;
        assert_eq!(lite(laughs), Err(XmlError::DoctypeForbidden));
    }

    #[test]
    fn character_references() {
        assert_eq!(unescape("&#65;&#x42;&#X43;").unwrap(), "ABC");
        assert_eq!(unescape("&#x1F600;").unwrap(), "\u{1F600}");
        for bad in
            ["&#0;", "&#xD800;", "&#x110000;", "&#;", "&#xZZ;", "&amp", "&#1;", "&#99999999999;"]
        {
            assert!(unescape(bad).is_err(), "{bad}");
        }
        assert!(matches!(unescape("&bogus;"), Err(XmlError::UndefinedEntity(_))));
    }

    #[test]
    fn structure_errors() {
        for bad in [
            "",
            "   ",
            "<a>",
            "<a></b>",
            "</a>",
            "<a/><b/>",
            "<a/>text",
            "text<a/>",
            "<a b></a>",
            "<a b=c></a>",
            r#"<a b="x></a>"#,
            r#"<a b="<"></a>"#,
            "<a><!-- unterminated</a>",
            "<a><![CDATA[x</a>",
            "<a><?pi</a>",
            "<1a/>",
            "<a / >",
            "<a><!ELEMENT x></a>",
            "<![CDATA[x]]><a/>",
        ] {
            assert!(lite(bad).is_err(), "should fail: {bad:?}");
        }
    }

    #[test]
    fn limits_are_enforced() {
        let deep = format!("{}{}", "<a>".repeat(65), "</a>".repeat(65));
        assert_eq!(lite(&deep), Err(XmlError::DepthExceeded(64)));
        let ok = format!("{}{}", "<a>".repeat(64), "</a>".repeat(64));
        assert!(lite(&ok).is_ok());

        let many = XmlLimits { max_attributes: 3, ..limits() };
        let src = r#"<a a="1" b="2" c="3" d="4"/>"#;
        assert_eq!(
            Reader::new(src, &many).unwrap().next().unwrap_err(),
            XmlError::TooManyAttributes(3)
        );

        let few = XmlLimits { max_events: 4, ..limits() };
        let mut r = Reader::new("<a><b/><b/><b/></a>", &few).unwrap();
        let mut res = Ok(None);
        for _ in 0..8 {
            res = r.next();
            if res.is_err() {
                break;
            }
        }
        assert_eq!(res.unwrap_err(), XmlError::TooManyEvents(4));

        let long = format!("<{}/>", "n".repeat(2000));
        assert_eq!(lite(&long), Err(XmlError::NameTooLong));

        let tiny = XmlLimits { max_input_bytes: 5, ..limits() };
        assert!(matches!(Reader::new("<a></a>", &tiny), Err(XmlError::InputTooLarge(5))));
    }

    #[test]
    fn bom_and_utf16_are_decoded_by_the_shared_helper() {
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "<a>é</a>".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        let text = decode(&utf16, &limits()).unwrap();
        assert_eq!(lite(&text).unwrap(), ["S a", "T é", "X a"]);
        assert!(decode(&[0xFF, 0xFF, 0xFF], &limits()).is_err());
    }

    /// The lean tokenizer must agree with the hardened reference reader on documents both accept.
    #[test]
    fn agrees_with_the_secure_reader() {
        let docs = [
            r#"<a xmlns:x="u"><x:b k="v">hello &lt;world&gt;</x:b><c/><d><e>1</e><e>2</e></d></a>"#,
            "<root>\n  <i>a</i>\n  <i>b &#x26; c</i>\n</root>",
            r#"<p><r><t>one</t></r><r><t xml:space="preserve"> two </t></r></p>"#,
            "<a><![CDATA[<raw>]]><!-- skip --><?pi data?>tail</a>",
        ];
        for doc in docs {
            let mut secure = SecureXmlReader::new(doc, limits()).unwrap();
            let mut reference = Vec::new();
            while let Some(item) = secure.next_item().unwrap() {
                use quick_xml::events::Event;
                match item.event {
                    Event::Start(e) => {
                        reference.push(format!("S {}", arca_container::xml::local_name(&e)));
                    }
                    Event::Empty(e) => {
                        let n = arca_container::xml::local_name(&e).to_owned();
                        reference.push(format!("E {n}"));
                        reference.push(format!("X {n}"));
                    }
                    Event::End(e) => {
                        let name = e.name();
                        let full: &str = name.into_inner();
                        reference.push(format!("X {}", local_part(full)));
                    }
                    other => {
                        let mut t = String::new();
                        arca_container::xml::push_text(&other, &mut t);
                        if !t.is_empty() {
                            match reference.last_mut() {
                                Some(last) if last.starts_with("T ") => last.push_str(&t),
                                _ => reference.push(format!("T {t}")),
                            }
                        }
                    }
                }
            }
            // Adjacent text-like tokens are merged on the reference side; do the same here.
            let mut mine: Vec<String> = Vec::new();
            for line in lite(doc).unwrap() {
                match (mine.last_mut(), line.strip_prefix("T ")) {
                    (Some(last), Some(rest)) if last.starts_with("T ") => last.push_str(rest),
                    _ => mine.push(line),
                }
            }
            assert_eq!(mine, reference, "{doc}");
        }
    }

    #[test]
    fn mutated_documents_never_panic() {
        let base = r#"<?xml version="1.0"?><w:document xmlns:w="u"><w:body><w:p a="1" b='2'><w:r><w:t>héllo &amp; bye</w:t></w:r><![CDATA[x]]><!--c--></w:p></w:body></w:document>"#;
        let bytes = base.as_bytes();
        for i in 0..bytes.len() {
            for mask in [0x01u8, 0x20, 0x80, 0xFF] {
                let mut m = bytes.to_vec();
                if let Some(b) = m.get_mut(i) {
                    *b ^= mask;
                }
                if let Ok(s) = std::str::from_utf8(&m) {
                    let _ = lite(s);
                }
            }
            if let Ok(s) = std::str::from_utf8(bytes.get(..i).unwrap_or_default()) {
                let _ = lite(s);
            }
        }
    }
}
