// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! A small JSON writer.
//!
//! The apps receive every result as UTF-8 JSON. Writing it by hand keeps the
//! dependency tree empty and the output deterministic. The writer cannot emit
//! malformed JSON for well-nested calls: commas, quotes and escapes are handled
//! here, and all text goes through [`JsonWriter::string`].

use core::fmt::Debug;

/// Streaming JSON writer into a byte buffer.
///
/// A writer made with [`JsonWriter::with_limit`] stops appending once the document would pass
/// the limit, so a result built from document-controlled sizes cannot grow without bound; see
/// [`JsonWriter::finish_within`].
#[derive(Debug)]
pub struct JsonWriter {
    out: Vec<u8>,
    /// One entry per open container: `true` while it has no element yet.
    first: Vec<bool>,
    /// A key was just written, so the next value follows without a comma.
    after_key: bool,
    /// The most bytes the document may have.
    limit: usize,
    /// The limit was reached; nothing more is appended.
    over: bool,
}

impl Default for JsonWriter {
    fn default() -> Self {
        Self {
            out: Vec::new(),
            first: Vec::new(),
            after_key: false,
            limit: usize::MAX,
            over: false,
        }
    }
}

impl JsonWriter {
    /// An empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty writer whose document may not pass `limit` bytes.
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        Self { limit, ..Self::default() }
    }

    /// `true` once the limit was reached (the rest of the document was not written).
    #[must_use]
    pub const fn is_over(&self) -> bool {
        self.over
    }

    /// The finished document.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.out
    }

    /// The finished document, or `None` if it did not fit the limit.
    #[must_use]
    pub fn finish_within(self) -> Option<Vec<u8>> {
        if self.over { None } else { Some(self.out) }
    }

    /// Appends raw bytes unless the limit is (or would be) passed.
    fn raw(&mut self, bytes: &[u8]) {
        if self.over {
            return;
        }
        if self.out.len().saturating_add(bytes.len()) > self.limit {
            self.over = true;
            return;
        }
        self.out.extend_from_slice(bytes);
    }

    /// Bytes written so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// `true` if nothing was written yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    fn before_value(&mut self) {
        if self.after_key {
            self.after_key = false;
            return;
        }
        if let Some(first) = self.first.last_mut() {
            if *first {
                *first = false;
            } else {
                self.raw(b",");
            }
        }
    }

    /// Opens an object.
    pub fn begin_object(&mut self) {
        self.before_value();
        self.raw(b"{");
        self.first.push(true);
    }

    /// Closes the innermost object.
    pub fn end_object(&mut self) {
        self.first.pop();
        self.raw(b"}");
    }

    /// Opens an array.
    pub fn begin_array(&mut self) {
        self.before_value();
        self.raw(b"[");
        self.first.push(true);
    }

    /// Closes the innermost array.
    pub fn end_array(&mut self) {
        self.first.pop();
        self.raw(b"]");
    }

    /// Writes an object key; the next call must write its value.
    pub fn key(&mut self, key: &str) {
        self.before_value();
        self.push_string(key);
        self.raw(b":");
        self.after_key = true;
    }

    /// Writes a string value.
    pub fn string(&mut self, value: &str) {
        self.before_value();
        self.push_string(value);
    }

    /// Writes the `Debug` name of an enum value as a string (stable identifiers such as
    /// `Docx` or `HiddenTrackers`).
    pub fn name(&mut self, value: &impl Debug) {
        self.string(&format!("{value:?}"));
    }

    /// Writes an unsigned integer.
    pub fn u64(&mut self, value: u64) {
        self.before_value();
        self.raw(value.to_string().as_bytes());
    }

    /// Writes a signed integer.
    pub fn i64(&mut self, value: i64) {
        self.before_value();
        self.raw(value.to_string().as_bytes());
    }

    /// Writes a number with at most two decimals (a coordinate or a size); not-a-number and
    /// infinities become `0`.
    pub fn f32(&mut self, value: f32) {
        self.before_value();
        let v = if value.is_finite() { value } else { 0.0 };
        let mut text = format!("{v:.2}");
        if text.contains('.') {
            while text.ends_with('0') {
                text.pop();
            }
            if text.ends_with('.') {
                text.pop();
            }
        }
        if text == "-0" {
            text = "0".into();
        }
        self.raw(text.as_bytes());
    }

    /// Writes a boolean.
    pub fn bool(&mut self, value: bool) {
        self.before_value();
        self.raw(if value { b"true" } else { b"false" });
    }

    /// Writes `null`.
    pub fn null(&mut self) {
        self.before_value();
        self.raw(b"null");
    }

    /// `"key": "value"`.
    pub fn field_str(&mut self, key: &str, value: &str) {
        self.key(key);
        self.string(value);
    }

    /// A string the document supplied that the app shows as a *fact about* the document (a
    /// location, a name, a target, a value) rather than as its content: written through
    /// [`arca_container::text::display_safe`], so line breaks, control, bidirectional and
    /// invisible characters arrive as visible escapes and the text is cut at `max_chars`.
    ///
    /// The guest normalises these already; the host does it again because the guest is the part
    /// that handles hostile input, and what it reports is not to be trusted to be display-safe.
    pub fn label(&mut self, value: &str, max_chars: usize) {
        self.string(&arca_container::text::display_safe(value, max_chars));
    }

    /// `"key": label`, see [`JsonWriter::label`].
    pub fn field_label(&mut self, key: &str, value: &str, max_chars: usize) {
        self.key(key);
        self.label(value, max_chars);
    }

    /// `"key": value`.
    pub fn field_u64(&mut self, key: &str, value: u64) {
        self.key(key);
        self.u64(value);
    }

    /// `"key": true|false`.
    pub fn field_bool(&mut self, key: &str, value: bool) {
        self.key(key);
        self.bool(value);
    }

    /// `"key": "DebugName"`.
    pub fn field_name(&mut self, key: &str, value: &impl Debug) {
        self.key(key);
        self.name(value);
    }

    fn push_string(&mut self, s: &str) {
        self.raw(b"\"");
        let mut buf = [0u8; 4];
        for c in s.chars() {
            if self.over {
                return;
            }
            match c {
                '"' => self.raw(b"\\\""),
                '\\' => self.raw(b"\\\\"),
                '\n' => self.raw(b"\\n"),
                '\r' => self.raw(b"\\r"),
                '\t' => self.raw(b"\\t"),
                // Control characters, DEL and the two line separators that break
                // JavaScript-era parsers: always escaped.
                c if u32::from(c) < 0x20 || matches!(c, '\u{7f}' | '\u{2028}' | '\u{2029}') => {
                    self.raw(format!("\\u{:04x}", u32::from(c)).as_bytes());
                }
                c => self.raw(c.encode_utf8(&mut buf).as_bytes()),
            }
        }
        self.raw(b"\"");
    }
}

