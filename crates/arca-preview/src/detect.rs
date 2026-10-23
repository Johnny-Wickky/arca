//! Format identification (from content only) and dispatch to the extractors.

use arca_container::cfb::{CompoundFile, format_clsid};
use arca_container::zip::ZipArchive;
use arca_xray::format::{
    Magic, classify_text, odf_format_from_mimetype, opc_format_from_content_type, sniff,
};

use crate::model::{FileFormat, NoticeKind};
use crate::opc::Package;
use crate::sink::Sink;
use crate::{PreviewError, PreviewPolicy, plain};

/// CLSIDs of Windows Installer packages (never opened).
const MSI_CLSIDS: [&str; 3] = [
    "000C1084-0000-0000-C000-000000000046",
    "000C1082-0000-0000-C000-000000000046",
    "000C109A-0000-0000-C000-000000000046",
];

/// An identified, opened document.
pub(crate) enum Doc<'a> {
    /// OPC package (Word, Excel, PowerPoint, Visio).
    Opc { pkg: Package<'a>, format: FileFormat },
    /// ODF package.
    Odf { pkg: Package<'a>, format: FileFormat },
    /// OLE2 compound file (legacy Office).
    Cfb { cf: CompoundFile<'a>, format: FileFormat },
    /// Rich Text Format bytes.
    Rtf(&'a [u8]),
    /// PDF bytes.
    Pdf(&'a [u8]),
    /// A picture file.
    Picture { data: &'a [u8], format: FileFormat },
    /// Access database bytes.
    Access { data: &'a [u8], format: FileFormat },
    /// Decoded text; `legacy` is the code page assumed for old 8-bit text.
    Text { text: String, format: FileFormat, legacy: Option<u16> },
}

/// Identifies and opens `data`.
pub(crate) fn open<'a>(data: &'a [u8], policy: &PreviewPolicy) -> Result<Doc<'a>, PreviewError> {
    match sniff(data) {
        Magic::Empty => Err(PreviewError::Unsupported(FileFormat::Empty)),
        Magic::Executable(_) => Err(PreviewError::Unsupported(FileFormat::Executable)),
        Magic::Jet(format) => Ok(Doc::Access { data, format }),
        Magic::Unknown => Err(PreviewError::Unsupported(FileFormat::Unknown)),
        Magic::Zip => open_zip(data, policy),
        Magic::Ole2 => open_cfb(data, policy),
        Magic::Pdf => Ok(Doc::Pdf(data)),
        Magic::Image(format) => Ok(Doc::Picture { data, format }),
        Magic::Rtf => Ok(Doc::Rtf(data)),
        Magic::Text => {
            let plain::Decoded { text, legacy } = plain::decode(data).ok_or_else(|| {
                PreviewError::Malformed(
                    "text is not valid UTF-8/UTF-16 or legacy 8-bit text".into(),
                )
            })?;
            let format = classify_text(&text);
            match format {
                FileFormat::Html | FileFormat::FlatXml => Err(PreviewError::Unsupported(format)),
                _ => Ok(Doc::Text { text, format, legacy }),
            }
        }
    }
}

fn open_zip<'a>(data: &'a [u8], policy: &PreviewPolicy) -> Result<Doc<'a>, PreviewError> {
    let zip = ZipArchive::parse(data, policy.zip)
        .map_err(|e| PreviewError::Malformed(format!("archive: {e}")))?;
    let is_opc = zip.entry("[Content_Types].xml").is_some();
    let is_odf = zip.entry("mimetype").is_some();
    let mut pkg = Package::new(zip, policy)?;
    if is_opc {
        let format = match pkg.main_part()? {
            Some(part) => opc_format_from_content_type(pkg.content_type(&part)),
            None => FileFormat::OpcOther,
        };
        return Ok(Doc::Opc { pkg, format });
    }
    if is_odf {
        let mimetype = pkg.read("mimetype")?.unwrap_or_default();
        let mimetype = String::from_utf8_lossy(&mimetype);
        let format = odf_format_from_mimetype(mimetype.trim());
        return Ok(Doc::Odf { pkg, format });
    }
    Err(PreviewError::Unsupported(FileFormat::Zip))
}

fn open_cfb<'a>(data: &'a [u8], policy: &PreviewPolicy) -> Result<Doc<'a>, PreviewError> {
    let cf = CompoundFile::parse(data, policy.cfb)
        .map_err(|e| PreviewError::Malformed(format!("compound file: {e}")))?;
    let root_clsid = cf.entries().first().map(|e| format_clsid(&e.clsid)).unwrap_or_default();
    let has = |name: &str| cf.find(name).is_some();
    let format = if MSI_CLSIDS.contains(&root_clsid.as_str()) {
        return Err(PreviewError::Unsupported(FileFormat::Msi));
    } else if has("EncryptedPackage") && has("EncryptionInfo") {
        return Err(PreviewError::Encrypted);
    } else if has("WordDocument") {
        FileFormat::Doc
    } else if has("Workbook") || has("Book") {
        FileFormat::Xls
    } else if has("PowerPoint Document") {
        FileFormat::Ppt
    } else if has("VisioDocument") {
        FileFormat::Vsd
    } else if cf.entries().iter().any(|e| e.name.starts_with("__substg1.0_")) {
        FileFormat::Msg
    } else {
        FileFormat::OleOther
    };
    Ok(Doc::Cfb { cf, format })
}

