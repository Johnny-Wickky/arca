//! Visitor-style driver over [`SecureXmlReader`] used by all XML part scanners.

use std::collections::HashMap;

use arca_container::xml::{self, SecureXmlReader, XmlError};
use quick_xml::events::{BytesStart, Event};

use crate::report::{ExternalReference, RefKind, ThreatKind};
use crate::{Abort, Ctx};

/// The local names of the currently open elements, outermost first.
///
/// All names live in one reusable buffer, so tracking the stack costs no heap
/// allocation per element (a spreadsheet part has millions of elements).
#[derive(Debug, Default)]
pub(crate) struct Ancestors {
    names: String,
    /// End offset in `names` of every open element's local name.
    ends: Vec<usize>,
}

impl Ancestors {
    fn push(&mut self, local: &str) {
        self.names.push_str(local);
        self.ends.push(self.names.len());
    }

    /// Removes the innermost name and returns it (copied: the buffer is reused).
    fn pop_into(&mut self, out: &mut String) {
        out.clear();
        if let Some(end) = self.ends.pop() {
            let start = self.ends.last().copied().unwrap_or(0);
            if let Some(name) = self.names.get(start..end) {
                out.push_str(name);
            }
            self.names.truncate(start);
        }
    }

    /// Number of open elements.
    pub(crate) fn len(&self) -> usize {
        self.ends.len()
    }

    /// `true` outside the root element.
    pub(crate) fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// The `i`-th open element, `0` being the outermost.
    pub(crate) fn get(&self, i: usize) -> Option<&str> {
        let end = *self.ends.get(i)?;
        let start = if i == 0 { 0 } else { *self.ends.get(i - 1)? };
        self.names.get(start..end)
    }

    /// The innermost open element.
    pub(crate) fn last(&self) -> Option<&str> {
        self.len().checked_sub(1).and_then(|i| self.get(i))
    }

    /// Open elements, outermost first.
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &str> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// `true` if the innermost open elements are exactly `tail` (outermost first).
    pub(crate) fn ends_with(&self, tail: &[&str]) -> bool {
        let Some(first) = self.len().checked_sub(tail.len()) else {
            return false;
        };
        tail.iter().enumerate().all(|(k, want)| self.get(first + k) == Some(*want))
    }
}

