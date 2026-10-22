//! The Upstream Creator Attribution & Patronage Notice.
//!
//! These constants **are** the "specified author attributions" required by
//! `LICENSE_ADDITIONAL_TERMS.md` (AGPLv3 §7(b), sections 1.2 and 2). A modified version must keep
//! them unchanged and visible; it may *add* its own attribution next to them
//! through `fork_attribution.toml` (see the crate documentation).
//!
//! Maintainers of the original project fill in their real identity and
//! donation links before the first public release. Hosts ending in
//! `.invalid` (RFC 2606: guaranteed never to resolve) mark values that are
//! not yet configured; release builds (`ARCA_RELEASE=1`) refuse to compile
//! while any remain.
//!
//! Every value in this file — including the card titles and the legal notice
//! texts — is covered by a SHA-256 fingerprint pinned in the file
//! `crates/arca-attribution/UPSTREAM_FINGERPRINT`. `build.rs` recomputes it
//! and stops the build on any difference, so the notice cannot be changed by
//! accident. When the original maintainers deliberately change their own
//! entries, the build error prints the new value to pin.

/// A person or team credited on the patronage screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Author {
    /// Display name.
    pub name: &'static str,
    /// Optional profile or homepage (HTTPS).
    pub url: Option<&'static str>,
}

/// A donation channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DonationLink {
    /// Short label ("Boosty", "Open Collective", "Liberapay" ...).
    pub label: &'static str,
    /// HTTPS URL.
    pub url: &'static str,
}

/// Name of the original project.
pub const UPSTREAM_PROJECT: &str = "Arca";

/// Homepage of the original project.
pub const UPSTREAM_HOMEPAGE: &str = "https://github.com/Johnny-Wickky/arca";

/// Where the original Corresponding Source is published.
pub const UPSTREAM_SOURCE: &str = "https://github.com/Johnny-Wickky/arca";

/// Creators of the original Arca core.
pub const UPSTREAM_AUTHORS: &[Author] = &[Author { name: "Pharos", url: None }];

/// Donation links of the original creators (none yet; add them here when they exist).
pub const UPSTREAM_DONATION_LINKS: &[DonationLink] = &[];

/// Copyright line of the original project.
pub const UPSTREAM_COPYRIGHT: &str = "Copyright © 2026 Pharos and contributors";

/// Appropriate Legal Notices (AGPLv3 §0) shown under the cards (English).
pub const LEGAL_NOTICE_EN: &str = "Copyright © 2026 Pharos and contributors. \
Arca is free software: you can redistribute it and/or modify it under the terms of the \
GNU Affero General Public License, version 3, with additional terms under its Section 7. \
It comes with ABSOLUTELY NO WARRANTY. The full license and the additional terms are \
available in the app under \"Licenses\" and in the source code. The original creators \
and their support link on this screen are a required legal notice: every modified \
version must keep them visible.";

/// Appropriate Legal Notices (AGPLv3 §0) shown under the cards (Russian).
pub const LEGAL_NOTICE_RU: &str = "Copyright © 2026 Pharos and contributors. \
Arca — свободная программа: вы можете распространять и изменять её на условиях \
GNU Affero General Public License версии 3 с дополнительными условиями по её разделу 7. \
Программа поставляется БЕЗ КАКИХ-ЛИБО ГАРАНТИЙ. Полный текст лицензии и дополнительных \
условий доступен в приложении в разделе «Лицензии» и в исходном коде. Создатели \
оригинального проекта и ссылка на их поддержку на этом экране — обязательное юридическое \
уведомление: любая модифицированная версия обязана сохранять их видимыми.";

/// Title of the upstream card (English).
pub const UPSTREAM_CARD_TITLE_EN: &str = "Creators of the original Arca core";

/// Title of the upstream card (Russian).
pub const UPSTREAM_CARD_TITLE_RU: &str = "Создатели оригинального ядра Arca";

/// Label of the support action on every card (English).
pub const SUPPORT_ACTION_EN: &str = "Support";

/// Label of the support action on every card (Russian).
pub const SUPPORT_ACTION_RU: &str = "Поддержать";
