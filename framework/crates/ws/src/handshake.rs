//! The RFC 6455 opening handshake: recognise a WebSocket upgrade request and
//! compute the `Sec-WebSocket-Accept` response token.
//!
//! Per §4.2.2, the server answers a client key by appending the protocol's
//! fixed GUID, taking SHA-1 of that ASCII string, and base64-encoding the
//! digest. The GUID is a constant defined by the spec — not a secret.

use crate::base64;
use crate::sha1::sha1;
use akurai_http::{Method, Request};

/// The "magic string" every RFC 6455 server appends to the client key (§1.3).
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The WebSocket version this implementation speaks. Clients send it in
/// `Sec-WebSocket-Version`; 13 is the only version standardised by RFC 6455.
pub const WS_VERSION: &str = "13";

/// Derive `Sec-WebSocket-Accept` from the client's `Sec-WebSocket-Key`.
pub fn accept_key(client_key: &str) -> String {
    let mut combined = String::with_capacity(client_key.len() + WS_GUID.len());
    combined.push_str(client_key);
    combined.push_str(WS_GUID);
    base64::encode(&sha1(combined.as_bytes()))
}

/// Is this request a well-formed WebSocket upgrade we can accept? Checks the
/// method, the `Upgrade`/`Connection` tokens, the version, and the key — all
/// case-insensitively where the spec allows.
pub fn is_websocket_upgrade(req: &Request) -> bool {
    req.method == Method::Get
        && header_eq(req, "Upgrade", "websocket")
        && header_has_token(req, "Connection", "upgrade")
        && req.header("Sec-WebSocket-Version") == Some(WS_VERSION)
        && client_key(req).is_some()
}

/// The client's `Sec-WebSocket-Key`, if present and non-empty.
pub fn client_key(req: &Request) -> Option<&str> {
    req.header("Sec-WebSocket-Key").filter(|k| !k.is_empty())
}

/// True when a header equals `want` ignoring ASCII case.
fn header_eq(req: &Request, name: &str, want: &str) -> bool {
    req.header(name)
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(want))
}

/// True when a comma-separated header list contains `token` (case-insensitive).
/// `Connection` is a list — e.g. `keep-alive, Upgrade` — so a plain equality
/// check would wrongly reject conforming clients.
fn header_has_token(req: &Request, name: &str, token: &str) -> bool {
    req.header(name)
        .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(raw: &str) -> Request {
        Request::parse_head(raw.as_bytes()).unwrap()
    }

    #[test]
    fn rfc6455_accept_vector() {
        // The exact example from RFC 6455 §1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn recognises_a_valid_upgrade() {
        let r = req("GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n");
        assert!(is_websocket_upgrade(&r));
        assert_eq!(client_key(&r), Some("dGhlIHNhbXBsZSBub25jZQ=="));
    }

    #[test]
    fn case_insensitive_upgrade_token() {
        let r = req("GET / HTTP/1.1\r\nUpgrade: WebSocket\r\nConnection: UPGRADE\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 13\r\n\r\n");
        assert!(is_websocket_upgrade(&r));
    }

    #[test]
    fn rejects_wrong_version() {
        let r = req("GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 8\r\n\r\n");
        assert!(!is_websocket_upgrade(&r));
    }

    #[test]
    fn rejects_post_and_missing_key() {
        let no_key = req("GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\r\n");
        assert!(!is_websocket_upgrade(&no_key));
        let post = req("POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 13\r\n\r\n");
        assert!(!is_websocket_upgrade(&post));
    }
}
