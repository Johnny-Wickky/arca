//! OpenDocument package rebuilding for the sanitizer (`.odt .ods .odp .odg`).
//!
//! An ODF package is a ZIP of XML parts. What goes:
//!
//! * macro libraries, scripts and dialogs (`Basic/`, `Scripts/`, `Dialogs/`), the user-interface
//!   configuration (`Configurations2/`), thumbnails, digital signatures (they would no longer
//!   match), embedded objects and their previews (`Object N/`, `ObjectReplacements/`) and any
//!   other member that is an OLE container or an executable;
//! * in the XML: `office:scripts`, event listeners, DDE links, plug-ins, applets, OLE frames,
//!   every reference that leaves the package or invokes a macro (hyperlinks keep their text),
//!   cell formulas that call out (the cached value stays), tracked changes (accepted),
//!   comments, and the fields that display who edited the document and when;
//! * metadata: everything under `office:meta`, and printer names in `settings.xml`.
//!
//! Everything else is copied byte for byte, the `mimetype` member stays first and uncompressed,
//! and the manifest is rewritten to match.

use std::collections::BTreeSet;

use arca_container::InflateBudget;
use arca_container::xml::rewrite::{Action, AttrAction, rebuild_tag, rewrite};
use arca_container::xml::{self, XmlLimits};
use arca_container::zip::ZipArchive;
use arca_container::zip::writer::{Compression, ZipWriter};
use quick_xml::events::{BytesStart, Event};

use super::{ActionKind, SanitizeAction, SanitizeError, SanitizeOptions};
use crate::format::{FileFormat, Magic, odf_format_from_mimetype, sniff};
use crate::{ScanPolicy, formulas, url};

fn err(e: impl core::fmt::Display) -> SanitizeError {
    SanitizeError::Rebuild(e.to_string())
}

/// Directories (and name prefixes) whose members never survive, with the reason.
const DROPPED_PREFIXES: &[(&str, &str)] = &[
    ("basic/", "macro library"),
    ("scripts/", "script"),
    ("dialogs/", "dialog"),
    ("configurations2/", "user-interface configuration"),
    ("thumbnails/", "thumbnail"),
    ("objectreplacements/", "object preview"),
    ("object ", "embedded object"),
    ("meta-inf/documentsignatures.xml", "digital signature"),
    ("meta-inf/macrosignatures.xml", "macro signature"),
];

/// Fields (`text:*`) that show who touched the document, when, or where it lives.
const META_FIELDS: &[&str] = &[
    "author-name",
    "author-initials",
    "initial-creator",
    "creator",
    "editing-cycles",
    "editing-duration",
    "creation-date",
    "creation-time",
    "modification-date",
    "modification-time",
    "print-date",
    "print-time",
    "printed-by",
    "file-name",
    "template-name",
    "user-defined",
    "sender-company",
    "sender-firstname",
    "sender-lastname",
    "sender-initials",
    "sender-street",
    "sender-country",
    "sender-postal-code",
    "sender-city",
    "sender-title",
    "sender-position",
    "sender-phone-private",
    "sender-phone-work",
    "sender-fax",
    "sender-email",
    "sender-state-or-province",
];

/// Which part is being rewritten.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    /// `content.xml`, `styles.xml`.
    Body,
    Meta,
    Settings,
    Manifest,
}

/// Why the member called `lname` (lower case) must go, judging by its name alone.
fn dropped_by_name(lname: &str) -> Option<&'static str> {
    DROPPED_PREFIXES.iter().find(|(p, _)| lname.starts_with(p)).map(|(_, why)| *why)
}

/// `true` for a link that leaves the package or runs a macro.
fn bad_href(href: &str) -> bool {
    let h = href.trim();
    let lower = h.to_ascii_lowercase();
    lower.starts_with("vnd.sun.star.script:")
        || lower.starts_with("macro:")
        || url::leaves_package(h)
        || h.starts_with('/')
        || h.trim_start_matches("./").starts_with("../")
}

fn start_of<'a>(item: &'a xml::XmlItem<'_>) -> Option<(&'a BytesStart<'a>, bool)> {
    match &item.event {
        Event::Start(e) => Some((e, false)),
        Event::Empty(e) => Some((e, true)),
        _ => None,
    }
}

fn prefix_of(e: &BytesStart<'_>) -> Option<String> {
    let name: &str = e.name().into_inner();
    xml::split_qname(name).0.map(str::to_owned)
}

