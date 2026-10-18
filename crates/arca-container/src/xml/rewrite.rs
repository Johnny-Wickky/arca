//! Span-preserving XML rewriting.
//!
//! The rewriter streams a document through a [`SecureXmlReader`] and asks a
//! policy callback what to do with each event. Untouched events are copied
//! **byte-for-byte** from the source, so every unknown element, attribute,
//! namespace declaration and markup-compatibility block survives unchanged.
//! This is the round-trip preservation property the sanitizer (and the future
//! editor) relies on.

use quick_xml::events::{BytesStart, Event};

use super::{SecureXmlReader, XmlError, XmlItem, XmlLimits, split_qname};

/// What to do with one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Copy the event verbatim.
    Keep,
    /// Omit the event. For a start tag this omits the whole element subtree.
    Drop,
    /// Omit the start and matching end tag, but keep the children (`Start` only).
    Unwrap,
    /// Emit the given text instead of the event (for `Start` the end tag is kept).
    Replace(String),
}

/// Context handed to the policy callback.
#[derive(Debug)]
pub struct Context<'s> {
    /// Local names of the currently open (emitted or unwrapped) ancestors.
    pub ancestors: &'s [String],
}

impl Context<'_> {
    /// `true` if any ancestor has the given local name.
    #[must_use]
    pub fn inside(&self, local: &str) -> bool {
        self.ancestors.iter().any(|a| a == local)
    }

    /// Local name of the direct parent.
    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        self.ancestors.last().map(String::as_str)
    }
}

/// Rewrites `src` according to `policy`.
///
/// # Errors
/// Propagates parser errors and errors returned by `policy`.
pub fn rewrite<F>(src: &str, limits: XmlLimits, mut policy: F) -> Result<String, XmlError>
where
    F: FnMut(&XmlItem<'_>, &Context<'_>) -> Result<Action, XmlError>,
{
    let mut reader = SecureXmlReader::new(src, limits)?;
    let mut out = String::with_capacity(src.len());
    // For every open element: `true` if its tags are being unwrapped.
    let mut frames: Vec<bool> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    // Depth of a dropped subtree root while skipping.
    let mut skipping: Option<usize> = None;

    while let Some(item) = reader.next_item()? {
        if let Some(d) = skipping {
            if matches!(item.event, Event::End(_)) && item.depth == d {
                skipping = None;
            }
            continue;
        }
        let raw = src
            .get(item.span.clone())
            .ok_or_else(|| XmlError::Malformed("span outside source".into()))?;
        match &item.event {
            Event::Start(e) => {
                let local = super::local_name(e).to_owned();
                let action = policy(&item, &Context { ancestors: &names })?;
                match action {
                    Action::Keep => {
                        out.push_str(raw);
                        frames.push(false);
                        names.push(local);
                    }
                    Action::Replace(text) => {
                        out.push_str(&text);
                        frames.push(false);
                        names.push(local);
                    }
                    Action::Unwrap => {
                        frames.push(true);
                        names.push(local);
                    }
                    Action::Drop => skipping = Some(item.depth),
                }
            }
            Event::End(_) => {
                let unwrapped =
                    frames.pop().ok_or_else(|| XmlError::Malformed("unbalanced end tag".into()))?;
                names.pop();
                if !unwrapped {
                    out.push_str(raw);
                }
            }
            _ => match policy(&item, &Context { ancestors: &names })? {
                Action::Keep => out.push_str(raw),
                Action::Replace(text) => out.push_str(&text),
                Action::Drop | Action::Unwrap => {}
            },
        }
    }
    Ok(out)
}

/// Decision for one attribute when rebuilding a start tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrAction {
    /// Keep the attribute verbatim.
    Keep,
    /// Remove the attribute.
    Drop,
    /// Replace its value with the given *unescaped* text.
    Set(String),
}

