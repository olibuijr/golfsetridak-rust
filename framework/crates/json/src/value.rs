//! The JSON [`Value`] type and its serializer.

use std::fmt::Write as _;

/// A JSON value. Objects keep insertion order (a `Vec` of pairs, not a map) so
/// serialized output is stable and reads the way it was written — and so we
/// stay dependency-free.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// An integer literal (no `.`/`e`), preserved exactly — record IDs depend
    /// on this not being coerced to `f64`.
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Look up a key in an object, returning `None` for non-objects or missing
    /// keys. First match wins.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
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

    /// Serialize to a compact JSON string.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(n) => {
                if n.is_finite() {
                    let _ = write!(out, "{n}");
                } else {
                    // JSON has no Infinity/NaN; null is the conventional fallback.
                    out.push_str("null");
                }
            }
            Value::Str(s) => write_escaped(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(k, out);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

/// Write a JSON string literal, escaping per RFC 8259.
fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_primitives() {
        assert_eq!(Value::Null.to_json(), "null");
        assert_eq!(Value::Bool(true).to_json(), "true");
        assert_eq!(Value::Int(42).to_json(), "42");
        assert_eq!(Value::Int(-7).to_json(), "-7");
        assert_eq!(Value::Str("hi".into()).to_json(), "\"hi\"");
    }

    #[test]
    fn escapes_strings() {
        assert_eq!(
            Value::Str("a\"b\\c\nd\te".into()).to_json(),
            "\"a\\\"b\\\\c\\nd\\te\""
        );
        assert_eq!(Value::Str("\u{01}".into()).to_json(), "\"\\u0001\"");
    }

    #[test]
    fn serializes_nested_object_in_order() {
        let v = Value::Object(vec![
            ("name".into(), Value::Str("demo".into())),
            ("ok".into(), Value::Bool(true)),
            (
                "tags".into(),
                Value::Array(vec![Value::Int(1), Value::Int(2)]),
            ),
        ]);
        assert_eq!(v.to_json(), r#"{"name":"demo","ok":true,"tags":[1,2]}"#);
    }

    #[test]
    fn non_finite_floats_become_null() {
        assert_eq!(Value::Float(f64::NAN).to_json(), "null");
        assert_eq!(Value::Float(f64::INFINITY).to_json(), "null");
        assert_eq!(Value::Float(1.5).to_json(), "1.5");
    }

    #[test]
    fn accessors_work() {
        let v = Value::Object(vec![("id".into(), Value::Int(9))]);
        assert_eq!(v.get("id").and_then(Value::as_i64), Some(9));
        assert_eq!(v.get("missing"), None);
    }
}
