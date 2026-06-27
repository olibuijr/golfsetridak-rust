//! Parse errors, carrying the 1-based line number where they occurred.

use std::fmt;

/// A TOML parse failure: a human-readable message plus the 1-based line in the
/// input where parsing gave up. The parser never panics on malformed input — it
/// always returns one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomlError {
    pub message: String,
    pub line: usize,
}

impl TomlError {
    pub(crate) fn new(message: impl Into<String>, line: usize) -> TomlError {
        TomlError {
            message: message.into(),
            line,
        }
    }
}

impl fmt::Display for TomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at line {}", self.message, self.line)
    }
}

impl std::error::Error for TomlError {}
