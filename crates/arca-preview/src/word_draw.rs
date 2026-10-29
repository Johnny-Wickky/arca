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

use crate::model::{
    Align, Block, ImageInfo, ParaStyle, Paragraph, Preview, Run, Section, SectionKind, Table, style,
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
    let shift = if run.style & style::SUPER != 0 {
        0.65
    } else if run.style & style::SUB != 0 {
        0.65
    } else {
        1.0
    };
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
        out.push(Piece { text, width, size, bits, color, bg, underline, strike, space, br: false });
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
            });
            in_space = false;
            continue;
        }
        let ws = c == ' ' || c == '\t' || c == '\u{a0}';
        if ws != in_space {
            flush(&mut word, in_space, out);
            in_space = ws;
        }
        // A tab is drawn as a few spaces: tab stops are not read.
        if c == '\t' {
            word.push_str("    ");
        } else {
            word.push(c);
        }
    }
    flush(&mut word, in_space, out);
}

// ---------------------------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------------------------

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

    for piece in all {
        if piece.br {
            lines.push(finish(&mut cur, &mut cur_w, true, &mut first));
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

struct Pager {
    size: (f32, f32),
    margin: f32,
    pages: Vec<Vec<Prim>>,
    cur: Vec<Prim>,
    y: f32,
    truncated: bool,
    /// Only one page may be made (a slide): whatever does not fit is dropped.
    single: bool,
    overflow: bool,
}

impl Pager {
    fn new(size: (f32, f32), margin: f32, single: bool) -> Self {
        Self {
            size,
            margin,
            pages: Vec::new(),
            cur: Vec::new(),
            y: margin,
            truncated: false,
            single,
            overflow: false,
        }
    }

    fn bottom(&self) -> f32 {
        self.size.1 - self.margin
    }

    fn page_break(&mut self) {
        if self.single {
            self.overflow = true;
            return;
        }
        if self.pages.len() >= MAX_PAGES {
            self.truncated = true;
            return;
        }
        self.pages.push(std::mem::take(&mut self.cur));
        self.y = self.margin;
    }

    fn full(&self) -> bool {
        self.truncated || self.overflow
    }

    /// Puts a paragraph on the page(s), a line at a time.
    fn put_para(&mut self, b: &ParaBox, x0: f32, width: f32) {
        if self.y > self.margin {
            self.y += b.before;
        }
        let mut seg_top = self.y;
        let mut seg_open = false;
        for line in &b.lines {
            if self.full() {
                return;
            }
            if self.y + line.h > self.bottom() && self.y > self.margin {
                self.close_border(b, x0, width, seg_top);
                self.page_break();
                if self.full() {
                    return;
                }
                seg_top = self.y;
            }
            seg_open = true;
            for d in &line.decos {
                d.emit(0.0, self.y, &mut self.cur);
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
        if b.boxed {
            Deco::Rect {
                x: x0 - 2.0,
                y: top - 1.0,
                w: width + 4.0,
                h: self.y - top + 2.0,
                fill: 0,
                stroke: b.border,
            }
            .emit(0.0, 0.0, &mut self.cur);
        } else {
            Deco::Rect {
                x: b.text_left - 7.0,
                y: top,
                w: 2.0,
                h: self.y - top,
                fill: b.border,
                stroke: 0,
            }
            .emit(0.0, 0.0, &mut self.cur);
        }
    }

    fn put_picture(&mut self, info: &ImageInfo, x0: f32, width: f32) {
        let (mut w, mut h) = PICTURE;
        if w > width {
            h *= width / w;
            w = width;
        }
        if self.y + h > self.bottom() && self.y > self.margin {
            self.page_break();
        }
        if self.full() {
            return;
        }
        let label = if info.alt.is_empty() { info.media.clone() } else { info.alt.clone() };
        Deco::Image { x: x0, y: 0.0, w, h, label, id: info.id }.emit(0.0, self.y, &mut self.cur);
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
            if self.y + height > self.bottom() && self.y > self.margin {
                self.page_break();
                if self.full() {
                    return;
                }
            }
            let top = self.y;
            for (cx, cw, boxes, header) in &laid {
                Deco::Rect {
                    x: *cx,
                    y: 0.0,
                    w: *cw,
                    h: height,
                    fill: if *header { 0xFFEE_EEEE } else { 0 },
                    stroke: 0xFF99_9999,
                }
                .emit(0.0, top, &mut self.cur);
                let mut y = top + PAD;
                for b in boxes {
                    for line in &b.lines {
                        for d in &line.decos {
                            d.emit(0.0, y, &mut self.cur);
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
        let x0 = self.margin;
        let width = self.size.0 - 2.0 * self.margin;
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
                    if self.y > self.margin {
                        self.page_break();
                    }
                }
            }
        }
    }

    fn finish(mut self) -> (Vec<Vec<Prim>>, bool) {
        self.pages.push(std::mem::take(&mut self.cur));
        (self.pages, self.truncated)
    }
}

/// The text pages of a document: every body section, one after the other.
fn text_pages(p: &Preview) -> (Vec<Vec<Prim>>, bool) {
    let mut pager = Pager::new(A4, MARGIN, false);
    for section in p.sections.iter().filter(|s| s.kind == SectionKind::Body) {
        pager.blocks(&section.blocks, BODY);
        if pager.truncated {
            break;
        }
    }
    pager.finish()
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
    let (mut pages, cut) = text_pages(&preview);
    let count = pages.len();
    if index >= count {
        return Err(PreviewError::Malformed("the document has no such page".into()));
    }
    let prims = pages.swap_remove(index);
    Ok(model(A4.0, A4.1, index, count, prims, cut))
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
