//! What a part of an OPC package *is*.
//!
//! The scanner and the sanitizer decide this in one place, so they can never disagree about
//! it. A part is classified by the first source that has an opinion, in order of authority:
//! what the package **declares** (its content type), what the package **links** to it with
//! (the relationship type that points at it), and only last what the part is **called**. A name
//! is chosen by whoever wrote the file, so it can never turn a part the scanner must read into a
//! part it skips; where the sources disagree the part is scanned under the most authoritative
//! role and the disagreement itself is a finding (see [`Classified::conflict`]).

/// Role of a part.
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
    /// Office add-in (web extension) definitions and task-pane lists.
    WebExtension,
    OtherXml,
    Binary,
}

/// The role a content type declares, if it declares one.
fn by_content_type(ct: &str) -> Option<Role> {
    let ct = ct.to_ascii_lowercase();
    let ct = ct.as_str();
    if ct == "application/vnd.openxmlformats-package.relationships+xml" {
        return Some(Role::Rels);
    }
    if ct == "application/vnd.ms-office.vbaproject" {
        return Some(Role::VbaProject);
    }
    if ct.starts_with("application/vnd.ms-office.vbaprojectsignature") {
        return Some(Role::VbaSignature);
    }
    if ct == "application/vnd.ms-word.vbadata+xml" {
        return Some(Role::VbaData);
    }
    if ct.contains("webextension") {
        return Some(Role::WebExtension);
    }
    if ct.contains("activex") {
        return Some(Role::ActiveX);
    }
    if ct.ends_with("officedocument.oleobject") {
        return Some(Role::Embedding);
    }
    if ct.contains("macrosheet") {
        return Some(Role::MacroSheet);
    }
    if ct.contains("externallink") {
        return Some(Role::ExternalLink);
    }
    if ct.contains("spreadsheetml.connections") {
        return Some(Role::Connections);
    }
    if ct.contains("printersettings") {
        return Some(Role::PrinterSettings);
    }
    if ct == "application/vnd.openxmlformats-package.core-properties+xml" {
        return Some(Role::CoreProps);
    }
    if ct == "application/vnd.openxmlformats-officedocument.extended-properties+xml" {
        return Some(Role::AppProps);
    }
    if ct == "application/vnd.openxmlformats-officedocument.custom-properties+xml" {
        return Some(Role::CustomProps);
    }
    if ct.contains("classificationlabels") {
        return Some(Role::Labels);
    }
    if ct.contains("obfuscatedfont")
        || ct.contains("fontdata")
        || ct.starts_with("application/x-font")
        || ct.starts_with("font/")
    {
        return Some(Role::Font);
    }
    if ct.contains("people+xml") || ct.contains("person+xml") {
        return Some(Role::People);
    }
    if ct.contains("comments") || ct.contains("commentauthors") {
        return Some(Role::Comments);
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
        return Some(Role::WordBody);
    }
    if ct.contains("wordprocessingml.settings") {
        return Some(Role::WordSettings);
    }
    if ct.contains("spreadsheetml.sheet.main")
        || ct.contains("ms-excel.sheet.macroenabled.main")
        || ct.contains("spreadsheetml.template.main")
        || ct.contains("ms-excel.template.macroenabled.main")
        || ct.contains("ms-excel.addin.macroenabled.main")
    {
        return Some(Role::Workbook);
    }
    if ct.contains("spreadsheetml.worksheet")
        || ct.contains("spreadsheetml.dialogsheet")
        || ct.contains("spreadsheetml.chartsheet")
    {
        return Some(Role::Worksheet);
    }
    if ct.contains("presentationml.slide+")
        || ct.contains("presentationml.slidelayout")
        || ct.contains("presentationml.slidemaster")
        || ct.contains("presentationml.notesslide")
        || ct.contains("presentationml.notesmaster")
        || ct.contains("presentationml.handoutmaster")
    {
        return Some(Role::Slide);
    }
    if ct.contains("presentationml.presentation.main")
        || ct.contains("presentationml.slideshow.main")
        || ct.contains("presentationml.template.main")
        || ct.contains("ms-powerpoint.")
    {
        return Some(Role::Presentation);
    }
    if ct.contains("ms-visio.") {
        return Some(Role::Visio);
    }
    if ct.contains("vmldrawing") {
        return Some(Role::Vml);
    }
    if ct == "image/svg+xml" {
        return Some(Role::Svg);
    }
    None
}

