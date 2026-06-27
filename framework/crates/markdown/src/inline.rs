//! Inline Markdown: code spans, links, bold, italic — with HTML escaping.
//!
//! A single left-to-right scan. Emphasis and link text recurse so `**[x](y)**`
//! works; code spans never recurse (their content is literal).

/// Render an inline string fragment to HTML.
pub fn render(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        match c {
            '`' => {
                if let Some(j) = find(&ch, i + 1, '`') {
                    out.push_str("<code>");
                    escape_slice(&ch[i + 1..j], &mut out);
                    out.push_str("</code>");
                    i = j + 1;
                    continue;
                }
            }
            '[' => {
                if let Some((text, url, end)) = parse_link(&ch, i) {
                    out.push_str("<a href=\"");
                    escape_attr(&url, &mut out);
                    out.push_str("\">");
                    out.push_str(&render(&text));
                    out.push_str("</a>");
                    i = end;
                    continue;
                }
            }
            '*' if i + 1 < ch.len() && ch[i + 1] == '*' => {
                if let Some(j) = find_seq(&ch, i + 2, '*') {
                    out.push_str("<strong>");
                    out.push_str(&render(&collect(&ch, i + 2, j)));
                    out.push_str("</strong>");
                    i = j + 2;
                    continue;
                }
            }
            '*' | '_' => {
                if let Some(j) = find(&ch, i + 1, c) {
                    out.push_str("<em>");
                    out.push_str(&render(&collect(&ch, i + 1, j)));
                    out.push_str("</em>");
                    i = j + 1;
                    continue;
                }
            }
            _ => {}
        }
        escape_char(c, &mut out);
        i += 1;
    }
    out
}

/// Parse `[text](url)` starting at the `[`; returns (text, url, index past `)`).
fn parse_link(ch: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = find(ch, start + 1, ']')?;
    if close + 1 >= ch.len() || ch[close + 1] != '(' {
        return None;
    }
    let paren = find(ch, close + 2, ')')?;
    Some((
        collect(ch, start + 1, close),
        collect(ch, close + 2, paren),
        paren + 1,
    ))
}

fn find(ch: &[char], from: usize, target: char) -> Option<usize> {
    (from..ch.len()).find(|&i| ch[i] == target)
}

/// Find a doubled delimiter run (`**`) starting at or after `from`.
fn find_seq(ch: &[char], from: usize, d: char) -> Option<usize> {
    let mut i = from;
    while i + 1 < ch.len() {
        if ch[i] == d && ch[i + 1] == d {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn collect(ch: &[char], a: usize, b: usize) -> String {
    ch[a..b].iter().collect()
}

fn escape_slice(ch: &[char], out: &mut String) {
    for &c in ch {
        escape_char(c, out);
    }
}

fn escape_char(c: char, out: &mut String) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        c => out.push(c),
    }
}

fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn plain_text_is_escaped() {
        assert_eq!(render("a < b & c"), "a &lt; b &amp; c");
    }

    #[test]
    fn code_span_is_literal() {
        assert_eq!(render("use `<T>` here"), "use <code>&lt;T&gt;</code> here");
    }

    #[test]
    fn bold_and_italic() {
        assert_eq!(
            render("**b** and *i* and _j_"),
            "<strong>b</strong> and <em>i</em> and <em>j</em>"
        );
    }

    #[test]
    fn links() {
        assert_eq!(render("[docs](/docs)"), "<a href=\"/docs\">docs</a>");
    }

    #[test]
    fn nested_bold_link() {
        assert_eq!(
            render("**[x](/y)**"),
            "<strong><a href=\"/y\">x</a></strong>"
        );
    }

    #[test]
    fn unterminated_markers_are_literal() {
        assert_eq!(render("a * b"), "a * b");
        assert_eq!(render("`open"), "`open");
    }
}
