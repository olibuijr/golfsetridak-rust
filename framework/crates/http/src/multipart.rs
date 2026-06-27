//! Parse `multipart/form-data` bodies — the format an HTML `<form
//! enctype="multipart/form-data">` submits when it carries file uploads. Pure
//! std: no allocation-heavy regex, just byte scanning per [RFC 7578].
//!
//! A body is a sequence of parts separated by a boundary delimiter taken from
//! the `Content-Type` header (`multipart/form-data; boundary=...`). Each part
//! has its own little header block (`Content-Disposition`, optionally
//! `Content-Type`) followed by a raw byte payload that runs until the next
//! delimiter. Payloads are arbitrary bytes — a part may be a UTF-8 text field
//! or a binary file containing CRLFs of its own — so the data is kept as
//! `Vec<u8>` and never decoded here.
//!
//! The parser is deliberately strict about structure (it returns an error
//! rather than guessing) but tolerant of the two line-ending conventions a
//! delimiter line may use. It does no I/O and never panics on malformed input.
//!
//! [RFC 7578]: https://www.rfc-editor.org/rfc/rfc7578

use std::fmt;

/// One decoded part of a `multipart/form-data` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The `name` parameter from `Content-Disposition` — the form field name.
    pub name: String,
    /// The `filename` parameter, present when the field is a file upload.
    pub filename: Option<String>,
    /// The part's own `Content-Type` header, if it declared one.
    pub content_type: Option<String>,
    /// The raw payload bytes, exactly as they appeared between the delimiters.
    pub data: Vec<u8>,
}

/// Why a `multipart/form-data` body failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultipartError {
    /// The `Content-Type` header was not `multipart/form-data` or carried no
    /// usable `boundary=` parameter.
    MissingBoundary,
    /// The body did not contain the opening boundary delimiter at all.
    NoOpeningBoundary,
    /// A part was structurally broken: no header/body separator, no closing
    /// delimiter, or a header block that is not valid UTF-8.
    MalformedPart,
    /// A part had no `Content-Disposition: form-data; name="..."` and so cannot
    /// be addressed as a form field.
    MissingName,
}

impl fmt::Display for MultipartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            MultipartError::MissingBoundary => "missing or invalid multipart boundary",
            MultipartError::NoOpeningBoundary => "body has no opening boundary delimiter",
            MultipartError::MalformedPart => "malformed multipart part",
            MultipartError::MissingName => "part is missing a form-data name",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for MultipartError {}

/// Parse a `multipart/form-data` body into its [`Part`]s.
///
/// `content_type_header` is the full header value, e.g.
/// `multipart/form-data; boundary=----WebKitFormBoundaryabc123`; the boundary
/// is extracted from it (quoted or unquoted). `body` is the raw request body.
///
/// Returns an error rather than panicking on any malformed or truncated input.
pub fn parse_multipart(
    content_type_header: &str,
    body: &[u8],
) -> Result<Vec<Part>, MultipartError> {
    let boundary = extract_boundary(content_type_header).ok_or(MultipartError::MissingBoundary)?;

    // The on-the-wire delimiter is "--" + boundary. The opening one may sit at
    // the very start of the body; every later one is preceded by a CRLF that
    // belongs to the *previous* part's terminator, not its payload.
    let mut delim = Vec::with_capacity(boundary.len() + 2);
    delim.extend_from_slice(b"--");
    delim.extend_from_slice(boundary.as_bytes());

    // Find the opening delimiter (ignoring any RFC "preamble" before it) and
    // position the cursor just past it.
    let mut pos = find(body, &delim).ok_or(MultipartError::NoOpeningBoundary)? + delim.len();

    let mut parts = Vec::new();
    loop {
        let rest = &body[pos..];

        // A delimiter immediately followed by "--" is the closing delimiter.
        if rest.starts_with(b"--") {
            return Ok(parts);
        }

        // Otherwise a CRLF (tolerating a bare LF) introduces the part's headers.
        let after_eol = strip_eol(rest).ok_or(MultipartError::MalformedPart)?;
        let head_start = pos + (rest.len() - after_eol.len());

        // Headers run until a blank line. HTTP multipart uses CRLFCRLF; accept a
        // bare LFLF too for robustness.
        let (sep_at, sep_len) = find_header_end(&body[head_start..])
            .map(|(i, n)| (head_start + i, n))
            .ok_or(MultipartError::MalformedPart)?;
        let header_block = std::str::from_utf8(&body[head_start..sep_at])
            .map_err(|_| MultipartError::MalformedPart)?;
        let (name, filename, content_type) = parse_part_headers(header_block)?;

        // The payload starts after the blank line and ends right before the next
        // CRLF + delimiter.
        let data_start = sep_at + sep_len;
        let next = find_next_delim(&body[data_start..], &delim)
            .map(|i| data_start + i)
            .ok_or(MultipartError::MalformedPart)?;

        parts.push(Part {
            name,
            filename,
            content_type,
            data: body[data_start..next].to_vec(),
        });

        // `next` points at the CRLF preceding the delimiter; skip CRLF + delim.
        pos = strip_eol(&body[next..])
            .map(|after| (body.len() - after.len()) + delim.len())
            .ok_or(MultipartError::MalformedPart)?;
    }
}

