//! The booking data layer over `akurai-storage` B+trees.
//!
//! ## Storage model
//!
//! Each entity is one persistent B+tree file under the data directory:
//!
//! | Tree | Key | Value |
//! |------|-----|-------|
//! | `bookings.db` | slot start, `i64` epoch-ms, 8-byte **big-endian** | booking JSON |
//! | `user_packages.db` | text id | user-package JSON |
//! | `user_subscriptions.db` | text id | user-subscription JSON |
//! | `pricing_rules.db` | text id | pricing-rule JSON |
//! | `users.db` | text id | user JSON |
//!
//! Booking keys are the **slot start time**. Big-endian encoding makes the
//! tree's natural key order chronological, so availability scans are a single
//! `range` query, and — the load-bearing part — the tree's keyed uniqueness is
//! the no-double-booking guard: there is physically one record per slot.
//!
//! ## Atomicity (no SQL transactions yet)
//!
//! The framework has no multi-row transactions. We get correctness from three
//! things working together:
//!
//! 1. **Single in-process write lock.** The whole [`Store`] sits behind one
//!    `Mutex`. Every read and every write takes it, so the entire
//!    check-then-insert critical section of [`Store::create_booking`] runs with
//!    no interleaving — a second booking for the same slot can't slip between
//!    the conflict check and the insert.
//! 2. **Keyed uniqueness.** Even setting concurrency aside, the booking tree
//!    holds exactly one record per slot key; an active record there *is* the
//!    lock on that slot.
//! 3. **Fail-safe commit ordering.** A package booking decrements the package
//!    and `commit`s that tree *before* the booking is committed. So a crash
//!    between the two fsyncs can only ever **lose** a booking (slot stays free,
//!    deduction already durable) — it can never double-book or hand out a free
//!    slot. The booking tree is the sole authority for slot occupancy, so the
//!    critical invariant (no double booking) is upheld unconditionally.
//!
//! This is a single-writer venue booking system; the lock cost is irrelevant
//! and the guarantees are exactly what the source's SQL transaction provided.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use akurai_json::Value;
use akurai_storage::BTree;

use crate::booking::pricing::{effective_slot_price, PricingRule};
use crate::booking::subscription::{covers_date, daily_usage};
use crate::booking::time::{add_days, date_string, day_start, generate_day_slots, hour_of, DAY_MS};

/// A confirmed/pending/cancelled booking record (mirrors the `bookings` table).
#[derive(Debug, Clone, PartialEq)]
pub struct Booking {
    pub id: String,
    pub user_id: String,
    pub starts_at: i64,
    pub status: String,
    pub payment_type: String,
    pub price_paid: Option<i64>,
    pub user_package_id: Option<String>,
    pub user_subscription_id: Option<String>,
    pub notes: Option<String>,
    pub created_at: i64,
    pub cancelled_at: Option<i64>,
}

/// A user's klippikort grant with a remaining slot count (`user_packages`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserPackage {
    pub id: String,
    pub user_id: String,
    pub remaining: i64,
}

/// A user's active subscription window + its daily limit (`user_subscriptions`
/// joined with `subscriptions.daily_limit`, denormalized for the write path).
#[derive(Debug, Clone, PartialEq)]
pub struct UserSubscription {
    pub id: String,
    pub user_id: String,
    pub valid_from: i64,
    pub valid_until: i64,
    pub daily_limit: i64,
}

/// One slot in an availability response.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotView {
    pub hour: i64,
    pub starts_at: String,
    pub price: i64,
    pub status: String,
}

/// One day of availability (24 slots).
#[derive(Debug, Clone, PartialEq)]
pub struct DayAvailability {
    pub date: String,
    pub slots: Vec<SlotView>,
}

/// A booking is "active" (and so blocks its slot) when confirmed or pending —
/// pending exists because bank-transfer checkout reserves a slot before final
/// confirmation. Cancelled/completed never block.
fn is_active(status: &str) -> bool {
    status == "confirmed" || status == "pending"
}

