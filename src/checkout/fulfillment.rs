//! Cart -> order fulfillment. The correctness-critical core, ported from the
//! source `src/lib/cart/fulfillment.ts` (`fulfillCart`).
//!
//! For a checking-out cart whose payment succeeded, this walks the cart's line
//! items and makes them real:
//!
//! * `slot` -> a confirmed booking via the booking store's atomic
//!   `create_booking` (single price recorded, or a package slot decremented /
//!   subscription quota validated).
//! * `package` -> one `user_package` grant per quantity, with the package's
//!   slot count as `remaining`.
//! * `subscription` -> a `user_subscription` window grant.
//! * `gift_card` -> an issued gift card (active, full balance).
//! * `product` -> no-op (no inventory in this port).
//!
//! Then the cart is marked `paid` and the payment `succeeded`. A cart already
//! `paid` short-circuits, so a duplicated callback never double-fulfills.
//!
//! Atomicity matches the booking store's model: the whole walk runs under the
//! checkout store's `fulfill_lock`, the booking tree's keyed uniqueness is the
//! no-double-booking guard, and each store commits atomically. A slot already
//! taken by another booking aborts fulfillment with `slot_taken_after_payment`,
//! exactly as the source raises `FulfillmentError`.

use super::Store as PaymentStore;
use crate::booking::time;
use crate::booking::{Store as BookingStore, UserPackage, UserSubscription};
use crate::cart::{CartItem, Store as CartStore};
use akurai_json::Value;

/// A fulfillment failure, carrying a payment-error `code` (used to pick the
/// cancel-page message and the payment audit) and a human message.
#[derive(Debug, Clone)]
pub struct FulfillError {
    pub code: String,
    pub message: String,
}

