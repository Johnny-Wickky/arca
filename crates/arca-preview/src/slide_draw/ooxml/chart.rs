//! Charts on a slide: column, bar, line, area, pie and doughnut charts, drawn from the numbers the
//! chart part itself caches (`c:numCache`, `c:strCache`): the ones PowerPoint shows when the
//! workbook behind the chart is not opened. The embedded workbook is never read.
//!
//! A chart is read into a small model ([`read`]) and drawn in its frame `0..w × 0..h`, in points
//! ([`draw`]); the caller maps the frame to the page. Everything is bounded: series, points,
//! labels and slices have limits, and a chart over a limit is drawn as far as it goes and the page
//! says so.

use std::f64::consts::{FRAC_PI_2, TAU};

use super::super::{put_text, text_width};
use super::{Canvas, Look, Mat, Node, Paint, RunStyle, Seg, emu_pt, num, off};
use crate::numfmt;

/// Most series read from one chart, groups of series (a bar chart and a line chart over the same
/// axes are two groups), and points of a series.
const MAX_SERIES: usize = 48;
const MAX_GROUPS: usize = 8;
const MAX_POINTS: usize = 2_000;
/// Most things (bars, markers, slices, labels) drawn in one chart.
const MAX_DRAWN: usize = 6_000;
/// Most ticks on an axis.
const MAX_TICKS: usize = 60;

/// Space around the chart, in points.
const PAD: f64 = 6.0;
/// The colours of a chart that names none: the Office accents.
const OFFICE: [u32; 6] =
    [0xFF44_72C4, 0xFFED_7D31, 0xFFA5_A5A5, 0xFFFF_C000, 0xFF5B_9BD5, 0xFF70_AD47];
const GRID: u32 = 0xFFD9_D9D9;
const AXIS: u32 = 0xFFBF_BFBF;
const TEXT: u32 = 0xFF59_5959;
const WHITE: u32 = 0xFFFF_FFFF;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Bar,
    Line,
    Area,
    Pie,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grouping {
    Standard,
    Stacked,
    Percent,
}

/// What a data label shows.
#[derive(Clone, Default)]
struct Labels {
    val: bool,
    pct: bool,
    cat: bool,
    fmt: Option<String>,
}

impl Labels {
    fn any(&self) -> bool {
        self.val || self.pct || self.cat
    }
}

struct Series {
    name: String,
    /// The colour of its bars, area or line; alpha 0 for a series drawn with no fill.
    color: u32,
    /// The colours the document gives single points (a slice, a bar), by index.
    points: Vec<(usize, u32)>,
    line_width: Option<f64>,
    cats: Vec<String>,
    vals: Vec<Option<f64>>,
    fmt: Option<String>,
    labels: Labels,
    marker: Option<bool>,
    smooth: bool,
}

struct Group {
    shape: Shape,
    horizontal: bool,
    grouping: Grouping,
    /// Gap between clusters of bars, as a fraction of a bar.
    gap: f64,
    /// Overlap of the bars of a cluster, as a fraction of a bar.
    overlap: f64,
    /// The hole of a doughnut, as a fraction of its radius (0 for a pie).
    hole: f64,
    /// Angle of the first slice, in radians clockwise from twelve o'clock.
    first: f64,
    /// Lines carry markers unless the series says otherwise.
    markers: bool,
    /// A single series takes one colour per point.
    vary: bool,
    series: Vec<Series>,
}

#[derive(Default)]
struct Axis {
    deleted: bool,
    title: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
    major: Option<f64>,
    fmt: Option<String>,
    reverse: bool,
    grid: bool,
    log: bool,
    /// Points sit on the tick marks (`midCat`) instead of between them.
    on_ticks: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Right,
    Left,
    Top,
    Bottom,
}

pub(super) struct Chart {
    groups: Vec<Group>,
    title: Option<(String, f64)>,
    legend: Option<Side>,
    /// Base font size.
    size: f64,
    text: u32,
    accent: [u32; 6],
    cat: Axis,
    val: Axis,
    fill: Option<u32>,
    plot_fill: Option<u32>,
}

// ---- reading -----------------------------------------------------------------------------------

/// `<c:x val="1"/>` and `<c:x/>` are on, `val="0"` is off, an absent element is off.
fn flag(n: Option<&Node>) -> bool {
    n.is_some_and(|n| !off(n.attr("val")))
}

fn val_of<'a>(n: &'a Node, child: &str) -> Option<&'a str> {
    n.child(child).and_then(|c| c.attr("val"))
}

fn number_of(n: &Node, child: &str) -> Option<f64> {
    val_of(n, child).and_then(num)
}

/// The data points of a cache (`strCache`, `numCache`, a `lvl`), by index.
fn cache_points(cache: &Node) -> Vec<Option<String>> {
    let count = val_of(cache, "ptCount")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_POINTS);
    let mut out: Vec<Option<String>> = Vec::new();
    for pt in cache.kids_named("pt") {
        let Some(i) = pt.attr("idx").and_then(|v| v.trim().parse::<usize>().ok()) else {
            continue;
        };
        if i >= MAX_POINTS {
            continue;
        }
        if out.len() <= i {
            out.resize(i + 1, None);
        }
        if let Some(slot) = out.get_mut(i) {
            *slot = pt.child("v").map(Node::own_text);
        }
    }
    if out.len() < count {
        out.resize(count, None);
    }
    out
}

/// The cache behind a `c:cat`, `c:val` or `c:tx`: a reference with its cache, or literal values.
fn cache_of(n: &Node) -> Option<&Node> {
    for (reference, cache) in [("strRef", "strCache"), ("numRef", "numCache")] {
        if let Some(c) = n.child(reference).and_then(|r| r.child(cache)) {
            return Some(c);
        }
    }
    if let Some(c) = n
        .child("multiLvlStrRef")
        .and_then(|r| r.child("multiLvlStrCache"))
        .and_then(|c| c.child("lvl"))
    {
        return Some(c);
    }
    n.child("strLit").or_else(|| n.child("numLit"))
}

/// The text of rich text (`c:tx/c:rich`): runs of a paragraph joined, paragraphs by a space.
fn rich_text(tx: Option<&Node>) -> String {
    let Some(rich) = tx.and_then(|t| t.child("rich")) else { return String::new() };
    let paras: Vec<String> = rich
        .kids_named("p")
        .map(|p| p.kids_named("r").filter_map(|r| r.child("t")).map(Node::own_text).collect())
        .collect();
    paras.join(" ").trim().to_owned()
}

/// The first font size (in points) set below `n`.
fn font_size(n: &Node, depth: usize) -> Option<f64> {
    if matches!(n.name.as_str(), "defRPr" | "rPr")
        && let Some(sz) = n.attr("sz").and_then(num)
    {
        return Some((sz / 100.0).clamp(4.0, 72.0));
    }
    if depth == 0 {
        return None;
    }
    n.kids.iter().find_map(|k| font_size(k, depth - 1))
}

