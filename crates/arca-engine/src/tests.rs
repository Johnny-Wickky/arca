// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Tests of the engine.
//!
//! Everything that needs the real guest module runs only when `ARCA_GUEST_XRAY_WASM` names
//! the built `arca_guest_xray.wasm` (CI does, and sets `ARCA_REQUIRE_E2E=1` so that a
//! missing module fails the build instead of silently skipping the checks).

use std::io::Cursor;

use arca_preview::model::{
    Align, Block, Cell, CellKind, FileFormat, Grid, GridCell, GridRow, ImageInfo, Notice,
    NoticeKind, ParaStyle, Paragraph, Preview, PreviewStats, Row, Run, Section, SectionKind, Table,
};

use crate::json::check::{Value, parse};
use crate::render::{self, PreviewIndex};
use arca_abi::request::Op;

use crate::{Engine, EngineConfig, Host, Status, op};

fn required() -> bool {
    std::env::var_os("ARCA_REQUIRE_E2E").is_some_and(|v| !v.is_empty() && v != "0")
}

fn host() -> Option<Host> {
    let Some(path) = std::env::var_os("ARCA_GUEST_XRAY_WASM") else {
        assert!(
            !required(),
            "ARCA_REQUIRE_E2E is set but ARCA_GUEST_XRAY_WASM is not: the engine tests would verify nothing"
        );
        #[allow(clippy::print_stderr)] // a skipped test says so
        {
            eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        }
        return None;
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
    Some(Host::new(Engine::new(bytes, EngineConfig::default())))
}

fn load(h: &Host, bytes: &[u8]) -> u64 {
    let doc = h.doc_new(u64::MAX);
    h.doc_append(doc, bytes).unwrap();
    doc
}

fn json(bytes: &[u8]) -> Value {
    parse(bytes).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(bytes)))
}

fn para(text: &str) -> Paragraph {
    Paragraph::plain(text)
}

fn sample_preview() -> Preview {
    let heading = Paragraph {
        style: ParaStyle::Heading1,
        align: Align::Center,
        level: 2,
        marker: "1.".into(),
        runs: vec![
            Run { text: "Заголовок ".into(), style: 1, href: String::new() },
            Run {
                text: "ссылка \"q\"".into(),
                style: 4 | 128,
                href: "https://x.test/?a=1&b=2".into(),
            },
        ],
    };
    let table = Table {
        rows: (0..3)
            .map(|r| Row {
                cells: vec![
                    Cell { span: 2, header: r == 0, paras: vec![para(&format!("r{r}c0"))] },
                    Cell { span: 1, header: false, paras: vec![para("x"), para("y")] },
                ],
            })
            .collect(),
    };
    let grid = Grid {
        cols: 4,
        rows: (1..=5)
            .map(|i| GridRow {
                index: i * 10,
                hidden: i == 2,
                cells: vec![
                    GridCell {
                        col: 0,
                        kind: CellKind::Text,
                        text: format!("name {i}"),
                        href: format!("https://x.example/{i}"),
                    },
                    GridCell {
                        col: 3,
                        kind: CellKind::Number,
                        text: "1 234,5".into(),
                        href: String::new(),
                    },
                ],
            })
            .collect(),
        truncated: true,
    };
    Preview {
        format: FileFormat::Xlsx,
        sections: vec![
            Section {
                kind: SectionKind::Body,
                name: String::new(),
                hidden: false,
                blocks: vec![
                    Block::Para(heading),
                    Block::Table(table),
                    Block::Rule,
                    Block::Image(ImageInfo {
                        alt: "logo".into(),
                        media: "png".into(),
                        bytes: 99,
                        external: true,
                        id: 3,
                    }),
                    Block::Para(para("end")),
                ],
            },
            Section {
                kind: SectionKind::Sheet,
                name: "Лист \"1\"".into(),
                hidden: true,
                blocks: vec![
                    Block::Grid(grid),
                    Block::Table(Table::default()),
                    Block::Para(para("tail")),
                ],
            },
            Section {
                kind: SectionKind::Slide,
                name: String::new(),
                hidden: false,
                blocks: vec![],
            },
        ],
        notices: vec![Notice { kind: NoticeKind::HiddenShown, count: 2, detail: "text".into() }],
        stats: PreviewStats { chars: 40, items: 12, truncated: false },
    }
}

