//! A minimal HTTP/1.1 client over `std::net::TcpStream`, plain HTTP only.
//!
//! TLS terminates at the edge (AGENTS.md #2); std has no TLS and we will not
//! pull a crate for it. An `https://` endpoint is rejected with a clear error.
//!
//! The **pure** parts — endpoint parsing, request building, response splitting
//! — are factored out and unit-tested against hand-written strings. The actual
//! socket round-trip ([`fetch`]) is not unit-tested (tests never touch the
//! network).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::client::EmbedInput;
use crate::error::EmbedError;

/// Read/connect timeout for the embedding round-trip. Generous enough for a
/// cold local model, short enough that the CLI fails fast to substring search.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A parsed plain-HTTP endpoint: `host` and `port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// Parse an endpoint like `http://host:port` (or `http://host`, defaulting to
/// port 80). A trailing path is ignored — we always POST to `/v1/embeddings`.
///
/// `https://` is rejected with [`EmbedError::TlsUnsupported`].
pub fn parse_endpoint(endpoint: &str) -> Result<Endpoint, EmbedError> {
    let trimmed = endpoint.trim();
    if let Some(rest) = strip_ci_prefix(trimmed, "https://") {
        let _ = rest;
        return Err(EmbedError::TlsUnsupported(trimmed.to_string()));
    }
    let rest = strip_ci_prefix(trimmed, "http://").ok_or_else(|| {
        EmbedError::InvalidEndpoint(format!("must start with http://: {trimmed}"))
    })?;

    // Drop any path/query/fragment; keep only the authority (host[:port]).
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if authority.is_empty() {
        return Err(EmbedError::InvalidEndpoint(format!(
            "missing host: {trimmed}"
        )));
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| EmbedError::InvalidEndpoint(format!("bad port: {p}")))?;
            (h, port)
        }
        None => (authority, 80u16),
    };
    if host.is_empty() {
        return Err(EmbedError::InvalidEndpoint(format!(
            "missing host: {trimmed}"
        )));
    }
    Ok(Endpoint {
        host: host.to_string(),
        port,
    })
}

/// Case-insensitively strip a known ASCII scheme prefix.
fn strip_ci_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// Build a complete HTTP/1.1 request string for an OpenAI-compatible
/// `POST /v1/embeddings`, with the JSON body
/// `{"model": <model>, "input": <text|[texts]>}`.
///
/// `Content-Length` is the exact UTF-8 byte length of the body, and the
/// connection is closed after the response (`Connection: close`) so the client
/// can read until EOF.
pub fn build_request(host: &str, port: u16, model: &str, input: &EmbedInput) -> String {
    build_request_with_headers(host, port, model, input, &[])
}

/// Build a request with additional HTTP headers. Header names and values with
/// CR/LF are skipped so env-sourced tokens cannot inject extra headers.
pub fn build_request_with_headers(
    host: &str,
    port: u16,
    model: &str,
    input: &EmbedInput,
    extra_headers: &[(&str, &str)],
) -> String {
    let body = input.to_json_body(model);
    let host_header = if port == 80 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    let mut header_block = String::new();
    for (name, value) in extra_headers {
        if safe_header_name(name) && safe_header_value(value) {
            header_block.push_str(name);
            header_block.push_str(": ");
            header_block.push_str(value);
            header_block.push_str("\r\n");
        }
    }
    format!(
        "POST /v1/embeddings HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json\r\n\
         {extra_headers}\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        host_header = host_header,
        extra_headers = header_block,
        len = body.len(),
        body = body,
    )
}

fn safe_header_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn safe_header_value(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n')
}

/// Split a raw HTTP response into `(status_code, body)`.
///
/// Returns [`EmbedError::ShortResponse`] if there's no header/body separator or
/// the status line is unparseable, and [`EmbedError::HttpStatus`] for any
/// non-2xx code (carrying a short body snippet for diagnosis).
pub fn split_response(raw: &str) -> Result<&str, EmbedError> {
    let sep = raw
        .find("\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| raw.find("\n\n").map(|i| (i, 2)));
    let (head_end, sep_len) = sep.ok_or(EmbedError::ShortResponse)?;
    let head = &raw[..head_end];
    let body = &raw[head_end + sep_len..];

    // Status line: "HTTP/1.1 200 OK"
    let status_line = head.lines().next().ok_or(EmbedError::ShortResponse)?;
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or(EmbedError::ShortResponse)?;

    if !(200..300).contains(&code) {
        let snippet: String = body.chars().take(200).collect();
        return Err(EmbedError::HttpStatus { code, snippet });
    }
    Ok(body)
}

/// Connect, send `request`, read the full response to EOF, and return the body.
///
/// Not exercised by unit tests (no network); the pure helpers above are.
pub fn fetch(endpoint: &Endpoint, request: &str) -> Result<String, EmbedError> {
    let addr = format!("{}:{}", endpoint.host, endpoint.port);
    let mut stream = TcpStream::connect(&addr).map_err(|e| EmbedError::Connect(e.to_string()))?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|e| EmbedError::Io(e.to_string()))?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|e| EmbedError::Io(e.to_string()))?;

    stream.write_all(request.as_bytes()).map_err(map_io)?;
    stream.flush().map_err(map_io)?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(map_io)?;
    if raw.is_empty() {
        return Err(EmbedError::ShortResponse);
    }
    let text = String::from_utf8(raw)
        .map_err(|_| EmbedError::MalformedJson("response was not valid UTF-8".into()))?;
    split_response(&text).map(|body| body.to_string())
}