/// A fill or line colour set by a shape property element: `Some(0)` for none.
fn colour(look: &Look, holder: Option<&Node>, cv: &mut Canvas) -> Option<u32> {
    match look.paint_in(holder?, None, cv) {
        Paint::Color(c) => Some(c),
        Paint::None => Some(0),
        Paint::Unset | Paint::Picture => None,
    }
}

fn index(n: &Node) -> Option<usize> {
    val_of(n, "idx").and_then(|v| v.trim().parse().ok())
}

fn labels(n: Option<&Node>, parent: &Labels) -> Labels {
    let Some(n) = n else { return parent.clone() };
    if flag(n.child("delete")) {
        return Labels::default();
    }
    let fmt = n
        .child("numFmt")
        .filter(|f| !on_attr(f.attr("sourceLinked")))
        .and_then(|f| f.attr("formatCode"))
        .map(str::to_owned);
    Labels {
        val: flag(n.child("showVal")),
        pct: flag(n.child("showPercent")),
        cat: flag(n.child("showCatName")),
        fmt: fmt.or_else(|| parent.fmt.clone()),
    }
}

fn on_attr(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("1" | "true" | "on"))
}

fn palette(accent: &[u32; 6], i: usize) -> u32 {
    let base = accent.get(i % 6).copied().unwrap_or(0xFF44_72C4);
    match i / 6 {
        0 => base,
        1 => mix(base, 0xFF00_0000, 0.35),
        2 => mix(base, WHITE, 0.4),
        k => mix(base, WHITE, (0.2 * f64::from(u32::try_from(k).unwrap_or(0))).min(0.8)),
    }
}

