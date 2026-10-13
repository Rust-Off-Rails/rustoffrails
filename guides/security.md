# Security

**Security is the gold standard of this framework.** Rails ships defenses on by default. Off Rails ships them as specs you generate on day one (`generators/new.md`), enforced by tests, CI scanners and plugin hooks. Agents go hard here: assume every input is hostile, every client is scripted, and every proxy lies. Never remove a defense to make something work. Fix the something.

## Non-negotiables

You must **never weaken** any of these without **both** a `docs/decisions.md` entry and the human's **explicit approval in conversation**. "It was blocking me" is not a reason. If a fix seems to need it, stop and ask.

1. **CSP**: the strict policy in [the header matrix](#the-header-matrix). No `unsafe-inline`, no `unsafe-eval`, no wildcard sources, no inline `<script>` or `on*=` handlers.
2. **Security headers** on every response class, verified by `tests/app/headers_test.rs`.
3. **CSRF and Origin checks**, including the WebSocket handshake. Safe methods never change state.
4. **Escaping**: Askama autoescape, React and Svelte text rendering. Raw HTML only from ammonia-sanitized helpers.
5. **Authorization on every route and every socket join and push**: `RequireUser` (or stronger) plus a policy or an owner-scoped query.
6. **Tenant scoping** on every query in multi-tenant apps, with the isolation suite ([test-standards](test-standards.md) §4).
7. **Rate limits** on login, sign-up, password reset, magic links, API tokens, anything that sends email or costs money, and socket frames.
8. **Tokens**: random (≥ 32 bytes), **only digests stored**, expiring, single-use where they grant access, minted inside jobs (never in job args).
9. **Session revocation**: logout, password change and reset end the server session, and **every** session on change or reset, including open sockets and streams ([channels](channels.md)).
10. **Cookies**: `HttpOnly`, `SameSite=Lax`, `Secure` in production; the session cookie is `__Host-` prefixed in production.
11. **Secrets hygiene**: env vars only. Never in code, logs, job args, error pages, fixtures or git history (gitleaks in CI).
12. **Dependencies**: cargo-deny green (no known advisories, permissive licenses, no banned crates, crates.io only).

The plugin's hooks flag the common ways these get weakened as you type (`|safe`, `{@html}`, `dangerouslySetInnerHTML`, inline scripts, `unsafe-inline`/`unsafe-eval`, permissive CORS, SQL built with `format!`, cookies with `Secure`/`HttpOnly` turned off). For a full review run **`/offrails:security`**: threat model, scanners, live header check, the checklist, and attack tests.

## CSRF

No tokens. Modern browsers say where a request came from, and sessions use `SameSite=Lax` cookies. This is **tower-http's `CsrfLayer`** (feature `csrf`, tower-http ≥ 0.7), a port of Go 1.25's cross-origin protection. It used to be a generated `kernel/csrf.rs` with the same rules. Safe methods (`GET`, `HEAD`, `OPTIONS`) always pass. An unsafe method passes if any of these hold:

1. `Origin` exactly matches a trusted origin. We trust `APP_URL`.
2. `Sec-Fetch-Site` is `same-origin` or `none`.
3. Neither `Sec-Fetch-Site` nor `Origin` is present (a non-browser client; cookies + SameSite still protect browsers).
4. `Origin`'s host:port equals the request's `Host`.

Everything else gets 403.

```rust
let csrf = CsrfLayer::new()
    .add_trusted_origin(&config.app_url)?                                   // validated: returns an error for a bad APP_URL
    .with_insecure_bypass(|_, uri| uri.path().starts_with("/webhooks/"))   // webhooks verify the provider's signature instead
    .with_rejection_response(|_| AppError::Forbidden.into_response());     // shaped like every other 403 (errors-and-logging.md)
```

- Safe methods **must not change state**. This whole design depends on it.
- Behind a proxy, `Host` and `Origin` must reach the app unchanged (Fly, Kamal-proxy, Render and Railway do this by default). Otherwise only `Sec-Fetch-Site` protects you.
- Tests: a `Sec-Fetch-Site: cross-site` POST and a foreign-`Origin` POST both get 403. `TestApp::post_form` sends `Sec-Fetch-Site: same-origin`, like a browser.

## The header matrix

One middleware, `kernel/security.rs` (`security::middleware`), sets the headers. It sits **outside** `CatchPanicLayer` and the timeout in `routes.rs`, so panic 500s and 408s get them too ([routing](routing.md#middleware)). It only fills in headers a handler didn't set, so a route can make a header *stricter* (the mailbox viewer's own CSP), never remove it.

| Header | Value | Notes |
|--------|-------|-------|
| `Content-Security-Policy` | `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'` | Plus `; upgrade-insecure-requests` when served over https (below). Add a storage host to `img-src` only if blobs are served from another origin |
| `Strict-Transport-Security` | `max-age=63072000; includeSubDomains` | Production **with an `https://` `APP_URL`** only; `max-age=0` otherwise, so localhost is never pinned. `preload` is opt-in per decision (it's hard to undo) |
| `X-Content-Type-Options` | `nosniff` | |
| `Referrer-Policy` | `strict-origin-when-cross-origin` | |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=(), bluetooth=(), hid=(), midi=(), browsing-topics=()` | Open a feature per spec. Don't list retired features (`interest-cohort`) |
| `Cross-Origin-Opener-Policy` | `same-origin` | Isolates the window from cross-origin openers |
| `Cross-Origin-Resource-Policy` | `same-origin` | Other sites can't embed your responses. Crawlers (OG images) aren't affected |
| `X-Frame-Options` | `DENY` | Legacy twin of `frame-ancestors 'none'` |
| `Cache-Control` | `no-store` | On **every response to a request with a session cookie** (except `/static/*`, `/favicon.ico`, `/robots.txt`), and on all kernel pages (`/dev/*`, `/admin/*`). Private data never lands in a shared or back/forward cache |
| `Cross-Origin-Embedder-Policy` | not set | Only needed for cross-origin isolation (`SharedArrayBuffer`). `require-corp` breaks third-party embeds such as video players. Set it per spec |

By response class (all verified by `tests/app/headers_test.rs`, in development and production):

| Class | Example | Required |
|-------|---------|----------|
| HTML page | `GET /` | full matrix |
| Authenticated page | `GET /books/1` logged in | full matrix + `no-store` |
| JSON | `GET /api/…`, `/dev/mailbox.json` | full matrix (+ `no-store` when authenticated or kernel) |
| SSE stream | `GET /streams/{stream}` | full matrix, `text/event-stream` |
| WebSocket handshake | `GET /socket` → 101, and the cross-origin 403 | `nosniff`, CORP, CSP on both |
| Kernel page | `/admin/jobs`, `/dev` | full matrix + `no-store` |
| Errors | 404, 403 (CSRF), 422, 500 (panic) | full matrix |
| Static asset | `/static/app.js?v=…` | full matrix, cacheable (`immutable` when digested) |
| Redirect | `303` to login | full matrix |

**Cookies.** Every cookie the app sets is `HttpOnly; SameSite=Lax`, plus `Secure` in production. The session cookie is named **`__Host-session_token`** in production (`kernel::session::name(secure)`): browsers refuse a `__Host-` cookie unless it's `Secure`, `Path=/` and has no `Domain`, so a sibling subdomain can't plant a session (session fixation, login CSRF). Production reads **only** that name. In production the middleware also adds any missing `Secure`/`HttpOnly`/`SameSite` to every `Set-Cookie`, as a backstop for cookies built without config (the flash).

**TLS-only directives key off `APP_URL`.** `upgrade-insecure-requests` and HSTS are sent only when `APP_ENV=production` **and** `APP_URL` starts with `https://`, which is always the case when deployed behind TLS. Verified in Chrome: on a production build run locally over plain http, `upgrade-insecure-requests` makes the browser upgrade same-origin **redirects** (not the initial requests) to `https://localhost`, and every redirect flow fails with `ERR_SSL_PROTOCOL_ERROR`. Production with a non-https `APP_URL` on a non-local host logs a loud warning at boot.

**Verified in current Chrome (2026-10-12):**
- `connect-src 'self'` covers same-origin `ws://` and `wss://` (CSP3). The book-room socket joins and presence renders under this exact CSP.
- `Secure` and `__Host-` cookies work on `http://localhost` (a secure context), so the production cookie path is testable locally.
- The Permissions-Policy above logs no console warnings.

**Development adjustments**, for Vite modes only (`APP_ENV=development`):
- add `http://localhost:5173` to `script-src`, `style-src` and `connect-src`;
- add `ws://localhost:5173` to `connect-src`;
- add `'unsafe-inline'` to `style-src`.

The React refresh preamble is a same-origin file, `/dev/vite-preamble.js` ([frontend-inertia](frontend-inertia.md)), so it needs no nonce. SvelteKit SPA's boot script is allowed by **sha256 hash** ([frontend-svelte](frontend-svelte.md)). Production never gets `unsafe-inline` or `unsafe-eval`.

The kernel pages (`/admin/jobs`, `/dev/*`) are covered by the same CSP. They use vendored htmx with `allowEval: false` and no inline scripts. The mailbox's HTML-body route sets its own, stricter headers ([mailers](mailers.md)).

## Rate limiting (`kernel/rate_limit.rs`)

Rails 8 `rate_limit to: 10, within: 3.minutes` equivalent:

- `RateLimiter` (in `AppState.rate_limit`) wraps a `moka::future::Cache<String, Window>`. `Window` is `{ hits: Arc<AtomicU32>, ttl }`, and a `moka::Expiry` impl expires each entry `ttl` after creation, so every call site can use its own window. The key is `"{route}:{client_ip}"`. Two methods: `check(key, limit, window)` counts a hit and errors with `TooManyRequests` past the limit; `exhausted(key, limit)` only peeks. Use the pair for failure-only limits (peek first, `check` after a failure), as `AdminAccess` does.
- `state.rate_limit.check(&key, limit, window).await -> Result<(), AppError>`. Over the limit → `AppError::TooManyRequests`. Forms a human submits (login, link request) catch it and **re-render the form with status 429** and the alert "Try again later.", so the person sees why. Other endpoints let the error map to a plain 429 (JSON on `/api`). Limited requests never reach argon2.
- Client IP comes from the `ClientIp(Option<IpAddr>)` extractor:
  - normally the socket address. This needs `serve` to use `into_make_service_with_connect_info::<SocketAddr>()`, and it's `None` in tests;
  - the **rightmost** `X-Forwarded-For` hop, only when `APP_TRUSTED_PROXY=true` (set behind Fly, Kamal-proxy, Render, and similar).

  `ClientIp::key(route)` builds the key.
- It's in-process, so the limit is per instance. That's acceptable, and documented in decisions. If a spec needs global limits, use the escape hatch to the `cache_entries` table.
- Applied to: login, password reset request, magic-link request (by IP **and** by email: `format!("login_links#create:{email}")`), sign-up, and any endpoint that sends email or costs money.
- Tests run without a socket, so every request shares the `"<route>:unknown"` key. One `TestApp` can exhaust a limit in a loop (the 11th attempt → 429) without any IP spoofing. To test a **per-email limit across IPs**, build the `TestApp` with `APP_TRUSTED_PROXY=true` and send a different `X-Forwarded-For` on each request: the IP keys differ, the email key is shared, and the 11th request still gets 429.

## XSS

- Askama autoescapes. `|safe` is allowed **only** on output of `views::helpers::markdown()` (ammonia-sanitized).
- React escapes. `dangerouslySetInnerHTML` is allowed only with server-sanitized HTML from the same helper, delivered in a field named `*_html`.
- Never build HTML with `format!` in Rust.
- Inline scripts and `on*=` attributes are blocked by CSP. Use `static/app.js` or React.

## Injection

- SQL: SeaORM builder with bound values only. Raw SQL only through the escape hatch with binds (`database.md`). Never `format!` user input into SQL, shell commands, or file paths.
- Dynamic sort/filter columns: map an enum from the query string onto `Column` variants with a `match`. Never trust a column name string.
- Shell: don't shell out from request handlers. If a job must, pass arguments as a vector (`Command::new(x).arg(y)`), never through a shell.

## Mass assignment

`<Model>Params` structs contain only user-editable fields. **Never** `user_id`, `author_id`, `account_id`, `role`, `admin`, `price`, `status` (unless the spec makes it user-editable). Ownership fields come from the extractor (`RequireUser`, `CurrentAccount`), not from the body.

## File uploads

See `storage.md`. In short: enforce a size limit per route (`DefaultBodyLimit`), use an allowlist of content types checked against magic bytes (not the client header), use random storage keys (never the user's filename as a path), serve with `Content-Disposition: attachment` unless it's an allowlisted image, and never serve uploads from the app origin with `text/html`.

## Admin & dev surfaces

- `/admin/jobs` ([jobs-dashboard](jobs-dashboard.md)) is protected by the `AdminAccess` extractor: `RequireAdmin` when roles exist, otherwise HTTP Basic with `APP_ADMIN_PASSWORD`. In production with neither, it isn't mounted. Any future `/admin/*` screen uses the same extractor.
- `/dev/*` ([dev-tools](dev-tools.md)) exists only in development. It's not hidden, it's not routed. A test proves it 404s with a production config.
- Admin actions are POSTs (CSRF applies), are logged with the acting user, and send `Cache-Control: no-store`.

## WebSockets

`/socket` ([channels](channels.md)) has its own rules, because CSRF protection doesn't cover WebSocket handshakes, and browsers send cookies on cross-site handshakes:

- **Origin check (CSWSH):** the handshake is refused (403) unless `Origin` exactly equals APP_URL's origin. A **missing** `Origin` is refused too.
- **Auth at upgrade:** the session cookie decides the member, and logged out → 401. Every topic join is then authorized with the same policy as the page (`Channels::authorize`). Private topics (`user:{id}`) accept only their owner.
- **Limits:**
  - frames over 4 KiB close the socket;
  - more than 100 frames per 10s close it with 1008;
  - malformed frames close it with 1002;
  - silence for 60s closes it;
  - per-person limits for app events (e.g. 10 messages per 10s) live in the room, so they hold across tabs and nodes.
- **Output:** room payloads carry raw user text. The client renders it with `textContent`, and the server-rendered page escapes it.
- **For the connection's whole life** ([channels#security](channels.md#security)): logout, a password change or revocation closes that session's sockets **and** SSE streams at once (4401 / `bye`), in every process and node; periodic revalidation closes expired sessions; every push is re-authorized against the database; per-user caps (8 connections, 20 topics, 30 joins/min); presence carries minimal metadata, never emails; the SSE and long-poll fallbacks refuse cross-site requests and bind relays to member + token.

## Secrets

- Only in env vars (`config.md`). `.env` is git-ignored, and `.env.example` has placeholders only.
- Never log secrets, tokens, passwords, or full request bodies of auth endpoints. `tracing` fields for these must be omitted, not redacted later.
- Errors shown to users never include internals (`errors-and-logging.md`).

## Automated enforcement

Every app gets these on day one ([new](../generators/new.md)). They run in CI on every push, and locally through `/offrails:check` and `/offrails:security`.

| Gate | Tool | Config (committed) | Fails on |
|------|------|--------------------|----------|
| Header matrix | `tests/app/headers_test.rs` | — | any response class missing a header; unprefixed or non-`Secure` production cookies; a planted unprefixed session cookie logging someone in |
| Dependencies | `cargo deny check` (cargo-deny 0.20) | `deny.toml` | RustSec advisories and yanked crates; unmaintained crates; non-permissive licenses; **banned crates** (the "Banned" list in [stack](../stack.md), generated into `[bans]`); git or non-crates.io sources |
| Secrets | `gitleaks` (8.30) over **all git history** | `.gitleaks.toml` | any secret in any commit |
| Dynamic scan | **OWASP ZAP baseline** (`ghcr.io/zaproxy/zaproxy:stable`) against the production Docker image | `.zap/rules.tsv` | any WARN not triaged in the rules file |
| JS dependencies | `pnpm audit --prod` (TS modes) | — | high or critical advisories |
| Updates | Dependabot, weekly and grouped | `.github/dependabot.yml` | — (opens PRs; CI gates them) |

The reference `deny.toml` (verified with cargo-deny 0.20.2: `advisories ok, bans ok, licenses ok, sources ok`):

```toml
# cargo-deny: run in CI and by /offrails:check (`cargo deny check`).
# Generated from stack.md "Banned". Never loosen without a docs/decisions.md entry + human approval.

[graph]
all-features = false

[advisories]
# RustSec advisory DB. A vulnerable or yanked crate fails the build; unmaintained crates are
# reported for the whole tree (they're the ones nobody will fix).
version = 2
yanked = "deny"
unmaintained = "all"
ignore = [
  # Triaged exceptions only, each with a reason and a review date:
  # { id = "RUSTSEC-YYYY-NNNN", reason = "…, not reachable because …; recheck 2026-12-01" },
]

[licenses]
version = 2
confidence-threshold = 0.9
# Permissive licenses only. A copyleft or unknown license fails the build.
allow = [
  "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause",
  "ISC", "Zlib", "0BSD", "BSL-1.0", "CC0-1.0", "Unicode-3.0", "CDLA-Permissive-2.0", "MPL-2.0",
]

# The app and its migration crate are `publish = false`; they're ours, not a dependency.
[licenses.private]
ignore = true

[bans]
multiple-versions = "warn"
wildcards = "deny"
highlight = "all"
# Never anywhere in the tree (stack.md "Banned"). Direct-only bans (lazy_static, once_cell,
# async-trait, log macros) are checked against Cargo.toml by the plugin hook and /offrails:doctor,
# because healthy crates still pull them in transitively.
deny = [
  { crate = "diesel", reason = "SeaORM is the one data layer (stack.md)" },
  { crate = "actix-web", reason = "one web stack: axum (stack.md)" },
  { crate = "rocket", reason = "one web stack: axum (stack.md)" },
  { crate = "poem", reason = "one web stack: axum (stack.md)" },
  { crate = "loco-rs", reason = "one web stack: axum (stack.md)" },
  { crate = "leptos", reason = "no WASM front ends (stack.md)" },
  { crate = "dioxus", reason = "no WASM front ends (stack.md)" },
  { crate = "yew", reason = "no WASM front ends (stack.md)" },
  { crate = "tera", reason = "runtime templates hide errors (stack.md)" },
  { crate = "handlebars", reason = "runtime templates hide errors (stack.md)" },
  { crate = "minijinja", reason = "runtime templates hide errors (stack.md)" },
  { crate = "askama_axum", reason = "removed upstream (stack.md)" },
  { crate = "axum-inertia", reason = "generate the adapter from the spec (stack.md)" },
  { crate = "redis", reason = "the database does jobs, cache and pub/sub (stack.md)" },
  { crate = "openssl", reason = "rustls only (security.md)" },
  { crate = "openssl-sys", reason = "rustls only (security.md)" },
  { crate = "native-tls", reason = "rustls only (security.md)" },
]

[sources]
# Only crates.io. A git or alternative-registry dependency must be justified and added here.
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

Details:
- **`deny.toml`:**
  - set `publish = false` on the app and migration crates and ignore private crates' licenses (`[licenses.private] ignore = true`), so only dependencies are judged;
  - crates that healthy dependencies pull in transitively (`lazy_static`, `once_cell`, `async-trait`, `log`) are banned as **direct** dependencies only, which the plugin hook and `/offrails:doctor` check against `Cargo.toml`;
  - everything else on the banned list is denied anywhere in the tree, plus `openssl`, `openssl-sys` and `native-tls` (rustls only).
- **Advisory ignores** in `deny.toml` need an id, a reason ("not reachable because …") and a recheck date.
- **gitleaks:**
  - the allowlist covers build and runtime directories only (`target/`, `storage/`, `node_modules/`, `web/dist/`, `static/vendor/`);
  - a known-public value (an RFC test vector) gets `// gitleaks:allow` **on the same line**, so move it into a one-line `const`.
- **ZAP:**
  - CI boots the production image and runs `zap-baseline.py -c rules.tsv` **without** `-I`, so every untriaged WARN fails;
  - each `IGNORE`/`INFO` line in `rules.tsv` states why, for example: htmx's `eval` (disabled by `allowEval:false` and the CSP), anti-CSRF tokens (by design: Fetch Metadata plus Origin), COEP (see the matrix);
  - with a rules file, ZAP's informational detectors (10049 cacheability, 10111 login form found, 10112 session cookie found) are reported as WARN, so mark them `INFO` with a note.

## Checklist

The "brakeman" pass. Run through this before finishing any work touching auth, payments, uploads, admin, or multi-tenancy. Report the result in your final message:

- [ ] Every state-changing route uses an unsafe method and passes CSRF middleware (no GET writes).
- [ ] Every non-public handler has `RequireUser` (or stronger) **and** a policy check or owner-scoped query.
- [ ] Tenant data: every query filters by `account_id`, and a cross-tenant test exists.
- [ ] No entity `Model` is serialized to a client. Only `*Json`/props structs.
- [ ] `/admin/*` requires `AdminAccess`; `/dev/*` is absent in production (test exists).
- [ ] Job args carry no secrets or tokens (they're visible in `/admin/jobs`). Auth emails create their token **inside** the job ([auth](auth.md#password-reset-passwordsrs-new-create-edit-update)).
- [ ] No `|safe` / `dangerouslySetInnerHTML` except sanitized `*_html` fields.
- [ ] No raw SQL outside the kernel or a logged escape hatch, and all values bound.
- [ ] Params structs contain no ownership/privilege fields.
- [ ] Sensitive endpoints are rate limited.
- [ ] Tokens are random (≥ 32 bytes) and stored hashed, with expiries.
- [ ] Redirect targets from user input are relative and same-origin (return-to rejects `//host` and `/\host`).
- [ ] Auth responses don't reveal whether an email exists (same status, location and flash), and magic links are consumed by POST only.
- [ ] The dev mailbox hint and `/dev/*` links render only when `APP_ENV=development` (test with production config).
- [ ] Uploads: size limit, type allowlist, random keys, safe `Content-Disposition`.
- [ ] WebSockets: Origin checked (missing = refused), auth at upgrade, every join authorized, frame size and rate limits, tests for each ([channels](channels.md#testing)).
- [ ] No secrets in code, logs, fixtures or `.env.example`. gitleaks is green over the full history.
- [ ] `headers_test.rs` covers every response class the change adds (a new kind of endpoint means a new row).
- [ ] `cargo deny check` is green. Any new advisory ignore has a reason and a recheck date.
- [ ] The ZAP baseline is green. Any new `rules.tsv` line explains its triage.
- [ ] The feature spec's `## Security` section lists the threats this change touches, each with its control and the test that proves it.
- [ ] No Non-negotiable was weakened, or there's a decision entry **and** the human's explicit approval.

## Gotchas agents get wrong

Each of these was a real finding in the reference app, fixed test-first:
- **Cookies built without config miss `Secure`.** The flash cookie is set from `IntoResponseParts`, which has no config, so in production it went out without `Secure`. The middleware now hardens every `Set-Cookie`, and the header test fails if any production cookie lacks `Secure` or `HttpOnly`.
- **Authenticated pages were cacheable.** Only kernel pages sent `no-store`, so a logged-in book page could be kept by a shared cache or the back/forward cache. Now any request carrying a session cookie gets `no-store`, static assets excepted.
- **Headers set inside `CatchPanicLayer` vanish on panics.** A layer only decorates responses produced *inside* it, so the hardening middleware sits outside the panic and timeout layers.
- **`upgrade-insecure-requests` over plain http breaks redirects.** See the matrix: TLS-only directives key off an `https://` `APP_URL`.
- **Testing headers on endless responses:** use `TestApp::request_head`, which returns status and headers without reading the body, for SSE. Use a raw TCP handshake against `support::serve` for the WebSocket 101.
