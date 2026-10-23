//! The output sink: the single choke point every extracted byte passes through.
//!
//! Extractors never build a [`Preview`] directly. They open sections and push
//! blocks into a [`Sink`], which
//!
//! 1. **sanitises every string** — control characters, bidirectional
//!    overrides ("Trojan Source"), zero-width characters, Unicode tag
//!    characters and variation-selector smuggling are removed and counted,
//!    link targets and cell text are length-capped;
//! 2. **enforces the output budget** — characters, items (runs, cells, rows,
//!    blocks), blocks and sections — so a hostile document cannot make the
//!    result larger than the sandbox's output limit allows;
//! 3. **records notices** that tell the user what the preview does not show.
//!
//! Because the guarantees live here, a bug in one format parser cannot emit
//! unsanitised text or an unbounded result.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::PreviewPolicy;
use crate::model::{
    Block, FileFormat, Grid, GridCell, GridRow, ImageInfo, Notice, NoticeKind, Paragraph, Preview,
    PreviewStats, Section, SectionKind, style,
};

/// Counters for removed characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanCounts {
    /// Bidirectional formatting characters.
    pub bidi: u64,
    /// Unicode tag characters (`U+E0000..U+E007F`) and variation selectors
    /// supplement (`U+E0100..U+E01EF`) — "ASCII smuggling".
    pub tags: u64,
    /// Zero-width and invisible formatting characters.
    pub zero_width: u64,
    /// C0 / C1 control characters and non-characters.
    pub control: u64,
}

impl CleanCounts {
    /// Total number of removed characters.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.bidi
            .saturating_add(self.tags)
            .saturating_add(self.zero_width)
            .saturating_add(self.control)
    }

    fn add(&mut self, other: Self) {
        self.bidi = self.bidi.saturating_add(other.bidi);
        self.tags = self.tags.saturating_add(other.tags);
        self.zero_width = self.zero_width.saturating_add(other.zero_width);
        self.control = self.control.saturating_add(other.control);
    }
}

enum Class {
    Keep,
    Newline,
    Bidi,
    Tag,
    ZeroWidth,
    Control,
    /// Dropped without being counted (carriage return).
    Silent,
}

fn classify(c: char) -> Class {
    match c {
        '\n' | '\t' => Class::Keep,
        '\r' => Class::Silent,
        '\u{85}' | '\u{2028}' | '\u{2029}' => Class::Newline,
        '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => Class::Control,
        '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{200E}'
        | '\u{200F}'
        | '\u{061C}' => Class::Bidi,
        '\u{E0000}'..='\u{E007F}' | '\u{E0100}'..='\u{E01EF}' => Class::Tag,
        '\u{200B}'
        | '\u{2060}'..='\u{2064}'
        | '\u{FEFF}'
        | '\u{180E}'
        | '\u{FFF9}'..='\u{FFFC}'
        | '\u{034F}'
        | '\u{115F}'
        | '\u{1160}'
        | '\u{3164}'
        | '\u{FFA0}' => Class::ZeroWidth,
        '\u{FDD0}'..='\u{FDEF}' | '\u{FFFE}' | '\u{FFFF}' => Class::Control,
        c if (u32::from(c) & 0xFFFE) == 0xFFFE => Class::Control,
        _ => Class::Keep,
    }
}

/// Removes control and invisible characters from `s`.
///
/// Returns the input unchanged (no allocation) when it is already clean.
#[must_use]
pub fn clean_text<'a>(s: &'a str, counts: &mut CleanCounts) -> Cow<'a, str> {
    // Fast path: printable ASCII plus \n and \t.
    if s.bytes().all(|b| matches!(b, b'\n' | b'\t' | 0x20..=0x7E)) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut local = CleanCounts::default();
    let mut changed = false;
    for c in s.chars() {
        match classify(c) {
            Class::Keep => out.push(c),
            Class::Newline => {
                out.push('\n');
                changed = true;
            }
            Class::Silent => changed = true,
            Class::Bidi => {
                local.bidi = local.bidi.saturating_add(1);
                changed = true;
            }
            Class::Tag => {
                local.tags = local.tags.saturating_add(1);
                changed = true;
            }
            Class::ZeroWidth => {
                local.zero_width = local.zero_width.saturating_add(1);
                changed = true;
            }
            Class::Control => {
                local.control = local.control.saturating_add(1);
                changed = true;
            }
        }
    }
    counts.add(local);
    if changed { Cow::Owned(out) } else { Cow::Borrowed(s) }
}

