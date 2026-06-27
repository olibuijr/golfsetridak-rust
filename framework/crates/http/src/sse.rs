//! Server-Sent Events — a one-way `text/event-stream` over a hijacked
//! connection, per the WHATWG HTML "server-sent events" spec.
//!
//! A handler calls [`sse`] with a closure; the framework writes the
//! `text/event-stream` head, then hands the closure an [`SseSink`] it writes
//! events to for as long as the client stays connected. When the client goes
//! away the next write fails and the closure returns — the dedicated upgrade
//! thread then ends. No framing library, no async: just bytes to a socket.
//!
//! Wire format (each event ends with a blank line):
//! ```text
//! event: tick
//! id: 7
//! data: {"n":1}
//! <blank line>
//! ```
//! Multi-line `data` is emitted as one `data:` line per source line, which the
//! browser rejoins with `\n`. Lines beginning with `:` are comments — used here
//! for keep-alive heartbeats.

use crate::{Reply, Response};
use std::io::{self, Write};
use std::net::TcpStream;

/// Build an SSE [`Reply`]: a `200 text/event-stream` head plus a hijack that
/// runs `body` against the live connection.
pub fn sse<F>(body: F) -> Reply
where
    F: FnOnce(SseSink) -> io::Result<()> + Send + 'static,
{
    let head = Response::new(200)
        .with_header("Content-Type", "text/event-stream; charset=utf-8")
        .with_header("Cache-Control", "no-cache")
        .with_header("Connection", "keep-alive")
        // Defeat proxy buffering so events are delivered as they happen.
        .with_header("X-Accel-Buffering", "no");
    Reply::upgrade(head, move |stream| body(SseSink::new(stream)))
}

/// The writable end of an SSE stream. Every method flushes, so an event reaches
/// the client immediately rather than sitting in a buffer.
pub struct SseSink {
    stream: TcpStream,
}

impl SseSink {
    fn new(stream: TcpStream) -> SseSink {
        SseSink { stream }
    }

    /// Send a `data`-only event. The common case: `sink.data("hello")?`.
    pub fn data(&mut self, data: &str) -> io::Result<()> {
        self.send(&Event::new().data(data))
    }

    /// Send a named event: `sink.event("tick", payload)?`.
    pub fn event(&mut self, name: &str, data: &str) -> io::Result<()> {
        self.send(&Event::new().name(name).data(data))
    }

    /// Send a fully-specified event (id / event / data / retry).
    pub fn send(&mut self, event: &Event) -> io::Result<()> {
        self.stream.write_all(&event.encode())?;
        self.stream.flush()
    }

    /// Write a comment line (`: ...`). Sends no event; use as a heartbeat to
    /// keep the connection (and intermediary proxies) alive.
    pub fn comment(&mut self, text: &str) -> io::Result<()> {
        let mut out = Vec::new();
        for line in split_lines(text) {
            out.extend_from_slice(b": ");
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
        self.stream.write_all(&out)?;
        self.stream.flush()
    }

    /// Advise the client's reconnection delay, in milliseconds.
    pub fn retry(&mut self, millis: u64) -> io::Result<()> {
        self.send(&Event::new().retry(millis))
    }
}

/// A builder for one SSE event. Any combination of fields; at least one should
/// be set for the event to mean anything.
#[derive(Default, Clone)]
pub struct Event {
    id: Option<String>,
    name: Option<String>,
    data: Option<String>,
    retry: Option<u64>,
}

impl Event {
    pub fn new() -> Event {
        Event::default()
    }
    /// Set the event id (becomes the client's `Last-Event-ID` on reconnect).
    pub fn id(mut self, id: &str) -> Event {
        self.id = Some(id.to_string());
        self
    }
    /// Set the event type (`event:` field); the browser dispatches it by name.
    pub fn name(mut self, name: &str) -> Event {
        self.name = Some(name.to_string());
        self
    }
    pub fn data(mut self, data: &str) -> Event {
        self.data = Some(data.to_string());
        self
    }
    pub fn retry(mut self, millis: u64) -> Event {
        self.retry = Some(millis);
        self
    }

    /// Serialize to the wire form, ending with the blank line that dispatches
    /// the event. Field order follows the spec's processing model.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(id) = &self.id {
            // An id must be single-line; ignore embedded newlines defensively.
            field(&mut out, "id", &first_line(id));
        }
        if let Some(name) = &self.name {
            field(&mut out, "event", &first_line(name));
        }
        if let Some(retry) = self.retry {
            field(&mut out, "retry", &retry.to_string());
        }
        if let Some(data) = &self.data {
            for line in split_lines(data) {
                field(&mut out, "data", line);
            }
        }
        out.push(b'\n'); // blank line → dispatch
        out
    }
}

/// Write one `field: value\n` line.
fn field(out: &mut Vec<u8>, name: &str, value: &str) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.push(b'\n');
}

/// Split on any newline style (`\r\n`, `\r`, `\n`) — the event stream uses `\n`
/// as its own separator, so embedded newlines must each become a new line.
fn split_lines(s: &str) -> impl Iterator<Item = &str> {
    s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

fn first_line(s: &str) -> String {
    split_lines(s).next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(e: &Event) -> String {
        String::from_utf8(e.encode()).unwrap()
    }

    #[test]
    fn data_event_ends_with_blank_line() {
        assert_eq!(encoded(&Event::new().data("hello")), "data: hello\n\n");
    }

    #[test]
    fn named_event_orders_fields() {
        assert_eq!(
            encoded(&Event::new().id("7").name("tick").data("{\"n\":1}")),
            "id: 7\nevent: tick\ndata: {\"n\":1}\n\n"
        );
    }

    #[test]
    fn multiline_data_becomes_multiple_data_lines() {
        assert_eq!(
            encoded(&Event::new().data("line one\nline two")),
            "data: line one\ndata: line two\n\n"
        );
    }

    #[test]
    fn crlf_in_data_is_normalized() {
        assert_eq!(
            encoded(&Event::new().data("a\r\nb")),
            "data: a\ndata: b\n\n"
        );
    }

    #[test]
    fn retry_only_event() {
        assert_eq!(encoded(&Event::new().retry(3000)), "retry: 3000\n\n");
    }

    #[test]
    fn id_newlines_are_stripped() {
        // An id field must not smuggle a second line into the stream.
        assert_eq!(
            encoded(&Event::new().id("a\nb").data("x")),
            "id: a\ndata: x\n\n"
        );
    }
}
