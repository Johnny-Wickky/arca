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
//! Nothing here interprets queries, forms, macros or modules, and nothing is executed. Indexes are
//! listed (names and columns) but never used: rows are read page by page.

use crate::bytes::{u16_le, u32_le};
use crate::text::{CodePage, decode_codepage, utf16le_lossy};
use std::cell::Cell;

mod complex;
mod props;
#[cfg(any(test, feature = "test-support"))]
mod testdb;
#[cfg(feature = "test-support")]
pub use testdb::database_with_objects;
#[cfg(test)]
mod values_tests;

pub use complex::{ComplexData, ComplexItem, ComplexKind};
pub use props::{PropValue, Property};

/// Column types (the byte in the column definition).
pub mod ty {
    /// Yes/No, stored as a single bit in the row's null mask.
    pub const BOOL: u8 = 0x01;
    /// Unsigned 8-bit integer.
    pub const BYTE: u8 = 0x02;
    /// Signed 16-bit integer.
    pub const INT: u8 = 0x03;
    /// Signed 32-bit integer.
    pub const LONG: u8 = 0x04;
    /// Currency: signed 64-bit integer in ten-thousandths.
    pub const MONEY: u8 = 0x05;
    /// 32-bit IEEE floating point.
    pub const FLOAT: u8 = 0x06;
    /// 64-bit IEEE floating point.
    pub const DOUBLE: u8 = 0x07;
    /// Date/time: a 64-bit float counting days since 1899-12-30.
    pub const DATE: u8 = 0x08;
    /// Binary data (up to 255 bytes), variable length.
    pub const BINARY: u8 = 0x09;
    /// Short text (up to 255 characters), variable length.
    pub const TEXT: u8 = 0x0A;
    /// Embedded OLE object or binary blob, variable length.
    pub const OLE: u8 = 0x0B;
    /// Long text (memo), possibly stored on separate pages.
    pub const MEMO: u8 = 0x0C;
    /// 128-bit globally unique identifier.
    pub const GUID: u8 = 0x0F;
    /// Fixed-point decimal number.
    pub const NUMERIC: u8 = 0x10;
    /// Complex field (attachment or multi-value), stored in a hidden table.
    pub const COMPLEX: u8 = 0x12;
    /// Signed 64-bit integer.
    pub const BIGINT: u8 = 0x13;
    /// Extended date/time with a wider range, stored as fixed-width ASCII digits.
    pub const EXT_DATE: u8 = 0x14;
}

/// Column flag: the column holds a fixed-length value.
const FLAG_FIXED: u8 = 0x01;
/// Column flag: the column is an automatic number (a counter or a random one).
const FLAG_AUTO_NUMBER: u8 = 0x04;
/// Column flag: the column is hidden in the datasheet (a complex column's history is).
const FLAG_HIDDEN: u8 = 0x10;
/// Column flag: an automatic number that is a GUID.
const FLAG_AUTO_GUID: u8 = 0x40;
/// Column flag: a memo column that holds hyperlinks.
const FLAG_HYPERLINK: u8 = 0x80;
/// Extended column flag (ACE): the column holds the key of a complex column's hidden table.
const EXT_COMPLEX_FK: u8 = 0x08;
/// Extended column flags (ACE 2010 and later): the column is calculated. Its stored value is
/// wrapped in a header, see [`unwrap_calculated`].
const EXT_CALCULATED: u8 = 0xC0;
/// Where the length of the data of a calculated value is, and where the data starts.
const CALC_LEN_AT: usize = 16;
const CALC_DATA_AT: usize = 20;

/// Row-location flags.
const ROW_DELETED: u16 = 0x8000;
const ROW_OVERFLOW: u16 = 0x4000;
const ROW_OFFSET: u16 = 0x1FFF;

/// Page types.
const PAGE_DATA: u8 = 0x01;
const PAGE_TABLE_DEF: u8 = 0x02;
const PAGE_USAGE_MAP: u8 = 0x05;

/// Page that holds the definition of `MSysObjects`.
const CATALOG_PAGE: u32 = 2;

/// Most pages of one table definition (it continues in further pages when it is long).
const MAX_TDEF_PAGES: usize = 64;
/// The column types a calculated column may have as its result.
const RESULT_TYPES: [u8; 13] =
    [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0A, 0x0C, 0x0F, 0x10, 0x13];
/// Most indexes of a table that are read (Access allows 32).
const MAX_INDEXES: usize = 256;
/// Most columns of one index (Access allows 10).
const MAX_INDEX_COLUMNS: usize = 10;
/// Most columns of a table (Access allows 255).
const MAX_COLUMNS: usize = 255;
/// Longest chain of overflow pointers followed for one row.
const MAX_OVERFLOW_DEPTH: usize = 8;
/// Most bytes of one memo or binary value that are read.
const MAX_LONG_VALUE: usize = 4 << 20;
/// Most rows examined in one table.
const MAX_ROWS_SCANNED: u64 = 4_000_000;
/// Work units one scan of a table may spend: one per memo or OLE chunk followed, plus one per
/// [`WORK_BYTES_PER_UNIT`] bytes of every value read or copied. Cycles and huge chains in a
/// hostile file cannot make the cost grow with the number of rows times the number of pages, and
/// neither can columns that all point at the same bytes of a row (a fixed-length column may start
/// anywhere: a hundred of them make a hundred-fold read of the row).
const MAX_SCAN_WORK: u64 = 1_000_000;
/// Most catalog objects collected.
const MAX_CATALOG_OBJECTS: usize = 100_000;
/// Bytes of copied long-value data that cost one work unit.
const WORK_BYTES_PER_UNIT: u64 = 256;

