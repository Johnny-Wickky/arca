//! Secure XML pull parsing on top of `quick-xml`.
//!
//! # Policy
//!
//! * **No DTD at all.** A `<!DOCTYPE ...>` declaration is a hard error. This
//!   removes external entities (XXE, SSRF via `SYSTEM "http://..."`), parameter
//!   entities and internal entity expansion ("billion laughs") in one stroke.
//!   No legitimate OOXML or ODF part carries a DTD.
//! * **Only the five predefined entities** and numeric character references
//!   are accepted; any other `&name;` is an error.
//! * **Depth ≤ 64**, attributes per element, events per document and name
//!   lengths are bounded ([`XmlLimits`]).
//! * **Exactly one root element**; markup after the root is an error.
//! * **Encoding**: UTF-8 (optionally with BOM) or UTF-16 with BOM, which is
//!   transcoded first. Anything else is refused rather than guessed: UTF-16/32
//!   without a byte order mark and EBCDIC are recognised by their first bytes, and
//!   an `encoding=` in the XML declaration must agree with the bytes.
//! * **Characters**: only the XML `Char` production. NUL and the other C0 controls
//!   (everything below U+0020 except tab, line feed and carriage return) and
//!   U+FFFE/U+FFFF are refused, so a parser that stops at NUL and one that does not
//!   cannot read two different documents out of the same bytes.
//!
//! Each event is returned together with its exact byte span in the source,
//! which the span-preserving [`rewrite`] module uses to re-emit untouched
//! markup verbatim (round-trip preservation of unknown nodes).

pub mod rewrite;

use core::fmt;
use core::ops::Range;
use std::borrow::Cow;
use std::collections::HashSet;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// Attribute names tracked on the stack by the duplicate check.
const INLINE_ATTRS: usize = 16;

/// Limits for one XML document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlLimits {
    /// Maximum element nesting depth (the specification mandates 64).
    pub max_depth: usize,
    /// Maximum number of attributes on one element.
    pub max_attributes: usize,
    /// Maximum number of events in one document.
    pub max_events: u64,
    /// Maximum length of an element or attribute name in bytes.
    pub max_name_len: usize,
    /// Maximum input size in bytes.
    pub max_input_bytes: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_attributes: 512,
            max_events: 50_000_000,
            max_name_len: 1024,
            max_input_bytes: 256 << 20,
        }
    }
}

/// Reasons an XML document is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlError {
    /// The document contains a `DOCTYPE` declaration.
    DoctypeForbidden,
    /// An entity other than the five predefined ones was referenced.
    UndefinedEntity(String),
    /// Element nesting exceeded the limit.
    DepthExceeded(usize),
    /// An element carries too many attributes.
    TooManyAttributes(usize),
    /// The event count exceeded the limit.
    TooManyEvents(u64),
    /// A name exceeded the limit.
    NameTooLong,
    /// The document has no root element.
    NoRoot,
    /// Markup follows the root element.
    ContentAfterRoot,
    /// The input is larger than allowed.
    InputTooLarge(usize),
    /// The byte encoding is not supported or invalid.
    Encoding(&'static str),
    /// The document is not well-formed.
    Malformed(String),
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DoctypeForbidden => {
                f.write_str("DOCTYPE declarations are forbidden (XXE / entity expansion)")
            }
            Self::UndefinedEntity(e) => write!(f, "undefined entity reference &{e};"),
            Self::DepthExceeded(d) => write!(f, "element nesting exceeds {d} levels"),
            Self::TooManyAttributes(n) => write!(f, "element has more than {n} attributes"),
            Self::TooManyEvents(n) => write!(f, "document exceeds {n} events"),
            Self::NameTooLong => f.write_str("name too long"),
            Self::NoRoot => f.write_str("document has no root element"),
            Self::ContentAfterRoot => f.write_str("markup after the root element"),
            Self::InputTooLarge(n) => write!(f, "document larger than {n} bytes"),
            Self::Encoding(why) => write!(f, "unsupported encoding: {why}"),
            Self::Malformed(why) => write!(f, "malformed XML: {why}"),
        }
    }
}

