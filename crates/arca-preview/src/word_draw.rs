// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The page view of text documents (DOCX, ODT, RTF, DOC) and of binary presentations (PPT).
//!
//! These formats are read into the common [`Preview`](crate::model::Preview) first; this module
//! lays that text out on pages: A4 sheets for documents, 4:3 slides for presentations. It is a
//! layout of the *reading view*, so it shows what the viewer found in the file (colours, sizes,
//! indents, line spacing, rules down the margin, tables) but not what a word processor would do
//! with the file's own page settings, fonts and floating objects: line breaks fall where this
//! module's width estimates put them, and the page is marked approximate for the person.
//!
//! The output is the same inert drawing as every other page ([`PageModel`]): text runs with a
//! position and a width, rectangles and lines, and pictures as placeholders. Nothing is decoded
//! and nothing is loaded.
//!
//! Colours the document gives to text are used only where they stay readable on a white sheet:
//! white or nearly white text is drawn in the viewer's own colour, so a colour can decorate text
//! but never hide it.

#![allow(
    clippy::cast_possible_truncation,
    reason = "text widths are measured in f64 and drawn in f32 points"
)]

use std::collections::HashMap;

use crate::model::{
    Align, Block, ImageInfo, PageSetup, ParaStyle, Paragraph, Preview, Run, Section, SectionKind,
    TabStop, Table, style,
};
use crate::page::{PageModel, Prim, op, style as ps};
use crate::slide_draw::{RunStyle, text_width};
use crate::{PreviewError, PreviewPolicy};

/// A4 in points.
const A4: (f32, f32) = (595.0, 842.0);
/// Slides are laid out on a 4:3 sheet.
const SLIDE: (f32, f32) = (720.0, 540.0);
/// Margin of a text page.
const MARGIN: f32 = 56.0;
/// Margin of a slide.
const SLIDE_MARGIN: f32 = 40.0;
/// Size of body text in points.
const BODY: f32 = 11.0;
/// Pages laid out for one document.
const MAX_PAGES: usize = 400;
/// Colour of text and rules the document does not colour.
const INK: u32 = 0xFF00_0000;
/// Link colour.
const LINK: u32 = 0xFF1A_5A9E;
/// Text the document marks hidden, and text the viewer made up.
const DIM: u32 = 0xFF70_7070;
/// Tracked deletion.
const DELETED: u32 = 0xFFA8_201A;
/// Tracked insertion.
const INSERTED: u32 = 0xFF14_672F;
/// Cell padding in points.
const PAD: f32 = 4.0;
/// Placeholder size of a picture in the flow of the text (its real size is not read).
const PICTURE: (f32, f32) = (240.0, 150.0);

// ---------------------------------------------------------------------------------------------
// Display list of one line, relative to the line's top-left corner
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Deco {
    Text { x: f32, y: f32, size: f32, width: f32, bits: u8, color: u32, text: String },
    Rect { x: f32, y: f32, w: f32, h: f32, fill: u32, stroke: u32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, color: u32, width: f32 },
    Image { x: f32, y: f32, w: f32, h: f32, label: String, id: u32 },
}

impl Deco {
    fn emit(&self, dx: f32, dy: f32, out: &mut Vec<Prim>) {
        match self {
            Self::Text { x, y, size, width, bits, color, text } => out.push(Prim::Text {
                x: x + dx,
                y: y + dy,
                size: *size,
                angle: 0.0,
                width: *width,
                style: *bits,
                color: *color,
                text: text.clone(),
            }),
            Self::Rect { x, y, w, h, fill, stroke } => {
                let (x, y) = (x + dx, y + dy);
                out.push(Prim::Path {
                    ops: vec![
                        op::MOVE,
                        x,
                        y,
                        op::LINE,
                        x + w,
                        y,
                        op::LINE,
                        x + w,
                        y + h,
                        op::LINE,
                        x,
                        y + h,
                        op::CLOSE,
                    ],
                    fill: *fill,
                    stroke: *stroke,
                    width: 0.6,
                    even_odd: false,
                });
            }
            Self::Line { x1, y1, x2, y2, color, width } => out.push(Prim::Path {
                ops: vec![op::MOVE, x1 + dx, y1 + dy, op::LINE, x2 + dx, y2 + dy],
                fill: 0,
                stroke: *color,
                width: *width,
                even_odd: false,
            }),
            Self::Image { x, y, w, h, label, id } => out.push(Prim::Image {
                m: [*w, 0.0, 0.0, *h, x + dx, y + dy],
                label: label.clone(),
                id: *id,
            }),
        }
    }
}

/// One laid-out line: its height and what is drawn in it.
#[derive(Debug, Clone, Default)]
struct LineBox {
    h: f32,
    decos: Vec<Deco>,
}

