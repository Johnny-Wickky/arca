#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::string_slice,
    reason = "test code with small constants and ASCII text"
)]

use super::*;

fn scan(v: Verdict, t: u32, a: u32) -> Event {
    Event::Scan { verdict: v, trackers: t, active: a }
}

fn c(
    opened: u64,
    clean: u64,
    trackers: u64,
    active: u64,
    rejected: u64,
    unverified: u64,
) -> Counts {
    Counts { opened, clean, trackers, active, rejected, unverified, sterile: 0, pictures: 0 }
}

/// Wraps `body` (everything between the version byte and the CRC) into a valid blob.
fn seal(version: u8, body: &[u8]) -> Vec<u8> {
    let mut v = MAGIC.to_vec();
    v.push(version);
    v.extend_from_slice(body);
    let crc = arca_digest::crc32(&v);
    v.extend_from_slice(&crc.to_le_bytes());
    v
}

fn varints(values: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        put_varint(&mut out, *v);
    }
    out
}

// ---------------------------------------------------------------- the verdict mapping

#[test]
fn every_verdict_maps_to_its_counters() {
    assert_eq!(counts_for_scan(Verdict::Clean, 0, 0), c(1, 1, 0, 0, 0, 0));
    assert_eq!(counts_for_scan(Verdict::HiddenTrackers, 3, 0), c(1, 0, 3, 0, 0, 0));
    assert_eq!(counts_for_scan(Verdict::ActiveContentNeutralized, 2, 4), c(1, 0, 2, 4, 0, 0));
    assert_eq!(counts_for_scan(Verdict::Rejected, 0, 0), c(0, 0, 0, 0, 1, 0));
    assert_eq!(counts_for_scan(Verdict::Unverified, 0, 0), c(1, 0, 0, 0, 0, 1));
}

#[test]
fn the_verdict_implies_at_least_one_neutralised_item() {
    assert_eq!(counts_for_scan(Verdict::HiddenTrackers, 0, 0).trackers, 1);
    assert_eq!(counts_for_scan(Verdict::ActiveContentNeutralized, 0, 0).active, 1);
    // A tracker-only verdict that also carries active findings keeps them.
    assert_eq!(counts_for_scan(Verdict::HiddenTrackers, 0, 5).active, 5);
}

#[test]
fn clean_and_rejected_ignore_finding_counts_and_unverified_keeps_them() {
    assert_eq!(counts_for_scan(Verdict::Clean, 9, 9), c(1, 1, 0, 0, 0, 0));
    assert_eq!(counts_for_scan(Verdict::Rejected, 9, 9), c(0, 0, 0, 0, 1, 0));
    assert_eq!(counts_for_scan(Verdict::Unverified, 2, 1), c(1, 0, 2, 1, 0, 1));
}

#[test]
fn verdict_codes_are_the_wire_tags_and_unknown_is_never_clean() {
    for v in [
        Verdict::Clean,
        Verdict::HiddenTrackers,
        Verdict::ActiveContentNeutralized,
        Verdict::Rejected,
        Verdict::Unverified,
    ] {
        assert_eq!(verdict_from_code(verdict_code(v)), v);
    }
    assert_eq!(verdict_code(Verdict::Clean), 0);
    assert_eq!(verdict_code(Verdict::Unverified), 4);
    for bad in [-1, 5, 99, i32::MAX, i32::MIN] {
        assert_eq!(verdict_from_code(bad), Verdict::Unverified);
    }
}

#[test]
fn sterile_copies_and_pictures_are_their_own_counters() {
    let mut s = Stats::new();
    s.record(Event::SterileCopy);
    s.record(Event::PictureOpened);
    s.record(Event::PictureOpened);
    let t = s.totals();
    assert_eq!((t.sterile, t.pictures, t.opened), (1, 2, 0));
}

