//! PDF object model and tokenizer (ISO 32000-1 §7.2–7.3).
//!
//! The same tokenizer reads the file body and content streams. It is lenient — real
//! files are full of small violations — but never allocates more than the bytes it reads
//! warrant, limits nesting and never panics.

use std::collections::BTreeMap;
use std::rc::Rc;

/// Maximum nesting of arrays and dictionaries.
pub(super) const MAX_DEPTH: usize = 32;
/// Maximum number of elements in one parsed object (arrays and dictionaries together).
pub(super) const MAX_ELEMS: usize = 1 << 19;

/// A dictionary. Keys are names without the leading slash.
pub(super) type Dict = BTreeMap<Rc<str>, Obj>;

/// A stream object: its dictionary and where its raw (still encoded) bytes lie in the file.
#[derive(Debug, PartialEq)]
pub(super) struct Stream {
    pub(super) dict: Dict,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// A PDF object. Cloning is cheap: containers are reference counted.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) enum Obj {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Rc<[u8]>),
    Name(Rc<str>),
    Arr(Rc<[Obj]>),
    Dict(Rc<Dict>),
    Stream(Rc<Stream>),
    Ref(u32, u16),
}

impl Obj {
    pub(super) fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Real(r) if r.is_finite() => Some(to_i64(*r)),
            _ => None,
        }
    }

    pub(super) fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(int_to_f64(*i)),
            Self::Real(r) if r.is_finite() => Some(*r),
            _ => None,
        }
    }

    pub(super) fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(n) => Some(n),
            _ => None,
        }
    }

    pub(super) fn as_str(&self) -> Option<&[u8]> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(super) fn as_arr(&self) -> Option<&[Obj]> {
        match self {
            Self::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// The dictionary of a dictionary or of a stream.
    pub(super) fn as_dict(&self) -> Option<&Dict> {
        match self {
            Self::Dict(d) => Some(d),
            Self::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub(super) fn as_stream(&self) -> Option<&Rc<Stream>> {
        match self {
            Self::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub(super) fn ref_num(&self) -> Option<u32> {
        match self {
            Self::Ref(n, _) => Some(*n),
            _ => None,
        }
    }

    pub(super) const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

/// Saturating conversion of a float to `i64` (NaN becomes 0).
#[allow(clippy::cast_possible_truncation)]
pub(super) fn to_i64(f: f64) -> i64 {
    if f.is_nan() { 0 } else { f.clamp(-9.0e15, 9.0e15).trunc() as i64 }
}

/// Exact for magnitudes below 2^53, which covers every number a PDF needs.
#[allow(clippy::cast_precision_loss)]
pub(super) const fn int_to_f64(i: i64) -> f64 {
    i as f64
}

/// A token of the PDF syntax.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok<'a> {
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(String),
    ArrOpen,
    ArrClose,
    DictOpen,
    DictClose,
    /// A bare word: keyword or content-stream operator.
    Kw(&'a [u8]),
    Eof,
}

/// PDF white-space characters.
pub(super) const fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

/// PDF delimiter characters.
pub(super) const fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

const fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Tokenizer over a byte slice.
#[derive(Debug, Clone)]
pub(super) struct Lexer<'a> {
    pub(super) data: &'a [u8],
    pub(super) pos: usize,
}

impl<'a> Lexer<'a> {
    pub(super) const fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Skips white space and comments.
    pub(super) fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Reads the next token.
    pub(super) fn next_tok(&mut self) -> Tok<'a> {
        self.skip_ws();
        let Some(b) = self.peek() else { return Tok::Eof };
        match b {
            b'[' => {
                self.pos += 1;
                Tok::ArrOpen
            }
            b']' => {
                self.pos += 1;
                Tok::ArrClose
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Tok::DictOpen
                } else {
                    self.pos += 1;
                    Tok::Str(self.hex_string())
                }
            }
            b'>' => {
                if self.data.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Tok::DictClose
                } else {
                    self.pos += 1;
                    self.next_tok()
                }
            }
            b'(' => {
                self.pos += 1;
                Tok::Str(self.literal_string())
            }
            b'/' => {
                self.pos += 1;
                Tok::Name(self.name())
            }
            b')' | b'{' | b'}' => {
                // Stray delimiter: skip.
                self.pos += 1;
                self.next_tok()
            }
            _ => self.word(),
        }
    }

    fn word(&mut self) -> Tok<'a> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
        }
        let w = self.data.get(start..self.pos).unwrap_or(&[]);
        parse_number(w).unwrap_or(Tok::Kw(w))
    }

    fn name(&mut self) -> String {
        let mut out = String::new();
        while let Some(b) = self.peek() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#'
                && let (Some(h), Some(l)) = (
                    self.data.get(self.pos).copied().and_then(hex_val),
                    self.data.get(self.pos + 1).copied().and_then(hex_val),
                )
            {
                self.pos += 2;
                out.push(char::from(h << 4 | l));
                continue;
            }
            out.push(char::from(b));
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let Some(v) = hex_val(b) else { continue };
            match hi.take() {
                Some(h) => out.push(h << 4 | v),
                None => hi = Some(v),
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(u8::try_from(v & 0xFF).unwrap_or(0));
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'\r' => {
                    // A bare CR or CRLF inside a string is one LF.
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// Parses one object. With `refs`, `N G R` is recognised as a reference.
    pub(super) fn object(&mut self, refs: bool) -> Option<Obj> {
        let mut budget = MAX_ELEMS;
        let tok = self.next_tok();
        self.object_from(tok, refs, 0, &mut budget)
    }

    /// Parses an object whose first token has been read. `None` on end of input or on a
    /// token that cannot start an object (a keyword, a closing bracket).
    pub(super) fn object_from(
        &mut self,
        tok: Tok<'a>,
        refs: bool,
        depth: usize,
        budget: &mut usize,
    ) -> Option<Obj> {
        *budget = budget.checked_sub(1)?;
        match tok {
            Tok::Int(i) => {
                if refs && (0..=i64::from(u32::MAX)).contains(&i) {
                    let save = self.pos;
                    if let Tok::Int(g) = self.next_tok()
                        && (0..=i64::from(u16::MAX)).contains(&g)
                        && self.next_tok() == Tok::Kw(b"R")
                    {
                        return Some(Obj::Ref(
                            u32::try_from(i).unwrap_or(0),
                            u16::try_from(g).unwrap_or(0),
                        ));
                    }
                    self.pos = save;
                }
                Some(Obj::Int(i))
            }
            Tok::Real(r) => Some(Obj::Real(r)),
            Tok::Str(s) => Some(Obj::Str(Rc::from(s))),
            Tok::Name(n) => Some(Obj::Name(Rc::from(n))),
            Tok::ArrOpen => {
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut items = Vec::new();
                loop {
                    let t = self.next_tok();
                    match t {
                        Tok::ArrClose | Tok::Eof => break,
                        Tok::Kw(k) => match k {
                            b"null" | b"true" | b"false" => {
                                items.push(self.keyword_obj(k));
                            }
                            // `endobj`, `stream`, ... : the array was never closed.
                            b"endobj" | b"endstream" | b"stream" | b"obj" => {
                                self.pos = self.pos.saturating_sub(k.len());
                                break;
                            }
                            _ => {}
                        },
                        Tok::DictClose => {}
                        t => match self.object_from(t, refs, depth + 1, budget) {
                            Some(o) => items.push(o),
                            None => break,
                        },
                    }
                }
                Some(Obj::Arr(Rc::from(items)))
            }
            Tok::DictOpen => {
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut map = Dict::new();
                loop {
                    let t = self.next_tok();
                    match t {
                        Tok::DictClose | Tok::Eof => break,
                        Tok::Name(key) => {
                            let save = self.pos;
                            let vt = self.next_tok();
                            let value = match vt {
                                Tok::DictClose => {
                                    map.insert(Rc::from(key), Obj::Null);
                                    break;
                                }
                                Tok::Eof => break,
                                Tok::Kw(k) => match k {
                                    b"null" | b"true" | b"false" => self.keyword_obj(k),
                                    b"endobj" | b"endstream" | b"stream" | b"obj" => {
                                        self.pos = save;
                                        break;
                                    }
                                    _ => Obj::Null,
                                },
                                vt => match self.object_from(vt, refs, depth + 1, budget) {
                                    Some(o) => o,
                                    None => break,
                                },
                            };
                            map.insert(Rc::from(key), value);
                        }
                        Tok::Kw(k) if matches!(k, b"endobj" | b"endstream" | b"stream") => {
                            self.pos = self.pos.saturating_sub(k.len());
                            break;
                        }
                        _ => {}
                    }
                }
                Some(Obj::Dict(Rc::new(map)))
            }
            Tok::Kw(k) => match k {
                b"null" | b"true" | b"false" => Some(self.keyword_obj(k)),
                _ => None,
            },
            Tok::ArrClose | Tok::DictClose | Tok::Eof => None,
        }
    }

    /// The object a keyword stands for. Only called for `null`, `true`, `false`.
    #[allow(clippy::unused_self)]
    fn keyword_obj(&self, k: &[u8]) -> Obj {
        match k {
            b"true" => Obj::Bool(true),
            b"false" => Obj::Bool(false),
            _ => Obj::Null,
        }
    }
}

/// Parses a PDF number: `+`/`-` sign, digits, optional fraction. Quirks seen in the wild
/// (`--5`, `4.`, `.5`, `-.002`, `1-2`) are tolerated; anything else is not a number.
fn parse_number(w: &[u8]) -> Option<Tok<'static>> {
    let mut i = 0usize;
    let mut neg = false;
    while let Some(&c) = w.get(i) {
        match c {
            b'-' => neg = true,
            b'+' => {}
            _ => break,
        }
        i += 1;
    }
    let mut int_part: i64 = 0;
    let mut saw_digit = false;
    while let Some(&c) = w.get(i) {
        if !c.is_ascii_digit() {
            break;
        }
        saw_digit = true;
        int_part = int_part.saturating_mul(10).saturating_add(i64::from(c - b'0'));
        i += 1;
    }
    let mut frac = 0.0f64;
    let mut is_real = false;
    if w.get(i) == Some(&b'.') {
        is_real = true;
        i += 1;
        let mut scale = 0.1f64;
        while let Some(&c) = w.get(i) {
            if !c.is_ascii_digit() {
                break;
            }
            saw_digit = true;
            frac += scale * f64::from(c - b'0');
            scale *= 0.1;
            i += 1;
        }
    }
    if !saw_digit {
        return None;
    }
    // Trailing garbage such as "1-2" (seen in broken producers): stop at the first sign.
    if let Some(&c) = w.get(i)
        && c != b'-'
        && c != b'+'
    {
        return None;
    }
    if is_real {
        let v = int_to_f64(int_part) + frac;
        Some(Tok::Real(if neg { -v } else { v }))
    } else {
        Some(Tok::Int(if neg { -int_part } else { int_part }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str, refs: bool) -> Option<Obj> {
        Lexer::new(s.as_bytes(), 0).object(refs)
    }

    fn toks(s: &str) -> Vec<Tok<'_>> {
        let mut l = Lexer::new(s.as_bytes(), 0);
        let mut v = Vec::new();
        loop {
            let t = l.next_tok();
            if t == Tok::Eof {
                return v;
            }
            v.push(t);
        }
    }

    #[test]
    fn numbers_and_quirks() {
        assert_eq!(
            toks("1 -2 +3 4. .5 -.25 --7 0"),
            vec![
                Tok::Int(1),
                Tok::Int(-2),
                Tok::Int(3),
                Tok::Real(4.0),
                Tok::Real(0.5),
                Tok::Real(-0.25),
                Tok::Int(-7),
                Tok::Int(0)
            ]
        );
        assert_eq!(toks("BT 12abc"), vec![Tok::Kw(b"BT"), Tok::Kw(b"12abc")]);
        assert_eq!(toks("99999999999999999999999"), vec![Tok::Int(i64::MAX)]);
    }

    #[test]
    fn strings() {
        assert_eq!(toks(r"(a\(b\)c(d)\101\n\\)"), vec![Tok::Str(b"a(b)c(d)A\n\\".to_vec())]);
        assert_eq!(toks("(line\\\r\ncont)"), vec![Tok::Str(b"linecont".to_vec())]);
        assert_eq!(toks("<48 65 6C6c 6F7>"), vec![Tok::Str(b"Hello\x70".to_vec())]);
        assert_eq!(toks("(unterminated"), vec![Tok::Str(b"unterminated".to_vec())]);
        assert_eq!(toks(r"(\777)"), vec![Tok::Str(vec![0xFF])]);
    }

    #[test]
    fn names_with_escapes() {
        assert_eq!(
            toks("/A#20B /Name#2F /"),
            vec![Tok::Name("A B".into()), Tok::Name("Name/".into()), Tok::Name(String::new())]
        );
    }

    #[test]
    fn objects_and_references() {
        let o = parse("<< /A 1 /B [1 0 R 2 (x) /N null true] /C << /D 5 >> >>", true);
        let Some(Obj::Dict(d)) = o else { panic!("dict") };
        assert_eq!(d.get("A"), Some(&Obj::Int(1)));
        let Some(Obj::Arr(a)) = d.get("B") else { panic!("arr") };
        assert_eq!(a.first(), Some(&Obj::Ref(1, 0)));
        assert_eq!(a.get(1), Some(&Obj::Int(2)));
        assert_eq!(a.get(4), Some(&Obj::Null));
        assert_eq!(a.get(5), Some(&Obj::Bool(true)));
        // Without reference recognition the same text is three integers... and a keyword.
        let Some(Obj::Arr(a)) = parse("[1 0 R]", false) else { panic!("arr") };
        assert_eq!(a.as_ref(), [Obj::Int(1), Obj::Int(0)]);
    }

    #[test]
    fn unterminated_containers_do_not_run_away() {
        assert!(parse("[1 2 3", true).is_some());
        assert!(parse("<< /A 1 /B", true).is_some());
        let deep = "[".repeat(10_000);
        let _ = parse(&deep, true);
        let deepd = "<</A".repeat(10_000);
        let _ = parse(&deepd, true);
        // An array broken off by `endobj` leaves the keyword for the caller.
        let mut l = Lexer::new(b"[1 2 endobj", 0);
        let _ = l.object(true);
        assert_eq!(l.next_tok(), Tok::Kw(b"endobj"));
    }

    #[test]
    fn element_budget_is_enforced() {
        let big = format!("[{}]", "1 ".repeat(MAX_ELEMS + 10));
        let Some(Obj::Arr(a)) = parse(&big, false) else { panic!("arr") };
        assert!(a.len() <= MAX_ELEMS);
    }

    #[test]
    fn comments_and_stray_delimiters() {
        assert_eq!(toks("1 % comment\n2 ) } 3"), vec![Tok::Int(1), Tok::Int(2), Tok::Int(3)]);
    }

    #[test]
    fn garbage_never_panics() {
        let mut x = 7u32;
        for len in 0..200 {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    b"[]<>()/ 0123456789.-+RaBT\\%#\r\n"[(x >> 16) as usize % 30]
                })
                .collect();
            let mut l = Lexer::new(&junk, 0);
            for _ in 0..(len + 5) {
                if l.next_tok() == Tok::Eof {
                    break;
                }
            }
            let _ = Lexer::new(&junk, 0).object(true);
        }
    }
}
