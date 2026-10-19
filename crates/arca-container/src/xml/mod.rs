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
//!   transcoded first. Anything else is refused rather than guessed.
//!
//! Each event is returned together with its exact byte span in the source,
//! which the span-preserving [`rewrite`] module uses to re-emit untouched
//! markup verbatim (round-trip preservation of unknown nodes).

pub mod rewrite;

use core::fmt;
use core::ops::Range;
use std::borrow::Cow;

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

/// Decodes raw XML bytes to text according to the BOM.
///
/// # Errors
/// Fails for invalid UTF-8/UTF-16 or oversized input.
pub fn decode_xml_bytes<'a>(bytes: &'a [u8], limits: &XmlLimits) -> Result<Cow<'a, str>, XmlError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(XmlError::InputTooLarge(limits.max_input_bytes));
    }
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => core::str::from_utf8(rest)
            .map(Cow::Borrowed)
            .map_err(|_| XmlError::Encoding("invalid UTF-8")),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes).map(Cow::Owned),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes).map(Cow::Owned),
        _ => core::str::from_utf8(bytes)
            .map(Cow::Borrowed)
            .map_err(|_| XmlError::Encoding("invalid UTF-8")),
    }
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
        let mut spill: Vec<&str> = Vec::new();
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
                spill.push(key);
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
