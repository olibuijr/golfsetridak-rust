//! A live WebSocket connection: whole-message reads and writes over a hijacked
//! socket, with the protocol's control-frame and fragmentation rules handled.
//!
//! [`WsConn::recv`] returns reassembled `Text`/`Binary` messages and the peer's
//! `Close`. Pings are answered with pongs automatically (RFC 6455 §5.5.2) and
//! not surfaced; pongs are ignored. Protocol violations trigger a Close with the
//! right code and end the stream. Reads and writes use two clones of the same
//! socket, so a single owning thread can do request/response cleanly.

use std::io::{self, BufReader};
use std::net::TcpStream;

use crate::frame::{read_frame, write_frame, FrameError, Opcode};
use crate::message::{close, CloseFrame, Message};

/// Largest single frame and largest reassembled message we accept (16 MiB).
/// Bounds memory against a hostile peer; raise if an app needs bigger payloads.
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// A WebSocket connection after a successful upgrade.
pub struct WsConn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    closed: bool,
}

impl WsConn {
    /// Wrap an upgraded stream. Clones the socket so reads and writes don't
    /// borrow-conflict within the owning thread.
    pub fn new(stream: TcpStream) -> io::Result<WsConn> {
        let writer = stream.try_clone()?;
        Ok(WsConn {
            reader: BufReader::new(stream),
            writer,
            closed: false,
        })
    }

    /// Receive the next application message, or `None` once the connection is
    /// closed (cleanly, by EOF, or by a protocol error we had to reject).
    /// Control frames are handled internally.
    pub fn recv(&mut self) -> io::Result<Option<Message>> {
        if self.closed {
            return Ok(None);
        }
        // Accumulates a fragmented data message across frames: its initial
        // opcode (Text/Binary) and the bytes gathered so far.
        let mut partial: Option<(Opcode, Vec<u8>)> = None;

        loop {
            let frame = match read_frame(&mut self.reader, MAX_PAYLOAD) {
                Ok(frame) => frame,
                Err(FrameError::Io(e)) if is_disconnect(&e) => {
                    self.closed = true;
                    return Ok(None);
                }
                Err(FrameError::Io(e)) => return Err(e),
                Err(FrameError::Protocol(p)) => return self.fail(p.code, p.message),
            };

            match frame.opcode {
                Opcode::Ping => {
                    self.send_frame(Opcode::Pong, &frame.payload)?; // §5.5.2
                }
                Opcode::Pong => { /* unsolicited or keep-alive: ignore */ }
                Opcode::Close => {
                    let cf = parse_close(&frame.payload);
                    let code = cf.as_ref().map_or(close::NORMAL, |c| c.code);
                    self.send_close(code, "")?; // echo the close, completing §5.5.1
                    self.closed = true;
                    return Ok(Some(Message::Close(cf)));
                }
                Opcode::Continuation => match partial.as_mut() {
                    Some((_, buf)) => {
                        if buf.len() + frame.payload.len() > MAX_PAYLOAD {
                            return self.fail(close::TOO_BIG, "message exceeds limit");
                        }
                        buf.extend_from_slice(&frame.payload);
                        if frame.fin {
                            let (opcode, bytes) = partial.take().unwrap();
                            return self.deliver(opcode, bytes);
                        }
                    }
                    None => return self.fail(close::PROTOCOL_ERROR, "continuation without start"),
                },
                Opcode::Text | Opcode::Binary => {
                    if partial.is_some() {
                        return self.fail(close::PROTOCOL_ERROR, "new data frame mid-fragment");
                    }
                    if frame.fin {
                        return self.deliver(frame.opcode, frame.payload);
                    }
                    partial = Some((frame.opcode, frame.payload)); // first of a fragmented message
                }
            }
        }
    }

    /// Turn a completed data message into `Text` (UTF-8 validated) or `Binary`.
    fn deliver(&mut self, opcode: Opcode, bytes: Vec<u8>) -> io::Result<Option<Message>> {
        match opcode {
            Opcode::Text => match String::from_utf8(bytes) {
                Ok(text) => Ok(Some(Message::Text(text))),
                Err(_) => self.fail(close::INVALID_PAYLOAD, "text was not valid UTF-8"),
            },
            _ => Ok(Some(Message::Binary(bytes))),
        }
    }

    /// Send a text message.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.send_frame(Opcode::Text, text.as_bytes())
    }

    /// Send a binary message.
    pub fn send_binary(&mut self, data: &[u8]) -> io::Result<()> {
        self.send_frame(Opcode::Binary, data)
    }

    /// Send a ping (payload clamped to the 125-byte control-frame limit).
    pub fn send_ping(&mut self, data: &[u8]) -> io::Result<()> {
        self.send_frame(Opcode::Ping, &data[..data.len().min(125)])
    }

    /// Send a close frame with a code and reason, ending the connection.
    pub fn close(&mut self, code: u16, reason: &str) -> io::Result<()> {
        self.send_close(code, reason)?;
        self.closed = true;
        Ok(())
    }

    fn send_close(&mut self, code: u16, reason: &str) -> io::Result<()> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        // The whole close payload is a control frame: cap at 125 bytes.
        payload.truncate(125);
        self.send_frame(Opcode::Close, &payload)
    }

    fn send_frame(&mut self, opcode: Opcode, payload: &[u8]) -> io::Result<()> {
        write_frame(&mut self.writer, true, opcode, payload)
    }

    /// Reject the connection: send a close with `code`, mark it done, and
    /// surface the reason to the caller as a `Close` message.
    fn fail(&mut self, code: u16, reason: &'static str) -> io::Result<Option<Message>> {
        let _ = self.send_close(code, reason); // best-effort; peer may be gone
        self.closed = true;
        Ok(Some(Message::Close(Some(CloseFrame::new(code, reason)))))
    }
}

/// Decode a close frame's payload: a 2-byte big-endian code then a UTF-8 reason.
/// An empty payload means "no code given" (§5.5.1).
fn parse_close(payload: &[u8]) -> Option<CloseFrame> {
    if payload.len() < 2 {
        return None;
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = String::from_utf8_lossy(&payload[2..]).into_owned();
    Some(CloseFrame { code, reason })
}

/// Read errors that mean "the peer is gone", not a real fault — treat as a
/// clean end of stream.
fn is_disconnect(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
    )
}