/// Pull the `boundary` value out of a `multipart/form-data` content-type header.
fn extract_boundary(header: &str) -> Option<String> {
    let mut parts = header.split(';');
    let media_type = parts.next()?.trim();
    if !media_type.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in parts {
        let param = param.trim();
        let eq = param.find('=')?;
        let (key, value) = param.split_at(eq);
        if key.trim().eq_ignore_ascii_case("boundary") {
            let value = value[1..].trim();
            // The value may be quoted; strip a single matching pair of quotes.
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

/// Parse one part's header block, returning `(name, filename, content_type)`.
fn parse_part_headers(
    block: &str,
) -> Result<(String, Option<String>, Option<String>), MultipartError> {
    let mut name = None;
    let mut filename = None;
    let mut content_type = None;

    for line in block.split("\r\n").flat_map(|l| l.split('\n')) {
        if line.trim().is_empty() {
            continue;
        }
        let (header, value) = match line.split_once(':') {
            Some(hv) => hv,
            None => return Err(MultipartError::MalformedPart),
        };
        let header = header.trim();
        let value = value.trim();
        if header.eq_ignore_ascii_case("content-disposition") {
            for (k, v) in disposition_params(value) {
                if k.eq_ignore_ascii_case("name") {
                    name = Some(v);
                } else if k.eq_ignore_ascii_case("filename") {
                    filename = Some(v);
                }
            }
        } else if header.eq_ignore_ascii_case("content-type") {
            content_type = Some(value.to_string());
        }
    }

    match name {
        Some(name) => Ok((name, filename, content_type)),
        None => Err(MultipartError::MissingName),
    }
}

/// Split the parameter tail of a `Content-Disposition` value into key/value
/// pairs, handling quoted and unquoted values. The leading `form-data` token
/// has no `=` and is skipped.
fn disposition_params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for param in value.split(';') {
        let param = param.trim();
        let Some((k, v)) = param.split_once('=') else {
            continue; // the `form-data` disposition token, or junk
        };
        let k = k.trim().to_string();
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(v);
        out.push((k, v.to_string()));
    }
    out
}

/// Strip a leading CRLF (or bare LF) from `bytes`, returning what follows, or
/// `None` if `bytes` does not begin with a line ending.
fn strip_eol(bytes: &[u8]) -> Option<&[u8]> {
    if let Some(rest) = bytes.strip_prefix(b"\r\n") {
        Some(rest)
    } else {
        bytes.strip_prefix(b"\n")
    }
}

/// Find the blank line that ends a header block, returning its offset and the
/// length of the separator (`\r\n\r\n` → 4, `\n\n` → 2).
fn find_header_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let crlf = find(bytes, b"\r\n\r\n");
    let lf = find(bytes, b"\n\n");
    match (crlf, lf) {
        (Some(c), Some(l)) => {
            if c <= l {
                Some((c, 4))
            } else {
                Some((l, 2))
            }
        }
        (Some(c), None) => Some((c, 4)),
        (None, Some(l)) => Some((l, 2)),
        (None, None) => None,
    }
}

/// Find the next part delimiter — a CRLF (or bare LF) immediately followed by
/// `--boundary` — returning the offset of the line ending.
fn find_next_delim(bytes: &[u8], delim: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = find(&bytes[from..], delim) {
        let at = from + rel;
        if at >= 2 && &bytes[at - 2..at] == b"\r\n" {
            return Some(at - 2);
        }
        if at >= 1 && bytes[at - 1] == b'\n' {
            return Some(at - 1);
        }
        from = at + delim.len();
    }
    None
}

/// First index at which `needle` occurs in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDARY: &str = "----AkurBoundary42";

    /// Build a well-formed body from `(headers, payload)` part specs.
    fn body(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (headers, payload) in parts {
            out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            out.extend_from_slice(headers.as_bytes());
            out.extend_from_slice(b"\r\n\r\n");
            out.extend_from_slice(payload);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        out
    }

    fn ctype() -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    #[test]
    fn parses_a_text_field_and_a_file_field() {
        let raw = body(&[
            ("Content-Disposition: form-data; name=\"greeting\"", b"hello world"),
            (
                "Content-Disposition: form-data; name=\"upload\"; filename=\"note.txt\"\r\nContent-Type: text/plain",
                b"file contents",
            ),
        ]);
        let parts = parse_multipart(&ctype(), &raw).unwrap();
        assert_eq!(parts.len(), 2);

        assert_eq!(parts[0].name, "greeting");
        assert_eq!(parts[0].filename, None);
        assert_eq!(parts[0].content_type, None);
        assert_eq!(parts[0].data, b"hello world");

        assert_eq!(parts[1].name, "upload");
        assert_eq!(parts[1].filename.as_deref(), Some("note.txt"));
        assert_eq!(parts[1].content_type.as_deref(), Some("text/plain"));
        assert_eq!(parts[1].data, b"file contents");
    }

    #[test]
    fn extracts_boundary_quoted_and_case_insensitively() {
        let raw = body(&[("Content-Disposition: form-data; name=\"a\"", b"1")]);
        let quoted = format!("multipart/form-data; BOUNDARY=\"{BOUNDARY}\"");
        let parts = parse_multipart(&quoted, &raw).unwrap();
        assert_eq!(parts[0].name, "a");
        assert_eq!(parts[0].data, b"1");
    }

    #[test]
    fn missing_boundary_is_an_error() {
        assert_eq!(
            parse_multipart("multipart/form-data", b"whatever"),
            Err(MultipartError::MissingBoundary)
        );
        assert_eq!(
            parse_multipart("application/json", b"{}"),
            Err(MultipartError::MissingBoundary)
        );
    }

    #[test]
    fn binary_payload_with_inner_crlf_round_trips() {
        // Bytes that include a CRLF and the boundary text as data must survive.
        let payload: Vec<u8> = vec![0x00, 0xff, b'\r', b'\n', 0x10, b'-', b'-', 0x42, 0x00];
        let raw = body(&[(
            "Content-Disposition: form-data; name=\"blob\"; filename=\"x.bin\"\r\nContent-Type: application/octet-stream",
            &payload,
        )]);
        let parts = parse_multipart(&ctype(), &raw).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].data, payload);
        assert_eq!(parts[0].filename.as_deref(), Some("x.bin"));
    }

    #[test]
    fn empty_payload_is_allowed() {
        let raw = body(&[("Content-Disposition: form-data; name=\"blank\"", b"")]);
        let parts = parse_multipart(&ctype(), &raw).unwrap();
        assert_eq!(parts[0].data, b"");
    }

    #[test]
    fn part_without_name_is_an_error() {
        let raw = body(&[("Content-Disposition: form-data", b"x")]);
        assert_eq!(
            parse_multipart(&ctype(), &raw),
            Err(MultipartError::MissingName)
        );
    }

    #[test]
    fn no_closing_delimiter_is_an_error() {
        let mut raw = Vec::new();
        raw.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        raw.extend_from_slice(b"Content-Disposition: form-data; name=\"a\"\r\n\r\n");
        raw.extend_from_slice(b"value with no terminator");
        assert_eq!(
            parse_multipart(&ctype(), &raw),
            Err(MultipartError::MalformedPart)
        );
    }

    #[test]
    fn no_opening_delimiter_is_an_error() {
        assert_eq!(
            parse_multipart(&ctype(), b"no boundary anywhere in here"),
            Err(MultipartError::NoOpeningBoundary)
        );
    }

    #[test]
    fn does_not_panic_on_junk() {
        // A pile of adversarial inputs must all return Result, never panic.
        let junk: &[&[u8]] = &[b"", b"--", b"------", &[0xff; 64], b"--?AkurBoundary42\r\n"];
        for input in junk {
            let _ = parse_multipart(&ctype(), input);
        }
        // Truncated mid-header.
        let mut t = Vec::new();
        t.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Dispo").as_bytes());
        let _ = parse_multipart(&ctype(), &t);
    }

    #[test]
    fn tolerates_unquoted_disposition_params() {
        let raw = body(&[("Content-Disposition: form-data; name=field", b"v")]);
        let parts = parse_multipart(&ctype(), &raw).unwrap();
        assert_eq!(parts[0].name, "field");
    }
}
