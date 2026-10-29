// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! A writer of small Access (Jet 4 / ACE) databases, for tests only.
//!
//! It follows the layouts the reader documents: page 0 is the masked header, page 2 the
//! definition of `MSysObjects`, a table is a definition page, a page holding its usage map and
//! data pages. Rows are written the way Jet 4 writes them: the column count, the fixed-length
//! values, the variable-length values, the table of their end offsets, their count, and the null
//! mask (a set bit means "has a value"; a Yes/No value is its bit).
//!
//! The file is std-only so the test modules of other crates can include it with `#[path]`:
//! tests for a reader need databases whose every byte they control, including structures
//! (saved queries, complex columns, calculated columns, property lists) that no available
//! database-writing library can produce.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::missing_docs_in_private_items
)]

const PAGE: usize = 4096;

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

/// Column type codes.
pub(crate) mod t {
    pub const BOOL: u8 = 0x01;
    pub const BYTE: u8 = 0x02;
    pub const INT: u8 = 0x03;
    pub const LONG: u8 = 0x04;
    pub const MONEY: u8 = 0x05;
    pub const FLOAT: u8 = 0x06;
    pub const DOUBLE: u8 = 0x07;
    pub const DATE: u8 = 0x08;
    pub const BINARY: u8 = 0x09;
    pub const TEXT: u8 = 0x0A;
    pub const OLE: u8 = 0x0B;
    pub const MEMO: u8 = 0x0C;
    pub const GUID: u8 = 0x0F;
    pub const NUMERIC: u8 = 0x10;
    pub const COMPLEX: u8 = 0x12;
    pub const BIGINT: u8 = 0x13;
    pub const EXT_DATE: u8 = 0x14;
}

/// What one column looks like.
#[derive(Clone)]
pub(crate) struct ColSpec {
    pub name: String,
    pub ty: u8,
    /// Column flag bits besides "fixed" (which the type decides): 0x04 counter, 0x10 hidden, ...
    pub flags: u8,
    /// Extended flags: 0x08 complex key, 0xC0 calculated.
    pub ext: u8,
    /// A length that overrides the default size of a fixed value (calculated columns).
    pub length: u16,
    pub scale: u8,
    pub complex_id: u32,
}

impl ColSpec {
    pub fn new(name: &str, ty: u8) -> Self {
        Self { name: name.to_owned(), ty, flags: 0, ext: 0, length: 0, scale: 0, complex_id: 0 }
    }

    pub fn flags(mut self, f: u8) -> Self {
        self.flags = f;
        self
    }

    pub fn ext(mut self, e: u8) -> Self {
        self.ext = e;
        self
    }

    pub fn length(mut self, l: u16) -> Self {
        self.length = l;
        self
    }

    pub fn scale(mut self, s: u8) -> Self {
        self.scale = s;
        self
    }

    pub fn complex(mut self, id: u32) -> Self {
        self.complex_id = id;
        self
    }

    fn fixed_size(&self) -> Option<usize> {
        let own = match self.ty {
            t::BOOL => 0,
            t::BYTE => 1,
            t::INT => 2,
            t::LONG | t::FLOAT | t::COMPLEX => 4,
            t::MONEY | t::DOUBLE | t::DATE | t::BIGINT => 8,
            t::GUID => 16,
            t::NUMERIC => 17,
            t::EXT_DATE => 42,
            _ => return None,
        };
        Some(own.max(usize::from(self.length)))
    }
}

/// A value to store.
#[derive(Clone)]
pub(crate) enum Cv {
    Null,
    Int(i64),
    Bool(bool),
    /// UTF-16LE text, stored uncompressed.
    Text(String),
    /// Text with the "compressed Unicode" marker, bytes as given after FF FE.
    Compressed(Vec<u8>),
    /// The bytes exactly as stored in the row (fixed values are padded with zeros).
    Raw(Vec<u8>),
    /// A memo whose text follows its 12-byte header inside the row.
    Memo(String),
    /// A memo or object whose bytes follow the header inside the row.
    Long(Vec<u8>),
    /// A memo or object stored as a chain of rows in pages of their own (any length).
    Big(Vec<u8>),
    /// A memo or object whose chain of rows leads back to its first row (a hostile file).
    Loop,
}

