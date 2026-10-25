// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Spreadsheet formulas, calculated for cells that have no stored result.
//!
//! Files written by libraries (openpyxl, xlsxwriter, …) keep the text of a formula and no result;
//! a spreadsheet application would calculate it when the file is opened. This is a small, strictly
//! read-only calculator for exactly that case. A cell that has a stored result is never
//! recalculated.
//!
//! What it is:
//!
//! * **pure** — a formula can read cells of this workbook and nothing else: no files, network,
//!   clock, random numbers, external workbooks, defined names, add-ins, DDE or macros; a function
//!   that needs any of them (`WEBSERVICE`, `INDIRECT`, `TODAY`, `RAND`, `CALL`, …) is simply not
//!   known and the cell keeps showing its formula;
//! * **bounded** — formula length, nesting, the number of steps (also counting the cells that a
//!   range visits), the length of a chain of dependent cells and the length of produced text are
//!   all capped; going over any of them leaves the cell unevaluated, never wrong;
//! * **honest** — whatever is not understood (arrays, unknown functions, circular references,
//!   dates written as text) is [`E::Fail`]: the cell shows its formula text and nothing that
//!   depends on it is calculated either.
//!
//! Results follow Excel (error propagation, text/number/boolean ordering in comparisons, rounding
//! half away from zero after normalising to 15 significant digits).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use crate::numfmt;

/// Longest formula (in characters) that is looked at.
const MAX_FORMULA_CHARS: usize = 4096;
/// Longest text a lookup key or a criterion may have (Excel answers `#VALUE!` beyond it); it
/// bounds the cost of wildcard matching, which is the product of pattern and text lengths.
const MAX_KEY_CHARS: usize = 255;
/// Most tokens in one formula.
const MAX_TOKENS: usize = 2048;
/// Deepest nesting of parentheses and function calls.
const MAX_NESTING: u32 = 48;
/// Longest chain of cells that depend on one another (recursion depth of the calculator).
const MAX_CHAIN: u32 = 96;
/// Steps (expression nodes and visited cells) one workbook may cost.
const STEP_LIMIT: u64 = 150_000;
/// Longest text a formula may produce.
const MAX_TEXT_CHARS: usize = 32_767;
/// Last row and column of a sheet.
const MAX_ROW: u32 = 1_048_576;
const MAX_COL: u32 = 16_384;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// An error value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrCode {
    Null,
    Div0,
    Value,
    Ref,
    Name,
    Num,
    Na,
}

impl ErrCode {
    const ALL: [Self; 7] =
        [Self::Null, Self::Div0, Self::Value, Self::Ref, Self::Name, Self::Num, Self::Na];

    /// The text a spreadsheet shows.
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::Null => "#NULL!",
            Self::Div0 => "#DIV/0!",
            Self::Value => "#VALUE!",
            Self::Ref => "#REF!",
            Self::Name => "#NAME?",
            Self::Num => "#NUM!",
            Self::Na => "#N/A",
        }
    }

    pub(crate) fn from_text(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.text().eq_ignore_ascii_case(s))
    }
}

/// A cell value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    Empty,
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ErrCode),
}

/// Why a formula could not be calculated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fail {
    /// Something this calculator does not do (or would do differently from a spreadsheet).
    Unsupported,
    /// A chain of dependent cells longer than [`MAX_CHAIN`].
    Deep,
    /// The step budget is spent.
    Budget,
}

/// Both ways a calculation can end early: with an error *value* (which a formula can catch) or
/// by not being calculable at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum E {
    Err(ErrCode),
    Fail(Fail),
}

impl From<ErrCode> for E {
    fn from(e: ErrCode) -> Self {
        Self::Err(e)
    }
}

impl From<Fail> for E {
    fn from(f: Fail) -> Self {
        Self::Fail(f)
    }
}

type X<T> = Result<T, E>;

const UNSUPPORTED: E = E::Fail(Fail::Unsupported);

// ---------------------------------------------------------------------------
// Workbook model
// ---------------------------------------------------------------------------

/// One cell as the calculator sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cell {
    /// The stored value (empty for a cell whose formula has to be calculated).
    pub(crate) value: Val,
    /// The formula to calculate (without `=`); `Some("")` marks one that cannot be.
    pub(crate) formula: Option<String>,
}

/// One sheet: its non-empty cells by (row, column), one-based row and zero-based column.
#[derive(Debug, Default)]
pub(crate) struct Sheet {
    name: String,
    cells: BTreeMap<(u32, u32), Cell>,
    max_row: u32,
    max_col: u32,
    /// All cells of the sheet are here. When not, nothing may refer to it.
    complete: bool,
}

impl Sheet {
    pub(crate) fn new(name: &str, cells: BTreeMap<(u32, u32), Cell>, complete: bool) -> Self {
        let max_row = cells.keys().map(|k| k.0).max().unwrap_or(0);
        let max_col = cells.keys().map(|k| k.1).max().unwrap_or(0);
        Self { name: name.to_owned(), cells, max_row, max_col, complete }
    }
}

/// The sheets of a workbook that were read.
#[derive(Debug, Default)]
pub(crate) struct Book {
    sheets: Vec<Sheet>,
    index: HashMap<String, usize>,
    date1904: bool,
}

/// What came out.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// Results by (sheet index, row, column) — only for cells that were calculated.
    pub(crate) values: BTreeMap<(usize, u32, u32), Val>,
    /// Formulas that were looked at and could not be calculated.
    pub(crate) failed: u64,
}

impl Book {
    pub(crate) fn new(date1904: bool) -> Self {
        Self { date1904, ..Self::default() }
    }

    /// Adds a sheet; its index is the order of the calls.
    pub(crate) fn add(&mut self, sheet: Sheet) -> usize {
        let at = self.sheets.len();
        self.index.entry(sheet.name.to_lowercase()).or_insert(at);
        self.sheets.push(sheet);
        at
    }

    fn sheet(&self, i: usize) -> Option<&Sheet> {
        self.sheets.get(i)
    }

