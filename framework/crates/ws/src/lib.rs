//! AkurAI-Framework WebSockets — RFC 6455, pure `std`, zero runtime deps.
//!
//! A blocking, server-side WebSocket built on the framework's own HTTP core. It
//! deliberately implements the standard and nothing more: the §4 opening
//! handshake, §5 base framing with required client-frame masking, fragmentation
//! reassembly, automatic ping/pong, and the §5.5.1 close handshake. No
//! extensions (`permessage-deflate`), no async runtime — the connection owns one
//! thread for its life, handed off by the HTTP server so it never pins a worker.
//!
//! Conceptually modelled on the well-trodden design of `tungstenite-rs` (the
//! de-facto Rust RFC 6455 codec), reduced to a blocking, dependency-free core.
//!
//! ```ignore
//! use akurai_ws::{upgrade, Message};
//!
//! // inside a handler returning `akurai_http::Reply`:
//! upgrade(req, |mut conn| {
//!     while let Some(msg) = conn.recv()? {
//!         if let Message::Text(t) = msg {
//!             conn.send_text(&t)?; // echo
//!         }
//!     }
//!     Ok(())
//! })
//! ```

#![forbid(unsafe_code)]

pub mod base64;
pub mod conn;
pub mod frame;
pub mod handshake;
pub mod message;
pub mod sha1;

pub use conn::WsConn;
pub use frame::Opcode;
pub use message::{close, CloseFrame, Message};

use akurai_http::{Reply, Request, Response};
use std::io;

/// Attempt to upgrade `req` to a WebSocket. On success returns a `101` reply
/// whose hijack runs `on_conn` against the live [`WsConn`]; on a non-WebSocket
/// or malformed request returns a `400` response instead.
///
/// `on_conn` runs on the dedicated connection thread; returning from it (or any
/// error) ends the connection.
pub fn upgrade<F>(req: &Request, on_conn: F) -> Reply
where
    F: FnOnce(WsConn) -> io::Result<()> + Send + 'static,
{
    if !handshake::is_websocket_upgrade(req) {
        return Reply::Response(
            Response::new(400).with_text("expected a WebSocket upgrade request"),
        );
    }

    // Safe to unwrap: is_websocket_upgrade already verified a non-empty key.
    let accept = handshake::accept_key(handshake::client_key(req).unwrap());
    let head = Response::new(101)
        .with_header("Upgrade", "websocket")
        .with_header("Connection", "Upgrade")
        .with_header("Sec-WebSocket-Accept", &accept);

    Reply::upgrade(head, move |stream| on_conn(WsConn::new(stream)?))
}