/// Rebuilds a start (or empty-element) tag, applying `decide` to each attribute.
///
/// Kept attributes retain their original escaped value; the quoting is
/// normalised to double quotes.
#[must_use]
pub fn rebuild_tag<F>(e: &BytesStart<'_>, empty: bool, mut decide: F) -> String
where
    F: FnMut(Option<&str>, &str, &str) -> AttrAction,
{
    let name: &str = e.name().into_inner();
    let mut out = String::with_capacity(64);
    out.push('<');
    out.push_str(name);
    for a in e.attributes().flatten() {
        let key: &str = a.key.into_inner();
        let (prefix, local) = split_qname(key);
        let raw_value: &str = &a.value;
        let unescaped = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_default();
        match decide(prefix, local, &unescaped) {
            AttrAction::Keep => {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                out.push_str(&raw_value.replace('"', "&quot;"));
                out.push('"');
            }
            AttrAction::Set(v) => {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                out.push_str(&super::escape(&v));
                out.push('"');
            }
            AttrAction::Drop => {}
        }
    }
    out.push_str(if empty { "/>" } else { ">" });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="urn:w" xmlns:x="urn:x" mc:Ignorable="x"><w:body><w:p w:rsidR="00AB"><w:ins w:author="Eve"><w:r><w:t>new</w:t></w:r></w:ins><w:del w:author="Eve"><w:r><w:delText>old</w:delText></w:r></w:del><x:unknown keep='a"b'/></w:p></w:body></w:document>"#;

    #[test]
    fn identity_rewrite_is_byte_exact() {
        let out = rewrite(DOC, XmlLimits::default(), |_, _| Ok(Action::Keep)).unwrap();
        assert_eq!(out, DOC);
    }

    #[test]
    fn accept_revisions_drop_attributes() {
        let out = rewrite(DOC, XmlLimits::default(), |item, _| {
            Ok(match &item.event {
                Event::Start(e) if super::super::local_name(e) == "del" => Action::Drop,
                Event::Start(e) if super::super::local_name(e) == "ins" => Action::Unwrap,
                Event::Start(e) if super::super::local_name(e) == "p" => {
                    Action::Replace(rebuild_tag(e, false, |_, l, _| {
                        if l.starts_with("rsid") { AttrAction::Drop } else { AttrAction::Keep }
                    }))
                }
                _ => Action::Keep,
            })
        })
        .unwrap();
        assert!(
            out.contains("<w:p><w:r><w:t>new</w:t></w:r><x:unknown keep='a\"b'/></w:p>"),
            "{out}"
        );
        assert!(!out.contains("Eve"));
        assert!(!out.contains("old"));
        assert!(!out.contains("rsid"));
        super::super::validate_document(&out, XmlLimits::default()).unwrap();
    }

    #[test]
    fn rebuilt_tag_requotes_safely() {
        let src = r#"<a k='x"y' z="1"/>"#;
        let out = rewrite(src, XmlLimits::default(), |item, _| {
            Ok(match &item.event {
                Event::Empty(e) => Action::Replace(rebuild_tag(e, true, |_, l, _| {
                    if l == "z" { AttrAction::Set("<&>".into()) } else { AttrAction::Keep }
                })),
                _ => Action::Keep,
            })
        })
        .unwrap();
        assert_eq!(out, r#"<a k="x&quot;y" z="&lt;&amp;&gt;"/>"#);
        super::super::validate_document(&out, XmlLimits::default()).unwrap();
    }

    #[test]
    fn context_tracks_ancestors() {
        let mut seen = Vec::new();
        rewrite("<a><b><c/></b></a>", XmlLimits::default(), |item, ctx| {
            if let Event::Empty(_) = item.event {
                seen.push(ctx.ancestors.to_vec());
                assert!(ctx.inside("a"));
                assert_eq!(ctx.parent(), Some("b"));
            }
            Ok(Action::Keep)
        })
        .unwrap();
        assert_eq!(seen, vec![vec!["a".to_owned(), "b".to_owned()]]);
    }
}
