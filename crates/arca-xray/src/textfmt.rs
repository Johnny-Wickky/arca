//! Plain-text family: TXT, CSV/TSV, SYLK, Markdown, flat XML documents and HTML.
//!
//! Text is not inert merely because it is text: a spreadsheet program turns `=cmd|...` in a CSV
//! cell into a DDE call, a browser engine runs the markup in a note, Windows runs an `.url` or
//! `.scf` file and a script host runs a `.vbs`. The file name is never consulted, so every kind
//! is recognised by what it says, and text that is *recognised* gets the scanner of its kind.
//! Text whose kind is not understood (an XML document with an unknown root element) is not
//! passed as clean.

use arca_container::xml::{self, XmlLimits};
use quick_xml::events::BytesStart;

use crate::format::{FileFormat, classify_text};
use crate::ooxml::{self, Rel};
use crate::report::{RefKind, Severity, ThreatKind};
use crate::xmlwalk::{self, Ancestors, Visitor};
use crate::{Abort, Ctx, formulas, has_bidi, memfind_ci, odf};

/// How much of a plain text file is searched for script markers.
const SCRIPT_WINDOW: usize = 256 << 10;
/// Decoded size accepted from one base64 blob in a flat XML document.
const MAX_BLOB: usize = 64 << 20;

/// Scans text content.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) -> Result<FileFormat, Abort> {
    let limits = XmlLimits { max_input_bytes: ctx.policy.max_input_bytes, ..ctx.policy.xml };
    let text = match xml::decode_xml_bytes(data, &limits) {
        Ok(t) => t,
        Err(_) => {
            // Not UTF-8: legacy 8-bit text if it looks like it, otherwise lossy.
            let decoded = match arca_container::text::decode_legacy_text(data) {
                Some((_, text)) => text,
                None => String::from_utf8_lossy(data).into_owned(),
            };
            std::borrow::Cow::Owned(decoded)
        }
    };
    if let Some(what) = executable_text(&text) {
        return executable(ctx, what, loc);
    }
    let mut format = classify_text(&text);
    if text.starts_with("ID;") {
        format = FileFormat::Csv;
    }
    ctx.identified(format);
    if has_bidi(&text) {
        ctx.report.add(ThreatKind::BidiOverride, loc, "text contains bidirectional override characters (content may display differently than it reads)");
    }
    // Markup is looked for in every kind of text: a note, a CSV cell or a Markdown file can
    // all carry `<script>` or `onload=`, and which kind a file was taken for is the author's
    // choice.
    scan_markup(ctx, &text, loc);
    match format {
        FileFormat::Csv | FileFormat::Tsv => {
            if text.starts_with("ID;") {
                sylk(ctx, &text, loc);
            } else {
                csv(
                    ctx,
                    &text,
                    if format == FileFormat::Tsv { Sep::Tabs } else { Sep::Commas },
                    loc,
                );
            }
        }
        FileFormat::Markdown => markdown(ctx, &text, loc),
        FileFormat::FlatXml => flat_xml(ctx, &text, loc)?,
        FileFormat::PlainText => {
            script_markers(ctx, data, loc);
            // Quoted fields make a table look irregular to the classifier, and a formula is a
            // formula whatever the file was taken for.
            csv(ctx, &text, Sep::Any, loc);
        }
        _ => {}
    }
    Ok(format)
}

/// Text files that a shell or a script host runs, or that make Windows fetch something, judged
/// by their content.
fn executable_text(text: &str) -> Option<&'static str> {
    let head = arca_container::text::truncate_utf8(text, 4096);
    let trimmed = head.trim_start_matches('\u{feff}').trim_start();
    let lower: String = trimmed.chars().take(1024).collect::<String>().to_ascii_lowercase();
    if lower.starts_with("[internetshortcut") || lower.contains("\n[internetshortcut]") {
        return Some("Internet shortcut (.url)");
    }
    if lower.starts_with("[shell]") {
        return Some("Windows Explorer command file (.scf)");
    }
    if lower.starts_with("[theme]") {
        return Some("Windows theme file (.theme)");
    }
    if lower.lines().next().is_some_and(|l| l.trim() == "web") {
        return Some("Excel web query (.iqy)");
    }
    if lower.starts_with("<") {
        let root = first_element(trimmed)?.to_ascii_lowercase();
        let root = root.rsplit(':').next().unwrap_or(&root);
        let all = trimmed.to_ascii_lowercase();
        return match root {
            "librarydescription" => Some("library description (.library-ms)"),
            "pcsettings" => Some("settings shortcut (.settingcontent-ms)"),
            "scriptlet" => Some("Windows script component (.sct)"),
            "jnlp" => Some("Java Web Start file (.jnlp)"),
            "job" | "package" if all.contains("<script") => Some("Windows Script File (.wsf)"),
            _ => None,
        };
    }
    None
}

