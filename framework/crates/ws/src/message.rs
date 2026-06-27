//! The application-level view of a WebSocket: whole messages and close frames,
//! plus the RFC 6455 §7.4.1 close codes.

/// A complete WebSocket message handed to (or sent by) the application. Data
/// fragmentation is reassembled before a `Text`/`Binary` is produced, so callers
/// never see continuation frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A UTF-8 text message (validity is enforced on receipt).
    Text(String),
    /// A binary message.
    Binary(Vec<u8>),
    /// A ping; the connection answers it with a pong automatically, but the
    /// application still sees it.
    Ping(Vec<u8>),
    /// A pong (an answer to a ping we sent, or unsolicited).
    Pong(Vec<u8>),
    /// The peer is closing, optionally with a code and reason.
    Close(Option<CloseFrame>),
}

/// The body of a close frame: a status code and a UTF-8 reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    pub code: u16,
    pub reason: String,
}

impl CloseFrame {
    pub fn new(code: u16, reason: &str) -> CloseFrame {
        CloseFrame {
            code,
            reason: reason.to_string(),
        }
    }
}

/// Standard close codes (RFC 6455 §7.4.1). Codes 1005/1006 are reserved for
/// local use and never appear on the wire, so they are intentionally absent.
pub mod close {
    /// Normal closure; the purpose was fulfilled.
    pub const NORMAL: u16 = 1000;
    /// An endpoint is going away (server shutdown, page navigation).
    pub const GOING_AWAY: u16 = 1001;
    /// A protocol error was detected.
    pub const PROTOCOL_ERROR: u16 = 1002;
    /// A data type the endpoint cannot accept was received.
    pub const UNSUPPORTED: u16 = 1003;
    /// A message was not consistent with its type (e.g. invalid UTF-8 in text).
    pub const INVALID_PAYLOAD: u16 = 1007;
    /// A message violated policy.
    pub const POLICY: u16 = 1008;
    /// A message was too big to process.
    pub const TOO_BIG: u16 = 1009;
    /// An unexpected condition prevented fulfilling the request.
    pub const INTERNAL_ERROR: u16 = 1011;
}