/// A paragraph laid out for a given width.
#[derive(Debug, Default)]
struct ParaBox {
    lines: Vec<LineBox>,
    before: f32,
    after: f32,
    /// Left edge of the text and the border to draw down it or around it, if any.
    text_left: f32,
    border: u32,
    boxed: bool,
}

// ---------------------------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------------------------

/// A piece of text with the look it is drawn in.
#[derive(Debug, Clone)]
struct Piece {
    text: String,
    width: f32,
    size: f32,
    bits: u8,
    color: u32,
    bg: u32,
    underline: bool,
    strike: bool,
    space: bool,
    /// An explicit line break.
    br: bool,
    /// A tab (its width is set when the line is filled).
    tab: bool,
    /// Never merged with its neighbours: a tab with or without a leader.
    fixed: bool,
    /// Blank space before the text of a piece (a leader is dots right-aligned in its tab).
    gap: f32,
}

/// The luminance of `0xAARRGGBB`, 0 to 1.
fn luminance(argb: u32) -> f32 {
    let c = |shift: u32| ((argb >> shift) & 0xFF) as f32 / 255.0;
    0.2126 * c(16) + 0.7152 * c(8) + 0.0722 * c(0)
}

/// A colour the document asked for, made safe on a white sheet.
fn readable(argb: u32, fallback: u32) -> u32 {
    if argb == 0 || luminance(argb) > 0.72 { fallback } else { argb }
}

/// A highlight, dropped when the text over it would be unreadable.
fn highlight(bg: u32, text: u32) -> u32 {
    if bg == 0 {
        return 0;
    }
    let (a, b) = (luminance(bg), luminance(text));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    if (hi + 0.05) / (lo + 0.05) >= 3.0 { bg } else { 0 }
}

fn role_size(role: ParaStyle, base: f32) -> (f32, bool) {
    match role {
        ParaStyle::Title => (base * 2.0, true),
        ParaStyle::Subtitle => (base * 1.4, false),
        ParaStyle::Heading1 => (base * 1.8, true),
        ParaStyle::Heading2 => (base * 1.5, true),
        ParaStyle::Heading3 => (base * 1.3, true),
        ParaStyle::Heading4 => (base * 1.15, true),
        ParaStyle::Heading5 | ParaStyle::Heading6 => (base, true),
        ParaStyle::Footnote | ParaStyle::HeaderFooter => (base * 0.8, false),
        ParaStyle::Code => (base * 0.9, false),
        _ => (base, false),
    }
}

fn pieces(p: &Paragraph, base: f32) -> Vec<Piece> {
    let (para_size, para_bold) = role_size(p.style, base);
    let mut out = Vec::new();
    for run in &p.runs {
        push_run(run, p.style, para_size, para_bold, &mut out);
    }
    out
}

fn push_run(run: &Run, role: ParaStyle, para_size: f32, para_bold: bool, out: &mut Vec<Piece>) {
    let size = if run.scale == 0 {
        para_size
    } else {
        (para_size * f32::from(run.scale) / 100.0).clamp(4.0, 120.0)
    };
    let mono = run.style & style::MONO != 0 || role == ParaStyle::Code;
    let bold = para_bold || run.style & style::BOLD != 0;
    let italic = run.style & style::ITALIC != 0
        || role == ParaStyle::Quote
        || run.style & style::SYNTHETIC != 0;
    let mut color = readable(run.fg, INK);
    let mut underline = run.style & style::UNDERLINE != 0;
    let mut strike = run.style & style::STRIKE != 0;
    if !run.href.is_empty() {
        color = LINK;
        underline = true;
    }
    if run.style & (style::HIDDEN | style::SYNTHETIC) != 0 {
        color = DIM;
    }
    if run.style & style::DELETED != 0 {
        color = DELETED;
        strike = true;
    }
    if run.style & style::INSERTED != 0 {
        color = INSERTED;
        underline = true;
    }
    let shift = if run.style & (style::SUPER | style::SUB) != 0 { 0.65 } else { 1.0 };
    let size = size * shift;
    let mut bits = 0u8;
    if bold {
        bits |= ps::BOLD;
    }
    if italic {
        bits |= ps::ITALIC;
    }
    if mono {
        bits |= ps::MONO;
    }
    let rs = RunStyle {
        size: f64::from(size),
        bold,
        italic,
        mono,
        serif: false,
        underline,
        strike,
        color,
        shift: 0,
    };
    let bg = highlight(run.bg, color);
    let mut word = String::new();
    let flush = |word: &mut String, space: bool, out: &mut Vec<Piece>| {
        if word.is_empty() {
            return;
        }
        let text = std::mem::take(word);
        let width = text_width(&text, &rs) as f32;
        out.push(Piece {
            text,
            width,
            size,
            bits,
            color,
            bg,
            underline,
            strike,
            space,
            br: false,
            tab: false,
            fixed: false,
            gap: 0.0,
        });
    };
    let mut in_space = false;
    for c in run.text.chars() {
        if c == '\n' {
            flush(&mut word, in_space, out);
            out.push(Piece {
                text: String::new(),
                width: 0.0,
                size,
                bits,
                color,
                bg: 0,
                underline: false,
                strike: false,
                space: false,
                br: true,
                tab: false,
                fixed: false,
                gap: 0.0,
            });
            in_space = false;
            continue;
        }
        if c == '\t' {
            // The width of a tab is set when the line is filled, from the tab stops.
            flush(&mut word, in_space, out);
            in_space = false;
            out.push(Piece {
                text: String::new(),
                width: 0.0,
                size,
                bits,
                color,
                bg: 0,
                underline: false,
                strike: false,
                space: false,
                br: false,
                tab: true,
                fixed: true,
                gap: 0.0,
            });
            continue;
        }
        let ws = c == ' ' || c == '\u{a0}';
        if ws != in_space {
            flush(&mut word, in_space, out);
            in_space = ws;
        }
        word.push(c);
    }
    flush(&mut word, in_space, out);
}

