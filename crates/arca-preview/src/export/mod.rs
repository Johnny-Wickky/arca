//! Sterile copies of the documents this crate can read but the X-ray cannot rebuild: legacy
//! Word, Excel and PowerPoint files and RTF.
//!
//! Editing a binary file in place is brittle, so the copy is **written new** from what the
//! preview read: the visible text and its basic formatting, tables, spreadsheet cells and slide
//! text go into a fresh OOXML package (`.docx`, `.xlsx`, `.pptx`). Everything the model does not
//! hold cannot come along: macros, OLE objects, ActiveX, embedded files, links, scripts,
//! metadata, hidden text and sheets. What the model drops that a reader may have wanted
//! (pictures, charts, exact layout) is listed in the report of changes.
//!
//! The copy is as faithful as the preview is, and no more: it is a *content* copy, not a
//! look-alike. The caller verifies the result with the X-ray, as for every sterile copy.

mod docx;
mod pptx;
mod xlsx;

use arca_container::zip::writer::{Compression, ZipWriter};
use arca_xray::sanitize::{ActionKind, SanitizeAction};

use crate::model::{FileFormat, NoticeKind, Paragraph, SectionKind, style};
use crate::{PreviewError, PreviewPolicy, preview};

/// What the caller may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyOptions {
    /// Accept tracked changes: deletions disappear, insertions become normal text. Otherwise
    /// they stay, struck through and underlined.
    pub accept_revisions: bool,
    /// Leave out reviewer comments.
    pub remove_comments: bool,
}

impl Default for LegacyOptions {
    fn default() -> Self {
        Self { accept_revisions: true, remove_comments: true }
    }
}

fn action(kind: ActionKind, target: impl Into<String>) -> SanitizeAction {
    SanitizeAction { kind, target: arca_container::text::display_safe(&target.into(), 300) }
}

/// Escapes text for XML.
pub(crate) fn esc(s: &str) -> String {
    arca_container::xml::escape(s)
}

/// A ZIP package under construction.
pub(crate) struct Package {
    zip: ZipWriter,
}

impl Package {
    pub(crate) fn new() -> Self {
        Self { zip: ZipWriter::new() }
    }

    pub(crate) fn add(&mut self, name: &str, text: &str) -> Result<(), PreviewError> {
        self.zip
            .add(name, text.as_bytes(), Compression::Deflated)
            .map_err(|e| PreviewError::Malformed(format!("could not write {name}: {e}")))
    }

    pub(crate) fn finish(self) -> Result<Vec<u8>, PreviewError> {
        self.zip
            .finish()
            .map_err(|e| PreviewError::Malformed(format!("could not write the package: {e}")))
    }
}

/// The address a link shows after its text in a copy: links are not carried over, so the reader
/// is told where each one pointed. Internal jumps (`#bookmark`) are not addresses.
pub(crate) fn visible_address(text: &str, href: &str) -> Option<String> {
    if href.is_empty() || href.starts_with('#') || text.trim() == href {
        return None;
    }
    Some(format!(" ({href})"))
}

/// Appends `text` to the pieces, merging it into the last one if the style is the same.
fn push_piece(out: &mut Vec<(String, u16)>, text: &str, st: u16) {
    match out.last_mut() {
        Some((t, s)) if *s == st => t.push_str(text),
        _ => out.push((text.to_owned(), st)),
    }
}

/// Writes the address of a linked stretch that has just ended, if it is worth showing.
fn close_link(open: &mut Option<(&str, String)>, out: &mut Vec<(String, u16)>) {
    if let Some((href, words)) = open.take()
        && let Some(addr) = visible_address(&words, href)
    {
        push_piece(out, &addr, 0);
    }
}