#[test]
fn counters_saturate_instead_of_wrapping() {
    let mut s = Stats::new();
    s.all = Counts { opened: u64::MAX - 1, trackers: u64::MAX, ..Counts::default() };
    s.record(scan(Verdict::HiddenTrackers, u32::MAX, 0));
    s.record(scan(Verdict::Clean, 0, 0));
    s.record(scan(Verdict::Clean, 0, 0));
    let t = s.totals();
    assert_eq!(t.opened, u64::MAX);
    assert_eq!(t.trackers, u64::MAX);
    assert_eq!(t.threats(), u64::MAX);
    // Round trip with 64-bit values.
    assert_eq!(Stats::decode(&s.encode()).totals().opened, u64::MAX);
    // JSON never exceeds what a signed 64-bit reader can parse.
    let json = String::from_utf8(s.summary_json()).unwrap();
    assert!(json.contains("\"opened\":9223372036854775807"), "{json}");
}

#[test]
fn reset_forgets_everything() {
    let mut s = Stats::new();
    s.record(scan(Verdict::Clean, 0, 0));
    s.reset();
    assert!(s.is_empty());
    assert_eq!(s, Stats::new());
}

#[test]
fn the_blob_holds_totals_and_no_dates() {
    // Version byte, eight varints, CRC: nothing else fits in the format, so no day, no time and
    // no per-day count can be stored. The same events always give the same bytes, whenever
    // they happened.
    let mut s = Stats::new();
    s.record(scan(Verdict::HiddenTrackers, 2, 0));
    s.record(scan(Verdict::Clean, 0, 0));
    s.record(Event::SterileCopy);
    let blob = s.encode();
    assert_eq!(&blob[..4], b"AST\x02");
    assert_eq!(blob.len(), 4 + 8 + 4, "eight one-byte totals between the header and the CRC");
    assert_eq!(Stats::decode(&blob), s);
    let mut again = Stats::new();
    again.record(Event::SterileCopy);
    again.record(scan(Verdict::Clean, 0, 0));
    again.record(scan(Verdict::HiddenTrackers, 2, 0));
    assert_eq!(again.encode(), blob, "the order of events leaves no trace");
}

#[test]
fn a_version_one_blob_with_a_daily_history_is_never_read() {
    // "AST", version 1, first day, eight totals, one bucket (a day and eight counters), CRC.
    let mut body = varints(&[20_001]);
    body.extend(varints(&[5, 4, 1, 0, 0, 0, 0, 0]));
    body.extend(varints(&[1, 20_000, 5, 4, 1, 0, 0, 0, 0, 0]));
    let blob = seal(1, &body);
    assert_eq!(Stats::try_decode(&blob), Err(DecodeError::UnknownVersion));
    assert!(Stats::decode(&blob).is_empty());
}

#[test]
fn every_truncation_and_every_bit_flip_is_rejected_as_empty() {
    let mut s = Stats::new();
    s.record(scan(Verdict::ActiveContentNeutralized, 1, 3));
    let blob = s.encode();
    for n in 0..blob.len() {
        assert!(Stats::decode(&blob[..n]).is_empty(), "prefix {n}");
    }
    for i in 0..blob.len() {
        for bit in 0..8 {
            let mut bad = blob.clone();
            bad[i] ^= 1 << bit;
            assert!(Stats::decode(&bad).is_empty(), "byte {i} bit {bit}");
        }
    }
    let mut longer = blob.clone();
    longer.push(0);
    assert!(Stats::decode(&longer).is_empty());
}

#[test]
fn foreign_data_and_oversized_input_give_empty_totals() {
    assert_eq!(Stats::try_decode(&[]), Err(DecodeError::BadMagic));
    assert_eq!(Stats::try_decode(b"XYZ\x02\0\0\0\0"), Err(DecodeError::BadMagic));
    assert_eq!(Stats::try_decode(&seal(3, &varints(&[0; 8]))), Err(DecodeError::UnknownVersion));
    assert_eq!(Stats::try_decode(&vec![0u8; MAX_BLOB_LEN + 1]), Err(DecodeError::TooLarge));
}

