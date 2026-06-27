//! Cookie-based email-OTP session authentication on B+trees.
//!
//! This is the golfsetridak auth layer. It mirrors the AkurAI-Framework's
//! `crates/cli/src/auth.rs` pattern (B+tree session/OTP/role storage) and
//! the source app's auth model (email-OTP only, roles user/admin, 60-day
//! sessions, profile stored with the user record).
//!
//! ## Storage layout
//!
//! Three B+tree files live under `data/auth/`:
//!
//! | File | Key | Value |
//! |------|-----|-------|
//! | `sessions.db` | opaque session id | session JSON |
//! | `users.db` | normalized email | user record JSON |
//! | `otp.db` | normalized email | OTP record JSON |
//!
//! ## OTP delivery
//!
//! The code is delivered by `POST`-ing to the local sidecar at
//! `http://127.0.0.1:<SIDECAR_PORT>/email` with JSON body
//! `{"to","subject","html","text"}`. The sidecar does TLS; this side is
//! plaintext localhost. Port is read from `SIDECAR_PORT` env var. When the
//! env var is absent or empty, a [`LogDeliver`] logs the code to stdout
//! (suitable for development and tests).
//!
//! ## Security notes (same caveats as the framework reference)
//!
//! * **PRNG:** `SystemTime` nanos + pid + atomic counter through SplitMix64.
//!   Demo-grade; not a CSPRNG.
//! * **Hash:** salted, stretched SHA-1 (4096 rounds). Not bcrypt/argon2.
//! * **Cookie:** `HttpOnly; Path=/; SameSite=Lax`. Not `Secure` — TLS at edge.
//! * **Sessions:** 60-day TTL. Expiry is checked on every `current_user` call.

use akurai_json::Value;
use akurai_storage::BTree;
use akurai_ws::sha1::sha1;
use std::io::{self, Read as IoRead, Write as IoWrite};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use akurai_http::{form, Method, Reply, Request, Response};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Session cookie name.
const COOKIE_NAME: &str = "gsd_session";

/// Session TTL in seconds (60 days — matching the source app).
const SESSION_TTL_SECS: i64 = 60 * 24 * 3600;

/// OTP validity window (10 minutes).
const OTP_TTL_SECS: i64 = 600;

/// Maximum wrong guesses before an OTP is locked.
const OTP_MAX_ATTEMPTS: i64 = 5;

/// Digit count in a generated OTP.
const OTP_CODE_DIGITS: usize = 6;

/// SHA-1 stretch rounds.
const HASH_ROUNDS: usize = 4096;

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// Authorization role. `Admin` satisfies every role check; `User` is the
/// default for any new account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Role {
        if s == "admin" {
            Role::Admin
        } else {
            Role::User
        }
    }
}

// ---------------------------------------------------------------------------
// AuthUser
// ---------------------------------------------------------------------------

/// The authenticated identity behind a request. The `email` field doubles as
/// the booking-engine `user_id` (normalized, stable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub email: String,
    pub role: Role,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Auth state: three durable stores + pluggable OTP delivery.
/// Cheap to clone — every store is internally `Arc<Mutex<…>>`.
#[derive(Clone)]
pub struct State {
    sessions: SessionStore,
    users: UserStore,
    otp: OtpStore,
    deliver: Arc<dyn OtpDeliver>,
}

impl State {
    /// Open the auth stores under `data_dir`.
    ///
    /// OTP delivery is auto-selected: when `SIDECAR_PORT` is set to a valid
    /// port number, the [`SidecarDeliver`] is used; otherwise [`LogDeliver`]
    /// logs codes to stdout (dev/test mode).
    pub fn open(data_dir: &Path) -> io::Result<State> {
        let deliver: Arc<dyn OtpDeliver> = match std::env::var("SIDECAR_PORT")
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
        {
            Some(port) => Arc::new(SidecarDeliver { port }),
            None => Arc::new(LogDeliver::new()),
        };
        State::open_with_deliver(data_dir, deliver)
    }

