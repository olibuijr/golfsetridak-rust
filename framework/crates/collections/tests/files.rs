//! Tests for the `File` field kind: a file field stores a `{blob,...}` JSON
//! descriptor object, accepts null/omitted JSON input, and rejects a
//! non-object/non-null value with a clear validation error.
//!
//! Each test gets its own file under `CARGO_TARGET_TMPDIR` (never `/tmp`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use akurai_collections::{meta, Collection, Field, FieldKind, Store};
use akurai_json::Value;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn db_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(format!("files-test-{n}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// A `documents` collection with a title and an optional `attachment` file field.
fn documents() -> Collection {
    Collection::new(
        "documents",
        vec![
            Field::new("title", FieldKind::Text).required(),
            Field::new("attachment", FieldKind::File),
        ],
    )
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn file_descriptor() -> Value {
    obj(vec![
        (
            "blob",
            Value::Str("0123456789abcdef0123456789abcdef".into()),
        ),
        ("filename", Value::Str("note.txt".into())),
        ("content_type", Value::Str("text/plain".into())),
        ("size", Value::Int(13)),
    ])
}

#[test]
fn file_field_is_reported_in_meta() {
    let m = meta(&[documents()]);
    let json = m.to_json();
    assert!(json.contains("\"type\":\"file\""), "got: {json}");
    assert_eq!(FieldKind::File.as_str(), "file");
}

#[test]
fn create_stores_a_file_descriptor_object() {
    let coll = documents();
    let mut store = Store::open(db_path()).unwrap();

    let created = store
        .create(
            &coll,
            obj(vec![
                ("title", Value::Str("Spec".into())),
                ("attachment", file_descriptor()),
            ]),
        )
        .unwrap();

    let att = created.get("attachment").expect("attachment present");
    assert_eq!(
        att.get("blob").and_then(Value::as_str),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        att.get("filename").and_then(Value::as_str),
        Some("note.txt")
    );
    assert_eq!(att.get("size").and_then(Value::as_i64), Some(13));
}

#[test]
fn file_field_omitted_is_fine() {
    let coll = documents();
    let mut store = Store::open(db_path()).unwrap();

    // No `attachment` key at all — accepted, and the stored record simply omits it.
    let created = store
        .create(&coll, obj(vec![("title", Value::Str("No file".into()))]))
        .unwrap();
    assert!(created.get("attachment").is_none());
}

#[test]
fn file_field_null_is_fine() {
    let coll = documents();
    let mut store = Store::open(db_path()).unwrap();

    let created = store
        .create(
            &coll,
            obj(vec![
                ("title", Value::Str("Null file".into())),
                ("attachment", Value::Null),
            ]),
        )
        .unwrap();
    assert!(created.get("attachment").is_none());
}

#[test]
fn non_object_file_value_is_a_validation_error() {
    let coll = documents();
    let mut store = Store::open(db_path()).unwrap();

    let err = store
        .create(
            &coll,
            obj(vec![
                ("title", Value::Str("Bad".into())),
                ("attachment", Value::Str("just a string".into())),
            ]),
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("attachment"), "got: {msg}");
    assert!(msg.contains("file"), "got: {msg}");
}

#[test]
fn update_replaces_the_file_descriptor() {
    let coll = documents();
    let mut store = Store::open(db_path()).unwrap();

    let created = store
        .create(
            &coll,
            obj(vec![
                ("title", Value::Str("Doc".into())),
                ("attachment", file_descriptor()),
            ]),
        )
        .unwrap();
    let id = created.get("id").and_then(Value::as_i64).unwrap() as u64;

    let replacement = obj(vec![
        (
            "blob",
            Value::Str("ffffffffffffffffffffffffffffffff".into()),
        ),
        ("filename", Value::Str("new.bin".into())),
        (
            "content_type",
            Value::Str("application/octet-stream".into()),
        ),
        ("size", Value::Int(4)),
    ]);
    let updated = store
        .update(&coll, id, obj(vec![("attachment", replacement)]))
        .unwrap()
        .unwrap();
    assert_eq!(
        updated
            .get("attachment")
            .and_then(|a| a.get("filename"))
            .and_then(Value::as_str),
        Some("new.bin")
    );

    // Clearing it with null drops the field.
    let cleared = store
        .update(&coll, id, obj(vec![("attachment", Value::Null)]))
        .unwrap()
        .unwrap();
    assert!(cleared.get("attachment").is_none());
}
