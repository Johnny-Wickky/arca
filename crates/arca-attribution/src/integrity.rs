//! Tripwires around the upstream notice.
//!
//! * [`upstream_fingerprint`] — SHA-256 over a canonical, length-prefixed
//!   encoding of everything in [`super::upstream`]. `build.rs` compares it with
//!   the pinned value, so editing the notice breaks the build until the pin is
//!   updated too. This is a guard against *accidental* removal, not DRM: the
//!   binding obligation is the licence term, and anybody can edit both files.
//! * [`release_blockers`] — placeholder values (RFC 2606 / RFC 6761 reserved
//!   names) that must never reach users. Release builds (`ARCA_RELEASE=1`)
//!   refuse to compile while any remain.
//!
//! This file is shared verbatim with `build.rs` through `#[path]`.

use core::fmt;

use arca_digest::{Digest, Sha256};

use super::fork::ForkAttribution;
use super::upstream;

/// Version of the canonical encoding; part of the hashed data.
const FINGERPRINT_DOMAIN: &str = "arca-attribution/upstream-notice/v1";

fn field(h: &mut Sha256, tag: u8, value: &str) {
    h.update(&[tag]);
    // Strings in the notice are far below 4 GiB; saturate rather than wrap.
    let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
    h.update(&len.to_le_bytes());
    h.update(value.as_bytes());
}

/// SHA-256 fingerprint of the complete upstream notice.
#[must_use]
pub fn upstream_fingerprint() -> Digest {
    let mut h = Sha256::new();
    field(&mut h, 0x00, FINGERPRINT_DOMAIN);
    field(&mut h, 0x01, upstream::UPSTREAM_PROJECT);
    field(&mut h, 0x02, upstream::UPSTREAM_HOMEPAGE);
    field(&mut h, 0x03, upstream::UPSTREAM_SOURCE);
    field(&mut h, 0x04, upstream::UPSTREAM_COPYRIGHT);
    for a in upstream::UPSTREAM_AUTHORS {
        field(&mut h, 0x10, a.name);
        match a.url {
            Some(u) => field(&mut h, 0x11, u),
            None => field(&mut h, 0x12, ""),
        }
    }
    for d in upstream::UPSTREAM_DONATION_LINKS {
        field(&mut h, 0x20, d.label);
        field(&mut h, 0x21, d.url);
    }
    field(&mut h, 0x30, upstream::LEGAL_NOTICE_EN);
    field(&mut h, 0x31, upstream::LEGAL_NOTICE_RU);
    field(&mut h, 0x40, upstream::UPSTREAM_CARD_TITLE_EN);
    field(&mut h, 0x41, upstream::UPSTREAM_CARD_TITLE_RU);
    field(&mut h, 0x42, upstream::SUPPORT_ACTION_EN);
    field(&mut h, 0x43, upstream::SUPPORT_ACTION_RU);
    h.finalize()
}

/// Something that must be fixed before a public release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseBlocker {
    /// A URL still points to a reserved placeholder domain.
    PlaceholderUrl {
        /// Where the URL is configured.
        field: String,
        /// The URL.
        url: String,
    },
}

impl fmt::Display for ReleaseBlocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlaceholderUrl { field, url } => {
                write!(f, "{field} = {url:?} is a reserved placeholder domain (RFC 2606/6761)")
            }
        }
    }
}

/// `true` for hosts reserved for documentation and testing (RFC 2606, RFC 6761).
#[must_use]
pub fn is_placeholder_url(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    let reserved_tld = ["invalid", "example", "test", "localhost", "local"];
    let reserved_sld = ["example.com", "example.net", "example.org"];
    host.is_empty()
        || reserved_tld.iter().any(|t| host == *t || host.ends_with(&format!(".{t}")))
        || reserved_sld.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Everything that prevents a public release of this build.
#[must_use]
pub fn release_blockers(fork: Option<&ForkAttribution>) -> Vec<ReleaseBlocker> {
    let mut urls: Vec<(String, &str)> = vec![
        ("UPSTREAM_HOMEPAGE".into(), upstream::UPSTREAM_HOMEPAGE),
        ("UPSTREAM_SOURCE".into(), upstream::UPSTREAM_SOURCE),
    ];
    for (i, a) in upstream::UPSTREAM_AUTHORS.iter().enumerate() {
        if let Some(u) = a.url {
            urls.push((format!("UPSTREAM_AUTHORS[{i}].url"), u));
        }
    }
    for (i, d) in upstream::UPSTREAM_DONATION_LINKS.iter().enumerate() {
        urls.push((format!("UPSTREAM_DONATION_LINKS[{i}].url"), d.url));
    }
    if let Some(f) = fork {
        if let Some(u) = &f.homepage {
            urls.push(("fork.homepage".into(), u));
        }
        if let Some(u) = &f.source {
            urls.push(("fork.source".into(), u));
        }
        for (i, m) in f.maintainers.iter().enumerate() {
            if let Some(u) = &m.url {
                urls.push((format!("fork.maintainers[{i}].url"), u));
            }
        }
        for (i, d) in f.donations.iter().enumerate() {
            urls.push((format!("fork.donations[{i}].url"), &d.url));
        }
    }
    urls
        .into_iter()
        .filter(|(_, u)| is_placeholder_url(u))
        .map(|(field, url)| ReleaseBlocker::PlaceholderUrl { field, url: url.to_owned() })
        .collect()
}
