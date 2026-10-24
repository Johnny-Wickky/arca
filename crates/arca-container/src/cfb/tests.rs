//! Adversarial tests for the compound file reader.

use super::writer::CfbBuilder;
use super::*;

fn sample() -> Vec<u8> {
    let mut b = CfbBuilder::new();
    b.add_stream("WordDocument", vec![0xA5; 5000]).unwrap();
    b.add_stream("\u{5}SummaryInformation", vec![1, 2, 3]).unwrap();
    b.add_stream("Macros/VBA/dir", vec![7; 100]).unwrap();
    b.add_stream("Macros/VBA/ThisDocument", vec![8; 4096]).unwrap();
    b.add_stream("Macros/PROJECT", b"ID=\"{}\"".to_vec()).unwrap();
    b.add_storage("ObjectPool/_1234", [0x11; 16]).unwrap();
    b.finish().unwrap()
}

fn dir_entry_offset(file: &[u8], index: usize) -> usize {
    let first_dir = u32::from_le_bytes(file[48..52].try_into().unwrap()) as usize;
    (first_dir + 1) * 512 + index * 128
}

#[test]
fn roundtrip_through_builder() {
    let file = sample();
    let cf = CompoundFile::parse(&file, CfbLimits::default()).unwrap();
    assert!(cf.anomalies().is_empty(), "{:?}", cf.anomalies());
    let wd = cf.find("worddocument").unwrap();
    assert_eq!(cf.read_stream(wd).unwrap(), vec![0xA5; 5000]);
    let si = cf.find("\u{5}SummaryInformation").unwrap();
    assert_eq!(cf.read_stream(si).unwrap(), vec![1, 2, 3]);
    let dir = cf.find("Macros/VBA/dir").unwrap();
    assert_eq!(cf.read_stream(dir).unwrap(), vec![7; 100]);
    let td = cf.find("macros/vba/thisdocument").unwrap();
    assert_eq!(cf.read_stream(td).unwrap(), vec![8; 4096]);
    let pool = cf.find("ObjectPool/_1234").unwrap();
    assert_eq!(cf.entries()[pool].kind, EntryKind::Storage);
    assert_eq!(cf.entries()[pool].clsid, [0x11; 16]);
    let macros = cf.find("Macros").unwrap();
    assert!(cf.child(macros, "vba").is_some());
    assert_eq!(cf.streams().count(), 5);
    assert!(matches!(cf.read_stream(macros), Err(CfbError::NotAStream)));
}

#[test]
fn large_file_uses_difat() {
    let mut b = CfbBuilder::new();
    b.add_stream("Big", vec![0x5A; 8 << 20]).unwrap();
    let file = b.finish().unwrap();
    let num_difat = u32::from_le_bytes(file[72..76].try_into().unwrap());
    assert!(num_difat > 0, "an 8 MiB file needs DIFAT sectors");
    let cf = CompoundFile::parse(&file, CfbLimits::default()).unwrap();
    let big = cf.find("Big").unwrap();
    assert_eq!(cf.read_stream(big).unwrap().len(), 8 << 20);
}

#[test]
fn fat_self_loop_is_a_cycle_not_a_hang() {
    let mut file = sample();
    let cf = CompoundFile::parse(&file, CfbLimits::default()).unwrap();
    let wd = cf.find("WordDocument").unwrap();
    let start = cf.entries()[wd].start_sector;
    // Point the stream's first FAT entry to itself.
    let fat0 = u32::from_le_bytes(file[76..80].try_into().unwrap()) as usize;
    let off = (fat0 + 1) * 512 + start as usize * 4;
    file[off..off + 4].copy_from_slice(&start.to_le_bytes());
    let cf = CompoundFile::parse(&file, CfbLimits::default()).unwrap();
    let wd = cf.find("WordDocument").unwrap();
    assert!(matches!(cf.read_stream(wd), Err(CfbError::ChainCycle(_))));
}

#[test]
fn directory_cycle_is_detected() {
    let mut file = sample();
    // Make the root's child subtree point back at itself via the left pointer.
    let root_child_off = dir_entry_offset(&file, 0) + 76;
    let child = u32::from_le_bytes(file[root_child_off..root_child_off + 4].try_into().unwrap());
    let child_left = dir_entry_offset(&file, child as usize) + 68;
    file[child_left..child_left + 4].copy_from_slice(&child.to_le_bytes());
    assert!(matches!(
        CompoundFile::parse(&file, CfbLimits::default()),
        Err(CfbError::DirectoryCycle)
    ));
}