#[test]
fn well_checksummed_but_malformed_bodies_are_rejected() {
    // Too few totals, too many, trailing bytes.
    assert_eq!(Stats::try_decode(&seal(2, &varints(&[1; 7]))), Err(DecodeError::Malformed));
    assert_eq!(Stats::try_decode(&seal(2, &varints(&[1; 9]))), Err(DecodeError::Malformed));
    let mut body = varints(&[1; 8]);
    body.push(0);
    assert_eq!(Stats::try_decode(&seal(2, &body)), Err(DecodeError::Malformed));
    // A varint that does not fit 64 bits, or never ends.
    let mut huge = vec![0xff; 9];
    huge.push(0x02);
    assert_eq!(Stats::try_decode(&seal(2, &huge)), Err(DecodeError::Malformed));
    assert_eq!(Stats::try_decode(&seal(2, &[0x80; 12])), Err(DecodeError::Malformed));
    // The all-ones body decodes: eight totals of one.
    assert_eq!(Stats::try_decode(&seal(2, &varints(&[1; 8]))).map(|s| s.totals().opened), Ok(1));
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn hostile_bytes_never_panic() {
    for seed in 1..200u64 {
        let len = (seed as usize * 7) % 300;
        let mut data = noise(seed, len);
        let _ = Stats::decode(&data);
        // Plausible header, random rest.
        let mut framed = MAGIC.to_vec();
        framed.push(VERSION);
        framed.append(&mut data);
        let _ = Stats::decode(&framed);
        let _ = blob_summary_json(&framed);
        let _ = blob_record_scan(&framed, 99, i32::MIN, i32::MAX);
    }
}

#[test]
fn the_summary_json_holds_the_all_time_totals_only() {
    let mut s = Stats::new();
    s.record(scan(Verdict::HiddenTrackers, 2, 0));
    s.record(scan(Verdict::Clean, 0, 0));
    let json = String::from_utf8(s.summary_json()).unwrap();
    assert!(json.contains("\"version\":2"), "{json}");
    assert!(json.contains("\"empty\":false"), "{json}");
    assert!(json.contains("\"all\":{\"opened\":2,\"clean\":1,\"trackers\":2"), "{json}");
    for forbidden in ["week", "month", "firstDay", "days"] {
        assert!(!json.contains(forbidden), "{forbidden} in {json}");
    }
    let empty = String::from_utf8(Stats::new().summary_json()).unwrap();
    assert!(empty.contains("\"empty\":true"), "{empty}");
}

#[test]
fn the_byte_interface_chains_and_clamps() {
    let blob = blob_record_scan(&[], 0, 0, 0);
    let blob = blob_record_scan(&blob, 1, 3, 0);
    let blob = blob_record_event(&blob, EVENT_STERILE_COPY);
    let blob = blob_record_event(&blob, EVENT_PICTURE_OPENED);
    // An unknown event changes nothing.
    assert_eq!(blob_record_event(&blob, 77), blob);
    let t = Stats::decode(&blob).totals();
    assert_eq!((t.opened, t.clean, t.trackers, t.sterile, t.pictures), (2, 1, 3, 1, 1));
    // Negative counts are zero, an unknown verdict is "unverified", never "clean".
    let t = Stats::decode(&blob_record_scan(&[], -9, -5, -5)).totals();
    assert_eq!((t.opened, t.clean, t.unverified, t.trackers), (1, 0, 1, 0));
    // A damaged blob starts again from empty.
    assert_eq!(Stats::decode(&blob_record_scan(b"junk", 0, 0, 0)).totals().opened, 1);
}

#[test]
fn the_patron_passport_is_built_from_the_totals_and_the_install_day() {
    let mut s = Stats::new();
    s.record(scan(Verdict::HiddenTrackers, 2, 0));
    s.record(scan(Verdict::Clean, 0, 0));
    let json = String::from_utf8(blob_passport_json(1, &s.encode(), 20_000, 20_002)).unwrap();
    assert!(json.contains("\"documents\":2"), "{json}");
    assert!(json.contains("\"trackers\":2"), "{json}");
    assert!(json.contains("\"days\":3"), "{json}");
    // No counter at all: a fresh passport, never an error.
    let json = String::from_utf8(blob_passport_json(0, &[], 20_000, 20_000)).unwrap();
    assert!(json.contains("\"documents\":0"), "{json}");
}

#[test]
fn nothing_about_a_document_can_be_recorded() {
    // The API has no place for a name, a hash or a time: an event is a verdict and two counts.
    // Two different documents with the same outcome leave identical statistics.
    let mut a = Stats::new();
    let mut b = Stats::new();
    a.record(scan(Verdict::HiddenTrackers, 1, 0));
    b.record(scan(Verdict::HiddenTrackers, 1, 0));
    assert_eq!(a.encode(), b.encode());
}
