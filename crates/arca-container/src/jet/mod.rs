// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! A bounds-checked reader for Jet 3 (Access 97), Jet 4 (Access 2000–2003) and ACE (Access
//! 2007 and later) database files.
//!
//! The file is a sequence of fixed-size pages. Page 0 is the header (partly obfuscated with a
//! fixed mask), page 2 holds the definition of the system catalog `MSysObjects`, which lists
//! every other object. A table is a definition page (its columns and the usage map of its data
//! pages) and data pages holding rows; long text and binary values live in other pages and are
//! reached through a pointer stored in the row.
//!
//! Nothing here interprets queries, forms, macros or modules, and nothing is executed. Indexes
//! are ignored: rows are read page by page.

use crate::bytes::{u16_le, u32_le};
use crate::text::{CodePage, decode_codepage, utf16le_lossy};

/// Column types (the byte in the column definition).
pub mod ty {
    pub const BOOL: u8 = 0x01;
    pub const BYTE: u8 = 0x02;
    pub const INT: u8 = 0x03;
    pub const LONG: u8 = 0x04;
    pub const MONEY: u8 = 0x05;
    pub const FLOAT: u8 = 0x06;
    pub const DOUBLE: u8 = 0x07;
    pub const DATE: u8 = 0x08;
    pub const TEXT: u8 = 0x0A;
    pub const OLE: u8 = 0x0B;
    pub const MEMO: u8 = 0x0C;
    pub const GUID: u8 = 0x0F;
    pub const NUMERIC: u8 = 0x10;
    pub const COMPLEX: u8 = 0x12;
    pub const BIGINT: u8 = 0x13;
    pub const EXT_DATE: u8 = 0x17;
}

const PAGE_DATA: u8 = 0x01;
const PAGE_TABLE: u8 = 0x02;

const ROW_OFFSET: u16 = 0x0FFF;
const ROW_DELETED: u16 = 0x4000;
const ROW_OVERFLOW: u16 = 0x2000;

const FLAG_FIXED: u8 = 0x01;

/// The page holding the catalog table definition is always page 2.
const CATALOG_PAGE: u32 = 2;

/// Most rows scanned across all pages of one table (protection against hostile files that loop
/// or declare immense sparse allocations).
const MAX_ROWS_SCANNED: u64 = 1_000_000;

/// Header obfuscation mask applied to bytes 24..150 of page 0.
#[rustfmt::skip]
const HEADER_MASK: [u8; 128] = [
    0x86, 0xFB, 0xEC, 0x37, 0x5D, 0x44, 0x9C, 0xFA, 0xC6, 0x5E, 0x28, 0xE6, 0x13, 0xB6, 0x8A, 0x60,
    0x54, 0x94, 0x7B, 0x36, 0xF5, 0x72, 0xDF, 0xF1, 0xB7, 0x63, 0x2D, 0x4B, 0x83, 0x01, 0x21, 0x80,
    0x4F, 0x61, 0x77, 0x7E, 0xAF, 0x24, 0x8A, 0x3A, 0x09, 0x0C, 0xA1, 0xBF, 0xA8, 0x0C, 0xDE, 0xF8,
    0xA3, 0x29, 0x2D, 0xD4, 0x96, 0x9F, 0x82, 0x91, 0x9F, 0x37, 0xFA, 0x95, 0xCE, 0x53, 0x27, 0x47,
    0x95, 0xAE, 0xD5, 0xE3, 0xAE, 0x56, 0x18, 0x75, 0x3C, 0x5B, 0x02, 0x7E, 0x02, 0x49, 0x7B, 0x22,
    0x71, 0xB9, 0x02, 0x5F, 0x59, 0x47, 0x04, 0x64, 0xA1, 0x6B, 0xB2, 0xD7, 0x0C, 0x70, 0x89, 0x11,
    0x39, 0x75, 0x3A, 0xB7, 0x45, 0x24, 0xB8, 0xEE, 0x62, 0x7E, 0x9A, 0x56, 0x84, 0x30, 0xAA, 0x6B,
    0x40, 0xA7, 0x3E, 0xD3, 0x03, 0x20, 0x26, 0x00, 0x5E, 0xAA, 0xF6, 0x99, 0x04, 0xA9, 0x56, 0xD5,
];