/// Reports a file that is executable or shortcut-like text: at the top level it is not a
/// document at all; inside another file it is an embedded executable.
fn executable(ctx: &mut Ctx<'_>, what: &str, loc: &str) -> Result<FileFormat, Abort> {
    if ctx.depth == 0 {
        Err(Abort::new(
            ThreatKind::DisguisedExecutable,
            loc,
            format!("{what} disguised as a document"),
        ))
    } else {
        ctx.report.add(ThreatKind::EmbeddedExecutable, loc, what);
        Ok(FileFormat::Executable)
    }
}

/// Name of the first element of an XML or markup text, skipping the prolog, comments,
/// processing instructions and the document type declaration.
pub(crate) fn first_element(text: &str) -> Option<&str> {
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.get(r.find("-->")? + 3..)?;
        } else if let Some(r) = rest.strip_prefix("<?") {
            rest = r.get(r.find("?>")? + 2..)?;
        } else if let Some(r) = rest.strip_prefix("<!") {
            rest = r.get(r.find('>')? + 1..)?;
        } else {
            break;
        }
    }
    let r = rest.strip_prefix('<')?;
    let end = r.find(|c: char| c.is_whitespace() || matches!(c, '>' | '/'))?;
    r.get(..end)
}

// ---- CSV, TSV and SYLK ---------------------------------------------------------------------

/// Calls `f(line, field)` for every field of a delimited text. Quotes follow RFC 4180: a
/// quoted field may contain separators, line breaks and doubled quotes.
/// Which characters separate cells.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sep {
    Commas,
    Tabs,
    /// Any of them: text of unknown shape is read as the table it might be.
    Any,
}

fn for_each_field(text: &str, sep: Sep, mut f: impl FnMut(usize, &str)) {
    const MAX_LINES: usize = 5_000_000;
    const MAX_FIELD: usize = 64 << 10;
    let mut chars = text.chars().peekable();
    let mut field = String::new();
    let mut quoted = false;
    let mut line = 1usize;
    let mut field_line = 1usize;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => {
                    if c == '\n' {
                        line += 1;
                    }
                    if field.len() < MAX_FIELD {
                        field.push(c);
                    }
                }
            }
            continue;
        }
        let is_sep = match sep {
            Sep::Tabs => c == '\t',
            Sep::Commas => c == ',' || c == ';',
            Sep::Any => c == ',' || c == ';' || c == '\t',
        };
        if c == '"' && field.trim().is_empty() {
            quoted = true;
            field.clear();
        } else if is_sep || c == '\n' {
            f(field_line, &field);
            field.clear();
            if c == '\n' {
                line += 1;
                if line > MAX_LINES {
                    return;
                }
            }
            field_line = line;
        } else if c != '\r' && field.len() < MAX_FIELD {
            field.push(c);
        }
    }
    f(field_line, &field);
}

/// Collected findings about the cells that start with a formula character.
#[derive(Default)]
struct Cells {
    dangerous: usize,
    plain: usize,
    sample: String,
}

impl Cells {
    /// Judges one cell. `line` is only used for the example in the report.
    fn cell(&mut self, ctx: &mut Ctx<'_>, line: usize, raw: &str, loc: &str) {
        let c = raw.trim();
        let Some(first) = c.chars().next() else { return };
        if !matches!(first, '=' | '+' | '-' | '@') {
            return;
        }
        let body = c.trim_start_matches(['=', '+', '-', '@']);
        // Negative numbers and phone numbers are not formulas — unless a function that
        // reaches out is called in what follows (`=1+WEBSERVICE(...)`, `+1|cmd!A0`).
        let numeric = body
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit() || ch == '.' || ch.is_whitespace());
        if numeric && !body.contains('|') && !formulas::may_contain_risk(body) {
            return;
        }
        formulas::record_urls(ctx, body, loc);
        if formulas::assess(body).is_empty() {
            self.plain += 1;
        } else {
            self.dangerous += 1;
            if self.sample.is_empty() {
                self.sample =
                    format!("line {line}: {}", arca_container::text::display_safe(c, 120));
            }
        }
    }

    fn report(&self, ctx: &mut Ctx<'_>, loc: &str) {
        if self.dangerous > 0 {
            ctx.report.add(
                ThreatKind::CsvFormulaInjection,
                loc,
                format!(
                    "{} cell(s) with DDE / command formulas, e.g. {}",
                    self.dangerous, self.sample
                ),
            );
        }
        if self.plain > 0 {
            ctx.report.add_with(
                ThreatKind::CsvFormulaInjection,
                Severity::Low,
                loc,
                format!(
                    "{} cell(s) start with a formula character (shown as text, never evaluated)",
                    self.plain
                ),
            );
        }
    }
}

