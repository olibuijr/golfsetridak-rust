# golfsetridak-rust — Port Plan & Status

Port of the **golfsetridak** Next.js 16 app onto the **AkurAI-Framework** as a
single Rust binary. This document records the architecture, what Phase 1 (this
foundation) delivers, the data-model mapping, and the deferred work by phase.

## Architecture

The app is **its own Rust binary** (`golfsetridak`) that depends on the
AkurAI-Framework crates as **path dependencies** (`../AkurAI-Framework/crates/*`)
— exactly how the framework's own `crates/cli` serves its `site/`. The binary
links **zero external runtime crates** (only `akurai-*` + `std`), the same
invariant the framework holds. Release profile mirrors the framework
(`lto=fat`, `codegen-units=1`, `panic=abort`, `strip`).

It boots a built-in HTTP server (`akurai-http`), builds a template engine from
`frontend/*.html` (`akurai-template`), loads `backend/page.json` as the shared
render context (`akurai-json`), routes declarative pages through
`backend/routes.json` (`akurai-router`), renders `content/**.md` via
`akurai-markdown`, and generates atomic utility CSS via `akurai-css`. This
mirrors `AkurAI-Framework/crates/cli/src/cmd_serve.rs`.

### App directory (the framework's app model)

```
frontend/      header.html footer.html article.html frettir.html
               notendahandbok.html placeholder.html notfound.html styles.css
backend/       routes.json page.json collections.toml data/*.json
content/       frettir/*.md  notendahandbok/*.md + _index.json  legal/*.md  um-okkur.md
```

### No outbound TLS (sidecar plan)

The framework forbids outbound TLS in the binary. Every external integration
(SMS, email/SES, Landsbankinn payments, Payday invoicing) will go through a
**local plaintext sidecar** in a later phase. In this foundation those seams are
not implemented — they are documented below and the gateway/secret fields live
in `collections.toml` only as config shapes (never called).

### Crate dependencies — wired now vs deferred

Phase 1 links only the crates it actually uses, to keep the binary lean (no dead
weight): `akurai-http`, `akurai-json`, `akurai-template`, `akurai-markdown`,
`akurai-router`, `akurai-css`. The data-layer crates (`akurai-sqlexec`,
`akurai-storage`, `akurai-collections`, `akurai-toml`, `akurai-blobs`,
`akurai-vector`) and the realtime crate (`akurai-ws`) are added in Phase 2 when
the live auto-API + booking/realtime features land. `collections.toml` is
authored to the framework's exact format now so it is ready to mount.

## Phase 1 — done (this commit)

- Binary skeleton: `Cargo.toml`, `src/main.rs` (arg parsing, `serve`),
  `src/serve.rs` (server + dispatch + rendering), `src/content.rs` (frontmatter
  + listings), `src/mime.rs`. `rust-toolchain.toml`, `VERSION` (0.0.1),
  `.gitignore`, committed `Cargo.lock`.
- Design/layout port: dark theme, emerald/green brand, sticky glass header with
  nav (Forsíða / Fréttir / Verslun / Gjafabréf / Notendahandbók + "Mínar síður"
  login button), three-column footer, `styles.css` token set ported from
  golfsetridak's `globals.css` + components.
- Static content pages — fully ported and rendered via `akurai-markdown`:
  - Fréttir list `/frettir` + detail `/frettir/:slug` (3 articles)
  - Notendahandbók `/notendahandbok` + detail `/notendahandbok/:slug`
    (index lists all chapters; 4 have source markdown, the rest render
    non-linked — the source repo itself only ships those 4)
  - Persónuvernd `/personuvernd`, Skilmálar `/skilmalar`
  - Um okkur `/um-okkur` (authored from company facts — no source `.md` existed)
- Data model: `backend/collections.toml` (catalog/config entities — see mapping).
- Placeholder pages so routes + nav work: `/` (booking calendar), `/verslun`,
  `/gjafabref`, `/my`, `/checkout`, `/admin` — each renders "coming soon".
- `/api/health` JSON endpoint; `/utilities.css` from the CSS engine; 404 page.

## Data-model mapping (Drizzle → framework)

The framework's collection model fits **catalog/config** entities (REST CRUD +
validation, auto integer id). Entities needing atomic constraints/transactions
are kept out and handled in Phase 3 SQL.

