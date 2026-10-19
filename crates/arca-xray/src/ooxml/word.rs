//! WordprocessingML: fields, hidden/invisible text, revisions, settings, VML.

use std::collections::BTreeSet;

use arca_container::xml;
use quick_xml::events::BytesStart;

use super::Package;
use crate::report::{MetaKey, RefKind, Severity, ThreatKind};
use crate::xmlwalk::{self, Ancestors, Visitor};
use crate::{Abort, Ctx, fields, url};

struct Field {
    instr: String,
    in_instr: bool,
}

#[derive(Default)]
struct RunState {
    hidden: bool,
    white: bool,
    tiny: bool,
    shaded: bool,
}

struct BodyVisitor<'a> {
    pkg: &'a Package,
    part: String,
    loc: String,
    fields: Vec<Field>,
    run: RunState,
    para_shaded: bool,
    cell_shaded: bool,
    dark_background: bool,
    hidden_runs: usize,
    hidden_sample: String,
    invisible_runs: usize,
    invisible_sample: String,
    revision_authors: BTreeSet<String>,
    insertions: usize,
    deletions: usize,
}

fn on(e: &BytesStart<'_>) -> bool {
    !matches!(xml::attr(e, "val").as_deref(), Some("0" | "false" | "off"))
}

fn parent_is(anc: &Ancestors, names: &[&str]) -> bool {
    anc.ends_with(names)
}

fn is_dark_hex(hex: &str) -> bool {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return false;
    }
    let comp = |i: usize| {
        h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).map_or(255u32, u32::from)
    };
    // Rec. 601 luma.
    (comp(0) * 299 + comp(2) * 587 + comp(4) * 114) / 1000 < 96
}

impl Visitor for BodyVisitor<'_> {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, anc: &Ancestors) {
        match xml::local_name(e) {
            "background" => {
                if let Some(c) = xml::attr(e, "color") {
                    self.dark_background = is_dark_hex(&c);
                }
            }
            "p" => self.para_shaded = false,
            "tc" => self.cell_shaded = false,
            "r" if !parent_is(anc, &["rPr"]) => self.run = RunState::default(),
            "vanish" if parent_is(anc, &["r", "rPr"]) => self.run.hidden = on(e),
            "color" if parent_is(anc, &["r", "rPr"]) => {
                let val = xml::attr(e, "val").unwrap_or_default();
                let theme = xml::attr(e, "themeColor").unwrap_or_default();
                self.run.white =
                    val.eq_ignore_ascii_case("FFFFFF") || theme.eq_ignore_ascii_case("background1");
            }
            "sz" if parent_is(anc, &["r", "rPr"]) => {
                self.run.tiny =
                    xml::attr(e, "val").and_then(|v| v.parse::<u32>().ok()).is_some_and(|v| v <= 2);
            }
            "shd" | "highlight" => {
                let fill = xml::attr(e, "fill")
                    .or_else(|| xml::attr(e, "val"))
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let colored =
                    !matches!(fill.as_str(), "" | "auto" | "ffffff" | "none" | "white" | "clear");
                if parent_is(anc, &["r", "rPr"]) {
                    self.run.shaded = colored;
                } else if parent_is(anc, &["pPr"]) {
                    self.para_shaded = colored;
                } else if parent_is(anc, &["tcPr"]) {
                    self.cell_shaded = colored;
                }
            }
            "fldChar" => match xml::attr(e, "fldCharType").as_deref() {
                Some("begin") => {
                    if self.fields.len() < 64 {
                        self.fields.push(Field { instr: String::new(), in_instr: true });
                    }
                }
                Some("separate") => {
                    if let Some(f) = self.fields.last_mut() {
                        f.in_instr = false;
                    }
                }
                Some("end") => {
                    if let Some(f) = self.fields.pop() {
                        self.finish_field(ctx, &f.instr);
                    }
                }
                _ => {}
            },
            "fldSimple" => {
                if let Some(instr) = xml::attr(e, "instr") {
                    fields::analyze(ctx, &instr, &self.loc);
                }
            }
            "ins" | "del" | "moveFrom" | "moveTo" | "rPrChange" | "pPrChange" => {
                if let Some(author) = xml::attr(e, "author") {
                    if self.revision_authors.len() < 256 {
                        self.revision_authors.insert(author);
                    }
                    match xml::local_name(e) {
                        "ins" | "moveTo" => self.insertions += 1,
                        "del" | "moveFrom" => self.deletions += 1,
                        _ => {}
                    }
                }
            }
            "OLEObject" => ole_object(ctx, e, &self.loc),
            "control" => {
                let name = xml::attr(e, "name").unwrap_or_default();
                ctx.report.add(
                    ThreatKind::ActiveXControl,
                    &self.loc,
                    format!("ActiveX control {name:?} placed in the document"),
                );
            }
            "imagedata" | "fill" => vml_sources(ctx, e, &self.loc),
            "altChunk" => {
                let id = xml::attr(e, "id").unwrap_or_default();
                let target =
                    self.pkg.rel(&self.part, &id).map(|r| r.target.clone()).unwrap_or_default();
                ctx.report.add(
                    ThreatKind::AltChunkImport,
                    &self.loc,
                    format!("altChunk {id} → {target}"),
                );
            }
            _ => {}
        }
    }

    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, anc: &Ancestors) {
        match anc.last() {
            Some("instrText" | "delInstrText") => {
                if let Some(f) = self.fields.last_mut()
                    && f.in_instr
                    && f.instr.len() < 8192
                {
                    f.instr.push_str(text);
                }
            }
            Some("t") if parent_is(anc, &["r", "t"]) => {
                let shaded =
                    self.run.shaded || self.para_shaded || self.cell_shaded || self.dark_background;
                if self.run.hidden && !text.trim().is_empty() {
                    self.hidden_runs += 1;
                    push_sample(&mut self.hidden_sample, text);
                } else if ((self.run.white && !shaded) || self.run.tiny) && !text.trim().is_empty()
                {
                    self.invisible_runs += 1;
                    push_sample(&mut self.invisible_sample, text);
                }
            }
            _ => {}
        }
    }
}

