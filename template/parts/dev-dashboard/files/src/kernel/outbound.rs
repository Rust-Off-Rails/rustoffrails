//! THE way this app calls another HTTP API (payments, shipping, a webhook, a partner's REST API):
//! a thin wrapper over one shared `reqwest::Client`. Every call gets
//!
//! - a **timeout** (`DEFAULT_TIMEOUT`, or the call's own; `without_timeout` is allowed and shows
//!   up as a "Missing timeout" insight on the dev dashboard),
//! - **bounded retries** for what is safe to retry (idempotent methods, or a call marked
//!   `idempotent()` because it carries an idempotency key), on timeouts, failed connections, 429,
//!   502, 503 and 504, with exponential backoff and jitter,
//! - a W3C **`traceparent`** header, so the API's own traces join this request's,
//! - a **span** (`http`, and one `http_attempt` per try when it retries) with the service, method,
//!   host, route template (`/items/{id}`: ids collapsed), status, outcome, attempts, timeout and
//!   bytes, which the dev dashboard turns into waterfall rows and its Upstreams view,
//! - a size cap on the answer (`MAX_BODY`).
//!
//! ```ignore
//! let payments = Outbound::new("payments", &config.payments_url)?.timeout(Duration::from_millis(800));
//! let reply = payments.post("/charges").bearer(&config.payments_key).json(&charge).idempotent().send().await?;
//! let charge: Charge = reply.error_for_status()?.json()?;
//! ```
//!
//! Never call `reqwest` directly from a controller, a model or a job: the call would be invisible
//! (no span, no Upstreams row), unbounded (no timeout) and unredacted.
//!
//! What it never records: query strings, userinfo, header values, request or response bodies.
//! The span and the dashboard see `host` and the route template only.
//!
//! GUIDE: .offrails/guides/errors-and-logging.md#logging

use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use reqwest::Method;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use tracing::Instrument;
use tracing::field::Empty;

/// A call without its own timeout waits at most this long. APP: your APIs' worst acceptable case.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Connecting (DNS, TCP, TLS) takes at most this long, whatever the call's timeout. APP.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Retries per call, at most, whatever a call asks for. APP.
pub const MAX_RETRIES: u32 = 3;
/// The first retry waits this long, each next one twice as long, plus up to 50% jitter. APP.
pub const BACKOFF: Duration = Duration::from_millis(100);
/// An answer larger than this is refused (`OutboundError::TooLarge`). APP.
pub const MAX_BODY: usize = 8 << 20;

// WHY one client: it pools connections (and TLS sessions) per host; a client per call reconnects
// every time.
// SECURITY: redirects are never followed: an API that redirects could send the request, and the
// header that authorizes it, to a host nobody chose (test: outbound_calls_are_traced_redacted_and_bounded).
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .unwrap_or_default()
});

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// An answer below 400.
    Ok,
    /// 4xx: the API refused (the caller's mistake, or a rate limit).
    ClientError,
    /// 5xx.
    ServerError,
    /// No answer within the timeout.
    Timeout,
    /// No connection (DNS, refused, TLS).
    Connect,
    /// The answer was over `MAX_BODY`.
    TooLarge,
    /// Anything else (a broken stream).
    Failed,
}

impl Outcome {
    pub fn key(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::ClientError => "client_error",
            Outcome::ServerError => "server_error",
            Outcome::Timeout => "timeout",
            Outcome::Connect => "connect",
            Outcome::TooLarge => "too_large",
            Outcome::Failed => "failed",
        }
    }

    /// Counts as a failed call (a 4xx other than 429 is the caller's own mistake).
    pub fn is_error(self, status: Option<u16>) -> bool {
        match self {
            Outcome::Ok => false,
            Outcome::ClientError => status == Some(429),
            _ => true,
        }
    }

    fn of_status(status: u16) -> Self {
        match status {
            500.. => Outcome::ServerError,
            400..=499 => Outcome::ClientError,
            _ => Outcome::Ok,
        }
    }

    /// Worth another try: the API may answer next time.
    fn retryable(self, status: Option<u16>) -> bool {
        match self {
            Outcome::Timeout | Outcome::Connect => true,
            Outcome::ServerError | Outcome::ClientError => {
                matches!(status, Some(429 | 502 | 503 | 504))
            }
            _ => false,
        }
    }
}

/// What a finished call reports to the observer (the dev dashboard's Upstreams view).
#[derive(Clone, Debug)]
pub struct CallRecord {
    pub service: &'static str,
    pub method: &'static str,
    /// `api.example.com` or `127.0.0.1:3000`: never userinfo.
    pub host: String,
    /// The path with ids collapsed (`/items/{id}`), never a query string.
    pub route: String,
    pub status: Option<u16>,
    pub outcome: Outcome,
    pub attempts: u32,
    /// `None`: the call had no timeout at all.
    pub timeout: Option<Duration>,
    pub dur: Duration,
    pub bytes: u64,
}