/// The runs of a paragraph as they are to be written: tracked changes applied or marked, links
/// turned into text (the address follows the linked words in parentheses). Returns `(text,
/// style bits)` pieces and the number of links.
pub(crate) fn runs_of(p: &Paragraph, opts: &LegacyOptions) -> (Vec<(String, u16)>, usize) {
    let mut out: Vec<(String, u16)> = Vec::new();
    let mut links = 0;
    // The linked stretch being written: its address and the words shown for it so far.
    let mut open: Option<(&str, String)> = None;
    for r in &p.runs {
        let same = open.as_ref().is_some_and(|(h, _)| *h == r.href.as_str());
        if !same {
            close_link(&mut open, &mut out);
        }
        let mut st = r.style & !(style::HIDDEN | style::DELETED | style::INSERTED);
        if r.style & style::DELETED != 0 {
            if opts.accept_revisions {
                continue;
            }
            st |= style::STRIKE;
        }
        if r.style & style::INSERTED != 0 && !opts.accept_revisions {
            st |= style::UNDERLINE;
        }
        if !r.href.is_empty() {
            if !same {
                links += 1;
                open = Some((r.href.as_str(), String::new()));
            }
            if let Some((_, words)) = open.as_mut() {
                words.push_str(&r.text);
            }
        }
        push_piece(&mut out, &r.text, st);
    }
    close_link(&mut open, &mut out);
    (out, links)
}

/// Counts of what the copy leaves behind, for the report.
#[derive(Default)]
pub(crate) struct Tally {
    pub(crate) links: usize,
    pub(crate) pictures: usize,
}

