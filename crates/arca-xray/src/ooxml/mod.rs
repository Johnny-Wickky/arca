//! OOXML / OPC package scanner (Word, Excel, PowerPoint, Visio, XPS).

mod excel;
mod props;
mod role;
mod slides;
mod word;

use std::collections::BTreeMap;

use arca_container::xml;
use arca_container::zip::{ZipArchive, ZipEntry};
use quick_xml::events::BytesStart;

use crate::format::{FileFormat, opc_format_from_content_type};
use crate::report::{
    EmbeddedKind, EmbeddedObject, ExternalReference, MetaKey, RefKind, Severity, TargetClass,
    ThreatKind,
};
use crate::xmlwalk::{self, Ancestors, NoopVisitor, Visitor};
use crate::{Abort, Ctx, url};

/// One OPC relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rel {
    pub(crate) id: String,
    pub(crate) rtype: String,
    pub(crate) target: String,
    pub(crate) external: bool,
}

impl Rel {
    /// Last segment of the relationship type URI.
    pub(crate) fn type_suffix(&self) -> &str {
        self.rtype.rsplit('/').next().unwrap_or(&self.rtype)
    }
}

/// `[Content_Types].xml`.
#[derive(Debug, Default)]
pub(crate) struct ContentTypes {
    /// Lower-case extension → content type.
    pub(crate) defaults: BTreeMap<String, String>,
    /// Lower-case part name (no leading slash) → content type.
    pub(crate) overrides: BTreeMap<String, String>,
}

impl ContentTypes {
    pub(crate) fn of(&self, part: &str) -> String {
        let key = part.trim_start_matches('/').to_ascii_lowercase();
        if let Some(ct) = self.overrides.get(&key) {
            return ct.clone();
        }
        let ext =
            key.rsplit('/').next().and_then(|f| f.rsplit_once('.')).map(|(_, e)| e).unwrap_or("");
        self.defaults.get(ext).cloned().unwrap_or_default()
    }
}

/// Parsed package-level structure shared by the part scanners.
#[derive(Debug, Default)]
pub(crate) struct Package {
    pub(crate) content_types: ContentTypes,
    /// Source part (lowercase, no leading slash; `""` = package) → relationships.
    pub(crate) rels: BTreeMap<String, Vec<Rel>>,
    /// Part (lowercase) → the role the relationship type that links to it implies.
    rel_roles: BTreeMap<String, Role>,
    /// Parts that relationships of different types claim as different things: part → the
    /// role of the second claim.
    rel_conflicts: BTreeMap<String, Role>,
}

impl Package {
    /// Builds the package structure and derives the roles its relationships imply.
    pub(crate) fn new(content_types: ContentTypes, rels: BTreeMap<String, Vec<Rel>>) -> Self {
        let mut pkg = Self {
            content_types,
            rels,
            rel_roles: BTreeMap::new(),
            rel_conflicts: BTreeMap::new(),
        };
        let mut roles: BTreeMap<String, Role> = BTreeMap::new();
        let mut conflicts: BTreeMap<String, Role> = BTreeMap::new();
        for (source, rels) in &pkg.rels {
            for rel in rels {
                if reaches_outside(rel) {
                    continue;
                }
                let (Some(role), Some(part)) =
                    (by_relationship(rel.type_suffix()), resolve_target(source, &rel.target))
                else {
                    continue;
                };
                let part = part.to_ascii_lowercase();
                match roles.get(&part) {
                    Some(first) if *first != role => {
                        conflicts.insert(part, role);
                    }
                    Some(_) => {}
                    None => {
                        roles.insert(part, role);
                    }
                }
            }
        }
        pkg.rel_roles = roles;
        pkg.rel_conflicts = conflicts;
        pkg
    }