type Act<'a> = &'a mut dyn FnMut(ActionKind, &str);

fn xml_part(
    src: &str,
    part: Part,
    opts: &SanitizeOptions,
    limits: XmlLimits,
    removed: &BTreeSet<String>,
    act: Act<'_>,
) -> Result<String, SanitizeError> {
    rewrite(src, limits, |item, ctx| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        let prefix = prefix_of(e);
        let prefix = prefix.as_deref();

        if part == Part::Manifest {
            if local == "file-entry" {
                let path = xml::attr(e, "full-path").unwrap_or_default();
                let lname = path.trim_end_matches('/').to_ascii_lowercase();
                let hit = removed.contains(&lname)
                    || dropped_by_name(&lname).is_some()
                    || dropped_by_name(&format!("{lname}/")).is_some();
                if hit {
                    return Ok(Action::Drop);
                }
            }
            return Ok(Action::Keep);
        }

        // Scripts, DDE and active frames: always.
        match local {
            "scripts" | "event-listeners" | "event-listener" => {
                act(ActionKind::RemovedElement, &format!("{}:{local}", prefix.unwrap_or("")));
                return Ok(Action::Drop);
            }
            "dde-source"
            | "dde-link"
            | "dde-links"
            | "dde-connection"
            | "dde-connection-decl"
            | "dde-connection-decls" => {
                act(ActionKind::RemovedElement, &format!("DDE link ({local})"));
                return Ok(Action::Drop);
            }
            "plugin" | "applet" | "floating-frame" | "object-ole" => {
                act(ActionKind::RemovedElement, &format!("{}:{local}", prefix.unwrap_or("")));
                return Ok(Action::Drop);
            }
            "object" if prefix == Some("draw") => {
                act(ActionKind::RemovedElement, "draw:object (embedded object)");
                return Ok(Action::Drop);
            }
            _ => {}
        }
        if opts.accept_revisions
            && (local == "tracked-changes"
                || (prefix == Some("text")
                    && matches!(local, "change" | "change-start" | "change-end")))
        {
            if local == "tracked-changes" {
                act(ActionKind::AcceptedRevisions, "tracked changes");
            }
            return Ok(Action::Drop);
        }
        if opts.remove_comments
            && prefix == Some("office")
            && matches!(local, "annotation" | "annotation-end")
        {
            if local == "annotation" {
                act(ActionKind::RemovedComments, "office:annotation");
            }
            return Ok(Action::Drop);
        }
        if opts.strip_metadata && prefix == Some("text") && META_FIELDS.contains(&local) {
            act(ActionKind::StrippedMetadata, &format!("field text:{local}"));
            return Ok(Action::Drop);
        }
        if part == Part::Meta && opts.strip_metadata && ctx.parent() == Some("meta") {
            act(ActionKind::StrippedMetadata, &format!("meta:{local}"));
            return Ok(Action::Drop);
        }
        if part == Part::Settings
            && opts.strip_metadata
            && local == "config-item"
            && matches!(
                xml::attr(e, "name").as_deref(),
                Some("PrinterName" | "PrinterSetup" | "PrinterPaperFromSetup")
            )
        {
            act(ActionKind::StrippedMetadata, "printer settings");
            return Ok(Action::Drop);
        }

        // Addresses: a link that leaves the package is removed (a hyperlink keeps its text);
        // so is a picture or a frame whose target was dropped with the package members.
        if let Some(href) = xml::attr(e, "href") {
            let target = href.trim_start_matches("./").to_ascii_lowercase();
            let dropped_target = removed.contains(&target)
                || dropped_by_name(&target).is_some()
                || dropped_by_name(&format!("{target}/")).is_some();
            if bad_href(&href) || dropped_target {
                act(ActionKind::RemovedExternalReference, &format!("{local} → {href}"));
                return Ok(if local == "a" { Action::Unwrap } else { Action::Drop });
            }
        }

        // Attributes: formulas that call out, and button targets that leave the package.
        let mut drop_formula = false;
        if matches!(local, "table-cell" | "covered-table-cell")
            && let Some(raw) = xml::attr(e, "formula")
        {
            let f = formulas::strip_odf_prefix(&raw);
            drop_formula = (formulas::may_contain_risk(f)
                && (!formulas::assess(f).is_empty() || !formulas::url_uses(f).is_empty()))
                || !formulas::odf_document_refs(f).is_empty();
            if drop_formula {
                act(ActionKind::RemovedFormula, &format!("={f}"));
            }
        }
        let bad_target = xml::attrs(e)
            .iter()
            .any(|(k, v)| xml::split_qname(k).1 == "target-location" && bad_href(v));
        if drop_formula || bad_target {
            if bad_target {
                act(ActionKind::RemovedExternalReference, &format!("{local} target location"));
            }
            return Ok(Action::Replace(rebuild_tag(e, empty, |_, l, v| {
                if (drop_formula && l == "formula") || (l == "target-location" && bad_href(v)) {
                    AttrAction::Drop
                } else {
                    AttrAction::Keep
                }
            })));
        }
        Ok(Action::Keep)
    })
    .map_err(err)
}

