# Errors & Logging

> **TL;DR**
> - One `AppError` enum in `src/error.rs` maps to responses per flavor (HTML, htmx, Inertia, JSON); handlers use `?`, never `unwrap`.
> - Records the user may not know exist are `NotFound`, not `Forbidden`; unique violations go through `sql_err()`, never message matching.
> - Panics render the standalone 500 page (`CatchPanicLayer::custom`); error pages are standalone templates with no DB access that load `app.js` for Back/Forward.
> - Log once, in `IntoResponse`, with `tracing` (JSON in production); `/up` uses `kernel::db::ping`.
>
> Read the rest when adding an error variant, changing error pages, or wiring logging.

**Use the part.** The `dev-dashboard` [parts](samples.md#the-samples) are this guide built, tested on SQLite and Postgres and security-reviewed, with more on top: traces as waterfalls, a live log tail and insights that rank what's failing, in development; and the `jobs-dashboard` sample groups job errors by root cause. When the product needs this, add the part rather than writing it from this guide: `cargo offrails add-part dev-dashboard` (dependencies come first; [workflow](../workflow.md#when-the-product-grows-a-need-add-the-part)). Keep its `// SECURITY:` markers and the tests its CHECKS.md cites. This guide stays the spec: hand-write from it only when no part fits, or the human declined the part.

| Rails | Off Rails |
|-------|-----------|
| `rescue_from` / exceptions app | `AppError` + `impl IntoResponse` in `src/error.rs` |
| `ActiveRecord::RecordNotFound` → 404 | `AppError::NotFound` (and `DbErr::RecordNotFound` maps to it) |
| `ActiveRecord::RecordInvalid` → 422 | `AppError::Invalid(Errors)` |
| `public/404.html`, `500.html` | `templates/errors/404.html`, `500.html` (htmx); `errors/Show` page (inertia); JSON (api) |
| `Rails.logger` | `tracing` |
| `config.log_tags = [:request_id]` | `TraceLayer` span with `request_id` |
| `/up` | `GET /up`: `kernel::db::ping` (`SELECT 1`), returns 200 or 503 |
| Sentry/Honeybadger gems | Only if the interview or a spec asks (log the decision) |

## `src/error.rs`

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]              NotFound,
    #[error("unauthorized")]           Unauthorized,          // not logged in
    #[error("forbidden")]              Forbidden,             // logged in, not allowed
    #[error("invalid")]                Invalid(Errors),       // validation failed
    #[error("too many requests")]      TooManyRequests,      // kernel/rate_limit.rs
    #[error("unavailable")]            Unavailable,          // overloaded: pool acquire timed out, live re-fetch gate full
    #[error(transparent)]              Db(DbErr),
    #[error(transparent)]              Template(#[from] askama::Error),
    #[error(transparent)]              Internal(#[from] anyhow::Error),
}

