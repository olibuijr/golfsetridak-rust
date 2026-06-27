//! Integration tests for the pager. Files are created under
//! `CARGO_TARGET_TMPDIR` (inside `target/`, never `/tmp`).

use akurai_storage::{Pager, PAGE_SIZE};
use std::path::PathBuf;

/// A unique-ish path under the cargo target tmp dir for one test.
fn db_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(format!("pager-{name}.db"));
    let _ = std::fs::remove_file(&p); // start clean
    p
}

#[test]
fn allocate_assigns_sequential_ids_and_grows_file() {
    let mut pager = Pager::open(db_path("alloc")).unwrap();
    assert_eq!(pager.page_count().unwrap(), 0);
    assert_eq!(pager.allocate().unwrap(), 0);
    assert_eq!(pager.allocate().unwrap(), 1);
    assert_eq!(pager.page_count().unwrap(), 2);
}

#[test]
fn write_then_read_round_trips() {
    let mut pager = Pager::open(db_path("rw")).unwrap();
    let id = pager.allocate().unwrap();

    let mut page = [0u8; PAGE_SIZE];
    page[0] = 0xAB;
    page[PAGE_SIZE - 1] = 0xCD;
    pager.write_page(id, &page).unwrap();

    let read = pager.read_page(id).unwrap();
    assert_eq!(read[0], 0xAB);
    assert_eq!(read[PAGE_SIZE - 1], 0xCD);
}

#[test]
fn data_persists_across_reopen() {
    let path = db_path("persist");

    {
        let mut pager = Pager::open(&path).unwrap();
        let id = pager.allocate().unwrap();
        let mut page = [0u8; PAGE_SIZE];
        page[42] = 0x99;
        pager.write_page(id, &page).unwrap();
        pager.sync().unwrap();
    } // pager dropped, file closed

    let mut reopened = Pager::open(&path).unwrap();
    assert_eq!(reopened.page_count().unwrap(), 1);
    assert_eq!(reopened.read_page(0).unwrap()[42], 0x99);
}

#[test]
fn reading_past_end_errors() {
    let mut pager = Pager::open(db_path("oob")).unwrap();
    pager.allocate().unwrap();
    assert!(pager.read_page(0).is_ok());
    assert!(pager.read_page(1).is_err());
}

#[test]
fn writing_to_the_next_page_extends_but_a_gap_is_rejected() {
    let mut pager = Pager::open(db_path("gap")).unwrap();
    // writing page 0 on an empty file is allowed (id == page_count)
    pager.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
    assert_eq!(pager.page_count().unwrap(), 1);
    // skipping to page 2 would leave page 1 as a gap → rejected
    assert!(pager.write_page(2, &[1u8; PAGE_SIZE]).is_err());
}

#[test]
fn overwrite_replaces_in_place_without_growing() {
    let mut pager = Pager::open(db_path("overwrite")).unwrap();
    let id = pager.allocate().unwrap();
    pager.write_page(id, &[0x11; PAGE_SIZE]).unwrap();
    pager.write_page(id, &[0x22; PAGE_SIZE]).unwrap();
    assert_eq!(pager.page_count().unwrap(), 1);
    assert_eq!(pager.read_page(id).unwrap()[0], 0x22);
}
