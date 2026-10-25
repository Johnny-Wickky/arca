// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! A tiny element tree built on arca-container's policy-enforcing XML reader.
//!
//! Android resources are plain XML without DOCTYPE or custom entities, so the strict reader
//! (which refuses both) is the right front end; the tree keeps what the checks need: element
//! names, attributes and the concatenated text of each element.

use arca_container::xml::{SecureXmlReader, XmlLimits, attrs, decode_xml_bytes, push_text};
use quick_xml::events::{BytesStart, Event};

/// One element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    /// Qualified tag name as written (`uses-permission`, `tools:foo`).
    pub name: String,
    /// Attributes as `(qualified name, value)`.
    pub attrs: Vec<(String, String)>,
    /// Child elements in document order.
    pub children: Vec<Element>,
    /// All character data below this element, in document order (comments excluded).
    pub text: String,
}

impl Element {
    /// The attribute with exactly this qualified name (`android:name`).
    #[must_use]
    pub fn attr(&self, qname: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == qname).map(|(_, v)| v.as_str())
    }

    /// This element and all descendants, parents before children.
    #[must_use]
    pub fn descendants(&self) -> Vec<&Element> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a Element>) {
        out.push(self);
        for c in &self.children {
            c.collect(out);
        }
    }
}

fn open(e: &BytesStart<'_>) -> Element {
    Element {
        name: e.name().into_inner().to_owned(),
        attrs: attrs(e),
        children: Vec::new(),
        text: String::new(),
    }
}

fn close(el: Element, stack: &mut [Element], root: &mut Option<Element>) {
    if let Some(parent) = stack.last_mut() {
        parent.text.push_str(&el.text);
        parent.children.push(el);
    } else {
        *root = Some(el);
    }
}

/// Parses an XML document (raw bytes, BOM and declared encoding honoured).
///
/// # Errors
/// The first well-formedness or policy error, as text.
pub fn parse(bytes: &[u8]) -> Result<Element, String> {
    let limits = XmlLimits::default();
    let src = decode_xml_bytes(bytes, &limits).map_err(|e| e.to_string())?;
    let mut reader = SecureXmlReader::new(&src, limits).map_err(|e| e.to_string())?;
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    while let Some(item) = reader.next_item().map_err(|e| e.to_string())? {
        match &item.event {
            Event::Start(e) => stack.push(open(e)),
            Event::Empty(e) => close(open(e), &mut stack, &mut root),
            Event::End(_) => {
                if let Some(el) = stack.pop() {
                    close(el, &mut stack, &mut root);
                }
            }
            other => {
                if let Some(top) = stack.last_mut() {
                    push_text(other, &mut top.text);
                }
            }
        }
    }
    root.ok_or_else(|| "document has no root element".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_tree_with_text_and_attributes() {
        let r = parse(
            br#"<?xml version="1.0"?><r a="1"><!-- c --><s name="x">Hi <b>there</b>!</s><e/></r>"#,
        )
        .unwrap();
        assert_eq!(r.name, "r");
        assert_eq!(r.attr("a"), Some("1"));
        assert_eq!(r.children.len(), 2);
        assert_eq!(r.children[0].text, "Hi there!");
        assert_eq!(r.children[0].attr("name"), Some("x"));
        assert_eq!(r.descendants().len(), 4);
    }

    #[test]
    fn refuses_what_is_not_well_formed_or_has_a_doctype() {
        assert!(parse(b"<a><b></a>").is_err());
        assert!(parse(b"<a>").is_err());
        assert!(parse(b"").is_err());
        assert!(parse(b"<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>").is_err());
    }

    #[test]
    fn entities_and_cdata_are_resolved() {
        let r = parse(b"<a>&lt;&amp;<![CDATA[<x>]]></a>").unwrap();
        assert_eq!(r.text, "<&<x>");
    }
}
