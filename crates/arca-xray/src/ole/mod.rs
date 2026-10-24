//! OLE2 compound files: `.doc .xls .ppt .vsd .msg`, encrypted OOXML, MSI,
//! `vbaProject.bin`, OLE objects; plus Access database triage.

mod biff;
mod ppt;
pub(crate) mod propset;
pub(crate) mod vba;
mod word_binary;

use arca_container::bytes::{LeReader, u32_le};
use arca_container::cfb::{CfbAnomaly, CompoundFile, EntryKind, clsid_is_null, format_clsid};
use arca_container::text::{CodePage, decode_codepage, utf16le_lossy};

use crate::format::FileFormat;
use crate::report::{EmbeddedKind, EmbeddedObject, MetaKey, RefKind, Severity, ThreatKind};
use crate::{Abort, Ctx, memfind, ooxml};

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
            | arca_container::cfb::CfbError::DirectoryCycle => ThreatKind::ParserDifferential,
            _ => ThreatKind::MalformedContainer,
        };
        Abort::new(kind, location, format!("compound file: {e}"))
    })?;
    for a in cf.anomalies() {
        match a {
            CfbAnomaly::OrphanedEntries(n) => ctx.report.add(
                ThreatKind::OrphanedOleEntries,
                location,
                format!(
                    "{n} directory entr(ies) unreachable from the root (possible hidden payload)"
                ),
            ),
            CfbAnomaly::TrailingData(n) => ctx.report.add(
                ThreatKind::TrailingData,
                location,
                format!("{n} bytes after the last sector"),
            ),
            CfbAnomaly::SizeHighBitsIgnored(_) | CfbAnomaly::ChainLongerThanSize(_) => {}
        }
    }

    let root_clsid = cf.entries().first().map(|e| format_clsid(&e.clsid)).unwrap_or_default();
    let has = |name: &str| cf.find(name).is_some();
    let format = if MSI_CLSIDS.contains(&root_clsid.as_str()) {
        FileFormat::Msi
    } else if has("EncryptedPackage") && has("EncryptionInfo") {
        FileFormat::EncryptedOoxml
    } else if has("WordDocument") {
        FileFormat::Doc
    } else if has("Workbook") || has("Book") {
        FileFormat::Xls
    } else if has("PowerPoint Document") {
        FileFormat::Ppt
    } else if has("VisioDocument") {
        FileFormat::Vsd
    } else if has("__properties_version1.0")
        || cf.entries().iter().any(|e| e.name.starts_with("__substg1.0_"))
    {
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

    scan_storage_tree(ctx, &cf, location)?;

    let read = |path: &str| cf.find(path).and_then(|i| cf.read_stream(i).ok());
    match format {
        FileFormat::Doc => {
            if let Some(s) = read("WordDocument") {
                word_binary::scan(ctx, &s, &Ctx::join(location, "WordDocument"));
            }
        }
        FileFormat::Xls => {
            if let Some(s) = read("Workbook").or_else(|| read("Book")) {
                biff::scan(ctx, &s, &Ctx::join(location, "Workbook"));
            }
        }
        FileFormat::Ppt => {
            if let Some(s) = read("PowerPoint Document") {
                ppt::scan(ctx, &s, &Ctx::join(location, "PowerPoint Document"))?;
            }
        }
        _ => {}
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
        vba::extract(ctx, &cf, p, location);
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
        vba::extract(ctx, cf, p, location);
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
    match (arca_container::bytes::u16_le(&data, 0), arca_container::bytes::u16_le(&data, 2)) {
        (Some(4), Some(4)) => String::from("Agile encryption"),
        (Some(3 | 4), Some(2)) => String::from("Standard encryption"),
        (Some(3 | 4), Some(3)) => String::from("Extensible encryption"),
        (Some(a), Some(b)) => format!("encryption version {a}.{b}"),
        _ => String::from("unknown scheme"),
    }
}

/// Access (Jet/ACE) triage: version, AutoExec macro objects, embedded OLE.
pub(crate) fn scan_access(ctx: &mut Ctx<'_>, data: &[u8], location: &str) {
    let fmt = match data.get(0x14).copied() {
        Some(0) => FileFormat::AccessJet3,
        Some(1) => FileFormat::AccessJet4,
        _ => FileFormat::AccessAce,
    };
    ctx.identified(fmt);
    // Structured catalog parsing via MSysObjects (eliminates F-2 false positives)
    match arca_container::jet::Db::open(data) {
        Ok(db) => {
            if let Some(objects) = db.catalog() {
                for o in &objects {
                    // Macro AutoExec: object of kind -32766 named "AutoExec"
                    if o.kind == -32766 && o.name.eq_ignore_ascii_case("AutoExec") {
                        ctx.report.add_with(
                            ThreatKind::MacroAutoExec,
                            Severity::High,
                            location,
                            "database contains an AutoExec macro object (Access executes this on open; Arca never does)",
                        );
                    }
                    // VBA modules: kind -32761, or MSysAccessStorage/MSysAccessObjects
                    if o.kind == -32761
                        || o.name == "MSysAccessStorage"
                        || o.name == "MSysAccessObjects"
                    {
                        ctx.report.add(
                            ThreatKind::VbaMacros,
                            location,
                            format!("database contains a VBA module or storage {:?}", o.name),
                        );
                    }
                }
            }
        }
        Err(arca_container::jet::OpenError::Encrypted) => {
            ctx.report.add(
                ThreatKind::EncryptedDocument,
                location,
                "database is encrypted or password-protected",
            );
        }
        Err(arca_container::jet::OpenError::Malformed(_)) => {
            // Malformed or truncated header: fallback to conservative UTF-16 check
            let autoexec_utf16: Vec<u8> = "AutoExec".encode_utf16().flat_map(u16::to_le_bytes).collect();
            if memfind(data, &autoexec_utf16).is_some() {
                ctx.report.add_with(ThreatKind::MacroAutoExec, Severity::High, location, "database contains an AutoExec string signature");
            }
        }
    }
    let mut ole_blobs = 0usize;
    let mut from = 0usize;
    while let Some(rel) = data.get(from..).and_then(|d| memfind(d, &arca_container::cfb::SIGNATURE))
    {
        ole_blobs += 1;
        from = from + rel + 8;
        if ole_blobs >= 1024 {
            break;
        }
    }
    if ole_blobs > 0 {
        ctx.report.add(
            ThreatKind::OleObject,
            location,
            format!("{ole_blobs} embedded OLE storage signature(s) in database pages"),
        );
    }
    ctx.report.add(
        ThreatKind::UnsupportedFormat,
        location,
        "Access databases are opened read-only; macros, modules and queries are never executed",
    );
}