/// Rebuilds an ODF package without anything active, remote or personal.
pub(super) fn rebuild(
    data: &[u8],
    policy: &ScanPolicy,
    opts: &SanitizeOptions,
    format: FileFormat,
) -> Result<(Vec<u8>, FileFormat, Vec<SanitizeAction>), SanitizeError> {
    let limits = policy.xml;
    let zip = ZipArchive::parse(data, policy.zip).map_err(err)?;
    let mut budget = InflateBudget::new(policy.inflate_budget, policy.aggregate_ratio, 16 << 20)
        .with_original_input(data.len() as u64);
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    for e in zip.entries() {
        if e.is_dir() {
            continue;
        }
        let bytes = zip.read(e, &mut budget).map_err(err)?;
        members.push((e.name().to_owned(), bytes));
    }
    let mut actions: Vec<SanitizeAction> = Vec::new();
    let mut act = |kind: ActionKind, target: &str| {
        if actions.len() < 10_000 {
            actions.push(SanitizeAction {
                kind,
                target: arca_container::text::display_safe(target, 300),
            });
        }
    };

    let mimetype = members
        .iter()
        .find(|(n, _)| n == "mimetype")
        .map(|(_, b)| b.clone())
        .ok_or_else(|| err("missing mimetype member"))?;
    let new_format = odf_format_from_mimetype(&String::from_utf8_lossy(&mimetype));
    debug_assert!(new_format == format || format == FileFormat::OdfOther);

    // ---- 1. members that go ---------------------------------------------------------
    let mut removed: BTreeSet<String> = BTreeSet::new();
    for (name, bytes) in &members {
        let lname = name.to_ascii_lowercase();
        if lname == "mimetype" {
            continue;
        }
        let why = dropped_by_name(&lname).map(str::to_owned).or_else(|| {
            if arca_container::cfb::has_cfb_signature(bytes) {
                Some("embedded OLE object".to_owned())
            } else if let Magic::Executable(what) = sniff(bytes) {
                Some(format!("{what} stored inside the package"))
            } else {
                None
            }
        });
        if let Some(why) = why {
            act(ActionKind::RemovedPart, &format!("{name} ({why})"));
            removed.insert(lname);
        }
    }

    // ---- 2. rewrite and write -------------------------------------------------------
    let mut w = ZipWriter::new();
    w.add("mimetype", &mimetype, Compression::Stored).map_err(err)?;
    for (name, bytes) in &members {
        let lname = name.to_ascii_lowercase();
        if lname == "mimetype" || removed.contains(&lname) {
            continue;
        }
        let part = match lname.as_str() {
            "content.xml" | "styles.xml" => Some(Part::Body),
            "meta.xml" => Some(Part::Meta),
            "settings.xml" => Some(Part::Settings),
            "meta-inf/manifest.xml" => Some(Part::Manifest),
            _ => None,
        };
        let out = match part {
            Some(part) => {
                let decoded = xml::decode_xml_bytes(bytes, &limits).map_err(err)?;
                // Written back as UTF-8: a UTF-16 declaration must not survive.
                let src = xml::declare_utf8(&decoded).into_owned();
                if part == Part::Manifest && src.contains("encryption-data") {
                    return Err(err("the document is encrypted and cannot be rebuilt"));
                }
                if part == Part::Meta && opts.strip_metadata {
                    act(ActionKind::StrippedMetadata, "meta.xml");
                }
                xml_part(&src, part, opts, limits, &removed, &mut act)?.into_bytes()
            }
            None => bytes.clone(),
        };
        w.add(name, &out, Compression::Deflated).map_err(err)?;
    }
    let bytes = w.finish().map_err(err)?;
    Ok((bytes, new_format, actions))
}
