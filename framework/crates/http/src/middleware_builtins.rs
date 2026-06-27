//! Ready-to-use [`Middleware`](crate::Middleware) implementations.
//!
//! These are small, dependency-free examples of the chain in action:
//! [`SecurityHeaders`] hardens every response, [`RequestLogger`] writes an
//! access line to stderr, and [`Timing`] reports how long the handler took.
//! Push them onto a [`MiddlewareStack`](crate::MiddlewareStack) in the order you
//! want them to wrap (first pushed = outermost).

use crate::{Method, Middleware, Reply, Request};
use std::time::Instant;

/// Adds a conservative set of security headers to every [`Response`].
///
/// * `X-Content-Type-Options: nosniff` — don't MIME-sniff the body.
/// * `X-Frame-Options: DENY` — never allow framing (clickjacking guard).
/// * `Referrer-Policy: no-referrer` — never leak the URL in the `Referer`.
///
/// Existing headers of the same name are replaced (see [`Response::with_header`]).
/// [`Upgrade`](crate::Upgrade) replies pass through untouched — their head is
/// owned by the upgraded protocol.
pub struct SecurityHeaders;

impl Middleware for SecurityHeaders {
    fn handle(&self, req: &Request, next: &dyn Fn(&Request) -> Reply) -> Reply {
        match next(req) {
            Reply::Response(resp) => Reply::Response(
                resp.with_header("X-Content-Type-Options", "nosniff")
                    .with_header("X-Frame-Options", "DENY")
                    .with_header("Referrer-Policy", "no-referrer"),
            ),
            other => other,
        }
    }
}

/// Writes one access-log line per request to stderr: `METHOD path -> status`.
///
/// Pure `std` via `eprintln!` — no logging crate. Push it outermost to capture
/// the status that inner middleware produced.
pub struct RequestLogger;

impl Middleware for RequestLogger {
    fn handle(&self, req: &Request, next: &dyn Fn(&Request) -> Reply) -> Reply {
        let reply = next(req);
        let status = match &reply {
            Reply::Response(r) => r.status,
            Reply::Upgrade(u) => u.head.status,
        };
        eprintln!("{} {} -> {}", method_label(&req.method), req.path, status);
        reply
    }
}

/// Times the inner chain and stamps the elapsed wall-clock microseconds onto the
/// response as `X-Response-Time-Us`. [`Upgrade`](crate::Upgrade) replies (whose
/// lifetime is unbounded) are left untouched.
pub struct Timing;

impl Middleware for Timing {
    fn handle(&self, req: &Request, next: &dyn Fn(&Request) -> Reply) -> Reply {
        let start = Instant::now();
        match next(req) {
            Reply::Response(resp) => {
                let micros = start.elapsed().as_micros();
                Reply::Response(resp.with_header("X-Response-Time-Us", &micros.to_string()))
            }
            other => other,
        }
    }
}

/// A stderr-friendly label for a [`Method`], borrowing for `Other`.
fn method_label(m: &Method) -> &str {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
        Method::Head => "HEAD",
        Method::Options => "OPTIONS",
        Method::Other(s) => s.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MiddlewareStack, Response};

    fn get(path: &str) -> Request {
        Request::parse_head(format!("GET {path} HTTP/1.1\r\n\r\n").as_bytes()).unwrap()
    }

    fn response(reply: Reply) -> Response {
        match reply {
            Reply::Response(r) => r,
            Reply::Upgrade(_) => panic!("expected a Response"),
        }
    }

    fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn security_headers_are_applied() {
        let stack = MiddlewareStack::new().push(SecurityHeaders);
        let resp = response(stack.handle(&get("/"), &|_| Reply::Response(Response::ok())));
        assert_eq!(header(&resp, "X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(header(&resp, "X-Frame-Options"), Some("DENY"));
        assert_eq!(header(&resp, "Referrer-Policy"), Some("no-referrer"));
    }

    #[test]
    fn timing_stamps_a_microsecond_header() {
        let stack = MiddlewareStack::new().push(Timing);
        let resp = response(stack.handle(&get("/"), &|_| Reply::Response(Response::ok())));
        let v = header(&resp, "X-Response-Time-Us").expect("timing header present");
        assert!(v.parse::<u128>().is_ok(), "got: {v:?}");
    }

    #[test]
    fn method_labels_cover_known_and_unknown() {
        assert_eq!(method_label(&Method::Get), "GET");
        assert_eq!(method_label(&Method::Other("PROPFIND".into())), "PROPFIND");
    }
}
