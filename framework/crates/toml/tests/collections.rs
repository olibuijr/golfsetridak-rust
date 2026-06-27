//! End-to-end test: parse a realistic `collections.toml` and assert the nested
//! array-of-tables shape the framework's schema layer will consume.

use akurai_toml::{parse, Value};

const COLLECTIONS: &str = r#"
# a collection
[[collection]]
name = "posts"

  [[collection.field]]
  name = "title"
  type = "text"
  required = true

  [[collection.field]]
  name = "body"
  type = "text"
  embed = true

[[collection]]
name = "notes"
  [[collection.field]]
  name = "n"
  type = "int"
"#;

fn field(arr: &[Value], i: usize) -> &Value {
    &arr[i]
}

#[test]
fn parses_full_collections_config() {
    let doc = parse(COLLECTIONS).expect("should parse");

    let collections = doc
        .get("collection")
        .and_then(Value::as_array)
        .expect("collection array of tables");
    assert_eq!(collections.len(), 2, "two collections");

    // ---- collection 0: posts, two fields ----
    let posts = &collections[0];
    assert_eq!(posts.get("name").and_then(Value::as_str), Some("posts"));
    let posts_fields = posts
        .get("field")
        .and_then(Value::as_array)
        .expect("posts.field array");
    assert_eq!(posts_fields.len(), 2);

    let title = field(posts_fields, 0);
    assert_eq!(title.get("name").and_then(Value::as_str), Some("title"));
    assert_eq!(title.get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(title.get("required").and_then(Value::as_bool), Some(true));
    assert!(title.get("embed").is_none());

    let body = field(posts_fields, 1);
    assert_eq!(body.get("name").and_then(Value::as_str), Some("body"));
    assert_eq!(body.get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(body.get("embed").and_then(Value::as_bool), Some(true));
    assert!(body.get("required").is_none());

    // ---- collection 1: notes, one field ----
    let notes = &collections[1];
    assert_eq!(notes.get("name").and_then(Value::as_str), Some("notes"));
    let notes_fields = notes
        .get("field")
        .and_then(Value::as_array)
        .expect("notes.field array");
    assert_eq!(notes_fields.len(), 1);

    let n = field(notes_fields, 0);
    assert_eq!(n.get("name").and_then(Value::as_str), Some("n"));
    assert_eq!(n.get("type").and_then(Value::as_str), Some("int"));
}

#[test]
fn top_level_is_a_table() {
    let doc = parse("name = \"x\"\n").unwrap();
    assert!(matches!(doc, Value::Table(_)));
}
