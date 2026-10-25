#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::string_slice,
    reason = "test code with small constants and ASCII text"
)]

use super::*;

const HOUR: i64 = 3_600_000;
/// 2026-03-01 00:00:00 UTC.
const T0: i64 = 1_772_323_200_000;

fn mins(ms: i64, off: i64) -> i64 {
    local_minute(ms, off)
}

#[test]
fn local_minute_handles_offsets_and_dates_before_the_epoch() {
    assert_eq!(mins(0, 0), 0);
    assert_eq!(mins(0, 3 * HOUR), 180);
    assert_eq!(mins(0, -HOUR), 23 * 60);
    assert_eq!(mins(T0 + 12 * HOUR + 5 * 60_000, 0), 12 * 60 + 5);
    // Negative instants are clamped to the epoch instead of panicking.
    assert_eq!(mins(-5, 0), 0);
    // An absurd offset is clamped to 18 hours.
    assert_eq!(mins(0, i64::MAX), 18 * 60);
    assert_eq!(mins(0, i64::MIN), 6 * 60);
    assert!((0..1440).contains(&mins(i64::MAX, i64::MAX)));
}

#[test]
fn plan_next_lands_7_to_9_days_ahead_between_noon_and_six() {
    for off_h in [-12, -8, -3, 0, 1, 3, 5, 9, 14] {
        let off = i64::from(off_h) * HOUR;
        for hour in 0..24 {
            let now = T0 + i64::from(hour) * HOUR + 17 * 60_000;
            for rnd in (0..1500u32).chain([u32::MAX, u32::MAX - 1]) {
                let t = plan_next(now, off, rnd);
                let m = mins(t, off);
                assert!((720..1080).contains(&m), "minute {m}");
                let gap = t - now;
                assert!(gap > 6 * DAY_MS + 12 * HOUR - 1, "gap {gap}");
                assert!(gap <= 10 * DAY_MS, "gap {gap}");
                let days = (t + off).div_euclid(DAY_MS) - (now + off).div_euclid(DAY_MS);
                assert!((7..=9).contains(&days), "days {days}");
            }
        }
    }
}

#[test]
fn plan_next_uses_every_day_and_minute_choice_and_is_deterministic() {
    let mut days = [false; 3];
    let mut early = false;
    let mut late = false;
    for rnd in 0..3000u32 {
        assert_eq!(plan_next(T0, 0, rnd), plan_next(T0, 0, rnd));
        let t = plan_next(T0, 0, rnd);
        let d = usize::try_from((t - T0) / DAY_MS - 7).unwrap();
        days[d] = true;
        early |= mins(t, 0) == 720;
        late |= mins(t, 0) == 1079;
    }
    assert_eq!(days, [true; 3]);
    assert!(early && late);
}

#[test]
fn extreme_arguments_neither_panic_nor_go_negative() {
    for now in [i64::MIN, -1, 0, 1, T0, i64::MAX / 4, i64::MAX] {
        for off in [i64::MIN, -1, 0, 1, i64::MAX] {
            for rnd in [0, 1, u32::MAX] {
                assert!(plan_next(now, off, rnd) >= 0);
                let d = on_fire(now, off, now, true, true, rnd);
                assert!(d.next_at_ms >= 0);
                let s = on_start(now, now, off, true, rnd);
                assert!(s.at_ms >= 0);
                let _ = on_start(i64::MIN, now, off, true, rnd);
                let _ = on_start(i64::MAX, now, off, true, rnd);
                assert!(refine_for_offset(now, off, off) >= 0);
            }
        }
    }
}

/// A fake zone: UTC+1 before `switch`, UTC+2 from then on (daylight saving time starts).
fn fake_dst(ms: i64, switch: i64) -> i64 {
    if ms < switch { HOUR } else { 2 * HOUR }
}

