//! The auto-generated REST API: `backend/collections.toml` → live CRUD endpoints.
//!
//! Declare collections in `backend/collections.toml` and this module mounts a
//! full REST surface for each one under `/api/collections/<name>/records`, with
//! validation, substring `?search`, and — when an embedding endpoint is
//! configured — semantic `?search`. No handwritten endpoints; the schema is the
//! single source of truth.
//!
//! The whole thing is additive: if `backend/collections.toml` is absent, the
//! server behaves exactly as it did before (zero collections, no new routes).
//!
//! ## Storage layout
//!
//! - `data/collections.db` — one B+tree shared by every collection (the
//!   [`akurai_collections::Store`]).
//! - `data/embeddings.db` — a separate B+tree of `<name>:<id_be8>` →
//!   `encode(vector)` rows, written best-effort on create/update and removed on
//!   delete. Embedding never blocks or fails a write.
//!
//! ## Embedding config (both optional)
//!
//! - `AKURAI_EMBED_URL` — an OpenAI-compatible `/v1/embeddings` endpoint. When
//!   unset, `?search` falls back to substring matching. `https://` is rejected
//!   by [`akurai_vector::embed`] (no TLS); use a plain-HTTP endpoint.
//! - `AKURAI_EMBED_MODEL` — the embedding model name (defaults to
//!   `embeddinggemma`).

use std::path::Path;
use std::sync::{Arc, Mutex};

use akurai_blobs::BlobStore;
use akurai_collections::{meta, CollError, Collection, Field, FieldKind, Store};
use akurai_http::{parse_multipart, Method, Response};
use akurai_json::Value;
use akurai_storage::BTree;

/// The default embedding model when `AKURAI_EMBED_MODEL` is unset.
const DEFAULT_EMBED_MODEL: &str = "embeddinggemma";

/// Top-k cap for semantic search when no `?limit` is supplied.
const DEFAULT_SEARCH_K: usize = 20;

/// Fallback `Content-Type` for an uploaded file with no declared part type.
const DEFAULT_FILE_CONTENT_TYPE: &str = "application/octet-stream";

/// The live auto-API: the parsed schema plus its shared storage handles.
pub struct CollectionsApi {
    collections: Vec<Collection>,
    store: Arc<Mutex<Store>>,
    embeddings: Arc<Mutex<BTree>>,
    /// Content-addressed store for `File`-field uploads (`data/blobs.db`).
    blobs: Arc<Mutex<BlobStore>>,
    embed_url: Option<String>,
    embed_model: String,
}

impl CollectionsApi {
    /// Build the API for a project rooted at `project_root` (the directory that
    /// holds `backend/`, `frontend/`, and `data/`).
    ///
    /// Reads `backend/collections.toml` (absent → zero collections), opens the
    /// two B+trees under `data/`, and reads the embed config from the
    /// environment. A malformed `collections.toml` is surfaced as `Err` so the
    /// caller can fail `serve` loudly rather than silently dropping the schema.
    pub fn open(project_root: &Path) -> Result<CollectionsApi, String> {
        let collections = load_collections(project_root)?;
        let data_dir = project_root.join("data");
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        let store = Store::open(data_dir.join("collections.db"))
            .map_err(|e| format!("cannot open collections store: {e}"))?;
        let embeddings = BTree::open(data_dir.join("embeddings.db"))
            .map_err(|e| format!("cannot open embeddings store: {e}"))?;
        let blobs = BlobStore::open(data_dir.join("blobs.db"))
            .map_err(|e| format!("cannot open blob store: {e}"))?;
        Ok(CollectionsApi::from_parts(
            collections,
            store,
            embeddings,
            blobs,
            std::env::var("AKURAI_EMBED_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            std::env::var("AKURAI_EMBED_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string()),
        ))
    }

    /// Assemble an API from already-opened parts. Used by [`open`](Self::open)
    /// and by tests that want a temp store.
    pub fn from_parts(
        collections: Vec<Collection>,
        store: Store,
        embeddings: BTree,
        blobs: BlobStore,
        embed_url: Option<String>,
        embed_model: String,
    ) -> CollectionsApi {
        CollectionsApi {
            collections,
            store: Arc::new(Mutex::new(store)),
            embeddings: Arc::new(Mutex::new(embeddings)),
            blobs: Arc::new(Mutex::new(blobs)),
            embed_url,
            embed_model,
        }
    }

    /// The parsed collections, for merging into the `/api/_meta` manifest.
    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    // ---- in-process reads (Phase 4A) --------------------------------------
    //
    // Thin read helpers so page handlers can query the collections store
    // directly instead of round-tripping through the HTTP dispatch. They mirror
    // the `list` arm of [`dispatch`] (a plain `Store::list`, newest-first) but
    // skip JSON (de)serialization and status codes — these never error to the
    // caller; an unknown collection simply yields nothing.

    /// Every record of `collection`, newest-first (highest id first), exactly as
    /// the list endpoint returns them. An unknown collection yields an empty vec.
    pub fn records(&self, collection: &str) -> Vec<Value> {
        let Some(coll) = self.collection(collection) else {
            return Vec::new();
        };
        let mut store = self.store.lock().expect("store mutex poisoned");
        store.list(coll, None).unwrap_or_default()
    }

