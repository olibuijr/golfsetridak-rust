//! AkurAI-Framework JSON — pure `std`, zero dependencies.
//!
//! A small, correct JSON implementation: an order-preserving [`Value`] type, a
//! recursive-descent [`parse`]r, and a serializer. Integers and floats are kept
//! distinct ([`Value::Int`] / [`Value::Float`]) so record IDs survive a
//! round-trip exactly — important once this feeds the database layer.

#![forbid(unsafe_code)]

mod error;
mod parse;
mod value;

pub use error::Error;
pub use parse::parse;
pub use value::Value;
