//! The look of an ODS sheet: column widths, row heights, cell styles, merged cells and the page
//! layout.
//!
//! As for XLSX, the text of the cells comes from the reading view. This pass walks the table
//! structure of `content.xml` (rows and cells with their repeat counts) and the styles of
//! `styles.xml` and `content.xml`. Repeated rows and cells are kept as ranges, never expanded to
//! the number the file claims.

use std::collections::HashMap;

use super::model::{
    Borders, Edge, Font, HAlign, Layout, MAX_COL_PT, MAX_COLS, MAX_MERGES, MAX_ROW_PT, MAX_ROWS,
    MAX_RUNS, MAX_STYLED, Merge, PageSetup, RawRun, Styles, VAlign, Wide, Xf, f32_of, number,
    opaque, sane_font_size, sane_size, u32_of,
};
use super::{Meta, Picked, Read};
use crate::PreviewError;
use crate::opc::Package;
use crate::xml::{self, Flow, Handler, Tag, XmlError};

/// Most styles kept.
const MAX_STYLES: usize = 100_000;
/// Longest inheritance chain followed.
const MAX_CHAIN: usize = 16;
/// Copies of a repeated cell that get a record each; beyond that the range is painted as one.
const MAX_COPIES: u64 = 64;
/// Most sheets listed.
const MAX_SHEETS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Fam {
    Cell,
    Column,
    Row,
    Table,
}

/// A length in points; `None` for anything that is not a length.
fn length(s: &str) -> Option<f64> {
    let s = s.trim();
    let end = s.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')));
    let (num, unit) = s.split_at(end.unwrap_or(s.len()));
    let v: f64 = num.parse().ok().filter(|v: &f64| v.is_finite())?;
    let k = match unit.trim() {
        "pt" | "" => 1.0,
        "cm" => 72.0 / 2.54,
        "mm" => 72.0 / 25.4,
        "in" => 72.0,
        "pc" => 12.0,
        "px" => 0.75,
        _ => return None,
    };
    Some(v * k)
}

/// `#rrggbb` to `0xRRGGBB`.
fn hex_color(s: &str) -> Option<u32> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

/// A border property: `Some(None)` for "no border", `Some(Some((width, colour, approx)))`.
type EdgeSpec = Option<Option<(f32, u32, bool)>>;

fn border_spec(s: &str) -> EdgeSpec {
    let mut width = None;
    let mut color = 0x000000u32;
    let mut kind = "solid";
    for tok in s.split_whitespace() {
        if let Some(c) = hex_color(tok) {
            color = c;
        } else if let Some(w) = length(tok) {
            width = Some(w);
        } else {
            kind = tok;
        }
    }
    if matches!(kind, "none" | "hidden") {
        return Some(None);
    }
    let w = f32_of(width.unwrap_or(0.75).clamp(0.25, 4.0));
    let approx = matches!(kind, "dashed" | "dotted" | "double" | "dot-dash" | "dot-dot-dash");
    Some(Some((w, opaque(color), approx)))
}

/// What a style says; every property may be left to the parent.
#[derive(Debug, Clone, Default)]
struct RawStyle {
    parent: String,
    master: String,
    fill: Option<u32>,
    edges: [EdgeSpec; 4],
    wrap: Option<bool>,
    v: Option<VAlign>,
    h: Option<HAlign>,
    margin: Option<f64>,
    rot: Option<bool>,
    size: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    color: Option<u32>,
    family: Option<String>,
    col_w: Option<f64>,
    row_h: Option<f64>,
    display: Option<bool>,
}

impl RawStyle {
    /// Fills what this style leaves open from `p`.
    fn inherit(&mut self, p: &Self) {
        macro_rules! take {
            ($($f:ident),*) => { $( if self.$f.is_none() { self.$f.clone_from(&p.$f); } )* };
        }
        take!(
            fill, wrap, v, h, margin, rot, size, bold, italic, underline, strike, color, family,
            col_w, row_h, display
        );
        for (mine, theirs) in self.edges.iter_mut().zip(p.edges.iter()) {
            if mine.is_none() {
                *mine = *theirs;
            }
        }
        if self.master.is_empty() {
            self.master.clone_from(&p.master);
        }
    }
}

