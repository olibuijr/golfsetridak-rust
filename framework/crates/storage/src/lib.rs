//! AkurAI-Framework storage engine — pure `std`, zero dependencies.
//!
//! The bottom of the database stack. Everything above (B+tree, catalog, SQL)
//! rests on a single-file [`Pager`] that reads and writes fixed-size pages and
//! can be forced durable with [`Pager::sync`]. Crash safety is built here
//! first, because nothing above it can be trusted otherwise.

#![forbid(unsafe_code)]

pub mod meta;
pub mod node;
pub mod pager;
pub mod tree;

pub use meta::Meta;
pub use node::Node;
pub use pager::{PageId, Pager, PAGE_SIZE};
pub use tree::BTree;