    /// The first record of `collection` whose string field `field` equals
    /// `value`, or `None`. Used to key business-user records by email/phone,
    /// neither of which is the auto-increment id.
    pub fn find_by(&self, collection: &str, field: &str, value: &str) -> Option<Value> {
        self.records(collection)
            .into_iter()
            .find(|r| r.get(field).and_then(Value::as_str) == Some(value))
    }

    /// Route a `/api/collections...` request. `segments` are the path segments
    /// *after* `/api/collections` (so `/api/collections/posts/records/3` arrives
    /// as `["posts", "records", "3"]`). Returns `None` when the path shape is
    /// not a collections route, so the caller falls through to its own handling.
    /// `content_type` is the request `Content-Type` header (used to detect
    /// `multipart/form-data` uploads); `body` is the raw request body bytes
    /// (binary-safe — multipart payloads are not UTF-8). A JSON request passes
    /// its body bytes here unchanged.
    pub fn dispatch(
        &self,
        method: &Method,
        segments: &[&str],
        query: Option<&str>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        match segments {
            // GET /api/collections  → the manifest.
            [] => match method {
                Method::Get => Some((200, meta(&self.collections))),
                _ => None,
            },
            // /api/collections/<name>/records
            [name, "records"] => self.records_collection(method, name, query, content_type, body),
            // /api/collections/<name>/records/<id>
            [name, "records", id] => self.records_item(method, name, id, query, content_type, body),
            // /api/collections/<name>/records/<id>/<field> (file download) is
            // served as raw bytes by [`download`](Self::download), not here.
            _ => None,
        }
    }

    /// `/api/collections/<name>/records` — list/search (GET) or create (POST).
    fn records_collection(
        &self,
        method: &Method,
        name: &str,
        query: Option<&str>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        let Some(coll) = self.collection(name) else {
            return Some(not_found(&format!("unknown collection: {name}")));
        };
        match method {
            Method::Get => {
                let params = QueryParams::parse(query);
                Some(self.list_or_search(coll, &params))
            }
            Method::Post => Some(self.create(coll, content_type, body)),
            _ => None,
        }
    }

    /// `/api/collections/<name>/records/<id>` — get/update/delete one record.
    fn records_item(
        &self,
        method: &Method,
        name: &str,
        id: &str,
        query: Option<&str>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        let Some(coll) = self.collection(name) else {
            return Some(not_found(&format!("unknown collection: {name}")));
        };
        let Ok(id) = id.parse::<u64>() else {
            return Some(bad_request("invalid record id"));
        };
        match method {
            Method::Get => {
                let params = QueryParams::parse(query);
                Some(self.get(coll, id, params.expand.as_deref()))
            }
            Method::Patch => Some(self.update(coll, id, content_type, body)),
            Method::Delete => Some(self.delete(coll, id)),
            _ => None,
        }
    }

    // ---- handlers ---------------------------------------------------------

    fn list_or_search(&self, coll: &Collection, params: &QueryParams) -> (u16, Value) {
        let mut store = self.store.lock().expect("store mutex poisoned");
        let result = match &params.search {
            Some(q) if !q.is_empty() => self.search(&mut store, coll, q, params.limit),
            // `?expand=a,b` inlines relation references; absent → plain list.
            _ => match &params.expand {
                Some(expand) => {
                    let fields = expand_fields(expand);
                    store.list_expanded(coll, params.limit, &fields, &self.collections)
                }
                None => store.list(coll, params.limit),
            },
        };
        match result {
            Ok(records) => (200, Value::Array(records)),
            Err(e) => coll_error(&e),
        }
    }

    fn create(&self, coll: &Collection, content_type: Option<&str>, body: &[u8]) -> (u16, Value) {
        let input = match self.build_input(coll, content_type, body) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let record = {
            let mut store = self.store.lock().expect("store mutex poisoned");
            // `create_checked` existence-checks relation ids against `self.collections`.
            match store.create_checked(coll, input, &self.collections) {
                Ok(r) => r,
                Err(e) => return coll_error(&e),
            }
        };
        self.index_record(coll, &record);
        (201, record)
    }

    fn get(&self, coll: &Collection, id: u64, expand: Option<&str>) -> (u16, Value) {
        let mut store = self.store.lock().expect("store mutex poisoned");
        let result = match expand {
            Some(expand) => {
                let fields = expand_fields(expand);
                store.get_expanded(coll, id, &fields, &self.collections)
            }
            None => store.get(coll, id),
        };
        match result {
            Ok(Some(record)) => (200, record),
            Ok(None) => not_found("record not found"),
            Err(e) => coll_error(&e),
        }
    }

    fn update(
        &self,
        coll: &Collection,
        id: u64,
        content_type: Option<&str>,
        body: &[u8],
    ) -> (u16, Value) {
        let patch = match self.build_input(coll, content_type, body) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let record = {
            let mut store = self.store.lock().expect("store mutex poisoned");
            // `update_checked` existence-checks any relation ids in the patch.
            match store.update_checked(coll, id, patch, &self.collections) {
                Ok(Some(r)) => r,
                Ok(None) => return not_found("record not found"),
                Err(e) => return coll_error(&e),
            }
        };
        self.index_record(coll, &record);
        (200, record)
    }

    // ---- input building: JSON or multipart upload -------------------------

