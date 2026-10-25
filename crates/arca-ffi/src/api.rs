// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The safe API layer behind the C ABI entry points.
//!
//! Conventions:
//! * handles are positive `i64`; a negative return value from a handle-producing
//!   function is `-status` (see [`arca_engine::Status`]);
//! * functions returning data return typed `Result<Vec<u8>, Status>` or `Option<Vec<u8>>`;
//! * nothing here panics on invalid arguments.

use std::io::{Read, Write};
use std::sync::OnceLock;
use std::time::Duration;

use arca_engine::render::{self, lang_from_code};
use arca_engine::{Engine, EngineConfig, Handle, Host, Status, schedule, selftest, stats};

mod guest {
    include!(concat!(env!("OUT_DIR"), "/guest.rs"));
}

pub use guest::GUEST_SHA256;

static HOST: OnceLock<Host> = OnceLock::new();

/// `true` if a guest module was embedded at build time.
#[must_use]
pub fn has_guest() -> bool {
    !guest::GUEST_WASM.is_empty()
}

/// `true` if the engine has been successfully initialised.
#[must_use]
pub fn is_init() -> bool {
    HOST.get().is_some()
}

/// Initialises the engine (once per process; later calls return the first result's state).
///
/// Limits come from the device: input in MiB, memory in MiB, scan and preview time in
/// seconds. Out-of-range values are clamped by the engine, never rejected.
#[must_use]
pub fn init(max_input_mib: i32, max_memory_mib: i32, scan_secs: i32, preview_secs: i32) -> i32 {
    if HOST.get().is_some() {
        return Status::Ok.code();
    }
    if !has_guest() {
        return Status::NotReady.code();
    }
    let secs = |v: i32, default: u64| u64::try_from(v).ok().filter(|v| *v > 0).unwrap_or(default);
    let d = EngineConfig::default();
    let config = EngineConfig {
        max_input_bytes: mib_to_bytes(max_input_mib, d.max_input_bytes),
        max_memory_bytes: mib_to_bytes(max_memory_mib, d.max_memory_bytes),
        scan_timeout: Duration::from_secs(secs(scan_secs, d.scan_timeout.as_secs())),
        preview_timeout: Duration::from_secs(secs(preview_secs, d.preview_timeout.as_secs())),
    };
    // The module must be the one this library was built with: its digest is a build-time
    // constant, not something computed from the bytes about to be trusted.
    let engine = match Engine::pinned(guest::GUEST_WASM, GUEST_SHA256, config) {
        Ok(engine) => engine,
        Err(status) => return status.code(),
    };
    // A concurrent first call may win; both built the same engine.
    let _ = HOST.set(Host::new(engine));
    Status::Ok.code()
}

/// `v` MiB in bytes; `default` for zero or a negative value. A figure that does not fit
/// `usize` (a 32-bit phone asked for 4 GiB) saturates, and the engine clamps it to its ceiling.
pub(crate) fn mib_to_bytes(v: i32, default: usize) -> usize {
    usize::try_from(v).ok().filter(|v| *v > 0).map_or(default, |v| v.saturating_mul(1 << 20))
}

fn host() -> Result<&'static Host, Status> {
    #[cfg(test)]
    if let Some(h) = test_host::current() {
        return Ok(h);
    }
    HOST.get().ok_or(Status::NotReady)
}

fn handle(raw: i64) -> Result<Handle, Status> {
    u64::try_from(raw).ok().filter(|h| *h > 0).ok_or(Status::BadHandle)
}

fn neg(status: Status) -> i64 {
    -i64::from(status.code())
}

fn code<T>(r: Result<T, Status>) -> i32 {
    match r {
        Ok(_) => Status::Ok.code(),
        Err(s) => s.code(),
    }
}

fn handle_or_status(r: Result<Handle, Status>) -> i64 {
    match r {
        Ok(h) => i64::try_from(h).unwrap_or_else(|_| neg(Status::Fault)),
        Err(s) => neg(s),
    }
}

fn count_or_status(r: Result<u64, Status>) -> i64 {
    match r {
        Ok(n) => i64::try_from(n).unwrap_or_else(|_| neg(Status::Fault)),
        Err(s) => neg(s),
    }
}

/// Facts about the engine and the guest as JSON.
#[must_use]
pub fn info() -> Option<Vec<u8>> {
    host().ok().map(Host::info_json)
}

/// Runs the built-in self-test and returns its JSON report.
#[must_use]
pub fn self_test() -> Option<Vec<u8>> {
    host().ok().map(selftest::run)
}

/// Starts an empty document accepting at most `max_bytes`.
#[must_use]
pub fn doc_new(max_bytes: i64) -> i64 {
    let Ok(max) = u64::try_from(max_bytes) else {
        return neg(Status::BadArgument);
    };
    match host() {
        Ok(h) => handle_or_status(Ok(h.doc_new(max))),
        Err(s) => neg(s),
    }
}

/// Appends `data` to the document.
#[must_use]
pub fn doc_append(doc: i64, data: &[u8]) -> i32 {
    code(host().and_then(|h| h.doc_append(handle(doc)?, data)))
}

