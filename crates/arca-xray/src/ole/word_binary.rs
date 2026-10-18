//! Word 97–2003 binary (`WordDocument` stream) triage.
//!
//! Full piece-table text reconstruction belongs to the Phase 3 parser. For
//! the X-ray two things are checked directly on the stream:
//!
//! * FIB flags — encryption / XOR obfuscation / template;
//! * field instructions — Word stores fields in the text as
//!   `0x13 instruction 0x14 result 0x15`. The stream is searched for the
//!   field-begin character followed by an instruction in both the 8-bit and
//!   UTF-16LE encodings, and every instruction found goes through the same
//!   analysis as OOXML fields (DDE, INCLUDEPICTURE, LINK, ...). This is a
//!   heuristic: text split across non-contiguous pieces may be missed.

use arca_container::bytes::u16_le;

use crate::report::ThreatKind;
use crate::{Ctx, fields};

const FIELD_BEGIN: u8 = 0x13;
const FIELD_SEPARATE: u8 = 0x14;
const FIELD_END: u8 = 0x15;

/// Scans the `WordDocument` stream.
pub(crate) fn scan(ctx: &mut Ctx<'_>, stream: &[u8], loc: &str) {
    match u16_le(stream, 0) {
        Some(0xA5EC | 0xA5DC) => {}
        _ => {
            ctx.report.add(
                ThreatKind::MalformedContainer,
                loc,
                "WordDocument stream lacks the FIB signature",
            );
            return;
        }
    }
    let flags = u16_le(stream, 0x0A).unwrap_or(0);
    if flags & 0x0100 != 0 {
        let how =
            if flags & 0x8000 != 0 { "XOR obfuscation" } else { "RC4 / CryptoAPI encryption" };
        ctx.report.add(
            ThreatKind::EncryptedDocument,
            loc,
            format!("document is protected with {how}; text was not inspected"),
        );
        return;
    }
    for instr in field_instructions(stream, 256) {
        fields::analyze(ctx, &instr, loc);
    }
}

/// Extracts candidate field instructions from raw stream bytes.
pub(crate) fn field_instructions(stream: &[u8], max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < stream.len() && out.len() < max {
        if stream.get(i) != Some(&FIELD_BEGIN) {
            i += 1;
            continue;
        }
        // UTF-16LE: 0x13 0x00 followed by ASCII-ish units.
        if stream.get(i + 1) == Some(&0) {
            let mut s = String::new();
            let mut j = i + 2;
            while s.len() < 1024 {
                let Some(u) = u16_le(stream, j) else { break };
                if u == u16::from(FIELD_SEPARATE)
                    || u == u16::from(FIELD_END)
                    || u == u16::from(FIELD_BEGIN)
                    || u == 0
                {
                    break;
                }
                match char::from_u32(u32::from(u)) {
                    Some(c) => s.push(c),
                    None => break,
                }
                j += 2;
            }
            if plausible(&s) {
                out.push(s);
            }
        }
        // 8-bit text.
        let mut s = String::new();
        let mut j = i + 1;
        while let Some(&b) = stream.get(j) {
            if b == FIELD_SEPARATE
                || b == FIELD_END
                || b == FIELD_BEGIN
                || b == 0
                || s.len() >= 1024
            {
                break;
            }
            s.push(char::from(b));
            j += 1;
        }
        if plausible(&s) {
            out.push(s);
        }
        i += 1;
    }
    out
}

/// Keeps only strings that start with a field keyword we analyse.
fn plausible(s: &str) -> bool {
    let first = s.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    matches!(
        first.as_str(),
        "DDE"
            | "DDEAUTO"
            | "INCLUDEPICTURE"
            | "INCLUDETEXT"
            | "IMPORT"
            | "LINK"
            | "DATABASE"
            | "MACROBUTTON"
            | "HYPERLINK"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fields_in_both_encodings() {
        let mut s = vec![0xEC, 0xA5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        s.extend_from_slice(b"text \x13 DDEAUTO cmd \"/c calc\" \x14result\x15 more");
        s.push(0x13);
        s.push(0x00);
        for u in " INCLUDEPICTURE \"http://t.example/x.png\" \\d".encode_utf16() {
            s.extend_from_slice(&u.to_le_bytes());
        }
        s.extend_from_slice(&[0x15, 0x00]);
        let found = field_instructions(&s, 10);
        assert!(found.iter().any(|f| f.contains("DDEAUTO cmd")), "{found:?}");
        assert!(
            found.iter().any(|f| f.contains("INCLUDEPICTURE \"http://t.example/x.png\"")),
            "{found:?}"
        );
    }
}
