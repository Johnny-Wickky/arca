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

    // Layout: a grid, in the order above, as square as it gets.
    let n = boxes.len();
    let mut per_row = 1usize;
    while per_row * per_row < n && per_row < MAX_PER_ROW {
        per_row += 1;
    }
    let mut col_w = vec![0.0f32; per_row];
    let mut row_h: Vec<f32> = Vec::new();
    for (i, b) in boxes.iter_mut().enumerate() {
        let (r, c) = (i / per_row, i % per_row);
        b.cell_col = c;
        if row_h.len() <= r {
            row_h.push(0.0);
        }
        if let Some(w) = col_w.get_mut(c) {
            *w = w.max(b.w);
        }
        if let Some(h) = row_h.get_mut(r) {
            *h = h.max(b.h);
        }
    }
    let mut col_x = Vec::with_capacity(per_row);
    let mut x = MARGIN;
    for w in &col_w {
        col_x.push(x);
        x += w + GAP_X;
    }
    let width = (x - GAP_X + MARGIN).max(2.0 * MARGIN + 240.0);
    let mut row_y = Vec::with_capacity(row_h.len());
    let mut y = MARGIN + TITLE;
    for h in &row_h {
        row_y.push(y);
        y += h + GAP_Y;
    }
    let height = (y - GAP_Y + MARGIN).max(MARGIN + TITLE + MARGIN);
    for (i, b) in boxes.iter_mut().enumerate() {
        b.x = col_x.get(i % per_row).copied().unwrap_or(MARGIN);
        b.y = row_y.get(i / per_row).copied().unwrap_or(MARGIN + TITLE);
    }

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
    for (k, rel) in rels.values().enumerate() {
        let (Some(a), Some(b)) = (find(&rel.table), find(&rel.related)) else { continue };
        let (Some(from), Some(to)) = (boxes.get(a), boxes.get(b)) else { continue };
        let (own, other) =
            rel.columns.first().map(|c| (c.1.as_str(), c.2.as_str())).unwrap_or(("", ""));
        let (ya, yb) = (from.row_y(own), to.row_y(other));
        let lane = count(k % 5) * 5.0 - 10.0;
        // Which sides the line leaves from and arrives at, and where it turns.
        let (xa, xb, turn, side_a, side_b) = if from.cell_col < to.cell_col {
            let gap = to.x - GAP_X / 2.0;
            (from.x + from.w, to.x, gap + lane, 1.0f32, -1.0f32)
        } else if from.cell_col > to.cell_col {
            let gap = from.x - GAP_X / 2.0;
            (from.x, to.x + to.w, gap + lane, -1.0, 1.0)
        } else {
            let far = (from.x + from.w).max(to.x + to.w);
            (from.x + from.w, to.x + to.w, far + GAP_X / 3.0 + lane.abs(), 1.0, 1.0)
        };
        prims.push(Prim::Path {
            ops: vec![op::MOVE, xa, ya, op::LINE, turn, ya, op::LINE, turn, yb, op::LINE, xb, yb],
            fill: 0,
            stroke: LINK,
            width: 1.1,
            even_odd: false,
        });
        let one_to_one = rel.flags & 0x1 != 0;
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