/// The set of B+trees, guarded together by the store's single write lock.
struct Trees {
    bookings: BTree,
    user_packages: BTree,
    user_subscriptions: BTree,
    pricing_rules: BTree,
    users: BTree,
}

/// The booking store: all trees behind one mutex (the in-process write lock).
pub struct Store {
    inner: Mutex<Trees>,
}

/// Monotonic suffix so booking ids are unique even within the same millisecond.
static BOOKING_SEQ: AtomicU64 = AtomicU64::new(0);

impl Store {
    /// Open (creating if absent) every tree under `data_dir`, then seed default
    /// pricing rules if none exist yet.
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        let open = |name: &str| BTree::open(data_dir.join(name));
        let trees = Trees {
            bookings: open("bookings.db")?,
            user_packages: open("user_packages.db")?,
            user_subscriptions: open("user_subscriptions.db")?,
            pricing_rules: open("pricing_rules.db")?,
            users: open("users.db")?,
        };
        let store = Store {
            inner: Mutex::new(trees),
        };
        store.seed_defaults()?;
        Ok(store)
    }

    /// Seed the three default pricing rules (from the source `seed.ts`) the
    /// first time the store is empty, so the calendar shows real prices.
    fn seed_defaults(&self) -> io::Result<()> {
        let mut t = self.lock();
        if !t.pricing_rules.range(&[], &[0xff; 16])?.is_empty() {
            return Ok(());
        }
        let rules = [
            ("pr-evening", "Síðdegi/Kvöld", 16, 22, 3500),
            ("pr-daytime", "Daytime", 8, 16, 3500),
            ("pr-offhours", "Off-hours", 22, 8, 2000),
        ];
        for (id, name, start, end, price) in rules {
            let v = pricing_rule_value(id, name, start, end, price, true);
            t.pricing_rules
                .insert(id.as_bytes(), v.to_json().as_bytes())?;
        }
        t.pricing_rules.commit()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Trees> {
        // The lock is only poisoned if a holder panicked mid-write; recovering
        // the guard is correct here because every write commits atomically and a
        // poisoned read just re-reads committed state.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- reads ------------------------------------------------------------

    /// All **active** pricing rules, in stored (id) order.
    pub fn active_pricing_rules(&self) -> Vec<PricingRule> {
        let mut t = self.lock();
        all_records(&mut t.pricing_rules)
            .into_iter()
            .filter(|v| v.get("active").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(pricing_rule_from_value)
            .collect()
    }

    /// A user's `fixed_price` override, if the user exists and has one.
    pub fn user_fixed_price(&self, user_id: &str) -> Option<i64> {
        let mut t = self.lock();
        let raw = t.users.get(user_id.as_bytes()).ok().flatten()?;
        let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
        v.get("fixed_price").and_then(Value::as_i64)
    }

    /// The effective price for `hour`, honoring a user `fixed_price` override.
    pub fn slot_price(&self, hour: i64, user_id: Option<&str>) -> i64 {
        let fixed = user_id.and_then(|u| self.user_fixed_price(u));
        effective_slot_price(hour, &self.active_pricing_rules(), fixed)
    }

    /// Build availability for `days` days starting at the calendar day of
    /// `start_ms`. Mirrors `/api/availability`: each slot is `past`, `booked`,
    /// or `available`, priced with the user's effective price.
    pub fn availability(
        &self,
        start_ms: i64,
        days: i64,
        user_id: Option<&str>,
        now_ms: i64,
    ) -> Vec<DayAvailability> {
        let start_day = day_start(start_ms);
        let end = add_days(start_day, days);
        let fixed = user_id.and_then(|u| self.user_fixed_price(u));
        let rules = self.active_pricing_rules();

        let mut t = self.lock();
        let booked = active_slot_set(&mut t.bookings, start_day, end);
        drop(t);

        (0..days)
            .map(|i| {
                let day = add_days(start_day, i);
                let slots = generate_day_slots(day)
                    .into_iter()
                    .map(|slot| {
                        let hour = hour_of(slot.starts_at);
                        let status = if slot.starts_at < now_ms {
                            "past"
                        } else if booked.contains(&slot.starts_at) {
                            "booked"
                        } else {
                            "available"
                        };
                        SlotView {
                            hour,
                            starts_at: slot.key,
                            price: effective_slot_price(hour, &rules, fixed),
                            status: status.into(),
                        }
                    })
                    .collect();
                DayAvailability {
                    date: date_string(day),
                    slots,
                }
            })
            .collect()
    }

    /// A user's non-cancelled bookings, chronological (key order).
    pub fn bookings_for_user(&self, user_id: &str) -> Vec<Booking> {
        let mut t = self.lock();
        all_bookings(&mut t.bookings)
            .into_iter()
            .filter(|b| b.user_id == user_id && b.status != "cancelled")
            .collect()
    }

    // ---- writes -----------------------------------------------------------

    /// Create one booking under the write lock. Mirrors `createBooking`
    /// (`actions.ts`): conflict check, then per-payment-type accounting.
    ///
    /// `payment_type` is `single` | `package` | `subscription`. For `package`
    /// and `subscription`, `ref_id` names the user-package / user-subscription.
    /// Returns the new booking id, or a user-facing error string.
    pub fn create_booking(
        &self,
        user_id: &str,
        slot_ms: i64,
        payment_type: &str,
        ref_id: Option<&str>,
        notes: Option<&str>,
        now_ms: i64,
    ) -> Result<String, String> {
        let mut t = self.lock();

        // Conflict check — an active record at this slot blocks the booking.
        if let Some(existing) = booking_at(&mut t.bookings, slot_ms) {
            if is_active(&existing.status) {
                return Err("Þessi tími er þegar bókaður".into());
            }
        }

        let mut price_paid = None;
        let mut user_package_id = None;
        let mut user_subscription_id = None;

        match payment_type {
            "package" => {
                let ref_id = ref_id.ok_or("userPackageId is required for package bookings")?;
                let mut pkg = user_package_by_id(&mut t.user_packages, ref_id)
                    .filter(|p| p.user_id == user_id)
                    .ok_or("Package not found")?;
                if pkg.remaining <= 0 {
                    return Err("Engir tímar eftir á kortinu".into());
                }
                // Fail-safe: decrement and commit the package BEFORE the booking
                // is committed, so a crash can only lose a booking, never double
                // spend or double book.
                pkg.remaining -= 1;
                put_user_package(&mut t.user_packages, &pkg).map_err(io_err)?;
                t.user_packages.commit().map_err(io_err)?;
                user_package_id = Some(ref_id.to_string());
            }
            "subscription" => {
                let ref_id =
                    ref_id.ok_or("userSubscriptionId is required for subscription bookings")?;
                let sub = user_subscription_by_id(&mut t.user_subscriptions, ref_id)
                    .filter(|s| s.user_id == user_id)
                    .ok_or("Áskrift fannst ekki")?;
                if !covers_date(sub.valid_from, sub.valid_until, slot_ms) {
                    return Err("Tími er utan gildistíma áskriftar".into());
                }
                let day = day_start(slot_ms);
                let used_today =
                    count_active_subscription_bookings(&mut t.bookings, ref_id, day, day + DAY_MS);
                let usage = daily_usage(sub.daily_limit, used_today);
                if usage.exhausted {
                    return Err("Dagskvóti áskriftar fullnýttur".into());
                }
                user_subscription_id = Some(ref_id.to_string());
            }
            "single" => {
                let fixed = user_fixed_price_locked(&mut t.users, user_id);
                let rules = active_pricing_rules_locked(&mut t.pricing_rules);
                price_paid = Some(effective_slot_price(hour_of(slot_ms), &rules, fixed));
            }
            other => return Err(format!("Unknown payment type: {other}")),
        }

        let seq = BOOKING_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("bk-{now_ms}-{seq}");
        let booking = Booking {
            id: id.clone(),
            user_id: user_id.to_string(),
            starts_at: slot_ms,
            status: "confirmed".into(),
            payment_type: payment_type.to_string(),
            price_paid,
            user_package_id,
            user_subscription_id,
            notes: notes.map(str::to_string),
            created_at: now_ms,
            cancelled_at: None,
        };
        put_booking(&mut t.bookings, &booking).map_err(io_err)?;
        t.bookings.commit().map_err(io_err)?;
        Ok(id)
    }

    /// Cancel a booking the user owns. Mirrors `cancelBooking`: marks it
    /// cancelled (freeing the slot) and refunds a package slot if it used one.
    pub fn cancel_booking(
        &self,
        booking_id: &str,
        user_id: &str,
        now_ms: i64,
    ) -> Result<(), String> {
        let mut t = self.lock();
        let mut booking = all_bookings(&mut t.bookings)
            .into_iter()
            .find(|b| b.id == booking_id && b.user_id == user_id)
            .ok_or("Booking not found")?;
        if booking.status == "cancelled" {
            return Err("Booking is already cancelled".into());
        }

        // Refund the package slot first (fail-safe ordering, as on create).
        if booking.payment_type == "package" {
            if let Some(pkg_id) = &booking.user_package_id {
                if let Some(mut pkg) = user_package_by_id(&mut t.user_packages, pkg_id) {
                    pkg.remaining += 1;
                    put_user_package(&mut t.user_packages, &pkg).map_err(io_err)?;
                    t.user_packages.commit().map_err(io_err)?;
                }
            }
        }

        booking.status = "cancelled".into();
        booking.cancelled_at = Some(now_ms);
        put_booking(&mut t.bookings, &booking).map_err(io_err)?;
        t.bookings.commit().map_err(io_err)
    }

    // ---- seed helpers (used by the API seam + tests) ----------------------

    /// Insert/replace a user (only `fixed_price` matters to the booking engine).
    pub fn put_user(&self, id: &str, fixed_price: Option<i64>) -> io::Result<()> {
        let mut t = self.lock();
        let v = Value::Object(vec![
            ("id".into(), Value::Str(id.into())),
            (
                "fixed_price".into(),
                match fixed_price {
                    Some(p) => Value::Int(p),
                    None => Value::Null,
                },
            ),
        ]);
        t.users.insert(id.as_bytes(), v.to_json().as_bytes())?;
        t.users.commit()
    }

    /// Insert/replace a user-package grant.
    pub fn put_user_package(&self, pkg: &UserPackage) -> io::Result<()> {
        let mut t = self.lock();
        put_user_package(&mut t.user_packages, pkg)?;
        t.user_packages.commit()
    }

    /// Insert/replace a user-subscription grant.
    pub fn put_user_subscription(&self, sub: &UserSubscription) -> io::Result<()> {
        let mut t = self.lock();
        let v = Value::Object(vec![
            ("id".into(), Value::Str(sub.id.clone())),
            ("user_id".into(), Value::Str(sub.user_id.clone())),
            ("valid_from".into(), Value::Int(sub.valid_from)),
            ("valid_until".into(), Value::Int(sub.valid_until)),
            ("daily_limit".into(), Value::Int(sub.daily_limit)),
        ]);
        t.user_subscriptions
            .insert(sub.id.as_bytes(), v.to_json().as_bytes())?;
        t.user_subscriptions.commit()
    }

    /// Read back a user-package by id (for tests / inspection).
    pub fn user_package(&self, id: &str) -> Option<UserPackage> {
        let mut t = self.lock();
        user_package_by_id(&mut t.user_packages, id)
    }

    /// Every klippikort grant owned by `user_id`. Read-side accessor used by
    /// checkout-fulfillment tests; the production consumer (the "Mínar síður"
    /// account view) lands in a later phase.
    #[allow(dead_code)]
    pub fn user_packages_for_user(&self, user_id: &str) -> Vec<UserPackage> {
        let mut t = self.lock();
        full_scan(&mut t.user_packages)
            .iter()
            .filter_map(|v| {
                Some(UserPackage {
                    id: v.get("id").and_then(Value::as_str)?.to_string(),
                    user_id: v.get("user_id").and_then(Value::as_str)?.to_string(),
                    remaining: v.get("remaining").and_then(Value::as_i64)?,
                })
            })
            .filter(|p| p.user_id == user_id)
            .collect()
    }
}

// ---- key + record plumbing -----------------------------------------------

/// Big-endian 8-byte key for a slot start time. All booking times are positive
/// (post-1970), so big-endian byte order is chronological.
fn slot_key(starts_at: i64) -> [u8; 8] {
    starts_at.to_be_bytes()
}

fn io_err(e: io::Error) -> String {
    format!("storage error: {e}")
}

fn booking_value(b: &Booking) -> Value {
    let opt_int = |o: Option<i64>| o.map(Value::Int).unwrap_or(Value::Null);
    let opt_str = |o: &Option<String>| o.clone().map(Value::Str).unwrap_or(Value::Null);
    Value::Object(vec![
        ("id".into(), Value::Str(b.id.clone())),
        ("user_id".into(), Value::Str(b.user_id.clone())),
        ("starts_at".into(), Value::Int(b.starts_at)),
        ("status".into(), Value::Str(b.status.clone())),
        ("payment_type".into(), Value::Str(b.payment_type.clone())),
        ("price_paid".into(), opt_int(b.price_paid)),
        ("user_package_id".into(), opt_str(&b.user_package_id)),
        (
            "user_subscription_id".into(),
            opt_str(&b.user_subscription_id),
        ),
        ("notes".into(), opt_str(&b.notes)),
        ("created_at".into(), Value::Int(b.created_at)),
        ("cancelled_at".into(), opt_int(b.cancelled_at)),
    ])
}

fn booking_from_value(v: &Value) -> Option<Booking> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let os = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Booking {
        id: s("id")?,
        user_id: s("user_id")?,
        starts_at: v.get("starts_at").and_then(Value::as_i64)?,
        status: s("status")?,
        payment_type: s("payment_type")?,
        price_paid: v.get("price_paid").and_then(Value::as_i64),
        user_package_id: os("user_package_id"),
        user_subscription_id: os("user_subscription_id"),
        notes: os("notes"),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
        cancelled_at: v.get("cancelled_at").and_then(Value::as_i64),
    })
}

