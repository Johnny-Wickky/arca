//! Access (Jet/ACE) triage over the real fixture databases and damaged copies of them.
//!
//! The rule under test: a database whose catalog was not read completely never comes out
//! `Clean`, whatever the reason (damage, truncation, encryption, a limit).

use crate::report::{ThreatKind, Verdict, XRayReport};
use crate::{FileFormat, scan_default};

const MDB: &[u8] = include_bytes!("../../../../tests/fixtures/access-2000.mdb");
const ACCDB: &[u8] = include_bytes!("../../../../tests/fixtures/access-2016.accdb");

/// Page size of Jet 4 and ACE databases.
const PAGE: usize = 4096;

fn assert_unverified(r: &XRayReport, why: ThreatKind) {
    assert!(r.has(why), "expected {why:?}; got {:#?}", r.findings);
    // A stronger verdict (active content found elsewhere in the file) may outrank
    // `Unverified`, but a coverage gap must never leave the file `Clean`.
    assert!(
        matches!(
            r.verdict,
            Verdict::Unverified | Verdict::ActiveContentNeutralized | Verdict::Rejected
        ),
        "{:#?}",
        r.findings
    );
}

#[test]
fn real_databases_are_read_completely() {
    for (name, data, format) in
        [("mdb", MDB, FileFormat::AccessJet4), ("accdb", ACCDB, FileFormat::AccessAce)]
    {
        let r = scan_default(data);
        assert_eq!(r.format, format, "{name}");
        for bad in [
            ThreatKind::MalformedContainer,
            ThreatKind::ScanLimitReached,
            ThreatKind::EncryptedDocument,
        ] {
            assert!(!r.has(bad), "{name}: unexpected {bad:?}: {:#?}", r.findings);
        }
        assert_ne!(r.verdict, Verdict::Unverified, "{name}");
    }
}

#[test]
fn a_planted_autoexec_name_is_found_by_the_byte_search() {
    let mut data = MDB.to_vec();
    data.extend_from_slice(b"AutoExec");
    let r = scan_default(&data);
    assert!(r.has(ThreatKind::MacroAutoExec), "{:#?}", r.findings);
    let mut wide = MDB.to_vec();
    wide.extend("AutoExec".encode_utf16().flat_map(u16::to_le_bytes));
    assert!(scan_default(&wide).has(ThreatKind::MacroAutoExec));
}

#[test]
fn a_damaged_catalog_is_unverified_not_clean() {
    for data in [MDB, ACCDB] {
        let mut d = data.to_vec();
        // Page 2 holds the definition of the system catalog.
        d[2 * PAGE..3 * PAGE].fill(0);
        assert_unverified(&scan_default(&d), ThreatKind::MalformedContainer);
    }
}

#[test]
fn truncated_databases_are_unverified_not_clean() {
    for data in [MDB, ACCDB] {
        for keep in [PAGE - 1, 2 * PAGE, 3 * PAGE, data.len() / 2] {
            let r = scan_default(&data[..keep]);
            assert_ne!(r.verdict, Verdict::Clean, "kept {keep} bytes: {:#?}", r.findings);
        }
        assert_unverified(&scan_default(&data[..2 * PAGE]), ThreatKind::MalformedContainer);
    }
}

#[test]
fn an_unknown_version_is_unverified_not_clean() {
    let mut d = MDB.to_vec();
    d[0x14] = 0x7F;
    let r = scan_default(&d);
    assert_ne!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn an_encrypted_database_is_reported_as_such() {
    for data in [MDB, ACCDB] {
        let mut d = data.to_vec();
        // Byte 62 is part of the masked header; changing it makes the decoded key non-zero.
        d[62] ^= 0x01;
        assert_unverified(&scan_default(&d), ThreatKind::EncryptedDocument);
    }
}

#[test]
fn a_header_only_database_is_unverified() {
    let mut d = vec![0u8; 3 * PAGE];
    d[4..19].copy_from_slice(b"Standard Jet DB");
    d[0x14] = 1;
    let r = scan_default(&d);
    assert_ne!(r.verdict, Verdict::Clean, "{:#?}", r.findings);
}

#[test]
fn scanning_garbage_inside_a_database_never_panics() {
    let mut seed = 0x9E37_79B9_u32;
    for data in [MDB, ACCDB] {
        for _ in 0..8 {
            let mut d = data.to_vec();
            for _ in 0..64 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = usize::try_from(seed >> 4).unwrap_or(0) % d.len();
                if let Some(b) = d.get_mut(at) {
                    *b = (seed >> 24).to_le_bytes()[0];
                }
            }
            // Whatever the verdict, the scan must return and must not panic.
            assert!(!scan_default(&d).findings.is_empty());
        }
    }
}
