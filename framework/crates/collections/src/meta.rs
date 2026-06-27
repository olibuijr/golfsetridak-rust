//! The `/api/_meta` manifest builder.
//!
//! [`meta`] turns a set of [`Collection`]s into a JSON [`Value`] describing each
//! collection, its fields, and the REST endpoints the framework exposes for it.
//! The CLI merges this into the framework-wide `/api/_meta` document.

use akurai_json::Value;

use crate::schema::Collection;

/// Build the manifest for a set of collections.
///
/// Shape:
///
/// ```json
/// {
///   "collections": [
///     {
///       "name": "posts",
///       "fields": [
///         { "name": "title", "type": "text", "required": true, "embed": false }
///       ],
///       "endpoints": {
///         "list":   "GET /api/collections/posts/records",
///         "create": "POST /api/collections/posts/records",
///         "get":    "GET /api/collections/posts/records/<id>",
///         "update": "PATCH /api/collections/posts/records/<id>",
///         "delete": "DELETE /api/collections/posts/records/<id>",
///         "search": "GET /api/collections/posts/records?search=<query>"
///       }
///     }
///   ]
/// }
/// ```
pub fn meta(colls: &[Collection]) -> Value {
    let items = colls.iter().map(collection_meta).collect();
    Value::Object(vec![("collections".into(), Value::Array(items))])
}

fn collection_meta(coll: &Collection) -> Value {
    let fields = coll
        .fields
        .iter()
        .map(|f| {
            let mut entry = vec![
                ("name".into(), Value::Str(f.name.clone())),
                ("type".into(), Value::Str(f.kind.as_str().into())),
                ("required".into(), Value::Bool(f.required)),
                ("embed".into(), Value::Bool(f.embed)),
            ];
            // A relation field also advertises its target collection.
            if let Some(target) = f.kind.relation_target() {
                entry.push(("collection".into(), Value::Str(target.to_string())));
            }
            Value::Object(entry)
        })
        .collect();

    let base = format!("/api/collections/{}/records", coll.name);
    let endpoints = Value::Object(vec![
        ("list".into(), Value::Str(format!("GET {base}"))),
        ("create".into(), Value::Str(format!("POST {base}"))),
        ("get".into(), Value::Str(format!("GET {base}/<id>"))),
        ("update".into(), Value::Str(format!("PATCH {base}/<id>"))),
        ("delete".into(), Value::Str(format!("DELETE {base}/<id>"))),
        (
            "search".into(),
            Value::Str(format!("GET {base}?search=<query>")),
        ),
    ]);

    Value::Object(vec![
        ("name".into(), Value::Str(coll.name.clone())),
        ("fields".into(), Value::Array(fields)),
        ("endpoints".into(), endpoints),
    ])
}
