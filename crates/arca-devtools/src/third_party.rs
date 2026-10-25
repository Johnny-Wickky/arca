// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! `third-party`: the third-party notices shown in the app (Licences -> Third-party).
//!
//! The list is computed, not typed: it is every crate that ends up inside `libarca_jni.so`
//! (the normal dependencies of `arca-jni` for the given target, found with `cargo metadata`),
//! each with its declared licence and the licence/notice files that ship in its source. MIT and
//! Apache-2.0 oblige redistributors to carry those texts; this is how the release does.
//!
//! The command fails when a crate has no declared licence or no licence text at all, so a
//! dependency that cannot be redistributed correctly stops the release instead of slipping in.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::json::{self, Value};
use crate::util::{list_dir, parse_args, read_text, say};

const ROOT_PACKAGE: &str = "arca-jni";
const NOTICE_PREFIXES: &[&str] =
    &["LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT"];
const MAX_FILE: usize = 200_000;

const HEADER: &str = "\
THIRD-PARTY SOFTWARE IN THIS APPLICATION
========================================

Arca itself is free software under the GNU Affero General Public License, version 3, with the
additional terms shown in the other tabs of this screen. The Rust libraries below are compiled
into the application's native library. Each is used under the licence it declares; the texts
that accompany each library are reproduced in full.

The list is generated from the build's dependency graph (arca-devtools third-party), so it
matches the build it ships in.
";

/// One shipped crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Crate name.
    pub name: String,
    /// Crate version.
    pub version: String,
    /// Directory of the crate's `Cargo.toml`.
    pub dir: PathBuf,
    /// Declared SPDX expression.
    pub license: Option<String>,
    /// Repository URL.
    pub repository: Option<String>,
    /// Authors as declared.
    pub authors: Vec<String>,
}