/// Page properties of a layout.
#[derive(Debug, Clone, Default)]
struct PageProps {
    w: Option<f64>,
    h: Option<f64>,
    landscape: bool,
    margins: [Option<f64>; 4],
    scale: Option<f64>,
    fit: Option<(u32, u32)>,
    grid: bool,
}

struct Tb {
    lay: Layout,
    index: usize,
    row: u32,
    col: u32,
    row_rep: u32,
    row_default: String,
    col_cursor: u32,
    col_defaults: Vec<(u32, u32, String)>,
}

struct OdsH {
    want: Option<usize>,
    reveal: bool,
    // Styles.
    styles: HashMap<(Fam, String), RawStyle>,
    default_cell: RawStyle,
    layouts: HashMap<String, PageProps>,
    masters: HashMap<String, String>,
    first_master: String,
    cur: Option<(Fam, String, RawStyle)>,
    cur_default: Option<RawStyle>,
    cur_layout: Option<(String, PageProps)>,
    // Body.
    in_body: bool,
    depth: u32,
    metas: Vec<Meta>,
    shown: usize,
    tb: Option<Tb>,
    picked: Option<(usize, Layout)>,
    // What the cells use.
    out: Styles,
    xf_cache: HashMap<String, u32>,
    nested: bool,
}

impl OdsH {
    fn new(want: Option<usize>, reveal: bool) -> Self {
        Self {
            want,
            reveal,
            styles: HashMap::new(),
            default_cell: RawStyle::default(),
            layouts: HashMap::new(),
            masters: HashMap::new(),
            first_master: String::new(),
            cur: None,
            cur_default: None,
            cur_layout: None,
            in_body: false,
            depth: 0,
            metas: Vec::new(),
            shown: 0,
            tb: None,
            picked: None,
            out: default_styles(),
            xf_cache: HashMap::new(),
            nested: false,
        }
    }

    /// The style `name` with everything it inherits.
    fn resolve(&self, fam: Fam, name: &str) -> RawStyle {
        let mut out = RawStyle::default();
        let mut cur = name.to_owned();
        for _ in 0..MAX_CHAIN {
            let Some(s) = self.styles.get(&(fam, cur.clone())) else { break };
            out.inherit(s);
            if s.parent.is_empty() {
                break;
            }
            cur.clone_from(&s.parent);
        }
        if fam == Fam::Cell {
            out.inherit(&self.default_cell);
        }
        out
    }

    /// The format index for a cell style, made on first use.
    fn xf_of(&mut self, name: &str) -> u32 {
        if let Some(&xf) = self.xf_cache.get(name) {
            return xf;
        }
        let s = self.resolve(Fam::Cell, name);
        let size = sane_font_size(s.size.unwrap_or(10.0), 10.0);
        let mut font = Font {
            size,
            color: opaque(s.color.unwrap_or(0)),
            bold: s.bold.unwrap_or(false),
            italic: s.italic.unwrap_or(false),
            underline: s.underline.unwrap_or(false),
            strike: s.strike.unwrap_or(false),
            ..Font::default()
        };
        font.set_family(s.family.as_deref().unwrap_or(""));
        let mut edges: Borders = [Edge::default(); 4];
        for (slot, spec) in edges.iter_mut().zip(s.edges.iter()) {
            if let Some(Some((width, color, approx))) = spec {
                *slot = Edge { width: *width, color: *color, approx: *approx };
            }
        }
        let has_edges = edges.iter().any(|e| e.width > 0.0);
        // `Some(0)` is "transparent"; a colour carries the marker `0x01` in its top byte.
        let fill = match s.fill {
            Some(c) if c >> 24 == 1 => opaque(c),
            _ => 0,
        };
        let fill_id = if fill == 0 { 0 } else { self.out.push_fill(fill) };
        let border_id = if has_edges { self.out.push_border(edges) } else { 0 };
        let font_id = self.out.push_font(font);
        let indent =
            s.margin.map_or(0, |m| u8::try_from(u32_of((m / 7.0).round()).min(15)).unwrap_or(0));
        let xf = if self.xf_cache.len() >= MAX_STYLES {
            self.out.over = true;
            0
        } else {
            self.out.push_xf(Xf {
                font: font_id,
                fill: fill_id,
                border: border_id,
                h: s.h.unwrap_or_default(),
                v: s.v.unwrap_or_default(),
                wrap: s.wrap.unwrap_or(false),
                indent,
                rot: s.rot.unwrap_or(false),
            })
        };
        self.xf_cache.insert(name.to_owned(), xf);
        xf
    }