/// Remembers the password (UTF-8) for the next runs over the document; empty forgets it.
#[must_use]
pub fn doc_set_password(doc: i64, password: &[u8]) -> i32 {
    code(host().and_then(|h| h.doc_set_password(handle(doc)?, password)))
}

/// Reads `reader` to its end into the document; returns the byte count or `-status`.
pub fn doc_read(doc: i64, reader: &mut dyn Read) -> i64 {
    count_or_status(host().and_then(|h| h.doc_read_from(handle(doc)?, reader)))
}

/// Length of the document so far, or `-status`.
#[must_use]
pub fn doc_len(doc: i64) -> i64 {
    count_or_status(host().and_then(|h| h.doc_len(handle(doc)?)))
}

/// Releases the document.
pub fn doc_free(doc: i64) {
    if let (Ok(h), Ok(d)) = (host(), handle(doc)) {
        h.doc_free(d);
    }
}

/// A new cancellation token; the handle or `-status`.
#[must_use]
pub fn job_new() -> i64 {
    match host() {
        Ok(h) => handle_or_status(Ok(h.job_new())),
        Err(s) => neg(s),
    }
}

/// Asks the job's run to stop.
pub fn job_cancel(job: i64) {
    if let (Ok(h), Ok(j)) = (host(), handle(job)) {
        h.job_cancel(j);
    }
}

/// Releases the job.
pub fn job_free(job: i64) {
    if let (Ok(h), Ok(j)) = (host(), handle(job)) {
        h.job_free(j);
    }
}

/// Runs an operation (blocking); returns the outcome handle or `-status`.
#[must_use]
pub fn run(job: i64, doc: i64, op: i32, flags: i32) -> i64 {
    let (Ok(op), Ok(flags)) = (u16::try_from(op), u32::try_from(flags)) else {
        return neg(Status::BadArgument);
    };
    handle_or_status(host().and_then(|h| h.run(handle(job)?, handle(doc)?, op, flags)))
}

/// Status code of a finished run (a negative `-status` for a bad handle).
#[must_use]
pub fn outcome_status(outcome: i64) -> i32 {
    match host().and_then(|h| h.outcome_status(handle(outcome)?)) {
        Ok(s) => s.code(),
        Err(s) => -s.code(),
    }
}

/// Diagnostics of a finished run as JSON.
pub fn outcome_info(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.outcome_info_json(handle(outcome)?)
}

/// The X-ray report as JSON.
pub fn report(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.report_json(handle(outcome)?)
}

/// Preview sections, notices and totals as JSON.
pub fn preview_meta(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.preview_meta_json(handle(outcome)?)
}

/// The drawing of a page (from a render run) as JSON.
pub fn page(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.page_json(handle(outcome)?)
}

/// Facts about a picture (from an image run) as JSON.
pub fn image_info(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.image_info_json(handle(outcome)?)
}

/// The pixels of a picture: four bytes (red, green, blue, alpha; premultiplied) per pixel.
pub fn image_pixels(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.image_pixels(handle(outcome)?)
}

/// A page of preview items as a JSON array.
pub fn preview_items(outcome: i64, section: i32, from: i64, count: i32) -> Result<Vec<u8>, Status> {
    let (Ok(section), Ok(from), Ok(count)) =
        (u32::try_from(section), u64::try_from(from), u32::try_from(count))
    else {
        return Err(Status::BadArgument);
    };
    host()?.preview_items_json(handle(outcome)?, section, from, count)
}

/// Description of a sterile copy as JSON.
pub fn sanitized_info(outcome: i64) -> Result<Vec<u8>, Status> {
    host()?.sanitized_info_json(handle(outcome)?)
}

/// Size in bytes of a sterile copy.
pub fn sanitized_len(outcome: i64) -> Result<u64, Status> {
    host()?.sanitized_len(handle(outcome)?)
}

/// The bytes of a sterile copy.
pub fn sanitized_bytes(outcome: i64) -> Result<Vec<u8>, Status> {
    let h = host()?;
    let o = handle(outcome)?;
    let mut buf = Vec::new();
    h.sanitized_write_to(o, &mut buf)?;
    Ok(buf)
}

/// Writes the sterile copy to `out`; returns the byte count or `-status`.
pub fn sanitized_write(outcome: i64, out: &mut dyn Write) -> i64 {
    count_or_status(host().and_then(|h| h.sanitized_write_to(handle(outcome)?, out)))
}

/// Releases a stored outcome.
pub fn outcome_free(outcome: i64) {
    if let (Ok(h), Ok(o)) = (host(), handle(outcome)) {
        h.outcome_free(o);
    }
}

/// The "About & Support" screen as JSON (`lang` 0 = Russian, otherwise English).
#[must_use]
pub fn about(lang: i32) -> Vec<u8> {
    render::about_json(lang_from_code(u32::try_from(lang).unwrap_or(1)))
}

