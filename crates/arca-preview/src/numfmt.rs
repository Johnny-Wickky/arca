//! Spreadsheet number formats (`General`, `0.00`, `#,##0`, percentages, dates, ...).
//!
//! The goal is to show values the way the document author sees them, with a
//! deliberately small, panic-free implementation: no locale database, no
//! evaluation of conditions, no fractions. Unknown constructs degrade to the
//! plain number.

use crate::model::CellKind;

/// Built-in format code for an id (ECMA-376 §18.8.30), `None` for custom ids.
#[must_use]
pub(crate) fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 | 27..=31 | 34..=36 | 50..=58 => "yyyy-mm-dd",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 | 32 => "h:mm",
        21 | 33 => "h:mm:ss",
        22 => "yyyy-mm-dd h:mm",
        37 => "#,##0;(#,##0)",
        38 => "#,##0;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        41 => "#,##0",
        42 => "#,##0",
        43 => "#,##0.00",
        44 => "#,##0.00",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Decimal helpers
// ---------------------------------------------------------------------------

/// The 15 significant decimal digits Excel works with, and the decimal point position.
///
/// Value = `0.d0 d1 d2 … × 10^point`. Returns `None` for non-finite numbers.
struct Digits {
    digits: Vec<u8>,
    point: i32,
}

fn digits_of(v: f64) -> Option<Digits> {
    if !v.is_finite() {
        return None;
    }
    let s = format!("{:.14e}", v.abs());
    let (mant, exp) = s.split_once('e')?;
    let exp: i32 = exp.parse().ok()?;
    let digits: Vec<u8> = mant.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    if digits.iter().all(|&d| d == 0) {
        return Some(Digits { digits: vec![0], point: 1 });
    }
    Some(Digits { digits, point: exp.saturating_add(1) })
}

impl Digits {
    /// Rounds half up to `decimals` fractional digits.
    fn rounded(mut self, decimals: usize) -> Self {
        let dec = i32::try_from(decimals).unwrap_or(i32::MAX);
        let keep = self.point.saturating_add(dec);
        if keep < 0 {
            return Self { digits: vec![0], point: 1 };
        }
        let keep = usize::try_from(keep).unwrap_or(0);
        if keep >= self.digits.len() {
            return self;
        }
        let up = self.digits.get(keep).is_some_and(|&d| d >= 5);
        self.digits.truncate(keep);
        if up {
            let mut i = self.digits.len();
            loop {
                if i == 0 {
                    self.digits.insert(0, 1);
                    self.point = self.point.saturating_add(1);
                    break;
                }
                i -= 1;
                match self.digits.get_mut(i) {
                    Some(d) if *d == 9 => *d = 0,
                    Some(d) => {
                        *d += 1;
                        break;
                    }
                    None => break,
                }
            }
        }
        if self.digits.is_empty() {
            self.digits.push(0);
            self.point = 1;
        }
        self
    }

    /// Integer digits and exactly `decimals` fractional digits.
    fn fixed(&self, decimals: usize) -> (String, String) {
        let mut int = String::new();
        let mut frac = String::new();
        for i in 0..self.point.max(0) {
            let d = usize::try_from(i).ok().and_then(|i| self.digits.get(i)).copied().unwrap_or(0);
            int.push(char::from(b'0' + d));
        }
        if int.is_empty() {
            int.push('0');
        }
        for k in 0..decimals {
            let idx = i32::try_from(k).unwrap_or(i32::MAX).saturating_add(self.point);
            let d =
                usize::try_from(idx).ok().and_then(|i| self.digits.get(i)).copied().unwrap_or(0);
            frac.push(char::from(b'0' + d));
        }
        (int, frac)
    }
}

