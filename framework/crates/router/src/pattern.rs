//! A single route pattern and how it matches a path.
//!
//! A pattern is a `/`-separated list of segments, each one of:
//! - **static** — `posts` — must equal the path segment verbatim,
//! - **param** — `:id` — captures exactly one path segment by name,
//! - **wildcard** — `*rest` — captures all remaining segments as one string;
//!   only meaningful as the final segment (it consumes the rest of the path).
//!
//! Matching is allocation-light: it borrows the path, captures only what the
//! params need, and returns `None` the instant a segment disagrees.

/// One piece of a route pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Static(String),
    Param(String),
    Wildcard(String),
}

/// A parsed route pattern, e.g. `/posts/:id/comments/:cid` or `/files/*path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    segments: Vec<Segment>,
}

impl Pattern {
    /// Parse a pattern string. Leading/trailing slashes are ignored, so `/a/`,
    /// `a`, and `/a` are equivalent.
    pub fn parse(pattern: &str) -> Pattern {
        let segments = split(pattern)
            .map(|s| {
                if let Some(name) = s.strip_prefix(':') {
                    Segment::Param(name.to_string())
                } else if let Some(name) = s.strip_prefix('*') {
                    Segment::Wildcard(name.to_string())
                } else {
                    Segment::Static(s.to_string())
                }
            })
            .collect();
        Pattern { segments }
    }

    /// Match `path`, returning the captured params (in pattern order) if it
    /// matches, or `None` if it does not.
    pub fn match_path(&self, path: &str) -> Option<Vec<(String, String)>> {
        let parts: Vec<&str> = split(path).collect();
        let mut params = Vec::new();
        let mut i = 0;

        for segment in &self.segments {
            match segment {
                Segment::Static(want) => {
                    if parts.get(i) != Some(&want.as_str()) {
                        return None;
                    }
                    i += 1;
                }
                Segment::Param(name) => {
                    let value = parts.get(i)?;
                    params.push((name.clone(), (*value).to_string()));
                    i += 1;
                }
                Segment::Wildcard(name) => {
                    // Consumes everything left (possibly nothing). Terminal.
                    params.push((name.clone(), parts[i..].join("/")));
                    return Some(params);
                }
            }
        }

        // No wildcard short-circuit fired: the path must be fully consumed.
        if i == parts.len() {
            Some(params)
        } else {
            None
        }
    }

    /// A sort key ranking how *specific* this pattern is, higher = more
    /// specific. Used to break ties when several patterns match one path:
    /// more static segments win; among equals, a pattern without a wildcard
    /// beats one with a wildcard; then more params.
    pub fn specificity(&self) -> (usize, usize, usize) {
        let statics = self
            .segments
            .iter()
            .filter(|s| matches!(s, Segment::Static(_)))
            .count();
        let params = self
            .segments
            .iter()
            .filter(|s| matches!(s, Segment::Param(_)))
            .count();
        let no_wildcard = usize::from(
            !self
                .segments
                .iter()
                .any(|s| matches!(s, Segment::Wildcard(_))),
        );
        (statics, no_wildcard, params)
    }
}

/// Split a path/pattern into non-empty segments.
fn split(s: &str) -> impl Iterator<Item = &str> {
    s.split('/').filter(|seg| !seg.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pat: &str) -> Pattern {
        Pattern::parse(pat)
    }

    #[test]
    fn static_matches_exact_only() {
        assert_eq!(p("/about").match_path("/about"), Some(vec![]));
        assert_eq!(p("/about").match_path("/about/team"), None);
        assert_eq!(p("/about").match_path("/contact"), None);
    }

    #[test]
    fn slashes_are_normalized() {
        assert_eq!(p("about").match_path("/about/"), Some(vec![]));
        assert_eq!(p("/about/").match_path("about"), Some(vec![]));
    }

    #[test]
    fn param_captures_one_segment() {
        assert_eq!(
            p("/posts/:id").match_path("/posts/42"),
            Some(vec![("id".into(), "42".into())])
        );
        // a param needs exactly one segment present
        assert_eq!(p("/posts/:id").match_path("/posts"), None);
        assert_eq!(p("/posts/:id").match_path("/posts/42/edit"), None);
    }

    #[test]
    fn multiple_params_keep_order() {
        assert_eq!(
            p("/posts/:pid/comments/:cid").match_path("/posts/7/comments/3"),
            Some(vec![("pid".into(), "7".into()), ("cid".into(), "3".into())])
        );
    }

    #[test]
    fn wildcard_captures_the_rest() {
        assert_eq!(
            p("/files/*path").match_path("/files/a/b/c.txt"),
            Some(vec![("path".into(), "a/b/c.txt".into())])
        );
        // wildcard may capture an empty remainder
        assert_eq!(
            p("/files/*path").match_path("/files"),
            Some(vec![("path".into(), "".into())])
        );
    }

    #[test]
    fn segment_count_must_align() {
        assert_eq!(p("/a/b").match_path("/a"), None);
        assert_eq!(p("/a").match_path("/a/b"), None);
        assert_eq!(p("/").match_path("/"), Some(vec![]));
    }

    #[test]
    fn specificity_orders_static_param_wildcard() {
        let stat = p("/a/b").specificity();
        let param = p("/a/:x").specificity();
        let wild = p("/a/*x").specificity();
        assert!(
            stat > param,
            "static should outrank param: {stat:?} vs {param:?}"
        );
        assert!(
            param > wild,
            "param should outrank wildcard: {param:?} vs {wild:?}"
        );
    }
}