/// `c` moved the fraction `t` of the way to `to`.
fn mix(c: u32, to: u32, t: f64) -> u32 {
    let ch = |shift: u32| {
        let a = f64::from((c >> shift) & 255);
        let b = f64::from((to >> shift) & 255);
        let v = (a + (b - a) * t).round().clamp(0.0, 255.0);
        // `v` is in 0..=255.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = v as u32;
        v
    };
    0xFF00_0000 | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

fn luminance(c: u32) -> f64 {
    let ch = |shift: u32| f64::from((c >> shift) & 255) / 255.0;
    0.299 * ch(16) + 0.587 * ch(8) + 0.114 * ch(0)
}

/// Black or white text, whichever reads on `bg`.
fn on_colour(bg: u32) -> u32 {
    if luminance(bg) < 0.58 { WHITE } else { 0xFF26_2626 }
}

fn read_axis(n: &Node) -> Axis {
    let scaling = n.child("scaling");
    Axis {
        deleted: flag(n.child("delete")),
        title: n.child("title").map(|t| rich_text(t.child("tx"))).filter(|t| !t.is_empty()),
        min: scaling.and_then(|s| number_of(s, "min")),
        max: scaling.and_then(|s| number_of(s, "max")),
        major: number_of(n, "majorUnit").filter(|v| *v > 0.0),
        fmt: n
            .child("numFmt")
            .filter(|f| !on_attr(f.attr("sourceLinked")))
            .and_then(|f| f.attr("formatCode"))
            .map(str::to_owned),
        reverse: scaling.and_then(|s| val_of(s, "orientation")) == Some("maxMin"),
        grid: n.child("majorGridlines").is_some(),
        log: scaling.is_some_and(|s| s.child("logBase").is_some()),
        on_ticks: val_of(n, "crossBetween") == Some("midCat"),
    }
}

fn read_series(
    s: &Node,
    shape: Shape,
    look: &Look,
    accent: &[u32; 6],
    number: usize,
    parent: &Labels,
    cv: &mut Canvas,
) -> Series {
    let sp = s.child("spPr");
    let ln = sp.and_then(|p| p.child("ln"));
    let own = if shape == Shape::Line { colour(look, ln, cv) } else { colour(look, sp, cv) };
    let points = s
        .kids_named("dPt")
        .filter_map(|d| Some((index(d)?, colour(look, d.child("spPr"), cv)?)))
        .collect();
    let cache = s.child("val").or_else(|| s.child("yVal")).and_then(cache_of);
    let long = |c: &Node| {
        val_of(c, "ptCount").and_then(|v| v.trim().parse::<usize>().ok()) > Some(MAX_POINTS)
    };
    if cache.is_some_and(long) {
        cv.approximated("a large chart is drawn in part");
    }
    let vals = cache
        .map(|c| cache_points(c).iter().map(|p| p.as_deref().and_then(num)).collect())
        .unwrap_or_default();
    let fmt = cache
        .and_then(|c| c.child("formatCode"))
        .map(Node::own_text)
        .filter(|f| !f.trim().is_empty() && f.trim() != "General");
    let cats = s
        .child("cat")
        .or_else(|| s.child("xVal"))
        .and_then(cache_of)
        .map(|c| cache_points(c).into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default();
    let name = s
        .child("tx")
        .and_then(|t| {
            cache_of(t)
                .and_then(|c| cache_points(c).into_iter().flatten().next())
                .or_else(|| t.child("v").map(Node::own_text))
        })
        .unwrap_or_default();
    Series {
        name,
        color: own.unwrap_or_else(|| palette(accent, number)),
        points,
        line_width: ln
            .and_then(|l| l.attr("w"))
            .map(|w| emu_pt(Some(w)))
            .filter(|w| *w > 0.0 && *w < 40.0),
        cats,
        vals,
        fmt,
        labels: labels(s.child("dLbls"), parent),
        marker: s.child("marker").and_then(|m| val_of(m, "symbol")).map(|sym| sym.trim() != "none"),
        smooth: flag(s.child("smooth")),
    }
}

/// Reads a chart part (`c:chartSpace`). `None` if it holds no kind of chart that is drawn.
pub(super) fn read(root: &Node, look: &Look, cv: &mut Canvas) -> Option<Chart> {
    let chart = root.child("chart")?;
    let plot = chart.child("plotArea")?;
    let accent: [u32; 6] = std::array::from_fn(|i| {
        look.scheme(&format!("accent{}", i + 1), None)
            .map_or_else(|| OFFICE.get(i).copied().unwrap_or(0xFF44_72C4), |c| c | 0xFF00_0000)
    });
    let mut groups: Vec<Group> = Vec::new();
    let (mut skipped, mut flat, mut total) = (false, false, 0usize);
    for g in &plot.kids {
        let (shape, three_d) = match g.name.as_str() {
            "barChart" => (Shape::Bar, false),
            "bar3DChart" => (Shape::Bar, true),
            "lineChart" => (Shape::Line, false),
            "line3DChart" => (Shape::Line, true),
            "areaChart" => (Shape::Area, false),
            "area3DChart" => (Shape::Area, true),
            "pieChart" | "doughnutChart" => (Shape::Pie, false),
            "pie3DChart" => (Shape::Pie, true),
            "scatterChart" | "radarChart" | "bubbleChart" | "stockChart" | "surfaceChart"
            | "surface3DChart" | "ofPieChart" => {
                skipped = true;
                continue;
            }
            _ => continue,
        };
        if groups.len() >= MAX_GROUPS || total >= MAX_SERIES {
            skipped = true;
            break;
        }
        flat |= three_d;
        let parent = labels(g.child("dLbls"), &Labels::default());
        let series: Vec<Series> = g
            .kids_named("ser")
            .take(MAX_SERIES - total)
            .enumerate()
            .map(|(i, s)| read_series(s, shape, look, &accent, total + i, &parent, cv))
            .collect();
        total += series.len();
        let grouping = match val_of(g, "grouping") {
            Some("stacked") => Grouping::Stacked,
            Some("percentStacked") => Grouping::Percent,
            _ => Grouping::Standard,
        };
        let pct = |name: &str| number_of(g, name).map(|v| v / 100.0);
        groups.push(Group {
            shape,
            horizontal: val_of(g, "barDir") == Some("bar"),
            grouping: if matches!(shape, Shape::Pie) { Grouping::Standard } else { grouping },
            gap: pct("gapWidth").unwrap_or(1.5).clamp(0.0, 5.0),
            overlap: pct("overlap")
                .unwrap_or(if grouping == Grouping::Standard { 0.0 } else { 1.0 })
                .clamp(-1.0, 1.0),
            hole: if g.name == "doughnutChart" {
                pct("holeSize").unwrap_or(0.5).clamp(0.1, 0.9)
            } else {
                0.0
            },
            first: number_of(g, "firstSliceAng").unwrap_or(0.0).to_radians(),
            markers: flag(g.child("marker")),
            vary: flag(g.child("varyColors")),
            series,
        });
    }
    if groups.iter().all(|g| g.series.iter().all(|s| s.vals.is_empty())) {
        return None;
    }
    if skipped {
        cv.approximated("some kinds of chart are not drawn");
    }
    if flat {
        cv.approximated("3-D charts are drawn flat");
    }

    let first_name = groups
        .first()
        .filter(|g| g.series.len() == 1)
        .and_then(|g| g.series.first())
        .map(|s| s.name.clone());
    let base = root.child("txPr").and_then(|t| font_size(t, 6)).unwrap_or(10.0);
    let title = chart.child("title").filter(|t| !flag(t.child("delete"))).and_then(|t| {
        let text = Some(rich_text(t.child("tx")))
            .filter(|s| !s.is_empty())
            .or_else(|| first_name.clone().filter(|s| !s.is_empty()))?;
        Some((text, font_size(t, 6).unwrap_or(base * 1.4)))
    });
    let legend = chart.child("legend").filter(|l| !flag(l.child("delete"))).map(|l| {
        match val_of(l, "legendPos") {
            Some("l") => Side::Left,
            Some("t") => Side::Top,
            Some("b") => Side::Bottom,
            _ => Side::Right,
        }
    });
    let axis = |names: &[&str]| {
        names.iter().find_map(|n| plot.child(n)).map(read_axis).unwrap_or_default()
    };
    let cat = axis(&["catAx", "dateAx", "serAx"]);
    let mut val = axis(&["valAx"]);
    // "Cross between" is stated on the value axis and holds for the category axis.
    let mut cat = cat;
    cat.on_ticks = val.on_ticks;
    val.on_ticks = false;
    if cat.log || val.log {
        cv.approximated("a logarithmic axis is drawn linear");
    }
    Some(Chart {
        groups,
        title,
        legend,
        size: base,
        text: root
            .child("txPr")
            .and_then(|t| {
                colour(
                    look,
                    t.child("p").and_then(|p| p.child("pPr")).and_then(|p| p.child("defRPr")),
                    cv,
                )
            })
            .filter(|c| c >> 24 != 0)
            .unwrap_or(TEXT),
        accent,
        cat,
        val,
        fill: colour(look, root.child("spPr"), cv).filter(|c| c >> 24 != 0),
        plot_fill: colour(look, plot.child("spPr"), cv).filter(|c| c >> 24 != 0),
    })
}

// ---- numbers -----------------------------------------------------------------------------------

fn fl(i: usize) -> f64 {
    f64::from(u32::try_from(i).unwrap_or(u32::MAX))
}

/// A whole number of steps (at least 1, at most `MAX_TICKS`) from a positive real.
fn steps(v: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = if v.is_finite() { v.round().clamp(1.0, 1.0e6) as usize } else { 1 };
    n.min(MAX_TICKS)
}

/// A step of 1, 2 or 5 times a power of ten that is near `raw` (and not smaller).
fn nice_step(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let exp = raw.log10().floor();
    let unit = 10f64.powf(exp);
    let f = raw / unit;
    let m = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    m * unit
}

struct Scale {
    min: f64,
    max: f64,
    ticks: Vec<f64>,
}

fn scale(lo: f64, hi: f64, axis: &Axis, zero: bool, percent: bool, target: usize) -> Scale {
    let (mut lo, mut hi) = (lo, hi);
    if !(lo.is_finite() && hi.is_finite()) {
        (lo, hi) = (0.0, 1.0);
    }
    if percent {
        lo = lo.clamp(-1.0, 0.0);
        hi = if hi > 0.0 { 1.0 } else { 0.0 };
    } else if zero {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    } else if lo >= 0.0 && hi > 0.0 && (hi - lo) / hi > 1.0 / 6.0 {
        lo = 0.0;
    }
    if hi - lo < 1e-12 {
        if lo == 0.0 {
            hi = 1.0;
        } else {
            let pad = lo.abs() * 0.1;
            (lo, hi) = (lo - pad, hi + pad);
        }
    }
    let span = axis.max.unwrap_or(hi) - axis.min.unwrap_or(lo);
    let mut step = match axis.major {
        Some(m) if span / m <= fl(MAX_TICKS) => m,
        _ => nice_step(span.max(1e-12) / fl(target.max(2))),
    };
    let min = axis.min.unwrap_or_else(|| (lo / step).floor() * step);
    let mut max = axis.max.unwrap_or_else(|| (hi / step).ceil() * step);
    if max <= min {
        max = min + step;
    }
    while (max - min) / step > fl(MAX_TICKS) {
        step *= 2.0;
    }
    let n = steps((max - min) / step);
    let ticks = (0..=n)
        .map(|k| {
            let v = min + fl(k) * step;
            if v.abs() < step * 1e-9 { 0.0 } else { v }
        })
        .collect();
    Scale { min, max, ticks }
}

/// `v` as the number format `code` writes it.
fn format(code: Option<&str>, v: f64) -> String {
    match code.map(str::trim) {
        Some(c) if !c.is_empty() && c != "General" => numfmt::parse(c).number(v, false).0,
        _ => numfmt::general(v),
    }
}

/// Per series, per category: from where to where a bar, an area or a line goes.
type Spans = Vec<Vec<Option<(f64, f64)>>>;

/// Where each series of a group stands on every category: from where to where its bar, area or
/// line goes. Stacked groups pile positives and negatives apart, percent groups scale each
/// category to a total of 1.
fn spans(g: &Group, n: usize) -> Spans {
    let totals: Vec<f64> = (0..n)
        .map(|i| {
            g.series
                .iter()
                .filter_map(|s| s.vals.get(i).copied().flatten())
                .map(f64::abs)
                .sum::<f64>()
        })
        .collect();
    let mut pos = vec![0.0f64; n];
    let mut neg = vec![0.0f64; n];
    g.series
        .iter()
        .map(|s| {
            (0..n)
                .map(|i| {
                    let v = s.vals.get(i).copied().flatten().filter(|v| v.is_finite())?;
                    match g.grouping {
                        Grouping::Standard => Some((0.0, v)),
                        Grouping::Stacked | Grouping::Percent => {
                            let v = if g.grouping == Grouping::Percent {
                                v / totals.get(i).copied().filter(|t| *t > 0.0).unwrap_or(1.0)
                            } else {
                                v
                            };
                            let acc = if v >= 0.0 { pos.get_mut(i)? } else { neg.get_mut(i)? };
                            let from = *acc;
                            *acc += v;
                            Some((from, *acc))
                        }
                    }
                })
                .collect()
        })
        .collect()
}

// ---- drawing -----------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Rect {
    fn w(&self) -> f64 {
        self.x1 - self.x0
    }

    fn h(&self) -> f64 {
        self.y1 - self.y0
    }
}

struct Pen<'a> {
    cv: &'a mut Canvas,
    m: &'a Mat,
    angle: f64,
    left: usize,
}

