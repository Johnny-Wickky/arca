//! "Паспорт Хранителя" (Patron Passport): honest, strictly local statistics.
//!
//! The counters live only on the device. This crate performs no I/O and reads
//! no clock: the host stores the 36-byte record ([`PatronStats::encode`]) in
//! its local, encrypted app storage and passes "today" as a day number, so the
//! core stays deterministic and nothing here can ever be sent anywhere.

use core::fmt;

use super::screen::{Lang, PatronageScreen};
use super::upstream;

/// Format version of [`PatronStats::encode`].
const RECORD_VERSION: u8 = 1;
/// Size of the encoded record.
pub const RECORD_LEN: usize = 36;
const RECORD_MAGIC: [u8; 3] = *b"APS";

/// Local usage counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PatronStats {
    /// Day of first use (days since 1970-01-01 in local time), supplied by the host.
    pub first_use_day: u32,
    /// Documents opened in the sandbox.
    pub documents_opened: u64,
    /// External references (trackers, remote templates, UNC paths) that were blocked.
    pub trackers_blocked: u64,
    /// Active payloads (macros, DDE, OLE, scripts) that were neutralised.
    pub active_content_neutralized: u64,
}

/// The stored record is corrupt or from an unknown version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordError;

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("patron statistics record is corrupt or of an unknown version")
    }
}

impl core::error::Error for RecordError {}

impl PatronStats {
    /// Fresh statistics starting on `today`.
    #[must_use]
    pub const fn starting(today: u32) -> Self {
        Self {
            first_use_day: today,
            documents_opened: 0,
            trackers_blocked: 0,
            active_content_neutralized: 0,
        }
    }

    /// Accounts for one opened document. Counters saturate, they never wrap.
    pub fn record_document(&mut self, trackers_blocked: u32, active_content_neutralized: u32) {
        self.documents_opened = self.documents_opened.saturating_add(1);
        self.trackers_blocked = self.trackers_blocked.saturating_add(u64::from(trackers_blocked));
        self.active_content_neutralized =
            self.active_content_neutralized.saturating_add(u64::from(active_content_neutralized));
    }

    /// Days of use including today (at least 1, also if the clock went backwards).
    #[must_use]
    pub const fn days_in_use(&self, today: u32) -> u32 {
        today.saturating_sub(self.first_use_day).saturating_add(1)
    }

    /// Fixed-size little-endian record:
    /// `"APS"`, version, first day (u32), documents, trackers, active (u64 each), CRC-32.
    #[must_use]
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut body = Vec::with_capacity(RECORD_LEN);
        body.extend_from_slice(&RECORD_MAGIC);
        body.push(RECORD_VERSION);
        body.extend_from_slice(&self.first_use_day.to_le_bytes());
        body.extend_from_slice(&self.documents_opened.to_le_bytes());
        body.extend_from_slice(&self.trackers_blocked.to_le_bytes());
        body.extend_from_slice(&self.active_content_neutralized.to_le_bytes());
        let crc = arca_digest::crc32(&body);
        body.extend_from_slice(&crc.to_le_bytes());
        let mut out = [0u8; RECORD_LEN];
        for (dst, src) in out.iter_mut().zip(body) {
            *dst = src;
        }
        out
    }

    /// Decodes [`Self::encode`] output.
    ///
    /// # Errors
    /// [`RecordError`] on wrong length, magic, version or checksum.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        if bytes.len() != RECORD_LEN {
            return Err(RecordError);
        }
        let (body, crc) = bytes.split_at_checked(RECORD_LEN - 4).ok_or(RecordError)?;
        let crc = u32::from_le_bytes(crc.try_into().map_err(|_| RecordError)?);
        if arca_digest::crc32(body) != crc
            || body.get(..3) != Some(&RECORD_MAGIC[..])
            || body.get(3) != Some(&RECORD_VERSION)
        {
            return Err(RecordError);
        }
        let u64_at =
            |o: usize| body.get(o..o + 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes);
        Ok(Self {
            first_use_day: body
                .get(4..8)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or(RecordError)?,
            documents_opened: u64_at(8).ok_or(RecordError)?,
            trackers_blocked: u64_at(16).ok_or(RecordError)?,
            active_content_neutralized: u64_at(24).ok_or(RecordError)?,
        })
    }
}

