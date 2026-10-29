//! Rules of the optional local support reminder, shared by every platform.
//!
//! The host (Android today, iOS later) only owns the operating-system calls: it arms one inexact
//! alarm or notification trigger, posts the notification, and asks this module *when* and
//! *whether*. Everything here is a pure function of the arguments: no clock is read (the host
//! passes "now" in epoch milliseconds and the UTC offset of its local time zone), no randomness
//! is drawn (the host passes a random `u32`), nothing is stored and nothing is sent anywhere.
//!
//! # The rules
//!
//! * A reminder is planned 7, 8 or 9 local days after "now" at a random minute between 12:00 and
//!   17:59 local time ([`plan_next`]).
//! * A reminder may be shown only between 11:00 (inclusive) and 20:30 (exclusive) local time. An
//!   alarm that fires outside of that window (the system delays inexact alarms; the time zone or
//!   daylight saving time may have changed since planning) is moved to 12:30 of the same or the
//!   next local day ([`on_fire`]).
//! * Two reminders are never closer than seven days: a stray extra alarm is dropped and replanned.
//!   A "last shown" time in the future (the clock was set backwards) is ignored. The host keeps
//!   only the *day* of the last reminder, never a time of day.
//! * The chain never dies silently: after a reminder that could not be posted because the
//!   permission is missing, the next one is planned all the same (the person may grant the
//!   permission in the system settings at any time).
//! * Alarms do not survive a reboot and the app holds no boot permission, so the host calls
//!   [`on_start`] whenever the app starts: it keeps a plausible pending time, runs a reminder
//!   that is only slightly overdue soon, and replans one that is long overdue, lies absurdly far
//!   in the future (clock set backwards) or never existed.
//! * Which text is shown is chosen by [`next_index`]: the host keeps one small position and the
//!   texts go round in order. No text quotes a counter, so nothing about the person's use of the
//!   app is needed to choose one.
//!
//! All arithmetic saturates and every input is clamped, so no argument can panic or overflow.

/// Milliseconds in a day.
pub const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;

/// Shortest planned gap in local days.
pub const MIN_GAP_DAYS: i64 = 7;
/// Longest planned gap in local days.
pub const MAX_GAP_DAYS: i64 = 9;
/// First minute of the local day (12:00) at which a reminder is planned.
pub const PLAN_FROM_MINUTE: i64 = 12 * 60;
/// Number of minutes from [`PLAN_FROM_MINUTE`] at which a reminder may be planned (until 17:59).
pub const PLAN_SPAN_MINUTES: u32 = 6 * 60;
/// The earliest local minute (11:00) at which a reminder may be shown.
pub const POST_FROM_MINUTE: i64 = 11 * 60;
/// The local minute (20:30) from which a reminder may no longer be shown.
pub const POST_UNTIL_MINUTE: i64 = 20 * 60 + 30;
/// Local minute (12:30) to which a reminder is moved when it fires outside the window.
pub const RETRY_MINUTE: i64 = 12 * 60 + 30;
/// Two shown reminders are at least this far apart.
pub const MIN_SPACING_MS: i64 = 7 * DAY_MS;
/// An alarm overdue by no more than this is still run (soon) when the app starts.
pub const OVERDUE_GRACE_MS: i64 = 12 * HOUR_MS;
/// "Soon" for an overdue alarm that is still run.
pub const SOON_MS: i64 = MINUTE_MS;

/// Time zones lie within UTC-12 … UTC+14; anything beyond ±18 h is a host error and clamped.
const MAX_OFFSET_MS: i64 = 18 * HOUR_MS;
/// Far enough from `i64::MAX` that no sum of the constants above overflows.
const TIME_CAP: i64 = i64::MAX / 4;

fn offset(ms: i64) -> i64 {
    ms.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS)
}

fn instant(ms: i64) -> i64 {
    ms.clamp(0, TIME_CAP)
}

/// Minute of the local day (0 … 1439) at `now_ms` in a zone `offset_ms` ahead of UTC.
#[must_use]
pub fn local_minute(now_ms: i64, offset_ms: i64) -> i64 {
    instant(now_ms).saturating_add(offset(offset_ms)).rem_euclid(DAY_MS) / MINUTE_MS
}

/// The UTC instant of the local midnight that starts the local day containing `now_ms`.
fn local_midnight(now_ms: i64, offset_ms: i64) -> i64 {
    let off = offset(offset_ms);
    let local = instant(now_ms).saturating_add(off);
    local.div_euclid(DAY_MS).saturating_mul(DAY_MS).saturating_sub(off)
}

fn at(midnight: i64, days: i64, minute: i64) -> i64 {
    midnight
        .saturating_add(days.saturating_mul(DAY_MS))
        .saturating_add(minute.saturating_mul(MINUTE_MS))
}

/// The time of the next reminder: 7 to 9 local days after `now_ms`, between 12:00 and 17:59.
///
/// `rnd` is any random number from the host. The result is always more than six days after
/// `now_ms`. It is computed with the UTC offset of "now"; when the offset at the planned time
/// differs (daylight saving time), the host can correct it with [`refine_for_offset`] — and the
/// window check in [`on_fire`] absorbs whatever remains.
#[must_use]
pub fn plan_next(now_ms: i64, offset_ms: i64, rnd: u32) -> i64 {
    let days = MIN_GAP_DAYS.saturating_add(i64::from(rnd % 3));
    let minute = PLAN_FROM_MINUTE.saturating_add(i64::from((rnd / 3) % PLAN_SPAN_MINUTES));
    at(local_midnight(now_ms, offset_ms), days, minute)
}

