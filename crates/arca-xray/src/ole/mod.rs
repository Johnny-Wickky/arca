//! OLE2 compound files: `.doc .xls .ppt .vsd .msg`, encrypted OOXML, MSI,
//! `vbaProject.bin`, OLE objects; plus Access database triage.

mod biff;
mod ppt;
pub(crate) mod propset;
pub(crate) mod vba;
mod visio_binary;
mod word_binary;

use arca_container::bytes::{LeReader, u16_le, u32_le};
use arca_container::cfb::{CfbAnomaly, CompoundFile, EntryKind, clsid_is_null, format_clsid};
use arca_container::text::{CodePage, decode_codepage, utf16le_lossy};

use crate::format::FileFormat;
use crate::report::{EmbeddedKind, EmbeddedObject, MetaKey, RefKind, Severity, ThreatKind};
use crate::{Abort, Ctx, memfind, memfind_ci, ooxml, utf16le_bytes};

/// Well-known CLSIDs with security relevance: `(clsid, description, kind, severity)`.
const CLSIDS: &[(&str, &str, ThreatKind, Severity)] = &[
    (
        "{0002CE02-0000-0000-C000-000000000046}",
        "Microsoft Equation 3.0 (CVE-2017-11882, CVE-2018-0802)",
        ThreatKind::ExploitableOleClass,
        Severity::Critical,
    ),
    (
        "{0003000C-0000-0000-C000-000000000046}",
        "OLE Package (Packager) wrapping a file",
        ThreatKind::OlePackage,
        Severity::High,
    ),
    (
        "{F20DA720-C02F-11CE-927B-0800095AE340}",
        "Packager shell object (CVE-2014-4114)",
        ThreatKind::ExploitableOleClass,
        Severity::Critical,
    ),
    (
        "{79EAC9E0-BAF9-11CE-8C82-00AA004BA90B}",
        "URL moniker (CVE-2017-0199 remote OLE link)",
        ThreatKind::ExternalObjectLink,
        Severity::Critical,
    ),
    (
        "{3050F4D8-98B5-11CF-BB82-00AA00BDCE0B}",
        "htmlfile (CVE-2017-0199)",
        ThreatKind::ExploitableOleClass,
        Severity::Critical,
    ),
    (
        "{8856F961-340A-11D0-A96B-00C04FD705A2}",
        "Shell.Explorer.2 / WebBrowser control",
        ThreatKind::ExploitableOleClass,
        Severity::High,
    ),
    (
        "{D27CDB6E-AE6D-11CF-96B8-444553540000}",
        "Shockwave Flash (CVE-2018-4878 and others)",
        ThreatKind::ExploitableOleClass,
        Severity::High,
    ),
    (
        "{BDD1F04B-858B-11D1-B16A-00C0F0283628}",
        "MSCOMCTL ListView (CVE-2012-0158)",
        ThreatKind::ExploitableOleClass,
        Severity::Critical,
    ),
    (
        "{C74190B6-8589-11D1-B16A-00C0F0283628}",
        "MSCOMCTL TreeView (CVE-2012-0158)",
        ThreatKind::ExploitableOleClass,
        Severity::Critical,
    ),
    (
        "{1EFB6596-857C-11D1-B16A-00C0F0283628}",
        "MSCOMCTL TabStrip (CVE-2012-0158)",
        ThreatKind::ExploitableOleClass,
        Severity::Critical,
    ),
    (
        "{00000300-0000-0000-C000-000000000046}",
        "StdOleLink (linked OLE object)",
        ThreatKind::ExternalObjectLink,
        Severity::High,
    ),
];

/// MSI root storage CLSIDs (installer packages are executables).
const MSI_CLSIDS: &[&str] = &[
    "{000C1084-0000-0000-C000-000000000046}",
    "{000C1086-0000-0000-C000-000000000046}",
    "{000C1082-0000-0000-C000-000000000046}",
];

/// Looks up a CLSID (braced, any case).
pub(crate) fn clsid_info(clsid: &str) -> Option<(&'static str, ThreatKind, Severity)> {
    let c = clsid.trim().to_ascii_uppercase();
    CLSIDS.iter().find(|(k, ..)| *k == c).map(|(_, d, k, s)| (*d, *k, *s))
}

