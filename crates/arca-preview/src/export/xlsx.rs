//! A new `.xlsx` from the sheets of a legacy Excel workbook.
//!
//! Values are written as the preview shows them: text as text, plain numbers as numbers (so the
//! sheet can still be summed), everything else (dates, percentages, formatted amounts, errors,
//! the text of formulas) as text. Formulas are not carried over: the sheet holds values only.

use std::collections::BTreeMap;

use super::{LegacyOptions, Package, Tally, esc};
use crate::PreviewError;
use crate::model::{Block, CellKind, Paragraph, Preview, SectionKind};

const SS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

const MAX_ROWS: u32 = 1_048_576;
const MAX_COLS: u32 = 16_384;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#;

/// `0 → A`, `25 → Z`, `26 → AA`.
fn column_name(mut n: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(char::from(b'A' + u8::try_from(n % 26).unwrap_or(0)));
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s.iter().rev().collect()
}

/// `true` for text that is a plain decimal number Excel would show unchanged.
fn plain_number(t: &str) -> bool {
    let body = t.strip_prefix('-').unwrap_or(t);
    let (int, frac) = body.split_once('.').map_or((body, None), |(a, b)| (a, Some(b)));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(int)
        && frac.is_none_or(digits)
        && (int == "0" || !int.starts_with('0'))
        && int.len() + frac.map_or(0, str::len) <= 15
        && t != "-0"
}

/// A sheet name Excel accepts: at most 31 characters, none of `[]:*?/\`, not empty, unique.
fn sheet_name(raw: &str, used: &mut Vec<String>, n: usize) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| if matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\') { ' ' } else { c })
        .collect();
    name = name.trim().trim_matches('\'').trim().chars().take(31).collect();
    if name.is_empty() {
        name = format!("Sheet{n}");
    }
    let mut candidate = name.clone();
    let mut k = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
        let suffix = format!(" ({k})");
        candidate = format!(
            "{}{suffix}",
            name.chars().take(31 - suffix.chars().count()).collect::<String>()
        );
        k += 1;
    }
    used.push(candidate.clone());
    candidate
}

type Cells = BTreeMap<u32, BTreeMap<u32, (CellKind, String)>>;

fn sheet_xml(cells: &Cells) -> String {
    let mut out = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet {SS}><sheetData>"#
    );
    for (row, cols) in cells {
        out.push_str(&format!(r#"<row r="{row}">"#));
        for (col, (kind, text)) in cols {
            let r = format!("{}{row}", column_name(*col));
            if *kind == CellKind::Number && plain_number(text) {
                out.push_str(&format!(r#"<c r="{r}"><v>{text}</v></c>"#));
            } else {
                out.push_str(&format!(
                    r#"<c r="{r}" t="inlineStr"><is><t xml:space="preserve">{}</t></is></c>"#,
                    esc(text)
                ));
            }
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData></worksheet>");
    out
}

/// Collects the cells of a section's grids; paragraphs (notes, comments) become one cell per line.
fn collect(blocks: &[Block], tally: &mut Tally) -> Cells {
    let mut cells = Cells::new();
    let mut next_line = 1u32;
    for b in blocks {
        match b {
            Block::Grid(g) => {
                for row in &g.rows {
                    if row.index == 0 || row.index > MAX_ROWS {
                        continue;
                    }
                    for c in &row.cells {
                        if c.col >= MAX_COLS {
                            continue;
                        }
                        if !c.href.is_empty() {
                            tally.links += 1;
                        }
                        cells.entry(row.index).or_default().insert(c.col, (c.kind, c.text.clone()));
                    }
                }
            }
            Block::Para(p) => line(&mut cells, &mut next_line, p),
            Block::Table(t) => {
                for row in &t.rows {
                    let text: Vec<String> = row
                        .cells
                        .iter()
                        .map(|c| c.paras.iter().map(Paragraph::text).collect::<Vec<_>>().join(" "))
                        .collect();
                    line(&mut cells, &mut next_line, &Paragraph::plain(&text.join("\t")));
                }
            }
            Block::Image(_) => tally.pictures += 1,
            Block::Rule => {}
        }
    }
    cells
}

fn line(cells: &mut Cells, next: &mut u32, p: &Paragraph) {
    let text = if p.marker.is_empty() { p.text() } else { format!("{} {}", p.marker, p.text()) };
    if text.trim().is_empty() || *next > MAX_ROWS {
        return;
    }
    cells.entry(*next).or_default().insert(0, (CellKind::Text, text));
    *next += 1;
}

#[cfg(test)]
pub(super) fn column_name_for_test(n: u32) -> String {
    column_name(n)
}

#[cfg(test)]
pub(super) fn plain_number_for_test(t: &str) -> bool {
    plain_number(t)
}

#[cfg(test)]
pub(super) fn sheet_name_for_test(raw: &str, used: &mut Vec<String>) -> String {
    let n = used.len() + 1;
    sheet_name(raw, used, n)
}

/// Writes the package.
pub(super) fn write(
    p: &Preview,
    opts: &LegacyOptions,
    tally: &mut Tally,
) -> Result<Vec<u8>, PreviewError> {
    let mut used = Vec::new();
    let mut sheets: Vec<(String, Cells)> = Vec::new();
    for sec in &p.sections {
        let (label, wanted) = match sec.kind {
            SectionKind::Sheet => (sec.name.as_str(), true),
            SectionKind::Comments => ("Comments", !opts.remove_comments),
            SectionKind::Notes => {
                (if sec.name.is_empty() { "Notes" } else { sec.name.as_str() }, true)
            }
            _ => ("", false),
        };
        if !wanted {
            continue;
        }
        let cells = collect(&sec.blocks, tally);
        // An empty sheet of the workbook is kept (its name may mean something); other empty
        // sections are not worth a sheet.
        if cells.is_empty() && sec.kind != SectionKind::Sheet {
            continue;
        }
        let name = sheet_name(label, &mut used, sheets.len() + 1);
        sheets.push((name, cells));
    }
    if sheets.is_empty() {
        sheets.push((sheet_name("Sheet1", &mut used, 1), Cells::new()));
    }

    let mut content_types = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#,
    );
    let mut workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook {SS}><sheets>"#
    );
    let mut rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (i, (name, _)) in sheets.iter().enumerate() {
        let n = i + 1;
        content_types.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        ));
        workbook.push_str(&format!(r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#, esc(name)));
        rels.push_str(&format!(
            r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#
        ));
    }
    let styles_id = sheets.len() + 1;
    rels.push_str(&format!(
        r#"<Relationship Id="rId{styles_id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#
    ));
    content_types.push_str("</Types>");
    workbook.push_str("</sheets></workbook>");

    let mut pkg = Package::new();
    pkg.add("[Content_Types].xml", &content_types)?;
    pkg.add("_rels/.rels", ROOT_RELS)?;
    pkg.add("xl/workbook.xml", &workbook)?;
    pkg.add("xl/_rels/workbook.xml.rels", &rels)?;
    pkg.add("xl/styles.xml", STYLES)?;
    for (i, (_, cells)) in sheets.iter().enumerate() {
        pkg.add(&format!("xl/worksheets/sheet{}.xml", i + 1), &sheet_xml(cells))?;
    }
    pkg.finish()
}