    pub(crate) fn rels_of(&self, part: &str) -> &[Rel] {
        self.rels.get(&part.to_ascii_lowercase()).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn rel(&self, part: &str, id: &str) -> Option<&Rel> {
        self.rels_of(part).iter().find(|r| r.id == id)
    }

    /// The role of the part `name`: content type, then relationships, then name.
    pub(crate) fn classify(&self, name: &str) -> Classified {
        let lower = name.to_ascii_lowercase();
        let ct = self.content_types.of(name);
        let mut c = classify(name, &ct, self.rel_roles.get(&lower).copied());
        if c.conflict.is_none() {
            c.conflict = self.rel_conflicts.get(&lower).copied();
        }
        c
    }
}

#[cfg(test)]
pub(crate) use role::role;
pub(crate) use role::{Classified, Role, by_relationship, classify, is_inert_media};
pub(crate) use slides::visio_active_functions;

/// Maps `dir/_rels/name.rels` to its source part (`""` for the package rels).
pub(crate) fn rels_source(rels_path: &str) -> Option<String> {
    let p = rels_path.trim_start_matches('/');
    let (dir, file) = match p.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => return None,
    };
    let file_stem = file.strip_suffix(".rels").or_else(|| file.strip_suffix(".RELS"))?;
    let parent = if dir.eq_ignore_ascii_case("_rels") {
        ""
    } else {
        dir.strip_suffix("/_rels").or_else(|| dir.strip_suffix("/_RELS"))?
    };
    if parent.is_empty() && file_stem.is_empty() {
        return Some(String::new());
    }
    Some(if parent.is_empty() { file_stem.to_owned() } else { format!("{parent}/{file_stem}") })
}