// ---------------------------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------------------------

/// Where the next tab stop after `rel` (points from the left edge of the column) is, and what
/// kind it is: the paragraph's own stops, then a stop every half inch.
fn next_stop(tabs: &[TabStop], rel: f32) -> (f32, u8, u8) {
    let own =
        tabs.iter().filter(|t| t.kind < 4 && f32::from(t.pos) > rel + 0.5).min_by_key(|t| t.pos);
    match own {
        Some(t) => (f32::from(t.pos), t.kind, t.leader),
        None => (((rel / 36.0).floor() + 1.0) * 36.0, 0, 0),
    }
}

/// The width of the piece of text that follows a tab, up to the next tab or line break; for a
/// decimal stop only the part before the first point or comma.
fn after_tab(rest: &[Piece], decimal: bool) -> f32 {
    let mut w = 0.0f32;
    for p in rest {
        if p.tab || p.br {
            break;
        }
        if decimal {
            if let Some(at) = p.text.find(['.', ',']) {
                let part = &p.text[..at];
                let frac = if p.text.is_empty() {
                    0.0
                } else {
                    part.chars().count() as f32 / p.text.chars().count() as f32
                };
                w += p.width * frac;
                break;
            }
        }
        w += p.width;
    }
    w
}

/// The piece that fills a tab `width` points wide, with its leader if it has one.
fn tab_piece(from: &Piece, width: f32, leader: u8) -> Piece {
    let ch = match leader {
        1 => '.',
        2 => '-',
        3 => '_',
        _ => ' ',
    };
    let mut piece =
        Piece { width, text: String::new(), tab: true, fixed: true, gap: 0.0, ..from.clone() };
    piece.underline = false;
    piece.strike = false;
    piece.bg = 0;
    if leader != 0 && width > 3.0 {
        let rs = RunStyle::plain(f64::from(from.size), from.color);
        let unit = text_width(&ch.to_string(), &rs) as f32;
        if unit > 0.1 {
            let n = (width / unit).floor();
            if n >= 1.0 {
                piece.text = ch.to_string().repeat(n as usize);
                piece.gap = width - n * unit;
            }
        }
    }
    piece
}