fn csv(ctx: &mut Ctx<'_>, text: &str, sep: Sep, loc: &str) {
    let mut cells = Cells::default();
    for_each_field(text, sep, |line, field| cells.cell(ctx, line, field, loc));
    if sep == Sep::Any {
        // Only what is dangerous is worth saying about text that merely might be a table.
        cells.plain = 0;
    }
    cells.report(ctx, loc);
}

/// SYLK: `C;X1;Y1;K"=cmd|'/c calc'!A0"` and `C;E=...` records are formulas.
fn sylk(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    let mut cells = Cells::default();
    for (n, line) in text.lines().enumerate().take(5_000_000) {
        if !line.starts_with("C;") && !line.starts_with("F;") {
            continue;
        }
        // Split at ';' outside double quotes.
        let mut quoted = false;
        let mut start = 0usize;
        let mut pieces: Vec<&str> = Vec::new();
        for (i, ch) in line.char_indices() {
            match ch {
                '"' => quoted = !quoted,
                ';' if !quoted => {
                    pieces.extend(line.get(start..i));
                    start = i + 1;
                }
                _ => {}
            }
        }
        pieces.extend(line.get(start..));
        for p in pieces {
            if let Some(expr) = p.strip_prefix('E') {
                // An expression is a formula whether or not it starts with `=`.
                cells.cell(ctx, n + 1, &format!("={}", expr.trim_start_matches('=')), loc);
            } else if let Some(value) = p.strip_prefix('K') {
                cells.cell(ctx, n + 1, value.trim_matches('"'), loc);
            }
        }
    }
    cells.report(ctx, loc);
}

fn markdown(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    let mut images = 0usize;
    let mut i = 0usize;
    while let Some(rel) = text.get(i..).and_then(|t| t.find("![")) {
        let at = i + rel;
        i = at + 2;
        let Some(open) = text.get(at..).and_then(|t| t.find("](")).map(|p| at + p + 2) else {
            continue;
        };
        let Some(close) = text.get(open..).and_then(|t| t.find(')')).map(|p| open + p) else {
            continue;
        };
        let target = text.get(open..close).unwrap_or("").split_whitespace().next().unwrap_or("");
        if crate::url::scheme(target).is_some() && images < 256 {
            images += 1;
            ooxml::record_external(ctx, target, RefKind::Image, true, loc);
        }
        i = close.max(i);
    }
}

// ---- scripts ---------------------------------------------------------------------------------

/// Phrases that appear in script code and almost never in prose (lower case).
const SCRIPT_MARKERS: &[&str] = &[
    "wscript.shell",
    "wscript.echo",
    "createobject(\"",
    "new activexobject",
    "scripting.filesystemobject",
    "powershell -",
    "powershell.exe",
    "invoke-expression",
    "iex(",
    "iex (",
    "start-process",
    "downloadstring(",
    "downloadfile(",
    "net.webclient",
    "-encodedcommand",
    "set-executionpolicy",
    "[system.reflection.assembly]",
    "@echo off",
    "cmd /c ",
    "cmd.exe /c",
    "regsvr32 ",
    "mshta ",
    "certutil -urlcache",
    "bitsadmin /transfer",
    "schtasks /create",
];

