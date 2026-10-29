//! The reminder rules of [`arca_attribution::reminder`] in the byte-and-integer shape that the
//! Android (JNI) and iOS (C) bindings expose. Nothing here keeps state or reads a clock: the
//! platform passes "now" (epoch milliseconds), the UTC offset of its local time zone in
//! milliseconds, and a random number.

use arca_attribution::reminder::{self, FireAction, StartAction};

use crate::json::JsonWriter;

/// A random number from a signed platform integer (the bits are reinterpreted, not clamped).
const fn bits(v: i32) -> u32 {
    u32::from_ne_bytes(v.to_ne_bytes())
}

/// The planned time of the next reminder (see [`reminder::plan_next`]).
#[must_use]
pub fn plan_next(now_ms: i64, offset_ms: i32, rnd: i32) -> i64 {
    reminder::plan_next(now_ms, i64::from(offset_ms), bits(rnd))
}

/// Corrects a planned time for a different UTC offset at that time (see
/// [`reminder::refine_for_offset`]).
#[must_use]
pub fn refine(target_ms: i64, offset_used_ms: i32, offset_at_target_ms: i32) -> i64 {
    reminder::refine_for_offset(
        target_ms,
        i64::from(offset_used_ms),
        i64::from(offset_at_target_ms),
    )
}

/// What to do when the alarm fires, as `{"action":"show","nextAt":123}` with `action` one of
/// `disabled`, `show`, `quietHours`, `tooSoon`, `noPermission` (see [`reminder::on_fire`]).
/// `enabled` and `can_post` are non-zero for true.
#[must_use]
pub fn on_fire_json(
    now_ms: i64,
    offset_ms: i32,
    last_shown_ms: i64,
    enabled: i32,
    can_post: i32,
    rnd: i32,
) -> Vec<u8> {
    let d = reminder::on_fire(
        now_ms,
        i64::from(offset_ms),
        last_shown_ms,
        enabled != 0,
        can_post != 0,
        bits(rnd),
    );
    let name = match d.action {
        FireAction::Disabled => "disabled",
        FireAction::Show => "show",
        FireAction::QuietHours => "quietHours",
        FireAction::TooSoon => "tooSoon",
        FireAction::NoPermission => "noPermission",
    };
    decision_json(name, d.next_at_ms)
}

/// What to do with the stored time when the app starts, as `{"action":"keep","at":123}` with
/// `action` one of `off`, `keep`, `plan` (see [`reminder::on_start`]).
#[must_use]
pub fn on_start_json(next_ms: i64, now_ms: i64, offset_ms: i32, enabled: i32, rnd: i32) -> Vec<u8> {
    let p = reminder::on_start(next_ms, now_ms, i64::from(offset_ms), enabled != 0, bits(rnd));
    let name = match p.action {
        StartAction::Off => "off",
        StartAction::Keep => "keep",
        StartAction::Plan => "plan",
    };
    decision_json(name, p.at_ms)
}

fn decision_json(action: &str, at: i64) -> Vec<u8> {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("action", action);
    w.key("at");
    w.i64(at);
    w.end_object();
    w.finish()
}

/// The index of the text to show for the stored position `seq` (see [`reminder::next_index`]).
#[must_use]
pub fn pick(seq: i32) -> i32 {
    i32::try_from(reminder::next_index(bits(seq))).unwrap_or(0)
}

/// The number of texts the host must have.
#[must_use]
pub fn item_count() -> i32 {
    i32::try_from(reminder::MESSAGE_COUNT).unwrap_or(0)
}

#[cfg(test)]
mod tests;
