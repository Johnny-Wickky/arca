// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Developer CLI for the Threat X-Ray (not shipped in the app).
//!
//! ```text
//! cargo run -p arca-xray --example xray -- scan     <file>...
//! cargo run -p arca-xray --example xray -- sanitize <in> <out>
//! ```
//!
//! The CLI runs the scanner natively for convenience; the application runs
//! it inside the Wasm sandbox (see `arca-guest-xray`).

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "command-line developer tool")]

use std::process::ExitCode;

use arca_xray::sanitize::{SanitizeOptions, sanitize};
use arca_xray::{ScanPolicy, XRayReport, scan};

fn print_report(path: &str, r: &XRayReport) {
    println!("== {path}");
    println!("   format   : {:?} ({:?})", r.format, r.format.family());
    println!(
        "   verdict  : {:?}{}",
        r.verdict,
        if r.rejection.is_empty() { String::new() } else { format!(" — {}", r.rejection) }
    );
    println!("   sha256   : {}", r.sha256);
    println!(
        "   stats    : {} parts, {} XML events, {} bytes inflated",
        r.stats.parts_scanned, r.stats.xml_events, r.stats.bytes_inflated
    );
    for f in &r.findings {
        println!("   [{:<8?}] {:<26} {}  ({})", f.severity, f.kind.id(), f.detail, f.location);
    }
    for m in &r.macros {
        println!(
            "   macro {:?} {:?} {} bytes, auto={:?} suspicious={:?} iocs={:?}",
            m.language,
            m.name,
            m.source.len(),
            m.auto_exec,
            m.suspicious,
            m.iocs
        );
    }
    for e in &r.external_refs {
        println!("   external {:?}/{:?} auto={} {}", e.kind, e.class, e.auto_load, e.target);
    }
    for m in &r.metadata {
        let name = if m.name.is_empty() { String::new() } else { format!("[{}]", m.name) };
        println!("   meta {:?}{name} = {}", m.key, m.value);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let policy = ScanPolicy::default();
    match args.split_first() {
        Some((cmd, files)) if cmd == "scan" && !files.is_empty() => {
            let mut worst = 0u8;
            for f in files {
                match std::fs::read(f) {
                    Ok(data) => {
                        let r = scan(&data, &policy);
                        print_report(f, &r);
                        if r.verdict == arca_xray::Verdict::Rejected {
                            worst = worst.max(2);
                        }
                    }
                    Err(e) => {
                        eprintln!("{f}: {e}");
                        worst = worst.max(1);
                    }
                }
            }
            ExitCode::from(worst)
        }
        Some((cmd, rest)) if cmd == "sanitize" && rest.len() == 2 => {
            let (Some(input), Some(output)) = (rest.first(), rest.get(1)) else {
                return ExitCode::from(64);
            };
            let data = match std::fs::read(input) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{input}: {e}");
                    return ExitCode::from(1);
                }
            };
            match sanitize(&data, &policy, &SanitizeOptions::default()) {
                Ok(doc) => {
                    for a in &doc.actions {
                        println!("   {:?}: {}", a.kind, a.target);
                    }
                    if let Err(e) = std::fs::write(output, &doc.bytes) {
                        eprintln!("{output}: {e}");
                        return ExitCode::from(1);
                    }
                    print_report(output, &doc.verification);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{input}: {e}");
                    ExitCode::from(2)
                }
            }
        }
        _ => {
            eprintln!("usage: xray scan <file>... | xray sanitize <in> <out>");
            ExitCode::from(64)
        }
    }
}
