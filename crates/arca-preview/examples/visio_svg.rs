//! Draws the pages of a Visio drawing (`.vsdx` family or `.vsd`) as SVG images (developer tool,
//! to look at what the page model holds).
//!
//! `cargo run -p arca-preview --example visio_svg -- drawing.vsdx out_prefix [index]`
//!
//! Writes `out_prefix-<n>.svg` for the page `index` (from 0), or for every page, and prints the
//! size, the number of primitives and whether the page was cut short or approximate.

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use arca_preview::page::{Prim, to_svg};
use arca_preview::{PreviewPolicy, render_page};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, prefix, only) = match args.as_slice() {
        [i, p] => (i, p, None),
        [i, p, n] => (i, p, n.parse::<usize>().ok()),
        _ => {
            eprintln!("usage: visio_svg <drawing> <out_prefix> [index]");
            return;
        }
    };
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return;
        }
    };
    let policy = PreviewPolicy::default();
    let mut index = only.unwrap_or(0);
    loop {
        match render_page(&data, index, &policy) {
            Ok(page) => {
                let texts = page.prims.iter().filter(|p| matches!(p, Prim::Text { .. })).count();
                println!(
                    "page {}/{}: {}x{} pt, {} primitives ({} text), truncated={}, approximate={}",
                    index + 1,
                    page.pages,
                    page.width,
                    page.height,
                    page.prims.len(),
                    texts,
                    page.truncated,
                    page.approximate
                );
                let out = format!("{prefix}-{}.svg", index + 1);
                if let Err(e) = std::fs::write(&out, to_svg(&page)) {
                    eprintln!("{out}: {e}");
                    return;
                }
                if only.is_some() || index + 1 >= page.pages as usize {
                    return;
                }
                index += 1;
            }
            Err(e) => {
                println!("ERROR: {e}");
                return;
            }
        }
    }
}
