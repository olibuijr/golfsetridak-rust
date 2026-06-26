//! Calendar/time helpers for the booking engine — pure `std`, no time crate.
//!
//! The source app (`src/lib/booking/slots.ts`) reasons in **local server time**:
//! it builds slots with `Date.setHours(h, 0, 0, 0)` and keys them by
//! `toISOString()` (UTC). Golfsetrið Akureyri runs in Iceland, which sits on
//! `Atlantic/Reykjavik` — permanently **UTC+0 with no daylight saving**. So
//! "local server time" is exactly UTC here, and we can do every calendar
//! computation in UTC and stay faithful to the source. This is a deliberate,
//! documented assumption: if the app is ever hosted outside Iceland the slot
//! hours would need a timezone, but for this venue UTC is correct and keeps the
//! whole engine deterministic and dependency-free.
//!
//! Instants are integer **epoch-milliseconds** (`i64`), matching the source's
//! timestamp columns. Civil-date conversion uses Howard Hinnant's well-known
//! `days_from_civil` / `civil_from_days` algorithm (valid across the proleptic
//! Gregorian calendar, no lookup tables).

/// Milliseconds in one hour.
pub const HOUR_MS: i64 = 3_600_000;
/// Milliseconds in one calendar day (no leap seconds — civil days are 24h here).
pub const DAY_MS: i64 = 86_400_000;

/// Days since the Unix epoch (1970-01-01) for a civil `(year, month, day)`.
///
/// `month` is 1..=12. Hinnant's algorithm; correct for any Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: civil `(year, month, day)` for a day count.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A civil instant decomposed in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub milli: i64,
}

/// Decompose an epoch-ms instant into its UTC civil parts.
pub fn to_civil(ms: i64) -> Civil {
    let days = ms.div_euclid(DAY_MS);
    let rem = ms.rem_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: rem / HOUR_MS,
        minute: (rem % HOUR_MS) / 60_000,
        second: (rem % 60_000) / 1000,
        milli: rem % 1000,
    }
}

/// Build an epoch-ms instant from UTC civil parts.
pub fn from_civil(c: Civil) -> i64 {
    days_from_civil(c.year, c.month, c.day) * DAY_MS
        + c.hour * HOUR_MS
        + c.minute * 60_000
        + c.second * 1000
        + c.milli
}

/// Floor `ms` to 00:00:00.000 UTC of its calendar day (`setHours(0,0,0,0)`).
pub fn day_start(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS) * DAY_MS
}

/// The hour-of-day (0..=23) for `ms`, in UTC (`Date.getHours()`).
pub fn hour_of(ms: i64) -> i64 {
    ms.rem_euclid(DAY_MS) / HOUR_MS
}

/// Add `n` calendar days to `ms` (`Date.setDate(getDate() + n)`).
pub fn add_days(ms: i64, n: i64) -> i64 {
    ms + n * DAY_MS
}

/// Day-of-week index for `ms`, `0 = Sunday .. 6 = Saturday` (`Date.getDay()`).
/// The Unix epoch (1970-01-01) was a Thursday, hence the `+4`.
pub fn weekday_index(ms: i64) -> usize {
    (ms.div_euclid(DAY_MS).rem_euclid(7) as usize + 4) % 7
}

/// Format `ms` as `YYYY-MM-DD` (UTC), matching `toISOString().slice(0,10)`.
pub fn date_string(ms: i64) -> String {
    let c = to_civil(ms);
    format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
}

/// Format `ms` as a UTC ISO-8601 string `YYYY-MM-DDTHH:MM:SS.mmmZ`, matching
/// the source's `Date.toISOString()` — this is the canonical slot key clients
/// receive from `/api/availability` and post back to `/api/book`.
pub fn iso_string(ms: i64) -> String {
    let c = to_civil(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.milli
    )
}

/// Parse a date-only `YYYY-MM-DD` string to the day-start epoch-ms (UTC).
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(from_civil(Civil {
        year: y,
        month: m,
        day: d,
        hour: 0,
        minute: 0,
        second: 0,
        milli: 0,
    }))
}

/// Parse an instant the way `new Date(value)` would for our inputs: either an
/// integer epoch-ms (e.g. `1750896000000`), a date-only `YYYY-MM-DD`, or a full
/// ISO-8601 `YYYY-MM-DDTHH:MM[:SS[.sss]][Z]`. A trailing `Z` (or its absence) is
/// treated as UTC — see the module note on Iceland being UTC+0.
pub fn parse_instant(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Pure integer epoch-ms.
    if s.chars().all(|c| c.is_ascii_digit() || c == '-') {
        if let Ok(ms) = s.parse::<i64>() {
            return Some(ms);
        }
    }
    // Split date and time on 'T' (or a space). Date-only has no separator.
    let (date_part, time_part) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let day = parse_date(date_part)?;
    let Some(time) = time_part else {
        return Some(day);
    };
    let time = time.trim_end_matches('Z');
    let mut hms = time.splitn(3, ':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next().unwrap_or("0").parse().ok()?;
    // Seconds may carry a fractional `.sss` part.
    let (sec_str, milli) = match hms.next() {
        Some(sec) => match sec.split_once('.') {
            Some((s, frac)) => {
                let mut frac = frac.to_string();
                frac.truncate(3);
                while frac.len() < 3 {
                    frac.push('0');
                }
                (s, frac.parse::<i64>().ok()?)
            }
            None => (sec, 0),
        },
        None => ("0", 0),
    };
    let second: i64 = sec_str.parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    Some(day + hour * HOUR_MS + minute * 60_000 + second * 1000 + milli)
}

/// A bookable slot: the instant it starts and the ISO key clients use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub starts_at: i64,
    pub key: String,
}