    /// Turn a request body into the create/update input object. A
    /// `multipart/form-data` request is parsed into parts: a part whose `name`
    /// matches a `File` field is stored in the blob store and becomes a
    /// `{blob,filename,content_type,size}` descriptor; every other matching part
    /// is its (text) value, coerced to the field's declared type. A non-multipart
    /// request is parsed as a JSON body, exactly as before.
    fn build_input(
        &self,
        coll: &Collection,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Value, (u16, Value)> {
        match content_type {
            Some(ct) if is_multipart(ct) => self.multipart_input(coll, ct, body),
            _ => parse_body(body),
        }
    }

    /// Build the input object from a `multipart/form-data` body, storing file
    /// parts in the blob store. Unknown part names (no matching field) are
    /// ignored, mirroring how a stray JSON key would be rejected later — except
    /// here we simply drop the bytes rather than fail the whole upload.
    fn multipart_input(
        &self,
        coll: &Collection,
        content_type: &str,
        body: &[u8],
    ) -> Result<Value, (u16, Value)> {
        let parts = parse_multipart(content_type, body)
            .map_err(|e| bad_request(&format!("invalid multipart body: {e}")))?;

        let mut obj: Vec<(String, Value)> = Vec::new();
        for part in parts {
            let Some(field) = coll.field(&part.name) else {
                continue; // not a declared field — drop it
            };
            let value = match &field.kind {
                FieldKind::File => {
                    let size = part.data.len() as i64;
                    let blob_id = {
                        let mut blobs = self.blobs.lock().expect("blob mutex poisoned");
                        blobs
                            .put(&part.data)
                            .map_err(|e| internal_error(&format!("blob write failed: {e}")))?
                    };
                    let content_type = part
                        .content_type
                        .unwrap_or_else(|| DEFAULT_FILE_CONTENT_TYPE.to_string());
                    Value::Object(vec![
                        ("blob".into(), Value::Str(blob_id)),
                        (
                            "filename".into(),
                            Value::Str(part.filename.unwrap_or_default()),
                        ),
                        ("content_type".into(), Value::Str(content_type)),
                        ("size".into(), Value::Int(size)),
                    ])
                }
                _ => {
                    let text = String::from_utf8_lossy(&part.data);
                    match text_to_value(field, text.trim()) {
                        Ok(v) => v,
                        Err(msg) => return Err(bad_request(&msg)),
                    }
                }
            };
            set_pair(&mut obj, &field.name, value);
        }
        Ok(Value::Object(obj))
    }

    /// Serve a `File` field's bytes: `GET /<name>/records/<id>/<field>`. Returns
    /// the blob with its stored `content_type` and a `Content-Disposition` naming
    /// the stored filename. A 404 (as a [`Response`]) is returned when the
    /// collection, record, field, file value, or blob is missing.
    pub fn download(&self, name: &str, id: &str, field: &str) -> Response {
        let Some(coll) = self.collection(name) else {
            return error_response(404, &format!("unknown collection: {name}"));
        };
        // The field must exist and be a File field.
        if !matches!(coll.field(field).map(|f| &f.kind), Some(FieldKind::File)) {
            return error_response(404, &format!("no file field '{field}'"));
        }
        let Ok(id) = id.parse::<u64>() else {
            return error_response(400, "invalid record id");
        };

        let record = {
            let mut store = self.store.lock().expect("store mutex poisoned");
            match store.get(coll, id) {
                Ok(Some(r)) => r,
                Ok(None) => return error_response(404, "record not found"),
                Err(e) => return error_response(500, &e.to_string()),
            }
        };
        let Some(file) = record.get(field) else {
            return error_response(404, "file not set");
        };
        let Some(blob_id) = file.get("blob").and_then(Value::as_str) else {
            return error_response(404, "file not set");
        };
        let bytes = {
            let mut blobs = self.blobs.lock().expect("blob mutex poisoned");
            match blobs.get(blob_id) {
                Ok(Some(b)) => b,
                Ok(None) => return error_response(404, "blob not found"),
                Err(e) => return error_response(500, &e.to_string()),
            }
        };
        let content_type = file
            .get("content_type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_FILE_CONTENT_TYPE)
            .to_string();
        let filename = file
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Response::ok()
            .with_body(&content_type, bytes)
            .with_header("Content-Disposition", &content_disposition(&filename))
    }

    fn delete(&self, coll: &Collection, id: u64) -> (u16, Value) {
        let removed = {
            let mut store = self.store.lock().expect("store mutex poisoned");
            match store.delete(coll, id) {
                Ok(removed) => removed,
                Err(e) => return coll_error(&e),
            }
        };
        if removed {
            self.deindex_record(coll, id);
            (204, Value::Null)
        } else {
            not_found("record not found")
        }
    }

    // ---- semantic search + embedding lifecycle ----------------------------

    /// Semantic search when an endpoint is configured, else substring. Always
    /// returns a plain JSON array of records in result order.
    fn search(
        &self,
        store: &mut Store,
        coll: &Collection,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, CollError> {
        let Some(url) = &self.embed_url else {
            return store.search(coll, query, limit);
        };
        // Embed the query; any failure falls back to substring search.
        let qvec = match akurai_vector::embed(url, &self.embed_model, query) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("collections: query embed failed ({e}); using substring search");
                return store.search(coll, query, limit);
            }
        };
        let k = limit.unwrap_or(DEFAULT_SEARCH_K);
        let candidates = self.load_embeddings(&coll.name);
        let ranked = akurai_vector::rank(&qvec, &candidates, k);
        let mut out = Vec::with_capacity(ranked.len());
        for (id, _score) in ranked {
            if let Some(record) = store.get(coll, id)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Load every stored `(id, vector)` for a collection by prefix-scanning the
    /// embeddings tree.
    fn load_embeddings(&self, name: &str) -> Vec<(u64, Vec<f32>)> {
        let (start, end) = embed_prefix_bounds(name);
        let rows = {
            let mut tree = self.embeddings.lock().expect("embeddings mutex poisoned");
            match tree.range(&start, &end) {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("collections: embeddings scan failed ({e})");
                    return Vec::new();
                }
            }
        };
        let mut out = Vec::with_capacity(rows.len());
        for (key, bytes) in rows {
            if let (Some(id), Some(vec)) = (id_from_key(name, &key), akurai_vector::decode(&bytes))
            {
                out.push((id, vec));
            }
        }
        out
    }

    /// Best-effort: embed a record's `embed` Text fields and store the vector.
    /// A missing endpoint, no embed fields, or any embed error is a no-op — the
    /// record is already saved regardless.
    fn index_record(&self, coll: &Collection, record: &Value) {
        let Some(url) = &self.embed_url else {
            return;
        };
        let text = embed_text(coll, record);
        if text.trim().is_empty() {
            return;
        }
        let Some(id) = record.get("id").and_then(Value::as_i64) else {
            return;
        };
        let vec = match akurai_vector::embed(url, &self.embed_model, &text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "collections: embed failed for {}:{id} ({e}); record saved",
                    coll.name
                );
                return;
            }
        };
        let key = embed_key(&coll.name, id as u64);
        let bytes = akurai_vector::encode(&vec);
        let mut tree = self.embeddings.lock().expect("embeddings mutex poisoned");
        if let Err(e) = tree.insert(&key, &bytes).and_then(|_| tree.commit()) {
            eprintln!(
                "collections: embeddings write failed for {}:{id} ({e})",
                coll.name
            );
        }
    }