### → `collections.toml` (Phase 1)

| Source table | Collection | Notes |
|---|---|---|
| productCategories | `product_categories` | `slug` UNIQUE → Phase-3 SQL |
| products | `products` | `category` relation → product_categories; `description` embed |
| packages | `packages` | klippikort slot packages |
| subscriptions | `subscriptions` | tier definitions |
| pricingRules | `pricing_rules` | hourly pricing |
| giftCards | `gift_cards` | issuance catalog; `balance` decrement is Phase 3 |
| smsTemplates | `sms_templates` | source PK `key` → UNIQUE text |
| emailTemplates | `email_templates` | source PK `key` → UNIQUE text |
| bankTransferSettings | `bank_transfer_settings` | singleton config |
| notificationSettings | `notification_settings` | singleton config |
| landsbankinnGatewaySettings | `landsbankinn_gateway_settings` | secrets → sidecar |
| paydaySettings | `payday_settings` | secrets → sidecar |

Constraints the collection model does **not** express (enforced in Phase 3 SQL):
DEFAULT values, UNIQUE, enum/check (status/role/type are plain `text`), composite
& partial indexes, text-UUID primary keys and UUID foreign keys (collections use
auto integer ids), epoch-ms timestamps are app-set.

### → SQL + transaction-backed (Phase 3, NOT collections)

These need atomic constraints / multi-row transactions and intended DDL:

- **bookings** — UNIQUE `starts_at` WHERE status IN (confirmed|pending) to
  prevent double-booking; slot lock + uniqueness check in one transaction.
  `id text pk, user_id fk, starts_at int, status text, payment_type text,
  price_paid int, user_package_id fk, user_subscription_id fk, cart_id fk,
  notes text, created_at int, cancelled_at int`.
- **carts** + **cart_items** — cart lifecycle + line items (CASCADE).
- **payments** — composite UNIQUE (provider, provider_ref); status transitions.
- **gift_card_redemptions** — atomically decrement `gift_cards.balance`.
- **user_packages** — atomic decrement of `remaining` slot count.
- **user_subscriptions** + **user_subscription_members** — two partial-unique
  constraints (active member by user_id; invited by phone); sharing/invites.
- **users** (business users; role enum, fixed_price, kennitala, payday_customer_id).
- **auth**: `user`, `session`, `account`, `verification` (better-auth shape) —
  Phase 2 when sessions/OTP land.
- **app_migrations** — migration tracking.

## Deferred work by phase

### Phase 2 — framework additions + auth
- [ ] Mount the live auto-API from `collections.toml` (add `akurai-collections`,
      `akurai-toml`, `akurai-storage`, `akurai-blobs`, `akurai-vector`).
- [ ] SQL aggregates / query helpers the booking + admin views need
      (`akurai-sqlexec`, `akurai-storage`).
- [ ] OTP login + roles/sessions (auth tables; cookie sessions).
- [ ] Realtime where useful (`akurai-ws`) — e.g. live calendar availability.
- [ ] Asset fingerprinting/minify pipeline (`akurai-assets`) for far-future
      caching (Phase 1 serves plain assets with `no-cache`).

### Phase 3 — features (SQL + transaction-backed)
- [ ] Booking engine: calendar, unique-slot enforcement, pricing rules.
- [ ] Cart + checkout flow.
- [ ] Shop fulfilment (products, categories, orders).
- [ ] Gift cards: purchase, delivery, redemption (balance decrement).
- [ ] Packages/subscriptions: purchase, slot/limit accounting, member invites.
- [ ] Admin: bookings, users, pricing, settings, templates, notifications.
- [ ] Payments: status machine + reconciliation.

### Sidecar (local plaintext, no TLS in this binary)
- [ ] SMS sending (templates → provider).
- [ ] Email / SES (templates → provider).
- [ ] Landsbankinn payment gateway.
- [ ] Payday invoicing / accounting + VAT.

## Local development

```
cd golfsetridak-rust        # sibling of AkurAI-Framework
cargo run --release -- serve --port 8090
# → http://127.0.0.1:8090
```

The path deps resolve `../AkurAI-Framework`; keep both repos as siblings under
the same parent directory.
