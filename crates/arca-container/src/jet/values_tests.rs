//! Value decoding checked against exact byte vectors, one per column type, plus the structures
//! built on them (calculated columns, complex columns, property lists, index lists) read from
//! databases written by the test-only writer.
//!
//! The byte layouts are the ones the open-source Jackcess library documents and applies to the
//! files Access itself writes (`ColumnImpl`, `CalculatedColumnUtil`, `ComplexColumnSupport`,
//! `PropertyMaps`, `TableImpl`); the vectors here were worked out from those descriptions by
//! hand, not produced by this reader.

use super::testdb::{Cv, ColSpec, IndexSpec, TestDb, int, t, text};
use super::*;

fn column(ty: u8) -> Column {
    Column {
        name: "c".into(),
        number: 0,
        ty,
        variable: true,
        var_index: 0,
        fixed_offset: 0,
        length: 0,
        scale: 0,
        flags: 0,
        ext_flags: 0,
        complex_id: 0,
    }
}

/// A Jet 4 database with nothing in it, for the value decoder (it only needs the format).
fn empty_jet4() -> Vec<u8> {
    TestDb::new(1).finish()
}

fn decode(c: &Column, bytes: &[u8]) -> Value {
    let data = empty_jet4();
    let db = Db::open(&data).unwrap();
    db.value(c, bytes)
}

fn jet3() -> Db<'static> {
    Db {
        data: &[],
        lay: &JET3,
        pages: 0,
        code_page: CodePage::Windows1252,
        work: Cell::new(MAX_SCAN_WORK),
        work_limit: MAX_SCAN_WORK,
        keep_blobs: Cell::new(false),
    }
}

#[test]
fn integers_are_little_endian_and_signed_except_byte() {
    assert_eq!(decode(&column(ty::BYTE), &[0xFF]), Value::Int(255));
    assert_eq!(decode(&column(ty::INT), &[0xFE, 0xFF]), Value::Int(-2));
    assert_eq!(decode(&column(ty::INT), &[0xFF, 0x7F]), Value::Int(32767));
    assert_eq!(decode(&column(ty::LONG), &[0, 0, 0, 0x80]), Value::Int(-2_147_483_648));
    assert_eq!(decode(&column(ty::LONG), &[0x78, 0x56, 0x34, 0x12]), Value::Int(0x1234_5678));
    assert_eq!(decode(&column(ty::BIGINT), &[0xFF; 8]), Value::Int(-1));
    assert_eq!(decode(&column(ty::BIGINT), &[0, 0, 0, 0, 0, 0, 0, 0x80]), Value::Int(i64::MIN));
    assert_eq!(decode(&column(ty::BIGINT), &[1, 0, 0, 0, 0, 0, 0, 0x7F]), Value::Int(0x7F00_0000_0000_0001));
    // A value that is too short is no value.
    assert_eq!(decode(&column(ty::LONG), &[1, 2, 3]), Value::Null);
}

#[test]
fn currency_is_a_signed_count_of_ten_thousandths() {
    assert_eq!(decode(&column(ty::MONEY), &12_345i64.to_le_bytes()), Value::Money(12_345));
    assert_eq!(decode(&column(ty::MONEY), &(-1i64).to_le_bytes()), Value::Money(-1));
    assert_eq!(decode(&column(ty::MONEY), &i64::MAX.to_le_bytes()), Value::Money(i64::MAX));
    assert_eq!(decode(&column(ty::MONEY), &[0; 7]), Value::Null);
}

