//! The relationship diagram of an Access database, drawn as one page.
//!
//! Tables are boxes with their columns (the primary key and the foreign-key columns are marked);
//! each relationship of `MSysRelationships` is a line from the column that holds the key to the
//! column it refers to, with `1` at the referred side and `∞` at the other (`1` at both for a
//! one-to-one relationship). Only names are drawn: no row of any table is read.

use super::db::{Db, IndexKind, Object, OpenError, Table};
use super::{Rel, is_user_object, kind, read_relationships};
use crate::page::{PageModel, Prim, op, style};
use crate::sink::{CleanCounts, clean_text};
use crate::{PreviewError, PreviewPolicy};
use std::collections::BTreeSet;

/// Most tables drawn.
const MAX_TABLES: usize = 48;
/// Most columns listed in one box.
const MAX_COLUMNS: usize = 24;
/// Longest name drawn, in characters.
const MAX_NAME: usize = 36;
/// Most boxes in a row of the layout.
const MAX_PER_ROW: usize = 8;

const MARGIN: f32 = 30.0;
const TITLE: f32 = 34.0;
const GAP_X: f32 = 64.0;
const GAP_Y: f32 = 36.0;
const HEADER: f32 = 20.0;
const LINE: f32 = 13.0;
const FONT: f32 = 9.0;

const BOX_FILL: u32 = 0xFFF7_F8FA;
const BOX_EDGE: u32 = 0xFF44_5566;
const HEADER_FILL: u32 = 0xFFD9_E3F2;
const INK: u32 = 0xFF1B_1F24;
const MUTED: u32 = 0xFF5A_6572;
const LINK: u32 = 0xFF2B_5C9E;