/// The role a relationship type (its last URI segment) implies for its target.
pub(crate) fn by_relationship(suffix: &str) -> Option<Role> {
    Some(match suffix {
        "vbaProject" => Role::VbaProject,
        "vbaProjectSignature" | "vbaProjectSignatureAgile" | "vbaProjectSignatureV3" => {
            Role::VbaSignature
        }
        "wordVbaData" => Role::VbaData,
        "oleObject" | "package" => Role::Embedding,
        "xlMacrosheet" | "xlIntlMacrosheet" => Role::MacroSheet,
        "externalLink" => Role::ExternalLink,
        "connections" => Role::Connections,
        "comments" | "commentAuthors" | "threadedComment" => Role::Comments,
        "header" | "footer" | "footnotes" | "endnotes" | "glossaryDocument" => Role::WordBody,
        "settings" => Role::WordSettings,
        "slide" | "slideLayout" | "slideMaster" | "notesSlide" | "notesMaster"
        | "handoutMaster" => Role::Slide,
        "webextension" | "webextensiontaskpanes" => Role::WebExtension,
        _ => return None,
    })
}

/// The role a part's name suggests: only names office software gives a fixed meaning to.
fn by_path(name: &str) -> Option<Role> {
    let n = name.to_ascii_lowercase();
    let file = n.rsplit('/').next().unwrap_or(&n);
    if n.ends_with(".rels") {
        return Some(Role::Rels);
    }
    if file == "vbaproject.bin" {
        return Some(Role::VbaProject);
    }
    if file.starts_with("vbaprojectsignature") {
        return Some(Role::VbaSignature);
    }
    if file == "vbadata.xml" {
        return Some(Role::VbaData);
    }
    if n.contains("webextensions/") {
        return Some(Role::WebExtension);
    }
    if n.contains("activex/") {
        return Some(Role::ActiveX);
    }
    if n.contains("embeddings/") {
        return Some(Role::Embedding);
    }
    if n.starts_with("customui/") || n.contains("/customui/") {
        return Some(Role::RibbonUi);
    }
    if n.contains("macrosheets/") {
        return Some(Role::MacroSheet);
    }
    if n.contains("externallinks/") {
        return Some(Role::ExternalLink);
    }
    if file == "connections.xml" {
        return Some(Role::Connections);
    }
    if n.contains("printersettings/") {
        return Some(Role::PrinterSettings);
    }
    if n == "docprops/core.xml" {
        return Some(Role::CoreProps);
    }
    if n == "docprops/app.xml" {
        return Some(Role::AppProps);
    }
    if n == "docprops/custom.xml" {
        return Some(Role::CustomProps);
    }
    if n.ends_with("labelinfo.xml") {
        return Some(Role::Labels);
    }
    if n.starts_with("customxml/") {
        return Some(Role::CustomXml);
    }
    if file.ends_with(".odttf") || file.ends_with(".fntdata") {
        return Some(Role::Font);
    }
    if file == "people.xml" || n.contains("persons/") {
        return Some(Role::People);
    }
    if file.starts_with("comments") {
        return Some(Role::Comments);
    }
    if n.starts_with("docprops/thumbnail") {
        return Some(Role::Thumbnail);
    }
    if file.ends_with(".vml") {
        return Some(Role::Vml);
    }
    if file.ends_with(".svg") {
        return Some(Role::Svg);
    }
    None
}

/// The outcome of classifying one part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Classified {
    /// The role the part is scanned (and sterilised) as.
    pub(crate) role: Role,
    /// A role another source claims for the part, when it disagrees with `role`.
    pub(crate) conflict: Option<Role>,
}

/// Decides the role of the part `name` whose declared content type is `ct` and which the
/// relationships of the package link to as `rel` (see [`by_relationship`]).
pub(crate) fn classify(name: &str, ct: &str, rel: Option<Role>) -> Classified {
    let declared = by_content_type(ct);
    let named = by_path(name);
    let role = declared.or(rel).or(named).unwrap_or_else(|| generic(name, ct));
    let conflict = [declared, rel, named].into_iter().flatten().find(|r| *r != role);
    Classified { role, conflict }
}

/// The role of a part nothing specific is known about.
fn generic(name: &str, ct: &str) -> Role {
    let n = name.to_ascii_lowercase();
    let ct = ct.to_ascii_lowercase();
    if ct.ends_with("+xml") || ct.ends_with("/xml") || n.ends_with(".xml") {
        Role::OtherXml
    } else {
        Role::Binary
    }
}

/// The role of `name` judged by content type and name alone.
#[cfg(test)]
pub(crate) fn role(name: &str, ct: &str) -> Role {
    classify(name, ct, None).role
}

/// `true` for media types of parts that office software only ever displays or plays: a part of
/// such a type needs no interpretation beyond checking what its bytes are.
pub(crate) fn is_inert_media(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ["image/", "audio/", "video/", "font/"].iter().any(|p| ct.starts_with(p))
        || matches!(
            ct.as_str(),
            "application/x-msmetafile"
                | "application/x-font-ttf"
                | "application/x-fontdata"
                | "application/vnd.ms-fontobject"
        )
}
