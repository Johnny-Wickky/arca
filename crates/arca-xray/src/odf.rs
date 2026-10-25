//! OpenDocument packages (`.odt .ods .odp .odg`): Basic macros, scripts,
//! event listeners, linked content, metadata and encryption.

use arca_container::xml;
use arca_container::zip::ZipArchive;
use quick_xml::events::BytesStart;

use crate::format::{FileFormat, odf_format_from_mimetype};
use crate::report::{MacroLanguage, MacroModule, MetaKey, RefKind, Severity, ThreatKind};
use crate::xmlwalk::{self, Ancestors, NoopVisitor, Visitor};
use crate::{Abort, Ctx, formulas, macro_analysis, memfind_ci, ooxml, url};

/// Scans an ODF package.
pub(crate) fn scan(
    ctx: &mut Ctx<'_>,
    zip: &ZipArchive<'_>,
    location: &str,
) -> Result<FileFormat, Abort> {
    // OpenDocument legitimately repeats a local name under several prefixes on one element
    // (`draw:style-name` and `presentation:style-name`); its attribute lookups below read every
    // candidate instead.
    let saved = core::mem::replace(&mut ctx.attr_collisions, false);
    let result = scan_package(ctx, zip, location);
    ctx.attr_collisions = saved;
    result
}

/// Scans a flat ODF document (`office:document` in one XML file).
pub(crate) fn scan_flat(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let saved = core::mem::replace(&mut ctx.attr_collisions, false);
    let result = (|| {
        let mut v = ContentVisitor { loc: loc.to_owned() };
        xmlwalk::walk(ctx, bytes, loc, &mut v)?;
        meta(ctx, bytes, loc)?;
        settings(ctx, bytes, loc)
    })();
    ctx.attr_collisions = saved;
    result
}

/// `true` if an attribute value invokes a script or a macro. Case does not matter, and neither
/// do the tabs and line breaks a URL parser drops.
fn is_script_url(value: &str) -> bool {
    let v: String = value
        .chars()
        .filter(|c| !c.is_ascii_control())
        .collect::<String>()
        .trim()
        .to_ascii_lowercase();
    v.starts_with("vnd.sun.star.script:")
        || v.starts_with("macro:")
        || v.starts_with("javascript:")
        || v.starts_with("vbscript:")
}

/// Cheap pre-filter on the raw attribute text: `false` means no attribute can be a script URL.
/// Character references are let through because they can spell the URL out.
fn may_hold_script_url(e: &BytesStart<'_>) -> bool {
    let raw = e.attributes_raw().as_bytes();
    memfind_ci(raw, b"script:").is_some()
        || memfind_ci(raw, b"macro:").is_some()
        || memfind_ci(raw, b"&#").is_some()
}

