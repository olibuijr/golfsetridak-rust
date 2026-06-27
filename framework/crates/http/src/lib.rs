//! AkurAI-Framework HTTP core — pure `std`, zero dependencies.
//!
//! This crate hand-rolls HTTP/1.1 because the framework links no external
//! crates at runtime (see `AGENTS.md` principle #1): a request-head parser, a
//! response builder, a small thread pool, and a blocking thread-pooled server.

#![forbid(unsafe_code)]

pub mod form;
pub mod middleware;
pub mod middleware_builtins;
pub mod multipart;
pub mod pool;
pub mod reply;
pub mod request;
pub mod response;
pub mod server;
pub mod sse;

pub use middleware::{Middleware, MiddlewareStack};
pub use middleware_builtins::{RequestLogger, SecurityHeaders, Timing};
pub use multipart::{parse_multipart, MultipartError, Part};
pub use reply::{Hijack, Reply, Upgrade};
pub use request::{Method, ParseError, Request};
pub use response::Response;
pub use server::{Handler, Server};
pub use sse::{sse, Event, SseSink};