/// The Patron Passport as JSON for the stored 36-byte `record` and the day number `today`.
#[must_use]
pub fn passport(lang: i32, record: &[u8], today: i32) -> Vec<u8> {
    render::passport_json(
        lang_from_code(u32::try_from(lang).unwrap_or(1)),
        record,
        u32::try_from(today).unwrap_or(0),
    )
}

/// The Patron Passport record after one more opened document.
#[must_use]
pub fn passport_update(record: &[u8], today: i32, trackers: i32, active: i32) -> Vec<u8> {
    render::passport_update(
        record,
        u32::try_from(today).unwrap_or(0),
        u32::try_from(trackers).unwrap_or(0),
        u32::try_from(active).unwrap_or(0),
    )
}

// ---- local statistics and reminder rules (see arca_engine::stats / schedule) ----

/// The statistics blob after one scan (`verdict` is the wire tag of the X-ray verdict).
#[must_use]
pub fn stats_record_scan(blob: &[u8], day: i32, verdict: i32, trackers: i32, active: i32) -> Vec<u8> {
    stats::blob_record_scan(blob, day, verdict, trackers, active)
}

/// The statistics blob after a sterile copy (`1`) or an opened picture (`2`).
#[must_use]
pub fn stats_record_event(blob: &[u8], day: i32, event: i32) -> Vec<u8> {
    stats::blob_record_event(blob, day, event)
}

/// The week, month and all-time totals of the blob as JSON.
#[must_use]
pub fn stats_summary(blob: &[u8], today: i32) -> Vec<u8> {
    stats::blob_summary_json(blob, today)
}

/// The blob with the counters of the older app versions folded into the all-time total.
#[must_use]
pub fn stats_fold_legacy(blob: &[u8], passport: &[u8], documents: i64, threats: i64, today: i32) -> Vec<u8> {
    stats::blob_fold_legacy(blob, passport, documents, threats, today)
}

/// The Patron Passport as JSON, built from the statistics blob.
#[must_use]
pub fn stats_passport(lang: i32, blob: &[u8], today: i32) -> Vec<u8> {
    stats::blob_passport_json(lang, blob, today)
}

/// The planned time of the next reminder (epoch milliseconds).
#[must_use]
pub fn reminder_plan(now_ms: i64, offset_ms: i32, rnd: i32) -> i64 {
    schedule::plan_next(now_ms, offset_ms, rnd)
}

/// A planned time corrected for the UTC offset at that time.
#[must_use]
pub fn reminder_refine(target_ms: i64, offset_used_ms: i32, offset_at_target_ms: i32) -> i64 {
    schedule::refine(target_ms, offset_used_ms, offset_at_target_ms)
}

/// What to do when the reminder alarm fires, as JSON.
#[must_use]
pub fn reminder_on_fire(
    now_ms: i64,
    offset_ms: i32,
    last_shown_ms: i64,
    enabled: i32,
    can_post: i32,
    rnd: i32,
) -> Vec<u8> {
    schedule::on_fire_json(now_ms, offset_ms, last_shown_ms, enabled, can_post, rnd)
}

/// What to do with the stored reminder time when the app starts, as JSON.
#[must_use]
pub fn reminder_on_start(next_ms: i64, now_ms: i64, offset_ms: i32, enabled: i32, rnd: i32) -> Vec<u8> {
    schedule::on_start_json(next_ms, now_ms, offset_ms, enabled, rnd)
}

/// The reminder text to show: `index | (uses << 8)`.
#[must_use]
pub fn reminder_pick(seq: i32, documents: i64, threats: i64) -> i32 {
    schedule::pick(seq, documents, threats)
}

/// The number of reminder texts the app must have.
#[must_use]
pub fn reminder_item_count() -> i32 {
    schedule::item_count()
}

/// The extra support addresses (wallets, Boosty) as JSON; an empty list until configured.
#[must_use]
pub fn extra_support() -> Vec<u8> {
    arca_engine::support::extra_support_json()
}

/// A licence text (see [`render::license_text`]).
#[must_use]
pub fn license_text(which: i32) -> Option<Vec<u8>> {
    u32::try_from(which).ok().and_then(render::license_text).map(|t| t.as_bytes().to_vec())
}

/// Release readiness of this build (placeholder URLs etc.) as JSON.
#[must_use]
pub fn release_info() -> Vec<u8> {
    render::release_blockers_json()
}

/// A per-thread host for tests: the handle tables work without a guest module, so the
/// tests of handle validation neither depend on nor change the process-wide host.
#[cfg(test)]
pub(crate) mod test_host {
    use std::cell::Cell;

    use super::{Engine, EngineConfig, Host};

    thread_local! {
        static CURRENT: Cell<Option<&'static Host>> = const { Cell::new(None) };
    }

    pub(crate) fn current() -> Option<&'static Host> {
        CURRENT.with(Cell::get)
    }

    /// Installs a fresh host (over an empty module, which is never run) for this thread.
    pub(crate) fn install() {
        // Leaked on purpose: a test host lives for the rest of the (test) process.
        let host: &'static Host =
            Box::leak(Box::new(Host::new(Engine::new(Vec::<u8>::new(), EngineConfig::default()))));
        CURRENT.with(|c| c.set(Some(host)));
    }
}