    /// Open with an explicit delivery backend (tests, custom integrations).
    pub fn open_with_deliver(data_dir: &Path, deliver: Arc<dyn OtpDeliver>) -> io::Result<State> {
        std::fs::create_dir_all(data_dir)?;
        Ok(State {
            sessions: SessionStore::open(data_dir.join("sessions.db"))?,
            users: UserStore::open(data_dir.join("users.db"))?,
            otp: OtpStore::open(data_dir.join("otp.db"))?,
            deliver,
        })
    }

    // ---- session middleware ------------------------------------------------

    /// The authenticated user for this request, or `None` if unauthenticated
    /// or if the session has expired.
    pub fn current_user(&self, req: &Request) -> Option<AuthUser> {
        let cookie = req.header("Cookie")?;
        let id = parse_cookie(cookie, COOKIE_NAME)?;
        let session = self.sessions.lookup(&id).ok().flatten()?;
        if now_secs() > session.expires_at {
            let _ = self.sessions.delete(&id);
            return None;
        }
        let role = self.users.role_of(&session.email).unwrap_or(Role::User);
        Some(AuthUser {
            email: session.email,
            role,
        })
    }

    /// Whether the request's authenticated user satisfies `role`.
    #[allow(dead_code)]
    pub fn require_role(&self, req: &Request, role: Role) -> bool {
        match self.current_user(req).map(|u| u.role) {
            Some(Role::Admin) => true,
            Some(actual) => actual == role,
            None => false,
        }
    }

    // ---- OTP flow ---------------------------------------------------------

    pub fn request_otp(&self, email: &str) -> io::Result<()> {
        self.otp.request(email, self.deliver.as_ref())
    }

    pub fn verify_otp(&self, email: &str, code: &str) -> io::Result<OtpOutcome> {
        self.otp.verify(email, code)
    }

    /// Ensure a passwordless user account exists for `email`. Idempotent.
    pub fn ensure_account(&self, email: &str) -> io::Result<()> {
        self.users.ensure_account(&normalize_email(email))
    }

    // ---- session lifecycle ------------------------------------------------

    pub fn create_session(&self, email: &str) -> io::Result<String> {
        self.sessions.create(email)
    }

