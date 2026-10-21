//! Helpers for building small documents in tests.

use arca_container::zip::writer::{Compression, ZipWriter};

use crate::model::{Block, Paragraph, Preview, Section};
use crate::{PreviewError, PreviewPolicy, preview};

/// Builds a ZIP archive from `(name, content)` pairs.
pub(crate) fn zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = ZipWriter::new();
    for (name, data) in parts {
        assert!(w.add(name, data, Compression::Deflated).is_ok(), "add {name}");
    }
    w.finish().unwrap_or_default()
}

/// Builds an OPC package: content types + package rels + the given parts.
///
/// `main` is `(part name, content type)`.
pub(crate) fn opc(main: (&str, &str), parts: &[(&str, &str)]) -> Vec<u8> {
    let (main_name, main_type) = main;
    let ct = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/{main_name}" ContentType="{main_type}"/></Types>"#
    );
    let rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="{main_name}"/></Relationships>"#
    );
    let mut all: Vec<(&str, &[u8])> =
        vec![("[Content_Types].xml", ct.as_bytes()), ("_rels/.rels", rels.as_bytes())];
    all.extend(parts.iter().map(|(n, c)| (*n, c.as_bytes())));
    zip(&all)
}

/// Relationships part for `source_dir/source_file`.
pub(crate) fn rels(rels: &[(&str, &str, &str)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (id, ty, target) in rels {
        let external = if target.starts_with("http") { r#" TargetMode="External""# } else { "" };
        let target = target.replace('&', "&amp;");
        s.push_str(&format!(
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{ty}" Target="{target}"{external}/>"#
        ));
    }
    s.push_str("</Relationships>");
    s
}

pub(crate) const W_NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office""#;

pub(crate) const DOCX_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

/// A `.docx` with the given `<w:body>` content and extra parts.
pub(crate) fn docx_with(body: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document {W_NS}><w:body>{body}</w:body></w:document>"#
    );
    let mut parts: Vec<(&str, &str)> = vec![("word/document.xml", doc.as_str())];
    parts.extend_from_slice(extra);
    opc(("word/document.xml", DOCX_CT), &parts)
}

/// A `.docx` with only a body.
pub(crate) fn docx(body: &str) -> Vec<u8> {
    docx_with(body, &[])
}

/// Previews with the default policy.
pub(crate) fn run(data: &[u8]) -> Result<Preview, PreviewError> {
    preview(data, &PreviewPolicy::default())
}

/// Previews and unwraps.
pub(crate) fn ok(data: &[u8]) -> Preview {
    match run(data) {
        Ok(p) => p,
        Err(e) => panic!("preview failed: {e}"),
    }
}

/// All paragraphs of a section, flattening tables.
pub(crate) fn paragraphs(section: &Section) -> Vec<&Paragraph> {
    let mut out = Vec::new();
    for b in &section.blocks {
        match b {
            Block::Para(p) => out.push(p),
            Block::Table(t) => {
                for r in &t.rows {
                    for c in &r.cells {
                        out.extend(c.paras.iter());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Text of all paragraphs in the section, one per line.
pub(crate) fn text_of(section: &Section) -> String {
    paragraphs(section).iter().map(|p| p.text()).collect::<Vec<_>>().join("\n")
}

/// The first section of a given kind.
pub(crate) fn section_of(p: &Preview, kind: crate::model::SectionKind) -> Option<&Section> {
    p.sections.iter().find(|s| s.kind == kind)
}

/// A small deterministic generator for mutation tests.
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    /// A number in `0..n` (`0` if `n` is `0`).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX).max(1);
        usize::try_from(self.next() % n).unwrap_or(0)
    }

    pub(crate) fn byte(&mut self) -> u8 {
        u8::try_from(self.next() & 0xFF).unwrap_or(0)
    }

    /// Overwrites `count` random bytes of `data`.
    pub(crate) fn mutate(&mut self, data: &mut [u8], count: usize) {
        for _ in 0..count {
            let pos = self.below(data.len());
            let b = self.byte();
            if let Some(slot) = data.get_mut(pos) {
                *slot = b;
            }
        }
    }
}

/// Number of mutation rounds: `ARCA_FUZZ_ITERS` or `default`.
pub(crate) fn fuzz_iterations(default: usize) -> usize {
    std::env::var("ARCA_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