fn scan_package(
    ctx: &mut Ctx<'_>,
    zip: &ZipArchive<'_>,
    location: &str,
) -> Result<FileFormat, Abort> {
    let mut format = FileFormat::OdfOther;
    if let Some(e) = zip.entry("mimetype") {
        let loc = Ctx::join(location, "mimetype");
        if zip.entries().first().map(arca_container::zip::ZipEntry::name) != Some("mimetype") {
            ctx.report.add_with(
                ThreatKind::ParserDifferential,
                Severity::Low,
                &loc,
                "mimetype is not the first member",
            );
        }
        if let Some(bytes) = ctx.read_member(zip, e, &loc)? {
            format = odf_format_from_mimetype(&String::from_utf8_lossy(&bytes));
        }
    }
    ctx.identified(format);

    for entry in zip.entries() {
        if entry.is_dir() {
            continue;
        }
        let name = entry.name();
        let lname = name.to_ascii_lowercase();
        let loc = Ctx::join(location, name);
        ctx.report.stats.parts_scanned = ctx.report.stats.parts_scanned.saturating_add(1);
        if lname == "mimetype" {
            continue;
        }
        if lname == "meta-inf/manifest.xml" {
            if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                let mut v = ManifestVisitor { loc: loc.clone(), encrypted: 0 };
                xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
                if v.encrypted > 0 {
                    ctx.report.add(ThreatKind::EncryptedDocument, &loc, format!("{} encrypted member(s); contents cannot be inspected without the password", v.encrypted));
                }
            }
            continue;
        }
        if lname.starts_with("basic/")
            && lname.ends_with(".xml")
            && !lname.ends_with("script-lb.xml")
            && !lname.ends_with("script-lc.xml")
        {
            if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                basic_module(ctx, &bytes, &loc)?;
            }
            continue;
        }
        if lname.starts_with("scripts/") {
            // Any file under Scripts/ is a script the document can run, whatever its extension.
            let lang = if lname.ends_with(".py") {
                Some(MacroLanguage::Python)
            } else if lname.ends_with(".js") {
                Some(MacroLanguage::JavaScript)
            } else if lname.ends_with(".bsh") || lname.ends_with(".java") {
                Some(MacroLanguage::BeanShell)
            } else {
                None
            };
            if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                match lang {
                    Some(lang) => {
                        let src = String::from_utf8_lossy(&bytes);
                        push_script(ctx, lang, &loc, name, &src);
                        ctx.report.add(
                            ThreatKind::EmbeddedScript,
                            &loc,
                            format!("{lang:?} script embedded in the document"),
                        );
                    }
                    None => ctx.report.add(
                        ThreatKind::EmbeddedScript,
                        &loc,
                        "script of an unrecognised language embedded in the document",
                    ),
                }
            }
            continue;
        }
        if lname.starts_with("configurations2/") && lname.ends_with(".xml") {
            // User-interface customisations (menus, toolbars, key bindings) can bind commands
            // to macros.
            if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                let mut v = ConfigurationVisitor { loc: loc.clone() };
                xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
            }
            continue;
        }
        if lname.ends_with(".xml") || lname.ends_with(".rdf") {
            if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
                if lname == "meta.xml" {
                    meta(ctx, &bytes, &loc)?;
                } else if lname == "settings.xml" {
                    settings(ctx, &bytes, &loc)?;
                } else if lname == "content.xml"
                    || lname == "styles.xml"
                    || lname.ends_with("/content.xml")
                    || lname.ends_with("/styles.xml")
                    || lname.starts_with("dialogs/")
                {
                    let mut v = ContentVisitor { loc: loc.clone() };
                    xmlwalk::walk(ctx, &bytes, &loc, &mut v)?;
                } else {
                    xmlwalk::walk(ctx, &bytes, &loc, &mut NoopVisitor)?;
                }
            }
            continue;
        }
        // Everything else (embedded objects, pictures, any binary): by what the bytes are.
        if let Some(bytes) = ctx.read_member(zip, entry, &loc)? {
            let ct = if lname.starts_with("pictures/")
                || lname.starts_with("thumbnails/")
                || lname.starts_with("objectreplacements/")
            {
                "image/*"
            } else {
                ""
            };
            if (lname.starts_with("object ") || lname.contains("/object "))
                && arca_container::cfb::has_cfb_signature(&bytes)
            {
                ctx.report.add(ThreatKind::OleObject, &loc, "embedded object");
            }
            ooxml::scan_binary_part(ctx, &bytes, &loc, ct)?;
        }
    }
    Ok(format)
}

struct ManifestVisitor {
    loc: String,
    encrypted: usize,
}

impl Visitor for ManifestVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "encryption-data" => self.encrypted += 1,
            "file-entry" => {
                let mt = xml::attr(e, "media-type").unwrap_or_default();
                if mt.contains("script") || mt.contains("basic") {
                    let path = xml::attr(e, "full-path").unwrap_or_default();
                    ctx.report.add(ThreatKind::EmbeddedScript, &self.loc, format!("{path} ({mt})"));
                }
            }
            _ => {}
        }
    }
}

fn push_script(ctx: &mut Ctx<'_>, lang: MacroLanguage, loc: &str, name: &str, src: &str) {
    let limit = ctx.policy.max_macro_source_bytes;
    let analysis = macro_analysis::analyze(src, lang);
    if !analysis.auto_exec.is_empty() {
        ctx.report.add(ThreatKind::MacroAutoExec, loc, analysis.auto_exec.join(", "));
    }
    ctx.report.macros.push(MacroModule {
        language: lang,
        location: loc.to_owned(),
        name: name.to_owned(),
        source: arca_container::text::truncate_utf8(src, limit).to_owned(),
        truncated: src.len() > limit,
        auto_exec: analysis.auto_exec,
        suspicious: analysis.suspicious,
        iocs: analysis.iocs,
    });
}

#[derive(Default)]
struct BasicVisitor {
    name: String,
    source: String,
}

impl Visitor for BasicVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if xml::local_name(e) == "module" {
            self.name = xml::attr(e, "name").unwrap_or_default();
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, anc: &Ancestors) {
        if anc.last().is_some_and(|a| a == "module") && self.source.len() < (16 << 20) {
            self.source.push_str(text);
        }
    }
}

