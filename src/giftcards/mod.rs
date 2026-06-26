//! Gift card issuance, lookup, and redemption.
//!
//! This module ports the source app's gift-cards.ts + scheduler.ts to Rust,
//! backed by BTree storage. It provides:
//!
//! - Code generation (ambiguity-free base32, prefix "GOLF-")
//! - Lookup by code (returns card details and balance)
//! - Redemption (atomically decrement balance, record redemption)
//! - Scheduled delivery (polling for pending deliveries)

use akurai_json::Value;
use akurai_storage::BTree;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const CODE_PREFIX: &str = "GOLF";
const CODE_GROUP_LEN: usize = 4;
const CODE_GROUPS: usize = 3;
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, PartialEq)]
pub struct GiftCard {
    pub id: String,
    pub code: String,
    pub amount: i64,
    pub balance: i64,
    pub currency: String,
    pub status: String,
    pub expires_at: Option<i64>,
    pub delivery_at: Option<i64>,
    pub delivered_at: Option<i64>,
    pub purchased_by_user_id: Option<String>,
    pub cart_id: Option<String>,
    pub recipient_email: Option<String>,
    pub recipient_phone: Option<String>,
    pub recipient_name: Option<String>,
    pub message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct GiftCardRedemption {
    pub id: String,
    pub gift_card_id: String,
    pub cart_id: Option<String>,
    pub payment_id: Option<String>,
    pub amount: i64,
    pub redeemed_at: i64,
}

pub struct Store {
    inner: Mutex<Trees>,
}

struct Trees {
    cards: BTree,
    redemptions: BTree,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        Ok(Store {
            inner: Mutex::new(Trees {
                cards: BTree::open(data_dir.join("gift_cards.db"))?,
                redemptions: BTree::open(data_dir.join("gift_card_redemptions.db"))?,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Trees> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn generate_code() -> String {
        let mut bytes = vec![0u8; CODE_GROUPS * CODE_GROUP_LEN];
        if let Ok(entropy) = random_bytes(bytes.len()) {
            bytes = entropy;
        }

        let mut groups = Vec::new();
        for g in 0..CODE_GROUPS {
            let mut group = String::new();
            for i in 0..CODE_GROUP_LEN {
                let idx = (bytes[g * CODE_GROUP_LEN + i] as usize) % ALPHABET.len();
                group.push(ALPHABET[idx] as char);
            }
            groups.push(group);
        }

        format!("{}-{}", CODE_PREFIX, groups.join("-"))
    }

    pub fn normalize_code(input: &str) -> String {
        input.trim().to_uppercase().replace(' ', "")
    }

    pub fn lookup(&self, raw_code: &str, now_ms: i64) -> Result<GiftCard, LookupError> {
        let code = Self::normalize_code(raw_code);
        if code.is_empty() {
            return Err(LookupError::NotFound);
        }

        let mut t = self.lock();
        let card = all_cards(&mut t.cards)
            .into_iter()
            .find(|c| c.code == code)
            .ok_or(LookupError::NotFound)?;

        if card.status == "cancelled" || card.status == "used" {
            return Err(LookupError::Inactive);
        }

        if let Some(exp) = card.expires_at {
            if exp <= now_ms {
                return Err(LookupError::Expired);
            }
        }

        if card.balance <= 0 {
            return Err(LookupError::Depleted);
        }

        Ok(card)
    }

    pub fn redeem(
        &self,
        code: &str,
        cart_total: i64,
        cart_id: Option<&str>,
        payment_id: Option<&str>,
        now_ms: i64,
    ) -> Result<RedemptionResult, LookupError> {
        if cart_total <= 0 {
            return Err(LookupError::Depleted);
        }

        let mut t = self.lock();
        let normalized_code = Self::normalize_code(code);

        let mut card = all_cards(&mut t.cards)
            .into_iter()
            .find(|c| c.code == normalized_code)
            .ok_or(LookupError::NotFound)?;

        if card.status != "active" {
            return Err(LookupError::Inactive);
        }
        if let Some(exp) = card.expires_at {
            if exp <= now_ms {
                return Err(LookupError::Expired);
            }
        }
        if card.balance <= 0 {
            return Err(LookupError::Depleted);
        }

        let applied = card.balance.min(cart_total);
        let new_balance = card.balance - applied;
        let new_status = if new_balance == 0 { "used" } else { "active" };

        card.balance = new_balance;
        card.status = new_status.to_string();
        card.updated_at = now_ms;
        // Storage failures here are unexpected; surface them as Depleted so the
        // caller treats the redemption as unsuccessful (no balance was spent
        // since the commit did not land).
        if put_card(&mut t.cards, &card).is_err() || t.cards.commit().is_err() {
            return Err(LookupError::Depleted);
        }

        let redemption = GiftCardRedemption {
            id: next_id("gcrp"),
            gift_card_id: card.id.clone(),
            cart_id: cart_id.map(|s| s.to_string()),
            payment_id: payment_id.map(|s| s.to_string()),
            amount: applied,
            redeemed_at: now_ms,
        };
        let _ = put_redemption(&mut t.redemptions, &redemption);
        let _ = t.redemptions.commit();

        Ok(RedemptionResult {
            gift_card_id: card.id,
            applied,
            remaining_balance: new_balance,
            remaining_cart_total: cart_total - applied,
        })
    }

    pub fn issue(
        &self,
        amount: i64,
        currency: Option<&str>,
        purchased_by_user_id: Option<&str>,
        cart_id: Option<&str>,
        recipient_email: Option<&str>,
        recipient_phone: Option<&str>,
        recipient_name: Option<&str>,
        message: Option<&str>,
        expires_at: Option<i64>,
        delivery_at: Option<i64>,
        now_ms: i64,
    ) -> Result<(String, String), String> {
        for _ in 0..5 {
            let code = Self::generate_code();
            let card = GiftCard {
                id: next_id("gc"),
                code: code.clone(),
                amount,
                balance: amount,
                currency: currency.unwrap_or("ISK").to_string(),
                status: "active".to_string(),
                expires_at,
                delivery_at,
                delivered_at: None,
                purchased_by_user_id: purchased_by_user_id.map(|s| s.to_string()),
                cart_id: cart_id.map(|s| s.to_string()),
                recipient_email: recipient_email.map(|s| s.to_string()),
                recipient_phone: recipient_phone.map(|s| s.to_string()),
                recipient_name: recipient_name.map(|s| s.to_string()),
                message: message.map(|s| s.to_string()),
                created_at: now_ms,
                updated_at: now_ms,
            };

            let mut t = self.lock();

            if all_cards(&mut t.cards).iter().any(|c| c.code == card.code) {
                continue;
            }

            if let Err(e) = put_card(&mut t.cards, &card) {
                return Err(format!("storage error: {e}"));
            }
            if let Err(e) = t.cards.commit() {
                return Err(format!("storage error: {e}"));
            }

            return Ok((card.id, code));
        }

        Err("Failed to issue gift card after retries (too many collisions)".to_string())
    }

    pub fn get_by_id(&self, id: &str) -> Option<GiftCard> {
        let mut t = self.lock();
        all_cards(&mut t.cards).into_iter().find(|c| c.id == id)
    }

    pub fn list_all(&self) -> Vec<GiftCard> {
        let mut t = self.lock();
        all_cards(&mut t.cards)
    }

    /// List the redemption history of a gift card, newest first (admin detail).
    pub fn redemptions_for_card(&self, gift_card_id: &str) -> Vec<GiftCardRedemption> {
        let mut t = self.lock();
        let mut rows: Vec<GiftCardRedemption> = full_scan(&mut t.redemptions)
            .iter()
            .filter_map(redemption_from_value)
            .filter(|r| r.gift_card_id == gift_card_id)
            .collect();
        rows.sort_by(|a, b| b.redeemed_at.cmp(&a.redeemed_at).then(a.id.cmp(&b.id)));
        rows
    }

    pub fn pending_deliveries(&self, now_ms: i64) -> Vec<GiftCard> {
        let mut t = self.lock();
        all_cards(&mut t.cards)
            .into_iter()
            .filter(|c| {
                let should_deliver = c.delivery_at.map(|d| d <= now_ms).unwrap_or(false);
                c.delivered_at.is_none() && c.status == "active" && should_deliver
            })
            .collect()
    }

    pub fn mark_delivered(&self, id: &str, now_ms: i64) -> Result<(), String> {
        let mut t = self.lock();
        if let Some(mut card) = all_cards(&mut t.cards).into_iter().find(|c| c.id == id) {
            card.delivered_at = Some(now_ms);
            card.updated_at = now_ms;
            put_card(&mut t.cards, &card).map_err(io_err)?;
            t.cards.commit().map_err(io_err)?;
            Ok(())
        } else {
            Err("gift card not found".to_string())
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LookupError {
    NotFound,
    Inactive,
    Expired,
    Depleted,
}

impl LookupError {
    pub fn as_str(&self) -> &'static str {
        match self {
            LookupError::NotFound => "not_found",
            LookupError::Inactive => "inactive",
            LookupError::Expired => "expired",
            LookupError::Depleted => "depleted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RedemptionResult {
    pub gift_card_id: String,
    pub applied: i64,
    pub remaining_balance: i64,
    pub remaining_cart_total: i64,
}

fn next_id(prefix: &str) -> String {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format!("{prefix}-{now}-{seq}")
}

fn random_bytes(len: usize) -> io::Result<Vec<u8>> {
    use std::fs::File;
    use std::io::Read as _;
    let mut file = File::open("/dev/urandom")?;
    let mut bytes = vec![0u8; len];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn io_err(e: io::Error) -> String {
    format!("storage error: {e}")
}

fn gift_card_record(card: &GiftCard) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(card.id.clone())),
        ("code".into(), Value::Str(card.code.clone())),
        ("amount".into(), Value::Int(card.amount)),
        ("balance".into(), Value::Int(card.balance)),
        ("currency".into(), Value::Str(card.currency.clone())),
        ("status".into(), Value::Str(card.status.clone())),
        ("expires_at".into(), opt_int(card.expires_at)),
        ("delivery_at".into(), opt_int(card.delivery_at)),
        ("delivered_at".into(), opt_int(card.delivered_at)),
        (
            "purchased_by_user_id".into(),
            opt_str(&card.purchased_by_user_id),
        ),
        ("cart_id".into(), opt_str(&card.cart_id)),
        ("recipient_email".into(), opt_str(&card.recipient_email)),
        ("recipient_phone".into(), opt_str(&card.recipient_phone)),
        ("recipient_name".into(), opt_str(&card.recipient_name)),
        ("message".into(), opt_str(&card.message)),
        ("created_at".into(), Value::Int(card.created_at)),
        ("updated_at".into(), Value::Int(card.updated_at)),
    ])
}

fn gift_card_from_value(v: &Value) -> Option<GiftCard> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let i = |k: &str| v.get(k).and_then(Value::as_i64);
    Some(GiftCard {
        id: s("id")?,
        code: s("code")?,
        amount: i("amount")?,
        balance: i("balance")?,
        currency: s("currency").unwrap_or_else(|| "ISK".into()),
        status: s("status")?,
        expires_at: i("expires_at"),
        delivery_at: i("delivery_at"),
        delivered_at: i("delivered_at"),
        purchased_by_user_id: s("purchased_by_user_id"),
        cart_id: s("cart_id"),
        recipient_email: s("recipient_email"),
        recipient_phone: s("recipient_phone"),
        recipient_name: s("recipient_name"),
        message: s("message"),
        created_at: i("created_at").unwrap_or(0),
        updated_at: i("updated_at").unwrap_or(0),
    })
}

fn redemption_from_value(v: &Value) -> Option<GiftCardRedemption> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let i = |k: &str| v.get(k).and_then(Value::as_i64);
    Some(GiftCardRedemption {
        id: s("id")?,
        gift_card_id: s("gift_card_id")?,
        cart_id: s("cart_id"),
        payment_id: s("payment_id"),
        amount: i("amount")?,
        redeemed_at: i("redeemed_at").unwrap_or(0),
    })
}

fn put_card(tree: &mut BTree, card: &GiftCard) -> io::Result<()> {
    tree.insert(
        card.id.as_bytes(),
        gift_card_record(card).to_json().as_bytes(),
    )
}

fn all_cards(tree: &mut BTree) -> Vec<GiftCard> {
    full_scan(tree)
        .iter()
        .filter_map(gift_card_from_value)
        .collect()
}

fn redemption_record(r: &GiftCardRedemption) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(r.id.clone())),
        ("gift_card_id".into(), Value::Str(r.gift_card_id.clone())),
        ("cart_id".into(), opt_str(&r.cart_id)),
        ("payment_id".into(), opt_str(&r.payment_id)),
        ("amount".into(), Value::Int(r.amount)),
        ("redeemed_at".into(), Value::Int(r.redeemed_at)),
    ])
}

