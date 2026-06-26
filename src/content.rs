//! Markdown content: frontmatter parsing and content-directory listings.
//!
//! The ported golfsetridak markdown carries YAML-ish frontmatter between `---`
//! fences (`title`, `date`, `summary`, `lead`, `lastUpdated`, …). `akurai_markdown`
//! only converts the body, so we strip and parse the frontmatter ourselves with
//! a small std-only parser — good enough for the simple `key: value` blocks the
//! content uses.

use akurai_json::Value;
use std::fs;
use std::path::Path;

/// A parsed markdown document: frontmatter key/value pairs plus the body text.
pub struct Doc {
    pub meta: Vec<(String, String)>,
    pub body: String,
}

impl Doc {
    /// Look up a frontmatter value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Split YAML-ish frontmatter delimited by `---` lines from the markdown body.
/// A document that does not open with a `---` fence (or whose fence is never
/// closed) yields empty `meta` and the whole text as `body`.
pub fn parse(raw: &str) -> Doc {
    let rest = match raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
    {
        Some(r) => r,
        None => {
            return Doc {
                meta: vec![],
                body: raw.to_string(),
            }
        }
    };

    let mut meta = Vec::new();
    let mut consumed = 0usize;
    let mut closed = false;
    for line in rest.split_inclusive('\n') {
        consumed += line.len();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            closed = true;
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_string();
            if !key.is_empty() {
                meta.push((key, unquote(v.trim())));
            }
        }
    }

    if closed {
        Doc {
            meta,
            body: rest[consumed..]
                .trim_start_matches(['\r', '\n'])
                .to_string(),
        }
    } else {
        // Unterminated fence: treat the whole input as body, no metadata.
        Doc {
            meta: vec![],
            body: raw.to_string(),
        }
    }
}

/// Strip a single pair of matching surrounding quotes, if present.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Build the news listing from `content/frettir/*.md`, newest first. Each entry
/// is a JSON object `{ slug, title, date, summary }` for the list template.
pub fn news_items(content_dir: &Path) -> Vec<Value> {
    let dir = content_dir.join("frettir");
    let mut items: Vec<(String, Value)> = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![];
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let slug = slug.to_string();
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let doc = parse(&raw);
        let title = doc.get("title").unwrap_or(&slug).to_string();
        let date = doc.get("date").unwrap_or("").to_string();
        let summary = doc.get("summary").unwrap_or("").to_string();
        let value = Value::Object(vec![
            ("slug".into(), Value::Str(slug)),
            ("title".into(), Value::Str(title)),
            ("date".into(), Value::Str(date.clone())),
            ("summary".into(), Value::Str(summary)),
        ]);
        // ISO `YYYY-MM-DD` dates sort lexicographically = chronologically.
        items.push((date, value));
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.into_iter().map(|(_, v)| v).collect()
}

/// Build the handbook chapter listing from `content/notendahandbok/_index.json`.
/// Each entry is `{ slug, title, exists }`; `exists` is false for chapters that
/// are referenced in the index but have no markdown file yet (the source repo
/// itself only ships a subset), so the template can render them non-linked.
pub fn handbook_chapters(content_dir: &Path) -> Vec<Value> {
    let dir = content_dir.join("notendahandbok");
    let Ok(raw) = fs::read_to_string(dir.join("_index.json")) else {
        return vec![];
    };
    let Ok(Value::Array(entries)) = akurai_json::parse(&raw) else {
        return vec![];
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let slug = entry.get("slug").and_then(Value::as_str)?.to_string();
            let title = entry
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&slug)
                .to_string();
            let exists = dir.join(format!("{slug}.md")).is_file();
            Some(Value::Object(vec![
                ("slug".into(), Value::Str(slug)),
                ("title".into(), Value::Str(title)),
                ("exists".into(), Value::Bool(exists)),
            ]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_and_plain_frontmatter() {
        let doc = parse("---\ntitle: \"Hello\"\ndate: 2026-01-02\n---\n\nBody text.\n");
        assert_eq!(doc.get("title"), Some("Hello"));
        assert_eq!(doc.get("date"), Some("2026-01-02"));
        assert_eq!(doc.body.trim(), "Body text.");
    }

    #[test]
    fn no_frontmatter_keeps_whole_body() {
        let doc = parse("# Just markdown\n\nNo fence here.");
        assert!(doc.meta.is_empty());
        assert!(doc.body.contains("Just markdown"));
    }

    #[test]
    fn unterminated_fence_is_treated_as_body() {
        let doc = parse("---\ntitle: x\nnever closed");
        assert!(doc.meta.is_empty());
        assert!(doc.body.starts_with("---"));
    }

    #[test]
    fn value_with_colon_keeps_remainder() {
        let doc = parse("---\nlead: A: B and C\n---\nx");
        assert_eq!(doc.get("lead"), Some("A: B and C"));
    }
}
