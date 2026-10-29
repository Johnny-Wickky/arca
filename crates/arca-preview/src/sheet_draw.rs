//! Worksheets drawn as printed pages: a grid with the column widths and row heights of the
//! file, fills, borders, fonts, alignment, wrapped text and merged cells, split into pages the
//! way the sheet's page setup would print it.
//!
//! The same rules as for the pages of a PDF ([`crate::page`]) hold. Nothing is decoded or run:
//! pictures and charts are not drawn (they are named in a notice), fonts are the app's system
//! fonts, and the app gets numbers, colours and strings only. The text of a cell is the text of
//! the reading view ([`crate::preview`]) of the same file, so the page and the text preview never
//! disagree about what a cell says; this module adds how it *looks*.
//!
//! * [`sheet_list`] lists the sheets (hidden ones only when the policy reveals hidden content);
//! * [`page_count`] says how many pages a sheet prints on;
//! * [`render_sheet_page`] draws one of them.
//!
//! XLSX, ODS and XLS (BIFF8; Excel 5/95 gets sizes and merged cells only) are supported.
//! Everything a document declares is bounded: a used range of `A1:XFD1048576` is split into at
//! most [`model::MAX_PAGES`] pages, a million merged ranges are cut at a hundred thousand, sizes
//! that are not numbers or are absurd fall back to the defaults, and the work done for one page
//! is capped. Whatever was cut is said in the notices and in [`PageModel::truncated`].

mod model;
mod ods;
mod paint;
mod text;
mod xls;
mod xlsx;

#[cfg(test)]
mod tests;

use crate::detect::{self, Doc};
use crate::model::{Block, FileFormat, Grid, Notice, NoticeKind, Section, SectionKind};
use crate::page::PageModel;
use crate::sink::{CleanCounts, Sink, clean_text};
use crate::{PreviewError, PreviewPolicy};

use self::model::{Layout, Pages, Sheet, Styles, assemble, paginate};

/// One sheet of a workbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetInfo {
    /// The sheet's name, cleaned of control and invisible characters.
    pub name: String,
    /// The document marks the sheet hidden (it is only listed when the policy reveals hidden
    /// content).
    pub hidden: bool,
}

/// The sheets of a workbook.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SheetList {
    /// The sheets, in order; a sheet is addressed by its position here.
    pub sheets: Vec<SheetInfo>,
    /// Hidden sheets left out because the policy does not reveal hidden content.
    pub hidden_omitted: u64,
}

/// One drawn page of a sheet and what is known about its fidelity.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetPage {
    /// The page to draw.
    pub page: PageModel,
    /// What the page leaves out or only approximates, and what was cut.
    pub notices: Vec<Notice>,
}

/// A sheet found in a workbook.
#[derive(Debug, Clone)]
struct Meta {
    name: String,
    hidden: bool,
}

/// The look of the sheet that was asked for.
struct Picked {
    /// Position among all the sheets of the workbook (hidden ones included).
    index: usize,
    styles: Styles,
    layout: Layout,
}

/// What a format reader returns.
struct Read {
    metas: Vec<Meta>,
    picked: Option<Picked>,
}

fn is_xlsx(format: FileFormat) -> bool {
    matches!(format, FileFormat::Xlsx | FileFormat::Xlsm | FileFormat::Xltx | FileFormat::Xltm)
}

fn doc_format(doc: &Doc<'_>) -> FileFormat {
    match doc {
        Doc::Opc { format, .. }
        | Doc::Odf { format, .. }
        | Doc::Cfb { format, .. }
        | Doc::Picture { format, .. }
        | Doc::Access { format, .. }
        | Doc::Text { format, .. } => *format,
        Doc::Rtf(_) => FileFormat::Rtf,
        Doc::Pdf(_) => FileFormat::Pdf,
    }
}

/// A name for the list: cleaned, one line, not long.
fn clean_name(s: &str) -> String {
    let mut counts = CleanCounts::default();
    let cleaned = clean_text(s, &mut counts);
    let flat: String = cleaned.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    arca_container::text::truncate_utf8(&flat, 512).to_owned()
}

fn check_size(data: &[u8], policy: &PreviewPolicy) -> Result<(), PreviewError> {
    if data.len() > policy.max_input_bytes {
        return Err(PreviewError::TooLarge(policy.max_input_bytes));
    }
    Ok(())
}

