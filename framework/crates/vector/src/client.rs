//! The embedding client: fetch embeddings from an OpenAI-compatible endpoint.
//!
//! Endpoint and model are always parameters — never hardcoded. The CLI reads
//! `AKURAI_EMBED_URL` / `AKURAI_EMBED_MODEL` from the environment and passes
//! them here. When no endpoint is configured the CLI falls back to substring
//! search; that fallback lives in the CLI, not in this crate.

use akurai_json::{parse, Value};

use crate::error::EmbedError;
use crate::http::{build_request, build_request_with_headers, fetch, parse_endpoint};

/// The `input` field of an embeddings request: one string or a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedInput {
    Single(String),
    Many(Vec<String>),
}

impl EmbedInput {
    /// Serialize the full request body `{"model": ..., "input": ...}` using
    /// `akurai_json` so strings are escaped correctly. Keys are emitted in
    /// insertion order: `model` then `input`.
    pub fn to_json_body(&self, model: &str) -> String {
        let input = match self {
            EmbedInput::Single(t) => Value::Str(t.clone()),
            EmbedInput::Many(ts) => {
                Value::Array(ts.iter().map(|t| Value::Str(t.clone())).collect())
            }
        };
        Value::Object(vec![
            ("model".into(), Value::Str(model.to_string())),
            ("input".into(), input),
        ])
        .to_json()
    }

    /// How many embeddings this input should produce.
    fn expected_len(&self) -> usize {
        match self {
            EmbedInput::Single(_) => 1,
            EmbedInput::Many(ts) => ts.len(),
        }
    }
}

/// Embed a single string. POSTs to `<endpoint>/v1/embeddings` and returns the
/// embedding vector.
pub fn embed(endpoint: &str, model: &str, text: &str) -> Result<Vec<f32>, EmbedError> {
    let input = EmbedInput::Single(text.to_string());
    let mut out = run(endpoint, model, &input)?;
    out.pop()
        .ok_or_else(|| EmbedError::UnexpectedShape("no embedding returned".into()))
}

/// Embed a single string while sending `Authorization: Bearer <token>`.
pub fn embed_with_bearer(
    endpoint: &str,
    model: &str,
    text: &str,
    token: &str,
) -> Result<Vec<f32>, EmbedError> {
    let input = EmbedInput::Single(text.to_string());
    let mut out = run_with_bearer(endpoint, model, &input, token)?;
    out.pop()
        .ok_or_else(|| EmbedError::UnexpectedShape("no embedding returned".into()))
}

/// Embed many strings in one request. Returns one vector per input, in order.
pub fn embed_many(
    endpoint: &str,
    model: &str,
    texts: &[&str],
) -> Result<Vec<Vec<f32>>, EmbedError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let input = EmbedInput::Many(texts.iter().map(|t| t.to_string()).collect());
    run(endpoint, model, &input)
}

/// Embed many strings while sending `Authorization: Bearer <token>`.
pub fn embed_many_with_bearer(
    endpoint: &str,
    model: &str,
    texts: &[&str],
    token: &str,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let input = EmbedInput::Many(texts.iter().map(|t| t.to_string()).collect());
    run_with_bearer(endpoint, model, &input, token)
}

/// Shared path: parse endpoint → build request → round-trip → parse body →
/// verify count.
fn run(endpoint: &str, model: &str, input: &EmbedInput) -> Result<Vec<Vec<f32>>, EmbedError> {
    let ep = parse_endpoint(endpoint)?;
    let request = build_request(&ep.host, ep.port, model, input);
    let body = fetch(&ep, &request)?;
    let embeddings = parse_embeddings_response(&body)?;
    let expected = input.expected_len();
    if embeddings.len() != expected {
        return Err(EmbedError::CountMismatch {
            expected,
            got: embeddings.len(),
        });
    }
    Ok(embeddings)
}

fn run_with_bearer(
    endpoint: &str,
    model: &str,
    input: &EmbedInput,
    token: &str,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    if token.trim().is_empty() {
        return run(endpoint, model, input);
    }
    let ep = parse_endpoint(endpoint)?;
    let auth = format!("Bearer {}", token.trim());
    let request =
        build_request_with_headers(&ep.host, ep.port, model, input, &[("Authorization", &auth)]);
    let body = fetch(&ep, &request)?;
    let embeddings = parse_embeddings_response(&body)?;
    let expected = input.expected_len();
    if embeddings.len() != expected {
        return Err(EmbedError::CountMismatch {
            expected,
            got: embeddings.len(),
        });
    }
    Ok(embeddings)
}

