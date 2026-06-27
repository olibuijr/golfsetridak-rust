//! AkurAI-Framework Markdown → HTML — pure `std`, zero dependencies.
//!
//! A pragmatic CommonMark subset, enough for docs and changelogs: ATX headings,
//! fenced code blocks, unordered/ordered lists, blockquotes, horizontal rules,
//! and paragraphs, with inline code / bold / italic / links (see [`inline`]).
//! Not a full CommonMark implementation — no tables, nested lists, or images yet.

#![forbid(unsafe_code)]

mod inline;

/// Render a Markdown document to an HTML fragment.
pub fn to_html(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block: ``` ... ```
        if let Some(lang) = trimmed.strip_prefix("```") {
            i += 1;
            let mut code = String::new();
            let mut raw = String::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                escape_text(lines[i], &mut code);
                code.push('\n');
                raw.push_str(lines[i]);
                raw.push('\n');
                i += 1;
            }
            i += 1; // consume closing fence
            let info = lang.trim();

            // `preview` fence: render the body as a LIVE component preview —
            // the raw, UNESCAPED HTML — alongside an escaped copy of the markup.
            //
            // SAFETY: this emits the fence body verbatim as HTML. It is for
            // first-party, trusted documentation content ONLY and must NEVER be
            // fed untrusted or user-supplied input (it would be an XSS sink).
            if info == "preview" {
                out.push_str("<div class=\"preview\">");
                out.push_str("<div class=\"preview-label\">Preview</div>");
                out.push_str("<div class=\"preview-demo\">");
                out.push_str(raw.trim_end_matches('\n'));
                out.push_str("</div>");
                out.push_str("<div class=\"preview-code\">");
                out.push_str(&format!("<pre><code>{code}</code></pre>"));
                out.push_str("</div></div>\n");
                continue;
            }

            let class = if info.is_empty() {
                String::new()
            } else {
                format!(" class=\"language-{info}\"")
            };
            out.push_str(&format!("<pre><code{class}>{code}</code></pre>\n"));
            continue;
        }

        // ATX heading: # .. ######
        if let Some((level, text)) = heading(trimmed) {
            out.push_str(&format!("<h{level}>{}</h{level}>\n", inline::render(text)));
            i += 1;
            continue;
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            out.push_str("<hr>\n");
            i += 1;
            continue;
        }

        // Unordered list
        if is_ul(trimmed) {
            out.push_str("<ul>\n");
            while i < lines.len() && is_ul(lines[i].trim()) {
                let item = &lines[i].trim()[2..];
                out.push_str(&format!("<li>{}</li>\n", inline::render(item)));
                i += 1;
            }
            out.push_str("</ul>\n");
            continue;
        }

        // Ordered list
        if is_ol(trimmed) {
            out.push_str("<ol>\n");
            while i < lines.len() && is_ol(lines[i].trim()) {
                let item = lines[i].trim().split_once(' ').map_or("", |(_, rest)| rest);
                out.push_str(&format!("<li>{}</li>\n", inline::render(item)));
                i += 1;
            }
            out.push_str("</ol>\n");
            continue;
        }

        // Blockquote
        if let Some(first) = trimmed.strip_prefix("> ") {
            let mut quote = String::from(first);
            i += 1;
            while i < lines.len() {
                if let Some(rest) = lines[i].trim().strip_prefix("> ") {
                    quote.push(' ');
                    quote.push_str(rest);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push_str(&format!(
                "<blockquote>{}</blockquote>\n",
                inline::render(&quote)
            ));
            continue;
        }

        // Paragraph: gather consecutive non-blank, non-block lines.
        let mut para = String::new();
        while i < lines.len() {
            let t = lines[i].trim();
            if t.is_empty()
                || t.starts_with("```")
                || heading(t).is_some()
                || is_ul(t)
                || is_ol(t)
                || t == "---"
                || t.starts_with("> ")
            {
                break;
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t);
            i += 1;
        }
        out.push_str(&format!("<p>{}</p>\n", inline::render(&para)));
    }

    out
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some((hashes, line[hashes + 1..].trim()))
    } else {
        None
    }
}

