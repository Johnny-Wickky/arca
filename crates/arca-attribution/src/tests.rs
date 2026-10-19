#![allow(clippy::print_stdout, reason = "print_fingerprint helper for maintainers")]

use super::fork::{self, ForkConfigError};
use super::integrity::{self, ReleaseBlocker};
use super::passport::{PatronPassport, PatronStats, RECORD_LEN};
use super::screen::{
    CardRole, Lang, PatronageScreen, ProminenceViolation, RenderedCard, check_upstream_prominence,
};
use super::*;

const EXAMPLE: &str = include_str!("../../../fork_attribution.example.toml");
const SHIPPED: &str = include_str!("../../../fork_attribution.toml");

fn minimal(extra_fork: &str) -> String {
    format!("[fork]\nname = \"Fork\"\n{extra_fork}\n[[fork.maintainers]]\nname = \"Team\"\n")
}

fn err(src: &str) -> ForkConfigError {
    fork::parse(src).unwrap_err()
}

// ---------------------------------------------------------------- fork file

#[test]
fn shipped_file_means_original_build() {
    assert_eq!(fork::parse(SHIPPED).unwrap(), None);
    assert_eq!(fork::parse("").unwrap(), None);
    assert_eq!(fork::parse("# only a comment\n\n   \n").unwrap(), None);
}

#[test]
fn example_file_parses() {
    let f = fork::parse(EXAMPLE).unwrap().unwrap();
    assert_eq!(f.name, "SuperDocs");
    assert_eq!(f.homepage.as_deref(), Some("https://superdocs.example"));
    assert_eq!(f.modifications.as_deref(), Some("Adds on-device OCR for scanned PDF files"));
    assert_eq!(f.maintainers.len(), 1);
    assert_eq!(f.donations.len(), 2);
    assert_eq!(f.donations[1].label, "Liberapay");
}

#[test]
fn string_forms_and_comments() {
    let src = "[[fork.maintainers]] # maintainers may come first\nname = 'Lit \\n eral'\n\
               [fork]\nname = \"Esc\\u0041pe \\\"q\\\"\" # trailing comment\n";
    let f = fork::parse(src).unwrap().unwrap();
    assert_eq!(f.name, "EscApe \"q\"");
    assert_eq!(f.maintainers[0].name, "Lit \\n eral");
}

#[test]
fn syntax_errors_carry_line_numbers() {
    assert!(matches!(err("[fork]\nname = 5\n"), ForkConfigError::Syntax { line: 2, .. }));
    assert!(matches!(err("[fork]\nname = true\n"), ForkConfigError::Syntax { line: 2, .. }));
    assert!(matches!(err("[fork]\nname = \"open\n"), ForkConfigError::Syntax { line: 2, .. }));
    assert!(matches!(err("[fork]\nname = \"a\" junk\n"), ForkConfigError::Syntax { line: 2, .. }));
    assert!(matches!(err("[fork]\nname\n"), ForkConfigError::Syntax { line: 2, .. }));
    assert!(matches!(err("name = \"x\"\n"), ForkConfigError::Syntax { line: 1, .. }));
    assert!(matches!(
        err("[fork]\nname = \"a\"\nname = \"b\"\n"),
        ForkConfigError::Syntax { line: 3, .. }
    ));
    assert!(matches!(err("[fork]\n[fork]\n"), ForkConfigError::Syntax { line: 2, .. }));
    assert!(matches!(
        err("[fork]\n\"quoted key\" = \"x\"\n"),
        ForkConfigError::Syntax { line: 2, .. }
    ));
    assert!(matches!(
        err("[fork]\nname = \"bad \\x escape\"\n"),
        ForkConfigError::Syntax { line: 2, .. }
    ));
    assert!(matches!(
        err("[fork]\nname = { a = \"b\" }\n"),
        ForkConfigError::Syntax { line: 2, .. }
    ));
}

