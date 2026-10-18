// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Upstream Creator Attribution & Patronage Notice, with additive fork attribution.
//!
//! * [`upstream`] — the original creators and their donation links, compiled
//!   into the core as constants. These are the "specified author attributions"
//!   of `LICENSE_ADDITIONAL_TERMS.md` (AGPLv3 §7(b)).
//! * [`fork`] — the `fork_attribution.toml` format through which maintainers of
//!   a modified version *add* their names and donation links next to the
//!   original ones. The file in the workspace root is validated by `build.rs`;
//!   an invalid or impersonating file fails the build.
//! * [`screen`] — the view model of the dual card on "About & Support":
//!   «Создатели оригинального ядра Arca (Поддержать)» and, in a fork,
//!   «Команда поддержки модификации / Форка (Поддержать)».
//! * [`passport`] — «Паспорт Хранителя»: local, never transmitted statistics.
//! * [`integrity`] — the notice fingerprint and release checks.
//!
//! The crate performs no I/O, no networking and reads no clock. Opening a
//! donation link is a host action that happens only after an explicit tap.
//!
//! ```
//! use arca_attribution::{patronage_screen, screen::{CardRole, Lang}};
//!
//! let screen = patronage_screen(Lang::Ru);
//! assert_eq!(screen.cards[0].role, CardRole::UpstreamCreators);
//! assert_eq!(screen.cards[0].title, "Создатели оригинального ядра Arca");
//! assert_eq!(screen.cards[0].support_label, "Поддержать");
//! ```

#![forbid(unsafe_code)]

pub mod fork;
pub mod integrity;
pub mod passport;
pub mod screen;
pub mod upstream;

pub use fork::{ForkAttribution, ForkConfigError};
pub use integrity::{ReleaseBlocker, release_blockers, upstream_fingerprint};
pub use passport::{PatronPassport, PatronStats};
pub use screen::{CardRole, CreatorCard, Lang, PatronageScreen};
pub use upstream::{
    UPSTREAM_AUTHORS, UPSTREAM_COPYRIGHT, UPSTREAM_DONATION_LINKS, UPSTREAM_PROJECT,
};

/// The GNU AGPL v3 text, for the in-app "Licenses" screen (works offline).
pub const LICENSE_TEXT: &str = include_str!("../../../LICENSE");

/// `LICENSE_ADDITIONAL_TERMS.md` (the additional terms), for the in-app "Licenses" screen.
pub const ADDITIONAL_TERMS_TEXT: &str = include_str!("../../../LICENSE_ADDITIONAL_TERMS.md");

mod generated {
    // Written by build.rs: `FORK_ATTRIBUTION_TOML` and `UPSTREAM_FINGERPRINT_PIN`.
    include!(concat!(env!("OUT_DIR"), "/attribution_config.rs"));
}

pub use generated::{FORK_ATTRIBUTION_TOML, UPSTREAM_FINGERPRINT_PIN};

/// The fork attribution compiled into this build, `None` for the original project.
///
/// The embedded file was validated by `build.rs` with the same parser, so an
/// error here is impossible in a successful build; should it happen anyway,
/// the fork card is omitted while the upstream card stays — the safe side of
/// the licence term.
#[must_use]
pub fn fork_attribution() -> Option<ForkAttribution> {
    FORK_ATTRIBUTION_TOML.and_then(|src| fork::parse(src).ok().flatten())
}

/// Name of this build: the fork name, or the original project name.
#[must_use]
pub fn app_name() -> String {
    fork_attribution().map_or_else(|| UPSTREAM_PROJECT.to_owned(), |f| f.name)
}

/// The "About & Support" screen of this build.
#[must_use]
pub fn patronage_screen(lang: Lang) -> PatronageScreen {
    PatronageScreen::build(lang, fork_attribution().as_ref())
}

/// The Patron Passport of this build for the given local statistics.
#[must_use]
pub fn patron_passport(lang: Lang, stats: &PatronStats, today: u32) -> PatronPassport {
    PatronPassport::build(lang, &app_name(), stats, today, patronage_screen(lang))
}

/// `Ok` when this build may be released publicly.
///
/// # Errors
/// Every [`ReleaseBlocker`] (placeholder URLs, missing donation links).
pub fn release_readiness() -> Result<(), Vec<ReleaseBlocker>> {
    let blockers = release_blockers(fork_attribution().as_ref());
    if blockers.is_empty() { Ok(()) } else { Err(blockers) }
}

#[cfg(test)]
mod tests;