fn is_ul(line: &str) -> bool {
    line.starts_with("- ") || line.starts_with("* ")
}

/// `1. `, `2. ` … — a digit run followed by `. `.
fn is_ol(line: &str) -> bool {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && line[digits..].starts_with(". ")
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::to_html;

    #[test]
    fn headings() {
        assert_eq!(to_html("# Title"), "<h1>Title</h1>\n");
        assert_eq!(to_html("### Sub"), "<h3>Sub</h3>\n");
        // not a heading without the space
        assert_eq!(to_html("#nope"), "<p>#nope</p>\n");
    }

    #[test]
    fn paragraph_joins_wrapped_lines() {
        assert_eq!(to_html("one\ntwo"), "<p>one two</p>\n");
    }

    #[test]
    fn fenced_code_is_escaped_and_not_inlined() {
        let html = to_html("```rust\nlet x = a < b;\n```");
        assert_eq!(
            html,
            "<pre><code class=\"language-rust\">let x = a &lt; b;\n</code></pre>\n"
        );
    }

    #[test]
    fn preview_fence_emits_live_html_and_escaped_copy() {
        let html = to_html("```preview\n<button class=\"btn\">Go</button>\n```");
        // Live preview: raw, UNESCAPED markup inside .preview-demo.
        assert!(html.contains("<div class=\"preview\">"));
        assert!(html.contains("<div class=\"preview-label\">Preview</div>"));
        assert!(
            html.contains("<div class=\"preview-demo\"><button class=\"btn\">Go</button></div>")
        );
        // Copyable source: escaped code block inside .preview-code.
        assert!(html.contains(
            "<div class=\"preview-code\"><pre><code>&lt;button class=\"btn\"&gt;Go&lt;/button&gt;\n</code></pre></div>"
        ));
    }

    #[test]
    fn normal_fence_still_escapes_after_preview_support() {
        // A plain (non-preview) fence must keep escaping its body unchanged.
        let html = to_html("```\n<button>Go</button>\n```");
        assert_eq!(
            html,
            "<pre><code>&lt;button&gt;Go&lt;/button&gt;\n</code></pre>\n"
        );
        assert!(!html.contains("preview-demo"));
        // And a language fence is likewise untouched.
        let rust = to_html("```rust\nlet x = a < b;\n```");
        assert_eq!(
            rust,
            "<pre><code class=\"language-rust\">let x = a &lt; b;\n</code></pre>\n"
        );
    }

    #[test]
    fn unordered_list() {
        assert_eq!(to_html("- a\n- b"), "<ul>\n<li>a</li>\n<li>b</li>\n</ul>\n");
    }

    #[test]
    fn ordered_list() {
        assert_eq!(
            to_html("1. a\n2. b"),
            "<ol>\n<li>a</li>\n<li>b</li>\n</ol>\n"
        );
    }

    #[test]
    fn blockquote_and_hr() {
        assert_eq!(to_html("> quoted"), "<blockquote>quoted</blockquote>\n");
        assert_eq!(to_html("---"), "<hr>\n");
    }

    #[test]
    fn inline_inside_blocks() {
        assert_eq!(
            to_html("# Hello **world**"),
            "<h1>Hello <strong>world</strong></h1>\n"
        );
        assert_eq!(
            to_html("see `code` and [x](/y)"),
            "<p>see <code>code</code> and <a href=\"/y\">x</a></p>\n"
        );
    }

    #[test]
    fn mixed_document() {
        let md = "# Title\n\nIntro para.\n\n- one\n- two\n\n```\nraw\n```";
        let html = to_html(md);
        assert!(html.starts_with("<h1>Title</h1>\n<p>Intro para.</p>\n<ul>"));
        assert!(html.contains("<pre><code>raw\n</code></pre>"));
    }
}
