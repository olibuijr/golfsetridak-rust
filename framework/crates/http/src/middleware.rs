//! A composable middleware chain that wraps a [`Handler`](crate::Handler).
//!
//! A [`Middleware`] sits *around* the request: it can inspect the [`Request`],
//! decide whether to call `next` (the rest of the chain, ending in the handler),
//! and post-process the [`Reply`] on the way back out. Middleware compose into an
//! ordered [`MiddlewareStack`]; the **first pushed runs outermost** — it sees the
//! request first and the reply last, wrapping everything inside it.
//!
//! This is an *additive* capability. The existing [`Server::run`](crate::Server::run)
//! path is unchanged; opt in with [`Server::run_with`](crate::Server::run_with).
//! An empty stack is a no-op that simply calls the handler.

use crate::{Reply, Request};
use std::sync::Arc;

/// One layer of request/response processing.
///
/// `handle` receives the request and `next` — a callable that runs the rest of
/// the chain (the inner middleware, ending in the handler). A middleware may:
///
/// * call `next(req)` and return its [`Reply`] unchanged (pass-through),
/// * call `next(req)` and modify the returned [`Reply`] (post-process), or
/// * return a [`Reply`] *without* calling `next` (short-circuit, e.g. a block).
///
/// `Send + Sync + 'static` so a stack can be shared across worker threads. A
/// closure of the right shape is a `Middleware` for free via the blanket impl.
pub trait Middleware: Send + Sync + 'static {
    fn handle(&self, req: &Request, next: &dyn Fn(&Request) -> Reply) -> Reply;
}

impl<F> Middleware for F
where
    F: Fn(&Request, &dyn Fn(&Request) -> Reply) -> Reply + Send + Sync + 'static,
{
    fn handle(&self, req: &Request, next: &dyn Fn(&Request) -> Reply) -> Reply {
        self(req, next)
    }
}

/// An ordered stack of [`Middleware`] that runs before a handler.
///
/// Build with [`MiddlewareStack::new`] then [`push`](MiddlewareStack::push) in
/// outermost-first order. The stack is cheap to clone-share (`Arc` layers) and
/// is consumed by [`Server::run_with`](crate::Server::run_with).
#[derive(Clone, Default)]
pub struct MiddlewareStack {
    layers: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareStack {
    /// An empty stack — running it is identical to calling the handler directly.
    pub fn new() -> MiddlewareStack {
        MiddlewareStack { layers: Vec::new() }
    }

    /// Append a middleware. The first pushed runs outermost (sees the request
    /// first, the reply last). Chainable.
    pub fn push<M: Middleware>(mut self, mw: M) -> MiddlewareStack {
        self.layers.push(Arc::new(mw));
        self
    }

    /// Number of layers.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether the stack has no layers (a no-op).
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Run the request through every layer and then `handler`, returning the
    /// final (possibly post-processed) [`Reply`]. The chain is built from the
    /// inside out so the first-pushed layer ends up outermost.
    pub fn handle<'a>(&'a self, req: &Request, handler: &'a dyn Fn(&Request) -> Reply) -> Reply {
        let mut next: Box<dyn Fn(&Request) -> Reply + 'a> = Box::new(handler);
        for mw in self.layers.iter().rev() {
            let prev = next;
            next = Box::new(move |r: &Request| mw.handle(r, &prev));
        }
        next(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Response;
    use std::sync::Mutex;

    fn get(path: &str) -> Request {
        Request::parse_head(format!("GET {path} HTTP/1.1\r\n\r\n").as_bytes()).unwrap()
    }

    fn response(reply: Reply) -> Response {
        match reply {
            Reply::Response(r) => r,
            Reply::Upgrade(_) => panic!("expected a Response, got an Upgrade"),
        }
    }

    fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn empty_stack_just_calls_the_handler() {
        let stack = MiddlewareStack::new();
        assert!(stack.is_empty());
        let reply = stack.handle(&get("/"), &|_| {
            Reply::Response(Response::ok().with_text("hi"))
        });
        assert_eq!(response(reply).body, b"hi");
    }

    #[test]
    fn a_middleware_can_add_a_header() {
        let add = |req: &Request, next: &dyn Fn(&Request) -> Reply| match next(req) {
            Reply::Response(r) => Reply::Response(r.with_header("X-Added", "yes")),
            other => other,
        };
        let stack = MiddlewareStack::new().push(add);
        let reply = stack.handle(&get("/"), &|_| Reply::Response(Response::ok()));
        assert_eq!(header(&response(reply), "X-Added"), Some("yes"));
    }

    #[test]
    fn a_middleware_can_short_circuit_without_calling_next() {
        let reached = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&reached);

        let block = |req: &Request, next: &dyn Fn(&Request) -> Reply| {
            if req.path == "/blocked" {
                return Reply::Response(Response::new(403).with_text("blocked"));
            }
            next(req)
        };
        let stack = MiddlewareStack::new().push(block);

        let handler = move |_req: &Request| {
            *flag.lock().unwrap() = true;
            Reply::Response(Response::ok())
        };
        let reply = stack.handle(&get("/blocked"), &handler);

        assert_eq!(response(reply).status, 403);
        assert!(
            !*reached.lock().unwrap(),
            "handler must not run when blocked"
        );
    }

    #[test]
    fn outer_middleware_wraps_inner() {
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let l_outer = Arc::clone(&log);
        let outer = move |req: &Request, next: &dyn Fn(&Request) -> Reply| {
            l_outer.lock().unwrap().push("outer-before");
            let reply = next(req);
            l_outer.lock().unwrap().push("outer-after");
            reply
        };

        let l_inner = Arc::clone(&log);
        let inner = move |req: &Request, next: &dyn Fn(&Request) -> Reply| {
            l_inner.lock().unwrap().push("inner-before");
            let reply = next(req);
            l_inner.lock().unwrap().push("inner-after");
            reply
        };

        let stack = MiddlewareStack::new().push(outer).push(inner);
        assert_eq!(stack.len(), 2);
        let _ = stack.handle(&get("/"), &|_| Reply::Response(Response::ok()));

        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer-before", "inner-before", "inner-after", "outer-after"]
        );
    }
}
