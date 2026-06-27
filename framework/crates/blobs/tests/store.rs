//! Filesystem round-trip tests for [`BlobStore`]. Integration tests so that
//! `CARGO_TARGET_TMPDIR` is available — every store lives in a unique file
//! under the target tmp dir, never `/tmp`.

use akurai_blobs::BlobStore;

/// A unique, freshly-removed file path inside `CARGO_TARGET_TMPDIR`.
fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(format!("blobstore-{}-{}.db", name, std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn put_then_get_round_trips_exact_bytes() {
    let path = tmp("roundtrip");
    let mut store = BlobStore::open(&path).unwrap();
    // Includes every byte value plus embedded NULs and CRLFs.
    let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    let id = store.put(&data).unwrap();
    assert_eq!(store.get(&id).unwrap(), Some(data));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn identical_bytes_dedupe_to_same_id() {
    let path = tmp("dedupe");
    let mut store = BlobStore::open(&path).unwrap();
    let id1 = store.put(b"same content").unwrap();
    let id2 = store.put(b"same content").unwrap();
    assert_eq!(id1, id2);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn different_bytes_get_different_ids() {
    let path = tmp("distinct");
    let mut store = BlobStore::open(&path).unwrap();
    let a = store.put(b"alpha").unwrap();
    let b = store.put(b"beta").unwrap();
    assert_ne!(a, b);
    assert_eq!(store.get(&a).unwrap(), Some(b"alpha".to_vec()));
    assert_eq!(store.get(&b).unwrap(), Some(b"beta".to_vec()));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn get_unknown_id_returns_none() {
    let path = tmp("unknown");
    let mut store = BlobStore::open(&path).unwrap();
    assert_eq!(store.get("deadbeef").unwrap(), None);
    assert!(!store.exists("deadbeef").unwrap());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn blobs_persist_across_reopen() {
    let path = tmp("persist");
    let id = {
        let mut store = BlobStore::open(&path).unwrap();
        store.put(b"durable payload").unwrap()
    };
    let mut store = BlobStore::open(&path).unwrap();
    assert!(store.exists(&id).unwrap());
    assert_eq!(store.get(&id).unwrap(), Some(b"durable payload".to_vec()));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn delete_removes_a_blob() {
    let path = tmp("delete");
    let mut store = BlobStore::open(&path).unwrap();
    let id = store.put(b"temporary").unwrap();
    assert!(store.delete(&id).unwrap());
    assert!(!store.delete(&id).unwrap()); // already gone
    assert_eq!(store.get(&id).unwrap(), None);
    std::fs::remove_file(&path).unwrap();
}