/// Offsets that differ between Jet 3 (Access 97) and Jet 4 / ACE (2000 and later).
#[derive(Clone, Copy)]
struct Layout {
    jet3: bool,
    page_size: usize,
    // Header.
    mask_len: usize,
    password_len: usize,
    // Table definition.
    num_rows: usize,
    num_cols: usize,
    num_indexes: usize,
    owned_pages: usize,
    index_def_block: usize,
    index_def_size: usize,
    column_header_size: usize,
    // Column definition.
    col_number: usize,
    col_var_index: usize,
    col_flags: usize,
    col_fixed_offset: usize,
    col_length: usize,
    col_scale: usize,
    // Data page.
    page_rows: usize,
    page_row_start: usize,
    // Row.
    name_len_size: usize,
    row_cols_size: usize,
}

const JET3: Layout = Layout {
    jet3: true,
    page_size: 2048,
    mask_len: 126,
    password_len: 20,
    num_rows: 12,
    num_cols: 25,
    num_indexes: 31,
    owned_pages: 35,
    index_def_block: 43,
    index_def_size: 8,
    column_header_size: 18,
    col_number: 1,
    col_var_index: 3,
    col_flags: 13,
    col_fixed_offset: 14,
    col_length: 16,
    col_scale: 12,
    page_rows: 8,
    page_row_start: 10,
    name_len_size: 1,
    row_cols_size: 1,
};

const JET4: Layout = Layout {
    jet3: false,
    page_size: 4096,
    mask_len: 128,
    password_len: 40,
    num_rows: 16,
    num_cols: 45,
    num_indexes: 51,
    owned_pages: 55,
    index_def_block: 63,
    index_def_size: 12,
    column_header_size: 25,
    col_number: 5,
    col_var_index: 7,
    col_flags: 15,
    col_fixed_offset: 21,
    col_length: 23,
    col_scale: 12,
    page_rows: 12,
    page_row_start: 14,
    name_len_size: 2,
    row_cols_size: 2,
};

/// Why a database could not be opened.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Encrypted or protected with a password.
    Encrypted,
    /// Not a structure this reader understands.
    Malformed(&'static str),
}

/// One value of a row.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Single(f32),
    Double(f64),
    /// Currency in ten-thousandths.
    Money(i64),
    /// Days since 1899-12-30 (the fraction is the time of day).
    Date(f64),
    Text(String),
    /// A hyperlink: the text shown and the address.
    Link(String, String),
    /// Binary data of this many bytes (not read).
    Binary(usize),
    /// An embedded object of this many bytes (not read).
    Ole(usize),
    Guid(String),
    /// A decimal number, already written out.
    Numeric(String),
    /// An attachment or multi-value field (stored elsewhere; not read).
    Complex,
    /// A date with a wider range, already written out.
    ExtDate(String),
}

/// A column of a table.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub number: u16,
    pub ty: u8,
    pub variable: bool,
    pub var_index: u16,
    pub fixed_offset: u16,
    pub length: u16,
    pub scale: u8,
    pub flags: u8,
}

/// A table definition.
#[derive(Debug, Clone)]
pub struct Table {
    pub columns: Vec<Column>,
    pub row_count: u32,
    pub owned: Vec<u32>,
}

/// An entry in the system catalog `MSysObjects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub id: i64,
    pub kind: i64,
    pub name: String,
    pub flags: u32,
    pub page: u32,
}

/// Statistics of scanning rows of a table.
#[derive(Debug, Default, Clone, Copy)]
pub struct RowStats {
    pub deleted: u64,
    pub stopped: bool,
}

