//! Resource measurement of the real guest: fuel, peak memory and wall time per
//! input.
//!
//! ```text
//! ARCA_GUEST_XRAY_WASM=target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm \
//!     cargo run --release -p arca-guest-xray --example measure -- file1.xlsx file2.docx
//! ```
//!
//! The fuel budget is lifted to the policy ceiling so the *real* cost is
//! visible; compare `fuel/byte` with `FuelPolicy::per_input_byte`.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "command-line measurement tool")]

use std::process::ExitCode;
use std::time::Instant;

use arca_abi::request::{Op, flags, header};
use arca_sandbox::{GuestModule, RunOptions, Sandbox, SandboxPolicy};

fn main() -> ExitCode {
    let Some(path) = std::env::var_os("ARCA_GUEST_XRAY_WASM") else {
        eprintln!("set ARCA_GUEST_XRAY_WASM to the built guest module");
        return ExitCode::from(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path:?}: {e}");
            return ExitCode::from(2);
        }
    };
    let module = GuestModule::new("arca-guest-xray", bytes);
    let mut policy = SandboxPolicy::default();
    policy.fuel.base = policy.fuel.max;
    let sandbox = Sandbox::new(policy, [module.pin()]);
    let policy_per_byte = SandboxPolicy::default().fuel.per_input_byte;

    println!(
        "{:<34} {:>11} {:>14} {:>9} {:>10} {:>9} {:>9} verdict",
        "file", "bytes", "fuel", "fuel/B", "peak MiB", "guest ms", "native ms"
    );
    for arg in std::env::args_os().skip(1) {
        let data = match std::fs::read(&arg) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("cannot read {arg:?}: {e}");
                continue;
            }
        };
        let name = std::path::Path::new(&arg)
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let t = Instant::now();
        let native = arca_xray::scan_default(&data);
        let native_ms = t.elapsed().as_millis();
        for op in [Op::Scan, Op::Preview, Op::Sanitize] {
            let op_flags = match op {
                Op::Preview => flags::REVEAL_HIDDEN,
                Op::Sanitize => flags::SANITIZE_ALL,
                Op::Scan => 0,
            };
            match sandbox.run_request(&module, &header(op, op_flags), &data, &RunOptions::new()) {
                Ok(out) => {
                    let per_byte =
                        out.stats.fuel_consumed / u64::try_from(data.len().max(1)).unwrap_or(1);
                    let flag = if per_byte > policy_per_byte { "  <-- over budget" } else { "" };
                    println!(
                        "{:<34} {:>11} {:>14} {:>9} {:>10.1} {:>9} {:>9} {:?}{flag}",
                        format!("{name} [{op:?}]"),
                        data.len(),
                        out.stats.fuel_consumed,
                        per_byte,
                        out.stats.peak_memory_bytes as f64 / 1_048_576.0,
                        out.stats.wall_time.as_millis(),
                        native_ms,
                        native.verdict,
                    );
                }
                Err(e) => println!("{name:<34} {:>11}  FAILED: {e}", data.len()),
            }
        }
    }
    ExitCode::SUCCESS
}