impl core::error::Error for XmlError {}

/// How the bytes of a document are encoded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ByteEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Decodes raw XML bytes to text according to the BOM.
///
/// Documents in UTF-16 or UTF-32 without a byte order mark (and EBCDIC ones) are refused:
/// they are read as UTF-8 by a reader that does not sniff and as something else by one that
/// does. The text must consist of XML `Char`s only and its `encoding=` declaration, if
/// any, must name the encoding the bytes are in.
///
/// # Errors
/// Fails for invalid UTF-8/UTF-16, an unsupported or inconsistent encoding, characters XML
/// does not allow, or oversized input.
pub fn decode_xml_bytes<'a>(bytes: &'a [u8], limits: &XmlLimits) -> Result<Cow<'a, str>, XmlError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(XmlError::InputTooLarge(limits.max_input_bytes));
    }
    let (text, encoding): (Cow<'a, str>, ByteEncoding) = match bytes {
        // UTF-32 byte order marks come first: FF FE 00 00 also starts with the UTF-16LE one.
        [0x00, 0x00, 0xFE, 0xFF, ..] | [0xFF, 0xFE, 0x00, 0x00, ..] => {
            return Err(XmlError::Encoding("UTF-32 is not supported"));
        }
        [0xFF, 0xFE, rest @ ..] => {
            (decode_utf16(rest, u16::from_le_bytes).map(Cow::Owned)?, ByteEncoding::Utf16Le)
        }
        [0xFE, 0xFF, rest @ ..] => {
            (decode_utf16(rest, u16::from_be_bytes).map(Cow::Owned)?, ByteEncoding::Utf16Be)
        }
        // `<` as the first character of UTF-16/UTF-32 without a byte order mark.
        [0x00, 0x3C, ..] | [0x3C, 0x00, ..] | [0x00, 0x00, 0x00, 0x3C, ..] => {
            return Err(XmlError::Encoding("UTF-16/32 without a byte order mark"));
        }
        // `<?xm` in EBCDIC.
        [0x4C, 0x6F, 0xA7, 0x94, ..] => return Err(XmlError::Encoding("EBCDIC is not supported")),
        [0xEF, 0xBB, 0xBF, rest @ ..] => (
            core::str::from_utf8(rest)
                .map(Cow::Borrowed)
                .map_err(|_| XmlError::Encoding("invalid UTF-8"))?,
            ByteEncoding::Utf8,
        ),
        _ => (
            core::str::from_utf8(bytes)
                .map(Cow::Borrowed)
                .map_err(|_| XmlError::Encoding("invalid UTF-8"))?,
            ByteEncoding::Utf8,
        ),
    };
    check_chars(&text)?;
    if let Some(declared) = declared_encoding(&text) {
        let ok = match encoding {
            ByteEncoding::Utf8 => declared.eq_ignore_ascii_case("utf-8"),
            ByteEncoding::Utf16Le => {
                declared.eq_ignore_ascii_case("utf-16") || declared.eq_ignore_ascii_case("utf-16le")
            }
            ByteEncoding::Utf16Be => {
                declared.eq_ignore_ascii_case("utf-16") || declared.eq_ignore_ascii_case("utf-16be")
            }
        };
        if !ok {
            return Err(XmlError::Encoding("declared encoding differs from the byte encoding"));
        }
    }
    Ok(text)
}

fn decode_utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> Result<String, XmlError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(XmlError::Encoding("odd UTF-16 length"));
    }
    let units = bytes.chunks_exact(2).filter_map(|c| <[u8; 2]>::try_from(c).ok()).map(f);
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| XmlError::Encoding("invalid UTF-16"))
}

