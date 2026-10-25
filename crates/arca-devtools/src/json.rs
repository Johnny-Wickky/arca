// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! A small JSON reader, just enough for the output of `cargo metadata`.

/// A parsed JSON value. Numbers are kept as text: nothing here does arithmetic on them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A number, as written.
    Num(String),
    /// A string, unescaped.
    Str(String),
    /// An array.
    Arr(Vec<Value>),
    /// An object, in document order.
    Obj(Vec<(String, Value)>),
}

const MAX_DEPTH: usize = 256;

impl Value {
    /// The member `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string value, if this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The elements, if this is an array (an empty slice otherwise).
    #[must_use]
    pub fn items(&self) -> &[Value] {
        match self {
            Self::Arr(items) => items,
            _ => &[],
        }
    }

    /// The string member `key`, if present and a string.
    #[must_use]
    pub fn str_of(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Self::as_str)
    }
}

/// Parses a complete JSON document.
///
/// # Errors
/// A description of the first syntax error, with the byte offset.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { src: text.as_bytes(), pos: 0 };
    let v = p.value(0)?;
    p.skip_ws();
    if p.pos != p.src.len() {
        return Err(p.err("trailing data"));
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("json: {what} at byte {}", self.pos)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.src.get(self.pos..).is_some_and(|r| r.starts_with(word.as_bytes())) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.pos;
                while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.pos += 1;
                }
                let text = self.src.get(start..self.pos).unwrap_or_default();
                Ok(Value::Num(String::from_utf8_lossy(text).into_owned()))
            }
            _ => Err(self.err("unexpected character")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_ws();
        if self.eat(b'}') {
            return Ok(Value::Obj(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("object key expected"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(b':') {
                return Err(self.err("':' expected"));
            }
            let v = self.value(depth + 1)?;
            members.push((key, v));
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Ok(Value::Obj(members));
            }
            return Err(self.err("',' or '}' expected"));
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']') {
            return Ok(Value::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Ok(Value::Arr(items));
            }
            return Err(self.err("',' or ']' expected"));
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits =
            self.src.get(self.pos..self.pos + 4).ok_or_else(|| self.err("short \\u escape"))?;
        let mut v = 0u32;
        for &d in digits {
            let n = char::from(d).to_digit(16).ok_or_else(|| self.err("bad \\u escape"))?;
            v = v * 16 + n;
        }
        self.pos += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = self.peek().ok_or_else(|| self.err("unterminated string"))?;
            self.pos += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = self.peek().ok_or_else(|| self.err("unterminated escape"))?;
                    self.pos += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi)
                                && self.src.get(self.pos..self.pos + 2) == Some(b"\\u")
                            {
                                self.pos += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    0xFFFD
                                }
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return Err(self.err("bad escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..=0x1F => return Err(self.err("control character in string")),
                _ => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"))
    }
}

/// Entry point: `json-get <member>` prints the top-level string member `member` of the JSON
/// document on standard input (for example `target_directory` of `cargo metadata`).
///
/// # Errors
/// Bad arguments, invalid JSON, or a member that is missing or not a string.
pub fn run_get(args: &[String], stdin: &mut dyn std::io::Read) -> Result<(), String> {
    let [key] = args else {
        return Err("usage: json-get <member>  (JSON on standard input)".into());
    };
    let mut text = String::new();
    stdin.read_to_string(&mut text).map_err(|e| format!("stdin: {e}"))?;
    let value = parse(&text)?;
    let s = value.str_of(key).ok_or_else(|| format!("no string member {key:?}"))?;
    crate::util::say(s);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_documents() {
        let v = parse(r#" {"a":[1,2.5e3,true,null,"x"],"b":{"c":"d"},"e":[]} "#).unwrap();
        assert_eq!(v.get("a").unwrap().items().len(), 5);
        assert_eq!(v.get("a").unwrap().items()[1], Value::Num("2.5e3".into()));
        assert_eq!(v.get("b").unwrap().str_of("c"), Some("d"));
        assert_eq!(v.get("a").unwrap().items()[3], Value::Null);
        assert!(v.get("e").unwrap().items().is_empty());
        assert!(v.get("zz").is_none());
    }

    #[test]
    fn unescapes_strings_including_surrogate_pairs() {
        let v = parse(r#""a\n\"\\\u00e9\ud83d\ude00\/""#).unwrap();
        assert_eq!(v.as_str(), Some("a\n\"\\\u{e9}\u{1F600}/"));
        assert_eq!(parse(r#""\ud83d""#).unwrap().as_str(), Some("\u{FFFD}"));
    }

    #[test]
    fn keeps_raw_utf8() {
        assert_eq!(parse("\"\u{43f}\u{440}\"").unwrap().as_str(), Some("\u{43f}\u{440}"));
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in
            ["", "{", "[1,]", "{\"a\" 1}", "\"abc", "tru", "[1] x", "\"\\x\"", "\"a\nb\"", "{1:2}"]
        {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn json_get_prints_a_top_level_string_member() {
        let mut input = std::io::Cursor::new(br#"{"target_directory":"/x/y","n":1}"#.to_vec());
        assert!(run_get(&["target_directory".to_owned()], &mut input).is_ok());
        let mut input = std::io::Cursor::new(br#"{"n":1}"#.to_vec());
        assert!(run_get(&["n".to_owned()], &mut input).is_err());
        assert!(run_get(&[], &mut std::io::empty()).is_err());
    }

    #[test]
    fn deep_nesting_is_refused_not_overflowed() {
        let deep = "[".repeat(10_000);
        assert!(parse(&deep).is_err());
    }
}
