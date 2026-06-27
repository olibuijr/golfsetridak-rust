//! End-to-end WebSocket: a real client performs the RFC 6455 handshake against
//! the framework's HTTP server, then exchanges masked/unmasked frames with the
//! built-in echo handler. This exercises the whole stack — upgrade primitive,
//! handshake, framing, masking, and the connection loop — over a live socket.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

use akurai_http::{Request, Server};
use akurai_ws::{Message, WsConn};

/// Spawn a server whose only route echoes WebSocket messages.
fn spawn_echo_server() -> SocketAddr {
    let server = Server::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    std::thread::spawn(move || {
        server
            .run(|req: &Request| {
                akurai_ws::upgrade(req, |mut conn: WsConn| {
                    while let Some(msg) = conn.recv()? {
                        match msg {
                            Message::Text(t) => conn.send_text(&t)?,
                            Message::Binary(b) => conn.send_binary(&b)?,
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                    Ok(())
                })
            })
            .unwrap();
    });
    addr
}

/// Read the HTTP response head (up to the blank line) from the stream.
fn read_head(conn: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        conn.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

/// Encode a masked client text frame, the way a browser must (§5.1).
fn masked_text(payload: &str) -> Vec<u8> {
    let mask = [0xA1, 0xB2, 0xC3, 0xD4];
    let bytes = payload.as_bytes();
    let mut out = vec![0x81, 0x80 | bytes.len() as u8]; // FIN+Text, mask bit + short len
    out.extend_from_slice(&mask);
    out.extend(bytes.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// Read one unmasked server frame; return (opcode, payload).
fn read_server_frame(conn: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0u8; 2];
    conn.read_exact(&mut head).unwrap();
    let opcode = head[0] & 0x0f;
    assert_eq!(head[1] & 0x80, 0, "server frames must not be masked");
    let len = (head[1] & 0x7f) as usize; // test payloads stay < 126
    let mut payload = vec![0u8; len];
    conn.read_exact(&mut payload).unwrap();
    (opcode, payload)
}

#[test]
fn handshake_then_echo_then_close() {
    let addr = spawn_echo_server();
    let mut conn = TcpStream::connect(addr).unwrap();

    // 1. Opening handshake with the RFC 6455 §1.3 example key.
    conn.write_all(
        b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .unwrap();

    let head = read_head(&mut conn);
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "head: {head:?}"
    );
    assert!(
        head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "wrong accept key in: {head:?}"
    );

    // 2. Send a masked text frame; expect the same text back, unmasked.
    conn.write_all(&masked_text("hello ws")).unwrap();
    let (opcode, payload) = read_server_frame(&mut conn);
    assert_eq!(opcode, 0x1, "expected a Text frame");
    assert_eq!(payload, b"hello ws");

    // 3. A second round-trip proves the connection stays open.
    conn.write_all(&masked_text("again")).unwrap();
    let (_, payload) = read_server_frame(&mut conn);
    assert_eq!(payload, b"again");

    // 4. Client close (masked, code 1000); expect the server's close echo.
    let mask = [1u8, 2, 3, 4];
    let body = [0x03u8, 0xE8]; // 1000
    let mut close = vec![0x88, 0x80 | body.len() as u8];
    close.extend_from_slice(&mask);
    close.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    conn.write_all(&close).unwrap();

    let (opcode, _) = read_server_frame(&mut conn);
    assert_eq!(opcode, 0x8, "expected a Close frame echo");
}
