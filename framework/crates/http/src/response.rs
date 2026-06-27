//! Building and serializing HTTP/1.1 responses.
//!
//! A [`Response`] is a plain value (status + headers + body); turning it into
//! wire bytes is pure, so it tests without a socket. The server module is the
//! only place that actually writes these to a stream.

/// An HTTP response ready to be serialized.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// A response with the given status, its canonical reason phrase, and an
    /// empty body.
    pub fn new(status: u16) -> Response {
        Response {
            status,
            reason: reason_phrase(status).to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn ok() -> Response {
        Response::new(200)
    }
    pub fn not_found() -> Response {
        Response::new(404).with_text("404 Not Found")
    }
    pub fn bad_request() -> Response {
        Response::new(400).with_text("400 Bad Request")
    }

    /// Set a header, replacing any existing header with the same (ASCII
    /// case-insensitive) name.
    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Set the body and its `Content-Type`.
    pub fn with_body(mut self, content_type: &str, body: Vec<u8>) -> Response {
        self.body = body;
        self.with_header("Content-Type", content_type)
    }

    pub fn with_text(self, text: &str) -> Response {
        self.with_body("text/plain; charset=utf-8", text.as_bytes().to_vec())
    }
    pub fn with_html(self, html: &str) -> Response {
        self.with_body("text/html; charset=utf-8", html.as_bytes().to_vec())
    }

    /// Serialize the full response (status line + headers + body) to bytes.
    /// `Content-Length` is always emitted from the actual body length, so
    /// callers cannot desync it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.write_status_and_headers();
        out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", self.body.len()).as_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    /// Serialize only the status line and headers, ending at the blank line —
    /// no `Content-Length` and no body. This is what an upgraded connection
    /// (WebSocket `101`, SSE `text/event-stream`) writes before the hijack takes
    /// over the socket; the body is then streamed by the protocol itself.
    pub fn to_head_bytes(&self) -> Vec<u8> {
        let mut out = self.write_status_and_headers();
        out.extend_from_slice(b"\r\n");
        out
    }

    /// Status line + header lines, with NO terminating blank line. Both
    /// serializers build on this and add their own tail.
    fn write_status_and_headers(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.body.len());
        out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", self.status, self.reason).as_bytes());
        for (k, v) in &self.headers {
            if k.eq_ignore_ascii_case("content-length") {
                continue; // we own this header; ignore any caller-supplied one
            }
            out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        out
    }
}

/// Canonical reason phrases for the statuses we emit; unknown codes get a
/// generic phrase rather than failing.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_str(r: &Response) -> String {
        String::from_utf8(r.to_bytes()).unwrap()
    }

    #[test]
    fn serializes_status_line_and_content_length() {
        let r = Response::ok().with_text("hi");
        let s = as_str(&r);
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"), "got: {s:?}");
        assert!(s.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(s.contains("Content-Length: 2\r\n"));
        assert!(s.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn with_header_replaces_case_insensitively() {
        let r = Response::ok()
            .with_header("X-A", "1")
            .with_header("x-a", "2");
        assert_eq!(
            r.headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("x-a"))
                .count(),
            1
        );
        assert_eq!(r.header_value("X-A"), Some("2"));
    }

    #[test]
    fn caller_cannot_override_content_length() {
        let r = Response::ok()
            .with_header("Content-Length", "999")
            .with_text("abc");
        let s = as_str(&r);
        assert!(s.contains("Content-Length: 3\r\n"));
        assert!(!s.contains("999"));
    }

    #[test]
    fn head_bytes_omit_content_length_and_body() {
        let r = Response::new(101)
            .with_header("Upgrade", "websocket")
            .with_header("Connection", "Upgrade");
        let s = String::from_utf8(r.to_head_bytes()).unwrap();
        assert!(
            s.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
            "got: {s:?}"
        );
        assert!(s.contains("Upgrade: websocket\r\n"));
        assert!(!s.to_ascii_lowercase().contains("content-length"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn not_found_has_body_and_status() {
        let s = as_str(&Response::not_found());
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.ends_with("404 Not Found"));
    }

    impl Response {
        fn header_value(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }
}
