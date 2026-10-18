//! Forensic metadata of OPC packages (core/app/custom properties, people,
//! comments, sensitivity labels, printer settings).

use std::collections::BTreeSet;

use arca_container::xml;
use quick_xml::events::BytesStart;

use crate::report::{MetaKey, ThreatKind};
use crate::xmlwalk::{self, Visitor};
use crate::{Abort, Ctx, url};

/// Collects the text of leaf elements keyed by their local name.
#[derive(Default)]
struct LeafText {
    current: Option<String>,
    text: String,
    pairs: Vec<(String, String)>,
}

impl Visitor for LeafText {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, empty: bool, anc: &[String]) {
        if !empty && anc.len() == 1 {
            self.current = Some(xml::local_name(e).to_owned());
            self.text.clear();
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, _anc: &[String]) {
        if self.current.is_some() && self.text.len() < 4096 {
            self.text.push_str(text);
        }
    }
    fn end(&mut self, _ctx: &mut Ctx<'_>, local: &str, anc: &[String]) {
        if anc.len() == 1
            && let Some(cur) = self.current.take()
            && cur == local
        {
            self.pairs.push((cur, core::mem::take(&mut self.text)));
        }
    }
}

/// `docProps/core.xml`.
pub(crate) fn core(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = LeafText::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    let mut any_person = false;
    for (k, val) in &v.pairs {
        let key = match k.as_str() {
            "title" => MetaKey::Title,
            "subject" => MetaKey::Subject,
            "creator" => {
                any_person = true;
                MetaKey::Author
            }
            "lastModifiedBy" => {
                any_person = true;
                MetaKey::LastModifiedBy
            }
            "keywords" => MetaKey::Keywords,
            "description" => MetaKey::Description,
            "revision" => MetaKey::Revision,
            "created" => MetaKey::Created,
            "modified" => MetaKey::Modified,
            "lastPrinted" => MetaKey::LastPrinted,
            "category" => MetaKey::Category,
            "contentStatus" => MetaKey::ContentStatus,
            "language" => MetaKey::Language,
            _ => continue,
        };
        ctx.report.meta(key, "", val, loc);
    }
    if any_person {
        ctx.report.add(ThreatKind::AuthorMetadata, loc, "author / last editor names are stored");
    }
    Ok(())
}

/// `docProps/app.xml`.
pub(crate) fn app(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = LeafText::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    for (k, val) in &v.pairs {
        match k.as_str() {
            "Application" => ctx.report.meta(MetaKey::Application, "", val, loc),
            "AppVersion" => ctx.report.meta(MetaKey::AppVersion, "", val, loc),
            "Company" => ctx.report.meta(MetaKey::Company, "", val, loc),
            "Manager" => ctx.report.meta(MetaKey::Manager, "", val, loc),
            "TotalTime" => ctx.report.meta(MetaKey::TotalEditMinutes, "", val, loc),
            "Template" => {
                ctx.report.meta(MetaKey::Template, "", val, loc);
                if url::classify(val) == crate::report::TargetClass::LocalFile {
                    ctx.report.add(ThreatKind::LocalPathLeak, loc, format!("template path {val}"));
                }
            }
            "DocSecurity" if val.trim() != "0" => {
                ctx.report.meta(MetaKey::Security, "", &format!("DocSecurity={val}"), loc)
            }
            "HyperlinkBase" if !val.trim().is_empty() => {
                ctx.report.meta(MetaKey::HyperlinkBase, "", val, loc);
                ctx.report.add(
                    ThreatKind::HyperlinkBase,
                    loc,
                    format!("relative links resolve against {val}"),
                );
            }
            "HiddenSlides" if val.trim().parse::<u32>().is_ok_and(|n| n > 0) => {
                ctx.report.add(ThreatKind::HiddenSlide, loc, format!("{val} hidden slide(s)"));
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Default)]
struct CustomVisitor {
    name: Option<String>,
    value: String,
    pairs: Vec<(String, String)>,
}

impl Visitor for CustomVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &[String]) {
        if xml::local_name(e) == "property" {
            self.name = xml::attr(e, "name");
            self.value.clear();
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, _anc: &[String]) {
        if self.name.is_some() && self.value.len() < 4096 {
            self.value.push_str(text);
        }
    }
    fn end(&mut self, _ctx: &mut Ctx<'_>, local: &str, _anc: &[String]) {
        if local == "property"
            && let Some(n) = self.name.take()
            && self.pairs.len() < 512
        {
            self.pairs.push((n, core::mem::take(&mut self.value)));
        }
    }
}

/// `docProps/custom.xml`.
pub(crate) fn custom(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = CustomVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    if !v.pairs.is_empty() {
        let names: Vec<&str> = v.pairs.iter().map(|(n, _)| n.as_str()).take(16).collect();
        ctx.report.add(
            ThreatKind::CustomProperties,
            loc,
            format!("{} custom propert(ies): {}", v.pairs.len(), names.join(", ")),
        );
    }
    for (n, val) in &v.pairs {
        ctx.report.meta(MetaKey::CustomProperty, n, val, loc);
    }
    Ok(())
}

struct LabelVisitor;

impl Visitor for LabelVisitor {
    fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &[String]) {
        if xml::local_name(e) == "label" {
            let id = xml::attr(e, "id").unwrap_or_default();
            let site = xml::attr(e, "siteId").unwrap_or_default();
            ctx.report.meta(
                MetaKey::CustomProperty,
                "Sensitivity label",
                &format!("label {id} tenant {site}"),
                "docMetadata/LabelInfo.xml",
            );
        }
    }
}

