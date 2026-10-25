// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Complex columns of Access 2007 and later: attachments, multi-value fields and the version
//! history of an "append only" memo.
//!
//! The row of the table holds only a key. The values live in a hidden "flat" table, whose rows
//! carry that key in a column marked as the complex key. `MSysComplexColumns` ties a column (by
//! the identifier in its definition) to the flat table and to a "type" table that names the
//! kind: `MSysComplexType_Attachment`, `MSysComplexType_<value type>` for a multi-value field and
//! `MSysComplexTypeVH_<column>` for a version history. This is the structure the open-source
//! Jackcess library documents (`ComplexColumnSupport`); here it is only read.
//!
//! Attachments are never extracted: the name and the stored size are all that is reported.

use super::{Column, Db, Object, Value, ty};
use std::collections::BTreeMap;

/// Most values collected for one complex column.
const MAX_COMPLEX_ITEMS: usize = 100_000;

/// What a complex column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexKind {
    /// Files attached to the row.
    Attachment,
    /// Several values of one simple type.
    MultiValue,
    /// The earlier texts of an "append only" memo.
    VersionHistory,
    /// A structure this reader does not recognise.
    Unsupported,
}

/// One value of a complex column.
#[derive(Debug, Clone, PartialEq)]
pub enum ComplexItem {
    /// An attached file: its name and the size of the stored (possibly compressed) bytes.
    Attachment {
        /// The file name.
        name: String,
        /// The stored size in bytes; the bytes are not read.
        size: usize,
    },
    /// One value of a multi-value field.
    Value(Value),
    /// One earlier version of an append-only memo (its text is not read).
    Version,
}

/// The values of one complex column, by the key the rows of the table hold.
#[derive(Debug)]
pub struct ComplexData {
    /// What the column holds.
    pub kind: ComplexKind,
    /// The values by key.
    pub items: BTreeMap<i64, Vec<ComplexItem>>,
    /// Whether reading stopped before the end (a limit was reached or the file is damaged).
    pub stopped: bool,
}

/// Value types a multi-value field may have (Jackcess `MULTI_VALUE_TYPES`).
fn multi_value_type(t: u8) -> bool {
    matches!(
        t,
        ty::BYTE | ty::INT | ty::LONG | ty::FLOAT | ty::DOUBLE | ty::GUID | ty::NUMERIC | ty::TEXT
            | ty::BIGINT
    )
}

/// The page of the object whose catalog identifier is `id`.
fn page_of(id: i64) -> Option<u32> {
    u32::try_from(id & 0x00FF_FFFF).ok()
}

impl Db<'_> {
    /// Reads the values of the complex column `column` of the table whose definition is at
    /// `table_page`. `objects` is the catalog ([`Db::catalog`]).
    ///
    /// `None` when `MSysComplexColumns` has no row for the column or the supporting tables are
    /// missing or inconsistent: the caller then shows the stored key only.
    pub fn complex_data(
        &self,
        objects: &[Object],
        table_page: u32,
        column: &Column,
    ) -> Option<ComplexData> {
        let reg = objects
            .iter()
            .find(|o| o.kind == 1 && o.name.eq_ignore_ascii_case("MSysComplexColumns"))?;
        let reg = self.table(reg.page)?;
        let col = |n: &str| reg.columns.iter().position(|c| c.name.eq_ignore_ascii_case(n));
        let (c_id, c_type, c_concept, c_flat) = (
            col("ComplexID")?,
            col("ComplexTypeObjectID")?,
            col("ConceptualTableID")?,
            col("FlatTableID")?,
        );
        let c_name = col("ColumnName");
        let int = |v: &[Value], i: usize| match v.get(i) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        };
        let wanted = i64::from(column.complex_id());
        let mut found: Option<(i64, i64, i64)> = None;
        self.rows(&reg, |v| {
            if int(v, c_id) == Some(wanted) {
                let named_ok = c_name.and_then(|i| v.get(i)).is_none_or(|n| match n {
                    Value::Text(t) => t.eq_ignore_ascii_case(&column.name),
                    _ => true,
                });
                if let (Some(t), Some(c), Some(f)) = (int(v, c_type), int(v, c_concept), int(v, c_flat))
                    && named_ok
                {
                    found = Some((t, c, f));
                }
                return false;
            }
            true
        });
        let (type_id, concept_id, flat_id) = found?;
        // The row must describe this very table.
        if page_of(concept_id)? != table_page {
            return None;
        }
        let type_page = page_of(type_id)?;
        let type_name = &objects.iter().find(|o| o.kind == 1 && o.page == type_page)?.name;
        let type_table = self.table(type_page)?;
        let flat = self.table(page_of(flat_id)?)?;

        let only_multi = type_table.columns.len() == 1
            && type_table.columns.first().is_some_and(|c| multi_value_type(c.ty));
        let has = |n: &str| type_table.columns.iter().any(|c| c.name.eq_ignore_ascii_case(n));
        let lower = type_name.to_ascii_lowercase();
        let kind = if lower == "msyscomplextype_attachment" {
            ComplexKind::Attachment
        } else if lower.starts_with("msyscomplextypevh_") {
            ComplexKind::VersionHistory
        } else if lower.starts_with("msyscomplextype_") {
            if only_multi { ComplexKind::MultiValue } else { ComplexKind::Unsupported }
        } else if only_multi {
            ComplexKind::MultiValue
        } else if has("FileName") && has("FileData") {
            ComplexKind::Attachment
        } else {
            ComplexKind::Unsupported
        };

        // The key column of the flat table: the one marked as such, else the first plain whole
        // number that does not belong to the type.
        let key = flat
            .columns
            .iter()
            .position(Column::is_complex_key)
            .or_else(|| {
                flat.columns.iter().position(|c| {
                    c.ty == ty::LONG && !c.is_auto_number() && !has(&c.name)
                })
            })?;
        let by_name = |n: &str| flat.columns.iter().position(|c| c.name.eq_ignore_ascii_case(n));
        let value_col = type_table.columns.first().and_then(|c| by_name(&c.name));
        let (name_col, data_col) = (by_name("FileName"), by_name("FileData"));

        let mut items: BTreeMap<i64, Vec<ComplexItem>> = BTreeMap::new();
        let mut total = 0usize;
        let mut capped = false;
        let stats = self.rows(&flat, |v| {
            let Some(Value::Int(k)) = v.get(key) else { return true };
            let item = match kind {
                ComplexKind::Attachment => ComplexItem::Attachment {
                    name: match name_col.and_then(|i| v.get(i)) {
                        Some(Value::Text(t)) => t.clone(),
                        _ => String::new(),
                    },
                    size: match data_col.and_then(|i| v.get(i)) {
                        Some(Value::Ole(n) | Value::Binary(n)) => *n,
                        _ => 0,
                    },
                },
                ComplexKind::MultiValue => ComplexItem::Value(
                    value_col.and_then(|i| v.get(i)).cloned().unwrap_or(Value::Null),
                ),
                ComplexKind::VersionHistory => ComplexItem::Version,
                ComplexKind::Unsupported => return false,
            };
            total += 1;
            if total > MAX_COMPLEX_ITEMS {
                capped = true;
                return false;
            }
            items.entry(*k).or_default().push(item);
            true
        });
        let stopped = (stats.stopped && kind != ComplexKind::Unsupported) || capped;
        Some(ComplexData { kind, items, stopped })
    }
}
