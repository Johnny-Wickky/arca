//! Extra support addresses (issue 51): crypto wallets and a Boosty page, next to the Lava.top
//! link of [`crate::upstream`].
//!
//! # Why this is a separate file
//!
//! `upstream.rs` is covered by `UPSTREAM_FINGERPRINT` and must not be edited by anyone but the
//! original maintainers, and nobody may invent a payment address. This file holds the
//! **mechanism**: the list [`EXTRA_SUPPORT_ADDRESSES`] is **optional** and ships empty, the About screen shows
//! the "Other ways to support" card only when the list has valid entries, and
//! every entry goes through [`is_valid`] first (an entry that fails is dropped, never shown).
//!
//! # What an entry holds
//!
//! For each wallet: the network or coin (`network`, for example `BTC`, `ETH`, `USDT-TRC20`,
//! `TON`), a display `label`, the full `value` (the address, exactly as the wallet shows it),
//! and the `memo` / tag when the network needs one (otherwise `""`). For Boosty: the full
//! `https://boosty.to/<name>` address. Entries are added to the list below. To have them
//! covered by the fingerprint like the Lava.top link, the list moves into `upstream.rs` and
//! `UPSTREAM_FINGERPRINT` is re-pinned.
//!
//! The app shows the full address first, copies it only on an explicit tap (flagged sensitive,
//! cleared from the clipboard after a minute where the platform allows), and opens a Boosty
//! link in the browser only after an explicit tap. Nothing is fetched.

/// What kind of address an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportKind {
    /// A page address (`https://boosty.to/...`), opened in the browser after a tap.
    Boosty,
    /// A cryptocurrency wallet address, copied after a tap.
    Crypto,
}

/// One extra way to support the creators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportAddress {
    /// The kind of address.
    pub kind: SupportKind,
    /// Display name, for example `Bitcoin (BTC)` or `Boosty`.
    pub label: &'static str,
    /// Network or coin ticker for a wallet (`""` for Boosty).
    pub network: &'static str,
    /// The full address, character for character.
    pub value: &'static str,
    /// Memo or destination tag the network needs (`""` if none).
    pub memo: &'static str,
}

// ---- optional extra entries ------------------------------------------------------------------
//
// Wallets and pages that the creators publish are added to this list, character for character;
// the list is empty until then and the About screen shows no card for it. Every entry goes
// through `is_valid` before it can be shown.

/// The configured extra addresses; each entry is checked by [`is_valid`] before it is shown.
pub const EXTRA_SUPPORT_ADDRESSES: &[SupportAddress] = &[];

/// Longest wallet address or memo accepted (the longest real formats are below 110 characters).
pub const MAX_VALUE_LEN: usize = 128;

fn printable_token(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_graphic())
}

/// Whether `entry` is well formed enough to be shown: a label and network that are short plain
/// text, a wallet address that is a single run of printable ASCII characters (no spaces, no
/// look-alike or control characters), a memo that is empty or likewise plain, and a Boosty
/// address that is an `https://boosty.to/` address without credentials or whitespace. Values
/// that look like unfilled placeholders are refused.
#[must_use]
pub fn is_valid(entry: &SupportAddress) -> bool {
    let plain = |s: &str, max: usize| {
        !s.is_empty() && s.chars().count() <= max && s.chars().all(|c| !c.is_control())
    };
    if !plain(entry.label, 40) {
        return false;
    }
    // Unfilled placeholders. Upper-case markers are matched as written: a real address is
    // case-sensitive text and must not be refused for a chance lower-case substring.
    let lower = entry.value.to_ascii_lowercase();
    if ["example", ".invalid", "placeholder", "your_"].iter().any(|p| lower.contains(p))
        || [concat!("TO", "DO"), "XXXX", "CHANGEME"].iter().any(|p| entry.value.contains(p))
    {
        return false;
    }
    match entry.kind {
        SupportKind::Crypto => {
            printable_token(entry.network, 24)
                && printable_token(entry.value, MAX_VALUE_LEN)
                && entry.value.len() >= 20
                && (entry.memo.is_empty() || printable_token(entry.memo, MAX_VALUE_LEN))
        }
        SupportKind::Boosty => {
            let Some(rest) = entry.value.strip_prefix("https://boosty.to/") else { return false };
            printable_token(entry.value, 200)
                && entry.network.is_empty()
                && entry.memo.is_empty()
                && !rest.is_empty()
                && !rest.contains(['@', '\\', '#'])
        }
    }
}

