//! OpenDocument presentations (`.odp`) drawn as pages.
//!
//! The pages of `content.xml` are drawn over the shapes of their master page. Styles come from
//! the automatic styles of `content.xml` and the named and automatic styles of `styles.xml`;
//! attributes are looked up by their qualified names as ODF writes them (`svg:x`, `draw:fill`).

use std::collections::BTreeMap;

use super::presets::{self, arc_to};
use super::{
    Align, Anchor, Canvas, CellDraw, Mat, Node, Outcome, Para, Piece, Rendered, RunStyle, Seg,
    Spacing, TextBody, draw_cells, draw_laid, f32c, family_of, fin, layout, num, parse, side,
};
use crate::model::IMAGE_NONE;
use crate::opc::Package;
use crate::page::Prim;
use crate::{PreviewError, PreviewPolicy};

/// Elements whose text is kept.
const KEEP_TEXT: [&str; 7] = ["p", "span", "h", "a", "title", "desc", "s"];
/// Page size when the master page names none: 28 × 15.75 cm.
const DEFAULT_SIZE: (f64, f64) = (793.7, 446.4);
/// Most parent styles followed.
const MAX_STYLE_DEPTH: usize = 10;
/// Most rows, columns and cells of a table drawn.
const MAX_TABLE_LINE: usize = 500;
const MAX_TABLE_CELLS: usize = 10_000;

type Props = BTreeMap<String, String>;

// ---- lengths and colours -------------------------------------------------------------------------

/// A length such as `2.5cm` in points.
fn len(s: &str) -> Option<f64> {
    let s = s.trim();
    let at = s.find(|c: char| c.is_ascii_alphabetic() || c == '%').unwrap_or(s.len());
    let n = num(s.get(..at)?)?;
    let k = match s.get(at..)?.trim() {
        "cm" => 72.0 / 2.54,
        "mm" => 72.0 / 25.4,
        "in" => 72.0,
        "pt" => 1.0,
        "pc" => 12.0,
        "px" => 0.75,
        _ => return None,
    };
    Some(fin(n.clamp(-1.0e9, 1.0e9) * k))
}

/// A percentage such as `45%` as a fraction.
fn percent(s: &str) -> Option<f64> {
    num(s.trim().strip_suffix('%')?).map(|v| v / 100.0)
}

fn color(s: &str) -> Option<u32> {
    let s = s.trim();
    if s == "transparent" {
        return Some(0);
    }
    let h = s.strip_prefix('#')?;
    (h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(h, 16).ok())
        .flatten()
        .map(|v| 0xFF00_0000 | v)
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("true" | "1"))
}

// ---- styles ----------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Label {
    Bullet(String),
    Number { format: String, prefix: String, suffix: String },
}

#[derive(Debug, Clone)]
struct ListLevel {
    label: Label,
    space_before: f64,
    label_width: f64,
}

struct Styles<'a> {
    by: BTreeMap<(String, String), (String, Props)>,
    lists: BTreeMap<String, BTreeMap<usize, ListLevel>>,
    gradients: BTreeMap<String, String>,
    layouts: BTreeMap<String, (Option<f64>, Option<f64>)>,
    masters: BTreeMap<String, &'a Node>,
}

const PROPERTY_ELEMENTS: [&str; 9] = [
    "graphic-properties",
    "text-properties",
    "paragraph-properties",
    "drawing-page-properties",
    "table-cell-properties",
    "table-column-properties",
    "table-row-properties",
    "table-properties",
    "list-level-properties",
];

impl<'a> Styles<'a> {
    fn load(trees: &[&'a Node]) -> Self {
        let mut s = Self {
            by: BTreeMap::new(),
            lists: BTreeMap::new(),
            gradients: BTreeMap::new(),
            layouts: BTreeMap::new(),
            masters: BTreeMap::new(),
        };
        for root in trees {
            for sect in &root.kids {
                match sect.name.as_str() {
                    "automatic-styles" | "styles" | "master-styles" => s.section(sect),
                    _ => {}
                }
            }
        }
        s
    }

    fn section(&mut self, sect: &'a Node) {
        for st in &sect.kids {
            match st.name.as_str() {
                "style" | "default-style" => {
                    let fam = st.attr("style:family").unwrap_or_default().to_owned();
                    let name = if st.name == "default-style" {
                        String::new()
                    } else {
                        st.attr("style:name").unwrap_or_default().to_owned()
                    };
                    let mut props = Props::new();
                    for k in &st.kids {
                        if PROPERTY_ELEMENTS.contains(&k.name.as_str())
                            || k.name.ends_with("-properties")
                        {
                            for (n, v) in k.attrs_iter() {
                                props.insert(n.to_owned(), v.to_owned());
                            }
                        }
                    }
                    let parent = st.attr("style:parent-style-name").unwrap_or_default().to_owned();
                    self.by.insert((fam, name), (parent, props));
                }
                "list-style" => {
                    let name = st.attr("text:name").or(st.attr("style:name")).unwrap_or_default();
                    let mut levels = BTreeMap::new();
                    for lv in &st.kids {
                        let n = lv
                            .attr("text:level")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 10);
                        let label = match lv.name.as_str() {
                            "list-level-style-bullet" => {
                                let c = lv.attr("text:bullet-char").unwrap_or("•");
                                // Symbol-font code points would draw as boxes.
                                let c = c.chars().next().filter(|c| {
                                    !('\u{E000}'..='\u{F8FF}').contains(c) && *c != '●'
                                });
                                Label::Bullet(c.map_or_else(|| "•".to_owned(), String::from))
                            }
                            "list-level-style-number" => Label::Number {
                                format: lv.attr("style:num-format").unwrap_or("1").to_owned(),
                                prefix: lv.attr("style:num-prefix").unwrap_or_default().to_owned(),
                                suffix: lv.attr("style:num-suffix").unwrap_or_default().to_owned(),
                            },
                            "list-level-style-image" => Label::Bullet("•".to_owned()),
                            _ => continue,
                        };
                        let pr = lv.child("list-level-properties");
                        let get = |k: &str| pr.and_then(|p| p.attr(k)).and_then(len);
                        levels.insert(
                            n,
                            ListLevel {
                                label,
                                space_before: get("text:space-before").unwrap_or(0.0).max(0.0),
                                label_width: get("text:min-label-width").unwrap_or(17.0).max(0.0),
                            },
                        );
                    }
                    self.lists.insert(name.to_owned(), levels);
                }
                "gradient" => {
                    if let (Some(n), Some(c)) = (st.attr("draw:name"), st.attr("draw:start-color"))
                    {
                        self.gradients.insert(n.to_owned(), c.to_owned());
                    }
                }
                "page-layout" => {
                    let pr = st.child("page-layout-properties");
                    let get = |k: &str| pr.and_then(|p| p.attr(k)).and_then(len);
                    self.layouts.insert(
                        st.attr("style:name").unwrap_or_default().to_owned(),
                        (get("fo:page-width"), get("fo:page-height")),
                    );
                }
                "master-page" => {
                    self.masters.insert(st.attr("style:name").unwrap_or_default().to_owned(), st);
                }
                _ => {}
            }
        }
    }

