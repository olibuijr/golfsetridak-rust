//! Booking-request validation — ported from `src/lib/booking/validation.ts`.
//!
//! A slot must be in the future and inside the booking window (14 days, or 30
//! with an active package). These are the cheap, stateless guards the API runs
//! before touching storage; the conflict/quota checks live in the store.

use crate::booking::time::booking_window_end;

/// Result of validating a booking request: ok, or a human-readable reason
/// (Icelandic where the source uses Icelandic; English where it does).
pub type Validation = Result<(), String>;

/// Validate that `slot_ms` is bookable at `now_ms`. Mirrors
/// `validateBookingRequest`: rejects past slots and slots beyond the window.
pub fn validate_booking_request(slot_ms: i64, has_package: bool, now_ms: i64) -> Validation {
    if slot_ms < now_ms {
        return Err("Cannot book a slot in the past".into());
    }
    let window_end = booking_window_end(now_ms, has_package, &[]);
    if slot_ms > window_end {
        let days = if has_package { 30 } else { 14 };
        return Err(format!("Slot is outside the {days}-day booking window"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::booking::time::{parse_date, parse_instant, HOUR_MS};

    #[test]
    fn rejects_past_slots() {
        let now = parse_instant("2026-06-26T12:00:00Z").unwrap();
        let past = now - HOUR_MS;
        assert!(validate_booking_request(past, false, now).is_err());
    }

    #[test]
    fn accepts_slot_inside_window() {
        let now = parse_date("2026-06-01").unwrap();
        let slot = parse_instant("2026-06-10T18:00:00Z").unwrap();
        assert!(validate_booking_request(slot, false, now).is_ok());
    }

    #[test]
    fn rejects_slot_beyond_default_window_but_package_extends() {
        let now = parse_date("2026-06-01").unwrap();
        let slot = parse_instant("2026-06-20T18:00:00Z").unwrap(); // day 19 > 14
        assert!(validate_booking_request(slot, false, now).is_err());
        assert!(validate_booking_request(slot, true, now).is_ok()); // 30-day window
    }
}
