//! Visio 2003–2010 binary drawings (`VisioDocument` stream).
//!
//! The stream is a tree of chunks, most of them LZ77-compressed, whose layout is only partly
//! public; the ShapeSheet formulas that run add-ons live inside them. They are therefore not
//! interpreted. What the scanner can do is look for the names of the dangerous ShapeSheet
//! functions in the bytes it can see (they appear in clear text in uncompressed chunks, as ASCII
//! or UTF-16LE), and — because everything else is invisible to it — report that the drawing
//! could not be verified, so that no `.vsd` is ever called clean.

use crate::report::{Severity, ThreatKind};
use crate::{Ctx, memfind_ci, utf16le_bytes};

/// ShapeSheet functions that run code or open things (lower case).
const ACTIVE: &[&str] = &[
    "runaddon",
    "runaddonwargs",
    "runmacro",
    "callthis",
    "openfile",
    "opensheetwin",
    "shellexecute",
];

/// Scans the `VisioDocument` stream.
pub(crate) fn scan(ctx: &mut Ctx<'_>, stream: &[u8], loc: &str) {
    for name in ACTIVE {
        if memfind_ci(stream, name.as_bytes()).is_some()
            || memfind_ci(stream, &utf16le_bytes(name)).is_some()
        {
            ctx.report.add_with(
                ThreatKind::ProgramAction,
                Severity::High,
                loc,
                format!(
                    "the drawing contains the ShapeSheet function {}",
                    name.to_ascii_uppercase()
                ),
            );
        }
    }
    ctx.report.add(
        ThreatKind::ScanLimitReached,
        loc,
        "binary Visio drawings are not interpreted (their ShapeSheet chunks are compressed); only a text search was possible",
    );
}
