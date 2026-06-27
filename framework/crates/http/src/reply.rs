//! What a handler returns: a finished [`Response`], or an [`Upgrade`] that hands
//! the raw connection to a closure once the head is written.
//!
//! This is the seam that lets protocols *built on* HTTP — WebSocket (a `101`
//! switch) and Server-Sent Events (a long-lived `text/event-stream` body) — live
//! outside this crate without `Response` having to know about sockets. A normal
//! handler keeps returning `Response`; it becomes a [`Reply`] for free via
//! `From`. Only the handful that hijack the stream build an [`Upgrade`].

use crate::Response;
use std::io;
use std::net::TcpStream;

/// A closure that takes ownership of a connection after its response head has
/// been written, and drives it for the rest of its life (frames, events, …).
pub type Hijack = Box<dyn FnOnce(TcpStream) -> io::Result<()> + Send>;

/// A response head to write, paired with the closure that then owns the socket.
pub struct Upgrade {
    /// Status line + headers to send before handing over (e.g. `101` for
    /// WebSocket, `200` + `text/event-stream` for SSE). The body is *not* sent
    /// and no `Content-Length` is emitted — the hijack owns all bytes after the
    /// blank line.
    pub head: Response,
    /// Takes the stream once the head is flushed.
    pub hijack: Hijack,
}

/// A handler's answer.
pub enum Reply {
    /// An ordinary buffered response.
    Response(Response),
    /// Take over the connection after writing a head.
    Upgrade(Upgrade),
}

impl Reply {
    /// Build an upgrade reply from a head and a hijack closure.
    pub fn upgrade<F>(head: Response, hijack: F) -> Reply
    where
        F: FnOnce(TcpStream) -> io::Result<()> + Send + 'static,
    {
        Reply::Upgrade(Upgrade {
            head,
            hijack: Box::new(hijack),
        })
    }
}

impl From<Response> for Reply {
    fn from(response: Response) -> Reply {
        Reply::Response(response)
    }
}
