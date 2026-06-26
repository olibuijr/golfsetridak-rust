//! Admin dashboard and management routes.
//!
//! Every route here is gated on the admin role at the dispatch layer
//! (`serve.rs`): HTML pages 302-redirect unauthenticated/non-admin callers to
//! `/login`, JSON APIs return 401. The handlers themselves re-check the role on
//! the JSON endpoints (defence in depth).
//!
//! Handles:
//! - Booking management (`/admin/bookings`)
//! - Payment history (`/admin/payments`)
//! - User administration (`/admin/users`)
//! - Settings (`/admin/settings`, `/admin/sms`, `/admin/tilkynningar`)
//! - Dashboard statistics (`/api/admin/dashboard`, `/api/admin/stats`)
//!
//! The booking/payment/user *data* views are Phase-3 (SQL-backed) work and still
//! render shells; the settings persistence + dashboard aggregates are real.

pub mod settings;

use crate::auth;
use crate::booking::{self, Booking, Store as BookingStore};
use crate::checkout::Store as CheckoutStore;
use akurai_http::form::{field, parse_urlencoded};
use akurai_http::{Method, Request, Response};
use akurai_json::Value;
use std::path::Path;

use settings::{BankTransferSettings, NotificationSettings, Store as SettingsStore};

/// Aggregate counts derived from a slice of bookings. Pure + testable — the
/// dashboard/stats endpoints serialize this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BookingTotals {
    /// Every booking in the store, regardless of status.
    pub total_bookings: i64,
    /// Bookings with status `confirmed`.
    pub confirmed_bookings: i64,
    /// Summed `price_paid` of confirmed bookings (unpaid/None contribute 0).
    pub revenue: i64,
}

/// Compute booking totals from a booking slice. Revenue counts only confirmed
/// bookings' recorded paid price, matching the source's `getMonthlyStats`
/// (`where status = 'confirmed'`, `sum(pricePaid)`).
pub fn booking_totals(bookings: &[Booking]) -> BookingTotals {
    let mut totals = BookingTotals {
        total_bookings: bookings.len() as i64,
        ..BookingTotals::default()
    };
    for b in bookings.iter().filter(|b| b.status == "confirmed") {
        totals.confirmed_bookings += 1;
        totals.revenue += b.price_paid.unwrap_or(0);
    }
    totals
}

/// Redirect `/admin` to `/admin/bookings`. Admin dashboard entry point.
pub fn admin_home_redirect() -> Response {
    Response::new(302).with_header("Location", "/admin/bookings")
}

/// Build the per-row view for a booking. The store holds no customer name/phone
/// — `user_id` is the booking user's email, so that is shown honestly under the
/// "Notandi" column and the phone is left blank rather than fabricated.
fn booking_row(b: &Booking) -> Value {
    let starts_at = format!(
        "{} {:02}:00",
        booking::time::date_string(b.starts_at),
        booking::time::hour_of(b.starts_at),
    );
    Value::Object(vec![
        ("userName".into(), Value::Str(b.user_id.clone())),
        ("userPhone".into(), Value::Str(String::new())),
        ("startsAt".into(), Value::Str(starts_at)),
        ("status".into(), Value::Str(b.status.clone())),
        (
            "priceLabel".into(),
            Value::Str(crate::serve::format_isk(b.price_paid.unwrap_or(0))),
        ),
    ])
}

/// Render the bookings administration page with the live booking rows
/// (newest-first) plus the system-wide counts.
pub fn admin_bookings_page(
    root: &Path,
    store: &BookingStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    let mut bookings = store.all_bookings();
    let totals = booking_totals(&bookings);
    bookings.sort_by_key(|b| std::cmp::Reverse(b.starts_at));
    let rows: Vec<Value> = bookings.iter().map(booking_row).collect();
    crate::serve::render(
        root,
        "admin_bookings",
        vec![
            (
                "page_title".into(),
                Value::Str("Bókanir — Stjórnborð".into()),
            ),
            ("total_bookings".into(), Value::Int(totals.total_bookings)),
            (
                "confirmed_bookings".into(),
                Value::Int(totals.confirmed_bookings),
            ),
            ("revenue".into(), Value::Int(totals.revenue)),
            ("bookings".into(), Value::Array(rows)),
        ],
        auth,
        req,
    )
}

