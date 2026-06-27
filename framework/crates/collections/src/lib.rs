//! AkurAI-Framework collections — the auto-generated-API engine.
//!
//! Declare a [`Collection`] (a name plus typed [`Field`]s) and get full CRUD,
//! a substring [`Store::search`], and a JSON manifest ([`meta`]) over the
//! embedded [`akurai_storage`] B+tree — no hand-written endpoints. The CLI maps
//! the resulting HTTP-agnostic API onto REST routes at integration time.
//!
//! This crate is pure `std` plus the framework's own `akurai-storage` and
//! `akurai-json` crates: zero external runtime dependencies.
//!
//! ```no_run
//! use akurai_collections::{Collection, Field, FieldKind, Store, meta};
//! use akurai_json::Value;
//!
//! let posts = Collection::new(
//!     "posts",
//!     vec![
//!         Field::new("title", FieldKind::Text).required().embed(),
//!         Field::new("views", FieldKind::Int),
//!     ],
//! );
//!
//! let mut store = Store::open("app.db").unwrap();
//! let input = Value::Object(vec![("title".into(), Value::Str("hello".into()))]);
//! let record = store.create(&posts, input).unwrap();
//! assert!(record.get("id").is_some());
//!
//! let _manifest = meta(&[posts]);
//! ```
//!
//! ## What the engine owns vs. what you supply
//!
//! The engine assigns and owns two reserved keys on every record: `id`
//! (auto-increment u64) and `created` (unix seconds). Input may not set them.
//! Everything else comes from the schema fields you declare.
//!
//! ## The `embed` flag
//!
//! [`Field::embed`] marks a `Text` field for *later* semantic indexing. This
//! crate stores and exposes the flag (via [`meta`]) but does not embed anything
//! — the CLI's vector layer reads the flag and does the indexing on top.

#![forbid(unsafe_code)]

mod error;
mod meta;
mod schema;
mod store;

pub use error::CollError;
pub use meta::meta;
pub use schema::{Collection, Field, FieldKind};
pub use store::Store;
