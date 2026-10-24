// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Tests of the host's defences against a hostile document and a hostile (compromised) guest.
//! None of them needs the guest module: they build the hostile inputs themselves.

use arca_abi::request::Op;
use arca_container::zip::writer::{Compression, ZipWriter};
use arca_preview::model::{
    Block, CellKind, FileFormat, Grid, GridCell, GridRow, Notice, NoticeKind, Preview,
    PreviewStats, Section, SectionKind,
};
use arca_xray::sanitize::SanitizedDocument;
use arca_xray::{Finding, Severity, ThreatKind};
use zeroize::Zeroizing;

use crate::host::{check_sterile, grow_wiping, sniff_inflated};
use crate::json::JsonWriter;
use crate::render::{self, PageError, PreviewIndex};
use crate::{Engine, EngineConfig, Host, RunOutput, Status};

fn dummy_engine() -> Engine {
    Engine::new(vec![0u8; 8], EngineConfig::default())
}

// ---------------------------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------------------------

#[test]
fn only_a_sterile_copy_may_come_back_larger_than_a_report_can_be() {
    let engine = dummy_engine();
    let len = 100 << 20;
    for op in [Op::Scan, Op::Preview, Op::Render] {
        assert_eq!(engine.policy_for(op, len, 0).max_output_bytes, 64 << 20, "{op:?}");
    }
    assert_eq!(engine.policy_for(Op::Image, len, 0).max_output_bytes, 40 << 20);
    // A sterile copy is about as large as the original, plus the room a report needs.
    assert_eq!(engine.policy_for(Op::Sanitize, len, 0).max_output_bytes, (64 << 20) + len);
}