#[test]
fn item_counts_follow_the_flattening_rules() {
    let idx = PreviewIndex::new(sample_preview());
    // para + 3 table rows + rule + image + para
    assert_eq!(idx.item_count(0), 7);
    // grid header + 5 rows, empty table (0), para
    assert_eq!(idx.item_count(1), 7);
    assert_eq!(idx.item_count(2), 0);
    assert_eq!(idx.item_count(9), 0);
}

#[test]
fn any_page_equals_the_matching_slice_of_the_whole() {
    let idx = PreviewIndex::new(sample_preview());
    for section in 0..2usize {
        let total = idx.item_count(section);
        let all = json(&idx.items_json(section, 0, 1000).unwrap());
        assert_eq!(u64::try_from(all.len()).unwrap(), total, "section {section}");
        let total_usize = usize::try_from(total).unwrap();
        for from in 0..=total + 2 {
            let from_usize = usize::try_from(from).unwrap();
            for count in 0..=total_usize + 2 {
                let page = json(&idx.items_json(section, from, count).unwrap());
                let want = usize::try_from(total.saturating_sub(from)).unwrap().min(count);
                assert_eq!(page.len(), want, "section {section} from {from} count {count}");
                for i in 0..want {
                    assert_eq!(
                        page.at(i),
                        all.at(from_usize + i),
                        "item {i} of page {from}+{count}"
                    );
                }
            }
        }
    }
    assert!(idx.items_json(3, 0, 10).is_none(), "unknown section");
}

#[test]
fn item_shapes_are_what_the_app_expects() {
    let idx = PreviewIndex::new(sample_preview());
    let body = json(&idx.items_json(0, 0, 100).unwrap());

    let p = body.at(0).unwrap();
    assert_eq!(p.get("k").and_then(Value::str), Some("p"));
    assert_eq!(p.get("s").and_then(Value::str), Some("Heading1"));
    assert_eq!(p.get("a").and_then(Value::num), Some(1.0));
    assert_eq!(p.get("l").and_then(Value::num), Some(2.0));
    assert_eq!(p.get("m").and_then(Value::str), Some("1."));
    let runs = p.get("r").unwrap();
    assert_eq!(runs.at(0).and_then(|r| r.at(0)).and_then(Value::str), Some("Заголовок "));
    assert_eq!(runs.at(0).map(Value::len), Some(2), "no href → two elements");
    assert_eq!(runs.at(1).and_then(|r| r.at(1)).and_then(Value::num), Some(132.0));
    assert_eq!(
        runs.at(1).and_then(|r| r.at(2)).and_then(Value::str),
        Some("https://x.test/?a=1&b=2")
    );

    // Default values are left out.
    let tail = body.at(6).unwrap();
    assert_eq!(tail.get("s"), None);
    assert_eq!(tail.get("a"), None);
    assert_eq!(tail.get("l"), None);
    assert_eq!(tail.get("m"), None);

    let row0 = body.at(1).unwrap();
    assert_eq!(row0.get("k").and_then(Value::str), Some("tr"));
    assert_eq!(row0.get("n").and_then(Value::num), Some(3.0), "columns = widest row");
    assert_eq!(row0.get("first"), Some(&Value::Bool(true)));
    assert_eq!(row0.get("last"), Some(&Value::Bool(false)));
    assert_eq!(body.at(3).and_then(|r| r.get("last")), Some(&Value::Bool(true)));
    let cell = row0.get("cells").and_then(|c| c.at(0)).unwrap();
    assert_eq!(cell.get("c").and_then(Value::num), Some(2.0));
    assert_eq!(cell.get("h"), Some(&Value::Bool(true)));
    assert_eq!(cell.get("p").map(Value::len), Some(1));
    assert_eq!(
        row0.get("cells").and_then(|c| c.at(1)).and_then(|c| c.get("p")).map(Value::len),
        Some(2)
    );

    assert_eq!(body.at(4).and_then(|r| r.get("k")).and_then(Value::str), Some("r"));
    let img = body.at(5).unwrap();
    assert_eq!(img.get("k").and_then(Value::str), Some("i"));
    assert_eq!(img.get("bytes").and_then(Value::num), Some(99.0));
    assert_eq!(img.get("ext"), Some(&Value::Bool(true)));

    let sheet = json(&idx.items_json(1, 0, 100).unwrap());
    let gh = sheet.at(0).unwrap();
    assert_eq!(gh.get("k").and_then(Value::str), Some("gh"));
    assert_eq!(gh.get("cols").and_then(Value::num), Some(4.0));
    assert_eq!(gh.get("rows").and_then(Value::num), Some(5.0));
    assert_eq!(gh.get("trunc"), Some(&Value::Bool(true)));
    let w = gh.get("w").unwrap();
    assert_eq!(w.len(), 4);
    assert_eq!(w.at(0).and_then(Value::num), Some(6.0), "\"name 1\" is 6 characters");
    assert_eq!(w.at(3).and_then(Value::num), Some(7.0), "'1 234,5' is 7 chars");
    assert_eq!(w.at(1).and_then(Value::num), Some(4.0), "empty column keeps the minimum");
    let gr = sheet.at(2).unwrap();
    assert_eq!(gr.get("k").and_then(Value::str), Some("gr"));
    assert_eq!(gr.get("i").and_then(Value::num), Some(20.0));
    assert_eq!(gr.get("h"), Some(&Value::Bool(true)));
    assert_eq!(
        gr.get("c").and_then(|c| c.at(1)).and_then(|c| c.at(1)).and_then(Value::num),
        Some(1.0)
    );
    assert_eq!(
        gr.get("c").and_then(|c| c.at(1)).and_then(|c| c.at(2)).and_then(Value::str),
        Some("1 234,5")
    );
    // A cell with a link carries its address as a fourth element; one without has three.
    let linked = gr.get("c").and_then(|c| c.at(0)).unwrap();
    assert_eq!(linked.len(), 4);
    assert_eq!(linked.at(3).and_then(Value::str), Some("https://x.example/2"));
    assert_eq!(gr.get("c").and_then(|c| c.at(1)).map(Value::len), Some(3));
}

