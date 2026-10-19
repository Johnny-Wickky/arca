//! SpreadsheetML: sheet visibility, Excel 4.0 macros, auto-open names,
//! DDE and dangerous formulas, external links, data connections.

use arca_container::xml;
use quick_xml::events::BytesStart;

use super::Package;
use crate::formulas;
use crate::macro_analysis;
use crate::report::{MacroLanguage, MacroModule, MetaKey, RefKind, Severity, ThreatKind};
use crate::xmlwalk::{self, Ancestors, Visitor};
use crate::{Abort, Ctx, url};

fn is_auto_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let n = n.strip_prefix("_xlnm.").unwrap_or(&n);
    ["auto_open", "auto_close", "auto_activate", "auto_deactivate"].iter().any(|p| n.starts_with(p))
}

struct WorkbookVisitor<'a> {
    pkg: &'a Package,
    part: String,
    loc: String,
    defined_name: Option<String>,
    defined_text: String,
}

impl Visitor for WorkbookVisitor<'_> {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "sheet" => {
                let name = xml::attr(e, "name").unwrap_or_default();
                let state = xml::attr(e, "state").unwrap_or_else(|| "visible".to_owned());
                let id = xml::attr(e, "id").unwrap_or_default();
                let rtype = self
                    .pkg
                    .rel(&self.part, &id)
                    .map(|r| r.type_suffix().to_ascii_lowercase())
                    .unwrap_or_default();
                if rtype.contains("macrosheet") {
                    ctx.report.add(
                        ThreatKind::XlmMacroSheet,
                        &self.loc,
                        format!("Excel 4.0 macro sheet {name:?} ({state})"),
                    );
                }
                match state.as_str() {
                    "hidden" => ctx.report.add(
                        ThreatKind::HiddenSheet,
                        &self.loc,
                        format!("sheet {name:?} is hidden"),
                    ),
                    "veryHidden" => ctx.report.add(
                        ThreatKind::VeryHiddenSheet,
                        &self.loc,
                        format!("sheet {name:?} is 'very hidden' (invisible in the Excel UI)"),
                    ),
                    _ => {}
                }
            }
            "definedName" => {
                self.defined_name = xml::attr(e, "name");
                self.defined_text.clear();
            }
            "fileSharing" => {
                if let Some(user) = xml::attr(e, "userName") {
                    ctx.report.meta(MetaKey::Person, "write-reservation user", &user, &self.loc);
                    ctx.report.add(
                        ThreatKind::PersonalIdentifier,
                        &self.loc,
                        format!("write-reservation user name {user:?}"),
                    );
                }
            }
            "absPath" => {
                if let Some(path) = xml::attr(e, "url") {
                    ctx.report.add(
                        ThreatKind::LocalPathLeak,
                        &self.loc,
                        format!("workbook was saved in {path}"),
                    );
                    ctx.report.meta(MetaKey::LocalPath, "", &path, &self.loc);
                    if let Some(u) = url::profile_user(&path) {
                        ctx.report.meta(MetaKey::Person, "Windows profile", &u, &self.loc);
                    }
                }
            }
            "fileVersion" => {
                if let Some(app) = xml::attr(e, "appName") {
                    let edited = xml::attr(e, "lastEdited").unwrap_or_default();
                    let build = xml::attr(e, "rupBuild").unwrap_or_default();
                    ctx.report.meta(
                        MetaKey::AppVersion,
                        "",
                        &format!("{app} lastEdited={edited} build={build}"),
                        &self.loc,
                    );
                }
            }
            "workbookProtection" => {
                ctx.report.meta(MetaKey::Security, "", "workbook structure protection", &self.loc)
            }
            _ => {}
        }
    }

    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, anc: &Ancestors) {
        if anc.last().is_some_and(|a| a == "definedName") && self.defined_text.len() < 1024 {
            self.defined_text.push_str(text);
        }
    }

    fn end(&mut self, ctx: &mut Ctx<'_>, local: &str, _anc: &Ancestors) {
        if local == "definedName"
            && let Some(name) = self.defined_name.take()
            && is_auto_name(&name)
        {
            ctx.report.add(
                ThreatKind::XlmAutoOpenName,
                &self.loc,
                format!("{name} → {}", self.defined_text),
            );
        }
    }
}

