//! Cart session and line-item storage.
//!
//! This ports the source app's cart/session + item model up to cart contents and
//! totals only. Checkout, payments, fulfillment, and gateway-specific behavior
//! intentionally stay outside this module.

use akurai_json::Value;
use akurai_storage::BTree;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const CART_COOKIE: &str = "cart_id";

#[derive(Debug, Clone, PartialEq)]
pub struct Cart {
    pub id: String,
    pub user_id: Option<String>,
    pub status: String,
    pub currency: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CartItem {
    pub id: String,
    pub cart_id: String,
    pub item_type: String,
    pub ref_id: String,
    pub name_snapshot: String,
    pub unit_price: i64,
    pub quantity: i64,
    pub metadata: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CartSummary {
    pub id: String,
    pub user_id: Option<String>,
    pub status: String,
    pub currency: String,
    pub items: Vec<CartItem>,
    pub subtotal: i64,
    pub item_count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedItem {
    pub item_type: String,
    pub ref_id: String,
    pub name_snapshot: String,
    pub unit_price: i64,
    pub quantity: i64,
    pub metadata: Value,
}

struct Trees {
    carts: BTree,
    items: BTree,
}

pub struct Store {
    inner: Mutex<Trees>,
}

static CART_SEQ: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        let open = |name: &str| BTree::open(data_dir.join(name));
        Ok(Store {
            inner: Mutex::new(Trees {
                carts: open("carts.db")?,
                items: open("cart_items.db")?,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Trees> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get_or_create_open(
        &self,
        cookie_cart_id: Option<&str>,
        now_ms: i64,
    ) -> Result<(CartSummary, bool), String> {
        let mut t = self.lock();
        let cart = cookie_cart_id
            .filter(|id| valid_cart_id(id))
            .and_then(|id| cart_by_id(&mut t.carts, id))
            .filter(|c| c.status == "open" && c.user_id.is_none());

        let (cart, created) = match cart {
            Some(cart) => (cart, false),
            None => {
                // Reuse the requested id when valid (stable per-user carts);
                // otherwise mint a random one (first-visit anonymous cart).
                let id = cookie_cart_id
                    .filter(|id| valid_cart_id(id))
                    .map(str::to_string)
                    .unwrap_or_else(|| next_id("cart", now_ms));
                let cart = Cart {
                    id,
                    user_id: None,
                    status: "open".into(),
                    currency: "ISK".into(),
                    created_at: now_ms,
                    updated_at: now_ms,
                };
                put_cart(&mut t.carts, &cart).map_err(io_err)?;
                t.carts.commit().map_err(io_err)?;
                (cart, true)
            }
        };
        Ok((summary_locked(&mut t, &cart), created))
    }

    pub fn add_item(
        &self,
        cart_id: &str,
        item: ResolvedItem,
        now_ms: i64,
    ) -> Result<CartSummary, String> {
        validate_item_type(&item.item_type)?;
        if item.ref_id.trim().is_empty() {
            return Err("refId required".into());
        }
        if item.name_snapshot.trim().is_empty() {
            return Err("nameSnapshot required".into());
        }
        if item.unit_price < 0 {
            return Err("unitPrice must be non-negative".into());
        }
        if item.quantity < 1 {
            return Err("quantity must be positive".into());
        }

        let mut t = self.lock();
        let mut cart = cart_by_id(&mut t.carts, cart_id)
            .filter(|c| c.status == "open")
            .ok_or_else(|| "cart not found".to_string())?;

        let existing = if item.item_type == "gift_card" {
            None
        } else {
            find_item(&mut t.items, cart_id, &item.item_type, &item.ref_id)
        };

        if let Some(mut existing) = existing {
            if item.item_type == "package" || item.item_type == "product" {
                existing.quantity += item.quantity;
                existing.unit_price = item.unit_price;
                existing.name_snapshot = item.name_snapshot;
                existing.metadata = item.metadata;
                put_item(&mut t.items, &existing).map_err(io_err)?;
                t.items.commit().map_err(io_err)?;
            }
        } else {
            let row = CartItem {
                id: next_id("ci", now_ms),
                cart_id: cart_id.to_string(),
                item_type: item.item_type,
                ref_id: item.ref_id,
                name_snapshot: item.name_snapshot,
                unit_price: item.unit_price,
                quantity: item.quantity,
                metadata: item.metadata,
                created_at: now_ms,
            };
            put_item(&mut t.items, &row).map_err(io_err)?;
            t.items.commit().map_err(io_err)?;
        }

        cart.updated_at = now_ms;
        put_cart(&mut t.carts, &cart).map_err(io_err)?;
        t.carts.commit().map_err(io_err)?;
        Ok(summary_locked(&mut t, &cart))
    }

    pub fn merge_open_cart_into(
        &self,
        source_cart_id: &str,
        target_cart_id: &str,
        now_ms: i64,
    ) -> Result<CartSummary, String> {
        let (target, _) = self.get_or_create_open(Some(target_cart_id), now_ms)?;
        if source_cart_id == target.id {
            return Ok(target);
        }

        let source_items = self
            .load_cart(source_cart_id)
            .filter(|(cart, _)| cart.status == "open")
            .map(|(_, items)| items)
            .unwrap_or_default();

        let mut next = target;
        for item in source_items {
            next = self.add_item(
                &next.id,
                ResolvedItem {
                    item_type: item.item_type,
                    ref_id: item.ref_id,
                    name_snapshot: item.name_snapshot,
                    unit_price: item.unit_price,
                    quantity: item.quantity,
                    metadata: item.metadata,
                },
                now_ms,
            )?;
        }
        self.clear_items(source_cart_id)?;
        Ok(next)
    }

    pub fn update_quantity(
        &self,
        cart_id: &str,
        item_id: &str,
        quantity: i64,
        now_ms: i64,
    ) -> Result<CartSummary, String> {
        if quantity < 0 {
            return Err("quantity must be a non-negative number".into());
        }
        let mut t = self.lock();
        let mut cart = cart_by_id(&mut t.carts, cart_id)
            .filter(|c| c.status == "open")
            .ok_or_else(|| "cart not found".to_string())?;
        let item = item_by_id(&mut t.items, item_id)
            .filter(|i| i.cart_id == cart_id)
            .ok_or_else(|| "item not found".to_string())?;
        if quantity == 0 {
            t.items.delete(item.id.as_bytes()).map_err(io_err)?;
        } else {
            let mut next = item;
            next.quantity = quantity;
            put_item(&mut t.items, &next).map_err(io_err)?;
        }
        t.items.commit().map_err(io_err)?;
        cart.updated_at = now_ms;
        put_cart(&mut t.carts, &cart).map_err(io_err)?;
        t.carts.commit().map_err(io_err)?;
        Ok(summary_locked(&mut t, &cart))
    }

    pub fn remove_item(
        &self,
        cart_id: &str,
        item_id: &str,
        now_ms: i64,
    ) -> Result<CartSummary, String> {
        let mut t = self.lock();
        let mut cart = cart_by_id(&mut t.carts, cart_id)
            .filter(|c| c.status == "open")
            .ok_or_else(|| "cart not found".to_string())?;
        if let Some(item) = item_by_id(&mut t.items, item_id).filter(|i| i.cart_id == cart_id) {
            t.items.delete(item.id.as_bytes()).map_err(io_err)?;
            t.items.commit().map_err(io_err)?;
        }
        cart.updated_at = now_ms;
        put_cart(&mut t.carts, &cart).map_err(io_err)?;
        t.carts.commit().map_err(io_err)?;
        Ok(summary_locked(&mut t, &cart))
    }

    /// Load a cart by id together with its line items, regardless of status.
    /// Used by checkout/fulfillment, which operate on `checking_out`/`paid`
    /// carts (the public `get_or_create_open` deliberately only returns *open*
    /// anonymous carts).
    pub fn load_cart(&self, cart_id: &str) -> Option<(Cart, Vec<CartItem>)> {
        let mut t = self.lock();
        let cart = cart_by_id(&mut t.carts, cart_id)?;
        let items = cart_items(&mut t.items, cart_id);
        Some((cart, items))
    }

    /// Remove every line item from a cart. Called by checkout fulfillment when a
    /// cart is paid: the lines are consumed, and clearing them keeps a fresh cart
    /// empty if its (deterministic, per-user) id is reused afterwards.
    pub fn clear_items(&self, cart_id: &str) -> Result<(), String> {
        let mut t = self.lock();
        for item in cart_items(&mut t.items, cart_id) {
            t.items.delete(item.id.as_bytes()).map_err(io_err)?;
        }
        t.items.commit().map_err(io_err)
    }

    /// Set a cart's lifecycle status (`open` | `checking_out` | `paid` |
    /// `failed`). Returns an error if the cart does not exist.
    pub fn set_status(&self, cart_id: &str, status: &str, now_ms: i64) -> Result<(), String> {
        let mut t = self.lock();
        let mut cart =
            cart_by_id(&mut t.carts, cart_id).ok_or_else(|| "cart not found".to_string())?;
        cart.status = status.to_string();
        cart.updated_at = now_ms;
        put_cart(&mut t.carts, &cart).map_err(io_err)?;
        t.carts.commit().map_err(io_err)
    }
}

pub fn cookie_cart_id(req: &akurai_http::Request) -> Option<String> {
    let raw = req.header("Cookie")?;
    for part in raw.split(';') {
        let (name, value) = part.trim().split_once('=')?;
        if name == CART_COOKIE && valid_cart_id(value) {
            return Some(value.to_string());
        }
    }
    None
}

pub fn cookie_header(cart_id: &str) -> String {
    format!("{CART_COOKIE}={cart_id}; Path=/; Max-Age=2592000; SameSite=Lax; HttpOnly")
}

pub fn cart_value(summary: &CartSummary) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(summary.id.clone())),
        ("userId".into(), opt_str(&summary.user_id)),
        ("status".into(), Value::Str(summary.status.clone())),
        ("currency".into(), Value::Str(summary.currency.clone())),
        (
            "items".into(),
            Value::Array(summary.items.iter().map(item_value).collect()),
        ),
        ("subtotal".into(), Value::Int(summary.subtotal)),
        ("itemCount".into(), Value::Int(summary.item_count)),
    ])
}

fn item_value(item: &CartItem) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(item.id.clone())),
        ("cartId".into(), Value::Str(item.cart_id.clone())),
        ("type".into(), Value::Str(item.item_type.clone())),
        (
            "typeLabel".into(),
            Value::Str(type_label(&item.item_type).to_string()),
        ),
        ("refId".into(), Value::Str(item.ref_id.clone())),
        (
            "nameSnapshot".into(),
            Value::Str(item.name_snapshot.clone()),
        ),
        ("unitPrice".into(), Value::Int(item.unit_price)),
        (
            "unitPriceLabel".into(),
            Value::Str(format_isk(item.unit_price)),
        ),
        ("quantity".into(), Value::Int(item.quantity)),
        ("metadata".into(), item.metadata.clone()),
        ("createdAt".into(), Value::Int(item.created_at)),
        (
            "lineTotal".into(),
            Value::Int(item.unit_price.saturating_mul(item.quantity)),
        ),
        (
            "lineTotalLabel".into(),
            Value::Str(format_isk(item.unit_price.saturating_mul(item.quantity))),
        ),
    ])
}

