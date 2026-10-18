//! OLE property sets ([MS-OLEPS]): `\x05SummaryInformation` and
//! `\x05DocumentSummaryInformation` of binary Office files.

use arca_container::bytes::{LeReader, u16_le, u32_le, u64_le};
use arca_container::text::{CodePage, decode_codepage, utf16le_until_nul};

use crate::Ctx;
use crate::report::{MetaKey, ThreatKind};

const FMTID_SUMMARY: [u8; 16] = [
    0xE0, 0x85, 0x9F, 0xF2, 0xF9, 0x4F, 0x68, 0x10, 0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9,
];
const FMTID_DOC_SUMMARY: [u8; 16] = [
    0x02, 0xD5, 0xCD, 0xD5, 0x9C, 0x2E, 0x1B, 0x10, 0x93, 0x97, 0x08, 0x00, 0x2B, 0x2C, 0xF9, 0xAE,
];
const FMTID_USER_DEFINED: [u8; 16] = [
    0x05, 0xD5, 0xCD, 0xD5, 0x9C, 0x2E, 0x1B, 0x10, 0x93, 0x97, 0x08, 0x00, 0x2B, 0x2C, 0xF9, 0xAE,
];

const VT_I2: u16 = 0x0002;
const VT_I4: u16 = 0x0003;
const VT_BOOL: u16 = 0x000B;
const VT_UI4: u16 = 0x0013;
const VT_LPSTR: u16 = 0x001E;
const VT_LPWSTR: u16 = 0x001F;
const VT_FILETIME: u16 = 0x0040;

/// A decoded property value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Int(i64),
    Text(String),
    FileTime(u64),
}

fn read_value(section: &[u8], offset: usize, cp: CodePage) -> Option<Value> {
    let vt = u16_le(section, offset)?;
    let body = offset.checked_add(4)?;
    match vt {
        VT_I2 => Some(Value::Int(i64::from(u16_le(section, body)?.cast_signed()))),
        VT_I4 => Some(Value::Int(i64::from(u32_le(section, body)?.cast_signed()))),
        VT_UI4 => Some(Value::Int(i64::from(u32_le(section, body)?))),
        VT_BOOL => Some(Value::Int(i64::from(u16_le(section, body)? != 0))),
        VT_LPSTR => {
            let len = usize::try_from(u32_le(section, body)?).ok()?;
            if len > 1 << 20 {
                return None;
            }
            let raw = arca_container::bytes::slice(section, body + 4, len)?;
            let raw = raw.split(|&b| b == 0).next().unwrap_or(raw);
            if cp == CodePage::Utf16Le {
                Some(Value::Text(utf16le_until_nul(arca_container::bytes::slice(
                    section,
                    body + 4,
                    len,
                )?)))
            } else {
                Some(Value::Text(decode_codepage(raw, cp)))
            }
        }
        VT_LPWSTR => {
            let chars = usize::try_from(u32_le(section, body)?).ok()?;
            if chars > 1 << 19 {
                return None;
            }
            let raw = arca_container::bytes::slice(section, body + 4, chars.checked_mul(2)?)?;
            Some(Value::Text(utf16le_until_nul(raw)))
        }
        VT_FILETIME => Some(Value::FileTime(u64_le(section, body)?)),
        _ => None,
    }
}

/// Converts a FILETIME (100 ns ticks since 1601-01-01) to ISO 8601.
pub(crate) fn filetime_to_iso(ft: u64) -> Option<String> {
    const TICKS_PER_SEC: u64 = 10_000_000;
    const EPOCH_DIFF: u64 = 11_644_473_600;
    if ft == 0 {
        return None;
    }
    let secs = (ft / TICKS_PER_SEC).checked_sub(EPOCH_DIFF)?;
    let days = i64::try_from(secs / 86_400).ok()?;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    ))
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// Parses one section and returns `(property id, value)` pairs plus the
/// user-defined dictionary (`id → name`) when present.
/// `(property id, value)` pairs and the user-defined dictionary (`id → name`).
type SectionData = (Vec<(u32, Value)>, Vec<(u32, String)>);