/// A plain text file full of script code is script code (`.vbs`, `.js`, `.bat`, `.ps1` are
/// told apart by name only, and the name is not looked at).
fn script_markers(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) {
    let window = data.get(..data.len().min(SCRIPT_WINDOW)).unwrap_or(data);
    let hits: Vec<&str> = SCRIPT_MARKERS
        .iter()
        .copied()
        .filter(|m| memfind_ci(window, m.as_bytes()).is_some())
        .collect();
    if !hits.is_empty() {
        ctx.report.add(
            ThreatKind::EmbeddedScript,
            loc,
            format!(
                "text contains script code ({}): VBScript, JScript, batch or PowerShell",
                hits.iter().take(4).copied().collect::<Vec<_>>().join(", ")
            ),
        );
    }
}

// ---- markup ----------------------------------------------------------------------------------

/// Scans markup in any text: HTML, but also an HTML fragment in a note or a Markdown file.
pub(crate) fn scan_html(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    scan_markup(ctx, text, loc);
}

fn starts_with_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.get(..needle.len()).is_some_and(|h| h.eq_ignore_ascii_case(needle))
}

/// `true` if `b` cannot continue a tag or attribute name.
fn name_ends(b: Option<&u8>) -> bool {
    !b.is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}

/// One pass over `text` for the constructs that make markup active: script-bearing elements,
/// event-handler attributes, script URLs and meta redirects. External pictures are recorded too.
fn scan_markup(ctx: &mut Ctx<'_>, text: &str, loc: &str) {
    const TAGS: &[(&[u8], &str)] = &[
        (b"script", "<script> element"),
        (b"iframe", "<iframe> element"),
        (b"frame", "<frame> element"),
        (b"object", "<object> element"),
        (b"embed", "<embed> element"),
        (b"applet", "<applet> element"),
        (b"hta:application", "HTA application declaration"),
    ];
    let bytes = text.as_bytes();
    let mut seen: Vec<&'static str> = Vec::new();
    let mut note = |what: &'static str| {
        if !seen.contains(&what) {
            seen.push(what);
        }
    };
    let mut images = 0usize;
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'<' => {
                let rest = bytes.get(i + 1..).unwrap_or(&[]);
                for (name, what) in TAGS {
                    if starts_with_ci(rest, name) && name_ends(rest.get(name.len())) {
                        note(what);
                    }
                }
                let is_tag =
                    |name: &[u8]| starts_with_ci(rest, name) && name_ends(rest.get(name.len()));
                if is_tag(b"meta") || is_tag(b"img") {
                    let tag_end = rest
                        .iter()
                        .take(4096)
                        .position(|&c| c == b'>')
                        .unwrap_or(rest.len().min(4096));
                    let tag = rest.get(..tag_end).unwrap_or(&[]);
                    if is_tag(b"meta") && memfind_ci(tag, b"refresh").is_some() {
                        note("meta refresh redirect");
                    }
                    if is_tag(b"img") && images < 256 {
                        let tag = String::from_utf8_lossy(tag);
                        if let Some(src) = xmlwalk::pseudo_attr(&tag, "src")
                            && crate::url::leaves_package(&src)
                        {
                            images += 1;
                            ooxml::record_external(ctx, &src, RefKind::Image, true, loc);
                        }
                    }
                }
            }
            b'=' => {
                // `onload=`, `onerror =`: an identifier starting with "on" before the equals sign.
                let mut end = i;
                while end > 0 && bytes.get(end - 1).is_some_and(|c| c.is_ascii_whitespace()) {
                    end -= 1;
                }
                let mut start = end;
                while start > 0 && bytes.get(start - 1).is_some_and(u8::is_ascii_alphabetic) {
                    start -= 1;
                }
                let ident = bytes.get(start..end).unwrap_or(&[]);
                let boundary = start == 0
                    || bytes.get(start - 1).is_some_and(|c| {
                        c.is_ascii_whitespace() || matches!(c, b'"' | b'\'' | b'/')
                    });
                if ident.len() >= 4 && boundary && starts_with_ci(ident, b"on") {
                    note("event handler attribute (on...=)");
                }
            }
            b':' => {
                // `javascript:` / `vbscript:` possibly interrupted by tabs or line breaks.
                let mut word = String::new();
                let mut j = i;
                while j > 0 && word.len() < 10 {
                    j -= 1;
                    match bytes.get(j) {
                        Some(c) if c.is_ascii_alphabetic() => {
                            word.insert(0, char::from(c.to_ascii_lowercase()))
                        }
                        Some(b'\t' | b'\n' | b'\r' | 0) => {}
                        _ => break,
                    }
                }
                if word.ends_with("javascript") {
                    note("javascript: URL");
                } else if word.ends_with("vbscript") {
                    note("vbscript: URL");
                }
            }
            _ => {}
        }
        i += 1;
    }
    for what in seen {
        ctx.report.add(ThreatKind::HtmlActiveContent, loc, what);
    }
}

