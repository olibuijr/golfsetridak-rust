//! Subscription coverage + daily-limit accounting — ported from
//! `src/lib/booking/subscription.ts`.
//!
//! A subscription covers a slot when the slot's calendar day is within
//! `[valid_from, valid_until]` (both ends inclusive, compared on day starts).
//! Each subscription has a `daily_limit` of bookings per calendar day.

use crate::booking::time::day_start;

/// Is `date_ms`'s calendar day inside `[valid_from, valid_until]` (inclusive)?
/// Mirrors `subscriptionCoversDate`. All three are compared on their day start.
pub fn covers_date(valid_from_ms: i64, valid_until_ms: i64, date_ms: i64) -> bool {
    let day = day_start(date_ms);
    day >= day_start(valid_from_ms) && day <= day_start(valid_until_ms)
}

/// Daily quota usage for a subscription. Mirrors `DailyUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyUsage {
    pub used: i64,
    pub limit: i64,
    pub remaining: i64,
    pub exhausted: bool,
}

/// Compute usage from a limit and today's booking count, clamping negatives to
/// zero. Mirrors `getDailySubscriptionUsage`.
pub fn daily_usage(daily_limit: i64, bookings_today: i64) -> DailyUsage {
    let limit = daily_limit.max(0);
    let used = bookings_today.max(0);
    let remaining = (limit - used).max(0);
    DailyUsage {
        used,
        limit,
        remaining,
        exhausted: remaining == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::booking::time::{parse_date, parse_instant};

    #[test]
    fn coverage_is_inclusive_on_both_ends() {
        let from = parse_date("2026-06-01").unwrap();
        let until = parse_date("2026-06-30").unwrap();
        assert!(covers_date(
            from,
            until,
            parse_instant("2026-06-01T08:00:00Z").unwrap()
        ));
        assert!(covers_date(
            from,
            until,
            parse_instant("2026-06-30T23:00:00Z").unwrap()
        ));
        assert!(covers_date(
            from,
            until,
            parse_instant("2026-06-15T12:00:00Z").unwrap()
        ));
    }

    #[test]
    fn coverage_excludes_outside_days() {
        let from = parse_date("2026-06-01").unwrap();
        let until = parse_date("2026-06-30").unwrap();
        assert!(!covers_date(
            from,
            until,
            parse_instant("2026-05-31T23:00:00Z").unwrap()
        ));
        assert!(!covers_date(
            from,
            until,
            parse_instant("2026-07-01T00:00:00Z").unwrap()
        ));
    }

    #[test]
    fn usage_clamps_and_flags_exhaustion() {
        let u = daily_usage(2, 0);
        assert_eq!((u.used, u.remaining, u.exhausted), (0, 2, false));
        let u = daily_usage(2, 1);
        assert_eq!((u.used, u.remaining, u.exhausted), (1, 1, false));
        let u = daily_usage(2, 2);
        assert_eq!((u.remaining, u.exhausted), (0, true));
        let u = daily_usage(2, 5); // over-used clamps to 0 remaining
        assert_eq!((u.remaining, u.exhausted), (0, true));
        let u = daily_usage(-1, -3); // negatives clamp to 0
        assert_eq!((u.limit, u.used, u.exhausted), (0, 0, true));
    }
}
