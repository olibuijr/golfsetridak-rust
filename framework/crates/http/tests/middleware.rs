//! End-to-end: a real `Server` driven with `run_with`, exercising the
//! middleware chain over a real `TcpStream` client (mirrors `server.rs` tests).

use akurai_http::{MiddlewareStack, Reply, Request, Response, SecurityHeaders, Server, Timing};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Send one request and read the full response as a string.
fn roundtrip(addr: std::net::SocketAddr, raw: &str) -> String {
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.write_all(raw.as_bytes()).unwrap();
    let mut resp = String::new();
    conn.read_to_string(&mut resp).unwrap();
    resp
}

#[test]
fn stack_adds_headers_and_can_short_circuit() {
    let server = Server::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();

    // SecurityHeaders outermost (so it stamps even short-circuited replies),
    // then a block gate, then timing innermost — all wrapping the handler.
    let block = |req: &Request, next: &dyn Fn(&Request) -> Reply| {
        if req.path == "/blocked" {
            return Reply::Response(Response::new(403).with_text("blocked"));
        }
        next(req)
    };
    let stack = MiddlewareStack::new()
        .push(SecurityHeaders)
        .push(block)
        .push(Timing);

    std::thread::spawn(move || {
        server
            .run_with(stack, |req: &Request| {
                Response::ok().with_text(&format!("path={}", req.path))
            })
            .unwrap();
    });

    // A normal request: handler runs, post-processed by SecurityHeaders + Timing.
    let ok = roundtrip(addr, "GET /hello HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "got: {ok:?}");
    assert!(
        ok.contains("X-Content-Type-Options: nosniff\r\n"),
        "got: {ok:?}"
    );
    assert!(ok.contains("X-Frame-Options: DENY\r\n"));
    assert!(ok.contains("Referrer-Policy: no-referrer\r\n"));
    assert!(ok.contains("X-Response-Time-Us: "), "got: {ok:?}");
    assert!(ok.ends_with("path=/hello"));

    // A blocked request: the block middleware short-circuits before the handler,
    // but the still-outer SecurityHeaders post-processing applies on the way out.
    let blocked = roundtrip(addr, "GET /blocked HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(
        blocked.starts_with("HTTP/1.1 403 Forbidden\r\n"),
        "got: {blocked:?}"
    );
    assert!(
        blocked.contains("X-Frame-Options: DENY\r\n"),
        "got: {blocked:?}"
    );
    assert!(blocked.ends_with("blocked"));
}