/// An index of a table.
#[derive(Clone)]
pub(crate) struct IndexSpec {
    pub name: String,
    /// Indexes into the column list, with `true` for ascending.
    pub columns: Vec<(usize, bool)>,
    /// 1 primary key, 2 foreign key, 0 normal.
    pub kind: u8,
    pub unique: bool,
}

struct Object {
    id: i64,
    kind: i64,
    name: String,
    flags: i64,
    props: Option<Vec<u8>>,
    connect: String,
    database: String,
}

pub(crate) struct TestDb {
    pages: Vec<Vec<u8>>,
    objects: Vec<Object>,
    version: u8,
}

fn le16(v: usize) -> [u8; 2] {
    (v as u16).to_le_bytes()
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn long_header(len: usize) -> Vec<u8> {
    let mut v = (0x8000_0000u32 | len as u32).to_le_bytes().to_vec();
    v.extend([0u8; 8]);
    v
}

impl TestDb {
    /// An empty database; `version` is the version byte (1 for Access 2000, 3 for 2010).
    pub fn new(version: u8) -> Self {
        Self { pages: vec![vec![0; PAGE]; 3], objects: Vec::new(), version }
    }

    fn alloc(&mut self) -> usize {
        self.pages.push(vec![0; PAGE]);
        self.pages.len() - 1
    }

    /// Adds an object that is not a table (a query, a form, a macro, ...).
    pub fn add_object(&mut self, id: i64, kind: i64, name: &str, flags: i64) {
        self.objects.push(Object {
            id,
            kind,
            name: name.to_owned(),
            flags,
            props: None,
            connect: String::new(),
            database: String::new(),
        });
    }

    /// Adds an object with a property list (`LvProp`).
    pub fn add_object_with_props(&mut self, id: i64, kind: i64, name: &str, props: Vec<u8>) {
        self.add_object(id, kind, name, 0);
        if let Some(o) = self.objects.last_mut() {
            o.props = Some(props);
        }
    }

    /// Adds a linked table to the catalog.
    pub fn add_link(&mut self, id: i64, kind: i64, name: &str, connect: &str, database: &str) {
        self.add_object(id, kind, name, 0);
        if let Some(o) = self.objects.last_mut() {
            o.connect = connect.to_owned();
            o.database = database.to_owned();
        }
    }

    /// Adds a table and returns the page of its definition (its catalog identifier). `flags` are
    /// the catalog flags: 0 for a user table, 0x8000_0000 for a system table.
    pub fn add_table(
        &mut self,
        name: &str,
        flags: i64,
        cols: &[ColSpec],
        rows: &[Vec<Cv>],
        indexes: &[IndexSpec],
    ) -> usize {
        let page = self.write_table(None, cols, rows, indexes);
        self.objects.push(Object {
            id: page as i64,
            kind: 1,
            name: name.to_owned(),
            flags,
            props: None,
            connect: String::new(),
            database: String::new(),
        });
        page
    }

    fn write_table(
        &mut self,
        fixed_page: Option<usize>,
        cols: &[ColSpec],
        rows: &[Vec<Cv>],
        indexes: &[IndexSpec],
    ) -> usize {
        let tdef = fixed_page.unwrap_or_else(|| self.alloc());
        let map_page = self.alloc();
        // Column placement.
        let mut fixed_at = Vec::new();
        let mut var_idx = Vec::new();
        let (mut fixed_len, mut nvar) = (0usize, 0usize);
        for c in cols {
            match c.fixed_size() {
                Some(sz) => {
                    fixed_at.push(fixed_len);
                    var_idx.push(0);
                    fixed_len += sz;
                }
                None => {
                    fixed_at.push(0);
                    var_idx.push(nvar);
                    nvar += 1;
                }
            }
        }
        // Data pages.
        let mut data_pages: Vec<usize> = Vec::new();
        let mut page_rows: Vec<Vec<u8>> = Vec::new();
        let mut used = 14usize;
        let mut cur: Vec<Vec<u8>> = Vec::new();
        let mut groups: Vec<Vec<Vec<u8>>> = Vec::new();
        for r in rows {
            let r: Vec<Cv> = r
                .iter()
                .map(|c| match c {
                    Cv::Big(b) => Cv::Raw(self.store_big(b, false)),
                    Cv::Loop => Cv::Raw(self.store_big(&[7; 100], true)),
                    other => other.clone(),
                })
                .collect();
            let bytes = self.row_bytes(cols, &r, fixed_len, &fixed_at);
            assert!(bytes.len() + 2 + 14 <= PAGE, "row of {} bytes", bytes.len());
            if used + 2 + bytes.len() > PAGE {
                groups.push(std::mem::take(&mut cur));
                used = 14;
            }
            used += 2 + bytes.len();
            cur.push(bytes);
        }
        if !cur.is_empty() {
            groups.push(cur);
        }
        for g in groups {
            let p = self.alloc();
            data_pages.push(p);
            page_rows.push(Vec::new());
            let page = &mut self.pages[p];
            page[0] = 1;
            page[4..8].copy_from_slice(&(tdef as u32).to_le_bytes());
            page[12..14].copy_from_slice(&le16(g.len()));
            let mut end = PAGE;
            for (i, row) in g.iter().enumerate() {
                let start = end - row.len();
                page[start..end].copy_from_slice(row);
                page[14 + 2 * i..16 + 2 * i].copy_from_slice(&le16(start));
                end = start;
            }
        }
        // The usage map: type 0, start page 0, a bit per page.
        let top = data_pages.iter().copied().max().unwrap_or(0).max(tdef);
        let mut bitmap = vec![0u8; top / 8 + 1];
        for &p in &data_pages {
            bitmap[p / 8] |= 1 << (p % 8);
        }
        let mut map = vec![0u8];
        map.extend([0u8; 4]);
        map.extend(bitmap);
        {
            let page = &mut self.pages[map_page];
            page[0] = 1;
            page[12..14].copy_from_slice(&le16(1));
            let start = PAGE - map.len();
            page[14..16].copy_from_slice(&le16(start));
            page[start..].copy_from_slice(&map);
        }
        // The definition.
        let mut d = vec![0u8; 63];
        d[0] = 2;
        d[16..20].copy_from_slice(&(rows.len() as u32).to_le_bytes());
        d[45..47].copy_from_slice(&le16(cols.len()));
        d[47..51].copy_from_slice(&(indexes.len() as u32).to_le_bytes());
        d[51..55].copy_from_slice(&(indexes.len() as u32).to_le_bytes());
        d[55] = 0;
        d[56..59].copy_from_slice(&(map_page as u32).to_le_bytes()[..3]);
        d.extend(vec![0u8; 12 * indexes.len()]);
        for (i, c) in cols.iter().enumerate() {
            let mut h = [0u8; 25];
            h[0] = c.ty;
            h[5..7].copy_from_slice(&le16(i));
            h[7..9].copy_from_slice(&le16(var_idx[i]));
            if c.ty == t::COMPLEX {
                h[11..15].copy_from_slice(&c.complex_id.to_le_bytes());
            } else {
                h[12] = c.scale;
            }
            let fixed = c.fixed_size().is_some();
            h[15] = c.flags | u8::from(fixed);
            h[16] = c.ext;
            h[21..23].copy_from_slice(&le16(fixed_at[i]));
            let len = c.fixed_size().unwrap_or(510);
            h[23..25].copy_from_slice(&le16(len));
            d.extend(h);
        }
        for c in cols {
            let n = utf16(&c.name);
            d.extend(le16(n.len()));
            d.extend(n);
        }
        for ix in indexes {
            let mut b = [0u8; 52];
            for k in 0..10 {
                let o = 4 + 3 * k;
                match ix.columns.get(k) {
                    Some((col, asc)) => {
                        b[o..o + 2].copy_from_slice(&le16(*col));
                        b[o + 2] = u8::from(*asc);
                    }
                    None => b[o..o + 2].copy_from_slice(&[0xFF, 0xFF]),
                }
            }
            b[46] = u8::from(ix.unique);
            d.extend(b);
        }
        for (i, ix) in indexes.iter().enumerate() {
            let mut b = [0u8; 28];
            b[4..8].copy_from_slice(&(i as u32).to_le_bytes());
            b[8..12].copy_from_slice(&(i as u32).to_le_bytes());
            b[23] = ix.kind;
            d.extend(b);
        }
        for ix in indexes {
            let n = utf16(&ix.name);
            d.extend(le16(n.len()));
            d.extend(n);
        }
        assert!(d.len() <= PAGE, "table definition of {} bytes", d.len());
        self.pages[tdef][..d.len()].copy_from_slice(&d);
        tdef
    }

    /// Stores `bytes` as a chain of one-row pages and returns the 12-byte pointer that goes in
    /// the table's row. With `cyclic` the last chunk points back to the first one.
    fn store_big(&mut self, bytes: &[u8], cyclic: bool) -> Vec<u8> {
        const CHUNK: usize = 4000;
        let chunks: Vec<&[u8]> = bytes.chunks(CHUNK).collect();
        let pages: Vec<usize> = chunks.iter().map(|_| self.alloc()).collect();
        for (i, c) in chunks.iter().enumerate() {
            let next = match pages.get(i + 1) {
                Some(p) => *p,
                None if cyclic => pages[0],
                None => 0,
            };
            let mut row = vec![0u8]; // the next row is row 0 of the next page
            row.extend_from_slice(&(next as u32).to_le_bytes()[..3]);
            row.extend_from_slice(c);
            let page = &mut self.pages[pages[i]];
            page[0] = 1;
            page[12..14].copy_from_slice(&le16(1));
            let start = PAGE - row.len();
            page[14..16].copy_from_slice(&le16(start));
            page[start..].copy_from_slice(&row);
        }
        let declared = if cyclic { 1 << 20 } else { bytes.len() as u32 };
        let mut ptr = (declared & 0x3FFF_FFFF).to_le_bytes().to_vec();
        ptr.push(0);
        ptr.extend_from_slice(&(pages.first().copied().unwrap_or(0) as u32).to_le_bytes()[..3]);
        ptr.extend([0u8; 4]);
        ptr
    }

    fn row_bytes(
        &self,
        cols: &[ColSpec],
        cells: &[Cv],
        fixed_len: usize,
        fixed_at: &[usize],
    ) -> Vec<u8> {
        let mut row = vec![0u8; 2 + fixed_len];
        row[..2].copy_from_slice(&le16(cols.len()));
        let mask_len = cols.len().div_ceil(8);
        let mut mask = vec![0u8; mask_len];
        let mut var: Vec<Option<Vec<u8>>> = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            let cell = cells.get(i).cloned().unwrap_or(Cv::Null);
            if c.ty == t::BOOL && c.ext & 0xC0 == 0 {
                if matches!(cell, Cv::Bool(true) | Cv::Int(1..)) {
                    mask[i / 8] |= 1 << (i % 8);
                }
                continue;
            }
            let bytes: Option<Vec<u8>> = match (&cell, c.fixed_size()) {
                (Cv::Null, _) => None,
                (Cv::Int(v), Some(sz)) => Some(v.to_le_bytes()[..sz.min(8)].to_vec()),
                (Cv::Int(v), None) => Some(v.to_le_bytes().to_vec()),
                (Cv::Bool(b), _) => Some(vec![u8::from(*b)]),
                (Cv::Text(s), _) => Some(utf16(s)),
                (Cv::Compressed(b), _) => {
                    let mut v = vec![0xFF, 0xFE];
                    v.extend(b);
                    Some(v)
                }
                (Cv::Raw(b), _) => Some(b.clone()),
                (Cv::Memo(s), _) => {
                    let text = utf16(s);
                    let mut v = long_header(text.len());
                    v.extend(text);
                    Some(v)
                }
                (Cv::Big(_) | Cv::Loop, _) => None,
                (Cv::Long(b), _) => {
                    let mut v = long_header(b.len());
                    v.extend(b);
                    Some(v)
                }
            };
            match c.fixed_size() {
                Some(sz) => {
                    if let Some(b) = bytes {
                        mask[i / 8] |= 1 << (i % 8);
                        let at = 2 + fixed_at[i];
                        let n = b.len().min(sz);
                        row[at..at + n].copy_from_slice(&b[..n]);
                    }
                }
                None => {
                    if bytes.is_some() {
                        mask[i / 8] |= 1 << (i % 8);
                    }
                    var.push(bytes);
                }
            }
        }
        let mut offsets = Vec::new();
        for v in &var {
            offsets.push(row.len());
            if let Some(b) = v {
                row.extend(b);
            }
        }
        let eod = row.len();
        // The offsets, last first: the end of the data, then the start of each value in reverse.
        row.extend(le16(eod));
        for o in offsets.iter().rev() {
            row.extend(le16(*o));
        }
        row.extend(le16(var.len()));
        row.extend(mask);
        row
    }

    /// Writes the catalog and returns the file.
    pub fn finish(mut self) -> Vec<u8> {
        let cols = [
            ColSpec::new("Id", t::LONG),
            ColSpec::new("ParentId", t::LONG),
            ColSpec::new("Name", t::TEXT),
            ColSpec::new("Type", t::INT),
            ColSpec::new("Flags", t::LONG),
            ColSpec::new("LvProp", t::OLE),
            ColSpec::new("Connect", t::TEXT),
            ColSpec::new("Database", t::TEXT),
        ];
        let mut rows: Vec<Vec<Cv>> = Vec::new();
        // The catalog lists itself.
        rows.push(vec![
            Cv::Int(2),
            Cv::Int(0),
            Cv::Text("MSysObjects".into()),
            Cv::Int(1),
            Cv::Int(i64::from(0x8000_0000u32 as i32)),
            Cv::Null,
            Cv::Null,
            Cv::Null,
        ]);
        for o in &self.objects {
            let text = |s: &str| if s.is_empty() { Cv::Null } else { Cv::Text(s.to_owned()) };
            rows.push(vec![
                Cv::Int(o.id),
                Cv::Int(0),
                Cv::Text(o.name.clone()),
                Cv::Int(o.kind),
                Cv::Int(o.flags),
                o.props.clone().map_or(Cv::Null, Cv::Long),
                text(&o.connect),
                text(&o.database),
            ]);
        }
        self.write_table(Some(2), &cols, &rows, &[]);
        let mut out = Vec::with_capacity(self.pages.len() * PAGE);
        for p in &self.pages {
            out.extend_from_slice(p);
        }
        let magic: &[u8] = if self.version >= 2 { b"Standard ACE DB" } else { b"Standard Jet DB" };
        out[4..19].copy_from_slice(magic);
        out[0x14] = self.version;
        for (i, m) in HEADER_MASK.iter().enumerate() {
            out[24 + i] = *m;
        }
        out
    }
}

/// The cells of a row from a short description: `i(5)`, `s("x")` and so on are spelled by the
/// tests with the enum directly; this helper makes a text cell.
pub(crate) fn text(s: &str) -> Cv {
    Cv::Text(s.to_owned())
}

pub(crate) fn int(v: i64) -> Cv {
    Cv::Int(v)
}

/// A database whose catalog lists exactly the given objects, `(type, name)`, besides itself.
/// `version` is the version byte (1 for Access 2000, 3 for 2010). For the tests of other crates.
#[cfg(feature = "test-support")]
#[must_use]
pub fn database_with_objects(version: u8, objects: &[(i64, &str)]) -> Vec<u8> {
    let mut db = TestDb::new(version);
    for (n, (kind, name)) in (10i64..).zip(objects) {
        db.add_object(n, *kind, name, 0);
    }
    db.finish()
}