/// The database reader.
pub struct Db<'a> {
    data: &'a [u8],
    lay: Layout,
    pages: usize,
    code_page: CodePage,
}

impl<'a> Db<'a> {
    /// Opens the database from its bytes.
    ///
    /// # Errors
    /// [`OpenError::Encrypted`] if the file has a password or a non-zero encryption key;
    /// [`OpenError::Malformed`] for truncation or an unrecognised format.
    pub fn open(data: &'a [u8]) -> Result<Self, OpenError> {
        let magic = data.get(..20).ok_or(OpenError::Malformed("the file is too short"))?;
        if magic.get(4..19) != Some(b"Standard Jet DB")
            && magic.get(4..19) != Some(b"Standard ACE DB")
        {
            return Err(OpenError::Malformed("not an Access database"));
        }
        let lay = match data.get(0x14).copied() {
            Some(0) => JET3,
            Some(1 | 2 | 3) => JET4,
            _ => return Err(OpenError::Malformed("an Access version this reader does not know")),
        };
        let mut head =
            data.get(..lay.page_size).ok_or(OpenError::Malformed("truncated header"))?.to_vec();
        for (i, m) in HEADER_MASK.iter().take(lay.mask_len).enumerate() {
            if let Some(b) = head.get_mut(24 + i) {
                *b ^= m;
            }
        }
        // A non-zero key means the pages are encrypted; a stored password means the owner asked
        // for one (the check is the reader's, so Arca does not look past it).
        if u32_le(&head, 62).ok_or(OpenError::Malformed("truncated header"))? != 0 {
            return Err(OpenError::Encrypted);
        }
        let password =
            head.get(66..66 + lay.password_len).ok_or(OpenError::Malformed("truncated header"))?;
        let date_mask = if lay.jet3 {
            [0u8; 4]
        } else {
            let bits = head
                .get(114..122)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map_or(0.0, f64::from_le_bytes);
            // The same `(int)` cast the writer applied: saturating, NaN is 0.
            #[allow(clippy::cast_possible_truncation)]
            let key = bits as i32;
            key.to_le_bytes()
        };
        let password_set = password.iter().enumerate().any(|(i, b)| {
            let m = date_mask.get(i % 4).copied().unwrap_or(0);
            b ^ m != 0
        });
        if password_set {
            return Err(OpenError::Encrypted);
        }
        let code_page = if lay.jet3 {
            match u16_le(&head, 60) {
                Some(0) | None => CodePage::Windows1252,
                Some(id) => CodePage::from_id(id),
            }
        } else {
            CodePage::Utf16Le
        };
        let pages = data.len() / lay.page_size;
        if pages <= usize::try_from(CATALOG_PAGE).unwrap_or(usize::MAX) {
            return Err(OpenError::Malformed("the file is too short to hold a catalog"));
        }
        Ok(Self { data, lay, pages, code_page })
    }

