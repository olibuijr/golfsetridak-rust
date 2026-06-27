//! RFC 6455 §5 framing: read and write the base WebSocket frame.
//!
//! A frame is a FIN bit, three reserved bits (must be zero — we negotiate no
//! extensions), a 4-bit opcode, a mask bit + key, and a payload whose length is
//! 7, 7+16, or 7+64 bits. This module deals in single frames; reassembly of
//! fragmented messages and control-frame semantics live in [`crate::conn`].
//!
//! Server rules enforced here: client→server frames MUST be masked (§5.1);
//! server→client frames we write are never masked; reserved bits and unknown
//! opcodes are protocol errors; control frames must be ≤125 bytes and unfragmented.

use std::io::{self, Read, Write};

/// The frame opcode (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn from_u8(n: u8) -> Option<Opcode> {
        match n {
            0x0 => Some(Opcode::Continuation),
            0x1 => Some(Opcode::Text),
            0x2 => Some(Opcode::Binary),
            0x8 => Some(Opcode::Close),
            0x9 => Some(Opcode::Ping),
            0xA => Some(Opcode::Pong),
            _ => None, // reserved opcodes 0x3-0x7 / 0xB-0xF → protocol error
        }
    }
    fn to_u8(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xA,
        }
    }
    /// Control frames (close/ping/pong) carry protocol signalling, not data,
    /// and have stricter rules (§5.5).
    pub fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

/// One decoded frame, payload already unmasked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

/// A framing violation. Carries the close code to send back (§7.4.1) before the
/// connection is torn down.
#[derive(Debug)]
pub struct ProtocolError {
    pub code: u16,
    pub message: &'static str,
}

/// Either a real I/O failure or a protocol violation.
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Protocol(ProtocolError),
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> FrameError {
        FrameError::Io(e)
    }
}

fn protocol(code: u16, message: &'static str) -> FrameError {
    FrameError::Protocol(ProtocolError { code, message })
}

/// Read one frame from `r`, rejecting anything that violates the server-side
/// rules. `max_payload` caps a single frame to bound memory.
pub fn read_frame(r: &mut impl Read, max_payload: usize) -> Result<Frame, FrameError> {
    use crate::message::close;

    let mut head = [0u8; 2];
    r.read_exact(&mut head)?;

    let fin = head[0] & 0x80 != 0;
    if head[0] & 0x70 != 0 {
        return Err(protocol(
            close::PROTOCOL_ERROR,
            "reserved bits must be zero",
        ));
    }
    let opcode = Opcode::from_u8(head[0] & 0x0f)
        .ok_or_else(|| protocol(close::PROTOCOL_ERROR, "reserved opcode"))?;

    let masked = head[1] & 0x80 != 0;
    if !masked {
        // §5.1: a server MUST close on an unmasked client frame.
        return Err(protocol(
            close::PROTOCOL_ERROR,
            "client frame was not masked",
        ));
    }

    let len = read_payload_len(r, head[1] & 0x7f)?;
    if opcode.is_control() && (len > 125 || !fin) {
        return Err(protocol(
            close::PROTOCOL_ERROR,
            "control frames must be final and ≤125 bytes",
        ));
    }
    if len > max_payload as u64 {
        return Err(protocol(close::TOO_BIG, "frame payload exceeds limit"));
    }

    let mut mask = [0u8; 4];
    r.read_exact(&mut mask)?;
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }

    Ok(Frame {
        fin,
        opcode,
        payload,
    })
}

/// Decode the extended payload length (§5.2): 7-bit, or `126` + u16, or `127` + u64.
fn read_payload_len(r: &mut impl Read, len7: u8) -> Result<u64, FrameError> {
    use crate::message::close;
    Ok(match len7 {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u16::from_be_bytes(b) as u64
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            let n = u64::from_be_bytes(b);
            if n & (1 << 63) != 0 {
                return Err(protocol(
                    close::PROTOCOL_ERROR,
                    "high bit of 64-bit length set",
                ));
            }
            n
        }
        n => n as u64,
    })
}

/// Write a server→client frame (never masked, per §5.1).
pub fn write_frame(
    w: &mut impl Write,
    fin: bool,
    opcode: Opcode,
    payload: &[u8],
) -> io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(if fin { 0x80 } else { 0 } | opcode.to_u8());

    let len = payload.len();
    if len < 126 {
        head.push(len as u8);
    } else if len <= u16::MAX as usize {
        head.push(126);
        head.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        head.push(127);
        head.extend_from_slice(&(len as u64).to_be_bytes());
    }

    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Encode a masked client frame the way a browser would, for round-trips.
    fn masked_client_frame(fin: bool, opcode: Opcode, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode.to_u8()];
        let len = payload.len();
        if len < 126 {
            out.push(0x80 | len as u8);
        } else if len <= u16::MAX as usize {
            out.push(0x80 | 126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(0x80 | 127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }

    #[test]
    fn reads_and_unmasks_a_short_text_frame() {
        let bytes = masked_client_frame(true, Opcode::Text, b"Hello", [0x37, 0xfa, 0x21, 0x3d]);
        let frame = read_frame(&mut Cursor::new(bytes), 1024).unwrap();
        assert_eq!(frame.opcode, Opcode::Text);
        assert!(frame.fin);
        assert_eq!(frame.payload, b"Hello");
    }

    #[test]
    fn reads_a_126_extended_length_frame() {
        let payload = vec![b'z'; 200];
        let bytes = masked_client_frame(true, Opcode::Binary, &payload, [1, 2, 3, 4]);
        let frame = read_frame(&mut Cursor::new(bytes), 1024).unwrap();
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn rejects_unmasked_client_frame() {
        // FIN+Text, len 0, no mask bit.
        let bytes = vec![0x81, 0x00];
        match read_frame(&mut Cursor::new(bytes), 1024) {
            Err(FrameError::Protocol(p)) => assert_eq!(p.code, 1002),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_oversized_payload() {
        let bytes = masked_client_frame(true, Opcode::Binary, &[0u8; 100], [9; 4]);
        match read_frame(&mut Cursor::new(bytes), 10) {
            Err(FrameError::Protocol(p)) => assert_eq!(p.code, 1009),
            other => panic!("expected too-big, got {other:?}"),
        }
    }

    #[test]
    fn rejects_fragmented_control_frame() {
        let bytes = masked_client_frame(false, Opcode::Ping, b"x", [9; 4]);
        match read_frame(&mut Cursor::new(bytes), 1024) {
            Err(FrameError::Protocol(p)) => assert_eq!(p.code, 1002),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn server_frame_round_trips_unmasked() {
        let mut buf = Vec::new();
        write_frame(&mut buf, true, Opcode::Text, b"hi").unwrap();
        assert_eq!(buf, vec![0x81, 0x02, b'h', b'i']); // no mask bit set
    }
}
