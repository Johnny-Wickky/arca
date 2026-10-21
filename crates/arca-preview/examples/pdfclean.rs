//! Builds a sterile copy of a PDF (developer tool).
//!
//! `cargo run -p arca-preview --example pdfclean -- in.pdf out.pdf`

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use arca_preview::{PdfSanitizeOptions, sanitize_pdf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: pdfclean <in.pdf> <out.pdf>");
        return;
    };
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return;
        }
    };
    match sanitize_pdf(&data, &PdfSanitizeOptions::default()) {
        Ok((bytes, actions)) => {
            for a in &actions {
                println!("{:?}: {}", a.kind, a.target);
            }
            println!("{} -> {} bytes", data.len(), bytes.len());
            if let Err(e) = std::fs::write(output, bytes) {
                eprintln!("{output}: {e}");
            }
        }
        Err(e) => println!("ERROR: {e}"),
    }
}
