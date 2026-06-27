//! End-to-end tests for the collections engine over a real on-disk B+tree.
//!
//! Every test gets its own file under `CARGO_TARGET_TMPDIR` (never `/tmp`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use akurai_collections::{meta, CollError, Collection, Field, FieldKind, Store};
use akurai_json::{parse, Value};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique db path inside the crate's target tmp dir.
fn db_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(format!("coll-test-{n}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// The canonical test collection: a `posts` resource.
fn posts() -> Collection {
    Collection::new(
        "posts",
        vec![
            Field::new("title", FieldKind::Text).required().embed(),
            Field::new("body", FieldKind::Text),
            Field::new("views", FieldKind::Int),
            Field::new("rating", FieldKind::Float),
            Field::new("draft", FieldKind::Bool),
        ],
    )
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

#[test]
fn create_get_list_update_delete_round_trip() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();

    let created = store
        .create(
            &coll,
            obj(vec![
                ("title", s("Hello")),
                ("body", s("World")),
                ("views", Value::Int(3)),
            ]),
        )
        .unwrap();

    let id = created.get("id").and_then(Value::as_i64).unwrap();
    assert_eq!(id, 1);
    assert!(created.get("created").and_then(Value::as_i64).is_some());
    assert_eq!(created.get("title").and_then(Value::as_str), Some("Hello"));
    // Optional fields not supplied are omitted, not stored as null.
    assert!(created.get("rating").is_none());

    // get
    let fetched = store.get(&coll, 1).unwrap().unwrap();
    assert_eq!(fetched, created);
    assert!(store.get(&coll, 999).unwrap().is_none());

    // list (newest first)
    let second = store
        .create(&coll, obj(vec![("title", s("Second"))]))
        .unwrap();
    let listed = store.list(&coll, None).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0], second);
    assert_eq!(listed[1], created);

    // list with limit
    let limited = store.list(&coll, Some(1)).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0], second);

    // update (partial)
    let updated = store
        .update(&coll, 1, obj(vec![("body", s("Edited"))]))
        .unwrap()
        .unwrap();
    assert_eq!(updated.get("body").and_then(Value::as_str), Some("Edited"));
    assert_eq!(updated.get("title").and_then(Value::as_str), Some("Hello"));
    assert_eq!(updated.get("id").and_then(Value::as_i64), Some(1));
    // update of a missing id is None
    assert!(store
        .update(&coll, 999, obj(vec![("body", s("x"))]))
        .unwrap()
        .is_none());

    // delete
    assert!(store.delete(&coll, 1).unwrap());
    assert!(!store.delete(&coll, 1).unwrap());
    assert!(store.get(&coll, 1).unwrap().is_none());
    assert_eq!(store.list(&coll, None).unwrap().len(), 1);
}

#[test]
fn float_field_accepts_and_coerces_int() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    let rec = store
        .create(
            &coll,
            obj(vec![("title", s("F")), ("rating", Value::Int(4))]),
        )
        .unwrap();
    assert_eq!(rec.get("rating"), Some(&Value::Float(4.0)));
}

#[test]
fn validation_rejects_missing_required() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    let err = store
        .create(&coll, obj(vec![("body", s("no title"))]))
        .unwrap_err();
    assert!(matches!(err, CollError::Validation(_)));
    // Null counts as missing for a required field.
    let err = store
        .create(&coll, obj(vec![("title", Value::Null)]))
        .unwrap_err();
    assert!(matches!(err, CollError::Validation(_)));
}

#[test]
fn validation_rejects_wrong_type() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    let err = store
        .create(
            &coll,
            obj(vec![("title", s("T")), ("views", s("not an int"))]),
        )
        .unwrap_err();
    assert!(matches!(err, CollError::Validation(_)));
}

#[test]
fn validation_rejects_unknown_and_reserved_fields() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    let unknown = store
        .create(&coll, obj(vec![("title", s("T")), ("nope", s("x"))]))
        .unwrap_err();
    assert!(matches!(unknown, CollError::Validation(_)));

    let reserved = store
        .create(&coll, obj(vec![("title", s("T")), ("id", Value::Int(5))]))
        .unwrap_err();
    assert!(matches!(reserved, CollError::Validation(_)));
}

#[test]
fn input_must_be_an_object() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    let err = store.create(&coll, Value::Int(3)).unwrap_err();
    assert!(matches!(err, CollError::Validation(_)));
}

#[test]
fn update_re_validates_changed_fields() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    store.create(&coll, obj(vec![("title", s("T"))])).unwrap();

    // wrong type on patch
    assert!(matches!(
        store
            .update(&coll, 1, obj(vec![("views", s("bad"))]))
            .unwrap_err(),
        CollError::Validation(_)
    ));
    // unknown field on patch
    assert!(matches!(
        store
            .update(&coll, 1, obj(vec![("nope", s("x"))]))
            .unwrap_err(),
        CollError::Validation(_)
    ));
    // required field cannot be nulled
    assert!(matches!(
        store
            .update(&coll, 1, obj(vec![("title", Value::Null)]))
            .unwrap_err(),
        CollError::Validation(_)
    ));
    // optional field CAN be cleared via null
    let cleared = store
        .update(&coll, 1, obj(vec![("body", s("temp"))]))
        .unwrap()
        .unwrap();
    assert!(cleared.get("body").is_some());
    let cleared = store
        .update(&coll, 1, obj(vec![("body", Value::Null)]))
        .unwrap()
        .unwrap();
    assert!(cleared.get("body").is_none());
}