/// The configured entries that pass [`is_valid`].
#[must_use]
pub fn configured() -> Vec<SupportAddress> {
    EXTRA_SUPPORT_ADDRESSES.iter().copied().filter(is_valid).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet(value: &'static str) -> SupportAddress {
        SupportAddress { kind: SupportKind::Crypto, label: "Bitcoin (BTC)", network: "BTC", value, memo: "" }
    }

    fn boosty(value: &'static str) -> SupportAddress {
        SupportAddress { kind: SupportKind::Boosty, label: "Boosty", network: "", value, memo: "" }
    }

    #[test]
    fn nothing_is_invented_every_configured_entry_is_valid() {
        for e in EXTRA_SUPPORT_ADDRESSES {
            assert!(is_valid(e), "invalid extra support entry: {e:?}");
        }
        assert!(configured().iter().all(is_valid));
    }

    #[test]
    fn well_formed_wallets_pass() {
        // Synthetic strings of realistic shapes (not real addresses).
        assert!(is_valid(&wallet("bc1qtestaddressforunittestsonlyxyz")));
        assert!(is_valid(&wallet("0xAbCdEf0123456789AbCdEf0123456789AbCdEf01")));
        let mut with_memo = wallet("EQtestaddressfortestsonly0123456789abcdefg");
        with_memo.memo = "123456";
        assert!(is_valid(&with_memo));
    }

    #[test]
    fn malformed_or_placeholder_wallets_are_dropped() {
        assert!(!is_valid(&wallet("")));
        assert!(!is_valid(&wallet("short")));
        assert!(!is_valid(&wallet("bc1q has a space in the middle of it")));
        assert!(!is_valid(&wallet("bc1qline\nbreakinsideaddress0123456")));
        assert!(!is_valid(&wallet("bc1q\u{0430}ddressWithCyrillicLookAlike012")));
        assert!(!is_valid(&wallet("0xYOUR_WALLET_ADDRESS_GOES_HERE_0000")));
        assert!(!is_valid(&wallet(concat!("TO", "DO-put-the-wallet-address-here-0000"))));
        let mut no_label = wallet("bc1qtestaddressforunittestsonlyxyz");
        no_label.label = "";
        assert!(!is_valid(&no_label));
        let mut no_network = wallet("bc1qtestaddressforunittestsonlyxyz");
        no_network.network = "";
        assert!(!is_valid(&no_network));
        let mut bad_memo = wallet("bc1qtestaddressforunittestsonlyxyz");
        bad_memo.memo = "has space";
        assert!(!is_valid(&bad_memo));
        assert!(!is_valid(&wallet(
            "bc1qaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )));
    }

    #[test]
    fn boosty_must_be_an_https_boosty_page() {
        assert!(is_valid(&boosty("https://boosty.to/pharosteam")));
        assert!(!is_valid(&boosty("http://boosty.to/pharosteam")));
        assert!(!is_valid(&boosty("https://boosty.to/")));
        assert!(!is_valid(&boosty("https://boosty.to.evil.example/x")));
        assert!(!is_valid(&boosty("https://evil.example/boosty.to/x")));
        assert!(!is_valid(&boosty("https://boosty.to/@user")));
        assert!(!is_valid(&boosty("https://boosty.to/a b")));
        assert!(!is_valid(&boosty("https://boosty.to/pharosteam\\@evil.com")));
    }
}