impl Pen<'_> {
    /// Takes one thing from the chart's budget.
    fn take(&mut self) -> bool {
        if self.left == 0 {
            self.cv.full = true;
            return false;
        }
        self.left -= 1;
        true
    }

    fn path(&mut self, segs: &[Seg], fill: u32, stroke: u32, width: f64) {
        self.cv.path(segs, self.m, fill, stroke, width, false);
    }

    fn rect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, fill: u32) {
        let segs = [Seg::M(x0, y0), Seg::L(x1, y0), Seg::L(x1, y1), Seg::L(x0, y1), Seg::Z];
        self.path(&segs, fill, 0, 0.0);
    }

    fn line(&mut self, a: (f64, f64), b: (f64, f64), color: u32, width: f64) {
        self.path(&[Seg::M(a.0, a.1), Seg::L(b.0, b.1)], 0, color, width);
    }

    fn style(size: f64, color: u32, bold: bool) -> RunStyle {
        RunStyle { bold, ..RunStyle::plain(size, color) }
    }

    fn width(s: &str, size: f64, bold: bool) -> f64 {
        text_width(s, &Self::style(size, 0, bold))
    }

    /// Text with its baseline starting at `(x, base)`.
    fn text(&mut self, x: f64, base: f64, s: &str, size: f64, color: u32, bold: bool) {
        let st = Self::style(size, color, bold);
        let w = text_width(s, &st);
        put_text(self.cv, self.m, self.angle, x, base, 1.0, s, &st, w);
    }

    /// Text centred on `cx`.
    fn text_c(&mut self, cx: f64, base: f64, s: &str, size: f64, color: u32, bold: bool) {
        let w = Self::width(s, size, bold);
        self.text(cx - w / 2.0, base, s, size, color, bold);
    }

    /// Text ending at `right`.
    fn text_r(&mut self, right: f64, base: f64, s: &str, size: f64, color: u32) {
        let w = Self::width(s, size, false);
        self.text(right - w, base, s, size, color, false);
    }

    /// Text running upwards, its baseline starting at `(x, base)`.
    fn text_up(&mut self, x: f64, base: f64, s: &str, size: f64, color: u32) {
        let st = Self::style(size, color, false);
        let w = text_width(s, &st);
        put_text(self.cv, self.m, self.angle - FRAC_PI_2, x, base, 1.0, s, &st, w);
    }
}

