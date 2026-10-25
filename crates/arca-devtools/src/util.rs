// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Small shared helpers: output, argument parsing, file listing.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Writes one line to standard output (a closed pipe is not an error worth stopping for).
pub fn say(msg: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{msg}");
}

/// Writes one line to standard error.
pub fn warn(msg: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{msg}");
}

/// Reads a whole file, with the path in the error message.
///
/// # Errors
/// The I/O error, prefixed with the path.
pub fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads a whole UTF-8 file (invalid sequences are replaced).
///
/// # Errors
/// The I/O error, prefixed with the path.
pub fn read_text(path: &Path) -> Result<String, String> {
    read_file(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Entries of a directory sorted by name: `(name, path)`.
///
/// # Errors
/// The I/O error, prefixed with the path.
pub fn list_dir(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        out.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
    }
    out.sort();
    Ok(out)
}

/// All regular files below `dir` (recursively), in sorted order. Symlinked directories are
/// not followed.
///
/// # Errors
/// The I/O error, prefixed with the path.
pub fn walk_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    walk_into(dir, &mut out, 0)?;
    Ok(out)
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err(format!("{}: directory nesting too deep", dir.display()));
    }
    for (_, path) in list_dir(dir)? {
        let meta = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            walk_into(&path, out, depth + 1)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Characters of `[A-Za-z0-9_]`.
#[must_use]
pub fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Parsed command line: positional arguments and `--name value` / `--name=value` options.
#[derive(Debug, Default)]
pub struct Parsed {
    /// Positional arguments in order.
    pub pos: Vec<String>,
    opts: Vec<(String, String)>,
    flags: Vec<String>,
}

impl Parsed {
    /// Value of the option `name` (without dashes), the last one given.
    #[must_use]
    pub fn opt(&self, name: &str) -> Option<&str> {
        self.opts.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Whether the boolean flag `name` was given.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }
}

/// Parses `args`; `valued` lists the options that take a value, `bools` the plain flags.
///
/// # Errors
/// An unknown option or a missing value.
pub fn parse_args(args: &[String], valued: &[&str], bools: &[&str]) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(rest) = arg.strip_prefix("--") else {
            out.pos.push(arg.clone());
            continue;
        };
        let (name, inline) = match rest.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (rest, None),
        };
        if valued.contains(&name) {
            let value = match inline {
                Some(v) => v,
                None => it.next().cloned().ok_or_else(|| format!("--{name} needs a value"))?,
            };
            out.opts.push((name.to_owned(), value));
        } else if bools.contains(&name) && inline.is_none() {
            out.flags.push(name.to_owned());
        } else {
            return Err(format!("unknown option --{name}"));
        }
    }
    Ok(out)
}

/// A scratch directory for tests, removed on drop.
#[cfg(test)]
pub struct TempDir(pub PathBuf);

#[cfg(test)]
impl TempDir {
    /// Creates a fresh, empty directory whose name contains `tag`.
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "arca-devtools-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// Writes `data` to `rel` below the directory, creating parents.
    pub fn write(&self, rel: &str, data: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
}

#[cfg(test)]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn options_flags_and_positionals() {
        let p =
            parse_args(&v(&["a", "--out", "x", "--n=3", "--fast", "b"]), &["out", "n"], &["fast"])
                .unwrap();
        assert_eq!(p.pos, ["a", "b"]);
        assert_eq!(p.opt("out"), Some("x"));
        assert_eq!(p.opt("n"), Some("3"));
        assert!(p.flag("fast"));
        assert!(!p.flag("slow"));
    }

    #[test]
    fn unknown_option_and_missing_value_are_errors() {
        assert!(parse_args(&v(&["--nope"]), &[], &[]).is_err());
        assert!(parse_args(&v(&["--out"]), &["out"], &[]).is_err());
        assert!(parse_args(&v(&["--fast=1"]), &[], &["fast"]).is_err());
    }

    #[test]
    fn walk_is_sorted_and_recursive() {
        let t = TempDir::new("walk");
        t.write("b/2.txt", "");
        t.write("a.txt", "");
        t.write("b/1.txt", "");
        let names: Vec<String> = walk_files(&t.0)
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(&t.0).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(names, ["a.txt", "b/1.txt", "b/2.txt"]);
    }
}