/// Scans `xl/workbook.xml`.
pub(crate) fn scan_workbook(
    ctx: &mut Ctx<'_>,
    pkg: &Package,
    part: &str,
    bytes: &[u8],
    loc: &str,
) -> Result<(), Abort> {
    let mut v = WorkbookVisitor {
        pkg,
        part: part.to_owned(),
        loc: loc.to_owned(),
        defined_name: None,
        defined_text: String::new(),
    };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}

#[derive(Default)]
struct SheetVisitor {
    loc: String,
    hidden_rows: u64,
    hidden_cols: u64,
    /// Text of the formula being read; the buffer is reused for every cell.
    formula: String,
    in_formula: bool,
}

impl Visitor for SheetVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "row" if xml::attr(e, "hidden").is_some_and(|v| v == "1" || v == "true") => {
                self.hidden_rows += 1
            }
            "col" if xml::attr(e, "hidden").is_some_and(|v| v == "1" || v == "true") => {
                let min = xml::attr(e, "min").and_then(|v| v.parse::<u64>().ok()).unwrap_or(1);
                let max = xml::attr(e, "max").and_then(|v| v.parse::<u64>().ok()).unwrap_or(min);
                self.hidden_cols = self
                    .hidden_cols
                    .saturating_add(max.saturating_sub(min).saturating_add(1).min(16_384));
            }
            "f" => {
                self.formula.clear();
                self.in_formula = true;
            }
            "oleObject" => {
                super::word::ole_object(ctx, e, &self.loc);
                if xml::attr(e, "oleUpdate")
                    .is_some_and(|u| u.eq_ignore_ascii_case("OLEUPDATE_ALWAYS"))
                {
                    let link = xml::attr(e, "link").unwrap_or_default();
                    ctx.report.add(
                        ThreatKind::ExternalObjectLink,
                        &self.loc,
                        format!("linked OLE object updated automatically on open: {link}"),
                    );
                }
            }
            "control" => {
                let name = xml::attr(e, "name").unwrap_or_default();
                ctx.report.add(
                    ThreatKind::ActiveXControl,
                    &self.loc,
                    format!("ActiveX control {name:?} on sheet"),
                );
            }
            _ => {}
        }
    }

    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, anc: &Ancestors) {
        if self.in_formula && anc.last().is_some_and(|a| a == "f") && self.formula.len() < 8192 {
            self.formula.push_str(text);
        }
    }

    fn end(&mut self, ctx: &mut Ctx<'_>, local: &str, _anc: &Ancestors) {
        if local == "f" && self.in_formula {
            self.in_formula = false;
            formulas::report(ctx, &self.formula, &self.loc);
        }
    }
}

/// Scans a worksheet / dialog sheet / chart sheet.
pub(crate) fn scan_sheet(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = SheetVisitor { loc: loc.to_owned(), ..SheetVisitor::default() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    if v.hidden_rows + v.hidden_cols > 0 {
        ctx.report.add(
            ThreatKind::HiddenRowsOrColumns,
            loc,
            format!("{} hidden row(s), {} hidden column(s)", v.hidden_rows, v.hidden_cols),
        );
    }
    Ok(())
}

#[derive(Default)]
struct MacroSheetVisitor {
    cell: String,
    formula: Option<String>,
    lines: String,
    count: usize,
    truncated: bool,
    limit: usize,
}

impl Visitor for MacroSheetVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "c" => self.cell = xml::attr(e, "r").unwrap_or_default(),
            "f" => self.formula = Some(String::new()),
            _ => {}
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, anc: &Ancestors) {
        if anc.last().is_some_and(|a| a == "f")
            && let Some(f) = self.formula.as_mut()
            && f.len() < 8192
        {
            f.push_str(text);
        }
    }
    fn end(&mut self, _ctx: &mut Ctx<'_>, local: &str, _anc: &Ancestors) {
        if local == "f"
            && let Some(f) = self.formula.take()
        {
            self.count += 1;
            if self.lines.len() + f.len() + self.cell.len() + 4 > self.limit {
                self.truncated = true;
            } else {
                self.lines.push_str(&self.cell);
                self.lines.push_str(": =");
                self.lines.push_str(&f);
                self.lines.push('\n');
            }
        }
    }
}