static OBSERVER: OnceLock<fn(&CallRecord)> = OnceLock::new();
static TRACE_ID: OnceLock<fn() -> Option<String>> = OnceLock::new();

/// Registers the function told about every finished call (the dev dashboard does, at boot).
pub fn observe(f: fn(&CallRecord)) {
    let _ = OBSERVER.set(f);
}

/// Reports a call made some other way (a vendor SDK with its own client) as if it went through
/// here: the Upstreams view counts it.
pub fn report(record: &CallRecord) {
    if let Some(f) = OBSERVER.get() {
        f(record);
    }
}

/// Registers where the current trace id comes from (32 lowercase hex digits): the dev dashboard
/// gives the request id, so an API's trace and this request's share one id.
pub fn trace_ids(f: fn() -> Option<String>) {
    let _ = TRACE_ID.set(f);
}

/// Why a call failed. The messages name the service, never the URL.
#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("{service}: invalid URL")]
    Url { service: &'static str },
    #[error("{service}: no answer within {after:?} ({attempts} attempts)")]
    Timeout {
        service: &'static str,
        after: Duration,
        attempts: u32,
    },
    #[error("{service}: could not connect ({attempts} attempts)")]
    Connect {
        service: &'static str,
        attempts: u32,
    },
    #[error("{service}: answered {status}")]
    Status { service: &'static str, status: u16 },
    #[error("{service}: the answer is over {MAX_BODY} bytes")]
    TooLarge { service: &'static str },
    #[error("{service}: {message}")]
    Failed {
        service: &'static str,
        message: String,
    },
    #[error("{service}: the answer isn't the JSON expected: {message}")]
    Json {
        service: &'static str,
        message: String,
    },
}

/// One API: its name, root URL and defaults.
#[derive(Clone)]
pub struct Outbound {
    service: &'static str,
    base: String,
    host: String,
    timeout: Option<Duration>,
    retries: u32,
    client: reqwest::Client,
}

impl Outbound {
    /// A client for the API at `base` (from config: https in production), named `service` on
    /// every page that shows it.
    // SECURITY: a base URL carrying `user:password@` is refused: credentials go in a header
    // (`bearer`, `header`), which is never recorded (test: outbound_calls_are_traced_redacted_and_bounded).
    pub fn new(service: &'static str, base: &str) -> Result<Self, OutboundError> {
        let url = reqwest::Url::parse(base).map_err(|_| OutboundError::Url { service })?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(OutboundError::Url { service });
        }
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            (None, _) => return Err(OutboundError::Url { service }),
        };
        Ok(Self {
            service,
            base: base.trim_end_matches('/').to_owned(),
            host,
            timeout: Some(DEFAULT_TIMEOUT),
            retries: 0,
            client: CLIENT.clone(),
        })
    }

    /// Every call of this client waits at most `t` (connect, send and the whole answer).
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    /// No timeout at all: a hung API holds the request (and a connection) forever. Allowed, and
    /// flagged by the dashboard's "Missing timeout" insight.
    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// Retries per call for this client (capped at `MAX_RETRIES`; only safe calls retry).
    pub fn retries(mut self, n: u32) -> Self {
        self.retries = n.min(MAX_RETRIES);
        self
    }

    /// Another `reqwest::Client` (a proxy, a client certificate). Keep it free of redirects.
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn service(&self) -> &'static str {
        self.service
    }

    pub fn get(&self, path: &str) -> Call<'_> {
        self.request(Method::GET, path)
    }

    pub fn post(&self, path: &str) -> Call<'_> {
        self.request(Method::POST, path)
    }

    pub fn put(&self, path: &str) -> Call<'_> {
        self.request(Method::PUT, path)
    }

    pub fn delete(&self, path: &str) -> Call<'_> {
        self.request(Method::DELETE, path)
    }

    /// `path` is relative to the base URL (`/items/42?expand=1`).
    pub fn request(&self, method: Method, path: &str) -> Call<'_> {
        Call {
            out: self,
            method,
            path: path.to_owned(),
            headers: HeaderMap::new(),
            body: None,
            timeout: self.timeout,
            retries: self.retries,
            idempotent: false,
        }
    }
}

/// One request being built.
pub struct Call<'a> {
    out: &'a Outbound,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Option<Duration>,
    retries: u32,
    idempotent: bool,
}