/// Number as `General` shows it: up to 15 significant digits, trailing zeros removed.
#[must_use]
pub(crate) fn general(v: f64) -> String {
    let Some(d) = digits_of(v) else {
        return "#NUM!".to_owned();
    };
    let neg = v < 0.0;
    let mut digits = d.digits.clone();
    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }
    let sign = if neg { "-" } else { "" };
    if digits == [0] {
        return "0".to_owned();
    }
    let exp10 = d.point.saturating_sub(1);
    if !(-9..15).contains(&exp10) {
        let mut m = String::new();
        for (i, dg) in digits.iter().enumerate() {
            if i == 1 {
                m.push('.');
            }
            m.push(char::from(b'0' + dg));
        }
        let e_sign = if exp10 < 0 { '-' } else { '+' };
        return format!("{sign}{m}E{e_sign}{:02}", exp10.abs());
    }
    let mut out = String::from(sign);
    if d.point <= 0 {
        out.push_str("0.");
        for _ in 0..(-d.point) {
            out.push('0');
        }
        for dg in &digits {
            out.push(char::from(b'0' + dg));
        }
    } else {
        let point = usize::try_from(d.point).unwrap_or(0);
        for i in 0..point {
            out.push(char::from(b'0' + digits.get(i).copied().unwrap_or(0)));
        }
        if digits.len() > point {
            out.push('.');
            for dg in digits.iter().skip(point) {
                out.push(char::from(b'0' + dg));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days from civil date to 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A broken-down date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DateTime {
    pub(crate) year: i64,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
    pub(crate) second: u32,
    /// Tenths of a second.
    pub(crate) tenth: u32,
    /// Whole hours elapsed (for `[h]`).
    pub(crate) total_hours: u64,
    /// Day of week, 0 = Sunday.
    pub(crate) weekday: u32,
}

/// Converts a spreadsheet serial number.
pub(crate) fn serial_to_datetime(serial: f64, date1904: bool) -> Option<DateTime> {
    if !serial.is_finite() || !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let mut whole = serial.floor();
    // Round to tenths of a second so that 0.99999999 does not show 23:59:59.9.
    let mut tenths = ((serial - whole) * 864_000.0).round();
    if tenths >= 864_000.0 {
        tenths -= 864_000.0;
        whole += 1.0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let (whole_i, tenths_i) = (whole as i64, tenths as i64);
    let (year, month, day, days_1970) = if date1904 {
        let d = whole_i + days_from_civil(1904, 1, 1);
        let (y, m, dd) = civil_from_days(d);
        (y, m, dd, d)
    } else if whole_i == 60 {
        (1900, 2, 29, -25_509 + 1)
    } else {
        let adj = i64::from(whole_i < 60);
        let d = whole_i - 25_569 + adj;
        let (y, m, dd) = civil_from_days(d);
        (y, m, dd, d)
    };
    let secs = tenths_i / 10;
    let tenth = u32::try_from(tenths_i % 10).unwrap_or(0);
    let hour = u32::try_from(secs / 3600).unwrap_or(0);
    let minute = u32::try_from((secs % 3600) / 60).unwrap_or(0);
    let second = u32::try_from(secs % 60).unwrap_or(0);
    let weekday = u32::try_from((days_1970 + 4).rem_euclid(7)).unwrap_or(0);
    let total_hours = u64::try_from(whole_i).unwrap_or(0) * 24 + u64::from(hour);
    Some(DateTime { year, month, day, hour, minute, second, tenth, total_hours, weekday })
}

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_RU: [&str; 12] = [
    "января",
    "февраля",
    "марта",
    "апреля",
    "мая",
    "июня",
    "июля",
    "августа",
    "сентября",
    "октября",
    "ноября",
    "декабря",
];
const MONTHS_RU_NOM: [&str; 12] = [
    "январь",
    "февраль",
    "март",
    "апрель",
    "май",
    "июнь",
    "июль",
    "август",
    "сентябрь",
    "октябрь",
    "ноябрь",
    "декабрь",
];
const DAYS_EN: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const DAYS_RU: [&str; 7] =
    ["воскресенье", "понедельник", "вторник", "среда", "четверг", "пятница", "суббота"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    En,
    Ru,
}

// ---------------------------------------------------------------------------
// Format codes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(String),
    /// `0`, `#` or `?`.
    Digit(char),
    Point,
    Comma,
    Percent,
    Exp,
    Text,
    Year(usize),
    Month(usize),
    Day(usize),
    Hour(usize),
    Minute(usize),
    Second(usize),
    ElapsedHours,
    AmPm(bool),
    SubSecond,
}

/// One section of a format code.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Section {
    toks: Vec<Tok>,
    lang_ru: bool,
    general: bool,
}

/// A parsed number format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Format {
    sections: Vec<Section>,
}

fn split_sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    let mut bracket = false;
    let mut escape = false;
    for (i, c) in code.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match c {
            '\\' | '_' | '*' if !quoted => escape = true,
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            ';' if !quoted && !bracket => {
                out.push(code.get(start..i).unwrap_or(""));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(code.get(start..).unwrap_or(""));
    out
}

fn parse_section(src: &str) -> Section {
    let mut sec = Section::default();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut lit = String::new();
    let flush = |lit: &mut String, toks: &mut Vec<Tok>| {
        if !lit.is_empty() {
            toks.push(Tok::Lit(std::mem::take(lit)));
        }
    };
    if src.trim().eq_ignore_ascii_case("general") {
        sec.general = true;
        return sec;
    }
    let mut prev_hour_or_min_ctx = false;
    while let Some(&c) = chars.get(i) {
        match c {
            '"' => {
                i += 1;
                while let Some(&q) = chars.get(i) {
                    if q == '"' {
                        break;
                    }
                    lit.push(q);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    lit.push(n);
                }
                i += 2;
            }
            '_' => {
                lit.push(' ');
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let end =
                    chars.iter().skip(i).position(|&ch| ch == ']').map_or(chars.len(), |p| i + p);
                let inner: String = chars.get(i + 1..end).unwrap_or_default().iter().collect();
                let lower = inner.to_ascii_lowercase();
                i = end + 1;
                if let Some(rest) = inner.strip_prefix('$') {
                    // [$€-407] → currency symbol; [$-419] → locale only.
                    let (sym, loc) = rest.split_once('-').unwrap_or((rest, ""));
                    if !sym.is_empty() {
                        lit.push_str(sym);
                    }
                    if loc.eq_ignore_ascii_case("419") || loc.to_ascii_lowercase().ends_with("419")
                    {
                        sec.lang_ru = true;
                    }
                } else if matches!(lower.as_str(), "h" | "hh") {
                    flush(&mut lit, &mut sec.toks);
                    sec.toks.push(Tok::ElapsedHours);
                    prev_hour_or_min_ctx = true;
                } else if matches!(lower.as_str(), "m" | "mm" | "s" | "ss") {
                    // Elapsed minutes / seconds are rare; treat like normal fields.
                    flush(&mut lit, &mut sec.toks);
                    let n = lower.len();
                    sec.toks.push(if lower.starts_with('m') {
                        Tok::Minute(n)
                    } else {
                        Tok::Second(n)
                    });
                }
                // Colours, conditions and other brackets are ignored.
            }
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' | 'e' | 'E' | 'g' | 'G'
            | 'b' | 'B' | 'a' | 'A' | '0' | '#' | '?' | '.' | ',' | '%' | '@' => {
                let same = |ch: char| {
                    chars.iter().skip(i).take_while(|&&x| x.eq_ignore_ascii_case(&ch)).count()
                };
                match c.to_ascii_lowercase() {
                    'y' => {
                        flush(&mut lit, &mut sec.toks);
                        let n = same('y');
                        sec.toks.push(Tok::Year(n));
                        i += n;
                    }
                    'd' => {
                        flush(&mut lit, &mut sec.toks);
                        let n = same('d');
                        sec.toks.push(Tok::Day(n));
                        i += n;
                    }
                    'h' => {
                        flush(&mut lit, &mut sec.toks);
                        let n = same('h');
                        sec.toks.push(Tok::Hour(n));
                        prev_hour_or_min_ctx = true;
                        i += n;
                    }
                    's' => {
                        flush(&mut lit, &mut sec.toks);
                        let n = same('s');
                        sec.toks.push(Tok::Second(n));
                        prev_hour_or_min_ctx = false;
                        i += n;
                    }
                    'm' => {
                        flush(&mut lit, &mut sec.toks);
                        let n = same('m');
                        // `m` directly after an hour field or before a seconds field is minutes.
                        let next_is_seconds = chars
                            .iter()
                            .skip(i + n)
                            .find(|x| !matches!(x, ':' | ' '))
                            .is_some_and(|x| x.eq_ignore_ascii_case(&'s'));
                        let minute =
                            (prev_hour_or_min_ctx && n <= 2) || (next_is_seconds && n <= 2);
                        sec.toks.push(if minute { Tok::Minute(n) } else { Tok::Month(n) });
                        prev_hour_or_min_ctx = false;
                        i += n;
                    }
                    'a' => {
                        let rest: String = chars.iter().skip(i).take(5).collect();
                        if rest.eq_ignore_ascii_case("am/pm") {
                            flush(&mut lit, &mut sec.toks);
                            sec.toks.push(Tok::AmPm(true));
                            i += 5;
                        } else if rest.get(..3).is_some_and(|r| r.eq_ignore_ascii_case("a/p")) {
                            flush(&mut lit, &mut sec.toks);
                            sec.toks.push(Tok::AmPm(false));
                            i += 3;
                        } else {
                            lit.push(c);
                            i += 1;
                        }
                    }
                    'e' => {
                        if matches!(chars.get(i + 1), Some('+' | '-')) {
                            flush(&mut lit, &mut sec.toks);
                            sec.toks.push(Tok::Exp);
                            // Keep the sign char for the exponent format.
                            sec.toks.push(Tok::Lit(
                                chars.get(i + 1).map(char::to_string).unwrap_or_default(),
                            ));
                            i += 2;
                        } else {
                            // Era year (Japanese) etc.: show as a year.
                            flush(&mut lit, &mut sec.toks);
                            sec.toks.push(Tok::Year(4));
                            i += 1;
                        }
                    }
                    'g' | 'b' => {
                        // Era / Buddhist year markers: ignored.
                        i += 1;
                    }
                    '0' | '#' | '?' => {
                        flush(&mut lit, &mut sec.toks);
                        sec.toks.push(Tok::Digit(c));
                        i += 1;
                    }
                    '.' => {
                        // ".0" directly after seconds is a sub-second field.
                        if matches!(sec.toks.last(), Some(Tok::Second(_)))
                            && chars.get(i + 1) == Some(&'0')
                        {
                            sec.toks.push(Tok::SubSecond);
                            i += 2;
                            while chars.get(i) == Some(&'0') {
                                i += 1;
                            }
                        } else {
                            flush(&mut lit, &mut sec.toks);
                            sec.toks.push(Tok::Point);
                            i += 1;
                        }
                    }
                    ',' => {
                        flush(&mut lit, &mut sec.toks);
                        sec.toks.push(Tok::Comma);
                        i += 1;
                    }
                    '%' => {
                        flush(&mut lit, &mut sec.toks);
                        sec.toks.push(Tok::Percent);
                        i += 1;
                    }
                    '@' => {
                        flush(&mut lit, &mut sec.toks);
                        sec.toks.push(Tok::Text);
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            c => {
                lit.push(c);
                i += 1;
            }
        }
    }
    flush(&mut lit, &mut sec.toks);
    if sec.is_date() {
        // In a date or time format '.', ',', '0' and '%' are plain punctuation.
        for t in &mut sec.toks {
            match t {
                Tok::Point => *t = Tok::Lit(".".to_owned()),
                Tok::Comma => *t = Tok::Lit(",".to_owned()),
                Tok::Percent => *t = Tok::Lit("%".to_owned()),
                Tok::Digit(c) => *t = Tok::Lit(c.to_string()),
                _ => {}
            }
        }
    }
    sec
}

impl Section {
    fn is_date(&self) -> bool {
        self.toks.iter().any(|t| {
            matches!(
                t,
                Tok::Year(_)
                    | Tok::Month(_)
                    | Tok::Day(_)
                    | Tok::Hour(_)
                    | Tok::Minute(_)
                    | Tok::Second(_)
                    | Tok::ElapsedHours
                    | Tok::AmPm(_)
            )
        })
    }

    fn is_text_only(&self) -> bool {
        self.toks.iter().any(|t| matches!(t, Tok::Text))
            && !self.toks.iter().any(|t| matches!(t, Tok::Digit(_)))
    }
}

/// Parses a format code.
pub(crate) fn parse(code: &str) -> Format {
    Format { sections: split_sections(code).into_iter().map(parse_section).collect() }
}

impl Format {
    /// `true` if the first section renders dates or times.
    #[cfg(test)]
    pub(crate) fn is_date(&self) -> bool {
        self.sections.first().is_some_and(Section::is_date)
    }

    /// Formats a number. Returns the text and whether it should be treated as a date.
    pub(crate) fn number(&self, v: f64, date1904: bool) -> (String, CellKind) {
        if !v.is_finite() {
            return ("#NUM!".to_owned(), CellKind::Error);
        }
        // Section choice: positive / negative / zero.
        let (sec, negate_auto) = match (self.sections.len(), v) {
            (0, _) => return (general(v), CellKind::Number),
            (1, _) => (self.sections.first(), v < 0.0),
            (_, x) if x < 0.0 => (self.sections.get(1), false),
            (3.., _) if v == 0.0 => (self.sections.get(2), false),
            _ => (self.sections.first(), false),
        };
        let Some(sec) = sec else { return (general(v), CellKind::Number) };
        if sec.general {
            return (general(v), CellKind::Number);
        }
        if sec.is_text_only() {
            return (general(v), CellKind::Number);
        }
        if sec.is_date() {
            return match serial_to_datetime(v.abs(), date1904) {
                Some(dt) => (render_date(sec, &dt), CellKind::Date),
                None => (general(v), CellKind::Number),
            };
        }
        (render_number(sec, v, negate_auto), CellKind::Number)
    }

    /// Formats text (the `@` section).
    #[cfg(test)]
    pub(crate) fn text(&self, s: &str) -> String {
        let sec = self.sections.get(3).or_else(|| self.sections.iter().find(|s| s.is_text_only()));
        match sec {
            Some(sec) if sec.is_text_only() => sec
                .toks
                .iter()
                .map(|t| match t {
                    Tok::Text => s.to_owned(),
                    Tok::Lit(l) => l.clone(),
                    _ => String::new(),
                })
                .collect(),
            _ => s.to_owned(),
        }
    }
}

fn render_date(sec: &Section, dt: &DateTime) -> String {
    let lang = if sec.lang_ru { Lang::Ru } else { Lang::En };
    let has_ampm = sec.toks.iter().any(|t| matches!(t, Tok::AmPm(_)));
    let mut out = String::new();
    // Russian month names are genitive when a day precedes them ("5 января").
    let day_present = sec.toks.iter().any(|t| matches!(t, Tok::Day(n) if *n <= 2));
    for t in &sec.toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Year(n) => {
                if *n <= 2 {
                    out.push_str(&format!("{:02}", dt.year.rem_euclid(100)));
                } else {
                    out.push_str(&format!("{:04}", dt.year));
                }
            }
            Tok::Month(n) => {
                let idx = usize::try_from(dt.month.saturating_sub(1)).unwrap_or(0);
                match n {
                    1 => out.push_str(&dt.month.to_string()),
                    2 => out.push_str(&format!("{:02}", dt.month)),
                    3 => {
                        let name = match lang {
                            Lang::En => MONTHS_EN.get(idx),
                            Lang::Ru => MONTHS_RU_NOM.get(idx),
                        };
                        out.push_str(
                            &name
                                .map(|m| m.chars().take(3).collect::<String>())
                                .unwrap_or_default(),
                        );
                    }
                    5 => {
                        let name = match lang {
                            Lang::En => MONTHS_EN.get(idx),
                            Lang::Ru => MONTHS_RU_NOM.get(idx),
                        };
                        out.push_str(
                            &name
                                .map(|m| m.chars().take(1).collect::<String>())
                                .unwrap_or_default(),
                        );
                    }
                    _ => {
                        let name = match lang {
                            Lang::En => MONTHS_EN.get(idx),
                            Lang::Ru if day_present => MONTHS_RU.get(idx),
                            Lang::Ru => MONTHS_RU_NOM.get(idx),
                        };
                        out.push_str(name.copied().unwrap_or_default());
                    }
                }
            }
            Tok::Day(n) => match n {
                1 => out.push_str(&dt.day.to_string()),
                2 => out.push_str(&format!("{:02}", dt.day)),
                3 => {
                    let idx = usize::try_from(dt.weekday).unwrap_or(0);
                    let name = match lang {
                        Lang::En => DAYS_EN.get(idx),
                        Lang::Ru => DAYS_RU.get(idx),
                    };
                    out.push_str(
                        &name.map(|d| d.chars().take(3).collect::<String>()).unwrap_or_default(),
                    );
                }
                _ => {
                    let idx = usize::try_from(dt.weekday).unwrap_or(0);
                    let name = match lang {
                        Lang::En => DAYS_EN.get(idx),
                        Lang::Ru => DAYS_RU.get(idx),
                    };
                    out.push_str(name.copied().unwrap_or_default());
                }
            },
            Tok::Hour(n) => {
                let h = if has_ampm { (dt.hour + 11) % 12 + 1 } else { dt.hour };
                if *n >= 2 {
                    out.push_str(&format!("{h:02}"));
                } else {
                    out.push_str(&h.to_string());
                }
            }
            Tok::ElapsedHours => out.push_str(&dt.total_hours.to_string()),
            Tok::Minute(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{:02}", dt.minute));
                } else {
                    out.push_str(&dt.minute.to_string());
                }
            }
            Tok::Second(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{:02}", dt.second));
                } else {
                    out.push_str(&dt.second.to_string());
                }
            }
            Tok::SubSecond => out.push_str(&format!(".{}", dt.tenth)),
            Tok::AmPm(long) => {
                let pm = dt.hour >= 12;
                out.push_str(match (long, pm) {
                    (true, false) => "AM",
                    (true, true) => "PM",
                    (false, false) => "A",
                    (false, true) => "P",
                });
            }
            Tok::Digit(_) | Tok::Point | Tok::Comma | Tok::Percent | Tok::Exp | Tok::Text => {}
        }
    }
    out
}

fn group_thousands(int: &str) -> String {
    let mut out = String::new();
    let n = int.len();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn render_number(sec: &Section, v: f64, auto_minus: bool) -> String {
    let toks = &sec.toks;
    if !toks.iter().any(|t| matches!(t, Tok::Digit(_))) {
        // A purely literal section such as "zero" or "n/a".
        return toks
            .iter()
            .map(|t| match t {
                Tok::Lit(l) => l.as_str(),
                Tok::Percent => "%",
                _ => "",
            })
            .collect();
    }
    let percents = toks.iter().filter(|t| matches!(t, Tok::Percent)).count();
    let mut value = v.abs();
    for _ in 0..percents {
        value *= 100.0;
    }
    let exp_pos = toks.iter().position(|t| matches!(t, Tok::Exp));
    let point_pos = toks.iter().position(|t| matches!(t, Tok::Point));
    let mantissa_end = exp_pos.unwrap_or(toks.len());

    // Scaling commas: trailing commas after the last digit placeholder divide by 1000.
    let last_digit = toks.iter().rposition(|t| matches!(t, Tok::Digit(_)));
    let mut scale_commas = 0usize;
    if let Some(ld) = last_digit {
        let mut k = ld + 1;
        while matches!(toks.get(k), Some(Tok::Comma)) {
            scale_commas += 1;
            k += 1;
        }
        // Commas followed by more placeholders are grouping, handled below.
        if matches!(toks.get(k), Some(Tok::Digit(_))) {
            scale_commas = 0;
        }
    }
    for _ in 0..scale_commas {
        value /= 1000.0;
    }

    let frac_digits = point_pos.map_or(0, |p| {
        toks.iter().take(mantissa_end).skip(p + 1).filter(|t| matches!(t, Tok::Digit(_))).count()
    });
    let grouping = toks.iter().enumerate().any(|(i, t)| {
        matches!(t, Tok::Comma)
            && toks.iter().take(i).any(|x| matches!(x, Tok::Digit(_)))
            && toks
                .iter()
                .skip(i + 1)
                .take(mantissa_end.saturating_sub(i + 1))
                .any(|x| matches!(x, Tok::Digit(_)))
    });
    let min_int = toks
        .iter()
        .take(point_pos.unwrap_or(mantissa_end))
        .filter(|t| matches!(t, Tok::Digit('0')))
        .count();

    let mut body: String;
    let mut exp_text = String::new();
    if exp_pos.is_some() {
        // Scientific notation: normalise the mantissa to the form d.ddd.
        let int_ph = toks
            .iter()
            .take(point_pos.unwrap_or(mantissa_end))
            .filter(|t| matches!(t, Tok::Digit(_)))
            .count()
            .max(1);
        let mut e = 0i32;
        let mut m = value;
        if m != 0.0 {
            let step = i32::try_from(int_ph).unwrap_or(1).max(1);
            let lg = m.log10().floor();
            #[allow(clippy::cast_possible_truncation)]
            {
                // "##0.0E+0" is engineering notation: the exponent is a multiple of 3.
                e = (lg as i32).div_euclid(step) * step;
            }
            m /= 10f64.powi(e);
        }
        let d = digits_of(m).map(|d| d.rounded(frac_digits));
        let (int, frac) =
            d.as_ref().map_or((String::from("0"), String::new()), |d| d.fixed(frac_digits));
        // Rounding may carry (9.99 → 10.0).
        let (int, e) = if int.len() > int_ph {
            (int.get(..int_ph).unwrap_or("1").to_owned(), e + 1)
        } else {
            (int, e)
        };
        body = int;
        if frac_digits > 0 {
            body.push('.');
            body.push_str(&frac);
        }
        let exp_digits = toks
            .iter()
            .skip(exp_pos.unwrap_or(0) + 2)
            .filter(|t| matches!(t, Tok::Digit(_)))
            .count()
            .max(1);
        let sign_tok = match toks.get(exp_pos.unwrap_or(0) + 1) {
            Some(Tok::Lit(s)) => s.clone(),
            _ => "+".to_owned(),
        };
        let sign = if e < 0 {
            "-"
        } else if sign_tok == "+" {
            "+"
        } else {
            ""
        };
        exp_text = format!("E{sign}{:0width$}", e.abs(), width = exp_digits);
    } else {
        let rounded = digits_of(value).map(|d| d.rounded(frac_digits));
        let (int, frac) =
            rounded.as_ref().map_or((String::from("0"), String::new()), |d| d.fixed(frac_digits));
        // Optional integer digits: "#.00" shows ".50", "0.00" shows "0.50".
        let mut int =
            if int == "0" && min_int == 0 && frac_digits > 0 { String::new() } else { int };
        while int.len() < min_int {
            int.insert(0, '0');
        }
        if grouping {
            int = group_thousands(&int);
        }
        body = int;
        if frac_digits > 0 {
            body.push('.');
            // `#` after the point drops trailing zeros.
            let optional = toks
                .iter()
                .take(mantissa_end)
                .skip(point_pos.map_or(0, |p| p + 1))
                .filter(|t| matches!(t, Tok::Digit(_)))
                .collect::<Vec<_>>();
            let mut f: Vec<char> = frac.chars().collect();
            for (idx, ph) in optional.iter().enumerate().rev() {
                if matches!(ph, Tok::Digit('#' | '?'))
                    && f.get(idx) == Some(&'0')
                    && idx + 1 == f.len()
                {
                    f.pop();
                } else {
                    break;
                }
            }
            if f.is_empty() {
                body.pop();
            } else {
                body.extend(f);
            }
        }
        // Mask-style formats (000-0000, (###) ###-####): digits fill the placeholders.
        let int_toks: Vec<&Tok> = toks.iter().take(point_pos.unwrap_or(mantissa_end)).collect();
        let first_ph = int_toks.iter().position(|t| matches!(t, Tok::Digit(_)));
        let last_ph = int_toks.iter().rposition(|t| matches!(t, Tok::Digit(_)));
        if let (Some(a), Some(b), None) = (first_ph, last_ph, point_pos)
            && int_toks.iter().take(b + 1).skip(a).any(|t| matches!(t, Tok::Lit(_)))
            && !grouping
        {
            let digits: Vec<char> = body.chars().filter(char::is_ascii_digit).collect();
            let mut di = digits.len();
            let mut out_rev: Vec<String> = Vec::new();
            for t in int_toks.iter().take(b + 1).skip(a).rev() {
                match t {
                    Tok::Digit(kind) => {
                        if di > 0 {
                            di -= 1;
                            out_rev.push(digits.get(di).map(char::to_string).unwrap_or_default());
                        } else if *kind == '0' {
                            out_rev.push("0".to_owned());
                        }
                    }
                    Tok::Lit(l) => out_rev.push(l.clone()),
                    _ => {}
                }
            }
            let extra: String = digits.iter().take(di).collect();
            body = format!("{extra}{}", out_rev.iter().rev().cloned().collect::<String>());
        }
    }

    // Prefix and suffix literals around the numeric core.
    let first_core = toks.iter().position(|t| matches!(t, Tok::Digit(_) | Tok::Point));
    let last_core = toks.iter().rposition(|t| matches!(t, Tok::Digit(_) | Tok::Exp));
    let mut prefix = String::new();
    let mut suffix = String::new();
    for (i, t) in toks.iter().enumerate() {
        let before = first_core.is_none_or(|f| i < f);
        let after = last_core.is_some_and(|l| i > l);
        let piece = match t {
            Tok::Lit(l) => l.as_str(),
            Tok::Percent => "%",
            _ => continue,
        };
        if matches!(t, Tok::Lit(_)) && exp_pos.is_some_and(|e| i == e + 1) {
            continue;
        }
        if before {
            prefix.push_str(piece);
        } else if after {
            suffix.push_str(piece);
        }
    }
    let sign = if auto_minus && (value != 0.0 || body.chars().any(|c| c != '0' && c != '.')) {
        "-"
    } else {
        ""
    };
    format!("{sign}{prefix}{body}{exp_text}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(code: &str, v: f64) -> String {
        parse(code).number(v, false).0
    }

    #[test]
    fn general_numbers() {
        assert_eq!(general(0.0), "0");
        assert_eq!(general(42.0), "42");
        assert_eq!(general(-3.5), "-3.5");
        assert_eq!(general(0.1 + 0.2), "0.3");
        assert_eq!(general(1.0 / 3.0), "0.333333333333333");
        assert_eq!(general(123_456_789_012.0), "123456789012");
        assert_eq!(general(1e20), "1E+20");
        assert_eq!(general(1.5e-10), "1.5E-10");
        assert_eq!(general(0.000_01), "0.00001");
        assert_eq!(general(100.0), "100");
        assert_eq!(general(-0.0), "0");
    }

    #[test]
    fn fixed_and_grouped() {
        assert_eq!(fmt("0", 2.5), "3");
        assert_eq!(fmt("0", 2.4), "2");
        assert_eq!(fmt("0.00", 2.675), "2.68", "Excel rounds half away from zero on 15 digits");
        assert_eq!(fmt("0.00", -1.005), "-1.01");
        assert_eq!(fmt("#,##0", 1_234_567.0), "1,234,567");
        assert_eq!(fmt("#,##0.00", 1234.5), "1,234.50");
        assert_eq!(fmt("#,##0.00", 0.0), "0.00");
        assert_eq!(fmt("000", 7.0), "007");
        assert_eq!(fmt("#.##", 0.5), ".5");
        assert_eq!(fmt("0.0#", 1.0), "1.0");
        assert_eq!(fmt("0.0#", 1.234), "1.23");
        assert_eq!(fmt("#,##0,", 1_234_567.0), "1,235");
        assert_eq!(fmt("0.00", 9.999), "10.00");
        assert_eq!(fmt("0.000", 0.0005), "0.001");
        assert_eq!(fmt("0.00", 1e-12), "0.00");
    }

    #[test]
    fn percent_currency_text_and_sections() {
        assert_eq!(fmt("0%", 0.256), "26%");
        assert_eq!(fmt("0.00%", 0.12345), "12.35%");
        assert_eq!(fmt("\"$\"#,##0.00", 1234.5), "$1,234.50");
        assert_eq!(fmt("[$€-407]#,##0.00", 3.0), "€3.00");
        assert_eq!(fmt("#,##0.00 \"руб.\"", 10.0), "10.00 руб.");
        assert_eq!(fmt("0.0 \"kg\"", 2.0), "2.0 kg");
        assert_eq!(fmt("#,##0;(#,##0)", -1500.0), "(1,500)");
        assert_eq!(fmt("#,##0;(#,##0)", 1500.0), "1,500");
        assert_eq!(fmt("0;-0;\"zero\"", 0.0), "zero");
        assert_eq!(fmt("0.00", -2.0), "-2.00");
        assert_eq!(fmt("General", 2.5), "2.5");
        assert_eq!(fmt("0.00E+00", 12345.0), "1.23E+04");
        assert_eq!(fmt("0.0E+0", 0.00123), "1.2E-3");
        assert_eq!(fmt("##0.0E+0", 12345.0), "12.3E+3");
    }

    #[test]
    fn masks() {
        assert_eq!(fmt("000-0000", 5_551_234.0), "555-1234");
        assert_eq!(fmt("(###) ###-####", 5_551_234_567.0), "(555) 123-4567");
        assert_eq!(fmt("000-0000", 12.0), "000-0012");
    }

    #[test]
    fn dates_and_times() {
        // 2024-03-05 18:30:00
        let serial = 45_356.0 + (18.0 * 60.0 + 30.0) / 1440.0;
        assert_eq!(fmt("yyyy-mm-dd", serial), "2024-03-05");
        assert_eq!(fmt("dd.mm.yyyy", serial), "05.03.2024");
        assert_eq!(fmt("d/m/yy", serial), "5/3/24");
        assert_eq!(fmt("yyyy-mm-dd hh:mm:ss", serial), "2024-03-05 18:30:00");
        assert_eq!(fmt("h:mm AM/PM", serial), "6:30 PM");
        assert_eq!(fmt("mmm d, yyyy", serial), "Mar 5, 2024");
        assert_eq!(fmt("mmmm yyyy", serial), "March 2024");
        assert_eq!(fmt("dddd", serial), "Tuesday");
        assert_eq!(fmt("ddd", serial), "Tue");
        assert_eq!(fmt("[$-419]d mmmm yyyy", serial), "5 марта 2024");
        assert_eq!(fmt("[$-419]mmmm", serial), "март");
        assert_eq!(fmt("mm:ss", 0.000_694_444), "01:00");
        assert_eq!(fmt("[h]:mm:ss", 1.5), "36:00:00");
        assert_eq!(fmt("h:mm:ss.0", 0.5), "12:00:00.0");
        assert!(parse("yyyy-mm-dd").is_date());
        assert!(!parse("0.00").is_date());
        assert!(!parse("General").is_date());
        assert!(!parse("#,##0 \"d\"").is_date(), "quoted letters are literals");
    }

    #[test]
    fn excel_date_quirks() {
        let dt =
            |s: f64, d1904: bool| serial_to_datetime(s, d1904).map(|d| (d.year, d.month, d.day));
        assert_eq!(dt(1.0, false), Some((1900, 1, 1)));
        assert_eq!(dt(59.0, false), Some((1900, 2, 28)));
        assert_eq!(dt(60.0, false), Some((1900, 2, 29)), "the Lotus 1-2-3 leap-year bug");
        assert_eq!(dt(61.0, false), Some((1900, 3, 1)));
        assert_eq!(dt(25_569.0, false), Some((1970, 1, 1)));
        assert_eq!(dt(36_526.0, false), Some((2000, 1, 1)));
        assert_eq!(dt(0.0, true), Some((1904, 1, 1)));
        assert_eq!(dt(1462.0, true), Some((1908, 1, 2)));
        assert_eq!(dt(-1.0, false), None);
        assert_eq!(dt(f64::NAN, false), None);
        assert_eq!(dt(1e12, false), None);
        // Rounding up to midnight carries into the next day.
        let d = serial_to_datetime(36_526.999_999_999, false).expect("date");
        assert_eq!((d.year, d.month, d.day, d.hour, d.minute, d.second), (2000, 1, 2, 0, 0, 0));
    }

    #[test]
    fn text_sections_and_builtins() {
        assert_eq!(parse("@").text("abc"), "abc");
        assert_eq!(parse("0;0;0;\"[\"@\"]\"").text("abc"), "[abc]");
        assert_eq!(builtin(14), Some("yyyy-mm-dd"));
        assert_eq!(builtin(49), Some("@"));
        assert_eq!(builtin(164), None);
        assert_eq!(parse("0").number(f64::INFINITY, false).1, CellKind::Error);
    }

    #[test]
    fn never_panics_on_garbage_codes() {
        for code in [
            "",
            ";",
            ";;;;",
            "[",
            "]",
            "\"",
            "\\",
            "_",
            "*",
            "0.0.0",
            "E+",
            "e",
            "0E",
            "0E+",
            ",",
            ",,,",
            ".",
            "%%",
            "h:mm:ss.000",
            "[h",
            "[$",
            "[$-",
            "a/p",
            "am/pm",
            "AM/P",
            "yyyyyyyyyy",
            "#,#,#,#",
            "?/?",
            "0\u{0}0",
            "\u{1F600}",
        ] {
            for v in [0.0, 1.0, -1.0, 0.5, 1e300, -1e-300, 45_000.123, f64::MAX, f64::MIN_POSITIVE]
            {
                let f = parse(code);
                let _ = f.number(v, false);
                let _ = f.number(v, true);
                let _ = f.text("x");
            }
        }
    }
}