fn clean_in_place(s: &mut String, counts: &mut CleanCounts, max_bytes: usize) {
    if let Cow::Owned(cleaned) = clean_text(s, counts) {
        *s = cleaned;
    }
    if s.len() > max_bytes {
        let kept = arca_container::text::truncate_utf8(s, max_bytes).len();
        s.truncate(kept);
    }
}

/// Upper bound on the number of listed "part skipped" notices.
const MAX_SKIPPED_NOTICES: usize = 32;

/// Handle of an open section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SectionId(usize);

/// Accumulates the preview under the policy's budget.
#[derive(Debug)]
pub(crate) struct Sink<'p> {
    pub(crate) policy: &'p PreviewPolicy,
    sections: Vec<Section>,
    /// Sections that exist only so extractors can keep writing to them; dropped at the end.
    discarded: Vec<usize>,
    notices: Vec<Notice>,
    counted: BTreeMap<(u8, &'static str), u64>,
    chars: usize,
    items: usize,
    blocks: usize,
    clean: CleanCounts,
    full: bool,
    truncated: bool,
    /// Hidden items left out so far.
    omitted: u64,
}

/// `true` if a block holds nothing to show.
fn is_empty_block(block: &Block) -> bool {
    match block {
        Block::Para(p) => p.runs.is_empty() && p.marker.is_empty(),
        Block::Grid(g) => g.rows.is_empty() && !g.truncated,
        _ => false,
    }
}

/// Cost of a block against the budget.
#[derive(Debug, Clone, Copy, Default)]
struct Weight {
    chars: usize,
    items: usize,
}

impl Weight {
    fn add(&mut self, other: Self) {
        self.chars = self.chars.saturating_add(other.chars);
        self.items = self.items.saturating_add(other.items);
    }
}

fn weigh_para(p: &Paragraph) -> Weight {
    let mut w = Weight { chars: p.marker.len(), items: 1_usize.saturating_add(p.runs.len()) };
    for r in &p.runs {
        w.chars = w.chars.saturating_add(r.text.len()).saturating_add(r.href.len());
    }
    w
}

fn weigh(block: &Block) -> Weight {
    match block {
        Block::Para(p) => weigh_para(p),
        Block::Table(t) => {
            let mut w = Weight { chars: 0, items: 1 };
            for row in &t.rows {
                w.items = w.items.saturating_add(1);
                for cell in &row.cells {
                    w.items = w.items.saturating_add(1);
                    for p in &cell.paras {
                        w.add(weigh_para(p));
                    }
                }
            }
            w
        }
        Block::Grid(g) => {
            let mut w = Weight { chars: 0, items: 1 };
            for row in &g.rows {
                w.items = w.items.saturating_add(1).saturating_add(row.cells.len());
                for c in &row.cells {
                    w.chars = w.chars.saturating_add(c.text.len());
                }
            }
            w
        }
        Block::Image(i) => Weight { chars: i.alt.len().saturating_add(i.media.len()), items: 1 },
        Block::Rule => Weight { chars: 0, items: 1 },
    }
}

impl<'p> Sink<'p> {
    pub(crate) fn new(policy: &'p PreviewPolicy) -> Self {
        Self {
            policy,
            sections: Vec::new(),
            discarded: Vec::new(),
            notices: Vec::new(),
            counted: BTreeMap::new(),
            chars: 0,
            items: 0,
            blocks: 0,
            clean: CleanCounts::default(),
            full: false,
            truncated: false,
            omitted: 0,
        }
    }

    /// `true` once a budget is exhausted; extractors should stop.
    pub(crate) const fn is_full(&self) -> bool {
        self.full
    }

    /// Items (runs, cells, rows, blocks) still available.
    pub(crate) fn items_left(&self) -> usize {
        self.policy.max_items.saturating_sub(self.items)
    }

    /// Records that content was cut. Idempotent per `what`.
    pub(crate) fn truncated(&mut self, what: &'static str) {
        self.truncated = true;
        let key = (NoticeKind::Truncated as u8, what);
        if self.counted.insert(key, 1).is_none() {
            self.notices.push(Notice {
                kind: NoticeKind::Truncated,
                count: 0,
                detail: what.to_owned(),
            });
        }
    }

    fn stop(&mut self, what: &'static str) {
        self.full = true;
        self.truncated(what);
    }

    /// Adds `n` to a counted notice such as hidden content or images.
    pub(crate) fn count(&mut self, kind: NoticeKind, detail: &'static str, n: u64) {
        let slot = self.counted.entry((kind as u8, detail)).or_insert(0);
        *slot = slot.saturating_add(n);
    }

    /// Records a one-off notice (deduplicated by kind and detail).
    pub(crate) fn note(&mut self, kind: NoticeKind, detail: &'static str) {
        let key = (kind as u8, detail);
        if self.counted.insert(key, 1).is_none() {
            self.notices.push(Notice { kind, count: 0, detail: detail.to_owned() });
        }
    }