/// Refuses text that is not made of XML `Char`s the parser may see: NUL and the other C0
/// controls except tab, line feed and carriage return, and U+FFFE / U+FFFF.
fn check_chars(text: &str) -> Result<(), XmlError> {
    let bytes = text.as_bytes();
    if bytes.iter().any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')) {
        return Err(XmlError::Encoding("control character is not allowed in XML"));
    }
    // U+FFFE and U+FFFF are EF BF BE and EF BF BF in UTF-8.
    let mut rest = bytes;
    while let Some(i) = rest.iter().position(|&b| b == 0xEF) {
        rest = rest.get(i + 1..).unwrap_or_default();
        if matches!(rest.get(..2), Some([0xBF, 0xBE | 0xBF])) {
            return Err(XmlError::Encoding("U+FFFE / U+FFFF is not allowed in XML"));
        }
    }
    Ok(())
}

/// The value of the `encoding` pseudo-attribute of the XML declaration at the start of `src`.
#[must_use]
pub fn declared_encoding(src: &str) -> Option<&str> {
    declared_encoding_span(src).and_then(|span| src.get(span))
}

/// Byte range of the value of the `encoding` pseudo-attribute in `src`.
fn declared_encoding_span(src: &str) -> Option<Range<usize>> {
    const WS: [char; 4] = [' ', '\t', '\r', '\n'];
    const PREFIX: &str = "<?xml";
    let decl = src.strip_prefix(PREFIX)?;
    // `<?xml-stylesheet` and friends are processing instructions, not the declaration.
    if !decl.starts_with(WS) {
        return None;
    }
    let decl = decl.get(..decl.find("?>")?)?;
    // `rest` is always a suffix of `decl`, which starts `PREFIX.len()` bytes into `src`.
    let at = |rest: &str| PREFIX.len() + decl.len() - rest.len();
    let mut rest = decl;
    loop {
        rest = rest.trim_start_matches(WS);
        let name_end = rest.find(|c: char| c == '=' || WS.contains(&c))?;
        let name = rest.get(..name_end)?;
        rest = rest.get(name_end..)?.trim_start_matches(WS).strip_prefix('=')?;
        rest = rest.trim_start_matches(WS);
        let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let body = rest.get(1..)?;
        let close = body.find(quote)?;
        if name == "encoding" {
            let start = at(body);
            return Some(start..start + close);
        }
        rest = body.get(close + 1..)?;
    }
}

/// One parser event with its location.
#[derive(Debug)]
pub struct XmlItem<'a> {
    /// The event.
    pub event: Event<'a>,
    /// Byte range of the event in the source text.
    pub span: Range<usize>,
    /// Depth: for `Start` the depth of the element (root = 1); for `End` the
    /// depth of the element being closed; otherwise the depth of the parent.
    pub depth: usize,
}

/// A policy-enforcing pull parser over an XML document held in memory.
pub struct SecureXmlReader<'a> {
    reader: Reader<&'a [u8]>,
    src: &'a str,
    limits: XmlLimits,
    depth: usize,
    events: u64,
    seen_root: bool,
    root_closed: bool,
    pos: usize,
}

impl fmt::Debug for SecureXmlReader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecureXmlReader")
            .field("depth", &self.depth)
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

impl<'a> SecureXmlReader<'a> {
    /// Creates a reader over `src` (use [`decode_xml_bytes`] for raw bytes).
    ///
    /// # Errors
    /// Fails if `src` exceeds [`XmlLimits::max_input_bytes`].
    pub fn new(src: &'a str, limits: XmlLimits) -> Result<Self, XmlError> {
        if src.len() > limits.max_input_bytes {
            return Err(XmlError::InputTooLarge(limits.max_input_bytes));
        }
        check_chars(src)?;
        let mut reader = Reader::from_str(src);
        let cfg = reader.config_mut();
        cfg.check_end_names = true;
        cfg.allow_unmatched_ends = false;
        cfg.allow_dangling_amp = false;
        cfg.expand_empty_elements = false;
        cfg.trim_text_start = false;
        cfg.trim_text_end = false;
        cfg.check_comments = false;
        Ok(Self {
            reader,
            src,
            limits,
            depth: 0,
            events: 0,
            seen_root: false,
            root_closed: false,
            pos: 0,
        })
    }

