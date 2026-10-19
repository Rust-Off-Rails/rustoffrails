# APIs: `FRONTEND_MODE: api` and the `API` axis

> **TL;DR**
> - `FRONTEND_MODE: api` means no UI at all; the `API` header (`none | rest | graphql | both`) is a separate choice, so any mode can expose a public API.
> - REST lives under `/api/v1`, returns `{"data": …}` (lists add `page`), takes bodies through `ApiJson<T>` (JSON 422s), and is documented by utoipa at `/api/openapi.json`, committed with a drift test.
> - Auth is bearer API tokens: digest-only, scoped, expiring, rate-limited per token.
> - CORS uses exact origins from `APP_CORS_ORIGINS`, never `*` and no credentials; those origins are also CSRF-trusted.
> - Never return an entity `Model` or a `json!` blob, and never 403 another user's record (scope the query, 404).
>
> Read the rest when building an API-only app, adding tokens, CORS or OpenAPI, or writing API tests.

Two separate choices, both made in the [interview](../interview.md) and recorded in the app's `AGENTS.md`:

| Header | Values | Means |
|--------|--------|-------|
| `FRONTEND_MODE` | `htmx \| inertia \| spa \| api` | Who renders the UI. **`api`** = no UI at all, like `rails new --api`: no app templates, no Tailwind, no `web/` |
| `API` | `none \| rest \| graphql \| both` | Whether the app exposes a **documented, versioned API to other programs** (mobile apps, partners, scripts), and in which style. Independent of the UI: an `htmx` app can have `API: rest` |

Rules:

- `FRONTEND_MODE: api` requires `API: rest | graphql | both`. Default: `rest`. Choose `graphql` when the clients are UIs with varied data needs (several screens, nested data), `both` when the spec names both kinds of client.
- `FRONTEND_MODE: spa` already has a JSON API for its own SPA under `/api`. That's an internal API (cookie sessions, types via ts-rs, no versioning promise). `API: rest` on top of it means *public*: `/api/v1`, bearer tokens, OpenAPI, frozen contracts ([test-standards](test-standards.md#2-the-required-test-set-by-archetype)).
- `API: none` is the default for every UI mode.

Every app, `api` mode included, keeps the kernel UIs: `/dev` (mailbox, routes, system), `/admin/jobs`. They're self-contained pages, not app UI.

Reference implementation: the `offrails-api-demo` app (`FRONTEND_MODE: api`, `API: both`), built from these docs and green on SQLite and Postgres. Its layout is what this guide describes.

## Layout (`api` mode)

```
src/controllers/
  api/mod.rs          ApiJson extractor, ApiDoc (OpenAPI), ErrorBody/InvalidBody, docs() dev page
  api/v1/<resource>.rs   REST handlers, one file per resource, annotated with #[utoipa::path]
  graphql.rs          POST /graphql + GraphiQL (dev)
  root.rs             GET / (JSON index), GET /robots.txt (Disallow: /)
  application.rs      ApiUser, OptionalApiUser (bearer tokens), AdminAccess
src/graphql/          schema, query.rs, mutation.rs, types.rs, loaders.rs (guides/graphql.md)
src/views/<resource>.rs   #[derive(Serialize, ToSchema)] JSON bodies: NoteJson, NotesPage, NoteBody
docs/openapi.json     committed; a test fails when it drifts
docs/schema.graphql   committed; a test fails when it drifts
```

No `templates/` except `templates/kernel/`, `templates/mailers/` and the standalone `templates/errors/` pages the kernel UIs use. No `assets/`, no `bin/setup-tailwind`.

## Responses

- **Errors** are always JSON (the `ResponseFlavor` is `Api` for every path except `/dev` and `/admin`):
  - `{"error": "not_found" | "unauthorized" | "forbidden" | "too_many_requests" | "payload_too_large" | "internal"}`;
  - validation: `422 {"errors": {"title": ["can't be blank"]}}`, with `base` for errors not tied to a field.
  - `401` carries `WWW-Authenticate: Bearer realm="api"`; `429` carries `Retry-After`.
