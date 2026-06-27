//! Class-token scanner.
//!
//! Extracts the class names used in `class="…"` / `class='…'` attributes from
//! HTML/template source. Returns the tokens in first-seen order, de-duplicated.
//! Matching is byte-oriented and quote-agnostic; it requires a word boundary
//! before `class` so identifiers like `className` or `myclass` are not matched.

/// Extract the de-duplicated, first-seen-ordered set of class tokens used in
/// `class` attributes within `source`.
pub fn classes(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        match find_class_attr(bytes, i) {
            Some((value_start, value_end, next)) => {
                let value = &source[value_start..value_end];
                for token in value.split_whitespace() {
                    if !out.iter().any(|c| c == token) {
                        out.push(token.to_string());
                    }
                }
                i = next;
            }
            None => break,
        }
    }

    out
}

/// Locate the next `class=…"value"` attribute at or after `from`.
///
/// Returns `(value_start, value_end, next_index)` where the value byte range is
/// the text between the quotes and `next_index` is just past the closing quote.
fn find_class_attr(bytes: &[u8], from: usize) -> Option<(usize, usize, usize)> {
    let mut i = from;
    while i + 5 <= bytes.len() {
        if matches_keyword(bytes, i) {
            // Word boundary before `class`: start-of-input or a non-identifier byte.
            let boundary = i == 0 || !is_ident_byte(bytes[i - 1]);
            if boundary {
                let mut j = i + 5;
                j = skip_ws(bytes, j);
                if j < bytes.len() && bytes[j] == b'=' {
                    j = skip_ws(bytes, j + 1);
                    if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        let quote = bytes[j];
                        let value_start = j + 1;
                        if let Some(close) = find_byte(bytes, value_start, quote) {
                            return Some((value_start, close, close + 1));
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn matches_keyword(bytes: &[u8], i: usize) -> bool {
    bytes[i..].starts_with(b"class")
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn find_byte(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    (from..bytes.len()).find(|&i| bytes[i] == target)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_attribute_multiple_classes() {
        assert_eq!(classes(r#"<div class="a b c">"#), vec!["a", "b", "c"]);
    }

    #[test]
    fn deduplicates_across_attributes() {
        let src = r#"<p class="a b"></p><span class="b c"></span>"#;
        assert_eq!(classes(src), vec!["a", "b", "c"]);
    }

    #[test]
    fn handles_single_quotes() {
        assert_eq!(classes("<div class='x y'>"), vec!["x", "y"]);
    }

    #[test]
    fn ignores_non_class_identifiers() {
        // `className` (JS) and a stray `myclass=` must not match.
        let src = r#"<div className="nope" myclass="no"><b class="yes">"#;
        assert_eq!(classes(src), vec!["yes"]);
    }

    #[test]
    fn tolerates_whitespace_around_equals() {
        assert_eq!(classes(r#"<div class = "a">"#), vec!["a"]);
    }

    #[test]
    fn collapses_extra_internal_whitespace() {
        assert_eq!(classes("<div class=\"  a   b \">"), vec!["a", "b"]);
    }

    #[test]
    fn empty_and_no_class() {
        assert!(classes("<div>no classes</div>").is_empty());
        assert!(classes(r#"<div class="">"#).is_empty());
    }
}