fn basic_module(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = BasicVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    if v.source.trim().is_empty() {
        return Ok(());
    }
    ctx.report.add(ThreatKind::StarBasicMacros, loc, format!("Basic module {:?}", v.name));
    let name = if v.name.is_empty() {
        loc.rsplit('/').next().unwrap_or(loc).to_owned()
    } else {
        v.name.clone()
    };
    push_script(ctx, MacroLanguage::StarBasic, loc, &name, &v.source);
    Ok(())
}

struct ContentVisitor {
    loc: String,
}

impl Visitor for ContentVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, anc: &Ancestors) {
        let local = xml::local_name(e);
        if matches!(local, "table-cell" | "covered-table-cell") {
            // Every `formula` attribute counts, whatever its prefix: a consumer picks the one
            // in the right namespace, and the scanner cannot tell which that is.
            for raw in xmlwalk::attr_all(e, "formula") {
                let formula = formulas::strip_odf_prefix(&raw);
                formulas::report(ctx, formula, &self.loc);
                for doc in formulas::odf_document_refs(formula) {
                    ooxml::record_external(ctx, &doc, RefKind::WorkbookLink, true, &self.loc);
                }
            }
        }
        if matches!(local, "dde-source" | "dde-link" | "dde-connection-decl") {
            let part = |name| xml::attr(e, name).unwrap_or_default();
            ctx.report.add(
                ThreatKind::DdeFormula,
                &self.loc,
                format!(
                    "DDE link: {} | {} | {}",
                    part("dde-application"),
                    part("dde-topic"),
                    part("dde-item")
                ),
            );
        }
        if local == "event-listener" {
            let event = xml::attr(e, "event-name").unwrap_or_default();
            let target =
                xml::attr(e, "href").or_else(|| xml::attr(e, "macro-name")).unwrap_or_default();
            ctx.report.add(ThreatKind::OdfEventScript, &self.loc, format!("{event} → {target}"));
            return;
        }
        // A script URL in *any* attribute (`vnd.sun.star.script:` is how a link, a button or a
        // form control invokes a macro). The raw text is searched first, without allocating.
        if may_hold_script_url(e) {
            for (k, v) in xml::attrs(e) {
                if is_script_url(&v) {
                    ctx.report.add(
                        ThreatKind::OdfEventScript,
                        &self.loc,
                        format!("<{local}> {} invokes {v}", xml::split_qname(&k).1),
                    );
                }
            }
        }
        for href in xmlwalk::attr_all(e, "href") {
            if is_script_url(&href) {
                ctx.report.add(
                    ThreatKind::OdfEventScript,
                    &self.loc,
                    format!("<{local}> invokes {href}"),
                );
                continue;
            }
            if !url::leaves_package(&href) && !href.starts_with('/') {
                continue;
            }
            let (kind, auto) = match local {
                "image" | "fill-image" => (RefKind::Image, true),
                "a" => (RefKind::Hyperlink, false),
                "section-source" | "table-source" | "plugin" | "applet" | "object" | "frame" => {
                    (RefKind::Frame, true)
                }
                "template" => (RefKind::Template, true),
                _ if anc.iter().any(|a| a == "form") => (RefKind::DataConnection, false),
                _ => (RefKind::Other, true),
            };
            ooxml::record_external(ctx, &href, kind, auto, &self.loc);
        }
    }
}

/// `Configurations2/`: menus, toolbars and key bindings. A command bound to a macro runs it.
struct ConfigurationVisitor {
    loc: String,
}

impl Visitor for ConfigurationVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if !may_hold_script_url(e) {
            return;
        }
        for (_, v) in xml::attrs(e) {
            if is_script_url(&v) {
                ctx.report.add(
                    ThreatKind::OdfEventScript,
                    &self.loc,
                    format!("user-interface command bound to {v}"),
                );
            }
        }
    }
}

#[derive(Default)]
struct MetaVisitor {
    current: Option<(MetaKey, String)>,
    text: String,
    found: Vec<(MetaKey, String, String)>,
}

