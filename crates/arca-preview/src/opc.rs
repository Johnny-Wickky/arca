//! Minimal Open Packaging Conventions reader: parts, content types, relationships.

use std::borrow::Cow;
use std::collections::BTreeMap;

use arca_container::InflateBudget;
use arca_container::zip::{ZipArchive, ZipError};

use crate::xml::{self, Ev, XmlError};
use crate::{PreviewError, PreviewPolicy};

/// One relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rel {
    pub(crate) id: String,
    pub(crate) rtype: String,
    pub(crate) target: String,
    pub(crate) external: bool,
}

impl Rel {
    /// Last segment of the relationship type URI.
    pub(crate) fn suffix(&self) -> &str {
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
    pub(crate) fn of(&self, part: &str) -> &str {
        let key = part.trim_start_matches('/').to_ascii_lowercase();
        if let Some(ct) = self.overrides.get(&key) {
            return ct;
        }
        let ext = key.rsplit('/').next().and_then(|f| f.rsplit_once('.')).map_or("", |(_, e)| e);
        self.defaults.get(ext).map_or("", String::as_str)
    }
}

/// An opened package.
pub(crate) struct Package<'a> {
    zip: ZipArchive<'a>,
    budget: InflateBudget,
    xml_limits: arca_container::xml::XmlLimits,
    types: ContentTypes,
    rels: BTreeMap<String, Vec<Rel>>,
    /// Parts that were present but could not be read (display-safe names).
    pub(crate) skipped: Vec<String>,
}

fn zip_failure(e: &ZipError) -> PreviewError {
    PreviewError::Malformed(format!("archive: {e}"))
}

fn xml_failure(part: &str, e: &XmlError) -> PreviewError {
    PreviewError::Malformed(format!("{}: {e}", arca_container::text::display_safe(part, 128)))
}

impl<'a> Package<'a> {
    /// Opens an already parsed archive as a package.
    pub(crate) fn new(zip: ZipArchive<'a>, policy: &PreviewPolicy) -> Result<Self, PreviewError> {
        let mut pkg = Self {
            zip,
            budget: InflateBudget::new(policy.inflate_budget, policy.aggregate_ratio, 16 << 20),
            xml_limits: policy.xml,
            types: ContentTypes::default(),
            rels: BTreeMap::new(),
            skipped: Vec::new(),
        };
        pkg.load_content_types()?;
        Ok(pkg)
    }

    /// Reads a part.
    ///
    /// Absent, encrypted and unsupported members yield `Ok(None)` (recorded in
    /// [`Package::skipped`]); structural attacks (bombs, lying sizes) are errors.
    pub(crate) fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, PreviewError> {
        let Some(entry) = self.zip.entry(name).cloned() else { return Ok(None) };
        match self.zip.read(&entry, &mut self.budget) {
            Ok(data) => Ok(Some(data)),
            Err(ZipError::Encrypted(_) | ZipError::UnsupportedCompression { .. }) => {
                self.skipped.push(arca_container::text::display_safe(name, 128));
                Ok(None)
            }
            Err(e) => Err(zip_failure(&e)),
        }
    }

    /// A stable number for a member: its position in the central directory. It is what a picture
    /// is later asked for by (see [`Package::read_by_id`]).
    pub(crate) fn entry_id(&self, name: &str) -> Option<u32> {
        let entry = self.zip.entry(name)?;
        let at = self.zip.entries().iter().position(|e| std::ptr::eq(e, entry))?;
        u32::try_from(at).ok()
    }

    /// The member with the number `id` (see [`Package::entry_id`]): its name and bytes.
    pub(crate) fn read_by_id(
        &mut self,
        id: u32,
    ) -> Result<Option<(String, Vec<u8>)>, PreviewError> {
        let Some(entry) = usize::try_from(id).ok().and_then(|i| self.zip.entries().get(i)).cloned()
        else {
            return Ok(None);
        };
        let name = entry.name().to_owned();
        Ok(self.read(&name)?.map(|bytes| (name, bytes)))
    }

    /// Size of a member without reading it.
    pub(crate) fn size_of(&self, name: &str) -> Option<u64> {
        self.zip.entry(name).map(arca_container::zip::ZipEntry::uncompressed_size)
    }

    /// Content type of a part.
    pub(crate) fn content_type(&self, part: &str) -> &str {
        self.types.of(part)
    }