// ---- flat XML --------------------------------------------------------------------------------

/// Remembers the root element of an XML document.
#[derive(Default)]
struct RootProbe {
    name: Option<String>,
    namespaces: Vec<String>,
}

impl Visitor for RootProbe {
    fn start(&mut self, _ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, anc: &Ancestors) {
        if anc.is_empty() && self.name.is_none() {
            let full: &str = e.name().into_inner();
            self.name = Some(full.to_owned());
            for (k, v) in xml::attrs(e) {
                if (k == "xmlns" || k.starts_with("xmlns:")) && self.namespaces.len() < 32 {
                    self.namespaces.push(v);
                }
            }
        }
    }
}

/// Dispatches a flat XML document to the scanners of the kind of document it is.
fn flat_xml(ctx: &mut Ctx<'_>, text: &str, loc: &str) -> Result<(), Abort> {
    let mut probe = RootProbe::default();
    if !xmlwalk::walk(ctx, text.as_bytes(), loc, &mut probe)? {
        return Ok(());
    }
    let Some(name) = probe.name else { return Ok(()) };
    let (prefix, local) = xml::split_qname(&name);
    let has_ns = |needle: &str| probe.namespaces.iter().any(|n| n.contains(needle));
    let bytes = text.as_bytes();
    match (prefix, local) {
        (_, "svg") => ooxml::scan_svg(ctx, bytes, loc)?,
        // RDF metadata (ODF `manifest.rdf`): statements about the document, nothing that runs.
        (_, "RDF") => {}
        (Some("pkg"), "package") => scan_flat_opc(ctx, bytes, loc)?,
        (_, "package") if has_ns("2006/xmlPackage") => scan_flat_opc(ctx, bytes, loc)?,
        (_, "wordDocument") | (Some("w"), "document") => {
            scan_word_xml(ctx, bytes, loc)?;
        }
        (_, "document" | "document-content" | "document-styles" | "document-meta")
            if prefix == Some("office") || has_ns("opendocument") =>
        {
            odf::scan_flat(ctx, bytes, loc)?;
        }
        (_, "Workbook") if has_ns("office:spreadsheet") => scan_excel_xml(ctx, bytes, loc)?,
        _ => ctx.report.add(
            ThreatKind::ScanLimitReached,
            loc,
            format!(
                "XML document with root element <{}> is not a recognised kind of document; its content was not interpreted",
                arca_container::text::display_safe(&name, 64)
            ),
        ),
    }
    Ok(())
}

/// Word 2003 XML and WordprocessingML written without a package: the body scanner reads it,
/// with the base64 blobs (`w:binData`) scanned as the files they are.
fn scan_word_xml(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    ooxml::scan_word_body(ctx, bytes, loc)?;
    scan_blobs(ctx, bytes, loc)
}

/// The XML Spreadsheet 2003 format keeps formulas in `ss:Formula` attributes.
fn scan_excel_xml(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    struct V {
        loc: String,
    }
    impl Visitor for V {
        fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _a: &Ancestors) {
            for (k, v) in xml::attrs(e) {
                let local = xml::split_qname(&k).1;
                if local == "Formula" {
                    formulas::report(ctx, v.trim_start_matches('='), &self.loc);
                } else if local == "Name" && xml::local_name(e) == "NamedRange" {
                    let n = v.to_ascii_lowercase();
                    if n.starts_with("auto_open") || n.starts_with("auto_close") {
                        ctx.report.add(
                            ThreatKind::XlmAutoOpenName,
                            &self.loc,
                            format!("defined name {v:?} (runs a macro on open)"),
                        );
                    }
                }
            }
        }
    }
    xmlwalk::walk(ctx, bytes, loc, &mut V { loc: loc.to_owned() })?;
    scan_blobs(ctx, bytes, loc)
}