    /// Like [`Sink::note`] for a detail computed at run time (deduplicated by kind and detail).
    pub(crate) fn note_detail(&mut self, kind: NoticeKind, detail: String) {
        if !self.notices.iter().any(|n| n.kind == kind && n.detail == detail) {
            self.notices.push(Notice { kind, count: 0, detail });
        }
    }

    /// Records a part that could not be read (at most [`MAX_SKIPPED_NOTICES`] are listed).
    pub(crate) fn skipped(&mut self, what: &str) {
        let listed = self.notices.iter().filter(|n| n.kind == NoticeKind::Skipped).count();
        if listed < MAX_SKIPPED_NOTICES {
            self.notices.push(Notice {
                kind: NoticeKind::Skipped,
                count: 0,
                detail: arca_container::text::display_safe(what, 128),
            });
        }
    }

    /// Opens a new section.
    pub(crate) fn open_section(
        &mut self,
        kind: SectionKind,
        name: &str,
        hidden: bool,
    ) -> Option<SectionId> {
        if self.sections.len() >= self.policy.max_sections {
            self.stop("sections");
            return None;
        }
        let mut name = name.to_owned();
        clean_in_place(&mut name, &mut self.clean, 512);
        if hidden {
            let kind_key = match kind {
                SectionKind::Sheet => "sheets",
                SectionKind::Slide => "slides",
                _ => "sections",
            };
            let notice = if self.policy.reveal_hidden {
                NoticeKind::HiddenShown
            } else {
                NoticeKind::HiddenOmitted
            };
            self.count(notice, kind_key, 1);
            if !self.policy.reveal_hidden {
                self.discarded.push(self.sections.len());
            }
        }
        self.sections.push(Section { kind, name, hidden, blocks: Vec::new() });
        Some(SectionId(self.sections.len().saturating_sub(1)))
    }

    /// Number of blocks already in a section.
    #[cfg(test)]
    pub(crate) fn section_len(&self, id: SectionId) -> usize {
        self.sections.get(id.0).map_or(0, |s| s.blocks.len())
    }

    /// Removes a section that turned out to be empty and was the last one opened.
    pub(crate) fn drop_if_empty(&mut self, id: SectionId) {
        if id.0.saturating_add(1) == self.sections.len()
            && self.sections.get(id.0).is_some_and(|s| s.blocks.is_empty())
        {
            self.sections.pop();
            self.discarded.retain(|d| *d != id.0);
        }
    }

    /// Sanitises and appends a block. Returns `false` (and drops the block)
    /// when a budget is exhausted.
    pub(crate) fn push(&mut self, id: SectionId, mut block: Block) -> bool {
        if self.full {
            return false;
        }
        if self.discarded.contains(&id.0) {
            // A hidden section that was not asked for: accepted and forgotten.
            return true;
        }
        let omitted_before = self.omitted;
        self.sanitize(&mut block);
        if self.omitted > omitted_before && is_empty_block(&block) {
            return true;
        }
        let w = weigh(&block);
        if self.blocks >= self.policy.max_blocks {
            self.stop("blocks");
            return false;
        }
        if self.chars.saturating_add(w.chars) > self.policy.max_chars {
            self.stop("characters");
            return false;
        }
        if self.items.saturating_add(w.items) > self.policy.max_items {
            self.stop("items");
            return false;
        }
        let Some(section) = self.sections.get_mut(id.0) else { return false };
        self.chars = self.chars.saturating_add(w.chars);
        self.items = self.items.saturating_add(w.items);
        self.blocks = self.blocks.saturating_add(1);
        section.blocks.push(block);
        true
    }

    /// Convenience: pushes a paragraph.
    pub(crate) fn para(&mut self, id: SectionId, p: Paragraph) -> bool {
        self.push(id, Block::Para(p))
    }

    /// Convenience: pushes a picture placeholder and counts it.
    pub(crate) fn image(&mut self, id: SectionId, info: ImageInfo) -> bool {
        self.count(NoticeKind::ImagesNotShown, "", 1);
        self.push(id, Block::Image(info))
    }