/// An f32 for a small count.
fn count(n: usize) -> f32 {
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

/// A name that is safe to draw: invisible and control characters removed, shortened.
fn label(s: &str) -> String {
    let mut counts = CleanCounts::default();
    let clean = clean_text(s, &mut counts);
    let mut out: String = clean.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if clean.chars().count() > MAX_NAME {
        out.push('…');
    }
    out
}

/// Width a run of text takes: a plain estimate, the app fits the text to it.
fn text_width(s: &str, size: f32) -> f32 {
    count(s.chars().count()) * size * 0.56
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<f32> {
    vec![op::MOVE, x, y, op::LINE, x + w, y, op::LINE, x + w, y + h, op::LINE, x, y + h, op::CLOSE]
}

struct Column {
    name: String,
    primary: bool,
    foreign: bool,
}

struct BoxModel {
    /// The name as the relationships spell it, to find the box again.
    key: String,
    title: String,
    columns: Vec<Column>,
    hidden_columns: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    cell_col: usize,
}

impl BoxModel {
    /// The vertical middle of the row of `column`, or of the header when the column is not drawn.
    fn row_y(&self, column: &str) -> f32 {
        match self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(column)) {
            Some(i) => self.y + HEADER + count(i) * LINE + LINE / 2.0 + 3.0,
            None => self.y + HEADER / 2.0,
        }
    }
}

fn boxes_for(
    tables: &[(&Object, Table)],
    rels: &std::collections::BTreeMap<String, Rel>,
) -> Vec<BoxModel> {
    tables
        .iter()
        .map(|(o, t)| {
            let foreign = |name: &str| {
                rels.values().any(|r| {
                    r.table.eq_ignore_ascii_case(&o.name)
                        && r.columns.iter().any(|c| c.1.eq_ignore_ascii_case(name))
                })
            };
            let primary = |name: &str| {
                t.indexes.iter().any(|ix| {
                    ix.kind == IndexKind::Primary
                        && ix.columns.iter().any(|(c, _)| c.eq_ignore_ascii_case(name))
                })
            };
            let all: Vec<Column> = t
                .columns
                .iter()
                .map(|c| Column {
                    name: label(&c.name),
                    primary: primary(&c.name),
                    foreign: foreign(&c.name),
                })
                .collect();
            let hidden_columns = all.len().saturating_sub(MAX_COLUMNS);
            // Key columns first when the list has to be cut.
            let mut columns = all;
            if hidden_columns > 0 {
                columns.sort_by_key(|c| !(c.primary || c.foreign));
                columns.truncate(MAX_COLUMNS);
            }
            let title = label(&o.name);
            let mut w = text_width(&title, FONT + 2.0) + 24.0;
            for c in &columns {
                w = w.max(text_width(&c.name, FONT) + 40.0);
            }
            let rows = columns.len() + usize::from(hidden_columns > 0);
            BoxModel {
                key: o.name.clone(),
                title,
                columns,
                hidden_columns,
                x: 0.0,
                y: 0.0,
                w: w.clamp(120.0, 300.0),
                h: HEADER + count(rows) * LINE + 8.0,
                cell_col: 0,
            }
        })
        .collect()
}

/// The relationships as pairs of box numbers (two different tables each).
fn table_edges(
    boxes: &[BoxModel],
    rels: &std::collections::BTreeMap<String, Rel>,
) -> Vec<(usize, usize)> {
    let find = |name: &str| boxes.iter().position(|b| b.key.eq_ignore_ascii_case(name));
    rels.values()
        .filter_map(|r| {
            let (a, b) = (find(&r.table)?, find(&r.related)?);
            (a != b).then_some((a, b))
        })
        .collect()
}

/// The grid slot of every box (slots are numbered row by row): tables that are related sit near
/// each other, so that the lines between them are short and few cross.
///
/// The boxes are first put in the order of a breadth-first walk from the most connected one, then
/// pairs of linked boxes are swapped while that makes the lines shorter (a few passes at most).
/// Tables with no relationship come last. The same input always gives the same slots.
fn arrange(n: usize, edges: &[(usize, usize)], per_row: usize) -> Vec<usize> {
    let per_row = per_row.max(1);
    let mut near: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a < n && b < n {
            if let Some(v) = near.get_mut(a) {
                v.push(b);
            }
            if let Some(v) = near.get_mut(b) {
                v.push(a);
            }
        }
    }
    let degree: Vec<usize> = near.iter().map(Vec::len).collect();
    let deg = |i: usize| degree.get(i).copied().unwrap_or(0);
    let mut by_degree: Vec<usize> = (0..n).collect();
    by_degree.sort_by(|a, b| deg(*b).cmp(&deg(*a)).then(a.cmp(b)));
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let is_seen = |seen: &[bool], i: usize| seen.get(i).copied().unwrap_or(true);
    for &start in &by_degree {
        if is_seen(&seen, start) || deg(start) == 0 {
            continue;
        }
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(u) = queue.pop_front() {
            order.push(u);
            let mut next: Vec<usize> = near
                .get(u)
                .map(|v| v.iter().copied().filter(|x| !is_seen(&seen, *x)).collect())
                .unwrap_or_default();
            next.sort_by(|a, b| deg(*b).cmp(&deg(*a)).then(a.cmp(b)));
            next.dedup();
            for v in next {
                if !is_seen(&seen, v) {
                    if let Some(s) = seen.get_mut(v) {
                        *s = true;
                    }
                    queue.push_back(v);
                }
            }
        }
    }
    let linked = order.len();
    order.extend((0..n).filter(|i| !is_seen(&seen, *i)));
    let mut slot = vec![0usize; n];
    for (s, &b) in order.iter().enumerate() {
        if let Some(x) = slot.get_mut(b) {
            *x = s;
        }
    }
    // How far apart two slots are on the grid (a step across a row or down a column is one).
    let apart = |s: usize, t: usize| -> usize {
        (s / per_row).abs_diff(t / per_row) + (s % per_row).abs_diff(t % per_row)
    };
    let at = |slot: &[usize], b: usize| slot.get(b).copied().unwrap_or(0);
    let cost_of = |b: usize, slot: &[usize]| -> usize {
        near.get(b).map(|v| v.iter().map(|&o| apart(at(slot, b), at(slot, o))).sum()).unwrap_or(0)
    };
    for _ in 0..8 {
        let mut better = false;
        for i in 0..linked {
            for j in i + 1..linked {
                let (Some(&bi), Some(&bj)) = (order.get(i), order.get(j)) else { continue };
                let before = cost_of(bi, &slot) + cost_of(bj, &slot);
                slot.swap(bi, bj);
                let after = cost_of(bi, &slot) + cost_of(bj, &slot);
                if after < before {
                    order.swap(i, j);
                    better = true;
                } else {
                    slot.swap(bi, bj);
                }
            }
        }
        if !better {
            break;
        }
    }
    slot
}