    /// The properties of a style with those of its parents under them.
    fn resolve(&self, fam: &str, name: &str) -> Props {
        let mut chain: Vec<&Props> = Vec::new();
        let mut cur = name.to_owned();
        for _ in 0..MAX_STYLE_DEPTH {
            let Some((parent, props)) = self.by.get(&(fam.to_owned(), cur.clone())) else { break };
            chain.push(props);
            if parent.is_empty() || *parent == cur {
                break;
            }
            cur.clone_from(parent);
        }
        let mut out = self
            .by
            .get(&(fam.to_owned(), String::new()))
            .map(|(_, p)| p.clone())
            .unwrap_or_default();
        for p in chain.into_iter().rev() {
            for (k, v) in p {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }
}

/// Lets `over` win in `base`.
fn merge(base: &mut Props, over: Props) {
    for (k, v) in over {
        base.insert(k, v);
    }
}

// ---- text ----------------------------------------------------------------------------------------

/// Applies the text properties in `p` to a run style.
fn apply_text(st: &mut RunStyle, p: &Props) {
    if let Some(v) = p.get("fo:font-size") {
        if let Some(k) = percent(v) {
            st.size = (st.size * k).clamp(1.0, 2000.0);
        } else if let Some(l) = len(v) {
            st.size = l.clamp(1.0, 2000.0);
        }
    }
    if let Some(v) = p.get("fo:font-weight") {
        st.bold = v == "bold" || v.trim().parse::<u32>().is_ok_and(|n| n >= 600);
    }
    if let Some(v) = p.get("fo:font-style") {
        st.italic = matches!(v.as_str(), "italic" | "oblique");
    }
    if let Some(c) = p.get("fo:color").and_then(|v| color(v)) {
        st.color = c;
    }
    if let Some(v) = p.get("style:text-underline-style") {
        st.underline = v != "none";
    }
    if let Some(v) = p.get("style:text-line-through-style") {
        st.strike = v != "none";
    }
    if let Some(v) = p.get("style:text-position") {
        let first = v.split_whitespace().next().unwrap_or("");
        st.shift = if first == "super" {
            1
        } else if first == "sub" {
            -1
        } else {
            match percent(first) {
                Some(k) if k > 0.0 => 1,
                Some(k) if k < 0.0 => -1,
                _ => 0,
            }
        };
    }
    if let Some(face) = p.get("style:font-name").or(p.get("fo:font-family")) {
        let (mono, serif) = family_of(face);
        st.mono = mono;
        st.serif = serif;
    }
}

struct Draw<'a, 'p> {
    cv: &'a mut Canvas,
    styles: &'a Styles<'a>,
    pkg: &'a Package<'p>,
    reveal: bool,
    page_no: usize,
    master: Option<&'a Node>,
}

/// Where a paragraph sits in lists: the style name, the level and the counters of the levels.
struct ListPos<'n> {
    style: &'n str,
    level: usize,
}

impl Draw<'_, '_> {
    /// The properties of a frame or shape: presentation style, graphic style, text style.
    fn props_of(&self, n: &Node) -> Props {
        let mut p = Props::new();
        if let Some(s) = n.attr("presentation:style-name") {
            merge(&mut p, self.styles.resolve("presentation", s));
        }
        if let Some(s) = n.attr("draw:style-name") {
            merge(&mut p, self.styles.resolve("graphic", s));
        }
        if let Some(s) = n.attr("draw:text-style-name") {
            merge(&mut p, self.styles.resolve("paragraph", s));
        }
        p
    }

    fn text_body(&mut self, container: &Node, shape: &Props) -> Option<TextBody> {
        let mut base = RunStyle::plain(18.0, 0xFF00_0000);
        apply_text(&mut base, shape);
        let pad = |side: &str, default: f64| -> f64 {
            shape
                .get(&format!("fo:padding-{side}"))
                .or(shape.get("fo:padding"))
                .and_then(|v| len(v))
                .unwrap_or(default)
                .max(0.0)
        };
        let ins = [pad("left", 7.1), pad("top", 3.5), pad("right", 7.1), pad("bottom", 3.5)];
        let anchor = match shape.get("draw:textarea-vertical-align").map(String::as_str) {
            Some("middle") => Anchor::Middle,
            Some("bottom") => Anchor::Bottom,
            _ => Anchor::Top,
        };
        let mut body = TextBody {
            paras: Vec::new(),
            ins,
            anchor,
            wrap: shape.get("fo:wrap-option").map(String::as_str) != Some("no-wrap"),
            scale: 1.0,
        };
        let mut counters = [0u32; 10];
        self.collect(container, &base, shape, None, &mut counters, &mut body.paras, 0);
        body.paras
            .iter()
            .any(|p| p.pieces.iter().any(|pc| !pc.text.trim().is_empty()))
            .then_some(body)
    }