/// A minimal JSON reader used by the tests of this crate to check that every
/// document the writers produce is well-formed and carries the expected values.
#[cfg(test)]
pub(crate) mod check {
    use std::collections::BTreeMap;

    /// A parsed JSON value.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(BTreeMap<String, Value>),
    }

    impl Value {
        pub(crate) fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Self::Obj(m) => m.get(key),
                _ => None,
            }
        }

        pub(crate) fn at(&self, i: usize) -> Option<&Value> {
            match self {
                Self::Arr(a) => a.get(i),
                _ => None,
            }
        }

        pub(crate) fn str(&self) -> Option<&str> {
            match self {
                Self::Str(s) => Some(s),
                _ => None,
            }
        }

        pub(crate) fn num(&self) -> Option<f64> {
            match self {
                Self::Num(n) => Some(*n),
                _ => None,
            }
        }

        pub(crate) fn len(&self) -> usize {
            match self {
                Self::Arr(a) => a.len(),
                Self::Obj(o) => o.len(),
                _ => 0,
            }
        }
    }

    struct P<'a> {
        s: &'a [u8],
        i: usize,
        depth: usize,
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Value, String> {
        // Valid UTF-8 is part of the contract.
        core::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
        let mut p = P { s: bytes, i: 0, depth: 0 };
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("trailing data at {}", p.i));
        }
        Ok(v)
    }

    impl P<'_> {
        fn ws(&mut self) {
            while matches!(self.s.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
                self.i += 1;
            }
        }

        fn eat(&mut self, c: u8) -> Result<(), String> {
            if self.s.get(self.i) == Some(&c) {
                self.i += 1;
                Ok(())
            } else {
                Err(format!("expected {:?} at {}", char::from(c), self.i))
            }
        }

        fn lit(&mut self, word: &str, v: Value) -> Result<Value, String> {
            if self.s.get(self.i..self.i + word.len()) == Some(word.as_bytes()) {
                self.i += word.len();
                Ok(v)
            } else {
                Err(format!("bad literal at {}", self.i))
            }
        }

        fn value(&mut self) -> Result<Value, String> {
            self.ws();
            self.depth += 1;
            if self.depth > 64 {
                return Err("too deep".into());
            }
            let v = match self.s.get(self.i) {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => self.string().map(Value::Str),
                Some(b't') => self.lit("true", Value::Bool(true)),
                Some(b'f') => self.lit("false", Value::Bool(false)),
                Some(b'n') => self.lit("null", Value::Null),
                Some(b'-' | b'0'..=b'9') => self.number(),
                other => Err(format!("unexpected {other:?} at {}", self.i)),
            };
            self.depth -= 1;
            v
        }

        fn number(&mut self) -> Result<Value, String> {
            let start = self.i;
            while matches!(self.s.get(self.i), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
            {
                self.i += 1;
            }
            let text = core::str::from_utf8(self.s.get(start..self.i).unwrap_or_default())
                .map_err(|e| e.to_string())?;
            text.parse::<f64>().map(Value::Num).map_err(|e| format!("{text}: {e}"))
        }

        fn string(&mut self) -> Result<String, String> {
            self.eat(b'"')?;
            let mut out = Vec::new();
            loop {
                let Some(&b) = self.s.get(self.i) else { return Err("unterminated string".into()) };
                self.i += 1;
                match b {
                    b'"' => break,
                    b'\\' => {
                        let Some(&e) = self.s.get(self.i) else { return Err("bad escape".into()) };
                        self.i += 1;
                        match e {
                            b'"' | b'\\' | b'/' => out.push(e),
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            b'u' => {
                                let hex = self
                                    .s
                                    .get(self.i..self.i + 4)
                                    .and_then(|h| core::str::from_utf8(h).ok())
                                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                                    .ok_or("bad \\u escape")?;
                                self.i += 4;
                                let c = char::from_u32(hex).unwrap_or('\u{fffd}');
                                let mut buf = [0u8; 4];
                                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                            }
                            _ => return Err("bad escape".into()),
                        }
                    }
                    0..=0x1f => return Err(format!("raw control byte {b:#x} in string")),
                    _ => out.push(b),
                }
            }
            String::from_utf8(out).map_err(|e| e.to_string())
        }

        fn array(&mut self) -> Result<Value, String> {
            self.eat(b'[')?;
            let mut v = Vec::new();
            self.ws();
            if self.s.get(self.i) == Some(&b']') {
                self.i += 1;
                return Ok(Value::Arr(v));
            }
            loop {
                v.push(self.value()?);
                self.ws();
                match self.s.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        return Ok(Value::Arr(v));
                    }
                    _ => return Err(format!("bad array at {}", self.i)),
                }
            }
        }

        fn object(&mut self) -> Result<Value, String> {
            self.eat(b'{')?;
            let mut m = BTreeMap::new();
            self.ws();
            if self.s.get(self.i) == Some(&b'}') {
                self.i += 1;
                return Ok(Value::Obj(m));
            }
            loop {
                self.ws();
                let k = self.string()?;
                self.ws();
                self.eat(b':')?;
                let v = self.value()?;
                if m.insert(k.clone(), v).is_some() {
                    return Err(format!("duplicate key {k}"));
                }
                self.ws();
                match self.s.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        return Ok(Value::Obj(m));
                    }
                    _ => return Err(format!("bad object at {}", self.i)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check::{Value, parse};
    use super::*;

    #[test]
    fn nesting_commas_and_keys() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_str("a", "x");
        w.key("list");
        w.begin_array();
        w.u64(1);
        w.begin_object();
        w.field_bool("t", true);
        w.key("n");
        w.null();
        w.end_object();
        w.begin_array();
        w.end_array();
        w.i64(-5);
        w.end_array();
        w.field_u64("z", u64::MAX);
        w.end_object();
        let text = String::from_utf8(w.finish()).unwrap();
        assert_eq!(
            text,
            r#"{"a":"x","list":[1,{"t":true,"n":null},[],-5],"z":18446744073709551615}"#
        );
        assert!(parse(text.as_bytes()).is_ok());
    }

    #[test]
    fn strings_are_escaped_and_roundtrip() {
        let nasty = "q\"uote \\ back\nline\r\ttab \u{1} \u{7f} \u{2028}\u{2029} Привет 😀 \u{0}";
        let mut w = JsonWriter::new();
        w.begin_array();
        w.string(nasty);
        w.end_array();
        let bytes = w.finish();
        // No raw control characters or separators in the output.
        assert!(bytes.iter().all(|b| *b >= 0x20 && *b != 0x7f));
        assert!(!String::from_utf8_lossy(&bytes).contains(['\u{2028}', '\u{2029}']));
        let v = parse(&bytes).unwrap();
        assert_eq!(v.at(0).and_then(Value::str), Some(nasty));
    }

    #[test]
    fn names_use_debug_identifiers() {
        #[derive(Debug)]
        enum E {
            HiddenTrackers,
        }
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_name("v", &E::HiddenTrackers);
        w.end_object();
        assert_eq!(String::from_utf8(w.finish()).unwrap(), r#"{"v":"HiddenTrackers"}"#);
    }

    #[test]
    fn the_checker_rejects_malformed_json() {
        for bad in ["{", "[1,]", r#"{"a":1,"a":2}"#, r#"{"a" 1}"#, "\"x", "tru", "[1 2]", "{}x"] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