#[test]
fn unknown_keys_and_tables_are_rejected() {
    assert_eq!(
        err("[fork]\nlogo = \"x\"\n"),
        ForkConfigError::Unknown { line: 2, key: "logo".into() }
    );
    assert_eq!(err("[upstream]\n"), ForkConfigError::Unknown { line: 1, key: "upstream".into() });
    assert_eq!(
        err("[[fork.sponsors]]\n"),
        ForkConfigError::Unknown { line: 1, key: "fork.sponsors".into() }
    );
    assert!(matches!(
        err("[[fork.donations]]\nname = \"x\"\n"),
        ForkConfigError::Unknown { line: 2, .. }
    ));
}

#[test]
fn required_fields_and_limits() {
    assert!(
        matches!(err("[fork]\n"), ForkConfigError::Invalid { field, .. } if field == "fork.name")
    );
    assert!(
        matches!(err("[fork]\nname = \"F\"\n"), ForkConfigError::Invalid { field, .. } if field == "fork.maintainers")
    );
    assert!(
        matches!(err("[fork]\nname = \"   \"\n"), ForkConfigError::Invalid { field, .. } if field == "fork.name")
    );
    let long = "x".repeat(65);
    assert!(matches!(
        err(&format!("[fork]\nname = \"{long}\"\n")),
        ForkConfigError::Invalid { .. }
    ));
    let mut many = minimal("");
    for i in 0..9 {
        many.push_str(&format!(
            "[[fork.donations]]\nlabel = \"L{i}\"\nurl = \"https://d{i}.org/x\"\n"
        ));
    }
    assert!(
        matches!(err(&many), ForkConfigError::Invalid { field, .. } if field == "fork.donations")
    );
    let mut m = String::from("[fork]\nname = \"F\"\n");
    for i in 0..17 {
        m.push_str(&format!("[[fork.maintainers]]\nname = \"M{i}\"\n"));
    }
    assert!(
        matches!(err(&m), ForkConfigError::Invalid { field, .. } if field == "fork.maintainers")
    );
    assert!(fork::parse(&minimal("")).unwrap().is_some());
}

#[test]
fn urls_must_be_plain_https() {
    for bad in [
        "http://fork.org",
        "https://",
        "https://localhost",
        "https://user:pw@fork.org",
        "https://fork.org/a b",
        "javascript:alert(1)",
        "https://-bad.org",
        "https://fork..org",
        "ftp://fork.org",
    ] {
        let src = minimal(&format!("homepage = \"{bad}\""));
        assert!(matches!(err(&src), ForkConfigError::Invalid { .. }), "{bad}");
    }
    let ok =
        fork::parse(&minimal("homepage = \"https://fork.org:8443/path?q=1#x\"")).unwrap().unwrap();
    assert_eq!(ok.homepage.as_deref(), Some("https://fork.org:8443/path?q=1#x"));
}

#[test]
fn control_and_bidi_characters_are_rejected() {
    for bad in ["Fork\\u202Ecod.exe", "Fork\\u2066x", "Fork\\tTab", "Fork\\u200Fx"] {
        let src = format!("[fork]\nname = \"{bad}\"\n[[fork.maintainers]]\nname = \"T\"\n");
        assert!(matches!(err(&src), ForkConfigError::Invalid { .. }), "{bad}");
    }
}

#[test]
fn impersonating_upstream_is_rejected() {
    for name in ["Arca", "a r c a", "Ar-ca", " ARCA "] {
        let src = format!("[fork]\nname = \"{name}\"\n[[fork.maintainers]]\nname = \"T\"\n");
        assert!(matches!(err(&src), ForkConfigError::ImpersonatesUpstream(_)), "{name}");
    }
    let author = UPSTREAM_AUTHORS[0].name;
    let src = format!("[fork]\nname = \"F\"\n[[fork.maintainers]]\nname = \"{author}\"\n");
    assert!(matches!(err(&src), ForkConfigError::ImpersonatesUpstream(_)));
    // Re-using the upstream donation URL as "the fork's" link is pointless and confusing.
    let link = UPSTREAM_DONATION_LINKS[0].url;
    let src = format!("{}[[fork.donations]]\nlabel = \"x\"\nurl = \"{link}\"\n", minimal(""));
    assert!(
        matches!(err(&src), ForkConfigError::Invalid { field, .. } if field == "fork.donations[0]")
    );
}

