// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The property lists of the system catalog (`LvProp`): database options such as the startup
//! form, column expressions and so on.
//!
//! The layout, as the open-source Jackcess library documents it (`PropertyMaps`): the magic
//! `MR2\0` (Access 2000 and later) or `KKD\0` (Access 97), then blocks, each starting with its own
//! length (4 bytes, counting the 6 bytes of the header) and a type (2 bytes). A block of type
//! `0x80` is the list of property names, each a 2-byte length and the name. Any other block holds
//! the values of one map (type 0 for the object itself, 1 for a column, 2 for an index): the
//! first entry names the map (4-byte length counting itself, then a counted name when longer than
//! 6), the rest are values, each `[length u16][flags u8][data type u8][name index u16][data
//! size u16][data]`.
//!
//! Nothing here is interpreted beyond reading: a property never causes anything to happen.

use crate::bytes::{u16_le, u32_le};

/// Most properties kept from one list.
const MAX_PROPERTIES: usize = 20_000;
/// Most property names read from one list.
const MAX_NAMES: usize = 20_000;

/// The data type codes of a property value (the same as column types).
const T_BOOL: u8 = 0x01;
const T_BYTE: u8 = 0x02;
const T_INT: u8 = 0x03;
const T_LONG: u8 = 0x04;
const T_TEXT: u8 = 0x0A;
const T_MEMO: u8 = 0x0C;

/// The value of a property.
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    /// A Yes/No value.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// Text.
    Text(String),
    /// A value of another type, of this many bytes (not read).
    Other(usize),
}

/// One property of an object.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The map the property belongs to: empty for the object itself, a column or index name
    /// otherwise.
    pub scope: String,
    /// The kind of map: 0 for the object, 1 for a column, 2 for an index.
    pub map_kind: u16,
    /// The property name.
    pub name: String,
    /// The value.
    pub value: PropValue,
}

/// Reads a property list. `text` decodes a name or a text value to a string. A list that is
/// damaged yields the properties read before the damage.
pub(super) fn parse(data: &[u8], text: &dyn Fn(&[u8]) -> String) -> Vec<Property> {
    let mut out = Vec::new();
    if !(data.starts_with(b"MR2\0") || data.starts_with(b"KKD\0")) {
        return out;
    }
    let mut names: Vec<String> = Vec::new();
    let mut at = 4usize;
    while let (Some(len), Some(kind)) = (u32_le(data, at), u16_le(data, at + 4)) {
        let Ok(len) = usize::try_from(len) else { break };
        // Every block has at least its own header, so the loop always advances.
        if len < 6 {
            break;
        }
        let Some(end) = at.checked_add(len) else { break };
        let Some(block) = data.get(at + 6..end.min(data.len())) else { break };
        if kind == 0x80 {
            names = read_names(block, text);
        } else if read_values(block, kind, &names, text, &mut out) {
            break;
        }
        at = end;
    }
    out
}

/// A counted name at `at`, and the position after it.
fn counted(block: &[u8], at: usize, text: &dyn Fn(&[u8]) -> String) -> Option<(String, usize)> {
    let len = usize::from(u16_le(block, at)?);
    let start = at.checked_add(2)?;
    let end = start.checked_add(len)?;
    Some((text(block.get(start..end)?), end))
}

fn read_names(block: &[u8], text: &dyn Fn(&[u8]) -> String) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while out.len() < MAX_NAMES {
        let Some((name, next)) = counted(block, at, text) else { break };
        out.push(name);
        at = next;
    }
    out
}

