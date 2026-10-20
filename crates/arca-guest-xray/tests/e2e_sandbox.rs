//! End-to-end: the real `arca_guest_xray.wasm` inside `arca-sandbox`.
//!
//! Build the guest first, then point the test at it:
//!
//! ```text
//! cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest
//! ARCA_GUEST_XRAY_WASM=target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm \
//!     cargo test -p arca-guest-xray --test e2e_sandbox
//! ```
//!
//! Without the variable the tests report themselves as skipped **unless**
//! `ARCA_REQUIRE_E2E` is set, in which case a missing guest module is a hard
//! failure: a green run must never mean "nothing was checked". CI sets
//! `ARCA_REQUIRE_E2E=1` in the job that builds the guest.

#![allow(clippy::print_stderr, reason = "skip notice and corpus statistics")]
#![allow(clippy::unwrap_used, clippy::panic, reason = "test fixtures outside #[test] functions")]

use arca_abi::request::{Op, flags, header};
use arca_container::zip::writer::{Compression, ZipWriter};
use arca_guest_xray::{decode_preview, decode_report, decode_sanitized};
use arca_sandbox::{
    CancelToken, FuelPolicy, GuestModule, GuestOutput, RunOptions, Sandbox, SandboxError,
    SandboxPolicy,
};
use arca_xray::{ThreatKind, Verdict};

fn run_op(
    sandbox: &Sandbox,
    m: &GuestModule,
    op: Op,
    op_flags: u32,
    doc: &[u8],
    options: &RunOptions,
) -> Result<GuestOutput, SandboxError> {
    sandbox.run_request(m, &header(op, op_flags), doc, options)
}

/// Threat X-ray of `doc` through the guest.
fn scan(sandbox: &Sandbox, m: &GuestModule, doc: &[u8]) -> Result<GuestOutput, SandboxError> {
    run_op(sandbox, m, Op::Scan, 0, doc, &RunOptions::new())
}

/// `true` when a skipped end-to-end test must fail instead (CI).
fn e2e_required() -> bool {
    std::env::var_os("ARCA_REQUIRE_E2E").is_some_and(|v| !v.is_empty() && v != "0")
}