impl Visitor for MetaVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        let local = xml::local_name(e);
        let key = match local {
            "generator" => Some(MetaKey::Generator),
            "title" => Some(MetaKey::Title),
            "description" => Some(MetaKey::Description),
            "subject" => Some(MetaKey::Subject),
            "keyword" => Some(MetaKey::Keywords),
            "initial-creator" => Some(MetaKey::Author),
            "creator" => Some(MetaKey::LastModifiedBy),
            "creation-date" => Some(MetaKey::Created),
            "date" => Some(MetaKey::Modified),
            "print-date" => Some(MetaKey::LastPrinted),
            "printed-by" => Some(MetaKey::Person),
            "editing-cycles" => Some(MetaKey::EditingCycles),
            "editing-duration" => Some(MetaKey::TotalEditMinutes),
            "language" => Some(MetaKey::Language),
            "user-defined" => Some(MetaKey::CustomProperty),
            _ => None,
        };
        if local == "template"
            && let Some(href) = xml::attr(e, "href")
            && !href.trim().is_empty()
        {
            ctx.report.meta(MetaKey::Template, "", &href, "meta.xml");
            if url::leaves_package(&href) || href.starts_with('/') {
                ooxml::record_external(ctx, &href, RefKind::Template, true, "meta.xml");
            }
        }
        if let Some(k) = key {
            let name = if k == MetaKey::CustomProperty {
                xml::attr(e, "name").unwrap_or_default()
            } else {
                String::new()
            };
            self.current = Some((k, name));
            self.text.clear();
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, _anc: &Ancestors) {
        if self.current.is_some() && self.text.len() < 4096 {
            self.text.push_str(text);
        }
    }
    fn end(&mut self, _ctx: &mut Ctx<'_>, _local: &str, _anc: &Ancestors) {
        if let Some((k, name)) = self.current.take()
            && self.found.len() < 512
        {
            self.found.push((k, name, core::mem::take(&mut self.text)));
        }
    }
}

/// Converts an ISO 8601 duration (`PT1H2M3S`, `P1DT2H`) to minutes.
fn iso_duration_minutes(d: &str) -> Option<u64> {
    let d = d.strip_prefix('P')?;
    let mut total_secs = 0u64;
    let mut num = String::new();
    let mut in_time = false;
    for c in d.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' | '.' => num.push(c),
            unit => {
                let v: f64 = num.parse().ok()?;
                num.clear();
                let secs = match (unit, in_time) {
                    ('D', false) => v * 86_400.0,
                    ('H', true) => v * 3_600.0,
                    ('M', true) => v * 60.0,
                    ('S', true) => v,
                    ('Y', false) => v * 31_536_000.0,
                    ('M', false) => v * 2_592_000.0,
                    ('W', false) => v * 604_800.0,
                    _ => return None,
                };
                if !(0.0..1e12).contains(&secs) {
                    return None;
                }
                // Truncation to whole seconds is intended.
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "range-checked above"
                )]
                let whole = secs as u64;
                total_secs = total_secs.saturating_add(whole);
            }
        }
    }
    Some(total_secs / 60)
}

fn meta(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = MetaVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    let mut people = false;
    for (k, name, val) in v.found {
        match k {
            MetaKey::TotalEditMinutes => {
                if let Some(m) = iso_duration_minutes(val.trim()) {
                    ctx.report.meta(k, "", &m.to_string(), loc);
                }
            }
            MetaKey::Person => {
                people = true;
                ctx.report.meta(k, "printed by", &val, loc);
            }
            _ => {
                people |= matches!(k, MetaKey::Author | MetaKey::LastModifiedBy);
                ctx.report.meta(k, &name, &val, loc);
            }
        }
    }
    if people {
        ctx.report.add(ThreatKind::AuthorMetadata, loc, "author / editor names are stored");
    }
    Ok(())
}

#[derive(Default)]
struct SettingsVisitor {
    item: Option<String>,
    text: String,
    printer: Option<String>,
}

impl Visitor for SettingsVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        if xml::local_name(e) == "config-item" {
            self.item = xml::attr(e, "name");
            self.text.clear();
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, _anc: &Ancestors) {
        if self.item.is_some() && self.text.len() < 512 {
            self.text.push_str(text);
        }
    }
    fn end(&mut self, _ctx: &mut Ctx<'_>, local: &str, _anc: &Ancestors) {
        if local == "config-item"
            && let Some(name) = self.item.take()
            && name == "PrinterName"
            && !self.text.trim().is_empty()
        {
            self.printer = Some(self.text.trim().to_owned());
        }
    }
}

fn settings(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = SettingsVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    if let Some(p) = v.printer {
        ctx.report.meta(MetaKey::Printer, "", &p, loc);
        ctx.report.add(ThreatKind::PrinterMetadata, loc, format!("printer {p:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(iso_duration_minutes("PT1H2M3S"), Some(62));
        assert_eq!(iso_duration_minutes("P1DT1H"), Some(1500));
        assert_eq!(iso_duration_minutes("PT45S"), Some(0));
        assert_eq!(iso_duration_minutes("garbage"), None);
    }
}
