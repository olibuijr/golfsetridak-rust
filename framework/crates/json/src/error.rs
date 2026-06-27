//! Parse errors, with the character offset where they occurred.

use std::fmt;

/// A JSON parse failure: a human-readable message plus the 0-based character
/// position in the input where parsing gave up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub position: usize,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>, position: usize) -> Error {
        Error {
            message: message.into(),
            position,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at position {}", self.message, self.position)
    }
}

impl std::error::Error for Error {}