    /// Paragraphs and lists inside `n`.
    #[allow(clippy::too_many_arguments)]
    fn collect(
        &mut self,
        n: &Node,
        base: &RunStyle,
        shape: &Props,
        list: Option<ListPos<'_>>,
        counters: &mut [u32; 10],
        out: &mut Vec<Para>,
        depth: usize,
    ) {
        if depth > 12 || out.len() > 2_000 {
            return;
        }
        for k in &n.kids {
            match k.name.as_str() {
                "p" | "h" => out.push(self.para(k, base, shape, list.as_ref(), counters)),
                "list" => {
                    let style = k
                        .attr("text:style-name")
                        .or_else(|| list.as_ref().map(|l| l.style))
                        .unwrap_or_default();
                    let level = list.as_ref().map_or(1, |l| l.level + 1);
                    // Deeper levels restart when a shallower one goes on.
                    for c in counters.iter_mut().skip(level) {
                        *c = 0;
                    }
                    for item in k.kids_named("list-item").chain(k.kids_named("list-header")) {
                        let pos = ListPos { style, level };
                        self.collect(item, base, shape, Some(pos), counters, out, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    fn para(
        &mut self,
        p: &Node,
        base: &RunStyle,
        shape: &Props,
        list: Option<&ListPos<'_>>,
        counters: &mut [u32; 10],
    ) -> Para {
        let pstyle = p
            .attr("text:style-name")
            .map(|s| self.styles.resolve("paragraph", s))
            .unwrap_or_default();
        let mut st = base.clone();
        apply_text(&mut st, &pstyle);
        let mut pieces: Vec<Piece> = Vec::new();
        let mut at_start = true;
        self.runs(p, &st, &mut pieces, &mut at_start, 0);

        let get = |k: &str| pstyle.get(k).or(shape.get(k));
        let align = match get("fo:text-align").map(String::as_str) {
            Some("center") => Align::Center,
            Some("end" | "right") => Align::End,
            Some("justify") => Align::Justify,
            _ => Align::Start,
        };
        let mut left = get("fo:margin-left").and_then(|v| len(v)).unwrap_or(0.0).max(0.0);
        let mut indent = get("fo:text-indent").and_then(|v| len(v)).unwrap_or(0.0);
        let mut bullet = None;
        if let Some(pos) = list {
            let lvl = self
                .styles
                .lists
                .get(pos.style)
                .and_then(|l| l.get(&pos.level))
                .cloned()
                .unwrap_or_else(|| ListLevel {
                    label: Label::Bullet("•".to_owned()),
                    space_before: 17.0 * (f64::from(u32::try_from(pos.level).unwrap_or(1)) - 1.0),
                    label_width: 17.0,
                });
            left += lvl.space_before + lvl.label_width;
            indent = -lvl.label_width;
            let text = match &lvl.label {
                Label::Bullet(c) => c.clone(),
                Label::Number { format, prefix, suffix } => {
                    let slot = pos.level.min(9);
                    let n = counters.get_mut(slot).map(|c| {
                        *c = c.saturating_add(1);
                        *c
                    });
                    format!("{prefix}{}{suffix}", crate::listfmt::counter(format, n.unwrap_or(1)))
                }
            };
            if !text.is_empty() {
                bullet = Some(Piece { text, st: st.clone() });
            }
        }
        let spacing = match get("fo:line-height") {
            Some(v) => percent(v)
                .map(|k| Spacing::Pct(k.clamp(0.1, 10.0)))
                .or_else(|| len(v).map(|l| Spacing::Exact(l.clamp(1.0, 1000.0))))
                .unwrap_or(Spacing::Pct(1.0)),
            None => Spacing::Pct(1.0),
        };
        Para {
            pieces,
            align,
            left,
            indent,
            bullet,
            before: get("fo:margin-top").and_then(|v| len(v)).unwrap_or(0.0).max(0.0),
            after: get("fo:margin-bottom").and_then(|v| len(v)).unwrap_or(0.0).max(0.0),
            line: spacing,
            end_size: st.size,
        }
    }

    /// The runs of a paragraph or span. White space collapses as ODF says: runs of it are one
    /// space, and none starts a paragraph.
    fn runs(
        &mut self,
        n: &Node,
        st: &RunStyle,
        out: &mut Vec<Piece>,
        at_start: &mut bool,
        depth: usize,
    ) {
        if depth > 16 || out.len() > super::MAX_LINES * 8 {
            return;
        }
        for k in &n.kids {
            match k.name.as_str() {
                "#text" => {
                    let mut s = String::with_capacity(k.text.len());
                    for c in k.text.chars() {
                        if matches!(c, ' ' | '\t' | '\n' | '\r') {
                            if !*at_start {
                                s.push(' ');
                                *at_start = true;
                            }
                        } else {
                            s.push(c);
                            *at_start = false;
                        }
                    }
                    if !s.is_empty() {
                        let text = self.cv.tidy(&s);
                        out.push(Piece { text, st: st.clone() });
                    }
                }
                "span" => {
                    let mut inner = st.clone();
                    if let Some(s) = k.attr("text:style-name") {
                        apply_text(&mut inner, &self.styles.resolve("text", s));
                    }
                    self.runs(k, &inner, out, at_start, depth + 1);
                }
                "s" => {
                    let c = k
                        .attr("text:c")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(100);
                    out.push(Piece { text: " ".repeat(c), st: st.clone() });
                    *at_start = false;
                }
                "tab" => {
                    out.push(Piece { text: "    ".to_owned(), st: st.clone() });
                    *at_start = true;
                }
                "line-break" => {
                    out.push(Piece { text: "\n".to_owned(), st: st.clone() });
                    *at_start = true;
                }
                "page-number" => {
                    out.push(Piece { text: self.page_no.to_string(), st: st.clone() });
                    *at_start = false;
                }
                _ => self.runs(k, st, out, at_start, depth + 1),
            }
        }
    }

    // ---- geometry --------------------------------------------------------------------------------

    /// `x, y, width, height` of a shape in points, filling what is missing from the master
    /// page's frame of the same class.
    fn frame_rect(&self, n: &Node) -> Option<[f64; 4]> {
        let get = |k: &str| n.attr(k).and_then(len);
        let (x, y, w, h) = (get("svg:x"), get("svg:y"), get("svg:width"), get("svg:height"));
        if let (Some(w), Some(h)) = (w, h) {
            return Some([x.unwrap_or(0.0), y.unwrap_or(0.0), w.max(0.0), h.max(0.0)]);
        }
        // A placeholder without a position takes its master page counterpart's.
        let class = n.attr("presentation:class")?;
        let want = match class {
            "subtitle" | "outline" | "text" => ["outline", "subtitle"],
            other => [other, other],
        };
        let m = self.master?.kids.iter().find(|k| {
            k.name == "frame" && k.attr("presentation:class").is_some_and(|c| want.contains(&c))
        })?;
        let mg = |k: &str| m.attr(k).and_then(len);
        Some([
            x.or(mg("svg:x")).unwrap_or(0.0),
            y.or(mg("svg:y")).unwrap_or(0.0),
            w.or(mg("svg:width"))?.max(0.0),
            h.or(mg("svg:height"))?.max(0.0),
        ])
    }

    /// The transform of `draw:transform`, applied first function first.
    fn transform(n: &Node) -> Mat {
        let Some(t) = n.attr("draw:transform") else { return Mat::ID };
        let mut m = Mat::ID;
        let mut rest = t;
        for _ in 0..16 {
            let Some(open) = rest.find('(') else { break };
            let Some(close) = rest.find(')') else { break };
            if close < open {
                break;
            }
            let name = rest.get(..open).unwrap_or("").trim();
            let args: Vec<&str> = rest
                .get(open + 1..close)
                .unwrap_or("")
                .split([' ', ',', '\t'])
                .filter(|a| !a.is_empty())
                .collect();
            let angle = |i: usize| args.get(i).and_then(|a| num(a)).unwrap_or(0.0);
            let length = |i: usize| args.get(i).and_then(|a| len(a)).unwrap_or(0.0);
            let step = match name {
                "rotate" => Mat::rotate(angle(0)),
                "translate" => Mat::translate(length(0), length(1)),
                "scale" => Mat::scale(angle(0), if args.len() > 1 { angle(1) } else { angle(0) }),
                "skewX" => Mat { a: 1.0, b: 0.0, c: angle(0).tan(), d: 1.0, e: 0.0, f: 0.0 },
                "skewY" => Mat { a: 1.0, b: angle(0).tan(), c: 0.0, d: 1.0, e: 0.0, f: 0.0 },
                "matrix" => Mat {
                    a: angle(0),
                    b: angle(1),
                    c: angle(2),
                    d: angle(3),
                    e: length(4),
                    f: length(5),
                },
                _ => Mat::ID,
            };
            m = m.then(step);
            rest = rest.get(close + 1..).unwrap_or("");
        }
        m
    }

    /// The paint of a shape from its properties: fill, stroke, stroke width.
    fn paint(&mut self, p: &Props, line_default: bool) -> (u32, u32, f64) {
        let opacity = p.get("draw:opacity").and_then(|v| percent(v)).unwrap_or(1.0).clamp(0.0, 1.0);
        let fill = match p.get("draw:fill").map(String::as_str) {
            Some("solid") => p.get("draw:fill-color").and_then(|v| color(v)).unwrap_or(0xFF72_9FCF),
            Some("gradient") => {
                self.cv.approximated("a gradient is drawn with its first colour");
                p.get("draw:fill-gradient-name")
                    .and_then(|n| self.styles.gradients.get(n))
                    .and_then(|c| color(c))
                    .unwrap_or(0)
            }
            Some("hatch" | "bitmap") => {
                self.cv.approximated("a hatch or picture fill is not drawn");
                0
            }
            _ => 0,
        };
        let fill = super::fade(fill, opacity);
        let stroke_kind = p.get("draw:stroke").map(String::as_str);
        let stroked = match stroke_kind {
            Some("none") => false,
            Some("dash") => {
                self.cv.approximated("dashed lines are drawn solid");
                true
            }
            Some(_) => true,
            None => line_default,
        };
        let stroke = if stroked {
            p.get("svg:stroke-color").and_then(|v| color(v)).unwrap_or(0xFF00_0000)
        } else {
            0
        };
        let width = p.get("svg:stroke-width").and_then(|v| len(v)).unwrap_or(0.0);
        (fill, stroke, if width < 0.25 { 0.5 } else { width })
    }

    fn hidden_opacity(&mut self) -> f64 {
        self.cv.opacity
    }

    // ---- shapes ----------------------------------------------------------------------------------

    fn shapes(&mut self, parent: &Node, m: &Mat, depth: usize, master: bool) {
        for k in &parent.kids {
            if self.cv.full {
                return;
            }
            match k.name.as_str() {
                "g" => {
                    if depth >= super::MAX_GROUP_DEPTH {
                        self.cv.full = true;
                        continue;
                    }
                    if !self.cv.take_shape() {
                        continue;
                    }
                    let gm = Self::transform(k).then(*m);
                    self.shapes(k, &gm, depth + 1, master);
                }
                "frame" => self.frame(k, m, master),
                "rect" | "ellipse" | "circle" | "line" | "connector" | "polyline" | "polygon"
                | "path" | "custom-shape" | "caption" | "regular-polygon" => {
                    self.shape(k, m, master);
                }
                _ => {}
            }
        }
    }

    /// `true` for master page frames that stand for something the page itself supplies.
    fn is_furniture(n: &Node) -> bool {
        n.attr("presentation:class").is_some()
    }

    fn frame(&mut self, f: &Node, parent: &Mat, master: bool) {
        if master && Self::is_furniture(f) {
            return;
        }
        if !self.cv.take_shape() {
            return;
        }
        let Some([x, y, w, h]) = self.frame_rect(f) else { return };
        let m = Mat::translate(x, y).then(Self::transform(f)).then(*parent);
        let props = self.props_of(f);
        let opacity = self.hidden_opacity();
        if let Some(img) = f.child("image") {
            let href = img.attr("xlink:href").unwrap_or_default();
            let path = href.trim_start_matches("./");
            let external = href.contains("://") || href.starts_with("//");
            let id = if external || path.is_empty() {
                IMAGE_NONE
            } else {
                self.pkg.entry_id(path).unwrap_or(IMAGE_NONE)
            };
            let alt = ["title", "desc"]
                .iter()
                .filter_map(|n| f.child(n))
                .map(Node::own_text)
                .find(|t| !t.trim().is_empty())
                .or_else(|| f.attr("draw:name").map(str::to_owned))
                .unwrap_or_default();
            self.cv.image(&m, w, h, &alt, id);
            return;
        }
        if let Some(tbl) = f.child("table") {
            self.table(tbl, &Mat::translate(x, y).then(Self::transform(f)).then(*parent));
            return;
        }
        if let Some(tb) = f.child("text-box") {
            let (fill, stroke, width) = self.paint(&props, false);
            self.cv.path(&presets::rect(w, h), &m, fill, stroke, width, false);
            self.text(tb, &props, &m, w, h);
            return;
        }
        if ["object", "object-ole", "plugin", "applet", "floating-frame"]
            .iter()
            .any(|n| f.child(n).is_some())
        {
            self.cv.image(&m, w, h, "object", IMAGE_NONE);
        }
        let _ = opacity;
    }

    fn text(&mut self, container: &Node, props: &Props, m: &Mat, w: f64, h: f64) {
        let Some(tb) = self.text_body(container, props) else { return };
        let angle = m.b.atan2(m.a);
        let (mt, tw, th) = text_frame(m, w, h, angle);
        let laid = layout(&tb, tw, self.cv);
        draw_laid(self.cv, &tb, &laid, tw, th, &mt);
    }

    #[allow(clippy::too_many_lines)] // one arm per kind of shape
    fn shape(&mut self, n: &Node, parent: &Mat, master: bool) {
        if master && Self::is_furniture(n) {
            return;
        }
        if !self.cv.take_shape() {
            return;
        }
        let props = self.props_of(n);
        let tr = Self::transform(n);
        let get = |k: &str| n.attr(k).and_then(len);
        let kind = n.name.as_str();
        // The frame the shape lives in.
        let (x, y, w, h) = match kind {
            "line" | "connector" => {
                let (x1, y1) = (get("svg:x1").unwrap_or(0.0), get("svg:y1").unwrap_or(0.0));
                let (x2, y2) = (get("svg:x2").unwrap_or(0.0), get("svg:y2").unwrap_or(0.0));
                (x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs())
            }
            _ => match self.frame_rect(n) {
                Some([x, y, w, h]) => (x, y, w, h),
                None => return,
            },
        };
        let m = Mat::translate(x, y).then(tr).then(*parent);
        let mut line_only = false;
        let segs: Vec<Seg> = match kind {
            "rect" | "caption" => {
                let r = get("draw:corner-radius").or(get("svg:rx")).unwrap_or(0.0);
                if r > 0.0 { presets::round_rect(w, h, r) } else { presets::rect(w, h) }
            }
            "ellipse" | "circle" => {
                if n.attr("draw:kind").is_some_and(|k| k != "full") {
                    self.cv.approximated("an ellipse section or arc is drawn as a full ellipse");
                }
                presets::ellipse(w, h)
            }
            "line" | "connector" => {
                line_only = true;
                let (x1, y1) = (get("svg:x1").unwrap_or(0.0), get("svg:y1").unwrap_or(0.0));
                let (x2, y2) = (get("svg:x2").unwrap_or(0.0), get("svg:y2").unwrap_or(0.0));
                if kind == "connector" && n.attr("draw:type").is_some_and(|t| t != "line") {
                    self.cv.approximated("a connector is drawn as a straight line");
                }
                vec![Seg::M(x1 - x, y1 - y), Seg::L(x2 - x, y2 - y)]
            }
            "polyline" | "polygon" => {
                let Some(pts) = polyline_points(n, w, h) else { return };
                line_only = kind == "polyline";
                presets::poly(&pts, !line_only)
            }
            "path" => {
                let Some((segs, approx)) = path_data(n, w, h) else { return };
                if approx {
                    self.cv.approximated("a path uses commands that are only approximated");
                }
                segs
            }
            "custom-shape" => {
                let (segs, mirror) = self.custom(n, w, h);
                if let Some(mm) = mirror {
                    let fm = Mat::translate(-w / 2.0, -h / 2.0)
                        .then(mm)
                        .then(Mat::translate(w / 2.0, h / 2.0))
                        .then(m);
                    return self.finish_shape(n, &props, &fm, w, h, &segs, line_only);
                }
                segs
            }
            _ => presets::rect(w, h),
        };
        self.finish_shape(n, &props, &m, w, h, &segs, line_only);
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_shape(
        &mut self,
        n: &Node,
        props: &Props,
        m: &Mat,
        w: f64,
        h: f64,
        segs: &[Seg],
        line_only: bool,
    ) {
        let (fill, stroke, width) = self.paint(props, line_only || n.name == "path");
        self.cv.path(segs, m, if line_only { 0 } else { fill }, stroke, width, false);
        let text_m = if n.name == "custom-shape" || n.name == "rect" || n.name == "caption" {
            // Text is not mirrored with the shape.
            Mat { a: m.a.abs().copysign(1.0), ..*m }
        } else {
            *m
        };
        self.text(n, props, &text_m, w, h);
    }

    /// The outline of a custom shape and the mirroring it asks for.
    fn custom(&mut self, n: &Node, w: f64, h: f64) -> (Vec<Seg>, Option<Mat>) {
        let Some(g) = n.child("enhanced-geometry") else {
            self.cv.approximated("a custom shape without geometry is drawn as its frame");
            return (presets::rect(w, h), None);
        };
        let mirror_h = truthy(g.attr("draw:mirror-horizontal"));
        let mirror_v = truthy(g.attr("draw:mirror-vertical"));
        let mirror = (mirror_h || mirror_v).then(|| {
            Mat::scale(if mirror_h { -1.0 } else { 1.0 }, if mirror_v { -1.0 } else { 1.0 })
        });
        let ty = g.attr("draw:type").unwrap_or_default();
        let preset = match ty {
            "rectangle" | "ooxml-rect" => Some("rect"),
            "round-rectangle" | "ooxml-roundRect" => Some("roundRect"),
            "ellipse" | "ooxml-ellipse" => Some("ellipse"),
            "diamond" | "ooxml-diamond" => Some("diamond"),
            "isosceles-triangle" | "ooxml-triangle" => Some("triangle"),
            "right-triangle" | "ooxml-rtTriangle" => Some("rtTriangle"),
            "parallelogram" | "ooxml-parallelogram" => Some("parallelogram"),
            "trapezoid" | "ooxml-trapezoid" => Some("trapezoid"),
            "hexagon" | "ooxml-hexagon" => Some("hexagon"),
            "octagon" | "ooxml-octagon" => Some("octagon"),
            "pentagon" | "ooxml-pentagon" => Some("pentagon"),
            "star5" | "ooxml-star5" => Some("star5"),
            "right-arrow" | "ooxml-rightArrow" => Some("rightArrow"),
            "left-arrow" | "ooxml-leftArrow" => Some("leftArrow"),
            "up-arrow" | "ooxml-upArrow" => Some("upArrow"),
            "down-arrow" | "ooxml-downArrow" => Some("downArrow"),
            "cross" | "ooxml-plus" => Some("plus"),
            "flowchart-process" => Some("flowChartProcess"),
            "flowchart-decision" => Some("flowChartDecision"),
            "flowchart-terminator" => Some("flowChartTerminator"),
            "flowchart-alternate-process" => Some("flowChartAlternateProcess"),
            _ => None,
        };
        if let Some(p) = preset.and_then(|p| presets::preset(p, w, h, [None, None])) {
            return (p.segs, mirror);
        }
        if let Some(p) = g.attr("draw:enhanced-path")
            && let Some(segs) = enhanced_path(p, g.attr("svg:viewBox"), w, h)
        {
            return (segs, mirror);
        }
        self.cv.approximated("a shape outside the preset table is drawn as its frame");
        (presets::rect(w, h), mirror)
    }

    // ---- tables ----------------------------------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn table(&mut self, tbl: &Node, base: &Mat) {
        // Columns and rows, flattened out of their groups.
        fn collect<'n>(n: &'n Node, name: &str, out: &mut Vec<&'n Node>, depth: usize) {
            if depth > 4 {
                return;
            }
            for k in &n.kids {
                if k.name == name {
                    out.push(k);
                } else if matches!(
                    k.name.as_str(),
                    "table-header-columns"
                        | "table-columns"
                        | "table-column-group"
                        | "table-header-rows"
                        | "table-rows"
                        | "table-row-group"
                ) {
                    collect(k, name, out, depth + 1);
                }
            }
        }
        let rep = |n: &Node, k: &str| -> usize {
            n.attr(k)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, MAX_TABLE_LINE)
        };
        let mut col_nodes = Vec::new();
        collect(tbl, "table-column", &mut col_nodes, 0);
        let mut cols: Vec<f64> = Vec::new();
        for c in col_nodes {
            let w = c
                .attr("table:style-name")
                .map(|s| self.styles.resolve("table-column", s))
                .and_then(|p| p.get("style:column-width").and_then(|v| len(v)))
                .unwrap_or(72.0);
            for _ in 0..rep(c, "table:number-columns-repeated") {
                if cols.len() >= MAX_TABLE_LINE {
                    self.cv.full = true;
                    break;
                }
                cols.push(w.max(0.0));
            }
        }
        let mut row_nodes = Vec::new();
        collect(tbl, "table-row", &mut row_nodes, 0);
        if cols.is_empty() || row_nodes.is_empty() {
            return;
        }
        struct Placed<'n> {
            row: usize,
            col: usize,
            span: usize,
            rspan: usize,
            cell: &'n Node,
        }
        let mut rows_h: Vec<f64> = Vec::new();
        let mut placed: Vec<Placed<'_>> = Vec::new();
        for r in row_nodes {
            let rh = r
                .attr("table:style-name")
                .map(|s| self.styles.resolve("table-row", s))
                .and_then(|p| {
                    p.get("style:row-height").or(p.get("style:min-row-height")).and_then(|v| len(v))
                })
                .unwrap_or(0.0);
            for _ in 0..rep(r, "table:number-rows-repeated") {
                if rows_h.len() >= MAX_TABLE_LINE {
                    self.cv.full = true;
                    break;
                }
                let ri = rows_h.len();
                rows_h.push(rh.max(0.0));
                let mut ci = 0usize;
                for c in &r.kids {
                    let repeated = rep(c, "table:number-columns-repeated");
                    match c.name.as_str() {
                        "table-cell" => {
                            for _ in 0..repeated {
                                if placed.len() >= MAX_TABLE_CELLS {
                                    self.cv.full = true;
                                    break;
                                }
                                placed.push(Placed {
                                    row: ri,
                                    col: ci,
                                    span: rep(c, "table:number-columns-spanned"),
                                    rspan: rep(c, "table:number-rows-spanned"),
                                    cell: c,
                                });
                                ci += 1;
                            }
                        }
                        "covered-table-cell" => ci += repeated,
                        _ => {}
                    }
                }
            }
        }
        struct Prep {
            row: usize,
            col: usize,
            span: usize,
            rspan: usize,
            fill: u32,
            borders: [Option<(u32, f64)>; 4],
            body: Option<TextBody>,
            laid: super::Laid,
        }
        let mut prepared: Vec<Prep> = Vec::new();
        for p in placed {
            if p.col >= cols.len() {
                continue;
            }
            let mut props = p
                .cell
                .attr("table:style-name")
                .map(|s| self.styles.resolve("table-cell", s))
                .unwrap_or_default();
            let fill = props.get("fo:background-color").and_then(|v| color(v)).unwrap_or(0);
            let border = |v: Option<&String>| -> Option<(u32, f64)> {
                let v = v?;
                let mut it = v.split_whitespace();
                let w = it.next().and_then(len)?;
                let style = it.next()?;
                let c = it.next().and_then(color)?;
                (style != "none" && style != "hidden").then_some((c, w.max(0.25)))
            };
            let all = border(props.get("fo:border"));
            let borders = [
                border(props.get("fo:border-left")).or(all),
                border(props.get("fo:border-top")).or(all),
                border(props.get("fo:border-right")).or(all),
                border(props.get("fo:border-bottom")).or(all),
            ];
            let valign = match props_valign(&props) {
                Anchor::Middle => "middle",
                Anchor::Bottom => "bottom",
                Anchor::Top => "top",
            };
            props
                .entry("draw:textarea-vertical-align".to_owned())
                .or_insert_with(|| valign.to_owned());
            let width: f64 = cols.iter().skip(p.col).take(p.span).sum();
            let mut body = self.text_body(p.cell, &props);
            let laid = match body.as_mut() {
                Some(b) => {
                    if !props.contains_key("fo:padding") && !props.contains_key("fo:padding-left") {
                        b.ins = [5.7, 2.8, 5.7, 2.8];
                    }
                    layout(b, width, self.cv)
                }
                None => super::Laid::default(),
            };
            if p.rspan == 1
                && let (Some(b), Some(h)) = (body.as_ref(), rows_h.get_mut(p.row))
            {
                *h = h.max((laid.height + b.ins[1] + b.ins[3]).min(5_000.0));
            }
            prepared.push(Prep {
                row: p.row,
                col: p.col,
                span: p.span,
                rspan: p.rspan,
                fill,
                borders,
                body,
                laid,
            });
        }
        let offsets = |v: &[f64]| -> Vec<f64> {
            let mut acc = 0.0;
            std::iter::once(0.0)
                .chain(v.iter().map(|d| {
                    acc += d;
                    acc
                }))
                .collect()
        };
        let (col_x, row_y) = (offsets(&cols), offsets(&rows_h));
        let mut draws: Vec<CellDraw> = Vec::new();
        for p in prepared {
            let x0 = col_x.get(p.col).copied().unwrap_or(0.0);
            let x1 = col_x.get((p.col + p.span).min(cols.len())).copied().unwrap_or(x0);
            let y0 = row_y.get(p.row).copied().unwrap_or(0.0);
            let y1 = row_y.get((p.row + p.rspan).min(rows_h.len())).copied().unwrap_or(y0);
            let body = p.body.unwrap_or(TextBody {
                paras: Vec::new(),
                ins: [0.0; 4],
                anchor: Anchor::Top,
                wrap: true,
                scale: 1.0,
            });
            draws.push(CellDraw {
                rect: [x0, y0, x1 - x0, y1 - y0],
                fill: p.fill,
                borders: p.borders,
                body,
                laid: p.laid,
            });
        }
        draw_cells(self.cv, &draws, base);
    }
}

fn props_valign(p: &Props) -> Anchor {
    match p.get("style:vertical-align").map(String::as_str) {
        Some("middle") => Anchor::Middle,
        Some("bottom") => Anchor::Bottom,
        _ => Anchor::Top,
    }
}

/// The frame text is laid out in, as in the presentation drawing: its size on the page and the
/// transform from it to the page.
fn text_frame(m: &Mat, w: f64, h: f64, angle: f64) -> (Mat, f64, f64) {
    let c = m.apply(w / 2.0, h / 2.0);
    let (sx, sy) = m.axis_scales();
    let (tw, th) = (w * sx, h * sy);
    let mt = Mat::translate(-tw / 2.0, -th / 2.0)
        .then(Mat::rotate(angle))
        .then(Mat::translate(c.0, c.1));
    (mt, tw, th)
}

// ---- path data -------------------------------------------------------------------------------------

/// The numbers of a string, SVG style (`10,20 30-40`).
fn numbers(s: &str) -> Vec<f64> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() && out.len() < 400_000 {
        let c = b.get(i).copied().unwrap_or(b' ');
        if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' {
            let start = i;
            i += 1;
            while let Some(&d) = b.get(i) {
                let prev = b.get(i - 1).copied().unwrap_or(b' ');
                let exp_sign = (d == b'-' || d == b'+') && (prev == b'e' || prev == b'E');
                if d.is_ascii_digit()
                    || d == b'.' && !s.get(start..i).unwrap_or("").contains('.')
                    || d == b'e'
                    || d == b'E'
                    || exp_sign
                {
                    i += 1;
                } else {
                    break;
                }
            }
            if let Some(v) = s.get(start..i).and_then(|t| t.parse::<f64>().ok()) {
                out.push(fin(v));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The viewBox of a shape: `x y w h`.
fn view_box(n: &Node) -> Option<[f64; 4]> {
    let v = numbers(n.attr("svg:viewBox")?);
    match v[..] {
        [x, y, w, h] if w > 0.0 && h > 0.0 => Some([x, y, w, h]),
        _ => None,
    }
}

fn polyline_points(n: &Node, w: f64, h: f64) -> Option<Vec<(f64, f64)>> {
    let [vx, vy, vw, vh] = view_box(n)?;
    let nums = numbers(n.attr("draw:points")?);
    let pts: Vec<(f64, f64)> = nums
        .chunks_exact(2)
        .take(100_000)
        .filter_map(|c| match c {
            [x, y] => Some(((x - vx) / vw * w, (y - vy) / vh * h)),
            _ => None,
        })
        .collect();
    (pts.len() >= 2).then_some(pts)
}

/// Path commands of `svg:d`; the flag says that something (an arc) was only approximated.
fn path_data(n: &Node, w: f64, h: f64) -> Option<(Vec<Seg>, bool)> {
    let [vx, vy, vw, vh] = view_box(n)?;
    let d = n.attr("svg:d")?;
    let (sx, sy) = (w / vw, h / vh);
    let mut segs = Vec::new();
    let mut approx = false;
    let (mut cx, mut cy) = (0.0, 0.0);
    let (mut sx0, mut sy0) = (0.0, 0.0);
    let mut last_ctrl: Option<(f64, f64)> = None;
    // Split into commands: a letter and the numbers up to the next letter.
    let mut cmds: Vec<(char, Vec<f64>)> = Vec::new();
    let mut cur: Option<(char, usize)> = None;
    for (i, c) in d.char_indices() {
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            if let Some((letter, start)) = cur.take() {
                cmds.push((letter, numbers(d.get(start..i).unwrap_or(""))));
            }
            cur = Some((c, i + 1));
        }
        if cmds.len() > 100_000 {
            return None;
        }
    }
    if let Some((letter, start)) = cur {
        cmds.push((letter, numbers(d.get(start..).unwrap_or(""))));
    }
    let map = |x: f64, y: f64| ((x - vx) * sx, (y - vy) * sy);
    for (letter, args) in cmds {
        let rel = letter.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let step = match letter.to_ascii_uppercase() {
            'M' | 'L' | 'T' => 2,
            'H' | 'V' => 1,
            'C' => 6,
            'S' | 'Q' => 4,
            'A' => 7,
            _ => 0,
        };
        if letter.eq_ignore_ascii_case(&'z') {
            segs.push(Seg::Z);
            cx = sx0;
            cy = sy0;
            continue;
        }
        if step == 0 {
            approx = true;
            continue;
        }
        let up = letter.to_ascii_uppercase();
        for (k, a) in args.chunks_exact(step).enumerate() {
            match (up, a) {
                ('M', [x, y]) if k == 0 => {
                    let (px, py) = map(ox + x, oy + y);
                    segs.push(Seg::M(px, py));
                    cx = ox + x;
                    cy = oy + y;
                    sx0 = cx;
                    sy0 = cy;
                    last_ctrl = None;
                }
                ('M' | 'L', [x, y]) => {
                    let (px, py) = map(ox + x, oy + y);
                    segs.push(Seg::L(px, py));
                    cx = ox + x;
                    cy = oy + y;
                    last_ctrl = None;
                }
                ('H', [x]) => {
                    cx = ox + x;
                    let (px, py) = map(cx, cy);
                    segs.push(Seg::L(px, py));
                    last_ctrl = None;
                }
                ('V', [y]) => {
                    cy = oy + y;
                    let (px, py) = map(cx, cy);
                    segs.push(Seg::L(px, py));
                    last_ctrl = None;
                }
                ('C', [x1, y1, x2, y2, x, y]) => {
                    let (a1, b1) = map(ox + x1, oy + y1);
                    let (a2, b2) = map(ox + x2, oy + y2);
                    let (a3, b3) = map(ox + x, oy + y);
                    segs.push(Seg::C(a1, b1, a2, b2, a3, b3));
                    last_ctrl = Some((ox + x2, oy + y2));
                    cx = ox + x;
                    cy = oy + y;
                }
                ('S', [x2, y2, x, y]) => {
                    let (c1x, c1y) =
                        last_ctrl.map_or((cx, cy), |(lx, ly)| (2.0 * cx - lx, 2.0 * cy - ly));
                    let (a1, b1) = map(c1x, c1y);
                    let (a2, b2) = map(ox + x2, oy + y2);
                    let (a3, b3) = map(ox + x, oy + y);
                    segs.push(Seg::C(a1, b1, a2, b2, a3, b3));
                    last_ctrl = Some((ox + x2, oy + y2));
                    cx = ox + x;
                    cy = oy + y;
                }
                ('Q', [x1, y1, x, y]) => {
                    let (qx, qy) = (ox + x1, oy + y1);
                    let (ex, ey) = (ox + x, oy + y);
                    let c1 = (cx + 2.0 / 3.0 * (qx - cx), cy + 2.0 / 3.0 * (qy - cy));
                    let c2 = (ex + 2.0 / 3.0 * (qx - ex), ey + 2.0 / 3.0 * (qy - ey));
                    let (a1, b1) = map(c1.0, c1.1);
                    let (a2, b2) = map(c2.0, c2.1);
                    let (a3, b3) = map(ex, ey);
                    segs.push(Seg::C(a1, b1, a2, b2, a3, b3));
                    last_ctrl = None;
                    cx = ex;
                    cy = ey;
                }
                ('A', [rx, ry, _rot, _large, _sweep, x, y]) => {
                    // Arcs are drawn as the chord, with the ends where they should be.
                    let _ = (rx, ry);
                    approx = true;
                    let (px, py) = map(ox + x, oy + y);
                    segs.push(Seg::L(px, py));
                    cx = ox + x;
                    cy = oy + y;
                    last_ctrl = None;
                }
                _ => approx = true,
            }
        }
    }
    let _ = arc_to;
    (!segs.is_empty()).then_some((segs, approx))
}

/// A custom shape's `draw:enhanced-path` when it is plain numbers (no formulas): `M L C Z N`.
fn enhanced_path(p: &str, view_box: Option<&str>, w: f64, h: f64) -> Option<Vec<Seg>> {
    let vb = numbers(view_box?);
    let [vx, vy, vw, vh] = vb[..] else { return None };
    if vw <= 0.0 || vh <= 0.0 {
        return None;
    }
    if p.contains('?') || p.contains('$') {
        return None;
    }
    let (sx, sy) = (w / vw, h / vh);
    let mut segs = Vec::new();
    let mut cmds: Vec<(char, Vec<f64>)> = Vec::new();
    let mut cur: Option<(char, usize)> = None;
    for (i, c) in p.char_indices() {
        if c.is_ascii_alphabetic() {
            if let Some((letter, start)) = cur.take() {
                cmds.push((letter, numbers(p.get(start..i).unwrap_or(""))));
            }
            cur = Some((c, i + 1));
        }
        if cmds.len() > 50_000 {
            return None;
        }
    }
    if let Some((letter, start)) = cur {
        cmds.push((letter, numbers(p.get(start..).unwrap_or(""))));
    }
    for (letter, a) in cmds {
        let pt = |x: f64, y: f64| ((x - vx) * sx, (y - vy) * sy);
        match letter {
            'M' => {
                for (i, c) in a.chunks_exact(2).enumerate() {
                    if let [x, y] = c {
                        let (px, py) = pt(*x, *y);
                        segs.push(if i == 0 { Seg::M(px, py) } else { Seg::L(px, py) });
                    }
                }
            }
            'L' => {
                for c in a.chunks_exact(2) {
                    if let [x, y] = c {
                        let (px, py) = pt(*x, *y);
                        segs.push(Seg::L(px, py));
                    }
                }
            }
            'C' => {
                for c in a.chunks_exact(6) {
                    if let [x1, y1, x2, y2, x, y] = c {
                        let (a1, b1) = pt(*x1, *y1);
                        let (a2, b2) = pt(*x2, *y2);
                        let (a3, b3) = pt(*x, *y);
                        segs.push(Seg::C(a1, b1, a2, b2, a3, b3));
                    }
                }
            }
            'Z' => segs.push(Seg::Z),
            'N' | 'F' | 'S' => {}
            _ => return None,
        }
    }
    (!segs.is_empty()).then_some(segs)
}

// ---- the deck ------------------------------------------------------------------------------------

fn read_part(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>, PreviewError> {
    let Some(bytes) = pkg.read(part)? else { return Ok(None) };
    Ok(Some(pkg.decode(part, &bytes)?.into_owned()))
}

/// Runs the deck: counts the pages and, if `want` is given, draws that page.
pub(super) fn run(pkg: &mut Package<'_>, want: Option<usize>, policy: &PreviewPolicy) -> Outcome {
    if let Some(manifest) = read_part(pkg, "META-INF/manifest.xml")?
        && manifest.contains("encryption-data")
    {
        return Err(PreviewError::Encrypted);
    }
    let content = read_part(pkg, "content.xml")?
        .ok_or_else(|| PreviewError::Malformed("the package has no content.xml".into()))?;
    let content = parse(&content, pkg.limits(), &KEEP_TEXT, &[])
        .map_err(|_| PreviewError::Malformed("content.xml is not readable".into()))?;
    let styles_tree = match read_part(pkg, "styles.xml")? {
        Some(t) => parse(&t, pkg.limits(), &KEEP_TEXT, &[]).ok(),
        None => None,
    };
    let mut trees: Vec<&Node> = Vec::new();
    if let Some(s) = &styles_tree {
        trees.push(&s.root);
    }
    trees.push(&content.root);
    let styles = Styles::load(&trees);

    let Some(presentation) = content.root.child("body").and_then(|b| b.child("presentation"))
    else {
        return Err(PreviewError::Unsupported(crate::model::FileFormat::Odp));
    };
    let is_hidden = |p: &Node| -> bool {
        p.attr("presentation:visibility") == Some("hidden")
            || p.attr("draw:style-name").is_some_and(|s| {
                styles.resolve("drawing-page", s).get("presentation:visibility").map(String::as_str)
                    == Some("hidden")
            })
    };
    let pages: Vec<&Node> = presentation
        .kids_named("page")
        .take(policy.max_sections)
        .filter(|p| policy.reveal_hidden || !is_hidden(p))
        .collect();
    let count = pages.len();
    let Some(index) = want else { return Ok((count, None)) };
    let page = pages
        .get(index)
        .copied()
        .ok_or_else(|| PreviewError::Malformed("the document has no such page".into()))?;

    let master = page.attr("draw:master-page-name").and_then(|n| styles.masters.get(n)).copied();
    let mut cv = Canvas::new();
    let (w, h) = master
        .and_then(|m| m.attr("style:page-layout-name"))
        .and_then(|l| styles.layouts.get(l))
        .and_then(|(w, h)| Some((side((*w)?)?, side((*h)?)?)))
        .unwrap_or_else(|| {
            cv.approximated("the page size is missing or not believable");
            DEFAULT_SIZE
        });
    cv.push(Prim::Save);
    cv.push(Prim::Clip { x: 0.0, y: 0.0, w: f32c(w), h: f32c(h) });

    // Background: the page's, else the master page's.
    let mut bg_props = Props::new();
    if let Some(s) = master.and_then(|m| m.attr("draw:style-name")) {
        merge(&mut bg_props, styles.resolve("drawing-page", s));
    }
    if let Some(s) = page.attr("draw:style-name") {
        merge(&mut bg_props, styles.resolve("drawing-page", s));
    }
    let pkg_ref: &Package<'_> = pkg;
    let mut d = Draw {
        cv: &mut cv,
        styles: &styles,
        pkg: pkg_ref,
        reveal: policy.reveal_hidden,
        page_no: index + 1,
        master,
    };
    let (bg, _, _) = d.paint(&bg_props, false);
    if bg >> 24 != 0 && bg != 0xFFFF_FFFF {
        d.cv.path(&presets::rect(w, h), &Mat::ID, bg, 0, 0.0, false);
    }
    if bg_props.get("presentation:background-objects-visible").map(String::as_str) != Some("false")
        && let Some(m) = master
    {
        d.shapes(m, &Mat::ID, 0, true);
    }
    d.shapes(page, &Mat::ID, 0, false);
    let _ = d.reveal;
    cv.push(Prim::Restore);
    let damaged = content.damaged || content.cut;
    if content.cut {
        cv.full = true;
    }
    let Rendered { page, notes } = cv.finish(w, h, index, count, damaged);
    Ok((count, Some(Rendered { page, notes })))
}
