// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Build-time enforcement of the attribution notice.
//!
//! 1. Recomputes the fingerprint of `src/upstream.rs` and compares it with
//!    `UPSTREAM_FINGERPRINT`; any difference stops the build.
//! 2. Validates the fork attribution file — `$ARCA_FORK_ATTRIBUTION` if set,
//!    otherwise `fork_attribution.toml` in the workspace root — with the very
//!    same parser the library uses, and embeds it.
//! 3. With `ARCA_RELEASE=1`, refuses placeholder URLs.
//!
//! The library sources are shared through `#[path]`, so the build script and
//! the runtime can never disagree about the rules.

#![allow(clippy::print_stdout, reason = "the cargo build-script protocol is stdout")]

use std::fmt;
use std::path::{Path, PathBuf};

#[allow(dead_code, reason = "shared with the library; only part of it is needed here")]
#[path = "src/upstream.rs"]
mod upstream;

#[allow(dead_code, reason = "shared with the library; only part of it is needed here")]
#[path = "src/fork.rs"]
mod fork;

#[allow(dead_code, reason = "shared with the library; only part of it is needed here")]
#[path = "src/integrity.rs"]
mod integrity;

/// Error shown verbatim by cargo (the `Debug` impl prints the message as is).
struct BuildError(String);

impl fmt::Debug for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\n\n")?;
        f.write_str(&self.0)?;
        f.write_str("\n")
    }
}

impl<E: std::error::Error> From<E> for BuildError {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

fn env_path(name: &str) -> Result<PathBuf, BuildError> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| BuildError(format!("{name} is not set")))
}

fn check_fingerprint(manifest_dir: &Path) -> Result<String, BuildError> {
    let pin_path = manifest_dir.join("UPSTREAM_FINGERPRINT");
    println!("cargo::rerun-if-changed={}", pin_path.display());
    let pinned = std::fs::read_to_string(&pin_path)
        .map_err(|e| BuildError(format!("cannot read {}: {e}", pin_path.display())))?;
    let pinned = pinned.trim().to_ascii_lowercase();
    let computed = integrity::upstream_fingerprint().to_string();
    if pinned != computed {
        return Err(BuildError(format!(
            "The Upstream Creator Attribution & Patronage Notice (crates/arca-attribution/src/upstream.rs)\n\
             does not match its pinned fingerprint.\n\
             \n    pinned:   {pinned}\n    computed: {computed}\n\n\
             Removing, hiding or replacing the original creators' attribution or donation links violates\n\
             LICENSE_ADDITIONAL_TERMS.md (AGPLv3 section 7(b)). Maintainers of a modified version add their\n\
             own names and donation links in fork_attribution.toml instead; both cards are shown together.\n\
             \n\
             If you are the original maintainers and changed your own entries on purpose, write the\n\
             computed value to crates/arca-attribution/UPSTREAM_FINGERPRINT."
        )));
    }
    Ok(computed)
}

fn main() -> Result<(), BuildError> {
    let manifest_dir = env_path("CARGO_MANIFEST_DIR")?;
    let out_dir = env_path("OUT_DIR")?;
    for f in ["build.rs", "src/upstream.rs", "src/fork.rs", "src/integrity.rs"] {
        println!("cargo::rerun-if-changed={}", manifest_dir.join(f).display());
    }
    println!("cargo::rerun-if-env-changed=ARCA_FORK_ATTRIBUTION");
    println!("cargo::rerun-if-env-changed=ARCA_RELEASE");

    let fingerprint = check_fingerprint(&manifest_dir)?;

    let fork_path = match std::env::var_os("ARCA_FORK_ATTRIBUTION") {
        Some(p) => Some(PathBuf::from(p)),
        None => {
            let p = manifest_dir.join("..").join("..").join("fork_attribution.toml");
            p.is_file().then_some(p)
        }
    };
    let (mut fork, mut fork_src) = (None, None);
    if let Some(path) = &fork_path {
        println!("cargo::rerun-if-changed={}", path.display());
        let src = std::fs::read_to_string(path)
            .map_err(|e| BuildError(format!("cannot read {}: {e}", path.display())))?;
        fork = fork::parse(&src).map_err(|e| BuildError(format!("{}: {e}", path.display())))?;
        if fork.is_some() {
            fork_src = Some(src);
        }
    }

    if std::env::var_os("ARCA_RELEASE").is_some_and(|v| v == "1") {
        let blockers = integrity::release_blockers(fork.as_ref());
        if !blockers.is_empty() {
            let list: Vec<String> = blockers.iter().map(|b| format!("  - {b}")).collect();
            return Err(BuildError(format!(
                "ARCA_RELEASE=1, but the attribution is not ready for a public release:\n{}",
                list.join("\n")
            )));
        }
    }

    let mut generated = String::from(
        "/// The validated `fork_attribution.toml` embedded at build time; `None` for the original project.\n",
    );
    match &fork_src {
        Some(src) => {
            std::fs::write(out_dir.join("fork_attribution.toml"), src)?;
            generated.push_str(
                "pub const FORK_ATTRIBUTION_TOML: Option<&str> = \
                 Some(include_str!(concat!(env!(\"OUT_DIR\"), \"/fork_attribution.toml\")));\n",
            );
        }
        None => generated.push_str("pub const FORK_ATTRIBUTION_TOML: Option<&str> = None;\n"),
    }
    generated.push_str("/// SHA-256 of the upstream notice as pinned in `UPSTREAM_FINGERPRINT` (verified at build time).\n");
    generated.push_str(&format!("pub const UPSTREAM_FINGERPRINT_PIN: &str = \"{fingerprint}\";\n"));
    std::fs::write(out_dir.join("attribution_config.rs"), generated)?;
    Ok(())
}
