//! The record store: CRUD + substring search over a single [`BTree`].
//!
//! ## Key layout
//!
//! Every record lives under a per-collection prefix:
//!
//! ```text
//! coll:<name>:<id>     where <id> is a big-endian u64   (the record)
//! coll:<name>:_seq     the auto-increment counter        (last id used, BE u64)
//! ```
//!
//! Because record ids are fixed 8-byte big-endian, they sort in numeric order,
//! so a key range scan over the prefix yields records in id (insertion) order.
//! The `_seq` counter shares the prefix but has a different key length, so range
//! scans filter strictly on `prefix.len() + 8` and never confuse the two.
//!
//! ## Stored shape
//!
//! A stored record is a JSON object whose first two keys are always the
//! engine-assigned `id` (int) and `created` (unix seconds, int), followed by the
//! schema fields that were supplied, in schema declaration order.

use std::time::{SystemTime, UNIX_EPOCH};

use akurai_json::{parse, Value};
use akurai_storage::BTree;

use crate::error::CollError;
use crate::schema::{Collection, Field, FieldKind};

/// Reserved record keys that the engine owns and input may not set.
const RESERVED: [&str; 2] = ["id", "created"];

/// A record store backed by one B+tree file. All collections share the tree,
/// separated by their key prefix.
pub struct Store {
    tree: BTree,
}