fn parse_section(data: &[u8], offset: usize) -> Option<SectionData> {
    let size = usize::try_from(u32_le(data, offset)?).ok()?;
    let section = arca_container::bytes::slice(data, offset, size)?;
    let count = usize::try_from(u32_le(section, 4)?).ok()?;
    if count > 4096 {
        return None;
    }
    let mut ids = Vec::with_capacity(count);
    let mut r = LeReader::at(section, 8);
    for _ in 0..count {
        let id = r.u32()?;
        let off = usize::try_from(r.u32()?).ok()?;
        ids.push((id, off));
    }
    // Code page first: it governs the decoding of every VT_LPSTR.
    let cp = ids
        .iter()
        .find(|(id, _)| *id == 1)
        .and_then(|(_, off)| read_value(section, *off, CodePage::Windows1252))
        .and_then(|v| if let Value::Int(c) = v { u16::try_from(c & 0xFFFF).ok() } else { None })
        .map_or(CodePage::Windows1252, CodePage::from_id);
    let mut values = Vec::new();
    let mut dictionary = Vec::new();
    for (id, off) in ids {
        if id == 0 {
            // Dictionary: count, then (id, length, name) entries.
            let mut d = LeReader::at(section, off);
            let n = d.u32().unwrap_or(0).min(1024);
            for _ in 0..n {
                let (Some(pid), Some(len)) = (d.u32(), d.u32()) else { break };
                let Ok(len) = usize::try_from(len) else { break };
                if cp == CodePage::Utf16Le {
                    let Some(raw) = len.checked_mul(2).and_then(|l| d.bytes(l)) else { break };
                    dictionary.push((pid, utf16le_until_nul(raw)));
                    // Entries are padded to 4 bytes in Unicode dictionaries.
                    let pad = (4 - (len * 2) % 4) % 4;
                    let _ = d.skip(pad);
                } else {
                    let Some(raw) = d.bytes(len) else { break };
                    let raw = raw.split(|&b| b == 0).next().unwrap_or(raw);
                    dictionary.push((pid, decode_codepage(raw, cp)));
                }
            }
            continue;
        }
        if let Some(v) = read_value(section, off, cp) {
            values.push((id, v));
        }
    }
    Some((values, dictionary))
}

/// Parses a property set stream and records metadata.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], location: &str) {
    let Some(sets) = parse_stream(data) else {
        ctx.report.add(ThreatKind::MalformedContainer, location, "unreadable property set stream");
        return;
    };
    let mut people = false;
    for (fmtid, values, dictionary) in sets {
        for (id, v) in values {
            let text = match &v {
                Value::Text(t) => t.clone(),
                Value::Int(i) => i.to_string(),
                Value::FileTime(ft) => filetime_to_iso(*ft).unwrap_or_default(),
            };
            let key = if fmtid == FMTID_SUMMARY {
                match id {
                    2 => Some(MetaKey::Title),
                    3 => Some(MetaKey::Subject),
                    4 => {
                        people = true;
                        Some(MetaKey::Author)
                    }
                    5 => Some(MetaKey::Keywords),
                    6 => Some(MetaKey::Description),
                    7 => Some(MetaKey::Template),
                    8 => {
                        people = true;
                        Some(MetaKey::LastModifiedBy)
                    }
                    9 => Some(MetaKey::Revision),
                    10 => {
                        // PIDSI_EDITTIME is a duration stored as FILETIME ticks.
                        if let Value::FileTime(t) = v {
                            ctx.report.meta(
                                MetaKey::TotalEditMinutes,
                                "",
                                &(t / 600_000_000).to_string(),
                                location,
                            );
                        }
                        None
                    }
                    11 => Some(MetaKey::LastPrinted),
                    12 => Some(MetaKey::Created),
                    13 => Some(MetaKey::Modified),
                    18 => Some(MetaKey::Application),
                    19 => Some(MetaKey::Security),
                    _ => None,
                }
            } else if fmtid == FMTID_DOC_SUMMARY {
                match id {
                    2 => Some(MetaKey::Category),
                    14 => Some(MetaKey::Manager),
                    15 => Some(MetaKey::Company),
                    23 => Some(MetaKey::AppVersion),
                    26 => Some(MetaKey::ContentStatus),
                    28 => Some(MetaKey::Language),
                    _ => None,
                }
            } else if fmtid == FMTID_USER_DEFINED && id > 1 {
                let name = dictionary
                    .iter()
                    .find(|(p, _)| *p == id)
                    .map_or_else(|| format!("#{id}"), |(_, n)| n.clone());
                ctx.report.meta(MetaKey::CustomProperty, &name, &text, location);
                None
            } else {
                None
            };
            if let Some(k) = key {
                if k == MetaKey::Template
                    && crate::url::classify(&text) == crate::report::TargetClass::LocalFile
                {
                    ctx.report.add(
                        ThreatKind::LocalPathLeak,
                        location,
                        format!("template path {text}"),
                    );
                }
                ctx.report.meta(k, "", &text, location);
            }
        }
    }
    if people {
        ctx.report.add(
            ThreatKind::AuthorMetadata,
            location,
            "author / last editor names are stored",
        );
    }
}