/// Lays out one paragraph in the column `[x0, x0 + width]`.
fn layout_para(p: &Paragraph, x0: f32, width: f32, base: f32) -> ParaBox {
    let (size, _) = role_size(p.style, base);
    let spacing = if p.line_spacing == 0 { 1.0 } else { f32::from(p.line_spacing) / 100.0 };
    let marker_indent = if p.marker.is_empty() { 0.0 } else { 18.0 * f32::from(p.level.max(1)) };
    let indent = f32::from(p.indent_left).max(marker_indent).min(width * 0.6);
    let text_left = x0 + indent;
    let avail = (width - indent).max(24.0);
    let first_shift = (p.indent_first as f32).clamp(-indent, avail * 0.5);
    let (before, after) = match p.style {
        ParaStyle::Title | ParaStyle::Heading1 => (size * 0.5, size * 0.35),
        ParaStyle::Heading2 | ParaStyle::Heading3 | ParaStyle::Heading4 => {
            (size * 0.45, size * 0.3)
        }
        _ => (0.0, size * 0.45),
    };

    let all = pieces(p, base);
    let mut lines: Vec<LineBox> = Vec::new();
    let mut cur: Vec<Piece> = Vec::new();
    let mut cur_w = 0.0f32;
    let mut first = true;
    let line_limit = |first: bool| if first { avail - first_shift } else { avail };

    let finish = |items: &mut Vec<Piece>, w: &mut f32, last: bool, first: &mut bool| -> LineBox {
        while items.last().is_some_and(|i| i.space) {
            if let Some(i) = items.pop() {
                *w -= i.width;
            }
        }
        let start = text_left + if *first { first_shift } else { 0.0 };
        let line = build_line(items, start, *w, avail, p.align, last, spacing, size);
        items.clear();
        *w = 0.0;
        *first = false;
        line
    };

    for (idx, piece) in all.iter().cloned().enumerate() {
        if piece.br {
            lines.push(finish(&mut cur, &mut cur_w, true, &mut first));
            continue;
        }
        if piece.tab {
            let rel = indent + if first { first_shift } else { 0.0 } + cur_w;
            let (pos, kind, leader) = next_stop(&p.tabs, rel);
            let rest = all.get(idx + 1..).unwrap_or(&[]);
            let width = match kind {
                1 => pos - rel - after_tab(rest, false) / 2.0,
                2 => pos - rel - after_tab(rest, false),
                3 => pos - rel - after_tab(rest, true),
                _ => pos - rel,
            };
            let room = (line_limit(first) - cur_w).max(0.0);
            let width = width.clamp(0.0, room);
            cur_w += width;
            cur.push(tab_piece(&piece, width, leader));
            continue;
        }
        if piece.space && cur.is_empty() {
            continue;
        }
        if !piece.space && cur_w + piece.width > line_limit(first) && !cur.is_empty() {
            lines.push(finish(&mut cur, &mut cur_w, false, &mut first));
        }
        if piece.width > line_limit(first) && !piece.space {
            // A word wider than the line is cut into pieces that fit.
            for part in cut_word(&piece, line_limit(first)) {
                if cur_w + part.width > line_limit(first) && !cur.is_empty() {
                    lines.push(finish(&mut cur, &mut cur_w, false, &mut first));
                }
                cur_w += part.width;
                cur.push(part);
            }
            continue;
        }
        cur_w += piece.width;
        cur.push(piece);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(finish(&mut cur, &mut cur_w, true, &mut first));
    }

    // The list marker sits in the margin of the first line.
    if !p.marker.is_empty()
        && let Some(line) = lines.first_mut()
    {
        let st = RunStyle::plain(f64::from(size), INK);
        let w = text_width(&p.marker, &st) as f32;
        let x = (text_left - w - 4.0).max(x0);
        line.decos.push(Deco::Text {
            x,
            y: size * 0.95,
            size,
            width: w,
            bits: 0,
            color: INK,
            text: p.marker.clone(),
        });
    }

    // Pictures in the flow of the paragraph are not placed; the paragraph is as the text says.
    ParaBox { lines, before, after, text_left, border: p.border, boxed: p.boxed }
}

/// A word split into pieces no wider than `limit`.
fn cut_word(p: &Piece, limit: f32) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut w = 0.0f32;
    let rs = RunStyle::plain(f64::from(p.size), p.color);
    for c in p.text.chars() {
        let cw = text_width(c.encode_utf8(&mut [0u8; 4]), &rs) as f32;
        if w + cw > limit && !text.is_empty() {
            out.push(Piece { text: std::mem::take(&mut text), width: w, ..p.clone() });
            w = 0.0;
        }
        text.push(c);
        w += cw;
    }
    if !text.is_empty() {
        out.push(Piece { text, width: w, ..p.clone() });
    }
    out
}