    fn page_setup(&self, table_style: &str) -> PageSetup {
        let mut setup = PageSetup::default();
        let ts = self.resolve(Fam::Table, table_style);
        let master = if !ts.master.is_empty() && self.masters.contains_key(&ts.master) {
            ts.master
        } else if self.masters.contains_key("Default") {
            "Default".to_owned()
        } else {
            self.first_master.clone()
        };
        let Some(props) = self.masters.get(&master).and_then(|l| self.layouts.get(l)) else {
            return setup;
        };
        if let (Some(w), Some(h)) = (props.w, props.h) {
            setup.set_paper(w, h);
        }
        setup.landscape = props.landscape || props.w.zip(props.h).is_some_and(|(w, h)| w > h);
        for (side, m) in props.margins.iter().enumerate() {
            if let (Some(slot), Some(m)) = (setup.margins.get_mut(side), *m)
                && (0.0..=1_440.0).contains(&m)
            {
                *slot = f32_of(m);
            }
        }
        if let Some(s) = props.scale
            && (10.0..=400.0).contains(&s)
        {
            setup.scale = f32_of(s / 100.0);
        }
        setup.fit = props.fit;
        setup.print_grid = props.grid;
        setup
    }

    fn style_props(&mut self, tag: &Tag<'_>) {
        if let Some((_, _, s)) = self.cur.as_mut() {
            apply_props(tag, s);
        } else if let Some(s) = self.cur_default.as_mut() {
            apply_props(tag, s);
        }
    }
}

/// Applies the property element `tag` to the style `s`.
fn apply_props(tag: &Tag<'_>, s: &mut RawStyle) {
    match tag.local() {
        "table-cell-properties" => {
            // `border` is the shorthand; the sides override it whatever the attribute order.
            if let Some(b) = tag.attr("border") {
                let spec = border_spec(&b);
                s.edges = [spec; 4];
            }
            for (k, name) in
                [(0, "border-left"), (1, "border-top"), (2, "border-right"), (3, "border-bottom")]
            {
                if let (Some(b), Some(slot)) = (tag.attr(name), s.edges.get_mut(k)) {
                    *slot = border_spec(&b);
                }
            }
            if let Some(bg) = tag.attr("background-color") {
                s.fill = Some(hex_color(&bg).map_or(0, |c| c | 0x0100_0000));
            }
            if let Some(v) = tag.attr("vertical-align") {
                s.v = match &*v {
                    "top" => Some(VAlign::Top),
                    "middle" => Some(VAlign::Center),
                    "bottom" => Some(VAlign::Bottom),
                    _ => None,
                };
            }
            if let Some(w) = tag.attr("wrap-option") {
                s.wrap = Some(&*w == "wrap");
            }
            if let Some(r) = tag.attr("rotation-angle") {
                s.rot = Some(r.trim().parse::<f64>().is_ok_and(|a| a != 0.0 && a.is_finite()));
            }
        }
        "paragraph-properties" => {
            if let Some(a) = tag.attr("text-align") {
                s.h = match &*a {
                    "start" | "left" | "justify" => Some(HAlign::Left),
                    "end" | "right" => Some(HAlign::Right),
                    "center" => Some(HAlign::Center),
                    _ => None,
                };
            }
            if let Some(m) = tag.attr("margin-left") {
                s.margin = length(&m);
            }
        }
        "text-properties" => {
            if let Some(v) = tag.attr("font-size") {
                s.size = length(&v);
            }
            if let Some(v) = tag.attr("font-weight") {
                s.bold = Some(&*v == "bold" || v.trim().parse::<u32>().is_ok_and(|n| n >= 600));
            }
            if let Some(v) = tag.attr("font-style") {
                s.italic = Some(matches!(&*v, "italic" | "oblique"));
            }
            if let Some(v) = tag.attr("color") {
                s.color = hex_color(&v);
            }
            if let Some(v) = tag.attr("text-underline-style") {
                s.underline = Some(&*v != "none");
            }
            if let Some(v) = tag.attr("text-line-through-style") {
                s.strike = Some(&*v != "none");
            }
            if let Some(v) = tag.attr("font-family").or_else(|| tag.attr("font-name")) {
                s.family = Some(v.into_owned());
            }
        }
        "table-column-properties" => {
            if let Some(v) = tag.attr("column-width") {
                s.col_w = length(&v);
            }
        }
        "table-row-properties" => {
            if let Some(v) = tag.attr("row-height").or_else(|| tag.attr("min-row-height")) {
                s.row_h = length(&v);
            }
        }
        "table-properties" => {
            if let Some(v) = tag.attr("display") {
                s.display = Some(&*v != "false");
            }
        }
        _ => {}
    }
}

