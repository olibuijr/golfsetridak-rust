//! Checkout + payments: the cart-to-order pipeline.
//!
//! This module ports the source app's checkout flow (`/api/cart/checkout`,
//! `/api/payments/landsbankinn/callback`, the `/checkout/*` pages) and the
//! correctness-critical fulfillment core (`src/lib/cart/fulfillment.ts`).
//!
//! ## Storage
//!
//! Two `akurai-storage` B+trees under `data/checkout/`:
//!
//! | Tree | Key | Value |
//! |------|-----|-------|
//! | `payments.db` | payment id | payment JSON (cart, provider, status, amount…) |
//! | `gift_cards.db` | gift-card code | gift-card JSON (amount, balance, status…) |
//!
//! ## Atomicity
//!
//! The framework has no cross-store transaction. Fulfillment gets its
//! correctness from the same three things the booking store documents: a single
//! in-process write lock (`fulfill_lock`) serializes a cart's fulfillment, the
//! booking tree's keyed uniqueness is the no-double-booking guard, and each
//! store commits atomically. A cart already marked `paid` short-circuits, so a
//! duplicated callback never double-fulfills.
//!
//! ## No outbound TLS
//!
//! The Landsbankinn gateway is reached through the local plaintext sidecar — see
//! [`landsbankinn`]. With no sidecar wired the gateway is mocked end-to-end.

mod fulfillment;
mod landsbankinn;

use fulfillment::{fulfill_cart, mark_cart_failed};

