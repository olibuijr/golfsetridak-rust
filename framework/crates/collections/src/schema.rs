//! The collection schema model — plain Rust, decoupled from any config format.
//!
//! A [`Collection`] is a named list of [`Field`]s. The CLI builds these from
//! `collections.toml` at integration time; this crate never parses TOML, it
//! only consumes the resulting types.

/// The declared type of a field. Determines validation and how the value is
/// matched during substring [`search`](crate::Store::search).
///
/// `Relation` carries a `String` (the target collection name), so this enum is
/// `Clone` but not `Copy` — match on `&field.kind` rather than moving it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// A UTF-8 string. Only `Text` fields participate in substring search.
    Text,
    /// A 64-bit signed integer.
    Int,
    /// A 64-bit float. Integer inputs are accepted and coerced.
    Float,
    /// A boolean.
    Bool,
    /// A reference to another collection's record by its `id`. The wrapped
    /// `String` is the *target* collection's name. The stored value is the
    /// referenced record's integer `id`; the target record can be inlined on
    /// read via [`Store::get_expanded`](crate::Store::get_expanded).
    Relation(String),
    /// An uploaded file. The stored value is a small JSON object describing the
    /// blob, of the shape
    /// `{ "blob": "<id>", "filename": "...", "content_type": "...", "size": <n> }`
    /// (or `null` when unset). Files arrive over `multipart/form-data`, not in a
    /// JSON body, so on JSON input a `File` field accepts only `null`/omitted or
    /// an object; the CLI's upload path writes the descriptor object after
    /// storing the bytes in the content-addressed blob store.
    File,
}

impl FieldKind {
    /// The lowercase wire name used in the `/api/_meta` manifest.
    pub fn as_str(&self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Int => "int",
            FieldKind::Float => "float",
            FieldKind::Bool => "bool",
            FieldKind::Relation(_) => "relation",
            FieldKind::File => "file",
        }
    }

    /// For a `Relation`, the target collection's name; `None` otherwise.
    pub fn relation_target(&self) -> Option<&str> {
        match self {
            FieldKind::Relation(target) => Some(target.as_str()),
            _ => None,
        }
    }
}

/// A single declared field on a collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The field's name. Must be unique within the collection and must not be
    /// one of the reserved keys (`id`, `created`).
    pub name: String,
    /// The field's declared type.
    pub kind: FieldKind,
    /// When `true`, the field must be present and non-null on `create`.
    pub required: bool,
    /// Marks a `Text` field for later semantic indexing. This crate only
    /// stores and exposes the flag — embedding is layered on by the CLI.
    pub embed: bool,
}

impl Field {
    /// Convenience constructor for an optional, non-embedded field.
    pub fn new(name: impl Into<String>, kind: FieldKind) -> Field {
        Field {
            name: name.into(),
            kind,
            required: false,
            embed: false,
        }
    }

    /// Convenience constructor for a relation field pointing at `target`.
    pub fn relation(name: impl Into<String>, target: impl Into<String>) -> Field {
        Field::new(name, FieldKind::Relation(target.into()))
    }

    /// Builder: mark this field as required.
    pub fn required(mut self) -> Field {
        self.required = true;
        self
    }

    /// Builder: mark this `Text` field for semantic embedding.
    pub fn embed(mut self) -> Field {
        self.embed = true;
        self
    }
}

/// A named collection: the unit that becomes an auto-generated REST resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    /// The collection's name. Used in storage keys and REST paths, so keep it
    /// to URL/key-safe characters (no `:` is required — but recommended).
    pub name: String,
    /// The declared fields, in declaration order.
    pub fields: Vec<Field>,
}

impl Collection {
    /// Construct a collection from a name and its fields.
    pub fn new(name: impl Into<String>, fields: Vec<Field>) -> Collection {
        Collection {
            name: name.into(),
            fields,
        }
    }

    /// Look up a field by name.
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}
