# SHOP + CART Port Report

## Files Added

- `src/shop/mod.rs` — B+tree-backed product and product-category store, serializers, slug helper, product CRUD test.
- `src/cart/mod.rs` — B+tree-backed opaque cart sessions, cart items, add/update/remove/totals, cart tests.
- `frontend/shop.html` — `/verslun` and `/my/verslun` shop page.
- `frontend/cart.html` — `/my/verslun/karfa` cart page.
- `frontend/admin_products.html` — `/admin/vorur` product list.
- `frontend/admin_categories.html` — `/admin/vorur/flokkar` category manager.
- `frontend/admin_product_form.html` — `/admin/vorur/nytt` and `/admin/vorur/:id` product form.

## Files Changed

- `src/main.rs` — registered `shop` and `cart` modules.
- `src/serve.rs` — added shop/cart store startup, API dispatch arms, page dispatch arms, JSON handlers, cart item resolution, and page render contexts.
- `frontend/styles.css` — appended scoped shop/cart/admin styling on the existing dark-green design system.
- `backend/routes.json` — appended shop/cart/admin page route declarations.
- `backend/collections.toml` — appended `carts` and `cart_items` metadata declarations.

## Data Model

`src/shop` stores two B+trees under `data/shop/`:

- `product_categories.db`, keyed by category id.
- `products.db`, keyed by product id.

Product fields: `id`, `name`, `description`, `price`, `image_url`, `category_id`, `active`, `position`, `created_at`, `updated_at`.

Category fields: `id`, `name`, `slug`, `description`, `position`, `active`, `created_at`, `updated_at`.

`src/cart` stores two B+trees under `data/cart/`:

- `carts.db`, keyed by cart id.
- `cart_items.db`, keyed by cart item id.

Cart fields: `id`, `user_id`, `status`, `currency`, `created_at`, `updated_at`.

Cart item fields: `id`, `cart_id`, `type`, `ref_id`, `name_snapshot`, `unit_price`, `quantity`, `metadata`, `created_at`.

Supported item types: `product`, `package`, `slot`, `subscription`, `gift_card`.

Cookie model: `cart_id` is an opaque server-side cart id with `HttpOnly`, `SameSite=Lax`, `Path=/`, 30-day max age. No crypto crate was added.

## Endpoints

Public catalog:

- `GET /api/shop/products`
- `GET /api/shop/categories`

Admin catalog:

- `GET /api/admin/shop/products`
- `POST /api/admin/shop/products`
- `PUT /api/admin/shop/products` with `id`, plus `PUT /api/admin/shop/products/:id`
- `DELETE /api/admin/shop/products` with `id`, plus `DELETE /api/admin/shop/products/:id`
- `GET /api/admin/shop/categories`
- `POST /api/admin/shop/categories`
- `PUT /api/admin/shop/categories` with `id`, plus `PUT /api/admin/shop/categories/:id`
- `DELETE /api/admin/shop/categories` with `id`, plus `DELETE /api/admin/shop/categories/:id`

Cart:

- `GET /api/cart`
- `POST /api/cart/items`
- `PUT /api/cart/items/:id`
- `PATCH /api/cart/items/:id` for source compatibility
- `DELETE /api/cart/items/:id`
- `POST /api/cart/items/bulk`

Pages:

- `/verslun`
- `/my/verslun`
- `/my/verslun/karfa`
- `/admin/vorur`
- `/admin/vorur/flokkar`
- `/admin/vorur/nytt`
- `/admin/vorur/:id`

Checkout, payment, and fulfillment are not implemented.

## Verification

Commands run in the real worktree:

```text
cargo fmt --all -- --check
Result: pass

RUSTC_WRAPPER= cargo clippy --all-targets -- -D warnings
Result: pass

RUSTC_WRAPPER= cargo build --release
Result: pass

RUSTC_WRAPPER= cargo test
Result: pass, 45 passed
```

I used `RUSTC_WRAPPER=` because `sccache` is not permitted in this sandbox.

Added unit coverage:

- `shop::tests::product_crud_round_trip`
- `cart::tests::add_merges_products_and_totals`
- `cart::tests::update_and_remove_item`

## Smoke Test

Required server command:

```text
./target/release/golfsetridak serve --dir frontend --host 127.0.0.1 --port 18203
```

Actual output:

```text
error: Operation not permitted (os error 1)
```

The binary fails before startup when binding the local HTTP listener. This environment blocks the bind/listen operation, so the required curl smoke flow could not run.

Captured curl attempts after the bind failure:

```text
curl -i --max-time 2 http://127.0.0.1:18203/api/shop/products
curl: (7) Failed to connect to 127.0.0.1 port 18203 after 0 ms: Could not connect to server

curl -i --max-time 2 http://127.0.0.1:18203/verslun
curl: (7) Failed to connect to 127.0.0.1 port 18203 after 0 ms: Could not connect to server
```

The release binary itself builds successfully; only the local server smoke is blocked by the sandbox.
