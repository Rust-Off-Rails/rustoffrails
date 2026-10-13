# Security

Rails ships defenses on by default. Off Rails ships them as specs you generate on day one (`generators/new.md`). Never remove one to make something work. Fix the something.

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

## Security headers

Set via `tower-http` `SetResponseHeaderLayer::if_not_present`, one layer per header, in `routes.rs` ([routing](routing.md#middleware)):

| Header | Value |
|--------|-------|
| `Content-Security-Policy` | `default-src 'self'; img-src 'self' data: <storage host>; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'` |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `strict-origin-when-cross-origin` |
| `X-Frame-Options` | `DENY` |
| `Strict-Transport-Security` | `max-age=63072000; includeSubDomains` in production, `max-age=0` otherwise (one layer, value chosen from config) |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=()` (unless a spec needs them) |

CSP adjustments, made only in `APP_ENV=development` for Vite modes:

- add `http://localhost:5173` to `script-src`, `style-src` and `connect-src`;
- add `ws://localhost:5173` to `connect-src`;
- add `'unsafe-inline'` to `style-src`.

The React refresh preamble is a same-origin file, `/dev/vite-preamble.js`, served by `kernel/assets.rs` in development ([frontend-inertia](frontend-inertia.md)), so it needs no nonce or hash. Production CSP has **no** `unsafe-inline` or `unsafe-eval`.

The kernel pages (`/admin/jobs`, `/dev/*`) are covered by the same CSP. They use vendored htmx with `allowEval: false` and no inline scripts. The one exception is the mailbox's HTML-body route, which sets its own stricter headers ([mailers](mailers.md)).

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
- Tests run without a socket, so every request shares the `"<route>:unknown"` key. One `TestApp` can exhaust a limit in a loop (the 11th attempt → 429) without any IP spoofing.

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

## Secrets

- Only in env vars (`config.md`). `.env` is git-ignored, and `.env.example` has placeholders only.
- Never log secrets, tokens, passwords, or full request bodies of auth endpoints. `tracing` fields for these must be omitted, not redacted later.
- Errors shown to users never include internals (`errors-and-logging.md`).

## Dependencies

`cargo audit` (and `pnpm audit` in Vite modes) in CI, weekly. Treat new advisories as bugs.

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
- [ ] No secrets in code, logs, fixtures or `.env.example`.
