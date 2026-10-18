//! OPC package rebuilding for the sanitizer.

use std::collections::{BTreeMap, BTreeSet};

use arca_container::InflateBudget;
use arca_container::xml::rewrite::{Action, AttrAction, Context, rebuild_tag, rewrite};
use arca_container::xml::{self, SecureXmlReader, XmlError, XmlItem, XmlLimits};
use arca_container::zip::ZipArchive;
use arca_container::zip::writer::{Compression, ZipWriter};
use quick_xml::events::{BytesStart, Event};

use super::{ActionKind, SanitizeAction, SanitizeError, SanitizeOptions};
use crate::format::{FileFormat, opc_format_from_content_type};
use crate::ooxml::{Rel, Role, rels_source, resolve_target, role};
use crate::{ScanPolicy, fields, formulas, url};

/// Relationship-valued attribute local names (with an `r:` prefix).
const REL_ATTRS: &[&str] = &["id", "embed", "link", "pict", "href", "dm", "lo", "qs", "cs"];

/// Elements removed entirely when they point at a removed relationship.
const DROP_IF_REFS: &[&str] = &[
    "attachedTemplate",
    "sheet",
    "externalReference",
    "control",
    "altChunk",
    "subDoc",
    "OLEObject",
    "oleObject",
    "hlinkClick",
    "hlinkHover",
    "hlinkMouseOver",
    "embedRegular",
    "embedBold",
    "embedItalic",
    "embedBoldItalic",
    "sourceFileName",
    "dataSource",
    "headerSource",
    "embeddedFont",
    "oleObj",
    "legacyDrawing",
];