impl Store {
    /// Open (or create) the backing tree at `path`.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Store, CollError> {
        Ok(Store {
            tree: BTree::open(path)?,
        })
    }

    /// Wrap an already-open tree (useful when the caller manages the file).
    pub fn from_tree(tree: BTree) -> Store {
        Store { tree }
    }

    /// Validate `input` against `coll`'s schema, assign an `id` and `created`
    /// timestamp, persist, commit, and return the stored record.
    ///
    /// Relation fields are type-checked (the value must be an integer id) but
    /// the referenced record's *existence* is not verified — use
    /// [`Store::create_checked`] to opt into referential checking.
    pub fn create(&mut self, coll: &Collection, input: Value) -> Result<Value, CollError> {
        self.create_impl(coll, input, None)
    }

    /// Like [`Store::create`], but additionally verifies that every relation
    /// value references an existing record in its target collection. `all` is
    /// the set of collections the caller knows about: if a relation's target
    /// collection is present in `all`, the referenced id must exist (else a
    /// [`CollError::Validation`] is returned); if the target is *not* in `all`,
    /// the id is accepted without an existence check.
    pub fn create_checked(
        &mut self,
        coll: &Collection,
        input: Value,
        all: &[Collection],
    ) -> Result<Value, CollError> {
        self.create_impl(coll, input, Some(all))
    }

    fn create_impl(
        &mut self,
        coll: &Collection,
        input: Value,
        check: Option<&[Collection]>,
    ) -> Result<Value, CollError> {
        let supplied = as_object(&input, "input must be a JSON object")?;
        validate_create(coll, supplied)?;
        if let Some(all) = check {
            self.verify_relations(coll, supplied, all)?;
        }

        let id = self.next_id(&coll.name)?;
        let created = now_secs();

        let mut record: Vec<(String, Value)> = Vec::with_capacity(coll.fields.len() + 2);
        record.push(("id".into(), Value::Int(id as i64)));
        record.push(("created".into(), Value::Int(created)));
        for field in &coll.fields {
            if let Some(raw) = lookup(supplied, &field.name) {
                if !matches!(raw, Value::Null) {
                    record.push((field.name.clone(), coerce(field, raw)));
                }
            }
        }

        let value = Value::Object(record);
        self.tree
            .insert(&record_key(&coll.name, id), value.to_json().as_bytes())?;
        self.tree.commit()?;
        Ok(value)
    }

    /// Fetch a single record by id, or `None` if it does not exist.
    pub fn get(&mut self, coll: &Collection, id: u64) -> Result<Option<Value>, CollError> {
        match self.tree.get(&record_key(&coll.name, id))? {
            Some(bytes) => Ok(Some(decode(&bytes)?)),
            None => Ok(None),
        }
    }

    /// List records, newest first (highest id first). `limit` caps the count.
    pub fn list(
        &mut self,
        coll: &Collection,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, CollError> {
        let mut records = self.scan(&coll.name)?;
        records.reverse();
        if let Some(n) = limit {
            records.truncate(n);
        }
        Ok(records)
    }

    /// Apply a partial `patch` to an existing record. Only the keys present in
    /// `patch` are changed; each changed field is re-validated against the
    /// schema. Returns the updated record, or `None` if the id does not exist.
    pub fn update(
        &mut self,
        coll: &Collection,
        id: u64,
        patch: Value,
    ) -> Result<Option<Value>, CollError> {
        self.update_impl(coll, id, patch, None)
    }

    /// Like [`Store::update`], but additionally verifies that any relation
    /// value present in `patch` references an existing record in its target
    /// collection (see [`Store::create_checked`] for the `all` semantics).
    pub fn update_checked(
        &mut self,
        coll: &Collection,
        id: u64,
        patch: Value,
        all: &[Collection],
    ) -> Result<Option<Value>, CollError> {
        self.update_impl(coll, id, patch, Some(all))
    }

    fn update_impl(
        &mut self,
        coll: &Collection,
        id: u64,
        patch: Value,
        check: Option<&[Collection]>,
    ) -> Result<Option<Value>, CollError> {
        let key = record_key(&coll.name, id);
        let existing = match self.tree.get(&key)? {
            Some(bytes) => decode(&bytes)?,
            None => return Ok(None),
        };
        let mut pairs = match existing {
            Value::Object(p) => p,
            _ => return Err(CollError::Corrupt("stored record is not an object")),
        };
        let updates = as_object(&patch, "patch must be a JSON object")?;
        if let Some(all) = check {
            self.verify_relations(coll, updates, all)?;
        }

        for (key_name, raw) in updates {
            if RESERVED.contains(&key_name.as_str()) {
                return Err(CollError::validation(format!(
                    "field '{key_name}' is reserved and cannot be patched"
                )));
            }
            let field = coll
                .field(key_name)
                .ok_or_else(|| CollError::validation(format!("unknown field '{key_name}'")))?;
            if matches!(raw, Value::Null) {
                if field.required {
                    return Err(CollError::validation(format!(
                        "required field '{key_name}' cannot be set to null"
                    )));
                }
                pairs.retain(|(k, _)| k != key_name);
                continue;
            }
            check_type(field, raw)?;
            let new_value = coerce(field, raw);
            set_field(&mut pairs, key_name, new_value);
        }

        let value = Value::Object(pairs);
        self.tree.insert(&key, value.to_json().as_bytes())?;
        self.tree.commit()?;
        Ok(Some(value))
    }

    /// Delete a record by id. Returns `true` if a record was removed.
    pub fn delete(&mut self, coll: &Collection, id: u64) -> Result<bool, CollError> {
        let removed = self.tree.delete(&record_key(&coll.name, id))?;
        if removed {
            self.tree.commit()?;
        }
        Ok(removed)
    }

    /// Case-insensitive substring search across the collection's `Text` fields.
    /// Honest baseline only — semantic ranking is layered on by the CLI. Results
    /// are newest first; `limit` caps the count. An empty query matches all.
    pub fn search(
        &mut self,
        coll: &Collection,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, CollError> {
        let needle = query.to_lowercase();
        let text_fields: Vec<&str> = coll
            .fields
            .iter()
            .filter(|f| f.kind == FieldKind::Text)
            .map(|f| f.name.as_str())
            .collect();

        let mut records = self.scan(&coll.name)?;
        records.reverse();

        let mut out = Vec::new();
        for record in records {
            if record_matches(&record, &text_fields, &needle) {
                out.push(record);
                if let Some(n) = limit {
                    if out.len() >= n {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Fetch a record by id and inline its relation references.
    ///
    /// For every name in `expand` that is a relation field on `coll`, the
    /// referenced record (looked up in its target collection within `all`) is
    /// inlined under a sibling key `"<field>_expanded"`. A reference that is
    /// absent, that points at a missing record, or whose target collection is
    /// not present in `all`, expands to `null`. Names in `expand` that are not
    /// relation fields are silently ignored. The original relation id key is
    /// left untouched. Returns `None` if the record itself does not exist.
    pub fn get_expanded(
        &mut self,
        coll: &Collection,
        id: u64,
        expand: &[&str],
        all: &[Collection],
    ) -> Result<Option<Value>, CollError> {
        let record = match self.get(coll, id)? {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(self.expand_record(coll, record, expand, all)?))
    }

    /// List records (newest first, `limit`-capped) with relation references
    /// inlined, following the same rules as [`Store::get_expanded`].
    pub fn list_expanded(
        &mut self,
        coll: &Collection,
        limit: Option<usize>,
        expand: &[&str],
        all: &[Collection],
    ) -> Result<Vec<Value>, CollError> {
        let records = self.list(coll, limit)?;
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            out.push(self.expand_record(coll, record, expand, all)?);
        }
        Ok(out)
    }

    /// Inline the requested relation references into one record. See
    /// [`Store::get_expanded`] for the expansion contract.
    fn expand_record(
        &mut self,
        coll: &Collection,
        record: Value,
        expand: &[&str],
        all: &[Collection],
    ) -> Result<Value, CollError> {
        let mut pairs = match record {
            Value::Object(p) => p,
            other => return Ok(other),
        };
        for &name in expand {
            // Ignore names that are not relation fields on this collection.
            let target = match coll.field(name).map(|f| &f.kind) {
                Some(FieldKind::Relation(t)) => t.clone(),
                _ => continue,
            };
            let ref_id = lookup(&pairs, name).and_then(Value::as_i64);
            let expanded = match ref_id {
                Some(rid) if all.iter().any(|c| c.name == target) => {
                    match self.tree.get(&record_key(&target, rid as u64))? {
                        Some(bytes) => decode(&bytes)?,
                        None => Value::Null,
                    }
                }
                _ => Value::Null,
            };
            set_field(&mut pairs, &format!("{name}_expanded"), expanded);
        }
        Ok(Value::Object(pairs))
    }

    /// Verify that every relation value in `supplied` references an existing
    /// record in its target collection. Only relation fields whose target is
    /// present in `all` are checked; unknown targets are accepted as-is. Type
    /// validation (relation == integer) is handled separately by `check_type`.
    fn verify_relations(
        &mut self,
        coll: &Collection,
        supplied: &[(String, Value)],
        all: &[Collection],
    ) -> Result<(), CollError> {
        for field in &coll.fields {
            let target = match &field.kind {
                FieldKind::Relation(t) => t,
                _ => continue,
            };
            // Only check ids that were actually supplied as integers; missing
            // or non-integer values are caught by required/type validation.
            let ref_id = match lookup(supplied, &field.name).and_then(Value::as_i64) {
                Some(id) => id,
                None => continue,
            };
            // Opt-in: only verify existence when the target is a known collection.
            if !all.iter().any(|c| c.name == *target) {
                continue;
            }
            if ref_id < 0 || self.tree.get(&record_key(target, ref_id as u64))?.is_none() {
                return Err(CollError::validation(format!(
                    "relation field '{}' references non-existent {} id {}",
                    field.name, target, ref_id
                )));
            }
        }
        Ok(())
    }

    /// Scan every record under a collection prefix, in ascending id order.
    fn scan(&mut self, name: &str) -> Result<Vec<Value>, CollError> {
        let prefix = record_prefix(name);
        let start = record_key(name, 0);
        let end = prefix_upper_bound(&prefix);
        let entries = self.tree.range(&start, &end)?;

        let mut out = Vec::with_capacity(entries.len());
        for (key, bytes) in entries {
            // Records are exactly prefix + 8 bytes; the `_seq` counter and any
            // future meta keys share the prefix but differ in length.
            if key.len() != prefix.len() + 8 {
                continue;
            }
            out.push(decode(&bytes)?);
        }
        Ok(out)
    }

    /// Read, bump, and persist the per-collection id counter, returning the new
    /// id. The counter is committed together with the record by the caller.
    fn next_id(&mut self, name: &str) -> Result<u64, CollError> {
        let key = seq_key(name);
        let last = match self.tree.get(&key)? {
            Some(bytes) => decode_counter(&bytes)?,
            None => 0,
        };
        let id = last + 1;
        self.tree.insert(&key, &id.to_be_bytes())?;
        Ok(id)
    }
}

// --- key helpers ------------------------------------------------------------

fn record_prefix(name: &str) -> Vec<u8> {
    format!("coll:{name}:").into_bytes()
}

fn record_key(name: &str, id: u64) -> Vec<u8> {
    let mut key = record_prefix(name);
    key.extend_from_slice(&id.to_be_bytes());
    key
}

fn seq_key(name: &str) -> Vec<u8> {
    format!("coll:{name}:_seq").into_bytes()
}

/// The smallest key strictly greater than every key sharing `prefix`. Built by
/// bumping the last non-`0xff` byte; the prefix ends in `:` so this is trivial.
fn prefix_upper_bound(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.last_mut() {
        if *last < 0xff {
            *last += 1;
            return end;
        }
        end.pop();
    }
    // Prefix was all 0xff (impossible for "coll:...:"); fall back to a max key.
    vec![0xff; prefix.len() + 9]
}

// --- value helpers ----------------------------------------------------------

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn decode(bytes: &[u8]) -> Result<Value, CollError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| CollError::Corrupt("stored record is not valid UTF-8"))?;
    Ok(parse(text)?)
}

fn decode_counter(bytes: &[u8]) -> Result<u64, CollError> {
    let arr: [u8; 8] = bytes
        .try_into()
        .map_err(|_| CollError::Corrupt("counter is not 8 bytes"))?;
    Ok(u64::from_be_bytes(arr))
}

fn as_object<'a>(v: &'a Value, msg: &'static str) -> Result<&'a [(String, Value)], CollError> {
    match v {
        Value::Object(pairs) => Ok(pairs),
        _ => Err(CollError::validation(msg)),
    }
}

fn lookup<'a>(pairs: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn set_field(pairs: &mut Vec<(String, Value)>, key: &str, value: Value) {
    if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        pairs.push((key.to_string(), value));
    }
}

fn record_matches(record: &Value, text_fields: &[&str], needle: &str) -> bool {
    for name in text_fields {
        if let Some(Value::Str(s)) = record.get(name) {
            if s.to_lowercase().contains(needle) {
                return true;
            }
        }
    }
    // An empty needle matches everything, even records with no text fields set.
    needle.is_empty()
}

// --- validation -------------------------------------------------------------

fn validate_create(coll: &Collection, input: &[(String, Value)]) -> Result<(), CollError> {
    // Reject unknown and reserved keys.
    for (key, _) in input {
        if RESERVED.contains(&key.as_str()) {
            return Err(CollError::validation(format!(
                "field '{key}' is reserved and assigned by the engine"
            )));
        }
        if coll.field(key).is_none() {
            return Err(CollError::validation(format!("unknown field '{key}'")));
        }
    }
    // Required-present and type checks.
    for field in &coll.fields {
        match lookup(input, &field.name) {
            None | Some(Value::Null) => {
                if field.required {
                    return Err(CollError::validation(format!(
                        "missing required field '{}'",
                        field.name
                    )));
                }
            }
            Some(raw) => check_type(field, raw)?,
        }
    }
    Ok(())
}

/// Verify `raw` matches the field's declared kind. `Int` is accepted where a
/// `Float` is expected (and coerced on store); everything else is strict.
fn check_type(field: &Field, raw: &Value) -> Result<(), CollError> {
    let ok = match &field.kind {
        FieldKind::Text => matches!(raw, Value::Str(_)),
        FieldKind::Int => matches!(raw, Value::Int(_)),
        FieldKind::Float => matches!(raw, Value::Float(_) | Value::Int(_)),
        FieldKind::Bool => matches!(raw, Value::Bool(_)),
        // A relation stores the referenced record's integer id.
        FieldKind::Relation(_) => matches!(raw, Value::Int(_)),
        // A file stores a `{blob,filename,content_type,size}` descriptor object.
        // `null`/omitted is handled by `validate_create` before this is reached,
        // so here a present value must be an object (uploads write it; raw JSON
        // generally leaves it null since files arrive over multipart).
        FieldKind::File => matches!(raw, Value::Object(_)),
    };
    if ok {
        Ok(())
    } else {
        Err(CollError::validation(format!(
            "field '{}' expects {}, got {}",
            field.name,
            field.kind.as_str(),
            type_name(raw)
        )))
    }
}

/// Apply storage-time coercion (currently only `Int` → `Float`). Assumes the
/// value already passed [`check_type`].
fn coerce(field: &Field, raw: &Value) -> Value {
    match (&field.kind, raw) {
        (FieldKind::Float, Value::Int(n)) => Value::Float(*n as f64),
        _ => raw.clone(),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "text",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