fn push_sample(sample: &mut String, text: &str) {
    if sample.chars().count() < 160 {
        if !sample.is_empty() {
            sample.push(' ');
        }
        sample.extend(text.chars().take(160));
    }
}

impl BodyVisitor<'_> {
    fn finish_field(&mut self, ctx: &mut Ctx<'_>, instr: &str) {
        // A nested `QUOTE 68 68 69` contributes its decoded text to the
        // enclosing field's instruction (classic DDE obfuscation).
        if let Some(decoded) = fields::decode_quote_codes(instr)
            && let Some(parent) = self.fields.last_mut()
            && parent.in_instr
        {
            parent.instr.push_str(&decoded);
            return;
        }
        fields::analyze(ctx, instr, &self.loc);
    }
}

/// `o:OLEObject` elements (Word and VML drawings).
pub(super) fn ole_object(ctx: &mut Ctx<'_>, e: &BytesStart<'_>, loc: &str) {
    let prog = xml::attr(e, "ProgID").or_else(|| xml::attr(e, "progId")).unwrap_or_default();
    let linked = xml::attr(e, "Type").is_some_and(|t| t.eq_ignore_ascii_case("Link"));
    let lower = prog.to_ascii_lowercase();
    if lower.starts_with("equation.") {
        ctx.report.add(
            ThreatKind::ExploitableOleClass,
            loc,
            format!("{prog}: Microsoft Equation Editor 3.0 (CVE-2017-11882, CVE-2018-0802)"),
        );
    } else if lower == "package" || lower.starts_with("package.") {
        ctx.report.add(
            ThreatKind::OlePackage,
            loc,
            format!("{prog}: OLE Packager object wrapping a file"),
        );
    } else if lower.starts_with("htmlfile") || lower.starts_with("shell.explorer") {
        ctx.report.add(
            ThreatKind::ExploitableOleClass,
            loc,
            format!("{prog}: HTML-rendering OLE class"),
        );
    } else {
        let what = if linked { "linked" } else { "embedded" };
        ctx.report.add(ThreatKind::OleObject, loc, format!("{what} OLE object {prog}"));
    }
}

