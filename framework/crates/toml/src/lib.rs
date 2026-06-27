//! AkurAI-Framework TOML — pure `std`, zero dependencies.
//!
//! A small, correct parser for the slice of TOML the framework needs to declare
//! data collections in `backend/collections.toml`. It produces an
//! order-preserving [`Value`] (mirroring `akurai-json`'s shape) so the schema
//! reads the way it was written.
//!
//! # Supported subset
//!
//! - `key = value` lines with bare keys (`A-Za-z0-9_-`) and dotted bare keys
//!   (`a.b = 1`).
//! - Values: basic strings (`"..."` with `\"`, `\\`, `\n`, `\t` escapes),
//!   integers (optional sign and `_` separators), floats, booleans, and inline
//!   arrays (`[a, b, c]`, trailing comma and inner newlines allowed).
//! - `[table]` headers and `[[array of tables]]` headers, both with dotted
//!   paths (`[collection.field]`).
//! - `#` comments (whole-line and trailing), blank lines, arbitrary indentation.
//!
//! # Out of scope (errors cleanly, never panics)
//!
//! Multi-line strings (`"""`), literal strings (`'...'`), datetimes, inline
//! tables (`{...}`), and non-decimal integers (`0x`/`0o`/`0b`).

#![forbid(unsafe_code)]

mod error;
mod lexer;
mod parser;
mod value;

pub use error::TomlError;
pub use parser::parse;
pub use value::Value;
