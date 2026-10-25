// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! `elf-check`: a shared library qualifies for Android devices with 16 KiB memory pages.
//!
//! Android 15+ can run with 16 KiB pages, and Google Play requires it of new apps. A library
//! whose loadable segments are aligned to only 4 KiB fails to load there. The check reads the
//! ELF program headers (no external tools):
//!
//! * every `PT_LOAD` segment has `p_align >= --align`, and offset and address agree modulo
//!   `p_align`;
//! * the library is a shared object, ELF64 or ELF32, little-endian, for the expected machine.

use std::path::Path;

use crate::util::{parse_args, read_file, say};

const PT_LOAD: u32 = 1;
const ET_DYN: u16 = 3;

/// `--machine` names and their `e_machine` values.
const MACHINES: &[(&str, u16)] = &[("aarch64", 183), ("arm", 40), ("x86_64", 62), ("x86", 3)];

/// What [`check`] verified.
#[derive(Debug, PartialEq, Eq)]
pub struct ElfInfo {
    /// 32 or 64.
    pub bits: u32,
    /// Number of `PT_LOAD` segments.
    pub loads: usize,
}

fn u16_at(d: &[u8], off: usize) -> Result<u16, String> {
    let b: [u8; 2] = d
        .get(off..off.saturating_add(2))
        .and_then(|s| s.try_into().ok())
        .ok_or("truncated ELF header")?;
    Ok(u16::from_le_bytes(b))
}

fn u32_at(d: &[u8], off: usize) -> Result<u32, String> {
    let b: [u8; 4] = d
        .get(off..off.saturating_add(4))
        .and_then(|s| s.try_into().ok())
        .ok_or("truncated ELF header")?;
    Ok(u32::from_le_bytes(b))
}

fn u64_at(d: &[u8], off: usize) -> Result<u64, String> {
    let b: [u8; 8] = d
        .get(off..off.saturating_add(8))
        .and_then(|s| s.try_into().ok())
        .ok_or("truncated ELF header")?;
    Ok(u64::from_le_bytes(b))
}

/// Checks `data` as an ELF shared object: alignment of the `PT_LOAD` segments to at least
/// `align` and, when given, the machine.
///
/// # Errors
/// A description of the first violation.
pub fn check(data: &[u8], align: u64, machine: Option<u16>) -> Result<ElfInfo, String> {
    if data.get(..4) != Some(b"\x7fELF") {
        return Err("not an ELF file".into());
    }
    let bits = match data.get(4) {
        Some(1) => 32u32,
        Some(2) => 64,
        _ => return Err("not a little-endian ELF32/ELF64 file".into()),
    };
    if data.get(5) != Some(&1) {
        return Err("not a little-endian ELF32/ELF64 file".into());
    }
    let e_type = u16_at(data, 16)?;
    let e_machine = u16_at(data, 18)?;
    let (phoff, phentsize, phnum) = if bits == 64 {
        (u64_at(data, 32)?, u16_at(data, 54)?, u16_at(data, 56)?)
    } else {
        (u64::from(u32_at(data, 28)?), u16_at(data, 42)?, u16_at(data, 44)?)
    };
    if e_type != ET_DYN {
        return Err(format!("not a shared object (e_type={e_type})"));
    }
    if let Some(m) = machine
        && e_machine != m
    {
        return Err(format!("machine {e_machine}, expected {m}"));
    }
    let mut loads = 0usize;
    for index in 0..u64::from(phnum) {
        let base = index
            .checked_mul(u64::from(phentsize))
            .and_then(|o| o.checked_add(phoff))
            .and_then(|o| usize::try_from(o).ok())
            .ok_or("program header offset overflows")?;
        let p_type = u32_at(data, base)?;
        let (p_offset, p_vaddr, p_align) = if bits == 64 {
            (
                u64_at(data, base.saturating_add(8))?,
                u64_at(data, base.saturating_add(16))?,
                u64_at(data, base.saturating_add(48))?,
            )
        } else {
            (
                u64::from(u32_at(data, base.saturating_add(4))?),
                u64::from(u32_at(data, base.saturating_add(8))?),
                u64::from(u32_at(data, base.saturating_add(28))?),
            )
        };
        if p_type != PT_LOAD {
            continue;
        }
        loads += 1;
        if p_align < align {
            return Err(format!("PT_LOAD #{index} is aligned to {p_align}, need {align}"));
        }
        if p_align != 0 && p_offset % p_align != p_vaddr % p_align {
            return Err(format!(
                "PT_LOAD #{index} offset {p_offset:#x} and address {p_vaddr:#x} disagree modulo {p_align}"
            ));
        }
    }
    if loads == 0 {
        return Err("no loadable segments".into());
    }
    Ok(ElfInfo { bits, loads })
}