/// VML `src` / `o:href` attributes that point outside the package.
pub(super) fn vml_sources(ctx: &mut Ctx<'_>, e: &BytesStart<'_>, loc: &str) {
    for (k, v) in xml::attrs(e) {
        let (_, local) = xml::split_qname(&k);
        match local {
            "src" if url::leaves_package(&v) => {
                super::record_external(ctx, &v, RefKind::Image, true, loc);
            }
            "href" if !v.is_empty() => {
                // o:href keeps the original location of an inserted picture.
                let class = url::classify(&v);
                if matches!(
                    class,
                    crate::report::TargetClass::LocalFile | crate::report::TargetClass::Unc
                ) {
                    ctx.report.add(
                        ThreatKind::LocalPathLeak,
                        loc,
                        format!("original picture location {v}"),
                    );
                    ctx.report.meta(MetaKey::LocalPath, "", &v, loc);
                    if let Some(u) = url::profile_user(&v) {
                        ctx.report.meta(MetaKey::Person, "Windows profile", &u, loc);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Scans a document, header, footer, footnote, endnote, comment or glossary part.
pub(crate) fn scan_body(
    ctx: &mut Ctx<'_>,
    pkg: &Package,
    part: &str,
    bytes: &[u8],
    loc: &str,
) -> Result<(), Abort> {
    let mut v = BodyVisitor {
        pkg,
        part: part.to_owned(),
        loc: loc.to_owned(),
        fields: Vec::new(),
        run: RunState::default(),
        para_shaded: false,
        cell_shaded: false,
        dark_background: false,
        hidden_runs: 0,
        hidden_sample: String::new(),
        invisible_runs: 0,
        invisible_sample: String::new(),
        revision_authors: BTreeSet::new(),
        insertions: 0,
        deletions: 0,
    };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    // Unterminated fields are still analysed.
    while let Some(f) = v.fields.pop() {
        fields::analyze(ctx, &f.instr, loc);
    }
    if v.hidden_runs > 0 {
        ctx.report.add(
            ThreatKind::HiddenText,
            loc,
            format!("{} hidden run(s): \"{}\"", v.hidden_runs, v.hidden_sample),
        );
    }
    if v.invisible_runs > 0 {
        ctx.report.add(
            ThreatKind::InvisibleText,
            loc,
            format!(
                "{} white-on-white or ≤1pt run(s): \"{}\"",
                v.invisible_runs, v.invisible_sample
            ),
        );
    }
    if !v.revision_authors.is_empty() || v.insertions + v.deletions > 0 {
        let authors: Vec<String> = v.revision_authors.iter().cloned().collect();
        ctx.report.add(
            ThreatKind::TrackedChanges,
            loc,
            format!(
                "{} insertion(s), {} deletion(s) by {}",
                v.insertions,
                v.deletions,
                authors.join(", ")
            ),
        );
        for a in authors {
            ctx.report.meta(MetaKey::Person, "revision author", &a, loc);
        }
    }
    Ok(())
}

#[derive(Default)]
struct SettingsVisitor {
    rsids: usize,
    doc_vars: Vec<String>,
    mail_merge: Vec<String>,
}

impl Visitor for SettingsVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, anc: &Ancestors) {
        match xml::local_name(e) {
            "rsid" | "rsidRoot" if anc.last().is_some_and(|a| a == "rsids") => self.rsids += 1,
            "docVar" => {
                let name = xml::attr(e, "name").unwrap_or_default();
                let len = xml::attr(e, "val").map_or(0, |v| v.len());
                if self.doc_vars.len() < 32 {
                    self.doc_vars.push(format!("{name} ({len} chars)"));
                }
            }
            "connectString" | "query" if anc.iter().any(|a| a == "mailMerge") => {
                if let Some(v) = xml::attr(e, "val")
                    && self.mail_merge.len() < 8
                {
                    self.mail_merge.push(v);
                }
            }
            "documentProtection" | "writeProtection" => {
                let edit = xml::attr(e, "edit").unwrap_or_else(|| "write".to_owned());
                ctx.report.meta(
                    MetaKey::Security,
                    "",
                    &format!("{} ({edit})", xml::local_name(e)),
                    "word/settings.xml",
                );
            }
            _ => {}
        }
    }
}

/// Scans `word/settings.xml`.
pub(crate) fn scan_settings(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = SettingsVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    if v.rsids > 0 {
        ctx.report.add(
            ThreatKind::EditingSessionIds,
            loc,
            format!("{} revision-save identifiers (editing sessions) recorded", v.rsids),
        );
        ctx.report.meta(MetaKey::EditingSessions, "", &v.rsids.to_string(), loc);
    }
    if !v.doc_vars.is_empty() {
        ctx.report.add(ThreatKind::DocumentVariables, loc, v.doc_vars.join(", "));
    }
    if !v.mail_merge.is_empty() {
        ctx.report.add_with(
            ThreatKind::MailMergeDataSource,
            Severity::High,
            loc,
            v.mail_merge.join(" | "),
        );
    }
    Ok(())
}

struct VmlVisitor {
    loc: String,
}

impl Visitor for VmlVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "OLEObject" => ole_object(ctx, e, &self.loc),
            _ => vml_sources(ctx, e, &self.loc),
        }
    }
}

/// Scans a VML drawing part.
pub(crate) fn scan_vml(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    // Legacy VML drawings are frequently not namespace-well-formed HTML-ish
    // XML; malformation is reported by `walk` and is not fatal.
    let mut v = VmlVisitor { loc: loc.to_owned() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_colors() {
        assert!(is_dark_hex("000000"));
        assert!(is_dark_hex("1F1F1F"));
        assert!(!is_dark_hex("FFFFFF"));
        assert!(!is_dark_hex("auto"));
    }
}