/// Extensions that denote executable or script content.
pub(crate) fn is_dangerous_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "exe"
            | "dll"
            | "scr"
            | "pif"
            | "com"
            | "cpl"
            | "msi"
            | "msp"
            | "bat"
            | "cmd"
            | "ps1"
            | "psm1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "wsh"
            | "hta"
            | "jar"
            | "lnk"
            | "inf"
            | "reg"
            | "sct"
            | "chm"
            | "iso"
            | "img"
            | "vhd"
            | "vhdx"
            | "appx"
            | "msix"
            | "application"
            | "gadget"
            | "url"
            | "settingcontent-ms"
            | "library-ms"
            | "xll"
            | "wll"
            | "scf"
            | "cab"
            | "msc"
            | "py"
            | "sh"
            | "app"
            | "command"
            | "desktop"
            | "apk"
            | "ipa"
    )
}

/// Scans a compound file.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], location: &str) -> Result<FileFormat, Abort> {
    let cf = CompoundFile::parse(data, ctx.policy.cfb).map_err(|e| {
        let kind = match e {
            arca_container::cfb::CfbError::ChainCycle(_)
            | arca_container::cfb::CfbError::CrossLinked(_)
            | arca_container::cfb::CfbError::DirectoryCycle => ThreatKind::ParserDifferential,
            _ => ThreatKind::MalformedContainer,
        };
        Abort::new(kind, location, format!("compound file: {e}"))
    })?;
    // A file is a header sector followed by whole sectors. Bytes after the last whole sector
    // belong to no sector at all (the container counts a partial sector as one), so they are
    // checked here.
    if let Some(shift) = u16_le(data, 30).filter(|s| (7..=16).contains(s)) {
        let sector = 1usize << shift;
        let partial = data.len().saturating_sub(sector) % sector;
        if partial > 0 && data.len() > sector {
            crate::report_trailing(ctx, data, partial, location, "the last whole sector");
        }
    }
    for a in cf.anomalies() {
        match a {
            CfbAnomaly::OrphanedEntries(n) => ctx.report.add(
                ThreatKind::OrphanedOleEntries,
                location,
                format!(
                    "{n} directory entr(ies) unreachable from the root (possible hidden payload)"
                ),
            ),
            CfbAnomaly::TrailingData(n) => {
                let n = usize::try_from(*n).unwrap_or(usize::MAX);
                crate::report_trailing(ctx, data, n, location, "the last sector");
            }
            CfbAnomaly::ChainLongerThanSize(name) => ctx.report.add_with(
                ThreatKind::ParserDifferential,
                Severity::Medium,
                &Ctx::join(location, name),
                "the sector chain of the stream is longer than its declared size; readers that follow the chain see more data than readers that trust the size",
            ),
            CfbAnomaly::SizeHighBitsIgnored(_) => {}
        }
    }

    let root_clsid = cf.entries().first().map(|e| format_clsid(&e.clsid)).unwrap_or_default();
    let has = |name: &str| cf.find(name).is_some();
    // Every kind of document this compound file claims to be. Each is recognised by the streams
    // its own reader opens, so a file can carry the streams of several at once; the readers
    // then disagree about what the file is.
    let is_msi = MSI_CLSIDS.contains(&root_clsid.as_str());
    let is_encrypted = has("EncryptedPackage") && has("EncryptionInfo");
    let is_word = has("WordDocument");
    let is_excel = has("Workbook") || has("Book");
    let is_ppt = has("PowerPoint Document");
    let is_visio = has("VisioDocument");
    let is_msg = has("__properties_version1.0")
        || cf.entries().iter().any(|e| e.name.starts_with("__substg1.0_"));
    let claims: Vec<&str> = [
        (is_msi, "Windows Installer"),
        (is_encrypted, "encrypted Office Open XML"),
        (is_word, "Word"),
        (is_excel, "Excel"),
        (is_ppt, "PowerPoint"),
        (is_visio, "Visio"),
        (is_msg, "Outlook message"),
    ]
    .iter()
    .filter_map(|&(yes, name)| yes.then_some(name))
    .collect();
    let format = if is_msi {
        FileFormat::Msi
    } else if is_encrypted {
        FileFormat::EncryptedOoxml
    } else if is_word {
        FileFormat::Doc
    } else if is_excel {
        FileFormat::Xls
    } else if is_ppt {
        FileFormat::Ppt
    } else if is_visio {
        FileFormat::Vsd
    } else if is_msg {
        FileFormat::Msg
    } else {
        FileFormat::OleOther
    };
    ctx.identified(format);

    match format {
        FileFormat::Msi => {
            if ctx.depth == 0 {
                return Err(Abort::new(
                    ThreatKind::DisguisedExecutable,
                    location,
                    "Windows Installer package disguised as a document",
                ));
            }
            ctx.report.add(
                ThreatKind::EmbeddedExecutable,
                location,
                "embedded Windows Installer package",
            );
            return Ok(format);
        }
        FileFormat::EncryptedOoxml => {
            let detail = encryption_info(&cf);
            ctx.report.add(ThreatKind::EncryptedDocument, location, format!("password-protected OOXML ({detail}); contents cannot be inspected without the password"));
        }
        _ => {}
    }
    if format == FileFormat::OleOther {
        // None of the streams that make a compound file a document were found, so nothing says
        // how its streams are to be read.
        ctx.report.add(
            ThreatKind::ScanLimitReached,
            location,
            "the compound file is not a document of a recognised kind; its streams were not interpreted",
        );
    }
    if claims.len() > 1 {
        ctx.report.add(
            ThreatKind::ParserDifferential,
            location,
            format!(
                "the compound file carries the streams of several document types ({}); every matching scanner was run, but readers of each type will see a different document",
                claims.join(", ")
            ),
        );
    }

    scan_storage_tree(ctx, &cf, location)?;

    // Every type the file claims is scanned, not only the first match: the scanner must not
    // pick the interpretation the author wants it to pick.
    let read = |path: &str| -> Option<Result<Vec<u8>, arca_container::cfb::CfbError>> {
        cf.find(path).map(|i| cf.read_stream(i))
    };
    let unreadable = |ctx: &mut Ctx<'_>, name: &str, e: &arca_container::cfb::CfbError| {
        ctx.report.add(
            ThreatKind::MalformedContainer,
            &Ctx::join(location, name),
            format!("stream could not be read, so it was not inspected: {e}"),
        );
    };
    if is_word {
        match read("WordDocument") {
            Some(Ok(s)) => word_binary::scan(ctx, &cf, &s, &Ctx::join(location, "WordDocument")),
            Some(Err(e)) => unreadable(ctx, "WordDocument", &e),
            None => {}
        }
    }
    if is_excel {
        let name = if has("Workbook") { "Workbook" } else { "Book" };
        match read(name) {
            Some(Ok(s)) => biff::scan(ctx, &s, &Ctx::join(location, name)),
            Some(Err(e)) => unreadable(ctx, name, &e),
            None => {}
        }
    }
    if is_ppt {
        match read("PowerPoint Document") {
            Some(Ok(s)) => ppt::scan(ctx, &s, &Ctx::join(location, "PowerPoint Document"))?,
            Some(Err(e)) => unreadable(ctx, "PowerPoint Document", &e),
            None => {}
        }
    }
    if is_visio {
        match read("VisioDocument") {
            Some(Ok(s)) => visio_binary::scan(ctx, &s, &Ctx::join(location, "VisioDocument")),
            Some(Err(e)) => unreadable(ctx, "VisioDocument", &e),
            None => {}
        }
    }
    Ok(format)
}