/// Resolves an internal relationship target against its source part.
/// Returns `None` when the target escapes the package root.
pub(crate) fn resolve_target(source_part: &str, target: &str) -> Option<String> {
    let target = url::percent_decode(target.split('#').next().unwrap_or(target));
    let mut parts: Vec<&str> = Vec::new();
    if !target.starts_with('/')
        && let Some((dir, _)) = source_part.rsplit_once('/')
    {
        parts.extend(dir.split('/').filter(|s| !s.is_empty()));
    }
    for comp in target.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// `true` if the relationship leaves the package.
///
/// A relationship reaches outside the package if it says so *or* if its target does:
/// `TargetMode="External"` is an attribute the author controls, and `//host/share/x.dotm` (an
/// RFC 3986 network-path reference), `\\host\x`, `C:\x` or `scheme:` targets are not
/// package-internal whatever it says. The sanitizer removes exactly the relationships this
/// reports.
pub(crate) fn reaches_outside(rel: &Rel) -> bool {
    rel.external || url::leaves_package(&rel.target)
}

/// Scans an OPC package.
pub(crate) fn scan(
    ctx: &mut Ctx<'_>,
    zip: &ZipArchive<'_>,
    location: &str,
) -> Result<FileFormat, Abort> {
    // ---- [Content_Types].xml ----------------------------------------------
    let mut content_types = ContentTypes::default();
    let ct_loc = Ctx::join(location, "[Content_Types].xml");
    if let Some(entry) = zip.entry("[Content_Types].xml")
        && let Some(bytes) = ctx.read_member(zip, entry, &ct_loc)?
    {
        let mut v = ContentTypesVisitor { ct: &mut content_types, conflicts: 0 };
        xmlwalk::walk(ctx, &bytes, &ct_loc, &mut v)?;
        if v.conflicts > 0 {
            ctx.report.add(
                ThreatKind::ParserDifferential,
                &ct_loc,
                format!(
                    "{} part(s) or extension(s) are given more than one content type; readers that keep the first and readers that keep the last disagree",
                    v.conflicts
                ),
            );
        }
    }

    // ---- all relationships (needed before parts are interpreted) ----------
    let mut rels: BTreeMap<String, Vec<Rel>> = BTreeMap::new();
    for entry in zip.entries() {
        if !entry.name().to_ascii_lowercase().ends_with(".rels") || entry.is_dir() {
            continue;
        }
        let loc = Ctx::join(location, entry.name());
        let Some(source) = rels_source(entry.name()) else {
            ctx.report.add(
                ThreatKind::MalformedContainer,
                &loc,
                "relationship part outside a _rels folder",
            );
            continue;
        };
        let Some(bytes) = ctx.read_member(zip, entry, &loc)? else { continue };
        let mut v = RelsVisitor { rels: Vec::new() };
        xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
        if rels.insert(source.to_ascii_lowercase(), v.rels).is_some() {
            ctx.report.add(
                ThreatKind::ParserDifferential,
                &loc,
                "two relationship parts describe the same source part (names differing only in case)",
            );
        }
    }
    let pkg = Package::new(content_types, rels);

    // ---- main part & format ------------------------------------------------
    let main_part = pkg
        .rels_of("")
        .iter()
        .find(|r| {
            !r.external
                && matches!(r.type_suffix(), "officeDocument" | "document" | "FixedRepresentation")
        })
        .and_then(|r| resolve_target("", &r.target));
    let format = match &main_part {
        Some(part) => opc_format_from_content_type(&pkg.content_types.of(part)),
        None => FileFormat::OpcOther,
    };
    ctx.identified(format);
    if format == FileFormat::OpcOther {
        // Nothing says what kind of document this package is, so nothing says how its parts
        // are to be read.
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            &Ctx::join(location, main_part.as_deref().unwrap_or("")),
            "the package has no main document part of a recognised type; its content model was not interpreted",
        );
    }
    if format.is_macro_enabled_type() {
        ctx.report.add(
            ThreatKind::MacroEnabledContentType,
            &Ctx::join(location, main_part.as_deref().unwrap_or("")),
            format!("{format:?} main part content type"),
        );
    }

    // ---- external relationships --------------------------------------------
    for (source, rels) in &pkg.rels {
        for rel in rels {
            let loc = Ctx::join(location, if source.is_empty() { "_rels/.rels" } else { source });
            inspect_relationship(ctx, rel, source, &loc, &|part| zip.entry(part).is_some());
        }
    }

    // ---- parts -------------------------------------------------------------
    let mut custom_xml = 0usize;
    let mut has_vba = false;
    for entry in zip.entries() {
        if entry.is_dir() {
            continue;
        }
        let name = entry.name();
        let loc = Ctx::join(location, name);
        let ct = pkg.content_types.of(name);
        ctx.report.stats.parts_scanned = ctx.report.stats.parts_scanned.saturating_add(1);
        let classified = pkg.classify(name);
        let mut role = classified.role;
        let is_rels_name = name.to_ascii_lowercase().ends_with(".rels");
        if role == Role::Rels && !is_rels_name {
            // Declared as relationships, but not where relationships live: nothing parsed it,
            // so it is read as the XML it is.
            role = Role::OtherXml;
            ctx.report.add(
                ThreatKind::ContentTypeMismatch,
                &loc,
                "the part is declared as relationships but is not named like a relationships part; it was read as plain XML",
            );
        }
        if let Some(other) = classified.conflict {
            ctx.report.add(
                ThreatKind::ContentTypeMismatch,
                &loc,
                format!(
                    "the content type, the relationships and the name of this part disagree about what it is ({role:?} / {other:?}); it was scanned as {role:?}"
                ),
            );
        } else if ct.is_empty() && role != Role::Rels {
            ctx.report.add(
                ThreatKind::ContentTypeMismatch,
                &loc,
                "the part has no content type in [Content_Types].xml",
            );
        }
        match role {
            Role::Rels => {}
            Role::VbaProject => {
                has_vba = true;
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    crate::ole::scan_vba_container(ctx, &bytes, &loc)?;
                }
            }
            Role::VbaSignature => ctx.report.add(ThreatKind::SignedMacroProject, &loc, "the macro project carries a digital signature (signature is not trusted by Arca)"),
            Role::VbaData => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    let mut v = VbaDataVisitor::default();
                    xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
                    if !v.macros.is_empty() {
                        ctx.report.add(ThreatKind::VbaMacros, &loc, format!("macro bindings: {}", v.macros.join(", ")));
                    }
                }
            }
            Role::ActiveX => scan_activex(ctx, zip, entry, &loc, &ct)?,
            Role::Embedding => scan_embedding(ctx, zip, entry, &loc)?,
            Role::RibbonUi => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    let mut v = RibbonVisitor::default();
                    xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
                    if !v.callbacks.is_empty() {
                        ctx.report.add(ThreatKind::RibbonCallbacks, &loc, v.callbacks.join(", "));
                    }
                }
            }
            Role::MacroSheet => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    excel::scan_macrosheet(ctx, &bytes, &loc)?;
                }
            }
            Role::ExternalLink => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    excel::scan_external_link(ctx, &bytes, &loc)?;
                }
            }
            Role::Connections => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    excel::scan_connections(ctx, &bytes, &loc)?;
                }
            }
            Role::PrinterSettings => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::printer_settings(ctx, &bytes, &loc);
                }
            }
            Role::CoreProps => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::core(ctx, &bytes, &loc)?;
                }
            }
            Role::AppProps => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::app(ctx, &bytes, &loc)?;
                }
            }
            Role::CustomProps => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::custom(ctx, &bytes, &loc)?;
                }
            }
            Role::Labels => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::labels(ctx, &bytes, &loc)?;
                }
            }
            Role::CustomXml => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    if name.to_ascii_lowercase().ends_with(".xml") {
                        custom_xml += 1;
                        xmlwalk::walk(ctx, &bytes, &loc, &mut NoopVisitor)?;
                    } else {
                        scan_binary_part(ctx, &bytes, &loc, &ct)?;
                    }
                }
            }
            Role::Font => ctx.report.add(ThreatKind::EmbeddedFont, &loc, "embedded font is never used for rendering (font parsers are a classic exploit target)"),
            Role::People => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::people(ctx, &bytes, &loc)?;
                }
            }
            Role::Comments => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    props::comments(ctx, &bytes, &loc)?;
                    // A comment is a document of its own: fields, objects and links in it are
                    // read like those in the body.
                    word::scan_body(ctx, &pkg, name, &bytes, &loc)?;
                }
            }
            Role::Thumbnail => {
                ctx.report.add(ThreatKind::StaleThumbnail, &loc, "a preview image is stored and may show an older state of the document");
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    scan_binary_part(ctx, &bytes, &loc, &ct)?;
                }
            }
            Role::WordBody => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    word::scan_body(ctx, &pkg, name, &bytes, &loc)?;
                }
            }
            Role::WordSettings => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    word::scan_settings(ctx, &bytes, &loc)?;
                }
            }
            Role::Workbook => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    excel::scan_workbook(ctx, &pkg, name, &bytes, &loc)?;
                }
            }
            Role::Worksheet => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    excel::scan_sheet(ctx, &bytes, &loc)?;
                }
            }
            Role::Slide => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    slides::scan_slide(ctx, &bytes, &loc)?;
                }
            }
            Role::Presentation => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    slides::scan_presentation(ctx, &bytes, &loc)?;
                }
            }
            Role::Visio => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    slides::scan_visio(ctx, &bytes, &loc)?;
                }
            }
            Role::Vml => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    word::scan_vml(ctx, &bytes, &loc)?;
                }
            }
            Role::Svg => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    scan_svg(ctx, &bytes, &loc)?;
                }
            }
            Role::WebExtension => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    scan_web_extension(ctx, &bytes, &loc)?;
                }
            }
            Role::OtherXml => {
                // Drawings, diagrams and charts carry the same action hyperlinks and objects
                // as slides do.
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    slides::scan_slide(ctx, &bytes, &loc)?;
                }
            }
            Role::Binary => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    scan_binary_part(ctx, &bytes, &loc, &ct)?;
                }
            }
        }
    }
    if custom_xml > 0 {
        ctx.report.add(
            ThreatKind::CustomXmlData,
            &Ctx::join(location, "customXml"),
            format!("{custom_xml} custom XML data part(s)"),
        );
    }
    if format.is_macro_enabled_type() && !has_vba && !ctx.report.has(ThreatKind::XlmMacroSheet) {
        ctx.report.add_with(
            ThreatKind::ContentTypeMismatch,
            Severity::Low,
            location,
            "macro-enabled package without a macro project",
        );
    }
    if has_vba && !format.is_macro_enabled_type() {
        ctx.report.add(
            ThreatKind::ContentTypeMismatch,
            location,
            "macro project inside a package declared as macro-free",
        );
    }
    Ok(format)
}