/// The obfuscation mask of the header: bytes `24..` of page 0 are XORed with it.
const HEADER_MASK: [u8; 128] = [
    0xB5, 0x6F, 0x03, 0x62, 0x61, 0x08, 0xC2, 0x55, 0xEB, 0xA9, 0x67, 0x72, 0x43, 0x3F, 0x00, 0x9C,
    0x7A, 0x9F, 0x90, 0xFF, 0x80, 0x9A, 0x31, 0xC5, 0x79, 0xBA, 0xED, 0x30, 0xBC, 0xDF, 0xCC, 0x9D,
    0x63, 0xD9, 0xE4, 0xC3, 0x7B, 0x42, 0xFB, 0x8A, 0xBC, 0x4E, 0x86, 0xFB, 0xEC, 0x37, 0x5D, 0x44,
    0x9C, 0xFA, 0xC6, 0x5E, 0x28, 0xE6, 0x13, 0xB6, 0x8A, 0x60, 0x54, 0x94, 0x7B, 0x36, 0xF5, 0x72,
    0xDF, 0xB1, 0x77, 0xF4, 0x13, 0x43, 0xCF, 0xAF, 0xB1, 0x33, 0x34, 0x61, 0x79, 0x5B, 0x92, 0xB5,
    0x7C, 0x2A, 0x05, 0xF1, 0x7C, 0x99, 0x01, 0x1B, 0x98, 0xFD, 0x12, 0x4F, 0x4A, 0x94, 0x6C, 0x3E,
    0x60, 0x26, 0x5F, 0x95, 0xF8, 0xD0, 0x89, 0x24, 0x85, 0x67, 0xC6, 0x1F, 0x27, 0x44, 0xD2, 0xEE,
    0xCF, 0x65, 0xED, 0xFF, 0x07, 0xC7, 0x46, 0xA1, 0x78, 0x16, 0x0C, 0xED, 0xE9, 0x2D, 0x62, 0xD4,
];

/// Byte offsets of the structures of one generation of the format.
struct Layout {
    jet3: bool,
    page_size: usize,
    /// Length of the obfuscated part of the header.
    mask_len: usize,
    password_len: usize,
    // Table definition.
    num_rows: usize,
    num_cols: usize,
    num_indexes: usize,
    owned_pages: usize,
    index_def_block: usize,
    index_def_size: usize,
    /// Number of logical indexes (a table lists more of them than real index structures).
    num_logical: usize,
    /// Bytes skipped at the start of a real-index block and of a logical-index entry.
    index_skip: usize,
    /// Size of the block that describes one real index, and where its flags byte is.
    index_col_block: usize,
    index_flags_at: usize,
    /// Size of the entry that describes one logical index.
    index_info_block: usize,
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
    num_logical: 27,
    index_skip: 0,
    index_col_block: 39,
    index_flags_at: 38,
    index_info_block: 20,
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
    num_logical: 47,
    index_skip: 4,
    index_col_block: 52,
    index_flags_at: 46,
    index_info_block: 28,
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
    /// No value (the column's null bit is set).
    Null,
    /// A Yes/No value.
    Bool(bool),
    /// Any of the integer column types, widened.
    Int(i64),
    /// A 32-bit floating point number.
    Single(f32),
    /// A 64-bit floating point number.
    Double(f64),
    /// Currency in ten-thousandths.
    Money(i64),
    /// Days since 1899-12-30 (the fraction is the time of day).
    Date(f64),
    /// Text, already decoded to UTF-8.
    Text(String),
    /// A hyperlink: the text shown and the address.
    Link(String, String),
    /// Binary data of this many bytes (not read).
    Binary(usize),
    /// An embedded object of this many bytes (not read).
    Ole(usize),
    /// A globally unique identifier written in the usual braced form.
    Guid(String),
    /// A decimal number, already written out.
    Numeric(String),
    /// A complex field (attachments, multiple values, a memo's version history): the key of its
    /// values in a hidden table, see [`Db::complex_data`].
    Complex(i64),
    /// The bytes of an object column. Only produced while [`Db::properties_of`] reads the
    /// property lists of the catalog; everywhere else an object is [`Value::Ole`].
    Blob(Vec<u8>),
    /// A date with a wider range, already written out.
    ExtDate(String),
}

/// A column of a table.
#[derive(Debug, Clone)]
pub struct Column {
    /// The column name.
    pub name: String,
    number: u16,
    /// The column type, one of the constants in [`ty`].
    pub ty: u8,
    variable: bool,
    var_index: u16,
    fixed_offset: u16,
    length: u16,
    scale: u8,
    flags: u8,
    ext_flags: u8,
    complex_id: u32,
}

impl Column {
    /// Whether the column is calculated from other columns (the file keeps the result).
    #[must_use]
    pub fn is_calculated(&self) -> bool {
        self.ext_flags & EXT_CALCULATED != 0
    }

    /// Whether the column is hidden in the datasheet.
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.flags & FLAG_HIDDEN != 0
    }

    /// Whether the column holds a hyperlink (a memo or text with the hyperlink flag).
    #[must_use]
    pub fn is_hyperlink(&self) -> bool {
        self.flags & FLAG_HYPERLINK != 0
    }

    /// Whether the column is an automatic number.
    #[must_use]
    pub fn is_auto_number(&self) -> bool {
        self.flags & (FLAG_AUTO_NUMBER | FLAG_AUTO_GUID) != 0
    }

    /// Whether the column holds the key of a complex column's values (in the hidden table).
    #[must_use]
    pub fn is_complex_key(&self) -> bool {
        self.ext_flags & EXT_COMPLEX_FK != 0
    }

    /// The identifier that ties a complex column to its row in `MSysComplexColumns`.
    #[must_use]
    pub fn complex_id(&self) -> u32 {
        self.complex_id
    }
}

