//! Painting one printed page of a sheet as a list of primitives.
//!
//! The order is the one a spreadsheet prints in: gridlines, fills (which cover the gridlines
//! under them), borders, then text. Coordinates in a sheet are in points at 100 %; the page
//! scale (print scale or fit to page) is applied when they are written.

use std::collections::BTreeMap;

use super::model::{
    HAlign, LINE, MAX_PAGES, Pages, Sheet, Styles, VAlign, PAD_X, PAD_Y, f32_of,
};
use super::text;
use crate::model::CellKind;
use crate::page::{MAX_PATH_NUMBERS, MAX_PRIMS, Prim, op, style};
use crate::sink::{CleanCounts, clean_text};

/// Most cell records examined for one page.
const MAX_WORK: usize = 6_000_000;
/// Most cells (with merged ranges) painted on one page.
const MAX_ITEMS: usize = 200_000;
/// Longest text drawn for a cell that does not wrap, in characters: more cannot be seen on a
/// page.
const MAX_LINE_CHARS: usize = 2_000;
/// Gridline colour and width.
const GRID: u32 = 0xFFD0_D0D0;
const GRID_WIDTH: f32 = 0.5;
/// How far a fill reaches over the gridline at its edge, in points.
const BLEED: f64 = 0.3;

/// What was painted.
pub(super) struct Painted {
    pub(super) prims: Vec<Prim>,
    pub(super) truncated: bool,
    pub(super) approximate: bool,
}

struct Out {
    prims: Vec<Prim>,
    full: bool,
}

impl Out {
    fn push(&mut self, p: Prim) {
        if self.prims.len() >= MAX_PRIMS {
            self.full = true;
        } else {
            self.prims.push(p);
        }
    }

    fn rect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, fill: u32) {
        let (a, b, c, d) = (f32_of(x0), f32_of(y0), f32_of(x1), f32_of(y1));
        self.push(Prim::Path {
            ops: vec![op::MOVE, a, b, op::LINE, c, b, op::LINE, c, d, op::LINE, a, d, op::CLOSE],
            fill,
            stroke: 0,
            width: 0.0,
            even_odd: false,
        });
    }

    /// Straight segments `[x0, y0, x1, y1]` in one or more paths.
    fn lines(&mut self, segs: &[[f64; 4]], stroke: u32, width: f32) {
        let mut ops: Vec<f32> = Vec::new();
        for s in segs {
            if ops.len() + 6 > MAX_PATH_NUMBERS / 2 {
                self.flush_lines(std::mem::take(&mut ops), stroke, width);
            }
            let [x0, y0, x1, y1] = *s;
            ops.extend([op::MOVE, f32_of(x0), f32_of(y0), op::LINE, f32_of(x1), f32_of(y1)]);
        }
        self.flush_lines(ops, stroke, width);
    }

    fn flush_lines(&mut self, ops: Vec<f32>, stroke: u32, width: f32) {
        if !ops.is_empty() {
            self.push(Prim::Path { ops, fill: 0, stroke, width, even_odd: false });
        }
    }
}

/// A cell, or a merged range, to paint.
struct Item {
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rec: Option<usize>,
    /// The first cell is on this page (the text is drawn here).
    anchor_in: bool,
    merged: bool,
}

/// Quantised length for joining border segments: hundredths of a point.
#[allow(clippy::cast_possible_truncation)]
fn q(v: f64) -> i64 {
    (v * 100.0).round().clamp(-1e12, 1e12) as i64
}

#[allow(clippy::cast_precision_loss)]
fn unq(v: i64) -> f64 {
    v as f64 / 100.0
}

/// Border segments by (width, colour): horizontal `(y, x0, x1)` and vertical `(x, y0, y1)`.
#[derive(Default)]
struct Segs {
    h: Vec<(i64, i64, i64)>,
    v: Vec<(i64, i64, i64)>,
}

/// Sorts and joins segments that touch or overlap.
fn joined(mut v: Vec<(i64, i64, i64)>) -> Vec<(i64, i64, i64)> {
    v.sort_unstable();
    let mut out: Vec<(i64, i64, i64)> = Vec::with_capacity(v.len());
    for s in v {
        match out.last_mut() {
            Some(last) if last.0 == s.0 && s.1 <= last.2 + 1 => last.2 = last.2.max(s.2),
            _ => out.push(s),
        }
    }
    out
}

/// Whether text looks like a number, for the alignment of a calculated value.
fn numeric_like(s: &str) -> bool {
    let t = s.trim();
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| c.is_ascii_digit() || " .,+-%()$\u{20AC}\u{A3}\u{A5}:/eE\u{a0}".contains(c))
}