/// Scans a `vbaProject.bin` (or any compound file expected to hold VBA).
pub(crate) fn scan_vba_container(
    ctx: &mut Ctx<'_>,
    data: &[u8],
    location: &str,
) -> Result<(), Abort> {
    if !arca_container::cfb::has_cfb_signature(data) {
        ctx.report.add(
            ThreatKind::VbaMacros,
            location,
            "macro project part is not a compound file",
        );
        return Ok(());
    }
    let cf = match CompoundFile::parse(data, ctx.policy.cfb) {
        Ok(cf) => cf,
        Err(e) => {
            ctx.report.add(
                ThreatKind::VbaMacros,
                location,
                format!("unreadable macro project: {e}"),
            );
            return Ok(());
        }
    };
    let projects = vba::find_projects(&cf);
    if projects.is_empty() {
        ctx.report.add(ThreatKind::VbaMacros, location, "macro project without a VBA storage");
    }
    for p in projects {
        vba::extract(ctx, &cf, p, location)?;
    }
    if let Some(i) = cf.find("PROJECT")
        && let Ok(text) = cf.read_stream(i)
    {
        project_stream(ctx, &text, location);
    }
    Ok(())
}

/// Walks every storage/stream of a compound file looking for macros,
/// embedded objects, packages, links and metadata.
fn scan_storage_tree(
    ctx: &mut Ctx<'_>,
    cf: &CompoundFile<'_>,
    location: &str,
) -> Result<(), Abort> {
    for p in vba::find_projects(cf) {
        vba::extract(ctx, cf, p, location)?;
        // The PROJECT stream is the sibling of the VBA storage.
        if let Some(parent) = cf.entries().get(p).and_then(|e| e.parent)
            && let Some(proj) = cf.child(parent, "PROJECT")
            && let Ok(text) = cf.read_stream(proj)
        {
            project_stream(ctx, &text, location);
        }
    }
    for (i, e) in cf.entries().iter().enumerate() {
        let loc = Ctx::join(location, &e.path);
        if e.kind != EntryKind::Stream && !clsid_is_null(&e.clsid) {
            let clsid = format_clsid(&e.clsid);
            if let Some((desc, kind, sev)) = clsid_info(&clsid) {
                ctx.report.add_with(kind, sev, &loc, format!("{clsid}: {desc}"));
            }
        }
        if e.kind != EntryKind::Stream {
            if e.kind == EntryKind::Storage
                && (e.name.starts_with('_')
                    && e.parent
                        .and_then(|p| cf.entries().get(p))
                        .is_some_and(|p| p.name == "ObjectPool")
                    || e.name.starts_with("MBD"))
            {
                ctx.report.add(
                    ThreatKind::OleObject,
                    &loc,
                    format!("embedded OLE object {}", format_clsid(&e.clsid)),
                );
            }
            continue;
        }
        let lname = e.name.to_ascii_lowercase();
        match lname.as_str() {
            "\u{1}ole10native" => {
                if let Ok(data) = cf.read_stream(i) {
                    ole10_native(ctx, &data, &loc)?;
                }
            }
            "\u{1}compobj" => {
                if let Ok(data) = cf.read_stream(i)
                    && let Some(class) = comp_obj_class(&data)
                {
                    let l = class.to_ascii_lowercase();
                    if l.starts_with("equation.") {
                        ctx.report.add(ThreatKind::ExploitableOleClass, &loc, format!("{class}: Microsoft Equation Editor 3.0 (CVE-2017-11882, CVE-2018-0802)"));
                    } else if l == "package" {
                        ctx.report.add(ThreatKind::OlePackage, &loc, "OLE Packager object");
                    } else if l.starts_with("htmlfile") || l.starts_with("shell.explorer") {
                        ctx.report.add(
                            ThreatKind::ExploitableOleClass,
                            &loc,
                            format!("{class}: HTML-rendering OLE class"),
                        );
                    }
                }
            }
            "\u{1}ole" => {
                if let Ok(data) = cf.read_stream(i) {
                    ole_link(ctx, &data, &loc);
                }
            }
            "equation native" => {
                ctx.report.add(
                    ThreatKind::ExploitableOleClass,
                    &loc,
                    "Equation Editor native data (CVE-2017-11882, CVE-2018-0802)",
                );
            }
            "\u{5}summaryinformation" | "\u{5}documentsummaryinformation" => {
                if let Ok(data) = cf.read_stream(i) {
                    propset::scan(ctx, &data, &loc);
                }
            }
            // Outlook message: the bytes of a file attachment (PR_ATTACH_DATA_BIN) ...
            "__substg1.0_37010102" => {
                if let Ok(data) = cf.read_stream(i) {
                    msg_attachment(ctx, cf, i, &data, &loc)?;
                }
            }
            // ... and the HTML body (PR_BODY_HTML, as binary or as text), which is rendered by
            // a browser engine in most mail clients.
            "__substg1.0_10130102" | "__substg1.0_1013001f" | "__substg1.0_1013001e" => {
                if let Ok(data) = cf.read_stream(i) {
                    let html = if lname.ends_with("001f") {
                        utf16le_lossy(&data)
                    } else {
                        String::from_utf8_lossy(&data).into_owned()
                    };
                    crate::textfmt::scan_html(ctx, &html, &loc);
                }
            }
            "package" | "contents" => {
                // Embedded OOXML/ODF/PDF packages inside OLE objects.
                if let Ok(data) = cf.read_stream(i)
                    && !matches!(
                        crate::format::sniff(&data),
                        crate::format::Magic::Unknown
                            | crate::format::Magic::Text
                            | crate::format::Magic::Empty
                    )
                {
                    let fmt = ctx.scan_nested(&data, &loc)?;
                    ctx.report.add(
                        ThreatKind::EmbeddedDocument,
                        &loc,
                        format!("embedded {fmt:?} (scanned recursively)"),
                    );
                    push_embedded(ctx, &loc, EmbeddedKind::Document, "", &data);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A file attached to an Outlook message: named by a sibling property stream, scanned by what
/// its bytes are, never by what its name says.
fn msg_attachment(
    ctx: &mut Ctx<'_>,
    cf: &CompoundFile<'_>,
    stream: usize,
    data: &[u8],
    loc: &str,
) -> Result<(), Abort> {
    let parent = cf.entries().get(stream).and_then(|e| e.parent);
    // Long file name, then short file name, then display name (all UTF-16LE strings).
    let name = ["__substg1.0_3707001F", "__substg1.0_3704001F", "__substg1.0_3001001F"]
        .iter()
        .find_map(|n| {
            let idx = cf.child(parent?, n)?;
            let raw = cf.read_stream(idx).ok()?;
            Some(utf16le_lossy(&raw).trim_end_matches('\0').to_owned())
        })
        .unwrap_or_default();
    if crate::has_bidi(&name) {
        ctx.report.add(
            ThreatKind::BidiOverride,
            loc,
            format!(
                "attachment name {name:?} uses bidirectional overrides to disguise its extension"
            ),
        );
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_owned()).unwrap_or_default();
    if is_dangerous_extension(&ext) {
        ctx.report.add(
            ThreatKind::EmbeddedExecutable,
            loc,
            format!("attachment {name:?} has an executable or script extension"),
        );
    }
    scan_payload(ctx, data, loc, &name)
}

/// Scans the bytes of an attached or packaged file by content: containers and pictures are
/// scanned recursively, executables are reported, and bytes of a kind the scanner cannot read
/// are a coverage gap rather than a pass.
pub(crate) fn scan_payload(
    ctx: &mut Ctx<'_>,
    data: &[u8],
    loc: &str,
    file_name: &str,
) -> Result<(), Abort> {
    use crate::format::Magic;
    match crate::format::sniff(data) {
        Magic::Empty => {}
        Magic::Executable(_) => {
            // `scan_nested` reports the executable with its location.
            ctx.scan_nested(data, loc)?;
            push_embedded(ctx, loc, EmbeddedKind::Executable, file_name, data);
        }
        Magic::Unknown => {
            ctx.report.add(
                ThreatKind::ScanLimitReached,
                loc,
                format!("attached file {file_name:?} is of a kind the scanner cannot read; it was not inspected"),
            );
            push_embedded(ctx, loc, EmbeddedKind::Other, file_name, data);
        }
        Magic::Image(_) | Magic::Text => {
            ctx.scan_nested(data, loc)?;
            push_embedded(ctx, loc, EmbeddedKind::Other, file_name, data);
        }
        Magic::Zip | Magic::Ole2 | Magic::Pdf | Magic::Rtf | Magic::Jet(_) => {
            let fmt = ctx.scan_nested(data, loc)?;
            ctx.report.add(
                ThreatKind::EmbeddedDocument,
                loc,
                format!("embedded {fmt:?} (scanned recursively)"),
            );
            push_embedded(ctx, loc, EmbeddedKind::Document, file_name, data);
        }
    }
    Ok(())
}

fn push_embedded(ctx: &mut Ctx<'_>, loc: &str, kind: EmbeddedKind, file_name: &str, data: &[u8]) {
    ctx.report.embedded.push(EmbeddedObject {
        location: loc.to_owned(),
        kind,
        class_name: String::new(),
        clsid: String::new(),
        file_name: arca_container::text::display_safe(file_name, 260),
        size: data.len() as u64,
        sha256: arca_digest::sha256(data).to_string(),
    });
}

/// Parses the `PROJECT` stream (plain text key=value lines).
fn project_stream(ctx: &mut Ctx<'_>, raw: &[u8], location: &str) {
    let text = decode_codepage(raw, CodePage::Windows1252);
    for line in text.lines() {
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        match k.trim() {
            "DPB" if v.len() > 2 => ctx.report.add_with(
                ThreatKind::VbaMacros,
                Severity::Medium,
                location,
                "VBA project is password-protected (source still extracted)",
            ),
            "HelpFile" if !v.trim_matches('"').is_empty() => {
                let path = v.trim_matches('"');
                if crate::url::classify(path) == crate::report::TargetClass::LocalFile {
                    ctx.report.meta(MetaKey::LocalPath, "", path, location);
                }
            }
            _ => {}
        }
    }
}

/// Returns the user type / ProgID recorded in a `\x01CompObj` stream.
fn comp_obj_class(data: &[u8]) -> Option<String> {
    // Header (28 bytes), then AnsiUserType, AnsiClipboardFormat, then the
    // Reserved1 length-prefixed ANSI ProgID string.
    let mut r = LeReader::at(data, 28);
    let mut strings = Vec::new();
    for _ in 0..3 {
        let len = usize::try_from(r.u32()?).ok()?;
        if len == 0xFFFF_FFFF || len == 0xFFFF_FFFE {
            // Clipboard format given as an id.
            r.skip(4)?;
            strings.push(String::new());
            continue;
        }
        if len > 4096 {
            return None;
        }
        let raw = r.bytes(len)?;
        let raw = raw.split(|&b| b == 0).next().unwrap_or(raw);
        strings.push(decode_codepage(raw, CodePage::Windows1252));
    }
    let progid = strings.get(2).filter(|s| !s.is_empty()).or_else(|| strings.first())?;
    Some(progid.clone())
}

/// Parses an `\x01Ole10Native` stream (OLE Packager).
pub(crate) fn ole10_native(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) -> Result<(), Abort> {
    let parsed = (|| {
        let mut r = LeReader::new(data);
        let _total = r.u32()?;
        let _kind = r.u16()?;
        let label = cstr(&mut r)?;
        let org_path = cstr(&mut r)?;
        let _unknown = r.u32()?;
        let temp_len = usize::try_from(r.u32()?).ok()?;
        let temp = r.bytes(temp_len)?;
        let temp_path =
            decode_codepage(temp.split(|&b| b == 0).next().unwrap_or(temp), CodePage::Windows1252);
        let size = usize::try_from(r.u32()?).ok()?;
        let payload = r.bytes(size)?;
        Some((label, org_path, temp_path, payload))
    })();
    let Some((label, org_path, temp_path, payload)) = parsed else {
        ctx.report.add(ThreatKind::OlePackage, loc, "OLE Packager stream (unparsable header)");
        return Ok(());
    };
    let name = if label.is_empty() {
        org_path.rsplit(['\\', '/']).next().unwrap_or("").to_owned()
    } else {
        label.clone()
    };
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let exec_magic = matches!(crate::format::sniff(payload), crate::format::Magic::Executable(_));
    let kind = if exec_magic || is_dangerous_extension(&ext) {
        EmbeddedKind::Executable
    } else {
        EmbeddedKind::Package
    };
    if kind == EmbeddedKind::Executable {
        ctx.report.add(
            ThreatKind::EmbeddedExecutable,
            loc,
            format!("packaged file {name:?} ({} bytes)", payload.len()),
        );
    } else {
        ctx.report.add(
            ThreatKind::OlePackage,
            loc,
            format!("packaged file {name:?} ({} bytes)", payload.len()),
        );
    }
    for path in [&org_path, &temp_path] {
        if !path.is_empty() {
            ctx.report.meta(MetaKey::LocalPath, "", path, loc);
            if let Some(u) = crate::url::profile_user(path) {
                ctx.report.meta(MetaKey::Person, "Windows profile", &u, loc);
            }
        }
    }
    if crate::has_bidi(&name) {
        ctx.report.add(
            ThreatKind::BidiOverride,
            loc,
            format!("file name {name:?} uses bidirectional overrides to disguise its extension"),
        );
    }
    push_embedded(ctx, loc, kind, &name, payload);
    if !matches!(
        crate::format::sniff(payload),
        crate::format::Magic::Unknown
            | crate::format::Magic::Text
            | crate::format::Magic::Executable(_)
            | crate::format::Magic::Empty
    ) {
        ctx.scan_nested(payload, loc)?;
    }
    Ok(())
}

fn cstr(r: &mut LeReader<'_>) -> Option<String> {
    let rest = r.rest();
    let end = rest.iter().position(|&b| b == 0)?;
    if end > 4096 {
        return None;
    }
    let s = decode_codepage(rest.get(..end)?, CodePage::Windows1252);
    r.skip(end + 1)?;
    Some(s)
}

/// Inspects an `\x01Ole` stream for linked objects and their monikers.
fn ole_link(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) {
    let flags = u32_le(data, 4).unwrap_or(0);
    if flags & 1 == 0 {
        return;
    }
    // Linked object: the absolute moniker may be a URL moniker
    // {79EAC9E0-BAF9-11CE-8C82-00AA004BA90B} followed by a UTF-16 URL.
    const URL_MONIKER: [u8; 16] = [
        0xE0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B, 0xA9,
        0x0B,
    ];
    if let Some(at) = memfind(data, &URL_MONIKER) {
        let len =
            u32_le(data, at + 16).and_then(|l| usize::try_from(l).ok()).unwrap_or(0).min(4096);
        let url = utf16le_lossy(data.get(at + 20..at + 20 + len).unwrap_or(&[]));
        let url = url.trim_end_matches('\0').to_owned();
        ctx.report.add(
            ThreatKind::ExternalObjectLink,
            loc,
            format!("OLE object linked through a URL moniker to {url} (CVE-2017-0199 pattern)"),
        );
        if !url.is_empty() {
            ooxml::record_external(ctx, &url, RefKind::ObjectLink, true, loc);
        }
    } else {
        ctx.report.add_with(
            ThreatKind::ExternalObjectLink,
            Severity::Medium,
            loc,
            "linked OLE object (updates from its source)",
        );
    }
}

fn encryption_info(cf: &CompoundFile<'_>) -> String {
    let Some(i) = cf.find("EncryptionInfo") else { return String::from("unknown scheme") };
    let Ok(data) = cf.read_stream(i) else { return String::from("unknown scheme") };
    match (u16_le(&data, 0), u16_le(&data, 2)) {
        (Some(4), Some(4)) => String::from("Agile encryption"),
        (Some(3 | 4), Some(2)) => String::from("Standard encryption"),
        (Some(3 | 4), Some(3)) => String::from("Extensible encryption"),
        (Some(a), Some(b)) => format!("encryption version {a}.{b}"),
        _ => String::from("unknown scheme"),
    }
}

/// Access (Jet/ACE) triage: version, AutoExec macro objects, embedded OLE.
pub(crate) fn scan_access(ctx: &mut Ctx<'_>, data: &[u8], location: &str) -> Result<(), Abort> {
    let fmt = match data.get(0x14).copied() {
        Some(0) => FileFormat::AccessJet3,
        Some(1) => FileFormat::AccessJet4,
        _ => FileFormat::AccessAce,
    };
    ctx.identified(fmt);
    // The system catalog (MSysObjects) is what Access itself consults: a macro, a module and the
    // table of stored objects (MSysAccessStorage) exist if and only if the catalog lists them.
    // When the catalog was read completely it alone decides, so a harmless database whose rows
    // merely contain these words stays clean. Anything short of a complete, successful read is
    // reported as a coverage gap, so the file can never come out Clean on the strength of a
    // failed parse, and then the names are searched for as bytes instead.
    let mut autoexec = false;
    let mut vba_object = false;
    // The table that holds the saved forms, reports, macros and modules of an ACCDB.
    let mut access_storage = false;
    let mut catalog_complete = false;
    // Databases are written in whole pages: a file that ends inside one was cut off.
    let page = if matches!(fmt, FileFormat::AccessJet3) { 2048 } else { 4096 };
    if data.len() % page != 0 {
        ctx.report.add(
            ThreatKind::MalformedContainer,
            location,
            "the database file ends inside a page (it is truncated)",
        );
    }
    match arca_container::jet::Db::open(data) {
        Ok(db) => match db.catalog_checked() {
            Some((objects, stats)) => {
                if stats.stopped {
                    ctx.report.add(
                        ThreatKind::ScanLimitReached,
                        location,
                        "the object catalog of the database was only partly read",
                    );
                } else {
                    catalog_complete = true;
                }
                for o in &objects {
                    if o.name.eq_ignore_ascii_case("MSysAccessStorage") {
                        access_storage = true;
                    }
                    // A project of the VBA editor is listed under its own name.
                    if o.name.eq_ignore_ascii_case("VBAProject") {
                        vba_object = true;
                    }
                    // A macro is a catalog object of kind -32766; Access runs the one named
                    // AutoExec when the database opens (the name is case-insensitive).
                    if o.kind == -32766 && o.name.eq_ignore_ascii_case("AutoExec") {
                        autoexec = true;
                    }
                    // Modules are objects of kind -32761.
                    if o.kind == -32761 {
                        vba_object = true;
                    }
                    // Linked tables (4: ODBC, 6: another Jet/ACE file or file-based source)
                    // fetch their rows from elsewhere when opened.
                    if matches!(o.kind, 4 | 6) {
                        let target = if o.database.is_empty() {
                            o.connect.as_str()
                        } else {
                            o.database.as_str()
                        };
                        ooxml::record_external(
                            ctx,
                            target,
                            RefKind::DataConnection,
                            false,
                            location,
                        );
                    }
                }
            }
            None => ctx.report.add(
                ThreatKind::MalformedContainer,
                location,
                "the object catalog of the database could not be read",
            ),
        },
        Err(arca_container::jet::OpenError::Encrypted) => {
            ctx.report.add(
                ThreatKind::EncryptedDocument,
                location,
                "database is encrypted or password-protected",
            );
        }
        Err(arca_container::jet::OpenError::Malformed(why)) => {
            ctx.report.add(
                ThreatKind::MalformedContainer,
                location,
                format!("database structure could not be read: {why}"),
            );
        }
    }
    if !catalog_complete {
        // A damaged or hostile file can hide its catalog, never its bytes: search for the names
        // in ASCII (which is also how Jet 4 "compressed Unicode" stores them) and in UTF-16LE.
        // Access compares object names without regard to case, so the search does too.
        let seen = |name: &str| {
            memfind_ci(data, name.as_bytes()).is_some()
                || memfind_ci(data, &utf16le_bytes(name)).is_some()
        };
        autoexec |= seen("autoexec");
        vba_object |= seen("vbaproject");
        access_storage |= seen("msysaccessstorage");
    }
    if autoexec {
        ctx.report.add_with(
            ThreatKind::MacroAutoExec,
            Severity::High,
            location,
            "database contains an object named AutoExec (Access runs this macro on open; Arca never does)",
        );
    }
    if vba_object {
        ctx.report.add(
            ThreatKind::VbaMacros,
            location,
            "database contains a VBA project or module (ignored by design)",
        );
    }
    // OLE storages stored in database pages (OLE Object columns, saved form resources) are
    // scanned like any other embedded object, up to a limit.
    const MAX_OLE_BLOBS: usize = 16;
    let mut ole_blobs = 0usize;
    let mut from = 0usize;
    while let Some(rel) = data.get(from..).and_then(|d| memfind(d, &arca_container::cfb::SIGNATURE))
    {
        let at = from + rel;
        from = at + 8;
        ole_blobs += 1;
        if ole_blobs > MAX_OLE_BLOBS {
            ctx.report.add(
                ThreatKind::ScanLimitReached,
                location,
                format!("more than {MAX_OLE_BLOBS} embedded OLE storages in database pages; the rest were not scanned"),
            );
            break;
        }
        // Jet stores long values in pages that need not be contiguous, so a signature is often
        // not the start of a storage; only one that parses as a compound file is scanned.
        if let Some(blob) = data.get(at..)
            && CompoundFile::parse(blob, ctx.policy.cfb).is_ok()
        {
            ctx.scan_nested(blob, &format!("{location}#ole{ole_blobs}"))?;
        }
    }
    if ole_blobs > 0 {
        ctx.report.add(
            ThreatKind::OleObject,
            location,
            format!(
                "{} embedded OLE storage signature(s) in database pages",
                ole_blobs.min(MAX_OLE_BLOBS)
            ),
        );
    }
    let stored = if access_storage {
        " The saved forms, reports, macros and modules (MSysAccessStorage) are not interpreted."
    } else {
        ""
    };
    ctx.report.add(
        ThreatKind::UnsupportedFormat,
        location,
        format!("Access databases are opened read-only; macros, modules and queries are never executed.{stored}"),
    );
    Ok(())
}