/// Moves a planned instant to keep its local wall-clock time when the offset at that instant
/// (`offset_at_target_ms`) differs from the one it was planned with (`offset_used_ms`).
#[must_use]
pub fn refine_for_offset(target_ms: i64, offset_used_ms: i64, offset_at_target_ms: i64) -> i64 {
    instant(
        target_ms
            .saturating_add(offset(offset_used_ms))
            .saturating_sub(offset(offset_at_target_ms)),
    )
}

/// What to do when the alarm fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireAction {
    /// Reminders are off: cancel everything, plan nothing.
    Disabled = 0,
    /// Post the notification now and arm the alarm for `next_at_ms`.
    Show = 1,
    /// Outside of the allowed hours: post nothing, arm the alarm for `next_at_ms` (12:30).
    QuietHours = 2,
    /// The previous reminder is too recent: post nothing, arm the alarm for `next_at_ms`.
    TooSoon = 3,
    /// Notifications are not allowed: post nothing, arm the alarm for `next_at_ms`.
    NoPermission = 4,
}

/// The verdict of [`on_fire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireDecision {
    /// What to do now.
    pub action: FireAction,
    /// When to arm the next alarm (epoch milliseconds); `0` for [`FireAction::Disabled`].
    pub next_at_ms: i64,
}

/// Decides what to do when the reminder alarm fires.
///
/// `last_shown_ms` is the time the previous reminder was shown (`0` if none); `enabled` is the
/// person's switch; `can_post` is whether the system lets the app post (runtime permission on
/// Android 13+, notifications not blocked otherwise).
#[must_use]
pub fn on_fire(
    now_ms: i64,
    offset_ms: i64,
    last_shown_ms: i64,
    enabled: bool,
    can_post: bool,
    rnd: u32,
) -> FireDecision {
    let now = instant(now_ms);
    let planned = plan_next(now, offset_ms, rnd);
    if !enabled {
        return FireDecision { action: FireAction::Disabled, next_at_ms: 0 };
    }
    if !can_post {
        return FireDecision { action: FireAction::NoPermission, next_at_ms: planned };
    }
    let minute = local_minute(now, offset_ms);
    let midnight = local_midnight(now, offset_ms);
    if minute < POST_FROM_MINUTE {
        return FireDecision {
            action: FireAction::QuietHours,
            next_at_ms: at(midnight, 0, RETRY_MINUTE),
        };
    }
    if minute >= POST_UNTIL_MINUTE {
        return FireDecision {
            action: FireAction::QuietHours,
            next_at_ms: at(midnight, 1, RETRY_MINUTE),
        };
    }
    // A last-shown time of zero or from the future (clock set backwards) does not count.
    if last_shown_ms > 0 && last_shown_ms <= now && now - last_shown_ms < MIN_SPACING_MS {
        return FireDecision { action: FireAction::TooSoon, next_at_ms: planned };
    }
    FireDecision { action: FireAction::Show, next_at_ms: planned }
}

/// What to do with the stored reminder time when the app starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAction {
    /// Reminders are off: cancel any pending alarm.
    Off = 0,
    /// Arm the alarm for `at_ms` (the stored time, or "soon" when it is slightly overdue).
    Keep = 1,
    /// Arm the alarm for the freshly planned `at_ms` and store it.
    Plan = 2,
}

/// The verdict of [`on_start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartPlan {
    /// What to do.
    pub action: StartAction,
    /// When to arm the alarm (epoch milliseconds); `0` for [`StartAction::Off`].
    pub at_ms: i64,
}

/// Decides what to do with the stored `next_ms` (`0` if none) when the app starts.
#[must_use]
pub fn on_start(next_ms: i64, now_ms: i64, offset_ms: i64, enabled: bool, rnd: u32) -> StartPlan {
    if !enabled {
        return StartPlan { action: StartAction::Off, at_ms: 0 };
    }
    let now = instant(now_ms);
    let plan = StartPlan { action: StartAction::Plan, at_ms: plan_next(now, offset_ms, rnd) };
    if next_ms <= 0 {
        return plan;
    }
    if next_ms >= now {
        // Further away than the longest planned gap plus a day: the clock went backwards since
        // planning, or the stored value is damaged.
        let horizon = MAX_GAP_DAYS.saturating_add(1).saturating_mul(DAY_MS);
        return if next_ms - now > horizon {
            plan
        } else {
            StartPlan { action: StartAction::Keep, at_ms: next_ms }
        };
    }
    if now - next_ms <= OVERDUE_GRACE_MS {
        StartPlan { action: StartAction::Keep, at_ms: now.saturating_add(SOON_MS) }
    } else {
        plan
    }
}

/// Number of reminder texts the host must have, in every language it ships.
pub const MESSAGE_COUNT: usize = 30;

/// The text to show for the stored position `seq`: the texts go round in order. The host stores
/// `index + 1` as the next position.
#[must_use]
pub fn next_index(seq: u32) -> usize {
    usize::try_from(seq).map_or(0, |s| s % MESSAGE_COUNT)
}

#[cfg(test)]
mod tests;
