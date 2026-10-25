//! Local usage statistics: what Arca did for the person, kept on the device and nowhere else.
//!
//! The statistics are a bounded ring of daily buckets (the last [`KEEP_DAYS`] days) plus an
//! all-time total. Nothing about a document is ever recorded: no name, no content, no hash, no
//! time of day; only counters of what was done, per day. The core has no clock: the platform
//! passes the day number (days since 1970-01-01 in its *local* calendar) with every call, and
//! stores the opaque byte blob of [`Stats::encode`] in its private storage. The blob is
//! versioned and protected by a CRC-32; [`Stats::decode`] accepts hostile bytes (any length,
//! any content) without panicking or allocating more than a few tens of kilobytes, and answers
//! with empty statistics for anything it does not understand.
//!
//! # How a result is counted (the one place)
//!
//! [`counts_for_scan`] is the only function that maps a finished scan to counters:
//!
//! | verdict                    | opened | clean | rejected | unverified | trackers          | active          |
//! |----------------------------|:------:|:-----:|:--------:|:----------:|-------------------|-----------------|
//! | `Clean`                    |   1    |   1   |          |            |                   |                 |
//! | `HiddenTrackers`           |   1    |       |          |            | `max(findings, 1)`| findings        |
//! | `ActiveContentNeutralized` |   1    |       |          |            | findings          | `max(findings,1)`|
//! | `Unverified`               |   1    |       |          |     1      | findings          | findings        |
//! | `Rejected`                 |        |       |    1     |            |                   |                 |
//!
//! "Findings" are the engine's counts of remote-content (`trackers`) and active-content
//! (`active`) findings of medium severity or higher. A rejected file is not opened. A verdict
//! code this build does not know counts as `Unverified` (never as clean). Sterile copies and
//! opened pictures are separate events.
//!
//! # Days
//!
//! A day number outside the sane range (before 2019 or after the year 9999) is clamped into it.
//! An event dated *before* the newest bucket (the clock was set back, or the person travelled
//! west across the date line) is added to the newest bucket: buckets stay strictly ordered, and
//! a window never loses it. Buckets older than [`KEEP_DAYS`] days are dropped when a newer day
//! is recorded; the all-time total keeps counting.

use arca_attribution::PatronStats;
use arca_xray::Verdict;

use crate::json::JsonWriter;
use crate::render;

/// Number of days of daily buckets that are kept.
pub const KEEP_DAYS: u32 = 400;
/// Largest encoded blob; a bigger input is not a blob of this module.
pub const MAX_BLOB_LEN: usize = 48 * 1024;
/// 2019-01-01 as a day number: no day recorded by this app can be earlier.
pub const MIN_DAY: u32 = 17_897;
/// 9999-12-31 as a day number.
pub const MAX_DAY: u32 = 2_932_896;

const MAGIC: [u8; 3] = *b"AST";
const VERSION: u8 = 1;
/// Number of counters in a [`Counts`].
const FIELDS: usize = 8;
/// What the JSON of a counter may reach: every consumer reads it as a signed 64-bit number.
const JSON_MAX: u64 = u64::MAX >> 1;

/// The counters of one period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Documents opened (every scanned file that was not rejected).
    pub opened: u64,
    /// Documents in which nothing was found and every part was inspected.
    pub clean: u64,
    /// Trackers and external references neutralised.
    pub trackers: u64,
    /// Active and dangerous content neutralised (macros, DDE, OLE, remote templates, scripts).
    pub active: u64,
    /// Files rejected as hostile or not a document.
    pub rejected: u64,
    /// Documents that could not be verified completely.
    pub unverified: u64,
    /// Sterile copies made.
    pub sterile: u64,
    /// Pictures opened.
    pub pictures: u64,
}

impl Counts {
    fn as_array(&self) -> [u64; FIELDS] {
        [
            self.opened,
            self.clean,
            self.trackers,
            self.active,
            self.rejected,
            self.unverified,
            self.sterile,
            self.pictures,
        ]
    }

    fn from_array(a: [u64; FIELDS]) -> Self {
        let [opened, clean, trackers, active, rejected, unverified, sterile, pictures] = a;
        Self { opened, clean, trackers, active, rejected, unverified, sterile, pictures }
    }