/// Runs the extractor for an opened document and returns its format.
pub(crate) fn extract(doc: Doc<'_>, sink: &mut Sink<'_>) -> Result<FileFormat, PreviewError> {
    match doc {
        Doc::Text { text, format, legacy } => {
            if let Some(cp) = legacy {
                sink.note_detail(NoticeKind::BestEffort, format!("text-encoding=cp{cp}"));
            }
            plain::extract(sink, &text, format);
            Ok(format)
        }
        Doc::Opc { mut pkg, format } => {
            use FileFormat as F;
            match format {
                F::Docx | F::Docm | F::Dotx | F::Dotm => crate::docx::extract(&mut pkg, sink)?,
                F::Xlsx | F::Xlsm | F::Xltx | F::Xltm => crate::xlsx::extract(&mut pkg, sink)?,
                F::Pptx | F::Pptm | F::Ppsx | F::Ppsm | F::Potx | F::Potm => {
                    crate::pptx::extract(&mut pkg, sink)?;
                }
                F::Vsdx | F::Vsdm | F::VisioTemplateOrStencil => {
                    crate::vsdx::extract(&mut pkg, sink)?;
                }
                _ => return Err(PreviewError::Unsupported(format)),
            }
            for s in std::mem::take(&mut pkg.skipped) {
                sink.skipped(&s);
            }
            Ok(format)
        }
        Doc::Odf { mut pkg, format } => {
            crate::odf::extract(&mut pkg, sink, format)?;
            for s in std::mem::take(&mut pkg.skipped) {
                sink.skipped(&s);
            }
            Ok(format)
        }
        Doc::Cfb { cf, format } => {
            match format {
                FileFormat::Doc => crate::doc::extract(&cf, sink)?,
                FileFormat::Xls => crate::xls::extract(&cf, sink)?,
                FileFormat::Ppt => crate::ppt::extract(&cf, sink)?,
                FileFormat::Msg => crate::msg::extract(&cf, sink)?,
                FileFormat::Vsd => crate::vsd::extract(&cf, sink)?,
                _ => return Err(PreviewError::Unsupported(format)),
            }
            Ok(format)
        }
        Doc::Rtf(data) => {
            crate::rtf::extract(data, sink)?;
            Ok(FileFormat::Rtf)
        }
        Doc::Pdf(data) => {
            crate::pdf::extract(data, sink)?;
            Ok(FileFormat::Pdf)
        }
        Doc::Picture { data, format } => {
            crate::picture::extract_file(data, format, sink)?;
            Ok(format)
        }
        Doc::Access { data, format } => {
            crate::access::extract(data, format, sink)?;
            Ok(format)
        }
    }
}

/// The format of `data`, as far as it can be told (for refusals).
pub(crate) fn format_of(data: &[u8], policy: &PreviewPolicy) -> FileFormat {
    match open(data, policy) {
        Ok(Doc::Opc { format, .. } | Doc::Odf { format, .. } | Doc::Cfb { format, .. }) => format,
        Ok(Doc::Rtf(_)) => FileFormat::Rtf,
        Ok(Doc::Pdf(_)) => FileFormat::Pdf,
        Ok(Doc::Access { format, .. } | Doc::Text { format, .. } | Doc::Picture { format, .. }) => {
            format
        }
        Err(PreviewError::Unsupported(f)) => f,
        Err(_) => FileFormat::Unknown,
    }
}