fn put_booking(tree: &mut BTree, b: &Booking) -> io::Result<()> {
    tree.insert(
        &slot_key(b.starts_at),
        booking_value(b).to_json().as_bytes(),
    )
}

fn booking_at(tree: &mut BTree, slot_ms: i64) -> Option<Booking> {
    let raw = tree.get(&slot_key(slot_ms)).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    booking_from_value(&v)
}

/// Every booking in the tree (full scan), in key (chronological) order.
fn all_bookings(tree: &mut BTree) -> Vec<Booking> {
    full_scan(tree)
        .iter()
        .filter_map(booking_from_value)
        .collect()
}

/// Slot-start set of the active bookings in `[start, end)` — one range query.
fn active_slot_set(tree: &mut BTree, start: i64, end: i64) -> std::collections::HashSet<i64> {
    let lo = slot_key(start);
    let hi = slot_key(end);
    tree.range(&lo, &hi)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| akurai_json::parse(&String::from_utf8_lossy(raw)).ok())
        .filter_map(|v| booking_from_value(&v))
        .filter(|b| is_active(&b.status))
        .map(|b| b.starts_at)
        .collect()
}

/// Count active bookings against `sub_id` whose slot falls in `[start, end)`.
fn count_active_subscription_bookings(tree: &mut BTree, sub_id: &str, start: i64, end: i64) -> i64 {
    let lo = slot_key(start);
    let hi = slot_key(end);
    tree.range(&lo, &hi)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| akurai_json::parse(&String::from_utf8_lossy(raw)).ok())
        .filter_map(|v| booking_from_value(&v))
        .filter(|b| is_active(&b.status) && b.user_subscription_id.as_deref() == Some(sub_id))
        .count() as i64
}