    fn sanitize(&mut self, block: &mut Block) {
        let max_cell = self.policy.max_cell_bytes;
        let max_href = self.policy.max_href_bytes;
        let max_runs = self.policy.max_runs_per_paragraph;
        let mut cut = false;
        let mut hidden_runs = 0u64;
        let mut tracked = 0u64;
        let reveal = self.policy.reveal_hidden;
        let mut omitted_runs = 0u64;
        let mut omitted_rows = 0u64;
        {
            let counts = &mut self.clean;
            let mut para = |p: &mut Paragraph, cut: &mut bool| {
                if p.runs.len() > max_runs {
                    p.runs.truncate(max_runs);
                    *cut = true;
                }
                clean_in_place(&mut p.marker, counts, 32);
                for r in &mut p.runs {
                    clean_in_place(&mut r.text, counts, max_cell);
                    clean_in_place(&mut r.href, counts, max_href);
                    r.style &= style::ALL;
                    if r.style & style::HIDDEN != 0 {
                        hidden_runs = hidden_runs.saturating_add(1);
                    }
                    if r.style & (style::DELETED | style::INSERTED) != 0 {
                        tracked = tracked.saturating_add(1);
                    }
                }
                p.runs.retain(|r| !r.text.is_empty());
                if !reveal {
                    let before = p.runs.len();
                    p.runs.retain(|r| r.style & style::HIDDEN == 0);
                    omitted_runs = omitted_runs
                        .saturating_add(u64::try_from(before - p.runs.len()).unwrap_or(0));
                }
            };
            match block {
                Block::Para(p) => para(p, &mut cut),
                Block::Table(t) => {
                    for row in &mut t.rows {
                        for cell in &mut row.cells {
                            cell.span = cell.span.clamp(1, 1024);
                            for p in &mut cell.paras {
                                para(p, &mut cut);
                            }
                        }
                    }
                }
                Block::Grid(g) => {
                    if !reveal {
                        let before = g.rows.len();
                        g.rows.retain(|r| !r.hidden);
                        omitted_rows = omitted_rows
                            .saturating_add(u64::try_from(before - g.rows.len()).unwrap_or(0));
                    }
                    for row in &mut g.rows {
                        for c in &mut row.cells {
                            clean_in_place(&mut c.text, counts, max_cell);
                            clean_in_place(&mut c.href, counts, max_href);
                        }
                    }
                }
                Block::Image(i) => {
                    clean_in_place(&mut i.alt, counts, 1024);
                    clean_in_place(&mut i.media, counts, 32);
                }
                Block::Rule => {}
            }
        }
        if cut {
            self.truncated("runs per paragraph");
        }
        if hidden_runs > 0 && reveal {
            self.count(NoticeKind::HiddenShown, "text", hidden_runs);
        }
        if omitted_runs > 0 {
            self.count(NoticeKind::HiddenOmitted, "text", omitted_runs);
        }
        if omitted_rows > 0 {
            self.count(NoticeKind::HiddenOmitted, "rows", omitted_rows);
        }
        self.omitted = self.omitted.saturating_add(omitted_runs).saturating_add(omitted_rows);
        if tracked > 0 {
            self.count(NoticeKind::TrackedChanges, "", tracked);
        }
    }

