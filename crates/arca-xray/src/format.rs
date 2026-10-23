//! Format identification from content only.
//!
//! The file extension is never consulted: a `.docx` that is really a PE
//! executable is an executable. Identification proceeds from magic bytes to
//! container structure ([`FileFormat`] is refined after the container has
//! been parsed, e.g. OPC content types or OLE2 stream names).

use arca_abi::wire_enum;

/// Every format the X-ray can tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileFormat {
    /// Zero-length input.
    Empty,
    /// Unrecognised content.
    Unknown,
    // ---- OOXML (OPC) ------------------------------------------------------
    /// `.docx`
    Docx,
    /// `.docm`
    Docm,
    /// `.dotx`
    Dotx,
    /// `.dotm`
    Dotm,
    /// `.xlsx`
    Xlsx,
    /// `.xlsm`
    Xlsm,
    /// `.xltx`
    Xltx,
    /// `.xltm`
    Xltm,
    /// `.xlsb`
    Xlsb,
    /// `.xlam`
    Xlam,
    /// `.pptx`
    Pptx,
    /// `.pptm`
    Pptm,
    /// `.ppsx`
    Ppsx,
    /// `.ppsm`
    Ppsm,
    /// `.potx`
    Potx,
    /// `.potm`
    Potm,
    /// `.vsdx`
    Vsdx,
    /// `.vsdm`
    Vsdm,
    /// `.vstx` / `.vstm` / `.vssx` / `.vssm`
    VisioTemplateOrStencil,
    /// `.xps` / `.oxps`
    Xps,
    /// Another OPC package.
    OpcOther,
    // ---- ODF --------------------------------------------------------------
    /// `.odt` / `.ott`
    Odt,
    /// `.ods` / `.ots`
    Ods,
    /// `.odp` / `.otp`
    Odp,
    /// `.odg` / `.otg`
    Odg,
    /// Another ODF package.
    OdfOther,
    // ---- OLE2 -------------------------------------------------------------
    /// `.doc` / `.dot` (Word binary)
    Doc,
    /// `.xls` / `.xlt` (BIFF)
    Xls,
    /// `.ppt` / `.pps` / `.pot`
    Ppt,
    /// `.vsd` / `.vss` / `.vst` / `.vdw`
    Vsd,
    /// `.msg` (Outlook item)
    Msg,
    /// OOXML encrypted with a password (OLE2 `EncryptedPackage`).
    EncryptedOoxml,
    /// Windows Installer package (OLE2) — an executable.
    Msi,
    /// Another compound file.
    OleOther,
    // ---- others -----------------------------------------------------------
    /// Rich Text Format.
    Rtf,
    /// PDF.
    Pdf,
    /// Access 97 (Jet 3).
    AccessJet3,
    /// Access 2000–2003 (Jet 4) `.mdb`.
    AccessJet4,
    /// Access 2007+ (ACE) `.accdb`.
    AccessAce,
    /// UTF-8/UTF-16 text.
    PlainText,
    /// Comma/semicolon separated values.
    Csv,
    /// Tab separated values.
    Tsv,
    /// Markdown.
    Markdown,
    /// HTML / MHT / HTA — active content containers.
    Html,
    /// Flat XML document (Word 2003 XML, flat ODF, ...).
    FlatXml,
    /// ZIP archive that is not an office package.
    Zip,
    /// Native executable, script or shortcut.
    Executable,
    // ---- pictures ---------------------------------------------------------
    /// PNG picture.
    Png,
    /// JPEG picture.
    Jpeg,
    /// GIF picture.
    Gif,
    /// Windows bitmap.
    Bmp,
}