    /// Decodes a part's bytes as XML text.
    pub(crate) fn decode<'b>(
        &self,
        part: &str,
        bytes: &'b [u8],
    ) -> Result<Cow<'b, str>, PreviewError> {
        xml::decode(bytes, &self.xml_limits).map_err(|e| xml_failure(part, &e))
    }

    /// XML limits in force.
    pub(crate) const fn limits(&self) -> &arca_container::xml::XmlLimits {
        &self.xml_limits
    }

    fn load_content_types(&mut self) -> Result<(), PreviewError> {
        let Some(bytes) = self.read("[Content_Types].xml")? else { return Ok(()) };
        let text = self.decode("[Content_Types].xml", &bytes)?;
        let mut rd = xml::Reader::new(&text, &self.xml_limits)
            .map_err(|e| xml_failure("[Content_Types]", &e))?;
        let mut types = ContentTypes::default();
        loop {
            match rd.next() {
                Ok(Some(Ev::Start(t) | Ev::Empty(t))) => {
                    let ctype = t.attr("ContentType").unwrap_or_default().trim().to_owned();
                    match t.local() {
                        "Default" => {
                            if let Some(ext) = t.attr("Extension") {
                                types.defaults.insert(ext.to_ascii_lowercase(), ctype);
                            }
                        }
                        "Override" => {
                            if let Some(part) = t.attr("PartName") {
                                types.overrides.insert(
                                    part.trim_start_matches('/').to_ascii_lowercase(),
                                    ctype,
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    self.skipped.push("[Content_Types].xml".to_owned());
                    let _ = e;
                    break;
                }
            }
        }
        self.types = types;
        Ok(())
    }

    /// Relationships whose source is `part` (`""` for the package itself).
    pub(crate) fn rels_of(&mut self, part: &str) -> Result<Vec<Rel>, PreviewError> {
        let key = part.trim_start_matches('/').to_ascii_lowercase();
        if let Some(cached) = self.rels.get(&key) {
            return Ok(cached.clone());
        }
        let rels_name = rels_path(&key);
        let mut out = Vec::new();
        if let Some(bytes) = self.read(&rels_name)? {
            let text = self.decode(&rels_name, &bytes)?;
            if let Ok(mut rd) = xml::Reader::new(&text, &self.xml_limits) {
                while let Ok(Some(ev)) = rd.next() {
                    if let Ev::Start(t) | Ev::Empty(t) = ev
                        && t.local() == "Relationship"
                    {
                        out.push(Rel {
                            id: t.attr("Id").unwrap_or_default().into_owned(),
                            rtype: t.attr("Type").unwrap_or_default().into_owned(),
                            target: t.attr("Target").unwrap_or_default().into_owned(),
                            external: t
                                .attr("TargetMode")
                                .is_some_and(|m| m.eq_ignore_ascii_case("external")),
                        });
                    }
                }
            }
        }
        self.rels.insert(key, out.clone());
        Ok(out)
    }

    /// The first internal relationship of `part` whose type ends with `suffix`.
    pub(crate) fn rel_target(
        &mut self,
        part: &str,
        suffix: &str,
    ) -> Result<Option<String>, PreviewError> {
        Ok(self
            .rels_of(part)?
            .into_iter()
            .find(|r| !r.external && r.suffix() == suffix)
            .and_then(|r| resolve(part, &r.target)))
    }

    /// All internal targets of `part` whose relationship type ends with `suffix`.
    pub(crate) fn rel_targets(
        &mut self,
        part: &str,
        suffix: &str,
    ) -> Result<Vec<String>, PreviewError> {
        Ok(self
            .rels_of(part)?
            .into_iter()
            .filter(|r| !r.external && r.suffix() == suffix)
            .filter_map(|r| resolve(part, &r.target))
            .collect())
    }

    /// Resolves relationship `id` of `part` to `(target, external)`.
    pub(crate) fn rel_by_id(
        &mut self,
        part: &str,
        id: &str,
    ) -> Result<Option<(String, bool)>, PreviewError> {
        Ok(self.rels_of(part)?.into_iter().find(|r| r.id == id).map(|r| {
            if r.external {
                (r.target, true)
            } else {
                (resolve(part, &r.target).unwrap_or(r.target), false)
            }
        }))
    }

    /// The main document part named by the package relationships.
    pub(crate) fn main_part(&mut self) -> Result<Option<String>, PreviewError> {
        for r in self.rels_of("")? {
            if !r.external
                && matches!(r.suffix(), "officeDocument" | "document" | "FixedRepresentation")
                && let Some(p) = resolve("", &r.target)
            {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }
}

/// `word/document.xml` → `word/_rels/document.xml.rels`; `""` → `_rels/.rels`.
pub(crate) fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None if part.is_empty() => "_rels/.rels".to_owned(),
        None => format!("_rels/{part}.rels"),
    }
}

/// Resolves `target` against the directory of `source` (a part name; `""` = package root).
///
/// Returns `None` for targets that climb out of the package.
pub(crate) fn resolve(source: &str, target: &str) -> Option<String> {
    let target = target.split(['#', '?']).next().unwrap_or(target);
    let base: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut dirs: Vec<&str> = source.trim_start_matches('/').split('/').collect();
        dirs.pop();
        dirs
    };
    let mut parts = base;
    for seg in target.trim_start_matches('/').split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() { None } else { Some(parts.join("/")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rels_paths() {
        assert_eq!(rels_path("word/document.xml"), "word/_rels/document.xml.rels");
        assert_eq!(rels_path(""), "_rels/.rels");
        assert_eq!(rels_path("a.xml"), "_rels/a.xml.rels");
    }

    #[test]
    fn target_resolution() {
        assert_eq!(resolve("word/document.xml", "styles.xml").as_deref(), Some("word/styles.xml"));
        assert_eq!(
            resolve("word/document.xml", "../docProps/a.xml").as_deref(),
            Some("docProps/a.xml")
        );
        assert_eq!(resolve("", "/word/document.xml").as_deref(), Some("word/document.xml"));
        assert_eq!(
            resolve("ppt/slides/slide1.xml", "../media/i.png").as_deref(),
            Some("ppt/media/i.png")
        );
        assert_eq!(resolve("a/b.xml", "../../../etc/passwd"), None);
        assert_eq!(resolve("a/b.xml", "c.xml#frag").as_deref(), Some("a/c.xml"));
        assert_eq!(resolve("a.xml", ""), None);
    }
}