fn non_empty(v: Option<&str>) -> Option<String> {
    v.filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The external crates reachable from `arca-jni` through normal (non-dev, non-build)
/// dependencies, sorted by name and version.
///
/// # Errors
/// The metadata lacks the root package or is inconsistent.
pub fn shipped_packages(meta: &Value) -> Result<Vec<Package>, String> {
    let packages = meta.get("packages").map_or(&[][..], Value::items);
    let by_id: HashMap<&str, &Value> =
        packages.iter().filter_map(|p| p.str_of("id").map(|id| (id, p))).collect();
    let nodes: HashMap<&str, &Value> = meta
        .get("resolve")
        .and_then(|r| r.get("nodes"))
        .map_or(&[][..], Value::items)
        .iter()
        .filter_map(|n| n.str_of("id").map(|id| (id, n)))
        .collect();
    let roots: Vec<&str> = packages
        .iter()
        .filter(|p| p.str_of("name") == Some(ROOT_PACKAGE))
        .filter_map(|p| p.str_of("id"))
        .collect();
    if roots.is_empty() {
        return Err(format!("package {ROOT_PACKAGE} not found"));
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = roots;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = nodes.get(id).ok_or_else(|| format!("no resolve node for {id}"))?;
        for dep in node.get("deps").map_or(&[][..], Value::items) {
            // Normal dependencies only: build scripts and tests do not ship.
            let normal = dep
                .get("dep_kinds")
                .map_or(&[][..], Value::items)
                .iter()
                .any(|k| matches!(k.get("kind"), None | Some(Value::Null)));
            if let (true, Some(pkg)) = (normal, dep.str_of("pkg")) {
                stack.push(pkg);
            }
        }
    }
    let mut out = Vec::new();
    for id in seen {
        let pkg = by_id.get(id).ok_or_else(|| format!("no package entry for {id}"))?;
        // A workspace member (no source) is Arca's own code.
        if !matches!(pkg.get("source"), Some(Value::Str(_))) {
            continue;
        }
        let manifest = pkg.str_of("manifest_path").ok_or("package without manifest_path")?;
        out.push(Package {
            name: pkg.str_of("name").unwrap_or_default().to_owned(),
            version: pkg.str_of("version").unwrap_or_default().to_owned(),
            dir: Path::new(manifest).parent().map(Path::to_path_buf).unwrap_or_default(),
            license: non_empty(pkg.str_of("license")),
            repository: non_empty(pkg.str_of("repository")),
            authors: pkg
                .get("authors")
                .map_or(&[][..], Value::items)
                .iter()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect(),
        });
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(out)
}

fn notice_files(pkg: &Package, fallbacks: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for (name, path) in list_dir(&pkg.dir)? {
        let upper = name.to_uppercase();
        if path.is_file() && NOTICE_PREFIXES.iter().any(|p| upper.starts_with(p)) {
            found.push(path);
        }
    }
    if !found.is_empty() {
        return Ok(found);
    }
    // Some published crates carry no licence file (the project keeps it at the repository
    // root, outside the package). tools/third_party_licenses/<name>.txt holds the text, taken
    // from that repository; a family of crates (wasmi_core, wasmi_ir, ...) shares the file of
    // its first name part.
    let family = pkg.name.split('_').next().unwrap_or_default();
    for stem in [pkg.name.as_str(), family] {
        let fallback = fallbacks.join(format!("{stem}.txt"));
        if fallback.is_file() {
            return Ok(vec![fallback]);
        }
    }
    Ok(Vec::new())
}

/// The complete notices text.
///
/// # Errors
/// One message per crate that cannot be redistributed correctly (no licence declared or no
/// licence text found), or an unreadable licence file.
pub fn render(packages: &[Package], fallbacks: &Path) -> Result<String, Vec<String>> {
    let mut problems = Vec::new();
    let mut parts: Vec<String> =
        vec![HEADER.to_owned(), format!("{} libraries:\n", packages.len())];
    for pkg in packages {
        parts.push(format!(
            "  - {} {}  ({})",
            pkg.name,
            pkg.version,
            pkg.license.as_deref().unwrap_or("NO LICENCE")
        ));
    }
    parts.push(String::new());
    let mut printed: HashMap<String, String> = HashMap::new();
    for pkg in packages {
        let name = format!("{} {}", pkg.name, pkg.version);
        if pkg.license.is_none() {
            problems.push(format!("{name}: no declared licence"));
        }
        let files = match notice_files(pkg, fallbacks) {
            Ok(f) => f,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        if files.is_empty() {
            problems.push(format!("{name}: no licence text in the source"));
        }
        let rule = "=".repeat(78);
        parts.push(rule.clone());
        parts.push(name.clone());
        parts.push(format!("Licence: {}", pkg.license.as_deref().unwrap_or("unknown")));
        if let Some(repo) = &pkg.repository {
            parts.push(format!("Source:  {repo}"));
        }
        if !pkg.authors.is_empty() {
            let names: Vec<&str> = pkg
                .authors
                .iter()
                .map(|a| a.split('<').next().unwrap_or_default().trim())
                .collect();
            parts.push(format!("Authors: {}", names.join("; ")));
        }
        parts.push(rule);
        for path in files {
            let raw = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    problems.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            let head = raw.get(..MAX_FILE).unwrap_or(&raw);
            let data = String::from_utf8_lossy(head).trim_matches('\n').to_owned();
            let file_name =
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            parts.push(format!("--- {file_name} ---"));
            if let Some(first) = printed.get(&data) {
                // Word-for-word the same text as above (the Apache-2.0 text, usually).
                parts.push(format!("(identical to {first})"));
            } else {
                printed.insert(data.clone(), format!("{file_name} of {name}"));
                parts.push(data);
            }
            parts.push(String::new());
        }
    }
    if problems.is_empty() { Ok(parts.join("\n") + "\n") } else { Err(problems) }
}

fn cargo_metadata(root: &Path, target: &str) -> Result<String, String> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1", "--locked", "--filter-platform", target]);
    // Extra flags (for example "--offline") are split on whitespace; none contains spaces.
    let extra = std::env::var("ARCA_CARGO_FLAGS").unwrap_or_default();
    cmd.args(extra.split_whitespace().filter(|f| *f != "--locked"));
    let done = cmd.current_dir(root).output().map_err(|e| format!("cannot run cargo: {e}"))?;
    if !done.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&done.stderr)));
    }
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

