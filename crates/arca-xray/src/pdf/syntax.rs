//! Just enough PDF syntax to read the dictionary of a stream.
//!
//! The scanner needs to know, for each stream, which filters it was written with, with which
//! parameters, and how long it claims to be. Finding the dictionary by looking for the last `<<`
//! before the keyword `stream` fails on nested dictionaries (`/DecodeParms << ... >>`), so the
//! dictionary is parsed properly: names, numbers, strings, arrays, dictionaries and references,
//! with a budget on tokens and depth so that hostile input costs a bounded amount of work.

use std::collections::BTreeMap;

use super::{is_delim, is_ws};

/// A parsed PDF object (strings are skipped, not decoded).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real,
    Name(String),
    Str,
    Array(Vec<Obj>),
    Dict(Vec<(String, Obj)>),
    Ref(u32),
}

impl Obj {
    /// The value of `key` if `self` is a dictionary.
    pub(super) fn get(&self, key: &str) -> Option<&Obj> {
        match self {
            Self::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(super) fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }
}

const MAX_TOKENS: usize = 8192;
const MAX_DEPTH: usize = 24;

/// A cursor over a bounded window of the file.
pub(super) struct Lexer<'a> {
    d: &'a [u8],
    pub(super) pos: usize,
    tokens: usize,
}

impl<'a> Lexer<'a> {
    pub(super) fn new(d: &'a [u8], pos: usize) -> Self {
        Self { d, pos, tokens: 0 }
    }