/// Writes a sterile copy of a legacy document.
///
/// Returns the new file, its format and the changes made.
///
/// # Errors
/// [`PreviewError::Unsupported`] for any format other than `.doc`, `.xls`, `.ppt` and RTF;
/// [`PreviewError::Encrypted`] / [`PreviewError::Malformed`] as the preview reports them; and
/// [`PreviewError::Malformed`] when the document is too large to copy completely.
pub fn sanitize_legacy(
    data: &[u8],
    options: &LegacyOptions,
) -> Result<(Vec<u8>, FileFormat, Vec<SanitizeAction>), PreviewError> {
    let policy = PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() };
    let p = preview(data, &policy)?;
    let target = match p.format {
        FileFormat::Doc | FileFormat::Rtf => FileFormat::Docx,
        FileFormat::Xls => FileFormat::Xlsx,
        FileFormat::Ppt => FileFormat::Pptx,
        other => return Err(PreviewError::Unsupported(other)),
    };
    if p.stats.truncated {
        // A copy that silently lacks the end of the document is worse than none.
        return Err(PreviewError::Malformed("the document is too large to copy completely".into()));
    }
    let mut tally = Tally::default();
    let bytes = match target {
        FileFormat::Docx => docx::write(&p, options, &mut tally)?,
        FileFormat::Xlsx => xlsx::write(&p, options, &mut tally)?,
        _ => pptx::write(&p, options, &mut tally)?,
    };

    let mut actions = vec![action(
        ActionKind::ConvertedContentType,
        format!("{:?} → {target:?} (rebuilt from the document's content)", p.format),
    )];
    actions.push(action(
        ActionKind::RemovedElement,
        "macros, embedded objects, scripts and everything that is not visible content were not carried over",
    ));
    if tally.links > 0 {
        actions.push(action(
            ActionKind::RemovedExternalReference,
            format!("{} hyperlink(s) turned into plain text", tally.links),
        ));
    }
    for n in &p.notices {
        match n.kind {
            NoticeKind::ImagesNotShown => actions.push(action(
                ActionKind::RemovedElement,
                format!("{} picture(s) and object(s) not carried over", n.count),
            )),
            NoticeKind::HiddenOmitted => actions.push(action(
                ActionKind::RemovedElement,
                format!("hidden content left out ({} {})", n.count, n.detail),
            )),
            NoticeKind::TrackedChanges if options.accept_revisions => {
                actions.push(action(ActionKind::AcceptedRevisions, "tracked changes accepted"));
            }
            _ => {}
        }
    }
    if options.remove_comments && p.sections.iter().any(|s| s.kind == SectionKind::Comments) {
        actions.push(action(ActionKind::RemovedComments, "comments"));
    }
    actions.push(action(
        ActionKind::StrippedMetadata,
        "author, company, dates and all other properties",
    ));
    Ok((bytes, target, actions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Block;
    use crate::testkit::{Lcg, fuzz_iterations};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/").to_owned() + name,
        )
        .expect("a fixture")
    }

    /// Every piece of text a preview holds, one per line.
    fn texts(p: &crate::Preview) -> String {
        let mut out = String::new();
        for s in &p.sections {
            out.push_str(&s.name);
            out.push('\n');
            for b in &s.blocks {
                match b {
                    Block::Para(x) => out.push_str(&format!("{}\n", x.text())),
                    Block::Table(t) => {
                        for r in &t.rows {
                            for c in &r.cells {
                                for x in &c.paras {
                                    out.push_str(&format!("{}\n", x.text()));
                                }
                            }
                        }
                    }
                    Block::Grid(g) => {
                        for r in &g.rows {
                            for c in &r.cells {
                                out.push_str(&format!("{}\n", c.text));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn words(s: &str) -> std::collections::BTreeSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_lowercase)
            .collect()
    }

    #[test]
    fn legacy_files_come_out_as_ooxml_with_their_text() {
        for (name, expect) in [
            ("report.doc", FileFormat::Docx),
            ("lo-rich.doc", FileFormat::Docx),
            ("report.rtf", FileFormat::Docx),
            ("sheet.xls", FileFormat::Xlsx),
            ("slides.ppt", FileFormat::Pptx),
            ("deck-rich.ppt", FileFormat::Pptx),
        ] {
            let data = fixture(name);
            // Everything kept, so that what comes out can be held against what went in.
            let keep_all = LegacyOptions { accept_revisions: false, remove_comments: false };
            let (out, format, actions) = sanitize_legacy(&data, &keep_all).expect(name);
            assert_eq!(format, expect, "{name}");
            assert!(!actions.is_empty());
            // The copy is a document of that format that the viewer reads again...
            let before =
                preview(&data, &PreviewPolicy { reveal_hidden: false, ..PreviewPolicy::default() })
                    .expect(name);
            let after = preview(&out, &PreviewPolicy::default()).expect(name);
            assert_eq!(after.format, expect, "{name}");
            // ...and no word of the text was lost on the way.
            let (a, b) = (words(&texts(&before)), words(&texts(&after)));
            let lost: Vec<&String> = a.difference(&b).collect();
            assert!(lost.is_empty(), "{name}: lost {lost:?}");
            // The default copy is clean as well.
            let (out, _, _) = sanitize_legacy(&data, &LegacyOptions::default()).expect(name);
            // ...and the X-ray finds nothing in it.
            let report = arca_xray::scan_default(&out);
            assert_eq!(report.verdict, arca_xray::Verdict::Clean, "{name}: {:#?}", report.findings);
        }
    }

    #[test]
    fn options_decide_what_happens_to_revisions_and_comments() {
        let data = fixture("lo-rich.doc");
        let (a, _, acts) = sanitize_legacy(&data, &LegacyOptions::default()).expect("a copy");
        let (b, _, _) = sanitize_legacy(
            &data,
            &LegacyOptions { accept_revisions: false, remove_comments: false },
        )
        .expect("a copy");
        let pa = texts(&preview(&a, &PreviewPolicy::default()).expect("preview"));
        let pb = texts(&preview(&b, &PreviewPolicy::default()).expect("preview"));
        // Kept on request, gone by default.
        assert!(pb.len() > pa.len(), "more is kept when asked: {} vs {}", pa.len(), pb.len());
        assert!(pb.contains("Comments") && !pa.contains("Comments"));
        let all: String = acts.iter().map(|a| a.target.as_str()).collect::<Vec<_>>().join("\n");
        assert!(all.contains("comments") && all.contains("tracked changes"), "{all}");
    }

    #[test]
    fn an_rtf_with_an_exploit_object_comes_out_clean() {
        let rtf = br"{\rtf1\ansi Before {\object\objemb{\*\objclass Equation.3}{\*\objdata 0105000002000000}}After}";
        let (out, format, _) = sanitize_legacy(rtf, &LegacyOptions::default()).expect("a copy");
        assert_eq!(format, FileFormat::Docx);
        let t = texts(&preview(&out, &PreviewPolicy::default()).expect("preview"));
        assert!(t.contains("Before") && t.contains("After"), "{t}");
        assert!(!out.windows(7).any(|w| w == b"objdata"));
        assert_eq!(arca_xray::scan_default(&out).verdict, arca_xray::Verdict::Clean);
    }

    #[test]
    fn other_formats_and_broken_files_are_refused() {
        assert!(matches!(
            sanitize_legacy(&fixture("report.docx"), &LegacyOptions::default()),
            Err(PreviewError::Unsupported(FileFormat::Docx))
        ));
        assert!(matches!(
            sanitize_legacy(b"plain text", &LegacyOptions::default()),
            Err(PreviewError::Unsupported(_))
        ));
        assert!(sanitize_legacy(b"", &LegacyOptions::default()).is_err());
    }

    #[test]
    fn helpers_name_and_number_things_the_way_excel_wants() {
        assert_eq!(xlsx::column_name_for_test(0), "A");
        assert_eq!(xlsx::column_name_for_test(25), "Z");
        assert_eq!(xlsx::column_name_for_test(26), "AA");
        assert_eq!(xlsx::column_name_for_test(16_383), "XFD");
        for (t, number) in [
            ("12", true),
            ("-3.5", true),
            ("0", true),
            ("0.25", true),
            ("007", false),
            ("1,234", false),
            ("", false),
            ("1e5", false),
            ("-0", false),
            ("1234567890123456", false),
            ("12.", false),
            (".5", false),
        ] {
            assert_eq!(xlsx::plain_number_for_test(t), number, "{t:?}");
        }
        let mut used = Vec::new();
        assert_eq!(xlsx::sheet_name_for_test("Data: 2024/[Q1]", &mut used), "Data  2024  Q1");
        assert_eq!(xlsx::sheet_name_for_test("data  2024  q1", &mut used), "data  2024  q1 (2)");
        assert_eq!(xlsx::sheet_name_for_test("", &mut used), "Sheet3");
        assert_eq!(xlsx::sheet_name_for_test(&"x".repeat(50), &mut used).chars().count(), 31);
    }

    #[test]
    fn sheet_names_are_unique_in_every_script() {
        // Excel treats these as the same name; a workbook that has both is "repaired".
        let mut used = Vec::new();
        assert_eq!(xlsx::sheet_name_for_test("Лист", &mut used), "Лист");
        assert_eq!(xlsx::sheet_name_for_test("ЛИСТ", &mut used), "ЛИСТ (2)");
        assert_eq!(xlsx::sheet_name_for_test("лист", &mut used), "лист (3)");
        assert_eq!(xlsx::sheet_name_for_test("Ünï", &mut used), "Ünï");
        assert_eq!(xlsx::sheet_name_for_test("üNÏ", &mut used), "üNÏ (2)");
    }

    #[test]
    fn a_linked_spreadsheet_cell_keeps_its_address_in_the_copy() {
        use crate::model::{
            Block, CellKind, Grid, GridCell, GridRow, Preview, PreviewStats, Section,
        };
        let cell = |col, text: &str, href: &str| GridCell {
            col,
            kind: CellKind::Text,
            text: text.into(),
            href: href.into(),
        };
        let p = Preview {
            format: FileFormat::Xls,
            sections: vec![Section {
            page: Default::default(),
                kind: SectionKind::Sheet,
                name: "S".into(),
                hidden: false,
                blocks: vec![Block::Grid(Grid {
                    cols: 2,
                    rows: vec![GridRow {
                        index: 1,
                        hidden: false,
                        cells: vec![
                            cell(0, "click", "https://evil.example/x"),
                            cell(1, "plain", ""),
                        ],
                    }],
                    truncated: false,
                })],
            }],
            notices: Vec::new(),
            stats: PreviewStats::default(),
        };
        let mut tally = Tally::default();
        let bytes = xlsx::write(&p, &LegacyOptions::default(), &mut tally).expect("a workbook");
        assert_eq!(tally.links, 1);
        let back = preview(&bytes, &PreviewPolicy::default()).expect("preview");
        let t = texts(&back);
        assert!(t.contains("click (https://evil.example/x)"), "{t}");
        assert!(t.contains("plain\n") || t.ends_with("plain\n"), "{t}");
    }

    #[test]
    fn mutated_legacy_files_never_panic() {
        let mut rng = Lcg::new(31);
        for name in ["report.doc", "sheet.xls", "slides.ppt", "report.rtf"] {
            let base = fixture(name);
            for _ in 0..fuzz_iterations(40) {
                let mut m = base.clone();
                rng.mutate(&mut m, 6);
                let _ = sanitize_legacy(&m, &LegacyOptions::default());
            }
        }
    }
}