/// Macro-enabled → macro-free content types.
const CT_MAP: &[(&str, &str)] = &[
    (
        "application/vnd.ms-word.document.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    ),
    (
        "application/vnd.ms-word.template.macroenabledtemplate.main+xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    ),
    (
        "application/vnd.ms-excel.sheet.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
    ),
    (
        "application/vnd.ms-excel.template.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
    ),
    (
        "application/vnd.ms-excel.addin.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
    ),
    (
        "application/vnd.ms-powerpoint.presentation.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
    ),
    (
        "application/vnd.ms-powerpoint.slideshow.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow.main+xml",
    ),
    (
        "application/vnd.ms-powerpoint.template.macroenabled.main+xml",
        "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml",
    ),
    (
        "application/vnd.ms-visio.drawing.macroenabled.main+xml",
        "application/vnd.ms-visio.drawing.main+xml",
    ),
    (
        "application/vnd.ms-visio.template.macroenabled.main+xml",
        "application/vnd.ms-visio.template.main+xml",
    ),
    (
        "application/vnd.ms-visio.stencil.macroenabled.main+xml",
        "application/vnd.ms-visio.stencil.main+xml",
    ),
];

fn err(e: impl core::fmt::Display) -> SanitizeError {
    SanitizeError::Rebuild(e.to_string())
}

struct Parts {
    order: Vec<String>,
    data: BTreeMap<String, Vec<u8>>,
}

impl Parts {
    fn key(name: &str) -> String {
        name.to_ascii_lowercase()
    }
    fn get(&self, name: &str) -> Option<&Vec<u8>> {
        self.data.get(&Self::key(name))
    }
}

fn text_of(bytes: &[u8], limits: &XmlLimits) -> Result<String, SanitizeError> {
    xml::decode_xml_bytes(bytes, limits).map(std::borrow::Cow::into_owned).map_err(err)
}

/// `(defaults: ext → ct, overrides: part → ct)`.
/// `(extension → content type, part name → content type)`.
type ContentTypeMaps = (BTreeMap<String, String>, BTreeMap<String, String>);

fn content_types(src: &str, limits: XmlLimits) -> Result<ContentTypeMaps, XmlError> {
    let mut defaults = BTreeMap::new();
    let mut overrides = BTreeMap::new();
    let mut r = SecureXmlReader::new(src, limits)?;
    while let Some(item) = r.next_item()? {
        if let Event::Start(e) | Event::Empty(e) = &item.event {
            match xml::local_name(e) {
                "Default" => {
                    if let (Some(x), Some(ct)) =
                        (xml::attr(e, "Extension"), xml::attr(e, "ContentType"))
                    {
                        defaults.insert(x.to_ascii_lowercase(), ct);
                    }
                }
                "Override" => {
                    if let (Some(p), Some(ct)) =
                        (xml::attr(e, "PartName"), xml::attr(e, "ContentType"))
                    {
                        overrides.insert(
                            url::percent_decode(p.trim_start_matches('/')).to_ascii_lowercase(),
                            ct,
                        );
                    }
                }
                _ => {}
            }
        }
    }
    Ok((defaults, overrides))
}

fn parse_rels(src: &str, limits: XmlLimits) -> Result<Vec<Rel>, XmlError> {
    let mut out = Vec::new();
    let mut r = SecureXmlReader::new(src, limits)?;
    while let Some(item) = r.next_item()? {
        if let Event::Start(e) | Event::Empty(e) = &item.event
            && xml::local_name(e) == "Relationship"
        {
            out.push(Rel {
                id: xml::attr(e, "Id").unwrap_or_default(),
                rtype: xml::attr(e, "Type").unwrap_or_default(),
                target: xml::attr(e, "Target").unwrap_or_default(),
                external: xml::attr(e, "TargetMode")
                    .is_some_and(|m| m.eq_ignore_ascii_case("External")),
            });
        }
    }
    Ok(out)
}

fn refers(e: &BytesStart<'_>, dropped: &BTreeSet<String>) -> bool {
    !dropped.is_empty()
        && xml::attrs(e).iter().any(|(k, v)| {
            let (p, l) = xml::split_qname(k);
            dropped.contains(v) && ((p == Some("r") && REL_ATTRS.contains(&l)) || l == "relid")
        })
}

/// Rebuilds the tag if any attribute must go; `None` if nothing changes.
fn strip_attrs(
    e: &BytesStart<'_>,
    empty: bool,
    mut drop: impl FnMut(Option<&str>, &str, &str) -> bool,
) -> Option<String> {
    let any = xml::attrs(e).iter().any(|(k, v)| {
        let (p, l) = xml::split_qname(k);
        drop(p, l, v)
    });
    any.then(|| {
        rebuild_tag(
            e,
            empty,
            |p, l, v| if drop(p, l, v) { AttrAction::Drop } else { AttrAction::Keep },
        )
    })
}

fn start_of<'a>(item: &'a XmlItem<'_>) -> Option<(&'a BytesStart<'a>, bool)> {
    match &item.event {
        Event::Start(e) => Some((e, false)),
        Event::Empty(e) => Some((e, true)),
        _ => None,
    }
}

/// Rebuilds the package.
pub(super) fn rebuild(
    data: &[u8],
    policy: &ScanPolicy,
    opts: &SanitizeOptions,
    format: FileFormat,
) -> Result<(Vec<u8>, FileFormat, Vec<SanitizeAction>), SanitizeError> {
    let limits = policy.xml;
    let zip = ZipArchive::parse(data, policy.zip).map_err(err)?;
    let mut budget = InflateBudget::new(policy.inflate_budget, policy.aggregate_ratio, 16 << 20);
    let mut parts = Parts { order: Vec::new(), data: BTreeMap::new() };
    for e in zip.entries() {
        if e.is_dir() {
            continue;
        }
        let bytes = zip.read(e, &mut budget).map_err(err)?;
        parts.order.push(e.name().to_owned());
        parts.data.insert(Parts::key(e.name()), bytes);
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

    let ct_src = text_of(
        parts.get("[Content_Types].xml").ok_or_else(|| err("missing [Content_Types].xml"))?,
        &limits,
    )?;
    let (defaults, overrides) = content_types(&ct_src, limits).map_err(err)?;
    let ct_of = |name: &str| -> String {
        let key = name.to_ascii_lowercase();
        overrides.get(&key).cloned().unwrap_or_else(|| {
            let ext = key
                .rsplit('/')
                .next()
                .and_then(|f| f.rsplit_once('.'))
                .map(|(_, x)| x)
                .unwrap_or("");
            defaults.get(ext).cloned().unwrap_or_default()
        })
    };

    // ---- 1. parts to remove -------------------------------------------------
    let mut dropped_parts: BTreeSet<String> = BTreeSet::new();
    for name in &parts.order {
        let lname = name.to_ascii_lowercase();
        let r = role(name, &ct_of(name));
        let drop = match r {
            Role::VbaProject
            | Role::VbaSignature
            | Role::VbaData
            | Role::ActiveX
            | Role::Embedding
            | Role::RibbonUi
            | Role::MacroSheet
            | Role::ExternalLink
            | Role::Connections
            | Role::PrinterSettings
            | Role::Font
            | Role::Thumbnail => true,
            Role::CustomProps | Role::Labels => opts.strip_metadata,
            Role::People | Role::Comments => opts.remove_comments,
            Role::CustomXml => opts.remove_custom_xml,
            _ => {
                lname.contains("querytables/")
                    || (lname.contains("externallinks/") && !lname.ends_with(".rels"))
            }
        };
        if drop && r != Role::Rels {
            dropped_parts.insert(lname);
        }
    }
    // Relationship parts of removed parts go too.
    for name in &parts.order {
        let lname = name.to_ascii_lowercase();
        if lname.ends_with(".rels")
            && let Some(src) = rels_source(name)
            && dropped_parts.contains(&src.to_ascii_lowercase())
        {
            dropped_parts.insert(lname);
        }
    }
    for name in &parts.order {
        if dropped_parts.contains(&name.to_ascii_lowercase()) {
            act(ActionKind::RemovedPart, name);
        }
    }

    // ---- 2. relationships -----------------------------------------------------
    // source part (lowercase) → removed relationship ids
    let mut dropped_ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut new_rels: BTreeMap<String, String> = BTreeMap::new();
    for name in &parts.order {
        let lname = name.to_ascii_lowercase();
        if !lname.ends_with(".rels") || dropped_parts.contains(&lname) {
            continue;
        }
        let Some(source) = rels_source(name) else { continue };
        let src = text_of(parts.get(name).ok_or_else(|| err("missing part"))?, &limits)?;
        let rels = parse_rels(&src, limits).map_err(err)?;
        let mut ids = BTreeSet::new();
        for rel in &rels {
            let external = rel.external
                || url::scheme(&rel.target).is_some()
                || rel.target.starts_with("\\\\");
            if external {
                ids.insert(rel.id.clone());
                act(
                    ActionKind::RemovedExternalReference,
                    &format!("{} → {}", rel.type_suffix(), rel.target),
                );
                continue;
            }
            match resolve_target(&source, &rel.target) {
                Some(t) if dropped_parts.contains(&t.to_ascii_lowercase()) => {
                    ids.insert(rel.id.clone());
                    act(ActionKind::RemovedRelationship, &format!("{} → {t}", rel.type_suffix()));
                }
                None => {
                    ids.insert(rel.id.clone());
                    act(
                        ActionKind::RemovedRelationship,
                        &format!("{} → {} (escapes package)", rel.type_suffix(), rel.target),
                    );
                }
                Some(_) => {}
            }
        }
        if !ids.is_empty() {
            let out = rewrite(&src, limits, |item, _| {
                Ok(match start_of(item) {
                    Some((e, _))
                        if xml::local_name(e) == "Relationship"
                            && xml::attr(e, "Id").is_some_and(|id| ids.contains(&id)) =>
                    {
                        Action::Drop
                    }
                    _ => Action::Keep,
                })
            })
            .map_err(err)?;
            new_rels.insert(lname.clone(), out);
            dropped_ids.insert(source.to_ascii_lowercase(), ids);
        }
    }

    // ---- 3. rewrite parts -----------------------------------------------------
    let mut out_parts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let empty = BTreeSet::new();
    let mut new_format = format;
    for name in &parts.order {
        let lname = name.to_ascii_lowercase();
        if dropped_parts.contains(&lname) {
            continue;
        }
        if let Some(r) = new_rels.remove(&lname) {
            out_parts.insert(lname, r.into_bytes());
            continue;
        }
        let bytes = parts.get(name).ok_or_else(|| err("missing part"))?;
        if lname == "[content_types].xml" {
            let (out, converted) = rewrite_content_types(&ct_src, &dropped_parts, limits)?;
            if let Some((from, to)) = converted {
                act(ActionKind::ConvertedContentType, &format!("{from} → {to}"));
                new_format = opc_format_from_content_type(&to);
            }
            out_parts.insert(lname, out.into_bytes());
            continue;
        }
        let ids = dropped_ids.get(&lname).unwrap_or(&empty);
        let r = role(name, &ct_of(name));
        let is_xml = !matches!(r, Role::Binary | Role::Thumbnail)
            && (lname.ends_with(".xml") || lname.ends_with(".vml") || ct_of(name).contains("xml"));
        if !is_xml {
            out_parts.insert(lname, bytes.clone());
            continue;
        }
        let src = text_of(bytes, &limits)?;
        let out = match r {
            Role::WordBody => word_body(&src, ids, opts, limits, &mut act)?,
            Role::WordSettings => word_settings(&src, ids, opts, limits, &mut act)?,
            Role::Workbook => workbook(&src, ids, opts, limits, &mut act)?,
            Role::Worksheet => worksheet(&src, ids, limits, &mut act)?,
            Role::Slide | Role::Presentation => slide(&src, ids, limits, &mut act)?,
            Role::Visio => visio(&src, ids, limits, &mut act)?,
            Role::CoreProps if opts.strip_metadata => core_props(&src, limits, &mut act)?,
            Role::AppProps if opts.strip_metadata => app_props(&src, limits, &mut act)?,
            Role::Vml => vml(&src, ids, opts, limits)?,
            _ if !ids.is_empty() => generic(&src, ids, limits)?,
            _ => src,
        };
        out_parts.insert(lname, out.into_bytes());
    }

    // ---- 4. write -------------------------------------------------------------
    let mut w = ZipWriter::new();
    let ct_key = "[content_types].xml";
    let ct_bytes = out_parts.get(ct_key).ok_or_else(|| err("content types lost"))?;
    w.add("[Content_Types].xml", ct_bytes, Compression::Deflated).map_err(err)?;
    for name in &parts.order {
        let lname = name.to_ascii_lowercase();
        if lname == ct_key {
            continue;
        }
        if let Some(b) = out_parts.get(&lname) {
            w.add(name, b, Compression::Deflated).map_err(err)?;
        }
    }
    let bytes = w.finish().map_err(err)?;
    Ok((bytes, new_format, actions))
}

fn rewrite_content_types(
    src: &str,
    dropped: &BTreeSet<String>,
    limits: XmlLimits,
) -> Result<(String, Option<(String, String)>), SanitizeError> {
    let mut converted = None;
    let out = rewrite(src, limits, |item, _| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        match xml::local_name(e) {
            "Override" => {
                let part = xml::attr(e, "PartName")
                    .map(|p| url::percent_decode(p.trim_start_matches('/')).to_ascii_lowercase())
                    .unwrap_or_default();
                if dropped.contains(&part) {
                    return Ok(Action::Drop);
                }
                let ct = xml::attr(e, "ContentType").unwrap_or_default();
                if let Some((_, to)) = CT_MAP.iter().find(|(from, _)| ct.eq_ignore_ascii_case(from))
                {
                    converted = Some((ct.clone(), (*to).to_owned()));
                    return Ok(Action::Replace(rebuild_tag(e, empty, |_, l, _| {
                        if l == "ContentType" {
                            AttrAction::Set((*to).to_owned())
                        } else {
                            AttrAction::Keep
                        }
                    })));
                }
                Ok(Action::Keep)
            }
            "Default" => {
                let ct = xml::attr(e, "ContentType").unwrap_or_default().to_ascii_lowercase();
                // Defaults that only ever describe macro projects.
                if ct == "application/vnd.ms-office.vbaproject"
                    || ct.contains("vbaprojectsignature")
                {
                    return Ok(Action::Drop);
                }
                Ok(Action::Keep)
            }
            _ => Ok(Action::Keep),
        }
    })
    .map_err(err)?;
    Ok((out, converted))
}

/// Generic part: drop elements / attributes pointing at removed relationships.
fn generic(src: &str, ids: &BTreeSet<String>, limits: XmlLimits) -> Result<String, SanitizeError> {
    rewrite(src, limits, |item, _| Ok(generic_action(item, ids))).map_err(err)
}

fn generic_action(item: &XmlItem<'_>, ids: &BTreeSet<String>) -> Action {
    let Some((e, empty)) = start_of(item) else { return Action::Keep };
    if refers(e, ids) {
        if DROP_IF_REFS.contains(&xml::local_name(e)) {
            return Action::Drop;
        }
        if let Some(t) = strip_attrs(e, empty, |p, l, v| {
            ids.contains(v) && ((p == Some("r") && REL_ATTRS.contains(&l)) || l == "relid")
        }) {
            return Action::Replace(t);
        }
    }
    Action::Keep
}

fn is_dangerous_field(instr: &str) -> bool {
    let args = fields::split_args(instr);
    let kw =
        args.first().map(|k| k.trim_start_matches('\\').to_ascii_uppercase()).unwrap_or_default();
    matches!(
        kw.as_str(),
        "DDE"
            | "DDEAUTO"
            | "INCLUDEPICTURE"
            | "INCLUDETEXT"
            | "IMPORT"
            | "INCLUDE"
            | "LINK"
            | "DATABASE"
            | "MACROBUTTON"
    ) || fields::decode_quote_codes(instr).is_some()
}

/// Word body: neutralise fields, accept revisions, drop comments, rsids,
/// OLE/ActiveX/altChunk anchors and dangling relationship references.
fn word_body(
    src: &str,
    ids: &BTreeSet<String>,
    opts: &SanitizeOptions,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    // Pass A0: collect field instructions in begin order. A nested
    // `QUOTE 68 68 69` contributes its decoded text to the enclosing
    // instruction, exactly like the X-ray does.
    let mut instrs: Vec<String> = Vec::new();
    let mut stack: Vec<(usize, bool)> = Vec::new();
    {
        let mut r = SecureXmlReader::new(src, limits).map_err(err)?;
        let mut in_instr_text = false;
        while let Some(item) = r.next_item().map_err(err)? {
            match &item.event {
                Event::Start(e) | Event::Empty(e) if xml::local_name(e) == "fldChar" => {
                    match xml::attr(e, "fldCharType").as_deref() {
                        Some("begin") => {
                            stack.push((instrs.len(), true));
                            instrs.push(String::new());
                        }
                        Some("separate") => {
                            if let Some(top) = stack.last_mut() {
                                top.1 = false;
                            }
                        }
                        Some("end") => {
                            if let Some((idx, _)) = stack.pop()
                                && let Some(decoded) =
                                    instrs.get(idx).and_then(|s| fields::decode_quote_codes(s))
                                && let Some(&(parent, true)) = stack.last()
                                && let Some(p) = instrs.get_mut(parent)
                            {
                                p.insert_str(0, &decoded);
                            }
                        }
                        _ => {}
                    }
                }
                Event::Start(e) if xml::local_name(e) == "instrText" => in_instr_text = true,
                Event::End(e) if e.local_name().as_ref() == "instrText" => in_instr_text = false,
                Event::Text(_) | Event::GeneralRef(_) | Event::CData(_) if in_instr_text => {
                    if let Some(s) = stack.last().and_then(|&(i, _)| instrs.get_mut(i))
                        && s.len() < 8192
                    {
                        xml::push_text(&item.event, s);
                    }
                }
                _ => {}
            }
        }
    }
    let dangerous: Vec<bool> = instrs.iter().map(|s| is_dangerous_field(s)).collect();
    for (s, d) in instrs.iter().zip(&dangerous) {
        if *d {
            act(ActionKind::NeutralizedField, s.trim());
        }
    }

    // Pass A: neutralise dangerous complex and simple fields.
    struct Frame {
        dangerous: bool,
        in_instr: bool,
    }
    let mut frames: Vec<Frame> = Vec::new();
    let mut counter = 0usize;
    let in_dangerous_instr = |frames: &[Frame]| frames.iter().any(|f| f.dangerous && f.in_instr);
    let mut simple_neutralized: Vec<String> = Vec::new();
    let pass_a = rewrite(src, limits, |item, _| {
        let Some((e, _)) = start_of(item) else { return Ok(Action::Keep) };
        match xml::local_name(e) {
            "fldChar" => {
                let outer = in_dangerous_instr(&frames);
                match xml::attr(e, "fldCharType").as_deref() {
                    Some("begin") => {
                        let d = dangerous.get(counter).copied().unwrap_or(false);
                        counter += 1;
                        frames.push(Frame { dangerous: d, in_instr: true });
                        Ok(if d || outer { Action::Drop } else { Action::Keep })
                    }
                    Some("separate") => {
                        let top_dangerous = frames.last().is_some_and(|f| f.dangerous);
                        if let Some(f) = frames.last_mut() {
                            f.in_instr = false;
                        }
                        let outer_after = in_dangerous_instr(&frames);
                        Ok(if top_dangerous || outer_after { Action::Drop } else { Action::Keep })
                    }
                    Some("end") => {
                        let f = frames.pop();
                        let outer_after = in_dangerous_instr(&frames);
                        Ok(if f.is_some_and(|f| f.dangerous) || outer_after {
                            Action::Drop
                        } else {
                            Action::Keep
                        })
                    }
                    _ => Ok(Action::Keep),
                }
            }
            // Everything inside a dangerous instruction goes, including the
            // visible results of nested (e.g. QUOTE) fields.
            "instrText" | "delInstrText" | "t" if in_dangerous_instr(&frames) => Ok(Action::Drop),
            "fldSimple" => match xml::attr(e, "instr") {
                Some(i) if is_dangerous_field(&i) => {
                    simple_neutralized.push(i);
                    Ok(Action::Unwrap)
                }
                _ => Ok(Action::Keep),
            },
            _ => Ok(Action::Keep),
        }
    })
    .map_err(err)?;
    for s in &simple_neutralized {
        act(ActionKind::NeutralizedField, s.trim());
    }

    // Pass B: revisions, comments, rsids, anchors, dangling references.
    let mut revisions = 0usize;
    let mut comments = 0usize;
    let mut removed: Vec<&'static str> = Vec::new();
    let out = rewrite(&pass_a, limits, |item, _ctx: &Context<'_>| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        if opts.accept_revisions {
            match local {
                "ins" | "moveTo" => {
                    revisions += 1;
                    return Ok(Action::Unwrap);
                }
                "del" | "moveFrom" | "rPrChange" | "pPrChange" | "sectPrChange" | "tblPrChange"
                | "tblGridChange" | "trPrChange" | "tcPrChange" | "tblPrExChange"
                | "numberingChange" | "moveFromRangeStart" | "moveFromRangeEnd"
                | "moveToRangeStart" | "moveToRangeEnd" | "cellIns" | "cellDel" | "cellMerge" => {
                    revisions += 1;
                    return Ok(Action::Drop);
                }
                l if l.starts_with("customXmlIns")
                    || l.starts_with("customXmlDel")
                    || l.starts_with("customXmlMove") =>
                {
                    return Ok(Action::Drop);
                }
                _ => {}
            }
        }
        if opts.remove_comments
            && matches!(
                local,
                "commentRangeStart" | "commentRangeEnd" | "commentReference" | "annotationRef"
            )
        {
            comments += 1;
            return Ok(Action::Drop);
        }
        match local {
            "control" => {
                removed.push("ActiveX control anchor");
                return Ok(Action::Drop);
            }
            "OLEObject" => {
                removed.push("OLE object (preview picture kept)");
                return Ok(Action::Drop);
            }
            "altChunk" => {
                removed.push("altChunk import");
                return Ok(Action::Drop);
            }
            "subDoc" => {
                removed.push("sub-document link");
                return Ok(Action::Drop);
            }
            _ => {}
        }
        if refers(e, ids) && DROP_IF_REFS.contains(&local) {
            return Ok(Action::Drop);
        }
        let strip = opts.strip_metadata;
        Ok(
            match strip_attrs(e, empty, |p, l, v| {
                (strip && l.starts_with("rsid"))
                    || (ids.contains(v)
                        && ((p == Some("r") && REL_ATTRS.contains(&l)) || l == "relid"))
            }) {
                Some(t) => Action::Replace(t),
                None => Action::Keep,
            },
        )
    })
    .map_err(err)?;
    if revisions > 0 {
        act(ActionKind::AcceptedRevisions, &format!("{revisions} revision mark(s)"));
    }
    if comments > 0 {
        act(ActionKind::RemovedComments, &format!("{comments} comment anchor(s)"));
    }
    for r in removed {
        act(ActionKind::RemovedElement, r);
    }
    Ok(out)
}

