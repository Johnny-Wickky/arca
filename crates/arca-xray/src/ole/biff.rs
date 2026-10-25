//! Excel 97–2003 (BIFF8) workbook-globals triage: sheet visibility and type,
//! Excel 4.0 macro sheets, `Auto_Open`-style names, encryption, external
//! workbook references.

use arca_container::bytes::{LeReader, u16_le};
use arca_container::text::utf16le_lossy;

use crate::Ctx;
use crate::report::{RefKind, Severity, ThreatKind};

const BOF: u16 = 0x0809;
const BOUNDSHEET: u16 = 0x0085;
const NAME: u16 = 0x0018;
const FILEPASS: u16 = 0x002F;
const OBPROJ: u16 = 0x00D3;
const SUPBOOK: u16 = 0x01AE;

/// Reads an `XLUnicodeStringNoCch` whose flag byte is at `at`.
fn short_string(body: &[u8], at: usize, cch: usize) -> String {
    let high = body.get(at).is_some_and(|f| f & 1 != 0);
    let start = at + 1;
    if high {
        utf16le_lossy(body.get(start..start.saturating_add(cch * 2)).unwrap_or(&[]))
    } else {
        // "Compressed" BIFF8 strings hold the low byte of each UTF-16 unit,
        // i.e. Latin-1 code points.
        body.get(start..start.saturating_add(cch))
            .unwrap_or(&[])
            .iter()
            .map(|&b| char::from(b))
            .collect()
    }
}

/// Scans a `Workbook` (BIFF8) or `Book` (BIFF5) stream.
pub(crate) fn scan(ctx: &mut Ctx<'_>, stream: &[u8], loc: &str) {
    let mut r = LeReader::new(stream);
    let mut records = 0u32;
    let mut macro_sheet_substreams = 0u32;
    let mut first_bof = true;
    while let (Some(rtype), Some(len)) = (r.u16(), r.u16()) {
        records += 1;
        if records > 2_000_000 {
            ctx.report.add(ThreatKind::ScanLimitReached, loc, "BIFF record limit reached");
            break;
        }
        let Some(body) = r.bytes(usize::from(len)) else {
            ctx.report.add(ThreatKind::MalformedContainer, loc, "truncated BIFF record");
            break;
        };
        match rtype {
            BOF => {
                let dt = u16_le(body, 2).unwrap_or(0);
                if !first_bof && dt == 0x0040 {
                    macro_sheet_substreams += 1;
                }
                first_bof = false;
            }
            FILEPASS => {
                let kind = match u16_le(body, 0) {
                    Some(0) => "XOR obfuscation",
                    Some(1) => "RC4 encryption",
                    _ => "unknown encryption",
                };
                ctx.report.add(ThreatKind::EncryptedDocument, loc, format!("workbook is protected with {kind}; contents after the globals were not inspected"));
                break;
            }
            OBPROJ => ctx.report.add(
                ThreatKind::VbaMacros,
                loc,
                "workbook declares a VBA project (OBPROJ record)",
            ),
            BOUNDSHEET => {
                let state = body.get(4).copied().unwrap_or(0) & 0x03;
                let dt = body.get(5).copied().unwrap_or(0);
                let cch = usize::from(body.get(6).copied().unwrap_or(0));
                let name = short_string(body, 7, cch);
                if dt == 0x01 {
                    ctx.report.add(
                        ThreatKind::XlmMacroSheet,
                        loc,
                        format!("Excel 4.0 macro sheet {name:?}"),
                    );
                }
                if dt == 0x06 {
                    ctx.report.add(
                        ThreatKind::VbaMacros,
                        loc,
                        format!("VBA module sheet {name:?}"),
                    );
                }
                match state {
                    1 => ctx.report.add(
                        ThreatKind::HiddenSheet,
                        loc,
                        format!("sheet {name:?} is hidden"),
                    ),
                    2 => ctx.report.add(
                        ThreatKind::VeryHiddenSheet,
                        loc,
                        format!("sheet {name:?} is 'very hidden'"),
                    ),
                    _ => {}
                }
            }
            NAME => {
                let flags = u16_le(body, 0).unwrap_or(0);
                let cch = usize::from(body.get(3).copied().unwrap_or(0));
                let builtin = flags & 0x0020 != 0;
                let name_at = 14usize;
                if builtin {
                    let code = body.get(name_at + 1).copied().unwrap_or(0xFF);
                    let label = match code {
                        0x01 => Some("Auto_Open"),
                        0x02 => Some("Auto_Close"),
                        0x0A => Some("Auto_Activate"),
                        0x0B => Some("Auto_Deactivate"),
                        _ => None,
                    };
                    if let Some(label) = label {
                        ctx.report.add(
                            ThreatKind::XlmAutoOpenName,
                            loc,
                            format!("built-in name {label} (runs a macro on open)"),
                        );
                    }
                } else {
                    let name = short_string(body, name_at, cch);
                    // Functions newer than BIFF8 and add-in functions are stored as names
                    // (`_xlfn.WEBSERVICE`); a name that is defined is a function that is used.
                    let lower = name.to_ascii_lowercase();
                    if let Some(f) =
                        lower.strip_prefix("_xlfn.").or_else(|| lower.strip_prefix("_xll."))
                        && let Some((_, label)) = crate::formulas::DANGEROUS
                            .iter()
                            .find(|(n, _)| n.eq_ignore_ascii_case(f))
                    {
                        ctx.report.add(
                            ThreatKind::DangerousFormula,
                            loc,
                            format!("{label} is used by a formula of the workbook"),
                        );
                    }
                    if name.to_ascii_lowercase().starts_with("auto_open")
                        || name.to_ascii_lowercase().starts_with("auto_close")
                    {
                        ctx.report.add(
                            ThreatKind::XlmAutoOpenName,
                            loc,
                            format!("defined name {name:?} (runs a macro on open)"),
                        );
                    }
                }
            }
            SUPBOOK => {
                let cch = u16_le(body, 2).unwrap_or(0);
                // 0x0401: self-reference, 0x3A01: add-in functions.
                if cch != 0x0401 && cch != 0x3A01 && cch > 0 {
                    let raw = short_string(body, 4, usize::from(cch));
                    // Encoded paths use control characters as separators.
                    let path: String =
                        raw.chars().map(|c| if c.is_control() { '\\' } else { c }).collect();
                    let path = path.trim_start_matches('\\').to_owned();
                    if !path.is_empty() {
                        crate::ooxml::record_external(ctx, &path, RefKind::WorkbookLink, true, loc);
                        // In an encoded file URL (it starts with U+0001) U+0003 is the directory
                        // separator; a DDE or OLE link is a plain "service U+0003 topic".
                        if !raw.starts_with('\u{1}') && raw.contains('\u{3}') {
                            ctx.report.add_with(
                                ThreatKind::DdeFormula,
                                Severity::High,
                                loc,
                                format!("DDE/OLE supporting link {path}"),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if macro_sheet_substreams > 0 {
        ctx.report.add(
            ThreatKind::XlmMacroSheet,
            loc,
            format!("{macro_sheet_substreams} macro sheet substream(s)"),
        );
    }
}