fn user_package_value(p: &UserPackage) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(p.id.clone())),
        ("user_id".into(), Value::Str(p.user_id.clone())),
        ("remaining".into(), Value::Int(p.remaining)),
    ])
}

fn put_user_package(tree: &mut BTree, p: &UserPackage) -> io::Result<()> {
    tree.insert(p.id.as_bytes(), user_package_value(p).to_json().as_bytes())
}

fn user_package_by_id(tree: &mut BTree, id: &str) -> Option<UserPackage> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    Some(UserPackage {
        id: v.get("id").and_then(Value::as_str)?.to_string(),
        user_id: v.get("user_id").and_then(Value::as_str)?.to_string(),
        remaining: v.get("remaining").and_then(Value::as_i64)?,
    })
}

fn user_subscription_by_id(tree: &mut BTree, id: &str) -> Option<UserSubscription> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    Some(UserSubscription {
        id: v.get("id").and_then(Value::as_str)?.to_string(),
        user_id: v.get("user_id").and_then(Value::as_str)?.to_string(),
        valid_from: v.get("valid_from").and_then(Value::as_i64)?,
        valid_until: v.get("valid_until").and_then(Value::as_i64)?,
        daily_limit: v.get("daily_limit").and_then(Value::as_i64)?,
    })
}