/// `docMetadata/LabelInfo.xml` (Microsoft Purview sensitivity labels).
pub(crate) fn labels(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    xmlwalk::walk(ctx, bytes, loc, &mut LabelVisitor)?;
    ctx.report.add(
        ThreatKind::CustomProperties,
        loc,
        "sensitivity label with organisation tenant identifier",
    );
    Ok(())
}

#[derive(Default)]
struct PeopleVisitor {
    people: BTreeSet<String>,
    accounts: BTreeSet<String>,
}

impl Visitor for PeopleVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &[String]) {
        match xml::local_name(e) {
            "person" => {
                if let Some(n) = xml::attr(e, "author").or_else(|| xml::attr(e, "displayName")) {
                    self.people.insert(n);
                }
                if let Some(u) = xml::attr(e, "userId") {
                    self.accounts.insert(u);
                }
            }
            "presenceInfo" => {
                if let Some(u) = xml::attr(e, "userId") {
                    self.accounts.insert(u);
                }
            }
            _ => {}
        }
    }
}

/// `word/people.xml`, `xl/persons/person.xml`.
pub(crate) fn people(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = PeopleVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    for p in &v.people {
        ctx.report.meta(MetaKey::Person, "co-author", p, loc);
    }
    for a in &v.accounts {
        ctx.report.meta(MetaKey::Person, "account", a, loc);
    }
    if !v.accounts.is_empty() {
        ctx.report.add(
            ThreatKind::PersonalIdentifier,
            loc,
            format!("{} directory account identifier(s) of co-authors", v.accounts.len()),
        );
    }
    Ok(())
}

#[derive(Default)]
struct CommentsVisitor {
    comments: usize,
    authors: BTreeSet<String>,
    in_author: bool,
    author_text: String,
}

impl Visitor for CommentsVisitor {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _anc: &[String]) {
        match xml::local_name(e) {
            "comment" | "cm" | "threadedComment" => {
                self.comments += 1;
                if let Some(a) = xml::attr(e, "author") {
                    self.authors.insert(a);
                }
            }
            "cmAuthor" => {
                if let Some(a) = xml::attr(e, "name") {
                    self.authors.insert(a);
                }
            }
            "author" => {
                self.in_author = true;
                self.author_text.clear();
            }
            _ => {}
        }
    }
    fn text(&mut self, _ctx: &mut Ctx<'_>, text: &str, _anc: &[String]) {
        if self.in_author && self.author_text.len() < 256 {
            self.author_text.push_str(text);
        }
    }
    fn end(&mut self, _ctx: &mut Ctx<'_>, local: &str, _anc: &[String]) {
        if local == "author" && self.in_author {
            self.in_author = false;
            if !self.author_text.trim().is_empty() && self.authors.len() < 256 {
                self.authors.insert(self.author_text.trim().to_owned());
            }
        }
    }
}

/// Comment parts of all three applications.
pub(crate) fn comments(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    let mut v = CommentsVisitor::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    if v.comments > 0 || !v.authors.is_empty() {
        let authors: Vec<&str> = v.authors.iter().map(String::as_str).collect();
        ctx.report.add(
            ThreatKind::Comments,
            loc,
            format!("{} comment(s) by {}", v.comments, authors.join(", ")),
        );
    }
    for a in &v.authors {
        ctx.report.meta(MetaKey::Person, "commenter", a, loc);
    }
    Ok(())
}

/// `printerSettingsN.bin` holds a Windows `DEVMODEW`; its first 32 UTF-16
/// code units are the printer's device name.
pub(crate) fn printer_settings(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) {
    let name_bytes = bytes.get(..64).unwrap_or(bytes);
    let name = arca_container::text::utf16le_until_nul(name_bytes);
    let name = name.trim();
    if !name.is_empty() && name.chars().all(|c| !c.is_control()) {
        ctx.report.meta(MetaKey::Printer, "", name, loc);
        ctx.report.add(ThreatKind::PrinterMetadata, loc, format!("printer {name:?}"));
        if let Some(host) = url::unc_host(name) {
            ctx.report.meta(MetaKey::Machine, "", &host, loc);
        }
    } else {
        ctx.report.add(ThreatKind::PrinterMetadata, loc, "printer driver settings are stored");
    }
}
