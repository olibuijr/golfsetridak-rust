//! The crate's error type. Bad input always returns `Err`; nothing here panics.

use std::fmt;
use std::io;

use akurai_json::Error as JsonError;

/// Anything that can go wrong while validating, storing, or reading a record.
#[derive(Debug)]
pub enum CollError {
    /// A schema validation failure (missing required field, wrong type,
    /// unknown field, or a reserved key in the input).
    Validation(String),
    /// A referenced record id does not exist.
    NotFound,
    /// A collection name was used that is not in the known set.
    UnknownCollection(String),
    /// An I/O error bubbled up from the storage layer.
    Io(io::Error),
    /// A stored record could not be parsed back into JSON, or the input was
    /// not valid JSON where JSON was expected.
    Json(JsonError),
    /// Persisted bytes were structurally wrong (e.g. a non-object record, or a
    /// malformed counter). Indicates corruption, not bad user input.
    Corrupt(&'static str),
}

impl fmt::Display for CollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CollError::Validation(m) => write!(f, "validation error: {m}"),
            CollError::NotFound => write!(f, "record not found"),
            CollError::UnknownCollection(c) => write!(f, "unknown collection: {c}"),
            CollError::Io(e) => write!(f, "io error: {e}"),
            CollError::Json(e) => write!(f, "json error: {e}"),
            CollError::Corrupt(what) => write!(f, "corrupt persisted data: {what}"),
        }
    }
}

impl std::error::Error for CollError {}

impl From<io::Error> for CollError {
    fn from(e: io::Error) -> Self {
        CollError::Io(e)
    }
}

impl From<JsonError> for CollError {
    fn from(e: JsonError) -> Self {
        CollError::Json(e)
    }
}

impl CollError {
    /// Shorthand for a [`CollError::Validation`] from any displayable message.
    pub(crate) fn validation(msg: impl Into<String>) -> CollError {
        CollError::Validation(msg.into())
    }
}