    /// Source text.
    #[must_use]
    pub const fn source(&self) -> &'a str {
        self.src
    }

    /// Current element depth.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the next event, `Ok(None)` at end of document.
    ///
    /// # Errors
    /// Any policy violation or well-formedness error.
    pub fn next_item(&mut self) -> Result<Option<XmlItem<'a>>, XmlError> {
        let start = self.pos;
        let event = self.reader.read_event().map_err(|e| XmlError::Malformed(e.to_string()))?;
        let end = usize::try_from(self.reader.buffer_position())
            .map_err(|_| XmlError::InputTooLarge(usize::MAX))?;
        self.pos = end;
        self.events = self.events.saturating_add(1);
        if self.events > self.limits.max_events {
            return Err(XmlError::TooManyEvents(self.limits.max_events));
        }
        let span = start..end;
        let depth = match &event {
            Event::Eof => {
                if self.depth != 0 {
                    return Err(XmlError::Malformed("unexpected end of document".into()));
                }
                if !self.seen_root {
                    return Err(XmlError::NoRoot);
                }
                return Ok(None);
            }
            Event::DocType(_) => return Err(XmlError::DoctypeForbidden),
            Event::Start(e) => {
                self.check_element(e)?;
                self.depth = self.depth.saturating_add(1);
                if self.depth > self.limits.max_depth {
                    return Err(XmlError::DepthExceeded(self.limits.max_depth));
                }
                self.depth
            }
            Event::Empty(e) => {
                self.check_element(e)?;
                if self.depth.saturating_add(1) > self.limits.max_depth {
                    return Err(XmlError::DepthExceeded(self.limits.max_depth));
                }
                if self.depth == 0 {
                    self.root_closed = true;
                }
                self.depth
            }
            Event::End(_) => {
                let d = self.depth;
                self.depth = self.depth.saturating_sub(1);
                if self.depth == 0 {
                    self.root_closed = true;
                }
                d
            }
            Event::GeneralRef(r) => {
                if r.is_char_ref() {
                    match r.resolve_char_ref() {
                        Ok(Some(_)) => {}
                        _ => return Err(XmlError::Malformed("invalid character reference".into())),
                    }
                } else {
                    let name = r.xml10_content();
                    if quick_xml::escape::resolve_predefined_entity(&name).is_none() {
                        return Err(XmlError::UndefinedEntity(crate::text::display_safe(
                            &name, 64,
                        )));
                    }
                }
                if self.depth == 0 {
                    return Err(XmlError::Malformed("entity reference outside the root".into()));
                }
                self.depth
            }
            Event::Text(t) => {
                if self.depth == 0 && !t.xml10_content().trim().is_empty() {
                    return Err(if self.root_closed {
                        XmlError::ContentAfterRoot
                    } else {
                        XmlError::Malformed("text before root".into())
                    });
                }
                self.depth
            }
            Event::CData(_) => {
                if self.depth == 0 {
                    return Err(XmlError::Malformed("CDATA outside the root".into()));
                }
                self.depth
            }
            Event::Comment(_) | Event::PI(_) | Event::Decl(_) => self.depth,
        };
        Ok(Some(XmlItem { event, span, depth }))
    }

    fn check_element(&mut self, e: &BytesStart<'_>) -> Result<(), XmlError> {
        if self.depth == 0 {
            if self.root_closed || self.seen_root {
                return Err(XmlError::ContentAfterRoot);
            }
            self.seen_root = true;
        }
        if e.name().as_ref().len() > self.limits.max_name_len {
            return Err(XmlError::NameTooLong);
        }
        // Duplicate detection is done here, allocation-free for the common case
        // (the parser's own `with_checks` allocates a vector per element, which
        // dominated the cost of large spreadsheet parts).
        let mut inline: [&str; INLINE_ATTRS] = [""; INLINE_ATTRS];
        let mut spill: HashSet<&str> = HashSet::new();
        let mut count = 0usize;
        let mut attrs = e.attributes();
        attrs.with_checks(false);
        for attr in attrs {
            let attr = attr.map_err(|err| XmlError::Malformed(err.to_string()))?;
            let key: &str = attr.key.into_inner();
            if key.len() > self.limits.max_name_len {
                return Err(XmlError::NameTooLong);
            }
            let seen_inline = inline.get(..count.min(INLINE_ATTRS)).unwrap_or(&[]);
            if seen_inline.contains(&key) || spill.contains(&key) {
                return Err(XmlError::Malformed(format!(
                    "duplicate attribute {}",
                    crate::text::display_safe(key, 64)
                )));
            }
            if let Some(slot) = inline.get_mut(count) {
                *slot = key;
            } else {
                spill.insert(key);
            }
            // Resolving the value enforces the entity policy inside attributes:
            // only the predefined entities and character references succeed.
            if attr.value.contains('&') && attr.normalized_value(XmlVersion::Implicit1_0).is_err() {
                return Err(XmlError::UndefinedEntity(crate::text::display_safe(&attr.value, 64)));
            }
            count = count.saturating_add(1);
            if count > self.limits.max_attributes {
                return Err(XmlError::TooManyAttributes(self.limits.max_attributes));
            }
        }
        Ok(())
    }
}

