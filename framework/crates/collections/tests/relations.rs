//! Tests for relation fields: schema, validation, `_meta`, and read-time
//! expansion. Every test gets its own file under `CARGO_TARGET_TMPDIR`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use akurai_collections::{meta, CollError, Collection, Field, FieldKind, Store};
use akurai_json::Value;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn db_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(format!("rel-test-{n}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn authors() -> Collection {
    Collection::new(
        "authors",
        vec![Field::new("name", FieldKind::Text).required()],
    )
}

/// `posts` with an `author` relation pointing at `authors`.
fn posts() -> Collection {
    Collection::new(
        "posts",
        vec![
            Field::new("title", FieldKind::Text).required(),
            Field::relation("author", "authors"),
        ],
    )
}

#[test]
fn relation_field_stores_id_and_expands_on_read() {
    let authors = authors();
    let posts = posts();
    let all = vec![authors.clone(), posts.clone()];
    let mut store = Store::open(db_path()).unwrap();

    let ada = store
        .create(&authors, obj(vec![("name", s("Ada"))]))
        .unwrap();
    let ada_id = ada.get("id").and_then(Value::as_i64).unwrap();
    assert_eq!(ada_id, 1);

    let post = store
        .create(
            &posts,
            obj(vec![("title", s("Hello")), ("author", Value::Int(ada_id))]),
        )
        .unwrap();
    // The relation stores the plain integer id.
    assert_eq!(post.get("author").and_then(Value::as_i64), Some(1));
    // Without expansion, get/list are unchanged (no `_expanded` key).
    let plain = store.get(&posts, 1).unwrap().unwrap();
    assert!(plain.get("author_expanded").is_none());

    // get_expanded inlines the referenced author under `author_expanded`.
    let expanded = store
        .get_expanded(&posts, 1, &["author"], &all)
        .unwrap()
        .unwrap();
    assert_eq!(expanded.get("author").and_then(Value::as_i64), Some(1));
    let inlined = expanded.get("author_expanded").unwrap();
    assert_eq!(inlined.get("name").and_then(Value::as_str), Some("Ada"));
    assert_eq!(inlined.get("id").and_then(Value::as_i64), Some(1));

    // list_expanded does the same for every record.
    let listed = store
        .list_expanded(&posts, None, &["author"], &all)
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0]
            .get("author_expanded")
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str),
        Some("Ada")
    );
}

#[test]
fn dangling_and_absent_references_expand_to_null() {
    let authors = authors();
    let posts = posts();
    let all = vec![authors.clone(), posts.clone()];
    let mut store = Store::open(db_path()).unwrap();

    // Reference an author id that does not exist (unchecked create accepts it).
    store
        .create(
            &posts,
            obj(vec![("title", s("Dangling")), ("author", Value::Int(999))]),
        )
        .unwrap();
    // A post with no author at all.
    store
        .create(&posts, obj(vec![("title", s("No author"))]))
        .unwrap();

    let dangling = store
        .get_expanded(&posts, 1, &["author"], &all)
        .unwrap()
        .unwrap();
    assert_eq!(dangling.get("author_expanded"), Some(&Value::Null));

    let absent = store
        .get_expanded(&posts, 2, &["author"], &all)
        .unwrap()
        .unwrap();
    assert_eq!(absent.get("author_expanded"), Some(&Value::Null));
}

#[test]
fn unknown_expand_names_are_ignored() {
    let authors = authors();
    let posts = posts();
    let all = vec![authors.clone(), posts.clone()];
    let mut store = Store::open(db_path()).unwrap();
    store
        .create(&authors, obj(vec![("name", s("Ada"))]))
        .unwrap();
    store
        .create(
            &posts,
            obj(vec![("title", s("T")), ("author", Value::Int(1))]),
        )
        .unwrap();

    // "title" is not a relation; "missing" is not a field at all. Both ignored.
    let expanded = store
        .get_expanded(&posts, 1, &["title", "missing"], &all)
        .unwrap()
        .unwrap();
    assert!(expanded.get("title_expanded").is_none());
    assert!(expanded.get("missing_expanded").is_none());
    // Asking to expand nothing yields the record unchanged.
    let none = store.get_expanded(&posts, 1, &[], &all).unwrap().unwrap();
    assert!(none.get("author_expanded").is_none());
}

