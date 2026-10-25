//! Word 97–2003 binary (`WordDocument` stream) triage.
//!
//! The text of a `.doc` is not stored in one piece: the *piece table* in the `Table` stream maps
//! ranges of characters to places in the `WordDocument` stream, as 8-bit or UTF-16 runs. A field
//! (`0x13 instruction 0x14 result 0x15`) can therefore straddle two pieces, and only the
//! reconstructed text shows it. This module
//!
//! * reads the FIB (encryption, which table stream, where the piece table and the string table
//!   of associated files are);
//! * rebuilds the document text from the piece table and runs every field through the shared
//!   field tracker (nested fields, `QUOTE` obfuscation, computed keywords);
//! * reads the associated-file strings, whose second entry is the attached template — a path or
//!   a URL that Word loads when the document opens;
//! * and, whatever the above finds, searches the raw stream for field markers as well (8-bit and
//!   UTF-16LE), so a piece table that lies or is damaged hides nothing; a document whose text
//!   could not be rebuilt is reported as not fully inspected.

use arca_container::bytes::{u16_le, u32_le};
use arca_container::cfb::CompoundFile;
use arca_container::text::utf16le_lossy;

use crate::report::{MetaKey, RefKind, ThreatKind};
use crate::{Ctx, fields, ooxml};

const FIELD_BEGIN: u8 = 0x13;
const FIELD_SEPARATE: u8 = 0x14;
const FIELD_END: u8 = 0x15;

/// Largest document text rebuilt from the piece table, in characters.
const MAX_TEXT: usize = 32 << 20;
/// Offset of `cbRgFcLcb` in the FIB, and of the `fc`/`lcb` pairs that follow it.
const OFF_CB_FC_LCB: usize = 152;
const OFF_FC_LCB: usize = 154;
/// Indices of the pairs this module uses.
const PAIR_STTBF_ASSOC: usize = 32;
const PAIR_CLX: usize = 33;

/// Scans the `WordDocument` stream of `cf`.
pub(crate) fn scan(ctx: &mut Ctx<'_>, cf: &CompoundFile<'_>, stream: &[u8], loc: &str) {
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
    let table_name = if flags & 0x0200 != 0 { "1Table" } else { "0Table" };
    let table = cf.find(table_name).and_then(|i| cf.read_stream(i).ok());
    let rebuilt = table.as_deref().and_then(|t| rebuild_text(stream, t));
    match &rebuilt {
        Some(text) => {
            let mut tracker = fields::Tracker::default();
            for c in text.chars() {
                match u8::try_from(u32::from(c)) {
                    Ok(FIELD_BEGIN) => {
                        tracker.begin();
                    }
                    Ok(FIELD_SEPARATE) => tracker.separate(),
                    Ok(FIELD_END) => {
                        if let Some(f) = tracker.end() {
                            fields::analyze_finished(ctx, &f, loc);
                        }
                    }
                    _ => {
                        if tracker.in_instruction() {
                            let mut b = [0u8; 4];
                            tracker.push_text(c.encode_utf8(&mut b));
                        }
                    }
                }
            }
            for f in tracker.finish() {
                fields::analyze_finished(ctx, &f, loc);
            }
            if tracker.too_deep {
                ctx.report.add(
                    ThreatKind::ScanLimitReached,
                    loc,
                    "fields are nested deeper than the scanner follows; the inner ones were not inspected",
                );
            }
        }
        None => ctx.report.add(
            ThreatKind::ScanLimitReached,
            loc,
            "the text of the document could not be rebuilt from its piece table; only a search of the raw stream was possible",
        ),
    }
    if let Some(template) = table.as_deref().and_then(|t| attached_template(stream, t))
        && !template.is_empty()
    {
        ctx.report.meta(MetaKey::Template, "", &template, loc);
        ooxml::record_external(ctx, &template, RefKind::Template, true, loc);
    }
    // The raw search runs regardless: it finds what a piece table was made to hide.
    for instr in field_instructions(stream, 256) {
        fields::analyze(ctx, &instr, loc);
    }
}

/// `(fc, lcb)` of pair `index` of the FIB.
fn fib_pair(stream: &[u8], index: usize) -> Option<(usize, usize)> {
    let count = usize::from(u16_le(stream, OFF_CB_FC_LCB)?);
    if index >= count {
        return None;
    }
    let at = OFF_FC_LCB + index * 8;
    Some((
        usize::try_from(u32_le(stream, at)?).ok()?,
        usize::try_from(u32_le(stream, at + 4)?).ok()?,
    ))
}

/// Rebuilds the text of the document from the piece table (`Clx`).
fn rebuild_text(stream: &[u8], table: &[u8]) -> Option<String> {
    let (fc, lcb) = fib_pair(stream, PAIR_CLX)?;
    let clx = table.get(fc..fc.checked_add(lcb)?)?;
    // Skip the Prc entries (formatting exceptions); the Pcdt follows.
    let mut pos = 0usize;
    loop {
        match clx.get(pos)? {
            0x01 => pos += 3 + usize::from(u16_le(clx, pos + 1)?),
            0x02 => break,
            _ => return None,
        }
    }
    let plc_len = usize::try_from(u32_le(clx, pos + 1)?).ok()?;
    let plc = clx.get(pos + 5..pos + 5 + plc_len)?;
    // n + 1 character positions, then n 8-byte piece descriptors.
    let n = plc_len.checked_sub(4)? / 12;
    let mut text = String::new();
    let mut total = 0usize;
    for i in 0..n {
        let cp_start = usize::try_from(u32_le(plc, i * 4)?).ok()?;
        let cp_end = usize::try_from(u32_le(plc, (i + 1) * 4)?).ok()?;
        let chars = cp_end.checked_sub(cp_start)?;
        total = total.checked_add(chars)?;
        if total > MAX_TEXT {
            return None;
        }
        let fc_raw = u32_le(plc, (n + 1) * 4 + i * 8 + 2)?;
        let compressed = fc_raw & 0x4000_0000 != 0;
        let offset = usize::try_from(fc_raw & 0x3FFF_FFFF).ok()?;
        if compressed {
            // 8-bit characters; the stored offset is doubled.
            let start = offset / 2;
            let bytes = stream.get(start..start.checked_add(chars)?)?;
            text.extend(bytes.iter().map(|&b| char::from(b)));
        } else {
            let bytes = stream.get(offset..offset.checked_add(chars.checked_mul(2)?)?)?;
            text.push_str(&utf16le_lossy(bytes));
        }
    }
    Some(text)
}

/// The attached template: entry 1 of the associated-file string table (`SttbfAssoc`).
fn attached_template(stream: &[u8], table: &[u8]) -> Option<String> {
    let (fc, lcb) = fib_pair(stream, PAIR_STTBF_ASSOC)?;
    let sttb = table.get(fc..fc.checked_add(lcb)?)?;
    // Extended string table: 0xFFFF, entry count, extra bytes per entry.
    if u16_le(sttb, 0)? != 0xFFFF {
        return None;
    }
    let count = usize::from(u16_le(sttb, 2)?);
    let extra = usize::from(u16_le(sttb, 4)?);
    let mut pos = 6usize;
    for i in 0..count.min(8) {
        let cch = usize::from(u16_le(sttb, pos)?);
        let raw = sttb.get(pos + 2..pos + 2 + cch * 2)?;
        if i == 1 {
            return Some(utf16le_lossy(raw).trim_end_matches('\0').to_owned());
        }
        pos += 2 + cch * 2 + extra;
    }
    None
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