/// Validates a whole document against the policy without retaining events.
///
/// # Errors
/// The first policy violation encountered.
pub fn validate_document(src: &str, limits: XmlLimits) -> Result<u64, XmlError> {
    let mut r = SecureXmlReader::new(src, limits)?;
    while r.next_item()?.is_some() {}
    Ok(r.events)
}

/// Local (unprefixed) name of an element.
#[must_use]
pub fn local_name<'b>(e: &'b BytesStart<'_>) -> &'b str {
    let full: &str = e.name().into_inner();
    // Element names are short: a plain reverse scan beats a substring searcher.
    // `:` is ASCII, so the byte after it is a character boundary.
    full.bytes().rposition(|b| b == b':').and_then(|i| full.get(i + 1..)).unwrap_or(full)
}

/// Splits a qualified name into `(prefix, local)`.
#[must_use]
pub fn split_qname(name: &str) -> (Option<&str>, &str) {
    match name.bytes().position(|b| b == b':') {
        Some(i) => (name.get(..i), name.get(i + 1..).unwrap_or_default()),
        None => (None, name),
    }
}

/// Well-formed attributes of `e`, without the parser's duplicate bookkeeping.
///
/// Elements reach callers only through [`SecureXmlReader`], whose
/// `check_element` has already rejected duplicates and malformed attributes,
/// so lookups can skip the per-call allocation of the checking iterator.
fn lookup<'e, 'a>(
    e: &'e BytesStart<'a>,
) -> impl Iterator<Item = quick_xml::events::attributes::Attribute<'e>> {
    let mut attrs = e.attributes();
    attrs.with_checks(false);
    attrs.flatten()
}

/// Returns the unescaped value of the first attribute whose *local* name is
/// `local` (prefix ignored). Malformed attributes yield `None`.
#[must_use]
pub fn attr(e: &BytesStart<'_>, local: &str) -> Option<String> {
    for a in lookup(e) {
        let key: &str = a.key.into_inner();
        if split_qname(key).1 == local {
            return a.normalized_value(XmlVersion::Implicit1_0).ok().map(Cow::into_owned);
        }
    }
    None
}

/// Result of a namespace-aware attribute lookup (see [`attr_ns`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrLookup {
    /// No attribute has this local name.
    Absent,
    /// The attribute, written with the expected prefix, and nothing else of that local name.
    Found(String),
    /// Only attributes with another prefix carry this local name (and exactly one does).
    /// It may be the same namespace under a different prefix, or a foreign attribute that
    /// merely shares the name; the caller decides whether that is acceptable.
    OtherPrefix(String),
    /// Several attributes share the local name under different prefixes (`w:instr` next to
    /// `x:instr`): readers that pick the first, the last or the one in the right namespace
    /// disagree. `preferred` is the one with the expected prefix, if there is one.
    Collision {
        /// Value of the attribute with the expected prefix.
        preferred: Option<String>,
    },
}