/// Render the payments administration page: pending bank transfers awaiting
/// manual confirmation (`provider = "bank_transfer"`, `status = "pending"`),
/// newest-first. The store has no separate customer name, so `user_id` (the
/// payer's email) is shown honestly rather than a fabricated name.
pub fn admin_payments_page(
    root: &Path,
    checkout: &CheckoutStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    let mut payments: Vec<_> = checkout
        .all_payments()
        .into_iter()
        .filter(|p| p.provider == "bank_transfer" && p.status == "pending")
        .collect();
    payments.sort_by_key(|p| std::cmp::Reverse(p.created_at));
    let rows: Vec<Value> = payments
        .iter()
        .map(|p| {
            Value::Object(vec![
                ("customerName".into(), Value::Str(p.user_id.clone())),
                ("providerRef".into(), Value::Str(p.provider_ref.clone())),
                (
                    "amountLabel".into(),
                    Value::Str(crate::serve::format_isk(p.amount)),
                ),
                (
                    "createdAt".into(),
                    Value::Str(booking::time::date_string(p.created_at)),
                ),
            ])
        })
        .collect();
    crate::serve::render(
        root,
        "admin_payments",
        vec![
            (
                "page_title".into(),
                Value::Str("Greiðslur — Stjórnborð".into()),
            ),
            ("payments".into(), Value::Array(rows)),
        ],
        auth,
        req,
    )
}

/// Render the users administration page (shell).
pub fn admin_users_page(
    root: &Path,
    _store: &BookingStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    crate::serve::render(
        root,
        "admin_users",
        vec![
            (
                "page_title".into(),
                Value::Str("Notendur — Stjórnborð".into()),
            ),
            ("users".into(), Value::Array(vec![])),
        ],
        auth,
        req,
    )
}

/// Render the settings page (bank transfer + notifications), or persist a POST.
pub fn admin_settings_page(
    root: &Path,
    settings: &SettingsStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    let mut error: Option<String> = None;
    let mut saved = false;

    if req.method == Method::Post {
        let pairs = parse_urlencoded(&req.body_str());
        let section = field(&pairs, "section").unwrap_or("");
        let result = match section {
            "notifications" => settings.set_notifications(&NotificationSettings {
                admin_copy_enabled: field(&pairs, "adminCopyEnabled")
                    .map(|v| v == "on" || v == "true" || v == "1")
                    .unwrap_or(false),
                admin_email: field(&pairs, "adminEmail").unwrap_or("").to_string(),
                admin_phone: field(&pairs, "adminPhone").unwrap_or("").to_string(),
            }),
            _ => settings.set_bank_transfer(&BankTransferSettings {
                bank_name: field(&pairs, "bankName").unwrap_or("").to_string(),
                account_holder: field(&pairs, "accountHolder").unwrap_or("").to_string(),
                kennitala: field(&pairs, "kennitala").unwrap_or("").to_string(),
                account_number: field(&pairs, "accountNumber").unwrap_or("").to_string(),
            }),
        };
        match result {
            Ok(()) => saved = true,
            Err(msg) => error = Some(msg),
        }
    }

    let bank = settings.bank_transfer();
    let notify = settings.notifications();

    let mut extra = vec![
        (
            "page_title".into(),
            Value::Str("Stillingar — Stjórnborð".into()),
        ),
        ("bank_name".into(), Value::Str(bank.bank_name)),
        ("account_holder".into(), Value::Str(bank.account_holder)),
        ("kennitala".into(), Value::Str(bank.kennitala)),
        ("account_number".into(), Value::Str(bank.account_number)),
        (
            "admin_copy_enabled".into(),
            Value::Bool(notify.admin_copy_enabled),
        ),
        ("admin_email".into(), Value::Str(notify.admin_email)),
        ("admin_phone".into(), Value::Str(notify.admin_phone)),
        ("saved".into(), Value::Bool(saved)),
    ];
    if let Some(msg) = error {
        extra.push(("error".into(), Value::Str(msg)));
    }
    crate::serve::render(root, "admin_settings", extra, auth, req)
}

