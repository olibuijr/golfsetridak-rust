//! AkurAI-Framework router core — pure `std`, zero dependencies.
//!
//! Maps URL patterns (`/posts/:id`, `/files/*path`) to a value plus captured
//! params. Transport-agnostic: it knows nothing about HTTP, so the HTTP layer
//! can route requests and a future client router can reuse the exact same
//! matching. When several patterns match one path, the most *specific* wins
//! (static > param > wildcard), so general fallbacks never shadow exact routes.
//!
//! Matching is linear over the route table, ordered by specificity — correct
//! and fast at the small/medium scale the framework targets. A radix trie is
//! the scale optimization for later (the same "brute force first" path the
//! vector search takes), and it can drop in behind this API unchanged.

#![forbid(unsafe_code)]

pub mod pattern;

pub use pattern::Pattern;

/// A routing table from patterns to values of type `T` (a template name, a
/// handler id, anything the caller wants to associate with a route).
pub struct Router<T> {
    routes: Vec<(Pattern, T)>,
}

/// The result of a successful match: the matched value and the captured params.
pub struct Match<'a, T> {
    pub value: &'a T,
    pub params: Vec<(String, String)>,
}

impl<'a, T> Match<'a, T> {
    /// Look up a captured param by name.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

impl<T> Default for Router<T> {
    fn default() -> Self {
        Router { routes: Vec::new() }
    }
}

impl<T> Router<T> {
    pub fn new() -> Router<T> {
        Router::default()
    }

    /// Register `pattern` → `value`. Returns `self` for chaining.
    pub fn route(&mut self, pattern: &str, value: T) -> &mut Self {
        self.routes.push((Pattern::parse(pattern), value));
        self
    }

    /// Find the most specific route matching `path`, with its captured params.
    pub fn match_path(&self, path: &str) -> Option<Match<'_, T>> {
        let mut best: Option<(Match<'_, T>, (usize, usize, usize))> = None;
        for (pattern, value) in &self.routes {
            if let Some(params) = pattern.match_path(path) {
                let score = pattern.specificity();
                if best.as_ref().is_none_or(|(_, b)| score > *b) {
                    best = Some((Match { value, params }, score));
                }
            }
        }
        best.map(|(m, _)| m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_most_specific_match() {
        let mut r = Router::new();
        r.route("/posts/:id", "param").route("/posts/new", "static");
        // exact static beats the param route
        assert_eq!(r.match_path("/posts/new").unwrap().value, &"static");
        // a real id falls through to the param route
        let m = r.match_path("/posts/42").unwrap();
        assert_eq!(m.value, &"param");
        assert_eq!(m.param("id"), Some("42"));
    }

    #[test]
    fn wildcard_is_the_lowest_priority_fallback() {
        let mut r = Router::new();
        r.route("/docs/*rest", "catch-all")
            .route("/docs/getting-started", "page");
        assert_eq!(
            r.match_path("/docs/getting-started").unwrap().value,
            &"page"
        );
        let m = r.match_path("/docs/a/b").unwrap();
        assert_eq!(m.value, &"catch-all");
        assert_eq!(m.param("rest"), Some("a/b"));
    }

    #[test]
    fn no_match_returns_none() {
        let mut r = Router::new();
        r.route("/a", 1);
        assert!(r.match_path("/b").is_none());
    }
}