#[allow(clippy::too_many_arguments, reason = "one line needs all of these")]
fn build_line(
    items: &[Piece],
    start: f32,
    width: f32,
    avail: f32,
    align: Align,
    last: bool,
    spacing: f32,
    base_size: f32,
) -> LineBox {
    let biggest = items.iter().map(|i| i.size).fold(0.0f32, f32::max);
    let size = if biggest > 0.0 { biggest } else { base_size };
    let h = size * 1.22 * spacing;
    let baseline = size * 0.95 + (h - size * 1.22).max(0.0);
    let spaces = items.iter().filter(|i| i.space).count();
    let free = (avail - width).max(0.0);
    let (mut x, extra) = match align {
        Align::Start => (start, 0.0),
        Align::Center => (start + free / 2.0, 0.0),
        Align::End => (start + free, 0.0),
        Align::Justify if !last && spaces > 0 => (start, free / spaces as f32),
        Align::Justify => (start, 0.0),
    };
    let mut decos: Vec<Deco> = Vec::new();
    // Neighbours with one look are one run of text.
    let mut run: Option<(f32, f32, &Piece, String)> = None;
    let flush = |run: &mut Option<(f32, f32, &Piece, String)>, decos: &mut Vec<Deco>| {
        if let Some((rx, rw, p, text)) = run.take() {
            if p.bg != 0 {
                decos.push(Deco::Rect {
                    x: rx,
                    y: baseline - p.size * 0.9,
                    w: rw,
                    h: p.size * 1.15,
                    fill: p.bg,
                    stroke: 0,
                });
            }
            decos.push(Deco::Text {
                x: rx,
                y: baseline,
                size: p.size,
                width: rw,
                bits: p.bits,
                color: p.color,
                text,
            });
            if p.underline {
                decos.push(Deco::Line {
                    x1: rx,
                    y1: baseline + p.size * 0.12,
                    x2: rx + rw,
                    y2: baseline + p.size * 0.12,
                    color: p.color,
                    width: 0.6,
                });
            }
            if p.strike {
                decos.push(Deco::Line {
                    x1: rx,
                    y1: baseline - p.size * 0.3,
                    x2: rx + rw,
                    y2: baseline - p.size * 0.3,
                    color: p.color,
                    width: 0.6,
                });
            }
        }
    };
    for item in items {
        let w = item.width + if item.space { extra } else { 0.0 };
        if item.fixed {
            flush(&mut run, &mut decos);
            if !item.text.is_empty() {
                decos.push(Deco::Text {
                    x: x + item.gap,
                    y: baseline,
                    size: item.size,
                    width: (w - item.gap).max(0.0),
                    bits: item.bits,
                    color: item.color,
                    text: item.text.clone(),
                });
            }
            x += w;
            continue;
        }
        let same = run.as_ref().is_some_and(|(_, _, p, _)| {
            p.size.to_bits() == item.size.to_bits()
                && p.bits == item.bits
                && p.color == item.color
                && p.bg == item.bg
                && p.underline == item.underline
                && p.strike == item.strike
        });
        if same {
            if let Some((_, rw, _, text)) = run.as_mut() {
                *rw += w;
                text.push_str(&item.text);
            }
        } else {
            flush(&mut run, &mut decos);
            run = Some((x, w, item, item.text.clone()));
        }
        x += w;
    }
    flush(&mut run, &mut decos);
    LineBox { h, decos }
}

// ---------------------------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------------------------

/// A header or footer laid out once for the width of the text area.
#[derive(Debug, Default)]
struct Running {
    header: Vec<ParaBox>,
    footer: Vec<ParaBox>,
}

fn boxes_height(boxes: &[ParaBox]) -> f32 {
    boxes.iter().map(|b| b.lines.iter().map(|l| l.h).sum::<f32>() + b.after).sum()
}

struct Pager {
    size: (f32, f32),
    /// Margins: left, right, top, bottom.
    m: [f32; 4],
    /// Number of text columns, the gap between them and the one being filled.
    cols: usize,
    gap: f32,
    col: usize,
    pages: Vec<Vec<Prim>>,
    cur: Vec<Prim>,
    y: f32,
    truncated: bool,
    /// Only one page may be made (a slide): whatever does not fit is dropped.
    single: bool,
    overflow: bool,
    /// Running header and footer, and whether pages are numbered.
    running: Running,
    numbered: bool,
    /// Pixel sizes of pictures whose size the document did not give, by picture id.
    natural: HashMap<u32, (u32, u32)>,
}

impl Pager {
    fn new(size: (f32, f32), margin: f32, single: bool) -> Self {
        Self {
            size,
            m: [margin; 4],
            cols: 1,
            gap: 0.0,
            col: 0,
            pages: Vec::new(),
            cur: Vec::new(),
            y: margin,
            truncated: false,
            single,
            overflow: false,
            running: Running::default(),
            numbered: false,
            natural: HashMap::new(),
        }
    }

    /// A pager for the page the document describes (A4 and the viewer's margins where it says
    /// nothing).
    fn for_page(setup: &PageSetup, base: f32) -> Self {
        let sane = |v: u16, lo: f32, hi: f32| {
            let v = f32::from(v);
            if v >= lo && v <= hi { Some(v) } else { None }
        };
        let w = sane(setup.width, 200.0, 2000.0);
        let h = sane(setup.height, 200.0, 2000.0);
        let size = match (w, h) {
            (Some(w), Some(h)) => (w, h),
            _ => A4,
        };
        let any_margin = setup.left | setup.right | setup.top | setup.bottom != 0;
        let margin = |v: u16, dim: f32| {
            if any_margin { f32::from(v).clamp(14.0, dim * 0.35) } else { MARGIN }
        };
        let mut p = Self::new(size, MARGIN, false);
        p.m = [
            margin(setup.left, size.0),
            margin(setup.right, size.0),
            margin(setup.top, size.1),
            margin(setup.bottom, size.1),
        ];
        p.y = p.m[2];
        p.numbered = true;
        let text_w = size.0 - p.m[0] - p.m[1];
        let cols = usize::from(setup.columns.clamp(1, 6));
        let gap = if setup.gap > 0 { f32::from(setup.gap).min(80.0) } else { 18.0 };
        if cols > 1 && (text_w - gap * (cols as f32 - 1.0)) / cols as f32 >= 80.0 {
            p.cols = cols;
            p.gap = gap;
        }
        let lay = |paras: &[Paragraph]| -> Vec<ParaBox> {
            paras.iter().take(6).map(|q| layout_para(q, p.m[0], text_w, base)).collect()
        };
        p.running = Running { header: lay(&setup.header), footer: lay(&setup.footer) };
        p
    }

