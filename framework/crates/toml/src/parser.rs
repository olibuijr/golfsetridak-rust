//! Structural TOML parsing: walks the document statement by statement, building
//! the nested table tree. Headers (`[table]`, `[[array of tables]]`) move the
//! "current table" the following key/value lines write into; dotted keys and
//! dotted headers create intermediate tables. Arrays (which may span lines) and
//! scalars come from the [`Cursor`] in `lexer.rs`.

use crate::error::TomlError;
use crate::lexer::Cursor;
use crate::value::Value;

/// Parse a TOML document, returning the top-level table as [`Value::Table`].
///
/// Never panics on malformed input — returns [`TomlError`] with a line number.
pub fn parse(input: &str) -> Result<Value, TomlError> {
    let mut parser = Parser {
        cur: Cursor::new(input),
        root: Vec::new(),
        current_path: Vec::new(),
    };
    parser.run()?;
    Ok(Value::Table(parser.root))
}

struct Parser {
    cur: Cursor,
    root: Vec<(String, Value)>,
    /// Path to the table that bare key/value lines currently write into.
    current_path: Vec<String>,
}

impl Parser {
    fn run(&mut self) -> Result<(), TomlError> {
        loop {
            self.cur.skip_blank();
            if self.cur.at_eof() {
                return Ok(());
            }
            if self.cur.peek() == Some('[') {
                self.parse_header()?;
            } else {
                self.parse_key_value()?;
            }
            self.finish_line()?;
        }
    }

    /// After a statement, only inline whitespace, a comment, then a newline (or
    /// EOF) may follow.
    fn finish_line(&mut self) -> Result<(), TomlError> {
        self.cur.skip_inline_ws();
        self.cur.skip_comment();
        match self.cur.peek() {
            None | Some('\n') => {
                self.cur.bump();
                Ok(())
            }
            Some(c) => Err(TomlError::new(
                format!("unexpected '{c}' after value"),
                self.cur.line(),
            )),
        }
    }

    /// Parse `[table]` or `[[array of tables]]` and reposition `current_path`.
    fn parse_header(&mut self) -> Result<(), TomlError> {
        let line = self.cur.line();
        self.cur.bump(); // '['
        let is_array = self.cur.peek() == Some('[');
        if is_array {
            self.cur.bump();
        }

        let path = self.read_dotted_path()?;

        self.cur.skip_inline_ws();
        if self.cur.bump() != Some(']') {
            return Err(TomlError::new("expected ']' to close header", line));
        }
        if is_array && self.cur.bump() != Some(']') {
            return Err(TomlError::new("expected ']]' to close header", line));
        }
        if path.is_empty() {
            return Err(TomlError::new("empty header", line));
        }

        if is_array {
            self.open_array_table(&path, line)?;
        } else {
            navigate(&mut self.root, &path, line)?;
        }
        self.current_path = path;
        Ok(())
    }

    /// Append a fresh table to the array of tables at `path`, creating the array
    /// if needed.
    fn open_array_table(&mut self, path: &[String], line: usize) -> Result<(), TomlError> {
        let (last, parents) = path
            .split_last()
            .ok_or_else(|| TomlError::new("empty header", line))?;
        let parent = navigate(&mut self.root, parents, line)?;
        let idx = match parent.iter().position(|(k, _)| k == last) {
            Some(i) => i,
            None => {
                parent.push((last.clone(), Value::Array(Vec::new())));
                parent.len() - 1
            }
        };
        match &mut parent[idx].1 {
            Value::Array(arr) => {
                arr.push(Value::Table(Vec::new()));
                Ok(())
            }
            _ => Err(TomlError::new(
                format!("'{last}' is already defined and is not an array of tables"),
                line,
            )),
        }
    }