#[test]
fn floating_point_values_keep_their_bits() {
    assert_eq!(decode(&column(ty::FLOAT), &[0, 0, 0x80, 0x3F]), Value::Single(1.0));
    assert_eq!(decode(&column(ty::FLOAT), &[0, 0, 0x80, 0xBF]), Value::Single(-1.0));
    assert_eq!(decode(&column(ty::DOUBLE), &[0, 0, 0, 0, 0, 0, 0xF8, 0x3F]), Value::Double(1.5));
    match decode(&column(ty::DOUBLE), &[0, 0, 0, 0, 0, 0, 0xF8, 0x7F]) {
        Value::Double(d) => assert!(d.is_nan()),
        other => panic!("{other:?}"),
    }
    // 2.5 as a date: noon on 1900-01-01 (day 2 is 1900-01-01), kept as the raw number.
    assert_eq!(decode(&column(ty::DATE), &2.5f64.to_le_bytes()), Value::Date(2.5));
}

#[test]
fn guids_use_the_mixed_byte_order() {
    let d = [
        0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    assert_eq!(
        decode(&column(ty::GUID), &d),
        Value::Guid("{00112233-4455-6677-8899-AABBCCDDEEFF}".into())
    );
}

#[test]
fn decimals_are_a_sign_byte_and_a_128_bit_magnitude_most_significant_word_first() {
    let mut c = column(ty::NUMERIC);
    c.scale = 3;
    // +1.250: magnitude 1250 in the last word.
    let mut d = vec![0u8; 17];
    d[13..17].copy_from_slice(&1250u32.to_le_bytes());
    assert_eq!(decode(&c, &d), Value::Numeric("1.250".into()));
    // The sign byte of a negative number is 0x80.
    d[0] = 0x80;
    assert_eq!(decode(&c, &d), Value::Numeric("-1.250".into()));
    // Zero is never negative.
    let mut z = vec![0u8; 17];
    z[0] = 0x80;
    assert_eq!(decode(&c, &z), Value::Numeric("0.000".into()));
    // A magnitude over 32 bits: 2^32 + 5 is words [0, 0, 1, 5].
    let mut big = vec![0u8; 17];
    big[9..13].copy_from_slice(&1u32.to_le_bytes());
    big[13..17].copy_from_slice(&5u32.to_le_bytes());
    c.scale = 0;
    assert_eq!(decode(&c, &big), Value::Numeric("4294967301".into()));
    // The largest magnitude the format has (28 or 38 digits are the limits Access enforces; the
    // reader handles all 128 bits).
    let mut max = vec![0xFFu8; 17];
    max[0] = 0;
    assert_eq!(
        decode(&c, &max),
        Value::Numeric("340282366920938463463374607431768211455".into())
    );
    assert_eq!(decode(&c, &d[..16]), Value::Null);
}

#[test]
fn extended_dates_are_ascii_digits_from_year_one() {
    let mut c = column(ty::EXT_DATE);
    c.variable = false;
    let ext = |days: u64, secs: u64, frac: u64| {
        let mut v = format!("{days:019}:{secs:012}{frac:07}").into_bytes();
        v.extend_from_slice(b":7\0");
        v
    };
    // Day 0 is 0001-01-01, which Access cannot hold: refused. 100-01-01 is the first valid day.
    assert_eq!(decode(&c, &ext(0, 0, 0)), Value::Null);
    // 1970-01-01 is day 719162 after 0001-01-01; 12:34:56 and 7 fractional digits.
    assert_eq!(
        decode(&c, &ext(719_162, 45_296, 1_234_567)),
        Value::ExtDate("1970-01-01 12:34:56.1234567".into())
    );
    // 9999-12-31 is day 3652058, the last one allowed.
    assert_eq!(
        decode(&c, &ext(3_652_058, 86_399, 0)),
        Value::ExtDate("9999-12-31 23:59:59.0000000".into())
    );
    assert_eq!(decode(&c, &ext(3_652_059, 0, 0)), Value::Null);
    // Garbage in the digits is no date.
    let mut bad = ext(719_162, 0, 0);
    bad[3] = b'x';
    assert_eq!(decode(&c, &bad), Value::Null);
    assert_eq!(decode(&c, &ext(1, 2, 3)[..30]), Value::Null);
}

#[test]
fn text_is_utf16_with_optional_compression() {
    let db_data = empty_jet4();
    let db = Db::open(&db_data).unwrap();
    // Plain UTF-16LE, with a surrogate pair.
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    assert_eq!(db.text(&utf16("Привет, 😀")), "Привет, 😀");
    assert_eq!(db.text(&[]), "");
    // Compressed: after FF FE, one byte per Latin-1 character.
    assert_eq!(db.text(&[0xFF, 0xFE, b'A', b'b', 0xE9]), "Abé");
    assert_eq!(db.text(&[0xFF, 0xFE]), "");
    // A zero byte switches to UTF-16 and the next one back.
    let mut mixed = vec![0xFF, 0xFE, b'a', 0x00];
    mixed.extend_from_slice(&utf16("д"));
    mixed.extend_from_slice(&[0x00, b'z']);
    assert_eq!(db.text(&mixed), "aдz");
    // The marker alone is not enough to be compressed when it is the only content of a
    // two-byte value that is not the marker.
    assert_eq!(db.text(&[0xFE, 0xFF]), "\u{FFFE}");
    // An odd trailing byte of UTF-16 does not panic.
    let _ = db.text(&[0x41, 0x00, 0x42]);
}

#[test]
fn access_97_text_uses_the_code_page_of_the_header() {
    let db = jet3();
    assert_eq!(db.text(b"Plain"), "Plain");
    assert_eq!(db.text(&[0x80, 0xE9]), "\u{20AC}\u{E9}");
}

#[test]
fn memo_pointers_have_three_forms() {
    let data = empty_jet4();
    let db = Db::open(&data).unwrap();
    // Inline: the text follows the 12-byte header.
    let mut inline = (0x8000_0000u32 | 4).to_le_bytes().to_vec();
    inline.extend([0u8; 8]);
    inline.extend("hé".encode_utf16().flat_map(u16::to_le_bytes));
    inline.extend(b"junk"); // beyond the declared length
    assert_eq!(db.value(&column(ty::MEMO), &inline), Value::Text("hé".into()));
    // A pointer to a row that is not there gives an empty text, not a made-up one.
    let mut dangling = 4u32.to_le_bytes().to_vec();
    dangling.extend([9, 0xFF, 0xFF, 0x00, 0, 0, 0, 0]);
    assert_eq!(db.value(&column(ty::MEMO), &dangling), Value::Text(String::new()));
    // The top bits 11 are no known form.
    let mut unknown = (0xC000_0000u32 | 4).to_le_bytes().to_vec();
    unknown.extend([0u8; 8]);
    assert_eq!(db.value(&column(ty::MEMO), &unknown), Value::Text(String::new()));
    // A hyperlink column splits at the hash marks.
    let mut link = column(ty::MEMO);
    link.flags = FLAG_HYPERLINK;
    let text = "Site#https://example.org/#";
    let mut bytes = (0x8000_0000u32 | u32::try_from(text.len() * 2).unwrap()).to_le_bytes().to_vec();
    bytes.extend([0u8; 8]);
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    assert_eq!(
        db.value(&link, &bytes),
        Value::Link("Site".into(), "https://example.org/".into())
    );
}

#[test]
fn calculated_values_sit_behind_a_header() {
    assert_eq!(unwrap_calculated(&[]), &[] as &[u8]);
    // Shorter than the header: returned as it is.
    assert_eq!(unwrap_calculated(&[1, 2, 3]), &[1, 2, 3]);
    let mut v = vec![0u8; 16];
    v.extend(3u32.to_le_bytes());
    v.extend([7, 8, 9, 0, 0, 0]);
    assert_eq!(unwrap_calculated(&v), &[7, 8, 9]);
    // A length that claims more than there is gives what there is.
    let mut short = vec![0u8; 16];
    short.extend(100u32.to_le_bytes());
    short.extend([1, 2]);
    assert_eq!(unwrap_calculated(&short), &[1, 2]);
}

fn wrapped(data: &[u8], total: usize) -> Vec<u8> {
    let mut v = vec![0u8; 16];
    v.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    v.extend(data);
    v.resize(total.max(v.len()), 0);
    v
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn scan(db: &Db<'_>, page: usize) -> Vec<Vec<Value>> {
    let table = db.table(u32::try_from(page).unwrap()).unwrap();
    let mut out = Vec::new();
    db.rows(&table, |v| {
        out.push(v.to_vec());
        true
    });
    out
}

#[test]
fn a_calculated_column_shows_the_result_access_stored() {
    let calc = 0xC0;
    let cols = [
        ColSpec::new("id", t::LONG),
        ColSpec::new("calc_long", t::LONG).ext(calc).length(39),
        ColSpec::new("calc_text", t::TEXT).ext(calc),
        ColSpec::new("calc_memo", t::MEMO).ext(calc),
        ColSpec::new("calc_bool", t::BOOL).ext(calc).length(39),
        ColSpec::new("calc_num", t::NUMERIC).ext(calc).length(39).scale(3),
    ];
    let mut num = vec![0u8; 17];
    num[13..17].copy_from_slice(&1250u32.to_le_bytes());
    let rows = vec![
        vec![
            int(1),
            Cv::Raw(wrapped(&7i32.to_le_bytes(), 39)),
            Cv::Raw(wrapped(&utf16("1_foo"), 0)),
            Cv::Long(wrapped(&utf16("a long result"), 0)),
            Cv::Raw(wrapped(&[0xFF], 39)),
            Cv::Raw(wrapped(&num, 39)),
        ],
        vec![int(2), Cv::Null, Cv::Null, Cv::Null, Cv::Null, Cv::Null],
        vec![
            int(3),
            Cv::Raw(wrapped(&(-37i32).to_le_bytes(), 39)),
            Cv::Raw(wrapped(&[], 0)),
            Cv::Long(wrapped(&[], 0)),
            Cv::Raw(wrapped(&[0], 39)),
            Cv::Raw(wrapped(&[0u8; 17], 39)),
        ],
    ];
    let mut tdb = TestDb::new(3);
    let page = tdb.add_table("T", 0, &cols, &rows, &[]);
    let data = tdb.finish();
    let db = Db::open(&data).unwrap();
    let got = scan(&db, page);
    assert_eq!(
        got[0],
        [
            Value::Int(1),
            Value::Int(7),
            Value::Text("1_foo".into()),
            Value::Text("a long result".into()),
            Value::Bool(true),
            Value::Numeric("1.250".into()),
        ]
    );
    assert!(got[1][1..].iter().all(|v| *v == Value::Null));
    assert_eq!(
        got[2],
        [
            Value::Int(3),
            Value::Int(-37),
            Value::Text(String::new()),
            Value::Text(String::new()),
            Value::Bool(false),
            Value::Numeric("0.000".into()),
        ]
    );
    let table = db.table(u32::try_from(page).unwrap()).unwrap();
    assert!(table.columns.iter().skip(1).all(Column::is_calculated));
    assert!(!table.columns[0].is_calculated());
}

#[test]
fn table_definitions_list_their_indexes() {
    let cols = [ColSpec::new("id", t::LONG), ColSpec::new("name", t::TEXT), ColSpec::new("n", t::INT)];
    let indexes = [
        IndexSpec { name: "PrimaryKey".into(), columns: vec![(0, true)], kind: 1, unique: true },
        IndexSpec { name: "byName".into(), columns: vec![(1, true), (2, false)], kind: 0, unique: false },
        IndexSpec { name: "Link".into(), columns: vec![(2, true)], kind: 2, unique: false },
    ];
    let mut tdb = TestDb::new(1);
    let page = tdb.add_table("T", 0, &cols, &[], &indexes);
    let data = tdb.finish();
    let db = Db::open(&data).unwrap();
    let table = db.table(u32::try_from(page).unwrap()).unwrap();
    let got: Vec<(&str, IndexKind, bool, Vec<(&str, bool)>)> = table
        .indexes
        .iter()
        .map(|i| {
            (
                i.name.as_str(),
                i.kind,
                i.unique,
                i.columns.iter().map(|(n, a)| (n.as_str(), *a)).collect(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("PrimaryKey", IndexKind::Primary, true, vec![("id", true)]),
            ("byName", IndexKind::Normal, false, vec![("name", true), ("n", false)]),
            ("Link", IndexKind::Foreign, false, vec![("n", true)]),
        ]
    );
}

#[test]
fn a_table_with_a_damaged_index_list_keeps_its_rows() {
    let cols = [ColSpec::new("id", t::LONG)];
    let indexes = [IndexSpec { name: "PK".into(), columns: vec![(0, true)], kind: 1, unique: true }];
    let mut tdb = TestDb::new(1);
    let page = tdb.add_table("T", 0, &cols, &[vec![int(5)]], &indexes);
    let mut data = tdb.finish();
    // The index count claims thousands of logical indexes.
    let at = page * 4096 + 47;
    data[at..at + 4].copy_from_slice(&5000u32.to_le_bytes());
    let db = Db::open(&data).unwrap();
    let table = db.table(u32::try_from(page).unwrap()).unwrap();
    assert!(table.indexes.is_empty());
    assert_eq!(scan(&db, page), [[Value::Int(5)]]);
}

/// A database with an attachment column, a multi-value column and an append-only memo, written
/// the way `MSysComplexColumns` describes them.
fn complex_database() -> (Vec<u8>, usize) {
    let mut tdb = TestDb::new(3);
    // The attachment type table and the flat table with the values.
    let att_type = tdb.add_table(
        "MSysComplexType_Attachment",
        i64::from(0x8000_0000u32 as i32),
        &[
            ColSpec::new("FileURL", t::MEMO),
            ColSpec::new("FileName", t::TEXT),
            ColSpec::new("FileType", t::TEXT),
            ColSpec::new("FileData", t::OLE),
            ColSpec::new("FileTimeStamp", t::DATE),
            ColSpec::new("FileFlags", t::LONG),
        ],
        &[],
        &[],
    );
    let att_flat = tdb.add_table(
        "f_att_Files",
        i64::from(0x8000_0000u32 as i32),
        &[
            ColSpec::new("_Id", t::LONG).flags(0x04),
            ColSpec::new("Files_FK", t::LONG).ext(0x08),
            ColSpec::new("FileURL", t::MEMO),
            ColSpec::new("FileName", t::TEXT),
            ColSpec::new("FileType", t::TEXT),
            ColSpec::new("FileData", t::OLE),
            ColSpec::new("FileTimeStamp", t::DATE),
            ColSpec::new("FileFlags", t::LONG),
        ],
        &[
            vec![int(1), int(7), Cv::Null, text("a.txt"), text("txt"), Cv::Long(vec![1; 300]), Cv::Null, int(0)],
            vec![int(2), int(7), Cv::Null, text("b.png"), text("png"), Cv::Long(vec![2; 10]), Cv::Null, int(0)],
            vec![int(3), int(8), Cv::Null, text("other.zip"), text("zip"), Cv::Long(vec![3; 5]), Cv::Null, int(0)],
        ],
        &[],
    );
    // A multi-value column of text.
    let mv_type = tdb.add_table(
        "MSysComplexType_Text",
        i64::from(0x8000_0000u32 as i32),
        &[ColSpec::new("Value", t::TEXT)],
        &[],
        &[],
    );
    let mv_flat = tdb.add_table(
        "f_mv_Tags",
        i64::from(0x8000_0000u32 as i32),
        &[
            ColSpec::new("_Id", t::LONG).flags(0x04),
            ColSpec::new("Tags_FK", t::LONG).ext(0x08),
            ColSpec::new("Value", t::TEXT),
        ],
        &[
            vec![int(1), int(21), text("red")],
            vec![int(2), int(21), text("blue")],
            vec![int(3), int(22), text("green")],
        ],
        &[],
    );
    // The version history of an append-only memo.
    let vh_type = tdb.add_table(
        "MSysComplexTypeVH_Notes",
        i64::from(0x8000_0000u32 as i32),
        &[ColSpec::new("Notes", t::MEMO), ColSpec::new("Modified", t::DATE)],
        &[],
        &[],
    );
    let vh_flat = tdb.add_table(
        "f_vh_Notes",
        i64::from(0x8000_0000u32 as i32),
        &[
            ColSpec::new("_Id", t::LONG).flags(0x04),
            ColSpec::new("Notes_FK", t::LONG).ext(0x08),
            ColSpec::new("Notes", t::MEMO),
            ColSpec::new("Modified", t::DATE),
        ],
        &[
            vec![int(1), int(31), Cv::Memo("first".into()), Cv::Null],
            vec![int(2), int(31), Cv::Memo("second".into()), Cv::Null],
        ],
        &[],
    );
    let main = tdb.add_table(
        "Docs",
        0,
        &[
            ColSpec::new("id", t::LONG),
            ColSpec::new("Files", t::COMPLEX).complex(101),
            ColSpec::new("Tags", t::COMPLEX).complex(102),
            ColSpec::new("Notes", t::MEMO),
            ColSpec::new("NotesHistory", t::COMPLEX).complex(103).flags(0x10),
        ],
        &[
            vec![int(1), int(7), int(21), Cv::Memo("current".into()), int(31)],
            vec![int(2), int(8), int(22), Cv::Null, Cv::Null],
            vec![int(3), int(9), Cv::Null, Cv::Null, Cv::Null],
        ],
        &[],
    );
    let reg = |id: i64, ty: usize, flat: usize, name: &str| {
        vec![int(id), int(ty as i64), int(main as i64), int(flat as i64), text(name)]
    };
    tdb.add_table(
        "MSysComplexColumns",
        i64::from(0x8000_0000u32 as i32),
        &[
            ColSpec::new("ComplexID", t::LONG),
            ColSpec::new("ComplexTypeObjectID", t::LONG),
            ColSpec::new("ConceptualTableID", t::LONG),
            ColSpec::new("FlatTableID", t::LONG),
            ColSpec::new("ColumnName", t::TEXT),
        ],
        &[
            reg(101, att_type, att_flat, "Files"),
            reg(102, mv_type, mv_flat, "Tags"),
            reg(103, vh_type, vh_flat, "NotesHistory"),
        ],
        &[],
    );
    (tdb.finish(), main)
}

#[test]
fn attachments_multi_values_and_histories_are_read_from_their_hidden_tables() {
    let (data, main) = complex_database();
    let db = Db::open(&data).unwrap();
    let objects = db.catalog().unwrap();
    let table = db.table(u32::try_from(main).unwrap()).unwrap();
    let by = |n: &str| table.columns.iter().find(|c| c.name == n).unwrap();
    let page = u32::try_from(main).unwrap();

    let files = db.complex_data(&objects, page, by("Files")).unwrap();
    assert_eq!(files.kind, ComplexKind::Attachment);
    assert!(!files.stopped);
    assert_eq!(
        files.items[&7],
        [
            ComplexItem::Attachment { name: "a.txt".into(), size: 300 },
            ComplexItem::Attachment { name: "b.png".into(), size: 10 },
        ]
    );
    assert_eq!(files.items[&8], [ComplexItem::Attachment { name: "other.zip".into(), size: 5 }]);

    let tags = db.complex_data(&objects, page, by("Tags")).unwrap();
    assert_eq!(tags.kind, ComplexKind::MultiValue);
    assert_eq!(
        tags.items[&21],
        [ComplexItem::Value(Value::Text("red".into())), ComplexItem::Value(Value::Text("blue".into()))]
    );

    let hist = db.complex_data(&objects, page, by("NotesHistory")).unwrap();
    assert_eq!(hist.kind, ComplexKind::VersionHistory);
    assert_eq!(hist.items[&31].len(), 2);

    // The row values: the key of each complex column, and the current text of the memo.
    let rows = scan(&db, main);
    assert_eq!(rows[0][1], Value::Complex(7));
    assert_eq!(rows[0][2], Value::Complex(21));
    assert_eq!(rows[0][3], Value::Text("current".into()));
    assert_eq!(rows[0][4], Value::Complex(31));
    assert!(by("NotesHistory").is_hidden());
    assert_eq!(by("Files").complex_id(), 101);
}

#[test]
fn a_complex_column_that_the_registry_does_not_describe_has_no_data() {
    let (mut data, main) = complex_database();
    let db = Db::open(&data).unwrap();
    let objects = db.catalog().unwrap();
    let table = db.table(u32::try_from(main).unwrap()).unwrap();
    let files = table.columns.iter().find(|c| c.name == "Files").unwrap().clone();
    // The wrong table: the row describes `Docs`, not this page.
    assert!(db.complex_data(&objects, 3, &files).is_none());
    // An identifier nobody registered.
    let mut other = files.clone();
    other.complex_id = 999;
    assert!(db.complex_data(&objects, u32::try_from(main).unwrap(), &other).is_none());
    drop(db);
    // Damage to the pages of the registry or of the flat tables is a refusal, never a panic.
    for page in 3..data.len() / 4096 {
        let saved = data[page * 4096..(page + 1) * 4096].to_vec();
        data[page * 4096..(page + 1) * 4096].fill(0xA5);
        if let Ok(db) = Db::open(&data) {
            if let Some(objects) = db.catalog() {
                let _ = db.complex_data(&objects, u32::try_from(main).unwrap(), &files);
            }
        }
        data[page * 4096..(page + 1) * 4096].copy_from_slice(&saved);
    }
}

#[test]
fn property_lists_of_the_catalog_are_read() {
    let mut names = Vec::new();
    for n in ["StartUpForm", "AllowBypassKey"] {
        let b = utf16(n);
        names.extend(u16::try_from(b.len()).unwrap().to_le_bytes());
        names.extend(b);
    }
    let entry = |dtype: u8, idx: u16, data: &[u8]| {
        let mut v = u16::try_from(8 + data.len()).unwrap().to_le_bytes().to_vec();
        v.extend([0, dtype]);
        v.extend(idx.to_le_bytes());
        v.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
        v.extend(data);
        v
    };
    let mut values = 4u32.to_le_bytes().to_vec();
    values.extend(entry(0x0A, 0, &utf16("frmMain")));
    values.extend(entry(0x01, 1, &[0]));
    let block = |kind: u16, body: &[u8]| {
        let mut v = u32::try_from(body.len() + 6).unwrap().to_le_bytes().to_vec();
        v.extend(kind.to_le_bytes());
        v.extend(body);
        v
    };
    let mut list = b"MR2\0".to_vec();
    list.extend(block(0x80, &names));
    list.extend(block(0, &values));
    let mut tdb = TestDb::new(1);
    tdb.add_object_with_props(0x0100_0001, 3, "MSysDb", list);
    tdb.add_object(0x0100_0002, 3, "Other", 0);
    let data = tdb.finish();
    let db = Db::open(&data).unwrap();
    let props = db.properties_of("msysdb").unwrap();
    assert_eq!(props.len(), 2);
    assert_eq!((props[0].name.as_str(), &props[0].value), ("StartUpForm", &PropValue::Text("frmMain".into())));
    assert_eq!(props[1].value, PropValue::Bool(false));
    assert!(db.properties_of("Other").is_none());
    assert!(db.properties_of("Nothing").is_none());
    // The ordinary row reader still sees an object column as a size.
    let table = db.table(CATALOG_PAGE).unwrap();
    let mut sizes = Vec::new();
    db.rows(&table, |v| {
        if let Some(Value::Ole(n)) = v.get(5) {
            sizes.push(*n);
        }
        true
    });
    assert_eq!(sizes.len(), 1);
}