#[test]
fn the_fuel_ceiling_is_a_constant_no_declared_size_can_raise() {
    const CEILING: u64 = 1_000_000_000_000;
    let engine = dummy_engine();
    for op in [Op::Scan, Op::Preview, Op::Sanitize, Op::Render, Op::Image] {
        for len in [0usize, 1 << 20, 100 << 20, usize::MAX >> 8] {
            for inflated in [0u64, 1 << 30, u64::MAX] {
                let policy = engine.policy_for(op, len, inflated);
                assert!(policy.fuel.max <= CEILING, "{op:?} {len} {inflated}: {}", policy.fuel.max);
                assert!(policy.fuel.for_input(len) <= CEILING, "{op:?} {len} {inflated}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// sniff_inflated trusts nothing the central directory says
// ---------------------------------------------------------------------------------------------

fn p16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn p32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

/// One member of a hand-built archive: its stored bytes and the sizes its headers *claim*.
struct Claim<'a> {
    name: &'a str,
    stored: &'a [u8],
    method: u16,
    uncompressed: u64,
    zip64: bool,
}

/// An archive whose headers say whatever the claims say (nothing here is ever inflated).
fn archive(claims: &[Claim<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    let extra_for = |c: &Claim<'_>| {
        if !c.zip64 {
            return Vec::new();
        }
        let mut e = Vec::new();
        p16(&mut e, 1);
        p16(&mut e, 16);
        e.extend_from_slice(&c.uncompressed.to_le_bytes());
        e.extend_from_slice(&u64::try_from(c.stored.len()).unwrap().to_le_bytes());
        e
    };
    let sizes = |c: &Claim<'_>| {
        if c.zip64 {
            (u32::MAX, u32::MAX)
        } else {
            (u32::try_from(c.stored.len()).unwrap(), u32::try_from(c.uncompressed).unwrap())
        }
    };
    for c in claims {
        offsets.push(u32::try_from(out.len()).unwrap());
        let (csize, usize_) = sizes(c);
        let extra = extra_for(c);
        p32(&mut out, 0x0403_4b50);
        p16(&mut out, 45);
        p16(&mut out, 0);
        p16(&mut out, c.method);
        p16(&mut out, 0);
        p16(&mut out, 0x21);
        p32(&mut out, 0);
        p32(&mut out, csize);
        p32(&mut out, usize_);
        p16(&mut out, u16::try_from(c.name.len()).unwrap());
        p16(&mut out, u16::try_from(extra.len()).unwrap());
        out.extend_from_slice(c.name.as_bytes());
        out.extend_from_slice(&extra);
        out.extend_from_slice(c.stored);
    }
    let cd_offset = u32::try_from(out.len()).unwrap();
    let mut cd = Vec::new();
    for (c, off) in claims.iter().zip(&offsets) {
        let (csize, usize_) = sizes(c);
        let extra = extra_for(c);
        p32(&mut cd, 0x0201_4b50);
        p16(&mut cd, 45);
        p16(&mut cd, 45);
        p16(&mut cd, 0);
        p16(&mut cd, c.method);
        p16(&mut cd, 0);
        p16(&mut cd, 0x21);
        p32(&mut cd, 0);
        p32(&mut cd, csize);
        p32(&mut cd, usize_);
        p16(&mut cd, u16::try_from(c.name.len()).unwrap());
        p16(&mut cd, u16::try_from(extra.len()).unwrap());
        p16(&mut cd, 0);
        p16(&mut cd, 0);
        p16(&mut cd, 0);
        p32(&mut cd, 0);
        p32(&mut cd, *off);
        cd.extend_from_slice(c.name.as_bytes());
        cd.extend_from_slice(&extra);
    }
    let cd_size = u32::try_from(cd.len()).unwrap();
    out.extend_from_slice(&cd);
    p32(&mut out, 0x0605_4b50);
    p16(&mut out, 0);
    p16(&mut out, 0);
    p16(&mut out, u16::try_from(claims.len()).unwrap());
    p16(&mut out, u16::try_from(claims.len()).unwrap());
    p32(&mut out, cd_size);
    p32(&mut out, cd_offset);
    p16(&mut out, 0);
    out
}

#[test]
fn an_honest_archive_gets_its_declared_size() {
    // Bytes that do not compress much, so the file is about as large as what it holds.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let noise: Vec<u8> = (0..300_000)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u8::try_from(state >> 56).unwrap()
        })
        .collect();
    let mut w = ZipWriter::new();
    w.add("a.xml", &noise, Compression::Deflated).unwrap();
    let bytes = w.finish().unwrap();
    assert_eq!(sniff_inflated(&bytes), Some(300_000));
    assert_eq!(sniff_inflated(b"not an archive"), None);
}

#[test]
fn declared_sizes_cannot_buy_a_big_budget() {
    // A megabyte of real, stored data makes the file big enough for the 100-to-1 cap on the
    // whole to allow a lot; the lie is in the other member: 10 bytes that "inflate" to 200 MiB.
    let filler = vec![7u8; 1 << 20];
    let lie_32 = 200u64 << 20;
    for zip64 in [false, true] {
        let bytes = archive(&[
            Claim {
                name: "real.bin",
                stored: &filler,
                method: 0,
                uncompressed: 1 << 20,
                zip64: false,
            },
            Claim { name: "lie.xml", stored: &[3; 10], method: 8, uncompressed: lie_32, zip64 },
        ]);
        let hint = sniff_inflated(&bytes).expect("the archive is well-formed");
        // The lie counts for what ten bytes could be: 10 x 100 + 1 MiB.
        assert!(hint <= (1 << 20) + 10 * 100 + (1 << 20), "zip64={zip64}: {hint}");
        assert!(hint >= 1 << 20, "the honest member still counts: {hint}");
    }
    // The same lie, many times: each entry is capped, so the sum stays near the real size.
    let claims: Vec<Claim<'_>> = (0..200)
        .map(|i| Claim {
            name: Box::leak(format!("lie{i}.xml").into_boxed_str()),
            stored: &[1; 10],
            method: 8,
            uncompressed: 4 << 30 >> 2,
            zip64: i % 2 == 0,
        })
        .collect();
    let hint = sniff_inflated(&archive(&claims)).unwrap();
    assert!(hint <= 200 * ((1 << 20) + 1_000), "{hint}");
}

// ---------------------------------------------------------------------------------------------
// The host does not take the guest's word for a sterile copy
// ---------------------------------------------------------------------------------------------

fn sterile(bytes: Vec<u8>) -> SanitizedDocument {
    let verification = arca_xray::scan_default(&bytes);
    SanitizedDocument { format: verification.format, bytes, actions: Vec::new(), verification }
}

fn zip_with(parts: &[&str]) -> Vec<u8> {
    let mut w = ZipWriter::new();
    for p in parts {
        w.add(p, b"<x/>", Compression::Deflated).unwrap();
    }
    w.finish().unwrap()
}

#[test]
fn a_sterile_copy_must_be_the_file_the_report_describes() {
    let good = sterile(zip_with(&["[Content_Types].xml", "word/document.xml"]));
    assert_eq!(check_sterile(&good), Ok(()));
    // The report vouches for other bytes than the ones handed over.
    let mut swapped = good.clone();
    swapped.bytes.push(0);
    assert!(check_sterile(&swapped).unwrap_err().contains("digest"));
    let mut elsewhere = good;
    elsewhere.verification.sha256 = "0".repeat(64);
    assert!(check_sterile(&elsewhere).is_err());
    // A copy that is not an archive is checked by its digest alone.
    assert_eq!(check_sterile(&sterile(b"%PDF-1.7 plain".to_vec())), Ok(()));
}

#[test]
fn a_sterile_copy_that_still_holds_what_sterilisation_removes_is_refused() {
    for part in [
        "word/vbaProject.bin",
        "xl/activeX/activeX1.xml",
        "word/embeddings/oleObject1.bin",
        "word/embeddings/Microsoft_Excel_Sheet1.xlsx",
        "xl/macrosheets/sheet1.xml",
    ] {
        let doc = sterile(zip_with(&["[Content_Types].xml", part]));
        assert!(check_sterile(&doc).unwrap_err().contains("part"), "{part}");
    }
    // An archive the host cannot read is not a verified one, however well its digest matches.
    let broken = sterile(b"PK\x03\x04 this is not an archive".to_vec());
    assert!(check_sterile(&broken).unwrap_err().contains("archive"));
}

// ---------------------------------------------------------------------------------------------
// Nothing prints a document
// ---------------------------------------------------------------------------------------------

#[test]
fn debug_output_of_the_host_shows_neither_documents_nor_passwords() {
    let host = Host::new(dummy_engine());
    let doc = host.doc_new(1 << 20);
    host.doc_append(doc, b"TOPSECRETDOCUMENT").unwrap();
    host.doc_set_password(doc, b"hunter2-password").unwrap();
    let shown = format!("{host:?}");
    for leak in ["TOPSECRET", "hunter2", "84, 79, 80, 83", "104, 117, 110, 116"] {
        assert!(!shown.contains(leak), "{leak:?} in {shown}");
    }
    // The size is still there for a log reader.
    assert!(shown.contains("17"), "{shown}");
}

#[test]
fn debug_output_of_a_run_names_the_result_not_its_content() {
    let secret = sterile(zip_with(&["[Content_Types].xml"]));
    let out = RunOutput {
        status: Status::Ok,
        unlock: crate::host::Unlock::Not,
        message: String::new(),
        data: Some(arca_guest_xray::Output::Sanitized(Box::new(secret.clone()))),
        stats: None,
    };
    let shown = format!("{out:?}");
    assert!(shown.contains("Sanitized"), "{shown}");
    assert!(!shown.contains("bytes:") && shown.len() < 400, "{shown}");
}

// ---------------------------------------------------------------------------------------------
// Growing a document buffer
// ---------------------------------------------------------------------------------------------

#[test]
fn a_document_buffer_grows_by_doubling_and_never_past_the_limit() {
    let mut buf: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::new());
    let limit = 10_000;
    for _ in 0..100 {
        grow_wiping(&mut buf, 100, limit).unwrap();
        buf.extend_from_slice(&[0xAB; 100]);
        assert!(buf.capacity() <= limit, "capacity {} past the limit", buf.capacity());
    }
    assert_eq!(buf.len(), 10_000);
    assert!(buf.iter().all(|&b| b == 0xAB), "growing must keep the content");
    // Room that exists is used without a move.
    let before = buf.as_ptr();
    grow_wiping(&mut buf, 0, limit).unwrap();
    assert_eq!(buf.as_ptr(), before);
}