    /// Best-effort: drop a record's stored embedding (called after delete).
    fn deindex_record(&self, coll: &Collection, id: u64) {
        if self.embed_url.is_none() {
            return;
        }
        let key = embed_key(&coll.name, id);
        let mut tree = self.embeddings.lock().expect("embeddings mutex poisoned");
        if let Err(e) = tree.delete(&key).and_then(|_| tree.commit()) {
            eprintln!(
                "collections: embeddings delete failed for {}:{id} ({e})",
                coll.name
            );
        }
    }

    fn collection(&self, name: &str) -> Option<&Collection> {
        self.collections.iter().find(|c| c.name == name)
    }
}

/// Parsed `?search` / `?limit` / `?expand` query parameters.
struct QueryParams {
    search: Option<String>,
    limit: Option<usize>,
    /// The raw `?expand=` value (comma-separated relation field names), if any.
    expand: Option<String>,
}

impl QueryParams {
    fn parse(query: Option<&str>) -> QueryParams {
        let mut search = None;
        let mut limit = None;
        let mut expand = None;
        if let Some(q) = query {
            for pair in q.split('&') {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                match k {
                    "search" => search = Some(percent_decode(v)),
                    "limit" => limit = percent_decode(v).parse::<usize>().ok(),
                    "expand" => expand = Some(percent_decode(v)),
                    _ => {}
                }
            }
        }
        QueryParams {
            search,
            limit,
            expand,
        }
    }
}

/// Split an `?expand=` value into a list of field names, dropping empties.
fn expand_fields(expand: &str) -> Vec<&str> {
    expand
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a `Content-Type` header names a multipart form upload.
fn is_multipart(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .map(str::trim)
        .map(|m| m.eq_ignore_ascii_case("multipart/form-data"))
        .unwrap_or(false)
}

/// Coerce a multipart text value to a field's declared type. `File` is handled
/// by the caller (file parts never reach here).
fn text_to_value(field: &Field, text: &str) -> Result<Value, String> {
    match &field.kind {
        FieldKind::Text => Ok(Value::Str(text.to_string())),
        FieldKind::Int | FieldKind::Relation(_) => text
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("field '{}' expects an integer, got '{text}'", field.name)),
        FieldKind::Float => text
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("field '{}' expects a number, got '{text}'", field.name)),
        FieldKind::Bool => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!(
                "field '{}' expects true/false, got '{text}'",
                field.name
            )),
        },
        FieldKind::File => Ok(Value::Str(text.to_string())),
    }
}

/// Insert or replace `key` in an object's pair list.
fn set_pair(pairs: &mut Vec<(String, Value)>, key: &str, value: Value) {
    if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        pairs.push((key.to_string(), value));
    }
}

/// A `Content-Disposition` header for a download. Uses `inline` with the stored
/// filename when present (quotes escaped); `inline` alone otherwise.
fn content_disposition(filename: &str) -> String {
    if filename.is_empty() {
        return "inline".to_string();
    }
    let escaped = filename.replace('\\', "\\\\").replace('"', "\\\"");
    format!("inline; filename=\"{escaped}\"")
}

/// Minimal `application/x-www-form-urlencoded` decode for query values: `+` →
/// space and `%XX` → byte. Invalid escapes are passed through literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |c: u8| match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                };
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h << 4 | l);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- collections.toml → Vec<Collection> ----------------------------------

