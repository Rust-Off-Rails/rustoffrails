# Errors & Logging

| Rails | Off Rails |
|-------|-----------|
| `rescue_from` / exceptions app | `AppError` + `impl IntoResponse` in `src/error.rs` |
| `ActiveRecord::RecordNotFound` → 404 | `AppError::NotFound` (and `DbErr::RecordNotFound` maps to it) |
| `ActiveRecord::RecordInvalid` → 422 | `AppError::Invalid(Errors)` |
| `public/404.html`, `500.html` | `templates/errors/404.html`, `500.html` (htmx); `errors/Show` page (inertia); JSON (api) |
| `Rails.logger` | `tracing` |
| `config.log_tags = [:request_id]` | `TraceLayer` span with `request_id` |
| `/up` | `GET /up`: runs `SELECT 1`, returns 200 or 503 |
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
    #[error(transparent)]              Db(DbErr),
    #[error(transparent)]              Template(#[from] askama::Error),
    #[error(transparent)]              Internal(#[from] anyhow::Error),
}

impl From<DbErr> for AppError {
    fn from(e: DbErr) -> Self {
        match e {
            DbErr::RecordNotFound(_) => AppError::NotFound,
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
| `Db`, `Template`, `Internal` | 500 + `errors/500.html` | 500 `errors/Show` | 500 `{"error":"internal"}` |

**The router fallback is marked.** `controllers::errors::not_found` (the router's `.fallback`) returns `([("x-route-fallback", "1")], AppError::NotFound)`, and `shape_errors` copies that header onto the page it renders. A handler's own 404 (no such record) carries no marker. `security_test.rs`'s routability check uses this to tell "not routed" from "not found" for parameterized paths too.

- **How it's wired** (verified in the reference app). `IntoResponse` can't see the request, so:
  1. `impl IntoResponse for AppError` returns a bare status response carrying a private `ErrorReport { status, errors: Option<Errors>, detail: Option<String> }` in its **response extensions**. It logs the 500s here (once).
  2. `pub async fn shape_errors(State(state): State<AppState>, req: Request, next: Next) -> Response`, mounted with `from_fn_with_state`, classifies the request into a `ResponseFlavor`:
     - `Api` when the path starts with `/api`;
     - `Inertia` when `X-Inertia` is present;
     - `Htmx` when `HX-Request` is present;
     - `Html` otherwise.

     It stores the flavor as a request extension, runs the request, and rewrites any response carrying an `ErrorReport` into the flavor's response from the table above: a JSON body, the `_errors` cookie + 303, `HX-Redirect`, or an error page.
  3. It sits **outside** the CSRF layer. tower-http's `CsrfLayer::with_rejection_response(|_| AppError::Forbidden.into_response())` makes CSRF rejections flow through the same shaping.

  Handlers just return `AppError`. This is the one place error responses are shaped.
- **Error pages never touch the DB** and never need the layout's current user. They're standalone Askama templates: `templates/errors/{404,403,500}.html`, with no `{% extends %}` and no `ctx`. The page links `/static/app.css` without a digest. `500.html` takes `detail: Option<String>`, which is set only in development.
- **Never leak internals.** 500 bodies are generic in every environment. Development may add the error chain to the HTML 500 page when `config.env == Development`, and nowhere else.
- **Log every 500** at `error!` with the full chain (`{err:#}` / `?err`) inside the request span, so it carries the `request_id`. Log 4xx at `debug!`, or not at all.
- Panics: add `tower_http::catch_panic::CatchPanicLayer` (feature `catch-panic`) so a panic becomes a logged 500, not a dropped connection.

## Logging

`main.rs` initializes `tracing-subscriber` once:

```rust
let filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new(match config.env {
        AppEnv::Production => "info",
        _ => "info,myapp=debug,tower_http=debug",
    }));
match config.log_format {
    LogFormat::Json   => tracing_subscriber::fmt().json().with_env_filter(filter).init(),
    LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
}
```

- `TraceLayer::new_for_http().make_span_with(...)` creates a span per request with `method`, `path` (the matched route, not the raw URI with ids, when available), and `request_id` from `SetRequestIdLayer` (`MakeRequestUuid`) + `PropagateRequestIdLayer` (header `x-request-id`).
- **Log structured events, not prose:** `info!(post_id = post.id, user_id = user.id, "post published")`.
- Workers: one span per job with `job_id`, `worker`, `queue`, `attempt` ([jobs](jobs.md#observability)).
- **Never log** passwords, session tokens, reset tokens, `Authorization`/`Cookie` headers, full email bodies, or `DATABASE_URL`/`SMTP_URL` (they contain credentials). Implement `Debug` manually (redacted) for `Config` and for any struct holding a secret.
- SeaORM SQL logging stays off (`sqlx_logging(false)`) except while debugging locally.

## Health check

`GET /up` (in `controllers/health.rs`) runs `db.execute_unprepared("SELECT 1")` with a 2s timeout and returns `200 "ok"` or `503`. It doesn't require auth, CSRF, sessions, or logging at info level. Deploy healthchecks point here.

## Gotchas agents get wrong

- `.unwrap()` on a query result in a handler, which turns a 404 into a panic. Use `?` and `.ok_or(AppError::NotFound)`.
- Returning `Forbidden` for records the user shouldn't know exist. Return `NotFound` when existence itself is private (other users' records).
- Matching on `DbErr` message strings. Use `sql_err()` (see models.md).
- Logging the same error twice (once in the handler, once in `IntoResponse`). Log in one place: `IntoResponse`.
- `anyhow` in model signatures. Models return `AppError`. `anyhow` is for jobs, tasks, `main`, and the `Internal` variant.