fn guest() -> Option<GuestModule> {
    let Some(path) = std::env::var_os("ARCA_GUEST_XRAY_WASM") else {
        assert!(
            !e2e_required(),
            "ARCA_REQUIRE_E2E is set but ARCA_GUEST_XRAY_WASM is not: the end-to-end tests would verify nothing"
        );
        return None;
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
    Some(GuestModule::new("arca-guest-xray", bytes))
}

fn docx(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut base: Vec<(String, String)> = vec![
        ("[Content_Types].xml".into(), r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.into()),
        ("_rels/.rels".into(), r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.into()),
        ("word/document.xml".into(), r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>hello</w:t></w:r></w:p></w:body></w:document>"#.into()),
    ];
    for (n, d) in parts {
        if let Some(p) = base.iter_mut().find(|(k, _)| k == n) {
            p.1 = (*d).to_owned();
        } else {
            base.push(((*n).to_owned(), (*d).to_owned()));
        }
    }
    let mut w = ZipWriter::new();
    for (n, d) in &base {
        w.add(n, d.as_bytes(), Compression::Deflated).unwrap();
    }
    w.finish().unwrap()
}

/// A worksheet of `rows` × 12 cells (strings, decimals, integers) plus a
/// formula per row, in a minimal SpreadsheetML package: the document class
/// with the highest cost per input byte.
fn xlsx(rows: usize) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut sheet = String::from(
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    let mut x = 0x9E37_79B9_7F4A_7C15_u64;
    for r in 1..=rows {
        write!(sheet, r#"<row r="{r}">"#).unwrap();
        for c in 0..12u8 {
            let col = char::from(b'A' + c);
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            match c % 3 {
                0 => write!(sheet, r#"<c r="{col}{r}" t="s"><v>{}</v></c>"#, x % 2000).unwrap(),
                1 => write!(
                    sheet,
                    r#"<c r="{col}{r}" s="1"><v>{}.{:04}</v></c>"#,
                    x % 10_000,
                    x % 9_973
                )
                .unwrap(),
                _ => write!(sheet, r#"<c r="{col}{r}"><v>{}</v></c>"#, x % 1_000_000).unwrap(),
            }
        }
        write!(sheet, r#"<c r="M{r}"><f>SUM(B{r}:D{r})</f><v>0</v></c></row>"#).unwrap();
    }
    sheet.push_str("</sheetData></worksheet>");
    let mut strings = String::from(
        r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2000" uniqueCount="2000">"#,
    );
    for i in 0..2000 {
        write!(strings, "<si><t>Item {i} description</t></si>").unwrap();
    }
    strings.push_str("</sst>");
    let parts: [(&str, &str); 6] = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        ("xl/sharedStrings.xml", &strings),
        ("xl/worksheets/sheet1.xml", &sheet),
    ];
    let mut w = ZipWriter::new();
    for (name, data) in parts {
        w.add(name, data.as_bytes(), Compression::Deflated).unwrap();
    }
    w.finish().unwrap()
}

#[test]
fn ordinary_spreadsheets_fit_the_default_budget() {
    // Regression for finding B-1: a 100 KB workbook used to need ~6 400 fuel
    // per input byte against a 4 000 budget and was refused as "too complex".
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let policy = SandboxPolicy::default();
    let sandbox = Sandbox::new(policy, [m.pin()]);
    for rows in [300, 1_200] {
        let doc = xlsx(rows);
        let out = scan(&sandbox, &m, &doc).unwrap_or_else(|e| panic!("{rows} rows: {e}"));
        let report = decode_report(out.kind, &out.payload).unwrap();
        assert_eq!(report.verdict, Verdict::Clean, "{rows} rows");
        assert_eq!(report, arca_xray::scan_default(&doc), "{rows} rows: guest != native");
        let per_byte = out.stats.fuel_consumed / u64::try_from(doc.len()).unwrap();
        // Half the budget is headroom for denser spreadsheets (≈ 10:1 deflate).
        assert!(
            per_byte < policy.fuel.per_input_byte / 2,
            "{rows} rows: {per_byte} fuel per input byte, budget {}",
            policy.fuel.per_input_byte
        );
        // The guest holds the input, one decompressed part and parse state, no more.
        let bound = doc.len() * 12 + (8 << 20);
        assert!(
            out.stats.peak_memory_bytes < bound,
            "{rows} rows: peak memory {} bytes for a {} byte workbook",
            out.stats.peak_memory_bytes,
            doc.len()
        );
    }
}

#[test]
fn guest_report_equals_native_report() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    let template = docx(&[(
        "word/_rels/document.xml.rels",
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="https://attacker.example/t.dotm" TargetMode="External"/></Relationships>"#,
    )]);
    for doc in [docx(&[]), template] {
        let out = scan(&sandbox, &m, &doc).unwrap();
        let report = decode_report(out.kind, &out.payload).unwrap();
        assert_eq!(report, arca_xray::scan_default(&doc));
        assert!(out.stats.fuel_consumed > 0);
    }
}

#[test]
fn guest_rejects_bombs_inside_the_sandbox() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    let bomb = "A".repeat(40 << 20);
    let doc = docx(&[("word/media/bomb.xml", &bomb)]);
    let out = scan(&sandbox, &m, &doc).unwrap();
    let report = decode_report(out.kind, &out.payload).unwrap();
    assert_eq!(report.verdict, Verdict::Rejected);
    assert!(report.has(ThreatKind::CompressionBomb));
    // The guest never materialised the 40 MiB member.
    assert!(out.stats.peak_memory_bytes < 16 << 20, "peak {} bytes", out.stats.peak_memory_bytes);
}

#[test]
fn starved_guest_is_stopped() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let policy = SandboxPolicy {
        fuel: FuelPolicy { base: 50_000, per_input_byte: 0, max: 50_000 },
        ..SandboxPolicy::default()
    };
    let sandbox = Sandbox::new(policy, [m.pin()]);
    assert!(matches!(scan(&sandbox, &m, &docx(&[])), Err(SandboxError::FuelExhausted { .. })));
    // A 3 MiB input cannot even be copied into a guest capped at 2 MiB.
    let policy = SandboxPolicy { max_memory_bytes: 2 << 20, ..SandboxPolicy::default() };
    let sandbox = Sandbox::new(policy, [m.pin()]);
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let noise: Vec<u8> = (0..3usize << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect();
    assert_eq!(scan(&sandbox, &m, &noise).unwrap_err(), SandboxError::MemoryLimitExceeded);
}

#[test]
fn slicing_does_not_change_the_guest_result() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let doc = docx(&[(
        "word/_rels/document.xml.rels",
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="https://attacker.example/t.dotm" TargetMode="External"/></Relationships>"#,
    )]);
    let whole = SandboxPolicy { slice_fuel: u64::MAX, ..SandboxPolicy::default() };
    let sliced = SandboxPolicy { slice_fuel: 20_000, ..SandboxPolicy::default() };
    let a = scan(&Sandbox::new(whole, [m.pin()]), &m, &doc).unwrap();
    let b = scan(&Sandbox::new(sliced, [m.pin()]), &m, &doc).unwrap();
    assert_eq!(a.stats.slices, 1);
    assert!(b.stats.slices > 1, "the real guest should need several 20k-fuel slices");
    assert_eq!(a.stats.fuel_consumed, b.stats.fuel_consumed);
    assert_eq!(a.payload[..], b.payload[..]);
}

#[test]
fn the_real_guest_can_be_cancelled_and_timed_out() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    let doc = docx(&[]);

    let token = CancelToken::new();
    token.cancel();
    let cancelled = RunOptions::new().cancel_token(token);
    assert_eq!(
        run_op(&sandbox, &m, Op::Scan, 0, &doc, &cancelled).unwrap_err(),
        SandboxError::Cancelled
    );

    // Cancelled from the progress callback of the very same run: the host
    // abandons the guest in the middle of `arca_run`.
    let token = CancelToken::new();
    let trip = token.clone();
    let options = RunOptions::new().cancel_token(token).on_progress(move |p| {
        if p.fuel_consumed > 100_000 {
            trip.cancel();
        }
    });
    let tiny = SandboxPolicy { slice_fuel: 20_000, ..SandboxPolicy::default() };
    let sandbox = Sandbox::new(tiny, [m.pin()]);
    assert_eq!(
        run_op(&sandbox, &m, Op::Scan, 0, &doc, &options).unwrap_err(),
        SandboxError::Cancelled
    );

    let limit = std::time::Duration::from_nanos(1);
    assert_eq!(
        run_op(&sandbox, &m, Op::Scan, 0, &doc, &RunOptions::new().deadline(limit)).unwrap_err(),
        SandboxError::DeadlineExceeded { limit }
    );
}

#[test]
fn real_world_corpus_if_provided() {
    // Optional: ARCA_CORPUS_DIR=/path/to/documents runs every file through the
    // guest and checks that it matches the native scan.
    let (Some(m), Some(dir)) = (guest(), std::env::var_os("ARCA_CORPUS_DIR")) else {
        assert!(
            !e2e_required(),
            "ARCA_REQUIRE_E2E is set but ARCA_CORPUS_DIR is not: the corpus comparison would verify nothing"
        );
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM / ARCA_CORPUS_DIR not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let out = scan(&sandbox, &m, &data).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let report = decode_report(out.kind, &out.payload).unwrap();
        assert_eq!(report, arca_xray::scan_default(&data), "{path:?}");
        // The preview through the guest equals the native preview byte for byte.
        let policy = arca_preview::PreviewPolicy::default();
        let native = arca_preview::preview(&data, &policy);
        let guest =
            run_op(&sandbox, &m, Op::Preview, flags::REVEAL_HIDDEN, &data, &RunOptions::new());
        match (native, guest) {
            (Ok(n), Ok(g)) => {
                assert_eq!(g.status, arca_abi::status::OK, "{path:?}");
                assert_eq!(decode_preview(g.kind, &g.payload).unwrap(), n, "{path:?}: preview");
            }
            (Err(_), Ok(g)) => {
                // Unsupported (6) or failed (7) is reported as a status plus an error envelope.
                assert!(g.status == 6 || g.status == 7, "{path:?}: status {}", g.status);
                assert!(decode_preview(g.kind, &g.payload).is_err());
            }
            (n, g) => panic!("{path:?}: native {:?} vs guest {:?}", n.map(|_| ()), g.map(|_| ())),
        }
        eprintln!(
            "{:<32} {:<10} {:<24} fuel={:>12} mem={:>6} KiB",
            path.file_name().unwrap().to_string_lossy(),
            format!("{:?}", report.format),
            format!("{:?}", report.verdict),
            out.stats.fuel_consumed,
            out.stats.peak_memory_bytes / 1024
        );
    }
}

#[test]
fn guest_preview_equals_native_preview() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    for doc in [docx(&[]), xlsx(300)] {
        for reveal in [flags::REVEAL_HIDDEN, 0] {
            let out = run_op(&sandbox, &m, Op::Preview, reveal, &doc, &RunOptions::new()).unwrap();
            let got = decode_preview(out.kind, &out.payload).unwrap();
            let policy = arca_preview::PreviewPolicy {
                reveal_hidden: reveal != 0,
                ..arca_preview::PreviewPolicy::default()
            };
            assert_eq!(got, arca_preview::preview(&doc, &policy).unwrap());
            assert!(!got.sections.is_empty());
        }
    }
}