/// Scans a part that has no XML structure to walk: what its bytes are decides.
///
/// Containers and documents are scanned recursively, pictures and text by their own scanners,
/// executables are reported. Bytes of a kind the scanner does not recognise are acceptable only
/// when the part's declared media type says they are plain media (fonts, metafiles, audio); in
/// any other case they were not inspected, which is a coverage gap rather than a pass.
pub(crate) fn scan_binary_part(
    ctx: &mut Ctx<'_>,
    bytes: &[u8],
    loc: &str,
    content_type: &str,
) -> Result<(), Abort> {
    use crate::format::Magic;
    match crate::format::sniff(bytes) {
        Magic::Empty => {}
        Magic::Executable(what) => {
            ctx.report.add(
                ThreatKind::EmbeddedExecutable,
                loc,
                format!("{what} stored as a package part"),
            );
        }
        Magic::Zip | Magic::Ole2 | Magic::Pdf | Magic::Rtf | Magic::Jet(_) => {
            let nested = ctx.scan_nested(bytes, loc)?;
            ctx.report.add(
                ThreatKind::EmbeddedDocument,
                loc,
                format!("embedded {nested:?} document (scanned recursively)"),
            );
        }
        Magic::Image(_) | Magic::Text => {
            ctx.scan_nested(bytes, loc)?;
        }
        Magic::Unknown => {
            if !is_inert_media(content_type) {
                ctx.report.add(
                    ThreatKind::ScanLimitReached,
                    loc,
                    format!(
                        "binary part of an unrecognised kind (content type {content_type:?}); it was not inspected"
                    ),
                );
            }
        }
    }
    Ok(())
}