/// `s` cut with an ellipsis to fit `max` points.
fn fit(s: &str, size: f64, max: f64) -> String {
    if Pen::width(s, size, false) <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    for c in s.chars() {
        let mut next = out.clone();
        next.push(c);
        next.push('…');
        if Pen::width(&next, size, false) > max {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { String::new() } else { format!("{out}…") }
}

struct Entry {
    name: String,
    color: u32,
    line: bool,
}

fn entries(c: &Chart) -> Vec<Entry> {
    if let Some(g) = c.groups.iter().find(|g| g.shape == Shape::Pie)
        && let Some(s) = g.series.first()
    {
        return (0..s.vals.len().min(MAX_POINTS))
            .map(|i| Entry {
                name: s.cats.get(i).cloned().unwrap_or_default(),
                color: slice_color(c, g, s, i),
                line: false,
            })
            .collect();
    }
    c.groups
        .iter()
        .flat_map(|g| {
            g.series.iter().map(|s| Entry {
                name: s.name.clone(),
                color: s.color,
                line: g.shape == Shape::Line,
            })
        })
        .collect()
}

/// The colour of the point `i` of a pie (or of a bar chart that varies colours by point).
fn slice_color(c: &Chart, g: &Group, s: &Series, i: usize) -> u32 {
    s.points.iter().find(|(p, _)| *p == i).map(|(_, col)| *col).unwrap_or_else(|| {
        if g.vary || g.shape == Shape::Pie { palette(&c.accent, i) } else { s.color }
    })
}

/// Draws a chart in the frame `0..w × 0..h` that `m` maps to the page.
pub(super) fn draw(c: &Chart, w: f64, h: f64, m: &Mat, cv: &mut Canvas) {
    let mut pen = Pen { cv, m, angle: m.b.atan2(m.a), left: MAX_DRAWN };
    if let Some(f) = c.fill {
        pen.rect(0.0, 0.0, w, h, f);
    }
    if w < 40.0 || h < 30.0 {
        return;
    }
    let fs = c.size;
    let mut area = Rect { x0: PAD, y0: PAD, x1: w - PAD, y1: h - PAD };

    if let Some((text, size)) = &c.title {
        let t = fit(text, *size, area.w());
        pen.text_c(w / 2.0, area.y0 + size, &t, *size, c.text, false);
        area.y0 += size * 1.3 + 4.0;
    }
    if let Some(side) = c.legend {
        legend(&mut pen, c, side, &mut area);
    }
    if area.w() < 20.0 || area.h() < 20.0 {
        return;
    }
    match c.groups.iter().find(|g| g.shape == Shape::Pie) {
        Some(g) => pie(&mut pen, c, g, area),
        None => cartesian(&mut pen, c, area, fs),
    }
}

fn legend(pen: &mut Pen<'_>, c: &Chart, side: Side, area: &mut Rect) {
    let fs = c.size;
    let es = entries(c);
    if es.is_empty() {
        return;
    }
    let swatch = 12.0;
    let width_of = |e: &Entry| swatch + 4.0 + Pen::width(&e.name, fs, false);
    let draw_one = |pen: &mut Pen<'_>, e: &Entry, x: f64, y: f64| {
        if e.line {
            pen.line((x, y), (x + swatch, y), e.color, 2.0);
            circle(pen, x + swatch / 2.0, y, 2.2, e.color);
        } else {
            pen.rect(x, y - 4.0, x + 8.0, y + 4.0, e.color);
        }
        pen.text(x + swatch + 4.0, y + fs * 0.35, &e.name, fs, c.text, false);
    };
    match side {
        Side::Top | Side::Bottom => {
            // Rows of entries, wrapped to the width.
            let avail = area.w();
            let mut rows: Vec<Vec<&Entry>> = vec![Vec::new()];
            let mut used = 0.0;
            for e in &es {
                let ew = width_of(e).min(avail) + 14.0;
                if used + ew > avail + 14.0 && used > 0.0 {
                    rows.push(Vec::new());
                    used = 0.0;
                }
                used += ew;
                if let Some(r) = rows.last_mut() {
                    r.push(e);
                }
            }
            rows.truncate(4);
            let pitch = fs + 6.0;
            let total = pitch * fl(rows.len()) + 2.0;
            let top = if side == Side::Top { area.y0 } else { area.y1 - total };
            for (ri, row) in rows.iter().enumerate() {
                let rw: f64 = row.iter().map(|e| width_of(e).min(avail) + 14.0).sum::<f64>() - 14.0;
                let mut x = area.x0 + (avail - rw).max(0.0) / 2.0;
                let y = top + pitch * (fl(ri) + 0.5) + 1.0;
                for e in row {
                    if pen.take() {
                        draw_one(pen, e, x, y);
                    }
                    x += width_of(e).min(avail) + 14.0;
                }
            }
            if side == Side::Top {
                area.y0 += total;
            } else {
                area.y1 -= total;
            }
        }
        Side::Left | Side::Right => {
            let widest = es.iter().map(width_of).fold(0.0, f64::max).min(area.w() * 0.4);
            let total = widest + 12.0;
            let pitch = fs + 6.0;
            let shown = es.len().min(steps((area.h() / pitch).floor()));
            let left = if side == Side::Left { area.x0 } else { area.x1 - total + 6.0 };
            let mut y = area.y0 + (area.h() - pitch * fl(shown)).max(0.0) / 2.0 + pitch / 2.0;
            for e in es.iter().take(shown) {
                if pen.take() {
                    let trimmed = Entry {
                        name: fit(&e.name, fs, (widest - swatch - 4.0).max(10.0)),
                        color: e.color,
                        line: e.line,
                    };
                    draw_one(pen, &trimmed, left, y);
                }
                y += pitch;
            }
            if side == Side::Left {
                area.x0 += total;
            } else {
                area.x1 -= total;
            }
        }
    }
}

fn circle(pen: &mut Pen<'_>, cx: f64, cy: f64, r: f64, fill: u32) {
    let k = 0.552_284_75 * r;
    let segs = [
        Seg::M(cx + r, cy),
        Seg::C(cx + r, cy + k, cx + k, cy + r, cx, cy + r),
        Seg::C(cx - k, cy + r, cx - r, cy + k, cx - r, cy),
        Seg::C(cx - r, cy - k, cx - k, cy - r, cx, cy - r),
        Seg::C(cx + k, cy - r, cx + r, cy - k, cx + r, cy),
        Seg::Z,
    ];
    pen.path(&segs, fill, 0, 0.0);
}

/// The plot of a chart with axes: where the value and the category axis run.
struct Plot {
    r: Rect,
    horizontal: bool,
    min: f64,
    max: f64,
    n: usize,
    reverse: bool,
    on_ticks: bool,
}

impl Plot {
    /// The position of the value `v` along the value axis (y for columns, x for bars).
    fn along(&self, v: f64) -> f64 {
        let f = ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        if self.horizontal { self.r.x0 + f * self.r.w() } else { self.r.y1 - f * self.r.h() }
    }

    fn extent(&self) -> f64 {
        if self.horizontal { self.r.h() } else { self.r.w() }
    }

    fn slot(&self) -> f64 {
        self.extent() / fl(self.n.max(1))
    }

    /// The start of the slot of category `i` along the category axis.
    fn slot_start(&self, i: usize) -> f64 {
        let k = if self.horizontal != self.reverse {
            fl(self.n - 1 - i.min(self.n - 1))
        } else {
            fl(i)
        };
        let origin = if self.horizontal { self.r.y0 } else { self.r.x0 };
        origin + k * self.slot()
    }

    /// Where a line or area point of category `i` sits along the category axis.
    fn centre(&self, i: usize) -> f64 {
        if self.on_ticks && self.n > 1 {
            let k = if self.horizontal != self.reverse {
                fl(self.n - 1 - i.min(self.n - 1))
            } else {
                fl(i)
            };
            let origin = if self.horizontal { self.r.y0 } else { self.r.x0 };
            origin + k * self.extent() / fl(self.n - 1)
        } else {
            self.slot_start(i) + self.slot() / 2.0
        }
    }

    /// A point from its place on the category axis and its value.
    fn pt(&self, cat: f64, v: f64) -> (f64, f64) {
        if self.horizontal { (self.along(v), cat) } else { (cat, self.along(v)) }
    }

    /// The part `off..off + width` of slot `i`, counted from the side the first series sits on.
    fn span(&self, i: usize, off: f64, width: f64) -> (f64, f64) {
        let start = self.slot_start(i);
        if self.horizontal && !self.reverse {
            let c1 = start + self.slot() - off;
            (c1 - width, c1)
        } else {
            (start + off, start + off + width)
        }
    }
}

fn cartesian(pen: &mut Pen<'_>, c: &Chart, area: Rect, fs: f64) {
    let groups: Vec<&Group> = c.groups.iter().filter(|g| g.shape != Shape::Pie).collect();
    let n = groups
        .iter()
        .flat_map(|g| g.series.iter())
        .map(|s| s.vals.len().max(s.cats.len()))
        .max()
        .unwrap_or(0)
        .min(MAX_POINTS);
    if n == 0 {
        return;
    }
    let horizontal = groups.iter().any(|g| g.shape == Shape::Bar && g.horizontal);
    let has_bars = groups.iter().any(|g| g.shape == Shape::Bar);
    let has_filled = groups.iter().any(|g| matches!(g.shape, Shape::Bar | Shape::Area));
    let percent = groups.iter().any(|g| g.grouping == Grouping::Percent);
    let all_spans: Vec<Spans> = groups.iter().map(|g| spans(g, n)).collect();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (a, b) in all_spans.iter().flatten().flatten().flatten() {
        lo = lo.min(*a).min(*b);
        hi = hi.max(*a).max(*b);
    }
    if !(lo.is_finite() && hi.is_finite()) {
        return;
    }
    let cats: Vec<String> = (0..n)
        .map(|i| {
            groups
                .iter()
                .flat_map(|g| g.series.iter())
                .find_map(|s| s.cats.get(i).filter(|t| !t.is_empty()).cloned())
                .unwrap_or_else(|| (i + 1).to_string())
        })
        .collect();
    let value_fmt = c.val.fmt.clone().or_else(|| {
        if percent {
            Some("0%".to_owned())
        } else {
            groups.iter().flat_map(|g| g.series.iter()).find_map(|s| s.fmt.clone())
        }
    });

    // The value axis needs its labels' size before the plot's size is known: ticks are chosen for
    // an estimate of the length and chosen again for the real one.
    let along_len = if horizontal { area.w() * 0.7 } else { area.h() * 0.7 };
    let target = |len: f64| steps((len / (fs * 3.4)).clamp(3.0, 10.0));
    let est = scale(lo, hi, &c.val, has_filled, percent, target(along_len));
    let widest_val = est
        .ticks
        .iter()
        .map(|t| Pen::width(&format(value_fmt.as_deref(), *t), fs, false))
        .fold(0.0, f64::max);
    let widest_cat = cats.iter().map(|t| Pen::width(t, fs, false)).fold(0.0, f64::max);

    let title_room = fs * 1.5;
    let (mut left, mut bottom) = (4.0, 4.0);
    if horizontal {
        if !c.cat.deleted {
            left += widest_cat.min(area.w() * 0.35) + 6.0;
        }
        if !c.val.deleted {
            bottom += fs + 6.0;
        }
        if c.cat.title.is_some() {
            left += title_room;
        }
        if c.val.title.is_some() {
            bottom += title_room;
        }
    } else {
        if !c.val.deleted {
            left += widest_val + 6.0;
        }
        if !c.cat.deleted {
            bottom += fs + 6.0;
        }
        if c.val.title.is_some() {
            left += title_room;
        }
        if c.cat.title.is_some() {
            bottom += title_room;
        }
    }
    let r = Rect {
        x0: area.x0 + left,
        y0: area.y0 + fs * 0.6,
        x1: area.x1 - 6.0 - if horizontal { widest_val / 2.0 } else { 0.0 },
        y1: area.y1 - bottom,
    };
    if r.w() < 20.0 || r.h() < 20.0 {
        return;
    }
    let sc =
        scale(lo, hi, &c.val, has_filled, percent, target(if horizontal { r.w() } else { r.h() }));
    let plot = Plot {
        r,
        horizontal,
        min: sc.min,
        max: sc.max,
        n,
        reverse: c.cat.reverse,
        on_ticks: c.cat.on_ticks && !has_bars,
    };

    if let Some(f) = c.plot_fill {
        pen.rect(r.x0, r.y0, r.x1, r.y1, f);
    }
    // Grid lines and value labels.
    for t in &sc.ticks {
        let a = plot.along(*t);
        if c.val.grid {
            let (p, q) = if horizontal { ((a, r.y0), (a, r.y1)) } else { ((r.x0, a), (r.x1, a)) };
            pen.line(p, q, GRID, 0.75);
        }
        if !c.val.deleted {
            let label = format(value_fmt.as_deref(), *t);
            if horizontal {
                pen.text_c(a, r.y1 + fs + 3.0, &label, fs, c.text, false);
            } else {
                pen.text_r(r.x0 - 5.0, a + fs * 0.35, &label, fs, c.text);
            }
        }
    }
    // The category axis crosses at zero.
    let at = plot.along(0.0f64.clamp(sc.min, sc.max));
    if horizontal {
        pen.line((at, r.y0), (at, r.y1), AXIS, 0.75);
    } else {
        pen.line((r.x0, at), (r.x1, at), AXIS, 0.75);
    }

    // The series: areas under bars under lines.
    for shape in [Shape::Area, Shape::Bar, Shape::Line] {
        for (g, sp) in groups.iter().zip(&all_spans) {
            if g.shape == shape {
                match shape {
                    Shape::Area => areas(pen, &plot, g, sp),
                    Shape::Bar => bars(pen, c, &plot, g, sp, fs),
                    Shape::Line => lines(pen, c, &plot, g, sp, fs),
                    Shape::Pie => {}
                }
            }
        }
    }

    // Category labels.
    if !c.cat.deleted {
        let slot = plot.slot();
        if horizontal {
            let step = steps(((fs + 3.0) / slot).ceil());
            for (i, t) in cats.iter().enumerate().step_by(step) {
                let label = fit(t, fs, (left - title_room.min(left) - 8.0).max(10.0));
                pen.text_r(r.x0 - 5.0, plot.centre(i) + fs * 0.35, &label, fs, c.text);
            }
        } else {
            let step = steps(((widest_cat + 8.0) / slot).ceil());
            for (i, t) in cats.iter().enumerate().step_by(step) {
                let label = if step == 1 { fit(t, fs, slot * 1.1 + 4.0) } else { t.clone() };
                pen.text_c(plot.centre(i), r.y1 + fs + 3.0, &label, fs, c.text, false);
            }
        }
    }
    // Axis titles.
    if let Some(t) = &c.cat.title {
        let t = fit(t, fs, r.w());
        if horizontal {
            let w = Pen::width(&t, fs, false);
            pen.text_up(area.x0 + fs, (r.y0 + r.y1) / 2.0 + w / 2.0, &t, fs, c.text);
        } else {
            pen.text_c((r.x0 + r.x1) / 2.0, area.y1 - 2.0, &t, fs, c.text, false);
        }
    }
    if let Some(t) = &c.val.title {
        if horizontal {
            pen.text_c((r.x0 + r.x1) / 2.0, area.y1 - 2.0, t, fs, c.text, false);
        } else {
            let t = fit(t, fs, r.h());
            let w = Pen::width(&t, fs, false);
            pen.text_up(area.x0 + fs, (r.y0 + r.y1) / 2.0 + w / 2.0, &t, fs, c.text);
        }
    }
}

fn bars(
    pen: &mut Pen<'_>,
    c: &Chart,
    p: &Plot,
    g: &Group,
    sp: &[Vec<Option<(f64, f64)>>],
    fs: f64,
) {
    let stacked = g.grouping != Grouping::Standard;
    let count = if stacked { 1.0 } else { fl(g.series.len()).max(1.0) };
    let overlap = if stacked { 1.0 } else { g.overlap };
    let cluster_units = count - (count - 1.0) * overlap;
    let bar = p.slot() / (cluster_units + g.gap);
    let first = (p.slot() - bar * cluster_units) / 2.0;
    for (j, (s, row)) in g.series.iter().zip(sp).enumerate() {
        let k = if stacked { 0.0 } else { fl(j) };
        for (i, span) in row.iter().enumerate() {
            let Some((a, b)) = *span else { continue };
            if !pen.take() {
                return;
            }
            let (c0, c1) = p.span(i, first + k * bar * (1.0 - overlap), bar);
            let (va, vb) = (p.along(a), p.along(b));
            let color =
                if g.vary && g.series.len() == 1 { slice_color(c, g, s, i) } else { s.color };
            let (x0, y0, x1, y1) =
                if p.horizontal { (va, c0, vb, c1) } else { (c0, va.min(vb), c1, va.max(vb)) };
            pen.rect(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1), color);
            if s.labels.any() && pen.left > 0 {
                let Some(v) = s.vals.get(i).copied().flatten() else { continue };
                let text = label_text(s, v, i, None);
                if text.is_empty() {
                    continue;
                }
                let tw = Pen::width(&text, fs, false);
                let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                if stacked {
                    pen.text_c(cx, cy + fs * 0.35, &text, fs, on_colour(color), false);
                } else if p.horizontal {
                    let x = if v >= 0.0 { x1.max(x0) + 3.0 } else { x0.min(x1) - 3.0 - tw };
                    pen.text(x, cy + fs * 0.35, &text, fs, c.text, false);
                } else {
                    let base = if v >= 0.0 { y0.min(y1) - 3.0 } else { y0.max(y1) + fs + 1.0 };
                    pen.text_c(cx, base, &text, fs, c.text, false);
                }
            }
        }
    }
}