impl Call<'_> {
    /// A header. Its value is never recorded anywhere.
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        if let Ok(mut v) = HeaderValue::from_str(value) {
            v.set_sensitive(true);
            self.headers.insert(HeaderName::from_static(name), v);
        }
        self
    }

    /// `Authorization: Bearer …`, marked sensitive (never in a log or a `Debug`).
    pub fn bearer(mut self, token: &str) -> Self {
        if let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {token}")) {
            v.set_sensitive(true);
            self.headers.insert(reqwest::header::AUTHORIZATION, v);
        }
        self
    }

    /// A JSON body (`Content-Type: application/json`).
    pub fn json<T: serde::Serialize + ?Sized>(mut self, body: &T) -> Self {
        if let Ok(bytes) = serde_json::to_vec(body) {
            self.headers
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            self.body = Some(bytes);
        }
        self
    }

    /// Raw bytes with their content type.
    pub fn body(mut self, content_type: &'static str, bytes: Vec<u8>) -> Self {
        self.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        self.body = Some(bytes);
        self
    }

    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    pub fn retries(mut self, n: u32) -> Self {
        self.retries = n.min(MAX_RETRIES);
        self
    }

    /// Safe to send twice (the API dedupes it, e.g. by an `Idempotency-Key` header), so a POST
    /// may retry too.
    pub fn idempotent(mut self) -> Self {
        self.idempotent = true;
        self
    }

    /// Sends it, retrying what is safe to retry; the answer, whatever its status (see
    /// `Reply::error_for_status`), or why there is none.
    pub async fn send(self) -> Result<Reply, OutboundError> {
        let service = self.out.service;
        let method = method_str(&self.method);
        let route = route_template(&self.path);
        let url = format!("{}{}", self.out.base, self.path);
        let url = reqwest::Url::parse(&url).map_err(|_| OutboundError::Url { service })?;
        let safe = self.idempotent
            || matches!(
                self.method,
                Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
            );
        let attempts_max = 1 + if safe {
            self.retries.min(MAX_RETRIES)
        } else {
            0
        };
        let span = tracing::info_span!(
            "http",
            service = service,
            http.method = method,
            http.host = %self.out.host,
            http.route = %route,
            timeout_ms = self.timeout.map_or(0, |t| t.as_millis() as u64),
            retries = attempts_max - 1,
            http.status = Empty,
            outcome = Empty,
            attempts = Empty,
            bytes = Empty,
        );
        let started = Instant::now();
        let (result, attempts) = self
            .attempts(&url, attempts_max, &span)
            .instrument(span.clone())
            .await;
        let (status, outcome, bytes) = match &result {
            Ok(r) => (Some(r.status), Outcome::of_status(r.status), r.body.len()),
            Err(e) => (None, e.1, 0),
        };
        if let Some(s) = status {
            span.record("http.status", s);
        }
        span.record("outcome", outcome.key());
        span.record("attempts", attempts);
        span.record("bytes", bytes as u64);
        if let Some(f) = OBSERVER.get() {
            f(&CallRecord {
                service,
                method,
                host: self.out.host.clone(),
                route,
                status,
                outcome,
                attempts,
                timeout: self.timeout,
                dur: started.elapsed(),
                bytes: bytes as u64,
            });
        }
        result.map_err(|e| e.0)
    }

    /// Tries up to `max` times; the last result and how many tries it took.
    async fn attempts(
        &self,
        url: &reqwest::Url,
        max: u32,
        call: &tracing::Span,
    ) -> (Result<Reply, (OutboundError, Outcome)>, u32) {
        let mut n = 0;
        loop {
            n += 1;
            let span = (max > 1).then(|| {
                tracing::info_span!(
                    "http_attempt",
                    attempt = n,
                    http.status = Empty,
                    outcome = Empty
                )
            });
            let parent = span.as_ref().unwrap_or(call);
            let result = match &span {
                Some(s) => self.once(url, parent).instrument(s.clone()).await,
                None => self.once(url, parent).await,
            };
            let (status, outcome) = match &result {
                Ok(r) => (Some(r.status), Outcome::of_status(r.status)),
                Err((_, o)) => (None, *o),
            };
            if let Some(s) = &span {
                if let Some(st) = status {
                    s.record("http.status", st);
                }
                s.record("outcome", outcome.key());
            }
            if n >= max || !outcome.retryable(status) {
                return (result, n);
            }
            let wait = BACKOFF.saturating_mul(1 << (n - 1).min(8));
            let jitter = wait.mul_f64(rand::random::<f64>() * 0.5);
            tokio::time::sleep(wait + jitter).await;
        }
    }

    async fn once(
        &self,
        url: &reqwest::Url,
        span: &tracing::Span,
    ) -> Result<Reply, (OutboundError, Outcome)> {
        let service = self.out.service;
        let mut req = self
            .out
            .client
            .request(self.method.clone(), url.clone())
            .headers(self.headers.clone())
            .header("traceparent", traceparent(span));
        if let Some(t) = self.timeout {
            req = req.timeout(t);
        }
        if let Some(b) = &self.body {
            req = req.body(b.clone());
        }
        let failed = |e: reqwest::Error| {
            if e.is_timeout() {
                (
                    OutboundError::Timeout {
                        service,
                        after: self.timeout.unwrap_or_default(),
                        attempts: 1,
                    },
                    Outcome::Timeout,
                )
            } else if e.is_connect() {
                (
                    OutboundError::Connect {
                        service,
                        attempts: 1,
                    },
                    Outcome::Connect,
                )
            } else {
                // SECURITY: reqwest's message quotes the URL and its query; it is dropped (test: outbound_calls_are_traced_redacted_and_bounded).
                (
                    OutboundError::Failed {
                        service,
                        message: crate::kernel::redact::text(&e.without_url().to_string()),
                    },
                    Outcome::Failed,
                )
            }
        };
        let mut res = req.send().await.map_err(failed)?;
        if res.content_length().is_some_and(|n| n as usize > MAX_BODY) {
            return Err((OutboundError::TooLarge { service }, Outcome::TooLarge));
        }
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(failed)? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err((OutboundError::TooLarge { service }, Outcome::TooLarge));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Reply {
            service,
            status,
            headers,
            body: Bytes::from(body),
        })
    }
}