/// Classifies one relationship and records external references.
///
/// `has_part` tells whether the package contains a part of the given (resolved) name.
pub(crate) fn inspect_relationship(
    ctx: &mut Ctx<'_>,
    rel: &Rel,
    source: &str,
    loc: &str,
    has_part: &dyn Fn(&str) -> bool,
) {
    let suffix = rel.type_suffix();
    if !reaches_outside(rel) {
        // Internal target: must stay inside the package.
        match resolve_target(source, &rel.target) {
            None => ctx.report.add_with(
                ThreatKind::MalformedContainer,
                Severity::Medium,
                loc,
                format!("relationship {} target {:?} escapes the package root", rel.id, rel.target),
            ),
            Some(part) => {
                if suffix == "aFChunk" && has_part(&part) {
                    ctx.report.add(
                        ThreatKind::AltChunkImport,
                        loc,
                        format!(
                            "altChunk imports {part} (HTML/RTF/MHT content merged at open time)"
                        ),
                    );
                }
            }
        }
        return;
    }
    let (kind, auto_load) = match suffix {
        "attachedTemplate" => (RefKind::Template, true),
        "image" => (RefKind::Image, true),
        "frame" | "subDocument" => (RefKind::Frame, true),
        "oleObject" | "package" => (RefKind::ObjectLink, true),
        "externalLinkPath" | "externalLink" | "xlExternalLinkPath" => (RefKind::WorkbookLink, true),
        "hyperlink" => (RefKind::Hyperlink, false),
        // Linked media are fetched when the slide show reaches them, and may start by
        // themselves.
        "video" | "audio" | "media" => (RefKind::Other, true),
        "aFChunk" => (RefKind::Frame, true),
        _ => (RefKind::Other, true),
    };
    record_external(ctx, &rel.target, kind, auto_load, loc);
}

/// Records an external reference and the matching finding(s).
pub(crate) fn record_external(
    ctx: &mut Ctx<'_>,
    target: &str,
    kind: RefKind,
    auto_load: bool,
    loc: &str,
) {
    if target.trim().is_empty() {
        return;
    }
    let class = url::classify(target);
    ctx.report.external(ExternalReference {
        target: arca_container::text::display_safe(target, 512),
        kind,
        class,
        location: loc.to_owned(),
        auto_load,
    });
    let detail = target.to_owned();
    if class == TargetClass::DangerousProtocol {
        ctx.report.add(ThreatKind::DangerousProtocolUri, loc, detail.clone());
    }
    if class == TargetClass::Unc {
        ctx.report.add(
            ThreatKind::UncPathReference,
            loc,
            format!(
                "{detail} (Windows would send NTLM credentials to {})",
                url::unc_host(target).unwrap_or_default()
            ),
        );
        if let Some(host) = url::unc_host(target) {
            ctx.report.meta(MetaKey::Machine, "", &host, loc);
        }
    }
    if class == TargetClass::LocalFile {
        ctx.report.add(ThreatKind::LocalPathLeak, loc, detail.clone());
        ctx.report.meta(MetaKey::LocalPath, "", target, loc);
        if let Some(user) = url::profile_user(target) {
            ctx.report.meta(MetaKey::Person, "Windows profile", &user, loc);
        }
    }
    let network = url::is_network(class);
    match kind {
        RefKind::Template if network => ctx.report.add(ThreatKind::RemoteTemplate, loc, detail),
        RefKind::Image if network && auto_load => {
            ctx.report.add(ThreatKind::ExternalImage, loc, detail)
        }
        RefKind::Frame if network => ctx.report.add(ThreatKind::ExternalFrame, loc, detail),
        RefKind::ObjectLink if network => {
            ctx.report.add(ThreatKind::ExternalObjectLink, loc, detail)
        }
        RefKind::ObjectLink => {
            ctx.report.add_with(ThreatKind::ExternalObjectLink, Severity::Medium, loc, detail)
        }
        RefKind::WorkbookLink if network => {
            ctx.report.add(ThreatKind::ExternalWorkbookLink, loc, detail)
        }
        RefKind::WorkbookLink => {
            ctx.report.add_with(ThreatKind::ExternalWorkbookLink, Severity::Low, loc, detail)
        }
        RefKind::DataConnection => ctx.report.add(ThreatKind::ExternalDataConnection, loc, detail),
        RefKind::Field if network => ctx.report.add(ThreatKind::RemoteFieldInclude, loc, detail),
        RefKind::Hyperlink if class == TargetClass::Web || class == TargetClass::Mail => {
            ctx.report.add(ThreatKind::ExternalHyperlink, loc, detail);
        }
        _ if network
            && auto_load
            && class != TargetClass::DangerousProtocol
            && class != TargetClass::Unc =>
        {
            ctx.report.add(ThreatKind::ExternalResource, loc, detail);
        }
        _ => {}
    }
}