wire_enum!(FileFormat {
    Empty = 0, Unknown = 1,
    Docx = 10, Docm = 11, Dotx = 12, Dotm = 13, Xlsx = 14, Xlsm = 15, Xltx = 16, Xltm = 17, Xlsb = 18,
    Xlam = 19, Pptx = 20, Pptm = 21, Ppsx = 22, Ppsm = 23, Potx = 24, Potm = 25, Vsdx = 26, Vsdm = 27,
    VisioTemplateOrStencil = 28, Xps = 29, OpcOther = 30,
    Odt = 40, Ods = 41, Odp = 42, Odg = 43, OdfOther = 44,
    Doc = 50, Xls = 51, Ppt = 52, Vsd = 53, Msg = 54, EncryptedOoxml = 55, Msi = 56, OleOther = 57,
    Rtf = 60, Pdf = 61, AccessJet3 = 62, AccessJet4 = 63, AccessAce = 64, PlainText = 65, Csv = 66,
    Tsv = 67, Markdown = 68, Html = 69, FlatXml = 70, Zip = 71, Executable = 72,
    Png = 80, Jpeg = 81, Gif = 82, Bmp = 83,
});

/// High-level document family (drives which viewer the UI opens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Word processing.
    Text,
    /// Spreadsheets.
    Spreadsheet,
    /// Presentations.
    Presentation,
    /// Diagrams (Visio, ODG).
    Diagram,
    /// Databases.
    Database,
    /// Fixed layout (PDF, XPS).
    FixedLayout,
    /// A picture file.
    Image,
    /// Not a viewable document.
    None,
}

impl FileFormat {
    /// Document family.
    #[must_use]
    pub const fn family(self) -> Family {
        use FileFormat as F;
        match self {
            F::Docx
            | F::Docm
            | F::Dotx
            | F::Dotm
            | F::Odt
            | F::Doc
            | F::Rtf
            | F::PlainText
            | F::Markdown
            | F::FlatXml => Family::Text,
            F::Xlsx
            | F::Xlsm
            | F::Xltx
            | F::Xltm
            | F::Xlsb
            | F::Xlam
            | F::Ods
            | F::Xls
            | F::Csv
            | F::Tsv => Family::Spreadsheet,
            F::Pptx | F::Pptm | F::Ppsx | F::Ppsm | F::Potx | F::Potm | F::Odp | F::Ppt => {
                Family::Presentation
            }
            F::Vsdx | F::Vsdm | F::VisioTemplateOrStencil | F::Vsd | F::Odg => Family::Diagram,
            F::AccessJet3 | F::AccessJet4 | F::AccessAce => Family::Database,
            F::Pdf | F::Xps => Family::FixedLayout,
            F::Png | F::Jpeg | F::Gif | F::Bmp => Family::Image,
            F::Empty
            | F::Unknown
            | F::OpcOther
            | F::OdfOther
            | F::Msg
            | F::EncryptedOoxml
            | F::Msi
            | F::OleOther
            | F::Html
            | F::Zip
            | F::Executable => Family::None,
        }
    }

    /// `true` for the macro-enabled OOXML variants.
    #[must_use]
    pub const fn is_macro_enabled_type(self) -> bool {
        matches!(
            self,
            Self::Docm
                | Self::Dotm
                | Self::Xlsm
                | Self::Xltm
                | Self::Xlsb
                | Self::Xlam
                | Self::Pptm
                | Self::Ppsm
                | Self::Potm
                | Self::Vsdm
        )
    }

    /// `true` for OPC packages.
    #[must_use]
    pub const fn is_opc(self) -> bool {
        use FileFormat as F;
        matches!(
            self,
            F::Docx
                | F::Docm
                | F::Dotx
                | F::Dotm
                | F::Xlsx
                | F::Xlsm
                | F::Xltx
                | F::Xltm
                | F::Xlsb
                | F::Xlam
                | F::Pptx
                | F::Pptm
                | F::Ppsx
                | F::Ppsm
                | F::Potx
                | F::Potm
                | F::Vsdx
                | F::Vsdm
                | F::VisioTemplateOrStencil
                | F::Xps
                | F::OpcOther
        )
    }
}

/// Container kind recognised from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Magic {
    /// Zero-length input.
    Empty,
    /// ZIP archive (OPC, ODF, plain archive).
    Zip,
    /// OLE2 compound file.
    Ole2,
    /// PDF.
    Pdf,
    /// Rich Text Format.
    Rtf,
    /// Access database (format already known).
    Jet(FileFormat),
    /// Native executable, script or shortcut (description).
    Executable(&'static str),
    /// A picture (the format is known).
    Image(FileFormat),
    /// Looks like UTF-8 / UTF-16 text.
    Text,
    /// Anything else.
    Unknown,
}

