// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Build and release helpers of the Arca repository, in Rust so that the build chain needs no
//! scripting language. One binary, `arca-devtools`, with a subcommand per job:
//!
//! | subcommand        | job                                                                    |
//! |-------------------|------------------------------------------------------------------------|
//! | `wasm-imports`    | the Wasm guest imports nothing; prints its SHA-256                      |
//! | `elf-check`       | a `.so` has 16 KiB-aligned `PT_LOAD` segments for the right machine     |
//! | `third-party`     | generates `THIRD_PARTY_NOTICES.txt` from the dependency graph           |
//! | `check-resources` | Android resources and manifest: parity, references, manifest policy    |
//! | `gen-r`           | a stand-in `R.java` for compile checks without the Android build tools |
//! | `check-lockfile`  | Cargo.lock lists only checksummed crates.io packages                    |
//! | `check-compat`    | newer-API errors only inside the `ApiNN` classes of `Compat.java`       |
//! | `json-get`        | prints a top-level string member of the JSON on standard input          |
//!
//! The crate is a workspace member only. Nothing depends on it, so it is never linked into the
//! application or into a Wasm guest.

#![forbid(unsafe_code)]

pub mod compat;
pub mod dom;
pub mod elf;
pub mod genr;
pub mod json;
pub mod lockfile;
pub mod resources;
pub mod third_party;
pub mod util;
pub mod wasm;

use std::io::Read;
use std::process::ExitCode;

use util::{say, warn};

const USAGE: &str = "\
usage: arca-devtools <command> [args]

commands:
  wasm-imports <file.wasm>
  elf-check <library.so> [--align N] [--machine aarch64|arm|x86_64|x86]
  third-party [--target T] [--out FILE] [--root DIR] [--metadata FILE|-]
  check-resources <android-dir> [--android-jar PATH] [--repo-root DIR]
  gen-r <res-dir> <package> <out-dir>
  check-lockfile [Cargo.lock]
  check-compat <Compat.java> <javac.log>
  json-get <member>              (JSON on standard input)";

/// Runs one command line; the exit status is 0 on success, 1 on a failed check, 2 on misuse.
pub fn run(args: &[String], stdin: &mut dyn Read) -> ExitCode {
    let Some((command, rest)) = args.split_first() else {
        warn(USAGE);
        return ExitCode::from(2);
    };
    let result = match command.as_str() {
        "wasm-imports" => wasm::run(rest),
        "elf-check" => elf::run(rest),
        "third-party" => third_party::run(rest, stdin),
        "check-resources" => resources::run(rest),
        "gen-r" => genr::run(rest),
        "check-lockfile" => lockfile::run(rest),
        "check-compat" => compat::run(rest),
        "json-get" => json::run_get(rest, stdin),
        "help" | "--help" | "-h" => {
            say(USAGE);
            return ExitCode::SUCCESS;
        }
        other => {
            warn(&format!("arca-devtools: unknown command {other}\n{USAGE}"));
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            warn(&format!("arca-devtools {command}: {message}"));
            ExitCode::FAILURE
        }
    }
}