use crate::auth;
use crate::booking::Store as BookingStore;
use crate::cart::Store as CartStore;
use akurai_http::form::{field, parse_urlencoded};
use akurai_http::{Method, Request, Response};
use akurai_json::Value;
use akurai_storage::BTree;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A payment record (mirrors the `payments` table). `raw_payload` carries
/// provider-specific data and the fulfillment audit on failure.
#[derive(Debug, Clone, PartialEq)]
pub struct Payment {
    pub id: String,
    pub cart_id: String,
    pub user_id: String,
    pub provider: String,
    pub provider_ref: String,
    pub status: String,
    pub amount: i64,
    pub currency: String,
    pub raw_payload: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

/// An issued gift card (mirrors `gift_cards`). Balance decrement / redemption is
/// out of scope here — issuance on fulfillment is what this port covers.
#[derive(Debug, Clone, PartialEq)]
pub struct GiftCard {
    pub code: String,
    pub amount: i64,
    pub balance: i64,
    pub currency: String,
    pub status: String,
    pub recipient_email: Option<String>,
    pub recipient_phone: Option<String>,
    pub recipient_name: Option<String>,
    pub message: Option<String>,
    pub theme: String,
    pub purchased_by_user_id: String,
    pub cart_id: String,
    pub created_at: i64,
}

struct Trees {
    payments: BTree,
    gift_cards: BTree,
}

/// The checkout store: payments + issued gift cards, plus the fulfillment lock.
pub struct Store {
    inner: Mutex<Trees>,
    /// Serializes a cart's fulfillment so two callbacks can't double-fulfill.
    fulfill: Mutex<()>,
}

static CHECKOUT_SEQ: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        let open = |name: &str| BTree::open(data_dir.join(name));
        Ok(Store {
            inner: Mutex::new(Trees {
                payments: open("payments.db")?,
                gift_cards: open("gift_cards.db")?,
            }),
            fulfill: Mutex::new(()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Trees> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hold this for the duration of a cart's fulfillment (see module docs).
    pub fn fulfill_lock(&self) -> MutexGuard<'_, ()> {
        self.fulfill.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- payments ---------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn create_payment(
        &self,
        cart_id: &str,
        user_id: &str,
        provider: &str,
        provider_ref: &str,
        status: &str,
        amount: i64,
        currency: &str,
        raw_payload: Value,
        now_ms: i64,
    ) -> Result<Payment, String> {
        let payment = Payment {
            id: next_id("pay", now_ms),
            cart_id: cart_id.to_string(),
            user_id: user_id.to_string(),
            provider: provider.to_string(),
            provider_ref: provider_ref.to_string(),
            status: status.to_string(),
            amount,
            currency: currency.to_string(),
            raw_payload,
            created_at: now_ms,
            updated_at: now_ms,
        };
        let mut t = self.lock();
        put_payment(&mut t.payments, &payment).map_err(io_err)?;
        t.payments.commit().map_err(io_err)?;
        Ok(payment)
    }

    #[allow(dead_code)] // read-side accessor; exercised by tests, prod consumer is later-phase
    pub fn payment_by_id(&self, id: &str) -> Option<Payment> {
        let mut t = self.lock();
        payment_by_id(&mut t.payments, id)
    }

    /// Every payment in the store, in scan order (admin payments view).
    pub fn all_payments(&self) -> Vec<Payment> {
        let mut t = self.lock();
        all_payments(&mut t.payments)
    }

    /// The payment for a `(cart, providerRef)` pair (the source's unique key).
    pub fn by_cart_and_ref(&self, cart_id: &str, provider_ref: &str) -> Option<Payment> {
        let mut t = self.lock();
        all_payments(&mut t.payments)
            .into_iter()
            .find(|p| p.cart_id == cart_id && p.provider_ref == provider_ref)
    }

    /// The most recent pending payment for a cart on a given provider — used by
    /// the callback when only the cart id is known.
    pub fn latest_pending_by_cart(&self, cart_id: &str, provider: &str) -> Option<Payment> {
        let mut t = self.lock();
        all_payments(&mut t.payments)
            .into_iter()
            .filter(|p| p.cart_id == cart_id && p.provider == provider && p.status == "pending")
            .max_by_key(|p| p.created_at)
    }

    pub fn mark_succeeded(&self, id: &str, now_ms: i64) -> Result<(), String> {
        let mut t = self.lock();
        let mut payment = payment_by_id(&mut t.payments, id).ok_or("payment not found")?;
        payment.status = "succeeded".into();
        payment.updated_at = now_ms;
        put_payment(&mut t.payments, &payment).map_err(io_err)?;
        t.payments.commit().map_err(io_err)
    }

    /// Mark a payment failed, recording an audit object under
    /// `raw_payload.fulfillment` (mirrors the source `markCartFailed`).
    pub fn mark_failed(&self, id: &str, error_code: &str, now_ms: i64) -> Result<(), String> {
        let mut t = self.lock();
        let mut payment = payment_by_id(&mut t.payments, id).ok_or("payment not found")?;
        payment.status = "failed".into();
        payment.updated_at = now_ms;
        let audit = Value::Object(vec![
            ("errorCode".into(), Value::Str(error_code.to_string())),
            (
                "providerRef".into(),
                Value::Str(payment.provider_ref.clone()),
            ),
            ("occurredAt".into(), Value::Int(now_ms)),
        ]);
        payment.raw_payload = merge_payload(&payment.raw_payload, "fulfillment", audit);
        put_payment(&mut t.payments, &payment).map_err(io_err)?;
        t.payments.commit().map_err(io_err)
    }

    // ---- gift cards -------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn create_gift_card(
        &self,
        amount: i64,
        currency: &str,
        recipient_email: Option<&str>,
        recipient_phone: Option<&str>,
        recipient_name: Option<&str>,
        message: Option<&str>,
        theme: Option<&str>,
        purchased_by_user_id: &str,
        cart_id: &str,
        now_ms: i64,
    ) -> Result<GiftCard, String> {
        let opt = |s: Option<&str>| s.map(str::to_string).filter(|s| !s.trim().is_empty());
        let card = GiftCard {
            code: gift_card_code(now_ms),
            amount,
            balance: amount,
            currency: currency.to_string(),
            status: "active".into(),
            recipient_email: opt(recipient_email),
            recipient_phone: opt(recipient_phone),
            recipient_name: opt(recipient_name),
            message: opt(message),
            theme: crate::giftcards::normalize_theme(theme).to_string(),
            purchased_by_user_id: purchased_by_user_id.to_string(),
            cart_id: cart_id.to_string(),
            created_at: now_ms,
        };
        let mut t = self.lock();
        put_gift_card(&mut t.gift_cards, &card).map_err(io_err)?;
        t.gift_cards.commit().map_err(io_err)?;
        Ok(card)
    }

    #[allow(dead_code)] // read-side accessor; gift-card redemption lands in a later phase
    pub fn gift_card(&self, code: &str) -> Option<GiftCard> {
        let mut t = self.lock();
        gift_card_by_code(&mut t.gift_cards, code)
    }

    #[allow(dead_code)] // read-side accessor; gift-card delivery/redemption lands later
    pub fn gift_cards_for_cart(&self, cart_id: &str) -> Vec<GiftCard> {
        let mut t = self.lock();
        all_gift_cards(&mut t.gift_cards)
            .into_iter()
            .filter(|c| c.cart_id == cart_id)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers — called from serve.rs dispatch arms
// ---------------------------------------------------------------------------

/// `POST /api/cart/checkout` — validate the cart, create a payment, and either
/// open a gateway session (Landsbankinn, via sidecar/mock) or record a bank
/// transfer. Returns the redirect target the browser should follow.
pub fn api_checkout(
    carts: &CartStore,
    payments: &Store,
    auth: &auth::State,
    req: &Request,
) -> Response {
    if req.method != Method::Post {
        return err_json(405, "method not allowed");
    }
    let Some(user) = auth.current_user(req) else {
        return json(
            401,
            &Value::Object(vec![
                (
                    "error".into(),
                    Value::Str("Sign-in required to check out".into()),
                ),
                ("loginRequired".into(), Value::Bool(true)),
            ]),
        );
    };
    let now = now_ms();
    let cart_id = crate::serve::user_cart_id(&user.email);
    let summary = match carts.get_or_create_open(Some(&cart_id), now) {
        Ok((summary, _)) => summary,
        Err(e) => return err_json(500, &e),
    };
    if summary.items.is_empty() {
        return err_json(400, "Cart is empty");
    }
    if summary.status != "open" {
        return err_json(409, &format!("Cart is {}", summary.status));
    }

    let body = akurai_json::parse(&req.body_str()).unwrap_or(Value::Object(vec![]));
    let bank_transfer = body.get("paymentMethod").and_then(Value::as_str) == Some("bank_transfer");
    let amount = summary.subtotal;
    let currency = summary.currency.as_str();

    if bank_transfer {
        let provider_ref = format!("bank-{now}-{}", seq());
        let raw = Value::Object(vec![("method".into(), Value::Str("bank_transfer".into()))]);
        if let Err(e) = payments.create_payment(
            &summary.id,
            &user.email,
            "bank_transfer",
            &provider_ref,
            "pending",
            amount,
            currency,
            raw,
            now,
        ) {
            return err_json(500, &e);
        }
        if let Err(e) = carts.set_status(&summary.id, "checking_out", now) {
            return err_json(500, &e);
        }
        let redirect = format!(
            "/checkout/bank-transfer?cart={}&ref={}",
            summary.id, provider_ref
        );
        return json(
            200,
            &Value::Object(vec![
                ("redirectUrl".into(), Value::Str(redirect)),
                ("providerRef".into(), Value::Str(provider_ref)),
                ("method".into(), Value::Str("bank_transfer".into())),
                ("mock".into(), Value::Bool(false)),
            ]),
        );
    }

    // Landsbankinn (card). The callback runs the fulfillment after the gateway
    // redirects back. Relative URLs — the browser resolves them.
    let callback = format!("/api/payments/landsbankinn/callback?cart={}", summary.id);
    let cancel = format!("{callback}&cancel=1");
    let session =
        match landsbankinn::create_checkout(amount, currency, &summary.id, &callback, &cancel) {
            Ok(s) => s,
            Err(e) => return err_json(502, &format!("Greiðsla mistókst: {e}")),
        };
    if let Err(e) = payments.create_payment(
        &summary.id,
        &user.email,
        "landsbankinn",
        &session.id,
        "pending",
        amount,
        currency,
        Value::Null,
        now,
    ) {
        return err_json(500, &e);
    }
    if let Err(e) = carts.set_status(&summary.id, "checking_out", now) {
        return err_json(500, &e);
    }
    json(
        200,
        &Value::Object(vec![
            ("redirectUrl".into(), Value::Str(session.redirect_url)),
            ("providerRef".into(), Value::Str(session.id)),
            ("method".into(), Value::Str("landsbankinn".into())),
            ("mock".into(), Value::Bool(session.mock)),
        ]),
    )
}

/// `GET /api/payments/landsbankinn/callback?cart=…[&cancel=1]` — reconcile the
/// gateway outcome: on success run fulfillment and redirect to the success page,
/// otherwise mark the cart failed and redirect to the cancel page.
pub fn api_landsbankinn_callback(
    carts: &CartStore,
    payments: &Store,
    bookings: &BookingStore,
    req: &Request,
) -> Response {
    let q = req
        .query
        .as_deref()
        .map(parse_urlencoded)
        .unwrap_or_default();
    let cancelled = field(&q, "cancel") == Some("1");
    let cart_from_query = field(&q, "cart").map(str::to_string);
    let ref_from_query = field(&q, "checkout_id").map(str::to_string);

    // Resolve (providerRef, cartId) from whatever the gateway handed back.
    let mut provider_ref = ref_from_query;
    let mut cart_id = cart_from_query;
    if provider_ref.is_none() {
        if let Some(cid) = &cart_id {
            provider_ref = payments
                .latest_pending_by_cart(cid, "landsbankinn")
                .map(|p| p.provider_ref);
        }
    }
    if cart_id.is_none() {
        if let Some(pref) = &provider_ref {
            cart_id = payments.by_ref(pref).map(|p| p.cart_id);
        }
    }
    let (Some(provider_ref), Some(cart_id)) = (provider_ref, cart_id) else {
        return redirect("/checkout/cancel?error=payment_cancelled");
    };

    let now = now_ms();
    if cancelled {
        mark_cart_failed(
            payments,
            carts,
            &cart_id,
            &provider_ref,
            "payment_cancelled",
            now,
        );
        return redirect(&cancel_path(&cart_id, "payment_cancelled"));
    }

    match landsbankinn::checkout_outcome(&provider_ref) {
        Ok(landsbankinn::Outcome::Succeeded) => {
            match fulfill_cart(payments, carts, bookings, &cart_id, &provider_ref, now) {
                Ok(()) => redirect(&format!("/checkout/success?cart={cart_id}")),
                Err(e) => {
                    eprintln!(
                        "[checkout] fulfillment failed for cart {cart_id} ({}): {}",
                        e.code, e.message
                    );
                    mark_cart_failed(payments, carts, &cart_id, &provider_ref, &e.code, now);
                    redirect(&cancel_path(&cart_id, &e.code))
                }
            }
        }
        Ok(landsbankinn::Outcome::Failed) => {
            mark_cart_failed(
                payments,
                carts,
                &cart_id,
                &provider_ref,
                "payment_failed",
                now,
            );
            redirect(&cancel_path(&cart_id, "payment_failed"))
        }
        Ok(landsbankinn::Outcome::Pending) => redirect(&cancel_path(&cart_id, "payment_cancelled")),
        Err(_) => {
            mark_cart_failed(
                payments,
                carts,
                &cart_id,
                &provider_ref,
                "payment_failed",
                now,
            );
            redirect(&cancel_path(&cart_id, "payment_failed"))
        }
    }
}

impl Store {
    /// The payment for a providerRef (any cart). Used to recover the cart id in
    /// the callback when only the gateway id is present.
    pub fn by_ref(&self, provider_ref: &str) -> Option<Payment> {
        let mut t = self.lock();
        all_payments(&mut t.payments)
            .into_iter()
            .find(|p| p.provider_ref == provider_ref)
    }
}

// ---- pages ----------------------------------------------------------------

/// `/checkout` — the cart summary + payment-method buttons (auth-gated).
pub fn page_checkout(
    root: &Path,
    carts: &CartStore,
    auth: &auth::State,
    req: &Request,
) -> Response {
    let Some(user) = auth.current_user(req) else {
        return redirect("/login");
    };
    let now = now_ms();
    let cart_id = crate::serve::user_cart_id(&user.email);
    let summary = match carts.get_or_create_open(Some(&cart_id), now) {
        Ok((summary, _)) => summary,
        Err(e) => return err_json(500, &e),
    };
    render_page(
        root,
        "checkout",
        vec![
            (
                "page_title".into(),
                Value::Str("Greiðsla — Golfsetrið Akureyri".into()),
            ),
            ("cart".into(), crate::cart::cart_value(&summary)),
            (
                "subtotal_label".into(),
                Value::Str(crate::shop::format_isk(summary.subtotal)),
            ),
            ("has_items".into(), Value::Bool(!summary.items.is_empty())),
        ],
        auth,
        req,
    )
}

/// `/checkout/bank-transfer` — payment instructions for the bank-transfer path.
pub fn page_bank_transfer(
    root: &Path,
    payments: &Store,
    auth: &auth::State,
    req: &Request,
) -> Response {
    if auth.current_user(req).is_none() {
        return redirect("/login");
    }
    let q = req
        .query
        .as_deref()
        .map(parse_urlencoded)
        .unwrap_or_default();
    let cart_id = field(&q, "cart").unwrap_or("");
    let provider_ref = field(&q, "ref").unwrap_or("");
    let (amount, currency) = payments
        .by_cart_and_ref(cart_id, provider_ref)
        .map(|p| (p.amount, p.currency))
        .unwrap_or((0, "ISK".into()));
    let _ = currency;
    render_page(
        root,
        "checkout_bank_transfer",
        vec![
            (
                "page_title".into(),
                Value::Str("Millifærsla — Golfsetrið Akureyri".into()),
            ),
            (
                "amount_label".into(),
                Value::Str(crate::shop::format_isk(amount)),
            ),
            ("provider_ref".into(), Value::Str(provider_ref.to_string())),
            ("cart_id".into(), Value::Str(cart_id.to_string())),
        ],
        auth,
        req,
    )
}

/// `/checkout/landsbankinn` — informational page for the hosted card flow.
pub fn page_landsbankinn(root: &Path, auth: &auth::State, req: &Request) -> Response {
    if auth.current_user(req).is_none() {
        return redirect("/login");
    }
    render_page(
        root,
        "checkout_landsbankinn",
        vec![(
            "page_title".into(),
            Value::Str("Kortagreiðsla — Golfsetrið Akureyri".into()),
        )],
        auth,
        req,
    )
}

/// `/checkout/success` — order confirmed.
pub fn page_success(root: &Path, auth: &auth::State, req: &Request) -> Response {
    render_page(
        root,
        "checkout_success",
        vec![(
            "page_title".into(),
            Value::Str("Greiðsla staðfest — Golfsetrið Akureyri".into()),
        )],
        auth,
        req,
    )
}

/// `/checkout/cancel?error=<code>` — payment did not complete; message by code.
pub fn page_cancel(root: &Path, auth: &auth::State, req: &Request) -> Response {
    let q = req
        .query
        .as_deref()
        .map(parse_urlencoded)
        .unwrap_or_default();
    let code = field(&q, "error").unwrap_or("payment_cancelled");
    let (title, message) = error_presentation(code);
    render_page(
        root,
        "checkout_cancel",
        vec![
            (
                "page_title".into(),
                Value::Str(format!("{title} — Golfsetrið Akureyri")),
            ),
            ("error_code".into(), Value::Str(code.to_string())),
            ("error_title".into(), Value::Str(title.to_string())),
            ("error_message".into(), Value::Str(message.to_string())),
        ],
        auth,
        req,
    )
}

/// Title + Icelandic message for each payment error code (mirrors the source's
/// `getPaymentErrorPresentation`).
fn error_presentation(code: &str) -> (&'static str, &'static str) {
    match code {
        "payment_failed" => (
            "Greiðsla mistókst",
            "Greiðslan gekk ekki í gegn. Reyndu aftur eða notaðu annað kort.",
        ),
        "slot_taken_after_payment" => (
            "Tími þegar bókaður",
            "Einn eða fleiri tímar voru bókaðir á meðan greiðslan fór fram. Vinsamlegast veldu aðra tíma.",
        ),
        "payment_succeeded_booking_failed" => (
            "Villa við staðfestingu",
            "Greiðslan tókst en ekki tókst að klára bókunina. Hafðu samband og við leysum málið.",
        ),
        "payment_pending_timeout" => (
            "Greiðsla í bið",
            "Við fengum ekki staðfestingu frá greiðslukerfinu í tæka tíð. Hafðu samband ef upphæðin var skuldfærð.",
        ),
        _ => (
            "Greiðsla hætt",
            "Greiðslunni var hætt. Karfan þín bíður þín ef þú vilt reyna aftur.",
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cancel_path(cart_id: &str, code: &str) -> String {
    format!("/checkout/cancel?cart={cart_id}&error={code}")
}

fn render_page(
    root: &Path,
    name: &str,
    mut extra: Vec<(String, Value)>,
    auth: &auth::State,
    req: &Request,
) -> Response {
    let engine = match crate::serve::build_engine_pub(root) {
        Ok(e) => e,
        Err(msg) => return Response::new(500).with_html(&msg),
    };
    if let Some(user) = auth.current_user(req) {
        extra.push(("auth_user_email".into(), Value::Str(user.email)));
    }
    let mut context = crate::serve::load_context_pub(root);
    if let Value::Object(pairs) = &mut context {
        pairs.extend(extra);
    }
    match engine.render(name, &context) {
        Ok(html) => Response::ok()
            .with_html(&html)
            .with_header("Cache-Control", "no-cache"),
        Err(e) => Response::new(500).with_html(&e.message),
    }
}

fn json(status: u16, value: &Value) -> Response {
    Response::new(status).with_body(
        "application/json; charset=utf-8",
        value.to_json().into_bytes(),
    )
}

fn err_json(status: u16, message: &str) -> Response {
    json(
        status,
        &Value::Object(vec![("error".into(), Value::Str(message.to_string()))]),
    )
}

fn redirect(location: &str) -> Response {
    Response::new(303).with_header("Location", location)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn seq() -> u64 {
    CHECKOUT_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn next_id(prefix: &str, now_ms: i64) -> String {
    format!("{prefix}-{now_ms}-{}", seq())
}

fn gift_card_code(now_ms: i64) -> String {
    let s = seq();
    format!(
        "GC-{:06}-{:04}",
        now_ms.unsigned_abs() % 1_000_000,
        s % 10_000
    )
}

fn io_err(e: io::Error) -> String {
    format!("storage error: {e}")
}

/// Merge `key -> value` into a JSON object payload, preserving prior keys. A
/// non-object payload (e.g. `Null`) is replaced with a fresh object.
fn merge_payload(prior: &Value, key: &str, value: Value) -> Value {
    let mut pairs = match prior {
        Value::Object(p) => p.clone(),
        _ => vec![],
    };
    pairs.retain(|(k, _)| k != key);
    pairs.push((key.to_string(), value));
    Value::Object(pairs)
}

// ---- payment record plumbing ----------------------------------------------

fn opt_str(value: &Option<String>) -> Value {
    value.clone().map(Value::Str).unwrap_or(Value::Null)
}

fn payment_record(p: &Payment) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(p.id.clone())),
        ("cart_id".into(), Value::Str(p.cart_id.clone())),
        ("user_id".into(), Value::Str(p.user_id.clone())),
        ("provider".into(), Value::Str(p.provider.clone())),
        ("provider_ref".into(), Value::Str(p.provider_ref.clone())),
        ("status".into(), Value::Str(p.status.clone())),
        ("amount".into(), Value::Int(p.amount)),
        ("currency".into(), Value::Str(p.currency.clone())),
        ("raw_payload".into(), p.raw_payload.clone()),
        ("created_at".into(), Value::Int(p.created_at)),
        ("updated_at".into(), Value::Int(p.updated_at)),
    ])
}

fn payment_from_value(v: &Value) -> Option<Payment> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Payment {
        id: s("id")?,
        cart_id: s("cart_id")?,
        user_id: s("user_id").unwrap_or_default(),
        provider: s("provider")?,
        provider_ref: s("provider_ref")?,
        status: s("status")?,
        amount: v.get("amount").and_then(Value::as_i64).unwrap_or(0),
        currency: s("currency").unwrap_or_else(|| "ISK".into()),
        raw_payload: v.get("raw_payload").cloned().unwrap_or(Value::Null),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
        updated_at: v.get("updated_at").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn put_payment(tree: &mut BTree, p: &Payment) -> io::Result<()> {
    tree.insert(p.id.as_bytes(), payment_record(p).to_json().as_bytes())
}

fn payment_by_id(tree: &mut BTree, id: &str) -> Option<Payment> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    payment_from_value(&v)
}

fn all_payments(tree: &mut BTree) -> Vec<Payment> {
    full_scan(tree)
        .iter()
        .filter_map(payment_from_value)
        .collect()
}

// ---- gift-card record plumbing --------------------------------------------

fn gift_card_record(c: &GiftCard) -> Value {
    Value::Object(vec![
        ("code".into(), Value::Str(c.code.clone())),
        ("amount".into(), Value::Int(c.amount)),
        ("balance".into(), Value::Int(c.balance)),
        ("currency".into(), Value::Str(c.currency.clone())),
        ("status".into(), Value::Str(c.status.clone())),
        ("recipient_email".into(), opt_str(&c.recipient_email)),
        ("recipient_phone".into(), opt_str(&c.recipient_phone)),
        ("recipient_name".into(), opt_str(&c.recipient_name)),
        ("message".into(), opt_str(&c.message)),
        (
            "theme".into(),
            Value::Str(crate::giftcards::normalize_theme(Some(&c.theme)).to_string()),
        ),
        (
            "purchased_by_user_id".into(),
            Value::Str(c.purchased_by_user_id.clone()),
        ),
        ("cart_id".into(), Value::Str(c.cart_id.clone())),
        ("created_at".into(), Value::Int(c.created_at)),
    ])
}

#[allow(dead_code)] // decode helper for the read-side gift-card accessors above
fn gift_card_from_value(v: &Value) -> Option<GiftCard> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(GiftCard {
        code: s("code")?,
        amount: v.get("amount").and_then(Value::as_i64)?,
        balance: v.get("balance").and_then(Value::as_i64).unwrap_or(0),
        currency: s("currency").unwrap_or_else(|| "ISK".into()),
        status: s("status").unwrap_or_else(|| "active".into()),
        recipient_email: s("recipient_email"),
        recipient_phone: s("recipient_phone"),
        recipient_name: s("recipient_name"),
        message: s("message"),
        theme: crate::giftcards::normalize_theme(v.get("theme").and_then(Value::as_str))
            .to_string(),
        purchased_by_user_id: s("purchased_by_user_id").unwrap_or_default(),
        cart_id: s("cart_id").unwrap_or_default(),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn put_gift_card(tree: &mut BTree, c: &GiftCard) -> io::Result<()> {
    tree.insert(c.code.as_bytes(), gift_card_record(c).to_json().as_bytes())
}

#[allow(dead_code)] // decode helper for the read-side gift-card accessors above
fn gift_card_by_code(tree: &mut BTree, code: &str) -> Option<GiftCard> {
    let raw = tree.get(code.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    gift_card_from_value(&v)
}

#[allow(dead_code)] // decode helper for the read-side gift-card accessors above
fn all_gift_cards(tree: &mut BTree) -> Vec<GiftCard> {
    full_scan(tree)
        .iter()
        .filter_map(gift_card_from_value)
        .collect()
}

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
    use std::path::PathBuf;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "gsd-checkout-test-{tag}-{}-{}",
            std::process::id(),
            CHECKOUT_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    #[test]
    fn payment_create_and_state_transitions() {
        let (store, dir) = temp_store("pay");
        let p = store
            .create_payment(
                "cart-1",
                "u@x.com",
                "landsbankinn",
                "ref-1",
                "pending",
                3500,
                "ISK",
                Value::Null,
                100,
            )
            .unwrap();
        assert_eq!(p.status, "pending");
        assert_eq!(store.by_cart_and_ref("cart-1", "ref-1").unwrap().id, p.id);
        assert_eq!(
            store
                .latest_pending_by_cart("cart-1", "landsbankinn")
                .unwrap()
                .id,
            p.id
        );

        store.mark_succeeded(&p.id, 200).unwrap();
        assert_eq!(store.payment_by_id(&p.id).unwrap().status, "succeeded");
        // No longer pending → not returned by the pending query.
        assert!(store
            .latest_pending_by_cart("cart-1", "landsbankinn")
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_failed_records_audit() {
        let (store, dir) = temp_store("fail");
        let p = store
            .create_payment(
                "cart-2",
                "u@x.com",
                "landsbankinn",
                "ref-2",
                "pending",
                1000,
                "ISK",
                Value::Null,
                100,
            )
            .unwrap();
        store.mark_failed(&p.id, "payment_failed", 200).unwrap();
        let after = store.payment_by_id(&p.id).unwrap();
        assert_eq!(after.status, "failed");
        let audit = after.raw_payload.get("fulfillment").unwrap();
        assert_eq!(
            audit.get("errorCode").and_then(Value::as_str),
            Some("payment_failed")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gift_card_issue_round_trip() {
        let (store, dir) = temp_store("gc");
        let card = store
            .create_gift_card(
                5000,
                "ISK",
                Some("g@x.com"),
                None,
                Some("Gjöf"),
                None,
                None,
                "buyer",
                "cart-3",
                100,
            )
            .unwrap();
        assert_eq!(card.balance, 5000);
        assert_eq!(store.gift_card(&card.code).unwrap().amount, 5000);
        assert_eq!(store.gift_cards_for_cart("cart-3").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