/// Classifies by magic bytes only.
#[must_use]
pub fn sniff(data: &[u8]) -> Magic {
    if data.is_empty() {
        return Magic::Empty;
    }
    if let Some(kind) = executable_magic(data) {
        return Magic::Executable(kind);
    }
    if arca_container::cfb::has_cfb_signature(data) {
        return Magic::Ole2;
    }
    if arca_container::zip::has_zip_signature(data) {
        return Magic::Zip;
    }
    if data.starts_with(b"%PDF-") {
        return Magic::Pdf;
    }
    // Word only checks the first four bytes, so `{\rtxyz` is RTF too.
    if data.starts_with(b"{\\rt") {
        return Magic::Rtf;
    }
    if let Some(sig) = data.get(4..19) {
        if sig == b"Standard Jet DB" {
            let version = data.get(0x14).copied().unwrap_or(0);
            return Magic::Jet(if version == 0 {
                FileFormat::AccessJet3
            } else {
                FileFormat::AccessJet4
            });
        }
        if sig == b"Standard ACE DB" {
            return Magic::Jet(FileFormat::AccessAce);
        }
    }
    if let Some(kind) = arca_image::sniff(data) {
        return Magic::Image(match kind {
            arca_image::ImageFormat::Png => FileFormat::Png,
            arca_image::ImageFormat::Jpeg => FileFormat::Jpeg,
            arca_image::ImageFormat::Gif => FileFormat::Gif,
            arca_image::ImageFormat::Bmp => FileFormat::Bmp,
        });
    }
    if looks_like_text(data) {
        return Magic::Text;
    }
    Magic::Unknown
}

fn executable_magic(data: &[u8]) -> Option<&'static str> {
    const LNK: [u8; 20] =
        [0x4C, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    let head4 = data.get(..4);
    if data.starts_with(b"MZ") {
        return Some("Windows PE executable");
    }
    if data.starts_with(b"\x7FELF") {
        return Some("ELF executable");
    }
    if matches!(head4, Some([0xFE, 0xED, 0xFA, 0xCE | 0xCF] | [0xCE | 0xCF, 0xFA, 0xED, 0xFE])) {
        return Some("Mach-O executable");
    }
    if data.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]) {
        return Some("Mach-O universal binary / Java class");
    }
    if data.starts_with(b"#!") {
        return Some("script with interpreter line");
    }
    if data.starts_with(&LNK) {
        return Some("Windows shortcut (.lnk)");
    }
    if data.starts_with(b"ITSF") {
        return Some("Compiled HTML help (.chm)");
    }
    if data.starts_with(b"\0asm") {
        return Some("WebAssembly module");
    }
    None
}

/// Heuristic: valid UTF-8 / UTF-16 (BOM) or convincing legacy 8-bit text, without binary
/// control bytes.
#[must_use]
pub fn looks_like_text(data: &[u8]) -> bool {
    if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
        return data.len().is_multiple_of(2);
    }
    let sample = data.get(..data.len().min(64 << 10)).unwrap_or(data);
    let text = match core::str::from_utf8(sample) {
        Ok(t) => t,
        // A multi-byte character may be cut at the sample boundary.
        Err(e) if e.error_len().is_none() && sample.len() < data.len() => {
            match sample.get(..e.valid_up_to()).map(core::str::from_utf8) {
                Some(Ok(t)) => t,
                _ => return false,
            }
        }
        // Old 8-bit text (DOS / early Windows): accepted when it convincingly decodes.
        Err(_) => return arca_container::text::decode_legacy_text(sample).is_some(),
    };
    let bad = text
        .chars()
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
        .count();
    bad == 0
}