fn scan_activex(
    ctx: &mut Ctx<'_>,
    zip: &ZipArchive<'_>,
    entry: &ZipEntry,
    loc: &str,
    content_type: &str,
) -> Result<(), Abort> {
    let Some(bytes) = ctx.read_member(zip, entry, loc)? else { return Ok(()) };
    if entry.name().to_ascii_lowercase().ends_with(".xml") {
        let mut v = ActiveXVisitor::default();
        xmlwalk::walk(ctx, &bytes, loc, &mut v)?;
        let clsid = v.classid.unwrap_or_default();
        let known = crate::ole::clsid_info(&clsid);
        let detail = match known {
            Some((desc, _, _)) => format!("ActiveX control {clsid} ({desc})"),
            None => format!("ActiveX control {clsid}"),
        };
        match known {
            Some((_, kind, sev)) => ctx.report.add_with(kind, sev, loc, detail),
            None => ctx.report.add(ThreatKind::ActiveXControl, loc, detail),
        }
        ctx.report.embedded.push(EmbeddedObject {
            location: loc.to_owned(),
            kind: EmbeddedKind::ActiveX,
            class_name: String::new(),
            clsid,
            file_name: String::new(),
            size: bytes.len() as u64,
            sha256: arca_digest::sha256(&bytes).to_string(),
        });
    } else {
        scan_binary_part(ctx, &bytes, loc, content_type)?;
    }
    Ok(())
}

fn scan_embedding(
    ctx: &mut Ctx<'_>,
    zip: &ZipArchive<'_>,
    entry: &ZipEntry,
    loc: &str,
) -> Result<(), Abort> {
    let Some(bytes) = ctx.read_member(zip, entry, loc)? else { return Ok(()) };
    let sha = arca_digest::sha256(&bytes).to_string();
    let magic = crate::format::sniff(&bytes);
    let kind = match magic {
        crate::format::Magic::Executable(_) => EmbeddedKind::Executable,
        crate::format::Magic::Ole2 => EmbeddedKind::OleObject,
        crate::format::Magic::Zip
        | crate::format::Magic::Pdf
        | crate::format::Magic::Rtf
        | crate::format::Magic::Jet(_) => EmbeddedKind::Document,
        _ => EmbeddedKind::Other,
    };
    let before = ctx.report.embedded.len();
    let nested = ctx.scan_nested(&bytes, loc)?;
    if nested == FileFormat::Unknown && matches!(magic, crate::format::Magic::Unknown) {
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            loc,
            "the embedded object is of a kind the scanner cannot read; it was not inspected",
        );
    }
    if kind == EmbeddedKind::Document || nested.family() != crate::format::Family::None {
        ctx.report.add(
            ThreatKind::EmbeddedDocument,
            loc,
            format!("embedded {nested:?} document (scanned recursively)"),
        );
    } else if kind == EmbeddedKind::OleObject && !ctx.report.has(ThreatKind::OlePackage) {
        ctx.report.add(ThreatKind::OleObject, loc, "embedded OLE object (never activated)");
    }
    // Keep the object inventory entry first for this location.
    ctx.report.embedded.insert(
        before.min(ctx.report.embedded.len()),
        EmbeddedObject {
            location: loc.to_owned(),
            kind,
            class_name: String::new(),
            clsid: String::new(),
            file_name: String::new(),
            size: bytes.len() as u64,
            sha256: sha,
        },
    );
    Ok(())
}

struct ContentTypesVisitor<'a> {
    ct: &'a mut ContentTypes,
    /// Parts or extensions declared more than once with different content types.
    conflicts: usize,
}

impl Visitor for ContentTypesVisitor<'_> {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "Default" => {
                if let (Some(ext), Some(ct)) =
                    (plain_attr(e, "Extension"), plain_attr(e, "ContentType"))
                    && let Some(old) = self.ct.defaults.insert(ext.to_ascii_lowercase(), ct.clone())
                {
                    self.conflicts += usize::from(old != ct);
                }
            }
            "Override" => {
                if let (Some(part), Some(ct)) =
                    (plain_attr(e, "PartName"), plain_attr(e, "ContentType"))
                    && let Some(old) = self.ct.overrides.insert(
                        url::percent_decode(part.trim_start_matches('/')).to_ascii_lowercase(),
                        ct.clone(),
                    )
                {
                    self.conflicts += usize::from(old != ct);
                }
            }
            _ => {}
        }
    }
}