#[test]
fn relation_validation_rejects_non_integer() {
    let posts = posts();
    let mut store = Store::open(db_path()).unwrap();
    let err = store
        .create(
            &posts,
            obj(vec![("title", s("T")), ("author", s("not an int"))]),
        )
        .unwrap_err();
    assert!(matches!(err, CollError::Validation(_)));
}

#[test]
fn checked_create_verifies_referenced_record_exists() {
    let authors = authors();
    let posts = posts();
    let all = vec![authors.clone(), posts.clone()];
    let mut store = Store::open(db_path()).unwrap();

    // No author #1 yet → checked create must reject the dangling reference.
    let err = store
        .create_checked(
            &posts,
            obj(vec![("title", s("T")), ("author", Value::Int(1))]),
            &all,
        )
        .unwrap_err();
    assert!(matches!(err, CollError::Validation(_)));

    // Create the author, then the checked create succeeds.
    store
        .create(&authors, obj(vec![("name", s("Ada"))]))
        .unwrap();
    let ok = store
        .create_checked(
            &posts,
            obj(vec![("title", s("T")), ("author", Value::Int(1))]),
            &all,
        )
        .unwrap();
    assert_eq!(ok.get("author").and_then(Value::as_i64), Some(1));

    // checked update is likewise verified.
    let bad = store
        .update_checked(&posts, 1, obj(vec![("author", Value::Int(42))]), &all)
        .unwrap_err();
    assert!(matches!(bad, CollError::Validation(_)));
}

#[test]
fn checked_create_skips_existence_when_target_unknown() {
    let posts = posts();
    // `all` does NOT include the `authors` target, so existence isn't checked.
    let all = vec![posts.clone()];
    let mut store = Store::open(db_path()).unwrap();
    let ok = store
        .create_checked(
            &posts,
            obj(vec![("title", s("T")), ("author", Value::Int(7))]),
            &all,
        )
        .unwrap();
    assert_eq!(ok.get("author").and_then(Value::as_i64), Some(7));
}

#[test]
fn meta_reports_relation_type_and_target() {
    let manifest = meta(&[posts()]);
    let collections = match manifest.get("collections").unwrap() {
        Value::Array(a) => a,
        _ => panic!("collections should be an array"),
    };
    let fields = match collections[0].get("fields").unwrap() {
        Value::Array(a) => a,
        _ => panic!("fields should be an array"),
    };
    // field[1] is the `author` relation.
    let author = &fields[1];
    assert_eq!(author.get("name").and_then(Value::as_str), Some("author"));
    assert_eq!(author.get("type").and_then(Value::as_str), Some("relation"));
    assert_eq!(
        author.get("collection").and_then(Value::as_str),
        Some("authors")
    );
    // A non-relation field carries no `collection` key.
    assert!(fields[0].get("collection").is_none());
}

#[test]
fn relations_round_trip_across_reopen() {
    let authors = authors();
    let posts = posts();
    let all = vec![authors.clone(), posts.clone()];
    let path = db_path();
    {
        let mut store = Store::open(&path).unwrap();
        store
            .create(&authors, obj(vec![("name", s("Ada"))]))
            .unwrap();
        store
            .create(
                &posts,
                obj(vec![("title", s("Kept")), ("author", Value::Int(1))]),
            )
            .unwrap();
    }
    // Reopen and confirm the relation id and expansion survive.
    let mut store = Store::open(&path).unwrap();
    let expanded = store
        .get_expanded(&posts, 1, &["author"], &all)
        .unwrap()
        .unwrap();
    assert_eq!(expanded.get("author").and_then(Value::as_i64), Some(1));
    assert_eq!(
        expanded
            .get("author_expanded")
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str),
        Some("Ada")
    );
}
