//! End-to-end: the server reads a POST body and hands it to the handler.

use std::io::{Read, Write};
use std::net::TcpStream;

use akurai_http::{form, Reply, Request, Response, Server};

#[test]
fn reads_post_body_and_parses_form() {
    let server = Server::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();

    std::thread::spawn(move || {
        server
            .run(|req: &Request| {
                let pairs = form::parse_urlencoded(&req.body_str());
                let name = form::field(&pairs, "name").unwrap_or("?");
                Reply::Response(Response::ok().with_text(&format!("hi {name}")))
            })
            .unwrap();
    });

    let body = "name=Ada+Lovelace&role=pioneer";
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.write_all(
        format!(
            "POST /submit HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .unwrap();

    let mut resp = String::new();
    conn.read_to_string(&mut resp).unwrap();
    assert!(resp.ends_with("hi Ada Lovelace"), "got: {resp:?}");
}