/// Scans an Excel 4.0 macro sheet and extracts its formulas as source.
pub(crate) fn scan_macrosheet(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = MacroSheetVisitor {
        limit: ctx.policy.max_macro_source_bytes,
        ..MacroSheetVisitor::default()
    };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    ctx.report.add(
        ThreatKind::XlmMacroSheet,
        loc,
        format!("Excel 4.0 macro sheet with {} formula(s)", v.count),
    );
    let analysis = macro_analysis::analyze(&v.lines, MacroLanguage::Xlm);
    if !analysis.suspicious.is_empty() {
        ctx.report.add_with(
            ThreatKind::XlmMacroSheet,
            Severity::Critical,
            loc,
            format!("XLM functions: {}", analysis.suspicious.join(", ")),
        );
    }
    ctx.report.macros.push(MacroModule {
        language: MacroLanguage::Xlm,
        location: loc.to_owned(),
        name: loc.rsplit('/').next().unwrap_or(loc).to_owned(),
        source: v.lines,
        truncated: v.truncated,
        auto_exec: analysis.auto_exec,
        suspicious: analysis.suspicious,
        iocs: analysis.iocs,
    });
    Ok(())
}

struct ExternalLinkVisitor {
    loc: String,
}

impl Visitor for ExternalLinkVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "ddeLink" => {
                let service = xml::attr(e, "ddeService").unwrap_or_default();
                let topic = xml::attr(e, "ddeTopic").unwrap_or_default();
                ctx.report.add(
                    ThreatKind::DdeFormula,
                    &self.loc,
                    format!("DDE link {service}|{topic}"),
                );
            }
            "oleLink" => {
                let prog = xml::attr(e, "progId").unwrap_or_default();
                ctx.report.add_with(
                    ThreatKind::ExternalObjectLink,
                    Severity::High,
                    &self.loc,
                    format!("OLE link via {prog}"),
                );
            }
            _ => {}
        }
    }
}

/// Scans `xl/externalLinks/externalLinkN.xml`.
pub(crate) fn scan_external_link(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = ExternalLinkVisitor { loc: loc.to_owned() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}

struct ConnectionsVisitor {
    loc: String,
    refresh_on_load: bool,
    name: String,
}

impl Visitor for ConnectionsVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &Ancestors) {
        match xml::local_name(e) {
            "connection" => {
                self.name = xml::attr(e, "name").unwrap_or_default();
                self.refresh_on_load =
                    xml::attr(e, "refreshOnLoad").is_some_and(|v| v == "1" || v == "true");
                for key in ["sourceFile", "odcFile"] {
                    if let Some(path) = xml::attr(e, key) {
                        super::record_external(
                            ctx,
                            &path,
                            RefKind::DataConnection,
                            self.refresh_on_load,
                            &self.loc,
                        );
                    }
                }
            }
            "webPr" => {
                if let Some(u) = xml::attr(e, "url") {
                    super::record_external(
                        ctx,
                        &u,
                        RefKind::DataConnection,
                        self.refresh_on_load,
                        &self.loc,
                    );
                }
            }
            "textPr" => {
                if let Some(path) = xml::attr(e, "sourceFile") {
                    super::record_external(
                        ctx,
                        &path,
                        RefKind::DataConnection,
                        self.refresh_on_load,
                        &self.loc,
                    );
                }
            }
            "dbPr" | "olapPr" => {
                let conn = xml::attr(e, "connection").unwrap_or_default();
                let cmd = xml::attr(e, "command").unwrap_or_default();
                let auto = if self.refresh_on_load { " (refreshes on open)" } else { "" };
                ctx.report.add(
                    ThreatKind::ExternalDataConnection,
                    &self.loc,
                    format!("{}: {conn} {cmd}{auto}", self.name),
                );
                let lower = conn.to_ascii_lowercase();
                if lower.contains("password=") || lower.contains("pwd=") {
                    ctx.report.add_with(
                        ThreatKind::PersonalIdentifier,
                        Severity::High,
                        &self.loc,
                        "connection string embeds a password",
                    );
                }
                if let Some(user) = lower
                    .find("user id=")
                    .and_then(|i| conn.get(i + 8..))
                    .and_then(|s| s.split(';').next())
                {
                    ctx.report.meta(MetaKey::Person, "database user", user, &self.loc);
                }
            }
            _ => {}
        }
    }
}

/// Scans `xl/connections.xml`.
pub(crate) fn scan_connections(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v =
        ConnectionsVisitor { loc: loc.to_owned(), refresh_on_load: false, name: String::new() };
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    Ok(())
}