/// The value of an attribute the specification defines without a prefix (`Id`, `Target`,
/// `ContentType`). If a same-named attribute with another prefix is present as well, the
/// unprefixed one wins: it is the one a conforming reader looks at. The collision itself is
/// reported by the XML walker.
pub(crate) fn plain_attr(e: &BytesStart<'_>, local: &str) -> Option<String> {
    match xml::attr_ns(e, None, local) {
        xml::AttrLookup::Found(v) | xml::AttrLookup::OtherPrefix(v) => Some(v),
        xml::AttrLookup::Collision { preferred } => preferred.or_else(|| xml::attr(e, local)),
        xml::AttrLookup::Absent => None,
    }
}

struct RelsVisitor {
    rels: Vec<Rel>,
}

impl Visitor for RelsVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if xml::local_name(e) == "Relationship" {
            // Every attribute that could be taken for `TargetMode` counts: a reader may look at
            // either, and "external" is the safe answer.
            let external = xml::attrs(e).iter().any(|(k, v)| {
                xml::split_qname(k).1 == "TargetMode" && v.eq_ignore_ascii_case("External")
            });
            self.rels.push(Rel {
                id: plain_attr(e, "Id").unwrap_or_default(),
                rtype: plain_attr(e, "Type").unwrap_or_default(),
                target: plain_attr(e, "Target").unwrap_or_default(),
                external,
            });
        }
    }
}

#[derive(Default)]
struct VbaDataVisitor {
    macros: Vec<String>,
}

impl Visitor for VbaDataVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if let Some(m) = xml::attr(e, "macroName")
            && !self.macros.contains(&m)
            && self.macros.len() < 64
        {
            self.macros.push(m);
        }
    }
}

#[derive(Default)]
struct RibbonVisitor {
    callbacks: Vec<String>,
}

impl Visitor for RibbonVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        for (k, v) in xml::attrs(e) {
            let local = xml::split_qname(&k).1;
            let is_cb = local == "onLoad"
                || local == "loadImage"
                || local.starts_with("on")
                || local.starts_with("get");
            if is_cb && !v.is_empty() && self.callbacks.len() < 64 {
                let s = format!("{local}={v}");
                if !self.callbacks.contains(&s) {
                    self.callbacks.push(s);
                }
            }
        }
    }
}

#[derive(Default)]
struct ActiveXVisitor {
    classid: Option<String>,
}

impl Visitor for ActiveXVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if self.classid.is_none()
            && let Some(c) = xml::attr(e, "classid")
        {
            self.classid = Some(c.to_ascii_uppercase());
        }
    }
}

struct SvgVisitor {
    location: String,
}

impl Visitor for SvgVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        let local = xml::local_name(e).to_ascii_lowercase();
        if matches!(local.as_str(), "script" | "foreignobject" | "iframe" | "embed" | "object") {
            ctx.report.add(
                ThreatKind::HtmlActiveContent,
                &self.location,
                format!("<{local}> inside SVG image"),
            );
        }
        for (k, v) in xml::attrs(e) {
            let lk = xml::split_qname(&k).1.to_ascii_lowercase();
            if lk.starts_with("on") {
                ctx.report.add(
                    ThreatKind::HtmlActiveContent,
                    &self.location,
                    format!("event handler {lk} in SVG"),
                );
            }
            if (lk == "href" || lk == "src") && url::leaves_package(&v) {
                record_external(ctx, &v, RefKind::Image, true, &self.location);
            }
            // Animation elements can set an `href` to a script URL (`<set attributeName="href"
            // to="javascript:...">`), so every attribute value is looked at.
            let lv = v.to_ascii_lowercase();
            if lv.contains("javascript:") || lv.contains("vbscript:") {
                ctx.report.add(
                    ThreatKind::HtmlActiveContent,
                    &self.location,
                    format!("script URL in the {lk} attribute of an SVG element"),
                );
            }
        }
    }

    fn text(&mut self, ctx: &mut Ctx<'_>, text: &str, anc: &Ancestors) {
        if anc.last().is_some_and(|a| a.eq_ignore_ascii_case("style")) {
            for u in url::find_urls(text, 8) {
                record_external(ctx, &u, RefKind::Image, true, &self.location);
            }
        }
    }
}

