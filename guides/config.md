# Config, Environments & Secrets

> **TL;DR**
> - One `Config` struct parsed from env vars once (`Config::from_env`); never `std::env::var` deep in the code.
> - `APP_ENV` = development | test | production; `.env` is loaded only in development; `.env.example` lists every variable with dev defaults (no real secrets).
> - In development `APP_URL` follows `PORT`; normalize it (no trailing slash).
> - Secrets live in the deploy platform's secret store, never in git or images.
>
> Read the rest when adding a setting, a secret, or environment-specific behavior.

| Rails | Off Rails |
|-------|-----------|
| `RAILS_ENV` | `APP_ENV` = `development` \| `test` \| `production` |
| `config/environments/*.rb` | One `match config.env` per concern, inside `Config::from_env` or the kernel module that cares |
| `database.yml` | `DATABASE_URL` (+ `DATABASE_POOL`) |
| `credentials.yml.enc` + master key | Env vars, set through the deploy platform's secret store |
| `Rails.application.config.x.foo` | A typed field on `Config` |
| `.env` (dotenv-rails) | `dotenvy`, loaded **only** in development |

## `src/config.rs`

```rust
/// The human product name: page titles, kernel UIs (/dev, /admin/jobs), the default MAIL_FROM.
/// Never derived from the crate name (env!("CARGO_PKG_NAME")): crates get renamed, products don't.
pub const APP_NAME: &str = "Bookshelf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv { Development, Test, Production }

#[derive(Debug, Clone)]
pub struct Config {
    pub env: AppEnv,
    pub database_url: String,
    pub database_pool: u32,           // DATABASE_POOL, default 10
    pub app_url: String,              // APP_URL, absolute, no trailing slash: used in emails and the origin check
    pub port: u16,                    // PORT, default 3000
    pub inline_worker: bool,          // APP_INLINE_WORKER: default true in dev, and in prod on SQLite
    pub queues: Vec<(String, usize)>, // APP_QUEUES, default "default:10,mailers:5" (queue:concurrency per node)
    pub jobs_prune_days: u32,         // APP_JOBS_PRUNE_DAYS, default 7: how long finished jobs stay visible
    pub jobs_shutdown_grace: u64,     // APP_JOBS_SHUTDOWN_GRACE seconds, default 25
    pub shutdown_drain: Duration,     // APP_SHUTDOWN_DRAIN_SECS, default 5: after the signal, open connections get this long, then are dropped (realtime.md)
    pub refresh_wait: Duration,       // not an env var: 5 s (Config::test(): 100 ms); how long a live re-fetch waits at the gate before 503 (realtime.md)
    pub admin_password: Option<String>, // APP_ADMIN_PASSWORD: HTTP Basic for /admin/jobs when the app has no admin role
    pub smtp_url: Option<String>,     // SMTP_URL: production delivery (ignored in development unless MAILER=smtp)
    pub mailer: Option<String>,       // MAILER = smtp | local | log: overrides the per-environment default (mailers.md)
    pub mail_from: String,            // MAIL_FROM, default "<App> <no-reply@localhost>"
    pub trusted_proxy: bool,          // APP_TRUSTED_PROXY: trust the rightmost X-Forwarded-For hop (rate limiting)
    pub log_format: LogFormat,        // LOG_FORMAT = pretty | json (default: json in production)
    pub revision: Option<String>,     // APP_REVISION: git sha of the build, shown on /dev/system
    pub tls: Option<TlsFiles>, // APP_TLS_CERT + APP_TLS_KEY (PEM paths): `serve` speaks HTTPS + h2 itself (deployment.md#direct-tls-optional); both or neither, one alone fails the boot.
    pub dev_http_port: Option<u16>, // APP_DEV_HTTP_PORT: development only (any other APP_ENV fails the boot); with TLS on, the same router on plain HTTP beside it (dev-tools.md#https-in-development)
                               // The development APP_URL default follows it: https://localhost:{PORT} when set (CSRF compares origins).
    // apps with an API (api.md):
    // pub cors_origins: Vec<String>, // APP_CORS_ORIGINS: exact origins, comma-separated; `*` fails at boot (config::parse_cors_origins)
    // added with the first attachment field (storage.md):
    // pub storage: StorageConfig,    // STORAGE_SERVICE, STORAGE_PATH, STORAGE_BUCKET, STORAGE_ENDPOINT, STORAGE_REGION, STORAGE_PUBLIC_HOST
    // app-specific settings: APP_* prefixed, typed, documented in .env.example
}

impl Config {
    /// Called once in main. Development: `dotenvy::dotenv().ok()` first.
    /// Collects ALL missing/invalid vars, then fails with one message listing them.
    pub fn from_env() -> anyhow::Result<Self>;
    pub fn test() -> Self;            // the test suite's config: APP_ENV=test, sqlite::memory:, never reads env
    pub fn is_production(&self) -> bool;
    pub fn is_development(&self) -> bool;
}

/// "default:10,mailers:5" → [("default", 10), ("mailers", 5)]; also used by `app worker --queues`.
pub fn parse_queues(s: &str) -> Result<Vec<(String, usize)>, String>;
```