fn format_isk(amount: i64) -> String {
    let neg = amount < 0;
    let digits = amount.unsigned_abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(ch);
    }
    format!("{}{} kr", if neg { "-" } else { "" }, out)
}

fn type_label(item_type: &str) -> &'static str {
    match item_type {
        "product" => "Vara",
        "package" => "Klippikort",
        "slot" => "Tími",
        "subscription" => "Áskrift",
        "gift_card" => "Gjafabréf",
        _ => "Karfaliður",
    }
}

fn validate_item_type(item_type: &str) -> Result<(), String> {
    match item_type {
        "product" | "package" | "slot" | "subscription" | "gift_card" => Ok(()),
        _ => Err("invalid type".into()),
    }
}

fn valid_cart_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn next_id(prefix: &str, now_ms: i64) -> String {
    let seq = CART_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{now_ms}-{seq}")
}

fn opt_str(value: &Option<String>) -> Value {
    value.clone().map(Value::Str).unwrap_or(Value::Null)
}

fn io_err(e: io::Error) -> String {
    format!("storage error: {e}")
}

fn cart_record(cart: &Cart) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(cart.id.clone())),
        ("user_id".into(), opt_str(&cart.user_id)),
        ("status".into(), Value::Str(cart.status.clone())),
        ("currency".into(), Value::Str(cart.currency.clone())),
        ("created_at".into(), Value::Int(cart.created_at)),
        ("updated_at".into(), Value::Int(cart.updated_at)),
    ])
}