type Section = ([u8; 16], Vec<(u32, Value)>, Vec<(u32, String)>);

fn parse_stream(data: &[u8]) -> Option<Vec<Section>> {
    if u16_le(data, 0)? != 0xFFFE {
        return None;
    }
    let n = u32_le(data, 24)?;
    if n == 0 || n > 4 {
        return None;
    }
    let mut out = Vec::new();
    let mut r = LeReader::at(data, 28);
    for _ in 0..n {
        let fmtid_bytes = r.bytes(16)?;
        let mut fmtid = [0u8; 16];
        fmtid.copy_from_slice(fmtid_bytes);
        let offset = usize::try_from(r.u32()?).ok()?;
        let (values, dict) = parse_section(data, offset)?;
        out.push((fmtid, values, dict));
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a SummaryInformation stream with the given `(id, text)` LPSTR properties.
    pub(crate) fn summary_stream(props: &[(u32, &str)], edit_minutes: u64) -> Vec<u8> {
        let mut values: Vec<(u32, Vec<u8>)> = Vec::new();
        values.push((
            1,
            [VT_I2.to_le_bytes().as_slice(), &[0, 0], &1251u16.to_le_bytes(), &[0, 0]].concat(),
        ));
        for (id, text) in props {
            let cp1251: Vec<u8> = text
                .chars()
                .map(|c| match c {
                    'А'..='я' => u8::try_from(u32::from(c) - 0x0410 + 0xC0).unwrap(),
                    c => u8::try_from(u32::from(c)).unwrap(),
                })
                .collect();
            let mut v = VT_LPSTR.to_le_bytes().to_vec();
            v.extend_from_slice(&[0, 0]);
            v.extend_from_slice(&u32::try_from(cp1251.len() + 1).unwrap().to_le_bytes());
            v.extend_from_slice(&cp1251);
            v.push(0);
            while !v.len().is_multiple_of(4) {
                v.push(0);
            }
            values.push((*id, v));
        }
        let mut ft = VT_FILETIME.to_le_bytes().to_vec();
        ft.extend_from_slice(&[0, 0]);
        ft.extend_from_slice(&(edit_minutes * 600_000_000).to_le_bytes());
        values.push((10, ft));

        let header_len = 8 + values.len() * 8;
        let mut section = Vec::new();
        let mut body = Vec::new();
        let mut index = Vec::new();
        for (id, v) in &values {
            index.push((*id, u32::try_from(header_len + body.len()).unwrap()));
            body.extend_from_slice(v);
        }
        section.extend_from_slice(&u32::try_from(header_len + body.len()).unwrap().to_le_bytes());
        section.extend_from_slice(&u32::try_from(values.len()).unwrap().to_le_bytes());
        for (id, off) in index {
            section.extend_from_slice(&id.to_le_bytes());
            section.extend_from_slice(&off.to_le_bytes());
        }
        section.extend_from_slice(&body);

        let mut out = Vec::new();
        out.extend_from_slice(&0xFFFEu16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x0002_0006u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&FMTID_SUMMARY);
        out.extend_from_slice(&48u32.to_le_bytes());
        out.extend_from_slice(&section);
        out
    }

    #[test]
    fn filetime_conversion() {
        // 2024-02-29T12:34:56Z
        let unix: u64 = 1_709_210_096;
        let ft = (unix + 11_644_473_600) * 10_000_000;
        assert_eq!(filetime_to_iso(ft).as_deref(), Some("2024-02-29T12:34:56Z"));
        assert_eq!(filetime_to_iso(0), None);
    }

    #[test]
    fn summary_roundtrip() {
        let s = summary_stream(&[(4, "Иван Петров"), (8, "Admin")], 95);
        let sets = parse_stream(&s).unwrap();
        let (fmtid, values, _) = &sets[0];
        assert_eq!(*fmtid, FMTID_SUMMARY);
        assert!(values.contains(&(4, Value::Text("Иван Петров".into()))));
        assert!(values.contains(&(8, Value::Text("Admin".into()))));
        assert!(values.iter().any(|(id, v)| *id == 10 && *v == Value::FileTime(95 * 600_000_000)));
    }

    #[test]
    fn garbage_is_rejected_without_panic() {
        assert!(parse_stream(&[]).is_none());
        let s = summary_stream(&[(4, "x")], 1);
        for cut in 0..s.len() {
            let _ = parse_stream(&s[..cut]);
        }
    }
}