fn word_settings(
    src: &str,
    ids: &BTreeSet<String>,
    opts: &SanitizeOptions,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    let mut removed: Vec<String> = Vec::new();
    let out = rewrite(src, limits, |item, _| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        match local {
            "attachedTemplate" | "docVars" | "mailMerge" => {
                removed.push(local.to_owned());
                return Ok(Action::Drop);
            }
            "rsids" if opts.strip_metadata => {
                removed.push("rsids".to_owned());
                return Ok(Action::Drop);
            }
            _ => {}
        }
        if refers(e, ids) && DROP_IF_REFS.contains(&local) {
            return Ok(Action::Drop);
        }
        let strip = opts.strip_metadata;
        Ok(
            match strip_attrs(e, empty, |p, l, v| {
                (strip && l.starts_with("rsid"))
                    || (ids.contains(v) && p == Some("r") && REL_ATTRS.contains(&l))
            }) {
                Some(t) => Action::Replace(t),
                None => Action::Keep,
            },
        )
    })
    .map_err(err)?;
    for r in removed {
        act(
            if r == "rsids" { ActionKind::StrippedMetadata } else { ActionKind::RemovedElement },
            &format!("settings: {r}"),
        );
    }
    Ok(out)
}

fn is_auto_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let n = n.strip_prefix("_xlnm.").unwrap_or(&n);
    ["auto_open", "auto_close", "auto_activate", "auto_deactivate"].iter().any(|p| n.starts_with(p))
}