/// A table definition.
#[derive(Debug)]
pub struct Table {
    /// Columns in the order of their numbers.
    pub columns: Vec<Column>,
    /// Rows the definition says the table has.
    pub row_count: u32,
    /// Pages that hold the table's rows.
    owned: Vec<u32>,
    /// `true` if the usage map of the table leads outside the file, so some of its rows are
    /// missing and [`Db::rows`] reports the scan as stopped early.
    pub truncated: bool,
    /// The indexes the definition lists (names and columns only; the index pages are not read).
    pub indexes: Vec<Index>,
}

/// What an index is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    /// An ordinary index.
    Normal,
    /// The primary key.
    Primary,
    /// The index that backs the foreign-key side of a relationship.
    Foreign,
    /// A kind this reader does not know.
    Other(u8),
}

/// A logical index of a table, as the table definition lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// The index name.
    pub name: String,
    /// The indexed columns in order, each with `true` for ascending.
    pub columns: Vec<(String, bool)>,
    /// What the index is for.
    pub kind: IndexKind,
    /// Whether the index requires distinct values.
    pub unique: bool,
}

/// An entry of the system catalog.
#[derive(Debug, Clone)]
pub struct Object {
    /// The raw catalog identifier; other system tables (for example `MSysQueries`) refer to an
    /// object by this value.
    pub id: i64,
    /// The object kind code (1 is a local table, 5 a query, and so on).
    pub kind: i64,
    /// The object name.
    pub name: String,
    /// The object flags; some bits mark system and hidden objects.
    pub flags: u32,
    /// The page of the table definition, for a table.
    pub page: u32,
    /// For a linked table, the connection string (for example an ODBC source); otherwise empty.
    pub connect: String,
    /// For a linked table, the path of the database that holds the data; otherwise empty.
    pub database: String,
}

/// What reading the rows of a table found besides the rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RowStats {
    /// Rows marked deleted (their bytes are still in the file), not counting the places where
    /// a long row continues.
    pub deleted: u64,
    /// Whether reading stopped before the end because the caller asked or a limit was reached.
    pub stopped: bool,
}

/// An opened database.
pub struct Db<'a> {
    data: &'a [u8],
    lay: &'static Layout,
    pages: usize,
    code_page: CodePage,
    /// What is left of the work budget of the scan in progress (see [`MAX_SCAN_WORK`]).
    work: Cell<u64>,
    /// The budget every scan starts with.
    work_limit: u64,
    /// Whether object columns are returned as their bytes (see [`Value::Blob`]).
    keep_blobs: Cell<bool>,
}

/// Reads the three-byte little-endian page number at `at`.
fn page3(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(3)?)?;
    Some(u32::from(*s.first()?) | (u32::from(*s.get(1)?) << 8) | (u32::from(*s.get(2)?) << 16))
}

impl<'a> Db<'a> {
    /// Opens the database from its bytes.
    ///
    /// # Errors
    /// [`OpenError::Encrypted`] if the file has a password or a non-zero encryption key;
    /// [`OpenError::Malformed`] for truncation or an unrecognised format.
    pub fn open(data: &'a [u8]) -> Result<Self, OpenError> {
        if data.get(4..19) != Some(&b"Standard Jet DB"[..])
            && data.get(4..19) != Some(&b"Standard ACE DB"[..])
        {
            return Err(OpenError::Malformed("not an Access database"));
        }
        let version = *data.get(0x14).ok_or(OpenError::Malformed("truncated header"))?;
        let lay: &'static Layout = match version {
            0 => &JET3,
            1..=6 => &JET4,
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
        Ok(Self {
            data,
            lay,
            pages,
            code_page,
            work: Cell::new(MAX_SCAN_WORK),
            work_limit: MAX_SCAN_WORK,
            keep_blobs: Cell::new(false),
        })
    }

    /// Lowers the work one scan of a table may spend (a unit is a chunk followed or 256 bytes
    /// read). A larger figure than the built-in one is ignored: the limit exists to be tightened.
    #[must_use]
    pub fn with_work_limit(mut self, units: u64) -> Self {
        self.work_limit = units.min(MAX_SCAN_WORK);
        self.work.set(self.work_limit);
        self
    }

    /// The bytes of page `n`, or `None` if it lies outside the file.
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

    /// Row `row` of page `page_no` by location (without following overflow pointers).
    fn row_at(&self, page_no: u32, row: usize) -> Option<(u16, &'a [u8])> {
        self.row_in_page(self.page(page_no)?, row)
    }