/// Read and parse `<project_root>/backend/collections.toml` into collections.
/// An absent file yields `Ok(vec![])`; a malformed file or an unknown field
/// type yields `Err` with the file path and (where available) the line number.
pub fn load_collections(project_root: &Path) -> Result<Vec<Collection>, String> {
    let path = project_root.join("backend").join("collections.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    parse_collections(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Map a `collections.toml` document string to a `Vec<Collection>`. Returns a
/// human-readable error (TOML syntax with line, or an unknown field type).
pub fn parse_collections(toml: &str) -> Result<Vec<Collection>, String> {
    let doc = akurai_toml::parse(toml).map_err(|e| format!("line {}: {}", e.line, e.message))?;

    let mut collections = Vec::new();
    let Some(items) = doc.get("collection").and_then(|v| v.as_array()) else {
        // No `[[collection]]` blocks at all → an empty (but valid) schema.
        return Ok(collections);
    };
    for (i, item) in items.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("collection #{} is missing a string `name`", i + 1))?;
        let mut fields = Vec::new();
        if let Some(field_items) = item.get("field").and_then(|v| v.as_array()) {
            for field in field_items {
                fields.push(parse_field(name, field)?);
            }
        }
        collections.push(Collection::new(name, fields));
    }
    Ok(collections)
}

/// Map one `[[collection.field]]` table to a [`Field`].
fn parse_field(coll: &str, field: &akurai_toml::Value) -> Result<Field, String> {
    let name = field
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("collection `{coll}`: a field is missing a string `name`"))?;
    let type_str = field
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("collection `{coll}`, field `{name}`: missing `type`"))?;
    let kind = field_kind(coll, name, type_str, field)?;
    let mut f = Field::new(name, kind);
    if field.get("required").and_then(|v| v.as_bool()) == Some(true) {
        f = f.required();
    }
    if field.get("embed").and_then(|v| v.as_bool()) == Some(true) {
        f = f.embed();
    }
    Ok(f)
}

/// Map a TOML `type` string (case-insensitive) to a [`FieldKind`]. A `relation`
/// type also requires a sibling `collection = "<target>"` key naming the target
/// collection; its absence is a clear error.
fn field_kind(
    coll: &str,
    name: &str,
    type_str: &str,
    field: &akurai_toml::Value,
) -> Result<FieldKind, String> {
    match type_str.to_ascii_lowercase().as_str() {
        "text" => Ok(FieldKind::Text),
        "int" => Ok(FieldKind::Int),
        "float" => Ok(FieldKind::Float),
        "bool" => Ok(FieldKind::Bool),
        "file" => Ok(FieldKind::File),
        "relation" => {
            let target = field
                .get("collection")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "collection `{coll}`, field `{name}`: a `relation` field \
                         requires a `collection = \"<target>\"` key"
                    )
                })?;
            Ok(FieldKind::Relation(target.to_string()))
        }
        other => Err(format!(
            "collection `{coll}`, field `{name}`: unknown type `{other}` \
             (expected text|int|float|bool|relation|file)"
        )),
    }
}

// ---- embedding helpers ----------------------------------------------------