fn put_redemption(tree: &mut BTree, r: &GiftCardRedemption) -> io::Result<()> {
    tree.insert(r.id.as_bytes(), redemption_record(r).to_json().as_bytes())
}

fn opt_str(value: &Option<String>) -> Value {
    value.clone().map(Value::Str).unwrap_or(Value::Null)
}

fn opt_int(value: Option<i64>) -> Value {
    value.map(Value::Int).unwrap_or(Value::Null)
}

fn full_scan(tree: &mut BTree) -> Vec<Value> {
    let hi = [0xff_u8; 64];
    tree.range(&[], &hi)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| akurai_json::parse(&String::from_utf8_lossy(raw)).ok())
        .collect()
}

// ---- Scheduled delivery ----------------------------------------------------

/// Deliver a gift card to its recipient through the local plaintext sidecar.
///
/// This mirrors the auth layer's [`crate::auth`] `SidecarDeliver`: the framework
/// has no outbound TLS client, so all external sends go through a sidecar that
/// terminates TLS. When `SIDECAR_PORT` is unset, the code is logged to stdout
/// (dev/test mode) and no network call is made.
///
/// Prefers SMS (`/sms` with the recipient phone) and falls back to email
/// (`/email` with the recipient email). When neither contact is present the
/// delivery is treated as a no-op success (nothing to send).
pub fn dispatch_delivery(card: &GiftCard) -> Result<(), String> {
    let port = std::env::var("SIDECAR_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok());

    let amount_label = format!("{} {}", card.amount, card.currency);
    let greeting = card
        .recipient_name
        .as_deref()
        .map(|n| format!("Hæ {n}!\n"))
        .unwrap_or_default();
    let note = card
        .message
        .as_deref()
        .map(|m| format!("\n\n{m}"))
        .unwrap_or_default();
    let text = format!(
        "{greeting}Þú hefur fengið gjafabréf hjá Golfsetrinu Akureyri.\n\
         Kóði: {code}\nUpphæð: {amount}{note}",
        code = card.code,
        amount = amount_label,
    );

    let Some(port) = port else {
        // Dev/test: log instead of sending.
        println!(
            "[GiftCardDelivery] (no sidecar) would deliver {}: {text}",
            card.code
        );
        return Ok(());
    };

    // Prefer SMS, fall back to email.
    if let Some(phone) = card.recipient_phone.as_deref().filter(|s| !s.is_empty()) {
        let body = Value::Object(vec![
            ("to".into(), Value::Str(phone.to_string())),
            ("message".into(), Value::Str(text.clone())),
        ])
        .to_json();
        return post_sidecar(port, "/sms", &body);
    }

    if let Some(email) = card.recipient_email.as_deref().filter(|s| !s.is_empty()) {
        let html = format!(
            "<p>{greeting}</p><p>Þú hefur fengið gjafabréf hjá Golfsetrinu Akureyri.</p>\
             <p>Kóði: <strong>{code}</strong></p><p>Upphæð: {amount}</p>",
            code = card.code,
            amount = amount_label,
        );
        let body = Value::Object(vec![
            ("to".into(), Value::Str(email.to_string())),
            (
                "subject".into(),
                Value::Str("Gjafabréf — Golfsetrið Akureyri".into()),
            ),
            ("html".into(), Value::Str(html)),
            ("text".into(), Value::Str(text)),
        ])
        .to_json();
        return post_sidecar(port, "/email", &body);
    }

    // No recipient contact — nothing to send.
    Ok(())
}

/// POST a JSON body to the local sidecar over plaintext TCP (mirrors the auth
/// layer's sidecar contract). Returns `Ok` on a 2xx response.
fn post_sidecar(port: u16, path: &str, body: &str) -> Result<(), String> {
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;

    let body_bytes = body.as_bytes();
    let addr = format!("127.0.0.1:{port}");
    let mut stream =
        TcpStream::connect(&addr).map_err(|e| format!("sidecar connect {addr}: {e}"))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body_bytes.len(),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("sidecar write: {e}"))?;
    stream
        .write_all(body_bytes)
        .map_err(|e| format!("sidecar write: {e}"))?;
    stream.flush().map_err(|e| format!("sidecar flush: {e}"))?;

    let mut resp = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e) => return Err(format!("sidecar read: {e}")),
        };
        if n == 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n]);
        if resp.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let status_line = std::str::from_utf8(&resp)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .map(|n| (200..300).contains(&n))
        .unwrap_or(false);

    if ok {
        Ok(())
    } else {
        Err(format!("sidecar rejected delivery: {status_line}"))
    }
}

