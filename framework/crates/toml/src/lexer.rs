//! Low-level scanning: a character [`Cursor`] over the input plus the scalar and
//! key readers. Structural parsing (headers, arrays, table nesting) lives in
//! `parser.rs`; this module only turns runs of characters into keys and scalar
//! [`Value`]s, tracking the line number for error reporting.

use crate::error::TomlError;
use crate::value::Value;

/// A cursor over the input characters. Operates on a `Vec<char>` for simple,
/// correct indexing (the input is already valid UTF-8). Tracks the 1-based line
/// so every error points somewhere useful.
pub(crate) struct Cursor {
    chars: Vec<char>,
    pos: usize,
    line: usize,
}

impl Cursor {
    pub fn new(input: &str) -> Cursor {
        Cursor {
            chars: input.chars().collect(),
            pos: 0,
            line: 1,
        }
    }

    pub fn line(&self) -> usize {
        self.line
    }

    pub fn at_eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    pub fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    pub fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    pub fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
            }
        }
        c
    }

    /// Skip spaces and tabs (TOML ignores indentation). Stops at a newline.
    pub fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.pos += 1;
        }
    }

    /// If the cursor is on a `#`, consume the rest of the line up to (but not
    /// including) the newline.
    pub fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.pos += 1;
            }
        }
    }

    /// Skip everything that separates statements: inline whitespace, comments,
    /// and newlines, repeatedly. Used both at top level and inside arrays.
    pub fn skip_blank(&mut self) {
        loop {
            self.skip_inline_ws();
            match self.peek() {
                Some('#') => self.skip_comment(),
                Some('\n') => {
                    self.bump();
                }
                _ => break,
            }
        }
    }

    /// Read a bare key: one or more of `A-Za-z0-9_-`.
    pub fn read_bare_key(&mut self) -> Result<String, TomlError> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                s.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        if s.is_empty() {
            return Err(TomlError::new("expected a bare key", self.line));
        }
        Ok(s)
    }

    /// Read a single scalar value (string, bool, integer, or float). Arrays are
    /// handled by the parser; this rejects `[`. Unsupported forms (literal
    /// strings, inline tables, multi-line strings) error cleanly.
    pub fn read_scalar(&mut self) -> Result<Value, TomlError> {
        self.skip_inline_ws();
        match self.peek() {
            None => Err(TomlError::new("expected a value", self.line)),
            Some('"') => {
                if self.peek_at(1) == Some('"') && self.peek_at(2) == Some('"') {
                    return Err(TomlError::new(
                        "multi-line basic strings are not supported",
                        self.line,
                    ));
                }
                Ok(Value::Str(self.read_basic_string()?))
            }
            Some('\'') => Err(TomlError::new(
                "literal strings ('...') are not supported",
                self.line,
            )),
            Some('{') => Err(TomlError::new(
                "inline tables ({...}) are not supported",
                self.line,
            )),
            Some('[') => Err(TomlError::new(
                "unexpected '[' (expected a scalar)",
                self.line,
            )),
            _ => self.read_number_or_bool(),
        }
    }

    /// Read a `"..."` basic string, applying the supported escapes.
    pub fn read_basic_string(&mut self) -> Result<String, TomlError> {
        let open_line = self.line;
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(TomlError::new("unterminated string", open_line)),
                Some('"') => return Ok(s),
                Some('\\') => self.read_escape(&mut s, open_line)?,
                Some('\n') => {
                    return Err(TomlError::new("newline inside basic string", open_line));
                }
                Some(c) if (c as u32) < 0x20 => {
                    return Err(TomlError::new("control character in string", self.line));
                }
                Some(c) => s.push(c),
            }
        }
    }

    fn read_escape(&mut self, s: &mut String, open_line: usize) -> Result<(), TomlError> {
        match self.bump() {
            Some('"') => s.push('"'),
            Some('\\') => s.push('\\'),
            Some('n') => s.push('\n'),
            Some('t') => s.push('\t'),
            Some(other) => {
                return Err(TomlError::new(
                    format!("unsupported escape '\\{other}'"),
                    self.line,
                ));
            }
            None => return Err(TomlError::new("unterminated string", open_line)),
        }
        Ok(())
    }

    /// Read an un-quoted token (everything up to a delimiter) and classify it as
    /// a boolean or a number.
    fn read_number_or_bool(&mut self) -> Result<Value, TomlError> {
        let line = self.line;
        let mut raw = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, ',' | ']' | '}' | '#') {
                break;
            }
            raw.push(c);
            self.pos += 1;
        }
        if raw.is_empty() {
            return Err(TomlError::new("expected a value", line));
        }
        match raw.as_str() {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            _ => {}
        }
        parse_number(&raw, line)
    }
}

/// Classify and parse a numeric token. Rejects unsupported non-decimal and
/// special-float forms.
fn parse_number(raw: &str, line: usize) -> Result<Value, TomlError> {
    let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    if body.starts_with("0x") || body.starts_with("0o") || body.starts_with("0b") {
        return Err(TomlError::new(
            "non-decimal integers (0x/0o/0b) are not supported",
            line,
        ));
    }
    if matches!(body, "inf" | "nan") {
        return Err(TomlError::new("inf/nan floats are not supported", line));
    }

    let cleaned = strip_underscores(raw, line)?;
    let is_float = cleaned.contains(['.', 'e', 'E']);
    if is_float {
        cleaned
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| TomlError::new(format!("invalid float '{raw}'"), line))
    } else {
        cleaned
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| TomlError::new(format!("invalid integer '{raw}'"), line))
    }
}

/// Remove `_` digit separators, requiring each to sit between two ASCII digits
/// (TOML's rule). Any stray underscore is an error.
fn strip_underscores(raw: &str, line: usize) -> Result<String, TomlError> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
            if !(prev_digit && next_digit) {
                return Err(TomlError::new(
                    format!("misplaced '_' in number '{raw}'"),
                    line,
                ));
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}