#[test]
fn ids_auto_increment_and_do_not_collide() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    let mut ids = Vec::new();
    for i in 0..50 {
        let rec = store
            .create(&coll, obj(vec![("title", s(&format!("p{i}")))]))
            .unwrap();
        ids.push(rec.get("id").and_then(Value::as_i64).unwrap());
    }
    assert_eq!(ids, (1..=50).collect::<Vec<_>>());
    // deletion does not let ids be reused
    store.delete(&coll, 50).unwrap();
    let rec = store
        .create(&coll, obj(vec![("title", s("after"))]))
        .unwrap();
    assert_eq!(rec.get("id").and_then(Value::as_i64), Some(51));
}

#[test]
fn records_and_counter_persist_across_reopen() {
    let coll = posts();
    let path = db_path();
    {
        let mut store = Store::open(&path).unwrap();
        store
            .create(&coll, obj(vec![("title", s("kept"))]))
            .unwrap();
        store
            .create(&coll, obj(vec![("title", s("kept2"))]))
            .unwrap();
    }
    // reopen
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.list(&coll, None).unwrap().len(), 2);
    assert_eq!(
        store
            .get(&coll, 1)
            .unwrap()
            .unwrap()
            .get("title")
            .and_then(Value::as_str),
        Some("kept")
    );
    // counter survived: next id is 3, not 1
    let rec = store.create(&coll, obj(vec![("title", s("new"))])).unwrap();
    assert_eq!(rec.get("id").and_then(Value::as_i64), Some(3));
}

#[test]
fn collections_are_isolated_by_prefix() {
    let posts = posts();
    let tags = Collection::new("tags", vec![Field::new("name", FieldKind::Text).required()]);
    let mut store = Store::open(db_path()).unwrap();

    store.create(&posts, obj(vec![("title", s("P"))])).unwrap();
    let t = store.create(&tags, obj(vec![("name", s("rust"))])).unwrap();
    // both got id 1 — counters are per-collection
    assert_eq!(t.get("id").and_then(Value::as_i64), Some(1));
    assert_eq!(store.list(&posts, None).unwrap().len(), 1);
    assert_eq!(store.list(&tags, None).unwrap().len(), 1);
    // a post id is not visible from the tags collection
    assert!(store.get(&tags, 1).unwrap().is_some());
    assert_eq!(
        store
            .get(&tags, 1)
            .unwrap()
            .unwrap()
            .get("name")
            .and_then(Value::as_str),
        Some("rust")
    );
}

#[test]
fn search_is_case_insensitive_substring_over_text_fields_and_respects_limit() {
    let coll = posts();
    let mut store = Store::open(db_path()).unwrap();
    store
        .create(
            &coll,
            obj(vec![("title", s("Rust is Great")), ("body", s("systems"))]),
        )
        .unwrap();
    store
        .create(
            &coll,
            obj(vec![
                ("title", s("Python")),
                ("body", s("scripting in RUST too")),
            ]),
        )
        .unwrap();
    store
        .create(
            &coll,
            obj(vec![("title", s("Go")), ("views", Value::Int(9))]),
        )
        .unwrap();

    // case-insensitive, matches across multiple text fields
    let hits = store.search(&coll, "rust", None).unwrap();
    assert_eq!(hits.len(), 2);
    // newest first
    assert_eq!(hits[0].get("title").and_then(Value::as_str), Some("Python"));

    // limit respected
    let limited = store.search(&coll, "rust", Some(1)).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(
        limited[0].get("title").and_then(Value::as_str),
        Some("Python")
    );

    // no match
    assert!(store.search(&coll, "haskell", None).unwrap().is_empty());

    // non-text fields are not searched
    assert!(store.search(&coll, "9", None).unwrap().is_empty());

    // empty query matches everything
    assert_eq!(store.search(&coll, "", None).unwrap().len(), 3);
}

#[test]
fn meta_emits_expected_structure() {
    let colls = vec![
        posts(),
        Collection::new("tags", vec![Field::new("name", FieldKind::Text).required()]),
    ];
    let manifest = meta(&colls);

    // Round-trip through the serializer to assert exact wire shape.
    let json = manifest.to_json();
    let reparsed = parse(&json).unwrap();
    assert_eq!(reparsed, manifest);

    let collections = manifest.get("collections").unwrap();
    let arr = match collections {
        Value::Array(a) => a,
        _ => panic!("collections should be an array"),
    };
    assert_eq!(arr.len(), 2);

    let first = &arr[0];
    assert_eq!(first.get("name").and_then(Value::as_str), Some("posts"));

    let fields = match first.get("fields").unwrap() {
        Value::Array(a) => a,
        _ => panic!("fields should be an array"),
    };
    assert_eq!(fields.len(), 5);
    let title = &fields[0];
    assert_eq!(title.get("name").and_then(Value::as_str), Some("title"));
    assert_eq!(title.get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(title.get("required").and_then(Value::as_bool), Some(true));
    assert_eq!(title.get("embed").and_then(Value::as_bool), Some(true));

    let endpoints = first.get("endpoints").unwrap();
    assert_eq!(
        endpoints.get("list").and_then(Value::as_str),
        Some("GET /api/collections/posts/records")
    );
    assert_eq!(
        endpoints.get("create").and_then(Value::as_str),
        Some("POST /api/collections/posts/records")
    );
    assert_eq!(
        endpoints.get("get").and_then(Value::as_str),
        Some("GET /api/collections/posts/records/<id>")
    );
    assert_eq!(
        endpoints.get("update").and_then(Value::as_str),
        Some("PATCH /api/collections/posts/records/<id>")
    );
    assert_eq!(
        endpoints.get("delete").and_then(Value::as_str),
        Some("DELETE /api/collections/posts/records/<id>")
    );
    assert_eq!(
        endpoints.get("search").and_then(Value::as_str),
        Some("GET /api/collections/posts/records?search=<query>")
    );
}