/// The text of a data label of the value `v` (`pct` for a pie, the part of the whole).
fn label_text(s: &Series, v: f64, i: usize, pct: Option<f64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.labels.cat
        && let Some(cat) = s.cats.get(i).filter(|t| !t.is_empty())
    {
        parts.push(cat.clone());
    }
    if s.labels.val {
        parts.push(format(s.labels.fmt.as_deref().or(s.fmt.as_deref()), v));
    }
    if s.labels.pct
        && let Some(p) = pct
    {
        parts.push(format(Some("0%"), p));
    }
    parts.join(", ")
}

fn areas(pen: &mut Pen<'_>, p: &Plot, g: &Group, sp: &[Vec<Option<(f64, f64)>>]) {
    for (s, row) in g.series.iter().zip(sp) {
        if !pen.take() {
            return;
        }
        let mut top: Vec<(f64, f64)> = Vec::new();
        let mut base: Vec<(f64, f64)> = Vec::new();
        for (i, span) in row.iter().enumerate() {
            let (a, b) = span.unwrap_or((0.0, 0.0));
            top.push(p.pt(p.centre(i), b));
            base.push(p.pt(p.centre(i), a));
        }
        let Some(&(sx, sy)) = top.first() else { continue };
        let mut segs = vec![Seg::M(sx, sy)];
        segs.extend(top.iter().skip(1).map(|&(x, y)| Seg::L(x, y)));
        segs.extend(base.iter().rev().map(|&(x, y)| Seg::L(x, y)));
        segs.push(Seg::Z);
        pen.path(&segs, s.color, 0, 0.0);
    }
}