/// An API's answer, read whole (at most `MAX_BODY`).
#[derive(Debug)]
pub struct Reply {
    service: &'static str,
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The answer, or `OutboundError::Status` for a 4xx or 5xx.
    pub fn error_for_status(self) -> Result<Self, OutboundError> {
        if self.status >= 400 {
            Err(OutboundError::Status {
                service: self.service,
                status: self.status,
            })
        } else {
            Ok(self)
        }
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, OutboundError> {
        serde_json::from_slice(&self.body).map_err(|e| OutboundError::Json {
            service: self.service,
            message: e.to_string(),
        })
    }
}

fn method_str(m: &Method) -> &'static str {
    match *m {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// `/items/42/reviews?page=2` → `/items/{id}/reviews`: numbers, UUIDs and long random-looking
/// segments become `{id}`; the query string is dropped.
// SECURITY: a query string (an API key, a signed URL's signature) never reaches a span, a page or
// the dashboard's maps; ids don't fan the Upstreams table out into one row per id
// (test: route_templates_collapse_ids_and_drop_queries).
pub fn route_template(path: &str) -> String {
    let bare = path.split(['?', '#']).next().unwrap_or_default();
    let segs: Vec<String> = bare
        .split('/')
        .map(|s| {
            let idish = !s.is_empty()
                && (s.bytes().all(|b| b.is_ascii_digit())
                    || (s.len() == 36 && s.bytes().filter(|b| *b == b'-').count() == 4)
                    || (s.len() >= 16
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                        && s.bytes().any(|b| b.is_ascii_digit())));
            if idish {
                "{id}".to_owned()
            } else {
                s.to_owned()
            }
        })
        .collect();
    let out = segs.join("/");
    let out = if out.is_empty() { "/".to_owned() } else { out };
    crate::kernel::ui::truncate(&out, 120)
}

/// `00-<trace id>-<span id>-01`: the current trace's id (or a fresh one) and this attempt's span.
fn traceparent(span: &tracing::Span) -> String {
    let trace = TRACE_ID
        .get()
        .and_then(|f| f())
        .filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| format!("{:032x}", rand::random::<u128>().max(1)));
    let id = span
        .id()
        .map(|i| i.into_u64())
        .unwrap_or_else(|| rand::random::<u64>().max(1));
    format!("00-{}-{id:016x}-01", trace.to_ascii_lowercase())
}

/// The trace id a `traceparent` header carries (`None` when malformed).
pub fn trace_id_of(traceparent: &str) -> Option<&str> {
    let mut parts = traceparent.split('-');
    let (version, trace, parent, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (version.len() == 2
        && trace.len() == 32
        && parent.len() == 16
        && flags.len() == 2
        && trace.bytes().all(|b| b.is_ascii_hexdigit())
        && trace.bytes().any(|b| b != b'0'))
    .then_some(trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_templates_collapse_ids_and_drop_queries() {
        assert_eq!(route_template("/items/42?key=s3cret"), "/items/{id}");
        assert_eq!(
            route_template("/users/0b8a7e2c-1f2d-4c3b-9a8e-7d6c5b4a3f21/orders"),
            "/users/{id}/orders"
        );
        assert_eq!(route_template("/charge"), "/charge");
        assert_eq!(route_template(""), "/");
    }

    #[test]
    fn traceparents_parse() {
        let tp = traceparent(&tracing::Span::none());
        assert!(trace_id_of(&tp).is_some(), "{tp}");
        assert!(trace_id_of("00-0000-1-01").is_none());
    }
}