    /// Adds `other`; every counter saturates, none wraps.
    pub fn add(&mut self, other: &Self) {
        self.opened = self.opened.saturating_add(other.opened);
        self.clean = self.clean.saturating_add(other.clean);
        self.trackers = self.trackers.saturating_add(other.trackers);
        self.active = self.active.saturating_add(other.active);
        self.rejected = self.rejected.saturating_add(other.rejected);
        self.unverified = self.unverified.saturating_add(other.unverified);
        self.sterile = self.sterile.saturating_add(other.sterile);
        self.pictures = self.pictures.saturating_add(other.pictures);
    }

    /// Trackers plus active content: everything dangerous that was neutralised.
    #[must_use]
    pub const fn threats(&self) -> u64 {
        self.trackers.saturating_add(self.active)
    }

    /// `true` if every counter is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.as_array().iter().all(|v| *v == 0)
    }

    fn raise_to(&mut self, other: &Self) {
        let (a, b) = (self.as_array(), other.as_array());
        let mut out = [0u64; FIELDS];
        for ((slot, x), y) in out.iter_mut().zip(a).zip(b) {
            *slot = x.max(y);
        }
        *self = Self::from_array(out);
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A document was scanned. `trackers` and `active` are the engine's counts of findings.
    Scan {
        /// The verdict of the X-ray.
        verdict: Verdict,
        /// Remote-content findings of medium severity or higher.
        trackers: u32,
        /// Active-content findings of medium severity or higher.
        active: u32,
    },
    /// A sterile copy was made.
    SterileCopy,
    /// A picture was opened.
    PictureOpened,
}

/// Event code of a sterile copy in the platform interfaces.
pub const EVENT_STERILE_COPY: i32 = 1;
/// Event code of an opened picture in the platform interfaces.
pub const EVENT_PICTURE_OPENED: i32 = 2;

/// The verdict for a platform code (the wire tags of the engine's `Verdict`: 0 clean … 4
/// unverified). Anything unknown is `Unverified`, never `Clean`.
#[must_use]
pub const fn verdict_from_code(code: i32) -> Verdict {
    match code {
        0 => Verdict::Clean,
        1 => Verdict::HiddenTrackers,
        2 => Verdict::ActiveContentNeutralized,
        3 => Verdict::Rejected,
        _ => Verdict::Unverified,
    }
}

/// The platform code of a verdict (inverse of [`verdict_from_code`]).
#[must_use]
pub const fn verdict_code(verdict: Verdict) -> i32 {
    match verdict {
        Verdict::Clean => 0,
        Verdict::HiddenTrackers => 1,
        Verdict::ActiveContentNeutralized => 2,
        Verdict::Rejected => 3,
        Verdict::Unverified => 4,
    }
}

/// The counters of one scan: the single mapping from a verdict to the statistics (see the
/// module documentation for the table).
#[must_use]
pub fn counts_for_scan(verdict: Verdict, trackers: u32, active: u32) -> Counts {
    let (t, a) = (u64::from(trackers), u64::from(active));
    let mut c = Counts::default();
    match verdict {
        Verdict::Clean => {
            c.opened = 1;
            c.clean = 1;
        }
        Verdict::HiddenTrackers => {
            c.opened = 1;
            c.trackers = t.max(1);
            c.active = a;
        }
        Verdict::ActiveContentNeutralized => {
            c.opened = 1;
            c.trackers = t;
            c.active = a.max(1);
        }
        Verdict::Unverified => {
            c.opened = 1;
            c.unverified = 1;
            c.trackers = t;
            c.active = a;
        }
        Verdict::Rejected => c.rejected = 1,
    }
    c
}

impl Event {
    /// The counters this event adds.
    #[must_use]
    pub fn counts(&self) -> Counts {
        match *self {
            Self::Scan { verdict, trackers, active } => counts_for_scan(verdict, trackers, active),
            Self::SterileCopy => Counts { sterile: 1, ..Counts::default() },
            Self::PictureOpened => Counts { pictures: 1, ..Counts::default() },
        }
    }
}

/// The period a total covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The last 7 days including today.
    Week,
    /// The last 30 days including today.
    Month,
    /// Everything since the statistics started.
    All,
}