    /// Parse a `key = value` line into the current table.
    fn parse_key_value(&mut self) -> Result<(), TomlError> {
        let line = self.cur.line();
        let key_path = self.read_dotted_path()?;
        self.cur.skip_inline_ws();
        if self.cur.bump() != Some('=') {
            return Err(TomlError::new("expected '=' after key", line));
        }
        let value = self.parse_value()?;

        let current = self.current_path.clone();
        let table = navigate(&mut self.root, &current, line)?;
        let (last, parents) = key_path
            .split_last()
            .ok_or_else(|| TomlError::new("expected a key", line))?;
        let target = navigate(table, parents, line)?;
        if target.iter().any(|(k, _)| k == last) {
            return Err(TomlError::new(format!("duplicate key '{last}'"), line));
        }
        target.push((last.clone(), value));
        Ok(())
    }

    /// A value is either an inline array or a scalar.
    fn parse_value(&mut self) -> Result<Value, TomlError> {
        self.cur.skip_inline_ws();
        if self.cur.peek() == Some('[') {
            self.parse_array()
        } else {
            self.cur.read_scalar()
        }
    }

    /// Parse `[a, b, c]` — newlines and a trailing comma are allowed inside.
    fn parse_array(&mut self) -> Result<Value, TomlError> {
        let line = self.cur.line();
        self.cur.bump(); // '['
        let mut items = Vec::new();
        loop {
            self.cur.skip_blank();
            match self.cur.peek() {
                Some(']') => {
                    self.cur.bump();
                    return Ok(Value::Array(items));
                }
                None => return Err(TomlError::new("unterminated array", line)),
                _ => {}
            }
            items.push(self.parse_value()?);
            self.cur.skip_blank();
            match self.cur.peek() {
                Some(',') => {
                    self.cur.bump();
                }
                Some(']') => {
                    self.cur.bump();
                    return Ok(Value::Array(items));
                }
                _ => {
                    return Err(TomlError::new(
                        "expected ',' or ']' in array",
                        self.cur.line(),
                    ))
                }
            }
        }
    }

    /// Read a dotted bare-key path: `a`, or `a.b.c`, with optional whitespace
    /// around the dots.
    fn read_dotted_path(&mut self) -> Result<Vec<String>, TomlError> {
        let mut path = Vec::new();
        loop {
            self.cur.skip_inline_ws();
            path.push(self.cur.read_bare_key()?);
            self.cur.skip_inline_ws();
            if self.cur.peek() == Some('.') {
                self.cur.bump();
            } else {
                return Ok(path);
            }
        }
    }
}

/// Walk `path` from `table`, descending (and creating tables) as needed; returns
/// the table at the end of the path.
fn navigate<'a>(
    mut table: &'a mut Vec<(String, Value)>,
    path: &[String],
    line: usize,
) -> Result<&'a mut Vec<(String, Value)>, TomlError> {
    for key in path {
        table = descend(table, key, line)?;
    }
    Ok(table)
}