    /// Finishes the preview.
    pub(crate) fn finish(mut self, format: FileFormat) -> Preview {
        let clean = self.clean;
        if clean.total() > 0 {
            self.notices.push(Notice {
                kind: NoticeKind::InvisibleRemoved,
                count: clean.total(),
                detail: format!(
                    "bidi={},tags={},zero-width={},control={}",
                    clean.bidi, clean.tags, clean.zero_width, clean.control
                ),
            });
        }
        let counted = std::mem::take(&mut self.counted);
        for ((code, detail), n) in counted {
            let Some(kind) = [
                NoticeKind::ImagesNotShown,
                NoticeKind::HiddenShown,
                NoticeKind::HiddenOmitted,
                NoticeKind::TrackedChanges,
                NoticeKind::FormulasNotEvaluated,
                NoticeKind::FormulasCalculated,
                NoticeKind::DeletedRecords,
            ]
            .into_iter()
            .find(|k| *k as u8 == code) else {
                continue;
            };
            if n > 0 {
                self.notices.push(Notice { kind, count: n, detail: detail.to_owned() });
            }
        }
        let discarded = std::mem::take(&mut self.discarded);
        let sections: Vec<Section> = self
            .sections
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !discarded.contains(i))
            .map(|(_, s)| s)
            .collect();
        Preview {
            format,
            sections,
            notices: self.notices,
            stats: PreviewStats {
                chars: u64::try_from(self.chars).unwrap_or(u64::MAX),
                items: u64::try_from(self.items).unwrap_or(u64::MAX),
                truncated: self.truncated,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Builders shared by the extractors
// ---------------------------------------------------------------------------

/// Builds a paragraph from pieces, merging adjacent runs with equal formatting.
#[derive(Debug, Default)]
pub(crate) struct ParaBuilder {
    runs: Vec<crate::model::Run>,
}

impl ParaBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Appends text. Empty text is ignored.
    pub(crate) fn text(&mut self, text: &str, style: u16, href: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut()
            && last.style == style
            && last.href == href
        {
            last.text.push_str(text);
            return;
        }
        self.runs.push(crate::model::Run { text: text.to_owned(), style, href: href.to_owned() });
    }

    /// `true` if nothing at all has been added.
    pub(crate) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Takes the paragraph, leaving the builder empty.
    pub(crate) fn take(&mut self, style: crate::model::ParaStyle) -> Paragraph {
        Paragraph { style, runs: std::mem::take(&mut self.runs), ..Paragraph::default() }
    }
}

/// Builds a [`Grid`] row by row under a per-grid cell budget.
#[derive(Debug)]
pub(crate) struct GridBuilder {
    grid: Grid,
    cells: usize,
    max_cells: usize,
    max_rows: u32,
    max_cols: u32,
    current: Option<GridRow>,
}

impl GridBuilder {
    /// `allowance` is the number of cells the sink can still take.
    pub(crate) fn new(policy: &PreviewPolicy, allowance: usize) -> Self {
        Self {
            grid: Grid::default(),
            cells: 0,
            max_cells: policy.max_grid_cells.min(allowance),
            max_rows: policy.max_grid_rows,
            max_cols: policy.max_grid_cols,
            current: None,
        }
    }

    /// Starts row `index` (one-based). Returns `false` once the grid is full.
    pub(crate) fn row(&mut self, index: u32, hidden: bool) -> bool {
        self.flush_row();
        if index == 0 || index > self.max_rows {
            self.grid.truncated = true;
            return false;
        }
        if self.cells >= self.max_cells {
            self.grid.truncated = true;
            return false;
        }
        self.current = Some(GridRow { index, hidden, cells: Vec::new() });
        true
    }

    /// Adds a cell to the current row. Returns `false` once the grid is full.
    pub(crate) fn cell(&mut self, col: u32, kind: crate::model::CellKind, text: String) -> bool {
        self.linked_cell(col, kind, text, String::new())
    }

    /// Like [`cell`](Self::cell), for a cell that links somewhere (the address is only shown).
    pub(crate) fn linked_cell(
        &mut self,
        col: u32,
        kind: crate::model::CellKind,
        text: String,
        href: String,
    ) -> bool {
        if col >= self.max_cols || text.is_empty() {
            if col >= self.max_cols {
                self.grid.truncated = true;
            }
            return true;
        }
        if self.cells >= self.max_cells {
            self.grid.truncated = true;
            return false;
        }
        let Some(row) = self.current.as_mut() else { return false };
        row.cells.push(GridCell { col, kind, text, href });
        self.cells = self.cells.saturating_add(1);
        self.grid.cols = self.grid.cols.max(col.saturating_add(1));
        true
    }

    fn flush_row(&mut self) {
        if let Some(row) = self.current.take()
            && !row.cells.is_empty()
        {
            self.grid.rows.push(row);
        }
    }

    pub(crate) fn finish(mut self) -> Grid {
        self.flush_row();
        self.grid
    }
}

/// A hyperlink of a range of spreadsheet cells (rows one-based, columns zero-based, inclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellLink {
    pub(crate) row1: u32,
    pub(crate) row2: u32,
    pub(crate) col1: u32,
    pub(crate) col2: u32,
    pub(crate) href: String,
}

/// Links kept per sheet.
const MAX_LINKS: usize = 20_000;
/// Rows of one link that are looked up (a link over a whole column covers only its first rows).
const MAX_LINK_ROWS: u32 = 256;
/// Row/link pairs in total, and cells examined while attaching them: the work is bounded
/// whatever the file declares.
const MAX_LINK_ROW_PAIRS: usize = 100_000;
const MAX_LINK_CELL_CHECKS: usize = 2_000_000;

impl Grid {
    /// Attaches hyperlinks to the cells they cover. Only cells that exist are marked (a link on an
    /// empty cell has nothing to show), the first link of a cell wins, and the work is bounded.
    pub(crate) fn attach_links(&mut self, links: &[CellLink]) {
        if links.is_empty() {
            return;
        }
        let mut by_row: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        let mut pairs = 0usize;
        'links: for (i, l) in links.iter().enumerate().take(MAX_LINKS) {
            let last = l.row1.saturating_add(l.row2.saturating_sub(l.row1).min(MAX_LINK_ROWS));
            for r in l.row1..=last {
                if pairs >= MAX_LINK_ROW_PAIRS {
                    break 'links;
                }
                by_row.entry(r).or_default().push(i);
                pairs += 1;
            }
        }
        let mut checks = 0usize;
        for row in &mut self.rows {
            let Some(ids) = by_row.get(&row.index) else { continue };
            for &i in ids {
                let Some(l) = links.get(i) else { continue };
                for cell in &mut row.cells {
                    checks += 1;
                    if checks > MAX_LINK_CELL_CHECKS {
                        return;
                    }
                    if cell.col >= l.col1 && cell.col <= l.col2 && cell.href.is_empty() {
                        cell.href.clone_from(&l.href);
                    }
                }
            }
        }
    }
}

