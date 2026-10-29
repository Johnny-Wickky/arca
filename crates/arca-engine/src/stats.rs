//! The optional local counter ("Passport"): flat all-time totals, kept on the device and nowhere
//! else, off unless the person switches it on.
//!
//! There is no history. Earlier versions kept a ring of 400 daily buckets, which on a seized
//! phone would have been a timeline of the days the person opened documents; this module keeps
//! one set of eight totals and nothing else: no dates, no timestamps, no per-day counts, no
//! document name, content or hash. The core has no clock and is never given one. The platform
//! stores the opaque byte blob of [`Stats::encode`] in its private storage and deletes it when the
//! counter is switched off or reset. The blob is versioned and protected by a CRC-32;
//! [`Stats::decode`] accepts hostile bytes (any length, any content) without panicking and
//! answers with empty totals for anything it does not understand, a blob written by a version
//! that still kept daily buckets included (its history is dropped, never read).
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

use arca_attribution::PatronStats;
use arca_xray::Verdict;

use crate::json::JsonWriter;
use crate::render;

/// Largest encoded blob; a bigger input is not a blob of this module.
pub const MAX_BLOB_LEN: usize = 1024;

const MAGIC: [u8; 3] = *b"AST";
/// Version 2: the eight flat totals. (Version 1 held daily buckets and is not read any more.)
const VERSION: u8 = 2;
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

/// The totals.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stats {
    all: Counts,
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
    /// Empty totals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `true` if nothing was ever counted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all.is_zero()
    }

    /// Counts `event`.
    pub fn record(&mut self, event: Event) {
        self.all.add(&event.counts());
    }

    /// The totals.
    #[must_use]
    pub const fn totals(&self) -> Counts {
        self.all
    }

    /// The compact, versioned, CRC-protected blob: `"AST"`, version, the eight totals (LEB128
    /// varints), then the CRC-32 of everything before it (little-endian).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        for v in self.all.as_array() {
            put_varint(&mut out, v);
        }
        let crc = arca_digest::crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decodes [`Self::encode`] output; empty totals for anything else.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Self {
        Self::try_decode(bytes).unwrap_or_default()
    }

    /// Strict decoding.
    ///
    /// # Errors
    /// [`DecodeError`] for a blob that is too large, of another kind or version, damaged, or
    /// not well formed. Memory use is bounded by [`MAX_BLOB_LEN`].
    pub fn try_decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() > MAX_BLOB_LEN {
            return Err(DecodeError::TooLarge);
        }
        if bytes.get(..3) != Some(&MAGIC[..]) {
            return Err(DecodeError::BadMagic);
        }
        if bytes.get(3) != Some(&VERSION) {
            return Err(if bytes.len() > 3 {
                DecodeError::UnknownVersion
            } else {
                DecodeError::Malformed
            });
        }
        let split = bytes.len().checked_sub(4).ok_or(DecodeError::Malformed)?;
        let (body, crc) = bytes.split_at_checked(split).ok_or(DecodeError::Malformed)?;
        let crc = u32::from_le_bytes(crc.try_into().map_err(|_| DecodeError::Malformed)?);
        if arca_digest::crc32(body) != crc {
            return Err(DecodeError::BadChecksum);
        }
        let mut r = Reader { data: body, pos: 4 };
        let all = r.counts().ok_or(DecodeError::Malformed)?;
        if r.pos != body.len() {
            return Err(DecodeError::Malformed);
        }
        Ok(Self { all })
    }

    /// The summary for the platforms as JSON: `{"version":2,"empty":bool,"all":{...}}` where the
    /// totals hold `opened, clean, trackers, active, threats, rejected, unverified, sterile,
    /// pictures` (numbers capped at 2^63 - 1).
    #[must_use]
    pub fn summary_json(&self) -> Vec<u8> {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_u64("version", u64::from(VERSION));
        w.field_bool("empty", self.is_empty());
        w.key("all");
        counts_json(&mut w, &self.all);
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

fn count_of(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

/// The blob after one scan (`verdict` as in [`verdict_from_code`]).
#[must_use]
pub fn blob_record_scan(blob: &[u8], verdict: i32, trackers: i32, active: i32) -> Vec<u8> {
    let mut s = Stats::decode(blob);
    s.record(Event::Scan {
        verdict: verdict_from_code(verdict),
        trackers: count_of(trackers),
        active: count_of(active),
    });
    s.encode()
}

/// The blob after a sterile copy ([`EVENT_STERILE_COPY`]) or an opened picture
/// ([`EVENT_PICTURE_OPENED`]); any other event code changes nothing.
#[must_use]
pub fn blob_record_event(blob: &[u8], event: i32) -> Vec<u8> {
    let mut s = Stats::decode(blob);
    match event {
        EVENT_STERILE_COPY => s.record(Event::SterileCopy),
        EVENT_PICTURE_OPENED => s.record(Event::PictureOpened),
        _ => {}
    }
    s.encode()
}

/// The summary JSON of the blob (see [`Stats::summary_json`]).
#[must_use]
pub fn blob_summary_json(blob: &[u8]) -> Vec<u8> {
    Stats::decode(blob).summary_json()
}

/// The Patron Passport JSON (see `render::passport_json`) built from the totals. The host passes
/// the day the app was installed (`first_use_day`) and today; neither is stored here.
#[must_use]
pub fn blob_passport_json(lang: i32, blob: &[u8], first_use_day: i32, today: i32) -> Vec<u8> {
    let all = Stats::decode(blob).totals();
    let today = u32::try_from(today).unwrap_or(0);
    let record = PatronStats {
        first_use_day: u32::try_from(first_use_day).unwrap_or(today),
        documents_opened: all.opened,
        trackers_blocked: all.trackers,
        active_content_neutralized: all.active,
    }
    .encode();
    render::passport_json(render::lang_from_code(u32::try_from(lang).unwrap_or(1)), &record, today)
}

#[cfg(test)]
mod tests;
