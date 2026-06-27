//! Minimal file-extension → MIME mapping for the static server. Ported from the
//! framework's `crates/cli/src/mime.rs` — std-only, no dependency.

/// Content-Type for a path, by extension. Defaults to a binary stream so we
/// never mislabel something we don't recognize.
pub fn for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        // ES modules must be served as JS or the browser refuses to run them.
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_types() {
        assert_eq!(for_path("/index.html"), "text/html; charset=utf-8");
        assert_eq!(for_path("/app.js"), "text/javascript; charset=utf-8");
        assert_eq!(for_path("/styles.css"), "text/css; charset=utf-8");
        assert_eq!(for_path("/logo.svg"), "image/svg+xml");
    }

    #[test]
    fn unknown_is_octet_stream() {
        assert_eq!(for_path("/file.unknownext"), "application/octet-stream");
        assert_eq!(for_path("/noext"), "application/octet-stream");
    }
}
