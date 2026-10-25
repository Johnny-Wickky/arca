// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! `wasm-imports`: the sandbox depends on the guest module importing nothing.

use std::path::Path;

use crate::util::{read_file, say};

const MAGIC: &[u8] = b"\0asm\x01\0\0\0";
const SECTION_IMPORT: u8 = 2;

/// What [`inspect`] found.
#[derive(Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    /// Number of sections.
    pub sections: usize,
    /// Number of entries of the import section (0 when there is none).
    pub imports: u32,
}

fn leb_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    let mut result = 0u32;
    for i in 0..5u32 {
        let b = *data.get(*pos).ok_or("truncated LEB128 integer")?;
        *pos += 1;
        let low = u32::from(b & 0x7F);
        if i == 4 && low > 0x0F {
            return Err("LEB128 integer does not fit in 32 bits".into());
        }
        result |= low << (7 * i);
        if b & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err("LEB128 integer is too long".into())
}

/// Walks the sections of a Wasm module and counts its imports.
///
/// # Errors
/// Not a Wasm v1 module, or a section that does not fit in the file.
pub fn inspect(data: &[u8]) -> Result<ModuleInfo, String> {
    if data.get(..8) != Some(MAGIC) {
        return Err("not a wasm v1 module".into());
    }
    let mut pos = 8usize;
    let mut sections = 0usize;
    let mut imports = 0u32;
    while pos < data.len() {
        let id = *data.get(pos).ok_or("truncated section header")?;
        pos += 1;
        let size = usize::try_from(leb_u32(data, &mut pos)?).map_err(|_| "section too large")?;
        let end = pos
            .checked_add(size)
            .filter(|&e| e <= data.len())
            .ok_or("section extends past the end of the file")?;
        if id == SECTION_IMPORT {
            let mut p = pos;
            imports = imports.saturating_add(leb_u32(data.get(..end).unwrap_or_default(), &mut p)?);
        }
        sections += 1;
        pos = end;
    }
    Ok(ModuleInfo { sections, imports })
}

/// Entry point: `wasm-imports <file>`.
///
/// # Errors
/// A malformed module, a module with imports, or an unreadable file.
pub fn run(args: &[String]) -> Result<(), String> {
    let [file] = args else { return Err("usage: wasm-imports <file.wasm>".into()) };
    let path = Path::new(file);
    let data = read_file(path)?;
    let info = inspect(&data).map_err(|e| format!("{file}: {e}"))?;
    if info.imports != 0 {
        return Err(format!(
            "{file}: guest module has an import section with {} entries",
            info.imports
        ));
    }
    say(&format!("guest imports nothing ({} sections)", info.sections));
    say(&format!("{}  {file}", arca_digest::sha256(&data)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(sections: &[(u8, &[u8])]) -> Vec<u8> {
        let mut m = MAGIC.to_vec();
        for (id, body) in sections {
            m.push(*id);
            m.push(u8::try_from(body.len()).unwrap());
            m.extend_from_slice(body);
        }
        m
    }

    #[test]
    fn a_module_without_imports_passes() {
        let m = module(&[(1, &[0]), (3, &[0]), (7, &[0])]);
        assert_eq!(inspect(&m).unwrap(), ModuleInfo { sections: 3, imports: 0 });
        assert_eq!(inspect(MAGIC).unwrap().sections, 0);
    }

    #[test]
    fn an_import_section_with_entries_is_counted() {
        // one import: module "a", name "b", function of type 0
        let m = module(&[(2, &[1, 1, b'a', 1, b'b', 0, 0])]);
        assert_eq!(inspect(&m).unwrap().imports, 1);
    }

    #[test]
    fn an_empty_import_section_has_no_entries() {
        let m = module(&[(2, &[0])]);
        assert_eq!(inspect(&m).unwrap().imports, 0);
    }

    #[test]
    fn malformed_modules_are_errors() {
        assert!(inspect(b"").is_err());
        assert!(inspect(b"\0asm\x02\0\0\0").is_err());
        let mut m = MAGIC.to_vec();
        m.extend_from_slice(&[1, 5, 0]); // section claims 5 bytes, has 1
        assert!(inspect(&m).is_err());
        let mut m = MAGIC.to_vec();
        m.extend_from_slice(&[1, 0x80]); // truncated LEB
        assert!(inspect(&m).is_err());
        let mut m = MAGIC.to_vec();
        m.extend_from_slice(&[1, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]); // > 32 bits
        assert!(inspect(&m).is_err());
    }
}