- `Config` holds credentials, so implement `Debug` by hand and print `[redacted]` for `database_url`, `smtp_url` and `admin_password`.

- **Env var names are string literals in `config.rs`** (`var("APP_URL")`, `parse("PORT", "3000")`, never a name built with `format!`), and every `"SCREAMING_SNAKE"` literal there is an env var: `offrails-check`'s `[env]` rule reads them all and fails on any missing from `.env.example`. A literal that isn't one gets a trailing `// not an env var`.
- **Read env vars only in `config.rs`.** Nowhere else calls `std::env::var` (the AWS keys read by `object_store`'s `from_env()` are the one sanctioned exception).
- `Config` lives in `AppState` (`state.config`), wrapped in `Arc` if it gets large.
- **Fail fast in production.** Missing `DATABASE_URL`, `APP_URL`, or `SMTP_URL` (when email is enabled) aborts boot with: `missing required env vars: APP_URL, SMTP_URL (see .env.example)`.
- Development has defaults for everything, so `cargo run` works with no `.env`:
  - `DATABASE_URL=sqlite://storage/development.sqlite3?mode=rwc`
  - `APP_URL=http://localhost:{PORT}`: it follows `PORT` (`config::dev_app_url(port)`), so emailed links and the CSRF origin check still work when you run on another port. Parse `PORT` before `APP_URL`.
- `APP_ENV` unset means `development`. `TestApp` builds `Config` directly (`Config::test()`) and never reads the environment.

## Per-environment behavior

Put it in one place, as a `match`:

```rust
// src/state.rs: select_mailer(&config, &db). The kernel only has constructors (Mailer::smtp/local/memory, Mailer::Log).
Ok(match (c.env, c.mailer.as_deref()) {
    (AppEnv::Test, _) => Mailer::memory(),
    (_, Some("smtp")) => smtp(from)?,                           // needs SMTP_URL: try real delivery from development
    (_, Some("local")) => Mailer::local(db.clone(), from),
    (_, Some("log")) => Mailer::Log,
    (_, Some(other)) => anyhow::bail!("MAILER must be smtp, local or log (got {other:?})"),
    (AppEnv::Development, None) => Mailer::local(db.clone(), from),   // ALWAYS the dev mailbox, even with SMTP_URL set
    (AppEnv::Production, None) if c.smtp_url.is_some() => smtp(from)?,
    (AppEnv::Production, None) => Mailer::Log,                  // EMAIL: not yet (logged only)
})
```

**Rate limits are per environment too** ([security](security.md#rate-limiting-kernelrate_limitrs)): in development every limit is **1000 per window** (`rate_limit::DEV_LIMIT`), so a person or agent retrying a form locally never locks themselves out; test and production use each call site's own limit (the 11th login is refused in tests exactly as in production). `RateLimiter::new(config.env)` decides it, once; call sites never branch on the environment.

**Never use `cfg!(debug_assertions)` or `#[cfg(debug_assertions)]` for behavior.** A release build running locally must behave like development when `APP_ENV=development`.

## `.env.example`

This file is the documentation for every variable. One line each, grouped, with a comment:

```sh
# --- core ---
APP_ENV=development
PORT=3000
# APP_URL=https://myapp.example.com   # required in production; in development it follows PORT (http://localhost:{PORT}), so leave it unset
DATABASE_URL=sqlite://storage/development.sqlite3?mode=rwc
# DATABASE_URL=postgres://postgres:postgres@localhost:5432/myapp_development
DATABASE_POOL=10
APP_INLINE_WORKER=true
# APP_TRUSTED_PROXY=true       # behind Fly, Kamal-proxy, Render: trust the rightmost X-Forwarded-For hop
# --- realtime (channels.md) ---
# APP_NODE=web-1               # this process's name on /dev/sockets and in the cluster (default <hostname>:<pid>)
# APP_CLUSTER=true             # Postgres only: share rooms and presence across nodes; refused at boot on SQLite
# APP_REALTIME_REVALIDATE_SECS=60    # long-lived sockets/streams re-check their session this often (logout closes them at once)
# APP_REALTIME_TRANSPORTS=websocket,sse,longpoll   # drop `websocket` behind a proxy that mishandles it; clients fall back
# --- jobs ---
APP_QUEUES=default:10,mailers:5
APP_JOBS_PRUNE_DAYS=7
APP_JOBS_SHUTDOWN_GRACE=25
# APP_SHUTDOWN_DRAIN_SECS=5    # web: open connections get this long after SIGTERM (streams end first); keep it below the platform's kill timeout
# APP_ADMIN_PASSWORD=          # production: protects /admin/jobs when there's no admin role
# --- email: development always uses the dev mailbox (/dev/mailbox); SMTP_URL is for production ---
# SMTP_URL=smtps://user:pass@smtp.provider.com:465
# MAILER=smtp                  # only to try real delivery from development (needs SMTP_URL)
MAIL_FROM="Myapp <no-reply@localhost>"
# --- storage (added with the first attachment field) ---
# STORAGE_SERVICE=local
# STORAGE_PATH=storage/files
# STORAGE_PUBLIC_HOST=        # only when serving S3 presigned URLs (storage.md)
# APP_REVISION=               # git sha, set by the deploy (shown on /dev/system)
# --- API (apps with an API; guides/api.md) ---
# APP_CORS_ORIGINS=https://app.example,http://localhost:5173   # browser apps on other origins; exact, never *
# --- direct TLS (optional; default: plain HTTP behind the platform's TLS proxy) ---
# APP_TLS_CERT=/etc/app/tls/cert.pem
# APP_TLS_KEY=/etc/app/tls/key.pem
# APP_DEV_HTTP_PORT=3001        # development only, with TLS on: the same app on plain http (bin/dev sets it with DEV_CERT_SELF_SIGNED=1)
# --- logging ---
RUST_LOG=info,myapp=debug
LOG_FORMAT=pretty
```

`bin/setup` copies it to `.env` if `.env` is missing. `.env` is git-ignored, always.

## Secrets in production

There's no encrypted credentials file. Secrets come from the platform:

| Target | How |
|--------|-----|
| Fly.io | `fly secrets set SMTP_URL=… ` |
| Kamal | `.kamal/secrets` (reads from a password manager or env; never commit raw values) + `env.secret` in `config/deploy.yml` |
| Railway / Render | Service variables in the dashboard or CLI |

When you add an env var: update `Config`, `.env.example`, and the deploy config. **Then tell the human which secret to set and where.** You can't set platform secrets for them unless they've given you CLI access.

## Gotchas agents get wrong

- Reading `std::env::var("X")` deep inside a handler. Add a `Config` field instead.
- Calling `dotenvy::dotenv()` unconditionally, so a stray `.env` in a production image overrides real config. Only call it when `APP_ENV` is unset or `development`.
- Trailing slash on `APP_URL`, which produces `//` in email links. Normalize it in `from_env`.
- Committing real values to `.env.example`. Only dev defaults and placeholders go there.
- Deriving the product name from `env!("CARGO_PKG_NAME")`. Renaming the crate (for example, to avoid a binary-name clash with another app on the same machine) then renames the product in every page title. Use `config::APP_NAME`.
