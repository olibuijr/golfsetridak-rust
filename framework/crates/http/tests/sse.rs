//! End-to-end Server-Sent Events: a real client reads a real `text/event-stream`
//! response off the socket and checks the head and the framed events.

use std::io::{Read, Write};
use std::net::TcpStream;

use akurai_http::{sse, Request, Server};

/// Read the whole response (head + streamed body) until the server closes.
fn fetch(addr: std::net::SocketAddr, path: &str) -> String {
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut out = String::new();
    conn.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn streams_events_with_event_stream_head() {
    let server = Server::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();

    std::thread::spawn(move || {
        server
            .run(|_req: &Request| {
                sse(|mut sink| {
                    sink.event("tick", "1")?;
                    sink.data("two")?;
                    Ok(()) // returning ends the stream and closes the socket
                })
            })
            .unwrap();
    });

    let resp = fetch(addr, "/events");
    let (head, body) = resp.split_once("\r\n\r\n").expect("head/body split");

    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "head: {head:?}");
    assert!(
        head.contains("Content-Type: text/event-stream"),
        "head: {head:?}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("content-length"),
        "SSE must not set Content-Length"
    );

    assert_eq!(body, "event: tick\ndata: 1\n\ndata: two\n\n");
}