#[test]
fn documents_load_chunk_by_chunk_and_the_limit_holds() {
    let host = Host::new(dummy_engine());
    let doc = host.doc_new(5_000);
    for _ in 0..50 {
        host.doc_append(doc, &[1u8; 100]).unwrap();
    }
    assert_eq!(host.doc_len(doc), Ok(5_000));
    assert_eq!(host.doc_append(doc, b"x"), Err(Status::TooLarge));
    let mut reader = std::io::Cursor::new(vec![9u8; 3_000]);
    let other = host.doc_new(2_000);
    assert_eq!(host.doc_read_from(other, &mut reader), Err(Status::TooLarge));
}

// ---------------------------------------------------------------------------------------------
// JSON of untrusted results
// ---------------------------------------------------------------------------------------------

fn grid_preview(cols: u32, grids: usize, cell_col: u32, text: &str) -> Preview {
    let grid = Grid {
        cols,
        rows: vec![GridRow {
            index: 1,
            hidden: false,
            cells: vec![GridCell {
                col: cell_col,
                kind: CellKind::Text,
                text: text.to_owned(),
                href: String::new(),
            }],
        }],
        truncated: false,
    };
    Preview {
        format: FileFormat::Xlsx,
        sections: vec![Section {
            kind: SectionKind::Sheet,
            name: "S".into(),
            hidden: false,
            blocks: (0..grids).map(|_| Block::Grid(grid.clone())).collect(),
        }],
        notices: Vec::new(),
        stats: PreviewStats::default(),
    }
}

#[test]
fn a_grid_that_declares_many_columns_costs_the_columns_it_uses() {
    // 200 grids each "having" 16 384 columns, with one cell in column 2.
    let idx = PreviewIndex::new(grid_preview(16_384, 200, 2, "abc"));
    // Offsets of 201 blocks + 3 measured widths per grid, not 16 384 per grid.
    assert!(idx.index_bytes() < 201 * 8 + 200 * 3 + 64, "{}", idx.index_bytes());
    // The header still reports every column the grid declares.
    let header = crate::json::check::parse(&idx.items_json(0, 0, 1).unwrap()).unwrap();
    let widths = header.at(0).and_then(|h| h.get("w")).expect("widths");
    assert_eq!(widths.len(), 16_384);
    assert_eq!(widths.at(2).and_then(crate::json::check::Value::num), Some(4.0));
}

