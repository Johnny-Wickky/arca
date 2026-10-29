//! Visio drawings as pages: what is drawn, what is approximated and what is not drawn.
//!
//! `.vsdx` (OPC/XML) and `.vsd` (binary) are read into one model ([`model`]) and drawn by one
//! renderer, so both formats are shown the same way: pages (the foreground ones; a background page
//! is drawn under the pages that name it), shapes placed with `PinX`, `PinY`, `Width`, `Height`,
//! `LocPinX`, `LocPinY`, `Angle`, `FlipX`, `FlipY`, groups, the inheritance shape → master shape →
//! style sheet, Geometry sections as paths (lines, arcs, ellipses, polylines, NURBS and splines),
//! fill, line and text formats, text blocks, connectors (their own geometry) and picture
//! placeholders.
//!
//! **Approximated** (the page is marked approximate and says why): NURBS and splines are sampled
//! polylines; fill patterns and gradients are flat colours; dashed lines are solid; arrowheads are
//! plain triangles; rounded corners are square; text is laid out with estimated glyph widths.
//! **Not drawn**: 3-D effects (bevel, extrusion, 3-D rotation: Visio has no 3-D scene, they are
//! effects of a shape), glow, soft edges, reflection and shadows, pictures (placeholders only).
//! **Not verified against files made by Microsoft**: the arithmetic of flips and the layouts of
//! binary `.vsd` records (see the module `vsd`); everything that follows from a layout that is
//! known only from open-source descriptions is listed in the page's notes.
//!
//! Zero-trust: nothing is executed or loaded; formulas are not evaluated (the stored results are
//! used); every loop is bounded; hostile numbers are clamped.

pub(crate) mod geom;
pub(crate) mod model;
mod render;
pub(crate) mod text;
mod vsdx_read;

#[cfg(test)]
pub(crate) mod tests;

use crate::detect::{self, Doc as Opened};
use crate::model::FileFormat;
use crate::page::PageModel;
use crate::{PreviewError, PreviewPolicy};

pub(crate) use render::Rendered;

/// What to read of a drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    /// The page list only.
    Count,
    /// The page list and the content of foreground page `n` (and its background page).
    Page(usize),
    /// Every page.
    All,
}

fn load(data: &[u8], policy: &PreviewPolicy, want: Want) -> Result<model::Doc, PreviewError> {
    if data.len() > policy.max_input_bytes {
        return Err(PreviewError::TooLarge(policy.max_input_bytes));
    }
    match detect::open(data, policy)? {
        Opened::Opc {
            mut pkg,
            format: FileFormat::Vsdx | FileFormat::Vsdm | FileFormat::VisioTemplateOrStencil,
        } => vsdx_read::read(&mut pkg, policy, want),
        Opened::Cfb { cf, format: FileFormat::Vsd } => crate::vsd::read_doc(&cf, policy, want),
        _ => Err(PreviewError::Unsupported(detect::format_of(data, policy))),
    }
}

/// The number of pages of a Visio drawing (`.vsdx` family or `.vsd`): the foreground pages.
///
/// # Errors
/// [`PreviewError::Unsupported`] for other formats; [`PreviewError::Malformed`] when the page list
/// cannot be read.
#[allow(dead_code)] // wired by the page-rendering entry point of the engine
pub(crate) fn visio_page_count(data: &[u8], policy: &PreviewPolicy) -> Result<usize, PreviewError> {
    let doc = load(data, policy, Want::Count)?;
    Ok(doc.pages.iter().filter(|p| !p.background).count())
}

/// Draws foreground page `index` (from 0) of a Visio drawing as a page.
///
/// # Errors
/// As [`visio_page_count`]; [`PreviewError::Malformed`] when there is no such page.
pub(crate) fn render_visio_page(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<PageModel, PreviewError> {
    render_visio_page_noted(data, index, policy).map(|r| r.page)
}

/// [`render_visio_page`] together with the reasons the page is approximate.
///
/// # Errors
/// As [`render_visio_page`].
pub(crate) fn render_visio_page_noted(
    data: &[u8],
    index: usize,
    policy: &PreviewPolicy,
) -> Result<Rendered, PreviewError> {
    let doc = load(data, policy, Want::Page(index))?;
    let foreground: Vec<&model::PageDef> = doc.pages.iter().filter(|p| !p.background).collect();
    let Some(page) = foreground.get(index).copied() else {
        return Err(PreviewError::Malformed("the document has no such page".into()));
    };
    if !page.loaded {
        return Err(PreviewError::Malformed("the page could not be read".into()));
    }
    let back = page
        .back_page
        .and_then(|id| doc.pages.iter().find(|p| p.id == id && p.background && p.loaded));
    let count = u32::try_from(foreground.len()).unwrap_or(u32::MAX);
    let mut r = render::draw_page(
        &doc,
        page,
        back,
        u32::try_from(index).unwrap_or(u32::MAX),
        count,
        policy.reveal_hidden,
    );
    r.page.truncated |= doc.pages_cut;
    Ok(r)
}