fn lines(
    pen: &mut Pen<'_>,
    c: &Chart,
    p: &Plot,
    g: &Group,
    sp: &[Vec<Option<(f64, f64)>>],
    fs: f64,
) {
    for (s, row) in g.series.iter().zip(sp) {
        // Runs of points; a missing value breaks the line.
        let mut runs: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
        for (i, span) in row.iter().enumerate() {
            match span {
                Some((_, b)) => {
                    if let Some(run) = runs.last_mut() {
                        run.push(p.pt(p.centre(i), *b));
                    }
                }
                None => {
                    if runs.last().is_some_and(|r| !r.is_empty()) {
                        runs.push(Vec::new());
                    }
                }
            }
        }
        let width = s.line_width.unwrap_or(2.25);
        for run in &runs {
            if !pen.take() {
                return;
            }
            let segs = if s.smooth { smooth(run) } else { polyline(run) };
            pen.path(&segs, 0, s.color, width);
        }
        if s.marker.unwrap_or(g.markers) {
            for run in &runs {
                for &(x, y) in run {
                    if !pen.take() {
                        return;
                    }
                    circle(pen, x, y, 2.2 + width * 0.4, s.color);
                }
            }
        }
        if s.labels.any() {
            for (i, span) in row.iter().enumerate() {
                let (Some((_, b)), Some(v)) = (span, s.vals.get(i).copied().flatten()) else {
                    continue;
                };
                let text = label_text(s, v, i, None);
                if text.is_empty() || !pen.take() {
                    continue;
                }
                let (x, y) = p.pt(p.centre(i), *b);
                pen.text_c(x, y - 6.0, &text, fs, c.text, false);
            }
        }
    }
}

fn polyline(pts: &[(f64, f64)]) -> Vec<Seg> {
    let mut it = pts.iter();
    let Some(&(x, y)) = it.next() else { return Vec::new() };
    let mut segs = vec![Seg::M(x, y)];
    segs.extend(it.map(|&(x, y)| Seg::L(x, y)));
    segs
}

/// A curve through the points (Catmull-Rom as Bézier segments).
fn smooth(pts: &[(f64, f64)]) -> Vec<Seg> {
    let Some(&(sx, sy)) = pts.first() else { return Vec::new() };
    let mut segs = vec![Seg::M(sx, sy)];
    let at = |i: usize| pts.get(i).or_else(|| pts.last()).copied().unwrap_or((sx, sy));
    for i in 0..pts.len().saturating_sub(1) {
        let p0 = at(i.saturating_sub(1));
        let (p1, p2, p3) = (at(i), at(i + 1), at(i + 2));
        segs.push(Seg::C(
            p1.0 + (p2.0 - p0.0) / 6.0,
            p1.1 + (p2.1 - p0.1) / 6.0,
            p2.0 - (p3.0 - p1.0) / 6.0,
            p2.1 - (p3.1 - p1.1) / 6.0,
            p2.0,
            p2.1,
        ));
    }
    segs
}

/// The point at angle `t` (radians clockwise from twelve o'clock) on a circle.
fn on_circle(cx: f64, cy: f64, r: f64, t: f64) -> (f64, f64) {
    (cx + r * t.sin(), cy - r * t.cos())
}

/// Cubic segments along the arc of the circle from `a0` to `a1` (either direction), continuing
/// from the point at `a0`.
fn arc(cx: f64, cy: f64, r: f64, a0: f64, a1: f64, out: &mut Vec<Seg>) {
    let parts = steps(((a1 - a0).abs() / FRAC_PI_2).ceil());
    let dt = (a1 - a0) / fl(parts);
    let k = 4.0 / 3.0 * (dt / 4.0).tan();
    for i in 0..parts {
        let (t0, t1) = (a0 + dt * fl(i), a0 + dt * fl(i + 1));
        let (p0, p3) = (on_circle(cx, cy, r, t0), on_circle(cx, cy, r, t1));
        out.push(Seg::C(
            p0.0 + k * r * t0.cos(),
            p0.1 + k * r * t0.sin(),
            p3.0 - k * r * t1.cos(),
            p3.1 - k * r * t1.sin(),
            p3.0,
            p3.1,
        ));
    }
}