/// Render the SMS templates page, or persist a POSTed template body.
pub fn admin_sms_page(
    root: &Path,
    settings: &SettingsStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    let mut error: Option<String> = None;
    let mut saved = false;

    if req.method == Method::Post {
        let pairs = parse_urlencoded(&req.body_str());
        let key = field(&pairs, "key").unwrap_or("");
        let body = field(&pairs, "body").unwrap_or("");
        match settings.set_sms_template(key, body) {
            Ok(()) => saved = true,
            Err(msg) => error = Some(msg),
        }
    }

    // The known automatic-SMS template keys (mirrors the source's seed set).
    let template_keys = [
        ("booking_confirmed", "Bókun staðfest"),
        ("booking_cancelled", "Bókun afturkölluð"),
        ("gift_card", "Gjafabréf"),
        ("payment_received", "Greiðsla móttekin"),
    ];
    let templates: Vec<Value> = template_keys
        .iter()
        .map(|(key, label)| {
            Value::Object(vec![
                ("key".into(), Value::Str((*key).into())),
                ("label".into(), Value::Str((*label).into())),
                (
                    "body".into(),
                    Value::Str(settings.sms_template(key).unwrap_or_default()),
                ),
            ])
        })
        .collect();

    let mut extra = vec![
        (
            "page_title".into(),
            Value::Str("SMS sniðmát — Stjórnborð".into()),
        ),
        ("templates".into(), Value::Array(templates)),
        ("saved".into(), Value::Bool(saved)),
    ];
    if let Some(msg) = error {
        extra.push(("error".into(), Value::Str(msg)));
    }
    crate::serve::render(root, "admin_sms", extra, auth, req)
}

/// Render the announcements / activity-feed page (shell).
pub fn admin_announcements_page(
    root: &Path,
    _store: &BookingStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    crate::serve::render(
        root,
        "admin_announcements",
        vec![
            (
                "page_title".into(),
                Value::Str("Tilkynningar — Stjórnborð".into()),
            ),
            ("announcements".into(), Value::Array(vec![])),
        ],
        auth,
        req,
    )
}

/// JSON endpoint: `GET /api/admin/dashboard` — overall stats snapshot.
pub fn api_admin_dashboard(store: &BookingStore, req: &Request, auth: &auth::State) -> Response {
    if req.method != Method::Get {
        return json(405, &error_value("method not allowed"));
    }
    if !auth.require_role(req, auth::Role::Admin) {
        return json(401, &error_value("Unauthorized"));
    }
    let totals = booking_totals(&store.all_bookings());
    json(200, &totals_value(&totals))
}

/// JSON endpoint: `GET /api/admin/stats` — detailed statistics.
pub fn api_admin_stats(store: &BookingStore, req: &Request, auth: &auth::State) -> Response {
    if req.method != Method::Get {
        return json(405, &error_value("method not allowed"));
    }
    if !auth.require_role(req, auth::Role::Admin) {
        return json(401, &error_value("Unauthorized"));
    }
    let totals = booking_totals(&store.all_bookings());
    json(200, &totals_value(&totals))
}

// ---- helpers --------------------------------------------------------------

fn totals_value(t: &BookingTotals) -> Value {
    Value::Object(vec![
        ("totalBookings".into(), Value::Int(t.total_bookings)),
        ("confirmedBookings".into(), Value::Int(t.confirmed_bookings)),
        ("revenue".into(), Value::Int(t.revenue)),
    ])
}

fn json(status: u16, value: &Value) -> Response {
    Response::new(status).with_body(
        "application/json; charset=utf-8",
        value.to_json().into_bytes(),
    )
}

fn error_value(msg: &str) -> Value {
    Value::Object(vec![("error".into(), Value::Str(msg.to_string()))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn booking(status: &str, price: Option<i64>) -> Booking {
        Booking {
            id: "b".into(),
            user_id: "u".into(),
            starts_at: 0,
            status: status.into(),
            payment_type: "single".into(),
            price_paid: price,
            user_package_id: None,
            user_subscription_id: None,
            notes: None,
            created_at: 0,
            cancelled_at: None,
        }
    }

    #[test]
    fn totals_empty() {
        assert_eq!(booking_totals(&[]), BookingTotals::default());
    }

    #[test]
    fn totals_only_count_confirmed_revenue() {
        let bookings = vec![
            booking("confirmed", Some(3500)),
            booking("confirmed", Some(2000)),
            booking("pending", Some(9999)), // blocks slot, no revenue
            booking("cancelled", Some(9999)), // ignored
            booking("confirmed", None),     // confirmed but unpaid → +0
        ];
        let totals = booking_totals(&bookings);
        assert_eq!(totals.total_bookings, 5);
        assert_eq!(totals.confirmed_bookings, 3);
        assert_eq!(totals.revenue, 5500);
    }

    #[test]
    fn totals_value_keys() {
        let v = totals_value(&BookingTotals {
            total_bookings: 7,
            confirmed_bookings: 4,
            revenue: 12000,
        });
        assert_eq!(v.get("totalBookings").and_then(Value::as_i64), Some(7));
        assert_eq!(v.get("confirmedBookings").and_then(Value::as_i64), Some(4));
        assert_eq!(v.get("revenue").and_then(Value::as_i64), Some(12000));
    }
}