/// Scans an SVG picture (a package part, or a document of its own).
pub(crate) fn scan_svg(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = SvgVisitor { location: loc.to_owned() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}

/// Collects what an Office add-in (web extension) definition says.
#[derive(Default)]
struct WebExtensionVisitor {
    references: Vec<String>,
    stores: Vec<String>,
    auto_show: bool,
}

impl Visitor for WebExtensionVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "reference" | "alternateReference" => {
                let id = xml::attr(e, "id").unwrap_or_default();
                let store = xml::attr(e, "store").unwrap_or_default();
                let kind = xml::attr(e, "storeType").unwrap_or_default();
                if self.references.len() < 16 {
                    self.references.push(format!("{id} ({kind} {store})"));
                }
                if !store.is_empty() && self.stores.len() < 16 {
                    self.stores.push(store);
                }
            }
            "property"
                if xml::attr(e, "name")
                    .is_some_and(|n| n == "Office.AutoShowTaskpaneWithDocument")
                    && xml::attr(e, "value").is_some_and(|v| v.contains("true")) =>
            {
                self.auto_show = true;
            }
            _ => {}
        }
    }
}

/// Office add-ins are web pages (with script) that Office loads into a task pane, optionally
/// the moment the document opens. A definition of one, or the list of open task panes, is
/// executable content however benign the page it names.
fn scan_web_extension(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = WebExtensionVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    for store in &v.stores {
        if url::leaves_package(store) {
            record_external(ctx, store, RefKind::Frame, true, loc);
        }
    }
    let auto = if v.auto_show { "; opens automatically with the document" } else { "" };
    ctx.report.add(
        ThreatKind::EmbeddedScript,
        loc,
        format!("Office add-in (web extension): {}{auto}", v.references.join(", ")),
    );
    Ok(())
}

// ---- scanners of the parts of a flat (single-file) package -------------------------------------
//
// The element names the part scanners look for do not depend on where the part sits, so a flat
// OPC document is scanned by running them over the whole file.

/// Scans WordprocessingML text that is not in a package.
pub(crate) fn scan_word_body(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    word::scan_body(ctx, &Package::default(), "", bytes, loc)?;
    word::scan_settings(ctx, bytes, loc)
}

/// Scans SpreadsheetML workbook text that is not in a package.
pub(crate) fn scan_workbook_xml(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    excel::scan_workbook(ctx, &Package::default(), "", bytes, loc)
}

/// Scans SpreadsheetML sheet text that is not in a package.
pub(crate) fn scan_sheet_xml(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    excel::scan_sheet(ctx, bytes, loc)
}

/// Scans PresentationML (and DrawingML) text that is not in a package.
pub(crate) fn scan_slide_xml(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    slides::scan_slide(ctx, bytes, loc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rels_paths() {
        assert_eq!(rels_source("_rels/.rels").as_deref(), Some(""));
        assert_eq!(
            rels_source("word/_rels/document.xml.rels").as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(
            rels_source("xl/worksheets/_rels/sheet1.xml.rels").as_deref(),
            Some("xl/worksheets/sheet1.xml")
        );
        assert_eq!(rels_source("word/document.xml.rels"), None);
    }

    #[test]
    fn target_resolution() {
        assert_eq!(
            resolve_target("word/document.xml", "media/image1.png").as_deref(),
            Some("word/media/image1.png")
        );
        assert_eq!(
            resolve_target("xl/worksheets/sheet1.xml", "../drawings/drawing1.xml").as_deref(),
            Some("xl/drawings/drawing1.xml")
        );
        assert_eq!(resolve_target("", "word/document.xml").as_deref(), Some("word/document.xml"));
        assert_eq!(
            resolve_target("word/document.xml", "/customXml/item1.xml").as_deref(),
            Some("customXml/item1.xml")
        );
        assert_eq!(resolve_target("word/document.xml", "../../../etc/passwd"), None);
        assert_eq!(
            resolve_target("word/document.xml", "media/a%20b.png").as_deref(),
            Some("word/media/a b.png")
        );
    }

    #[test]
    fn roles() {
        assert_eq!(role("word/vbaProject.bin", ""), Role::VbaProject);
        assert_eq!(
            role("xl/macrosheets/sheet1.xml", "application/vnd.ms-excel.macrosheet+xml"),
            Role::MacroSheet
        );
        assert_eq!(
            role(
                "word/document.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            ),
            Role::WordBody
        );
        assert_eq!(
            role(
                "word/embeddings/oleObject1.bin",
                "application/vnd.openxmlformats-officedocument.oleObject"
            ),
            Role::Embedding
        );
        assert_eq!(
            role("docProps/core.xml", "application/vnd.openxmlformats-package.core-properties+xml"),
            Role::CoreProps
        );
        assert_eq!(role("word/media/image1.png", "image/png"), Role::Binary);
    }
}
