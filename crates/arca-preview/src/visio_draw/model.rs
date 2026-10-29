//! The shape model shared by the two Visio readers.
//!
//! `.vsdx` (XML) and `.vsd` (binary) are both read into the same tree of *sheets*: a sheet is a
//! shape, a master shape or a style sheet, and holds named cells, sections of rows (geometry,
//! character and paragraph formats), text and the shape data. The drawing (module `render`) and
//! the reading view work on this model only, so the format-specific code stays in the readers.
//!
//! Cell names are the ShapeSheet names of the Visio SDK reference (`PinX`, `LineWeight`,
//! `FillForegnd`, ...); the binary reader stores the numbers it finds under the same names.
//! Values are the *cached results* of the cells; formulas are never evaluated.

use std::collections::BTreeMap;

/// A cell value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    /// A finite number (lengths in inches, angles in radians, as stored by Visio).
    Num(f64),
    /// A colour as `0xRRGGBB`.
    Rgb(u32),
    /// Anything else (a string value, a formula result that is not a number).
    Text(String),
}

/// The cells of a sheet or a row.
pub(crate) type Cells = Vec<(&'static str, Val)>;

/// Looks a cell up.
pub(crate) fn cell<'a>(cells: &'a Cells, name: &str) -> Option<&'a Val> {
    cells.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
}

/// A number from a cell.
pub(crate) fn num(cells: &Cells, name: &str) -> Option<f64> {
    match cell(cells, name)? {
        Val::Num(n) => Some(*n),
        _ => None,
    }
}

/// The names of the cells the model keeps; every other cell is dropped while reading.
const KNOWN: &[&str] = &[
    "PinX", "PinY", "Width", "Height", "LocPinX", "LocPinY", "Angle", "FlipX", "FlipY", "BeginX",
    "BeginY", "EndX", "EndY", "TxtPinX", "TxtPinY", "TxtLocPinX", "TxtLocPinY", "TxtWidth",
    "TxtHeight", "TxtAngle", "LineWeight", "LineColor", "LinePattern", "LineColorTrans",
    "Rounding", "BeginArrow", "EndArrow", "BeginArrowSize", "EndArrowSize", "FillForegnd",
    "FillBkgnd", "FillPattern", "FillForegndTrans", "FillBkgndTrans", "ShdwPattern", "LeftMargin",
    "RightMargin", "TopMargin", "BottomMargin", "VerticalAlign", "TextBkgnd", "HideText", "Size",
    "Style", "Color", "ColorTrans", "HorzAlign", "SpLine", "X", "Y", "A", "B", "C", "D", "E",
    "NoFill", "NoLine", "NoShow", "PageWidth", "PageHeight", "PageScale", "DrawingScale",
    "LayerMember", "Visible", "Label", "Value", "Prompt", "Address", "SubAddress", "Description",
    "ImgOffsetX", "ImgOffsetY", "ImgWidth", "ImgHeight",
];

/// The static spelling of a cell name the model keeps.
pub(crate) fn intern(name: &str) -> Option<&'static str> {
    KNOWN.iter().find(|k| **k == name).copied()
}

/// What a geometry row draws (the row types of the Geometry section, Visio SDK reference).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    MoveTo,
    LineTo,
    ArcTo,
    EllipticalArcTo,
    Ellipse,
    NurbsTo,
    PolylineTo,
    InfiniteLine,
    SplineStart,
    SplineKnot,
    RelMoveTo,
    RelLineTo,
    RelCubBezTo,
    RelQuadBezTo,
    RelEllipticalArcTo,
}

impl RowKind {
    /// The row type from its `T` attribute.
    pub(crate) fn from_name(t: &str) -> Option<Self> {
        Some(match t {
            "MoveTo" => Self::MoveTo,
            "LineTo" => Self::LineTo,
            "ArcTo" => Self::ArcTo,
            "EllipticalArcTo" => Self::EllipticalArcTo,
            "Ellipse" => Self::Ellipse,
            "NURBSTo" => Self::NurbsTo,
            "PolylineTo" => Self::PolylineTo,
            "InfiniteLine" => Self::InfiniteLine,
            "SplineStart" => Self::SplineStart,
            "SplineKnot" => Self::SplineKnot,
            "RelMoveTo" => Self::RelMoveTo,
            "RelLineTo" => Self::RelLineTo,
            "RelCubBezTo" => Self::RelCubBezTo,
            "RelQuadBezTo" => Self::RelQuadBezTo,
            "RelEllipticalArcTo" => Self::RelEllipticalArcTo,
            _ => return None,
        })
    }
}