/// Reads the values of one map into `out`; `true` when the limit on properties is reached.
fn read_values(
    block: &[u8],
    map_kind: u16,
    names: &[String],
    text: &dyn Fn(&[u8]) -> String,
    out: &mut Vec<Property>,
) -> bool {
    let mut scope = String::new();
    let mut at = 0usize;
    if let Some(name_block) = u32_le(block, 0) {
        let Ok(name_block) = usize::try_from(name_block) else { return false };
        if name_block > 6
            && let Some((name, _)) = counted(block, 4, text)
        {
            scope = name;
        }
        // The name entry counts its own length field.
        at = name_block.max(4);
    }
    while let Some(len) = block.get(at..).and_then(|b| u16_le(b, 0)) {
        let len = usize::from(len);
        // An entry has at least its 8-byte header, so the loop always advances.
        if len < 8 {
            break;
        }
        let Some(entry) = at.checked_add(len).and_then(|e| block.get(at..e)) else { break };
        at += len;
        let (Some(&dtype), Some(idx), Some(size)) =
            (entry.get(3), u16_le(entry, 4), u16_le(entry, 6))
        else {
            break;
        };
        let Some(data) = entry.get(8..8 + usize::from(size)) else { continue };
        let Some(name) = names.get(usize::from(idx)) else { continue };
        let value = match dtype {
            T_BOOL => PropValue::Bool(data.first().is_some_and(|b| *b != 0)),
            T_BYTE => data.first().map_or(PropValue::Other(data.len()), |b| PropValue::Int(i64::from(*b))),
            T_INT => data
                .get(..2)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map_or(PropValue::Other(data.len()), |b| {
                    PropValue::Int(i64::from(i16::from_le_bytes(b)))
                }),
            T_LONG => data
                .get(..4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(PropValue::Other(data.len()), |b| {
                    PropValue::Int(i64::from(i32::from_le_bytes(b)))
                }),
            T_TEXT | T_MEMO => PropValue::Text(text(data)),
            _ => PropValue::Other(data.len()),
        };
        out.push(Property { scope: scope.clone(), map_kind, name: name.clone(), value });
        if out.len() >= MAX_PROPERTIES {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn decode(b: &[u8]) -> String {
        crate::text::utf16le_lossy(b)
    }

    fn counted_name(s: &str) -> Vec<u8> {
        let n = utf16(s);
        let mut v = u16::try_from(n.len()).unwrap().to_le_bytes().to_vec();
        v.extend(n);
        v
    }

    fn block(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut v = u32::try_from(body.len() + 6).unwrap().to_le_bytes().to_vec();
        v.extend(kind.to_le_bytes());
        v.extend(body);
        v
    }

    fn entry(dtype: u8, idx: u16, data: &[u8]) -> Vec<u8> {
        let mut v = u16::try_from(8 + data.len()).unwrap().to_le_bytes().to_vec();
        v.push(0); // flags
        v.push(dtype);
        v.extend(idx.to_le_bytes());
        v.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
        v.extend(data);
        v
    }

    /// A list with a name block and one default map: a text, a Yes/No and a number.
    fn sample() -> Vec<u8> {
        let mut names = counted_name("StartUpForm");
        names.extend(counted_name("AllowBypassKey"));
        names.extend(counted_name("Version"));
        let mut values = 4u32.to_le_bytes().to_vec(); // an unnamed map
        values.extend(entry(T_TEXT, 0, &utf16("Switchboard")));
        values.extend(entry(T_BOOL, 1, &[0]));
        values.extend(entry(T_LONG, 2, &(-7i32).to_le_bytes()));
        let mut d = b"MR2\0".to_vec();
        d.extend(block(0x80, &names));
        d.extend(block(0, &values));
        d
    }

    #[test]
    fn a_property_list_reads_back() {
        let props = parse(&sample(), &decode);
        let got: Vec<(&str, &PropValue)> = props.iter().map(|p| (p.name.as_str(), &p.value)).collect();
        assert_eq!(
            got,
            [
                ("StartUpForm", &PropValue::Text("Switchboard".into())),
                ("AllowBypassKey", &PropValue::Bool(false)),
                ("Version", &PropValue::Int(-7)),
            ]
        );
        assert!(props.iter().all(|p| p.scope.is_empty() && p.map_kind == 0));
    }

    #[test]
    fn a_named_map_is_a_column_scope() {
        let mut names = counted_name("Description");
        names.extend(counted_name("Id"));
        let mut values = Vec::new();
        let scope = counted_name("Id");
        values.extend(u32::try_from(4 + scope.len()).unwrap().to_le_bytes());
        values.extend(scope);
        values.extend(entry(T_MEMO, 0, &utf16("the key")));
        let mut d = b"MR2\0".to_vec();
        d.extend(block(0x80, &names));
        d.extend(block(1, &values));
        let props = parse(&d, &decode);
        assert_eq!(props.len(), 1);
        assert_eq!((props[0].scope.as_str(), props[0].map_kind), ("Id", 1));
        assert_eq!(props[0].value, PropValue::Text("the key".into()));
    }

    #[test]
    fn damage_loses_the_rest_but_never_panics() {
        let good = sample();
        assert!(parse(b"nope", &decode).is_empty());
        for cut in 0..good.len() {
            let _ = parse(&good[..cut], &decode);
        }
        // Every single byte flipped: still returns.
        for i in 0..good.len() {
            let mut d = good.clone();
            d[i] ^= 0xFF;
            let _ = parse(&d, &decode);
        }
        // A block that claims to be shorter than its header ends the list.
        let mut d = b"MR2\0".to_vec();
        d.extend([1, 0, 0, 0, 0, 0]);
        assert!(parse(&d, &decode).is_empty());
    }
}
