//! View model of the "About & Support" screen.
//!
//! The model is deliberately presentation-agnostic: the Flutter client renders
//! it, it never renders itself. Two properties are guaranteed by construction:
//!
//! * the upstream card is always present and always first;
//! * no card carries its own emphasis or style — every card on the screen is
//!   rendered with one shared style, which is what "the same visual priority"
//!   in `LICENSE_ADDITIONAL_TERMS.md` means in code.
//!
//! [`check_upstream_prominence`] turns the same rule into a check that UI
//! golden tests run against the *rendered* layout.

use core::fmt;

use super::fork::ForkAttribution;
use super::upstream;

/// UI language of the generated texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Russian.
    Ru,
    /// English.
    En,
}

/// Which card this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardRole {
    /// "Creators of the original Arca core" — mandatory, §7(b).
    UpstreamCreators,
    /// "Maintainers of this modification / fork" — optional, additive.
    ForkMaintainers,
}

/// A credited person or team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// Display name.
    pub name: String,
    /// Optional HTTPS URL.
    pub url: Option<String>,
}

/// One donation channel behind the card's support action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportLink {
    /// Short label.
    pub label: String,
    /// HTTPS URL, opened in the system browser only after an explicit tap.
    pub url: String,
}

/// One card of the dual attribution block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatorCard {
    /// Upstream or fork.
    pub role: CardRole,
    /// Localised card title.
    pub title: String,
    /// Project name shown on the card.
    pub project: String,
    /// Credited people.
    pub people: Vec<Person>,
    /// Localised label of the support action ("Поддержать" / "Support").
    pub support_label: &'static str,
    /// Donation channels; the support action is shown when non-empty.
    pub support_links: Vec<SupportLink>,
    /// Homepage.
    pub homepage: Option<String>,
    /// Corresponding Source location.
    pub source: Option<String>,
    /// Description of the modifications (fork card only).
    pub modifications: Option<String>,
}

/// The complete "About & Support" screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatronageScreen {
    /// Language of all texts.
    pub lang: Lang,
    /// Upstream card first, then the fork card if this build is a fork.
    pub cards: Vec<CreatorCard>,
    /// The legal notice shown under the cards.
    pub legal_notice: &'static str,
    /// "X is a modified version of Arca …" (AGPLv3 §7(c)); `None` for the original.
    pub modified_version_notice: Option<String>,
}

impl PatronageScreen {
    /// Builds the screen for `lang`, with the fork card when `fork` is set.
    #[must_use]
    pub fn build(lang: Lang, fork: Option<&ForkAttribution>) -> Self {
        let support_label = match lang {
            Lang::Ru => upstream::SUPPORT_ACTION_RU,
            Lang::En => upstream::SUPPORT_ACTION_EN,
        };
        let mut cards = vec![CreatorCard {
            role: CardRole::UpstreamCreators,
            title: match lang {
                Lang::Ru => upstream::UPSTREAM_CARD_TITLE_RU,
                Lang::En => upstream::UPSTREAM_CARD_TITLE_EN,
            }
            .to_owned(),
            project: upstream::UPSTREAM_PROJECT.to_owned(),
            people: upstream::UPSTREAM_AUTHORS
                .iter()
                .map(|a| Person { name: a.name.to_owned(), url: a.url.map(str::to_owned) })
                .collect(),
            support_label,
            support_links: upstream::UPSTREAM_DONATION_LINKS
                .iter()
                .map(|d| SupportLink { label: d.label.to_owned(), url: d.url.to_owned() })
                .collect(),
            homepage: Some(upstream::UPSTREAM_HOMEPAGE.to_owned()),
            source: Some(upstream::UPSTREAM_SOURCE.to_owned()),
            modifications: None,
        }];
        let modified_version_notice = fork.map(|f| match lang {
            Lang::Ru => format!(
                "{} — модифицированная версия {}, которую сопровождает независимая команда. \
                 Создатели оригинального проекта не отвечают за изменения.",
                f.name,
                upstream::UPSTREAM_PROJECT
            ),
            Lang::En => format!(
                "{} is a modified version of {} maintained independently. \
                 The original creators are not responsible for the modifications.",
                f.name,
                upstream::UPSTREAM_PROJECT
            ),
        });
        if let Some(f) = fork {
            cards.push(CreatorCard {
                role: CardRole::ForkMaintainers,
                title: match lang {
                    Lang::Ru => "Команда поддержки модификации / Форка",
                    Lang::En => "Maintainers of this modification / fork",
                }
                .to_owned(),
                project: f.name.clone(),
                people: f
                    .maintainers
                    .iter()
                    .map(|m| Person { name: m.name.clone(), url: m.url.clone() })
                    .collect(),
                support_label,
                support_links: f
                    .donations
                    .iter()
                    .map(|d| SupportLink { label: d.label.clone(), url: d.url.clone() })
                    .collect(),
                homepage: f.homepage.clone(),
                source: f.source.clone(),
                modifications: f.modifications.clone(),
            });
        }
        Self {
            lang,
            cards,
            legal_notice: match lang {
                Lang::Ru => upstream::LEGAL_NOTICE_RU,
                Lang::En => upstream::LEGAL_NOTICE_EN,
            },
            modified_version_notice,
        }
    }