/// Concatenate a record's `embed` Text fields (in schema order) into one blob.
fn embed_text(coll: &Collection, record: &Value) -> String {
    let mut parts = Vec::new();
    for field in &coll.fields {
        if field.embed && field.kind == FieldKind::Text {
            if let Some(s) = record.get(&field.name).and_then(Value::as_str) {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n")
}

/// The storage key for a record's embedding: `<name>:<id_be8>`.
fn embed_key(name: &str, id: u64) -> Vec<u8> {
    let mut key = format!("{name}:").into_bytes();
    key.extend_from_slice(&id.to_be_bytes());
    key
}

/// Half-open `[start, end)` bounds covering every `<name>:` key. `:` is 0x3a; the
/// upper bound bumps it to 0x3b so the scan stops right after this prefix.
fn embed_prefix_bounds(name: &str) -> (Vec<u8>, Vec<u8>) {
    let start = format!("{name}:").into_bytes();
    let mut end = start.clone();
    *end.last_mut().expect("prefix has a trailing ':'") = b';';
    (start, end)
}

/// Recover the record id from an embeddings key, validating the `<name>:` prefix.
fn id_from_key(name: &str, key: &[u8]) -> Option<u64> {
    let prefix = format!("{name}:");
    let rest = key.strip_prefix(prefix.as_bytes())?;
    let bytes: [u8; 8] = rest.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

// ---- response helpers -----------------------------------------------------

fn parse_body(body: &[u8]) -> Result<Value, (u16, Value)> {
    let raw = String::from_utf8_lossy(body);
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Value::Object(vec![]));
    }
    akurai_json::parse(raw).map_err(|e| bad_request(&format!("invalid JSON body: {e}")))
}

fn coll_error(e: &CollError) -> (u16, Value) {
    match e {
        CollError::Validation(_) | CollError::Json(_) => bad_request(&e.to_string()),
        CollError::NotFound => not_found("record not found"),
        CollError::UnknownCollection(c) => not_found(&format!("unknown collection: {c}")),
        // Storage/corruption are server faults, not client errors.
        CollError::Io(_) | CollError::Corrupt(_) => (
            500,
            Value::Object(vec![("error".into(), Value::Str(e.to_string()))]),
        ),
    }
}

fn bad_request(msg: &str) -> (u16, Value) {
    (
        400,
        Value::Object(vec![("error".into(), Value::Str(msg.into()))]),
    )
}

fn not_found(msg: &str) -> (u16, Value) {
    (
        404,
        Value::Object(vec![("error".into(), Value::Str(msg.into()))]),
    )
}

fn internal_error(msg: &str) -> (u16, Value) {
    (
        500,
        Value::Object(vec![("error".into(), Value::Str(msg.into()))]),
    )
}

/// A JSON error [`Response`] — the raw-bytes equivalent of [`bad_request`] /
/// [`not_found`] for the file-download path, which returns a `Response` rather
/// than a `(u16, Value)` tuple.
fn error_response(status: u16, msg: &str) -> Response {
    let body = Value::Object(vec![("error".into(), Value::Str(msg.into()))]);
    Response::new(status).with_body(
        "application/json; charset=utf-8",
        body.to_json().into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The scratch base: `CARGO_TARGET_TMPDIR` when present (integration tests),
    /// otherwise a dir under this crate's own `target/` — never `/tmp`. Mirrors
    /// the helper in `auth.rs`.
    fn scratch_base() -> PathBuf {
        match std::env::var_os("CARGO_TARGET_TMPDIR") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("collapi-test-tmp"),
        }
    }

    // ---- load_collections / parse_collections mapping --------------------

    const SAMPLE: &str = "\
[[collection]]
name = \"posts\"

  [[collection.field]]
  name = \"title\"
  type = \"text\"
  required = true

  [[collection.field]]
  name = \"body\"
  type = \"TEXT\"
  embed = true

  [[collection.field]]
  name = \"views\"
  type = \"int\"
";

    #[test]
    fn parses_collections_and_fields() {
        let colls = parse_collections(SAMPLE).expect("valid schema");
        assert_eq!(colls.len(), 1);
        let posts = &colls[0];
        assert_eq!(posts.name, "posts");
        assert_eq!(posts.fields.len(), 3);

        let title = posts.field("title").unwrap();
        assert_eq!(title.kind, FieldKind::Text);
        assert!(title.required);
        assert!(!title.embed);

        let body = posts.field("body").unwrap();
        assert_eq!(body.kind, FieldKind::Text); // case-insensitive `TEXT`
        assert!(body.embed);
        assert!(!body.required);

        let views = posts.field("views").unwrap();
        assert_eq!(views.kind, FieldKind::Int);
    }

    #[test]
    fn empty_doc_is_zero_collections() {
        assert!(parse_collections("").unwrap().is_empty());
    }

    #[test]
    fn unknown_field_type_is_an_error() {
        let toml = "\
[[collection]]
name = \"posts\"
  [[collection.field]]
  name = \"x\"
  type = \"date\"
";
        let err = parse_collections(toml).unwrap_err();
        assert!(err.contains("unknown type"), "got: {err}");
        assert!(err.contains("date"), "got: {err}");
    }

    #[test]
    fn missing_file_yields_empty() {
        let root = scratch_base().join("no-backend-here");
        assert!(load_collections(&root).unwrap().is_empty());
    }

    // ---- dispatch CRUD over a temp store ---------------------------------

    fn temp_api(label: &str) -> CollectionsApi {
        let posts = Collection::new(
            "posts",
            vec![
                Field::new("title", FieldKind::Text).required(),
                Field::new("body", FieldKind::Text).embed(),
            ],
        );
        api_with(label, vec![posts])
    }

    /// Build a temp API with the given schema, fresh stores under a per-label dir.
    fn api_with(label: &str, collections: Vec<Collection>) -> CollectionsApi {
        let dir = scratch_base().join(format!("collapi-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        // Fresh files per test run.
        let _ = std::fs::remove_file(dir.join("c.db"));
        let _ = std::fs::remove_file(dir.join("e.db"));
        let _ = std::fs::remove_file(dir.join("b.db"));
        let store = Store::open(dir.join("c.db")).unwrap();
        let embeddings = BTree::open(dir.join("e.db")).unwrap();
        let blobs = BlobStore::open(dir.join("b.db")).unwrap();
        // No embed URL → substring search, no network.
        CollectionsApi::from_parts(collections, store, embeddings, blobs, None, "x".into())
    }

    /// JSON dispatch helper: no multipart content type, body bytes from `json`.
    fn json_dispatch(
        api: &CollectionsApi,
        method: &Method,
        segments: &[&str],
        query: Option<&str>,
        json: Option<&str>,
    ) -> Option<(u16, Value)> {
        api.dispatch(
            method,
            segments,
            query,
            None,
            json.map(str::as_bytes).unwrap_or(&[]),
        )
    }

    #[test]
    fn crud_and_substring_search() {
        let api = temp_api("crud");

        // create
        let (status, rec) = json_dispatch(
            &api,
            &Method::Post,
            &["posts", "records"],
            None,
            Some(r#"{"title":"Hello","body":"a wonderful world"}"#),
        )
        .unwrap();
        assert_eq!(status, 201);
        let id = rec.get("id").and_then(Value::as_i64).unwrap();

        // a second record for search discrimination
        json_dispatch(
            &api,
            &Method::Post,
            &["posts", "records"],
            None,
            Some(r#"{"title":"Other","body":"unrelated text"}"#),
        )
        .unwrap();

        // list → 2 records
        let (status, list) =
            json_dispatch(&api, &Method::Get, &["posts", "records"], None, None).unwrap();
        assert_eq!(status, 200);
        let Value::Array(items) = &list else {
            panic!("list not an array")
        };
        assert_eq!(items.len(), 2);

        // get one
        let (status, got) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records", &id.to_string()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(got.get("title").and_then(Value::as_str), Some("Hello"));

        // update
        let (status, upd) = json_dispatch(
            &api,
            &Method::Patch,
            &["posts", "records", &id.to_string()],
            None,
            Some(r#"{"title":"Hello edited"}"#),
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            upd.get("title").and_then(Value::as_str),
            Some("Hello edited")
        );

        // substring search (endpoint unset → Store::search over Text fields)
        let (status, found) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records"],
            Some("search=wonderful"),
            None,
        )
        .unwrap();
        assert_eq!(status, 200);
        let Value::Array(hits) = &found else {
            panic!("search not an array")
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("id").and_then(Value::as_i64), Some(id));

        // delete → 204, then a re-get is 404
        let (status, _) = json_dispatch(
            &api,
            &Method::Delete,
            &["posts", "records", &id.to_string()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(status, 204);
        let (status, _) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records", &id.to_string()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(status, 404);
    }

    #[test]
    fn validation_failure_is_400() {
        let api = temp_api("validation");
        // `title` is required → missing it is a validation error.
        let (status, resp) = json_dispatch(
            &api,
            &Method::Post,
            &["posts", "records"],
            None,
            Some(r#"{"body":"no title"}"#),
        )
        .unwrap();
        assert_eq!(status, 400);
        assert!(resp.get("error").is_some());
    }

    #[test]
    fn unknown_collection_is_404() {
        let api = temp_api("unknown");
        let (status, resp) =
            json_dispatch(&api, &Method::Get, &["ghosts", "records"], None, None).unwrap();
        assert_eq!(status, 404);
        assert!(resp.get("error").is_some());
    }

    #[test]
    fn bad_id_is_400() {
        let api = temp_api("badid");
        let (status, _) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records", "not-a-number"],
            None,
            None,
        )
        .unwrap();
        assert_eq!(status, 400);
    }

    #[test]
    fn manifest_lists_collections() {
        let api = temp_api("manifest");
        let (status, m) = json_dispatch(&api, &Method::Get, &[], None, None).unwrap();
        assert_eq!(status, 200);
        assert!(m.get("collections").is_some());
    }

    #[test]
    fn non_collections_shape_falls_through() {
        let api = temp_api("shape");
        assert!(json_dispatch(
            &api,
            &Method::Get,
            &["posts", "extra", "1", "2"],
            None,
            None
        )
        .is_none());
    }

    #[test]
    fn query_params_parse() {
        let p = QueryParams::parse(Some("search=hello%20world&limit=5&expand=author,editor"));
        assert_eq!(p.search.as_deref(), Some("hello world"));
        assert_eq!(p.limit, Some(5));
        assert_eq!(p.expand.as_deref(), Some("author,editor"));
        assert_eq!(expand_fields("author, editor ,,"), vec!["author", "editor"]);
    }

    // ---- relations: TOML mapping + existence checks + ?expand -------------

    #[test]
    fn relation_toml_maps_to_relation_kind() {
        let toml = "\
[[collection]]
name = \"posts\"
  [[collection.field]]
  name = \"author\"
  type = \"relation\"
  collection = \"users\"
";
        let colls = parse_collections(toml).expect("valid schema");
        let author = colls[0].field("author").unwrap();
        assert_eq!(author.kind, FieldKind::Relation("users".into()));
    }

    #[test]
    fn relation_without_collection_is_an_error() {
        let toml = "\
[[collection]]
name = \"posts\"
  [[collection.field]]
  name = \"author\"
  type = \"relation\"
";
        let err = parse_collections(toml).unwrap_err();
        assert!(err.contains("collection"), "got: {err}");
        assert!(err.contains("relation"), "got: {err}");
    }

    #[test]
    fn file_toml_maps_to_file_kind() {
        let toml = "\
[[collection]]
name = \"docs\"
  [[collection.field]]
  name = \"attachment\"
  type = \"file\"
";
        let colls = parse_collections(toml).expect("valid schema");
        assert_eq!(colls[0].field("attachment").unwrap().kind, FieldKind::File);
    }

    /// Two collections: `users` and `posts` with a relation to users.
    fn blog_schema() -> Vec<Collection> {
        vec![
            Collection::new(
                "users",
                vec![Field::new("name", FieldKind::Text).required()],
            ),
            Collection::new(
                "posts",
                vec![
                    Field::new("title", FieldKind::Text).required(),
                    Field::relation("author", "users"),
                ],
            ),
        ]
    }

    #[test]
    fn expand_inlines_the_related_record() {
        let api = api_with("expand", blog_schema());

        // Create a user, then a post referencing it.
        let (_, user) = json_dispatch(
            &api,
            &Method::Post,
            &["users", "records"],
            None,
            Some(r#"{"name":"Ada"}"#),
        )
        .unwrap();
        let uid = user.get("id").and_then(Value::as_i64).unwrap();

        let (status, post) = json_dispatch(
            &api,
            &Method::Post,
            &["posts", "records"],
            None,
            Some(&format!(r#"{{"title":"Hello","author":{uid}}}"#)),
        )
        .unwrap();
        assert_eq!(status, 201);
        let pid = post.get("id").and_then(Value::as_i64).unwrap();

        // GET one with ?expand=author inlines `author_expanded`.
        let (status, got) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records", &pid.to_string()],
            Some("expand=author"),
            None,
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            got.get("author_expanded")
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str),
            Some("Ada")
        );

        // LIST with ?expand=author inlines it on each record too.
        let (status, list) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records"],
            Some("expand=author"),
            None,
        )
        .unwrap();
        assert_eq!(status, 200);
        let Value::Array(items) = &list else {
            panic!("not an array")
        };
        assert_eq!(
            items[0]
                .get("author_expanded")
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str),
            Some("Ada")
        );

        // Without ?expand, the relation is just the id (no `_expanded`).
        let (_, plain) = json_dispatch(
            &api,
            &Method::Get,
            &["posts", "records", &pid.to_string()],
            None,
            None,
        )
        .unwrap();
        assert!(plain.get("author_expanded").is_none());
        assert_eq!(plain.get("author").and_then(Value::as_i64), Some(uid));
    }

    #[test]
    fn create_with_bad_relation_id_is_400() {
        let api = api_with("badrel", blog_schema());
        // No user with id 999 exists → existence check fails → 400.
        let (status, resp) = json_dispatch(
            &api,
            &Method::Post,
            &["posts", "records"],
            None,
            Some(r#"{"title":"Hello","author":999}"#),
        )
        .unwrap();
        assert_eq!(status, 400);
        assert!(resp.get("error").is_some());
    }

    // ---- file uploads: multipart create + download ------------------------

    /// A `documents` collection with a title and an `attachment` file field.
    fn docs_schema() -> Vec<Collection> {
        vec![Collection::new(
            "documents",
            vec![
                Field::new("title", FieldKind::Text).required(),
                Field::new("attachment", FieldKind::File),
            ],
        )]
    }

    /// Build a `multipart/form-data` body from `(headers, payload)` part specs.
    fn multipart_body(boundary: &str, parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (headers, payload) in parts {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            out.extend_from_slice(headers.as_bytes());
            out.extend_from_slice(b"\r\n\r\n");
            out.extend_from_slice(payload);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    #[test]
    fn multipart_create_stores_a_blob_and_download_returns_bytes() {
        let api = api_with("upload", docs_schema());
        let boundary = "----AkurTest";
        let file_bytes: &[u8] = b"binary\x00\xff data";
        let raw = multipart_body(
            boundary,
            &[
                (
                    "Content-Disposition: form-data; name=\"title\"",
                    b"My Document",
                ),
                (
                    "Content-Disposition: form-data; name=\"attachment\"; filename=\"data.bin\"\r\nContent-Type: application/octet-stream",
                    file_bytes,
                ),
            ],
        );
        let ct = format!("multipart/form-data; boundary={boundary}");

        let (status, rec) = api
            .dispatch(
                &Method::Post,
                &["documents", "records"],
                None,
                Some(&ct),
                &raw,
            )
            .unwrap();
        assert_eq!(status, 201);
        assert_eq!(
            rec.get("title").and_then(Value::as_str),
            Some("My Document")
        );
        let att = rec.get("attachment").expect("attachment descriptor");
        assert_eq!(
            att.get("filename").and_then(Value::as_str),
            Some("data.bin")
        );
        assert_eq!(
            att.get("size").and_then(Value::as_i64),
            Some(file_bytes.len() as i64)
        );
        assert!(att.get("blob").and_then(Value::as_str).is_some());

        let id = rec.get("id").and_then(Value::as_i64).unwrap();

        // Download returns the exact bytes with the stored content type + filename.
        let resp = api.download("documents", &id.to_string(), "attachment");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, file_bytes);
        let ct = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str());
        assert_eq!(ct, Some("application/octet-stream"));
        let cd = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-disposition"))
            .map(|(_, v)| v.as_str());
        assert_eq!(cd, Some("inline; filename=\"data.bin\""));
    }

    #[test]
    fn json_create_still_works_with_file_field_omitted() {
        let api = api_with("jsonfile", docs_schema());
        // A plain JSON create with no file part is fine; attachment is absent.
        let (status, rec) = json_dispatch(
            &api,
            &Method::Post,
            &["documents", "records"],
            None,
            Some(r#"{"title":"Text only"}"#),
        )
        .unwrap();
        assert_eq!(status, 201);
        assert_eq!(rec.get("title").and_then(Value::as_str), Some("Text only"));
        assert!(rec.get("attachment").is_none());
    }

    #[test]
    fn download_missing_field_or_record_is_404() {
        let api = api_with("dl404", docs_schema());
        // Unknown record id.
        assert_eq!(api.download("documents", "1", "attachment").status, 404);
        // A non-file field name.
        assert_eq!(api.download("documents", "1", "title").status, 404);
        // Unknown collection.
        assert_eq!(api.download("ghosts", "1", "attachment").status, 404);
    }

    #[test]
    fn embed_prefix_bounds_isolate_a_collection() {
        let (start, end) = embed_prefix_bounds("posts");
        let key = embed_key("posts", 7);
        assert!(key.as_slice() >= start.as_slice() && key.as_slice() < end.as_slice());
        // A different collection whose name shares a prefix is excluded.
        let other = embed_key("postsx", 7);
        assert!(!(other.as_slice() >= start.as_slice() && other.as_slice() < end.as_slice()));
        assert_eq!(id_from_key("posts", &key), Some(7));
    }
}
