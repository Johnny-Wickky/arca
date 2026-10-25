use super::*;

const D0: u32 = 20_000;

fn scan(v: Verdict, t: u32, a: u32) -> Event {
    Event::Scan { verdict: v, trackers: t, active: a }
}

fn c(opened: u64, clean: u64, trackers: u64, active: u64, rejected: u64, unverified: u64) -> Counts {
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
    for v in
        [Verdict::Clean, Verdict::HiddenTrackers, Verdict::ActiveContentNeutralized, Verdict::Rejected, Verdict::Unverified]
    {
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
    s.record(D0, Event::SterileCopy);
    s.record(D0, Event::PictureOpened);
    s.record(D0, Event::PictureOpened);
    let t = s.totals(Window::All, D0);
    assert_eq!((t.sterile, t.pictures, t.opened), (1, 2, 0));
}

// ---------------------------------------------------------------- windows

#[test]
fn week_and_month_windows_have_exact_edges() {
    let mut s = Stats::new();
    let today = D0 + 100;
    for back in [100u32, 31, 30, 29, 7, 6, 0] {
        s.record(today - back, scan(Verdict::Clean, 0, 0));
    }
    assert_eq!(s.totals(Window::Week, today).opened, 2); // today and 6 days ago
    assert_eq!(s.totals(Window::Month, today).opened, 4); // + 7 and 29 days ago
    assert_eq!(s.totals(Window::All, today).opened, 7);
}

#[test]
fn windows_move_with_today_and_empty_stats_are_zero() {
    let mut s = Stats::new();
    assert!(s.is_empty());
    assert!(s.totals(Window::Week, D0).is_zero());
    s.record(D0, scan(Verdict::Clean, 0, 0));
    assert_eq!(s.totals(Window::Week, D0 + 6).opened, 1);
    assert_eq!(s.totals(Window::Week, D0 + 7).opened, 0);
    assert_eq!(s.totals(Window::All, D0 + 7).opened, 1);
}

#[test]
fn a_clock_set_back_neither_loses_nor_reorders_events() {
    let mut s = Stats::new();
    s.record(D0 + 50, scan(Verdict::Clean, 0, 0));
    s.record(D0 + 10, scan(Verdict::HiddenTrackers, 2, 0)); // the clock went back 40 days
    assert_eq!(s.bucket_count(), 1);
    assert_eq!(s.totals(Window::All, D0 + 10).opened, 2);
    // Seen from the (wrong) earlier "today" the future bucket still counts everywhere.
    assert_eq!(s.totals(Window::Week, D0 + 10).opened, 2);
    // The first day follows the earliest recorded day.
    assert_eq!(s.first_day(), Some(D0 + 10));
    let again = Stats::decode(&s.encode());
    assert_eq!(again, s);
}

#[test]
fn insane_days_are_clamped() {
    let mut s = Stats::new();
    s.record(0, scan(Verdict::Clean, 0, 0));
    s.record(u32::MAX, scan(Verdict::Clean, 0, 0));
    assert_eq!(s.first_day(), Some(MIN_DAY));
    assert_eq!(s.totals(Window::All, MAX_DAY).opened, 2);
    // The year-2019 bucket is more than 400 days older than the year-9999 one and expires.
    assert_eq!(s.bucket_count(), 1);
    assert!(s.days_in_use(0) >= 1);
}

// ---------------------------------------------------------------- the ring

#[test]
fn the_ring_keeps_400_days_and_the_all_time_total_keeps_counting() {
    let mut s = Stats::new();
    for i in 0..1000u32 {
        s.record(D0 + i, scan(Verdict::Clean, 0, 0));
    }
    assert_eq!(s.bucket_count(), 400);
    let today = D0 + 999;
    assert_eq!(s.totals(Window::All, today).opened, 1000);
    assert_eq!(s.totals(Window::Week, today).opened, 7);
    assert_eq!(s.totals(Window::Month, today).opened, 30);
    // The oldest kept day is today - 399.
    assert_eq!(s.first_day(), Some(D0));
    assert_eq!(Stats::decode(&s.encode()), s);
}

#[test]
fn old_sparse_buckets_expire_by_age_not_only_by_count() {
    let mut s = Stats::new();
    s.record(D0, scan(Verdict::Clean, 0, 0));
    s.record(D0 + 399, scan(Verdict::Clean, 0, 0));
    assert_eq!(s.bucket_count(), 2);
    s.record(D0 + 400, scan(Verdict::Clean, 0, 0)); // D0 is now 400 days old
    assert_eq!(s.bucket_count(), 2);
    s.record(D0 + 5000, scan(Verdict::Clean, 0, 0));
    assert_eq!(s.bucket_count(), 1);
    assert_eq!(s.totals(Window::All, D0 + 5000).opened, 4);
}

#[test]
fn counters_saturate_instead_of_wrapping() {
    let mut s = Stats::new();
    s.fold_legacy(&Counts { opened: u64::MAX - 1, trackers: u64::MAX, ..Counts::default() }, None);
    s.record(D0, scan(Verdict::HiddenTrackers, u32::MAX, 0));
    s.record(D0, scan(Verdict::Clean, 0, 0));
    s.record(D0, scan(Verdict::Clean, 0, 0));
    let t = s.totals(Window::All, D0);
    assert_eq!(t.opened, u64::MAX);
    assert_eq!(t.trackers, u64::MAX);
    assert_eq!(t.threats(), u64::MAX);
    // Round trip with 64-bit values.
    assert_eq!(Stats::decode(&s.encode()).totals(Window::All, D0).opened, u64::MAX);
    // JSON never exceeds what a signed 64-bit reader can parse.
    let json = String::from_utf8(s.summary_json(D0)).unwrap();
    assert!(json.contains("\"opened\":9223372036854775807"), "{json}");
}

#[test]
fn reset_forgets_everything() {
    let mut s = Stats::new();
    s.record(D0, scan(Verdict::Clean, 0, 0));
    s.reset();
    assert!(s.is_empty());
    assert_eq!(s.first_day(), None);
    assert_eq!(s, Stats::new());
}

// ---------------------------------------------------------------- encoding

#[test]
fn encode_decode_round_trip() {
    let mut s = Stats::new();
    s.record(D0, scan(Verdict::Clean, 0, 0));
    s.record(D0, Event::SterileCopy);
    s.record(D0 + 3, scan(Verdict::ActiveContentNeutralized, 1, 7));
    s.record(D0 + 9, scan(Verdict::Rejected, 0, 0));
    s.record(D0 + 9, scan(Verdict::Unverified, 0, 0));
    s.record(D0 + 9, Event::PictureOpened);
    let blob = s.encode();
    assert_eq!(&blob[..4], b"AST\x01");
    assert_eq!(Stats::try_decode(&blob), Ok(s.clone()));
    assert_eq!(Stats::decode(&blob), s);
    // Deterministic.
    assert_eq!(blob, s.encode());
    // Compact: a few days are tens of bytes.
    assert!(blob.len() < 120, "{}", blob.len());
    // Empty statistics are tiny and survive too.
    assert_eq!(Stats::decode(&Stats::new().encode()), Stats::new());
    assert!(Stats::new().encode().len() < 24);
}

#[test]
fn the_worst_case_blob_stays_under_the_limit() {
    let mut s = Stats::new();
    for i in 0..400u32 {
        let big = Counts {
            opened: u64::MAX,
            clean: u64::MAX,
            trackers: u64::MAX,
            active: u64::MAX,
            rejected: u64::MAX,
            unverified: u64::MAX,
            sterile: u64::MAX,
            pictures: u64::MAX,
        };
        s.add_counts(MAX_DAY - 399 + i, &big);
    }
    let blob = s.encode();
    assert!(blob.len() <= MAX_BLOB_LEN, "{}", blob.len());
    // Small enough for the 64 KiB argument limit of the JNI layer.
    assert!(blob.len() < 60_000);
    assert_eq!(Stats::try_decode(&blob), Ok(s));
}

#[test]
fn every_truncation_and_every_bit_flip_is_rejected_as_empty() {
    let mut s = Stats::new();
    for i in 0..5u32 {
        s.record(D0 + i * 3, scan(Verdict::HiddenTrackers, i + 1, 0));
    }
    let blob = s.encode();
    for len in 0..blob.len() {
        assert!(Stats::try_decode(&blob[..len]).is_err(), "prefix {len}");
        assert!(Stats::decode(&blob[..len]).is_empty());
    }
    for byte in 0..blob.len() {
        for bit in 0..8 {
            let mut broken = blob.clone();
            broken[byte] ^= 1 << bit;
            assert!(Stats::try_decode(&broken).is_err(), "byte {byte} bit {bit}");
        }
    }
    let mut longer = blob.clone();
    longer.push(0);
    assert!(Stats::try_decode(&longer).is_err());
}

#[test]
fn unknown_versions_and_foreign_data_give_empty_statistics() {
    let body = varints(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let v2 = seal(2, &body);
    assert_eq!(Stats::try_decode(&v2), Err(DecodeError::UnknownVersion));
    assert!(Stats::decode(&v2).is_empty());
    let v0 = seal(0, &body);
    assert_eq!(Stats::try_decode(&v0), Err(DecodeError::UnknownVersion));
    assert_eq!(Stats::try_decode(b"APS\x01"), Err(DecodeError::BadMagic));
    assert_eq!(Stats::try_decode(&[]), Err(DecodeError::BadMagic));
    assert_eq!(Stats::try_decode(b"AST"), Err(DecodeError::Malformed));
    // A 36-byte old passport record is not a statistics blob.
    let old = PatronStats::starting(D0).encode();
    assert!(Stats::try_decode(&old).is_err());
}

#[test]
fn oversized_input_is_refused_before_any_work() {
    let big = vec![0x41u8; MAX_BLOB_LEN + 1];
    assert_eq!(Stats::try_decode(&big), Err(DecodeError::TooLarge));
    let huge = vec![0u8; 5 * 1024 * 1024];
    assert_eq!(Stats::try_decode(&huge), Err(DecodeError::TooLarge));
}

#[test]
fn well_checksummed_but_malformed_bodies_are_rejected() {
    let zero8 = [0u64; 8];
    let ok = |first: u64, n: u64, days: &[u64]| {
        let mut parts = vec![first];
        parts.extend_from_slice(&zero8);
        parts.push(n);
        parts.extend_from_slice(days);
        seal(1, &varints(&parts))
    };
    // Sanity: a hand-made valid blob with one bucket decodes.
    let mut one = vec![D0 as u64];
    one.extend_from_slice(&[1; 8]);
    assert!(Stats::try_decode(&ok(0, 1, &one)).is_ok());
    // More buckets than the ring holds.
    assert_eq!(Stats::try_decode(&ok(0, 401, &[])), Err(DecodeError::Malformed));
    assert_eq!(Stats::try_decode(&ok(0, u64::MAX, &[])), Err(DecodeError::Malformed));
    // Fewer buckets than announced.
    assert_eq!(Stats::try_decode(&ok(0, 2, &one)), Err(DecodeError::Malformed));
    // A day that does not move forward.
    let mut two = one.clone();
    two.push(0);
    two.extend_from_slice(&[1; 8]);
    assert_eq!(Stats::try_decode(&ok(0, 2, &two)), Err(DecodeError::Malformed));
    // A first day that does not fit 32 bits; a day delta that does not fit; a day overflow.
    assert_eq!(Stats::try_decode(&ok(u64::MAX, 0, &[])), Err(DecodeError::Malformed));
    let mut wide = vec![u64::from(u32::MAX) + 1];
    wide.extend_from_slice(&[0; 8]);
    assert_eq!(Stats::try_decode(&ok(0, 1, &wide)), Err(DecodeError::Malformed));
    let mut over = vec![u64::from(u32::MAX)];
    over.extend_from_slice(&[0; 8]);
    over.push(5);
    over.extend_from_slice(&[0; 8]);
    assert_eq!(Stats::try_decode(&ok(0, 2, &over)), Err(DecodeError::Malformed));
    // Bytes left over after the last bucket.
    let mut extra = varints(&{
        let mut p = vec![0];
        p.extend_from_slice(&zero8);
        p.push(0);
        p
    });
    extra.push(0);
    assert_eq!(Stats::try_decode(&seal(1, &extra)), Err(DecodeError::Malformed));
}

#[test]
fn bad_varints_are_rejected() {
    // Eleven continuation bytes; a tenth byte above 1 (would overflow 64 bits); a cut varint.
    for body in [
        vec![0x80u8; 11],
        {
            let mut v = vec![0xFFu8; 9];
            v.push(0x02);
            v
        },
        vec![0x80],
        vec![],
    ] {
        assert_eq!(Stats::try_decode(&seal(1, &body)), Err(DecodeError::Malformed), "{body:?}");
    }
    // The largest value is accepted.
    let mut r = Reader { data: &varints(&[u64::MAX]), pos: 0 };
    assert_eq!(r.varint(), Some(u64::MAX));
}

#[test]
fn a_decoded_blob_never_claims_less_all_time_than_its_buckets_hold() {
    // All-time says 0 but a bucket holds 5: the total is raised, windows never exceed it.
    let mut parts = vec![0u64];
    parts.extend_from_slice(&[0; 8]);
    parts.push(1);
    parts.push(D0 as u64);
    parts.extend_from_slice(&[5, 0, 0, 0, 0, 0, 0, 0]);
    let s = Stats::try_decode(&seal(1, &varints(&parts))).unwrap();
    assert_eq!(s.totals(Window::All, D0).opened, 5);
    assert!(s.totals(Window::Week, D0).opened <= s.totals(Window::All, D0).opened);
}

/// Deterministic pseudo-random bytes (xorshift), so no dependency is needed.
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
    for seed in 1..400u64 {
        let len = (seed as usize * 37) % 700;
        let mut bytes = noise(seed, len);
        let _ = Stats::decode(&bytes);
        // The same noise dressed as a blob of this module, with a valid checksum.
        if bytes.len() > 8 {
            bytes[0..4].copy_from_slice(b"AST\x01");
            let n = bytes.len() - 4;
            let crc = arca_digest::crc32(&bytes[..n]);
            bytes[n..].copy_from_slice(&crc.to_le_bytes());
            let _ = Stats::decode(&bytes);
            if let Ok(s) = Stats::try_decode(&bytes) {
                assert!(s.bucket_count() <= 400);
                let _ = s.summary_json(D0);
            }
        }
    }
}

// ---------------------------------------------------------------- JSON and the byte interface

#[test]
fn the_summary_json_has_all_three_windows() {
    let mut s = Stats::new();
    s.record(D0 - 10, scan(Verdict::Clean, 0, 0));
    s.record(D0, scan(Verdict::ActiveContentNeutralized, 1, 2));
    let json = String::from_utf8(s.summary_json(D0)).unwrap();
    assert!(json.starts_with("{\"version\":1,\"empty\":false,\"firstDay\":19990,\"days\":11,"), "{json}");
    assert!(json.contains("\"week\":{\"opened\":1,\"clean\":0,\"trackers\":1,\"active\":2,\"threats\":3,"), "{json}");
    assert!(json.contains("\"month\":{\"opened\":2,"), "{json}");
    assert!(json.contains("\"all\":{\"opened\":2,"), "{json}");
    let empty = String::from_utf8(Stats::new().summary_json(D0)).unwrap();
    assert!(empty.contains("\"empty\":true,\"firstDay\":0,\"days\":1,"), "{empty}");
}

#[test]
fn the_byte_interface_chains_and_clamps() {
    let mut blob = Vec::new();
    blob = blob_record_scan(&blob, D0 as i32, 0, 0, 0);
    blob = blob_record_scan(&blob, D0 as i32, 1, 4, 0);
    blob = blob_record_scan(&blob, D0 as i32 + 1, 2, -5, 3); // a negative count is zero
    blob = blob_record_scan(&blob, D0 as i32 + 1, 3, 0, 0);
    blob = blob_record_scan(&blob, D0 as i32 + 1, 77, 0, 0); // unknown = unverified
    blob = blob_record_event(&blob, D0 as i32 + 1, EVENT_STERILE_COPY);
    blob = blob_record_event(&blob, D0 as i32 + 1, EVENT_PICTURE_OPENED);
    blob = blob_record_event(&blob, D0 as i32 + 1, 99); // ignored
    let s = Stats::decode(&blob);
    let t = s.totals(Window::All, D0 + 1);
    assert_eq!((t.opened, t.clean, t.trackers, t.active), (4, 1, 4, 3));
    assert_eq!((t.rejected, t.unverified, t.sterile, t.pictures), (1, 1, 1, 1));
    // A damaged blob starts again instead of failing.
    let fresh = blob_record_scan(&[1, 2, 3], D0 as i32, 0, 0, 0);
    assert_eq!(Stats::decode(&fresh).totals(Window::All, D0).opened, 1);
    // Negative days are clamped, nothing panics.
    let _ = blob_record_scan(&fresh, -1, 0, i32::MIN, i32::MAX);
    assert!(String::from_utf8(blob_summary_json(&blob, -3)).unwrap().contains("\"all\""));
}

#[test]
fn legacy_counters_are_folded_into_all_time_only() {
    // From the old passport record.
    let mut old = PatronStats::starting(D0 - 30);
    old.record_document(3, 1);
    old.record_document(0, 0);
    let blob = blob_fold_legacy(&[], &old.encode(), 99, 99, D0 as i32);
    let s = Stats::decode(&blob);
    let all = s.totals(Window::All, D0);
    assert_eq!((all.opened, all.trackers, all.active), (2, 3, 1));
    assert!(s.totals(Window::Month, D0).is_zero());
    assert_eq!(s.first_day(), Some(D0 - 30));
    assert_eq!(s.bucket_count(), 0);
    // Without a usable passport record the two plain counters are used.
    let blob = blob_fold_legacy(&[], &[], 7, 4, D0 as i32);
    let all = Stats::decode(&blob).totals(Window::All, D0);
    assert_eq!((all.opened, all.active, all.trackers), (7, 4, 0));
    // Nothing old: nothing changes (and no first day appears).
    let blob = blob_fold_legacy(&[], &[], 0, 0, D0 as i32);
    assert!(Stats::decode(&blob).is_empty());
    assert_eq!(Stats::decode(&blob).first_day(), None);
    // Negative legacy counters are ignored.
    let blob = blob_fold_legacy(&[], &[], -5, -5, D0 as i32);
    assert!(Stats::decode(&blob).is_empty());
    // Folding adds to existing statistics.
    let mut now = Stats::new();
    now.record(D0, scan(Verdict::Clean, 0, 0));
    let blob = blob_fold_legacy(&now.encode(), &old.encode(), 0, 0, D0 as i32);
    assert_eq!(Stats::decode(&blob).totals(Window::All, D0).opened, 3);
    assert_eq!(Stats::decode(&blob).totals(Window::Week, D0).opened, 1);
}

#[test]
fn the_patron_passport_is_built_from_the_all_time_totals() {
    let mut s = Stats::new();
    s.record(D0, scan(Verdict::HiddenTrackers, 2, 0));
    s.record(D0 + 2, scan(Verdict::Clean, 0, 0));
    let json = String::from_utf8(blob_passport_json(1, &s.encode(), (D0 + 2) as i32)).unwrap();
    assert!(json.contains("\"documents\":2"), "{json}");
    assert!(json.contains("\"trackers\":2"), "{json}");
    assert!(json.contains("\"days\":3"), "{json}");
    // No statistics at all: a fresh passport, never an error.
    let json = String::from_utf8(blob_passport_json(0, &[], D0 as i32)).unwrap();
    assert!(json.contains("\"documents\":0"), "{json}");
}

#[test]
fn nothing_about_a_document_can_be_recorded() {
    // The API has no place for a name, a hash or a time of day: an event is a verdict and two
    // counts. Two different documents with the same outcome leave identical statistics.
    let mut a = Stats::new();
    let mut b = Stats::new();
    a.record(D0, scan(Verdict::HiddenTrackers, 1, 0));
    b.record(D0, scan(Verdict::HiddenTrackers, 1, 0));
    assert_eq!(a.encode(), b.encode());
}