    /// The mandatory upstream card.
    #[must_use]
    pub fn upstream_card(&self) -> Option<&CreatorCard> {
        self.cards.iter().find(|c| c.role == CardRole::UpstreamCreators)
    }

    /// The fork card, if this build is a fork.
    #[must_use]
    pub fn fork_card(&self) -> Option<&CreatorCard> {
        self.cards.iter().find(|c| c.role == CardRole::ForkMaintainers)
    }
}

/// Measurements of one rendered card, reported by a UI golden test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderedCard {
    /// Which card was measured.
    pub role: CardRole,
    /// Fully inside the viewport of the same screen without scrolling to another page.
    pub on_screen: bool,
    /// Visible area in logical pixels².
    pub area: u32,
    /// Title font size in 1/100 logical pixels.
    pub title_size: u32,
    /// Text-to-background contrast ratio × 100 (WCAG; 450 = 4.5:1).
    pub contrast: u32,
    /// The support action is shown and can be tapped.
    pub support_tappable: bool,
}

/// Why a rendered layout does not give the upstream card its required prominence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProminenceViolation {
    /// The upstream card is missing or rendered more than once.
    UpstreamCardCount(usize),
    /// The upstream card is not on the same screen.
    UpstreamOffScreen,
    /// The upstream card is smaller than 90 % of the fork card.
    UpstreamSmaller,
    /// The upstream title is set in a smaller font than the fork title.
    UpstreamTitleSmaller,
    /// The upstream card's text contrast is below WCAG AA (4.5:1).
    UpstreamLowContrast,
    /// The upstream support action is hidden or disabled.
    UpstreamSupportHidden,
}

impl fmt::Display for ProminenceViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UpstreamCardCount(n) => {
                write!(f, "expected exactly one upstream card, found {n}")
            }
            Self::UpstreamOffScreen => {
                f.write_str("upstream card is not visible on the same screen")
            }
            Self::UpstreamSmaller => {
                f.write_str("upstream card is smaller than 90% of the fork card")
            }
            Self::UpstreamTitleSmaller => {
                f.write_str("upstream title font is smaller than the fork title font")
            }
            Self::UpstreamLowContrast => f.write_str("upstream card text contrast is below 4.5:1"),
            Self::UpstreamSupportHidden => {
                f.write_str("upstream support action is hidden or disabled")
            }
        }
    }
}

impl core::error::Error for ProminenceViolation {}

/// Checks a rendered "About & Support" layout against the additive-attribution
/// rule: the upstream card is on the same screen, at least as prominent as any
/// fork card (area tolerance 10 % for text reflow), readable, and its support
/// action is usable whenever upstream donation links exist.
///
/// # Errors
/// The first [`ProminenceViolation`] found.
pub fn check_upstream_prominence(cards: &[RenderedCard]) -> Result<(), ProminenceViolation> {
    let upstream_cards: Vec<&RenderedCard> =
        cards.iter().filter(|c| c.role == CardRole::UpstreamCreators).collect();
    let [up] = upstream_cards.as_slice() else {
        return Err(ProminenceViolation::UpstreamCardCount(upstream_cards.len()));
    };
    if !up.on_screen {
        return Err(ProminenceViolation::UpstreamOffScreen);
    }
    if up.contrast < 450 {
        return Err(ProminenceViolation::UpstreamLowContrast);
    }
    if !upstream::UPSTREAM_DONATION_LINKS.is_empty() && !up.support_tappable {
        return Err(ProminenceViolation::UpstreamSupportHidden);
    }
    for fork in cards.iter().filter(|c| c.role == CardRole::ForkMaintainers) {
        if u64::from(up.area) * 10 < u64::from(fork.area) * 9 {
            return Err(ProminenceViolation::UpstreamSmaller);
        }
        if up.title_size < fork.title_size {
            return Err(ProminenceViolation::UpstreamTitleSmaller);
        }
    }
    Ok(())
}