/// Lists the sheets of a workbook (XLSX, ODS, XLS).
///
/// # Errors
/// See [`PreviewError`]; [`PreviewError::Unsupported`] for other formats.
pub fn sheet_list(data: &[u8], policy: &PreviewPolicy) -> Result<SheetList, PreviewError> {
    check_size(data, policy)?;
    let doc = detect::open(data, policy)?;
    let reveal = policy.reveal_hidden;
    let read = match doc {
        Doc::Opc { mut pkg, format } if is_xlsx(format) => xlsx::read(&mut pkg, None, reveal)?,
        Doc::Odf { mut pkg, format: FileFormat::Ods } => ods::read(&mut pkg, None, reveal)?,
        Doc::Cfb { cf, format: FileFormat::Xls } => xls::read(&cf, None, reveal)?,
        other => return Err(PreviewError::Unsupported(doc_format(&other))),
    };
    let mut out = SheetList::default();
    for m in read.metas {
        if m.hidden && !reveal {
            out.hidden_omitted = out.hidden_omitted.saturating_add(1);
        } else {
            out.sheets.push(SheetInfo { name: clean_name(&m.name), hidden: m.hidden });
        }
    }
    Ok(out)
}

/// A sheet loaded, ready to be paginated and painted.
struct Loaded {
    sheet: Sheet,
    styles: Styles,
    policy_reveal: bool,
}

/// The grid of the sheet at `index` among `metas` out of the reading view.
fn take_grid(sections: Vec<Section>, metas: &[Meta], index: usize) -> Option<Grid> {
    let mut sheets: Vec<Section> =
        sections.into_iter().filter(|s| s.kind == SectionKind::Sheet).collect();
    let at = if sheets.len() == metas.len() {
        index
    } else {
        // Some sheet was left out of the reading view: find this one by its name.
        let name = clean_name(&metas.get(index)?.name);
        sheets.iter().position(|s| clean_name(&s.name) == name)?
    };
    let sec = sheets.get_mut(at)?;
    std::mem::take(&mut sec.blocks).into_iter().find_map(|b| match b {
        Block::Grid(g) => Some(g),
        _ => None,
    })
}

fn load(data: &[u8], sheet: usize, policy: &PreviewPolicy) -> Result<Loaded, PreviewError> {
    check_size(data, policy)?;
    let doc = detect::open(data, policy)?;
    let reveal = policy.reveal_hidden;
    // The reading view always includes hidden content here: which sheets and rows are shown is
    // decided by the page, which also counts what it leaves out.
    let mut all = policy.clone();
    all.reveal_hidden = true;
    let (read, preview) = match doc {
        Doc::Opc { mut pkg, format } if is_xlsx(format) => {
            let read = xlsx::read(&mut pkg, Some(sheet), reveal)?;
            let mut sink = Sink::new(&all);
            crate::xlsx::extract(&mut pkg, &mut sink)?;
            (read, sink.finish(format))
        }
        Doc::Odf { mut pkg, format: format @ FileFormat::Ods } => {
            let read = ods::read(&mut pkg, Some(sheet), reveal)?;
            let mut sink = Sink::new(&all);
            crate::odf::extract(&mut pkg, &mut sink, format)?;
            (read, sink.finish(format))
        }
        Doc::Cfb { cf, format: format @ FileFormat::Xls } => {
            let read = xls::read(&cf, Some(sheet), reveal)?;
            let mut sink = Sink::new(&all);
            crate::xls::extract(&cf, &mut sink)?;
            (read, sink.finish(format))
        }
        other => return Err(PreviewError::Unsupported(doc_format(&other))),
    };
    let Read { metas, picked } = read;
    let Some(Picked { index, styles, layout }) = picked else {
        return Err(PreviewError::Malformed("the document has no such sheet".into()));
    };
    let grid = take_grid(preview.sections, &metas, index);
    let sheet = assemble(layout, &styles, grid, reveal);
    Ok(Loaded { sheet, styles, policy_reveal: reveal })
}