/// Entry point: `elf-check <library> [--align N] [--machine aarch64|arm|x86_64|x86]`.
///
/// # Errors
/// Bad arguments, an unreadable file or a library that does not qualify.
pub fn run(args: &[String]) -> Result<(), String> {
    let p = parse_args(args, &["align", "machine"], &[])?;
    let [lib] = p.pos.as_slice() else {
        return Err(
            "usage: elf-check <library.so> [--align N] [--machine aarch64|arm|x86_64|x86]".into()
        );
    };
    let align = match p.opt("align") {
        Some(a) => a.parse::<u64>().map_err(|_| format!("--align: not a number: {a}"))?,
        None => 16384,
    };
    let machine = match p.opt("machine") {
        Some(name) => Some(
            MACHINES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .ok_or_else(|| format!("--machine: unknown machine {name}"))?,
        ),
        None => None,
    };
    let data = read_file(Path::new(lib))?;
    let info = check(&data, align, machine).map_err(|e| format!("{lib}: {e}"))?;
    say(&format!(
        "elf-check: {lib}: {} PT_LOAD segments aligned to >= {align} (ELF{})",
        info.loads, info.bits
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 shared object with the given `PT_LOAD` segments `(offset, vaddr, align)`.
    fn elf64(machine: u16, loads: &[(u64, u64, u64)]) -> Vec<u8> {
        let mut d = vec![0u8; 64 + 56 * loads.len()];
        d[..4].copy_from_slice(b"\x7fELF");
        d[4] = 2;
        d[5] = 1;
        d[16..18].copy_from_slice(&3u16.to_le_bytes());
        d[18..20].copy_from_slice(&machine.to_le_bytes());
        d[32..40].copy_from_slice(&64u64.to_le_bytes());
        d[54..56].copy_from_slice(&56u16.to_le_bytes());
        d[56..58].copy_from_slice(&u16::try_from(loads.len()).unwrap().to_le_bytes());
        for (i, (off, va, al)) in loads.iter().enumerate() {
            let b = 64 + 56 * i;
            d[b..b + 4].copy_from_slice(&1u32.to_le_bytes());
            d[b + 8..b + 16].copy_from_slice(&off.to_le_bytes());
            d[b + 16..b + 24].copy_from_slice(&va.to_le_bytes());
            d[b + 48..b + 56].copy_from_slice(&al.to_le_bytes());
        }
        d
    }

    /// The same for ELF32.
    fn elf32(machine: u16, loads: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut d = vec![0u8; 52 + 32 * loads.len()];
        d[..4].copy_from_slice(b"\x7fELF");
        d[4] = 1;
        d[5] = 1;
        d[16..18].copy_from_slice(&3u16.to_le_bytes());
        d[18..20].copy_from_slice(&machine.to_le_bytes());
        d[28..32].copy_from_slice(&52u32.to_le_bytes());
        d[42..44].copy_from_slice(&32u16.to_le_bytes());
        d[44..46].copy_from_slice(&u16::try_from(loads.len()).unwrap().to_le_bytes());
        for (i, (off, va, al)) in loads.iter().enumerate() {
            let b = 52 + 32 * i;
            d[b..b + 4].copy_from_slice(&1u32.to_le_bytes());
            d[b + 4..b + 8].copy_from_slice(&off.to_le_bytes());
            d[b + 8..b + 12].copy_from_slice(&va.to_le_bytes());
            d[b + 28..b + 32].copy_from_slice(&al.to_le_bytes());
        }
        d
    }

    #[test]
    fn aligned_libraries_pass() {
        let d = elf64(183, &[(0, 0, 16384), (0x4000, 0x14000, 16384)]);
        assert_eq!(check(&d, 16384, Some(183)).unwrap(), ElfInfo { bits: 64, loads: 2 });
        let d = elf32(40, &[(0, 0, 65536)]);
        assert_eq!(check(&d, 16384, Some(40)).unwrap(), ElfInfo { bits: 32, loads: 1 });
    }

    #[test]
    fn four_kib_alignment_is_refused() {
        let d = elf64(183, &[(0, 0, 4096)]);
        assert!(check(&d, 16384, None).unwrap_err().contains("aligned to 4096"));
        assert!(check(&d, 4096, None).is_ok());
    }

    #[test]
    fn offset_and_address_must_agree_modulo_alignment() {
        let d = elf64(62, &[(0x1000, 0x2000, 16384)]);
        assert!(check(&d, 16384, None).unwrap_err().contains("disagree"));
    }

    #[test]
    fn wrong_machine_type_and_garbage_are_refused() {
        let d = elf64(183, &[(0, 0, 16384)]);
        assert!(check(&d, 16384, Some(62)).unwrap_err().contains("machine 183"));
        let mut exec = d.clone();
        exec[16] = 2;
        assert!(check(&exec, 16384, None).unwrap_err().contains("not a shared object"));
        assert!(check(b"MZ", 16384, None).is_err());
        assert!(check(&d[..40], 16384, None).is_err());
        assert!(check(&elf64(183, &[]), 16384, None).unwrap_err().contains("no loadable"));
    }

    #[test]
    fn a_header_table_past_the_end_is_an_error_not_a_panic() {
        let mut d = elf64(183, &[(0, 0, 16384)]);
        d[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(check(&d, 16384, None).is_err());
    }
}
