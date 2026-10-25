//! The preview data model.
//!
//! A [`Preview`] is *inert data*: plain text with a handful of style bits,
//! tables and cell grids. There is no markup, no script, no URL that is ever
//! fetched and no image that is ever decoded — images appear as placeholders
//! ([`ImageInfo`]). The UI renders it with its own fonts and colours, which
//! also defeats the classic concealment tricks (white-on-white text, 1-pt
//! fonts, text off the page): everything the document *contains* is shown.
//!
//! The model crosses the sandbox boundary in the `arca-abi` wire format, so
//! every type implements [`Wire`](arca_abi::wire::Wire).

use arca_abi::wire::{Reader, Wire, WireError, Writer};
use arca_abi::{wire_enum, wire_struct};
pub use arca_xray::FileFormat;

/// Run style bits ([`Run::style`]).
pub mod style {
    /// Bold.
    pub const BOLD: u16 = 1;
    /// Italic.
    pub const ITALIC: u16 = 1 << 1;
    /// Underlined.
    pub const UNDERLINE: u16 = 1 << 2;
    /// Struck through.
    pub const STRIKE: u16 = 1 << 3;
    /// Monospaced.
    pub const MONO: u16 = 1 << 4;
    /// Superscript.
    pub const SUPER: u16 = 1 << 5;
    /// Subscript.
    pub const SUB: u16 = 1 << 6;
    /// The document marks this text as hidden (it is shown anyway, dimmed).
    pub const HIDDEN: u16 = 1 << 7;
    /// Tracked deletion (text the author removed but the file still contains).
    pub const DELETED: u16 = 1 << 8;
    /// Tracked insertion.
    pub const INSERTED: u16 = 1 << 9;
    /// The text was made by the viewer from the document's data (a reconstructed SQL query, for
    /// example); it is not text the document contains and must be drawn so that it cannot be
    /// mistaken for it.
    pub const SYNTHETIC: u16 = 1 << 10;
    /// All defined bits.
    pub const ALL: u16 = (1 << 11) - 1;
}

/// A span of text with uniform formatting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    /// The text (already sanitised: no control or invisible characters).
    pub text: String,
    /// Style bits, see [`style`].
    pub style: u16,
    /// Link target, empty for none. **Display only** — never followed.
    pub href: String,
}
wire_struct!(Run { text, style, href });

/// Role of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParaStyle {
    /// Ordinary text.
    #[default]
    Body,
    /// Document or slide title.
    Title,
    /// Subtitle.
    Subtitle,
    /// Heading level 1.
    Heading1,
    /// Heading level 2.
    Heading2,
    /// Heading level 3.
    Heading3,
    /// Heading level 4.
    Heading4,
    /// Heading level 5.
    Heading5,
    /// Heading level 6.
    Heading6,
    /// Quotation.
    Quote,
    /// Preformatted text.
    Code,
    /// Speaker notes.
    Note,
    /// A reviewer comment.
    Comment,
    /// A footnote or endnote.
    Footnote,
    /// Header or footer text of a page.
    HeaderFooter,
}
wire_enum!(ParaStyle {
    Body = 0, Title = 1, Subtitle = 2, Heading1 = 3, Heading2 = 4, Heading3 = 5, Heading4 = 6,
    Heading5 = 7, Heading6 = 8, Quote = 9, Code = 10, Note = 11, Comment = 12, Footnote = 13,
    HeaderFooter = 14,
});

impl ParaStyle {
    /// Heading style for an outline level (`1` = highest); levels above 6 map to 6.
    #[must_use]
    pub const fn heading(level: u8) -> Self {
        match level {
            0 | 1 => Self::Heading1,
            2 => Self::Heading2,
            3 => Self::Heading3,
            4 => Self::Heading4,
            5 => Self::Heading5,
            _ => Self::Heading6,
        }
    }
}

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Start edge (left in LTR text).
    #[default]
    Start,
    /// Centred.
    Center,
    /// End edge.
    End,
    /// Justified.
    Justify,
}
wire_enum!(Align { Start = 0, Center = 1, End = 2, Justify = 3 });