#[test]
fn preview_meta_lists_sections_and_notices() {
    let meta = json(&PreviewIndex::new(sample_preview()).meta_json());
    assert_eq!(meta.get("format").and_then(Value::str), Some("Xlsx"));
    assert_eq!(meta.get("family").and_then(Value::str), Some("Spreadsheet"));
    let s1 = meta.get("sections").and_then(|s| s.at(1)).unwrap();
    assert_eq!(s1.get("kind").and_then(Value::str), Some("Sheet"));
    assert_eq!(s1.get("name").and_then(Value::str), Some("Лист \"1\""));
    assert_eq!(s1.get("hidden"), Some(&Value::Bool(true)));
    assert_eq!(s1.get("items").and_then(Value::num), Some(7.0));
    let n = meta.get("notices").and_then(|n| n.at(0)).unwrap();
    assert_eq!(n.get("kind").and_then(Value::str), Some("HiddenShown"));
    assert_eq!(n.get("count").and_then(Value::num), Some(2.0));
}

#[test]
fn a_huge_sparse_grid_is_indexed_without_materialising_it() {
    let rows = 200_000u32;
    let grid = Grid {
        cols: 3,
        rows: (1..=rows)
            .map(|i| GridRow {
                index: i,
                hidden: false,
                cells: vec![GridCell {
                    col: 1,
                    kind: CellKind::Number,
                    text: i.to_string(),
                    href: String::new(),
                }],
            })
            .collect(),
        truncated: false,
    };
    let preview = Preview {
        format: FileFormat::Csv,
        sections: vec![Section {
            kind: SectionKind::Sheet,
            name: "big".into(),
            hidden: false,
            blocks: vec![Block::Grid(grid)],
        }],
        notices: vec![],
        stats: PreviewStats::default(),
    };
    let idx = PreviewIndex::new(preview);
    assert_eq!(idx.item_count(0), u64::from(rows) + 1);
    let page = json(&idx.items_json(0, 123_456, 50).unwrap());
    assert_eq!(page.len(), 50);
    // Item 0 is the header, so item k is the row numbered k.
    assert_eq!(page.at(0).and_then(|r| r.get("i")).and_then(Value::num), Some(123_456.0));
    let last = json(&idx.items_json(0, u64::from(rows), 10).unwrap());
    assert_eq!(last.len(), 1);
    assert_eq!(last.at(0).and_then(|r| r.get("i")).and_then(Value::num), Some(f64::from(rows)));
}