fn cart_from_value(v: &Value) -> Option<Cart> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Cart {
        id: s("id")?,
        user_id: s("user_id"),
        status: s("status")?,
        currency: s("currency").unwrap_or_else(|| "ISK".into()),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
        updated_at: v.get("updated_at").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn put_cart(tree: &mut BTree, cart: &Cart) -> io::Result<()> {
    tree.insert(cart.id.as_bytes(), cart_record(cart).to_json().as_bytes())
}

fn cart_by_id(tree: &mut BTree, id: &str) -> Option<Cart> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    cart_from_value(&v)
}

fn item_record(item: &CartItem) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(item.id.clone())),
        ("cart_id".into(), Value::Str(item.cart_id.clone())),
        ("type".into(), Value::Str(item.item_type.clone())),
        ("ref_id".into(), Value::Str(item.ref_id.clone())),
        (
            "name_snapshot".into(),
            Value::Str(item.name_snapshot.clone()),
        ),
        ("unit_price".into(), Value::Int(item.unit_price)),
        ("quantity".into(), Value::Int(item.quantity)),
        ("metadata".into(), item.metadata.clone()),
        ("created_at".into(), Value::Int(item.created_at)),
    ])
}

fn item_from_value(v: &Value) -> Option<CartItem> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(CartItem {
        id: s("id")?,
        cart_id: s("cart_id")?,
        item_type: s("type")?,
        ref_id: s("ref_id")?,
        name_snapshot: s("name_snapshot")?,
        unit_price: v.get("unit_price").and_then(Value::as_i64)?,
        quantity: v.get("quantity").and_then(Value::as_i64)?,
        metadata: v.get("metadata").cloned().unwrap_or(Value::Object(vec![])),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn put_item(tree: &mut BTree, item: &CartItem) -> io::Result<()> {
    tree.insert(item.id.as_bytes(), item_record(item).to_json().as_bytes())
}

fn item_by_id(tree: &mut BTree, id: &str) -> Option<CartItem> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    item_from_value(&v)
}

fn all_items(tree: &mut BTree) -> Vec<CartItem> {
    full_scan(tree).iter().filter_map(item_from_value).collect()
}

fn cart_items(tree: &mut BTree, cart_id: &str) -> Vec<CartItem> {
    let mut rows: Vec<CartItem> = all_items(tree)
        .into_iter()
        .filter(|i| i.cart_id == cart_id)
        .collect();
    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    rows
}

fn find_item(tree: &mut BTree, cart_id: &str, item_type: &str, ref_id: &str) -> Option<CartItem> {
    all_items(tree)
        .into_iter()
        .find(|i| i.cart_id == cart_id && i.item_type == item_type && i.ref_id == ref_id)
}

fn summary_locked(t: &mut Trees, cart: &Cart) -> CartSummary {
    let items = cart_items(&mut t.items, &cart.id);
    let subtotal = items
        .iter()
        .map(|i| i.unit_price.saturating_mul(i.quantity))
        .sum();
    let item_count = items.iter().map(|i| i.quantity).sum();
    CartSummary {
        id: cart.id.clone(),
        user_id: cart.user_id.clone(),
        status: cart.status.clone(),
        currency: cart.currency.clone(),
        items,
        subtotal,
        item_count,
    }
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
    use std::path::{Path, PathBuf};

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "gsd-cart-test-{tag}-{}-{}",
            std::process::id(),
            CART_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn product(ref_id: &str, price: i64, qty: i64) -> ResolvedItem {
        ResolvedItem {
            item_type: "product".into(),
            ref_id: ref_id.into(),
            name_snapshot: "Golfboltar".into(),
            unit_price: price,
            quantity: qty,
            metadata: Value::Object(vec![]),
        }
    }

    fn slot(ref_id: &str, price: i64) -> ResolvedItem {
        ResolvedItem {
            item_type: "slot".into(),
            ref_id: ref_id.into(),
            name_snapshot: "Tími".into(),
            unit_price: price,
            quantity: 1,
            metadata: Value::Object(vec![]),
        }
    }

    #[test]
    fn add_merges_products_and_totals() {
        let (store, dir) = temp_store("add");
        let (cart, _) = store.get_or_create_open(None, 100).unwrap();
        let cart = store
            .add_item(&cart.id, product("p1", 1200, 1), 101)
            .unwrap();
        assert_eq!(cart.subtotal, 1200);
        assert_eq!(cart.item_count, 1);

        let cart = store
            .add_item(&cart.id, product("p1", 1200, 2), 102)
            .unwrap();
        assert_eq!(cart.items.len(), 1);
        assert_eq!(cart.items[0].quantity, 3);
        assert_eq!(cart.subtotal, 3600);
        assert_eq!(cart.item_count, 3);
        cleanup(&dir);
    }

    #[test]
    fn update_and_remove_item() {
        let (store, dir) = temp_store("remove");
        let (cart, _) = store.get_or_create_open(None, 100).unwrap();
        let cart = store
            .add_item(&cart.id, product("p1", 500, 2), 101)
            .unwrap();
        let item_id = cart.items[0].id.clone();

        let cart = store.update_quantity(&cart.id, &item_id, 5, 102).unwrap();
        assert_eq!(cart.subtotal, 2500);
        assert_eq!(cart.item_count, 5);

        let cart = store.remove_item(&cart.id, &item_id, 103).unwrap();
        assert_eq!(cart.items.len(), 0);
        assert_eq!(cart.subtotal, 0);
        cleanup(&dir);
    }

    #[test]
    fn merge_open_cart_into_user_cart_moves_items_and_dedupes_slots() {
        let (store, dir) = temp_store("merge");
        let (anon, _) = store.get_or_create_open(Some("anon-cart"), 100).unwrap();
        store
            .add_item(&anon.id, product("p1", 1200, 2), 101)
            .unwrap();
        store
            .add_item(&anon.id, slot("2026-06-27T10:00:00.000Z", 3500), 102)
            .unwrap();
        store
            .add_item(&anon.id, slot("2026-06-27T10:00:00.000Z", 3500), 103)
            .unwrap();

        let merged = store
            .merge_open_cart_into("anon-cart", "user-cart", 104)
            .unwrap();
        assert_eq!(merged.id, "user-cart");
        assert_eq!(merged.items.len(), 2);
        assert_eq!(merged.item_count, 3);
        assert_eq!(merged.subtotal, 5900);

        let anon_after = store.get_or_create_open(Some("anon-cart"), 105).unwrap().0;
        assert!(anon_after.items.is_empty());
        cleanup(&dir);
    }
}