#[test]
fn refine_keeps_the_wall_clock_time_across_a_dst_change() {
    let switch = T0 + 3 * DAY_MS;
    for rnd in 0..500u32 {
        let off_now = fake_dst(T0, switch);
        let first = plan_next(T0, off_now, rnd);
        let wanted = mins(first, off_now);
        // Without the correction the wall-clock time moves by an hour.
        assert_eq!(mins(first, fake_dst(first, switch)), (wanted + 60) % 1440);
        let fixed = refine_for_offset(first, off_now, fake_dst(first, switch));
        assert_eq!(mins(fixed, fake_dst(fixed, switch)), wanted);
    }
    // The other direction (daylight saving time ends): UTC+2 now, UTC+1 at the target.
    let first = plan_next(T0, 2 * HOUR, 7);
    let wanted = mins(first, 2 * HOUR);
    let fixed = refine_for_offset(first, 2 * HOUR, HOUR);
    assert_eq!(mins(fixed, HOUR), wanted);
    // Same offset: no change.
    assert_eq!(refine_for_offset(first, HOUR, HOUR), first);
}

#[test]
fn even_an_uncorrected_dst_shift_stays_inside_the_window() {
    // 12:00 .. 17:59 shifted by one hour either way is inside 11:00 .. 20:30.
    for rnd in 0..360u32 * 3 {
        let t = plan_next(T0, 0, rnd);
        for shift in [-HOUR, HOUR] {
            let m = mins(t, shift);
            assert!((POST_FROM_MINUTE..POST_UNTIL_MINUTE).contains(&m));
        }
    }
}

fn fire(now: i64, off: i64, last: i64, enabled: bool, can: bool) -> FireDecision {
    on_fire(now, off, last, enabled, can, 5)
}

#[test]
fn a_disabled_reminder_plans_nothing() {
    let d = fire(T0 + 13 * HOUR, 0, 0, false, true);
    assert_eq!(d, FireDecision { action: FireAction::Disabled, next_at_ms: 0 });
    let d = fire(T0 + 13 * HOUR, 0, 0, false, false);
    assert_eq!(d.action, FireAction::Disabled);
}

#[test]
fn a_missing_permission_keeps_the_chain_alive() {
    // The old code returned without planning anything: the reminders stopped for good.
    let now = T0 + 13 * HOUR;
    let d = fire(now, 0, 0, true, false);
    assert_eq!(d.action, FireAction::NoPermission);
    assert!(d.next_at_ms > now + 6 * DAY_MS);
}

#[test]
fn the_window_is_eleven_to_half_past_eight() {
    let off = 3 * HOUR;
    let day = T0 - off; // local midnight
    let at_min = |m: i64| day + m * 60_000;
    for (m, shown) in [
        (0, false),
        (5 * 60, false),
        (10 * 60 + 59, false),
        (11 * 60, true),
        (12 * 60, true),
        (20 * 60 + 29, true),
        (20 * 60 + 30, false),
        (20 * 60 + 59, false), // the old code showed everything before 21:00
        (23 * 60 + 59, false),
    ] {
        let now = at_min(m);
        let d = fire(now, off, 0, true, true);
        assert_eq!(d.action == FireAction::Show, shown, "minute {m}");
        assert!(d.next_at_ms > now, "minute {m}");
        if !shown {
            assert_eq!(d.action, FireAction::QuietHours);
            assert_eq!(mins(d.next_at_ms, off), RETRY_MINUTE);
            let days = (d.next_at_ms + off).div_euclid(DAY_MS) - (now + off).div_euclid(DAY_MS);
            assert_eq!(days, i64::from(m >= 20 * 60 + 30), "minute {m}");
        }
    }
}

#[test]
fn a_time_zone_change_is_judged_by_the_zone_at_fire_time() {
    // Planned at noon in UTC+3, but the phone is now in UTC-8: it is 01:00-04:00 there.
    let planned = plan_next(T0, 3 * HOUR, 0);
    let d = fire(planned, -8 * HOUR, 0, true, true);
    assert_eq!(d.action, FireAction::QuietHours);
    assert_eq!(mins(d.next_at_ms, -8 * HOUR), RETRY_MINUTE);
    assert!(d.next_at_ms > planned);
    // And the other way: planned in UTC-8, now UTC+5:30.
    let planned = plan_next(T0, -8 * HOUR, 0);
    let d = fire(planned, 5 * HOUR + 30 * 60_000, 0, true, true);
    assert_eq!(d.action, FireAction::QuietHours);
}