/// A paragraph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Paragraph {
    /// Role.
    pub style: ParaStyle,
    /// Alignment.
    pub align: Align,
    /// List nesting level, `0` outside lists.
    pub level: u8,
    /// List marker such as `•` or `3.`; empty outside lists.
    pub marker: String,
    /// The text.
    pub runs: Vec<Run>,
}
wire_struct!(Paragraph { style, align, level, marker, runs });

impl Paragraph {
    /// A plain paragraph with a single unstyled run.
    #[must_use]
    pub fn plain(text: &str) -> Self {
        Self { runs: vec![Run { text: text.to_owned(), ..Run::default() }], ..Self::default() }
    }

    /// The concatenated text of all runs.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// `true` if the paragraph holds no visible text.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.runs.iter().all(|r| r.text.trim().is_empty())
    }
}

/// One cell of a [`Table`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// Number of columns spanned (≥ 1).
    pub span: u16,
    /// Header cell.
    pub header: bool,
    /// Content.
    pub paras: Vec<Paragraph>,
}
wire_struct!(Cell { span, header, paras });

/// A table row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// Cells in order.
    pub cells: Vec<Cell>,
}
wire_struct!(Row { cells });

/// A table inside a document or slide.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// Rows in order.
    pub rows: Vec<Row>,
}
wire_struct!(Table { rows });

/// What a spreadsheet cell holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellKind {
    /// Text.
    #[default]
    Text,
    /// A number (right aligned).
    Number,
    /// A date or time.
    Date,
    /// TRUE / FALSE.
    Bool,
    /// An error value such as `#DIV/0!`.
    Error,
    /// A formula whose cached result is shown, or whose text is when it has none and could not be
    /// calculated.
    Formula,
    /// The result of a formula that had no stored result, calculated by the viewer.
    Calculated,
}
wire_enum!(CellKind {
    Text = 0, Number = 1, Date = 2, Bool = 3, Error = 4, Formula = 5, Calculated = 6,
});

/// One non-empty cell of a [`Grid`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridCell {
    /// Zero-based column.
    pub col: u32,
    /// Kind of content.
    pub kind: CellKind,
    /// Display text (formatted as the document formats it).
    pub text: String,
    /// Where the cell links to; shown to the reader, never followed. Empty for no link.
    pub href: String,
}
wire_struct!(GridCell { col, kind, text, href });

/// One non-empty row of a [`Grid`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridRow {
    /// One-based row number as the spreadsheet shows it.
    pub index: u32,
    /// Hidden in the source document (shown anyway).
    pub hidden: bool,
    /// Non-empty cells in column order.
    pub cells: Vec<GridCell>,
}
wire_struct!(GridRow { index, hidden, cells });

/// A sparse spreadsheet grid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Grid {
    /// Number of columns that contain data (highest used column + 1).
    pub cols: u32,
    /// Non-empty rows in order.
    pub rows: Vec<GridRow>,
    /// Content was cut because a limit was reached.
    pub truncated: bool,
}
wire_struct!(Grid { cols, rows, truncated });

/// The id of a picture that cannot be opened (a remote link, a format that is not read).
pub const IMAGE_NONE: u32 = u32::MAX;

/// A picture in the document. The preview holds only this description; the pixels are decoded
/// in a separate sandbox run, and only when the reader asks for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// Alternative text or the picture's name.
    pub alt: String,
    /// Media type or extension of the embedded file (`png`, `emf`, ...), if known.
    pub media: String,
    /// Size of the embedded file in bytes, `0` if not embedded.
    pub bytes: u64,
    /// The picture is a link to a remote resource (and was not fetched).
    pub external: bool,
    /// What to ask the sandbox for to see the picture ([`IMAGE_NONE`] if it cannot be shown).
    /// The meaning belongs to the document format and is only ever interpreted by the guest.
    pub id: u32,
}
wire_struct!(ImageInfo { alt, media, bytes, external, id });