/// Descend one level into `key`, creating an empty table if it is missing.
/// Descending into an array of tables targets its most recent element (TOML's
/// rule for dotted paths through `[[...]]`).
fn descend<'a>(
    table: &'a mut Vec<(String, Value)>,
    key: &str,
    line: usize,
) -> Result<&'a mut Vec<(String, Value)>, TomlError> {
    let idx = match table.iter().position(|(k, _)| k == key) {
        Some(i) => i,
        None => {
            table.push((key.to_string(), Value::Table(Vec::new())));
            table.len() - 1
        }
    };
    match &mut table[idx].1 {
        Value::Table(t) => Ok(t),
        Value::Array(arr) => match arr.last_mut() {
            Some(Value::Table(t)) => Ok(t),
            _ => Err(TomlError::new(format!("'{key}' is not a table"), line)),
        },
        _ => Err(TomlError::new(format!("'{key}' is not a table"), line)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        let v = parse("s = \"hi\"\nn = 42\nf = 1.5\nb = true\n").unwrap();
        assert_eq!(v.get("s").and_then(Value::as_str), Some("hi"));
        assert_eq!(v.get("n").and_then(Value::as_i64), Some(42));
        assert_eq!(v.get("f").and_then(Value::as_f64), Some(1.5));
        assert_eq!(v.get("b").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn signed_and_underscored_integers() {
        let v = parse("a = -7\nb = +10\nc = 1_000_000\n").unwrap();
        assert_eq!(v.get("a").and_then(Value::as_i64), Some(-7));
        assert_eq!(v.get("b").and_then(Value::as_i64), Some(10));
        assert_eq!(v.get("c").and_then(Value::as_i64), Some(1_000_000));
    }

    #[test]
    fn string_escapes() {
        let v = parse(r#"s = "a\"b\\c\n\td""#).unwrap();
        assert_eq!(v.get("s").and_then(Value::as_str), Some("a\"b\\c\n\td"));
    }

    #[test]
    fn comments_and_blank_lines() {
        let src = "# leading comment\n\nname = \"x\"  # trailing\n\n# tail\n";
        let v = parse(src).unwrap();
        assert_eq!(v.get("name").and_then(Value::as_str), Some("x"));
    }

    #[test]
    fn inline_arrays() {
        let v = parse("xs = [1, 2, 3]\nss = [\"a\", \"b\"]\n").unwrap();
        assert_eq!(
            v.get("xs"),
            Some(&Value::Array(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3)
            ]))
        );
        assert_eq!(
            v.get("ss"),
            Some(&Value::Array(vec![
                Value::Str("a".into()),
                Value::Str("b".into())
            ]))
        );
    }

    #[test]
    fn multiline_array_with_trailing_comma() {
        let src = "xs = [\n  1,\n  2,\n  3,\n]\n";
        let v = parse(src).unwrap();
        assert_eq!(
            v.get("xs"),
            Some(&Value::Array(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3)
            ]))
        );
    }

    #[test]
    fn tables_and_dotted_keys() {
        let v = parse("[server]\nhost = \"localhost\"\nport = 8080\n").unwrap();
        let server = v.get("server").unwrap();
        assert_eq!(
            server.get("host").and_then(Value::as_str),
            Some("localhost")
        );
        assert_eq!(server.get("port").and_then(Value::as_i64), Some(8080));

        let v2 = parse("a.b.c = 1\n").unwrap();
        assert_eq!(
            v2.get("a")
                .and_then(|t| t.get("b"))
                .and_then(|t| t.get("c")),
            Some(&Value::Int(1))
        );
    }

    #[test]
    fn indentation_is_ignored() {
        let src = "[a]\n    x = 1\n        y = 2\n";
        let v = parse(src).unwrap();
        let a = v.get("a").unwrap();
        assert_eq!(a.get("x").and_then(Value::as_i64), Some(1));
        assert_eq!(a.get("y").and_then(Value::as_i64), Some(2));
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("key").is_err()); // no '='
        assert!(parse("key =").is_err()); // no value
        assert!(parse("= 1").is_err()); // no key
        assert!(parse("a = [1, 2").is_err()); // unterminated array
        assert!(parse("a = \"oops").is_err()); // unterminated string
        assert!(parse("[unclosed\n").is_err()); // unterminated header
        assert!(parse("a = 1\na = 2\n").is_err()); // duplicate key
        assert!(parse("a = 1 2\n").is_err()); // trailing junk after value
        assert!(parse("a = 1__0\n").is_err()); // bad underscore
    }

    #[test]
    fn rejects_unsupported_features() {
        assert!(parse("s = 'literal'").is_err());
        assert!(parse("s = \"\"\"multi\"\"\"").is_err());
        assert!(parse("t = {a = 1}").is_err());
        assert!(parse("n = 0xFF").is_err());
    }

    #[test]
    fn never_panics_on_random_input() {
        for src in ["", "[[", "]]", "=", ".", "[.]", "a.=1", "[[a]\n", "\"\\q\""] {
            let _ = parse(src); // must return, not panic
        }
    }
}