#[test]
fn the_widths_of_a_whole_preview_share_one_budget() {
    // 4 MiB of widths in all: grids past it are drawn at the minimum width.
    let idx = PreviewIndex::new(grid_preview(16_000, 400, 15_999, "x"));
    assert!(idx.index_bytes() <= (4 << 20) + 401 * 8, "{}", idx.index_bytes());
    let last = crate::json::check::parse(&idx.items_json(0, 399 * 2, 1).unwrap()).unwrap();
    let widths = last.at(0).and_then(|h| h.get("w")).expect("widths");
    assert_eq!(widths.len(), 16_000, "the header keeps its shape");
}

#[test]
fn a_page_of_huge_cells_is_refused_not_built() {
    let big = "x".repeat(2 << 20);
    let idx = PreviewIndex::new(grid_preview(1, 1, 0, &big));
    // One row of 2 MiB fits an 8 MiB page; four of them in a page would not fit 1 MiB.
    assert!(idx.items_json_within(0, 0, 10, 8 << 20).is_ok());
    assert_eq!(idx.items_json_within(0, 0, 10, 1 << 20), Err(PageError::TooLarge));
    assert_eq!(idx.items_json_within(5, 0, 10, 1 << 20), Err(PageError::NoSuchSection));
}

#[test]
fn a_writer_with_a_limit_stops_instead_of_growing() {
    let mut w = JsonWriter::with_limit(100);
    w.begin_array();
    for _ in 0..1_000 {
        w.string("0123456789");
    }
    w.end_array();
    assert!(w.is_over());
    assert!(w.len() <= 100);
    assert_eq!(w.finish_within(), None);
    let mut ok = JsonWriter::with_limit(100);
    ok.begin_array();
    ok.string("fits");
    ok.end_array();
    assert_eq!(ok.finish_within().as_deref(), Some(&b"[\"fits\"]"[..]));
}

#[test]
fn the_report_json_has_a_size_limit_too() {
    let mut report = arca_xray::scan_default(b"");
    for i in 0..2_000 {
        report.findings.push(Finding {
            kind: ThreatKind::ExternalImage,
            severity: Severity::Medium,
            location: format!("word/_rels/{i}.rels"),
            detail: "https://tracker.invalid/pixel.gif".repeat(20),
            neutralized: true,
        });
    }
    assert!(
        render::report_json_within(&report, 1 << 20).is_some()
            == (render::report_json(&report).len() <= 1 << 20)
    );
    assert!(render::report_json_within(&report, 10_000).is_none());
}

#[test]
fn strings_the_guest_reports_reach_the_app_as_one_visible_line() {
    let mut report = arca_xray::scan_default(b"");
    report.findings.push(Finding {
        kind: ThreatKind::ExternalImage,
        severity: Severity::Medium,
        // A bidirectional override, a tag character, a zero-width space and a forged second line.
        location: "word/\u{202E}gpj.exe\u{E0041}\u{200B}".into(),
        detail: "https://a.invalid/\nverdict: Clean".into(),
        neutralized: true,
    });
    report.rejection = "line one\nline two".into();
    let json = String::from_utf8(render::report_json(&report)).unwrap();
    for raw in ['\u{202E}', '\u{E0041}', '\u{200B}', '\n'] {
        assert!(!json.contains(raw), "{raw:?} reached the app: {json}");
    }
    assert!(json.contains("\\\\u{202E}") && json.contains("\\\\x0A"), "{json}");
    // A well-formed JSON document, whatever the text said.
    assert!(crate::json::check::parse(json.as_bytes()).is_ok());

    // The same for what a preview calls a section or a notice.
    let mut preview = grid_preview(1, 1, 0, "cell \u{202E} text");
    preview.sections[0].name = "Sheet\u{200B}\n2".into();
    preview.notices.push(Notice {
        kind: NoticeKind::HiddenShown,
        count: 1,
        detail: "x\u{E0041}y".into(),
    });
    let meta = String::from_utf8(PreviewIndex::new(preview.clone()).meta_json()).unwrap();
    assert!(!meta.contains('\u{200B}') && !meta.contains('\u{E0041}'), "{meta}");
    // The body of the document is its content: shown as it is.
    let items = String::from_utf8(PreviewIndex::new(preview).items_json(0, 0, 5).unwrap()).unwrap();
    assert!(items.contains('\u{202E}'), "{items}");
}