/// Paints page `p` of the sheet.
pub(super) fn paint(sheet: &Sheet, styles: &Styles, pages: &Pages, p: usize) -> Painted {
    let (cb, rb) = pages.band_of(p);
    let band_end = |starts: &[u32], i: usize, end: u32| -> u32 {
        starts.get(i + 1).copied().unwrap_or(end).saturating_sub(1)
    };
    let c1 = pages.col_starts.get(cb).copied().unwrap_or(0);
    let c2 = band_end(&pages.col_starts, cb, pages.col_end).max(c1);
    let r1 = pages.row_starts.get(rb).copied().unwrap_or(0);
    let r2 = band_end(&pages.row_starts, rb, pages.row_end).max(r1);
    let [ml, mt, _, _] = pages.margins;
    let scale = pages.scale;
    let x0 = sheet.cols.start(c1);
    let y0 = sheet.rows.start(r1);
    let x_end = sheet.cols.start(c2.saturating_add(1));
    let y_end = sheet.rows.start(r2.saturating_add(1));
    let px = |ux: f64| f64::from(ml) + (ux - x0) * scale;
    let py = |uy: f64| f64::from(mt) + (uy - y0) * scale;

    let mut out = Out { prims: Vec::new(), full: false };
    let mut truncated = pages.truncated || sheet.truncated;
    let mut counts = CleanCounts::default();
    let mut work = 0usize;

    // The cells to paint: merged ranges that reach this page, and the cells on it.
    let mut items: Vec<Item> = Vec::new();
    for m in &sheet.merges {
        work += 1;
        if m.r1 > r2 {
            break;
        }
        if m.r2 < r1 || m.c2 < c1 || m.c1 > c2 {
            continue;
        }
        if items.len() >= MAX_ITEMS {
            truncated = true;
            break;
        }
        items.push(Item {
            r1: m.r1,
            c1: m.c1,
            r2: m.r2,
            c2: m.c2,
            rec: sheet.rec_at(m.r1, m.c1),
            anchor_in: (r1..=r2).contains(&m.r1) && (c1..=c2).contains(&m.c1),
            merged: true,
        });
    }
    let mut si = sheet.rows_idx.partition_point(|s| s.row < r1);
    'rows: while let Some(span) = sheet.rows_idx.get(si) {
        if span.row > r2 {
            break;
        }
        si += 1;
        let Some(slice) = sheet.recs.get(span.from..span.to) else { continue };
        let first = slice.partition_point(|r| r.col < c1);
        work += 1;
        for (off, rec) in slice.iter().enumerate().skip(first) {
            if rec.col > c2 {
                break;
            }
            work += 1;
            if rec.covered || sheet.anchors.contains_key(&(rec.row, rec.col)) {
                continue;
            }
            if items.len() >= MAX_ITEMS || work > MAX_WORK {
                truncated = true;
                break 'rows;
            }
            items.push(Item {
                r1: rec.row,
                c1: rec.col,
                r2: rec.row,
                c2: rec.col,
                rec: Some(span.from + off),
                anchor_in: true,
                merged: false,
            });
        }
    }
    items.sort_by_key(|i| (i.r1, i.c1));

    // The page-space rectangle of an item, clipped to the band; `None` for a hidden one.
    let rect_of = |it: &Item| -> Option<(f64, f64, f64, f64)> {
        let ux0 = sheet.cols.start(it.c1).max(x0);
        let ux1 = sheet.cols.start(it.c2.saturating_add(1)).min(x_end);
        let uy0 = sheet.rows.start(it.r1).max(y0);
        let uy1 = sheet.rows.start(it.r2.saturating_add(1)).min(y_end);
        (ux1 > ux0 && uy1 > uy0).then(|| (px(ux0), py(uy0), px(ux1), py(uy1)))
    };

    // Gridlines.
    if sheet.gridlines || sheet.setup.print_grid {
        let mut segs: Vec<[f64; 4]> = Vec::new();
        let (top, bottom) = (py(y0), py(y_end));
        let (left, right) = (px(x0), px(x_end));
        let mut last = f64::NAN;
        for c in c1..=c2.saturating_add(1) {
            let x = px(sheet.cols.start(c));
            if x != last {
                segs.push([x, top, x, bottom]);
                last = x;
            }
        }
        last = f64::NAN;
        for r in r1..=r2.saturating_add(1) {
            let y = py(sheet.rows.start(r));
            if y != last {
                segs.push([left, y, right, y]);
                last = y;
            }
        }
        out.lines(&segs, GRID, GRID_WIDTH);
    }

    // Fills. Neighbouring cells of one colour in a row become one rectangle.
    let mut pend: Option<(f64, f64, f64, f64, u32)> = None;
    let flush = |out: &mut Out, pend: &mut Option<(f64, f64, f64, f64, u32)>| {
        if let Some((a, b, c, d, fill)) = pend.take() {
            out.rect(a - BLEED, b - BLEED, c + BLEED, d + BLEED, fill);
        }
    };
    for it in &items {
        let Some((a, b, c, d)) = rect_of(it) else { continue };
        let own = it
            .rec
            .and_then(|i| sheet.recs.get(i))
            .map_or(0, |r| styles.fill(styles.xf(r.xf).fill));
        let fill = if own >> 24 != 0 {
            own
        } else if it.merged {
            // A merged range shows no gridlines inside it.
            0xFFFF_FFFF
        } else {
            continue;
        };
        match pend {
            Some((pa, pb, pc, pd, pf))
                if !it.merged && pf == fill && (pb - b).abs() < 0.01 && (pd - d).abs() < 0.01 && (pc - a).abs() < 0.01 =>
            {
                pend = Some((pa, pb, c, pd, pf));
            }
            _ => {
                flush(&mut out, &mut pend);
                pend = if it.merged {
                    out.rect(a - BLEED, b - BLEED, c + BLEED, d + BLEED, fill);
                    None
                } else {
                    Some((a, b, c, d, fill))
                };
            }
        }
    }
    flush(&mut out, &mut pend);

    // Borders: segments of one width and colour are joined into long lines.
    let mut by_look: BTreeMap<(u32, u32), Segs> = BTreeMap::new();
    for it in &items {
        let Some(rec) = it.rec.and_then(|i| sheet.recs.get(i)) else { continue };
        let edges = styles.edges(styles.xf(rec.xf).border);
        if edges.iter().all(|e| e.width <= 0.0) {
            continue;
        }
        let Some((a, b, c, d)) = rect_of(it) else { continue };
        // An edge outside the band belongs to the page that holds it.
        let sides = [
            (it.c1 >= c1, 0usize),
            (it.r1 >= r1, 1),
            (it.c2 <= c2, 2),
            (it.r2 <= r2, 3),
        ];
        for (inside, k) in sides {
            let Some(e) = edges.get(k) else { continue };
            if !inside || e.width <= 0.0 {
                continue;
            }
            let segs = by_look.entry((e.width.to_bits(), e.color)).or_default();
            match k {
                0 => segs.v.push((q(a), q(b), q(d))),
                1 => segs.h.push((q(b), q(a), q(c))),
                2 => segs.v.push((q(c), q(b), q(d))),
                _ => segs.h.push((q(d), q(a), q(c))),
            }
        }
    }
    for ((w_bits, color), segs) in by_look {
        let width = f32::from_bits(w_bits);
        let half = f64::from(width) / 2.0;
        let mut lines: Vec<[f64; 4]> = Vec::new();
        for (y, a, b) in joined(segs.h) {
            lines.push([unq(a) - half, unq(y), unq(b) + half, unq(y)]);
        }
        for (x, a, b) in joined(segs.v) {
            lines.push([unq(x), unq(a) - half, unq(x), unq(b) + half]);
        }
        out.lines(&lines, color, width);
    }

    // Text.
    for it in &items {
        if !it.anchor_in {
            continue;
        }
        let Some(idx) = it.rec else { continue };
        let Some(rec) = sheet.recs.get(idx) else { continue };
        if rec.text.is_empty() {
            continue;
        }
        let ux0 = sheet.cols.start(it.c1);
        let ux1 = sheet.cols.start(it.c2.saturating_add(1));
        let uy0 = sheet.rows.start(it.r1);
        let uy1 = sheet.rows.start(it.r2.saturating_add(1));
        if ux1 <= ux0 || uy1 <= uy0 {
            continue;
        }
        let xf = styles.xf(rec.xf);
        let font = styles.font(xf.font);
        let size = f64::from(font.size);
        let indent = f64::from(xf.indent) * size * 0.9;
        let inner = f32_of((ux1 - ux0).min(1e6) - f64::from(2.0 * PAD_X) - indent);
        let lines: Vec<text::Line> = if xf.wrap {
            text::wrap(&rec.text, inner, &font).0
        } else {
            let flat: String = rec
                .text
                .chars()
                .take(MAX_LINE_CHARS)
                .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
                .collect();
            let width = text::width(&flat, &font);
            vec![text::Line { text: flat, width }]
        };
        let h = match xf.h {
            HAlign::General => match rec.kind {
                CellKind::Number | CellKind::Date => HAlign::Right,
                CellKind::Bool | CellKind::Error => HAlign::Center,
                CellKind::Calculated | CellKind::Formula if numeric_like(&rec.text) => HAlign::Right,
                _ => HAlign::Left,
            },
            other => other,
        };
        let widest = f64::from(lines.iter().map(|l| l.width).fold(0.0f32, f32::max));
        let fits = widest <= f64::from(inner);

        // Where the text may be seen: the cell, or for text that does not fit and does not
        // wrap, also the empty cells next to it.
        let mut ax0 = ux0.max(x0);
        let mut ax1 = ux1.min(x_end);
        if !xf.wrap && !fits && matches!(rec.kind, CellKind::Text | CellKind::Formula | CellKind::Calculated) {
            if h != HAlign::Right {
                let mut j = idx + 1;
                let mut stop = x_end;
                while let Some(n) = sheet.recs.get(j) {
                    work += 1;
                    if n.row != rec.row || work > MAX_WORK {
                        break;
                    }
                    if !n.covered && !n.text.is_empty() {
                        stop = sheet.cols.start(n.col);
                        break;
                    }
                    j += 1;
                }
                ax1 = stop.min(x_end).max(ax1);
            }
            if h != HAlign::Left {
                let mut j = idx;
                let mut stop = x0;
                while j > 0 {
                    j -= 1;
                    let Some(n) = sheet.recs.get(j) else { break };
                    work += 1;
                    if n.row != rec.row || work > MAX_WORK {
                        break;
                    }
                    if !n.covered && !n.text.is_empty() {
                        stop = sheet.cols.start(n.col.saturating_add(1));
                        break;
                    }
                }
                ax0 = stop.max(x0).min(ax0);
            }
        }
        let (cy0, cy1) = (uy0.max(y0), uy1.min(y_end));

        let line_h = size * f64::from(LINE);
        #[allow(clippy::cast_precision_loss)]
        let block = lines.len() as f64 * line_h;
        let top = match xf.v {
            VAlign::Top => uy0 + f64::from(PAD_Y),
            VAlign::Center => (uy0 + uy1) / 2.0 - block / 2.0,
            VAlign::Bottom => uy1 - f64::from(PAD_Y) - block,
        };
        let mut placed: Vec<(f64, f64, &text::Line)> = Vec::new();
        let mut needs_clip = top < cy0 - 0.01 || top + block > cy1 + 0.01;
        for (n, line) in lines.iter().enumerate() {
            let w = f64::from(line.width);
            let tx = match h {
                HAlign::Right => ux1 - f64::from(PAD_X) - w,
                HAlign::Center => (ux0 + ux1) / 2.0 - w / 2.0,
                _ => ux0 + f64::from(PAD_X) + indent,
            };
            if tx < ax0 - 0.01 || tx + w > ax1 + 0.01 {
                needs_clip = true;
            }
            #[allow(clippy::cast_precision_loss)]
            let base = top + n as f64 * line_h + size * 0.95;
            placed.push((tx, base, line));
        }
        if placed.iter().all(|&(tx, base, line)| {
            tx + f64::from(line.width) < x0 || tx > x_end || base < y0 || base - size > y_end
        }) {
            continue;
        }
        let mut bits = 0u8;
        if font.bold {
            bits |= style::BOLD;
        }
        if font.italic {
            bits |= style::ITALIC;
        }
        if font.mono {
            bits |= style::MONO;
        }
        if font.serif {
            bits |= style::SERIF;
        }
        if needs_clip {
            out.push(Prim::Save);
            out.push(Prim::Clip {
                x: f32_of(px(ax0)),
                y: f32_of(py(cy0)),
                w: f32_of((ax1 - ax0) * scale),
                h: f32_of((cy1 - cy0) * scale),
            });
        }
        for (tx, base, line) in placed {
            let cleaned = clean_text(&line.text, &mut counts);
            let flat: String = cleaned.chars().map(|c| if matches!(c, '\n' | '\t') { ' ' } else { c }).collect();
            if flat.is_empty() {
                continue;
            }
            let w = f64::from(line.width);
            out.push(Prim::Text {
                x: f32_of(px(tx)),
                y: f32_of(py(base)),
                size: f32_of(size * scale),
                angle: 0.0,
                width: f32_of(w * scale),
                style: bits,
                color: font.color,
                text: flat,
            });
            let thin = f32_of(size * scale / 14.0);
            if font.underline {
                let y = py(base + size * 0.12);
                out.lines(&[[px(tx), y, px(tx + w), y]], font.color, thin);
            }
            if font.strike {
                let y = py(base - size * 0.3);
                out.lines(&[[px(tx), y, px(tx + w), y]], font.color, thin);
            }
        }
        if needs_clip {
            out.push(Prim::Restore);
        }
    }
    if work > MAX_WORK {
        truncated = true;
    }
    let _ = MAX_PAGES;
    Painted {
        prims: out.prims,
        truncated: truncated || out.full,
        approximate: sheet.approximate || counts.total() > 0,
    }
}
