//! OOXML / OPC package scanner (Word, Excel, PowerPoint, Visio, XPS).

mod excel;
mod props;
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
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
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
}

impl Package {
    pub(crate) fn rels_of(&self, part: &str) -> &[Rel] {
        self.rels.get(&part.to_ascii_lowercase()).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn rel(&self, part: &str, id: &str) -> Option<&Rel> {
        self.rels_of(part).iter().find(|r| r.id == id)
    }
}

/// Role of a part, derived from its content type and, as a fallback, its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Rels,
    VbaProject,
    VbaSignature,
    VbaData,
    ActiveX,
    Embedding,
    RibbonUi,
    MacroSheet,
    ExternalLink,
    Connections,
    PrinterSettings,
    CoreProps,
    AppProps,
    CustomProps,
    CustomXml,
    Font,
    People,
    Comments,
    Thumbnail,
    WordBody,
    WordSettings,
    Workbook,
    Worksheet,
    Slide,
    Presentation,
    Visio,
    Vml,
    Svg,
    Labels,
    OtherXml,
    Binary,
}

pub(crate) fn role(name: &str, ct: &str) -> Role {
    let n = name.to_ascii_lowercase();
    let file = n.rsplit('/').next().unwrap_or(&n);
    let ct = ct.to_ascii_lowercase();
    if n.ends_with(".rels") {
        return Role::Rels;
    }
    if ct == "application/vnd.ms-office.vbaproject" || file == "vbaproject.bin" {
        return Role::VbaProject;
    }
    if ct.starts_with("application/vnd.ms-office.vbaprojectsignature")
        || file.starts_with("vbaprojectsignature")
    {
        return Role::VbaSignature;
    }
    if file == "vbadata.xml" || ct == "application/vnd.ms-word.vbadata+xml" {
        return Role::VbaData;
    }
    if ct.contains("activex") || n.contains("activex/") {
        return Role::ActiveX;
    }
    if n.contains("embeddings/")
        || ct.ends_with("officedocument.oleobject")
        || ct == "application/vnd.openxmlformats-officedocument.oleobject"
    {
        return Role::Embedding;
    }
    if n.starts_with("customui/") || n.contains("/customui/") {
        return Role::RibbonUi;
    }
    if ct.contains("macrosheet") || n.contains("macrosheets/") {
        return Role::MacroSheet;
    }
    if ct.contains("externallink") || n.contains("externallinks/") {
        return if n.ends_with(".xml") { Role::ExternalLink } else { Role::Binary };
    }
    if ct.contains("spreadsheetml.connections") || file == "connections.xml" {
        return Role::Connections;
    }
    if ct.contains("printersettings") || n.contains("printersettings/") {
        return Role::PrinterSettings;
    }
    if ct == "application/vnd.openxmlformats-package.core-properties+xml"
        || n == "docprops/core.xml"
    {
        return Role::CoreProps;
    }
    if ct == "application/vnd.openxmlformats-officedocument.extended-properties+xml"
        || n == "docprops/app.xml"
    {
        return Role::AppProps;
    }
    if ct == "application/vnd.openxmlformats-officedocument.custom-properties+xml"
        || n == "docprops/custom.xml"
    {
        return Role::CustomProps;
    }
    if ct.contains("classificationlabels") || n.ends_with("labelinfo.xml") {
        return Role::Labels;
    }
    if n.starts_with("customxml/") {
        return Role::CustomXml;
    }
    if ct.contains("obfuscatedfont")
        || ct.contains("fontdata")
        || ct.starts_with("application/x-font")
        || ct.starts_with("font/")
        || file.ends_with(".odttf")
        || file.ends_with(".fntdata")
    {
        return Role::Font;
    }
    if ct.contains("people+xml")
        || ct.contains("person+xml")
        || file == "people.xml"
        || n.contains("persons/")
    {
        return Role::People;
    }
    if ct.contains("comments") || ct.contains("commentauthors") || n.contains("comments") {
        return Role::Comments;
    }
    if n.starts_with("docprops/thumbnail") {
        return Role::Thumbnail;
    }
    if ct.contains("wordprocessingml.document.main")
        || ct.contains("ms-word.document.macroenabled.main")
        || ct.contains("wordprocessingml.template.main")
        || ct.contains("ms-word.template.macroenabledtemplate.main")
        || ct.contains("wordprocessingml.header")
        || ct.contains("wordprocessingml.footer")
        || ct.contains("wordprocessingml.footnotes")
        || ct.contains("wordprocessingml.endnotes")
        || ct.contains("document.glossary")
    {
        return Role::WordBody;
    }
    if ct.contains("wordprocessingml.settings") {
        return Role::WordSettings;
    }
    if ct.contains("spreadsheetml.sheet.main")
        || ct.contains("ms-excel.sheet.macroenabled.main")
        || ct.contains("spreadsheetml.template.main")
        || ct.contains("ms-excel.template.macroenabled.main")
        || ct.contains("ms-excel.addin.macroenabled.main")
    {
        return Role::Workbook;
    }
    if ct.contains("spreadsheetml.worksheet")
        || ct.contains("spreadsheetml.dialogsheet")
        || ct.contains("spreadsheetml.chartsheet")
    {
        return Role::Worksheet;
    }
    if ct.contains("presentationml.slide+")
        || ct.contains("presentationml.slidelayout")
        || ct.contains("presentationml.slidemaster")
        || ct.contains("presentationml.notesslide")
        || ct.contains("presentationml.notesmaster")
        || ct.contains("presentationml.handoutmaster")
    {
        return Role::Slide;
    }
    if ct.contains("presentationml.presentation.main")
        || ct.contains("presentationml.slideshow.main")
        || ct.contains("presentationml.template.main")
        || ct.contains("ms-powerpoint.")
    {
        return Role::Presentation;
    }
    if ct.contains("ms-visio.") {
        return Role::Visio;
    }
    if ct.contains("vmldrawing") || file.ends_with(".vml") {
        return Role::Vml;
    }
    if ct == "image/svg+xml" || file.ends_with(".svg") {
        return Role::Svg;
    }
    if ct.ends_with("+xml") || ct.ends_with("/xml") || file.ends_with(".xml") {
        return Role::OtherXml;
    }
    Role::Binary
}

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