/// A row of a Geometry section.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub(crate) kind: RowKind,
    pub(crate) ix: u32,
    pub(crate) del: bool,
    pub(crate) cells: Cells,
    /// The numbers of the formula of the cell that carries a spline or polyline definition (`E`
    /// of a NURBSTo row: `NURBS(lastKnot, degree, xType, yType, x, y, knot, weight, ...)`; `A` of
    /// a PolylineTo row: `POLYLINE(xType, yType, x, y, ...)`). `None` if the formula is not a
    /// list of plain numbers; the formula is never evaluated.
    pub(crate) args: Option<Vec<f64>>,
}

/// A Geometry section (one path of the shape).
#[derive(Debug, Clone, Default)]
pub(crate) struct Geom {
    pub(crate) ix: u32,
    pub(crate) no_fill: Option<bool>,
    pub(crate) no_line: Option<bool>,
    pub(crate) no_show: Option<bool>,
    pub(crate) del: bool,
    pub(crate) rows: Vec<Row>,
}

/// A row of a Character or Paragraph section.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fmt {
    pub(crate) ix: u32,
    pub(crate) cells: Cells,
}

/// A piece of text with the formats that apply to it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Piece {
    pub(crate) text: String,
    pub(crate) char_ix: u32,
    pub(crate) para_ix: u32,
}

/// What a sheet is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Kind {
    #[default]
    Shape,
    Group,
    Foreign,
    Guide,
}

/// A shape, a master shape or a style sheet.
#[derive(Debug, Clone, Default)]
pub(crate) struct Sheet {
    pub(crate) id: u32,
    pub(crate) kind: Kind,
    pub(crate) name: String,
    /// The master this shape is an instance of.
    pub(crate) master: Option<u32>,
    /// The shape of the master this one stands for.
    pub(crate) master_shape: Option<u32>,
    pub(crate) line_style: Option<u32>,
    pub(crate) fill_style: Option<u32>,
    pub(crate) text_style: Option<u32>,
    pub(crate) cells: Cells,
    pub(crate) geoms: Vec<Geom>,
    pub(crate) chars: Vec<Fmt>,
    pub(crate) paras: Vec<Fmt>,
    pub(crate) text: Vec<Piece>,
    /// Shape data as `(label, value)`.
    pub(crate) data: Vec<(String, String)>,
    /// Hyperlink targets as text (never followed).
    pub(crate) links: Vec<String>,
    /// The layers the shape is a member of.
    pub(crate) layers: Vec<u32>,
    pub(crate) kids: Vec<Sheet>,
    /// What a picture or embedded object is (`bitmap`, `metafile`, `object`).
    pub(crate) foreign: Option<String>,
    /// The sheet has cells of a 3-D effect (bevel, extrusion, rotation).
    pub(crate) effect_3d: bool,
    /// The sheet has cells of a visual effect that is not drawn (glow, soft edges, reflection).
    pub(crate) effect_other: bool,
}

impl Sheet {
    pub(crate) fn num(&self, name: &str) -> Option<f64> {
        num(&self.cells, name)
    }

    /// Whether the shape is one-dimensional (a connector): it has begin and end points.
    #[allow(dead_code)]
    pub(crate) fn is_1d(&self) -> bool {
        cell(&self.cells, "BeginX").is_some() && cell(&self.cells, "EndX").is_some()
    }

    /// The text as one string.
    pub(crate) fn plain_text(&self) -> String {
        self.text.iter().map(|p| p.text.as_str()).collect()
    }
}

/// A master: its shapes and an index from shape id to the position in the tree.
#[derive(Debug, Clone, Default)]
pub(crate) struct Master {
    pub(crate) id: u32,
    #[allow(dead_code)]
    pub(crate) name: String,
    /// A virtual group that holds the top-level shapes of the master.
    pub(crate) root: Sheet,
    index: BTreeMap<u32, Vec<usize>>,
}

impl Master {
    pub(crate) fn new(id: u32, name: String, shapes: Vec<Sheet>) -> Self {
        let root = Sheet { kids: shapes, kind: Kind::Group, ..Sheet::default() };
        let mut m = Self { id, name, root, index: BTreeMap::new() };
        let mut path = Vec::new();
        let mut index = BTreeMap::new();
        index_tree(&m.root, &mut path, &mut index, 0);
        m.index = index;
        m
    }