impl From<DbErr> for AppError {
    fn from(e: DbErr) -> Self {
        match e {
            DbErr::RecordNotFound(_) => AppError::NotFound,
            // The pool had no free connection within acquire_timeout (5 s): overloaded, not broken.
            DbErr::ConnectionAcquire(ConnAcquireErr::Timeout) => AppError::Unavailable,
            other => AppError::Db(other),
        }
    }
}
// + From<serde_json::Error> for AppError → Internal
```

## Response mapping

| Variant | htmx (HTML) | inertia | `/api/*` (JSON) |
|---------|-------------|---------|-----------------|
| `NotFound` | 404 + `errors/404.html` | 404 `errors/Show` `{status: 404}` | 404 `{"error":"not_found"}` |
| `Unauthorized` | 303 → `paths::new_session()` (htmx request: `HX-Redirect` header) | 409 + `X-Inertia-Location: /session/new` (or 303 for a full visit) | 401 `{"error":"unauthorized"}` |
| `Forbidden` | 403 + `errors/403.html` | 403 `errors/Show` | 403 `{"error":"forbidden"}` |
| `Invalid(e)` | 422 + the `errors/_invalid.html` fragment, a list of full messages (fallback; handlers normally re-render the form themselves) | errors map → `_errors` cookie + **303 redirect back** to `Referer` (fallback: the form's GET path); see frontend-inertia | 422 `{"errors": {"field": ["msg"]}}` |
| `TooManyRequests` | 429 | 429 | 429 + `Retry-After` |
| `Unavailable` | 503 + `Retry-After: 1` + `errors/500.html` with "busy, retry" wording (htmx fragment requests: empty body) | 503 `errors/Show` | 503 + `Retry-After: 1` `{"error":"unavailable"}` |

**Overload is 503 + `Retry-After`, never 408 and never a 30 s hang.** Three things produce it: the pool's `acquire_timeout` (5 s, `kernel::db::connect`), the live re-fetch gate ([realtime](realtime.md#keep-re-fetches-from-starving-user-requests)), and the request `TimeoutLayer` (30 s, `StatusCode::SERVICE_UNAVAILABLE`). The timeout's response comes from tower-http with no headers, so `error::retry_after` (an `axum::middleware::map_response` just outside the timeout, [routing](routing.md#middleware)) adds `Retry-After: 1` to any 503 that lacks one. 408 means the client was too slow to send its request; under a thundering herd the server was the slow one, and a 408 made browsers and proxies treat it as the client's fault. Test: with the gate full, a live re-fetch gets 503 with `Retry-After`; `From<DbErr>` maps `ConnectionAcquire(Timeout)` to `Unavailable` (a unit test).
| `Db`, `Template`, `Internal` | 500 + `errors/500.html` | 500 `errors/Show` | 500 `{"error":"internal"}` |

**The router fallback is marked.** `controllers::errors::not_found` (the router's `.fallback`) returns `([("x-route-fallback", "1")], AppError::NotFound)`, and `shape_errors` copies that header onto the page it renders. A handler's own 404 (no such record) carries no marker. `security_test.rs`'s routability check uses this to tell "not routed" from "not found" for parameterized paths too.

- **How it's wired** (verified in the reference app). `IntoResponse` can't see the request, so:
  1. `impl IntoResponse for AppError` returns a bare status response carrying a private `ErrorReport { status, errors: Option<Errors>, detail: Option<String> }` in its **response extensions**. It logs the 500s here (once).
  2. `pub async fn shape_errors(State(state): State<AppState>, req: Request, next: Next) -> Response`, mounted with `from_fn_with_state`, classifies the request into a `ResponseFlavor`:
     - `Api` when the path starts with `/api` **or `/streams/`, or `Accept` contains `text/event-stream`**. A logged-out stream must get a 401, not a 303 to the login page: `EventSource` can't follow a redirect to HTML (it errors and retries forever), and the stream path would become the `return_to` page after login;
     - `Inertia` when `X-Inertia` is present;
     - `Htmx` when `HX-Request` is present;
     - `Html` otherwise.

     It stores the flavor as a request extension, runs the request, and rewrites any response carrying an `ErrorReport` into the flavor's response from the table above: a JSON body, the `_errors` cookie + 303, `HX-Redirect`, or an error page.
  3. It sits **outside** the CSRF layer. tower-http's `CsrfLayer::with_rejection_response(|_| AppError::Forbidden.into_response())` makes CSRF rejections flow through the same shaping.

  Handlers just return `AppError`. This is the one place error responses are shaped.
- **Error pages never touch the DB** and never need the layout's current user. They're standalone Askama templates: `templates/errors/{404,403,500}.html`, with no `{% extends %}` and no `ctx`. The page links `/static/app.css` without a digest, and loads `/static/app.js` (`defer`), which reloads on Back/Forward:

  ```js
  // static/app.js: error pages have no htmx, so the browser's history needs a nudge
  window.addEventListener("pageshow", (e) => { if (e.persisted) location.reload(); });
  window.addEventListener("popstate", () => { if (!window.htmx) location.reload(); });
  ```

  Without it, going Forward from a 404 changes the URL but not the page. `500.html` takes `detail: Option<String>`, which is set only in development.
- **Never leak internals.** 500 bodies are generic in every environment. Development may add the error chain to the HTML 500 page when `config.env == Development`, and nowhere else.
- **Log every 500** at `error!` with the full chain (`{err:#}` / `?err`) inside the request span, so it carries the `request_id`. Log 4xx at `debug!`, or not at all.
- Panics: `tower_http::catch_panic::CatchPanicLayer::custom(error::panic_response)` (feature `catch-panic`), so a panic becomes a logged 500 rendered with the standalone `errors/500.html` page, not a dropped connection or a bare text body. It sits outside `shape_errors`, so `shape_errors` can't style it: `panic_response(Box<dyn Any + Send>) -> Response<Body>` logs the panic message (`String` or `&str` payload) at error level and renders the page itself, without detail.

## Logging

`main.rs` initializes `tracing-subscriber` once, as a registry with room for the layers an optional [sample](samples.md) adds (the dev dashboard's log tail and traces) beside the formatter:

```rust
use tracing_subscriber::{EnvFilter, Layer, Registry, layer::SubscriberExt, util::SubscriberInitExt};

fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(match config.env {
            AppEnv::Production => "info",
            _ => "info,myapp=debug,tower_http=debug",
        })
    });
    let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![
        // samples: tracing layers
    ];
    let registry = tracing_subscriber::registry().with(sample_layers(layers)).with(filter);
    match config.log_format {
        LogFormat::Json => registry.with(tracing_subscriber::fmt::layer().json()).init(),
        LogFormat::Pretty => registry.with(tracing_subscriber::fmt::layer()).init(),
    }
}

/// The samples' layers, or none at all.
// WHY: an empty `Vec` layer answers `Interest::never()` for every callsite, which silences the
// whole subscriber (no log line at all, warnings included); `None` is a true no-op
// (test: an_app_without_sample_layers_still_logs).
fn sample_layers(
    layers: Vec<Box<dyn Layer<Registry> + Send + Sync>>,
) -> Option<Vec<Box<dyn Layer<Registry> + Send + Sync>>> {
    (!layers.is_empty()).then_some(layers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_without_sample_layers_still_logs() {
        let subscriber = tracing_subscriber::registry()
            .with(sample_layers(Vec::new()))
            .with(EnvFilter::new("info"));
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::enabled!(tracing::Level::WARN));
            assert!(!tracing::enabled!(tracing::Level::DEBUG));
        });
    }
}
```

Never pass the `Vec` itself to `.with(…)`: with no layers in it, the app logs **nothing**, not even the boot warnings (found in the samples' host app, whose log went silent; the test above fails on that version). Verified with tracing-subscriber 0.3.23, 2026-10-16.

- `TraceLayer::new_for_http().make_span_with(...)` creates a span per request with `method`, `path` (the matched route, not the raw URI with ids, when available), and `request_id` from `SetRequestIdLayer` (`MakeRequestUuid`) + `PropagateRequestIdLayer` (header `x-request-id`).
- **Log structured events, not prose:** `info!(post_id = post.id, user_id = user.id, "post published")`.
- Workers: one span per job with `job_id`, `worker`, `queue`, `attempt` ([jobs](jobs.md#observability)).
- **Never log** passwords, session tokens, reset tokens, `Authorization`/`Cookie` headers, full email bodies, or `DATABASE_URL`/`SMTP_URL` (they contain credentials). Implement `Debug` manually for `Config` (an allowlist: [config](config.md#srcconfigrs)) and for any struct holding a secret.
- SeaORM SQL logging stays off (`sqlx_logging(false)`) except while debugging locally.

### Redacting stored error text

Error messages quote what failed, and what failed is often a URL with its credentials (`https://deploy:hunter2@api.example.com/v1/hooks?token=…`), or an emailed link with its token. Logging them is bad enough; **storing** them (a job's `errors`, shown on `/admin/jobs`, searched, printed by `app jobs show`) keeps the secret for weeks and shows it to every admin. Text that is stored or shown after the fact goes through `kernel::redact::text` first. `src/kernel/redact.rs`, whole (from the samples' host app, trimmed to free text; compiled, clippy-clean and tested with Rust 1.99, edition 2024, 2026-10-16):

```rust
//! Free text with secrets replaced by `[redacted]`, for text that is stored or shown after the
//! fact (a job's failure message, an error shown on a dashboard): a URL's `user:password@`, its
//! random-looking path segments and secret query values; `key=value` pairs whose key names a
//! secret (`password=…`, `api_key=…`); long random-looking words (tokens).
//! GUIDE: .offrails/guides/errors-and-logging.md#redacting-stored-error-text

/// What a secret becomes.
pub const REDACTED: &str = "[redacted]";

/// APP: names whose value is always a secret, whatever it looks like (`x_token` matches `token`).
pub const SECRET_PARAMS: &[&str] = &[
    "token", "code", "key", "secret", "password", "signature", "sig", "otp",
];

/// A word at least this long that looks random (base64url or hex, with a digit or a capital) is
/// treated as a token.
const RANDOM_MIN_LEN: usize = 20;

// SECURITY: errors often quote the URL that failed, with its credentials; stored or shown error
// text goes through here first (test: secrets_in_free_text_are_redacted).
pub fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word = String::new();
    for c in s.chars() {
        if c.is_whitespace()
            || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '`')
        {
            out.push_str(&word_redacted(&word));
            word.clear();
            out.push(c);
        } else {
            word.push(c);
        }
    }
    out.push_str(&word_redacted(&word));
    out
}

fn word_redacted(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    if let Some((scheme, rest)) = word.split_once("://") {
        let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(split);
        let host = match authority.rsplit_once('@') {
            Some((_, host)) => format!("{REDACTED}@{host}"),
            None => authority.to_owned(),
        };
        let (before_fragment, fragment) = match tail.split_once('#') {
            Some((b, f)) => (b, Some(f)),
            None => (tail, None),
        };
        let (p, q) = match before_fragment.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (before_fragment, None),
        };
        let mut out = format!("{scheme}://{host}{}", path(p));
        if let Some(q) = q {
            out.push('?');
            out.push_str(&query(q));
        }
        if let Some(f) = fragment {
            out.push('#');
            out.push_str(&query(f));
        }
        return out;
    }
    if let Some((k, v)) = word.split_once(['=', ':'])
        && !k.is_empty()
        && !v.is_empty()
        && (is_secret_param(k.trim_start_matches(['-', '?', '&'])) || looks_random(v))
    {
        let sep = &word[k.len()..k.len() + 1];
        return format!("{k}{sep}{REDACTED}");
    }
    if looks_random(word.trim_end_matches(['.', ':'])) {
        return REDACTED.to_owned();
    }
    word.to_owned()
}

/// A URL path with random-looking segments (tokens in emailed links) redacted.
fn path(path: &str) -> String {
    path.split('/')
        .map(|s| if looks_random(s) { REDACTED } else { s })
        .collect::<Vec<_>>()
        .join("/")
}

fn query(q: &str) -> String {
    q.split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) if is_secret_param(k) || looks_random(v) => format!("{k}={REDACTED}"),
            Some(_) => pair.to_owned(),
            None if looks_random(pair) => REDACTED.to_owned(),
            None => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn is_secret_param(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_PARAMS.iter().any(|p| name == *p || name.ends_with(&format!("_{p}")))
}

/// Long, URL-safe, and not a lowercase slug: base64url tokens, hex digests, signed blobs.
fn looks_random(s: &str) -> bool {
    s.len() >= RANDOM_MIN_LEN
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'%'))
        && s.bytes().any(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "q8Zr2mX0kLwP4vN7tY1sB9cD3fG6hJ5aE2uI8oQ4rTw";

    #[test]
    fn secrets_in_free_text_are_redacted() {
        let shown = text(&format!(
            "GET https://deploy:hunter2@api.example.com/v1/hooks?token=abc123&page=2 failed: 401 (key {TOKEN}); retry with password=swordfish"
        ));
        for secret in ["deploy", "hunter2", "abc123", TOKEN, "swordfish"] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
        assert_eq!(
            shown,
            "GET https://[redacted]@api.example.com/v1/hooks?token=[redacted]&page=2 failed: 401 (key [redacted]); retry with password=[redacted]"
        );
        assert_eq!(text(&format!("https://app.example/login_links/{TOKEN}")), "https://app.example/login_links/[redacted]");
        // Ordinary text is untouched, and redacting twice changes nothing.
        let plain = "charging card 42: connection refused (os error 61) at hooks.partner.invalid:443";
        assert_eq!(text(plain), plain);
        assert_eq!(text(&shown), shown);
        assert_eq!(text("postgres://u:p@db/app"), "postgres://[redacted]@db/app");
    }
}
```

It's for text, after the fact. It never replaces the rules above: secrets stay out of job args, logs and error pages to begin with.

## Spans

**The rule (one, everywhere: the guides, the generators, the snippets, the parts' READMEs):**

| What | Attribute | Why |
|------|-----------|-----|
| a controller action, a public model or service fn, a job's `perform`, a mailer fn | `#[tracing::instrument(skip_all)]` (the default level, INFO) | one or a few per request: production traces name the work |
| a helper called many times per request (in a loop, per row), or a step only worth seeing while debugging | `#[tracing::instrument(level = "debug", skip_all)]` | free in production (the filter is `info`: the span is never built); the dev dashboard shows it |
| anything | never without `skip_all` | arguments are secrets and personal data more often than not |

So each request's traces (the `dev-dashboard` part's waterfalls, any OpenTelemetry layer) name the work it did. The template's actions follow it; so do generated code and the snippets. Add a field only when it's safe and useful, e.g. `#[tracing::instrument(skip_all, fields(id))]` for a record id. **Never put secrets or personal data in span fields** (passwords, tokens, emails, names, request bodies): `skip_all` is the default for that reason, and span fields reach logs and dashboards.

## Health check

`GET /up` (in `controllers/health.rs`) calls `kernel::db::ping(&db)` (a `SELECT 1`; raw SQL stays in the kernel) with a 2s timeout and returns `200 "ok"`, or `503` when the database doesn't answer or migrations are pending ([dev-tools](dev-tools.md#pending-migrations)). It doesn't require auth, CSRF, sessions, or logging at info level. Deploy healthchecks point here.

## Gotchas agents get wrong

- `.unwrap()` on a query result in a handler, which turns a 404 into a panic. Use `?` and `.ok_or(AppError::NotFound)`.
- Returning `Forbidden` for records the user shouldn't know exist. Return `NotFound` when existence itself is private (other users' records).
- Matching on `DbErr` message strings. Use `sql_err()` (see models.md).
- Logging the same error twice (once in the handler, once in `IntoResponse`). Log in one place: `IntoResponse`.
- `anyhow` in model signatures. Models return `AppError`. `anyhow` is for jobs, tasks, `main`, and the `Internal` variant.
