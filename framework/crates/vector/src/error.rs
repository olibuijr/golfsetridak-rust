//! Errors for the embedding client.
//!
//! Every failure mode of the plain-HTTP round-trip maps to a distinct,
//! non-panicking variant so callers (the CLI) can react precisely — and so the
//! `?search` path can degrade to substring search instead of crashing.

use std::fmt;

/// A failure while fetching embeddings from an OpenAI-compatible endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedError {
    /// The endpoint string couldn't be parsed into `host[:port]`.
    InvalidEndpoint(String),
    /// An `https://` URL was supplied. std has no TLS; terminate TLS at the
    /// edge (Caddy/nginx) and point this client at the plain-HTTP origin.
    TlsUnsupported(String),
    /// TCP connect failed (e.g. connection refused, DNS failure, host down).
    Connect(String),
    /// The socket read timed out before a full response arrived.
    Timeout,
    /// A lower-level I/O error during send/receive.
    Io(String),
    /// The server replied with a non-2xx status. Carries the code and a short
    /// snippet of the body for diagnosis.
    HttpStatus { code: u16, snippet: String },
    /// The response was missing, truncated, or had no header/body separator.
    ShortResponse,
    /// The body was not valid JSON.
    MalformedJson(String),
    /// The JSON parsed but didn't match `{"data":[{"embedding":[...]}, ...]}`.
    UnexpectedShape(String),
    /// `embed_many` asked for N inputs but the server returned a different
    /// count of embeddings.
    CountMismatch { expected: usize, got: usize },
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmbedError::InvalidEndpoint(s) => write!(f, "invalid endpoint: {s}"),
            EmbedError::TlsUnsupported(s) => write!(
                f,
                "TLS is not supported (std has no TLS); use a plain http:// origin, not {s}"
            ),
            EmbedError::Connect(s) => write!(f, "could not connect: {s}"),
            EmbedError::Timeout => write!(f, "read timed out"),
            EmbedError::Io(s) => write!(f, "I/O error: {s}"),
            EmbedError::HttpStatus { code, snippet } => {
                write!(f, "embedding endpoint returned HTTP {code}: {snippet}")
            }
            EmbedError::ShortResponse => write!(f, "empty or truncated HTTP response"),
            EmbedError::MalformedJson(s) => write!(f, "malformed JSON response: {s}"),
            EmbedError::UnexpectedShape(s) => write!(f, "unexpected response shape: {s}"),
            EmbedError::CountMismatch { expected, got } => {
                write!(f, "expected {expected} embeddings, got {got}")
            }
        }
    }
}

impl std::error::Error for EmbedError {}