#[test]
fn about_screen_keeps_the_upstream_card_first_in_both_languages() {
    for code in [0u32, 1] {
        let v = json(&render::about_json(render::lang_from_code(code)));
        let cards = v.get("screen").and_then(|s| s.get("cards")).unwrap();
        assert!(cards.len() >= 1);
        let first = cards.at(0).unwrap();
        assert_eq!(first.get("role").and_then(Value::str), Some("Upstream"));
        assert_eq!(
            first.get("support").map(Value::len).unwrap_or(0),
            arca_attribution::UPSTREAM_DONATION_LINKS.len(),
            "the upstream support links are all present"
        );
        assert_eq!(v.get("fingerprint").and_then(Value::str).map(str::len), Some(64));
        assert!(
            v.get("screen")
                .and_then(|s| s.get("legal"))
                .and_then(Value::str)
                .unwrap()
                .contains("AGPL")
                || v.get("screen")
                    .and_then(|s| s.get("legal"))
                    .and_then(Value::str)
                    .unwrap()
                    .contains("Affero")
        );
    }
    let ru = json(&render::about_json(render::lang_from_code(0)));
    let title = ru
        .get("screen")
        .and_then(|s| s.get("cards"))
        .and_then(|c| c.at(0))
        .and_then(|c| c.get("title"))
        .and_then(Value::str);
    assert_eq!(title, Some("Создатели оригинального ядра Arca"));
}

#[test]
fn passport_record_roundtrips_and_garbage_starts_afresh() {
    let today = 20_000;
    let r1 = render::passport_update(&[], today, 2, 1);
    assert_eq!(r1.len(), 36);
    let r2 = render::passport_update(&r1, today + 4, 0, 3);
    let v = json(&render::passport_json(render::lang_from_code(1), &r2, today + 4));
    assert_eq!(v.get("documents").and_then(Value::num), Some(2.0));
    assert_eq!(v.get("trackers").and_then(Value::num), Some(2.0));
    assert_eq!(v.get("active").and_then(Value::num), Some(4.0));
    assert_eq!(v.get("days").and_then(Value::num), Some(5.0));
    assert!(v.get("body").and_then(Value::str).unwrap().contains("safely opened 2 documents"));
    // A corrupted record never panics and never reports someone else's numbers.
    let mut bad = r2.clone();
    bad[10] ^= 0xFF;
    let v = json(&render::passport_json(render::lang_from_code(0), &bad, today));
    assert_eq!(v.get("documents").and_then(Value::num), Some(0.0));
    assert!(parse(&render::passport_json(render::lang_from_code(0), &[1, 2, 3], today)).is_ok());
}

#[test]
fn licence_texts_are_embedded() {
    assert!(render::license_text(0).unwrap().contains("GNU AFFERO"));
    assert!(render::license_text(1).is_some());
    assert!(render::license_text(2).is_some());
    assert!(render::license_text(3).is_none());
}

#[test]
fn release_readiness_reports_placeholders_as_json() {
    let v = json(&render::release_blockers_json());
    assert!(matches!(v.get("ready"), Some(Value::Bool(_))));
    assert!(v.get("blockers").is_some());
}

#[test]
fn documents_have_limits_handles_are_never_reused_and_stale_ones_fail_cleanly() {
    let h = Host::new(Engine::new(vec![0u8; 8], EngineConfig::default()));
    let a = h.doc_new(10);
    h.doc_append(a, b"12345").unwrap();
    h.doc_append(a, b"67890").unwrap();
    assert_eq!(h.doc_append(a, b"x"), Err(Status::TooLarge));
    assert_eq!(h.doc_len(a), Ok(10));
    // Reading from a stream respects the limit too.
    let b = h.doc_new(4);
    assert_eq!(h.doc_read_from(b, &mut Cursor::new(vec![7u8; 100])), Err(Status::TooLarge));
    let c = h.doc_new(1 << 20);
    assert_eq!(h.doc_read_from(c, &mut Cursor::new(vec![7u8; 100_000])), Ok(100_000));
    assert_eq!(h.doc_len(c), Ok(100_000));
    assert!(h.doc_free(a));
    assert!(!h.doc_free(a), "second free is harmless");
    assert_eq!(h.doc_len(a), Err(Status::BadHandle));
    assert_eq!(h.doc_append(a, b"x"), Err(Status::BadHandle));
    let d = h.doc_new(10);
    assert!(d > c, "handles only grow");
    assert_eq!(h.run(0, d, op::SCAN, 0).unwrap_err(), Status::BadHandle, "no such job");
    let job = h.job_new();
    assert_eq!(h.run(job, 9999, op::SCAN, 0).unwrap_err(), Status::BadHandle);
    assert_eq!(h.run(job, d, 77, 0).unwrap_err(), Status::BadArgument, "unknown operation");
    assert_eq!(h.run(job, d, op::SCAN, 1).unwrap_err(), Status::BadArgument, "unknown flag");
    assert_eq!(h.outcome_status(5).unwrap_err(), Status::BadHandle);
    assert_eq!(h.report_json(5).unwrap_err(), Status::BadHandle);
}

