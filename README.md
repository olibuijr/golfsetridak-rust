# golfsetridak-rust

Port of the [golfsetridak.is](https://golfsetridak.is) Next.js app to the
**AkurAI-Framework** — a single Rust binary with **zero external runtime
dependencies** (only `akurai-*` crates + `std`).

This is the **Phase 1 foundation**: design/layout, static content (news, the
user handbook, legal pages), the declarative data model, and "coming soon"
placeholders for the dynamic pages. See **[PORT.md](./PORT.md)** for the
architecture, data-model mapping, and the Phase 2/3 roadmap.

## Run

The binary depends on the framework via path deps, so keep this repo as a
**sibling of `AkurAI-Framework`** under the same parent directory:

```
cargo run --release -- serve --port 8090
# → http://127.0.0.1:8090
```

## Layout

```
src/        main.rs · serve.rs · content.rs · mime.rs
frontend/   templates (header/footer/article/…) + styles.css
backend/    routes.json · page.json · collections.toml · data/*.json
content/    frettir/ · notendahandbok/ · legal/ · um-okkur.md  (markdown)
```
