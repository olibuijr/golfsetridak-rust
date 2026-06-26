//! Shop catalog storage over `akurai-storage` B+trees.
//!
//! The source app stores products and product categories in SQL. This port keeps
//! the same business fields in two string-keyed B+trees and exposes small CRUD
//! helpers for the HTTP layer. IDs are opaque text ids so API payloads stay close
//! to the source app's UUID-shaped contract without adding a UUID crate.

use akurai_json::Value;
use akurai_storage::BTree;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub struct ProductCategory {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub position: i64,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub price: i64,
    pub image_url: Option<String>,
    pub category_id: Option<String>,
    pub active: bool,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ProductUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub price: Option<i64>,
    pub image_url: Option<Option<String>>,
    pub category_id: Option<Option<String>>,
    pub active: Option<bool>,
    pub position: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ProductDraft<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub price: i64,
    pub image_url: Option<&'a str>,
    pub category_id: Option<&'a str>,
    pub active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CategoryUpdate {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<Option<String>>,
    pub position: Option<i64>,
    pub active: Option<bool>,
}

struct Trees {
    products: BTree,
    categories: BTree,
}

pub struct Store {
    inner: Mutex<Trees>,
}

static SHOP_SEQ: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        let open = |name: &str| BTree::open(data_dir.join(name));
        Ok(Store {
            inner: Mutex::new(Trees {
                products: open("products.db")?,
                categories: open("product_categories.db")?,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Trees> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list_categories(&self, active_only: bool) -> Vec<ProductCategory> {
        let mut t = self.lock();
        let mut rows: Vec<ProductCategory> = full_scan(&mut t.categories)
            .iter()
            .filter_map(category_from_value)
            .filter(|c| !active_only || c.active)
            .collect();
        rows.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.name.cmp(&b.name))
        });
        rows
    }

    pub fn category(&self, id: &str) -> Option<ProductCategory> {
        let mut t = self.lock();
        category_by_id(&mut t.categories, id)
    }

    pub fn create_category(
        &self,
        name: &str,
        slug: Option<&str>,
        description: Option<&str>,
        position: i64,
        active: bool,
        now_ms: i64,
    ) -> Result<ProductCategory, String> {
        let name = clean_required(name, "name required")?;
        let slug = slug
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| slugify(&name));
        if slug.is_empty() {
            return Err("valid slug required".into());
        }
        let mut t = self.lock();
        if category_slug_exists(&mut t.categories, &slug, None) {
            return Err("slug already exists".into());
        }
        let category = ProductCategory {
            id: next_id("cat", now_ms),
            name,
            slug,
            description: clean_optional(description),
            position,
            active,
            created_at: now_ms,
            updated_at: now_ms,
        };
        put_category(&mut t.categories, &category).map_err(io_err)?;
        t.categories.commit().map_err(io_err)?;
        Ok(category)
    }

    pub fn update_category(
        &self,
        id: &str,
        update: CategoryUpdate,
        now_ms: i64,
    ) -> Result<ProductCategory, String> {
        let mut t = self.lock();
        let mut category =
            category_by_id(&mut t.categories, id).ok_or_else(|| "not found".to_string())?;
        if let Some(name) = update.name {
            category.name = clean_required(&name, "name required")?;
        }
        if let Some(slug) = update.slug {
            let slug = slug.trim().to_string();
            if slug.is_empty() {
                return Err("valid slug required".into());
            }
            if category_slug_exists(&mut t.categories, &slug, Some(id)) {
                return Err("slug already exists".into());
            }
            category.slug = slug;
        }
        if let Some(description) = update.description {
            category.description = clean_optional(description.as_deref());
        }
        if let Some(position) = update.position {
            category.position = position;
        }
        if let Some(active) = update.active {
            category.active = active;
        }
        category.updated_at = now_ms;
        put_category(&mut t.categories, &category).map_err(io_err)?;
        t.categories.commit().map_err(io_err)?;
        Ok(category)
    }

    pub fn delete_category(&self, id: &str) -> Result<(), String> {
        let mut t = self.lock();
        if category_by_id(&mut t.categories, id).is_none() {
            return Err("not found".into());
        }
        for mut product in all_products(&mut t.products) {
            if product.category_id.as_deref() == Some(id) {
                product.category_id = None;
                put_product(&mut t.products, &product).map_err(io_err)?;
            }
        }
        t.products.commit().map_err(io_err)?;
        t.categories.delete(id.as_bytes()).map_err(io_err)?;
        t.categories.commit().map_err(io_err)
    }

    pub fn list_products(&self, active_only: bool) -> Vec<Product> {
        let mut t = self.lock();
        let mut rows: Vec<Product> = all_products(&mut t.products)
            .into_iter()
            .filter(|p| !active_only || p.active)
            .collect();
        rows.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.name.cmp(&b.name))
        });
        rows
    }

    pub fn product(&self, id: &str) -> Option<Product> {
        let mut t = self.lock();
        product_by_id(&mut t.products, id)
    }

    pub fn create_product(&self, draft: ProductDraft<'_>, now_ms: i64) -> Result<Product, String> {
        let name = clean_required(draft.name, "name required")?;
        if draft.price < 0 {
            return Err("valid price required".into());
        }
        let mut t = self.lock();
        let category_id = match clean_optional(draft.category_id) {
            Some(id) => {
                if category_by_id(&mut t.categories, &id).is_none() {
                    return Err("category not found".into());
                }
                Some(id)
            }
            None => None,
        };
        let product = Product {
            id: next_id("prod", now_ms),
            name,
            description: clean_optional(draft.description),
            price: draft.price,
            image_url: clean_optional(draft.image_url),
            category_id,
            active: draft.active,
            position: 0,
            created_at: now_ms,
            updated_at: now_ms,
        };
        put_product(&mut t.products, &product).map_err(io_err)?;
        t.products.commit().map_err(io_err)?;
        Ok(product)
    }

    pub fn update_product(
        &self,
        id: &str,
        update: ProductUpdate,
        now_ms: i64,
    ) -> Result<Product, String> {
        let mut t = self.lock();
        let mut product =
            product_by_id(&mut t.products, id).ok_or_else(|| "not found".to_string())?;
        if let Some(name) = update.name {
            product.name = clean_required(&name, "name required")?;
        }
        if let Some(description) = update.description {
            product.description = clean_optional(description.as_deref());
        }
        if let Some(price) = update.price {
            if price < 0 {
                return Err("valid price required".into());
            }
            product.price = price;
        }
        if let Some(image_url) = update.image_url {
            product.image_url = clean_optional(image_url.as_deref());
        }
        if let Some(category_id) = update.category_id {
            product.category_id = match clean_optional(category_id.as_deref()) {
                Some(id) => {
                    if category_by_id(&mut t.categories, &id).is_none() {
                        return Err("category not found".into());
                    }
                    Some(id)
                }
                None => None,
            };
        }
        if let Some(active) = update.active {
            product.active = active;
        }
        if let Some(position) = update.position {
            product.position = position;
        }
        product.updated_at = now_ms;
        put_product(&mut t.products, &product).map_err(io_err)?;
        t.products.commit().map_err(io_err)?;
        Ok(product)
    }

    pub fn delete_product(&self, id: &str) -> Result<(), String> {
        let mut t = self.lock();
        if product_by_id(&mut t.products, id).is_none() {
            return Err("not found".into());
        }
        t.products.delete(id.as_bytes()).map_err(io_err)?;
        t.products.commit().map_err(io_err)
    }

    pub fn category_name(&self, id: Option<&str>) -> Option<String> {
        let id = id?;
        self.category(id).map(|c| c.name)
    }
}