#[test]
fn configuration_cannot_be_set_to_defeat_the_sandbox() {
    let c = EngineConfig {
        max_input_bytes: usize::MAX,
        max_memory_bytes: usize::MAX,
        scan_timeout: std::time::Duration::from_secs(10_000_000),
        preview_timeout: std::time::Duration::ZERO,
    }
    .clamped();
    assert_eq!(c.max_input_bytes, 512 << 20);
    assert_eq!(c.max_memory_bytes, 1 << 30);
    assert_eq!(c.scan_timeout.as_secs(), 600);
    assert_eq!(c.preview_timeout.as_secs(), 5);
    let c = EngineConfig { max_input_bytes: 0, max_memory_bytes: 0, ..EngineConfig::default() }
        .clamped();
    assert_eq!(c.max_input_bytes, 1 << 20);
    assert_eq!(c.max_memory_bytes, 160 << 20);
}

// ----------------------------------------------------------------------------------------
// With the real guest
// ----------------------------------------------------------------------------------------

#[test]
fn the_self_test_passes_through_the_real_sandbox() {
    let Some(h) = host() else { return };
    let v = json(&crate::selftest::run(&h));
    assert_eq!(v.get("ok"), Some(&Value::Bool(true)), "{v:?}");
    let steps = v.get("steps").unwrap();
    assert_eq!(steps.len(), 4);
    for i in 0..steps.len() {
        assert_eq!(
            steps.at(i).and_then(|s| s.get("ok")),
            Some(&Value::Bool(true)),
            "{:?}",
            steps.at(i)
        );
    }
    assert_eq!(h.live_objects(), (0, 0, 0), "the self-test cleans up after itself");
}

#[test]
fn scan_preview_and_sanitize_through_the_host() {
    let Some(h) = host() else { return };
    let doc = load(&h, &crate::selftest::sample_document());
    let job = h.job_new();

    let o = h.run(job, doc, op::SCAN, 0).unwrap();
    assert_eq!(h.outcome_status(o), Ok(Status::Ok));
    let report = json(&h.report_json(o).unwrap());
    assert_eq!(report.get("verdict").and_then(Value::str), Some("HiddenTrackers"));
    assert_eq!(report.get("format").and_then(Value::str), Some("Docx"));
    assert_eq!(
        report.get("counters").and_then(|c| c.get("trackers")).and_then(Value::num),
        Some(1.0)
    );
    assert_eq!(
        h.preview_meta_json(o).unwrap_err(),
        Status::BadArgument,
        "a report is not a preview"
    );
    let info = json(&h.outcome_info_json(o).unwrap());
    assert_eq!(info.get("statusName").and_then(Value::str), Some("Ok"));
    assert!(info.get("stats").and_then(|s| s.get("fuel")).and_then(Value::num).unwrap() > 0.0);

    let p = h.run(job, doc, op::PREVIEW, 0).unwrap();
    let meta = json(&h.preview_meta_json(p).unwrap());
    let hidden_omitted = meta.get("notices").map(Value::len).unwrap_or(0);
    let items = String::from_utf8(h.preview_items_json(p, 0, 0, 50).unwrap()).unwrap();
    assert!(items.contains("Arca self-test"));
    assert!(!items.contains("hidden line"), "hidden text stays out unless requested: {items}");
    assert!(hidden_omitted >= 1, "and the omission is announced");
    let p2 = h.run(job, doc, op::PREVIEW, op::REVEAL_HIDDEN).unwrap();
    let items = String::from_utf8(h.preview_items_json(p2, 0, 0, 50).unwrap()).unwrap();
    assert!(items.contains("hidden line"));
    assert_eq!(
        h.preview_items_json(p2, 5, 0, 50).unwrap_err(),
        Status::BadArgument,
        "no such section"
    );

    let s = h.run(job, doc, op::SANITIZE, op::ACCEPT_REVISIONS | op::STRIP_METADATA).unwrap();
    assert_eq!(h.outcome_status(s), Ok(Status::Ok));
    let info = json(&h.sanitized_info_json(s).unwrap());
    assert_eq!(
        info.get("verification").and_then(|v| v.get("verdict")).and_then(Value::str),
        Some("Clean")
    );
    let mut out = Vec::new();
    let n = h.sanitized_write_to(s, &mut out).unwrap();
    assert_eq!(usize::try_from(n).unwrap(), out.len());
    assert_eq!(h.sanitized_len(s), Ok(n));
    assert!(out.starts_with(b"PK"));

    for x in [o, p, p2, s] {
        assert!(h.outcome_free(x));
    }
    assert!(h.doc_free(doc));
    assert!(h.job_free(job));
    assert_eq!(h.live_objects(), (0, 0, 0));
}