    /// Skips white space and comments.
    pub(super) fn skip_ws(&mut self) {
        loop {
            match self.d.get(self.pos) {
                Some(&b) if is_ws(b) => self.pos += 1,
                Some(b'%') => {
                    while self.d.get(self.pos).is_some_and(|&b| b != b'\n' && b != b'\r') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// `true` once only white space and comments remain.
    pub(super) fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.d.len()
    }

    fn name(&mut self) -> String {
        // The caller is positioned on '/'.
        let (name, _, end) = super::read_name(self.d, self.pos);
        self.pos = end.max(self.pos + 1);
        name
    }

    fn skip_literal_string(&mut self) -> Option<()> {
        let mut depth = 0usize;
        while let Some(&b) = self.d.get(self.pos) {
            self.pos += 1;
            match b {
                b'\\' => self.pos += 1,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(());
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn token(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.d.get(self.pos).is_some_and(|&b| !is_ws(b) && !is_delim(b)) {
            self.pos += 1;
        }
        self.d.get(start..self.pos).unwrap_or(&[])
    }

    /// Parses one object, or `None` for anything that is not valid object syntax.
    pub(super) fn object(&mut self, depth: usize) -> Option<Obj> {
        self.tokens += 1;
        if self.tokens > MAX_TOKENS || depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match *self.d.get(self.pos)? {
            b'/' => Some(Obj::Name(self.name())),
            b'<' if self.d.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.d.get(self.pos) == Some(&b'>')
                        && self.d.get(self.pos + 1) == Some(&b'>')
                    {
                        self.pos += 2;
                        return Some(Obj::Dict(items));
                    }
                    if self.d.get(self.pos) != Some(&b'/') {
                        return None;
                    }
                    let key = self.name();
                    let value = self.object(depth + 1)?;
                    items.push((key, value));
                }
            }
            b'<' => {
                // Hex string.
                while let Some(&b) = self.d.get(self.pos) {
                    self.pos += 1;
                    if b == b'>' {
                        return Some(Obj::Str);
                    }
                }
                None
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.d.get(self.pos) == Some(&b']') {
                        self.pos += 1;
                        return Some(Obj::Array(items));
                    }
                    items.push(self.object(depth + 1)?);
                }
            }
            b'(' => self.skip_literal_string().map(|()| Obj::Str),
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                let tok = self.token();
                let text = core::str::from_utf8(tok).ok()?;
                if let Ok(n) = text.parse::<i64>() {
                    // `12 0 R`
                    if let Ok(num) = u32::try_from(n) {
                        let save = self.pos;
                        self.skip_ws();
                        let gen_tok = self.token();
                        if !gen_tok.is_empty() && gen_tok.iter().all(u8::is_ascii_digit) {
                            self.skip_ws();
                            if self.d.get(self.pos) == Some(&b'R')
                                && self.d.get(self.pos + 1).is_none_or(|&b| is_ws(b) || is_delim(b))
                            {
                                self.pos += 1;
                                return Some(Obj::Ref(num));
                            }
                        }
                        self.pos = save;
                    }
                    Some(Obj::Int(n))
                } else if text.parse::<f64>().is_ok() {
                    Some(Obj::Real)
                } else {
                    None
                }
            }
            _ => match self.token() {
                b"true" => Some(Obj::Bool(true)),
                b"false" => Some(Obj::Bool(false)),
                b"null" => Some(Obj::Null),
                _ => None,
            },
        }
    }
}

/// The dictionary that ends exactly at the end of `window` (the bytes in front of a `stream`
/// keyword): it follows the last `obj` keyword of the window.
pub(super) fn dict_before_stream(window: &[u8]) -> Option<Obj> {
    // The last `obj` that starts a token (`12 0 obj`, not `endobj`).
    let mut from = window.len();
    let start = loop {
        let k = window.get(..from)?.windows(3).rposition(|w| w == b"obj")?;
        let before_ok = k > 0 && window.get(k - 1).is_some_and(|&b| is_ws(b) || b.is_ascii_digit());
        let after_ok = window.get(k + 3).is_none_or(|&b| is_ws(b) || is_delim(b));
        if before_ok && after_ok {
            break k + 3;
        }
        from = k + 2;
        if from < 3 {
            return None;
        }
    };
    let mut lx = Lexer::new(window, start);
    let dict = lx.object(0)?;
    (matches!(dict, Obj::Dict(_)) && lx.at_end()).then_some(dict)
}

/// Start of the run of digits that ends at `end`.
fn digits_back(data: &[u8], end: usize) -> usize {
    let mut s = end;
    while s > 0 && data.get(s - 1).is_some_and(u8::is_ascii_digit) {
        s -= 1;
    }
    s
}

/// Start of the run of white space that ends at `end`.
fn ws_back(data: &[u8], end: usize) -> usize {
    let mut s = end;
    while s > 0 && data.get(s - 1).is_some_and(|&b| is_ws(b)) {
        s -= 1;
    }
    s
}

/// Integer objects (`12 0 obj 345 endobj`) of the file by number: where `/Length 12 0 R` points.
/// Numbers defined more than once with different values map to `None`.
pub(super) fn integer_objects(data: &[u8]) -> BTreeMap<u32, Option<i64>> {
    const MAX_ENTRIES: usize = 65_536;
    let mut out: BTreeMap<u32, Option<i64>> = BTreeMap::new();
    let mut from = 0usize;
    while let Some(rel) = data.get(from..).and_then(|d| d.windows(3).position(|w| w == b"obj")) {
        let at = from + rel;
        from = at + 3;
        if out.len() >= MAX_ENTRIES {
            break;
        }
        // `<num> <gen> obj` in front ...
        let gen_end = ws_back(data, at);
        let gen_start = digits_back(data, gen_end);
        let num_end = ws_back(data, gen_start);
        let num_start = digits_back(data, num_end);
        if gen_end == at || gen_start == gen_end || num_end == gen_start || num_start == num_end {
            continue;
        }
        let Some(num) = data
            .get(num_start..num_end)
            .and_then(|t| core::str::from_utf8(t).ok())
            .and_then(|t| t.parse::<u32>().ok())
        else {
            continue;
        };
        // ... and `<int> endobj` after.
        let mut lx = Lexer::new(data, at + 3);
        lx.skip_ws();
        let tok = lx.token();
        let value = core::str::from_utf8(tok).ok().and_then(|t| t.parse::<i64>().ok());
        lx.skip_ws();
        if let Some(v) = value
            && lx.token() == b"endobj"
        {
            out.entry(num)
                .and_modify(|old| {
                    if *old != Some(v) {
                        *old = None;
                    }
                })
                .or_insert(Some(v));
        }
    }
    out
}
