//! The reading view of a drawing read into the shape model: the text of every shape in reading
//! order (top to bottom, left to right) with groups kept together, shape data as "label: value",
//! hyperlinks as plain text, a marker for every picture and the connections as "A → B" lines.
//!
//! Lines the viewer makes up (a connection, the name of a shape without text) carry
//! [`style::SYNTHETIC`], so that a shape whose own text reads "A → B" cannot pass for a
//! connection. Nothing is followed or fetched.

use std::collections::BTreeMap;

use super::model::{Connect, Doc, Kind, PageDef, Sheet};
use crate::model::{Block, ImageInfo, ParaStyle, Paragraph, Run, SectionKind, style};
use crate::sink::{SectionId, Sink};

/// Deepest group nesting that is walked.
const MAX_DEPTH: usize = 64;
/// Most labels collected for connections.
const MAX_LABELS: usize = 100_000;

/// What a shape is called when something refers to it: its first line of text, else its name.
fn label_of(s: &Sheet) -> String {
    let text = s.plain_text();
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let label = if line.is_empty() { s.name.trim() } else { line };
    let label: String = label.chars().take(60).collect();
    if label.is_empty() { format!("#{}", s.id) } else { label }
}

fn collect_labels(shapes: &[Sheet], out: &mut BTreeMap<u32, String>, depth: usize) {
    for s in shapes {
        if out.len() < MAX_LABELS {
            out.entry(s.id).or_insert_with(|| label_of(s));
        }
        if depth < MAX_DEPTH {
            collect_labels(&s.kids, out, depth + 1);
        }
    }
}

/// Reading order: top to bottom (Visio's y axis points up), then left to right.
fn reading_order(shapes: &[Sheet]) -> Vec<&Sheet> {
    let mut v: Vec<&Sheet> = shapes.iter().collect();
    #[allow(clippy::cast_possible_truncation)]
    let key = |s: &&Sheet| {
        let y = s.num("PinY").unwrap_or(0.0);
        let x = s.num("PinX").unwrap_or(0.0);
        (((y * 20.0).round() as i64).saturating_neg(), (x * 20.0).round() as i64, s.id)
    };
    v.sort_by_key(key);
    v
}

fn emit_shapes(
    shapes: &[Sheet],
    sec: SectionId,
    sink: &mut Sink<'_>,
    depth: usize,
    ordered: bool,
) -> bool {
    let list: Vec<&Sheet> = if ordered { reading_order(shapes) } else { shapes.iter().collect() };
    for s in list {
        let level = u8::try_from(depth.min(6)).unwrap_or(6);
        let all = s.plain_text();
        let text = all.trim();
        let href = s.links.first().cloned().unwrap_or_default();
        if !text.is_empty() {
            for line in text
                .split(['\n', '\r', '\u{2028}', '\u{b}'])
                .map(str::trim)
                .filter(|l| !l.is_empty())
            {
                let p = Paragraph {
                    level,
                    runs: vec![Run { text: line.to_owned(), style: 0, href: href.clone() }],
                    ..Paragraph::default()
                };
                if !sink.para(sec, p) {
                    return false;
                }
            }
        } else if !s.data.is_empty() || !s.links.is_empty() {
            let p = Paragraph {
                level,
                runs: vec![Run {
                    text: label_of(s),
                    style: style::ITALIC | style::SYNTHETIC,
                    href: href.clone(),
                }],
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                return false;
            }
        }
        for (label, value) in &s.data {
            let mut runs = Vec::new();
            if !label.is_empty() {
                runs.push(Run {
                    text: format!("{label}: "),
                    style: style::BOLD,
                    href: String::new(),
                });
            }
            runs.push(Run { text: value.clone(), style: 0, href: String::new() });
            let p = Paragraph { style: ParaStyle::Note, level, runs, ..Paragraph::default() };
            if !sink.para(sec, p) {
                return false;
            }
        }
        // Hyperlinks beyond the first (the first rides on the shape's text).
        for link in s.links.iter().skip(usize::from(!text.is_empty())) {
            let p = Paragraph {
                style: ParaStyle::Note,
                level,
                runs: vec![Run {
                    text: link.clone(),
                    style: style::SYNTHETIC,
                    href: String::new(),
                }],
                ..Paragraph::default()
            };
            if !sink.para(sec, p) {
                return false;
            }
        }
        if s.kind == Kind::Foreign || s.foreign.is_some() {
            let alt = if text.is_empty() { s.name.clone() } else { label_of(s) };
            let media = s.foreign.clone().unwrap_or_default();
            let info =
                ImageInfo { alt: alt.chars().take(60).collect(), media, ..ImageInfo::default() };
            if !sink.image(sec, info) {
                return false;
            }
        }
        if depth < MAX_DEPTH && !emit_shapes(&s.kids, sec, sink, depth + 1, true) {
            return false;
        }
    }
    true
}

fn emit_connections(
    connects: &[Connect],
    labels: &BTreeMap<u32, String>,
    sec: SectionId,
    sink: &mut Sink<'_>,
) {
    let mut ends: BTreeMap<u32, (Option<u32>, Option<u32>)> = BTreeMap::new();
    for c in connects {
        let e = ends.entry(c.from).or_default();
        if c.begin {
            e.0.get_or_insert(c.to);
        } else {
            e.1.get_or_insert(c.to);
        }
    }
    let name = |id: Option<u32>| {
        id.map_or_else(
            || "?".to_owned(),
            |i| labels.get(&i).cloned().unwrap_or_else(|| format!("#{i}")),
        )
    };
    let mut first = true;
    for (begin, end) in ends.values() {
        if begin.is_none() && end.is_none() {
            continue;
        }
        if first {
            first = false;
            if !sink.push(sec, Block::Rule) {
                return;
            }
        }
        let p = Paragraph {
            runs: vec![Run {
                text: format!("{} → {}", name(*begin), name(*end)),
                style: style::SYNTHETIC,
                href: String::new(),
            }],
            ..Paragraph::default()
        };
        if !sink.para(sec, p) {
            return;
        }
    }
}

/// Writes one page into the reading view.
pub(crate) fn emit_page(page: &PageDef, title: &str, sink: &mut Sink<'_>) -> bool {
    let Some(sec) = sink.open_section(SectionKind::Page, title, false) else { return false };
    let mut labels = BTreeMap::new();
    collect_labels(&page.shapes, &mut labels, 0);
    // The page lists its shapes in the order of the file when it has no positions to sort by.
    let ordered = page.shapes.iter().any(|s| s.num("PinX").is_some() || s.num("PinY").is_some());
    let ok = emit_shapes(&page.shapes, sec, sink, 0, ordered);
    if ok {
        emit_connections(&page.connects, &labels, sec, sink);
    }
    sink.drop_if_empty(sec);
    ok
}

/// Writes every page of the drawing.
pub(crate) fn emit_doc(doc: &Doc, sink: &mut Sink<'_>) {
    for (i, page) in doc.pages.iter().enumerate() {
        if sink.is_full() {
            break;
        }
        let name =
            if page.name.trim().is_empty() { format!("Page-{}", i + 1) } else { page.name.clone() };
        let title = if page.background { format!("{name} (background)") } else { name };
        if !emit_page(page, &title, sink) {
            break;
        }
    }
}