#[test]
fn failures_are_classified_not_thrown() {
    let Some(h) = host() else { return };
    let job = h.job_new();
    let run = |bytes: &[u8], operation: u16, f: u32| {
        let doc = load(&h, bytes);
        let o = h.run(job, doc, operation, f).unwrap();
        h.doc_free(doc);
        h.outcome_status(o).unwrap()
    };
    // An executable has no preview and no sterile form.
    assert_eq!(run(b"MZ\x90\x00\x03\x00\x00\x00", op::PREVIEW, 0), Status::Unsupported);
    assert_eq!(run(b"plain text", op::SANITIZE, op::ACCEPT_REVISIONS), Status::Unsupported);
    // A truncated archive is rejected, not a crash.
    let mut broken = crate::selftest::sample_document();
    broken.truncate(broken.len() / 2);
    assert_eq!(run(&broken, op::PREVIEW, 0), Status::Rejected);
    // The scan itself always yields a report (it says "Rejected" inside it).
    assert_eq!(run(&broken, op::SCAN, 0), Status::Ok);
    assert_eq!(run(b"", op::SCAN, 0), Status::Ok);
}

#[test]
fn a_cancelled_job_stops_the_run() {
    let Some(h) = host() else { return };
    let job = h.job_new();
    // Cancel before the run starts: the first slice boundary notices.
    assert!(h.job_cancel(job));
    let doc = load(&h, &crate::selftest::sample_document());
    let o = h.run(job, doc, op::SCAN, 0).unwrap();
    assert_eq!(h.outcome_status(o), Ok(Status::Cancelled));
    assert_eq!(h.report_json(o).unwrap_err(), Status::BadArgument);
    assert!(!h.job_cancel(9999));
}

#[test]
fn oversized_input_is_refused_before_the_sandbox_is_involved() {
    let Some(host) = host() else { return };
    let small = Host::new(Engine::new(
        std::fs::read(std::env::var_os("ARCA_GUEST_XRAY_WASM").unwrap()).unwrap(),
        EngineConfig { max_input_bytes: 1 << 20, ..EngineConfig::default() },
    ));
    let doc = small.doc_new(u64::MAX);
    assert_eq!(small.doc_append(doc, &vec![0u8; (1 << 20) + 1]), Err(Status::TooLarge));
    drop(host);
}

#[test]
fn concurrent_runs_do_not_interfere() {
    let Some(h) = host() else { return };
    let h = std::sync::Arc::new(h);
    let sample = crate::selftest::sample_document();
    std::thread::scope(|s| {
        for t in 0..4 {
            let h = std::sync::Arc::clone(&h);
            let sample = sample.clone();
            s.spawn(move || {
                for _ in 0..3 {
                    let doc = load(&h, &sample);
                    let job = h.job_new();
                    let o = h
                        .run(job, doc, if t % 2 == 0 { op::SCAN } else { op::PREVIEW }, 0)
                        .unwrap();
                    assert_eq!(h.outcome_status(o), Ok(Status::Ok));
                    h.outcome_free(o);
                    h.job_free(job);
                    h.doc_free(doc);
                }
            });
        }
    });
    assert_eq!(h.live_objects(), (0, 0, 0));
}