/// Russian plural category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuPlural {
    One,
    Few,
    Many,
}

fn ru_plural(n: u64) -> RuPlural {
    let (d10, d100) = (n % 10, n % 100);
    if d10 == 1 && d100 != 11 {
        RuPlural::One
    } else if (2..=4).contains(&d10) && !(12..=14).contains(&d100) {
        RuPlural::Few
    } else {
        RuPlural::Many
    }
}

fn ru(n: u64, one: &str, few: &str, many: &str) -> String {
    let word = match ru_plural(n) {
        RuPlural::One => one,
        RuPlural::Few => few,
        RuPlural::Many => many,
    };
    format!("{n} {word}")
}

fn en(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The rendered passport: statistics text plus the dual donation block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatronPassport {
    /// "Паспорт Хранителя" / "Patron Passport".
    pub title: &'static str,
    /// The statistics sentence(s).
    pub body: String,
    /// Call to action.
    pub call_to_action: &'static str,
    /// The same cards as the "About & Support" screen (upstream first).
    pub support: PatronageScreen,
}

impl PatronPassport {
    /// Builds the passport. `app_name` is the name of this build (fork name or "Arca").
    #[must_use]
    pub fn build(
        lang: Lang,
        app_name: &str,
        stats: &PatronStats,
        today: u32,
        support: PatronageScreen,
    ) -> Self {
        let days = u64::from(stats.days_in_use(today));
        let docs = stats.documents_opened;
        let trackers = stats.trackers_blocked;
        let active = stats.active_content_neutralized;
        let body = match lang {
            Lang::Ru => {
                let mut s = if days == 1 {
                    format!("Вы пользуетесь {app_name} первый день.")
                } else {
                    format!("Вы пользуетесь {app_name} уже {}.", ru(days, "день", "дня", "дней"))
                };
                let mut done = vec![format!(
                    "безопасно открыли для вас {}",
                    ru(docs, "документ", "документа", "документов")
                )];
                if trackers > 0 {
                    done.push(format!(
                        "нейтрализовали {}",
                        ru(trackers, "скрытый трекер", "скрытых трекера", "скрытых трекеров")
                    ));
                }
                if active > 0 {
                    done.push(format!(
                        "обезвредили {}",
                        ru(active, "активную угрозу", "активные угрозы", "активных угроз")
                    ));
                }
                s.push_str(&format!(" Мы {} за 0 ₽.", join_ru(&done)));
                s
            }
            Lang::En => {
                let mut s = if days == 1 {
                    format!("Today is your first day with {app_name}.")
                } else {
                    format!("You have been using {app_name} for {}.", en(days, "day", "days"))
                };
                let mut done =
                    vec![format!("safely opened {} for you", en(docs, "document", "documents"))];
                if trackers > 0 {
                    done.push(format!(
                        "neutralized {}",
                        en(trackers, "hidden tracker", "hidden trackers")
                    ));
                }
                if active > 0 {
                    done.push(format!(
                        "disarmed {}",
                        en(active, "active threat", "active threats")
                    ));
                }
                s.push_str(&format!(" We {} — free of charge.", join_en(&done)));
                s
            }
        };
        Self {
            title: match lang {
                Lang::Ru => "Паспорт Хранителя",
                Lang::En => "Patron Passport",
            },
            body,
            call_to_action: match lang {
                Lang::Ru => "Поддержите независимую открытую разработку",
                Lang::En => "Support independent open-source development",
            },
            support,
        }
    }

    /// Name of the original project, for hosts that label the passport.
    #[must_use]
    pub const fn upstream_project() -> &'static str {
        upstream::UPSTREAM_PROJECT
    }
}

fn join_ru(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} и {last}", init.join(", ")),
    }
}

fn join_en(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}
