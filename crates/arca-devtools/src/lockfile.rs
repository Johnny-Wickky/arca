// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! `check-lockfile`: every external package in Cargo.lock comes from crates.io, is listed once
//! and carries a SHA-256 checksum.

use std::collections::HashSet;
use std::path::Path;

use crate::util::{read_text, say};

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const SPARSE: &str = "sparse+https://index.crates.io/";

/// Summary of an acceptable lockfile.
#[derive(Debug, PartialEq, Eq)]
pub struct LockInfo {
    /// All `[[package]]` entries.
    pub packages: usize,
    /// Those that are not workspace members.
    pub external: usize,
}

/// The value of the line `key = "value"` in a package block.
fn field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    block.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(" = \"")?;
        let value = rest.strip_suffix('"')?;
        (!value.contains('"')).then_some(value)
    })
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks the text of a Cargo.lock.
///
/// # Errors
/// One message per offending package.
pub fn check(text: &str) -> Result<LockInfo, Vec<String>> {
    let blocks: Vec<&str> = text.split("[[package]]").skip(1).collect();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut external = 0usize;
    for block in &blocks {
        let name = field(block, "name").unwrap_or("?");
        let version = field(block, "version").unwrap_or("?");
        if !seen.insert((name, version)) {
            errors.push(format!("{name} {version}: listed twice"));
        }
        let Some(source) = field(block, "source") else { continue };
        external += 1;
        if source != CRATES_IO && source != SPARSE {
            errors.push(format!("{name} {version}: source {source:?} is not crates.io"));
        } else if !field(block, "checksum").is_some_and(is_sha256_hex) {
            errors.push(format!("{name} {version}: no SHA-256 checksum in Cargo.lock"));
        }
    }
    if errors.is_empty() { Ok(LockInfo { packages: blocks.len(), external }) } else { Err(errors) }
}

/// Entry point: `check-lockfile [Cargo.lock]`.
///
/// # Errors
/// An unreadable file or an unacceptable lockfile.
pub fn run(args: &[String]) -> Result<(), String> {
    let path = match args {
        [] => "Cargo.lock",
        [p] => p.as_str(),
        _ => return Err("usage: check-lockfile [Cargo.lock]".into()),
    };
    let text = read_text(Path::new(path))?;
    match check(&text) {
        Ok(info) => {
            say(&format!(
                "Cargo.lock ok: {} packages, {} external, all checksummed",
                info.packages, info.external
            ));
            Ok(())
        }
        Err(errors) => {
            let mut msg = String::from("Cargo.lock is not acceptable:");
            for e in errors {
                msg.push_str("\n  - ");
                msg.push_str(&e);
            }
            Err(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn lock(blocks: &[String]) -> String {
        format!(
            "version = 4\n{}",
            blocks.iter().map(|b| format!("\n[[package]]\n{b}")).collect::<String>()
        )
    }

    #[test]
    fn workspace_members_and_checksummed_registry_packages_pass() {
        let text = lock(&[
            "name = \"arca\"\nversion = \"0.1.0\"\ndependencies = [\n \"x\",\n]\n".into(),
            format!(
                "name = \"x\"\nversion = \"1.0.0\"\nsource = \"{CRATES_IO}\"\nchecksum = \"{SUM}\"\n"
            ),
        ]);
        assert_eq!(check(&text).unwrap(), LockInfo { packages: 2, external: 1 });
    }

    #[test]
    fn a_missing_or_malformed_checksum_is_refused() {
        let text = lock(&[format!("name = \"x\"\nversion = \"1.0.0\"\nsource = \"{SPARSE}\"\n")]);
        assert_eq!(check(&text).unwrap_err().len(), 1);
        let text = lock(&[format!(
            "name = \"x\"\nversion = \"1.0.0\"\nsource = \"{SPARSE}\"\nchecksum = \"ABC\"\n"
        )]);
        assert!(check(&text).is_err());
    }

    #[test]
    fn git_sources_and_duplicates_are_refused() {
        let git =
            lock(&["name = \"x\"\nversion = \"1.0.0\"\nsource = \"git+https://example.com/x\"\n"
                .into()]);
        assert!(check(&git).unwrap_err()[0].contains("not crates.io"));
        let dup = lock(&[
            "name = \"x\"\nversion = \"1.0.0\"\n".into(),
            "name = \"x\"\nversion = \"1.0.0\"\n".into(),
        ]);
        assert!(check(&dup).unwrap_err()[0].contains("listed twice"));
    }
}