/// Refines a text file into its likely sub-format.
#[must_use]
pub fn classify_text(text: &str) -> FileFormat {
    let head = arca_container::text::truncate_utf8(text, 64 << 10);
    let trimmed = head.trim_start_matches('\u{feff}').trim_start();
    let lower: String = trimmed.chars().take(512).collect::<String>().to_ascii_lowercase();
    if lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.starts_with("<script")
        || lower.starts_with("<hta:")
        || lower.contains("<html")
        || lower.starts_with("mime-version:")
    {
        return FileFormat::Html;
    }
    if lower.starts_with("<?xml") || lower.starts_with('<') {
        return FileFormat::FlatXml;
    }
    let lines: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).take(20).collect();
    if lines.len() >= 2 {
        for (sep, fmt) in [('\t', FileFormat::Tsv), (',', FileFormat::Csv), (';', FileFormat::Csv)]
        {
            let counts: Vec<usize> = lines.iter().map(|l| l.matches(sep).count()).collect();
            if let Some(&first) = counts.first()
                && first > 0
                && counts.iter().all(|&c| c == first)
            {
                return fmt;
            }
        }
    }
    let md_markers = lines
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("# ")
                || t.starts_with("## ")
                || t.starts_with("```")
                || t.starts_with("- [")
                || (t.contains("](") && t.contains('['))
        })
        .count();
    if md_markers > 0 {
        return FileFormat::Markdown;
    }
    FileFormat::PlainText
}

/// Maps the content type of an OPC main part to a format.
#[must_use]
pub fn opc_format_from_content_type(ct: &str) -> FileFormat {
    let ct = ct.trim().to_ascii_lowercase();
    match ct.as_str() {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml" => {
            FileFormat::Docx
        }
        "application/vnd.ms-word.document.macroenabled.main+xml" => FileFormat::Docm,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml" => {
            FileFormat::Dotx
        }
        "application/vnd.ms-word.template.macroenabledtemplate.main+xml" => FileFormat::Dotm,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml" => {
            FileFormat::Xlsx
        }
        "application/vnd.ms-excel.sheet.macroenabled.main+xml" => FileFormat::Xlsm,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml" => {
            FileFormat::Xltx
        }
        "application/vnd.ms-excel.template.macroenabled.main+xml" => FileFormat::Xltm,
        "application/vnd.ms-excel.sheet.binary.macroenabled.main" => FileFormat::Xlsb,
        "application/vnd.ms-excel.addin.macroenabled.main+xml" => FileFormat::Xlam,
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml" => {
            FileFormat::Pptx
        }
        "application/vnd.ms-powerpoint.presentation.macroenabled.main+xml" => FileFormat::Pptm,
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow.main+xml" => {
            FileFormat::Ppsx
        }
        "application/vnd.ms-powerpoint.slideshow.macroenabled.main+xml" => FileFormat::Ppsm,
        "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml" => {
            FileFormat::Potx
        }
        "application/vnd.ms-powerpoint.template.macroenabled.main+xml" => FileFormat::Potm,
        "application/vnd.ms-visio.drawing.main+xml" => FileFormat::Vsdx,
        "application/vnd.ms-visio.drawing.macroenabled.main+xml" => FileFormat::Vsdm,
        "application/vnd.ms-visio.template.main+xml"
        | "application/vnd.ms-visio.template.macroenabled.main+xml"
        | "application/vnd.ms-visio.stencil.main+xml"
        | "application/vnd.ms-visio.stencil.macroenabled.main+xml" => {
            FileFormat::VisioTemplateOrStencil
        }
        "application/vnd.ms-package.xps-fixeddocumentsequence+xml"
        | "application/vnd.ms-package.xps-fixeddocument+xml"
        | "application/oxps" => FileFormat::Xps,
        _ => FileFormat::OpcOther,
    }
}