/// Entry point: `third-party [--target T] [--out FILE] [--root DIR] [--metadata FILE|-]`.
///
/// Without `--metadata` it runs `cargo metadata --format-version 1 --locked --filter-platform T`
/// in `--root` (default: the current directory); with `--metadata -` the JSON is read from
/// standard input.
///
/// # Errors
/// Bad arguments, a failing `cargo`, or crates without a usable licence.
pub fn run(args: &[String], stdin: &mut dyn Read) -> Result<(), String> {
    let p = parse_args(args, &["target", "out", "root", "metadata"], &[])?;
    if !p.pos.is_empty() {
        return Err(
            "usage: third-party [--target T] [--out FILE] [--root DIR] [--metadata FILE|-]".into(),
        );
    }
    let root = PathBuf::from(p.opt("root").unwrap_or("."));
    let target = p.opt("target").unwrap_or("aarch64-linux-android");
    let out = p.opt("out").map_or_else(
        || root.join("android/app/src/main/assets/THIRD_PARTY_NOTICES.txt"),
        PathBuf::from,
    );
    let text = match p.opt("metadata") {
        Some("-") => {
            let mut s = String::new();
            stdin.read_to_string(&mut s).map_err(|e| format!("stdin: {e}"))?;
            s
        }
        Some(file) => read_text(Path::new(file))?,
        None => cargo_metadata(&root, target)?,
    };
    let meta = json::parse(&text)?;
    let packages = shipped_packages(&meta)?;
    let notices =
        render(&packages, &root.join("tools/third_party_licenses")).map_err(|problems| {
            let mut msg = String::from("cannot produce complete notices:");
            for line in problems {
                msg.push_str("\n  ");
                msg.push_str(&line);
            }
            msg
        })?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&out, notices.as_bytes()).map_err(|e| format!("{}: {e}", out.display()))?;
    say(&format!(
        "third-party: {} libraries -> {} ({} bytes)",
        packages.len(),
        out.display(),
        notices.len()
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TempDir;

    fn metadata(dir: &Path) -> String {
        let d = dir.to_string_lossy().replace('\\', "/");
        format!(
            r#"{{"packages":[
 {{"id":"jni 1","name":"arca-jni","version":"0.1.0","source":null,"manifest_path":"{d}/jni/Cargo.toml"}},
 {{"id":"own 1","name":"arca-own","version":"0.1.0","source":null,"manifest_path":"{d}/own/Cargo.toml"}},
 {{"id":"b 1","name":"beta","version":"1.0.0","source":"registry+x","license":"MIT","repository":"https://e/b","authors":["A <a@e>","B"],"manifest_path":"{d}/beta/Cargo.toml"}},
 {{"id":"a 1","name":"alpha_core","version":"2.0.0","source":"registry+x","license":"Apache-2.0","manifest_path":"{d}/alpha/Cargo.toml"}},
 {{"id":"t 1","name":"testonly","version":"1.0.0","source":"registry+x","license":"MIT","manifest_path":"{d}/t/Cargo.toml"}}],
"resolve":{{"nodes":[
 {{"id":"jni 1","deps":[{{"pkg":"own 1","dep_kinds":[{{"kind":null}}]}},{{"pkg":"b 1","dep_kinds":[{{"kind":null}}]}},{{"pkg":"t 1","dep_kinds":[{{"kind":"dev"}}]}}]}},
 {{"id":"own 1","deps":[{{"pkg":"a 1","dep_kinds":[{{"kind":null,"target":null}},{{"kind":"build"}}]}}]}},
 {{"id":"b 1","deps":[]}},{{"id":"a 1","deps":[]}},{{"id":"t 1","deps":[]}}]}}}}"#
        )
    }

    fn fixture() -> TempDir {
        let t = TempDir::new("tp");
        t.write("beta/LICENSE-MIT", "MIT text\n\n");
        t.write("beta/README.md", "ignored");
        t.write("alpha/Cargo.toml", "");
        t.write("lic/alpha.txt", "fallback text");
        t
    }

    #[test]
    fn only_normal_dependencies_of_the_root_are_listed() {
        let t = fixture();
        let meta = json::parse(&metadata(&t.0)).unwrap();
        let names: Vec<String> =
            shipped_packages(&meta).unwrap().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["alpha_core", "beta"]);
    }

    #[test]
    fn renders_notices_with_licence_files_and_family_fallbacks() {
        let t = fixture();
        let meta = json::parse(&metadata(&t.0)).unwrap();
        let text = render(&shipped_packages(&meta).unwrap(), &t.0.join("lic")).unwrap();
        assert!(text.starts_with("THIRD-PARTY SOFTWARE"));
        assert!(text.contains(
            "2 libraries:\n\n  - alpha_core 2.0.0  (Apache-2.0)\n  - beta 1.0.0  (MIT)\n"
        ));
        assert!(text.contains("Authors: A; B"));
        assert!(text.contains("Source:  https://e/b"));
        assert!(text.contains("--- LICENSE-MIT ---\nMIT text\n"));
        assert!(text.contains("--- alpha.txt ---\nfallback text\n"));
        assert!(!text.contains("ignored"));
        assert!(text.ends_with("\n"));
    }

    #[test]
    fn identical_texts_are_printed_once() {
        let t = fixture();
        t.write("alpha/LICENSE-APACHE", "MIT text");
        let meta = json::parse(&metadata(&t.0)).unwrap();
        let text = render(&shipped_packages(&meta).unwrap(), &t.0.join("lic")).unwrap();
        assert!(text.contains("(identical to LICENSE-APACHE of alpha_core 2.0.0)"));
    }

    #[test]
    fn a_crate_without_licence_text_fails_the_run() {
        let t = fixture();
        let meta = json::parse(&metadata(&t.0)).unwrap();
        let problems = render(&shipped_packages(&meta).unwrap(), &t.0.join("nowhere")).unwrap_err();
        assert_eq!(problems, ["alpha_core 2.0.0: no licence text in the source"]);
    }

    #[test]
    fn a_crate_without_declared_licence_fails_the_run() {
        let t = fixture();
        let md = metadata(&t.0).replace(r#""license":"MIT","repository""#, r#""repository""#);
        let meta = json::parse(&md).unwrap();
        let problems = render(&shipped_packages(&meta).unwrap(), &t.0.join("lic")).unwrap_err();
        assert_eq!(problems, ["beta 1.0.0: no declared licence"]);
    }

    #[test]
    fn missing_root_package_is_an_error() {
        let meta = json::parse(r#"{"packages":[],"resolve":{"nodes":[]}}"#).unwrap();
        assert!(shipped_packages(&meta).is_err());
    }

    #[test]
    fn run_reads_metadata_from_stdin_and_writes_the_file() {
        let t = fixture();
        let out = t.0.join("out/NOTICES.txt");
        let args: Vec<String> = ["--metadata", "-", "--root"]
            .iter()
            .map(|s| (*s).to_owned())
            .chain([
                t.0.to_string_lossy().into_owned(),
                "--out".to_owned(),
                out.to_string_lossy().into_owned(),
            ])
            .collect();
        t.write("tools/third_party_licenses/alpha.txt", "fallback");
        let mut input = std::io::Cursor::new(metadata(&t.0).into_bytes());
        run(&args, &mut input).unwrap();
        assert!(fs::read_to_string(&out).unwrap().contains("alpha_core 2.0.0"));
    }
}
