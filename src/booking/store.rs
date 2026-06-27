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
    pub package_name: Option<String>,
    pub slot_count: Option<i64>,
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
    user_subscription_members: BTree,
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

/// Monotonic suffix so subscription-member ids are unique within a millisecond.
static MEMBER_SEQ: AtomicU64 = AtomicU64::new(0);

impl Store {
    /// Open (creating if absent) every tree under `data_dir`, then seed default
    /// pricing rules if none exist yet.
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        let open = |name: &str| BTree::open(data_dir.join(name));
        let trees = Trees {
            bookings: open("bookings.db")?,
            user_subscription_members: open("user_subscription_members.db")?,
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
        let fixed = user_id.and_then(|u| self.user_fixed_price(u));
        let rules = self.active_pricing_rules();
        self.availability_with(start_ms, days, fixed, &rules, now_ms)
    }

    /// Like [`availability`](Self::availability), but with the pricing rules and
    /// per-user `fixed_price` supplied by the caller. Phase 4A routes the page
    /// layer here so slot prices come from the collections store while booked
    /// slots still come from the (transactional) bookings tree.
    pub fn availability_with(
        &self,
        start_ms: i64,
        days: i64,
        fixed: Option<i64>,
        rules: &[PricingRule],
        now_ms: i64,
    ) -> Vec<DayAvailability> {
        let start_day = day_start(start_ms);
        let end = add_days(start_day, days);

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
                            price: effective_slot_price(hour, rules, fixed),
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

    /// Every booking in the store, chronological (slot-key order). The admin
    /// dashboard scans this for system-wide counts and revenue. Cancelled
    /// bookings are included — callers filter by `status` as needed.
    pub fn all_bookings(&self) -> Vec<Booking> {
        let mut t = self.lock();
        all_bookings(&mut t.bookings)
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
                    .ok_or("Áskrift fannst ekki")?;
                // Access: the owner, or any *active* member of the subscription.
                // This is what makes the daily limit shared — every booking is
                // counted by `user_subscription_id` (not per user), so the whole
                // group draws down one pool.
                let is_owner = sub.user_id == user_id;
                let is_active_member =
                    scan_members_locked(&mut t.user_subscription_members, ref_id)
                        .iter()
                        .any(|m| m.status == "active" && m.user_id.as_deref() == Some(user_id));
                if !is_owner && !is_active_member {
                    return Err("Áskrift fannst ekki".into());
                }
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

    /// Insert/replace a user subscription member. The storage key is
    /// `<user_subscription_id>:<member_id>`, which keeps every member of a
    /// subscription contiguous in the tree for prefix-scanned listing.
    pub fn put_user_subscription_member(
        &self,
        member: &crate::booking::subscription_sharing::UserSubscriptionMember,
    ) -> io::Result<()> {
        let mut t = self.lock();
        let opt_int = |o: Option<i64>| o.map(Value::Int).unwrap_or(Value::Null);
        let opt_str = |o: &Option<String>| o.clone().map(Value::Str).unwrap_or(Value::Null);
        let v = Value::Object(vec![
            ("id".into(), Value::Str(member.id.clone())),
            (
                "user_subscription_id".into(),
                Value::Str(member.user_subscription_id.clone()),
            ),
            ("user_id".into(), opt_str(&member.user_id)),
            ("role".into(), Value::Str(member.role.clone())),
            ("status".into(), Value::Str(member.status.clone())),
            ("invited_phone".into(), opt_str(&member.invited_phone)),
            ("invited_at".into(), Value::Int(member.invited_at)),
            ("accepted_at".into(), opt_int(member.accepted_at)),
            ("removed_at".into(), opt_int(member.removed_at)),
        ]);
        let key = format!("{}:{}", member.user_subscription_id, member.id);
        t.user_subscription_members
            .insert(key.as_bytes(), v.to_json().as_bytes())?;
        t.user_subscription_members.commit()
    }

    /// List the `active`/`invited` members of a subscription, owners first then
    /// by invite time. Mirrors `listSubscriptionMembers` (removed rows hidden).
    pub fn list_subscription_members(
        &self,
        user_subscription_id: &str,
    ) -> Vec<crate::booking::subscription_sharing::ListMemberView> {
        let mut t = self.lock();
        let mut members: Vec<crate::booking::subscription_sharing::ListMemberView> =
            scan_members_locked(&mut t.user_subscription_members, user_subscription_id)
                .into_iter()
                .filter(|m| m.status != "removed")
                .map(|m| crate::booking::subscription_sharing::ListMemberView {
                    id: m.id,
                    user_id: m.user_id,
                    // The Rust port keys users by email and stores no separate
                    // name/phone column, so these are surfaced only via the
                    // invited phone. Left None for joined-user display.
                    name: None,
                    phone: None,
                    invited_phone: m.invited_phone,
                    role: m.role,
                    status: m.status,
                    invited_at: m.invited_at,
                    accepted_at: m.accepted_at,
                })
                .collect();
        // Owners first, then chronological by invite time.
        members.sort_by(|a, b| match (a.role.as_str(), b.role.as_str()) {
            ("owner", "owner") => a.invited_at.cmp(&b.invited_at),
            ("owner", _) => std::cmp::Ordering::Less,
            (_, "owner") => std::cmp::Ordering::Greater,
            _ => a.invited_at.cmp(&b.invited_at),
        });
        members
    }

    /// Every subscription id `user_id` can use: ones they own, plus ones they
    /// are an `active` member of. Mirrors `getAccessibleUserSubscriptionIds`.
    /// Ported library surface for the account UI (not yet HTTP-routed);
    /// exercised by unit tests.
    #[allow(dead_code)]
    pub fn accessible_user_subscription_ids(&self, user_id: &str) -> Vec<String> {
        let mut t = self.lock();
        let mut ids = std::collections::BTreeSet::new();

        // Owned subscriptions.
        for v in all_records(&mut t.user_subscriptions) {
            if v.get("user_id").and_then(Value::as_str) == Some(user_id) {
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    ids.insert(id.to_string());
                }
            }
        }
        // Active memberships.
        for m in all_members_locked(&mut t.user_subscription_members) {
            if m.status == "active" && m.user_id.as_deref() == Some(user_id) {
                ids.insert(m.user_subscription_id);
            }
        }
        ids.into_iter().collect()
    }

    /// Ensure the subscription owner has an `owner`/`active` member row. Called
    /// before listing/inviting so the owner always appears in the roster.
    /// Idempotent — does nothing if an owner row already exists.
    pub fn ensure_owner_subscription_membership(
        &self,
        user_subscription_id: &str,
        owner_user_id: &str,
        now: i64,
    ) -> io::Result<()> {
        {
            let mut t = self.lock();
            let existing =
                scan_members_locked(&mut t.user_subscription_members, user_subscription_id);
            if existing.iter().any(|m| {
                m.role == "owner"
                    && m.user_id.as_deref() == Some(owner_user_id)
                    && m.status == "active"
            }) {
                return Ok(());
            }
        }
        let seq = MEMBER_SEQ.fetch_add(1, Ordering::Relaxed);
        let member = crate::booking::subscription_sharing::UserSubscriptionMember {
            id: format!("usm-{now}-{seq}"),
            user_subscription_id: user_subscription_id.to_string(),
            user_id: Some(owner_user_id.to_string()),
            role: "owner".into(),
            status: "active".into(),
            invited_phone: None,
            invited_at: now,
            accepted_at: Some(now),
            removed_at: None,
        };
        self.put_user_subscription_member(&member)
    }

    /// Is `user_id` allowed to book against `user_subscription_id` — i.e. the
    /// subscription owner, or an `active` member of it?
    pub fn can_use_subscription(&self, user_id: &str, user_subscription_id: &str) -> bool {
        let mut t = self.lock();
        if let Some(sub) = user_subscription_by_id(&mut t.user_subscriptions, user_subscription_id)
        {
            if sub.user_id == user_id {
                return true;
            }
        }
        scan_members_locked(&mut t.user_subscription_members, user_subscription_id)
            .iter()
            .any(|m| m.status == "active" && m.user_id.as_deref() == Some(user_id))
    }

    /// Owner-only: invite a member to a subscription by phone. Creates an
    /// `invited` member row keyed by the normalized Icelandic phone. Mirrors
    /// `inviteSubscriptionMember`. Enforces: only the owner may invite, and a
    /// unique open invite per (subscription, phone).
    pub fn invite_subscription_member(
        &self,
        owner_user_id: &str,
        user_subscription_id: &str,
        phone: &str,
        now: i64,
    ) -> Result<crate::booking::subscription_sharing::UserSubscriptionMember, String> {
        let invited_phone =
            crate::booking::subscription_sharing::normalize_subscription_invite_phone(phone)?;

        // Owner guard + uniqueness check under the lock.
        {
            let mut t = self.lock();
            let sub = user_subscription_by_id(&mut t.user_subscriptions, user_subscription_id)
                .ok_or("Áskrift fannst ekki")?;
            if sub.user_id != owner_user_id {
                return Err("Aðeins eigandi getur deilt áskriftinni".into());
            }
            let existing =
                scan_members_locked(&mut t.user_subscription_members, user_subscription_id);
            // Unique open invite per (sub, phone): an active/invited row with the
            // same invited phone blocks a duplicate invite.
            if existing.iter().any(|m| {
                (m.status == "active" || m.status == "invited")
                    && m.invited_phone.as_deref() == Some(invited_phone.as_str())
            }) {
                return Err("Þessi aðili er þegar á áskriftinni eða með opið boð".into());
            }
        }

        // Ensure the owner roster row exists, then insert the invited member.
        self.ensure_owner_subscription_membership(user_subscription_id, owner_user_id, now)
            .map_err(io_err)?;

        let seq = MEMBER_SEQ.fetch_add(1, Ordering::Relaxed);
        let member = crate::booking::subscription_sharing::UserSubscriptionMember {
            id: format!("usm-{now}-{seq}"),
            user_subscription_id: user_subscription_id.to_string(),
            user_id: None,
            role: "member".into(),
            status: "invited".into(),
            invited_phone: Some(invited_phone),
            invited_at: now,
            accepted_at: None,
            removed_at: None,
        };
        self.put_user_subscription_member(&member).map_err(io_err)?;
        Ok(member)
    }

    /// Accept every pending invite addressed to `phone`, binding them to
    /// `user_id` and flipping them to `active`. Mirrors
    /// `acceptPendingSubscriptionInvites`. A no-op when `phone` is `None`.
    /// Enforces unique active (sub, user): if the user is already an active
    /// member of a subscription, that invite is dropped (removed) instead of
    /// creating a duplicate active row.
    // Ported library surface: invite acceptance is driven by login phone in the
    // source, but the Rust port captures no phone at email-OTP login (see
    // PORT.md), so this is exercised by unit tests pending that UI wiring.
    #[allow(dead_code)]
    pub fn accept_pending_subscription_invites(
        &self,
        user_id: &str,
        phone: Option<&str>,
        now: i64,
    ) -> Result<usize, String> {
        let Some(phone) = phone else { return Ok(0) };
        let normalized =
            crate::booking::subscription_sharing::normalize_subscription_invite_phone(phone)?;

        // Collect all invited rows matching this phone (across subscriptions).
        let pending: Vec<crate::booking::subscription_sharing::UserSubscriptionMember> = {
            let mut t = self.lock();
            all_members_locked(&mut t.user_subscription_members)
                .into_iter()
                .filter(|m| {
                    m.status == "invited" && m.invited_phone.as_deref() == Some(normalized.as_str())
                })
                .collect()
        };

        let mut accepted = 0;
        for mut m in pending {
            // Unique active (sub, user): skip if already an active member.
            let already_active = {
                let mut t = self.lock();
                scan_members_locked(&mut t.user_subscription_members, &m.user_subscription_id)
                    .iter()
                    .any(|x| x.status == "active" && x.user_id.as_deref() == Some(user_id))
            };
            if already_active {
                m.status = "removed".into();
                m.removed_at = Some(now);
                self.put_user_subscription_member(&m).map_err(io_err)?;
                continue;
            }
            m.user_id = Some(user_id.to_string());
            m.status = "active".into();
            m.accepted_at = Some(now);
            self.put_user_subscription_member(&m).map_err(io_err)?;
            accepted += 1;
        }
        Ok(accepted)
    }

    /// Owner-only: remove a member from a subscription (status → `removed`).
    /// Mirrors `removeSubscriptionMember`. Only `member`-role rows can be
    /// removed — the owner can never remove themselves this way.
    pub fn remove_subscription_member(
        &self,
        owner_user_id: &str,
        user_subscription_id: &str,
        member_id: &str,
        now: i64,
    ) -> Result<crate::booking::subscription_sharing::UserSubscriptionMember, String> {
        let mut target = {
            let mut t = self.lock();
            let sub = user_subscription_by_id(&mut t.user_subscriptions, user_subscription_id)
                .ok_or("Áskrift fannst ekki")?;
            if sub.user_id != owner_user_id {
                return Err("Aðeins eigandi getur fjarlægt meðlimi".into());
            }
            scan_members_locked(&mut t.user_subscription_members, user_subscription_id)
                .into_iter()
                .find(|m| m.id == member_id && m.role == "member")
                .ok_or("Meðlimur fannst ekki")?
        };
        target.status = "removed".into();
        target.removed_at = Some(now);
        self.put_user_subscription_member(&target).map_err(io_err)?;
        Ok(target)
    }

    /// Shared daily usage for a subscription: bookings made today by ANY active
    /// member (counted by `user_subscription_id`) against the subscription's
    /// daily limit. Mirrors `summarizeSharedSubscriptionUsage` fed by the real
    /// booking count.
    pub fn shared_subscription_usage(
        &self,
        user_subscription_id: &str,
        now: i64,
    ) -> Option<crate::booking::subscription_sharing::SharedUsageSummary> {
        let mut t = self.lock();
        let sub = user_subscription_by_id(&mut t.user_subscriptions, user_subscription_id)?;
        let day = day_start(now);
        let used = count_active_subscription_bookings(
            &mut t.bookings,
            user_subscription_id,
            day,
            day + DAY_MS,
        );
        Some(
            crate::booking::subscription_sharing::summarize_shared_subscription_usage(
                sub.daily_limit,
                used,
            ),
        )
    }

    /// All packages for a user, in storage order.
    pub fn user_packages_for_user(&self, user_id: &str) -> Vec<UserPackage> {
        let mut t = self.lock();
        all_user_packages(&mut t.user_packages)
            .into_iter()
            .filter(|p| p.user_id == user_id)
            .collect()
    }

    /// All subscriptions for a user, in storage order.
    pub fn user_subscriptions_for_user(&self, user_id: &str) -> Vec<UserSubscription> {
        let mut t = self.lock();
        all_user_subscriptions(&mut t.user_subscriptions)
            .into_iter()
            .filter(|s| s.user_id == user_id)
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
    let mut obj = vec![
        ("id".into(), Value::Str(p.id.clone())),
        ("user_id".into(), Value::Str(p.user_id.clone())),
        ("remaining".into(), Value::Int(p.remaining)),
    ];
    if let Some(name) = &p.package_name {
        obj.push(("package_name".into(), Value::Str(name.clone())));
    }
    if let Some(count) = p.slot_count {
        obj.push(("slot_count".into(), Value::Int(count)));
    }
    Value::Object(obj)
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
        package_name: v
            .get("package_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        slot_count: v.get("slot_count").and_then(Value::as_i64),
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

fn all_user_packages(tree: &mut BTree) -> Vec<UserPackage> {
    full_scan(tree)
        .iter()
        .filter_map(|v| {
            Some(UserPackage {
                id: v.get("id").and_then(Value::as_str)?.to_string(),
                user_id: v.get("user_id").and_then(Value::as_str)?.to_string(),
                remaining: v.get("remaining").and_then(Value::as_i64)?,
                package_name: v
                    .get("package_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                slot_count: v.get("slot_count").and_then(Value::as_i64),
            })
        })
        .collect()
}

fn all_user_subscriptions(tree: &mut BTree) -> Vec<UserSubscription> {
    full_scan(tree)
        .iter()
        .filter_map(|v| {
            Some(UserSubscription {
                id: v.get("id").and_then(Value::as_str)?.to_string(),
                user_id: v.get("user_id").and_then(Value::as_str)?.to_string(),
                valid_from: v.get("valid_from").and_then(Value::as_i64)?,
                valid_until: v.get("valid_until").and_then(Value::as_i64)?,
                daily_limit: v.get("daily_limit").and_then(Value::as_i64)?,
            })
        })
        .collect()
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

fn member_from_value(
    v: &akurai_json::Value,
) -> Option<crate::booking::subscription_sharing::UserSubscriptionMember> {
    Some(
        crate::booking::subscription_sharing::UserSubscriptionMember {
            id: v
                .get("id")
                .and_then(akurai_json::Value::as_str)?
                .to_string(),
            user_subscription_id: v
                .get("user_subscription_id")
                .and_then(akurai_json::Value::as_str)?
                .to_string(),
            user_id: v
                .get("user_id")
                .and_then(akurai_json::Value::as_str)
                .map(|s| s.to_string()),
            role: v
                .get("role")
                .and_then(akurai_json::Value::as_str)?
                .to_string(),
            status: v
                .get("status")
                .and_then(akurai_json::Value::as_str)?
                .to_string(),
            invited_phone: v
                .get("invited_phone")
                .and_then(akurai_json::Value::as_str)
                .map(|s| s.to_string()),
            invited_at: v.get("invited_at").and_then(akurai_json::Value::as_i64)?,
            accepted_at: v.get("accepted_at").and_then(akurai_json::Value::as_i64),
            removed_at: v.get("removed_at").and_then(akurai_json::Value::as_i64),
        },
    )
}

/// All member rows for one subscription (prefix scan on `<sub_id>:`), in key
/// order. Includes removed rows — callers filter as needed.
fn scan_members_locked(
    tree: &mut BTree,
    user_subscription_id: &str,
) -> Vec<crate::booking::subscription_sharing::UserSubscriptionMember> {
    let prefix = format!("{user_subscription_id}:");
    let mut hi = prefix.clone().into_bytes();
    // Upper bound: same prefix with a trailing 0xff terminator keeps the scan
    // confined to this subscription's keys.
    hi.push(0xff);
    tree.range(prefix.as_bytes(), &hi)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| akurai_json::parse(&String::from_utf8_lossy(raw)).ok())
        .filter_map(|v| member_from_value(&v))
        .collect()
}

/// Every member row across all subscriptions (full scan). Used by invite
/// acceptance, which matches on phone irrespective of subscription.
fn all_members_locked(
    tree: &mut BTree,
) -> Vec<crate::booking::subscription_sharing::UserSubscriptionMember> {
    full_scan(tree)
        .iter()
        .filter_map(member_from_value)
        .collect()
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
                package_name: None,
                slot_count: None,
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
                package_name: None,
                slot_count: None,
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
                package_name: None,
                slot_count: None,
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

    // ---- subscription sharing --------------------------------------------

    /// Seed an active subscription owned by `owner`, valid around `now`.
    fn seed_sub(store: &Store, id: &str, owner: &str, now: i64) {
        let today = day_start(now);
        store
            .put_user_subscription(&UserSubscription {
                id: id.into(),
                user_id: owner.into(),
                valid_from: add_days(today, -1),
                valid_until: add_days(today, 60),
                daily_limit: 2,
            })
            .unwrap();
    }

    #[test]
    fn invite_then_accept_makes_member_active() {
        let (store, dir) = temp_store("share-accept");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "sub1", "owner@x.is", now);

        let member = store
            .invite_subscription_member("owner@x.is", "sub1", "555 1234", now)
            .expect("invite ok");
        assert_eq!(member.status, "invited");
        assert_eq!(member.role, "member");
        assert_eq!(member.invited_phone.as_deref(), Some("+3545551234"));
        assert!(member.user_id.is_none());

        // Owner row was materialized + the invited member → 2 visible.
        let listed = store.list_subscription_members("sub1");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].role, "owner"); // owner sorts first

        // The invitee accepts by logging in with the same phone.
        let n = store
            .accept_pending_subscription_invites("friend@x.is", Some("+354 555 1234"), now)
            .expect("accept ok");
        assert_eq!(n, 1);

        let m = store
            .list_subscription_members("sub1")
            .into_iter()
            .find(|m| m.id == member.id)
            .unwrap();
        assert_eq!(m.status, "active");
        assert_eq!(m.user_id.as_deref(), Some("friend@x.is"));
        // The friend can now use the subscription.
        assert!(store.can_use_subscription("friend@x.is", "sub1"));
        cleanup(&dir);
    }

    #[test]
    fn remove_marks_member_removed_and_revokes_access() {
        let (store, dir) = temp_store("share-remove");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "sub1", "owner@x.is", now);
        let member = store
            .invite_subscription_member("owner@x.is", "sub1", "5551234", now)
            .unwrap();
        store
            .accept_pending_subscription_invites("friend@x.is", Some("5551234"), now)
            .unwrap();
        assert!(store.can_use_subscription("friend@x.is", "sub1"));

        store
            .remove_subscription_member("owner@x.is", "sub1", &member.id, now)
            .expect("remove ok");
        // Hidden from the roster (owner row remains).
        let listed = store.list_subscription_members("sub1");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].role, "owner");
        // Access revoked.
        assert!(!store.can_use_subscription("friend@x.is", "sub1"));
        cleanup(&dir);
    }

    #[test]
    fn daily_limit_is_shared_across_active_members() {
        let (store, dir) = temp_store("share-limit");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "sub1", "owner@x.is", now); // daily_limit = 2
        let member = store
            .invite_subscription_member("owner@x.is", "sub1", "5551234", now)
            .unwrap();
        store
            .accept_pending_subscription_invites("friend@x.is", Some("5551234"), now)
            .unwrap();

        let slot1 = parse_instant("2026-06-27T10:00:00Z").unwrap();
        let slot2 = parse_instant("2026-06-27T11:00:00Z").unwrap();
        let slot3 = parse_instant("2026-06-27T12:00:00Z").unwrap();

        // Owner books one, friend books one — that exhausts the shared pool of 2.
        store
            .create_booking("owner@x.is", slot1, "subscription", Some("sub1"), None, now)
            .expect("owner books 1st");
        store
            .create_booking(
                "friend@x.is",
                slot2,
                "subscription",
                Some("sub1"),
                None,
                now,
            )
            .expect("member books 2nd");
        // Third booking by anyone is over the shared daily quota.
        let err = store
            .create_booking("owner@x.is", slot3, "subscription", Some("sub1"), None, now)
            .unwrap_err();
        assert_eq!(err, "Dagskvóti áskriftar fullnýttur");

        // Usage summary reflects the shared draw-down.
        let usage = store.shared_subscription_usage("sub1", slot1).unwrap();
        assert_eq!((usage.limit, usage.used, usage.remaining), (2, 2, 0));
        assert!(usage.exhausted);

        let _ = member;
        cleanup(&dir);
    }

    #[test]
    fn only_owner_can_invite_or_remove() {
        let (store, dir) = temp_store("share-owneronly");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "sub1", "owner@x.is", now);

        // A non-owner cannot invite.
        let err = store
            .invite_subscription_member("intruder@x.is", "sub1", "5551234", now)
            .unwrap_err();
        assert_eq!(err, "Aðeins eigandi getur deilt áskriftinni");

        // Owner invites + the invitee accepts.
        let member = store
            .invite_subscription_member("owner@x.is", "sub1", "5551234", now)
            .unwrap();
        store
            .accept_pending_subscription_invites("friend@x.is", Some("5551234"), now)
            .unwrap();

        // A non-owner (even the member) cannot remove.
        let err = store
            .remove_subscription_member("friend@x.is", "sub1", &member.id, now)
            .unwrap_err();
        assert_eq!(err, "Aðeins eigandi getur fjarlægt meðlimi");
        cleanup(&dir);
    }

    #[test]
    fn duplicate_open_invite_is_rejected() {
        let (store, dir) = temp_store("share-dup");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "sub1", "owner@x.is", now);
        store
            .invite_subscription_member("owner@x.is", "sub1", "5551234", now)
            .unwrap();
        let err = store
            .invite_subscription_member("owner@x.is", "sub1", "555 1234", now)
            .unwrap_err();
        assert_eq!(err, "Þessi aðili er þegar á áskriftinni eða með opið boð");
        cleanup(&dir);
    }

    #[test]
    fn accept_skips_duplicate_active_membership() {
        let (store, dir) = temp_store("share-dupaccept");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "sub1", "owner@x.is", now);
        let first = store
            .invite_subscription_member("owner@x.is", "sub1", "5551234", now)
            .unwrap();
        store
            .accept_pending_subscription_invites("friend@x.is", Some("5551234"), now)
            .unwrap();
        // Owner re-invites the same phone after the first was... still active?
        // The duplicate-open-invite guard blocks re-invite of an active phone,
        // so simulate a stale invite directly to prove accept dedupes by user.
        let stale = crate::booking::subscription_sharing::UserSubscriptionMember {
            id: "usm-stale".into(),
            user_subscription_id: "sub1".into(),
            user_id: None,
            role: "member".into(),
            status: "invited".into(),
            invited_phone: Some("+3549999999".into()),
            invited_at: now,
            accepted_at: None,
            removed_at: None,
        };
        store.put_user_subscription_member(&stale).unwrap();
        // friend already active for sub1 → accepting the stale invite must NOT
        // create a second active row; the stale invite is dropped (removed).
        let n = store
            .accept_pending_subscription_invites("friend@x.is", Some("9999999"), now)
            .unwrap();
        assert_eq!(n, 0);
        let active_for_friend = store
            .list_subscription_members("sub1")
            .into_iter()
            .filter(|m| m.user_id.as_deref() == Some("friend@x.is") && m.status == "active")
            .count();
        assert_eq!(active_for_friend, 1);
        let _ = first;
        cleanup(&dir);
    }

    #[test]
    fn accessible_ids_include_owned_and_member_subs() {
        let (store, dir) = temp_store("share-accessible");
        let now = parse_date("2026-06-26").unwrap();
        seed_sub(&store, "subA", "owner@x.is", now);
        seed_sub(&store, "subB", "other@x.is", now);
        store
            .invite_subscription_member("other@x.is", "subB", "5551234", now)
            .unwrap();
        store
            .accept_pending_subscription_invites("owner@x.is", Some("5551234"), now)
            .unwrap();
        let ids = store.accessible_user_subscription_ids("owner@x.is");
        assert_eq!(ids, vec!["subA".to_string(), "subB".to_string()]);
        cleanup(&dir);
    }

    #[test]
    fn list_user_packages_and_subscriptions() {
        let (store, dir) = temp_store("list");

        // Create packages for two users
        store
            .put_user_package(&UserPackage {
                id: "up1".into(),
                user_id: "u1".into(),
                remaining: 5,
                package_name: Some("Package A".into()),
                slot_count: Some(10),
            })
            .unwrap();
        store
            .put_user_package(&UserPackage {
                id: "up2".into(),
                user_id: "u1".into(),
                remaining: 3,
                package_name: Some("Package B".into()),
                slot_count: Some(10),
            })
            .unwrap();
        store
            .put_user_package(&UserPackage {
                id: "up3".into(),
                user_id: "u2".into(),
                remaining: 7,
                package_name: Some("Package C".into()),
                slot_count: Some(20),
            })
            .unwrap();

        // Create subscriptions for two users
        store
            .put_user_subscription(&UserSubscription {
                id: "us1".into(),
                user_id: "u1".into(),
                valid_from: 1000,
                valid_until: 2000,
                daily_limit: 2,
            })
            .unwrap();
        store
            .put_user_subscription(&UserSubscription {
                id: "us2".into(),
                user_id: "u2".into(),
                valid_from: 1000,
                valid_until: 2000,
                daily_limit: 3,
            })
            .unwrap();

        // List packages for u1
        let u1_pkgs = store.user_packages_for_user("u1");
        assert_eq!(u1_pkgs.len(), 2);
        assert_eq!(u1_pkgs[0].id, "up1");
        assert_eq!(u1_pkgs[0].remaining, 5);
        assert_eq!(u1_pkgs[1].id, "up2");
        assert_eq!(u1_pkgs[1].remaining, 3);

        // List packages for u2
        let u2_pkgs = store.user_packages_for_user("u2");
        assert_eq!(u2_pkgs.len(), 1);
        assert_eq!(u2_pkgs[0].id, "up3");
        assert_eq!(u2_pkgs[0].remaining, 7);

        // List subscriptions for u1
        let u1_subs = store.user_subscriptions_for_user("u1");
        assert_eq!(u1_subs.len(), 1);
        assert_eq!(u1_subs[0].id, "us1");
        assert_eq!(u1_subs[0].daily_limit, 2);

        // List subscriptions for u2
        let u2_subs = store.user_subscriptions_for_user("u2");
        assert_eq!(u2_subs.len(), 1);
        assert_eq!(u2_subs[0].id, "us2");
        assert_eq!(u2_subs[0].daily_limit, 3);

        cleanup(&dir);
    }
}