#[test]
fn two_reminders_are_never_closer_than_six_days() {
    let now = T0 + 13 * HOUR;
    assert_eq!(fire(now, 0, now - 1, true, true).action, FireAction::TooSoon);
    assert_eq!(fire(now, 0, now - 3 * DAY_MS, true, true).action, FireAction::TooSoon);
    assert_eq!(fire(now, 0, now - 6 * DAY_MS + 1, true, true).action, FireAction::TooSoon);
    assert_eq!(fire(now, 0, now - 6 * DAY_MS, true, true).action, FireAction::Show);
    let d = fire(now, 0, now - DAY_MS, true, true);
    assert!(d.next_at_ms > now + 6 * DAY_MS);
}

#[test]
fn a_last_shown_time_in_the_future_does_not_block_forever() {
    // The clock was set back a year: "last shown" lies ahead. Treat it as unknown.
    let now = T0 + 13 * HOUR;
    assert_eq!(fire(now, 0, now + 365 * DAY_MS, true, true).action, FireAction::Show);
    assert_eq!(fire(now, 0, i64::MAX, true, true).action, FireAction::Show);
    assert_eq!(fire(now, 0, -5, true, true).action, FireAction::Show);
    assert_eq!(fire(now, 0, 0, true, true).action, FireAction::Show);
}

#[test]
fn the_next_time_is_never_in_the_past() {
    for hour in 0..24 {
        for off_h in [-11, -3, 0, 4, 13] {
            let now = T0 + i64::from(hour) * HOUR;
            let off = i64::from(off_h) * HOUR;
            for can in [true, false] {
                for last in [0, now - DAY_MS, now + DAY_MS] {
                    let d = on_fire(now, off, last, true, can, 11);
                    assert!(d.next_at_ms > now, "{d:?}");
                }
            }
        }
    }
}

#[test]
fn start_without_a_stored_time_plans_one() {
    let now = T0 + 9 * HOUR;
    let p = on_start(0, now, 0, true, 3);
    assert_eq!(p.action, StartAction::Plan);
    assert_eq!(p.at_ms, plan_next(now, 0, 3));
    assert_eq!(on_start(-4, now, 0, true, 3).action, StartAction::Plan);
}

#[test]
fn start_with_reminders_off_cancels() {
    let p = on_start(T0 + DAY_MS, T0, 0, false, 3);
    assert_eq!(p, StartPlan { action: StartAction::Off, at_ms: 0 });
}

#[test]
fn start_keeps_a_plausible_time_and_rearms_it_after_a_reboot() {
    let now = T0;
    for ahead in [1, HOUR, DAY_MS, 7 * DAY_MS, 10 * DAY_MS] {
        let p = on_start(now + ahead, now, 0, true, 3);
        assert_eq!(p, StartPlan { action: StartAction::Keep, at_ms: now + ahead });
    }
}

#[test]
fn start_replans_a_time_that_is_absurdly_far_away() {
    // The clock was set a year back after planning: do not wait a year.
    let now = T0;
    let p = on_start(now + 10 * DAY_MS + 1, now, 0, true, 3);
    assert_eq!(p.action, StartAction::Plan);
    assert_eq!(on_start(now + 400 * DAY_MS, now, 0, true, 3).action, StartAction::Plan);
    assert_eq!(on_start(i64::MAX, now, 0, true, 3).action, StartAction::Plan);
}

#[test]
fn start_runs_a_slightly_overdue_reminder_soon_and_replans_an_old_one() {
    let now = T0 + 14 * HOUR;
    let p = on_start(now - HOUR, now, 0, true, 3);
    assert_eq!(p, StartPlan { action: StartAction::Keep, at_ms: now + SOON_MS });
    assert_eq!(on_start(now - OVERDUE_GRACE_MS, now, 0, true, 3).action, StartAction::Keep);
    let old = on_start(now - OVERDUE_GRACE_MS - 1, now, 0, true, 3);
    assert_eq!(old.action, StartAction::Plan);
    assert!(old.at_ms > now + 6 * DAY_MS);
    // Whatever is stored, a started alarm is never in the past.
    for stored in [1, now - 1, now, now + 1, now - 30 * DAY_MS] {
        assert!(on_start(stored, now, 0, true, 9).at_ms >= now);
    }
}

#[test]
fn the_text_table_has_a_text_for_every_counter_state() {
    assert_eq!(ITEM_COUNT, 38);
    assert!((0..ITEM_COUNT).any(|i| item_uses(i) == Some(Nothing)));
    assert_eq!(item_uses(ITEM_COUNT), None);
    assert_eq!(item_uses(usize::MAX), None);
}