impl Default for ImageInfo {
    fn default() -> Self {
        Self { alt: String::new(), media: String::new(), bytes: 0, external: false, id: IMAGE_NONE }
    }
}

/// A block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A paragraph.
    Para(Paragraph),
    /// A table.
    Table(Table),
    /// A spreadsheet grid.
    Grid(Grid),
    /// A picture placeholder.
    Image(ImageInfo),
    /// A page / section break.
    Rule,
}

impl Wire for Block {
    fn encode(&self, w: &mut Writer) {
        match self {
            Self::Para(p) => {
                w.u8(0);
                p.encode(w);
            }
            Self::Table(t) => {
                w.u8(1);
                t.encode(w);
            }
            Self::Grid(g) => {
                w.u8(2);
                g.encode(w);
            }
            Self::Image(i) => {
                w.u8(3);
                i.encode(w);
            }
            Self::Rule => w.u8(4),
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        match r.u8()? {
            0 => Ok(Self::Para(Paragraph::decode(r)?)),
            1 => Ok(Self::Table(Table::decode(r)?)),
            2 => Ok(Self::Grid(Grid::decode(r)?)),
            3 => Ok(Self::Image(ImageInfo::decode(r)?)),
            4 => Ok(Self::Rule),
            t => Err(WireError::InvalidTag(u16::from(t))),
        }
    }
}

/// What a [`Section`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SectionKind {
    /// The body of a text document.
    #[default]
    Body,
    /// A worksheet.
    Sheet,
    /// A slide.
    Slide,
    /// A page of a fixed-layout document.
    Page,
    /// Reviewer comments.
    Comments,
    /// Footnotes and endnotes.
    Notes,
    /// Headers and footers.
    Headers,
}
wire_enum!(SectionKind {
    Body = 0, Sheet = 1, Slide = 2, Page = 3, Comments = 4, Notes = 5, Headers = 6,
});

/// A sheet, slide, page or the body of a document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    /// What it is.
    pub kind: SectionKind,
    /// Display name (sheet name, slide title, ...); may be empty.
    pub name: String,
    /// Hidden in the source document (shown anyway; a classic concealment).
    pub hidden: bool,
    /// Content.
    pub blocks: Vec<Block>,
}
wire_struct!(Section { kind, name, hidden, blocks });

/// Why a [`Notice`] was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// A limit was reached and content was cut; `detail` names the limit.
    Truncated,
    /// A feature is not rendered; `detail` names it.
    Unsupported,
    /// A part of the document could not be read; `detail` names it.
    Skipped,
    /// Invisible or control characters were removed; `count` says how many and
    /// `detail` breaks them down (`bidi=1,tags=0,zero-width=2,control=0`).
    InvisibleRemoved,
    /// Pictures are not displayed; `count` says how many.
    ImagesNotShown,
    /// Content flagged hidden by the document is shown; `count` says how much,
    /// `detail` says what (`text`, `sheets`, `slides`, `rows`, ...).
    HiddenShown,
    /// The document tracks changes; `count` of insertions and deletions shown.
    TrackedChanges,
    /// Formulas are shown with their cached results and were not evaluated.
    FormulasNotEvaluated,
    /// Text is a best-effort extraction (legacy binary format).
    BestEffort,
    /// Content flagged hidden by the document was left out because the request did not ask
    /// for hidden content; `count` says how much, `detail` says what.
    HiddenOmitted,
    /// Formulas that had no stored result were calculated by the viewer; `count` of them.
    FormulasCalculated,
    /// Records the document marks as deleted are still stored in the file and were not shown;
    /// `count` of them.
    DeletedRecords,
}
wire_enum!(NoticeKind {
    Truncated = 0, Unsupported = 1, Skipped = 2, InvisibleRemoved = 3, ImagesNotShown = 4,
    HiddenShown = 5, TrackedChanges = 6, FormulasNotEvaluated = 7, BestEffort = 8,
    HiddenOmitted = 9, FormulasCalculated = 10, DeletedRecords = 11,
});