    pub fn page(&self, n: u32) -> Option<&'a [u8]> {
        let n = usize::try_from(n).ok()?;
        if n >= self.pages {
            return None;
        }
        let start = n.checked_mul(self.lay.page_size)?;
        self.data.get(start..start.checked_add(self.lay.page_size)?)
    }

    /// The bytes of row `row` of the data page `page`, with its location flags.
    fn row_in_page(&self, page: &'a [u8], row: usize) -> Option<(u16, &'a [u8])> {
        let at = self.lay.page_row_start.checked_add(row.checked_mul(2)?)?;
        let raw = u16_le(page, at)?;
        let start = usize::from(raw & ROW_OFFSET);
        let end = if row == 0 {
            self.lay.page_size
        } else {
            let prev = self.lay.page_row_start.checked_add((row - 1).checked_mul(2)?)?;
            usize::from(u16_le(page, prev)? & ROW_OFFSET)
        };
        Some((raw, page.get(start..end)?))
    }

    /// Row `row` of page `page`.
    fn row_at(&self, page: u32, row: usize) -> Option<(u16, &'a [u8])> {
        let page = self.page(page)?;
        self.row_in_page(page, row)
    }

    /// Follows an overflow row pointer (a row stored on another page because it outgrew its slot).
    fn follow_overflow(&self, def: &'a [u8]) -> Option<&'a [u8]> {
        let row = usize::from(*def.first()?);
        let page = page3(def, 1)?;
        let (raw, bytes) = self.row_at(page, row)?;
        if raw & (ROW_DELETED | ROW_OVERFLOW) != 0 {
            return None;
        }
        Some(bytes)
    }

    /// Reads the pages a table owns from its usage map.
    fn usage_pages(&self, def: &[u8]) -> Vec<u32> {
        let Some(raw) = u32_le(def, 0) else { return Vec::new() };
        let page = raw & 0x00FF_FFFF;
        if raw & 0x8000_0000 != 0 {
            // An inline bitmap: bytes 1..4 of the definition are 32 bits of page presence,
            // starting at `page`.
            let mut out = Vec::new();
            for byte in 1..4 {
                let Some(b) = def.get(byte) else { break };
                for bit in 0..8 {
                    if (b >> bit) & 1 == 1 {
                        out.push(page + u32::try_from((byte - 1) * 8 + bit).unwrap_or(0));
                    }
                }
            }
            return out;
        }
        // A single-page map on page `page`: the map has a 4-byte header, then a 32-bit start
        // page, then the bit-per-page map.
        let Some(p) = self.page(page) else { return Vec::new() };
        let lay = self.lay;
        let at = if lay.jet3 { 4 } else { 8 };
        let Some(start) = u32_le(p, at) else { return Vec::new() };
        let map_at = at + 4;
        let mut out = Vec::new();
        for (i, b) in p.get(map_at..).unwrap_or(&[]).iter().enumerate() {
            if *b == 0 {
                continue;
            }
            for bit in 0..8 {
                if (b >> bit) & 1 == 1 {
                    let off = u32::try_from(i * 8 + bit).unwrap_or(0);
                    if let Some(n) = start.checked_add(off) {
                        out.push(n);
                    }
                }
            }
        }
        out
    }

    /// Reads a table definition.
    pub fn table(&self, page: u32) -> Option<Table> {
        let p = self.page(page)?;
        if p.first() != Some(&PAGE_TABLE) {
            return None;
        }
        let lay = self.lay;
        let def_len = usize::from(u16_le(p, 2)?);
        let def = p.get(..def_len.min(lay.page_size))?;
        let row_count = u32_le(def, lay.num_rows)?;
        let ncols = usize::from(u16_le(def, lay.num_cols)?);
        let nidx = usize::from(u16_le(def, lay.num_indexes)?);
        if ncols > 1000 {
            return None;
        }
        let owned = self.usage_pages(def.get(lay.owned_pages..lay.owned_pages + 4)?);

        let headers = lay.index_def_block.checked_add(nidx.checked_mul(lay.index_def_size)?)?;
        let mut names_at = headers.checked_add(ncols.checked_mul(lay.column_header_size)?)?;
        let mut columns = Vec::with_capacity(ncols);
        for i in 0..ncols {
            let h = headers + i * lay.column_header_size;
            let len = if lay.name_len_size == 1 {
                usize::from(*def.get(names_at)?)
            } else {
                usize::from(u16_le(def, names_at)?)
            };
            let start = names_at + lay.name_len_size;
            let raw = def.get(start..start.checked_add(len)?)?;
            names_at = start + len;
            let name =
                if lay.jet3 { decode_codepage(raw, self.code_page) } else { utf16le_lossy(raw) };
            let flags = *def.get(h + lay.col_flags)?;
            columns.push(Column {
                name,
                number: u16_le(def, h + lay.col_number)?,
                ty: *def.get(h)?,
                variable: flags & FLAG_FIXED == 0,
                var_index: u16_le(def, h + lay.col_var_index)?,
                fixed_offset: u16_le(def, h + lay.col_fixed_offset)?,
                length: u16_le(def, h + lay.col_length)?,
                scale: *def.get(h + lay.col_scale)?,
                flags,
            });
        }
        columns.sort_by_key(|c| c.number);
        Some(Table { columns, row_count, owned })
    }

    /// The objects of the database (tables, queries, forms, ...).
    pub fn catalog(&self) -> Option<Vec<Object>> {
        let table = self.table(CATALOG_PAGE)?;
        let col = |name: &str| table.columns.iter().position(|c| c.name == name);
        let (at_id, at_name, at_kind, at_flags) =
            (col("Id")?, col("Name")?, col("Type")?, col("Flags"));
        let mut out = Vec::new();
        self.rows(&table, |vals| {
            let int = |i: usize| match vals.get(i) {
                Some(Value::Int(v)) => Some(*v),
                _ => None,
            };
            let (Some(id), Some(kind)) = (int(at_id), int(at_kind)) else { return true };
            let Some(Value::Text(name)) = vals.get(at_name) else { return true };
            let flags = at_flags.and_then(int).unwrap_or(0);
            out.push(Object {
                id,
                kind,
                name: name.clone(),
                // The flags are a signed 32-bit number whose top bit marks a system object.
                flags: u32::try_from(flags & 0xFFFF_FFFF).unwrap_or(0),
                page: u32::try_from(id & 0x00FF_FFFF).unwrap_or(0),
            });
            out.len() < 100_000
        });
        Some(out)
    }

    /// Calls `f` with the values of every live row of `table`, in file order, until it returns
    /// `false`.
    pub fn rows(&self, table: &Table, mut f: impl FnMut(&[Value]) -> bool) -> RowStats {
        let mut stats = RowStats::default();
        let mut scanned = 0u64;
        let mut followed = 0u64;
        'pages: for &p in &table.owned {
            let Some(page) = self.page(p) else { continue };
            if page.first() != Some(&PAGE_DATA) {
                continue;
            }
            let n = usize::from(u16_le(page, self.lay.page_rows).unwrap_or(0));
            let room = (self.lay.page_size - self.lay.page_row_start) / 2;
            for i in 0..n.min(room) {
                scanned += 1;
                if scanned > MAX_ROWS_SCANNED {
                    stats.stopped = true;
                    break 'pages;
                }
                let Some((raw, bytes)) = self.row_in_page(page, i) else { continue };
                if raw & ROW_DELETED != 0 {
                    stats.deleted += 1;
                    continue;
                }
                let bytes = if raw & ROW_OVERFLOW != 0 {
                    followed += 1;
                    match self.follow_overflow(bytes) {
                        Some(b) => b,
                        None => continue,
                    }
                } else {
                    bytes
                };
                let values = self.decode_row(table, bytes);
                if !f(&values) {
                    stats.stopped = true;
                    break 'pages;
                }
            }
        }
        // The row a pointer leads to is itself marked deleted; it is not a deleted row.
        stats.deleted = stats.deleted.saturating_sub(followed);
        stats
    }

    /// The byte offsets of each variable column within a row.
    fn var_offsets(&self, row: &[u8], mask_len: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let Some(mask_at) = row.len().checked_sub(mask_len) else { return out };
        if self.lay.jet3 {
            let Some(at) = mask_at.checked_sub(1) else { return out };
            let Some(count) = row.get(at) else { return out };
            for i in 0..=usize::from(*count) {
                let Some(at) = mask_at.checked_sub(2 + i) else { return Vec::new() };
                let Some(b) = row.get(at) else { return Vec::new() };
                out.push(usize::from(*b));
            }
        } else if row.len() > 256 {
            let Some(count) = mask_at.checked_sub(2).and_then(|at| u16_le(row, at)) else {
                return out;
            };
            let jumbos = mask_at.checked_sub(2).and_then(|at| row.get(at..at + 2)).unwrap_or(&[]);
            let jumps = usize::from(jumbos.first().copied().unwrap_or(0));
            for i in 0..=usize::from(count).min(4096) {
                let used = (0..jumps).filter(|&j| i > j * 256).count();
                let Some(at) = mask_at.checked_sub(3 + 2 * jumps + i) else { return Vec::new() };
                let Some(b) = row.get(at) else { return Vec::new() };
                out.push(usize::from(*b) + used * 256);
            }
        } else {
            let Some(count) = mask_at.checked_sub(2).and_then(|at| u16_le(row, at)) else {
                return out;
            };
            for i in 0..=usize::from(count).min(4096) {
                let Some(at) = mask_at.checked_sub(4 + 2 * i) else { return Vec::new() };
                let Some(v) = u16_le(row, at) else { return Vec::new() };
                out.push(usize::from(v));
            }
        }
        out
    }

    fn decode_row(&self, table: &Table, row: &[u8]) -> Vec<Value> {
        let lay = self.lay;
        let ncols = if lay.jet3 {
            row.first().map_or(0, |b| usize::from(*b))
        } else {
            u16_le(row, 0).map_or(0, usize::from)
        };
        let mask_len = ncols.div_ceil(8);
        let mask = row.len().checked_sub(mask_len).and_then(|at| row.get(at..)).unwrap_or(&[]);
        let bit =
            |n: usize| n < ncols && mask.get(n / 8).is_some_and(|b| ((b >> (n % 8)) & 1) == 1);
        let offsets = if table.columns.iter().any(|c| c.variable) {
            self.var_offsets(row, mask_len)
        } else {
            Vec::new()
        };
        let mut out = Vec::with_capacity(table.columns.len());
        for c in &table.columns {
            let present = bit(usize::from(c.number));
            if c.ty == ty::BOOL {
                // A yes/no value is the bit itself.
                out.push(Value::Bool(present));
                continue;
            }
            if !present {
                out.push(Value::Null);
                continue;
            }
            let bytes = if c.variable {
                let i = usize::from(c.var_index);
                match (offsets.get(i), offsets.get(i + 1)) {
                    (Some(&s), Some(&e)) => row.get(s..e),
                    _ => None,
                }
            } else {
                let start = lay.row_cols_size + usize::from(c.fixed_offset);
                row.get(start..start + fixed_size(c))
            };
            out.push(bytes.map_or(Value::Null, |b| self.value(c, b)));
        }
        out
    }

    fn value(&self, c: &Column, d: &[u8]) -> Value {
        let arr = |n: usize| d.get(..n);
        match c.ty {
            ty::BYTE => d.first().map_or(Value::Null, |b| Value::Int(i64::from(*b))),
            ty::INT => arr(2)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Int(i64::from(i16::from_le_bytes(b)))),
            ty::LONG => arr(4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Int(i64::from(i32::from_le_bytes(b)))),
            ty::BIGINT => arr(8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Int(i64::from_le_bytes(b))),
            ty::MONEY => arr(8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Money(i64::from_le_bytes(b))),
            ty::FLOAT => arr(4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Single(f32::from_le_bytes(b))),
            ty::DOUBLE => arr(8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Double(f64::from_le_bytes(b))),
            ty::DATE => arr(8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Date(f64::from_le_bytes(b))),
            ty::TEXT => {
                let s = if self.lay.jet3 {
                    decode_codepage(d, self.code_page)
                } else if let Some(b) = d.strip_prefix(&[0xFF, 0xFE]) {
                    // Compressed Unicode: ASCII characters packed one byte each.
                    decode_codepage(b, CodePage::Windows1252)
                } else {
                    utf16le_lossy(d)
                };
                if c.flags & 0x80 != 0 { link(&s) } else { Value::Text(s) }
            }
            ty::OLE => match self.long_value(d) {
                Some(b) => Value::Ole(b.len()),
                None => Value::Ole(long_value_len(d)),
            },
            ty::MEMO => {
                let raw = match self.long_value(d) {
                    Some(b) => b,
                    None => return Value::Null,
                };
                let s = if self.lay.jet3 {
                    decode_codepage(&raw, self.code_page)
                } else if let Some(b) = raw.strip_prefix(&[0xFF, 0xFE]) {
                    decode_codepage(b, CodePage::Windows1252)
                } else {
                    utf16le_lossy(&raw)
                };
                if c.flags & 0x80 != 0 { link(&s) } else { Value::Text(s) }
            }
            ty::GUID => guid(d).map_or(Value::Null, Value::Guid),
            ty::NUMERIC => numeric(d, c.scale).map_or(Value::Null, Value::Numeric),
            ty::EXT_DATE => ext_date(d).map_or(Value::Null, Value::ExtDate),
            ty::COMPLEX => Value::Complex,
            _ => Value::Binary(d.len()),
        }
    }

    /// Reads a memo or OLE value that outgrew the row itself.
    fn long_value(&self, def: &[u8]) -> Option<Vec<u8>> {
        let len = long_value_len(def);
        let flag = u16_le(def, 2)?;
        if flag & 0x8000 != 0 {
            // Stored in the row itself, after the length/flags header.
            return Some(def.get(12..12 + len)?.to_vec());
        }
        match *def.get(3)? {
            // A single page chunk, pointer at bytes 4..8.
            1 => {
                let row = usize::from(*def.get(4)?);
                let page = page3(def, 5)?;
                let (_, bytes) = self.row_at(page, row)?;
                Some(bytes.get(..len)?.to_vec())
            }
            // A chain of data chunks: first page at 4..8, each chunk starts with the pointer to the next.
            0 => {
                let mut out = Vec::new();
                let (mut row, mut page) = (usize::from(*def.get(4)?), page3(def, 5)?);
                for _ in 0..self.pages {
                    if out.len() >= len || page == 0 {
                        break;
                    }
                    let (_, bytes) = self.row_at(page, row)?;
                    let chunk = bytes.get(4..)?;
                    let take = chunk.len().min(len - out.len());
                    out.extend_from_slice(chunk.get(..take)?);
                    row = usize::from(*bytes.first()?);
                    page = page3(bytes, 1)?;
                }
                Some(out)
            }
            _ => None,
        }
    }
}