    pub fn delete_session(&self, req: &Request) -> io::Result<()> {
        if let Some(cookie) = req.header("Cookie") {
            if let Some(id) = parse_cookie(cookie, COOKIE_NAME) {
                let _ = self.sessions.delete(&id);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// Handle `GET /login` and `POST /login` (email → OTP request; email+code → verify).
pub fn login(
    state: &State,
    root: &Path,
    req: &Request,
    booking_store: &crate::booking::Store,
) -> Reply {
    if state.current_user(req).is_some() {
        return Reply::Response(redirect("/my"));
    }
    let response = match req.method {
        Method::Post => login_submit(state, root, req, booking_store),
        _ => render_login(root, "", "", false),
    };
    Reply::Response(response)
}

fn login_submit(
    state: &State,
    root: &Path,
    req: &Request,
    booking_store: &crate::booking::Store,
) -> Response {
    let pairs = form::parse_urlencoded(&req.body_str());
    let email_raw = form::field(&pairs, "email")
        .unwrap_or("")
        .trim()
        .to_string();
    let code = form::field(&pairs, "code").unwrap_or("").trim().to_string();

    if email_raw.is_empty() {
        return render_login(root, "Sláðu inn netfangið þitt.", "", false);
    }
    let email = normalize_email(&email_raw);

    // Verify step: both email and code present.
    if !code.is_empty() {
        return match state.verify_otp(&email, &code) {
            Ok(OtpOutcome::Verified) => {
                if let Err(e) = state.ensure_account(&email) {
                    return server_err_raw(&format!("could not create account: {e}"));
                }
                // Register in the booking store on first login (idempotent).
                let _ = booking_store.put_user(&email, None);
                match state.create_session(&email) {
                    Ok(id) => redirect("/my").with_header("Set-Cookie", &set_cookie(&id)),
                    Err(e) => server_err_raw(&format!("could not start session: {e}")),
                }
            }
            Ok(OtpOutcome::Expired) => render_login(
                root,
                "Kóðinn er útrunninn. Biðja um nýjan kóða.",
                &email,
                false,
            ),
            Ok(OtpOutcome::TooManyAttempts) => render_login(
                root,
                "Of margar tilraunir. Biðja um nýjan kóða.",
                &email,
                false,
            ),
            Ok(OtpOutcome::NoCode) => render_login(
                root,
                "Enginn kóði í bið. Biðja um kóða fyrst.",
                &email,
                false,
            ),
            Ok(OtpOutcome::WrongCode) => {
                render_login(root, "Rangur kóði. Reyndu aftur.", &email, true)
            }
            Err(e) => server_err_raw(&format!("verify failed: {e}")),
        };
    }

    // Request step: only email submitted, mint and deliver a code.
    match state.request_otp(&email) {
        Ok(()) => render_login(root, "", &email, true),
        Err(e) => server_err_raw(&format!("could not send code: {e}")),
    }
}

/// Handle `GET /logout` and `POST /logout`.
pub fn logout(state: &State, req: &Request) -> Reply {
    let _ = state.delete_session(req);
    Reply::Response(redirect("/login").with_header("Set-Cookie", &expired_cookie()))
}

/// `POST /api/auth/dev-login` — development-only OTP bypass (mirrors the source
/// `auth/dev-login`). Gated behind the `GOLF_DEV_LOGIN` env var: when unset
/// (production) it 404s as if the route did not exist, so it can never weaken
/// prod auth. Body `{ email }` → ensures the account, mints a session, sets the
/// cookie.
pub fn dev_login(state: &State, req: &Request, booking_store: &crate::booking::Store) -> Reply {
    let enabled = std::env::var("GOLF_DEV_LOGIN")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let json = |status: u16, body: &str| {
        Reply::Response(
            Response::new(status)
                .with_body("application/json; charset=utf-8", body.as_bytes().to_vec()),
        )
    };
    if !enabled {
        return json(404, "{\"error\":\"not found\"}");
    }
    if req.method != Method::Post {
        return json(405, "{\"error\":\"method not allowed\"}");
    }
    let body = akurai_json::parse(&req.body_str()).unwrap_or(Value::Object(vec![]));
    let email_raw = body
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if email_raw.is_empty() {
        return json(400, "{\"error\":\"email required\"}");
    }
    let email = normalize_email(&email_raw);
    if let Err(e) = state.ensure_account(&email) {
        return Reply::Response(server_err_raw(&format!("could not create account: {e}")));
    }
    let _ = booking_store.put_user(&email, None);
    match state.create_session(&email) {
        Ok(id) => Reply::Response(
            Response::new(200)
                .with_body("application/json; charset=utf-8", b"{\"ok\":true}".to_vec())
                .with_header("Set-Cookie", &set_cookie(&id)),
        ),
        Err(e) => Reply::Response(server_err_raw(&format!("could not start session: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Template rendering
// ---------------------------------------------------------------------------

fn render_login(root: &Path, error: &str, email: &str, sent: bool) -> Response {
    use crate::serve::{build_engine_pub, load_context_pub};
    let engine = match build_engine_pub(root) {
        Ok(e) => e,
        Err(msg) => return server_err_raw(&msg),
    };
    let mut context = load_context_pub(root);
    if let Value::Object(pairs) = &mut context {
        pairs.push(("error".into(), Value::Str(error.to_string())));
        pairs.push(("form_email".into(), Value::Str(email.to_string())));
        pairs.push(("sent".into(), bool_flag(sent)));
        pairs.push((
            "page_title".into(),
            Value::Str("Innskráning — Golfsetrið Akureyri".into()),
        ));
    }
    match engine.render("login", &context) {
        Ok(html) => Response::ok().with_html(&html),
        Err(e) => server_err_raw(&e.message),
    }
}

fn server_err_raw(msg: &str) -> Response {
    let mut escaped = String::new();
    for c in msg.chars() {
        match c {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            c => escaped.push(c),
        }
    }
    Response::new(500).with_html(&format!(
        "<!doctype html><meta charset=utf-8><title>Error</title>\
         <body style=\"font:16px system-ui;padding:40px\">\
         <h1>Server error</h1><pre>{escaped}</pre></body>"
    ))
}

fn redirect(location: &str) -> Response {
    Response::new(303).with_header("Location", location)
}

fn bool_flag(on: bool) -> Value {
    Value::Str(if on { "1" } else { "" }.to_string())
}

// ---------------------------------------------------------------------------
// Cookie helpers (public — used in tests)
// ---------------------------------------------------------------------------

pub fn parse_cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k.trim() == name {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

fn set_cookie(id: &str) -> String {
    format!("{COOKIE_NAME}={id}; HttpOnly; Path=/; SameSite=Lax")
}

pub fn expired_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0")
}

// ---------------------------------------------------------------------------
// Session store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    email: String,
    created: i64,
    expires_at: i64,
}

#[derive(Clone)]
struct SessionStore {
    tree: Arc<Mutex<BTree>>,
}

impl SessionStore {
    fn open(path: impl AsRef<Path>) -> io::Result<SessionStore> {
        Ok(SessionStore {
            tree: Arc::new(Mutex::new(BTree::open(path)?)),
        })
    }

    fn create(&self, email: &str) -> io::Result<String> {
        let id = random_id();
        let created = now_secs();
        let expires_at = created + SESSION_TTL_SECS;
        let record = Value::Object(vec![
            ("email".into(), Value::Str(email.to_string())),
            ("created".into(), Value::Int(created)),
            ("expires_at".into(), Value::Int(expires_at)),
        ]);
        let mut tree = self.tree.lock().unwrap();
        tree.insert(id.as_bytes(), record.to_json().as_bytes())?;
        tree.commit()?;
        Ok(id)
    }

    fn lookup(&self, id: &str) -> io::Result<Option<Session>> {
        let mut tree = self.tree.lock().unwrap();
        let Some(bytes) = tree.get(id.as_bytes())? else {
            return Ok(None);
        };
        Ok(decode_session(&bytes))
    }

    fn delete(&self, id: &str) -> io::Result<bool> {
        let mut tree = self.tree.lock().unwrap();
        let removed = tree.delete(id.as_bytes())?;
        tree.commit()?;
        Ok(removed)
    }
}

fn decode_session(bytes: &[u8]) -> Option<Session> {
    let text = std::str::from_utf8(bytes).ok()?;
    let value = akurai_json::parse(text).ok()?;
    let email = value.get("email")?.as_str()?.to_string();
    let created = value.get("created").and_then(Value::as_i64).unwrap_or(0);
    let expires_at = value
        .get("expires_at")
        .and_then(Value::as_i64)
        .unwrap_or(created + SESSION_TTL_SECS);
    Some(Session {
        email,
        created,
        expires_at,
    })
}

// ---------------------------------------------------------------------------
// User store
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct UserStore {
    tree: Arc<Mutex<BTree>>,
}

impl UserStore {
    fn open(path: impl AsRef<Path>) -> io::Result<UserStore> {
        Ok(UserStore {
            tree: Arc::new(Mutex::new(BTree::open(path)?)),
        })
    }

    /// Ensure a passwordless user record for `email` exists. Idempotent.
    fn ensure_account(&self, email: &str) -> io::Result<()> {
        let mut tree = self.tree.lock().unwrap();
        if tree.get(email.as_bytes())?.is_some() {
            return Ok(());
        }
        let record = Value::Object(vec![
            ("email".into(), Value::Str(email.to_string())),
            ("role".into(), Value::Str(Role::User.as_str().to_string())),
        ]);
        tree.insert(email.as_bytes(), record.to_json().as_bytes())?;
        tree.commit()?;
        Ok(())
    }

    /// The role of `email`, or `None` if there is no such record. A record with
    /// no `role` field (forward-compat) defaults to [`Role::User`].
    fn role_of(&self, email: &str) -> Option<Role> {
        let mut tree = self.tree.lock().unwrap();
        let bytes = tree.get(email.as_bytes()).ok()??;
        let text = std::str::from_utf8(&bytes).ok()?;
        let value = akurai_json::parse(text).ok()?;
        Some(
            value
                .get("role")
                .and_then(Value::as_str)
                .map(Role::parse)
                .unwrap_or(Role::User),
        )
    }
}

// ---------------------------------------------------------------------------
// OTP outcome + delivery trait
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtpOutcome {
    Verified,
    WrongCode,
    Expired,
    TooManyAttempts,
    NoCode,
}

pub trait OtpDeliver: Send + Sync {
    fn deliver(&self, email: &str, code: &str) -> io::Result<()>;
}

// ---- LogDeliver (dev / test) -----------------------------------------------

#[derive(Clone, Default)]
pub struct LogDeliver {
    delivered: Arc<Mutex<Vec<(String, String)>>>,
}

impl LogDeliver {
    pub fn new() -> LogDeliver {
        LogDeliver {
            delivered: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[allow(dead_code)]
    pub fn last_code(&self, email: &str) -> Option<String> {
        let em = normalize_email(email);
        let log = self.delivered.lock().unwrap();
        log.iter()
            .rev()
            .find(|(e, _)| *e == em)
            .map(|(_, c)| c.clone())
    }
}

impl OtpDeliver for LogDeliver {
    fn deliver(&self, email: &str, code: &str) -> io::Result<()> {
        println!("  [otp] {email}: {code}  (LogDeliver — set SIDECAR_PORT to wire real email)");
        self.delivered
            .lock()
            .unwrap()
            .push((email.to_string(), code.to_string()));
        Ok(())
    }
}

// ---- SidecarDeliver --------------------------------------------------------

/// Delivers OTP codes by POSTing to a local sidecar over plaintext TCP.
///
/// ## Sidecar contract
///
/// ```text
/// POST /email HTTP/1.1
/// Host: 127.0.0.1:<port>
/// Content-Type: application/json
///
/// {"to":"<email>","subject":"<subject>","html":"<html>","text":"<text>"}
/// ```
///
/// The sidecar must respond with a 2xx status. Any TCP error or non-2xx
/// response is returned as `io::Error`. The framework has no outbound TLS
/// client; this uses `std::net::TcpStream` — plaintext is safe for localhost.
pub struct SidecarDeliver {
    port: u16,
}

impl OtpDeliver for SidecarDeliver {
    fn deliver(&self, email: &str, code: &str) -> io::Result<()> {
        let subject = "Innskráningarkóði — Golfsetrið Akureyri";
        let html = format!(
            "<p>Innskráningarkóðinn þinn er: <strong>{code}</strong></p>\
             <p>Kóðinn rennur út eftir 10 mínútur.</p>"
        );
        let text =
            format!("Innskráningarkóðinn þinn er: {code}\nKóðinn rennur út eftir 10 mínútur.");

        let body = Value::Object(vec![
            ("to".into(), Value::Str(email.to_string())),
            ("subject".into(), Value::Str(subject.to_string())),
            ("html".into(), Value::Str(html)),
            ("text".into(), Value::Str(text)),
        ])
        .to_json();
        let body_bytes = body.as_bytes();

        let addr = format!("127.0.0.1:{}", self.port);
        let mut stream = TcpStream::connect(&addr)
            .map_err(|e| io::Error::new(e.kind(), format!("sidecar connect {addr}: {e}")))?;

        let request = format!(
            "POST /email HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n",
            port = self.port,
            len = body_bytes.len(),
        );
        stream.write_all(request.as_bytes())?;
        stream.write_all(body_bytes)?;
        stream.flush()?;

        // Read response header to check the status code.
        let mut resp = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                break;
            }
            resp.extend_from_slice(&buf[..n]);
            if resp.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        let status_line = std::str::from_utf8(&resp)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        let ok = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .map(|n| (200..300).contains(&n))
            .unwrap_or(false);

        if ok {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "sidecar rejected email: {status_line}"
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// OTP store
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct OtpStore {
    tree: Arc<Mutex<BTree>>,
}

struct OtpRecord {
    salt: String,
    code_hash: String,
    expires_at: i64,
    attempts: i64,
}

impl OtpStore {
    fn open(path: impl AsRef<Path>) -> io::Result<OtpStore> {
        Ok(OtpStore {
            tree: Arc::new(Mutex::new(BTree::open(path)?)),
        })
    }

    fn request(&self, email: &str, deliver: &dyn OtpDeliver) -> io::Result<()> {
        let email = normalize_email(email);
        let code = random_code();
        let salt = random_id();
        let record = Value::Object(vec![
            ("salt".into(), Value::Str(salt.clone())),
            ("code_hash".into(), Value::Str(hash_password(&salt, &code))),
            ("expires_at".into(), Value::Int(now_secs() + OTP_TTL_SECS)),
            ("attempts".into(), Value::Int(0)),
        ]);
        {
            let mut tree = self.tree.lock().unwrap();
            tree.insert(email.as_bytes(), record.to_json().as_bytes())?;
            tree.commit()?;
        }
        deliver.deliver(&email, &code)
    }

    fn verify(&self, email: &str, code: &str) -> io::Result<OtpOutcome> {
        let email = normalize_email(email);
        let code = code.trim();
        let mut tree = self.tree.lock().unwrap();

        let Some(bytes) = tree.get(email.as_bytes())? else {
            return Ok(OtpOutcome::NoCode);
        };
        let Some(rec) = decode_otp(&bytes) else {
            tree.delete(email.as_bytes())?;
            tree.commit()?;
            return Ok(OtpOutcome::NoCode);
        };

        if now_secs() > rec.expires_at {
            tree.delete(email.as_bytes())?;
            tree.commit()?;
            return Ok(OtpOutcome::Expired);
        }
        if rec.attempts >= OTP_MAX_ATTEMPTS {
            return Ok(OtpOutcome::TooManyAttempts);
        }

        if constant_time_eq(&hash_password(&rec.salt, code), &rec.code_hash) {
            tree.delete(email.as_bytes())?;
            tree.commit()?;
            return Ok(OtpOutcome::Verified);
        }

        let attempts = rec.attempts + 1;
        let updated = Value::Object(vec![
            ("salt".into(), Value::Str(rec.salt)),
            ("code_hash".into(), Value::Str(rec.code_hash)),
            ("expires_at".into(), Value::Int(rec.expires_at)),
            ("attempts".into(), Value::Int(attempts)),
        ]);
        tree.insert(email.as_bytes(), updated.to_json().as_bytes())?;
        tree.commit()?;

        if attempts >= OTP_MAX_ATTEMPTS {
            Ok(OtpOutcome::TooManyAttempts)
        } else {
            Ok(OtpOutcome::WrongCode)
        }
    }

    #[cfg(test)]
    fn seed_code(&self, email: &str, code: &str, expires_at: i64) -> io::Result<()> {
        let email = normalize_email(email);
        let salt = random_id();
        let record = Value::Object(vec![
            ("salt".into(), Value::Str(salt.clone())),
            ("code_hash".into(), Value::Str(hash_password(&salt, code))),
            ("expires_at".into(), Value::Int(expires_at)),
            ("attempts".into(), Value::Int(0)),
        ]);
        let mut tree = self.tree.lock().unwrap();
        tree.insert(email.as_bytes(), record.to_json().as_bytes())?;
        tree.commit()
    }
}

fn decode_otp(bytes: &[u8]) -> Option<OtpRecord> {
    let text = std::str::from_utf8(bytes).ok()?;
    let value = akurai_json::parse(text).ok()?;
    Some(OtpRecord {
        salt: value.get("salt")?.as_str()?.to_string(),
        code_hash: value.get("code_hash")?.as_str()?.to_string(),
        expires_at: value.get("expires_at").and_then(Value::as_i64).unwrap_or(0),
        attempts: value.get("attempts").and_then(Value::as_i64).unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Hash helpers
// ---------------------------------------------------------------------------

pub fn hash_password(salt: &str, password: &str) -> String {
    let mut digest = sha1(format!("{salt}:{password}").as_bytes());
    let salt_bytes = salt.as_bytes();
    for _ in 0..HASH_ROUNDS {
        let mut buf = Vec::with_capacity(digest.len() + salt_bytes.len());
        buf.extend_from_slice(&digest);
        buf.extend_from_slice(salt_bytes);
        digest = sha1(&buf);
    }
    to_hex(&digest)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ---------------------------------------------------------------------------
// Id / code generation
// ---------------------------------------------------------------------------

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut seed_a = nanos ^ pid.rotate_left(32);
    let mut seed_b = count
        .wrapping_add(nanos.rotate_left(17))
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let a = split_mix64(&mut seed_a);
    let b = split_mix64(&mut seed_b);
    format!("{a:016x}{b:016x}")
}

fn random_code() -> String {
    let id = random_id();
    let word = u32::from_str_radix(&id[..8], 16).unwrap_or(0);
    let n = word % 10u32.pow(OTP_CODE_DIGITS as u32);
    format!("{n:0width$}", width = OTP_CODE_DIGITS)
}

fn split_mix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(tag: &str) -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("auth-test-tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join(format!(
            "gsd-auth-{tag}-{}-{}",
            std::process::id(),
            ID_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn open_state(tag: &str) -> (State, LogDeliver) {
        let dir = scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let log = LogDeliver::new();
        let state = State::open_with_deliver(&dir, Arc::new(log.clone())).unwrap();
        (state, log)
    }

    fn fake_req(cookie: Option<&str>) -> Request {
        let head = match cookie {
            Some(c) => format!("GET / HTTP/1.1\r\nCookie: {c}\r\n\r\n"),
            None => "GET / HTTP/1.1\r\n\r\n".to_string(),
        };
        Request::parse_head(head.as_bytes()).unwrap()
    }

    // ---- OTP: mint / verify / expiry / attempt cap -------------------------

    #[test]
    fn otp_mint_verify_happy_path() {
        let (state, log) = open_state("otp-happy");
        let email = "golfer@example.com";
        state.request_otp(email).unwrap();
        let code = log.last_code(email).expect("code delivered");
        assert_eq!(code.len(), OTP_CODE_DIGITS);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(
            state.verify_otp(email, &code).unwrap(),
            OtpOutcome::Verified
        );
        // Single-use: second verify finds nothing.
        assert_eq!(state.verify_otp(email, &code).unwrap(), OtpOutcome::NoCode);
    }

    #[test]
    fn otp_expiry_clears_and_rejects() {
        let (state, _) = open_state("otp-expired");
        state
            .otp
            .seed_code("u@x.com", "123456", now_secs() - 1)
            .unwrap();
        assert_eq!(
            state.verify_otp("u@x.com", "123456").unwrap(),
            OtpOutcome::Expired
        );
        assert_eq!(
            state.verify_otp("u@x.com", "123456").unwrap(),
            OtpOutcome::NoCode
        );
    }

    #[test]
    fn otp_attempt_cap_locks_even_correct_code() {
        let (state, log) = open_state("otp-cap");
        state.request_otp("cap@x.com").unwrap();
        let code = log.last_code("cap@x.com").unwrap();
        for _ in 0..(OTP_MAX_ATTEMPTS - 1) {
            assert_eq!(
                state.verify_otp("cap@x.com", "000000").unwrap(),
                OtpOutcome::WrongCode
            );
        }
        assert_eq!(
            state.verify_otp("cap@x.com", "000000").unwrap(),
            OtpOutcome::TooManyAttempts
        );
        assert_eq!(
            state.verify_otp("cap@x.com", &code).unwrap(),
            OtpOutcome::TooManyAttempts,
            "cap locks out the correct code"
        );
    }

    #[test]
    fn otp_re_request_resets_attempts() {
        let (state, log) = open_state("otp-reset");
        state.request_otp("re@x.com").unwrap();
        let _ = state.verify_otp("re@x.com", "000000"); // burn one attempt
        state.request_otp("re@x.com").unwrap();
        let code = log.last_code("re@x.com").unwrap();
        assert_eq!(
            state.verify_otp("re@x.com", &code).unwrap(),
            OtpOutcome::Verified
        );
    }

    #[test]
    fn otp_unknown_email_is_no_code() {
        let (state, _) = open_state("otp-unknown");
        assert_eq!(
            state.verify_otp("ghost@x.com", "000000").unwrap(),
            OtpOutcome::NoCode
        );
    }

    // ---- Session: create / lookup / expiry ---------------------------------

    #[test]
    fn session_create_lookup_delete() {
        let (state, _) = open_state("sess-roundtrip");
        let id = state.create_session("a@b.com").unwrap();
        let hdr = format!("{COOKIE_NAME}={id}");
        let req = fake_req(Some(&hdr));
        let user = state.current_user(&req).expect("authenticated");
        assert_eq!(user.email, "a@b.com");
        assert_eq!(user.role, Role::User);
        state.sessions.delete(&id).unwrap();
        assert!(state.current_user(&req).is_none());
    }

    #[test]
    fn session_no_cookie_is_none() {
        let (state, _) = open_state("sess-none");
        assert!(state.current_user(&fake_req(None)).is_none());
    }

    #[test]
    fn session_expired_is_rejected() {
        let (state, _) = open_state("sess-expired");
        let id = random_id();
        let now = now_secs();
        let record = Value::Object(vec![
            ("email".into(), Value::Str("old@x.com".into())),
            ("created".into(), Value::Int(now - SESSION_TTL_SECS - 10)),
            ("expires_at".into(), Value::Int(now - 1)),
        ]);
        {
            let mut tree = state.sessions.tree.lock().unwrap();
            tree.insert(id.as_bytes(), record.to_json().as_bytes())
                .unwrap();
            tree.commit().unwrap();
        }
        let hdr = format!("{COOKIE_NAME}={id}");
        assert!(state.current_user(&fake_req(Some(&hdr))).is_none());
    }

    // ---- Role gating -------------------------------------------------------

    #[test]
    fn role_gating_user_vs_admin() {
        let dir = scratch_dir("role-gate");
        std::fs::create_dir_all(&dir).unwrap();
        let state = State::open_with_deliver(&dir, Arc::new(LogDeliver::new())).unwrap();

        state.ensure_account("user@x.com").unwrap();
        let uid = state.create_session("user@x.com").unwrap();
        let ureq = fake_req(Some(&format!("{COOKIE_NAME}={uid}")));
        assert!(state.require_role(&ureq, Role::User));
        assert!(!state.require_role(&ureq, Role::Admin));

        // Seed an admin record directly.
        {
            let rec = Value::Object(vec![
                ("email".into(), Value::Str("adm@x.com".into())),
                ("role".into(), Value::Str("admin".into())),
            ]);
            let mut tree = state.users.tree.lock().unwrap();
            tree.insert(b"adm@x.com", rec.to_json().as_bytes()).unwrap();
            tree.commit().unwrap();
        }
        let aid = state.create_session("adm@x.com").unwrap();
        let areq = fake_req(Some(&format!("{COOKIE_NAME}={aid}")));
        assert!(state.require_role(&areq, Role::Admin));
        assert!(
            state.require_role(&areq, Role::User),
            "admin satisfies user role"
        );

        assert!(!state.require_role(&fake_req(None), Role::User));
    }

    // ---- Hashing -----------------------------------------------------------

    #[test]
    fn hash_is_deterministic_and_distinct() {
        assert_eq!(hash_password("s", "pw"), hash_password("s", "pw"));
        assert_ne!(hash_password("s", "pw"), hash_password("s", "other"));
        assert_ne!(hash_password("a", "pw"), hash_password("b", "pw"));
    }

    #[test]
    fn hash_is_hex_40_chars_and_no_plaintext() {
        let h = hash_password("salt", "hunter2");
        assert_eq!(h.len(), 40);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!h.contains("hunter2"));
    }

    // ---- Cookie helpers ----------------------------------------------------

    #[test]
    fn parse_cookie_finds_named_value() {
        let hdr = "theme=dark; gsd_session=abc123; lang=en";
        assert_eq!(parse_cookie(hdr, "gsd_session").as_deref(), Some("abc123"));
        assert_eq!(parse_cookie(hdr, "theme").as_deref(), Some("dark"));
        assert_eq!(parse_cookie(hdr, "missing"), None);
    }

    #[test]
    fn set_and_expire_cookie_attributes() {
        let s = set_cookie("tok");
        assert!(s.starts_with("gsd_session=tok"));
        assert!(s.contains("HttpOnly"));
        assert!(s.contains("SameSite=Lax"));
        let e = expired_cookie();
        assert!(e.contains("gsd_session=;"));
        assert!(e.contains("Max-Age=0"));
    }
}