- **Bodies**: single resources are wrapped, `{"data": {...}}`, so fields can be added next to `data` later; lists are `{"data": [...], "page": {"page": 1, "per": 25, "has_more": true}}`.
- **Pagination**: `?page=&per=` (`models::Page`, `per` clamped to 1..=100). Fetch `per + 1` rows to compute `has_more`; never `COUNT(*)` on the hot path. Order by `created_at DESC, id DESC`.
- **IDs** are numbers in REST (`i64` fits JSON safely up to 2^53) and `ID` strings in GraphQL.
- **Field names** are `snake_case`, the same as the database and Rust. No serde renames.
- **Bodies are parsed by `ApiJson<T>`**, never bare `Json<T>`. Axum's own rejections are plain text (400/415/422); `ApiJson` maps them to `422 {"errors": {"body": ["is not valid JSON"]}}`.
- **Entity models never cross the boundary.** Handlers return `views::<resource>` structs (`From<Model>`). That's how `password_digest` never leaks.

## Versioning

- Everything public lives under `/api/v1`. A breaking change (removing or renaming a field, changing a type or a status code) means `/api/v2` next to `v1`, never an edit of `v1`. Additions (new fields, new endpoints) are not breaking.
- The contract snapshots (insta) and the committed `docs/openapi.json` are what reviewers see. Changing them is a decision: log it in `docs/decisions.md` and tell the human.
- Unversioned and kernel paths: `/`, `/up`, `/robots.txt`, `/api/openapi.json`, `/graphql` (GraphQL evolves by adding fields and `@deprecated`, not by URL).

## Authentication: API tokens

Bearer tokens, stored like sessions: random 32 bytes, base64url, **only the SHA-256 digest in the database** (`kernel::session::{generate_token, digest}`).

```
api_tokens:
  id            big_integer pk auto
  user_id       big_integer → users.id ON DELETE CASCADE (indexed)
  name          string                 // "CLI on my laptop"
  token_digest  string UNIQUE
  scopes        json                   // ["read","write"]
  last_used_at  timestamptz NULL       // written at most once a minute per token
  expires_at    timestamptz NOT NULL   // default now() + 90 days at mint; `--expires never` is an operator-only flag, logged in decisions
  created_at    timestamptz
```

| Route | What |
|-------|------|
| `POST /api/v1/users` | Sign-up (`ACCOUNTS: password`, open sign-up): creates the user **and the first token in one transaction**, enqueues the welcome email, returns `201 {"token": "…", "data": TokenJson, "user": UserJson}` |
| `POST /api/v1/tokens` | Email + password (+ optional `name`, `scopes`, `expires_in_days` 1..=365) → a new token. Same 422 (`base: "Invalid email or password"`) for an unknown email and a wrong password; one argon2 verify either way |
| `GET /api/v1/tokens` | The caller's tokens, metadata only |
| `DELETE /api/v1/tokens/current` | Log out: revoke the token making the request |

A password reset or change deletes the user's tokens along with their sessions ([auth](auth.md#password-reset-passwordsrs-new-create-edit-update)); `app task tokens revoke-all <email>` does it from the terminal.
| `GET /api/v1/me` | The caller |
| `app task tokens create <email> [name] \| list <email> \| revoke <id>` | The same from the terminal, for operators and CI |

The raw token is shown exactly once, in the `201` body or the task's output. It's never logged, never listed, never put in job args.

Extractors (`controllers/application.rs`):

- `ApiUser { user, token, scopes }`: missing, unknown, revoked or expired token → `Unauthorized`. Looked up **once per request** and cached in the extensions (one query: token join user), then rate-limited per token and `touch`ed.
- `OptionalApiUser(Option<ApiUser>)`: for public endpoints and the GraphQL context.
- `me.require(Scope::Write)?` → `Forbidden` (403) when the token lacks the scope. Policies take the scopes too: `policies::note::can_edit(&me.user, &me.scopes, &note)`.

Scopes are `read` and `write` by default. Add more only when a spec names them (e.g. `admin`), and never a wildcard.

**Cookie sessions** stay available for apps with a UI (`htmx`/`inertia`/`spa`) that also expose `API`: browsers use the session cookie, other programs use tokens. The two extractors are separate; a handler accepts the one its route is for. `api` mode has no cookie sessions.

## Rate limits

