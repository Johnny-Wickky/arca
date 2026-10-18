//! Visitor-style driver over [`SecureXmlReader`] used by all XML part scanners.

use arca_container::xml::{self, SecureXmlReader, XmlError};
use quick_xml::events::{BytesStart, Event};

use crate::report::{ExternalReference, RefKind, ThreatKind};
use crate::{Abort, Ctx};

/// Callbacks for one XML part. `ancestors` holds the local names of the open
/// elements *excluding* the current one.
pub(crate) trait Visitor {
    fn start(
        &mut self,
        _ctx: &mut Ctx<'_>,
        _e: &BytesStart<'_>,
        _empty: bool,
        _ancestors: &[String],
    ) {
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, _text: &str, _ancestors: &[String]) {}
    fn end(&mut self, _ctx: &mut Ctx<'_>, _local: &str, _ancestors: &[String]) {}
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
    let mut ancestors: Vec<String> = Vec::new();
    let mut events = 0u64;
    let mut buf = String::new();
    loop {
        let item = match reader.next_item() {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(e) => {
                ctx.report.stats.xml_events = ctx.report.stats.xml_events.saturating_add(events);
                return map_error(ctx, e, location);
            }
        };
        events = events.saturating_add(1);
        match &item.event {
            Event::Start(e) => {
                v.start(ctx, e, false, &ancestors);
                ancestors.push(xml::local_name(e).to_owned());
            }
            Event::Empty(e) => {
                v.start(ctx, e, true, &ancestors);
                v.end(ctx, xml::local_name(e), &ancestors);
            }
            Event::End(e) => {
                let local = ancestors.pop().unwrap_or_default();
                let _ = e;
                v.end(ctx, &local, &ancestors);
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
    Ok(true)
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