#[test]
fn with_no_counters_only_texts_without_a_number_are_picked() {
    for seq in (0..200u32).chain([u32::MAX, u32::MAX - 1]) {
        let p = pick_item(seq, 0, 0);
        assert_eq!(p.uses, Nothing);
        assert_eq!(item_uses(p.index), Some(Nothing));
    }
}

#[test]
fn texts_with_numbers_need_those_numbers() {
    for seq in 0..200u32 {
        let p = pick_item(seq, 5, 0);
        assert!(matches!(p.uses, Nothing | Documents), "{p:?}");
        let p = pick_item(seq, 0, 5);
        assert!(matches!(p.uses, Nothing | Threats), "{p:?}");
        let p = pick_item(seq, 5, 5);
        assert_eq!(item_uses(p.index), Some(p.uses));
    }
}

#[test]
fn picking_walks_forward_and_wraps() {
    // With every counter positive, every text is usable in turn.
    let mut seq = 0u32;
    let mut seen = vec![false; ITEM_COUNT];
    for _ in 0..ITEM_COUNT {
        let p = pick_item(seq, 1, 1);
        assert_eq!(p.index, usize::try_from(seq).unwrap() % ITEM_COUNT);
        seen[p.index] = true;
        seq = u32::try_from(p.index + 1).unwrap();
    }
    assert!(seen.iter().all(|s| *s));
    assert_eq!(pick_item(u32::try_from(ITEM_COUNT).unwrap(), 1, 1).index, 0);
    assert_eq!(pick_item(0, u64::MAX, u64::MAX).index, 0);
}

// ------------------------------------------------------------------------------------------
// The Android table must agree with the Rust table (count, order and placeholders).

const JAVA: &str = include_str!("../../../../android/app/src/main/java/org/arca/viewer/Reminders.java");
const STRINGS_EN: &str = include_str!("../../../../android/app/src/main/res/values/strings.xml");
const STRINGS_RU: &str = include_str!("../../../../android/app/src/main/res/values-ru/strings.xml");

fn java_items() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in JAVA.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("new Item(R.string.") {
            let mut parts = rest.split(", R.string.");
            let title = parts.next().unwrap().to_owned();
            let body = parts.next().unwrap().trim_end_matches("),").to_owned();
            out.push((title, body));
        }
    }
    out
}

fn string_of<'a>(xml: &'a str, name: &str) -> &'a str {
    let key = format!("<string name=\"{name}\">");
    let start = xml.find(&key).unwrap_or_else(|| panic!("missing string {name}")) + key.len();
    let end = xml[start..].find("</string>").unwrap();
    &xml[start..start + end]
}

#[test]
fn the_java_table_matches_the_rust_table() {
    let items = java_items();
    assert_eq!(items.len(), ITEM_COUNT, "Reminders.java and reminder.rs disagree on the count");
    for (i, (title, body)) in items.iter().enumerate() {
        assert!(title.ends_with("_title") && body.ends_with("_body"), "{title} {body}");
        assert_eq!(title.trim_end_matches("_title"), body.trim_end_matches("_body"));
        let uses = item_uses(i).unwrap();
        for xml in [STRINGS_EN, STRINGS_RU] {
            let (t, b) = (string_of(xml, title), string_of(xml, body));
            let one = t.contains("%1$d") || b.contains("%1$d");
            let two = t.contains("%2$d") || b.contains("%2$d");
            let stray = |s: &str| s.replace("%1$d", "").replace("%2$d", "").replace("%%", "").contains('%');
            assert!(!stray(t) && !stray(b), "{title}: unknown format specifier");
            match uses {
                Nothing => assert!(!one && !two, "{title} quotes a number but the table says none"),
                Documents | Threats => assert!(one && !two, "{title}: expected exactly %1$d"),
                Both => assert!(one && two, "{title}: expected %1$d and %2$d"),
            }
        }
    }
}

#[test]
fn the_reminder_texts_carry_no_wallet_address() {
    // The reminder only points at the About screen, where the full address is shown first.
    for (title, body) in java_items() {
        for xml in [STRINGS_EN, STRINGS_RU] {
            for name in [&title, &body] {
                let s = string_of(xml, name);
                assert!(!s.contains("0x") && !s.contains("bc1"), "{name}");
            }
        }
    }
}