#[test]
fn orphaned_entries_are_reported() {
    let mut file = sample();
    let root_child_off = dir_entry_offset(&file, 0) + 76;
    file[root_child_off..root_child_off + 4].copy_from_slice(&NOSTREAM.to_le_bytes());
    let cf = CompoundFile::parse(&file, CfbLimits::default()).unwrap();
    assert!(cf.anomalies().iter().any(|a| matches!(a, CfbAnomaly::OrphanedEntries(n) if *n > 0)));
}

#[test]
fn header_validation() {
    let good = sample();
    let patch = |off: usize, bytes: &[u8]| {
        let mut f = good.clone();
        f[off..off + bytes.len()].copy_from_slice(bytes);
        CompoundFile::parse(&f, CfbLimits::default()).map(|_| ())
    };
    assert!(matches!(patch(0, &[0]), Err(CfbError::NotCompoundFile)));
    assert!(matches!(patch(26, &4u16.to_le_bytes()), Err(CfbError::InvalidHeader(_))));
    assert!(matches!(patch(28, &0xFEFFu16.to_le_bytes()), Err(CfbError::InvalidHeader(_))));
    assert!(matches!(patch(32, &7u16.to_le_bytes()), Err(CfbError::InvalidHeader(_))));
    assert!(matches!(patch(56, &1u32.to_le_bytes()), Err(CfbError::InvalidHeader(_))));
    assert!(matches!(patch(44, &u32::MAX.to_le_bytes()), Err(CfbError::InvalidHeader(_))));
    assert!(CompoundFile::parse(&good[..300], CfbLimits::default()).is_err());
}

#[test]
fn stream_size_limit() {
    let file = sample();
    let limits = CfbLimits { max_stream_size: 1000, ..CfbLimits::default() };
    let cf = CompoundFile::parse(&file, limits).unwrap();
    let wd = cf.find("WordDocument").unwrap();
    assert!(matches!(cf.read_stream(wd), Err(CfbError::StreamTooLarge(5000))));
}

#[test]
fn corruption_never_panics() {
    let file = sample();
    for cut in (0..file.len()).step_by(7) {
        if let Ok(cf) = CompoundFile::parse(&file[..cut], CfbLimits::default()) {
            for i in 0..cf.entries().len() {
                let _ = cf.read_stream(i);
            }
        }
    }
    for i in 0..file.len() {
        for mask in [0x01u8, 0x80, 0xFF] {
            let mut c = file.clone();
            c[i] ^= mask;
            if let Ok(cf) = CompoundFile::parse(&c, CfbLimits::default()) {
                for j in 0..cf.entries().len() {
                    let _ = cf.read_stream(j);
                }
            }
        }
    }
}

#[test]
fn clsid_formatting() {
    // Microsoft Equation 3.0
    let c = [
        0x02, 0xCE, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ];
    assert_eq!(format_clsid(&c), "{0002CE02-0000-0000-C000-000000000046}");
    assert!(clsid_is_null(&[0; 16]));
}

