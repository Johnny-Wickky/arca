//! Prints a plain-text rendering of a document preview (developer tool).
//!
//! `cargo run -p arca-preview --example dump -- file.docx`

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use arca_preview::model::{Block, Paragraph, SectionKind};
use arca_preview::{PreviewPolicy, preview};

fn para(p: &Paragraph) -> String {
    let mut s = String::new();
    if !p.marker.is_empty() {
        s.push_str(&"  ".repeat(usize::from(p.level.saturating_sub(1))));
        s.push_str(&p.marker);
        s.push(' ');
    }
    for r in &p.runs {
        let mut t = r.text.clone();
        if r.style & arca_preview::model::style::BOLD != 0 {
            t = format!("**{t}**");
        }
        if r.style & arca_preview::model::style::ITALIC != 0 {
            t = format!("_{t}_");
        }
        if r.style & arca_preview::model::style::HIDDEN != 0 {
            t = format!("[hidden:{t}]");
        }
        if r.style & arca_preview::model::style::DELETED != 0 {
            t = format!("[-{t}-]");
        }
        if !r.href.is_empty() {
            t = format!("{t} <{}>", r.href);
        }
        s.push_str(&t);
    }
    format!("{:?}| {s}", p.style)
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump <file>");
        return;
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return;
        }
    };
    let started = std::time::Instant::now();
    match preview(&data, &PreviewPolicy::default()) {
        Err(e) => println!("ERROR: {e}"),
        Ok(p) => {
            println!(
                "format={:?} sections={} stats={:?} ({:?})",
                p.format,
                p.sections.len(),
                p.stats,
                started.elapsed()
            );
            for n in &p.notices {
                println!("notice: {:?} count={} {}", n.kind, n.count, n.detail);
            }
            for s in &p.sections {
                println!(
                    "=== {:?} \"{}\"{}",
                    s.kind,
                    s.name,
                    if s.hidden { " (hidden)" } else { "" }
                );
                let _ = SectionKind::Body;
                for b in &s.blocks {
                    match b {
                        Block::Para(p) => println!("{}", para(p)),
                        Block::Table(t) => {
                            println!("[table {} rows]", t.rows.len());
                            for r in &t.rows {
                                let cells: Vec<String> = r
                                    .cells
                                    .iter()
                                    .map(|c| {
                                        c.paras
                                            .iter()
                                            .map(|p| p.text())
                                            .collect::<Vec<_>>()
                                            .join(" / ")
                                    })
                                    .collect();
                                println!("  | {}", cells.join(" | "));
                            }
                        }
                        Block::Grid(g) => {
                            println!(
                                "[grid cols={} rows={} truncated={}]",
                                g.cols,
                                g.rows.len(),
                                g.truncated
                            );
                            for r in g.rows.iter().take(40) {
                                let cells: Vec<String> = r
                                    .cells
                                    .iter()
                                    .map(|c| {
                                        if c.href.is_empty() {
                                            format!("{}:{}", c.col, c.text)
                                        } else {
                                            format!("{}:{} <{}>", c.col, c.text, c.href)
                                        }
                                    })
                                    .collect();
                                println!(
                                    "  {:>4}{}| {}",
                                    r.index,
                                    if r.hidden { "h" } else { " " },
                                    cells.join(" | ")
                                );
                            }
                        }
                        Block::Image(i) => println!(
                            "[image {:?} {} {}B external={}]",
                            i.alt, i.media, i.bytes, i.external
                        ),
                        Block::Rule => println!("-----"),
                    }
                }
            }
        }
    }
}