fn workbook(
    src: &str,
    ids: &BTreeSet<String>,
    opts: &SanitizeOptions,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    // Pass 1: names of removed sheets and texts of defined names.
    let mut removed_sheets: Vec<String> = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();
    {
        let mut r = SecureXmlReader::new(src, limits).map_err(err)?;
        let mut current: Option<(String, String)> = None;
        while let Some(item) = r.next_item().map_err(err)? {
            match &item.event {
                Event::Start(e) | Event::Empty(e)
                    if xml::local_name(e) == "sheet" && refers(e, ids) =>
                {
                    removed_sheets.push(xml::attr(e, "name").unwrap_or_default());
                }
                Event::Start(e) if xml::local_name(e) == "definedName" => {
                    current = Some((xml::attr(e, "name").unwrap_or_default(), String::new()))
                }
                Event::Empty(e) if xml::local_name(e) == "definedName" => {
                    names.push((xml::attr(e, "name").unwrap_or_default(), String::new()))
                }
                Event::End(e) if e.local_name().as_ref() == "definedName" => {
                    if let Some(c) = current.take() {
                        names.push(c);
                    }
                }
                Event::Text(_) | Event::GeneralRef(_) => {
                    if let Some((_, t)) = current.as_mut() {
                        xml::push_text(&item.event, t);
                    }
                }
                _ => {}
            }
        }
    }
    let drop_name: Vec<bool> = names
        .iter()
        .map(|(n, text)| {
            is_auto_name(n)
                || removed_sheets.iter().any(|s| {
                    !s.is_empty()
                        && (text.contains(&format!("{s}!")) || text.contains(&format!("'{s}'!")))
                })
                || text.contains('[')
        })
        .collect();
    let mut name_idx = 0usize;
    let mut removed: Vec<String> = Vec::new();
    let out = rewrite(src, limits, |item, _| {
        let Some((e, _)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        match local {
            "sheet" if refers(e, ids) => {
                removed.push(format!("sheet {:?}", xml::attr(e, "name").unwrap_or_default()));
                return Ok(Action::Drop);
            }
            "definedName" => {
                let d = drop_name.get(name_idx).copied().unwrap_or(false);
                name_idx += 1;
                if d {
                    removed.push(format!(
                        "defined name {:?}",
                        xml::attr(e, "name").unwrap_or_default()
                    ));
                    return Ok(Action::Drop);
                }
            }
            "externalReferences" => {
                removed.push("external workbook references".to_owned());
                return Ok(Action::Drop);
            }
            "fileSharing" | "absPath" if opts.strip_metadata => {
                removed.push(local.to_owned());
                return Ok(Action::Drop);
            }
            _ => {}
        }
        Ok(generic_action(item, ids))
    })
    .map_err(err)?;
    for r in removed {
        act(ActionKind::RemovedElement, &format!("workbook: {r}"));
    }
    Ok(out)
}

fn worksheet(
    src: &str,
    ids: &BTreeSet<String>,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    // Pass 1: which <f> elements are dangerous.
    let mut flags: Vec<(bool, String)> = Vec::new();
    {
        let mut r = SecureXmlReader::new(src, limits).map_err(err)?;
        let mut current: Option<String> = None;
        while let Some(item) = r.next_item().map_err(err)? {
            match &item.event {
                Event::Start(e) if xml::local_name(e) == "f" => current = Some(String::new()),
                Event::Empty(e) if xml::local_name(e) == "f" => flags.push((false, String::new())),
                Event::End(e) if e.local_name().as_ref() == "f" => {
                    if let Some(f) = current.take() {
                        flags.push((!formulas::assess(&f).is_empty(), f));
                    }
                }
                Event::Text(_) | Event::GeneralRef(_) | Event::CData(_) => {
                    if let Some(f) = current.as_mut()
                        && f.len() < 8192
                    {
                        xml::push_text(&item.event, f);
                    }
                }
                _ => {}
            }
        }
    }
    for (d, f) in &flags {
        if *d {
            act(ActionKind::RemovedFormula, &format!("={f}"));
        }
    }
    let mut idx = 0usize;
    let mut removed: Vec<&'static str> = Vec::new();
    let out = rewrite(src, limits, |item, _| {
        let Some((e, _)) = start_of(item) else { return Ok(Action::Keep) };
        match xml::local_name(e) {
            "f" => {
                let d = flags.get(idx).is_some_and(|(d, _)| *d);
                idx += 1;
                if d {
                    return Ok(Action::Drop);
                }
            }
            "oleObjects" => {
                removed.push("OLE objects");
                return Ok(Action::Drop);
            }
            "controls" => {
                removed.push("ActiveX controls");
                return Ok(Action::Drop);
            }
            _ => {}
        }
        Ok(generic_action(item, ids))
    })
    .map_err(err)?;
    for r in removed {
        act(ActionKind::RemovedElement, r);
    }
    Ok(out)
}

fn dangerous_action(v: &str) -> bool {
    let a = v.to_ascii_lowercase();
    a.starts_with("ppaction://program")
        || a.starts_with("ppaction://macro")
        || a.starts_with("ppaction://ole")
}

fn slide(
    src: &str,
    ids: &BTreeSet<String>,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    // Pass 1: graphic frames that host an OLE object.
    let mut frames: Vec<bool> = Vec::new();
    {
        let mut r = SecureXmlReader::new(src, limits).map_err(err)?;
        let mut open: Vec<usize> = Vec::new();
        while let Some(item) = r.next_item().map_err(err)? {
            match &item.event {
                Event::Start(e) if xml::local_name(e) == "graphicFrame" => {
                    open.push(frames.len());
                    frames.push(false);
                }
                Event::Empty(e) if xml::local_name(e) == "graphicFrame" => frames.push(false),
                Event::End(e) if e.local_name().as_ref() == "graphicFrame" => {
                    open.pop();
                }
                Event::Start(e) | Event::Empty(e) if xml::local_name(e) == "oleObj" => {
                    for &i in &open {
                        if let Some(f) = frames.get_mut(i) {
                            *f = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut idx = 0usize;
    let mut removed: Vec<String> = Vec::new();
    let out = rewrite(src, limits, |item, _| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        match local {
            "graphicFrame" => {
                let d = frames.get(idx).copied().unwrap_or(false);
                idx += 1;
                if d {
                    removed.push("OLE object frame".to_owned());
                    return Ok(Action::Drop);
                }
            }
            "controls" => {
                removed.push("ActiveX controls".to_owned());
                return Ok(Action::Drop);
            }
            "embeddedFontLst" => {
                removed.push("embedded fonts".to_owned());
                return Ok(Action::Drop);
            }
            "hlinkClick" | "hlinkHover" | "hlinkMouseOver"
                if xml::attr(e, "action").is_some_and(|a| dangerous_action(&a)) =>
            {
                removed.push(format!("action {}", xml::attr(e, "action").unwrap_or_default()));
                return Ok(Action::Drop);
            }
            _ => {}
        }
        if let Some(a) = xml::attr(e, "action")
            && dangerous_action(&a)
        {
            removed.push(format!("action {a}"));
            return Ok(Action::Replace(rebuild_tag(e, empty, |_, l, _| {
                if l == "action" { AttrAction::Drop } else { AttrAction::Keep }
            })));
        }
        Ok(generic_action(item, ids))
    })
    .map_err(err)?;
    for r in removed {
        act(ActionKind::RemovedElement, &r);
    }
    Ok(out)
}

fn visio(
    src: &str,
    ids: &BTreeSet<String>,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    const ACTIVE: &[&str] = &[
        "RUNADDON(",
        "RUNADDONWARGS(",
        "RUNMACRO(",
        "CALLTHIS(",
        "OPENFILE(",
        "OPENSHEETWIN(",
        "SHELLEXECUTE",
    ];
    let mut removed: Vec<String> = Vec::new();
    let out = rewrite(src, limits, |item, _| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        match xml::local_name(e) {
            "EventItem" => {
                removed.push("document event".to_owned());
                return Ok(Action::Drop);
            }
            "Cell" => {
                if let Some(f) = xml::attr(e, "F")
                    && ACTIVE.iter().any(|a| f.to_ascii_uppercase().contains(a))
                {
                    removed.push(format!("ShapeSheet formula {f}"));
                    return Ok(Action::Replace(rebuild_tag(e, empty, |_, l, _| {
                        if l == "F" { AttrAction::Drop } else { AttrAction::Keep }
                    })));
                }
            }
            _ => {}
        }
        Ok(generic_action(item, ids))
    })
    .map_err(err)?;
    for r in removed {
        act(ActionKind::RemovedElement, &r);
    }
    Ok(out)
}

fn core_props(
    src: &str,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    let mut removed: Vec<String> = Vec::new();
    let out = rewrite(src, limits, |item, ctx| {
        let Some((e, _)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        if ctx.ancestors.len() == 1
            && matches!(
                local,
                "creator" | "lastModifiedBy" | "lastPrinted" | "revision" | "created" | "modified"
            )
        {
            removed.push(local.to_owned());
            return Ok(Action::Drop);
        }
        Ok(Action::Keep)
    })
    .map_err(err)?;
    if !removed.is_empty() {
        act(ActionKind::StrippedMetadata, &format!("core properties: {}", removed.join(", ")));
    }
    Ok(out)
}

fn app_props(
    src: &str,
    limits: XmlLimits,
    act: &mut dyn FnMut(ActionKind, &str),
) -> Result<String, SanitizeError> {
    let mut removed: Vec<String> = Vec::new();
    let out = rewrite(src, limits, |item, ctx| {
        let Some((e, _)) = start_of(item) else { return Ok(Action::Keep) };
        let local = xml::local_name(e);
        if ctx.ancestors.len() == 1
            && matches!(
                local,
                "Company"
                    | "Manager"
                    | "TotalTime"
                    | "Template"
                    | "HyperlinkBase"
                    | "HLinks"
                    | "Application"
                    | "AppVersion"
            )
        {
            removed.push(local.to_owned());
            return Ok(Action::Drop);
        }
        Ok(Action::Keep)
    })
    .map_err(err)?;
    if !removed.is_empty() {
        act(ActionKind::StrippedMetadata, &format!("extended properties: {}", removed.join(", ")));
    }
    Ok(out)
}

fn vml(
    src: &str,
    ids: &BTreeSet<String>,
    opts: &SanitizeOptions,
    limits: XmlLimits,
) -> Result<String, SanitizeError> {
    rewrite(src, limits, |item, _| {
        let Some((e, empty)) = start_of(item) else { return Ok(Action::Keep) };
        if xml::local_name(e) == "OLEObject" {
            return Ok(Action::Drop);
        }
        if refers(e, ids) && DROP_IF_REFS.contains(&xml::local_name(e)) {
            return Ok(Action::Drop);
        }
        let strip = opts.strip_metadata;
        Ok(
            match strip_attrs(e, empty, |p, l, v| {
                (strip && l == "href" && p == Some("o"))
                    || (l == "src" && url::scheme(v).is_some())
                    || (ids.contains(v)
                        && ((p == Some("r") && REL_ATTRS.contains(&l)) || l == "relid"))
            }) {
                Some(t) => Action::Replace(t),
                None => Action::Keep,
            },
        )
    })
    .map_err(err)
}
