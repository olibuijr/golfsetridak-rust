//! AkurAI-Framework content-addressed blob store — pure `std`, zero external
//! dependencies (it rests on the framework's own [`akurai_storage`] B+tree).
//!
//! A [`BlobStore`] keeps arbitrary byte payloads keyed by a hash of their own
//! content. Content addressing buys two things for free:
//!
//! * **Deduplication** — identical bytes hash to the same id, so storing the
//!   same file twice is a no-op and costs no extra space.
//! * **Integrity of reference** — the id *is* a digest of the data, so a caller
//!   holding an id is naming an exact byte sequence, not a mutable slot.
//!
//! ## The hash
//!
//! The id is a 128-bit [FNV-1a] digest rendered as 32 lowercase hex chars.
//! FNV-1a is **not cryptographic** — it is fast, std-only, and deterministic,
//! which is all a content key for trusted local uploads needs. It is *not* a
//! defense against an adversary deliberately crafting two different inputs that
//! collide. Widening the digest to 128 bits (versus the 64-bit hash the asset
//! fingerprinter uses) makes accidental collisions astronomically unlikely for
//! the upload volumes this store is built for. If a future requirement needs
//! collision resistance against malicious input, swap [`content_id`] for a
//! cryptographic hash; nothing else in the API changes.
//!
//! [FNV-1a]: https://en.wikipedia.org/wiki/Fowler–Noll–Vo_hash_function
//! [`akurai_storage`]: akurai_storage

#![forbid(unsafe_code)]

use std::io;
use std::path::Path;

use akurai_storage::BTree;

/// FNV-1a 128-bit offset basis and prime (see the module docs for the caveat).
const FNV128_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// A persistent, content-addressed store of byte payloads.
///
/// Each blob is keyed by [`content_id`] of its bytes and stored verbatim, so a
/// `put` followed by `get` round-trips the exact payload. Backed by a single
/// [`BTree`] file; every mutating call commits, so writes survive a reopen.
pub struct BlobStore {
    tree: BTree,
}

impl BlobStore {
    /// Open (creating if absent) the blob store at `path`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<BlobStore> {
        Ok(BlobStore {
            tree: BTree::open(path)?,
        })
    }

    /// Store `bytes`, returning their content id (32-char lowercase hex).
    ///
    /// Idempotent: storing bytes already present writes nothing and returns the
    /// same id, so identical payloads are deduplicated automatically.
    pub fn put(&mut self, bytes: &[u8]) -> io::Result<String> {
        let id = content_id(bytes);
        if self.tree.get(id.as_bytes())?.is_none() {
            self.tree.insert(id.as_bytes(), bytes)?;
            self.tree.commit()?;
        }
        Ok(id)
    }

    /// Fetch the bytes for `id`, or `None` if no such blob exists.
    pub fn get(&mut self, id: &str) -> io::Result<Option<Vec<u8>>> {
        self.tree.get(id.as_bytes())
    }

    /// Whether a blob with `id` is present.
    pub fn exists(&mut self, id: &str) -> io::Result<bool> {
        Ok(self.tree.get(id.as_bytes())?.is_some())
    }

    /// Remove the blob `id`. Returns whether it was present. Committed on
    /// success so the deletion survives a reopen.
    pub fn delete(&mut self, id: &str) -> io::Result<bool> {
        let removed = self.tree.delete(id.as_bytes())?;
        if removed {
            self.tree.commit()?;
        }
        Ok(removed)
    }
}

/// Content id of `bytes`: a 128-bit FNV-1a digest as 32 lowercase hex chars.
///
/// Deterministic — identical bytes always yield the same id, and a single
/// flipped byte reliably changes it. Not cryptographic (see the module docs).
pub fn content_id(bytes: &[u8]) -> String {
    let mut hash = FNV128_OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u128;
        hash = hash.wrapping_mul(FNV128_PRIME);
    }
    format!("{hash:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Filesystem-backed `BlobStore` tests live in `tests/store.rs`, where
    // `CARGO_TARGET_TMPDIR` is defined (it is only set for integration tests).
    // These unit tests cover the pure hashing logic.

    #[test]
    fn content_id_is_stable_and_sensitive() {
        assert_eq!(content_id(b"x"), content_id(b"x"));
        assert_ne!(content_id(b"x"), content_id(b"y"));
        assert_eq!(content_id(b"").len(), 32);
        // Single flipped byte changes the digest.
        assert_ne!(content_id(b"aaaa"), content_id(b"aaab"));
    }
}
