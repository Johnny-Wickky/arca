//! PresentationML and Visio (VSDX) parts.

use arca_container::xml;
use quick_xml::events::BytesStart;

use crate::report::{Severity, ThreatKind};
use crate::xmlwalk::{self, Visitor};
use crate::{Abort, Ctx};

/// PowerPoint action verbs that run something.
fn action_risk(action: &str) -> Option<(&'static str, Severity)> {
    let a = action.to_ascii_lowercase();
    if a.starts_with("ppaction://program") {
        Some(("runs an external program", Severity::Critical))
    } else if a.starts_with("ppaction://macro") {
        Some(("runs a macro", Severity::High))
    } else if a.starts_with("ppaction://ole") {
        Some(("activates an OLE object", Severity::High))
    } else {
        None
    }
}

struct SlideVisitor {
    loc: String,
}

impl Visitor for SlideVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, anc: &[String]) {
        let local = xml::local_name(e);
        if let Some(action) = xml::attr(e, "action")
            && let Some((what, sev)) = action_risk(&action)
        {
            let trigger = if local.eq_ignore_ascii_case("hlinkHover")
                || local.eq_ignore_ascii_case("hlinkMouseOver")
            {
                "on mouse-over"
            } else {
                "on click"
            };
            ctx.report.add_with(
                ThreatKind::ProgramAction,
                sev,
                &self.loc,
                format!("{action} {what} {trigger}"),
            );
        }
        match local {
            "sld"
                if anc.is_empty()
                    && xml::attr(e, "show").is_some_and(|v| v == "0" || v == "false") =>
            {
                ctx.report.add(
                    ThreatKind::HiddenSlide,
                    &self.loc,
                    "slide is hidden from the slide show",
                );
            }
            "oleObj" => super::word::ole_object(ctx, e, &self.loc),
            "control" => {
                let name = xml::attr(e, "name").unwrap_or_default();
                ctx.report.add(
                    ThreatKind::ActiveXControl,
                    &self.loc,
                    format!("ActiveX control {name:?} on slide"),
                );
            }
            _ => {}
        }
    }
}

/// Scans a slide, layout, master or notes part.
pub(crate) fn scan_slide(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = SlideVisitor { loc: loc.to_owned() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}

struct PresentationVisitor {
    loc: String,
}

impl Visitor for PresentationVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, anc: &[String]) {
        if xml::local_name(e) == "font"
            && anc.last().is_some_and(|a| a == "embeddedFont")
            && let Some(face) = xml::attr(e, "typeface")
        {
            ctx.report.add(
                ThreatKind::EmbeddedFont,
                &self.loc,
                format!("embedded font {face:?} is never used for rendering"),
            );
        }
    }
}

/// Scans `ppt/presentation.xml`.
pub(crate) fn scan_presentation(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = PresentationVisitor { loc: loc.to_owned() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}

/// ShapeSheet functions that execute code or open things.
const VISIO_ACTIVE: &[(&str, &str)] = &[
    ("RUNADDON(", "RUNADDON"),
    ("RUNADDONWARGS(", "RUNADDONWARGS"),
    ("RUNMACRO(", "RUNMACRO"),
    ("CALLTHIS(", "CALLTHIS"),
    ("OPENFILE(", "OPENFILE"),
    ("OPENSHEETWIN(", "OPENSHEETWIN"),
    ("HYPERLINK(", "HYPERLINK"),
    ("SHELLEXECUTE", "ShellExecute"),
];

/// ShapeSheet cells that fire without a deliberate command.
const VISIO_EVENTS: &[&str] = &[
    "EventDblClick",
    "EventDrop",
    "EventMultiDrop",
    "EventXFMod",
    "TheData",
    "TheText",
    "EventChildDrop",
];

struct VisioVisitor {
    loc: String,
}

impl Visitor for VisioVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &[String]) {
        match xml::local_name(e) {
            "Cell" => {
                let Some(formula) = xml::attr(e, "F") else { return };
                let upper = formula.to_ascii_uppercase();
                let cell = xml::attr(e, "N").unwrap_or_default();
                for (needle, label) in VISIO_ACTIVE {
                    if upper.contains(needle) {
                        let sev = if VISIO_EVENTS.iter().any(|ev| ev.eq_ignore_ascii_case(&cell)) {
                            Severity::High
                        } else {
                            Severity::Medium
                        };
                        ctx.report.add_with(
                            ThreatKind::ProgramAction,
                            sev,
                            &self.loc,
                            format!("ShapeSheet {cell} = {formula} ({label})"),
                        );
                    }
                }
            }
            "EventItem" => {
                let action = xml::attr(e, "Action").unwrap_or_default();
                let target = xml::attr(e, "Target").unwrap_or_default();
                let args = xml::attr(e, "TargetArgs").unwrap_or_default();
                if !target.is_empty() {
                    ctx.report.add(
                        ThreatKind::ProgramAction,
                        &self.loc,
                        format!("document event runs add-on {target:?} {args:?} (action {action})"),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Scans Visio pages, masters and the document part.
pub(crate) fn scan_visio(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = VisioVisitor { loc: loc.to_owned() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}