    fn top(&self) -> f32 {
        self.m[2]
    }

    fn bottom(&self) -> f32 {
        self.size.1 - self.m[3]
    }

    /// The width of a column.
    fn col_width(&self) -> f32 {
        let text = self.size.0 - self.m[0] - self.m[1];
        (text - self.gap * (self.cols as f32 - 1.0)) / self.cols as f32
    }

    /// How far the column being filled lies right of the first one.
    fn dx(&self) -> f32 {
        self.col as f32 * (self.col_width() + self.gap)
    }

    /// Draws the running header, the footer and the page number on the page being closed.
    fn seal(&mut self) {
        if self.single {
            return;
        }
        let number = self.pages.len() + 1;
        let (left, right) = (self.m[0], self.size.0 - self.m[1]);
        let head_h = boxes_height(&self.running.header);
        let mut y = ((self.m[2] - head_h) / 2.0).max(6.0);
        for b in &self.running.header {
            for line in &b.lines {
                for d in &line.decos {
                    d.emit(0.0, y, &mut self.cur);
                }
                y += line.h;
            }
            y += b.after;
        }
        let foot_h = boxes_height(&self.running.footer);
        let zone_top = self.size.1 - self.m[3];
        let mut fy = if foot_h + 4.0 < self.m[3] {
            zone_top + ((self.m[3] - foot_h) / 2.0).max(2.0)
        } else {
            (self.size.1 - foot_h - 2.0).max(zone_top - 40.0)
        };
        let first_line = fy;
        for b in &self.running.footer {
            for line in &b.lines {
                for d in &line.decos {
                    d.emit(0.0, fy, &mut self.cur);
                }
                fy += line.h;
            }
            fy += b.after;
        }
        if self.numbered {
            let label = number.to_string();
            let st = RunStyle::plain(9.0, DIM);
            let w = text_width(&label, &st) as f32;
            let (x, ny) = if self.running.footer.is_empty() {
                ((left + right - w) / 2.0, zone_top + (self.m[3] / 2.0 - 4.0).max(2.0))
            } else {
                (right - w, first_line)
            };
            self.cur.push(Prim::Text {
                x,
                y: ny + 9.0 * 0.95,
                size: 9.0,
                angle: 0.0,
                width: w,
                style: 0,
                color: DIM,
                text: label,
            });
        }
    }

    /// Moves to the next column, or to the next page after the last column.
    fn page_break(&mut self) {
        if self.single {
            self.overflow = true;
            return;
        }
        if self.col + 1 < self.cols {
            self.col += 1;
            self.y = self.top();
            return;
        }
        if self.pages.len() >= MAX_PAGES {
            self.truncated = true;
            return;
        }
        self.seal();
        self.pages.push(std::mem::take(&mut self.cur));
        self.col = 0;
        self.y = self.top();
    }

    fn full(&self) -> bool {
        self.truncated || self.overflow
    }

    /// Puts a paragraph on the page(s), a line at a time.
    fn put_para(&mut self, b: &ParaBox, x0: f32, width: f32) {
        if self.y > self.top() {
            self.y += b.before;
        }
        let mut seg_top = self.y;
        let mut seg_open = false;
        for line in &b.lines {
            if self.full() {
                return;
            }
            if self.y + line.h > self.bottom() && self.y > self.top() {
                self.close_border(b, x0, width, seg_top);
                self.page_break();
                if self.full() {
                    return;
                }
                seg_top = self.y;
            }
            seg_open = true;
            let dx = self.dx();
            for d in &line.decos {
                d.emit(dx, self.y, &mut self.cur);
            }
            self.y += line.h;
        }
        if seg_open {
            self.close_border(b, x0, width, seg_top);
        }
        self.y += b.after;
    }

    /// The rule down the margin of a paragraph, or the frame around it, for the part on this page.
    fn close_border(&mut self, b: &ParaBox, x0: f32, width: f32, top: f32) {
        if b.border == 0 || self.y <= top {
            return;
        }
        let dx = self.dx();
        if b.boxed {
            Deco::Rect {
                x: x0 - 2.0,
                y: top - 1.0,
                w: width + 4.0,
                h: self.y - top + 2.0,
                fill: 0,
                stroke: b.border,
            }
            .emit(dx, 0.0, &mut self.cur);
        } else {
            Deco::Rect {
                x: b.text_left - 7.0,
                y: top,
                w: 2.0,
                h: self.y - top,
                fill: b.border,
                stroke: 0,
            }
            .emit(dx, 0.0, &mut self.cur);
        }
    }