impl Window {
    /// From a platform code: `7`, `30`; anything else is all time.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            7 => Self::Week,
            30 => Self::Month,
            _ => Self::All,
        }
    }

    const fn days(self) -> Option<u32> {
        match self {
            Self::Week => Some(7),
            Self::Month => Some(30),
            Self::All => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bucket {
    day: u32,
    counts: Counts,
}

/// The statistics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stats {
    first_day: Option<u32>,
    all: Counts,
    /// Strictly ascending by day, at most [`KEEP_DAYS`] entries.
    days: Vec<Bucket>,
}

/// Why [`Stats::try_decode`] refused a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Longer than [`MAX_BLOB_LEN`].
    TooLarge,
    /// Ends early, or has bytes left over.
    Malformed,
    /// Not a statistics blob.
    BadMagic,
    /// A version this build does not know.
    UnknownVersion,
    /// The checksum does not match.
    BadChecksum,
}

impl Stats {
    /// Empty statistics.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `true` if nothing was ever recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all.is_zero() && self.days.is_empty()
    }

    /// The day of the first recorded event, if any.
    #[must_use]
    pub const fn first_day(&self) -> Option<u32> {
        self.first_day
    }

    /// Number of daily buckets held (at most [`KEEP_DAYS`]).
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.days.len()
    }

    /// Records `event` on `day` (days since 1970-01-01 in the platform's local calendar).
    pub fn record(&mut self, day: u32, event: Event) {
        self.add_counts(day, &event.counts());
    }

    fn add_counts(&mut self, day: u32, counts: &Counts) {
        let day = day.clamp(MIN_DAY, MAX_DAY);
        self.all.add(counts);
        self.first_day = Some(self.first_day.map_or(day, |f| f.min(day)));
        match self.days.last_mut() {
            Some(last) if last.day >= day => last.counts.add(counts),
            _ => {
                self.days.push(Bucket { day, counts: *counts });
                self.prune(day);
            }
        }
    }

    /// Drops buckets that are [`KEEP_DAYS`] or more days older than `newest`, and the excess.
    fn prune(&mut self, newest: u32) {
        let old = self.days.iter().take_while(|b| newest.saturating_sub(b.day) >= KEEP_DAYS).count();
        let excess = self.days.len().saturating_sub(usize::try_from(KEEP_DAYS).unwrap_or(400));
        let drop = old.max(excess).min(self.days.len());
        self.days.drain(..drop);
    }

    /// Adds counters carried over from an older version of the app to the all-time total only
    /// (they have no days), and moves the first day back to `first_day` if it is earlier.
    pub fn fold_legacy(&mut self, counts: &Counts, first_day: Option<u32>) {
        self.all.add(counts);
        if let Some(d) = first_day {
            let d = d.clamp(MIN_DAY, MAX_DAY);
            self.first_day = Some(self.first_day.map_or(d, |f| f.min(d)));
        }
    }

    /// The totals of `window` as seen on day `today`. A bucket dated after `today` (the clock
    /// went back) belongs to every window.
    #[must_use]
    pub fn totals(&self, window: Window, today: u32) -> Counts {
        let Some(span) = window.days() else { return self.all };
        let mut sum = Counts::default();
        for b in &self.days {
            if today.saturating_sub(b.day) < span {
                sum.add(&b.counts);
            }
        }
        sum
    }

    /// Days of use up to and including `today` (at least 1).
    #[must_use]
    pub fn days_in_use(&self, today: u32) -> u32 {
        self.first_day.map_or(1, |f| today.saturating_sub(f).saturating_add(1))
    }

    /// The compact, versioned, CRC-protected blob: `"AST"`, version, first day + 1 (0 = none),
    /// the eight all-time counters, the number of buckets, then per bucket the day (the first
    /// absolute, the others as the difference to the previous) and its eight counters (all
    /// LEB128 varints), then the CRC-32 of everything before it (little-endian).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.days.len() * 12);
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        put_varint(&mut out, self.first_day.map_or(0, |d| u64::from(d) + 1));
        for v in self.all.as_array() {
            put_varint(&mut out, v);
        }
        put_varint(&mut out, u64::try_from(self.days.len()).unwrap_or(0));
        let mut prev = 0u32;
        for b in &self.days {
            put_varint(&mut out, u64::from(b.day.saturating_sub(prev)));
            prev = b.day;
            for v in b.counts.as_array() {
                put_varint(&mut out, v);
            }
        }
        let crc = arca_digest::crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decodes [`Self::encode`] output; empty statistics for anything else.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Self {
        Self::try_decode(bytes).unwrap_or_default()
    }

    /// Strict decoding.
    ///
    /// # Errors
    /// [`DecodeError`] for a blob that is too large, of another kind or version, damaged, or
    /// not well formed. Memory use is bounded by [`MAX_BLOB_LEN`] and [`KEEP_DAYS`].
    pub fn try_decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() > MAX_BLOB_LEN {
            return Err(DecodeError::TooLarge);
        }
        if bytes.get(..3) != Some(&MAGIC[..]) {
            return Err(DecodeError::BadMagic);
        }
        if bytes.get(3) != Some(&VERSION) {
            return Err(if bytes.len() > 3 { DecodeError::UnknownVersion } else { DecodeError::Malformed });
        }
        let split = bytes.len().checked_sub(4).ok_or(DecodeError::Malformed)?;
        let (body, crc) = bytes.split_at_checked(split).ok_or(DecodeError::Malformed)?;
        let crc = u32::from_le_bytes(crc.try_into().map_err(|_| DecodeError::Malformed)?);
        if arca_digest::crc32(body) != crc {
            return Err(DecodeError::BadChecksum);
        }
        let mut r = Reader { data: body, pos: 4 };
        let bad = DecodeError::Malformed;
        let first = r.varint().ok_or(bad)?;
        let first_day = match first {
            0 => None,
            n => Some(u32::try_from(n - 1).map_err(|_| bad)?),
        };
        let all = r.counts().ok_or(bad)?;
        let n = r.varint().ok_or(bad)?;
        if n > u64::from(KEEP_DAYS) {
            return Err(bad);
        }
        let mut days: Vec<Bucket> = Vec::new();
        let mut prev = 0u32;
        for i in 0..n {
            let delta = u32::try_from(r.varint().ok_or(bad)?).map_err(|_| bad)?;
            // Days are strictly ascending (the first may be anything).
            if i > 0 && delta == 0 {
                return Err(bad);
            }
            prev = prev.checked_add(delta).ok_or(bad)?;
            days.push(Bucket { day: prev, counts: r.counts().ok_or(bad)? });
        }
        if r.pos != body.len() {
            return Err(bad);
        }
        let mut stats = Self { first_day, all, days };
        // The all-time total can never be smaller than what the buckets add up to.
        let mut sum = Counts::default();
        for b in &stats.days {
            sum.add(&b.counts);
        }
        stats.all.raise_to(&sum);
        Ok(stats)
    }

    /// The summary for the platforms as JSON:
    /// `{"empty":bool,"firstDay":N,"days":N,"week":{…},"month":{…},"all":{…}}` where each
    /// window holds `opened, clean, trackers, active, threats, rejected, unverified, sterile,
    /// pictures` (numbers capped at 2^63 - 1). `firstDay` is `0` when nothing was recorded.
    #[must_use]
    pub fn summary_json(&self, today: u32) -> Vec<u8> {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_u64("version", u64::from(VERSION));
        w.field_bool("empty", self.is_empty());
        w.field_u64("firstDay", u64::from(self.first_day.unwrap_or(0)));
        w.field_u64("days", u64::from(self.days_in_use(today)));
        for (key, window) in [("week", Window::Week), ("month", Window::Month), ("all", Window::All)] {
            w.key(key);
            counts_json(&mut w, &self.totals(window, today));
        }
        w.end_object();
        w.finish()
    }
}