/// How many pages sheet number `sheet` (from 0, as listed by [`sheet_list`]) prints on.
///
/// # Errors
/// See [`PreviewError`]; [`PreviewError::Malformed`] for a sheet that does not exist.
pub fn page_count(data: &[u8], sheet: usize, policy: &PreviewPolicy) -> Result<u32, PreviewError> {
    let loaded = load(data, sheet, policy)?;
    let pages = paginate(&loaded.sheet);
    Ok(u32::try_from(pages.count).unwrap_or(u32::MAX))
}

/// Most sheets that are counted to number the pages of one workbook.
const MAX_FLAT_SHEETS: usize = 256;

/// Draws page `index` (from 0) of a whole workbook: the pages of its sheets one after another, in
/// the order of [`sheet_list`]. `pages` of the result is the number of pages of the workbook.
///
/// # Errors
/// See [`PreviewError`]; [`PreviewError::Unsupported`] for other formats,
/// [`PreviewError::Malformed`] for a page that does not exist.
pub fn render_workbook_page(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<PageModel, PreviewError> {
    let list = sheet_list(data, policy)?;
    let mut counts: Vec<usize> = Vec::new();
    let mut total = 0usize;
    for sheet in 0..list.sheets.len().min(MAX_FLAT_SHEETS) {
        let n = page_count(data, sheet, policy).map_or(0, |n| n as usize);
        counts.push(n);
        total = total.saturating_add(n);
    }
    let mut rest = index;
    for (sheet, n) in counts.iter().enumerate() {
        if rest < *n {
            let mut page = render_sheet_page(data, sheet, rest, policy)?.page;
            page.index = u32::try_from(index).unwrap_or(u32::MAX);
            page.pages = u32::try_from(total).unwrap_or(u32::MAX);
            return Ok(page);
        }
        rest -= *n;
    }
    Err(PreviewError::Malformed("the workbook has no such page".into()))
}

fn notices_of(sheet: &Sheet, pages: &Pages, reveal: bool, painted_truncated: bool) -> Vec<Notice> {
    let mut out: Vec<Notice> = Vec::new();
    for what in &sheet.notes.items {
        out.push(Notice { kind: NoticeKind::Unsupported, count: 0, detail: (*what).to_owned() });
    }
    let hidden_kind = if reveal { NoticeKind::HiddenShown } else { NoticeKind::HiddenOmitted };
    for (n, what) in [(sheet.hidden_rows, "rows"), (sheet.hidden_cols, "columns")] {
        if n > 0 {
            out.push(Notice { kind: hidden_kind, count: n, detail: what.to_owned() });
        }
    }
    if pages.truncated {
        out.push(Notice {
            kind: NoticeKind::Truncated,
            count: 0,
            detail: "sheet pages".to_owned(),
        });
    }
    if sheet.truncated || painted_truncated {
        out.push(Notice {
            kind: NoticeKind::Truncated,
            count: 0,
            detail: "sheet content".to_owned(),
        });
    }
    if sheet.approximate {
        out.push(Notice {
            kind: NoticeKind::BestEffort,
            count: 0,
            detail: "sheet layout".to_owned(),
        });
    }
    out
}

/// Draws page `page` (from 0) of sheet number `sheet` (from 0, as listed by [`sheet_list`]).
///
/// # Errors
/// See [`PreviewError`]; [`PreviewError::Malformed`] for a sheet or page that does not exist.
pub fn render_sheet_page(
    data: &[u8],
    sheet: usize,
    page: usize,
    policy: &PreviewPolicy,
) -> Result<SheetPage, PreviewError> {
    let loaded = load(data, sheet, policy)?;
    let pages = paginate(&loaded.sheet);
    if page >= pages.count {
        return Err(PreviewError::Malformed("the sheet has no such page".into()));
    }
    let painted = paint::paint(&loaded.sheet, &loaded.styles, &pages, page);
    let notices = notices_of(&loaded.sheet, &pages, loaded.policy_reveal, painted.truncated);
    let model = PageModel {
        width: pages.width,
        height: pages.height,
        index: u32::try_from(page).unwrap_or(u32::MAX),
        pages: u32::try_from(pages.count).unwrap_or(u32::MAX),
        prims: painted.prims,
        truncated: painted.truncated,
        approximate: painted.approximate,
    };
    Ok(SheetPage { page: model, notices })
}