    /// The size a picture is drawn at: what the document says, else what its header says
    /// (pixels at 96 per inch), else a placeholder; never wider than the column or taller than
    /// the page.
    fn picture_size(&self, info: &ImageInfo, width: f32) -> (f32, f32) {
        let (mut w, mut h) = if info.width > 0 && info.height > 0 {
            (f32::from(info.width), f32::from(info.height))
        } else if let Some(&(pw, ph)) = self.natural.get(&info.id) {
            (pw as f32 * 0.75, ph as f32 * 0.75)
        } else {
            PICTURE
        };
        if w > width {
            h *= width / w;
            w = width;
        }
        let room = (self.size.1 - self.m[2] - self.m[3]).max(40.0);
        if h > room {
            w *= room / h;
            h = room;
        }
        (w.max(2.0), h.max(2.0))
    }

    fn put_picture(&mut self, info: &ImageInfo, x0: f32, width: f32) {
        let (w, h) = self.picture_size(info, width);
        if self.y + h > self.bottom() && self.y > self.top() {
            self.page_break();
        }
        if self.full() {
            return;
        }
        let label = if info.alt.is_empty() { info.media.clone() } else { info.alt.clone() };
        let dx = self.dx();
        Deco::Image { x: x0, y: 0.0, w, h, label, id: info.id }.emit(dx, self.y, &mut self.cur);
        self.y += h + 6.0;
    }

    fn put_table(&mut self, t: &Table, x0: f32, width: f32, base: f32) {
        let cols = t
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| usize::from(c.span.max(1))).sum::<usize>())
            .max()
            .unwrap_or(1)
            .max(1);
        let col_w = width / cols as f32;
        for row in &t.rows {
            if self.full() {
                return;
            }
            // Lay out every cell, then place the row where it fits.
            let mut laid: Vec<(f32, f32, Vec<ParaBox>, bool)> = Vec::new();
            let mut col = 0usize;
            let mut height = 0.0f32;
            for cell in &row.cells {
                let span = usize::from(cell.span.max(1));
                let cx = x0 + col as f32 * col_w;
                let cw = col_w * span as f32;
                let boxes: Vec<ParaBox> = cell
                    .paras
                    .iter()
                    .map(|p| {
                        let mut p = p.clone();
                        if cell.header {
                            for r in &mut p.runs {
                                r.style |= style::BOLD;
                            }
                        }
                        layout_para(&p, cx + PAD, (cw - 2.0 * PAD).max(20.0), base)
                    })
                    .collect();
                let h: f32 = boxes
                    .iter()
                    .map(|b| b.lines.iter().map(|l| l.h).sum::<f32>() + b.after)
                    .sum::<f32>()
                    + 2.0 * PAD;
                height = height.max(h);
                laid.push((cx, cw, boxes, cell.header));
                col += span;
            }
            if self.y + height > self.bottom() && self.y > self.top() {
                self.page_break();
                if self.full() {
                    return;
                }
            }
            let top = self.y;
            let dx = self.dx();
            for (cx, cw, boxes, header) in &laid {
                Deco::Rect {
                    x: *cx,
                    y: 0.0,
                    w: *cw,
                    h: height,
                    fill: if *header { 0xFFEE_EEEE } else { 0 },
                    stroke: 0xFF99_9999,
                }
                .emit(dx, top, &mut self.cur);
                let mut y = top + PAD;
                for b in boxes {
                    for line in &b.lines {
                        for d in &line.decos {
                            d.emit(dx, y, &mut self.cur);
                        }
                        y += line.h;
                    }
                    y += b.after;
                }
            }
            self.y = top + height;
        }
        self.y += 6.0;
    }

    fn blocks(&mut self, blocks: &[Block], base: f32) {
        let x0 = self.m[0];
        let width = self.col_width();
        for block in blocks {
            if self.full() {
                return;
            }
            match block {
                Block::Para(p) => {
                    let b = layout_para(p, x0, width, base);
                    self.put_para(&b, x0, width);
                }
                Block::Table(t) => self.put_table(t, x0, width, base),
                Block::Grid(g) => {
                    // A spreadsheet grid inside a text document: its size is said, not drawn.
                    let note = Paragraph::plain(&format!("[table {} x {}]", g.rows.len(), g.cols));
                    let b = layout_para(&note, x0, width, base);
                    self.put_para(&b, x0, width);
                }
                Block::Image(info) => self.put_picture(info, x0, width),
                Block::Rule => {
                    if self.y > self.top() || self.col > 0 {
                        // A page break ends the column too.
                        self.col = self.cols.saturating_sub(1);
                        self.page_break();
                    }
                }
            }
        }
    }

    fn finish(mut self) -> (Vec<Vec<Prim>>, bool) {
        self.seal();
        self.pages.push(std::mem::take(&mut self.cur));
        (self.pages, self.truncated)
    }
}