/// Flat OPC: every part of a package in one XML file. The part scanners run over the whole
/// text (their element names do not depend on where the part sits), relationships are
/// inspected, and base64 parts are decoded and scanned.
fn scan_flat_opc(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    struct Rels {
        loc: String,
    }
    impl Visitor for Rels {
        fn start(&mut self, ctx: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _a: &Ancestors) {
            if xml::local_name(e) == "Relationship" {
                let external = xml::attrs(e).iter().any(|(k, v)| {
                    xml::split_qname(k).1 == "TargetMode" && v.eq_ignore_ascii_case("External")
                });
                let rel = Rel {
                    id: ooxml::plain_attr(e, "Id").unwrap_or_default(),
                    rtype: ooxml::plain_attr(e, "Type").unwrap_or_default(),
                    target: ooxml::plain_attr(e, "Target").unwrap_or_default(),
                    external,
                };
                ooxml::inspect_relationship(ctx, &rel, "", &self.loc, &|_| false);
            }
        }
    }
    xmlwalk::walk(ctx, bytes, loc, &mut Rels { loc: loc.to_owned() })?;
    scan_word_xml(ctx, bytes, loc)?;
    ooxml::scan_workbook_xml(ctx, bytes, loc)?;
    ooxml::scan_sheet_xml(ctx, bytes, loc)?;
    ooxml::scan_slide_xml(ctx, bytes, loc)?;
    Ok(())
}

/// Decodes the base64 payloads of a flat XML document (`pkg:binaryData`, `w:binData`) and scans
/// them as the files they are.
fn scan_blobs(ctx: &mut Ctx<'_>, bytes: &[u8], loc: &str) -> Result<(), Abort> {
    #[derive(Default)]
    struct V {
        blobs: Vec<(String, String)>,
        current: Option<(String, String)>,
        parts: usize,
    }
    impl Visitor for V {
        fn start(&mut self, _c: &mut Ctx<'_>, e: &BytesStart<'_>, _empty: bool, _a: &Ancestors) {
            if matches!(xml::local_name(e), "binaryData" | "binData") && self.parts < 256 {
                self.parts += 1;
                let name = xml::attr(e, "name").unwrap_or_default();
                self.current = Some((name, String::new()));
            }
        }
        fn text(&mut self, _c: &mut Ctx<'_>, text: &str, _a: &Ancestors) {
            if let Some((_, b64)) = self.current.as_mut()
                && b64.len() < MAX_BLOB * 4 / 3
            {
                b64.push_str(text);
            }
        }
        fn end(&mut self, _c: &mut Ctx<'_>, local: &str, _a: &Ancestors) {
            if matches!(local, "binaryData" | "binData")
                && let Some(b) = self.current.take()
            {
                self.blobs.push(b);
            }
        }
    }
    let mut v = V::default();
    xmlwalk::walk(ctx, bytes, loc, &mut v)?;
    for (n, (name, b64)) in v.blobs.iter().enumerate() {
        let part = format!("{loc}#blob{}", n + 1);
        match base64_decode(b64) {
            Some(data) => ooxml::scan_binary_part(ctx, &data, &part, &blob_type(name))?,
            None => ctx.report.add(
                ThreatKind::MalformedContainer,
                &part,
                "base64 payload could not be decoded, so it was not inspected",
            ),
        }
    }
    Ok(())
}

/// The media type a blob name suggests (only used to tell plain media from the unknown).
fn blob_type(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "emf" | "wmf" | "tif" | "tiff" => {
            format!("image/{ext}")
        }
        _ => String::new(),
    }
}

/// Decodes standard base64, ignoring whitespace. `None` for invalid input or more than
/// [`MAX_BLOB`] decoded bytes.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xFF).ok()?);
            if out.len() > MAX_BLOB {
                return None;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_decode("SGVsbG8=").as_deref(), Some(&b"Hello"[..]));
        assert_eq!(base64_decode("SGVs\nbG8").as_deref(), Some(&b"Hello"[..]));
        assert_eq!(base64_decode("!!"), None);
    }

    #[test]
    fn fields_follow_rfc_4180() {
        let mut got = Vec::new();
        for_each_field("a,\"b,c\",\"d\"\"e\"\n=x;y\n", Sep::Commas, |l, f| {
            got.push((l, f.to_owned()))
        });
        assert_eq!(got[1].1, "b,c");
        assert_eq!(got[2].1, "d\"e");
        assert_eq!(got[3], (2, "=x".to_owned()));
    }

    #[test]
    fn first_element_skips_the_prolog() {
        assert_eq!(
            first_element("<?xml version=\"1.0\"?><!-- c --><!DOCTYPE x><a:b x='1'/>"),
            Some("a:b")
        );
        assert_eq!(first_element("no markup"), None);
    }
}
