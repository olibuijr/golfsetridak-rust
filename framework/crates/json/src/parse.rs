//! Recursive-descent JSON parser.
//!
//! Operates over a `Vec<char>` for simple, correct indexing (the input is
//! already valid UTF-8). Speed work can come later; correctness first.

use crate::error::Error;
use crate::value::Value;

/// Parse a complete JSON document. Trailing non-whitespace is an error.
pub fn parse(input: &str) -> Result<Value, Error> {
    let mut p = Parser {
        chars: input.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(Error::new("trailing characters after JSON value", p.pos));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self) -> Result<Value, Error> {
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => Ok(Value::Str(self.parse_string()?)),
            Some('t') | Some('f') => self.parse_bool(),
            Some('n') => self.parse_null(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(Error::new(format!("unexpected character '{c}'"), self.pos)),
            None => Err(Error::new("unexpected end of input", self.pos)),
        }
    }

    fn expect_literal(&mut self, literal: &str, value: Value) -> Result<Value, Error> {
        let start = self.pos;
        for expected in literal.chars() {
            if self.bump() != Some(expected) {
                return Err(Error::new(format!("expected '{literal}'"), start));
            }
        }
        Ok(value)
    }

    fn parse_bool(&mut self) -> Result<Value, Error> {
        if self.peek() == Some('t') {
            self.expect_literal("true", Value::Bool(true))
        } else {
            self.expect_literal("false", Value::Bool(false))
        }
    }

    fn parse_null(&mut self) -> Result<Value, Error> {
        self.expect_literal("null", Value::Null)
    }

    fn parse_number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        let mut is_float = false;

        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }

        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| Error::new("invalid number", start))
        } else {
            // Integers that overflow i64 fall back to float rather than failing.
            match text.parse::<i64>() {
                Ok(n) => Ok(Value::Int(n)),
                Err(_) => text
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|_| Error::new("invalid number", start)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, Error> {
        let open = self.pos;
        self.bump(); // consume opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::new("unterminated string", open)),
                Some('"') => return Ok(s),
                Some('\\') => self.parse_escape(&mut s)?,
                Some(c) if (c as u32) < 0x20 => {
                    return Err(Error::new("control character in string", self.pos - 1));
                }
                Some(c) => s.push(c),
            }
        }
    }

    fn parse_escape(&mut self, s: &mut String) -> Result<(), Error> {
        let at = self.pos - 1;
        match self.bump() {
            Some('"') => s.push('"'),
            Some('\\') => s.push('\\'),
            Some('/') => s.push('/'),
            Some('n') => s.push('\n'),
            Some('t') => s.push('\t'),
            Some('r') => s.push('\r'),
            Some('b') => s.push('\u{08}'),
            Some('f') => s.push('\u{0c}'),
            Some('u') => self.parse_unicode_escape(s, at)?,
            _ => return Err(Error::new("invalid escape", at)),
        }
        Ok(())
    }

    fn parse_unicode_escape(&mut self, s: &mut String, at: usize) -> Result<(), Error> {
        let hi = self.read_hex4(at)?;
        // Surrogate pair: a high surrogate must be followed by `\uXXXX` low.
        if (0xD800..=0xDBFF).contains(&hi) {
            if self.bump() != Some('\\') || self.bump() != Some('u') {
                return Err(Error::new("expected low surrogate", at));
            }
            let lo = self.read_hex4(at)?;
            if !(0xDC00..=0xDFFF).contains(&lo) {
                return Err(Error::new("invalid low surrogate", at));
            }
            let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
            match char::from_u32(c) {
                Some(c) => s.push(c),
                None => return Err(Error::new("invalid surrogate pair", at)),
            }
        } else {
            match char::from_u32(hi) {
                Some(c) => s.push(c),
                None => return Err(Error::new("invalid unicode escape", at)),
            }
        }
        Ok(())
    }

    fn read_hex4(&mut self, at: usize) -> Result<u32, Error> {
        let mut value = 0u32;
        for _ in 0..4 {
            let c = self
                .bump()
                .ok_or_else(|| Error::new("truncated unicode escape", at))?;
            let digit = c
                .to_digit(16)
                .ok_or_else(|| Error::new("invalid hex digit", self.pos - 1))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_array(&mut self) -> Result<Value, Error> {
        self.bump(); // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some(']') => return Ok(Value::Array(items)),
                _ => return Err(Error::new("expected ',' or ']' in array", self.pos - 1)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, Error> {
        self.bump(); // '{'
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Value::Object(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(Error::new("expected string key in object", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.bump() != Some(':') {
                return Err(Error::new("expected ':' after object key", self.pos - 1));
            }
            self.skip_ws();
            let value = self.parse_value()?;
            pairs.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => return Ok(Value::Object(pairs)),
                _ => return Err(Error::new("expected ',' or '}' in object", self.pos - 1)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_primitives() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(parse("false").unwrap(), Value::Bool(false));
        assert_eq!(parse("  42 ").unwrap(), Value::Int(42));
        assert_eq!(parse("-7").unwrap(), Value::Int(-7));
        assert_eq!(parse("2.5").unwrap(), Value::Float(2.5));
        assert_eq!(parse("1e3").unwrap(), Value::Float(1000.0));
        assert_eq!(parse("\"hello\"").unwrap(), Value::Str("hello".into()));
    }

    #[test]
    fn integers_stay_integers() {
        // The classic JSON foot-gun: a big id must not become an inexact float.
        assert_eq!(
            parse("9007199254740993").unwrap(),
            Value::Int(9007199254740993)
        );
    }

    #[test]
    fn parses_string_escapes() {
        assert_eq!(
            parse(r#""a\"b\\c\n""#).unwrap(),
            Value::Str("a\"b\\c\n".into())
        );
        assert_eq!(parse(r#""Aé""#).unwrap(), Value::Str("Aé".into()));
    }

    #[test]
    fn parses_surrogate_pair() {
        // U+1F600 GRINNING FACE encoded as a UTF-16 surrogate pair.
        assert_eq!(parse(r#""😀""#).unwrap(), Value::Str("😀".into()));
    }

    #[test]
    fn parses_nested_structures() {
        let v = parse(r#"{ "a": [1, 2, {"b": null}], "c": true }"#).unwrap();
        assert_eq!(
            v.get("a").unwrap(),
            &Value::Array(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Object(vec![("b".into(), Value::Null)]),
            ])
        );
        assert_eq!(v.get("c"), Some(&Value::Bool(true)));
    }

    #[test]
    fn parses_empty_containers() {
        assert_eq!(parse("[]").unwrap(), Value::Array(vec![]));
        assert_eq!(parse("{}").unwrap(), Value::Object(vec![]));
    }

    #[test]
    fn round_trips() {
        let src = r#"{"name":"demo","n":42,"f":1.5,"ok":true,"xs":[1,2,3],"nil":null}"#;
        assert_eq!(parse(src).unwrap().to_json(), src);
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("nul").is_err());
        assert!(parse("1 2").is_err()); // trailing characters
        assert!(parse("\"unterminated").is_err());
        assert!(parse("{a:1}").is_err()); // unquoted key
    }
}