    /// The pages marked in the usage map that the four bytes `ptr` (row, then page) point to.
    ///
    /// The flag is `true` when the map leads outside the file (the file is truncated or the
    /// map is damaged), so the list of pages is known to be incomplete.
    fn usage_pages(&self, ptr: &[u8]) -> (Vec<u32>, bool) {
        let mut out = Vec::new();
        let missing = Cell::new(false);
        let (Some(&row), Some(page)) = (ptr.first(), page3(ptr, 1)) else {
            return (out, true);
        };
        let Some((_, map)) = self.row_at(page, usize::from(row)) else { return (out, true) };
        let mark = |out: &mut Vec<u32>, base: usize, bits: &[u8]| {
            for (i, byte) in bits.iter().enumerate() {
                if *byte == 0 {
                    continue;
                }
                for bit in 0..8usize {
                    if (byte >> bit) & 1 == 1 {
                        let n = base.saturating_add(i.saturating_mul(8)).saturating_add(bit);
                        if n < self.pages
                            && let Ok(n) = u32::try_from(n)
                        {
                            out.push(n);
                        } else {
                            missing.set(true);
                        }
                    }
                }
            }
        };
        match map.first() {
            Some(0) => {
                let start = usize::try_from(u32_le(map, 1).unwrap_or(u32::MAX)).unwrap_or(0);
                mark(&mut out, start, map.get(5..).unwrap_or(&[]));
            }
            Some(1) => {
                let per_page = (self.lay.page_size - 4) * 8;
                for k in 0..map.len().saturating_sub(1) / 4 {
                    let Some(mp) = u32_le(map, 1 + 4 * k) else { break };
                    if mp == 0 {
                        continue;
                    }
                    let Some(p) = self.page(mp) else {
                        missing.set(true);
                        continue;
                    };
                    if p.first() != Some(&PAGE_USAGE_MAP) {
                        continue;
                    }
                    mark(&mut out, k * per_page, p.get(4..).unwrap_or(&[]));
                }
            }
            _ => {}
        }
        out.sort_unstable();
        out.dedup();
        (out, missing.get())
    }

    /// Reads the definition of the table whose definition starts at page `page_no`.
    pub fn table(&self, page_no: u32) -> Option<Table> {
        let lay = self.lay;
        let first = self.page(page_no)?;
        if first.first() != Some(&PAGE_TABLE_DEF) {
            return None;
        }
        // A long definition continues in further pages, each contributing all but its 8-byte
        // header.
        let mut def = first.to_vec();
        let mut next = u32_le(first, 4)?;
        let mut guard = 0;
        while next != 0 {
            guard += 1;
            if guard > MAX_TDEF_PAGES {
                return None;
            }
            let p = self.page(next)?;
            def.extend_from_slice(p.get(8..)?);
            next = u32_le(p, 4)?;
        }
        let row_count = u32_le(&def, lay.num_rows)?;
        let ncols = usize::from(u16_le(&def, lay.num_cols)?);
        let nidx = usize::try_from(u32_le(&def, lay.num_indexes)?).ok()?;
        if ncols > MAX_COLUMNS || nidx > 1000 {
            return None;
        }
        let (owned, truncated) = self.usage_pages(def.get(lay.owned_pages..lay.owned_pages + 4)?);

        let headers = lay.index_def_block.checked_add(nidx.checked_mul(lay.index_def_size)?)?;
        let mut names_at = headers.checked_add(ncols.checked_mul(lay.column_header_size)?)?;
        let mut columns = Vec::with_capacity(ncols);
        for i in 0..ncols {
            let h = headers + i * lay.column_header_size;
            let len = if lay.name_len_size == 1 {
                usize::from(*def.get(names_at)?)
            } else {
                usize::from(u16_le(&def, names_at)?)
            };
            let start = names_at + lay.name_len_size;
            let raw = def.get(start..start.checked_add(len)?)?;
            names_at = start + len;
            let name =
                if lay.jet3 { decode_codepage(raw, self.code_page) } else { utf16le_lossy(raw) };
            let flags = *def.get(h + lay.col_flags)?;
            let ty = *def.get(h)?;
            // Access 97 has no extended flags; later formats keep them in the byte after the
            // flags. Only a complex column has an identifier (in the bytes of the precision).
            let ext_flags = if lay.jet3 { 0 } else { *def.get(h + lay.col_flags + 1)? };
            let complex_id =
                if ty == ty::COMPLEX && !lay.jet3 { u32_le(&def, h + 11).unwrap_or(0) } else { 0 };
            columns.push(Column {
                name,
                number: u16_le(&def, h + lay.col_number)?,
                ty,
                variable: flags & FLAG_FIXED == 0,
                var_index: u16_le(&def, h + lay.col_var_index)?,
                fixed_offset: u16_le(&def, h + lay.col_fixed_offset)?,
                length: u16_le(&def, h + lay.col_length)?,
                scale: *def.get(h + lay.col_scale)?,
                flags,
                ext_flags,
                complex_id,
            });
        }
        // The indexes follow the column names: the real ones (columns and flags), then the
        // logical ones (which real index they use and what they are for), then their names. A
        // damaged list loses only the indexes, never the table.
        let indexes = self.indexes(&def, names_at, nidx, &columns).unwrap_or_default();
        self.apply_result_types(page_no, &mut columns);
        columns.sort_by_key(|c| c.number);
        Some(Table { columns, row_count, owned, truncated, indexes })
    }

    /// A calculated column is stored under a placeholder type; the type of its result is the
    /// property `ResultType` of the column, in the property list of the table's catalog entry
    /// (the open-source Jackcess library documents this in `ColumnImpl.create`). Sets the type of
    /// each calculated column from it. A column without the property keeps the stored type.
    fn apply_result_types(&self, page_no: u32, columns: &mut [Column]) {
        if page_no == CATALOG_PAGE || !columns.iter().any(Column::is_calculated) {
            return;
        }
        let Some(catalog) = self.table(CATALOG_PAGE) else { return };
        let col = |n: &str| catalog.columns.iter().position(|c| c.name == n);
        let (Some(at_id), Some(at_kind), Some(at_prop)) = (col("Id"), col("Type"), col("LvProp"))
        else {
            return;
        };
        let mut list = None;
        self.rows_raw(&catalog, |vals| {
            if let (Some(Value::Int(id)), Some(Value::Int(kind)), Some(Value::Blob(b))) =
                (vals.get(at_id), vals.get(at_kind), vals.get(at_prop))
                && *kind == 1
                && u32::try_from(id & 0x00FF_FFFF) == Ok(page_no)
            {
                list = Some(props::parse(b, &|raw| self.name_text(raw)));
                return false;
            }
            true
        });
        for c in columns.iter_mut().filter(|c| c.is_calculated()) {
            let found = list.iter().flatten().find_map(|p| match &p.value {
                PropValue::Int(t)
                    if p.map_kind == 1
                        && p.name.eq_ignore_ascii_case("ResultType")
                        && p.scope.eq_ignore_ascii_case(&c.name) =>
                {
                    u8::try_from(*t).ok()
                }
                _ => None,
            });
            if let Some(t) = found.filter(|t| RESULT_TYPES.contains(t)) {
                c.ty = t;
            }
        }
    }

