//! Builds a sterile OOXML copy of a legacy Office document or an RTF file (developer tool).
//!
//! `cargo run -p arca-preview --example legacyclean -- in.doc out.docx`

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use arca_preview::{LegacyOptions, sanitize_legacy};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: legacyclean <in> <out>");
        return;
    };
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return;
        }
    };
    match sanitize_legacy(&data, &LegacyOptions::default()) {
        Ok((bytes, format, actions)) => {
            for a in &actions {
                println!("{:?}: {}", a.kind, a.target);
            }
            println!("{format:?}: {} -> {} bytes", data.len(), bytes.len());
            if let Err(e) = std::fs::write(output, bytes) {
                eprintln!("{output}: {e}");
            }
        }
        Err(e) => println!("ERROR: {e}"),
    }
}