/// Callbacks for one XML part. `ancestors` holds the local names of the open
/// elements *excluding* the current one.
pub(crate) trait Visitor {
    fn start(
        &mut self,
        _ctx: &mut Ctx<'_>,
        _e: &BytesStart<'_>,
        _empty: bool,
        _ancestors: &Ancestors,
    ) {
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, _text: &str, _ancestors: &Ancestors) {}
    fn end(&mut self, _ctx: &mut Ctx<'_>, _local: &str, _ancestors: &Ancestors) {}
}

/// A visitor that only performs the generic checks of [`walk`].
pub(crate) struct NoopVisitor;
impl Visitor for NoopVisitor {}

/// Walks an XML part.
///
/// Generic checks applied to every part: the secure reader policy (DOCTYPE
/// and undefined entities reject the document), `xml-stylesheet` processing
/// instructions (external fetch) and event accounting.
///
/// Returns `Ok(false)` when the part is malformed (a finding is recorded).
pub(crate) fn walk(
    ctx: &mut Ctx<'_>,
    bytes: &[u8],
    location: &str,
    v: &mut dyn Visitor,
) -> Result<bool, Abort> {
    let text = match xml::decode_xml_bytes(bytes, &ctx.policy.xml) {
        Ok(t) => t,
        Err(e) => {
            ctx.report.add(ThreatKind::MalformedContainer, location, e.to_string());
            return Ok(false);
        }
    };
    let mut reader = match SecureXmlReader::new(&text, ctx.policy.xml) {
        Ok(r) => r,
        Err(e) => return map_error(ctx, e, location),
    };
    let mut ancestors = Ancestors::default();
    let mut closing = String::new();
    let mut events = 0u64;
    let mut buf = String::new();
    let mut collisions = Collisions::default();
    loop {
        let item = match reader.next_item() {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(e) => {
                ctx.report.stats.xml_events = ctx.report.stats.xml_events.saturating_add(events);
                collisions.report(ctx, location);
                return map_error(ctx, e, location);
            }
        };
        events = events.saturating_add(1);
        match &item.event {
            Event::Start(e) => {
                if ctx.attr_collisions {
                    collisions.note(e);
                }
                v.start(ctx, e, false, &ancestors);
                ancestors.push(xml::local_name(e));
            }
            Event::Empty(e) => {
                if ctx.attr_collisions {
                    collisions.note(e);
                }
                v.start(ctx, e, true, &ancestors);
                v.end(ctx, xml::local_name(e), &ancestors);
            }
            Event::End(_) => {
                ancestors.pop_into(&mut closing);
                v.end(ctx, &closing, &ancestors);
            }
            Event::Text(_) | Event::CData(_) | Event::GeneralRef(_) => {
                buf.clear();
                xml::push_text(&item.event, &mut buf);
                if !buf.is_empty() {
                    v.text(ctx, &buf, &ancestors);
                }
            }
            Event::PI(pi) => {
                if pi.target().eq_ignore_ascii_case("xml-stylesheet") {
                    let content = pi.content();
                    let href = pseudo_attr(content, "href").unwrap_or_else(|| content.to_owned());
                    let class = crate::url::classify(&href);
                    ctx.report.external(ExternalReference {
                        target: arca_container::text::display_safe(&href, 512),
                        kind: RefKind::Stylesheet,
                        class,
                        location: location.to_owned(),
                        auto_load: true,
                    });
                    ctx.report.add(ThreatKind::XmlStylesheetReference, location, href);
                }
            }
            Event::Comment(_) | Event::Decl(_) | Event::DocType(_) | Event::Eof => {}
        }
    }
    ctx.report.stats.xml_events = ctx.report.stats.xml_events.saturating_add(events);
    collisions.report(ctx, location);
    Ok(true)
}

/// Elements of one part whose attributes share a local name under different prefixes.
///
/// `foo:instr` next to `w:instr`, `a:Target` next to `Target`: a reader that matches
/// namespaces, one that matches local names and one that takes the last occurrence each
/// pick a different value. The scanner cannot know which one the consumer will use, so the part
/// is reported (as a parser differential, which prevents a clean verdict).
#[derive(Default)]
struct Collisions {
    count: u64,
    example: String,
}

impl Collisions {
    fn note(&mut self, e: &BytesStart<'_>) {
        // Every prefixed attribute has a `:` in its raw text; an element without one cannot
        // collide (two unprefixed attributes of one name are rejected by the reader).
        if !e.attributes_raw().contains(':') {
            return;
        }
        // (local name, qualified name) of the attributes seen so far.
        let mut inline: [(&str, &str); 16] = [("", ""); 16];
        let mut spill: HashMap<&str, &str> = HashMap::new();
        let mut n = 0usize;
        let mut attrs = e.attributes();
        attrs.with_checks(false);
        for attr in attrs.flatten() {
            let key: &str = attr.key.into_inner();
            if key == "xmlns" || key.starts_with("xmlns:") {
                continue;
            }
            let local = xml::split_qname(key).1;
            let earlier = inline
                .get(..n.min(16))
                .unwrap_or(&[])
                .iter()
                .find(|(l, _)| *l == local)
                .map(|(_, k)| *k)
                .or_else(|| spill.get(local).copied());
            if let Some(other) = earlier {
                // `id` next to `r:id` is how Office writes a relationship reference beside the
                // object's own number (`<p:sldId id="256" r:id="rId2"/>`).
                let by_design =
                    local == "id" && matches!((key, other), ("id", "r:id") | ("r:id", "id"));
                if !by_design {
                    self.count += 1;
                    if self.example.is_empty() {
                        self.example =
                            format!("<{}> has several {local:?} attributes", xml::local_name(e));
                    }
                    return;
                }
                continue;
            }
            match inline.get_mut(n) {
                Some(slot) => *slot = (local, key),
                None => {
                    spill.insert(local, key);
                }
            }
            n += 1;
        }
    }

    fn report(&self, ctx: &mut Ctx<'_>, location: &str) {
        if self.count > 0 {
            ctx.report.add(
                ThreatKind::ParserDifferential,
                location,
                format!(
                    "{} element(s) carry attributes with the same local name under different prefixes ({}); readers disagree on which one counts",
                    self.count, self.example
                ),
            );
        }
    }
}

fn map_error(ctx: &mut Ctx<'_>, e: XmlError, location: &str) -> Result<bool, Abort> {
    match e {
        XmlError::DoctypeForbidden | XmlError::UndefinedEntity(_) => {
            Err(Abort::new(ThreatKind::XmlEntityAttack, location, e.to_string()))
        }
        other => {
            ctx.report.add(ThreatKind::MalformedContainer, location, other.to_string());
            Ok(false)
        }
    }
}

/// Values of every attribute of `e` whose local name is `local`, whatever its prefix.
///
/// For attributes a consumer picks by namespace (`table:formula`, `xlink:href`): taking only the
/// first match would let a harmless attribute in another namespace hide the real one.
pub(crate) fn attr_all(e: &BytesStart<'_>, local: &str) -> Vec<String> {
    xml::attrs(e)
        .into_iter()
        .filter(|(k, _)| xml::split_qname(k).1 == local)
        .map(|(_, v)| v)
        .collect()
}

/// Extracts a pseudo-attribute (`href="..."`) from processing-instruction content.
pub(crate) fn pseudo_attr(content: &str, name: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let key = format!("{name}=");
    let at = lower.find(&key)?;
    let rest = content.get(at + key.len()..)?;
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = rest.get(1..)?;
    let end = body.find(quote)?;
    body.get(..end).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudo_attributes() {
        assert_eq!(
            pseudo_attr(r#"type="text/xsl" href="http://x/s.xsl""#, "href").as_deref(),
            Some("http://x/s.xsl")
        );
        assert_eq!(pseudo_attr("href='a'", "href").as_deref(), Some("a"));
        assert_eq!(pseudo_attr("nohref", "href"), None);
    }
}
