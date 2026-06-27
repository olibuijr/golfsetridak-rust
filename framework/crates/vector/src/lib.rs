//! AkurAI-Framework vector — pure-`std` semantic-search primitives.
//!
//! Two halves, both dependency-free (only the workspace's own `akurai-json`):
//!
//! 1. **Vector math + codec** — [`cosine`] similarity, top-k [`rank`]ing, and
//!    little-endian f32 [`encode`]/[`decode`] so embeddings can be stored as
//!    byte blobs in the B+tree.
//! 2. **Embedding client** — [`embed`] / [`embed_many`] POST to an
//!    OpenAI-compatible `/v1/embeddings` endpoint over a minimal plain-HTTP/1.1
//!    client built on `std::net::TcpStream`. Bearer-token variants are
//!    available for protected loopback routers. **No TLS** (std can't; TLS
//!    terminates at the edge) — an `https://` endpoint returns
//!    [`EmbedError::TlsUnsupported`].
//!
//! The crate is **config-free**: the endpoint and model are always parameters.
//! The CLI reads `AKURAI_EMBED_URL` / `AKURAI_EMBED_MODEL` and drives `?search`
//! by embedding the query, computing [`cosine`] against stored record
//! embeddings, and taking the top-k. When no endpoint is set the CLI falls back
//! to substring search — that fallback lives in the CLI, not here.
//!
//! This crate does **not** wire HTTP routes or touch collections.

#![forbid(unsafe_code)]

mod client;
mod codec;
mod error;
mod http;
mod math;

pub use client::{
    embed, embed_many, embed_many_with_bearer, embed_with_bearer, parse_embeddings_response,
    EmbedInput,
};
pub use codec::{decode, encode};
pub use error::EmbedError;
pub use http::{build_request, parse_endpoint, split_response, Endpoint};
pub use math::{cosine, rank};