#[test]
fn errors_display_human_readable_text() {
    assert_eq!(err("[x]\n").to_string(), "line 1: unknown key or table \"x\"");
    assert!(err("[fork]\nname = \"Arca\"\n").to_string().contains("§7(c)"));
    assert!(LICENSE_TEXT.contains("GNU AFFERO GENERAL PUBLIC LICENSE"));
    assert!(ADDITIONAL_TERMS_TEXT.contains("Additive Fork Attribution"));
    assert!(
        upstream::LEGAL_NOTICE_EN.contains("NO WARRANTY")
            && upstream::LEGAL_NOTICE_RU.contains("БЕЗ КАКИХ-ЛИБО ГАРАНТИЙ")
    );
}

#[test]
fn authors_file_and_manifest_match_the_upstream_constants() {
    let authors_file = include_str!("../../../AUTHORS");
    let manifest_authors: Vec<&str> = env!("CARGO_PKG_AUTHORS").split(':').collect();
    for a in UPSTREAM_AUTHORS {
        assert!(authors_file.contains(a.name), "AUTHORS lacks {}", a.name);
        if let Some(url) = a.url {
            assert!(authors_file.contains(url), "AUTHORS lacks {url}");
        }
        assert!(manifest_authors.contains(&a.name), "Cargo.toml `authors` lacks {}", a.name);
    }
}