pub fn product_value(product: &Product, category_name: Option<String>) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(product.id.clone())),
        ("name".into(), Value::Str(product.name.clone())),
        ("description".into(), opt_str(&product.description)),
        ("price".into(), Value::Int(product.price)),
        ("priceLabel".into(), Value::Str(format_isk(product.price))),
        ("imageUrl".into(), opt_str(&product.image_url)),
        ("categoryId".into(), opt_str(&product.category_id)),
        (
            "categoryName".into(),
            category_name.map(Value::Str).unwrap_or(Value::Null),
        ),
        ("active".into(), Value::Bool(product.active)),
        ("position".into(), Value::Int(product.position)),
        ("createdAt".into(), Value::Int(product.created_at)),
        ("updatedAt".into(), Value::Int(product.updated_at)),
        ("hasImage".into(), Value::Bool(product.image_url.is_some())),
    ])
}

pub fn category_value(category: &ProductCategory) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(category.id.clone())),
        ("name".into(), Value::Str(category.name.clone())),
        ("slug".into(), Value::Str(category.slug.clone())),
        ("description".into(), opt_str(&category.description)),
        ("position".into(), Value::Int(category.position)),
        ("active".into(), Value::Bool(category.active)),
        ("createdAt".into(), Value::Int(category.created_at)),
        ("updatedAt".into(), Value::Int(category.updated_at)),
        ("inactive".into(), Value::Bool(!category.active)),
    ])
}

pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in text.trim().to_lowercase().chars() {
        let repl = match ch {
            'a'..='z' | '0'..='9' => Some(ch.to_string()),
            'á' | 'à' | 'â' | 'ä' | 'å' => Some("a".into()),
            'é' | 'è' | 'ê' | 'ë' => Some("e".into()),
            'í' | 'ì' | 'î' | 'ï' => Some("i".into()),
            'ó' | 'ò' | 'ô' => Some("o".into()),
            'ö' => Some("o".into()),
            'ú' | 'ù' | 'û' | 'ü' => Some("u".into()),
            'ý' | 'ÿ' => Some("y".into()),
            'æ' => Some("ae".into()),
            'þ' => Some("th".into()),
            'ð' => Some("d".into()),
            _ => None,
        };
        if let Some(s) = repl {
            if !s.is_empty() {
                out.push_str(&s);
                last_dash = false;
            }
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub fn format_isk(amount: i64) -> String {
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

fn next_id(prefix: &str, now_ms: i64) -> String {
    let seq = SHOP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{now_ms}-{seq}")
}

fn clean_required(value: &str, error: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(error.into())
    } else {
        Ok(trimmed.to_string())
    }
}

fn clean_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn opt_str(value: &Option<String>) -> Value {
    value.clone().map(Value::Str).unwrap_or(Value::Null)
}

fn io_err(e: io::Error) -> String {
    format!("storage error: {e}")
}

fn product_record(product: &Product) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(product.id.clone())),
        ("name".into(), Value::Str(product.name.clone())),
        ("description".into(), opt_str(&product.description)),
        ("price".into(), Value::Int(product.price)),
        ("image_url".into(), opt_str(&product.image_url)),
        ("category_id".into(), opt_str(&product.category_id)),
        ("active".into(), Value::Bool(product.active)),
        ("position".into(), Value::Int(product.position)),
        ("created_at".into(), Value::Int(product.created_at)),
        ("updated_at".into(), Value::Int(product.updated_at)),
    ])
}

fn product_from_value(v: &Value) -> Option<Product> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Product {
        id: s("id")?,
        name: s("name")?,
        description: s("description"),
        price: v.get("price").and_then(Value::as_i64)?,
        image_url: s("image_url"),
        category_id: s("category_id"),
        active: v.get("active").and_then(Value::as_bool).unwrap_or(true),
        position: v.get("position").and_then(Value::as_i64).unwrap_or(0),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
        updated_at: v.get("updated_at").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn put_product(tree: &mut BTree, product: &Product) -> io::Result<()> {
    tree.insert(
        product.id.as_bytes(),
        product_record(product).to_json().as_bytes(),
    )
}

fn product_by_id(tree: &mut BTree, id: &str) -> Option<Product> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    product_from_value(&v)
}

fn all_products(tree: &mut BTree) -> Vec<Product> {
    full_scan(tree)
        .iter()
        .filter_map(product_from_value)
        .collect()
}

fn category_record(category: &ProductCategory) -> Value {
    Value::Object(vec![
        ("id".into(), Value::Str(category.id.clone())),
        ("name".into(), Value::Str(category.name.clone())),
        ("slug".into(), Value::Str(category.slug.clone())),
        ("description".into(), opt_str(&category.description)),
        ("position".into(), Value::Int(category.position)),
        ("active".into(), Value::Bool(category.active)),
        ("created_at".into(), Value::Int(category.created_at)),
        ("updated_at".into(), Value::Int(category.updated_at)),
    ])
}

fn category_from_value(v: &Value) -> Option<ProductCategory> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(ProductCategory {
        id: s("id")?,
        name: s("name")?,
        slug: s("slug")?,
        description: s("description"),
        position: v.get("position").and_then(Value::as_i64).unwrap_or(0),
        active: v.get("active").and_then(Value::as_bool).unwrap_or(true),
        created_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0),
        updated_at: v.get("updated_at").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn put_category(tree: &mut BTree, category: &ProductCategory) -> io::Result<()> {
    tree.insert(
        category.id.as_bytes(),
        category_record(category).to_json().as_bytes(),
    )
}

fn category_by_id(tree: &mut BTree, id: &str) -> Option<ProductCategory> {
    let raw = tree.get(id.as_bytes()).ok().flatten()?;
    let v = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
    category_from_value(&v)
}

fn category_slug_exists(tree: &mut BTree, slug: &str, except_id: Option<&str>) -> bool {
    full_scan(tree)
        .iter()
        .filter_map(category_from_value)
        .any(|c| c.slug == slug && Some(c.id.as_str()) != except_id)
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
            "gsd-shop-test-{tag}-{}-{}",
            std::process::id(),
            SHOP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn product_crud_round_trip() {
        let (store, dir) = temp_store("crud");
        let cat = store
            .create_category("Hanskar", None, Some("Aukahlutir"), 0, true, 100)
            .unwrap();
        assert_eq!(cat.slug, "hanskar");

        let product = store
            .create_product(
                ProductDraft {
                    name: "Golfhanski",
                    description: Some("Vinstri hönd"),
                    price: 2990,
                    image_url: None,
                    category_id: Some(&cat.id),
                    active: true,
                },
                101,
            )
            .unwrap();
        assert_eq!(store.list_products(true).len(), 1);
        assert_eq!(store.product(&product.id).unwrap().price, 2990);

        let updated = store
            .update_product(
                &product.id,
                ProductUpdate {
                    price: Some(3490),
                    active: Some(false),
                    ..ProductUpdate::default()
                },
                102,
            )
            .unwrap();
        assert_eq!(updated.price, 3490);
        assert!(store.list_products(true).is_empty());
        assert_eq!(store.list_products(false).len(), 1);

        store.delete_product(&product.id).unwrap();
        assert!(store.product(&product.id).is_none());
        cleanup(&dir);
    }
}
