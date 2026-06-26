//! The booking engine — the Phase-3 core ported from the source's
//! `src/lib/booking/` (`slots.ts`, `pricing.ts`, `validation.ts`,
//! `subscription.ts`, `actions.ts`) onto the AkurAI-Framework storage engine.
//!
//! - [`time`] — calendar math, slot generation, the booking window (UTC; see
//!   the module note on Iceland being UTC+0).
//! - [`pricing`] — hour-based pricing rules + fixed-price override.
//! - [`subscription`] — coverage + daily-limit accounting.
//! - [`validation`] — stateless request guards (past slot / outside window).
//! - [`store`] — the B+tree-backed data layer: conflict-free booking creation,
//!   package decrement, subscription quota, availability, cancellation.

pub mod pricing;
pub mod store;
pub mod subscription;
pub mod time;
pub mod validation;

pub use store::{Booking, DayAvailability, Store, UserPackage, UserSubscription};