/// A remark about what the preview does or does not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Why.
    pub kind: NoticeKind,
    /// How many, if the kind counts things.
    pub count: u64,
    /// Short technical detail (never localised; the UI maps `kind` to text).
    pub detail: String,
}
wire_struct!(Notice { kind, count, detail });

/// Counters for the whole preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PreviewStats {
    /// Characters of text emitted.
    pub chars: u64,
    /// Runs, cells, blocks and rows emitted.
    pub items: u64,
    /// Some content was cut because a limit was reached.
    pub truncated: bool,
}
wire_struct!(PreviewStats { chars, items, truncated });

/// The complete preview of one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Format the content was extracted from.
    pub format: FileFormat,
    /// Sheets, slides, pages or the body, in reading order.
    pub sections: Vec<Section>,
    /// What the preview does not show or had to change.
    pub notices: Vec<Notice>,
    /// Totals.
    pub stats: PreviewStats,
}
wire_struct!(Preview { format, sections, notices, stats });

#[cfg(test)]
mod tests {
    use super::*;
    use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};

    pub(crate) fn sample() -> Preview {
        let para = Paragraph {
            style: ParaStyle::Heading2,
            align: Align::Center,
            level: 2,
            marker: "1.".into(),
            runs: vec![
                Run { text: "Привет ".into(), style: style::BOLD, href: String::new() },
                Run {
                    text: "мир".into(), style: style::ITALIC | style::HIDDEN, href: "u".into()
                },
            ],
        };
        Preview {
            format: FileFormat::Docx,
            sections: vec![Section {
                kind: SectionKind::Sheet,
                name: "Лист1".into(),
                hidden: true,
                blocks: vec![
                    Block::Para(para.clone()),
                    Block::Table(Table {
                        rows: vec![Row {
                            cells: vec![Cell { span: 2, header: true, paras: vec![para] }],
                        }],
                    }),
                    Block::Grid(Grid {
                        cols: 3,
                        rows: vec![GridRow {
                            index: 7,
                            hidden: false,
                            cells: vec![GridCell {
                                col: 2,
                                kind: CellKind::Number,
                                text: "1,5".into(),
                                href: "https://example.org/".into(),
                            }],
                        }],
                        truncated: true,
                    }),
                    Block::Image(ImageInfo {
                        alt: "logo".into(),
                        media: "png".into(),
                        bytes: 12,
                        external: true,
                        id: 7,
                    }),
                    Block::Rule,
                ],
            }],
            notices: vec![Notice {
                kind: NoticeKind::InvisibleRemoved,
                count: 3,
                detail: "bidi=3".into(),
            }],
            stats: PreviewStats { chars: 10, items: 5, truncated: true },
        }
    }

    fn limits() -> DecodeLimits {
        DecodeLimits { max_string_bytes: 1 << 20, max_items: 1 << 20, max_depth: 32 }
    }

    #[test]
    fn wire_roundtrip() {
        let p = sample();
        let bytes = to_bytes(&p).unwrap();
        let back: Preview = from_bytes(&bytes, limits()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn every_truncation_and_bit_flip_fails_or_decodes_cleanly() {
        let bytes = to_bytes(&sample()).unwrap();
        for cut in 0..bytes.len() {
            assert!(from_bytes::<Preview>(bytes.get(..cut).unwrap_or_default(), limits()).is_err());
        }
        for i in 0..bytes.len() {
            let mut m = bytes.clone();
            if let Some(b) = m.get_mut(i) {
                *b ^= 0xFF;
            }
            let _ = from_bytes::<Preview>(&m, limits());
        }
    }

    #[test]
    fn heading_levels() {
        assert_eq!(ParaStyle::heading(1), ParaStyle::Heading1);
        assert_eq!(ParaStyle::heading(0), ParaStyle::Heading1);
        assert_eq!(ParaStyle::heading(6), ParaStyle::Heading6);
        assert_eq!(ParaStyle::heading(200), ParaStyle::Heading6);
    }

    #[test]
    fn paragraph_helpers() {
        let p = Paragraph::plain("  ");
        assert!(p.is_blank());
        assert_eq!(Paragraph::plain("a").text(), "a");
    }
}