/// Map an I/O error to a timeout or generic I/O variant.
fn map_io(e: std::io::Error) -> EmbedError {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => EmbedError::Timeout,
        _ => EmbedError::Io(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_and_port() {
        let e = parse_endpoint("http://192.168.8.14:8081").unwrap();
        assert_eq!(e.host, "192.168.8.14");
        assert_eq!(e.port, 8081);
    }

    #[test]
    fn defaults_port_80() {
        let e = parse_endpoint("http://localhost").unwrap();
        assert_eq!(e.host, "localhost");
        assert_eq!(e.port, 80);
    }

    #[test]
    fn ignores_trailing_path() {
        let e = parse_endpoint("http://host:9000/v1/embeddings").unwrap();
        assert_eq!(e.host, "host");
        assert_eq!(e.port, 9000);
    }

    #[test]
    fn rejects_https() {
        assert_eq!(
            parse_endpoint("https://host:443"),
            Err(EmbedError::TlsUnsupported("https://host:443".into()))
        );
    }

    #[test]
    fn rejects_unknown_scheme_and_empty_host() {
        assert!(matches!(
            parse_endpoint("ftp://host"),
            Err(EmbedError::InvalidEndpoint(_))
        ));
        assert!(matches!(
            parse_endpoint("http://"),
            Err(EmbedError::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn build_request_is_well_formed_single() {
        let input = EmbedInput::Single("hello".into());
        let req = build_request("titan", 8081, "embeddinggemma", &input);
        let body = r#"{"model":"embeddinggemma","input":"hello"}"#;
        assert!(req.starts_with("POST /v1/embeddings HTTP/1.1\r\n"));
        assert!(req.contains("Host: titan:8081\r\n"));
        assert!(req.contains("Content-Type: application/json\r\n"));
        assert!(req.contains("Connection: close\r\n"));
        assert!(req.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(req.ends_with(&format!("\r\n\r\n{body}")));
    }

    #[test]
    fn build_request_host_header_omits_port_80() {
        let input = EmbedInput::Single("x".into());
        let req = build_request("localhost", 80, "m", &input);
        assert!(req.contains("Host: localhost\r\n"));
    }

    #[test]
    fn build_request_accepts_safe_extra_headers() {
        let input = EmbedInput::Single("x".into());
        let req = build_request_with_headers(
            "localhost",
            80,
            "m",
            &input,
            &[("Authorization", "Bearer akr_test")],
        );
        assert!(req.contains("Authorization: Bearer akr_test\r\n"));
    }

    #[test]
    fn build_request_skips_unsafe_extra_headers() {
        let input = EmbedInput::Single("x".into());
        let req = build_request_with_headers(
            "localhost",
            80,
            "m",
            &input,
            &[
                ("Bad\r\nName", "ok"),
                ("Authorization", "Bearer x\r\nBad: y"),
            ],
        );
        assert!(!req.contains("Bad"));
        assert!(!req.contains("Authorization"));
    }

    #[test]
    fn build_request_batch_body() {
        let input = EmbedInput::Many(vec!["a".into(), "b".into()]);
        let req = build_request("h", 8080, "m", &input);
        let body = r#"{"model":"m","input":["a","b"]}"#;
        assert!(req.ends_with(&format!("\r\n\r\n{body}")));
        assert!(req.contains(&format!("Content-Length: {}\r\n", body.len())));
    }

    #[test]
    fn split_response_extracts_2xx_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"data\":[]}";
        assert_eq!(split_response(raw).unwrap(), "{\"data\":[]}");
    }

    #[test]
    fn split_response_accepts_lf_only_separator() {
        let raw = "HTTP/1.1 201 Created\nX: y\n\nBODY";
        assert_eq!(split_response(raw).unwrap(), "BODY");
    }

    #[test]
    fn split_response_flags_non_2xx() {
        let raw = "HTTP/1.1 500 Internal Server Error\r\n\r\noops";
        assert_eq!(
            split_response(raw),
            Err(EmbedError::HttpStatus {
                code: 500,
                snippet: "oops".into()
            })
        );
    }

    #[test]
    fn split_response_rejects_garbage() {
        assert_eq!(
            split_response("no separator here"),
            Err(EmbedError::ShortResponse)
        );
        assert_eq!(
            split_response("GARBAGE LINE\r\n\r\nbody"),
            Err(EmbedError::ShortResponse)
        );
    }
}