/// Generate the 24 hourly slots for the calendar day containing `date_ms`.
/// Mirrors `generateDaySlots` (`slots.ts`).
pub fn generate_day_slots(date_ms: i64) -> Vec<Slot> {
    let start = day_start(date_ms);
    (0..24)
        .map(|h| {
            let starts_at = start + h * HOUR_MS;
            Slot {
                starts_at,
                key: iso_string(starts_at),
            }
        })
        .collect()
}

/// Compute the end of the bookable window. Mirrors `getBookingWindowEnd`
/// (`slots.ts`): 14 days by default, 30 with an active package, and extended to
/// the end-of-day of the latest subscription `valid_until` when that is later.
///
/// `subscription_valid_untils` are day-start epoch-ms (a calendar day); the
/// window is pushed to 23:59:59.999 of that day so late slots stay inside it.
pub fn booking_window_end(
    now_ms: i64,
    has_package: bool,
    subscription_valid_untils: &[i64],
) -> i64 {
    let days = if has_package { 30 } else { 14 };
    let base_end = add_days(now_ms, days);
    let mut extended = base_end;
    for &raw in subscription_valid_untils {
        let end_of_day = day_start(raw) + DAY_MS - 1;
        if end_of_day > extended {
            extended = end_of_day;
        }
    }
    extended
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips_known_instant() {
        // 2026-06-26T13:00:00.000Z  →  1782824400000 ms.
        let c = Civil {
            year: 2026,
            month: 6,
            day: 26,
            hour: 13,
            minute: 0,
            second: 0,
            milli: 0,
        };
        let ms = from_civil(c);
        assert_eq!(to_civil(ms), c);
        assert_eq!(date_string(ms), "2026-06-26");
        assert_eq!(iso_string(ms), "2026-06-26T13:00:00.000Z");
        assert_eq!(hour_of(ms), 13);
    }

    #[test]
    fn epoch_zero_is_1970() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(iso_string(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn day_start_floors_to_midnight() {
        let ms = parse_instant("2026-06-26T13:45:30.500Z").unwrap();
        assert_eq!(date_string(day_start(ms)), "2026-06-26");
        assert_eq!(hour_of(day_start(ms)), 0);
        assert_eq!(to_civil(day_start(ms)).minute, 0);
    }

    #[test]
    fn parse_instant_accepts_iso_date_and_epoch() {
        let a = parse_instant("2026-06-26").unwrap();
        let b = parse_instant("2026-06-26T00:00:00.000Z").unwrap();
        assert_eq!(a, b);
        let c = parse_instant(&a.to_string()).unwrap();
        assert_eq!(a, c);
        assert!(parse_instant("not-a-date").is_none());
        assert!(parse_instant("2026-13-01").is_none());
    }

    #[test]
    fn weekday_matches_known_dates() {
        // 2026-06-26 is a Friday → index 5; 1970-01-01 a Thursday → 4.
        assert_eq!(weekday_index(parse_date("2026-06-26").unwrap()), 5);
        assert_eq!(weekday_index(0), 4);
        assert_eq!(weekday_index(parse_date("2026-06-28").unwrap()), 0); // Sunday
    }

    #[test]
    fn generate_day_slots_yields_24_hourly_slots() {
        let day = parse_date("2026-06-26").unwrap();
        let slots = generate_day_slots(day + 9 * HOUR_MS); // any time in the day
        assert_eq!(slots.len(), 24);
        assert_eq!(slots[0].key, "2026-06-26T00:00:00.000Z");
        assert_eq!(slots[8].key, "2026-06-26T08:00:00.000Z");
        assert_eq!(hour_of(slots[23].starts_at), 23);
    }

    #[test]
    fn window_is_14_days_default_30_with_package() {
        let now = parse_date("2026-06-01").unwrap();
        assert_eq!(
            date_string(booking_window_end(now, false, &[])),
            "2026-06-15"
        );
        assert_eq!(
            date_string(booking_window_end(now, true, &[])),
            "2026-07-01"
        );
    }

    #[test]
    fn window_extends_to_subscription_end_when_later() {
        let now = parse_date("2026-06-01").unwrap();
        let sub_end = parse_date("2026-08-31").unwrap();
        let end = booking_window_end(now, false, &[sub_end]);
        assert_eq!(date_string(end), "2026-08-31");
        // End-of-day, so a 22:00 slot on the last day is still inside.
        assert!(end >= parse_instant("2026-08-31T22:00:00.000Z").unwrap());
    }

    #[test]
    fn window_ignores_earlier_subscription_than_base() {
        let now = parse_date("2026-06-01").unwrap();
        let sub_end = parse_date("2026-06-05").unwrap(); // earlier than 14-day base
        assert_eq!(
            date_string(booking_window_end(now, false, &[sub_end])),
            "2026-06-15"
        );
    }
}
