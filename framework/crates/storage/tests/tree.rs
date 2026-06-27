//! Integration tests for the copy-on-write B+tree against real files.
//!
//! These exercise the property the unit tests can't reach cleanly: that a torn
//! or corrupt meta page on the *newest* slot is survived by recovering the
//! previous committed tree from the other slot.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

use akurai_storage::{BTree, PAGE_SIZE};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("akurai-tree-it-{}-{}.db", name, std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Overwrite an entire page with zeros, simulating a torn/lost write to it.
fn zero_page(path: &std::path::Path, page: u64) {
    let mut f = OpenOptions::new().write(true).open(path).unwrap();
    f.seek(SeekFrom::Start(page * PAGE_SIZE as u64)).unwrap();
    f.write_all(&[0u8; PAGE_SIZE]).unwrap();
    f.sync_all().unwrap();
}

#[test]
fn corrupt_newest_meta_slot_recovers_previous_commit() {
    let path = tmp("recover");

    // Two commits. Starting txn id is 1 (slot 1); commit -> 2 (slot 0);
    // commit -> 3 (slot 1). So the newest meta lives in slot 1 == page 1.
    {
        let mut t = BTree::open(&path).unwrap();
        t.insert(b"phase", b"one").unwrap();
        t.commit().unwrap();
        t.insert(b"phase", b"two").unwrap();
        t.commit().unwrap();
    }

    // Simulate the second commit's meta write being torn away.
    zero_page(&path, 1);

    // Recovery must fall back to the first commit's durable tree.
    let mut t = BTree::open(&path).unwrap();
    assert_eq!(t.get(b"phase").unwrap(), Some(b"one".to_vec()));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn large_dataset_survives_reopen() {
    let path = tmp("bulk");
    {
        let mut t = BTree::open(&path).unwrap();
        for i in 0..2_000u32 {
            t.insert(&i.to_be_bytes(), format!("val-{i}").as_bytes())
                .unwrap();
        }
        t.commit().unwrap();
    }
    let mut t = BTree::open(&path).unwrap();
    for i in 0..2_000u32 {
        assert_eq!(
            t.get(&i.to_be_bytes()).unwrap(),
            Some(format!("val-{i}").into_bytes()),
            "key {i}"
        );
    }
    // A spot range check across the multi-level tree.
    let window = t
        .range(&100u32.to_be_bytes(), &110u32.to_be_bytes())
        .unwrap();
    assert_eq!(window.len(), 10);

    std::fs::remove_file(&path).unwrap();
}