    /// The logical indexes of a table definition whose real-index blocks start at `at`.
    fn indexes(
        &self,
        def: &[u8],
        at: usize,
        real: usize,
        columns: &[Column],
    ) -> Option<Vec<Index>> {
        let lay = self.lay;
        let logical = usize::try_from(u32_le(def, lay.num_logical)?).ok()?;
        if logical > MAX_INDEXES || real > MAX_INDEXES {
            return None;
        }
        // (column numbers with their ascending flag, unique)
        let mut datas: Vec<(Vec<(u16, bool)>, bool)> = Vec::new();
        let mut pos = at;
        for _ in 0..real {
            let block = def.get(pos..pos.checked_add(lay.index_col_block)?)?;
            let mut cols = Vec::new();
            for k in 0..MAX_INDEX_COLUMNS {
                let o = lay.index_skip + 3 * k;
                let number = u16_le(block, o)?;
                if number != 0xFFFF {
                    cols.push((number, block.get(o + 2)? & 1 == 1));
                }
            }
            let unique = block.get(lay.index_flags_at)? & 1 == 1;
            datas.push((cols, unique));
            pos += lay.index_col_block;
        }
        let mut entries = Vec::new();
        for _ in 0..logical {
            let block = def.get(pos..pos.checked_add(lay.index_info_block)?)?;
            let base = lay.index_skip;
            let data = usize::try_from(u32_le(block, base + 4)?).ok()?;
            let kind = *block.get(base + 19)?;
            entries.push((data, kind));
            pos += lay.index_info_block;
        }
        let mut out = Vec::with_capacity(logical);
        for (data, kind) in entries {
            let len = if lay.name_len_size == 1 {
                usize::from(*def.get(pos)?)
            } else {
                usize::from(u16_le(def, pos)?)
            };
            let start = pos + lay.name_len_size;
            let raw = def.get(start..start.checked_add(len)?)?;
            pos = start + len;
            let name =
                if lay.jet3 { decode_codepage(raw, self.code_page) } else { utf16le_lossy(raw) };
            let (cols, unique) = datas.get(data)?;
            let columns = cols
                .iter()
                .map(|(n, asc)| {
                    let name = columns.iter().find(|c| c.number == *n).map(|c| c.name.clone());
                    (name.unwrap_or_default(), *asc)
                })
                .collect();
            let kind = match kind {
                0 | 3 => IndexKind::Normal,
                1 => IndexKind::Primary,
                2 => IndexKind::Foreign,
                other => IndexKind::Other(other),
            };
            out.push(Index { name, columns, kind, unique: *unique });
        }
        Some(out)
    }

    /// The declared size of a column for display: bytes for the fixed-size types, characters for
    /// text, bytes for binary data; `None` for the types that have no declared size.
    #[must_use]
    pub fn column_size(&self, c: &Column) -> Option<u32> {
        match c.ty {
            ty::BOOL | ty::BYTE => Some(1),
            ty::INT => Some(2),
            ty::LONG | ty::FLOAT => Some(4),
            ty::MONEY | ty::DOUBLE | ty::DATE | ty::BIGINT => Some(8),
            ty::GUID => Some(16),
            ty::BINARY => Some(u32::from(c.length)),
            ty::TEXT if self.lay.jet3 => Some(u32::from(c.length)),
            ty::TEXT => Some(u32::from(c.length) / 2),
            _ => None,
        }
    }

    /// The objects of the database (tables, queries, forms, ...).
    pub fn catalog(&self) -> Option<Vec<Object>> {
        self.catalog_checked().map(|(objects, _)| objects)
    }