/// Looks an attribute up by local name *and* prefix, reporting same-name collisions.
///
/// `prefix` is the prefix the consumer expects (`Some("r")` for `r:id`, `None` for an
/// unprefixed attribute). Unlike [`attr`], which returns the first attribute whose local
/// name matches whatever its prefix, this tells a consumer when the answer would depend on
/// which attribute a reader looks at. A consumer should treat [`AttrLookup::Collision`] as a
/// parser differential, not pick a value silently.
#[must_use]
pub fn attr_ns(e: &BytesStart<'_>, prefix: Option<&str>, local: &str) -> AttrLookup {
    let mut preferred: Option<String> = None;
    let mut other: Option<String> = None;
    let mut matches = 0usize;
    for a in lookup(e) {
        let key: &str = a.key.into_inner();
        let (p, l) = split_qname(key);
        if l != local {
            continue;
        }
        matches += 1;
        // A value that cannot be unescaped counts as present but empty of meaning.
        let value =
            a.normalized_value(XmlVersion::Implicit1_0).map(Cow::into_owned).unwrap_or_default();
        if p == prefix {
            preferred = Some(value);
        } else {
            other = Some(value);
        }
    }
    match (matches, preferred, other) {
        (0, ..) => AttrLookup::Absent,
        (1, Some(v), None) => AttrLookup::Found(v),
        (1, None, Some(v)) => AttrLookup::OtherPrefix(v),
        (_, preferred, _) => AttrLookup::Collision { preferred },
    }
}

/// Replaces the encoding named in the XML declaration at the start of `src` with `UTF-8`.
///
/// A part that was UTF-16 and is written out as UTF-8 (a sterile copy, a rewrite) must not
/// keep saying `encoding="UTF-16"`: the next reader would decode the bytes it was given as
/// something they are not. Text without a declaration, or already declaring UTF-8, is
/// returned unchanged.
#[must_use]
pub fn declare_utf8(src: &str) -> Cow<'_, str> {
    let Some(span) = declared_encoding_span(src) else { return Cow::Borrowed(src) };
    match (src.get(span.clone()), src.get(..span.start), src.get(span.end..)) {
        (Some(enc), Some(head), Some(tail)) if !enc.eq_ignore_ascii_case("utf-8") => {
            Cow::Owned(format!("{head}UTF-8{tail}"))
        }
        _ => Cow::Borrowed(src),
    }
}

/// Returns the unescaped value of the attribute with exactly this qualified name.
#[must_use]
pub fn attr_q(e: &BytesStart<'_>, qname: &str) -> Option<String> {
    for a in lookup(e) {
        if a.key.into_inner() == qname {
            return a.normalized_value(XmlVersion::Implicit1_0).ok().map(Cow::into_owned);
        }
    }
    None
}

/// All attributes as `(qualified name, unescaped value)` pairs.
#[must_use]
pub fn attrs(e: &BytesStart<'_>) -> Vec<(String, String)> {
    lookup(e)
        .filter_map(|a| {
            let k = a.key.into_inner().to_owned();
            a.normalized_value(XmlVersion::Implicit1_0).ok().map(|v| (k, v.into_owned()))
        })
        .collect()
}

/// Appends the textual content carried by `event` (text, CDATA or a resolved
/// entity/character reference) to `out`.
pub fn push_text(event: &Event<'_>, out: &mut String) {
    match event {
        Event::Text(t) => out.push_str(&t.xml10_content()),
        Event::CData(c) => out.push_str(&c.xml10_content()),
        Event::GeneralRef(r) => {
            if r.is_char_ref() {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    out.push(c);
                }
            } else if let Some(v) = quick_xml::escape::resolve_predefined_entity(&r.xml10_content())
            {
                out.push_str(v);
            }
        }
        _ => {}
    }
}

/// Escapes text for inclusion in XML character data or attribute values.
#[must_use]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
