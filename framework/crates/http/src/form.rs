//! Parse `application/x-www-form-urlencoded` bodies (and query strings) — the
//! format a plain HTML `<form method="post">` submits. Pure std: split on `&`,
//! then percent-decode each key and value (`+` means space). Decoded bytes are
//! interpreted as UTF-8 (lossily), so a malformed escape degrades to a
//! replacement char rather than failing the whole parse.

/// Parse `a=1&b=hello+world` into ordered key/value pairs, percent-decoded.
pub fn parse_urlencoded(input: &str) -> Vec<(String, String)> {
    input
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

/// First value for `name` in a parsed pair list.
pub fn field<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Percent-decode a form component: `%XX` → byte, `+` → space.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi * 16 + lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%'); // not a valid escape — keep the literal %
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_with_plus_as_space() {
        let pairs = parse_urlencoded("name=Ada&message=hello+world");
        assert_eq!(field(&pairs, "name"), Some("Ada"));
        assert_eq!(field(&pairs, "message"), Some("hello world"));
        assert_eq!(field(&pairs, "missing"), None);
    }

    #[test]
    fn percent_decodes_utf8_and_specials() {
        let pairs = parse_urlencoded("who=%C3%93li&tag=%3Cscript%3E");
        assert_eq!(field(&pairs, "who"), Some("Óli"));
        assert_eq!(field(&pairs, "tag"), Some("<script>"));
    }

    #[test]
    fn handles_empty_values_and_blanks() {
        let pairs = parse_urlencoded("a=&b&=c");
        assert_eq!(field(&pairs, "a"), Some(""));
        assert_eq!(field(&pairs, "b"), Some(""));
    }

    #[test]
    fn malformed_escape_is_kept_literal() {
        let pairs = parse_urlencoded("x=50%25");
        assert_eq!(field(&pairs, "x"), Some("50%")); // %25 → %
        let bad = parse_urlencoded("y=ab%");
        assert_eq!(field(&bad, "y"), Some("ab%"));
    }
}