#[test]
fn the_budget_of_a_preview_or_a_sterile_copy_grows_with_what_the_scan_unpacked() {
    let engine = Engine::new(vec![0u8; 8], EngineConfig::default());
    let len = 3 << 20;
    let none_preview = engine.policy_for(Op::Preview, len, 0);
    let none_sanitize = engine.policy_for(Op::Sanitize, len, 0);
    // 44 MB of XML in a 3 MB workbook (the figure the scan reports).
    let inflated = 44 << 20;
    let preview = engine.policy_for(Op::Preview, len, inflated);
    let sanitize = engine.policy_for(Op::Sanitize, len, inflated);
    assert!(preview.fuel.for_input(len) > none_preview.fuel.for_input(len));
    assert!(sanitize.fuel.for_input(len) > 2 * none_sanitize.fuel.for_input(len));
    // A sterile copy of such a workbook measured 4 700 fuel per expanded byte: it now fits.
    assert!(sanitize.fuel.for_input(len) > 4_700 * inflated);
    assert!(sanitize.memory_limit_for(len) > none_sanitize.memory_limit_for(len));
    // A scan within 20:1 ratio keeps its base budget; above 20:1 it receives adaptive excess fuel.
    assert_eq!(
        engine.policy_for(Op::Scan, len, inflated).fuel,
        engine.policy_for(Op::Scan, len, 0).fuel
    );
    let highly_compressed = 100 << 20; // > 20:1 for a 3 MB file
    assert!(
        engine.policy_for(Op::Scan, len, highly_compressed).fuel.for_input(len)
            > engine.policy_for(Op::Scan, len, 0).fuel.for_input(len)
    );
    // The fixed ceilings stay: fuel, the device's memory limit and the wall-clock limits.
    let huge = engine.policy_for(Op::Sanitize, len, u64::MAX);
    assert!(huge.fuel.for_input(len) <= huge.fuel.max);
    assert!(huge.memory_limit_for(len) <= engine.config().max_memory_bytes);
    assert_eq!(huge.max_wall_time, none_sanitize.max_wall_time);
}

#[test]
fn a_sterile_copy_of_a_picture_gets_a_picture_budget_and_nothing_else_does() {
    use arca_abi::request::flags;
    let engine = Engine::new(vec![0u8; 8], EngineConfig::default());
    let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
    let len = png.len();
    let adjusted = |op: Op, op_flags: u32, doc: &[u8]| {
        let mut policy = engine.policy_for(op, doc.len(), 0);
        engine.size_for_picture_copy(&mut policy, op, op_flags, doc);
        policy
    };
    let plain = engine.policy_for(Op::Sanitize, len, 0);
    let reduced = adjusted(Op::Sanitize, flags::SANITIZE_ALL, &png);
    let full = adjusted(Op::Sanitize, flags::SANITIZE_ALL | flags::FULL_SIZE, &png);
    // A picture costs by its pixels: more fuel and time than a document of the same size, and the
    // full-size copy more than the reduced one.
    assert!(reduced.fuel.for_input(len) > 10 * plain.fuel.for_input(len));
    assert!(full.fuel.for_input(len) > reduced.fuel.for_input(len));
    assert!(full.max_wall_time > reduced.max_wall_time);
    assert!(reduced.memory_limit_for(len) >= 256 << 20);
    // The fixed ceilings stay.
    assert!(full.fuel.for_input(len) <= full.fuel.max);
    assert!(full.max_wall_time <= Some(std::time::Duration::from_secs(900)));
    // Nothing else gets it: not a document, not another operation on a picture.
    let docx = b"PK\x03\x04 not a picture";
    assert_eq!(adjusted(Op::Sanitize, flags::SANITIZE_ALL, docx).fuel, plain.fuel);
    assert_eq!(
        adjusted(Op::Sanitize, flags::SANITIZE_ALL | flags::FULL_SIZE, docx).max_wall_time,
        plain.max_wall_time
    );
    assert_eq!(adjusted(Op::Scan, 0, &png).fuel, engine.policy_for(Op::Scan, len, 0).fuel);
}
