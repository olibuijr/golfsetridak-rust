//! The blocking, thread-pooled HTTP/1.1 server.
//!
//! [`Server::bind`] grabs the socket (so the caller can read the real local
//! address before serving — useful for `:0` ephemeral ports in tests), and
//! [`Server::run`] accepts connections, parses each request head, calls the
//! handler, and writes the response.

use crate::pool::ThreadPool;
use crate::{MiddlewareStack, Reply, Request, Response};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::thread;

/// Largest request head we will buffer before giving up (64 KiB). The body is
/// read separately afterwards (see [`read_body`]), bounded by [`MAX_BODY_BYTES`].
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// A handler turns a parsed request into a [`Reply`] — either a buffered
/// [`Response`] or a connection [`Upgrade`](crate::Upgrade). `Send + Sync` so it
/// can be shared across worker threads. A closure returning a plain `Response`
/// works directly: `Response` converts into `Reply` via `From`.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: &Request) -> Reply;
}

impl<F, R> Handler for F
where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<Reply>,
{
    fn handle(&self, req: &Request) -> Reply {
        self(req).into()
    }
}

pub struct Server {
    listener: TcpListener,
    workers: usize,
}

impl Server {
    /// Bind without serving yet. Use `"127.0.0.1:0"` to get an OS-assigned port.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Server> {
        Ok(Server {
            listener: TcpListener::bind(addr)?,
            workers: default_workers(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept connections forever, dispatching each to the handler on a worker
    /// thread. Blocks the calling thread.
    pub fn run<H: Handler>(self, handler: H) -> io::Result<()> {
        self.run_with(MiddlewareStack::new(), handler)
    }

    /// Like [`run`](Server::run), but every request first passes through
    /// `middleware` (outermost-first) before reaching `handler`, and the
    /// resulting [`Reply`] passes back out through the same chain. An empty
    /// stack behaves exactly like [`run`].
    pub fn run_with<H: Handler>(self, middleware: MiddlewareStack, handler: H) -> io::Result<()> {
        let pool = ThreadPool::new(self.workers);
        let handler = Arc::new(handler);
        let middleware = Arc::new(middleware);
        for stream in self.listener.incoming() {
            let stream = stream?;
            let handler = Arc::clone(&handler);
            let middleware = Arc::clone(&middleware);
            pool.execute(move || {
                if let Err(e) = serve_connection(stream, middleware.as_ref(), handler.as_ref()) {
                    // A broken pipe / reset mid-write is normal; don't crash.
                    let _ = e;
                }
            });
        }
        Ok(())
    }
}

fn serve_connection<H: Handler>(
    mut stream: TcpStream,
    middleware: &MiddlewareStack,
    handler: &H,
) -> io::Result<()> {
    let head = match read_head(&mut stream)? {
        Some(bytes) => bytes,
        None => return Ok(()), // connection closed before a full head arrived
    };

    let reply = match Request::parse_head(&head) {
        Ok(mut req) => match read_body(&mut req, &head, &mut stream) {
            Ok(()) => middleware.handle(&req, &|r: &Request| handler.handle(r)),
            Err(BodyError::TooLarge) => {
                Reply::Response(Response::new(413).with_text("413 Payload Too Large"))
            }
            Err(BodyError::Io(e)) => return Err(e),
        },
        Err(_) => Reply::Response(Response::bad_request()),
    };

    match reply {
        Reply::Response(response) => {
            stream.write_all(&response.to_bytes())?;
            stream.flush()
        }
        Reply::Upgrade(up) => {
            // Send the head (101 / event-stream …), then hand the socket to the
            // protocol. We move it to a *dedicated* thread so a long-lived
            // WebSocket or SSE stream doesn't pin a pool worker for its whole
            // life — the request pool stays free to accept new connections.
            stream.write_all(&up.head.to_head_bytes())?;
            stream.flush()?;
            let hijack = up.hijack;
            thread::Builder::new()
                .name("akurai-upgrade".into())
                .spawn(move || {
                    let _ = hijack(stream); // a dropped peer is normal; don't crash
                })?;
            Ok(())
        }
    }
}

/// Largest request body we will buffer (8 MiB), bounding memory per request.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Why reading a body failed.
enum BodyError {
    TooLarge,
    Io(io::Error),
}

/// Read the request body into `req.body`, using `Content-Length`. Any bytes the
/// head reader already pulled past the blank line are the body's start; the rest
/// is read from the socket. No `Content-Length` means no body (the common case).
fn read_body(req: &mut Request, head: &[u8], stream: &mut TcpStream) -> Result<(), BodyError> {
    let Some(len) = req.content_length() else {
        return Ok(());
    };
    if len > MAX_BODY_BYTES {
        return Err(BodyError::TooLarge);
    }

    let start = find_head_end(head).unwrap_or(head.len());
    let mut body = head[start..].to_vec(); // bytes already buffered past the head
    body.truncate(len); // never keep more than declared

    let mut chunk = [0u8; 8192];
    while body.len() < len {
        let want = (len - body.len()).min(chunk.len());
        let n = stream.read(&mut chunk[..want]).map_err(BodyError::Io)?;
        if n == 0 {
            break; // peer closed early; hand over whatever arrived
        }
        body.extend_from_slice(&chunk[..n]);
    }

    req.body = body;
    Ok(())
}

/// Read until the blank line that ends the request head. Returns `None` if the
/// peer closed cleanly first, and errors if the head exceeds [`MAX_HEAD_BYTES`].
fn read_head(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if find_head_end(&buf).is_some() {
            return Ok(Some(buf));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(if buf.is_empty() { None } else { Some(buf) });
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Index just past the header terminator (`\r\n\r\n`, or bare `\n\n`).
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

fn default_workers() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn serves_a_request_end_to_end() {
        let server = Server::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();

        std::thread::spawn(move || {
            server
                .run(|req: &Request| Response::ok().with_text(&format!("path={}", req.path)))
                .unwrap();
        });

        let mut conn = TcpStream::connect(addr).unwrap();
        conn.write_all(b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut resp = String::new();
        conn.read_to_string(&mut resp).unwrap();

        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "got: {resp:?}");
        assert!(resp.ends_with("path=/hello"));
    }

    #[test]
    fn finds_head_terminators() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\n"), Some(18));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\n\n"), Some(16));
        assert_eq!(find_head_end(b"incomplete\r\n"), None);
    }
}