    /// The shape with this id.
    pub(crate) fn find(&self, id: u32) -> Option<&Sheet> {
        let path = self.index.get(&id)?;
        let mut s = &self.root;
        for i in path {
            s = s.kids.get(*i)?;
        }
        Some(s)
    }

    /// The shape an instance of the whole master stands for: the first top-level shape.
    pub(crate) fn top(&self) -> Option<&Sheet> {
        self.root.kids.first()
    }
}

/// Deepest nesting indexed.
const INDEX_DEPTH: usize = 64;
/// Most shapes indexed in one master.
const INDEX_MAX: usize = 50_000;

fn index_tree(
    s: &Sheet,
    path: &mut Vec<usize>,
    index: &mut BTreeMap<u32, Vec<usize>>,
    depth: usize,
) {
    if depth >= INDEX_DEPTH {
        return;
    }
    for (i, k) in s.kids.iter().enumerate() {
        if index.len() >= INDEX_MAX {
            return;
        }
        path.push(i);
        index.entry(k.id).or_insert_with(|| path.clone());
        index_tree(k, path, index, depth + 1);
        path.pop();
    }
}

/// A glued connector end: connector `from` (`BeginX`/`EndX`, ...) is attached to shape `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Connect {
    pub(crate) from: u32,
    pub(crate) begin: bool,
    pub(crate) to: u32,
}

/// A layer of a page.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Layer {
    pub(crate) ix: u32,
    pub(crate) visible: bool,
}

/// A page.
#[derive(Debug, Clone, Default)]
pub(crate) struct PageDef {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) background: bool,
    /// The background page this page lies over.
    pub(crate) back_page: Option<u32>,
    /// The cells of the page sheet.
    pub(crate) sheet: Cells,
    pub(crate) layers: Vec<Layer>,
    pub(crate) shapes: Vec<Sheet>,
    pub(crate) connects: Vec<Connect>,
    /// Something was cut by a reading limit.
    pub(crate) truncated: bool,
    /// The content of the page was not read (it is not part of this document view).
    pub(crate) loaded: bool,
}

/// A drawing.
#[derive(Debug, Clone, Default)]
pub(crate) struct Doc {
    pub(crate) pages: Vec<PageDef>,
    pub(crate) masters: BTreeMap<u32, Master>,
    pub(crate) styles: BTreeMap<u32, Sheet>,
    /// The colour table (index to `0xRRGGBB`), empty if the file has none.
    pub(crate) colors: Vec<u32>,
    /// The page list was cut by a limit.
    pub(crate) pages_cut: bool,
    /// Layouts that were read but not confirmed against the format description.
    pub(crate) unverified: Vec<&'static str>,
}

impl Doc {
    /// Remembers that a layout was used without confirmation (each once).
    pub(crate) fn unverified(&mut self, what: &'static str) {
        if !self.unverified.contains(&what) {
            self.unverified.push(what);
        }
    }
}

/// Parses a cell value the way the XML readers do: a finite number, a `#RRGGBB` colour or text.
pub(crate) fn parse_val(raw: &str) -> Val {
    let t = raw.trim();
    if let Some(h) = t.strip_prefix('#')
        && h.len() == 6
        && let Ok(v) = u32::from_str_radix(h, 16)
    {
        return Val::Rgb(v);
    }
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => Val::Num(n),
        _ => Val::Text(t.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_numbers_colours_or_text() {
        assert_eq!(parse_val(" 1.5 "), Val::Num(1.5));
        assert_eq!(parse_val("#FF8000"), Val::Rgb(0x00FF_8000));
        assert!(matches!(parse_val("NaN"), Val::Text(_)));
        assert!(matches!(parse_val("inf"), Val::Text(_)));
        assert!(matches!(parse_val("#12"), Val::Text(_)));
    }

    #[test]
    fn a_master_finds_its_shapes_by_id() {
        let leaf = Sheet { id: 7, ..Sheet::default() };
        let group = Sheet { id: 3, kids: vec![leaf], kind: Kind::Group, ..Sheet::default() };
        let m = Master::new(1, "m".into(), vec![group]);
        assert_eq!(m.find(7).map(|s| s.id), Some(7));
        assert_eq!(m.top().map(|s| s.id), Some(3));
        assert!(m.find(99).is_none());
    }
}