fn counts_json(w: &mut JsonWriter, c: &Counts) {
    w.begin_object();
    for (key, v) in [
        ("opened", c.opened),
        ("clean", c.clean),
        ("trackers", c.trackers),
        ("active", c.active),
        ("threats", c.threats()),
        ("rejected", c.rejected),
        ("unverified", c.unverified),
        ("sterile", c.sterile),
        ("pictures", c.pictures),
    ] {
        w.field_u64(key, v.min(JSON_MAX));
    }
    w.end_object();
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    /// A LEB128 varint of at most 10 bytes that fits 64 bits.
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10u32 {
            let b = self.byte()?;
            // The tenth byte may only carry the 64th bit.
            if i == 9 && b > 1 {
                return None;
            }
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    fn counts(&mut self) -> Option<Counts> {
        let mut a = [0u64; FIELDS];
        for slot in &mut a {
            *slot = self.varint()?;
        }
        Some(Counts::from_array(a))
    }
}

// ---------------------------------------------------------------------------------------------
// The byte-and-integer interface shared by the Android (JNI) and iOS (C) bindings. Every function
// takes the stored blob (empty or damaged = no statistics yet) and returns the new blob or JSON.
// Negative or out-of-range integers are clamped, nothing panics.
// ---------------------------------------------------------------------------------------------

fn day_of(day: i32) -> u32 {
    u32::try_from(day).unwrap_or(0)
}

fn count_of(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

/// The blob after one scan (`verdict` as in [`verdict_from_code`]).
#[must_use]
pub fn blob_record_scan(blob: &[u8], day: i32, verdict: i32, trackers: i32, active: i32) -> Vec<u8> {
    let mut s = Stats::decode(blob);
    s.record(
        day_of(day),
        Event::Scan { verdict: verdict_from_code(verdict), trackers: count_of(trackers), active: count_of(active) },
    );
    s.encode()
}

/// The blob after a sterile copy ([`EVENT_STERILE_COPY`]) or an opened picture
/// ([`EVENT_PICTURE_OPENED`]); any other event code changes nothing.
#[must_use]
pub fn blob_record_event(blob: &[u8], day: i32, event: i32) -> Vec<u8> {
    let mut s = Stats::decode(blob);
    match event {
        EVENT_STERILE_COPY => s.record(day_of(day), Event::SterileCopy),
        EVENT_PICTURE_OPENED => s.record(day_of(day), Event::PictureOpened),
        _ => {}
    }
    s.encode()
}

/// The summary JSON of the blob (see [`Stats::summary_json`]).
#[must_use]
pub fn blob_summary_json(blob: &[u8], today: i32) -> Vec<u8> {
    Stats::decode(blob).summary_json(day_of(today))
}

/// The blob with the counters of the older versions folded into the all-time total.
///
/// `passport` is the old 36-byte record (documents, trackers, active and the first day); when it
/// is missing or damaged, `documents` and `threats` (the old two plain counters) are used
/// instead, the threats under "active content" because the old counter did not tell the two
/// apart. Call it once and then delete the old data.
#[must_use]
pub fn blob_fold_legacy(blob: &[u8], passport: &[u8], documents: i64, threats: i64, today: i32) -> Vec<u8> {
    let mut s = Stats::decode(blob);
    let (counts, first) = match PatronStats::decode(passport) {
        Ok(p) => (
            Counts {
                opened: p.documents_opened,
                trackers: p.trackers_blocked,
                active: p.active_content_neutralized,
                ..Counts::default()
            },
            Some(p.first_use_day),
        ),
        Err(_) => (
            Counts {
                opened: u64::try_from(documents).unwrap_or(0),
                active: u64::try_from(threats).unwrap_or(0),
                ..Counts::default()
            },
            Some(day_of(today)),
        ),
    };
    if !counts.is_zero() {
        s.fold_legacy(&counts, first);
    }
    s.encode()
}

/// The Patron Passport JSON (see `render::passport_json`) built from the all-time totals.
#[must_use]
pub fn blob_passport_json(lang: i32, blob: &[u8], today: i32) -> Vec<u8> {
    let s = Stats::decode(blob);
    let today = day_of(today);
    let all = s.totals(Window::All, today);
    let record = PatronStats {
        first_use_day: s.first_day.unwrap_or(today),
        documents_opened: all.opened,
        trackers_blocked: all.trackers,
        active_content_neutralized: all.active,
    }
    .encode();
    render::passport_json(render::lang_from_code(u32::try_from(lang).unwrap_or(1)), &record, today)
}

#[cfg(test)]
mod tests;
