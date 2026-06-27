//! Parsing the HTTP/1.1 request head: request line + headers.
//!
//! Scope is deliberately narrow — this module turns the bytes *up to and
//! including* the blank line that ends the headers into a [`Request`]. Body
//! reading, streaming, and limits live elsewhere; keeping the parser pure
//! (bytes in, value out, no I/O) makes it trivially testable.

use std::fmt;

/// An HTTP request method. Common verbs are named; anything else is preserved
/// verbatim in [`Method::Other`] so we never reject a valid-but-unusual method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Other(String),
}

impl Method {
    fn parse(token: &str) -> Method {
        match token {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "HEAD" => Method::Head,
            "OPTIONS" => Method::Options,
            other => Method::Other(other.to_string()),
        }
    }
}

/// A parsed request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// Path portion of the target, percent-encoding left intact (decoding is a
    /// later concern for the router).
    pub path: String,
    /// Raw query string after `?`, if present (without the `?`).
    pub query: Option<String>,
    /// e.g. `"HTTP/1.1"`.
    pub version: String,
    /// Header name/value pairs, in wire order. Names are stored as received;
    /// use [`Request::header`] for case-insensitive lookup.
    pub headers: Vec<(String, String)>,
    /// The request body. Empty for `GET` and for any request without a
    /// `Content-Length`; the server reads it after the head and fills it in.
    pub body: Vec<u8>,
}

impl Request {
    /// Case-insensitive header lookup, returning the first match.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The declared body length from `Content-Length`, if present and valid.
    pub fn content_length(&self) -> Option<usize> {
        self.header("Content-Length")?.trim().parse().ok()
    }

    /// The body as text (lossy UTF-8) — convenient for form and JSON bodies.
    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// Parse a request head from raw bytes (request line + headers, terminated
    /// by a blank line). Bytes must be valid UTF-8 for now; binary header
    /// values are out of scope until we need them.
    pub fn parse_head(bytes: &[u8]) -> Result<Request, ParseError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;

        // Split into lines on CRLF (tolerating bare LF), dropping the trailing
        // blank line that ends the head.
        let mut lines = text.split("\r\n").flat_map(|l| l.split('\n'));

        let request_line = lines
            .next()
            .filter(|l| !l.is_empty())
            .ok_or(ParseError::Empty)?;
        let (method, path, query, version) = parse_request_line(request_line)?;

        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                break; // blank line ends the head
            }
            let (name, value) = line.split_once(':').ok_or(ParseError::MalformedHeader)?;
            let name = name.trim();
            if name.is_empty() {
                return Err(ParseError::MalformedHeader);
            }
            headers.push((name.to_string(), value.trim().to_string()));
        }

        Ok(Request {
            method,
            path,
            query,
            version,
            headers,
            body: Vec::new(),
        })
    }
}

fn parse_request_line(line: &str) -> Result<(Method, String, Option<String>, String), ParseError> {
    let mut parts = line.splitn(3, ' ');
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::MalformedRequestLine)?;
    let target = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::MalformedRequestLine)?;
    let version = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::MalformedRequestLine)?;

    if !version.starts_with("HTTP/") {
        return Err(ParseError::MalformedRequestLine);
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (target.to_string(), None),
    };

    Ok((Method::parse(method), path, query, version.to_string()))
}

/// Why a request head failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    NotUtf8,
    MalformedRequestLine,
    MalformedHeader,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            ParseError::Empty => "empty request",
            ParseError::NotUtf8 => "request head was not valid UTF-8",
            ParseError::MalformedRequestLine => "malformed request line",
            ParseError::MalformedHeader => "malformed header line",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Request, ParseError> {
        Request::parse_head(s.as_bytes())
    }

    #[test]
    fn parses_a_simple_get() {
        let req = parse("GET /hello HTTP/1.1\r\nHost: example.com\r\n\r\n").unwrap();
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.path, "/hello");
        assert_eq!(req.query, None);
        assert_eq!(req.version, "HTTP/1.1");
        assert_eq!(req.header("host"), Some("example.com"));
    }

    #[test]
    fn splits_path_and_query() {
        let req = parse("GET /search?q=rust&page=2 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(req.path, "/search");
        assert_eq!(req.query.as_deref(), Some("q=rust&page=2"));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let req = parse("GET / HTTP/1.1\r\nContent-Type: application/json\r\n\r\n").unwrap();
        assert_eq!(req.header("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(req.header("content-type"), Some("application/json"));
        assert_eq!(req.header("missing"), None);
    }

    #[test]
    fn trims_header_whitespace() {
        let req = parse("GET / HTTP/1.1\r\nX-Trim:    spaced   \r\n\r\n").unwrap();
        assert_eq!(req.header("x-trim"), Some("spaced"));
    }

    #[test]
    fn preserves_unknown_methods() {
        let req = parse("PROPFIND / HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(req.method, Method::Other("PROPFIND".to_string()));
    }

    #[test]
    fn recognizes_common_verbs() {
        for (s, m) in [
            ("POST", Method::Post),
            ("PUT", Method::Put),
            ("PATCH", Method::Patch),
            ("DELETE", Method::Delete),
        ] {
            let req = parse(&format!("{s} / HTTP/1.1\r\n\r\n")).unwrap();
            assert_eq!(req.method, m);
        }
    }

    #[test]
    fn tolerates_bare_lf_line_endings() {
        let req = parse("GET / HTTP/1.1\nHost: x\n\n").unwrap();
        assert_eq!(req.header("host"), Some("x"));
    }

    #[test]
    fn empty_input_is_an_error() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse("\r\n"), Err(ParseError::Empty));
    }

    #[test]
    fn request_line_needs_three_parts() {
        assert_eq!(
            parse("GET /\r\n\r\n"),
            Err(ParseError::MalformedRequestLine)
        );
        assert_eq!(parse("GET\r\n\r\n"), Err(ParseError::MalformedRequestLine));
    }

    #[test]
    fn rejects_non_http_version() {
        assert_eq!(
            parse("GET / FTP/1.1\r\n\r\n"),
            Err(ParseError::MalformedRequestLine)
        );
    }

    #[test]
    fn rejects_header_without_colon() {
        assert_eq!(
            parse("GET / HTTP/1.1\r\nBadHeader\r\n\r\n"),
            Err(ParseError::MalformedHeader)
        );
    }

    #[test]
    fn rejects_non_utf8() {
        assert_eq!(Request::parse_head(&[0xff, 0xfe]), Err(ParseError::NotUtf8));
    }
}