impl Store {
    /// Run one pass of the scheduled-delivery loop: find every gift card whose
    /// `delivery_at` is due and not yet delivered, dispatch it, and mark it
    /// delivered. Returns `(sent, errors)`. The app can call this on a timer.
    pub fn run_delivery_scheduler(&self, now_ms: i64) -> (usize, usize) {
        let pending = self.pending_deliveries(now_ms);
        if pending.is_empty() {
            return (0, 0);
        }

        let mut sent = 0;
        let mut errors = 0;
        for card in pending {
            match dispatch_delivery(&card) {
                Ok(()) => {
                    if self.mark_delivered(&card.id, now_ms).is_ok() {
                        sent += 1;
                    } else {
                        errors += 1;
                    }
                }
                Err(e) => {
                    eprintln!("[GiftCardScheduler] Failed to deliver {}: {e}", card.code);
                    errors += 1;
                }
            }
        }
        (sent, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "gsd-giftcards-test-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn generate_code_format() {
        let code = Store::generate_code();
        assert!(code.starts_with("GOLF-"));
        assert_eq!(code.matches('-').count(), 3);
        assert!(code.chars().all(|c| c.is_ascii_uppercase() || c == '-'));
    }

    #[test]
    fn normalize_code() {
        assert_eq!(
            Store::normalize_code("golf-abcd-efgh-jklm"),
            "GOLF-ABCD-EFGH-JKLM"
        );
        assert_eq!(Store::normalize_code("  GOLF ABCD  "), "GOLFABCD");
    }

    #[test]
    fn issue_lookup() {
        let (store, dir) = temp_store("issue");
        let now = 1000;
        let (id, code) = store
            .issue(
                5000, None, None, None, None, None, None, None, None, None, now,
            )
            .unwrap();
        assert!(!id.is_empty());
        assert!(code.starts_with("GOLF-"));
        let card = store.lookup(&code, now).unwrap();
        assert_eq!(card.balance, 5000);
        assert_eq!(card.status, "active");
        cleanup(&dir);
    }

    #[test]
    fn redeem() {
        let (store, dir) = temp_store("redeem");
        let now = 1000;
        let (_, code) = store
            .issue(
                5000, None, None, None, None, None, None, None, None, None, now,
            )
            .unwrap();
        let result = store.redeem(&code, 2000, None, None, now).unwrap();
        assert_eq!(result.applied, 2000);
        assert_eq!(result.remaining_balance, 3000);
        let card = store.lookup(&code, now).unwrap();
        assert_eq!(card.balance, 3000);
        assert_eq!(card.status, "active");
        cleanup(&dir);
    }

    #[test]
    fn errors() {
        let (store, dir) = temp_store("errors");
        let now = 1000;
        assert!(matches!(
            store.lookup("GOLF-XXXX-XXXX-XXXX", now),
            Err(LookupError::NotFound)
        ));
        let (_, code) = store
            .issue(
                5000, None, None, None, None, None, None, None, None, None, now,
            )
            .unwrap();
        store.redeem(&code, 5000, None, None, now).unwrap();
        assert!(matches!(
            store.lookup(&code, now),
            Err(LookupError::Depleted)
        ));
        cleanup(&dir);
    }
}