impl FulfillError {
    fn new(code: &str, message: &str) -> FulfillError {
        FulfillError {
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

/// 90 days in ms — the default subscription window when none is in metadata.
const DEFAULT_SUB_WINDOW_MS: i64 = 90 * 24 * 3600 * 1000;

/// Fulfill a cart against a succeeded payment. See module docs.
pub fn fulfill_cart(
    payments: &PaymentStore,
    carts: &CartStore,
    bookings: &BookingStore,
    cart_id: &str,
    provider_ref: &str,
    now_ms: i64,
) -> Result<(), FulfillError> {
    // Serialize this cart's fulfillment against any concurrent callback.
    let _guard = payments.fulfill_lock();

    let (cart, items) = carts
        .load_cart(cart_id)
        .ok_or_else(|| FulfillError::new("payment_succeeded_booking_failed", "cart not found"))?;
    let payment = payments
        .by_cart_and_ref(cart_id, provider_ref)
        .ok_or_else(|| {
            FulfillError::new("payment_succeeded_booking_failed", "payment row missing")
        })?;

    // The user id behind the cart. The booking engine keys users by email, which
    // is exactly the payment's `user_id` (set at checkout from the session).
    let user_id = payment.user_id.clone();

    // Idempotency: an already-paid cart just ensures the payment is succeeded.
    if cart.status == "paid" {
        if payment.status != "succeeded" {
            let _ = payments.mark_succeeded(&payment.id, now_ms);
        }
        return Ok(());
    }

    for item in &items {
        match item.item_type.as_str() {
            "package" => grant_package(bookings, &user_id, item, now_ms)?,
            "subscription" => grant_subscription(bookings, &user_id, item, now_ms)?,
            "gift_card" => issue_gift_card(payments, cart_id, &user_id, item, now_ms)?,
            "product" => {}
            "slot" => fulfill_slot(bookings, &user_id, item, now_ms)?,
            _ => {}
        }
    }

    carts
        .set_status(cart_id, "paid", now_ms)
        .map_err(|e| FulfillError::new("payment_succeeded_booking_failed", &e))?;
    payments
        .mark_succeeded(&payment.id, now_ms)
        .map_err(|e| FulfillError::new("payment_succeeded_booking_failed", &e))?;
    // The cart's lines are now consumed; clear them so a fresh cart on the same
    // (deterministic, per-user) id starts empty.
    let _ = carts.clear_items(cart_id);
    Ok(())
}

/// Mark a cart + its payment failed (mirrors the source `markCartFailed`).
/// Best-effort: storage errors here are swallowed because the caller is already
/// on an error path and the user is being redirected to the cancel page.
pub fn mark_cart_failed(
    payments: &PaymentStore,
    carts: &CartStore,
    cart_id: &str,
    provider_ref: &str,
    error_code: &str,
    now_ms: i64,
) {
    let _ = carts.set_status(cart_id, "failed", now_ms);
    if let Some(payment) = payments.by_cart_and_ref(cart_id, provider_ref) {
        let _ = payments.mark_failed(&payment.id, error_code, now_ms);
    }
}

// ---- per-item handlers ----------------------------------------------------

/// Grant `quantity` klippikort packages, each with the package's slot count as
/// `remaining` (source: insert one `userPackages` row per unit).
fn grant_package(
    bookings: &BookingStore,
    user_id: &str,
    item: &CartItem,
    now_ms: i64,
) -> Result<(), FulfillError> {
    let slot_count = meta_int(item, &["slotCount", "slots", "remaining"])
        .unwrap_or(1)
        .max(1);
    for _ in 0..item.quantity.max(1) {
        let pkg = UserPackage {
            id: super::next_id("up", now_ms),
            user_id: user_id.to_string(),
            remaining: slot_count,
            package_name: Some(item.name_snapshot.clone()),
            slot_count: Some(slot_count),
        };
        bookings
            .put_user_package(&pkg)
            .map_err(|e| FulfillError::new("payment_succeeded_booking_failed", &e.to_string()))?;
    }
    Ok(())
}

/// Grant a subscription window. `validFrom`/`validUntil` (ISO or epoch-ms) and
/// `dailyLimit` come from the item metadata, with sane defaults.
fn grant_subscription(
    bookings: &BookingStore,
    user_id: &str,
    item: &CartItem,
    now_ms: i64,
) -> Result<(), FulfillError> {
    let valid_from = meta_instant(item, "validFrom").unwrap_or(now_ms);
    let valid_until = meta_instant(item, "validUntil").unwrap_or(now_ms + DEFAULT_SUB_WINDOW_MS);
    let daily_limit = meta_int(item, &["dailyLimit", "daily_limit"])
        .unwrap_or(2)
        .max(1);
    for _ in 0..item.quantity.max(1) {
        let sub = UserSubscription {
            id: super::next_id("us", now_ms),
            user_id: user_id.to_string(),
            valid_from,
            valid_until,
            daily_limit,
        };
        bookings
            .put_user_subscription(&sub)
            .map_err(|e| FulfillError::new("payment_succeeded_booking_failed", &e.to_string()))?;
    }
    Ok(())
}

/// Issue `quantity` gift cards, each for the item's unit price.
fn issue_gift_card(
    payments: &PaymentStore,
    cart_id: &str,
    user_id: &str,
    item: &CartItem,
    now_ms: i64,
) -> Result<(), FulfillError> {
    let currency = meta_str(item, "currency").unwrap_or("ISK");
    for _ in 0..item.quantity.max(1) {
        payments
            .create_gift_card(
                item.unit_price,
                currency,
                meta_str(item, "recipientEmail"),
                meta_str(item, "recipientPhone"),
                meta_str(item, "recipientName"),
                meta_str(item, "message"),
                user_id,
                cart_id,
                now_ms,
            )
            .map_err(|e| FulfillError::new("payment_succeeded_booking_failed", &e))?;
    }
    Ok(())
}

/// Turn a slot line item into a confirmed booking, deferring all the accounting
/// (conflict check, package decrement, subscription quota, single price) to the
/// booking store's atomic `create_booking`.
fn fulfill_slot(
    bookings: &BookingStore,
    user_id: &str,
    item: &CartItem,
    now_ms: i64,
) -> Result<(), FulfillError> {
    // The slot instant: metadata.startsAt, falling back to the item's refId
    // (which `resolve_slot_item` sets to the same ISO string).
    let starts = meta_str(item, "startsAt")
        .map(str::to_string)
        .unwrap_or_else(|| item.ref_id.clone());
    let slot_ms = time::parse_instant(&starts).ok_or_else(|| {
        FulfillError::new(
            "payment_succeeded_booking_failed",
            &format!("slot item {} has invalid startsAt", item.id),
        )
    })?;

    // Payment type (mirrors the source's precedence): an explicit
    // userSubscriptionId/userPackageId or a paymentType tag wins; else `single`.
    let has_sub = meta_str(item, "userSubscriptionId").is_some()
        || meta_str(item, "paymentType") == Some("subscription");
    let has_pkg = meta_str(item, "userPackageId").is_some()
        || meta_str(item, "paymentType") == Some("package");
    let (payment_type, ref_id) = if has_sub {
        ("subscription", meta_str(item, "userSubscriptionId"))
    } else if has_pkg {
        ("package", meta_str(item, "userPackageId"))
    } else {
        ("single", None)
    };

    match bookings.create_booking(user_id, slot_ms, payment_type, ref_id, None, now_ms) {
        Ok(_) => Ok(()),
        Err(e) => {
            let code = if e.contains("þegar bókaður") {
                "slot_taken_after_payment"
            } else {
                "payment_succeeded_booking_failed"
            };
            Err(FulfillError::new(code, &e))
        }
    }
}

// ---- metadata accessors ---------------------------------------------------

fn meta_str<'a>(item: &'a CartItem, key: &str) -> Option<&'a str> {
    item.metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn meta_int(item: &CartItem, keys: &[&str]) -> Option<i64> {
    for key in keys {
        match item.metadata.get(key) {
            Some(Value::Int(n)) => return Some(*n),
            Some(Value::Float(n)) if n.is_finite() => return Some(*n as i64),
            Some(Value::Str(s)) => {
                if let Ok(n) = s.trim().parse::<i64>() {
                    return Some(n);
                }
            }
            _ => {}
        }
    }
    None
}

/// An instant from metadata: an epoch-ms integer, or an ISO-8601 string.
fn meta_instant(item: &CartItem, key: &str) -> Option<i64> {
    match item.metadata.get(key) {
        Some(Value::Int(n)) => Some(*n),
        Some(Value::Float(n)) if n.is_finite() => Some(*n as i64),
        Some(Value::Str(s)) => {
            time::parse_instant(s.trim()).or_else(|| s.trim().parse::<i64>().ok())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::ResolvedItem;
    use std::path::PathBuf;

    fn dirs(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "gsd-fulfill-{tag}-{}-{}",
            std::process::id(),
            now_ms_seq()
        ));
        let _ = std::fs::remove_dir_all(&base);
        (
            base.join("checkout"),
            base.join("cart"),
            base.join("booking"),
        )
    }

    fn now_ms_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static S: AtomicU64 = AtomicU64::new(0);
        S.fetch_add(1, Ordering::Relaxed)
    }

    fn slot_item(starts_iso: &str, price: i64) -> ResolvedItem {
        ResolvedItem {
            item_type: "slot".into(),
            ref_id: starts_iso.into(),
            name_snapshot: format!("Rástími {starts_iso}"),
            unit_price: price,
            quantity: 1,
            metadata: Value::Object(vec![("startsAt".into(), Value::Str(starts_iso.into()))]),
        }
    }

    fn package_item(slot_count: i64) -> ResolvedItem {
        ResolvedItem {
            item_type: "package".into(),
            ref_id: "pkg-catalog-1".into(),
            name_snapshot: "Klippikort 10".into(),
            unit_price: 30000,
            quantity: 1,
            metadata: Value::Object(vec![("slotCount".into(), Value::Int(slot_count))]),
        }
    }

    /// Cart with a slot item -> a booking is created and the cart is marked paid.
    #[test]
    fn slot_item_creates_booking_and_marks_cart_paid() {
        let (cdir, cartdir, bdir) = dirs("slot");
        let payments = PaymentStore::open(&cdir).unwrap();
        let carts = CartStore::open(&cartdir).unwrap();
        let bookings = BookingStore::open(&bdir).unwrap();

        let now = time::parse_date("2026-06-26").unwrap();
        let (cart, _) = carts.get_or_create_open(Some("u-golfer"), now).unwrap();
        carts
            .add_item(&cart.id, slot_item("2026-06-27T10:00:00Z", 3500), now)
            .unwrap();
        carts.set_status(&cart.id, "checking_out", now).unwrap();

        let payment = payments
            .create_payment(
                &cart.id,
                "golfer@x.com",
                "landsbankinn",
                "ref-s",
                "pending",
                3500,
                "ISK",
                Value::Null,
                now,
            )
            .unwrap();

        fulfill_cart(
            &payments,
            &carts,
            &bookings,
            &cart.id,
            &payment.provider_ref,
            now,
        )
        .expect("fulfillment succeeds");

        let booked = bookings.bookings_for_user("golfer@x.com");
        assert_eq!(booked.len(), 1, "one booking created");
        assert_eq!(booked[0].status, "confirmed");
        assert_eq!(booked[0].price_paid, Some(3500));

        let (cart_after, _) = carts.load_cart(&cart.id).unwrap();
        assert_eq!(cart_after.status, "paid");
        assert_eq!(
            payments.payment_by_id(&payment.id).unwrap().status,
            "succeeded"
        );
    }

    /// Cart with a package item -> a user_package is granted with the right slots.
    #[test]
    fn package_item_grants_user_package() {
        let (cdir, cartdir, bdir) = dirs("pkg");
        let payments = PaymentStore::open(&cdir).unwrap();
        let carts = CartStore::open(&cartdir).unwrap();
        let bookings = BookingStore::open(&bdir).unwrap();

        let now = time::parse_date("2026-06-26").unwrap();
        let (cart, _) = carts.get_or_create_open(Some("u-buyer"), now).unwrap();
        carts.add_item(&cart.id, package_item(10), now).unwrap();
        carts.set_status(&cart.id, "checking_out", now).unwrap();

        let payment = payments
            .create_payment(
                &cart.id,
                "buyer@x.com",
                "landsbankinn",
                "ref-p",
                "pending",
                30000,
                "ISK",
                Value::Null,
                now,
            )
            .unwrap();

        fulfill_cart(
            &payments,
            &carts,
            &bookings,
            &cart.id,
            &payment.provider_ref,
            now,
        )
        .expect("fulfillment succeeds");

        let grants = bookings.user_packages_for_user("buyer@x.com");
        assert_eq!(grants.len(), 1, "one package granted");
        assert_eq!(grants[0].remaining, 10);
        assert_eq!(carts.load_cart(&cart.id).unwrap().0.status, "paid");
    }

    /// A second fulfillment of a paid cart is a no-op (idempotent callback).
    #[test]
    fn second_fulfillment_is_idempotent() {
        let (cdir, cartdir, bdir) = dirs("idem");
        let payments = PaymentStore::open(&cdir).unwrap();
        let carts = CartStore::open(&cartdir).unwrap();
        let bookings = BookingStore::open(&bdir).unwrap();

        let now = time::parse_date("2026-06-26").unwrap();
        let (cart, _) = carts.get_or_create_open(Some("u-id"), now).unwrap();
        carts
            .add_item(&cart.id, slot_item("2026-06-28T09:00:00Z", 3500), now)
            .unwrap();
        carts.set_status(&cart.id, "checking_out", now).unwrap();
        let payment = payments
            .create_payment(
                &cart.id,
                "id@x.com",
                "landsbankinn",
                "ref-i",
                "pending",
                3500,
                "ISK",
                Value::Null,
                now,
            )
            .unwrap();

        fulfill_cart(
            &payments,
            &carts,
            &bookings,
            &cart.id,
            &payment.provider_ref,
            now,
        )
        .unwrap();
        // Re-run: must not create a second booking.
        fulfill_cart(
            &payments,
            &carts,
            &bookings,
            &cart.id,
            &payment.provider_ref,
            now,
        )
        .unwrap();
        assert_eq!(bookings.bookings_for_user("id@x.com").len(), 1);
    }

    /// A gift-card item is issued with full balance on fulfillment.
    #[test]
    fn gift_card_item_is_issued() {
        let (cdir, cartdir, bdir) = dirs("gc");
        let payments = PaymentStore::open(&cdir).unwrap();
        let carts = CartStore::open(&cartdir).unwrap();
        let bookings = BookingStore::open(&bdir).unwrap();

        let now = time::parse_date("2026-06-26").unwrap();
        let (cart, _) = carts.get_or_create_open(Some("u-gc"), now).unwrap();
        let gc = ResolvedItem {
            item_type: "gift_card".into(),
            ref_id: "gift-1".into(),
            name_snapshot: "Gjafabréf".into(),
            unit_price: 10000,
            quantity: 1,
            metadata: Value::Object(vec![(
                "recipientEmail".into(),
                Value::Str("friend@x.com".into()),
            )]),
        };
        carts.add_item(&cart.id, gc, now).unwrap();
        carts.set_status(&cart.id, "checking_out", now).unwrap();
        let payment = payments
            .create_payment(
                &cart.id,
                "gc@x.com",
                "landsbankinn",
                "ref-g",
                "pending",
                10000,
                "ISK",
                Value::Null,
                now,
            )
            .unwrap();

        fulfill_cart(
            &payments,
            &carts,
            &bookings,
            &cart.id,
            &payment.provider_ref,
            now,
        )
        .unwrap();
        let issued = payments.gift_cards_for_cart(&cart.id);
        assert_eq!(issued.len(), 1);
        assert_eq!(issued[0].balance, 10000);
        assert_eq!(issued[0].recipient_email.as_deref(), Some("friend@x.com"));
    }
}
