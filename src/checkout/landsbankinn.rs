//! Landsbankinn (Verifone) payment-gateway client — over the local plaintext
//! sidecar.
//!
//! The framework binary has no outbound TLS client (same invariant as the rest
//! of the app). So, exactly like `auth::SidecarDeliver`, every call to the
//! payment gateway is a plaintext `POST`/`GET` to a local sidecar at
//! `127.0.0.1:<SIDECAR_PORT>` that performs the real TLS request upstream.
//!
//! ## Sidecar contract
//!
//! ```text
//! POST /payment/landsbankinn/checkout
//! {"amount":<int>,"currency":"ISK","orderRef":"<cartId>",
//!  "returnUrl":"<url>","cancelUrl":"<url>"}
//!   -> 2xx {"id":"<checkoutId>","url":"<redirectUrl>"}
//!
//! GET /payment/landsbankinn/checkout/<checkoutId>
//!   -> 2xx {"status":"succeeded"|"failed"|"pending"}
//! ```
//!
//! When `SIDECAR_PORT` is absent the client runs in **mock mode**: checkout
//! returns a synthetic id and routes the redirect straight back at the callback
//! (so the success path is exercisable end-to-end), and the outcome query always
//! resolves `succeeded`. This mirrors how `auth` degrades to `LogDeliver`.

use akurai_json::Value;
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// A created checkout session: where to send the browser, plus the gateway id we
/// persist on the payment row to reconcile the callback.
pub struct CheckoutSession {
    pub id: String,
    pub redirect_url: String,
    pub mock: bool,
}

/// The terminal (or not-yet-terminal) state of a checkout, mirroring the
/// source's `CheckoutOutcome.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Pending,
}

/// The sidecar port, when configured to a valid `u16`.
fn sidecar_port() -> Option<u16> {
    std::env::var("SIDECAR_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
}

/// Open a checkout session. POSTs to the sidecar, or mocks when none is wired.
pub fn create_checkout(
    amount: i64,
    currency: &str,
    order_ref: &str,
    return_url: &str,
    cancel_url: &str,
) -> Result<CheckoutSession, String> {
    match sidecar_port() {
        Some(port) => {
            let body = Value::Object(vec![
                ("amount".into(), Value::Int(amount)),
                ("currency".into(), Value::Str(currency.to_string())),
                ("orderRef".into(), Value::Str(order_ref.to_string())),
                ("returnUrl".into(), Value::Str(return_url.to_string())),
                ("cancelUrl".into(), Value::Str(cancel_url.to_string())),
            ])
            .to_json();
            let resp =
                sidecar_request(port, "POST", "/payment/landsbankinn/checkout", Some(&body))?;
            let v = akurai_json::parse(&resp)
                .map_err(|_| "invalid sidecar checkout response".to_string())?;
            let id = v
                .get("id")
                .and_then(Value::as_str)
                .ok_or("sidecar checkout response missing id")?
                .to_string();
            let url = v
                .get("url")
                .and_then(Value::as_str)
                .ok_or("sidecar checkout response missing url")?
                .to_string();
            Ok(CheckoutSession {
                id,
                redirect_url: url,
                mock: false,
            })
        }
        None => {
            // Mock: no gateway. Send the browser straight to the callback so the
            // success path is fully exercisable in dev/tests.
            Ok(CheckoutSession {
                id: format!("mock-{order_ref}"),
                redirect_url: return_url.to_string(),
                mock: true,
            })
        }
    }
}

/// Query the gateway for a checkout's outcome. Mocks `Succeeded` with no sidecar.
pub fn checkout_outcome(checkout_id: &str) -> Result<Outcome, String> {
    match sidecar_port() {
        Some(port) => {
            let path = format!("/payment/landsbankinn/checkout/{checkout_id}");
            let resp = sidecar_request(port, "GET", &path, None)?;
            let v = akurai_json::parse(&resp)
                .map_err(|_| "invalid sidecar outcome response".to_string())?;
            Ok(classify(
                v.get("status").and_then(Value::as_str).unwrap_or(""),
            ))
        }
        None => Ok(Outcome::Succeeded),
    }
}

/// Map a gateway status string to an [`Outcome`]. Accepts both the sidecar's
/// normalized words and the raw Verifone statuses from the source client.
fn classify(status: &str) -> Outcome {
    let s = status.trim().to_ascii_uppercase();
    match s.as_str() {
        "SUCCEEDED"
        | "SUCCESS"
        | "AUTHORIZED"
        | "AUTHORISED"
        | "COMPLETED"
        | "SETTLEMENT_REQUESTED"
        | "SETTLEMENT_SUBMITTED"
        | "SETTLEMENT_COMPLETED" => Outcome::Succeeded,
        "FAILED" | "DECLINED" | "CANCELLED" | "CANCELED" | "EXPIRED" => Outcome::Failed,
        _ => Outcome::Pending,
    }
}

/// One plaintext HTTP/1.1 request/response against the local sidecar. Copies the
/// exact outbound pattern of `auth::SidecarDeliver`, but reads the whole body
/// (Connection: close) and returns it as a string.
fn sidecar_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let addr = format!("127.0.0.1:{port}");
    let mut stream =
        TcpStream::connect(&addr).map_err(|e| format!("sidecar connect {addr}: {e}"))?;

    let body_bytes: &[u8] = body.map(str::as_bytes).unwrap_or(&[]);
    let head = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body_bytes.len(),
    );
    write_all(&mut stream, head.as_bytes())?;
    if !body_bytes.is_empty() {
        write_all(&mut stream, body_bytes)?;
    }
    stream.flush().map_err(|e| format!("sidecar flush: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("sidecar read: {e}"))?;
    let text = String::from_utf8_lossy(&raw);

    // Reject non-2xx so a sidecar error never masquerades as a missing field.
    let status_ok = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .map(|n| (200..300).contains(&n))
        .unwrap_or(false);
    if !status_ok {
        let line = text.lines().next().unwrap_or("");
        return Err(format!("sidecar returned non-2xx: {line}"));
    }

    Ok(text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default())
}

fn write_all(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), String> {
    io::Write::write_all(stream, bytes).map_err(|e| format!("sidecar write: {e}"))
}