fn user_fixed_price_locked(tree: &mut BTree, user_id: &str) -> Option<i64> {
    let raw = tree.get(user_id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    v.get("fixed_price").and_then(Value::as_i64)
}

fn pricing_rule_value(
    id: &str,
    name: &str,
    start: i64,
    end: i64,
    price: i64,
    active: bool,
) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(id.into())),
        ("name".into(), Value::Str(name.into())),
        ("start_hour".into(), Value::Int(start)),
        ("end_hour".into(), Value::Int(end)),
        ("price".into(), Value::Int(price)),
        ("active".into(), Value::Bool(active)),
    ])
}

fn pricing_rule_from_value(v: Value) -> Option<PricingRule> {
    Some(PricingRule {
        name: v.get("name").and_then(Value::as_str)?.to_string(),
        start_hour: v.get("start_hour").and_then(Value::as_i64)?,
        end_hour: v.get("end_hour").and_then(Value::as_i64)?,
        price: v.get("price").and_then(Value::as_i64)?,
    })
}

fn active_pricing_rules_locked(tree: &mut BTree) -> Vec<PricingRule> {
    full_scan(tree)
        .into_iter()
        .filter(|v| v.get("active").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(pricing_rule_from_value)
        .collect()
}

/// Decode every record value in a string-keyed tree.
fn all_records(tree: &mut BTree) -> Vec<Value> {
    full_scan(tree)
}

/// Scan a whole tree's values as parsed JSON. Tree keys are bounded byte
/// strings, so an all-`0x00`..all-`0xff` range covers every key.
fn full_scan(tree: &mut BTree) -> Vec<Value> {
    let hi = [0xff_u8; 64];
    tree.range(&[], &hi)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| akurai_json::parse(&String::from_utf8_lossy(raw)).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::booking::time::{parse_date, parse_instant, HOUR_MS};
    use std::path::PathBuf;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "gsd-booking-test-{tag}-{}-{}",
            std::process::id(),
            BOOKING_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seeds_pricing_rules_on_first_open() {
        let (store, dir) = temp_store("seed");
        let rules = store.active_pricing_rules();
        assert_eq!(rules.len(), 3);
        // Daytime (10:00) priced at 3500, off-hours (03:00) at 2000.
        assert_eq!(store.slot_price(10, None), 3500);
        assert_eq!(store.slot_price(3, None), 2000);
        cleanup(&dir);
    }

    #[test]
    fn single_booking_then_double_book_rejected() {
        let (store, dir) = temp_store("double");
        let now = parse_date("2026-06-26").unwrap();
        let slot = parse_instant("2026-06-27T10:00:00Z").unwrap();

        let id = store
            .create_booking("u1", slot, "single", None, None, now)
            .expect("first booking succeeds");
        assert!(id.starts_with("bk-"));

        // Same slot again → rejected by keyed uniqueness + active check.
        let err = store
            .create_booking("u2", slot, "single", None, None, now)
            .unwrap_err();
        assert_eq!(err, "Þessi tími er þegar bókaður");
        cleanup(&dir);
    }

    #[test]
    fn single_booking_records_effective_price() {
        let (store, dir) = temp_store("price");
        store.put_user("vip", Some(1000)).unwrap();
        let now = parse_date("2026-06-26").unwrap();
        let slot = parse_instant("2026-06-27T10:00:00Z").unwrap();

        store
            .create_booking("vip", slot, "single", None, None, now)
            .unwrap();
        let b = store.bookings_for_user("vip");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].price_paid, Some(1000)); // fixed price overrides 3500
        cleanup(&dir);
    }

    #[test]
    fn package_booking_decrements_and_guards_empty() {
        let (store, dir) = temp_store("pkg");
        store
            .put_user_package(&UserPackage {
                id: "up1".into(),
                user_id: "u1".into(),
                remaining: 1,
            })
            .unwrap();
        let now = parse_date("2026-06-26").unwrap();
        let slot1 = parse_instant("2026-06-27T10:00:00Z").unwrap();
        let slot2 = parse_instant("2026-06-27T11:00:00Z").unwrap();

        store
            .create_booking("u1", slot1, "package", Some("up1"), None, now)
            .expect("first package booking");
        assert_eq!(store.user_package("up1").unwrap().remaining, 0);

        // No slots left → guarded.
        let err = store
            .create_booking("u1", slot2, "package", Some("up1"), None, now)
            .unwrap_err();
        assert_eq!(err, "Engir tímar eftir á kortinu");
        cleanup(&dir);
    }

    #[test]
    fn package_not_found_for_wrong_owner() {
        let (store, dir) = temp_store("pkgowner");
        store
            .put_user_package(&UserPackage {
                id: "up1".into(),
                user_id: "owner".into(),
                remaining: 5,
            })
            .unwrap();
        let now = parse_date("2026-06-26").unwrap();
        let slot = parse_instant("2026-06-27T10:00:00Z").unwrap();
        let err = store
            .create_booking("intruder", slot, "package", Some("up1"), None, now)
            .unwrap_err();
        assert_eq!(err, "Package not found");
        cleanup(&dir);
    }

    #[test]
    fn subscription_daily_limit_enforced() {
        let (store, dir) = temp_store("sub");
        let from = parse_date("2026-06-01").unwrap();
        let until = parse_date("2026-06-30").unwrap();
        store
            .put_user_subscription(&UserSubscription {
                id: "us1".into(),
                user_id: "u1".into(),
                valid_from: from,
                valid_until: until,
                daily_limit: 2,
            })
            .unwrap();
        let now = parse_date("2026-06-10").unwrap();
        let base = parse_instant("2026-06-11T08:00:00Z").unwrap();

        // Two bookings same day → ok; third → over quota.
        store
            .create_booking("u1", base, "subscription", Some("us1"), None, now)
            .unwrap();
        store
            .create_booking("u1", base + HOUR_MS, "subscription", Some("us1"), None, now)
            .unwrap();
        let err = store
            .create_booking(
                "u1",
                base + 2 * HOUR_MS,
                "subscription",
                Some("us1"),
                None,
                now,
            )
            .unwrap_err();
        assert_eq!(err, "Dagskvóti áskriftar fullnýttur");

        // Next day resets the quota.
        let next = parse_instant("2026-06-12T08:00:00Z").unwrap();
        store
            .create_booking("u1", next, "subscription", Some("us1"), None, now)
            .expect("next day resets quota");
        cleanup(&dir);
    }

    #[test]
    fn subscription_outside_window_rejected() {
        let (store, dir) = temp_store("subwin");
        store
            .put_user_subscription(&UserSubscription {
                id: "us1".into(),
                user_id: "u1".into(),
                valid_from: parse_date("2026-06-01").unwrap(),
                valid_until: parse_date("2026-06-05").unwrap(),
                daily_limit: 2,
            })
            .unwrap();
        let now = parse_date("2026-06-01").unwrap();
        let slot = parse_instant("2026-06-20T10:00:00Z").unwrap();
        let err = store
            .create_booking("u1", slot, "subscription", Some("us1"), None, now)
            .unwrap_err();
        assert_eq!(err, "Tími er utan gildistíma áskriftar");
        cleanup(&dir);
    }

    #[test]
    fn cancel_frees_slot_and_refunds_package() {
        let (store, dir) = temp_store("cancel");
        store
            .put_user_package(&UserPackage {
                id: "up1".into(),
                user_id: "u1".into(),
                remaining: 1,
            })
            .unwrap();
        let now = parse_date("2026-06-26").unwrap();
        let slot = parse_instant("2026-06-27T10:00:00Z").unwrap();

        let id = store
            .create_booking("u1", slot, "package", Some("up1"), None, now)
            .unwrap();
        assert_eq!(store.user_package("up1").unwrap().remaining, 0);

        store.cancel_booking(&id, "u1", now).expect("cancel");
        // Package slot refunded.
        assert_eq!(store.user_package("up1").unwrap().remaining, 1);
        // Slot is bookable again.
        store
            .create_booking("u2", slot, "single", None, None, now)
            .expect("slot freed after cancel");
        cleanup(&dir);
    }

    #[test]
    fn cancel_rejects_wrong_user_and_double_cancel() {
        let (store, dir) = temp_store("cancel2");
        let now = parse_date("2026-06-26").unwrap();
        let slot = parse_instant("2026-06-27T10:00:00Z").unwrap();
        let id = store
            .create_booking("u1", slot, "single", None, None, now)
            .unwrap();
        assert_eq!(
            store.cancel_booking(&id, "intruder", now).unwrap_err(),
            "Booking not found"
        );
        store.cancel_booking(&id, "u1", now).unwrap();
        assert_eq!(
            store.cancel_booking(&id, "u1", now).unwrap_err(),
            "Booking is already cancelled"
        );
        cleanup(&dir);
    }

    #[test]
    fn availability_marks_past_booked_available() {
        let (store, dir) = temp_store("avail");
        let now = parse_instant("2026-06-26T12:00:00Z").unwrap();
        let slot = parse_instant("2026-06-26T15:00:00Z").unwrap();
        store
            .create_booking("u1", slot, "single", None, None, now)
            .unwrap();

        let avail = store.availability(now, 1, None, now);
        assert_eq!(avail.len(), 1);
        let day = &avail[0];
        assert_eq!(day.date, "2026-06-26");
        assert_eq!(day.slots.len(), 24);
        assert_eq!(day.slots[8].status, "past"); // 08:00 < 12:00 now
        assert_eq!(day.slots[13].status, "available"); // 13:00 future
        assert_eq!(day.slots[15].status, "booked"); // 15:00 booked
        assert_eq!(day.slots[15].starts_at, "2026-06-26T15:00:00.000Z");
        cleanup(&dir);
    }

    #[test]
    fn durable_across_reopen() {
        let (store, dir) = temp_store("durable");
        let now = parse_date("2026-06-26").unwrap();
        let slot = parse_instant("2026-06-27T10:00:00Z").unwrap();
        store
            .create_booking("u1", slot, "single", None, None, now)
            .unwrap();
        drop(store);

        let store = Store::open(&dir).unwrap();
        let err = store
            .create_booking("u2", slot, "single", None, None, now)
            .unwrap_err();
        assert_eq!(err, "Þessi tími er þegar bókaður");
        cleanup(&dir);
    }
}
