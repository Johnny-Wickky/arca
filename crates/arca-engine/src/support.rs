//! The extra support addresses (crypto wallets, Boosty) as JSON for the "About & Support"
//! screen. The list comes from [`arca_attribution::support`] and ships empty: with no entry the
//! apps show nothing. Every entry has passed the validity check there.

use arca_attribution::support::{SupportKind, configured};

use crate::json::JsonWriter;

/// `{"items":[{"kind":"crypto"|"boosty","label":…,"network":…,"value":…,"memo":…}]}`.
#[must_use]
pub fn extra_support_json() -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.key("items");
    w.begin_array();
    for e in configured() {
        w.begin_object();
        w.field_str(
            "kind",
            match e.kind {
                SupportKind::Boosty => "boosty",
                SupportKind::Crypto => "crypto",
            },
        );
        w.field_str("label", e.label);
        w.field_str("network", e.network);
        w.field_str("value", e.value);
        w.field_str("memo", e.memo);
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_valid_json_and_empty_while_only_placeholders_are_set() {
        let json = String::from_utf8(extra_support_json()).expect("utf-8");
        if configured().is_empty() {
            assert_eq!(json, "{\"items\":[]}");
        } else {
            assert!(json.starts_with("{\"items\":[{\"kind\":"), "{json}");
        }
    }
}