- Per token: 300 requests per minute across REST and GraphQL (`TOKEN_RATE_LIMIT`), in `load_api_user`.
- Per IP: sign-up and token creation, 10 per 3 minutes (like the login form).
- Same limiter, same rules ([security](security.md#rate-limiting-kernelrate_limitrs)): the window is fixed from the first request, refused requests don't extend it, and development allows 1000 per window.
- Limits are per process (moka), like every rate limit here ([security](security.md)). Behind several web processes, divide or move the counter to the cache table.

## CORS

Only browser apps on **other origins** need CORS (a separate SPA, a partner's site). Same-origin pages and non-browser clients never do.

- `APP_CORS_ORIGINS=https://app.example,http://localhost:5173`: exact origins, comma-separated. `*` or anything not `http(s)://…` fails at boot (`config::parse_cors_origins`). Unset: no CORS headers at all.
- `tower_http::cors::CorsLayer` with `AllowOrigin::list`, methods `GET POST PATCH DELETE`, headers `authorization content-type`, `max_age` 600s, **no credentials** (bearer tokens, not cookies).
- The same origins are added to the CSRF layer's trusted origins (`CsrfLayer::add_trusted_origin`). Otherwise the allowed SPA's cross-site `POST` is refused by CSRF. Non-browser clients send neither `Origin` nor `Sec-Fetch-Site` and pass CSRF as-is.
- Layer order: CORS outside CSRF, so preflights are answered before CSRF runs.

## OpenAPI (`API: rest | both`)

`utoipa` generates an OpenAPI 3.1 document from the code: `#[utoipa::path(...)]` on each handler, `#[derive(ToSchema)]` on bodies and `Params`, `#[derive(IntoParams)]` on `Page`. One `#[derive(OpenApi)] struct ApiDoc` in `controllers/api/mod.rs` lists every path, plus a `Modify` that adds the `bearer` security scheme.

```rust
#[utoipa::path(
    post, path = "/api/v1/notes", tag = "notes",
    request_body = NoteParams,
    security(("bearer" = [])),
    responses((status = 201, body = NoteBody), (status = 422, body = InvalidBody), (status = 403, body = ErrorBody))
)]
pub async fn create(State(s): State<AppState>, me: ApiUser, ApiJson(params): ApiJson<NoteParams>)
    -> Result<(StatusCode, Json<NoteBody>), AppError> { … }
```

- Served at `GET /api/openapi.json` (public; it's documentation).
- **Committed at `docs/openapi.json`.** A test compares the served document with the file and fails when it drifts; it also asserts every `/api/v1` row of `ROUTES` is in the document. Regenerate after an intended change: `UPDATE_SCHEMAS=1 cargo nextest run openapi`.
- **Dev reference UI** at `/dev/api-docs` (development only): a hand-written page that loads Scalar from jsDelivr at a **pinned** version (`SCALAR_VERSION`) with no inline script, and its own CSP (`script-src 'self' https://cdn.jsdelivr.net`, `style-src … 'unsafe-inline'`). Don't add `utoipa-scalar`/`utoipa-swagger-ui`: the first loads an **unpinned** CDN script, the second downloads Swagger UI from GitHub at build time (breaks offline and Docker builds).
- utoipa fills `info.license` from Cargo metadata with an empty name, which is invalid OpenAPI. The `Modify` sets it to `None`.

## Testing

`TestApp` helpers for APIs: `login_as(&user) -> token` (a real read+write token, sent from then on), `login_with_scopes(&user, &["read"])`, `use_token(Some(..)/None)`, `post_json`, `patch_json`, `delete`, `json(method, path, body)`, `graphql(query, variables)`, and `TestResponse::json()`.

Required for `API: rest`:

- per endpoint: the request matrix (happy path, 422 with exact body, 401 without a token, 404 for another user's record, 403 for a missing scope);
- `security_test.rs`: missing/garbage/revoked/expired tokens are all the same 401; a read-only token can't write anything; the per-token rate limit; CORS allows only the allowlist (preflight and real request) and never sends credentials; foreign `Origin` still fails CSRF; JSON bodies ignore ownership fields; every response is `application/json` with `nosniff`;
- `api_contract_test.rs`: insta snapshots of each body shape (redact ids and timestamps), and the OpenAPI drift test;
- the list endpoint's query count is stable (warm up first: the first request with a token writes `last_used_at`).

## Gotchas agents get wrong

- Using `Json<T>` for request bodies: clients get plain-text 400s. Use `ApiJson<T>`.
- Returning the entity `Model` (leaks columns) or building JSON with `json!` in handlers (no schema, no snapshot). Use a `views` struct.
- `403` for someone else's record: it leaks existence. Scope the query to the owner and return 404.
- Adding CORS origins but not CSRF trusted origins (the SPA's POSTs get 403), or `allow_credentials(true)` with bearer tokens (pointless, and widens the attack surface).
- Forgetting `UPDATE_SCHEMAS=1` after an intended change, then "fixing" the drift test by deleting it.
- A query-count test without the warm-up request: the first request touches `last_used_at`, so counts differ by one.