/// A 3-byte little-endian page pointer (Jet stores page numbers as 24 bits in many places).
fn page3(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at + 3)?;
    Some(u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16))
}

/// The size in bytes of a fixed-length value.
fn fixed_size(c: &Column) -> usize {
    let own = match c.ty {
        ty::BYTE => 1,
        ty::INT => 2,
        ty::LONG | ty::FLOAT | ty::COMPLEX => 4,
        ty::MONEY | ty::DOUBLE | ty::DATE | ty::BIGINT => 8,
        ty::GUID => 16,
        ty::NUMERIC => 17,
        ty::EXT_DATE => 42,
        ty::BOOL => 0,
        _ => 0,
    };
    own.max(usize::from(c.length))
}

/// The declared length of a memo or OLE value.
fn long_value_len(def: &[u8]) -> usize {
    u32_le(def, 0).and_then(|w| usize::try_from(w & 0x3FFF_FFFF).ok()).unwrap_or(0)
}

/// Splits a hyperlink value `text#address#sub-address#screen-tip`.
fn link(s: &str) -> Value {
    let mut parts = s.split('#');
    let text = parts.next().unwrap_or("");
    let address = parts.next().unwrap_or("");
    let sub = parts.next().unwrap_or("");
    if address.is_empty() && sub.is_empty() {
        return Value::Text(s.to_owned());
    }
    let target = if sub.is_empty() { address.to_owned() } else { format!("{address}#{sub}") };
    Value::Link(if text.is_empty() { target.clone() } else { text.to_owned() }, target)
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
fn guid(d: &[u8]) -> Option<String> {
    let b = d.get(..16)?;
    let g = |i: usize| b.get(i).copied().unwrap_or(0);
    Some(format!(
        "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g(3),
        g(2),
        g(1),
        g(0),
        g(5),
        g(4),
        g(7),
        g(6),
        g(8),
        g(9),
        g(10),
        g(11),
        g(12),
        g(13),
        g(14),
        g(15)
    ))
}

/// A `NUMERIC` value: a sign byte and a 128-bit magnitude stored as four little-endian words,
/// most significant first.
fn numeric(d: &[u8], scale: u8) -> Option<String> {
    let negative = *d.first()? != 0;
    let mut v: u128 = 0;
    for w in 0..4 {
        let word = u32_le(d, 1 + 4 * w)?;
        v = (v << 32) | u128::from(word);
    }
    let digits = v.to_string();
    let scale = usize::from(scale).min(38);
    let body = if scale == 0 {
        digits
    } else {
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (int, frac) = padded.split_at(padded.len() - scale);
        format!("{int}.{frac}")
    };
    Some(if negative && v != 0 { format!("-{body}") } else { body })
}

/// Howard Hinnant's calendar conversions.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}

/// An extended date: `<days>:<seconds>.<fraction>` as fixed-width ASCII digits since 0001-01-01.
fn ext_date(d: &[u8]) -> Option<String> {
    let num = |from: usize, len: usize| -> Option<i64> {
        let s = std::str::from_utf8(d.get(from..from + len)?).ok()?;
        s.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok()
    };
    let days = num(0, 19)?;
    let secs = num(20, 12)?;
    let frac = num(32, 7)?;
    let epoch = days_from_civil(1, 1, 1);
    let day = days.checked_add(epoch)?.checked_add(secs.div_euclid(86_400))?;
    let (y, m, dd) = civil_from_days(day);
    let s = secs.rem_euclid(86_400);
    Some(format!(
        "{y:04}-{m:02}-{dd:02} {:02}:{:02}:{:02}.{frac:07}",
        s / 3600,
        s / 60 % 60,
        s % 60
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_values_are_written_out() {
        // 1234.50 with scale 2: magnitude 123450.
        let mut d = vec![0u8];
        d.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        d.extend_from_slice(&123_450u32.to_le_bytes());
        assert_eq!(numeric(&d, 2).as_deref(), Some("1234.50"));
        d[0] = 1;
        assert_eq!(numeric(&d, 2).as_deref(), Some("-1234.50"));
        assert_eq!(numeric(&d, 0).as_deref(), Some("-123450"));
        assert_eq!(numeric(&d, 7).as_deref(), Some("-0.0123450"));
        assert_eq!(numeric(&d[..5], 2), None);
    }

    #[test]
    fn guids_follow_the_mixed_byte_order() {
        let d = [
            0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB,
            0xCD, 0xEF,
        ];
        assert_eq!(guid(&d).as_deref(), Some("{12345678-9ABC-DEF0-0123-456789ABCDEF}"));
        assert_eq!(guid(&d[..8]), None);
    }

    #[test]
    fn hyperlinks_split_at_hash_marks() {
        assert_eq!(
            link("Arca#https://example.org/a#frag#tip"),
            Value::Link("Arca".into(), "https://example.org/a#frag".into())
        );
        assert_eq!(link("plain text"), Value::Text("plain text".into()));
        assert_eq!(
            link("#https://example.org/#"),
            Value::Link("https://example.org/".into(), "https://example.org/".into())
        );
    }

    #[test]
    fn page_numbers_have_three_bytes() {
        assert_eq!(page3(&[9, 0x34, 0x12, 0x01], 1), Some(0x0001_1234));
        assert_eq!(page3(&[9, 1, 2], 1), None);
    }

    #[test]
    fn junk_is_refused() {
        assert_eq!(
            Db::open(b"not a database at all, nothing here").err(),
            Some(OpenError::Malformed("not an Access database"))
        );
        let mut head = vec![0u8; 4096 * 3];
        head[4..19].copy_from_slice(b"Standard Jet DB");
        head[0x14] = 9;
        assert!(Db::open(&head).is_err());
    }
}