    /// Calculates every cell that holds a formula and no stored result.
    pub(crate) fn calculate(&self) -> Outcome {
        let mut ev = Eval { book: self, memo: BTreeMap::new(), steps: 0, depth: 0, sheet: 0 };
        let mut todo: Vec<(usize, u32, u32)> = Vec::new();
        for (si, sh) in self.sheets.iter().enumerate() {
            for (&(r, c), cell) in &sh.cells {
                if cell.formula.is_some() {
                    todo.push((si, r, c));
                }
            }
        }
        // Forward first: most formulas refer to cells above and to the left, so by the time
        // they are asked for their precedents are already known and the recursion stays shallow.
        // Cells that still hit the chain limit get a second go from the other end.
        let mut deep: Vec<(usize, u32, u32)> = Vec::new();
        for &(si, r, c) in &todo {
            if let Err(E::Fail(Fail::Deep)) = ev.cell(si, r, c) {
                deep.push((si, r, c));
            }
        }
        let deep: std::collections::HashSet<(usize, u32, u32)> = deep.into_iter().collect();
        for &(si, r, c) in todo.iter().rev() {
            if deep.contains(&(si, r, c)) {
                let _ = ev.cell(si, r, c);
            }
        }
        let mut out = Outcome::default();
        for &(si, r, c) in &todo {
            match ev.memo.get(&memo_key(si, r, c)) {
                Some(Memo::Done(v)) => {
                    let v = if *v == Val::Empty { Val::Num(0.0) } else { v.clone() };
                    out.values.insert((si, r, c), v);
                }
                _ => out.failed = out.failed.saturating_add(1),
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellTok {
    col: u32,
    row: u32,
    col_abs: bool,
    row_abs: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Err(ErrCode),
    Name(String),
    Cell(CellTok),
    /// `Sheet1!` or `'My sheet'!`.
    Sheet(String),
    Op(char),
    Ne,
    Le,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
struct Lexed {
    tok: Tok,
    start: usize,
    end: usize,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

fn column_number(letters: &str) -> Option<u32> {
    let mut n: u32 = 0;
    for b in letters.bytes() {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        n = n.checked_mul(26)?.checked_add(u32::from(b.to_ascii_uppercase() - b'A') + 1)?;
    }
    (1..=MAX_COL).contains(&n).then_some(n - 1)
}

/// A cell reference at char index `i`: `$?LETTERS$?DIGITS`, not followed by anything that would
/// make it part of a longer name, a function call or a sheet prefix.
fn match_cell(cs: &[(usize, char)], i: usize) -> Option<(CellTok, usize)> {
    let mut j = i;
    let col_abs = cs.get(j).is_some_and(|c| c.1 == '$');
    if col_abs {
        j += 1;
    }
    let mut letters = String::new();
    while let Some(&(_, c)) = cs.get(j) {
        if c.is_ascii_alphabetic() && letters.len() < 3 {
            letters.push(c);
            j += 1;
        } else {
            break;
        }
    }
    if letters.is_empty() {
        return None;
    }
    let row_abs = cs.get(j).is_some_and(|c| c.1 == '$');
    if row_abs {
        j += 1;
    }
    let mut digits = String::new();
    while let Some(&(_, c)) = cs.get(j) {
        if c.is_ascii_digit() && digits.len() < 8 {
            digits.push(c);
            j += 1;
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    if let Some(&(_, c)) = cs.get(j)
        && (is_ident_char(c) || c == '(' || c == '!' || c == '$')
    {
        return None;
    }
    let col = column_number(&letters)?;
    let row: u32 = digits.parse().ok()?;
    if !(1..=MAX_ROW).contains(&row) {
        return None;
    }
    Some((CellTok { col, row, col_abs, row_abs }, j))
}

fn lex(src: &str) -> Option<Vec<Lexed>> {
    // The byte test first: counting the characters of a megabyte formula is itself work, and a
    // file may ask for it once per cell.
    if src.len() > MAX_FORMULA_CHARS * 4 || src.chars().count() > MAX_FORMULA_CHARS {
        return None;
    }
    let cs: Vec<(usize, char)> = src.char_indices().collect();
    let at = |k: usize| cs.get(k).map_or(src.len(), |c| c.0);
    let mut out: Vec<Lexed> = Vec::new();
    let mut i = 0;
    while let Some(&(start, c)) = cs.get(i) {
        if out.len() > MAX_TOKENS {
            return None;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let (tok, next) = match c {
            '"' => {
                let mut s = String::new();
                let mut j = i + 1;
                loop {
                    let &(_, ch) = cs.get(j)?;
                    if ch == '"' {
                        if cs.get(j + 1).is_some_and(|n| n.1 == '"') {
                            s.push('"');
                            j += 2;
                            continue;
                        }
                        j += 1;
                        break;
                    }
                    s.push(ch);
                    j += 1;
                }
                (Tok::Str(s), j)
            }
            '\'' => {
                let mut s = String::new();
                let mut j = i + 1;
                loop {
                    let &(_, ch) = cs.get(j)?;
                    if ch == '\'' {
                        if cs.get(j + 1).is_some_and(|n| n.1 == '\'') {
                            s.push('\'');
                            j += 2;
                            continue;
                        }
                        j += 1;
                        break;
                    }
                    s.push(ch);
                    j += 1;
                }
                if cs.get(j).map(|c| c.1) != Some('!') {
                    return None;
                }
                (Tok::Sheet(s), j + 1)
            }
            '#' => {
                let rest = src.get(start..)?;
                let code = ErrCode::ALL.into_iter().find(|e| {
                    rest.get(..e.text().len()).is_some_and(|h| h.eq_ignore_ascii_case(e.text()))
                })?;
                (Tok::Err(code), i + code.text().chars().count())
            }
            '0'..='9' | '.' => {
                let mut j = i;
                let mut seen_digit = false;
                while cs.get(j).is_some_and(|c| c.1.is_ascii_digit()) {
                    j += 1;
                    seen_digit = true;
                }
                if cs.get(j).is_some_and(|c| c.1 == '.') {
                    j += 1;
                    while cs.get(j).is_some_and(|c| c.1.is_ascii_digit()) {
                        j += 1;
                        seen_digit = true;
                    }
                }
                if !seen_digit {
                    return None;
                }
                if cs.get(j).is_some_and(|c| c.1 == 'e' || c.1 == 'E') {
                    let mut k = j + 1;
                    if cs.get(k).is_some_and(|c| c.1 == '+' || c.1 == '-') {
                        k += 1;
                    }
                    if cs.get(k).is_some_and(|c| c.1.is_ascii_digit()) {
                        while cs.get(k).is_some_and(|c| c.1.is_ascii_digit()) {
                            k += 1;
                        }
                        j = k;
                    }
                }
                let n: f64 = src.get(at(i)..at(j))?.parse().ok()?;
                if !n.is_finite() {
                    return None;
                }
                (Tok::Num(n), j)
            }
            '$' | 'A'..='Z' | 'a'..='z' | '_' | '\\' => {
                if let Some((cell, j)) = match_cell(&cs, i) {
                    (Tok::Cell(cell), j)
                } else if c == '$' {
                    return None;
                } else {
                    let mut j = i;
                    while cs.get(j).is_some_and(|c| is_ident_char(c.1)) {
                        j += 1;
                    }
                    if j == i {
                        return None;
                    }
                    let name = src.get(at(i)..at(j))?.to_owned();
                    if cs.get(j).is_some_and(|c| c.1 == '!') {
                        (Tok::Sheet(name), j + 1)
                    } else {
                        (Tok::Name(name), j)
                    }
                }
            }
            c if c.is_alphabetic() => {
                // A name in another script (a sheet called "Лист1").
                let mut j = i;
                while cs.get(j).is_some_and(|c| is_ident_char(c.1)) {
                    j += 1;
                }
                let name = src.get(at(i)..at(j))?.to_owned();
                if cs.get(j).is_some_and(|c| c.1 == '!') {
                    (Tok::Sheet(name), j + 1)
                } else {
                    (Tok::Name(name), j)
                }
            }
            '<' => match cs.get(i + 1).map(|c| c.1) {
                Some('>') => (Tok::Ne, i + 2),
                Some('=') => (Tok::Le, i + 2),
                _ => (Tok::Op('<'), i + 1),
            },
            '>' => {
                if cs.get(i + 1).is_some_and(|c| c.1 == '=') {
                    (Tok::Ge, i + 2)
                } else {
                    (Tok::Op('>'), i + 1)
                }
            }
            '+' | '-' | '*' | '/' | '^' | '&' | '=' | '(' | ')' | ',' | ':' | '%' => {
                (Tok::Op(c), i + 1)
            }
            _ => return None,
        };
        out.push(Lexed { tok, start, end: at(next) });
        i = next;
    }
    Some(out)
}

/// The same formula as it reads in a cell `rows` down and `cols` to the right (shared
/// formulas). `None` when that moves a reference off the sheet or the formula is not understood.
pub(crate) fn shifted(src: &str, rows: i64, cols: i64) -> Option<String> {
    let toks = lex(src)?;
    let mut out = String::new();
    let mut last = 0;
    for (k, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Cell(c) => {
                let row = if c.row_abs { i64::from(c.row) } else { i64::from(c.row) + rows };
                let col = if c.col_abs { i64::from(c.col) } else { i64::from(c.col) + cols };
                if !(1..=i64::from(MAX_ROW)).contains(&row)
                    || !(0..i64::from(MAX_COL)).contains(&col)
                {
                    return None;
                }
                out.push_str(src.get(last..t.start)?);
                if c.col_abs {
                    out.push('$');
                }
                out.push_str(&column_name(u32::try_from(col).ok()?));
                if c.row_abs {
                    out.push('$');
                }
                out.push_str(&row.to_string());
                last = t.end;
            }
            // `A:A` and `1:1` would need shifting too; do not guess.
            Tok::Name(n)
                if column_number(n).is_some()
                    && toks.get(k + 1).is_some_and(|x| x.tok == Tok::Op(':')) =>
            {
                return None;
            }
            Tok::Num(_) if toks.get(k + 1).is_some_and(|x| x.tok == Tok::Op(':')) => {
                return None;
            }
            _ => {}
        }
    }
    out.push_str(src.get(last..)?);
    Some(out)
}

fn column_name(mut col: u32) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(char::from(b'A' + u8::try_from(col % 26).unwrap_or(0)));
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    letters.iter().rev().collect()
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Cat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    /// An omitted argument: `IF(A1,,2)`.
    Missing,
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ErrCode),
    Ref {
        sheet: Option<String>,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    },
    Neg(Box<Expr>),
    Pct(Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

struct Parser {
    toks: Vec<Lexed>,
    pos: usize,
    depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k).map(|t| &t.tok)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos)?.tok.clone();
        self.pos += 1;
        Some(t)
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Tok::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Option<()> {
        self.depth += 1;
        (self.depth <= MAX_NESTING).then_some(())
    }

    fn expr(&mut self) -> Option<Expr> {
        self.enter()?;
        let mut left = self.concat()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op('=')) => Bin::Eq,
                Some(Tok::Ne) => Bin::Ne,
                Some(Tok::Op('<')) => Bin::Lt,
                Some(Tok::Le) => Bin::Le,
                Some(Tok::Op('>')) => Bin::Gt,
                Some(Tok::Ge) => Bin::Ge,
                _ => break,
            };
            self.pos += 1;
            let right = self.concat()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        self.depth -= 1;
        Some(left)
    }

    fn concat(&mut self) -> Option<Expr> {
        let mut left = self.add()?;
        while self.eat('&') {
            let right = self.add()?;
            left = Expr::Bin(Bin::Cat, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn add(&mut self) -> Option<Expr> {
        let mut left = self.mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op('+')) => Bin::Add,
                Some(Tok::Op('-')) => Bin::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.mul()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn mul(&mut self) -> Option<Expr> {
        let mut left = self.pow()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op('*')) => Bin::Mul,
                Some(Tok::Op('/')) => Bin::Div,
                _ => break,
            };
            self.pos += 1;
            let right = self.pow()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn pow(&mut self) -> Option<Expr> {
        let mut left = self.unary()?;
        while self.eat('^') {
            let right = self.unary()?;
            left = Expr::Bin(Bin::Pow, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    /// In a spreadsheet a minus sign binds tighter than `^`: `-2^2` is 4.
    fn unary(&mut self) -> Option<Expr> {
        if self.eat('-') {
            self.enter()?;
            let inner = self.unary()?;
            self.depth -= 1;
            return Some(Expr::Neg(Box::new(inner)));
        }
        if self.eat('+') {
            self.enter()?;
            let inner = self.unary()?;
            self.depth -= 1;
            return Some(inner);
        }
        let mut e = self.primary()?;
        while self.eat('%') {
            e = Expr::Pct(Box::new(e));
        }
        Some(e)
    }

    fn primary(&mut self) -> Option<Expr> {
        match self.next()? {
            Tok::Num(n) => {
                // `1:3` is a range of rows.
                if self.peek() == Some(&Tok::Op(':')) {
                    let Some(&Tok::Num(m)) = self.peek_at(1) else {
                        return None;
                    };
                    if n.fract() != 0.0 || m.fract() != 0.0 || n < 1.0 || m < 1.0 {
                        return None;
                    }
                    self.pos += 2;
                    return Some(Expr::Ref {
                        sheet: None,
                        r1: to_u32(n.min(m))?,
                        c1: 0,
                        r2: to_u32(n.max(m))?,
                        c2: MAX_COL - 1,
                    });
                }
                Some(Expr::Num(n))
            }
            Tok::Str(s) => Some(Expr::Str(s)),
            Tok::Err(e) => Some(Expr::Err(e)),
            Tok::Op('(') => {
                let e = self.expr()?;
                self.eat(')').then_some(e)
            }
            Tok::Sheet(name) => self.reference(Some(name)),
            Tok::Cell(c) => self.reference_from_cell(None, c),
            Tok::Name(n) => {
                if self.peek() == Some(&Tok::Op('(')) {
                    self.pos += 1;
                    return self.call(&n);
                }
                if n.eq_ignore_ascii_case("TRUE") {
                    return Some(Expr::Bool(true));
                }
                if n.eq_ignore_ascii_case("FALSE") {
                    return Some(Expr::Bool(false));
                }
                // `A:C`, a range of whole columns.
                if column_number(&n).is_some() && self.peek() == Some(&Tok::Op(':')) {
                    self.pos -= 1;
                    return self.reference(None);
                }
                // A defined name: this calculator does not read them.
                None
            }
            _ => None,
        }
    }

    /// A reference after an optional sheet prefix (the prefix is already consumed).
    fn reference(&mut self, sheet: Option<String>) -> Option<Expr> {
        match self.next()? {
            Tok::Cell(c) => self.reference_from_cell(sheet, c),
            Tok::Name(n) => {
                let c1 = column_number(&n)?;
                if !self.eat(':') {
                    return None;
                }
                let Tok::Name(m) = self.next()? else {
                    return None;
                };
                let c2 = column_number(&m)?;
                Some(Expr::Ref { sheet, r1: 1, c1: c1.min(c2), r2: MAX_ROW, c2: c1.max(c2) })
            }
            _ => None,
        }
    }

    /// The cell token `c` has just been read; a `:` and a second cell make it a range.
    fn reference_from_cell(&mut self, sheet: Option<String>, c: CellTok) -> Option<Expr> {
        if self.eat(':') {
            let Tok::Cell(d) = self.next()? else {
                return None;
            };
            Some(Expr::Ref {
                sheet,
                r1: c.row.min(d.row),
                c1: c.col.min(d.col),
                r2: c.row.max(d.row),
                c2: c.col.max(d.col),
            })
        } else {
            Some(Expr::Ref { sheet, r1: c.row, c1: c.col, r2: c.row, c2: c.col })
        }
    }

    fn call(&mut self, name: &str) -> Option<Expr> {
        self.enter()?;
        let mut args = Vec::new();
        if self.eat(')') {
            self.depth -= 1;
            return Some(Expr::Call(canonical(name), args));
        }
        loop {
            match self.peek() {
                Some(Tok::Op(',')) | Some(Tok::Op(')')) => args.push(Expr::Missing),
                _ => args.push(self.expr()?),
            }
            if self.eat(',') {
                continue;
            }
            if self.eat(')') {
                break;
            }
            return None;
        }
        self.depth -= 1;
        Some(Expr::Call(canonical(name), args))
    }
}

/// A whole non-negative number as `u32`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_u32(x: f64) -> Option<u32> {
    (x >= 0.0 && x <= f64::from(u32::MAX)).then_some(x as u32)
}

/// Float to index; negative and NaN become 0, huge values saturate (the semantics of `as`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_usize(x: f64) -> usize {
    x as usize
}

#[allow(clippy::cast_possible_truncation)]
fn to_i64(x: f64) -> i64 {
    x as i64
}

#[allow(clippy::cast_possible_truncation)]
fn to_i32(x: f64) -> i32 {
    x as i32
}

/// Function name in upper case without the prefixes newer functions are stored with.
fn canonical(name: &str) -> String {
    let up = name.to_uppercase();
    for prefix in ["_XLFN._XLWS.", "_XLFN.", "_XLWS."] {
        if let Some(rest) = up.strip_prefix(prefix) {
            return rest.to_owned();
        }
    }
    up
}

fn parse(src: &str) -> Option<Expr> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return None;
    }
    let mut p = Parser { toks, pos: 0, depth: 0 };
    let e = p.expr()?;
    (p.pos == p.toks.len()).then_some(e)
}

// ---------------------------------------------------------------------------
// Calculator
// ---------------------------------------------------------------------------

/// Sheet, row (21 bits) and column (14 bits) in one number.
fn memo_key(sheet: usize, row: u32, col: u32) -> u64 {
    (u64::try_from(sheet).unwrap_or(0) << 40) | (u64::from(row) << 16) | u64::from(col)
}

#[derive(Debug, Clone)]
enum Memo {
    Busy,
    Done(Val),
    Failed,
}

#[derive(Debug, Clone, Copy)]
struct Rng {
    sheet: usize,
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
}

impl Rng {
    fn single(self) -> bool {
        self.r1 == self.r2 && self.c1 == self.c2
    }

    fn rows(self) -> u32 {
        self.r2 - self.r1 + 1
    }

    fn cols(self) -> u32 {
        self.c2 - self.c1 + 1
    }
}

enum Opd {
    V(Val),
    R(Rng),
}

struct Eval<'a> {
    book: &'a Book,
    memo: BTreeMap<u64, Memo>,
    steps: u64,
    depth: u32,
    /// The sheet of the formula being calculated.
    sheet: usize,
}

/// An error value or a failure, as early return from a function body.
fn err<T>(e: ErrCode) -> X<T> {
    Err(E::Err(e))
}

impl Eval<'_> {
    fn step(&mut self, n: u64) -> X<()> {
        self.steps = self.steps.saturating_add(n);
        if self.steps > STEP_LIMIT {
            return Err(E::Fail(Fail::Budget));
        }
        Ok(())
    }

    /// The value of a cell, calculating its formula if it has one without a stored result.
    fn cell(&mut self, si: usize, r: u32, c: u32) -> X<Val> {
        let Some(sheet) = self.book.sheet(si) else {
            return err(ErrCode::Ref);
        };
        let Some(cell) = sheet.cells.get(&(r, c)) else {
            return Ok(Val::Empty);
        };
        let Some(src) = cell.formula.as_deref() else {
            return Ok(cell.value.clone());
        };
        match self.memo.get(&memo_key(si, r, c)) {
            Some(Memo::Done(v)) => return Ok(v.clone()),
            Some(Memo::Failed) | Some(Memo::Busy) => return Err(UNSUPPORTED),
            None => {}
        }
        if self.depth >= MAX_CHAIN {
            return Err(E::Fail(Fail::Deep));
        }
        self.step(1)?;
        self.memo.insert(memo_key(si, r, c), Memo::Busy);
        let saved = (self.sheet, self.depth);
        self.sheet = si;
        self.depth += 1;
        let expr = parse(src);
        let result = match &expr {
            Some(e) => self.eval(e).and_then(|o| self.scalar(o)),
            None => Err(UNSUPPORTED),
        };
        (self.sheet, self.depth) = saved;
        match result {
            Ok(v) => {
                self.memo.insert(memo_key(si, r, c), Memo::Done(v.clone()));
                Ok(v)
            }
            // An error value is a result like any other.
            Err(E::Err(code)) => {
                let v = Val::Err(code);
                self.memo.insert(memo_key(si, r, c), Memo::Done(v.clone()));
                Ok(v)
            }
            Err(E::Fail(Fail::Unsupported)) => {
                self.memo.insert(memo_key(si, r, c), Memo::Failed);
                Err(UNSUPPORTED)
            }
            Err(E::Fail(f)) => {
                // Not the formula's fault: leave it to be tried again.
                self.memo.remove(&memo_key(si, r, c));
                Err(E::Fail(f))
            }
        }
    }

    fn scalar(&mut self, o: Opd) -> X<Val> {
        match o {
            Opd::V(v) => Ok(v),
            Opd::R(r) if r.single() => self.cell(r.sheet, r.r1, r.c1),
            // Implicit intersection and arrays are what a spreadsheet would do differently.
            Opd::R(_) => Err(UNSUPPORTED),
        }
    }

    fn value(&mut self, e: &Expr) -> X<Val> {
        let o = self.eval(e)?;
        self.scalar(o)
    }

    fn eval(&mut self, e: &Expr) -> X<Opd> {
        self.step(1)?;
        match e {
            Expr::Missing => Ok(Opd::V(Val::Empty)),
            Expr::Num(n) => Ok(Opd::V(Val::Num(*n))),
            Expr::Str(s) => Ok(Opd::V(Val::Str(s.clone()))),
            Expr::Bool(b) => Ok(Opd::V(Val::Bool(*b))),
            Expr::Err(c) => Ok(Opd::V(Val::Err(*c))),
            Expr::Ref { .. } => {
                let rng = self.ref_range(e)?;
                if rng.single() {
                    Ok(Opd::V(self.cell(rng.sheet, rng.r1, rng.c1)?))
                } else {
                    Ok(Opd::R(rng))
                }
            }
            Expr::Neg(inner) => {
                let v = self.value(inner)?;
                let n = self.num(&v)?;
                Ok(Opd::V(Val::Num(-n)))
            }
            Expr::Pct(inner) => {
                let v = self.value(inner)?;
                let n = self.num(&v)?;
                Ok(Opd::V(Val::Num(finite(n / 100.0)?)))
            }
            Expr::Bin(op, a, b) => {
                let l = self.value(a)?;
                let r = self.value(b)?;
                self.binary(*op, &l, &r).map(Opd::V)
            }
            Expr::Call(name, args) => match self.call(name, args) {
                Ok(o) => Ok(o),
                Err(E::Err(c)) => Ok(Opd::V(Val::Err(c))),
                Err(f) => Err(f),
            },
        }
    }

    /// The cells a reference names, whatever their number.
    fn ref_range(&mut self, e: &Expr) -> X<Rng> {
        let Expr::Ref { sheet, r1, c1, r2, c2 } = e else {
            return Err(UNSUPPORTED);
        };
        let si = match sheet {
            None => self.sheet,
            Some(name) => match self.book.index.get(&name.to_lowercase()) {
                Some(&i) => i,
                // A sheet that was not read (or does not exist).
                None => return Err(UNSUPPORTED),
            },
        };
        if !self.book.sheet(si).is_some_and(|sh| sh.complete) {
            return Err(UNSUPPORTED);
        }
        Ok(Rng { sheet: si, r1: *r1, c1: *c1, r2: *r2, c2: *c2 })
    }

    /// Like [`Self::eval`], but a reference stays a reference even when it names one cell: a
    /// function treats the contents of a cell it is pointed at differently from a typed value
    /// (`SUM` skips text in a cell and refuses text typed in).
    fn operand(&mut self, e: &Expr) -> X<Opd> {
        if matches!(e, Expr::Ref { .. }) {
            self.step(1)?;
            self.ref_range(e).map(Opd::R)
        } else {
            self.eval(e)
        }
    }

    // ---- coercions -----------------------------------------------------------------------

    fn num(&self, v: &Val) -> X<f64> {
        match v {
            Val::Empty => Ok(0.0),
            Val::Num(n) => Ok(*n),
            Val::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Val::Err(c) => err(*c),
            Val::Str(s) => text_to_number(s),
        }
    }

    fn text(&self, v: &Val) -> X<String> {
        match v {
            Val::Empty => Ok(String::new()),
            Val::Num(n) => Ok(numfmt::general(*n)),
            Val::Str(s) => Ok(s.clone()),
            Val::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_owned()),
            Val::Err(c) => err(*c),
        }
    }

    fn truth(&self, v: &Val) -> X<bool> {
        match v {
            Val::Empty => Ok(false),
            Val::Num(n) => Ok(*n != 0.0),
            Val::Bool(b) => Ok(*b),
            Val::Err(c) => err(*c),
            Val::Str(s) => {
                if s.eq_ignore_ascii_case("TRUE") {
                    Ok(true)
                } else if s.eq_ignore_ascii_case("FALSE") {
                    Ok(false)
                } else {
                    err(ErrCode::Value)
                }
            }
        }
    }

    fn binary(&self, op: Bin, l: &Val, r: &Val) -> X<Val> {
        match op {
            Bin::Cat => {
                let mut s = self.text(l)?;
                s.push_str(&self.text(r)?);
                if s.chars().count() > MAX_TEXT_CHARS {
                    return err(ErrCode::Value);
                }
                Ok(Val::Str(s))
            }
            Bin::Eq | Bin::Ne | Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                let ord = compare(l, r)?;
                Ok(Val::Bool(match op {
                    Bin::Eq => ord == Ordering::Equal,
                    Bin::Ne => ord != Ordering::Equal,
                    Bin::Lt => ord == Ordering::Less,
                    Bin::Le => ord != Ordering::Greater,
                    Bin::Gt => ord == Ordering::Greater,
                    _ => ord != Ordering::Less,
                }))
            }
            Bin::Add | Bin::Sub | Bin::Mul | Bin::Div | Bin::Pow => {
                let a = self.num(l)?;
                let b = self.num(r)?;
                match op {
                    Bin::Add => Ok(Val::Num(finite(a + b)?)),
                    Bin::Sub => Ok(Val::Num(finite(a - b)?)),
                    Bin::Mul => Ok(Val::Num(finite(a * b)?)),
                    Bin::Div => {
                        if b == 0.0 {
                            err(ErrCode::Div0)
                        } else {
                            Ok(Val::Num(finite(a / b)?))
                        }
                    }
                    _ => power(a, b).map(Val::Num),
                }
            }
        }
    }

    // ---- ranges --------------------------------------------------------------------------

    /// The non-empty cells of a range in row-major order, with their positions.
    fn cells_of(&mut self, r: Rng) -> X<Vec<((u32, u32), Val)>> {
        let book = self.book;
        let sh = book.sheet(r.sheet).ok_or(UNSUPPORTED)?;
        let mut out = Vec::new();
        for (&(row, col), cell) in sh.cells.range((r.r1, 0)..=(r.r2, u32::MAX)) {
            // Cells beside the range are walked past, so they are charged too: a narrow column
            // range over a wide sheet is as much work as a wide one.
            self.step(1)?;
            if col < r.c1 || col > r.c2 {
                continue;
            }
            let v = match cell.formula {
                None => cell.value.clone(),
                Some(_) => self.cell(r.sheet, row, col)?,
            };
            if v != Val::Empty {
                out.push(((row, col), v));
            }
        }
        Ok(out)
    }

    /// How much of a vector range lies within the used part of its sheet: lookups over a whole
    /// column stop where the data stops.
    fn used(&self, r: Rng, vertical: bool) -> u32 {
        let Some(sh) = self.book.sheet(r.sheet) else {
            return 0;
        };
        let (len, top, max) =
            if vertical { (r.rows(), r.r1, sh.max_row) } else { (r.cols(), r.c1, sh.max_col) };
        if max < top { 0 } else { len.min(max - top + 1) }
    }

    fn cell_at(&mut self, r: Rng, row_off: u32, col_off: u32) -> X<Val> {
        self.cell(r.sheet, r.r1 + row_off, r.c1 + col_off)
    }

    /// Every argument flattened: `(from_a_range, value)`; empty cells of ranges are left out.
    fn flat(&mut self, args: &[Expr]) -> X<Vec<(bool, Val)>> {
        let mut out = Vec::new();
        for a in args {
            match self.operand(a)? {
                Opd::V(v) => out.push((false, v)),
                Opd::R(r) => {
                    for (_, v) in self.cells_of(r)? {
                        out.push((true, v));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Like [`Self::flat`], but an argument that is itself an error stays a value instead of
    /// ending the calculation (`COUNT(1/0)` is 0, `COUNTA(1/0)` is 1).
    fn flat_lenient(&mut self, args: &[Expr]) -> X<Vec<(bool, Val)>> {
        let mut out = Vec::new();
        for a in args {
            match self.flat(std::slice::from_ref(a)) {
                Ok(v) => out.extend(v),
                Err(E::Err(c)) => out.push((false, Val::Err(c))),
                Err(f) => return Err(f),
            }
        }
        Ok(out)
    }

    /// Numbers for SUM-like functions: from ranges only real numbers, directly typed arguments
    /// are coerced; errors anywhere propagate.
    fn numbers(&mut self, args: &[Expr]) -> X<Vec<f64>> {
        let mut out = Vec::new();
        for (from_range, v) in self.flat(args)? {
            match (&v, from_range) {
                (Val::Err(c), _) => return err(*c),
                (Val::Num(n), _) => out.push(*n),
                (_, true) => {}
                (Val::Bool(b), false) => out.push(if *b { 1.0 } else { 0.0 }),
                (Val::Empty, false) => {}
                (Val::Str(s), false) => out.push(text_to_number(s)?),
            }
        }
        Ok(out)
    }

    fn arg_val(&mut self, args: &[Expr], i: usize) -> X<Val> {
        match args.get(i) {
            Some(e) => self.value(e),
            None => Ok(Val::Empty),
        }
    }

    fn arg_num(&mut self, args: &[Expr], i: usize) -> X<f64> {
        let v = self.arg_val(args, i)?;
        self.num(&v)
    }

    fn arg_text(&mut self, args: &[Expr], i: usize) -> X<String> {
        let v = self.arg_val(args, i)?;
        self.text(&v)
    }

    fn arg_opt_num(&mut self, args: &[Expr], i: usize, default: f64) -> X<f64> {
        match args.get(i) {
            None | Some(Expr::Missing) => Ok(default),
            Some(_) => self.arg_num(args, i),
        }
    }

    fn arg_range(&mut self, args: &[Expr], i: usize) -> X<Rng> {
        match args.get(i).map(|e| self.operand(e)).transpose()? {
            Some(Opd::R(r)) => Ok(r),
            Some(Opd::V(_)) | None => Err(UNSUPPORTED),
        }
    }

    // ---- functions -----------------------------------------------------------------------

    fn call(&mut self, name: &str, args: &[Expr]) -> X<Opd> {
        let argc = args.len();
        let need = |lo: usize, hi: usize| -> X<()> {
            if (lo..=hi).contains(&argc) { Ok(()) } else { Err(UNSUPPORTED) }
        };
        let num = |n: f64| -> X<Opd> { Ok(Opd::V(Val::Num(finite(n)?))) };
        let text = |s: String| -> X<Opd> {
            if s.chars().count() > MAX_TEXT_CHARS {
                err(ErrCode::Value)
            } else {
                Ok(Opd::V(Val::Str(s)))
            }
        };
        let boolean = |b: bool| -> X<Opd> { Ok(Opd::V(Val::Bool(b))) };
        match name {
            // ---- logic ----
            "TRUE" => {
                need(0, 0)?;
                boolean(true)
            }
            "FALSE" => {
                need(0, 0)?;
                boolean(false)
            }
            "IF" => {
                need(2, 3)?;
                let c = self.arg_val(args, 0)?;
                if self.truth(&c)? {
                    self.branch(args.get(1))
                } else if argc == 3 {
                    self.branch(args.get(2))
                } else {
                    boolean(false)
                }
            }
            "IFERROR" | "IFNA" => {
                need(2, 2)?;
                let first = args.first().ok_or(UNSUPPORTED)?;
                let caught = match self.eval(first).and_then(|o| self.scalar(o)) {
                    Ok(Val::Err(c)) | Err(E::Err(c)) => Some(c),
                    Ok(v) => return Ok(Opd::V(v)),
                    Err(f) => return Err(f),
                };
                match caught {
                    Some(ErrCode::Na) => self.branch(args.get(1)),
                    Some(_) if name == "IFERROR" => self.branch(args.get(1)),
                    Some(c) => err(c),
                    None => Err(UNSUPPORTED),
                }
            }
            "AND" | "OR" | "XOR" => {
                need(1, 255)?;
                let mut seen = false;
                let mut acc = name == "AND";
                let mut parity = false;
                for (from_range, v) in self.flat(args)? {
                    let b = match (&v, from_range) {
                        (Val::Err(c), _) => return err(*c),
                        (Val::Str(_) | Val::Empty, true) => continue,
                        (Val::Empty, false) => false,
                        _ => self.truth(&v)?,
                    };
                    seen = true;
                    match name {
                        "AND" => acc &= b,
                        "OR" => acc |= b,
                        _ => parity ^= b,
                    }
                }
                if !seen {
                    return err(ErrCode::Value);
                }
                boolean(if name == "XOR" { parity } else { acc })
            }
            "NOT" => {
                need(1, 1)?;
                let v = self.arg_val(args, 0)?;
                boolean(!self.truth(&v)?)
            }
            "CHOOSE" => {
                need(2, 255)?;
                let i = self.arg_num(args, 0)?.trunc();
                if i < 1.0 || i >= argc as f64 {
                    return err(ErrCode::Value);
                }
                self.branch(args.get(to_usize(i)))
            }
            // ---- information ----
            "ISNUMBER" | "ISTEXT" | "ISBLANK" | "ISLOGICAL" | "ISERROR" | "ISERR" | "ISNA"
            | "ISNONTEXT" => {
                need(1, 1)?;
                let v = match args.first().map(|e| self.eval(e).and_then(|o| self.scalar(o))) {
                    Some(Ok(v)) => v,
                    Some(Err(E::Err(c))) => Val::Err(c),
                    Some(Err(f)) => return Err(f),
                    None => return Err(UNSUPPORTED),
                };
                boolean(match name {
                    "ISNUMBER" => matches!(v, Val::Num(_)),
                    "ISTEXT" => matches!(v, Val::Str(_)),
                    "ISNONTEXT" => !matches!(v, Val::Str(_)),
                    "ISBLANK" => v == Val::Empty,
                    "ISLOGICAL" => matches!(v, Val::Bool(_)),
                    "ISERROR" => matches!(v, Val::Err(_)),
                    "ISERR" => matches!(v, Val::Err(c) if c != ErrCode::Na),
                    _ => v == Val::Err(ErrCode::Na),
                })
            }
            // ---- aggregates ----
            "SUM" => {
                need(1, 255)?;
                num(self.numbers(args)?.iter().sum())
            }
            "PRODUCT" => {
                need(1, 255)?;
                let ns = self.numbers(args)?;
                num(if ns.is_empty() { 0.0 } else { ns.iter().product() })
            }
            "SUMSQ" => {
                need(1, 255)?;
                num(self.numbers(args)?.iter().map(|x| x * x).sum())
            }
            "AVERAGE" => {
                need(1, 255)?;
                let ns = self.numbers(args)?;
                if ns.is_empty() {
                    return err(ErrCode::Div0);
                }
                num(ns.iter().sum::<f64>() / ns.len() as f64)
            }
            "MIN" | "MAX" => {
                need(1, 255)?;
                let ns = self.numbers(args)?;
                let pick = if name == "MIN" {
                    ns.iter()
                        .copied()
                        .fold(None, |a: Option<f64>, x| Some(a.map_or(x, |a| a.min(x))))
                } else {
                    ns.iter()
                        .copied()
                        .fold(None, |a: Option<f64>, x| Some(a.map_or(x, |a| a.max(x))))
                };
                num(pick.unwrap_or(0.0))
            }
            "MEDIAN" => {
                need(1, 255)?;
                let mut ns = self.numbers(args)?;
                if ns.is_empty() {
                    return err(ErrCode::Num);
                }
                ns.sort_by(f64::total_cmp);
                let mid = ns.len() / 2;
                let hi = *ns.get(mid).ok_or(UNSUPPORTED)?;
                if ns.len() % 2 == 1 {
                    num(hi)
                } else {
                    let lo = *ns.get(mid - 1).ok_or(UNSUPPORTED)?;
                    num((lo + hi) / 2.0)
                }
            }
            "LARGE" | "SMALL" => {
                need(2, 2)?;
                let r = self.arg_range(args, 0)?;
                let k = self.arg_num(args, 1)?.trunc();
                let mut ns: Vec<f64> = Vec::new();
                for (_, v) in self.cells_of(r)? {
                    match v {
                        Val::Num(n) => ns.push(n),
                        Val::Err(c) => return err(c),
                        _ => {}
                    }
                }
                if k < 1.0 || k > ns.len() as f64 {
                    return err(ErrCode::Num);
                }
                ns.sort_by(f64::total_cmp);
                if name == "LARGE" {
                    ns.reverse();
                }
                num(*ns.get(to_usize(k) - 1).ok_or(UNSUPPORTED)?)
            }
            "COUNT" => {
                need(1, 255)?;
                let mut n = 0u32;
                for (from_range, v) in self.flat_lenient(args)? {
                    match (&v, from_range) {
                        (Val::Num(_), _) => n += 1,
                        (Val::Bool(_), false) => n += 1,
                        (Val::Str(s), false) if text_to_number(s).is_ok() => n += 1,
                        _ => {}
                    }
                }
                num(f64::from(n))
            }
            "COUNTA" => {
                need(1, 255)?;
                let n = self.flat_lenient(args)?.iter().filter(|(_, v)| *v != Val::Empty).count();
                num(n as f64)
            }
            "COUNTBLANK" => {
                need(1, 1)?;
                let r = self.arg_range(args, 0)?;
                let area = u64::from(r.rows()) * u64::from(r.cols());
                let filled = self
                    .cells_of(r)?
                    .iter()
                    .filter(|(_, v)| !matches!(v, Val::Str(s) if s.is_empty()))
                    .count();
                num(area.saturating_sub(filled as u64) as f64)
            }
            "SUMPRODUCT" => {
                need(1, 30)?;
                let mut rngs = Vec::new();
                for i in 0..argc {
                    rngs.push(self.arg_range(args, i)?);
                }
                let first = *rngs.first().ok_or(UNSUPPORTED)?;
                if rngs.iter().any(|r| r.rows() != first.rows() || r.cols() != first.cols()) {
                    return err(ErrCode::Value);
                }
                let mut total = 0.0;
                // Only the cells that the first range holds can contribute: the rest multiply
                // by an empty cell.
                for ((row, col), v) in self.cells_of(first)? {
                    let mut prod = match v {
                        Val::Num(n) => n,
                        Val::Err(c) => return err(c),
                        _ => 0.0,
                    };
                    for r in rngs.iter().skip(1) {
                        self.step(1)?;
                        prod *= match self.cell(
                            r.sheet,
                            r.r1 + (row - first.r1),
                            r.c1 + (col - first.c1),
                        )? {
                            Val::Num(n) => n,
                            Val::Err(c) => return err(c),
                            _ => 0.0,
                        };
                    }
                    total += prod;
                }
                num(total)
            }
            "SUMIF" | "COUNTIF" | "AVERAGEIF" => {
                if name == "COUNTIF" {
                    need(2, 2)?;
                } else {
                    need(2, 3)?;
                }
                let r = self.arg_range(args, 0)?;
                let crit = self.arg_val(args, 1)?;
                let crit = Criterion::new(&crit)?;
                let sum_r =
                    if name == "COUNTIF" || argc < 3 { r } else { self.arg_range(args, 2)? };
                let mut total = 0.0;
                let mut n = 0u32;
                // The offsets (row, column) from the top left of the range of every cell that
                // meets the criterion. A blank cell meets "<>x", and the cell beside it in the
                // sum range may hold a number, so for such criteria blanks are visited as well.
                let mut hits: Vec<(u32, u32)> = Vec::new();
                if crit.matches_empty() {
                    let rows = self.used(r, true).max(self.used(sum_r, true)).min(r.rows());
                    let cols = self.used(r, false).max(self.used(sum_r, false)).min(r.cols());
                    for dr in 0..rows {
                        for dc in 0..cols {
                            self.step(1)?;
                            let v = self.cell_at(r, dr, dc)?;
                            if crit.matches(&v) {
                                hits.push((dr, dc));
                            }
                        }
                    }
                    if name == "COUNTIF" {
                        // The blanks outside the used part of the sheet count too.
                        let area = u64::from(r.rows()) * u64::from(r.cols());
                        let visited = u64::from(rows) * u64::from(cols);
                        n = n.saturating_add(
                            u32::try_from(area.saturating_sub(visited)).unwrap_or(u32::MAX),
                        );
                    }
                } else {
                    for ((row, col), v) in self.cells_of(r)? {
                        if crit.matches(&v) {
                            hits.push((row - r.r1, col - r.c1));
                        }
                    }
                }
                for (dr, dc) in hits {
                    n = n.saturating_add(1);
                    if name != "COUNTIF" {
                        match self.cell_at(sum_r, dr, dc)? {
                            Val::Num(x) => total += x,
                            Val::Err(c) => return err(c),
                            _ => n -= 1,
                        }
                    }
                }
                match name {
                    "COUNTIF" => num(f64::from(n)),
                    "SUMIF" => num(total),
                    _ if n == 0 => err(ErrCode::Div0),
                    _ => num(total / f64::from(n)),
                }
            }
            // ---- arithmetic ----
            "ABS" => {
                need(1, 1)?;
                num(self.arg_num(args, 0)?.abs())
            }
            "SIGN" => {
                need(1, 1)?;
                let x = self.arg_num(args, 0)?;
                num(if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                })
            }
            "SQRT" => {
                need(1, 1)?;
                let x = self.arg_num(args, 0)?;
                if x < 0.0 { err(ErrCode::Num) } else { num(x.sqrt()) }
            }
            "EXP" => {
                need(1, 1)?;
                num(self.arg_num(args, 0)?.exp())
            }
            "LN" | "LOG10" => {
                need(1, 1)?;
                let x = self.arg_num(args, 0)?;
                if x <= 0.0 {
                    err(ErrCode::Num)
                } else {
                    num(if name == "LN" { x.ln() } else { x.log10() })
                }
            }
            "LOG" => {
                need(1, 2)?;
                let x = self.arg_num(args, 0)?;
                let b = self.arg_opt_num(args, 1, 10.0)?;
                if x <= 0.0 || b <= 0.0 || b == 1.0 {
                    err(ErrCode::Num)
                } else {
                    num(x.ln() / b.ln())
                }
            }
            "NA" => {
                need(0, 0)?;
                err(ErrCode::Na)
            }
            "PI" => {
                need(0, 0)?;
                num(std::f64::consts::PI)
            }
            "POWER" => {
                need(2, 2)?;
                let a = self.arg_num(args, 0)?;
                let b = self.arg_num(args, 1)?;
                num(power(a, b)?)
            }
            "MOD" => {
                need(2, 2)?;
                let n = self.arg_num(args, 0)?;
                let d = self.arg_num(args, 1)?;
                if d == 0.0 { err(ErrCode::Div0) } else { num(n - d * (n / d).floor()) }
            }
            "INT" => {
                need(1, 1)?;
                num(norm15(self.arg_num(args, 0)?).floor())
            }
            "TRUNC" | "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                if name == "TRUNC" {
                    need(1, 2)?;
                } else {
                    need(2, 2)?;
                }
                let x = self.arg_num(args, 0)?;
                let d = self.arg_opt_num(args, 1, 0.0)?.trunc();
                if d.abs() > 15.0 {
                    return Err(UNSUPPORTED);
                }
                let mode = match name {
                    "ROUND" => Rounding::Nearest,
                    "ROUNDUP" => Rounding::Up,
                    _ => Rounding::Down,
                };
                num(round_to(x, to_i32(d), mode))
            }
            // ---- text ----
            "LEN" => {
                need(1, 1)?;
                num(self.arg_text(args, 0)?.chars().count() as f64)
            }
            "UPPER" => {
                need(1, 1)?;
                text(self.arg_text(args, 0)?.to_uppercase())
            }
            "LOWER" => {
                need(1, 1)?;
                text(self.arg_text(args, 0)?.to_lowercase())
            }
            "TRIM" => {
                need(1, 1)?;
                let s = self.arg_text(args, 0)?;
                text(s.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))
            }
            "PROPER" => {
                need(1, 1)?;
                let s = self.arg_text(args, 0)?;
                let mut out = String::new();
                let mut start = true;
                for c in s.chars() {
                    if c.is_alphabetic() {
                        if start {
                            out.extend(c.to_uppercase());
                        } else {
                            out.extend(c.to_lowercase());
                        }
                        start = false;
                    } else {
                        out.push(c);
                        start = true;
                    }
                }
                text(out)
            }
            "LEFT" | "RIGHT" => {
                need(1, 2)?;
                let s = self.arg_text(args, 0)?;
                let n = self.arg_opt_num(args, 1, 1.0)?.trunc();
                if n < 0.0 {
                    return err(ErrCode::Value);
                }
                let n = to_usize(n.min(1e9));
                let total = s.chars().count();
                let out: String = if name == "LEFT" {
                    s.chars().take(n).collect()
                } else {
                    s.chars().skip(total.saturating_sub(n)).collect()
                };
                text(out)
            }
            "MID" => {
                need(3, 3)?;
                let s = self.arg_text(args, 0)?;
                let start = self.arg_num(args, 1)?.trunc();
                let n = self.arg_num(args, 2)?.trunc();
                if start < 1.0 || n < 0.0 {
                    return err(ErrCode::Value);
                }
                text(
                    s.chars()
                        .skip(to_usize(start.min(1e9)) - 1)
                        .take(to_usize(n.min(1e9)))
                        .collect(),
                )
            }
            "CONCAT" | "CONCATENATE" => {
                need(1, 255)?;
                let mut s = String::new();
                for (_, v) in self.flat(args)? {
                    s.push_str(&self.text(&v)?);
                    if s.chars().count() > MAX_TEXT_CHARS {
                        return err(ErrCode::Value);
                    }
                }
                text(s)
            }
            "TEXTJOIN" => {
                need(3, 255)?;
                let delim = self.arg_text(args, 0)?;
                let skip_empty = {
                    let v = self.arg_val(args, 1)?;
                    self.truth(&v)?
                };
                let mut parts = Vec::new();
                for (_, v) in self.flat(args.get(2..).unwrap_or_default())? {
                    let t = self.text(&v)?;
                    if !(skip_empty && t.is_empty()) {
                        parts.push(t);
                    }
                }
                text(parts.join(&delim))
            }
            "EXACT" => {
                need(2, 2)?;
                boolean(self.arg_text(args, 0)? == self.arg_text(args, 1)?)
            }
            "REPT" => {
                need(2, 2)?;
                let s = self.arg_text(args, 0)?;
                let n = self.arg_num(args, 1)?.trunc();
                if n < 0.0 || s.chars().count() as f64 * n > MAX_TEXT_CHARS as f64 {
                    return err(ErrCode::Value);
                }
                text(s.repeat(to_usize(n)))
            }
            "SUBSTITUTE" => {
                need(3, 4)?;
                let s = self.arg_text(args, 0)?;
                let old = self.arg_text(args, 1)?;
                let new = self.arg_text(args, 2)?;
                if old.is_empty() {
                    return text(s);
                }
                let out = if argc == 4 {
                    let which = self.arg_num(args, 3)?.trunc();
                    if which < 1.0 {
                        return err(ErrCode::Value);
                    }
                    let mut out = String::new();
                    let mut rest = s.as_str();
                    let mut seen = 0.0;
                    while let Some(at) = rest.find(&old) {
                        seen += 1.0;
                        let (head, tail) = rest.split_at(at);
                        out.push_str(head);
                        let tail = tail.get(old.len()..).unwrap_or_default();
                        if seen == which {
                            out.push_str(&new);
                            out.push_str(tail);
                            rest = "";
                            break;
                        }
                        out.push_str(&old);
                        rest = tail;
                    }
                    out.push_str(rest);
                    out
                } else {
                    s.replace(&old, &new)
                };
                text(out)
            }
            "FIND" | "SEARCH" => {
                need(2, 3)?;
                let pat = self.arg_text(args, 0)?;
                let within = self.arg_text(args, 1)?;
                let start = self.arg_opt_num(args, 2, 1.0)?.trunc();
                if start < 1.0 || start > within.chars().count() as f64 + 1.0 {
                    return err(ErrCode::Value);
                }
                if name == "SEARCH" && pat.contains(['*', '?', '~']) {
                    return Err(UNSUPPORTED);
                }
                let (pat, hay) = if name == "SEARCH" {
                    (pat.to_lowercase(), within.to_lowercase())
                } else {
                    (pat, within)
                };
                let hay_chars: Vec<char> = hay.chars().collect();
                let pat_chars: Vec<char> = pat.chars().collect();
                let from = to_usize(start) - 1;
                for i in from..=hay_chars.len().saturating_sub(pat_chars.len()).max(from) {
                    if hay_chars.get(i..i + pat_chars.len()) == Some(pat_chars.as_slice()) {
                        return num((i + 1) as f64);
                    }
                }
                err(ErrCode::Value)
            }
            "VALUE" => {
                need(1, 1)?;
                let v = self.arg_val(args, 0)?;
                match v {
                    Val::Str(ref s) => num(text_to_number(s)?),
                    other => num(self.num(&other)?),
                }
            }
            "TEXT" => {
                need(2, 2)?;
                let v = self.arg_val(args, 0)?;
                let fmt = self.arg_text(args, 1)?;
                if fmt.chars().count() > 128 {
                    return Err(UNSUPPORTED);
                }
                let f = numfmt::parse(&fmt);
                match v {
                    Val::Num(n) => text(f.number(n, self.book.date1904).0),
                    Val::Bool(_) | Val::Empty => {
                        let n = self.num(&v)?;
                        text(f.number(n, self.book.date1904).0)
                    }
                    Val::Str(s) => text(f.text(&s)),
                    Val::Err(c) => err(c),
                }
            }
            // ---- dates ----
            "DATE" => {
                need(3, 3)?;
                let y = self.arg_num(args, 0)?.trunc();
                let m = self.arg_num(args, 1)?.trunc();
                let d = self.arg_num(args, 2)?.trunc();
                if !(0.0..10000.0).contains(&y) || m.abs() > 1e6 || d.abs() > 1e7 {
                    return err(ErrCode::Num);
                }
                let y = if y < 1900.0 { y + 1900.0 } else { y };
                let serial =
                    numfmt::serial_from_civil(to_i64(y), to_i64(m), to_i64(d), self.book.date1904)
                        .ok_or(UNSUPPORTED)?;
                num(serial)
            }
            "YEAR" | "MONTH" | "DAY" | "WEEKDAY" => {
                need(1, if name == "WEEKDAY" { 2 } else { 1 })?;
                let serial = self.arg_num(args, 0)?;
                let Some(dt) = numfmt::serial_to_datetime(serial.floor(), self.book.date1904)
                else {
                    return err(ErrCode::Num);
                };
                match name {
                    "YEAR" => num(dt.year as f64),
                    "MONTH" => num(f64::from(dt.month)),
                    "DAY" => num(f64::from(dt.day)),
                    _ => {
                        let kind = self.arg_opt_num(args, 1, 1.0)?.trunc();
                        let wd = dt.weekday;
                        match to_i64(kind) {
                            1 => num(f64::from(wd + 1)),
                            2 => num(f64::from((wd + 6) % 7 + 1)),
                            3 => num(f64::from((wd + 6) % 7)),
                            _ => err(ErrCode::Num),
                        }
                    }
                }
            }
            // ---- lookup ----
            "ROWS" | "COLUMNS" => {
                need(1, 1)?;
                match args.first().map(|e| self.operand(e)).transpose()? {
                    Some(Opd::R(r)) => {
                        num(f64::from(if name == "ROWS" { r.rows() } else { r.cols() }))
                    }
                    Some(Opd::V(_)) => num(1.0),
                    None => Err(UNSUPPORTED),
                }
            }
            "INDEX" => {
                need(2, 3)?;
                let r = self.arg_range(args, 0)?;
                let i = self.arg_num(args, 1)?.trunc();
                let j = self.arg_opt_num(args, 2, 0.0)?.trunc();
                let (row, col) = match (r.rows(), r.cols(), argc) {
                    (_, 1, 2) => (i, 1.0),
                    (1, _, 2) => (1.0, i),
                    (_, _, 3) => (i, j),
                    _ => return Err(UNSUPPORTED),
                };
                if row < 1.0 || col < 1.0 || row > f64::from(r.rows()) || col > f64::from(r.cols())
                {
                    return err(ErrCode::Ref);
                }
                Ok(Opd::V(self.cell_at(
                    r,
                    to_u32(row).ok_or(UNSUPPORTED)? - 1,
                    to_u32(col).ok_or(UNSUPPORTED)? - 1,
                )?))
            }
            "MATCH" => {
                need(2, 3)?;
                let key = lookup_key(self.arg_val(args, 0)?)?;
                let r = self.arg_range(args, 1)?;
                let kind = self.arg_opt_num(args, 2, 1.0)?.trunc();
                if r.rows() != 1 && r.cols() != 1 {
                    return err(ErrCode::Na);
                }
                let n = self.used(r, r.cols() == 1);
                let mut hit: Option<u32> = None;
                for k in 0..n {
                    self.step(1)?;
                    let (dr, dc) = if r.cols() == 1 { (k, 0) } else { (0, k) };
                    let v = self.cell_at(r, dr, dc)?;
                    if kind == 0.0 {
                        if exact_equal(&key, &v) {
                            hit = Some(k);
                            break;
                        }
                    } else if same_kind(&key, &v) {
                        let ord = compare(&v, &key)?;
                        let ok = if kind > 0.0 {
                            ord != Ordering::Greater
                        } else {
                            ord != Ordering::Less
                        };
                        if ok {
                            hit = Some(k);
                        } else {
                            break;
                        }
                    }
                }
                match hit {
                    Some(k) => num(f64::from(k + 1)),
                    None => err(ErrCode::Na),
                }
            }
            "VLOOKUP" | "HLOOKUP" => {
                need(3, 4)?;
                let key = lookup_key(self.arg_val(args, 0)?)?;
                let r = self.arg_range(args, 1)?;
                let idx = self.arg_num(args, 2)?.trunc();
                let approx = match args.get(3) {
                    None | Some(Expr::Missing) => true,
                    Some(_) => {
                        let v = self.arg_val(args, 3)?;
                        self.truth(&v)?
                    }
                };
                let vertical = name == "VLOOKUP";
                let width = if vertical { r.cols() } else { r.rows() };
                let len = self.used(r, vertical);
                if idx < 1.0 {
                    return err(ErrCode::Value);
                }
                if idx > f64::from(width) {
                    return err(ErrCode::Ref);
                }
                let at = |k: u32, off: u32| if vertical { (k, off) } else { (off, k) };
                let mut hit: Option<u32> = None;
                for k in 0..len {
                    self.step(1)?;
                    let (dr, dc) = at(k, 0);
                    let v = self.cell_at(r, dr, dc)?;
                    if approx {
                        if same_kind(&key, &v) {
                            if compare(&v, &key)? != Ordering::Greater {
                                hit = Some(k);
                            } else {
                                break;
                            }
                        }
                    } else if exact_equal(&key, &v) {
                        hit = Some(k);
                        break;
                    }
                }
                let Some(k) = hit else {
                    return err(ErrCode::Na);
                };
                let (dr, dc) = at(k, to_u32(idx).ok_or(UNSUPPORTED)? - 1);
                Ok(Opd::V(self.cell_at(r, dr, dc)?))
            }
            // Anything else — including every function that would read the clock, a random
            // number, another file or the network — is not calculated.
            _ => Err(UNSUPPORTED),
        }
    }

    /// An argument that is only evaluated when it is chosen.
    fn branch(&mut self, e: Option<&Expr>) -> X<Opd> {
        match e {
            None | Some(Expr::Missing) => Ok(Opd::V(Val::Num(0.0))),
            Some(e) => self.eval(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn finite(x: f64) -> X<f64> {
    if x.is_finite() { Ok(x) } else { err(ErrCode::Num) }
}

fn power(a: f64, b: f64) -> X<f64> {
    if a == 0.0 && b == 0.0 {
        return err(ErrCode::Num);
    }
    if a == 0.0 && b < 0.0 {
        return err(ErrCode::Div0);
    }
    if a < 0.0 && b.fract() != 0.0 {
        return err(ErrCode::Num);
    }
    finite(a.powf(b))
}

/// A number the way a spreadsheet keeps it: 15 significant digits.
fn norm15(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    format!("{x:.14e}").parse().unwrap_or(x)
}

#[derive(Debug, Clone, Copy)]
enum Rounding {
    Nearest,
    Up,
    Down,
}

fn round_to(x: f64, digits: i32, mode: Rounding) -> f64 {
    let scale = 10f64.powi(digits.abs());
    let y = if digits >= 0 { norm15(x * scale) } else { norm15(x / scale) };
    let r = match mode {
        Rounding::Nearest => y.round(),
        Rounding::Up => y.signum() * y.abs().ceil(),
        Rounding::Down => y.trunc(),
    };
    let out = if digits >= 0 { r / scale } else { r * scale };
    if out == 0.0 { 0.0 } else { out }
}

/// A number from text, as a spreadsheet reads one in an arithmetic expression. Text that a
/// spreadsheet might still read as a date, a currency or the like is not decided here.
fn text_to_number(s: &str) -> X<f64> {
    let t = s.trim();
    let (body, pct) = match t.strip_suffix('%') {
        Some(b) => (b.trim_end(), true),
        None => (t, false),
    };
    let plain = !body.is_empty()
        && body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        && body.chars().any(|c| c.is_ascii_digit());
    if plain && let Ok(n) = body.parse::<f64>() {
        let n = if pct { n / 100.0 } else { n };
        return if n.is_finite() { Ok(n) } else { err(ErrCode::Value) };
    }
    if t.chars().any(|c| c.is_ascii_digit()) {
        // "2020-01-01", "1,234", "$5", "12:30": a spreadsheet may well read these.
        Err(UNSUPPORTED)
    } else {
        err(ErrCode::Value)
    }
}

fn rank(v: &Val) -> u8 {
    match v {
        Val::Num(_) | Val::Empty => 0,
        Val::Str(_) => 1,
        Val::Bool(_) => 2,
        Val::Err(_) => 3,
    }
}

/// Ordering as a spreadsheet compares: numbers before text before booleans; text without regard
/// to case; an empty cell is 0, "" or FALSE depending on the other side.
fn compare(a: &Val, b: &Val) -> X<Ordering> {
    if let Val::Err(c) = a {
        return err(*c);
    }
    if let Val::Err(c) = b {
        return err(*c);
    }
    let (a, b) = match (a, b) {
        (Val::Empty, Val::Str(_)) => (&Val::Str(String::new()), b),
        (Val::Str(_), Val::Empty) => (a, &Val::Str(String::new())),
        (Val::Empty, Val::Bool(_)) => (&Val::Bool(false), b),
        (Val::Bool(_), Val::Empty) => (a, &Val::Bool(false)),
        _ => (a, b),
    };
    Ok(match (a, b) {
        (Val::Str(x), Val::Str(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Val::Bool(x), Val::Bool(y)) => x.cmp(y),
        _ if rank(a) != rank(b) => rank(a).cmp(&rank(b)),
        _ => {
            let x = match a {
                Val::Num(n) => *n,
                _ => 0.0,
            };
            let y = match b {
                Val::Num(n) => *n,
                _ => 0.0,
            };
            x.partial_cmp(&y).unwrap_or(Ordering::Equal)
        }
    })
}

fn same_kind(a: &Val, b: &Val) -> bool {
    rank(a) == rank(b) && !matches!(b, Val::Empty)
}

/// A lookup key; text longer than Excel allows is `#VALUE!`.
fn lookup_key(v: Val) -> X<Val> {
    match &v {
        Val::Str(s) if s.chars().nth(MAX_KEY_CHARS).is_some() => err(ErrCode::Value),
        _ => Ok(v),
    }
}

/// Exact-match lookup: same kind, equal; text may use `*` and `?`.
fn exact_equal(key: &Val, cell: &Val) -> bool {
    match (key, cell) {
        (Val::Str(k), Val::Str(c)) => wildcard_match(&k.to_lowercase(), &c.to_lowercase()),
        (Val::Num(k), Val::Num(c)) => k == c,
        (Val::Bool(k), Val::Bool(c)) => k == c,
        (Val::Empty, Val::Num(c)) => *c == 0.0,
        _ => false,
    }
}

/// `*` any run, `?` any one character, `~` quotes the next one.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Iterative matching with backtracking to the last `*`; cost is bounded by |p|·|t|.
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some('?') => {
                pi += 1;
                ti += 1;
            }
            Some('~') if p.get(pi + 1).is_some() => {
                if p.get(pi + 1) == t.get(ti) {
                    pi += 2;
                    ti += 1;
                } else if let Some((sp, st)) = star {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                } else {
                    return false;
                }
            }
            Some(c) if Some(c) == t.get(ti) => {
                pi += 1;
                ti += 1;
            }
            _ => {
                if let Some((sp, st)) = star {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                } else {
                    return false;
                }
            }
        }
    }
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
}

/// The criterion of `SUMIF` and friends: `5`, `">5"`, `"<>x"`, `"a*"`.
struct Criterion {
    op: Bin,
    rhs: Val,
}

impl Criterion {
    fn new(v: &Val) -> X<Self> {
        match v {
            Val::Err(c) => err(*c),
            Val::Str(s) if s.chars().nth(MAX_KEY_CHARS + 2).is_some() => err(ErrCode::Value),
            Val::Str(s) => {
                let (op, rest) = ["<>", "<=", ">=", "<", ">", "="]
                    .iter()
                    .find_map(|p| s.strip_prefix(p).map(|r| (*p, r)))
                    .unwrap_or(("=", s.as_str()));
                let op = match op {
                    "<>" => Bin::Ne,
                    "<=" => Bin::Le,
                    ">=" => Bin::Ge,
                    "<" => Bin::Lt,
                    ">" => Bin::Gt,
                    _ => Bin::Eq,
                };
                let trimmed = rest.trim();
                let rhs = match trimmed.parse::<f64>() {
                    Ok(n) if n.is_finite() && !trimmed.is_empty() => Val::Num(n),
                    _ if trimmed.eq_ignore_ascii_case("TRUE") => Val::Bool(true),
                    _ if trimmed.eq_ignore_ascii_case("FALSE") => Val::Bool(false),
                    _ => Val::Str(rest.to_owned()),
                };
                Ok(Self { op, rhs })
            }
            other => Ok(Self { op: Bin::Eq, rhs: other.clone() }),
        }
    }

    /// Whether a blank cell meets the criterion: `"<>x"` and `""` do, `0` and `">1"` do not.
    fn matches_empty(&self) -> bool {
        match (&self.rhs, self.op) {
            (Val::Str(p), Bin::Eq) => p.is_empty(),
            (Val::Str(p), Bin::Ne) => !p.is_empty(),
            (_, Bin::Ne) => true,
            _ => false,
        }
    }

    fn matches(&self, cell: &Val) -> bool {
        if matches!(cell, Val::Err(_)) {
            return false;
        }
        if *cell == Val::Empty {
            return self.matches_empty();
        }
        if let (Val::Str(p), Val::Str(s)) = (&self.rhs, cell)
            && matches!(self.op, Bin::Eq | Bin::Ne)
        {
            let m = wildcard_match(&p.to_lowercase(), &s.to_lowercase());
            return m == (self.op == Bin::Eq);
        }
        if rank(&self.rhs) != rank(cell) || matches!(cell, Val::Empty) {
            // Different kinds are never equal and never ordered, but "not equal" holds.
            return self.op == Bin::Ne;
        }
        match compare(cell, &self.rhs) {
            Ok(ord) => match self.op {
                Bin::Eq => ord == Ordering::Equal,
                Bin::Ne => ord != Ordering::Equal,
                Bin::Lt => ord == Ordering::Less,
                Bin::Le => ord != Ordering::Greater,
                Bin::Gt => ord == Ordering::Greater,
                _ => ord != Ordering::Less,
            },
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests;