    /// Like [`Db::catalog`], and also says how the scan of the catalog ended: when
    /// [`RowStats::stopped`] is set the list is incomplete (a limit was reached or the file is
    /// truncated), so a caller that must not miss an object has to treat the database as not
    /// fully read.
    pub fn catalog_checked(&self) -> Option<(Vec<Object>, RowStats)> {
        let table = self.table(CATALOG_PAGE)?;
        let col = |name: &str| table.columns.iter().position(|c| c.name == name);
        let (at_id, at_name, at_kind, at_flags) =
            (col("Id")?, col("Name")?, col("Type")?, col("Flags"));
        let (at_connect, at_database) = (col("Connect"), col("Database"));
        let mut out = Vec::new();
        let stats = self.rows(&table, |vals| {
            let text = |i: Option<usize>| match i.and_then(|i| vals.get(i)) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
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
                connect: text(at_connect),
                database: text(at_database),
            });
            out.len() < MAX_CATALOG_OBJECTS
        });
        Some((out, stats))
    }

    /// The properties stored with the catalog object called `name` (case-insensitive): for the
    /// database itself, the object `MSysDb`. `None` if there is no such object or no list.
    pub fn properties_of(&self, name: &str) -> Option<Vec<Property>> {
        let table = self.table(CATALOG_PAGE)?;
        let col = |n: &str| table.columns.iter().position(|c| c.name == n);
        let (at_name, at_prop) = (col("Name")?, col("LvProp")?);
        let mut found = None;
        self.keep_blobs.set(true);
        self.rows(&table, |vals| {
            if let (Some(Value::Text(n)), Some(Value::Blob(b))) =
                (vals.get(at_name), vals.get(at_prop))
                && n.eq_ignore_ascii_case(name)
            {
                found = Some(props::parse(b, &|raw| self.name_text(raw)));
                return false;
            }
            true
        });
        self.keep_blobs.set(false);
        found
    }

    /// Decodes a name or text value stored without compression.
    fn name_text(&self, raw: &[u8]) -> String {
        if self.lay.jet3 { decode_codepage(raw, self.code_page) } else { utf16le_lossy(raw) }
    }

    /// Like [`Db::rows`], but object and binary columns come back as their bytes
    /// ([`Value::Blob`], at most 4 MiB each) instead of their size. For the system tables that
    /// keep structured data in such a column (the sort key of a saved query, a property list).
    pub fn rows_raw(&self, table: &Table, f: impl FnMut(&[Value]) -> bool) -> RowStats {
        self.keep_blobs.set(true);
        let stats = self.rows(table, f);
        self.keep_blobs.set(false);
        stats
    }

    /// Calls `f` with the values of every live row of `table`, in file order, until it returns
    /// `false`.
    pub fn rows(&self, table: &Table, mut f: impl FnMut(&[Value]) -> bool) -> RowStats {
        let mut stats = RowStats::default();
        self.work.set(self.work_limit);
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
                if !f(&values) || self.work.get() == 0 {
                    stats.stopped = true;
                    break 'pages;
                }
            }
        }
        if table.truncated {
            stats.stopped = true;
        }
        // The row a pointer leads to is itself marked deleted; it is not a deleted row.
        stats.deleted = stats.deleted.saturating_sub(followed);
        stats
    }

    /// The row an overflow pointer (row number, then page) leads to.
    fn follow_overflow(&self, mut ptr: &'a [u8]) -> Option<&'a [u8]> {
        for _ in 0..MAX_OVERFLOW_DEPTH {
            let row = usize::from(*ptr.first()?);
            let page = page3(ptr, 1)?;
            let (raw, bytes) = self.row_at(page, row)?;
            if raw & ROW_OVERFLOW == 0 {
                return Some(bytes);
            }
            ptr = bytes;
        }
        None
    }

    /// The offsets of the variable-length values of a row: value `i` is `offsets[i]..offsets[i+1]`.
    fn var_offsets(&self, row: &[u8], mask_len: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let Some(mask_at) = row.len().checked_sub(mask_len) else { return out };
        if self.lay.jet3 {
            // One byte per offset, with "jump" entries for rows longer than 255 bytes.
            let Some(end) = row.len().checked_sub(1) else { return out };
            let Some(count_at) = end.checked_sub(mask_len) else { return out };
            let count = usize::from(row.get(count_at).copied().unwrap_or(0));
            let mut jumps = end / 256;
            let Some(offsets_at) = count_at.checked_sub(jumps + 1) else { return out };
            if offsets_at.saturating_sub(count) / 256 < jumps {
                jumps = jumps.saturating_sub(1);
            }
            let mut used = 0usize;
            for i in 0..=count {
                while used < jumps {
                    let Some(at) = count_at.checked_sub(used + 1) else { break };
                    if row.get(at).map(|b| usize::from(*b)) == Some(i) {
                        used += 1;
                    } else {
                        break;
                    }
                }
                let Some(at) = offsets_at.checked_sub(i) else { return Vec::new() };
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
            // A calculated Yes/No column keeps its result in the row like any other fixed value
            // and uses the null bit as "has a value".
            if c.ty == ty::BOOL && !c.is_calculated() {
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
            let value = bytes.map_or(Value::Null, |b| {
                if self.spend(u64::try_from(b.len()).unwrap_or(u64::MAX) / WORK_BYTES_PER_UNIT) {
                    self.value(c, b)
                } else {
                    // Out of work: the scan ends after this row (see `Db::rows`).
                    Value::Null
                }
            });
            out.push(value);
        }
        out
    }

    fn value(&self, c: &Column, d: &[u8]) -> Value {
        // The result of a calculated column sits behind a header, except in a long value, where
        // the header is part of the stored bytes and is removed after they are fetched.
        let d = if c.is_calculated() && c.ty != ty::MEMO && c.ty != ty::OLE {
            unwrap_calculated(d)
        } else {
            d
        };
        let arr = |n: usize| d.get(..n);
        match c.ty {
            ty::BOOL => Value::Bool(d.first().is_some_and(|b| *b != 0)),
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
            ty::TEXT => Value::Text(self.text(d)),
            ty::MEMO => {
                let bytes = self.long_value(d).unwrap_or_default();
                let bytes = if c.is_calculated() { unwrap_calculated(&bytes) } else { &bytes };
                let text = self.text(bytes);
                if c.flags & FLAG_HYPERLINK != 0 { link(&text) } else { Value::Text(text) }
            }
            ty::OLE if self.keep_blobs.get() => Value::Blob(self.long_value(d).unwrap_or_default()),
            ty::OLE => Value::Ole(long_value_len(d)),
            ty::GUID => guid(d).map_or(Value::Null, Value::Guid),
            ty::NUMERIC if c.is_calculated() => calc_numeric(d).map_or(Value::Null, Value::Numeric),
            ty::NUMERIC => numeric(d, c.scale).map_or(Value::Null, Value::Numeric),
            ty::COMPLEX => d
                .get(..4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(Value::Null, |b| Value::Complex(i64::from(i32::from_le_bytes(b)))),
            ty::EXT_DATE => ext_date(d).map_or(Value::Null, Value::ExtDate),
            ty::BINARY if self.keep_blobs.get() => Value::Blob(d.to_vec()),
            _ => Value::Binary(d.len()),
        }
    }

    /// Decodes the text of a value.
    fn text(&self, d: &[u8]) -> String {
        if self.lay.jet3 {
            return decode_codepage(d, self.code_page);
        }
        // Jet 4 stores short text compressed: after the marker FF FE, bytes are Latin-1
        // characters, and a zero byte switches between that and plain UTF-16.
        if d.starts_with(&[0xFF, 0xFE]) {
            let mut out = String::with_capacity(d.len());
            let mut compressed = true;
            let mut seg_start = 2;
            for i in 2..=d.len() {
                if i == d.len() || d.get(i) == Some(&0) {
                    let seg = d.get(seg_start..i).unwrap_or(&[]);
                    if compressed {
                        out.extend(seg.iter().map(|b| char::from(*b)));
                    } else {
                        out.push_str(&utf16le_lossy(seg));
                    }
                    compressed = !compressed;
                    seg_start = i + 1;
                }
            }
            return out;
        }
        utf16le_lossy(d)
    }

    /// Takes `units` from the work budget of the current scan; `false` once it is used up.
    fn spend(&self, units: u64) -> bool {
        let left = self.work.get();
        if left < units {
            self.work.set(0);
            return false;
        }
        self.work.set(left - units);
        true
    }

    /// The bytes a memo or OLE value points to (or holds).
    fn long_value(&self, def: &[u8]) -> Option<Vec<u8>> {
        let word = u32_le(def, 0)?;
        let len = usize::try_from(word & 0x3FFF_FFFF).ok()?.min(MAX_LONG_VALUE);
        match word >> 30 {
            // The value follows the 12-byte header.
            2 => {
                let rest = def.get(12..)?;
                let part = rest.get(..len.min(rest.len()))?;
                self.spend(u64::try_from(part.len()).unwrap_or(u64::MAX) / WORK_BYTES_PER_UNIT)
                    .then(|| part.to_vec())
            }
            // The value is one row of another page.
            1 => {
                let (row, page) = (usize::from(*def.get(4)?), page3(def, 5)?);
                let (_, bytes) = self.row_at(page, row)?;
                let part = bytes.get(..len.min(bytes.len()))?;
                self.spend(u64::try_from(part.len()).unwrap_or(u64::MAX) / WORK_BYTES_PER_UNIT)
                    .then(|| part.to_vec())
            }
            // The value is a chain of rows, each starting with a pointer to the next.
            0 => {
                let mut out = Vec::new();
                let (mut row, mut page) = (usize::from(*def.get(4)?), page3(def, 5)?);
                // A genuine chain never visits the same row twice; a cycle is damage or an
                // attack, and is cut instead of followed until the page count runs out.
                let mut visited = std::collections::BTreeSet::new();
                for _ in 0..self.pages {
                    if out.len() >= len || page == 0 {
                        break;
                    }
                    if !visited.insert((page, row)) || !self.spend(1) {
                        return None;
                    }
                    let (_, bytes) = self.row_at(page, row)?;
                    let chunk = bytes.get(4..)?;
                    let take = chunk.len().min(len - out.len());
                    let bytes_cost = u64::try_from(take).unwrap_or(u64::MAX) / WORK_BYTES_PER_UNIT;
                    if !self.spend(bytes_cost) {
                        return None;
                    }
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

/// The result of a calculated column. Access (2010 and later) keeps it behind a 20-byte header
/// whose last four bytes are the length of the result (the open-source Jackcess library documents
/// this as `CalculatedColumnUtil`). A value too short to carry the header is returned as it is.
fn unwrap_calculated(d: &[u8]) -> &[u8] {
    let Some(len) = u32_le(d, CALC_LEN_AT) else { return d };
    let Some(rest) = d.get(CALC_DATA_AT..) else { return d };
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    rest.get(..len.min(rest.len())).unwrap_or(&[])
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
    Some(decimal(negative, v, scale))
}

/// The result of a calculated decimal column. Behind the calculated-value header (already
/// removed) it holds a 2-byte length of what follows, the scale, the sign and the magnitude as
/// 1 to 4 little-endian words, most significant first (the open-source Jackcess library documents
/// this as `CalcNumericColImpl`). An empty result is no value.
fn calc_numeric(d: &[u8]) -> Option<String> {
    let total = i16::from_le_bytes([*d.first()?, *d.get(1)?]);
    let after = d.get(2..)?;
    let counted = usize::try_from(total).ok().filter(|t| *t > 0).unwrap_or(after.len());
    // The length counts the scale and the sign; only whole words are magnitude.
    let words = (counted.checked_sub(2)? / 4 * 4).min(16);
    let scale = *after.first()?;
    let negative = *after.get(1)? != 0;
    let magnitude = after.get(2..2usize.checked_add(words)?)?;
    let mut v: u128 = 0;
    for w in magnitude.chunks_exact(4) {
        v = (v << 32) | u128::from(u32_le(w, 0)?);
    }
    Some(decimal(negative, v, scale))
}

/// Writes a decimal number with `scale` digits after the point; zero is never negative.
fn decimal(negative: bool, v: u128, scale: u8) -> String {
    let digits = v.to_string();
    let scale = usize::from(scale).min(38);
    let body = if scale == 0 {
        digits
    } else {
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (int, frac) = padded.split_at(padded.len() - scale);
        format!("{int}.{frac}")
    };
    if negative && v != 0 { format!("-{body}") } else { body }
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

/// The inverse of [`days_from_civil`]. The caller bounds `z` (see [`ext_date`]), so none of the
/// arithmetic below can overflow; everything is `i64`, which also avoids signed/unsigned casts.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
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
    // Access dates span the years 100..=9999; anything else is a damaged value, not a date to
    // print, and the bound keeps the calendar arithmetic far from overflow.
    if !(days_from_civil(100, 1, 1)..=days_from_civil(9999, 12, 31)).contains(&day) {
        return None;
    }
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

    /// A Jet 4 database whose catalog table has `cols` fixed-length text columns that all start
    /// at the same place of a row, and `rows` rows of about a page each (one per data page).
    fn hostile_database(rows: usize, cols: usize) -> Vec<u8> {
        const PAGE: usize = 4096;
        let row_len = 4000usize;
        let col_len = 3900u16;
        let mut db = vec![0u8; PAGE * (4 + rows)];
        // Page 0: the header, with its mask applied to all-zero contents (no key, no password).
        db[4..19].copy_from_slice(b"Standard Jet DB");
        db[0x14] = 1;
        for (i, m) in HEADER_MASK.iter().enumerate() {
            db[24 + i] = *m;
        }
        // Page 2: the table definition.
        let def = &mut db[2 * PAGE..3 * PAGE];
        def[0] = PAGE_TABLE_DEF;
        def[16..20].copy_from_slice(&u32::try_from(rows).unwrap().to_le_bytes());
        def[45..47].copy_from_slice(&u16::try_from(cols).unwrap().to_le_bytes());
        def[55] = 0; // usage map: row 0 of page 3
        def[56..59].copy_from_slice(&[3, 0, 0]);
        let mut names_at = 63 + cols * 25;
        for i in 0..cols {
            let h = 63 + i * 25;
            def[h] = ty::TEXT;
            def[h + 5..h + 7].copy_from_slice(&u16::try_from(i).unwrap().to_le_bytes());
            def[h + 15] = FLAG_FIXED;
            def[h + 23..h + 25].copy_from_slice(&col_len.to_le_bytes());
            let name: Vec<u8> = format!("c{i}").encode_utf16().flat_map(u16::to_le_bytes).collect();
            let len = u16::try_from(name.len()).unwrap();
            def[names_at..names_at + 2].copy_from_slice(&len.to_le_bytes());
            def[names_at + 2..names_at + 2 + name.len()].copy_from_slice(&name);
            names_at += 2 + name.len();
        }
        // Page 3: one row, the usage map: pages 4.. hold the table.
        let mut map = vec![0u8; 5 + (4 + rows).div_ceil(8)];
        for page in 4..4 + rows {
            map[5 + page / 8] |= 1 << (page % 8);
        }
        let page3 = &mut db[3 * PAGE..4 * PAGE];
        page3[0] = PAGE_DATA;
        page3[12..14].copy_from_slice(&1u16.to_le_bytes());
        let at = PAGE - map.len();
        page3[14..16].copy_from_slice(&u16::try_from(at).unwrap().to_le_bytes());
        page3[at..].copy_from_slice(&map);
        // Pages 4..: one row each, every column present.
        for n in 0..rows {
            let page = &mut db[(4 + n) * PAGE..(5 + n) * PAGE];
            page[0] = PAGE_DATA;
            page[12..14].copy_from_slice(&1u16.to_le_bytes());
            let at = PAGE - row_len;
            page[14..16].copy_from_slice(&u16::try_from(at).unwrap().to_le_bytes());
            let row = &mut page[at..];
            row[0..2].copy_from_slice(&u16::try_from(cols).unwrap().to_le_bytes());
            for b in &mut row[2..2 + usize::from(col_len)] {
                *b = b'a';
            }
            let mask_at = row_len - cols.div_ceil(8);
            row[mask_at..].fill(0xFF);
        }
        db
    }

    #[test]
    fn the_synthetic_database_reads_back() {
        let bytes = hostile_database(3, 20);
        let db = Db::open(&bytes).unwrap();
        let table = db.table(CATALOG_PAGE).unwrap();
        assert_eq!(table.columns.len(), 20);
        let mut seen = Vec::new();
        let stats = db.rows(&table, |vals| {
            seen.push(vals.len());
            true
        });
        assert_eq!(seen, [20, 20, 20]);
        assert!(!stats.stopped);
    }

    #[test]
    fn columns_that_read_the_same_bytes_cannot_multiply_the_work_of_a_scan() {
        // 90 columns each reading 3 900 bytes of a 4 000-byte row: 90 times the bytes of the row,
        // on every one of 1 500 rows. The scan has a budget and stops when it is spent (a small
        // one here, to keep the test quick: 5 MB read in all).
        let rows = 1_500;
        let bytes = hostile_database(rows, 90);
        let db = Db::open(&bytes).unwrap().with_work_limit(20_000);
        let table = db.table(CATALOG_PAGE).unwrap();
        let mut seen = 0usize;
        let stats = db.rows(&table, |_| {
            seen += 1;
            true
        });
        assert!(stats.stopped, "the scan must report that it stopped early");
        assert!(seen > 0 && seen < rows, "{seen} of {rows} rows were decoded");
        // A fresh scan has a fresh budget.
        let mut again = 0usize;
        db.rows(&table, |_| {
            again += 1;
            true
        });
        assert_eq!(again, seen);
        // The budget can only be lowered.
        let db = Db::open(&bytes).unwrap().with_work_limit(u64::MAX);
        assert_eq!(db.work_limit, MAX_SCAN_WORK);
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