/// Cuts a link target to `max` bytes at a character boundary.
pub(crate) fn clip_href(href: &str, max: usize) -> String {
    if href.len() <= max {
        return href.to_owned();
    }
    let mut end = max;
    while end > 0 && !href.is_char_boundary(end) {
        end -= 1;
    }
    href.get(..end).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellKind, ParaStyle, Row, Run, Table};

    fn policy() -> PreviewPolicy {
        PreviewPolicy::default()
    }

    #[test]
    fn clean_text_removes_and_counts() {
        let mut c = CleanCounts::default();
        let out = clean_text("a\u{202E}b\u{200B}c\u{E0041}d\u{1}e\r\nf\u{2028}g\u{7F}", &mut c);
        assert_eq!(out, "abcde\nf\ng");
        assert_eq!(c, CleanCounts { bidi: 1, tags: 1, zero_width: 1, control: 2 });
        assert_eq!(c.total(), 5);
    }

    #[test]
    fn clean_text_borrows_when_clean() {
        let mut c = CleanCounts::default();
        assert!(matches!(clean_text("plain\ttext\nline", &mut c), Cow::Borrowed(_)));
        assert!(matches!(clean_text("Привет, мир! 日本語 😀", &mut c), Cow::Borrowed(_)));
        assert_eq!(c.total(), 0);
        // ZWNJ / ZWJ are legitimate in Persian and Indic scripts and stay.
        assert_eq!(clean_text("می\u{200C}خواهم", &mut c), "می\u{200C}خواهم");
        assert_eq!(c.total(), 0);
    }

    #[test]
    fn all_dangerous_ranges_are_covered() {
        let mut c = CleanCounts::default();
        for cp in (0x202A..=0x202E).chain(0x2066..=0x2069).chain([0x200E, 0x200F, 0x061C]) {
            let s = char::from_u32(cp).map(String::from).unwrap_or_default();
            assert_eq!(clean_text(&s, &mut c), "", "{cp:X}");
        }
        for cp in 0xE0000..=0xE007F {
            let s = char::from_u32(cp).map(String::from).unwrap_or_default();
            assert_eq!(clean_text(&s, &mut c), "", "{cp:X}");
        }
        for cp in [0x80u32, 0x9F, 0xFDD0, 0xFFFE, 0xFFFF, 0x1FFFE, 0x10FFFF] {
            let s = char::from_u32(cp).map(String::from).unwrap_or_default();
            assert_eq!(clean_text(&s, &mut c), "", "{cp:X}");
        }
    }

    #[test]
    fn hidden_content_is_left_out_and_counted_when_not_requested() {
        let p = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
        let mut sink = Sink::new(&p);
        let sec = sink.open_section(SectionKind::Body, "", false).unwrap();
        let mixed = Paragraph {
            runs: vec![
                Run { text: "seen ".into(), style: 0, href: String::new() },
                Run { text: "secret".into(), style: style::HIDDEN, href: String::new() },
            ],
            ..Paragraph::default()
        };
        let only_hidden = Paragraph {
            runs: vec![Run { text: "gone".into(), style: style::HIDDEN, href: String::new() }],
            ..Paragraph::default()
        };
        assert!(sink.para(sec, mixed));
        assert!(sink.para(sec, only_hidden));
        let grid = Grid {
            cols: 1,
            rows: vec![
                GridRow {
                    index: 1,
                    hidden: false,
                    cells: vec![GridCell {
                        col: 0,
                        kind: CellKind::Text,
                        text: "a".into(),
                        href: String::new(),
                    }],
                },
                GridRow {
                    index: 2,
                    hidden: true,
                    cells: vec![GridCell {
                        col: 0,
                        kind: CellKind::Text,
                        text: "b".into(),
                        href: String::new(),
                    }],
                },
            ],
            truncated: false,
        };
        assert!(sink.push(sec, Block::Grid(grid)));
        let sheet = sink.open_section(SectionKind::Sheet, "Hidden", true).unwrap();
        assert!(sink.para(sheet, Paragraph::plain("inside a hidden sheet")));
        let out = sink.finish(FileFormat::Docx);
        assert_eq!(out.sections.len(), 1, "the hidden sheet is not part of the result");
        let body = out.sections.first().unwrap();
        assert_eq!(body.blocks.len(), 2, "the all-hidden paragraph is dropped");
        let Some(Block::Para(first)) = body.blocks.first() else { panic!("para") };
        assert_eq!(first.text(), "seen ");
        let Some(Block::Grid(g)) = body.blocks.get(1) else { panic!("grid") };
        assert_eq!(g.rows.len(), 1);
        let omitted = |d: &str| {
            out.notices
                .iter()
                .find(|n| n.kind == NoticeKind::HiddenOmitted && n.detail == d)
                .map(|n| n.count)
        };
        assert_eq!(omitted("text"), Some(2));
        assert_eq!(omitted("rows"), Some(1));
        assert_eq!(omitted("sheets"), Some(1));
        assert!(!out.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown));
    }

    #[test]
    fn sink_sanitizes_everything_it_stores() {
        let p = policy();
        let mut sink = Sink::new(&p);
        let sec = sink.open_section(SectionKind::Body, "na\u{202E}me", false).unwrap();
        let para = Paragraph {
            marker: "\u{1}•".into(),
            runs: vec![Run {
                text: "x\u{200B}y".into(),
                style: style::HIDDEN | 0x8000,
                href: "http://a\u{0}b".into(),
            }],
            ..Paragraph::default()
        };
        assert!(sink.para(sec, para));
        let table = Block::Table(Table {
            rows: vec![Row {
                cells: vec![crate::model::Cell {
                    span: 0,
                    header: false,
                    paras: vec![Paragraph::plain("t\u{E0001}")],
                }],
            }],
        });
        assert!(sink.push(sec, table));
        let grid = Block::Grid(Grid {
            cols: 1,
            rows: vec![GridRow {
                index: 1,
                hidden: false,
                cells: vec![GridCell {
                    col: 0,
                    kind: CellKind::Text,
                    text: "g\u{202A}".into(),
                    href: "http://c\u{0}d".into(),
                }],
            }],
            truncated: false,
        });
        assert!(sink.push(sec, grid));
        assert!(sink.image(
            sec,
            ImageInfo {
                alt: "a\u{1}".into(),
                media: "png".into(),
                bytes: 1,
                ..ImageInfo::default()
            }
        ));
        let out = sink.finish(FileFormat::Docx);
        let Some(section) = out.sections.first() else { panic!("no section") };
        assert_eq!(section.name, "name");
        let Some(Block::Para(para)) = section.blocks.first() else { panic!("no para") };
        assert_eq!(para.marker, "•");
        assert_eq!(para.runs.first().map(|r| r.text.as_str()), Some("xy"));
        assert_eq!(para.runs.first().map(|r| r.href.as_str()), Some("http://ab"));
        // Unknown style bits are masked off.
        assert_eq!(para.runs.first().map(|r| r.style), Some(style::HIDDEN));
        let Some(Block::Grid(g)) = section.blocks.get(2) else { panic!("no grid") };
        let cell = g.rows.first().and_then(|r| r.cells.first()).expect("a cell");
        assert_eq!((cell.text.as_str(), cell.href.as_str()), ("g", "http://cd"));
        let Some(Block::Table(t)) = section.blocks.get(1) else { panic!("no table") };
        assert_eq!(t.rows.first().and_then(|r| r.cells.first()).map(|c| c.span), Some(1));
        assert!(out.notices.iter().any(|n| n.kind == NoticeKind::InvisibleRemoved && n.count == 8));
        assert!(out.notices.iter().any(|n| n.kind == NoticeKind::ImagesNotShown && n.count == 1));
        assert!(
            out.notices.iter().any(|n| n.kind == NoticeKind::HiddenShown && n.detail == "text")
        );
    }

    #[test]
    fn links_are_attached_to_the_cells_they_cover() {
        let cell = |col: u32, text: &str| GridCell {
            col,
            kind: CellKind::Text,
            text: text.into(),
            href: String::new(),
        };
        let mut g = Grid {
            cols: 4,
            rows: vec![
                GridRow { index: 1, hidden: false, cells: vec![cell(0, "a"), cell(2, "c")] },
                GridRow { index: 3, hidden: false, cells: vec![cell(1, "x"), cell(3, "y")] },
            ],
            truncated: false,
        };
        let link = |r1, r2, c1, c2, h: &str| CellLink {
            row1: r1,
            row2: r2,
            col1: c1,
            col2: c2,
            href: h.into(),
        };
        g.attach_links(&[
            link(1, 1, 0, 0, "https://a.example/"),
            link(1, 3, 1, 3, "https://range.example/"),
            link(1, 1, 0, 0, "https://second.example/"),
            link(9, 9, 0, 0, "https://nowhere.example/"),
        ]);
        let href = |r: usize, c: usize| g.rows[r].cells[c].href.as_str();
        assert_eq!(href(0, 0), "https://a.example/", "the first link of a cell wins");
        assert_eq!(href(0, 1), "https://range.example/");
        assert_eq!(href(1, 0), "https://range.example/", "a range reaches later rows");
        assert_eq!(href(1, 1), "https://range.example/");
        // No links, and a link far beyond any row: nothing happens.
        let mut same = g.clone();
        same.attach_links(&[]);
        same.attach_links(&[link(u32::MAX - 1, u32::MAX, 0, u32::MAX, "https://far.example/")]);
        assert_eq!(same, g);
        // A link over a whole column is bounded, not expanded to a million rows.
        let mut many = Grid::default();
        many.attach_links(&[link(1, 1_048_576, 0, 16_383, "https://col.example/")]);
        assert_eq!(clip_href("https://é.example/", 9), "https://");
        assert_eq!(clip_href("short", 99), "short");
    }

    #[test]
    fn budgets_stop_the_flood() {
        let p = PreviewPolicy { max_chars: 100, max_blocks: 1000, ..policy() };
        let mut sink = Sink::new(&p);
        let sec = sink.open_section(SectionKind::Body, "", false).unwrap();
        let mut accepted = 0;
        for _ in 0..50 {
            if sink.para(sec, Paragraph::plain("0123456789")) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 10);
        assert!(sink.is_full());
        let out = sink.finish(FileFormat::PlainText);
        assert!(out.stats.truncated);
        assert!(out.notices.iter().any(|n| n.kind == NoticeKind::Truncated));

        let p = PreviewPolicy { max_blocks: 3, ..policy() };
        let mut sink = Sink::new(&p);
        let sec = sink.open_section(SectionKind::Body, "", false).unwrap();
        for _ in 0..10 {
            sink.push(sec, Block::Rule);
        }
        assert!(sink.is_full());
        assert_eq!(sink.section_len(sec), 3);

        let p = PreviewPolicy { max_sections: 2, ..policy() };
        let mut sink = Sink::new(&p);
        assert!(sink.open_section(SectionKind::Sheet, "a", false).is_some());
        assert!(sink.open_section(SectionKind::Sheet, "b", true).is_some());
        assert!(sink.open_section(SectionKind::Sheet, "c", false).is_none());
        assert!(sink.is_full());
    }

    #[test]
    fn runs_per_paragraph_are_capped() {
        let p = PreviewPolicy { max_runs_per_paragraph: 3, ..policy() };
        let mut sink = Sink::new(&p);
        let sec = sink.open_section(SectionKind::Body, "", false).unwrap();
        let runs = (0..10).map(|i| Run { text: format!("{i}"), ..Run::default() }).collect();
        assert!(sink.para(sec, Paragraph { runs, ..Paragraph::default() }));
        let out = sink.finish(FileFormat::Docx);
        let Some(Block::Para(para)) = out.sections.first().and_then(|s| s.blocks.first()) else {
            panic!("no para")
        };
        assert_eq!(para.runs.len(), 3);
        assert!(out.stats.truncated);
    }

    #[test]
    fn para_builder_merges_equal_runs() {
        let mut b = ParaBuilder::new();
        b.text("a", 0, "");
        b.text("b", 0, "");
        b.text("c", style::BOLD, "");
        b.text("", style::BOLD, "");
        b.text("d", style::BOLD, "u");
        let p = b.take(ParaStyle::Body);
        assert_eq!(p.runs.len(), 3);
        assert_eq!(p.runs.first().map(|r| r.text.as_str()), Some("ab"));
        assert!(b.is_empty());
    }

    #[test]
    fn grid_builder_limits() {
        let p = PreviewPolicy { max_grid_cells: 3, max_grid_cols: 4, max_grid_rows: 5, ..policy() };
        let mut g = GridBuilder::new(&p, 100);
        assert!(g.row(1, false));
        assert!(g.cell(0, CellKind::Text, "a".into()));
        assert!(g.cell(9, CellKind::Text, "beyond columns".into())); // skipped, not fatal
        assert!(g.cell(3, CellKind::Number, "1".into()));
        assert!(g.row(2, false));
        assert!(g.cell(1, CellKind::Text, "c".into()));
        assert!(!g.cell(2, CellKind::Text, "over budget".into()));
        assert!(!g.row(3, false));
        assert!(!g.row(6, false));
        let grid = g.finish();
        assert!(grid.truncated);
        assert_eq!(grid.cols, 4);
        assert_eq!(grid.rows.len(), 2);
        assert!(GridBuilder::new(&p, 0).finish().rows.is_empty());
    }
}
