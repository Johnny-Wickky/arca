//! Dumps the pages of a worksheet as SVG files for looking at them (developer tool).
//!
//! `cargo run -p arca-preview --example sheet_svg -- book.xlsx out_dir [sheet]`
//!
//! Writes `out_dir/sheet<N>-page<M>.svg` for every page of the sheet (all sheets if none is
//! named) and prints the notices.

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use arca_preview::PreviewPolicy;
use arca_preview::page::to_svg;
use arca_preview::sheet_draw::{page_count, render_sheet_page, sheet_list};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(input), Some(dir)) = (args.first(), args.get(1)) else {
        eprintln!("usage: sheet_svg <book> <out_dir> [sheet]");
        return;
    };
    let only: Option<usize> = args.get(2).and_then(|s| s.parse().ok());
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return;
        }
    };
    let policy = PreviewPolicy::default();
    let list = match sheet_list(&data, &policy) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{input}: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("{dir}: {e}");
        return;
    }
    for (i, info) in list.sheets.iter().enumerate() {
        if only.is_some_and(|o| o != i) {
            continue;
        }
        let pages = page_count(&data, i, &policy).unwrap_or(0);
        println!("sheet {i} {:?}: {pages} page(s)", info.name);
        // A sheet can print on thousands of pages; a look at the first few is enough.
        for p in 0..usize::try_from(pages).unwrap_or(0).min(20) {
            match render_sheet_page(&data, i, p, &policy) {
                Ok(page) => {
                    for n in &page.notices {
                        println!("  notice {:?} {} {}", n.kind, n.count, n.detail);
                    }
                    let path = format!("{dir}/sheet{i}-page{p}.svg");
                    if let Err(e) = std::fs::write(&path, to_svg(&page.page)) {
                        eprintln!("{path}: {e}");
                    }
                }
                Err(e) => eprintln!("sheet {i} page {p}: {e}"),
            }
        }
    }
}
