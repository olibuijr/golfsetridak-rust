//! The TOML [`Value`] type.
//!
//! Tables and arrays keep insertion order (a `Vec` of pairs, not a map) so the
//! parsed schema reads the way it was written — and so we stay dependency-free.
//! Integers and floats are kept distinct so a field's `int` type never silently
//! becomes a float.

/// A parsed TOML value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A table: ordered key/value pairs. The top-level document is one of these.
    Table(Vec<(String, Value)>),
    /// An array. May be an array of tables (`[[...]]`) or an inline array.
    Array(Vec<Value>),
    Str(String),
    /// An integer literal (no `.`/`e`), preserved exactly.
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    /// Look up a key in a table, returning `None` for non-tables or missing
    /// keys. First match wins.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Table(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Table(pairs) => Some(pairs),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(n) => Some(*n),
            Value::Int(n) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}