#[test]
fn guest_sanitize_equals_native_sanitize() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    let doc = docx(&[(
        "word/_rels/document.xml.rels",
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="https://attacker.example/t.dotm" TargetMode="External"/></Relationships>"#,
    )]);
    let out =
        run_op(&sandbox, &m, Op::Sanitize, flags::SANITIZE_ALL, &doc, &RunOptions::new()).unwrap();
    let got = decode_sanitized(out.kind, &out.payload).unwrap();
    let native = arca_xray::sanitize::sanitize(
        &doc,
        &arca_xray::ScanPolicy::default(),
        &arca_xray::sanitize::SanitizeOptions::default(),
    )
    .unwrap();
    assert_eq!(got, native);
    assert_eq!(got.verification.verdict, Verdict::Clean);
    // Plain text cannot be rebuilt: a status, not a trap.
    let out = run_op(&sandbox, &m, Op::Sanitize, flags::SANITIZE_ALL, b"text", &RunOptions::new())
        .unwrap();
    assert_eq!(out.status, arca_abi::status::UNSUPPORTED_FORMAT);
    assert!(decode_sanitized(out.kind, &out.payload).is_err());
}

#[test]
fn bad_requests_are_statuses_not_traps() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    // Not a request at all.
    let out = sandbox.run(&m, &docx(&[])).unwrap();
    assert_eq!(out.status, arca_abi::status::BAD_REQUEST);
    assert_eq!(out.kind, arca_abi::envelope::PayloadKind::Error);
    // Flags the operation does not define.
    let out = run_op(&sandbox, &m, Op::Scan, 1, &docx(&[]), &RunOptions::new()).unwrap();
    assert_eq!(out.status, arca_abi::status::BAD_REQUEST);
}

#[test]
fn preview_of_a_large_spreadsheet_stays_inside_the_budget() {
    let Some(m) = guest() else {
        eprintln!("skipped: ARCA_GUEST_XRAY_WASM not set");
        return;
    };
    let sandbox = Sandbox::new(SandboxPolicy::default(), [m.pin()]);
    let doc = xlsx(1_200);
    let out =
        run_op(&sandbox, &m, Op::Preview, flags::REVEAL_HIDDEN, &doc, &RunOptions::new()).unwrap();
    let p = decode_preview(out.kind, &out.payload).unwrap();
    assert!(!p.stats.truncated);
    let per_byte = out.stats.fuel_consumed / u64::try_from(doc.len()).unwrap();
    assert!(per_byte < sandbox.policy().fuel.per_input_byte / 2, "{per_byte} fuel per input byte");
}