/// Scans an OPC package.
pub(crate) fn scan(
    ctx: &mut Ctx<'_>,
    zip: &ZipArchive<'_>,
    location: &str,
) -> Result<FileFormat, Abort> {
    let mut pkg = Package::default();

    // ---- [Content_Types].xml ----------------------------------------------
    let ct_loc = Ctx::join(location, "[Content_Types].xml");
    if let Some(entry) = zip.entry("[Content_Types].xml")
        && let Some(bytes) = ctx.read_member(zip, entry, &ct_loc)?
    {
        let mut v = ContentTypesVisitor { ct: &mut pkg.content_types };
        xmlwalk::walk(ctx, &bytes, &ct_loc, &mut v)?;
    }

    // ---- all relationships (needed before parts are interpreted) ----------
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
        pkg.rels.insert(source.to_ascii_lowercase(), v.rels);
    }

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
            inspect_relationship(ctx, rel, source, &loc, zip);
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
        match role(name, &ct) {
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
            Role::ActiveX => scan_activex(ctx, zip, entry, &loc)?,
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
                if name.to_ascii_lowercase().ends_with(".xml") {
                    custom_xml += 1;
                    if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                        xmlwalk::walk(ctx, &bytes, &loc, &mut NoopVisitor)?;
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
                }
            }
            Role::Thumbnail => ctx.report.add(ThreatKind::StaleThumbnail, &loc, "a preview image is stored and may show an older state of the document"),
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
                    let mut v = SvgVisitor { location: loc.clone() };
                    xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
                }
            }
            Role::OtherXml => {
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                    xmlwalk::walk(ctx, &bytes, &loc, &mut NoopVisitor)?;
                }
            }
            Role::Binary => {
                // Media must be media: sniff a bounded prefix for executables.
                if let Some(bytes) = ctx.read_member(zip, entry, &loc)?
                    && let crate::format::Magic::Executable(what) = crate::format::sniff(&bytes)
                {
                    ctx.report.add(ThreatKind::EmbeddedExecutable, &loc, format!("{what} stored as a package part"));
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

/// Classifies one relationship and records external references.
fn inspect_relationship(
    ctx: &mut Ctx<'_>,
    rel: &Rel,
    source: &str,
    loc: &str,
    zip: &ZipArchive<'_>,
) {
    let suffix = rel.type_suffix();
    // A relationship reaches outside the package if it says so *or* if its
    // target does: `TargetMode="External"` is an attribute the author controls,
    // and `//host/share/x.dotm` (an RFC 3986 network-path reference), `\\host\x`,
    // `C:\x` or `scheme:` targets are not package-internal whatever it says.
    let reaches_outside = rel.external || url::leaves_package(&rel.target);
    if !reaches_outside {
        // Internal target: must stay inside the package.
        match resolve_target(source, &rel.target) {
            None => ctx.report.add_with(
                ThreatKind::MalformedContainer,
                Severity::Medium,
                loc,
                format!("relationship {} target {:?} escapes the package root", rel.id, rel.target),
            ),
            Some(part) => {
                if suffix == "aFChunk" && zip.entry(&part).is_some() {
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
        "video" | "audio" | "media" => (RefKind::Image, false),
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
    } else if arca_container::cfb::has_cfb_signature(&bytes) {
        ctx.scan_nested(&bytes, loc)?;
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
        crate::format::Magic::Zip | crate::format::Magic::Pdf | crate::format::Magic::Rtf => {
            EmbeddedKind::Document
        }
        _ => EmbeddedKind::Other,
    };
    let before = ctx.report.embedded.len();
    let nested = ctx.scan_nested(&bytes, loc)?;
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
}

impl Visitor for ContentTypesVisitor<'_> {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "Default" => {
                if let (Some(ext), Some(ct)) =
                    (xml::attr(e, "Extension"), xml::attr(e, "ContentType"))
                {
                    self.ct.defaults.insert(ext.to_ascii_lowercase(), ct);
                }
            }
            "Override" => {
                if let (Some(part), Some(ct)) =
                    (xml::attr(e, "PartName"), xml::attr(e, "ContentType"))
                {
                    self.ct.overrides.insert(
                        url::percent_decode(part.trim_start_matches('/')).to_ascii_lowercase(),
                        ct,
                    );
                }
            }
            _ => {}
        }
    }
}

struct RelsVisitor {
    rels: Vec<Rel>,
}

impl Visitor for RelsVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if xml::local_name(e) == "Relationship" {
            let external =
                xml::attr(e, "TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
            self.rels.push(Rel {
                id: xml::attr(e, "Id").unwrap_or_default(),
                rtype: xml::attr(e, "Type").unwrap_or_default(),
                target: xml::attr(e, "Target").unwrap_or_default(),
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
        }
    }
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
