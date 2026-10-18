//! PowerPoint 97–2003 (`PowerPoint Document` stream) triage.
//!
//! Walks the record tree ([MS-PPT] RecordHeader: `recVer:4 recInstance:12
//! recType:16 recLen:32`) with a depth limit and inspects:
//!
//! * `ExOleObjStg` (0x1011) — embedded OLE storages, including the VBA
//!   project; zlib-compressed when `recInstance == 1`. Each is decompressed
//!   against the shared budget and scanned as a nested compound file.
//! * `InteractiveInfoAtom` (0x0FF3) — "run program" / "run macro" actions.
//! * `CString` (0x0FBA) inside `ExHyperlink` (0x0FD7) — hyperlink targets.
//! * `VBAInfoAtom` (0x0400) — `fHasMacros` flag of the macro project
//!   (some writers emit an empty `VBAInfo` container, so only the flag counts).

use arca_container::bytes::{u16_le, u32_le};

use crate::report::{RefKind, Severity, ThreatKind};
use crate::{Abort, Ctx, ooxml};

const RT_EX_OLE_OBJ_STG: u16 = 0x1011;
const RT_INTERACTIVE_INFO_ATOM: u16 = 0x0FF3;
const RT_CSTRING: u16 = 0x0FBA;
const RT_EX_HYPERLINK: u16 = 0x0FD7;
const RT_VBA_INFO_ATOM: u16 = 0x0400;

/// Scans the `PowerPoint Document` stream.
pub(crate) fn scan(ctx: &mut Ctx<'_>, stream: &[u8], loc: &str) -> Result<(), Abort> {
    let mut objects = 0usize;
    walk(ctx, stream, loc, 0, None, &mut objects)
}

fn walk(
    ctx: &mut Ctx<'_>,
    data: &[u8],
    loc: &str,
    depth: usize,
    parent: Option<u16>,
    objects: &mut usize,
) -> Result<(), Abort> {
    if depth > 16 {
        ctx.report.add(ThreatKind::MalformedContainer, loc, "PowerPoint record nesting too deep");
        return Ok(());
    }
    let mut pos = 0usize;
    let mut count = 0u32;
    while pos + 8 <= data.len() {
        count += 1;
        if count > 1_000_000 {
            ctx.report.add(ThreatKind::ScanLimitReached, loc, "PowerPoint record limit reached");
            break;
        }
        let (Some(ver_inst), Some(rtype), Some(len)) =
            (u16_le(data, pos), u16_le(data, pos + 2), u32_le(data, pos + 4))
        else {
            break;
        };
        let Ok(len) = usize::try_from(len) else { break };
        let body_start = pos + 8;
        let Some(body) = body_start.checked_add(len).and_then(|end| data.get(body_start..end))
        else {
            ctx.report.add(
                ThreatKind::MalformedContainer,
                loc,
                format!("record 0x{rtype:04X} overruns its container"),
            );
            break;
        };
        let rec_ver = ver_inst & 0x000F;
        let instance = ver_inst >> 4;
        if rec_ver == 0x000F {
            walk(ctx, body, loc, depth + 1, Some(rtype), objects)?;
        } else {
            match rtype {
                RT_EX_OLE_OBJ_STG => {
                    *objects += 1;
                    let obj_loc = format!("{loc}#ExOleObjStg{}", *objects);
                    let storage = if instance == 1 {
                        let declared = u32_le(body, 0).map_or(0, u64::from);
                        let packed = body.get(4..).unwrap_or(&[]);
                        if declared > ctx.policy.cfb.max_stream_size {
                            return Err(Abort::new(
                                ThreatKind::CompressionBomb,
                                &obj_loc,
                                format!("embedded storage declares {declared} bytes"),
                            ));
                        }
                        if let Err(e) = ctx.budget.charge(declared, packed.len() as u64) {
                            return Err(Abort::new(
                                ThreatKind::CompressionBomb,
                                &obj_loc,
                                e.to_string(),
                            ));
                        }
                        let limit = usize::try_from(declared).unwrap_or(0);
                        match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(packed, limit)
                        {
                            Ok(v) => v,
                            Err(_) => {
                                ctx.report.add(
                                    ThreatKind::MalformedContainer,
                                    &obj_loc,
                                    "embedded storage does not inflate to its declared size",
                                );
                                continue_after(&mut pos, body_start, len);
                                continue;
                            }
                        }
                    } else {
                        body.to_vec()
                    };
                    ctx.scan_nested(&storage, &obj_loc)?;
                }
                RT_INTERACTIVE_INFO_ATOM => match body.get(8).copied() {
                    Some(1) => ctx.report.add(
                        ThreatKind::ProgramAction,
                        loc,
                        "action setting runs a macro",
                    ),
                    Some(2) => ctx.report.add_with(
                        ThreatKind::ProgramAction,
                        Severity::Critical,
                        loc,
                        "action setting runs an external program",
                    ),
                    Some(5) => ctx.report.add(
                        ThreatKind::ProgramAction,
                        loc,
                        "action setting activates an OLE object",
                    ),
                    _ => {}
                },
                RT_VBA_INFO_ATOM if u32_le(body, 4) == Some(1) => {
                    ctx.report.add(
                        ThreatKind::VbaMacros,
                        loc,
                        "presentation declares a VBA project (VBAInfoAtom.fHasMacros)",
                    );
                }
                RT_CSTRING if parent == Some(RT_EX_HYPERLINK) && instance == 1 => {
                    let target = arca_container::text::utf16le_lossy(body);
                    if !target.is_empty() {
                        ooxml::record_external(ctx, &target, RefKind::Hyperlink, false, loc);
                    }
                }
                _ => {}
            }
        }
        continue_after(&mut pos, body_start, len);
    }
    Ok(())
}

fn continue_after(pos: &mut usize, body_start: usize, len: usize) {
    *pos = body_start.saturating_add(len);
}