/// Maps an ODF `mimetype` value to a format.
#[must_use]
pub fn odf_format_from_mimetype(mt: &str) -> FileFormat {
    let mt = mt.trim();
    let base = mt.strip_suffix("-template").unwrap_or(mt);
    match base {
        "application/vnd.oasis.opendocument.text" => FileFormat::Odt,
        "application/vnd.oasis.opendocument.spreadsheet" => FileFormat::Ods,
        "application/vnd.oasis.opendocument.presentation" => FileFormat::Odp,
        "application/vnd.oasis.opendocument.graphics" => FileFormat::Odg,
        _ => FileFormat::OdfOther,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_bytes() {
        assert_eq!(sniff(b""), Magic::Empty);
        assert_eq!(sniff(b"MZ\x90\x00"), Magic::Executable("Windows PE executable"));
        assert_eq!(sniff(b"\x7FELF\x02"), Magic::Executable("ELF executable"));
        assert_eq!(sniff(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0]), Magic::Ole2);
        assert_eq!(sniff(b"PK\x03\x04rest"), Magic::Zip);
        assert_eq!(sniff(b"%PDF-1.7\n"), Magic::Pdf);
        assert_eq!(sniff(b"{\\rtf1\\ansi"), Magic::Rtf);
        assert_eq!(sniff(b"{\\rtxa evil"), Magic::Rtf);
        let mut jet = vec![0u8; 0x20];
        jet[4..19].copy_from_slice(b"Standard Jet DB");
        jet[0x14] = 1;
        assert_eq!(sniff(&jet), Magic::Jet(FileFormat::AccessJet4));
        assert_eq!(sniff("Привет, мир".as_bytes()), Magic::Text);
        assert_eq!(sniff(&[0x00, 0x01, 0x02, 0x03]), Magic::Unknown);
    }

    #[test]
    fn legacy_eight_bit_text_is_text_but_binary_is_not() {
        // Windows-1251: "Отчёт за квартал: все в порядке."
        let cp1251 = [
            0xCE, 0xF2, 0xF7, 0xB8, 0xF2, b' ', 0xE7, 0xE0, b' ', 0xEA, 0xE2, 0xE0, 0xF0, 0xF2,
            0xE0, 0xEB, b':', b' ', 0xE2, 0xF1, 0xE5, b' ', 0xE2, b' ', 0xEF, 0xEE, 0xF0, 0xFF,
            0xE4, 0xEA, 0xE5, b'.',
        ];
        assert_eq!(sniff(&cp1251), Magic::Text);
        // Latin-1 accents in Western text.
        assert_eq!(sniff(b"Caf\xE9 cr\xE8me, tr\xE8s bon, se\xF1or M\xFCller.\n"), Magic::Text);
        // Binary stays unknown: control bytes, or high bytes with no text structure.
        assert_eq!(sniff(&[0x01, 0xE9, 0x20, 0x41]), Magic::Unknown);
        assert_eq!(sniff(&[0xB0, 0xA1, 0xB0, 0xA2, 0xB1, 0xA3, 0xC4, 0xE3]), Magic::Unknown);
        assert_eq!(sniff(&[0xFF, 0xFF, 0xFF]), Magic::Unknown);
    }

    #[test]
    fn extension_is_irrelevant() {
        // A PE file is an executable no matter what it is called.
        assert!(matches!(sniff(b"MZ...pretending to be invoice.docx"), Magic::Executable(_)));
    }

    #[test]
    fn text_subformats() {
        assert_eq!(classify_text("a,b,c\n1,2,3\n4,5,6\n"), FileFormat::Csv);
        assert_eq!(classify_text("a\tb\n1\t2\n"), FileFormat::Tsv);
        assert_eq!(classify_text("# Title\n\nSome text\n"), FileFormat::Markdown);
        assert_eq!(
            classify_text("<!DOCTYPE html><html><script>x</script></html>"),
            FileFormat::Html
        );
        assert_eq!(classify_text("just words\nand more words\n"), FileFormat::PlainText);
        assert_eq!(classify_text("<?xml version=\"1.0\"?><w:wordDocument/>"), FileFormat::FlatXml);
    }

    #[test]
    fn content_types() {
        assert_eq!(
            opc_format_from_content_type("application/vnd.ms-word.document.macroEnabled.main+xml"),
            FileFormat::Docm
        );
        assert!(FileFormat::Docm.is_macro_enabled_type());
        assert!(!FileFormat::Docx.is_macro_enabled_type());
        assert_eq!(
            odf_format_from_mimetype("application/vnd.oasis.opendocument.text-template"),
            FileFormat::Odt
        );
        assert_eq!(FileFormat::Vsdx.family(), Family::Diagram);
    }
}