fn pie(pen: &mut Pen<'_>, c: &Chart, g: &Group, area: Rect) {
    let Some(s) = g.series.first() else { return };
    let vals: Vec<(usize, f64)> = s
        .vals
        .iter()
        .take(500)
        .enumerate()
        .filter_map(|(i, v)| Some((i, (*v)?)))
        .filter(|(_, v)| v.is_finite() && *v > 0.0)
        .collect();
    let total: f64 = vals.iter().map(|(_, v)| v).sum();
    if total <= 0.0 {
        return;
    }
    let (cx, cy) = ((area.x0 + area.x1) / 2.0, (area.y0 + area.y1) / 2.0);
    let r = (area.w().min(area.h()) / 2.0 - 2.0).max(5.0);
    let inner = r * g.hole;
    let fs = c.size;
    let mut at = g.first;
    for (i, v) in vals {
        if !pen.take() {
            return;
        }
        let share = v / total;
        let sweep = (share * TAU).min(TAU);
        let color = slice_color(c, g, s, i);
        let mut segs: Vec<Seg> = Vec::new();
        if share > 0.9999 {
            let (x, y) = on_circle(cx, cy, r, at);
            segs.push(Seg::M(x, y));
            arc(cx, cy, r, at, at + TAU, &mut segs);
            segs.push(Seg::Z);
            if inner > 0.0 {
                let (x, y) = on_circle(cx, cy, inner, at);
                segs.push(Seg::M(x, y));
                arc(cx, cy, inner, at, at - TAU, &mut segs);
                segs.push(Seg::Z);
            }
        } else {
            let (x, y) = on_circle(cx, cy, r, at);
            if inner > 0.0 {
                segs.push(Seg::M(x, y));
                arc(cx, cy, r, at, at + sweep, &mut segs);
                let (x, y) = on_circle(cx, cy, inner, at + sweep);
                segs.push(Seg::L(x, y));
                arc(cx, cy, inner, at + sweep, at, &mut segs);
            } else {
                segs.push(Seg::M(cx, cy));
                segs.push(Seg::L(x, y));
                arc(cx, cy, r, at, at + sweep, &mut segs);
            }
            segs.push(Seg::Z);
        }
        // Both rings of a full doughnut turn the same way round, so they are filled even-odd.
        if share > 0.9999 && inner > 0.0 {
            pen.cv.path(&segs, pen.m, color, WHITE, 1.0, true);
        } else {
            pen.path(&segs, color, WHITE, 1.0);
        }
        if s.labels.any() && share >= 0.03 && pen.left > 0 {
            let text = label_text(s, v, i, Some(share));
            if !text.is_empty() {
                let mid = at + sweep / 2.0;
                let radius = if inner > 0.0 { (r + inner) / 2.0 } else { r * 0.64 };
                let (x, y) = on_circle(cx, cy, radius, mid);
                pen.text_c(x, y + fs * 0.35, &text, fs, on_colour(color), false);
            }
        }
        at += sweep;
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::float_cmp, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_one_two_or_five_times_a_power_of_ten() {
        assert!((nice_step(0.7) - 1.0).abs() < 1e-12);
        assert!((nice_step(1.3) - 2.0).abs() < 1e-12);
        assert!((nice_step(3.0) - 5.0).abs() < 1e-12);
        assert!((nice_step(7.0) - 10.0).abs() < 1e-12);
        assert!((nice_step(0.03) - 0.05).abs() < 1e-12);
        assert!((nice_step(f64::NAN) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_bar_axis_starts_at_zero_and_ends_on_a_tick_above_the_data() {
        let sc = scale(3.0, 47.0, &Axis::default(), true, false, 5);
        assert!((sc.min - 0.0).abs() < 1e-9, "{}", sc.min);
        assert!(sc.max >= 47.0 && sc.max <= 60.0, "{}", sc.max);
        assert!(sc.ticks.len() >= 4 && sc.ticks.len() <= 12);
    }

    #[test]
    fn a_line_axis_may_start_above_zero_when_the_data_is_far_from_it() {
        let sc = scale(100.0, 104.0, &Axis::default(), false, false, 5);
        assert!(sc.min > 50.0, "{}", sc.min);
        let sc = scale(10.0, 104.0, &Axis::default(), false, false, 5);
        assert!((sc.min - 0.0).abs() < 1e-9);
    }

    #[test]
    fn the_axis_of_the_document_wins() {
        let axis = Axis { min: Some(10.0), max: Some(50.0), major: Some(10.0), ..Axis::default() };
        let sc = scale(0.0, 1000.0, &axis, true, false, 5);
        assert_eq!(sc.ticks, vec![10.0, 20.0, 30.0, 40.0, 50.0]);
    }

    #[test]
    fn an_axis_never_has_more_ticks_than_the_limit() {
        let axis = Axis { major: Some(1e-9), ..Axis::default() };
        let sc = scale(0.0, 1.0e6, &axis, true, false, 5);
        assert!(sc.ticks.len() <= MAX_TICKS + 1);
    }

    #[test]
    fn stacked_values_pile_up_and_percent_ones_fill_the_unit() {
        let s = |v: &[f64]| Series {
            name: String::new(),
            color: 0,
            points: Vec::new(),
            line_width: None,
            cats: Vec::new(),
            vals: v.iter().map(|x| Some(*x)).collect(),
            fmt: None,
            labels: Labels::default(),
            marker: None,
            smooth: false,
        };
        let mut g = Group {
            shape: Shape::Bar,
            horizontal: false,
            grouping: Grouping::Stacked,
            gap: 1.0,
            overlap: 1.0,
            hole: 0.0,
            first: 0.0,
            markers: false,
            vary: false,
            series: vec![s(&[1.0, -2.0]), s(&[3.0, 4.0])],
        };
        let sp = spans(&g, 2);
        assert_eq!(sp[0][0], Some((0.0, 1.0)));
        assert_eq!(sp[1][0], Some((1.0, 4.0)));
        assert_eq!(sp[0][1], Some((0.0, -2.0)));
        assert_eq!(sp[1][1], Some((0.0, 4.0)));
        g.grouping = Grouping::Percent;
        let sp = spans(&g, 2);
        assert_eq!(sp[1][0], Some((0.25, 1.0)));
    }

    #[test]
    fn long_labels_are_cut_to_fit() {
        let s = fit("Quarterly revenue by region", 10.0, 60.0);
        assert!(s.ends_with('…'));
        assert!(Pen::width(&s, 10.0, false) <= 60.0);
        assert_eq!(fit("Q1", 10.0, 60.0), "Q1");
        assert_eq!(fit("anything", 10.0, 1.0), "");
    }

    #[test]
    fn the_colours_repeat_with_variations_after_the_sixth() {
        let a = OFFICE;
        assert_eq!(palette(&a, 0), OFFICE[0]);
        assert_ne!(palette(&a, 6), palette(&a, 0));
        assert_ne!(palette(&a, 12), palette(&a, 6));
        assert!(palette(&a, 600) >> 24 == 0xFF);
    }
}