/// Draws the relationship diagram of the database in `data` as page 0 (the only page).
///
/// # Errors
/// [`PreviewError::Encrypted`] for a protected database, [`PreviewError::Malformed`] for one whose
/// catalog cannot be read or for a page other than the first.
pub(crate) fn render_er(
    data: &[u8],
    index: usize,
    _policy: &PreviewPolicy,
) -> Result<PageModel, PreviewError> {
    if index != 0 {
        return Err(PreviewError::Malformed("page does not exist".into()));
    }
    let db = Db::open(data).map_err(|e| match e {
        OpenError::Encrypted => PreviewError::Encrypted,
        OpenError::Malformed(why) => PreviewError::Malformed(why.to_owned()),
    })?;
    let (objects, catalog_stats) = db
        .catalog_checked()
        .ok_or_else(|| PreviewError::Malformed("the system catalog is unreadable".into()))?;
    let (rels, rels_stopped) = read_relationships(&db, &objects).unwrap_or_default();

    let mut user: Vec<&Object> =
        objects.iter().filter(|o| o.kind == kind::TABLE && is_user_object(o)).collect();
    user.sort_by_key(|o| o.name.to_lowercase());
    // The tables that take part in a relationship come first.
    let linked: BTreeSet<String> =
        rels.values().flat_map(|r| [r.table.to_lowercase(), r.related.to_lowercase()]).collect();
    user.sort_by_key(|o| !linked.contains(&o.name.to_lowercase()));
    let mut truncated = catalog_stats.stopped || rels_stopped;
    if user.len() > MAX_TABLES {
        user.truncate(MAX_TABLES);
        truncated = true;
    }
    let tables: Vec<(&Object, Table)> =
        user.into_iter().filter_map(|o| db.table(o.page).map(|t| (o, t))).collect();
    let mut boxes = boxes_for(&tables, &rels);

    // Layout: a grid, as square as it gets, with related tables near each other.
    let n = boxes.len();
    let mut per_row = 1usize;
    while per_row * per_row < n && per_row < MAX_PER_ROW {
        per_row += 1;
    }
    let edges = table_edges(&boxes, &rels);
    let slot = arrange(n, &edges, per_row);
    let slot_of = |i: usize| slot.get(i).copied().unwrap_or(0);
    let cell = |i: usize| (slot_of(i) / per_row, slot_of(i) % per_row);
    let rows_n = n.div_ceil(per_row).max(1);
    let mut col_w = vec![0.0f32; per_row];
    let mut row_h = vec![0.0f32; rows_n];
    for (i, b) in boxes.iter_mut().enumerate() {
        let (r, c) = (slot_of(i) / per_row, slot_of(i) % per_row);
        b.cell_col = c;
        if let Some(w) = col_w.get_mut(c) {
            *w = w.max(b.w);
        }
        if let Some(h) = row_h.get_mut(r) {
            *h = h.max(b.h);
        }
    }
    // Half a gap of room on the left: the lines run in the gutters between the columns, and the
    // first gutter is the one left of the first column.
    let mut col_x = Vec::with_capacity(per_row);
    let mut x = MARGIN + GAP_X / 2.0;
    for w in &col_w {
        col_x.push(x);
        x += w + GAP_X;
    }
    let width = (x - GAP_X / 2.0 + MARGIN).max(2.0 * MARGIN + 240.0);
    let mut row_y = Vec::with_capacity(rows_n);
    let mut y = MARGIN + TITLE;
    for h in &row_h {
        row_y.push(y);
        y += h + GAP_Y;
    }
    let height = (y - GAP_Y + MARGIN).max(MARGIN + TITLE + MARGIN);
    for (i, b) in boxes.iter_mut().enumerate() {
        b.x = col_x.get(slot_of(i) % per_row).copied().unwrap_or(MARGIN);
        b.y = row_y.get(slot_of(i) / per_row).copied().unwrap_or(MARGIN + TITLE);
    }
    // The vertical lines run in the gutters (`g` is left of column `g`) and the horizontal ones in
    // the channels (`k` is above row `k`), so that no line crosses a box.
    let gutter_x = |g: usize| -> f32 {
        if g == 0 {
            col_x.first().copied().unwrap_or(MARGIN) - GAP_X / 2.0
        } else {
            let c = g - 1;
            col_x.get(c).copied().unwrap_or(MARGIN)
                + col_w.get(c).copied().unwrap_or(0.0)
                + GAP_X / 2.0
        }
    };
    let channel_y = |k: usize| -> f32 {
        if k < rows_n {
            row_y.get(k).copied().unwrap_or(MARGIN + TITLE) - GAP_Y / 2.0
        } else {
            row_y.last().copied().unwrap_or(MARGIN + TITLE)
                + row_h.last().copied().unwrap_or(0.0)
                + GAP_Y / 2.0
        }
    };
    let mut gutter_used = vec![0usize; per_row + 2];
    let mut channel_used = vec![0usize; rows_n + 2];

    let mut prims: Vec<Prim> = Vec::new();
    let heading = if rels.is_empty() {
        "Tables (the database defines no relationships)".to_owned()
    } else {
        format!("Relationships: {} tables, {} relationships", n, rels.len())
    };
    prims.push(Prim::Text {
        x: MARGIN,
        y: MARGIN + 6.0,
        size: FONT + 3.0,
        angle: 0.0,
        width: text_width(&heading, FONT + 3.0),
        style: style::BOLD,
        color: INK,
        text: heading,
    });

    // Lines first, so that the boxes cover their ends.
    let find = |name: &str| boxes.iter().position(|b| b.key.eq_ignore_ascii_case(name));
    for rel in rels.values() {
        let (Some(a), Some(b)) = (find(&rel.table), find(&rel.related)) else { continue };
        let (Some(from), Some(to)) = (boxes.get(a), boxes.get(b)) else { continue };
        let (own, other) =
            rel.columns.first().map(|c| (c.1.as_str(), c.2.as_str())).unwrap_or(("", ""));
        let (ya, yb) = (from.row_y(own), to.row_y(other));
        let ((ra, ca), (rb, cb)) = (cell(a), cell(b));
        // A leaves on the side that faces B (the right one for a table in its own column) and B is
        // entered on its facing side; both stay in the gutter next to the box.
        let exit_right = cb >= ca;
        let enter_right = cb < ca || ca == cb;
        let (ga, gb) =
            (if exit_right { ca + 1 } else { ca }, if enter_right { cb + 1 } else { cb });
        let (xa, xb) = (
            if exit_right { from.x + from.w } else { from.x },
            if enter_right { to.x + to.w } else { to.x },
        );
        let lane = |used: &mut Vec<usize>, id: usize, step: f32, span: usize| -> f32 {
            let k = used.get(id).copied().unwrap_or(0);
            if let Some(u) = used.get_mut(id) {
                *u += 1;
            }
            let half = count(span / 2);
            (count(k % span) - half) * step
        };
        let lane_a = lane(&mut gutter_used, ga, 3.0, 9);
        let lane_b = if gb == ga { lane_a } else { lane(&mut gutter_used, gb, 3.0, 9) };
        let (gxa, gxb) = (gutter_x(ga) + lane_a, gutter_x(gb) + lane_b);
        let mut ops = vec![op::MOVE, xa, ya, op::LINE, gxa, ya];
        if ga == gb {
            ops.extend([op::LINE, gxa, yb]);
        } else {
            // Across: down (or up) the gutter to a channel, along the channel, to the other gutter.
            let ch = if ra == rb {
                ra + 1
            } else if rb > ra {
                rb
            } else {
                ra
            };
            let ych = channel_y(ch) + lane(&mut channel_used, ch, 3.0, 5);
            ops.extend([op::LINE, gxa, ych, op::LINE, gxb, ych, op::LINE, gxb, yb]);
        }
        ops.extend([op::LINE, xb, yb]);
        prims.push(Prim::Path { ops, fill: 0, stroke: LINK, width: 1.1, even_odd: false });
        let one_to_one = rel.flags & 0x1 != 0;
        let side_a = if exit_right { 1.0f32 } else { -1.0 };
        let side_b = if enter_right { 1.0f32 } else { -1.0 };
        let mark = |x: f32, side: f32, y: f32, text: &str| Prim::Text {
            x: x + side * 9.0 - if side < 0.0 { 3.0 } else { 0.0 },
            y: y - 3.0,
            size: FONT,
            angle: 0.0,
            width: text_width(text, FONT),
            style: style::BOLD,
            color: LINK,
            text: text.to_owned(),
        };
        prims.push(mark(xa, side_a, ya, if one_to_one { "1" } else { "∞" }));
        prims.push(mark(xb, side_b, yb, "1"));
    }

    for b in &boxes {
        prims.push(Prim::Path {
            ops: rect(b.x, b.y, b.w, b.h),
            fill: BOX_FILL,
            stroke: BOX_EDGE,
            width: 1.0,
            even_odd: false,
        });
        prims.push(Prim::Path {
            ops: rect(b.x, b.y, b.w, HEADER),
            fill: HEADER_FILL,
            stroke: BOX_EDGE,
            width: 1.0,
            even_odd: false,
        });
        prims.push(Prim::Text {
            x: b.x + 8.0,
            y: b.y + HEADER - 6.0,
            size: FONT + 2.0,
            angle: 0.0,
            width: text_width(&b.title, FONT + 2.0),
            style: style::BOLD,
            color: INK,
            text: b.title.clone(),
        });
        for (i, c) in b.columns.iter().enumerate() {
            let base = b.y + HEADER + count(i) * LINE + LINE;
            let tag = match (c.primary, c.foreign) {
                (true, true) => "PK FK",
                (true, false) => "PK",
                (false, true) => "FK",
                (false, false) => "",
            };
            if !tag.is_empty() {
                prims.push(Prim::Text {
                    x: b.x + 5.0,
                    y: base,
                    size: 6.5,
                    angle: 0.0,
                    width: text_width(tag, 6.5),
                    style: style::BOLD,
                    color: MUTED,
                    text: tag.to_owned(),
                });
            }
            prims.push(Prim::Text {
                x: b.x + 30.0,
                y: base,
                size: FONT,
                angle: 0.0,
                width: text_width(&c.name, FONT),
                style: if c.primary { style::BOLD } else { 0 },
                color: INK,
                text: c.name.clone(),
            });
        }
        if b.hidden_columns > 0 {
            let more = format!("… {} more columns", b.hidden_columns);
            prims.push(Prim::Text {
                x: b.x + 30.0,
                y: b.y + HEADER + count(b.columns.len()) * LINE + LINE,
                size: FONT,
                angle: 0.0,
                width: text_width(&more, FONT),
                style: style::ITALIC,
                color: MUTED,
                text: more,
            });
        }
    }

    Ok(PageModel { width, height, index: 0, pages: 1, prims, truncated, approximate: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database written by Jackcess (`tools/fixtures/access/MakeRelations.java`): fifteen tables
    /// and fifteen relationships of every kind that matters for the picture.
    fn northwind() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/relations-northwind.mdb");
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// `(parent, child)` of every relationship the generator wrote, as the table names.
    const PAIRS: [(&str, &str); 15] = [
        ("Customers", "Orders"),
        ("Employees", "Orders"),
        ("Shippers", "Orders"),
        ("Addresses", "Orders"),
        ("Addresses", "Orders"),
        ("Orders", "OrderDetails"),
        ("Products", "OrderDetails"),
        ("OrderDetails", "OrderDetailNotes"),
        ("Suppliers", "Products"),
        ("Categories", "Products"),
        ("Employees", "Employees"),
        ("Employees", "EmployeeTerritories"),
        ("Territories", "EmployeeTerritories"),
        ("Regions", "Territories"),
        ("Employees", "EmployeePhotos"),
    ];

    struct Drawn {
        /// `(title, x, y, w, h)` of every table box.
        boxes: Vec<(String, f32, f32, f32, f32)>,
        /// The points of every relationship line.
        lines: Vec<Vec<(f32, f32)>>,
        /// `(text, x, y)` of the "1" and "∞" marks.
        marks: Vec<(String, f32, f32)>,
        heading: String,
    }

    fn corners(ops: &[f32]) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < ops.len() {
            if ops[i] == op::MOVE || ops[i] == op::LINE {
                out.push((ops[i + 1], ops[i + 2]));
                i += 3;
            } else {
                i += 1;
            }
        }
        out
    }

    fn draw(data: &[u8]) -> Drawn {
        let page = render_er(data, 0, &PreviewPolicy::default()).unwrap();
        let mut boxes = Vec::new();
        let mut lines = Vec::new();
        let mut heads: Vec<(f32, f32, f32, f32)> = Vec::new();
        let mut texts: Vec<(String, f32, f32)> = Vec::new();
        for p in &page.prims {
            match p {
                Prim::Path { ops, fill, .. } if *fill == BOX_FILL => {
                    let c = corners(ops);
                    boxes.push((String::new(), c[0].0, c[0].1, c[2].0 - c[0].0, c[2].1 - c[0].1));
                }
                Prim::Path { ops, fill, .. } if *fill == HEADER_FILL => {
                    let c = corners(ops);
                    heads.push((c[0].0, c[0].1, c[2].0 - c[0].0, c[2].1 - c[0].1));
                }
                Prim::Path { ops, fill: 0, stroke, .. } if *stroke == LINK => {
                    lines.push(corners(ops))
                }
                Prim::Text { text, x, y, .. } => texts.push((text.clone(), *x, *y)),
                _ => {}
            }
        }
        // The title of a box is the text in its header.
        for b in &mut boxes {
            let head = heads.iter().find(|h| (h.0 - b.1).abs() < 0.01 && (h.1 - b.2).abs() < 0.01);
            let h = head.expect("every box has a header");
            b.0 = texts
                .iter()
                .find(|t| t.1 > h.0 && t.1 < h.0 + h.2 && t.2 > h.1 && t.2 < h.1 + h.3)
                .map(|t| t.0.clone())
                .expect("every header has a title");
        }
        let marks = texts.iter().filter(|t| t.0 == "1" || t.0 == "∞").cloned().collect();
        let heading = texts.first().map(|t| t.0.clone()).unwrap_or_default();
        Drawn { boxes, lines, marks, heading }
    }

    /// Whether the open segment `a`-`b` (axis-parallel) passes through the inside of the box.
    fn crosses(a: (f32, f32), b: (f32, f32), bx: &(String, f32, f32, f32, f32)) -> bool {
        let eps = 0.5;
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        x1 > bx.1 + eps && x0 < bx.1 + bx.3 - eps && y1 > bx.2 + eps && y0 < bx.2 + bx.4 - eps
    }

    fn on_edge(p: (f32, f32), b: &(String, f32, f32, f32, f32)) -> bool {
        let eps = 0.5;
        let inside_y = p.1 >= b.2 - eps && p.1 <= b.2 + b.4 + eps;
        let inside_x = p.0 >= b.1 - eps && p.0 <= b.1 + b.3 + eps;
        ((p.0 - b.1).abs() < eps || (p.0 - (b.1 + b.3)).abs() < eps) && inside_y
            || ((p.1 - b.2).abs() < eps || (p.1 - (b.2 + b.4)).abs() < eps) && inside_x
    }

    #[test]
    fn every_relationship_of_a_real_database_joins_its_own_two_tables() {
        let d = draw(&northwind());
        assert_eq!(d.boxes.len(), 15);
        assert_eq!(d.lines.len(), 15, "one line for each relationship");
        assert_eq!(d.heading, "Relationships: 15 tables, 15 relationships");
        let mut want: Vec<(String, String)> =
            PAIRS.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect();
        let mut got: Vec<(String, String)> = Vec::new();
        for line in &d.lines {
            let (first, last) = (line[0], *line.last().unwrap());
            let at = |p: (f32, f32)| d.boxes.iter().find(|b| on_edge(p, b)).map(|b| b.0.clone());
            // The line starts at the table with the foreign key (the child) and ends at the one it
            // refers to (the parent).
            let (child, parent) =
                (at(first).expect("starts on a box"), at(last).expect("ends on a box"));
            got.push((parent, child));
        }
        want.sort();
        got.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn the_one_is_at_the_parent_and_the_many_at_the_child() {
        let d = draw(&northwind());
        for line in &d.lines {
            let (child_end, parent_end) = (line[0], *line.last().unwrap());
            let mark_near = |p: (f32, f32)| {
                d.marks
                    .iter()
                    .filter(|m| (m.1 - p.0).abs() < 14.0 && (m.2 - p.1).abs() < 6.0)
                    .map(|m| m.0.as_str())
                    .collect::<Vec<_>>()
            };
            // The photo table is keyed by the employee: that relationship is one to one, and
            // Jackcess flags it so (grbit 1): "1" at both ends.
            let child = d.boxes.iter().find(|b| on_edge(child_end, b)).map(|b| b.0.as_str());
            let want = if child == Some("EmployeePhotos") { "1" } else { "∞" };
            assert!(
                mark_near(child_end).contains(&want),
                "no {want} at the child end {child_end:?}"
            );
            assert!(mark_near(parent_end).contains(&"1"), "no 1 at the parent end {parent_end:?}");
        }
        assert_eq!(d.marks.iter().filter(|m| m.0 == "∞").count(), 14);
        assert_eq!(d.marks.iter().filter(|m| m.0 == "1").count(), 16);
    }

    #[test]
    fn boxes_never_overlap_and_no_line_crosses_a_box() {
        let d = draw(&northwind());
        for (i, a) in d.boxes.iter().enumerate() {
            for b in &d.boxes[i + 1..] {
                let apart =
                    a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1 || a.2 + a.4 <= b.2 || b.2 + b.4 <= a.2;
                assert!(apart, "{} and {} overlap", a.0, b.0);
            }
        }
        for line in &d.lines {
            for w in line.windows(2) {
                assert!(
                    (w[0].0 - w[1].0).abs() < 0.01 || (w[0].1 - w[1].1).abs() < 0.01,
                    "a slanted segment {w:?}"
                );
                for b in &d.boxes {
                    assert!(!crosses(w[0], w[1], b), "a line crosses {}: {line:?}", b.0);
                }
            }
        }
    }

    #[test]
    fn related_tables_are_neighbours_in_the_layout() {
        // Tables that are related sit near each other: no relationship spans more than three
        // cells of the grid (the arrangement is not alphabetical).
        let d = draw(&northwind());
        let centre = |n: &str| {
            let b = d.boxes.iter().find(|b| b.0 == n).unwrap();
            (b.1 + b.3 / 2.0, b.2 + b.4 / 2.0)
        };
        let col_step = 220.0f32;
        let far = PAIRS
            .iter()
            .filter(|(a, b)| a != b)
            .filter(|(a, b)| {
                let (p, q) = (centre(a), centre(b));
                (p.0 - q.0).abs() > 3.0 * col_step
            })
            .count();
        assert_eq!(far, 0);
    }

    #[test]
    fn the_same_database_always_gives_the_same_picture() {
        let a = render_er(&northwind(), 0, &PreviewPolicy::default()).unwrap();
        let b = render_er(&northwind(), 0, &PreviewPolicy::default()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn the_arrangement_puts_neighbours_in_neighbouring_slots_and_keeps_unlinked_tables_last() {
        // A chain 0-1-2-3 and an island 4: the chain is laid along the grid, the island after it.
        let slot = arrange(5, &[(0, 1), (1, 2), (2, 3)], 3);
        assert_eq!(slot.len(), 5);
        let mut sorted = slot.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4], "every box has a slot of its own");
        assert_eq!(slot[4], 4, "the island is last");
        let apart = |a: usize, b: usize| {
            (slot[a] / 3).abs_diff(slot[b] / 3) + (slot[a] % 3).abs_diff(slot[b] % 3)
        };
        assert!(apart(0, 1) + apart(1, 2) + apart(2, 3) <= 4, "{slot:?}");
        // No relationships at all, or edges to boxes that do not exist, are fine.
        assert_eq!(arrange(3, &[], 2), vec![0, 1, 2]);
        assert_eq!(arrange(2, &[(0, 9)], 2).len(), 2);
        assert!(arrange(0, &[], 1).is_empty());
    }
}