/// The text pages of a document: every body section, one after the other, set on the page the
/// first one describes. Returns the pages, whether they were cut, and the sheet size.
fn text_pages(
    p: &Preview,
    natural: HashMap<u32, (u32, u32)>,
) -> (Vec<Vec<Prim>>, bool, (f32, f32)) {
    let setup = p
        .sections
        .iter()
        .find(|s| s.kind == SectionKind::Body)
        .map(|s| s.page.clone())
        .unwrap_or_default();
    let mut pager = Pager::for_page(&setup, BODY);
    pager.natural = natural;
    let size = pager.size;
    for section in p.sections.iter().filter(|s| s.kind == SectionKind::Body) {
        pager.blocks(&section.blocks, BODY);
        if pager.truncated {
            break;
        }
    }
    let (pages, cut) = pager.finish();
    (pages, cut, size)
}

/// One slide as one page; the text shrinks until it fits.
fn slide_page(section: &Section) -> (Vec<Prim>, bool) {
    let mut base = 20.0f32;
    let mut result: Option<Vec<Prim>> = None;
    let mut overflow = false;
    for _ in 0..8 {
        let mut pager = Pager::new(SLIDE, SLIDE_MARGIN, true);
        let first_is_title = matches!(
            section.blocks.first(),
            Some(Block::Para(p)) if matches!(p.style, ParaStyle::Title | ParaStyle::Heading1)
        );
        let mut blocks: Vec<Block> = Vec::new();
        if !first_is_title && !section.name.trim().is_empty() {
            let mut t = Paragraph::plain(section.name.trim());
            t.style = ParaStyle::Title;
            blocks.push(Block::Para(t));
        }
        blocks.extend(section.blocks.iter().cloned());
        pager.blocks(&blocks, base);
        overflow = pager.overflow;
        let (mut pages, _) = pager.finish();
        let prims = if pages.is_empty() { Vec::new() } else { pages.remove(0) };
        result = Some(prims);
        if !overflow || base <= 8.0 {
            break;
        }
        base *= 0.85;
    }
    let mut prims = result.unwrap_or_default();
    // The edge of the slide.
    Deco::Rect {
        x: 6.0,
        y: 6.0,
        w: SLIDE.0 - 12.0,
        h: SLIDE.1 - 12.0,
        fill: 0,
        stroke: 0xFFBB_BBBB,
    }
    .emit(0.0, 0.0, &mut prims);
    (prims, overflow)
}

fn model(
    width: f32,
    height: f32,
    index: usize,
    pages: usize,
    prims: Vec<Prim>,
    cut: bool,
) -> PageModel {
    PageModel {
        width,
        height,
        index: u32::try_from(index).unwrap_or(u32::MAX),
        pages: u32::try_from(pages).unwrap_or(u32::MAX),
        prims,
        truncated: cut,
        approximate: true,
    }
}

/// Page `index` (from 0) of a text document, laid out from its reading view.
///
/// # Errors
/// Whatever the preview reports; [`PreviewError::Malformed`] for a page that does not exist.
pub(crate) fn render_text_page(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<PageModel, PreviewError> {
    let preview = crate::preview(data, policy)?;
    // Pictures whose size the document did not give are sized by their own headers.
    let mut ids: Vec<u32> = Vec::new();
    for s in preview.sections.iter().filter(|s| s.kind == SectionKind::Body) {
        for b in &s.blocks {
            if let Block::Image(i) = b
                && (i.width == 0 || i.height == 0)
                && i.id != crate::model::IMAGE_NONE
                && ids.len() < 512
            {
                ids.push(i.id);
            }
        }
    }
    let natural: HashMap<u32, (u32, u32)> = if ids.is_empty() {
        HashMap::new()
    } else {
        crate::picture::natural_sizes(data, &ids, policy)
            .into_iter()
            .map(|(id, w, h)| (id, (w, h)))
            .collect()
    };
    let (mut pages, cut, size) = text_pages(&preview, natural);
    let count = pages.len();
    if index >= count {
        return Err(PreviewError::Malformed("the document has no such page".into()));
    }
    let prims = pages.swap_remove(index);
    Ok(model(size.0, size.1, index, count, prims, cut))
}

/// Slide `index` (from 0) of a binary presentation, laid out from its reading view.
///
/// # Errors
/// Whatever the preview reports; [`PreviewError::Malformed`] for a slide that does not exist.
pub(crate) fn render_slide_page(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<PageModel, PreviewError> {
    let preview = crate::preview(data, policy)?;
    let slides: Vec<&Section> =
        preview.sections.iter().filter(|s| s.kind == SectionKind::Slide).collect();
    let Some(section) = slides.get(index) else {
        return Err(PreviewError::Malformed("the document has no such page".into()));
    };
    let (prims, cut) = slide_page(section);
    Ok(model(SLIDE.0, SLIDE.1, index, slides.len(), prims, cut))
}

#[cfg(test)]
mod tests;