#[test]
fn english_translation_follows_the_russian_original() {
    let headings = |t: &str| t.lines().filter(|l| l.starts_with("### ")).count();
    let clauses = |t: &str| {
        t.lines()
            .filter(|l| {
                let l = l.trim_start();
                l.split_once(". ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
            .count()
    };
    assert_eq!(headings(ADDITIONAL_TERMS_TEXT), headings(ADDITIONAL_TERMS_TRANSLATION_EN));
    assert_eq!(clauses(ADDITIONAL_TERMS_TEXT), clauses(ADDITIONAL_TERMS_TRANSLATION_EN));
    // The translation must say which text is binding, and keep the legal anchors.
    assert!(ADDITIONAL_TERMS_TRANSLATION_EN.contains("the Russian original prevails"));
    for anchor in ["Section 7(b)", "Section 8", "Additive Fork Attribution"] {
        assert!(ADDITIONAL_TERMS_TRANSLATION_EN.contains(anchor), "{anchor}");
    }
}

// ---------------------------------------------------------------- screen

#[test]
fn original_build_shows_only_the_upstream_card() {
    let s = PatronageScreen::build(Lang::Ru, None);
    assert_eq!(s.cards.len(), 1);
    let up = s.upstream_card().unwrap();
    assert_eq!(up.title, "Создатели оригинального ядра Arca");
    assert_eq!(up.support_label, "Поддержать");
    assert_eq!(up.support_links.len(), UPSTREAM_DONATION_LINKS.len());
    assert_eq!(up.people[0].name, UPSTREAM_AUTHORS[0].name);
    assert!(s.fork_card().is_none());
    assert!(s.modified_version_notice.is_none());
    assert_eq!(s.legal_notice, upstream::LEGAL_NOTICE_RU);
}

#[test]
fn fork_build_shows_both_cards_upstream_first() {
    let f = fork::parse(EXAMPLE).unwrap().unwrap();
    for lang in [Lang::Ru, Lang::En] {
        let s = PatronageScreen::build(lang, Some(&f));
        let roles: Vec<CardRole> = s.cards.iter().map(|c| c.role).collect();
        assert_eq!(roles, [CardRole::UpstreamCreators, CardRole::ForkMaintainers]);
        let (up, fk) = (&s.cards[0], &s.cards[1]);
        // One shared support label: neither card is styled as the "main" one.
        assert_eq!(up.support_label, fk.support_label);
        assert_eq!(fk.project, "SuperDocs");
        assert_eq!(fk.support_links.len(), 2);
        assert_eq!(fk.modifications.as_deref(), f.modifications.as_deref());
        assert!(s.modified_version_notice.as_deref().unwrap().contains("SuperDocs"));
    }
    let ru = PatronageScreen::build(Lang::Ru, Some(&f));
    assert_eq!(ru.cards[1].title, "Команда поддержки модификации / Форка");
    assert!(
        ru.modified_version_notice.unwrap().starts_with("SuperDocs — модифицированная версия Arca")
    );
    let en = PatronageScreen::build(Lang::En, Some(&f));
    assert_eq!(en.cards[0].title, "Creators of the original Arca core");
    assert_eq!(en.cards[1].support_label, "Support");
}

fn rendered(role: CardRole) -> RenderedCard {
    RenderedCard {
        role,
        on_screen: true,
        area: 100_000,
        title_size: 1_700,
        contrast: 1_200,
        support_tappable: true,
    }
}

#[test]
fn prominence_check_accepts_equal_cards() {
    let up = rendered(CardRole::UpstreamCreators);
    let fk = rendered(CardRole::ForkMaintainers);
    assert_eq!(check_upstream_prominence(&[up, fk]), Ok(()));
    assert_eq!(check_upstream_prominence(&[up]), Ok(()));
    // 10 % tolerance for text reflow.
    let fk_bigger = RenderedCard { area: 110_000, ..fk };
    assert_eq!(check_upstream_prominence(&[up, fk_bigger]), Ok(()));
}

#[test]
fn prominence_check_rejects_hidden_or_demoted_upstream() {
    let up = rendered(CardRole::UpstreamCreators);
    let fk = rendered(CardRole::ForkMaintainers);
    let cases = [
        (vec![fk], ProminenceViolation::UpstreamCardCount(0)),
        (vec![up, up, fk], ProminenceViolation::UpstreamCardCount(2)),
        (vec![RenderedCard { on_screen: false, ..up }, fk], ProminenceViolation::UpstreamOffScreen),
        (vec![RenderedCard { area: 80_000, ..up }, fk], ProminenceViolation::UpstreamSmaller),
        (
            vec![RenderedCard { title_size: 1_200, ..up }, fk],
            ProminenceViolation::UpstreamTitleSmaller,
        ),
        (vec![RenderedCard { contrast: 300, ..up }, fk], ProminenceViolation::UpstreamLowContrast),
        (
            vec![RenderedCard { support_tappable: false, ..up }, fk],
            ProminenceViolation::UpstreamSupportHidden,
        ),
    ];
    for (cards, expected) in cases {
        assert_eq!(check_upstream_prominence(&cards), Err(expected));
        assert!(!expected.to_string().is_empty());
    }
}

// ---------------------------------------------------------------- integrity

#[test]
fn fingerprint_matches_the_pin() {
    assert_eq!(upstream_fingerprint().to_string(), UPSTREAM_FINGERPRINT_PIN);
    assert_eq!(UPSTREAM_FINGERPRINT_PIN.len(), 64);
}

#[test]
fn print_fingerprint() {
    // `cargo test -p arca-attribution -- --nocapture print_fingerprint`
    println!("UPSTREAM_FINGERPRINT = {}", upstream_fingerprint());
}

#[test]
fn placeholder_detection() {
    for p in [
        "https://arca.invalid/donate",
        "https://superdocs.example",
        "https://example.com/x",
        "https://www.example.org",
        "https://a.test:443/",
        "https://localhost/",
        "https://printer.local",
        "https://user@EXAMPLE.NET./x",
        "",
    ] {
        assert!(integrity::is_placeholder_url(p), "{p}");
    }
    for real in [
        "https://liberapay.com/x",
        "https://opencollective.com/x",
        "https://examples.com",
        "https://invalid.org",
    ] {
        assert!(!integrity::is_placeholder_url(real), "{real}");
    }
}

#[test]
fn release_blockers_list_every_placeholder() {
    let upstream_placeholders = core::iter::once(upstream::UPSTREAM_HOMEPAGE)
        .chain([upstream::UPSTREAM_SOURCE])
        .chain(UPSTREAM_AUTHORS.iter().filter_map(|a| a.url))
        .chain(UPSTREAM_DONATION_LINKS.iter().map(|d| d.url))
        .filter(|u| integrity::is_placeholder_url(u))
        .count();
    let f = fork::parse(EXAMPLE).unwrap().unwrap();
    let blockers = release_blockers(Some(&f));
    // homepage, source, maintainer url and two donation links of the example fork.
    assert_eq!(blockers.len(), upstream_placeholders + 5);
    assert!(blockers.iter().any(|b| matches!(b, ReleaseBlocker::PlaceholderUrl { field, .. } if field == "fork.donations[1].url")));
    let real = fork::parse(&minimal("homepage = \"https://fork.org\"")).unwrap().unwrap();
    assert_eq!(release_blockers(Some(&real)).len(), upstream_placeholders);
    assert_eq!(
        release_readiness().is_ok(),
        release_blockers(fork_attribution().as_ref()).is_empty()
    );
}

#[test]
fn embedded_configuration_is_consistent() {
    assert_eq!(FORK_ATTRIBUTION_TOML.is_some(), fork_attribution().is_some());
    let screen = patronage_screen(Lang::En);
    assert_eq!(screen.cards[0].role, CardRole::UpstreamCreators);
    match fork_attribution() {
        Some(f) => {
            assert_eq!(screen.cards.len(), 2);
            assert_eq!(app_name(), f.name);
        }
        None => {
            assert_eq!(screen.cards.len(), 1);
            assert_eq!(app_name(), "Arca");
        }
    }
}

// ---------------------------------------------------------------- passport

#[test]
fn stats_record_round_trips_and_detects_corruption() {
    let mut s = PatronStats::starting(20_000);
    s.record_document(4, 1);
    s.record_document(0, 2);
    let rec = s.encode();
    assert_eq!(rec.len(), RECORD_LEN);
    assert_eq!(PatronStats::decode(&rec), Ok(s));
    assert_eq!(s.documents_opened, 2);
    assert_eq!(s.trackers_blocked, 4);
    assert_eq!(s.active_content_neutralized, 3);
    for i in 0..RECORD_LEN {
        let mut bad = rec;
        bad[i] ^= 0x01;
        assert!(PatronStats::decode(&bad).is_err(), "byte {i}");
    }
    assert!(PatronStats::decode(&rec[..RECORD_LEN - 1]).is_err());
    assert!(PatronStats::decode(&[]).is_err());
}

#[test]
fn counters_saturate_and_days_never_underflow() {
    let mut s = PatronStats {
        first_use_day: 100,
        documents_opened: u64::MAX,
        trackers_blocked: u64::MAX - 1,
        active_content_neutralized: 0,
    };
    s.record_document(5, 0);
    assert_eq!(s.documents_opened, u64::MAX);
    assert_eq!(s.trackers_blocked, u64::MAX);
    assert_eq!(s.days_in_use(100), 1);
    assert_eq!(s.days_in_use(129), 30);
    assert_eq!(s.days_in_use(50), 1, "clock moved backwards");
    assert_eq!(PatronStats::starting(0).days_in_use(u32::MAX), u32::MAX);
}

fn passport(lang: Lang, days: u32, docs: u64, trackers: u64, active: u64) -> PatronPassport {
    let stats = PatronStats {
        first_use_day: 1_000,
        documents_opened: docs,
        trackers_blocked: trackers,
        active_content_neutralized: active,
    };
    PatronPassport::build(
        lang,
        "Arca",
        &stats,
        1_000 + days - 1,
        PatronageScreen::build(lang, None),
    )
}

#[test]
fn passport_text_matches_the_specification() {
    let p = passport(Lang::Ru, 30, 64, 4, 0);
    assert_eq!(p.title, "Паспорт Хранителя");
    assert_eq!(
        p.body,
        "Вы пользуетесь Arca уже 30 дней. Мы безопасно открыли для вас 64 документа и нейтрализовали 4 скрытых трекера за 0 ₽."
    );
    assert_eq!(p.call_to_action, "Поддержите независимую открытую разработку");
    assert_eq!(p.support.cards[0].role, CardRole::UpstreamCreators);
    let en = passport(Lang::En, 30, 64, 4, 0);
    assert_eq!(
        en.body,
        "You have been using Arca for 30 days. We safely opened 64 documents for you and neutralized 4 hidden trackers — free of charge."
    );
}

#[test]
fn russian_plurals() {
    let cases = [
        (
            2,
            1,
            1,
            1,
            "уже 2 дня. Мы безопасно открыли для вас 1 документ, нейтрализовали 1 скрытый трекер и обезвредили 1 активную угрозу за 0 ₽.",
        ),
        (
            5,
            2,
            2,
            2,
            "уже 5 дней. Мы безопасно открыли для вас 2 документа, нейтрализовали 2 скрытых трекера и обезвредили 2 активные угрозы за 0 ₽.",
        ),
        (
            11,
            11,
            12,
            14,
            "уже 11 дней. Мы безопасно открыли для вас 11 документов, нейтрализовали 12 скрытых трекеров и обезвредили 14 активных угроз за 0 ₽.",
        ),
        (
            21,
            21,
            22,
            25,
            "уже 21 день. Мы безопасно открыли для вас 21 документ, нейтрализовали 22 скрытых трекера и обезвредили 25 активных угроз за 0 ₽.",
        ),
        (112, 0, 0, 0, "уже 112 дней. Мы безопасно открыли для вас 0 документов за 0 ₽."),
    ];
    for (days, docs, trackers, active, tail) in cases {
        let body = passport(Lang::Ru, days, docs, trackers, active).body;
        assert_eq!(body, format!("Вы пользуетесь Arca {tail}"));
    }
    assert!(passport(Lang::Ru, 1, 0, 0, 0).body.starts_with("Вы пользуетесь Arca первый день."));
    assert!(
        passport(Lang::En, 1, 1, 0, 0)
            .body
            .starts_with("Today is your first day with Arca. We safely opened 1 document")
    );
    assert!(passport(Lang::En, 3, 3, 1, 2).body.ends_with("documents for you, neutralized 1 hidden tracker, and disarmed 2 active threats — free of charge."));
}

#[test]
fn notice_file_matches_the_upstream_constants() {
    let notice = include_str!("../../../NOTICE.md");
    let mut expected = vec![
        UPSTREAM_PROJECT,
        UPSTREAM_COPYRIGHT,
        upstream::UPSTREAM_HOMEPAGE,
        upstream::UPSTREAM_SOURCE,
        upstream::UPSTREAM_CARD_TITLE_EN,
        upstream::UPSTREAM_CARD_TITLE_RU,
    ];
    for a in UPSTREAM_AUTHORS {
        expected.push(a.name);
        expected.extend(a.url);
    }
    for d in UPSTREAM_DONATION_LINKS {
        expected.extend([d.label, d.url]);
    }
    for value in expected {
        assert!(notice.contains(value), "NOTICE.md does not mention {value:?}");
    }
}