fn set_fat(file: &mut [u8], sector: u32, value: u32) {
    let fat0 = u32::from_le_bytes(file[76..80].try_into().unwrap()) as usize;
    let off = (fat0 + 1) * 512 + sector as usize * 4;
    file[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn start_of(file: &[u8], path: &str) -> u32 {
    let cf = CompoundFile::parse(file, CfbLimits::default()).unwrap();
    cf.entries()[cf.find(path).unwrap()].start_sector
}

fn utf16(name: &str) -> Vec<u8> {
    name.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Overwrites the first occurrence of the UTF-16LE `from` with `to` (same length).
fn rename_raw(file: &mut [u8], from: &str, to: &[u16]) {
    let needle = utf16(from);
    let at = file.windows(needle.len()).position(|w| w == needle).unwrap();
    for (i, u) in to.iter().enumerate() {
        file[at + i * 2..at + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
    }
}

#[test]
fn a_sector_shared_by_two_streams_is_refused() {
    let mut file = sample();
    let a = start_of(&file, "WordDocument");
    let b = start_of(&file, "Macros/VBA/ThisDocument");
    // WordDocument's chain: a -> b -> b+1 ...: its sectors are also ThisDocument's.
    set_fat(&mut file, a, b);
    assert!(matches!(
        CompoundFile::parse(&file, CfbLimits::default()),
        Err(CfbError::CrossLinked(_))
    ));
}

#[test]
fn a_stream_over_the_fat_sectors_is_refused() {
    let mut file = sample();
    let a = start_of(&file, "WordDocument");
    let fat0 = u32::from_le_bytes(file[76..80].try_into().unwrap());
    set_fat(&mut file, a, fat0);
    assert!(matches!(
        CompoundFile::parse(&file, CfbLimits::default()),
        Err(CfbError::CrossLinked(_))
    ));
}

#[test]
fn two_mini_streams_sharing_a_mini_sector_are_refused() {
    let mut b = CfbBuilder::new();
    b.add_stream("One", vec![1; 200]).unwrap();
    b.add_stream("Two", vec![2; 200]).unwrap();
    let mut file = b.finish().unwrap();
    // Raw directory: entry 1 and 2 are the streams; point "Two" at the start of "One".
    let (mut starts, mut offs) = (Vec::new(), Vec::new());
    for i in 1..3 {
        let off = dir_entry_offset(&file, i) + 116;
        starts.push(u32::from_le_bytes(file[off..off + 4].try_into().unwrap()));
        offs.push(off);
    }
    assert_ne!(starts[0], starts[1]);
    file[offs[1]..offs[1] + 4].copy_from_slice(&starts[0].to_le_bytes());
    assert!(matches!(
        CompoundFile::parse(&file, CfbLimits::default()),
        Err(CfbError::CrossLinked(_))
    ));
}

#[test]
fn rereading_streams_is_metered() {
    let file = sample();
    let limits = CfbLimits { max_total_read: 6000, ..CfbLimits::default() };
    let cf = CompoundFile::parse(&file, limits).unwrap();
    let wd = cf.find("WordDocument").unwrap();
    assert_eq!(cf.read_stream(wd).unwrap().len(), 5000);
    assert!(matches!(cf.read_stream(wd), Err(CfbError::ReadBudgetExceeded)));
    // A small stream still fits in what is left.
    let si = cf.find("\u{5}SummaryInformation").unwrap();
    assert_eq!(cf.read_stream(si).unwrap().len(), 3);
}

#[test]
fn forbidden_characters_in_names_are_refused() {
    for bad in ['/', '\\', ':', '!'] {
        let mut file = sample();
        rename_raw(&mut file, "WordDocument", &[bad as u16]);
        assert!(
            matches!(
                CompoundFile::parse(&file, CfbLimits::default()),
                Err(CfbError::InvalidDirectory(_))
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn data_hidden_after_the_name_terminator_is_refused() {
    let mut b = CfbBuilder::new();
    b.add_stream("Workbook", vec![1; 10]).unwrap();
    let mut file = b.finish().unwrap();
    // "Workbook" -> "Work\0ook" with the declared length unchanged: other readers see a
    // different name than the one this reader stops at.
    rename_raw(&mut file, "Workbook", &['W' as u16, 'o' as u16, 'r' as u16, 'k' as u16, 0]);
    assert!(matches!(
        CompoundFile::parse(&file, CfbLimits::default()),
        Err(CfbError::InvalidDirectory(_))
    ));
}

#[test]
fn duplicate_sibling_names_are_refused_ignoring_case() {
    let mut b = CfbBuilder::new();
    b.add_stream("Alpha", vec![1; 10]).unwrap();
    b.add_stream("Alphb", vec![2; 10]).unwrap();
    let mut file = b.finish().unwrap();
    rename_raw(&mut file, "Alphb", &['A' as u16, 'L' as u16, 'P' as u16, 'H' as u16, 'A' as u16]);
    assert!(matches!(
        CompoundFile::parse(&file, CfbLimits::default()),
        Err(CfbError::InvalidDirectory(_))
    ));
    // The same name under different storages is fine.
    let mut ok = CfbBuilder::new();
    ok.add_stream("A/Alpha", vec![1]).unwrap();
    ok.add_stream("B/Alpha", vec![1]).unwrap();
    assert!(CompoundFile::parse(&ok.finish().unwrap(), CfbLimits::default()).is_ok());
}

#[test]
fn the_directory_chain_is_counted_before_it_is_copied() {
    let mut b = CfbBuilder::new();
    for i in 0..40 {
        b.add_stream(&format!("S{i}"), vec![1; 4]).unwrap();
    }
    let file = b.finish().unwrap();
    let limits = CfbLimits { max_directory_entries: 8, ..CfbLimits::default() };
    assert!(
        matches!(CompoundFile::parse(&file, limits), Err(CfbError::TooManyEntries(n)) if n >= 40)
    );
}