/// Styles that hold the default look at index 0 of every list: a cell with that format is a
/// cell without one.
fn default_styles() -> Styles {
    let mut s = Styles::default();
    s.push_font(Font { size: 10.0, ..Font::default() });
    s.push_fill(0);
    s.push_border([Edge::default(); 4]);
    s.push_xf(Xf::default());
    s
}

fn family(s: &str) -> Option<Fam> {
    match s {
        "table-cell" => Some(Fam::Cell),
        "table-column" => Some(Fam::Column),
        "table-row" => Some(Fam::Row),
        "table" => Some(Fam::Table),
        _ => None,
    }
}

fn repeat(tag: &Tag<'_>, name: &str) -> u32 {
    tag.attr(name).and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(1).clamp(1, MAX_ROWS)
}

fn hidden_flag(tag: &Tag<'_>) -> bool {
    tag.attr("visibility").is_some_and(|v| matches!(&*v, "collapse" | "filter"))
}

impl Handler for OdsH {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        let local = tag.local();
        match local {
            // --- styles ---
            "style" => {
                let fam = tag.attr("family").as_deref().and_then(family);
                let name = tag.attr("name").unwrap_or_default().into_owned();
                if let Some(fam) = fam {
                    let raw = RawStyle {
                        parent: tag.attr("parent-style-name").unwrap_or_default().into_owned(),
                        master: tag.attr("master-page-name").unwrap_or_default().into_owned(),
                        ..RawStyle::default()
                    };
                    self.cur = Some((fam, name, raw));
                }
            }
            "default-style" => {
                if tag.attr("family").as_deref() == Some("table-cell") {
                    self.cur_default = Some(RawStyle::default());
                }
            }
            "table-cell-properties"
            | "paragraph-properties"
            | "text-properties"
            | "table-column-properties"
            | "table-row-properties"
            | "table-properties" => self.style_props(tag),
            "page-layout" => {
                self.cur_layout =
                    Some((tag.attr("name").unwrap_or_default().into_owned(), PageProps::default()));
            }
            "page-layout-properties" => {
                if let Some((_, p)) = self.cur_layout.as_mut() {
                    p.w = tag.attr("page-width").as_deref().and_then(length);
                    p.h = tag.attr("page-height").as_deref().and_then(length);
                    p.landscape = tag.attr("print-orientation").as_deref() == Some("landscape");
                    for (k, n) in ["margin-left", "margin-top", "margin-right", "margin-bottom"]
                        .iter()
                        .enumerate()
                    {
                        if let Some(slot) = p.margins.get_mut(k) {
                            *slot = tag.attr(n).as_deref().and_then(length);
                        }
                    }
                    p.scale =
                        tag.attr("scale-to").and_then(|v| number(v.trim().trim_end_matches('%')));
                    let x = tag.attr("scale-to-X").and_then(|v| v.trim().parse::<u32>().ok());
                    let y = tag.attr("scale-to-Y").and_then(|v| v.trim().parse::<u32>().ok());
                    let n = tag.attr("scale-to-pages").and_then(|v| v.trim().parse::<u32>().ok());
                    p.fit = match (x, y, n) {
                        (None, None, Some(n)) if n > 0 => Some((n.min(100), n.min(100))),
                        (None, None, _) => None,
                        (x, y, _) => Some((x.unwrap_or(0).min(100), y.unwrap_or(0).min(100))),
                    };
                    p.grid = tag
                        .attr("print")
                        .is_some_and(|v| v.split_whitespace().any(|w| w == "grid"));
                }
            }
            "master-page" => {
                let name = tag.attr("name").unwrap_or_default().into_owned();
                let layout = tag.attr("page-layout-name").unwrap_or_default().into_owned();
                if self.first_master.is_empty() {
                    self.first_master.clone_from(&name);
                }
                if self.masters.len() < 1_024 {
                    self.masters.insert(name, layout);
                }
            }

            // --- body ---
            "spreadsheet" => self.in_body = true,
            "table" if self.in_body => {
                self.depth += 1;
                if self.depth > 1 {
                    self.nested = true;
                    return Flow::SkipChildren;
                }
                if self.metas.len() >= MAX_SHEETS {
                    return Flow::SkipChildren;
                }
                let name = tag.attr("name").unwrap_or_default().into_owned();
                let style_name = tag.attr("style-name").unwrap_or_default().into_owned();
                let hidden = self.resolve(Fam::Table, &style_name).display == Some(false);
                let index = self.metas.len();
                self.metas.push(Meta { name, hidden });
                if !(self.reveal || !hidden) {
                    return Flow::SkipChildren;
                }
                let mine = self.shown;
                self.shown += 1;
                if self.want != Some(mine) {
                    return Flow::SkipChildren;
                }
                let mut lay = Layout {
                    gridlines: true,
                    default_col: 64.26,
                    default_row: 12.81,
                    setup: self.page_setup(&style_name),
                    ..Layout::default()
                };
                if tag.attr("print-ranges").is_some() {
                    lay.notes.add("print area (ignored)");
                }
                self.tb = Some(Tb {
                    lay,
                    index,
                    row: 0,
                    col: 0,
                    row_rep: 1,
                    row_default: String::new(),
                    col_cursor: 0,
                    col_defaults: Vec::new(),
                });
            }
            "table-column" if self.tb.is_some() => self.column(tag),
            "table-row" if self.tb.is_some() => {
                let rep = repeat(tag, "number-rows-repeated");
                let style = tag.attr("style-name").unwrap_or_default().into_owned();
                let rs = self.resolve(Fam::Row, &style);
                let hidden = hidden_flag(tag);
                let size = rs.row_h.and_then(|h| sane_size(h, MAX_ROW_PT));
                if let Some(tb) = self.tb.as_mut() {
                    tb.col = 0;
                    tb.row_rep = rep;
                    tb.row_default =
                        tag.attr("default-cell-style-name").unwrap_or_default().into_owned();
                    if (size.is_some() || hidden)
                        && tb.row < MAX_ROWS
                        && tb.lay.rows.len() < MAX_RUNS
                    {
                        let last = tb.row.saturating_add(rep - 1).min(MAX_ROWS - 1);
                        tb.lay.rows.push(RawRun { first: tb.row, last, size, hidden });
                    }
                }
            }
            "table-cell" | "covered-table-cell" if self.tb.is_some() => {
                self.cell(tag, local == "covered-table-cell");
            }
            "frame" | "custom-shape" | "shapes" | "chart" | "object" | "image" | "control"
                if self.tb.is_some() =>
            {
                if let Some(tb) = self.tb.as_mut() {
                    tb.lay.notes.add("drawings, charts and pictures (not drawn)");
                }
            }
            "conditional-formats" | "conditional-format" | "color-scale" | "icon-set"
                if self.tb.is_some() =>
            {
                if let Some(tb) = self.tb.as_mut() {
                    tb.lay.notes.add("conditional formatting (not drawn)");
                }
            }
            "data-bar" if self.tb.is_some() => {
                if let Some(tb) = self.tb.as_mut() {
                    tb.lay.notes.add("data bars (not drawn)");
                }
            }
            "sparkline-groups" | "sparkline-group" if self.tb.is_some() => {
                if let Some(tb) = self.tb.as_mut() {
                    tb.lay.notes.add("sparklines (not drawn)");
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, local: &str) {
        match local {
            "style" => {
                if let Some((fam, name, raw)) = self.cur.take()
                    && self.styles.len() < MAX_STYLES
                {
                    self.styles.insert((fam, name), raw);
                }
            }
            "default-style" => {
                if let Some(d) = self.cur_default.take() {
                    self.default_cell = d;
                }
            }
            "page-layout" => {
                if let Some((name, p)) = self.cur_layout.take()
                    && self.layouts.len() < 1_024
                {
                    self.layouts.insert(name, p);
                }
            }
            "table-row" => {
                if let Some(tb) = self.tb.as_mut() {
                    tb.row = tb.row.saturating_add(tb.row_rep).min(MAX_ROWS);
                }
            }
            "table" if self.in_body => {
                self.depth = self.depth.saturating_sub(1);
                if self.depth == 0
                    && let Some(tb) = self.tb.take()
                {
                    self.picked = Some((tb.index, tb.lay));
                }
            }
            "spreadsheet" => self.in_body = false,
            _ => {}
        }
    }

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

impl OdsH {
    fn column(&mut self, tag: &Tag<'_>) {
        let rep = repeat(tag, "number-columns-repeated");
        let style = tag.attr("style-name").unwrap_or_default().into_owned();
        let cs = self.resolve(Fam::Column, &style);
        let hidden = hidden_flag(tag);
        let size = cs.col_w.and_then(|w| sane_size(w, MAX_COL_PT));
        let default = tag.attr("default-cell-style-name").unwrap_or_default().into_owned();
        let Some(tb) = self.tb.as_mut() else { return };
        let first = tb.col_cursor;
        let last = first.saturating_add(rep - 1).min(MAX_COLS - 1);
        tb.col_cursor = first.saturating_add(rep);
        if first >= MAX_COLS {
            return;
        }
        if (size.is_some() || hidden) && tb.lay.cols.len() < 20_000 {
            tb.lay.cols.push(RawRun { first, last, size, hidden });
        }
        if !default.is_empty() && tb.col_defaults.len() < 20_000 {
            tb.col_defaults.push((first, last, default));
        }
    }

    fn cell(&mut self, tag: &Tag<'_>, covered: bool) {
        let reps = repeat(tag, "number-columns-repeated");
        let Some(tb) = self.tb.as_mut() else { return };
        let col = tb.col;
        tb.col = col.saturating_add(reps);
        if covered || col >= MAX_COLS || tb.row >= MAX_ROWS {
            return;
        }
        let (row, row_rep) = (tb.row, tb.row_rep);
        let mut name = tag.attr("style-name").unwrap_or_default().into_owned();
        if name.is_empty() {
            name.clone_from(&tb.row_default);
        }
        if name.is_empty() {
            let i = tb.col_defaults.partition_point(|d| d.1 < col);
            if let Some(d) = tb.col_defaults.get(i).filter(|d| d.0 <= col) {
                name.clone_from(&d.2);
            }
        }
        let cs = tag
            .attr("number-columns-spanned")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(1);
        let rs =
            tag.attr("number-rows-spanned").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(1);
        let merge = (cs > 1 || rs > 1)
            .then(|| {
                Merge::new(
                    row,
                    col,
                    row.saturating_add(rs.max(1) - 1),
                    col.saturating_add(cs.max(1) - 1),
                )
            })
            .flatten();
        if let Some(m) = merge
            && let Some(tb) = self.tb.as_mut()
        {
            if tb.lay.merges.len() < MAX_MERGES {
                tb.lay.merges.push(m);
            } else {
                tb.lay.truncated = true;
            }
        }
        if name.is_empty() {
            return;
        }
        let xf = self.xf_of(&name);
        let visible = self.out.visible(xf);
        let Some(tb) = self.tb.as_mut() else { return };
        if xf == 0 {
            return;
        }
        let copies = u64::from(reps) * u64::from(row_rep);
        if copies <= MAX_COPIES {
            for r in 0..row_rep {
                for c in 0..reps {
                    if tb.lay.styled.len() >= MAX_STYLED {
                        tb.lay.truncated = true;
                        return;
                    }
                    tb.lay.styled.push((row + r, col.saturating_add(c), xf));
                }
            }
        } else if visible && tb.lay.wide.len() < 200_000 {
            tb.lay.wide.push(Wide {
                r1: row,
                r2: row.saturating_add(row_rep - 1).min(MAX_ROWS - 1),
                c1: col,
                c2: col.saturating_add(reps - 1).min(MAX_COLS - 1),
                xf,
            });
        }
    }
}

fn read_part(pkg: &mut Package<'_>, name: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(name)? else { return Ok(None) };
    Ok(Some(pkg.decode(name, &bytes)?.into_owned()))
}

/// Lists the sheets and reads the look of the one at `want` (counted among the sheets that are
/// shown: hidden ones only when `reveal`).
pub(super) fn read(
    pkg: &mut Package<'_>,
    want: Option<usize>,
    reveal: bool,
) -> Result<Read, PreviewError> {
    let content = read_part(pkg, "content.xml")?
        .ok_or_else(|| PreviewError::Malformed("the package has no content.xml".into()))?;
    let mut h = OdsH::new(want, reveal);
    if let Some(styles) = read_part(pkg, "styles.xml")? {
        // A damaged styles part costs the looks it defines.
        let _ = xml::drive(&styles, pkg.limits(), &mut h);
    }
    let res = xml::drive(&content, pkg.limits(), &mut h);
    if want.is_some() && h.picked.is_none() {
        return Err(PreviewError::Malformed("the document has no such sheet".into()));
    }
    let nested = h.nested;
    let metas = std::mem::take(&mut h.metas);
    let picked = h.picked.take().map(|(index, mut layout)| {
        layout.truncated |= res.is_err();
        if nested {
            layout.notes.add("tables inside cells (not drawn)");
        }
        let mut styles = std::mem::take(&mut h.out);
        if styles.fonts.is_empty() {
            styles.fonts.push(Font::default());
        }
        Picked { index, styles, layout }
    });
    Ok(Read { metas, picked })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_in_points() {
        assert!((length("2.54cm").unwrap_or(0.0) - 72.0).abs() < 1e-9);
        assert!((length("1in").unwrap_or(0.0) - 72.0).abs() < 1e-9);
        assert!((length("10pt").unwrap_or(0.0) - 10.0).abs() < 1e-9);
        assert!((length("96px").unwrap_or(0.0) - 72.0).abs() < 1e-9);
        assert!((length("25.4mm").unwrap_or(0.0) - 72.0).abs() < 1e-9);
        assert_eq!(length("abc"), None);
        assert_eq!(length("12furlongs"), None);
        assert_eq!(length("NaNcm"), None);
        assert_eq!(length("1e999cm"), None);
    }

    #[test]
    fn borders_and_colours() {
        assert_eq!(border_spec("none"), Some(None));
        assert_eq!(border_spec("0.74pt solid #ff0000"), Some(Some((0.74, 0xFFFF_0000, false))));
        assert_eq!(border_spec("1pt dashed #000000"), Some(Some((1.0, 0xFF00_0000, true))));
        assert_eq!(border_spec("999pt solid #000000").map(|b| b.map(|e| e.0)), Some(Some(4.0)));
        assert_eq!(hex_color("#00ff7f"), Some(0x00FF7F));
        assert_eq!(hex_color("transparent"), None);
        assert_eq!(hex_color("#12"), None);
    }
}