/// Parse an OpenAI-compatible embeddings response body:
/// `{"data":[{"embedding":[...]}, ...]}`.
///
/// Pure (no I/O), so it's unit-tested directly against sample strings.
pub fn parse_embeddings_response(body: &str) -> Result<Vec<Vec<f32>>, EmbedError> {
    let value = parse(body).map_err(|e| EmbedError::MalformedJson(e.to_string()))?;
    let data = value
        .get("data")
        .ok_or_else(|| EmbedError::UnexpectedShape("missing `data` field".into()))?;
    let items = match data {
        Value::Array(items) => items,
        _ => return Err(EmbedError::UnexpectedShape("`data` is not an array".into())),
    };

    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let embedding = item
            .get("embedding")
            .ok_or_else(|| EmbedError::UnexpectedShape(format!("data[{i}] missing `embedding`")))?;
        let nums = match embedding {
            Value::Array(nums) => nums,
            _ => {
                return Err(EmbedError::UnexpectedShape(format!(
                    "data[{i}].embedding is not an array"
                )))
            }
        };
        let mut vec = Vec::with_capacity(nums.len());
        for n in nums {
            let f = n.as_f64().ok_or_else(|| {
                EmbedError::UnexpectedShape(format!("data[{i}].embedding has a non-number"))
            })?;
            vec.push(f as f32);
        }
        out.push(vec);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_body_serializes_in_order() {
        let body = EmbedInput::Single("hej \"þú\"".into()).to_json_body("m");
        assert_eq!(body, r#"{"model":"m","input":"hej \"þú\""}"#);
    }

    #[test]
    fn many_body_serializes_array() {
        let body = EmbedInput::Many(vec!["a".into(), "b".into()]).to_json_body("gemma");
        assert_eq!(body, r#"{"model":"gemma","input":["a","b"]}"#);
    }

    #[test]
    fn parses_single_embedding() {
        let body = r#"{"object":"list","data":[{"embedding":[0.1,0.2,-0.3]}]}"#;
        let got = parse_embeddings_response(body).unwrap();
        assert_eq!(got.len(), 1);
        assert!((got[0][0] - 0.1).abs() < 1e-6);
        assert!((got[0][2] - (-0.3)).abs() < 1e-6);
    }

    #[test]
    fn parses_batch_embeddings() {
        let body = r#"{"data":[{"embedding":[1,2]},{"embedding":[3,4]}]}"#;
        let got = parse_embeddings_response(body).unwrap();
        assert_eq!(got, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }

    #[test]
    fn parses_integer_and_float_components() {
        // JSON ints must coerce to f32 just like floats.
        let body = r#"{"data":[{"embedding":[0,1.5,2]}]}"#;
        let got = parse_embeddings_response(body).unwrap();
        assert_eq!(got[0], vec![0.0, 1.5, 2.0]);
    }

    #[test]
    fn empty_data_is_ok_but_empty() {
        let got = parse_embeddings_response(r#"{"data":[]}"#).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn malformed_json_errs() {
        assert!(matches!(
            parse_embeddings_response("{not json"),
            Err(EmbedError::MalformedJson(_))
        ));
        assert!(matches!(
            parse_embeddings_response(""),
            Err(EmbedError::MalformedJson(_))
        ));
    }

    #[test]
    fn missing_data_field_errs() {
        assert!(matches!(
            parse_embeddings_response(r#"{"object":"list"}"#),
            Err(EmbedError::UnexpectedShape(_))
        ));
    }

    #[test]
    fn wrong_shape_errs() {
        assert!(matches!(
            parse_embeddings_response(r#"{"data":"nope"}"#),
            Err(EmbedError::UnexpectedShape(_))
        ));
        assert!(matches!(
            parse_embeddings_response(r#"{"data":[{"embedding":"x"}]}"#),
            Err(EmbedError::UnexpectedShape(_))
        ));
        assert!(matches!(
            parse_embeddings_response(r#"{"data":[{"embedding":["a"]}]}"#),
            Err(EmbedError::UnexpectedShape(_))
        ));
        assert!(matches!(
            parse_embeddings_response(r#"{"data":[{}]}"#),
            Err(EmbedError::UnexpectedShape(_))
        ));
    }

    #[test]
    fn embed_many_empty_short_circuits_without_network() {
        // No endpoint is touched for an empty batch.
        assert_eq!(
            embed_many("http://0.0.0.0:1", "m", &[]).unwrap(),
            Vec::<Vec<f32>>::new()
        );
        assert_eq!(
            embed_many_with_bearer("http://0.0.0.0:1", "m", &[], "tok").unwrap(),
            Vec::<Vec<f32>>::new()
        );
    }

    #[test]
    fn embed_rejects_https_without_network() {
        assert!(matches!(
            embed("https://secure:443", "m", "hi"),
            Err(EmbedError::TlsUnsupported(_))
        ));
    }
}
