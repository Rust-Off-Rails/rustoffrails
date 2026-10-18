# Test Standards: what every app must prove

> **TL;DR**
> - Every `AC-<feature>-<n>` in `product/` is cited by a test; the required suites depend on the app's archetypes (web, inertia, spa, API, jobs, realtime, multi-tenant, money).
> - Security tests walk `ROUTES` (access matrix, CSRF, escaping, sessions, rate limits, mass assignment, the header matrix); multi-tenant apps get a tenant isolation suite.
> - Gates: ≥ 80% line coverage (≥ 90% models and policies), mutation testing on policies, N+1 guards, no flaky tests, never weaken a test to pass.
> - UI changes get a real-browser smoke (§9) and the axe accessibility suite in light and dark (§10); Lighthouse alone isn't enough.
>
> Read the rest when deciding what to test for a feature, or checking a gate.

[`testing.md`](testing.md) says **how** to write tests (TestApp, factories, layout). This guide says **what** must be tested, how much is enough, and how that's enforced. It's part of the Definition of Done, so none of it is optional.

The rule: **a behavior that isn't tested doesn't exist.** The human writes no code and doesn't read it either. The suite is the only proof they get that the app does what `product/` says.

## 1. Traceability: specs ↔ tests

Every acceptance criterion gets a stable ID, and every test that proves one cites it.

```markdown
<!-- product/features/books.md -->
- [x] AC-books-1: A logged-in user can add a book with a title and author
- [ ] AC-books-2: Adding a book without a title shows "Title can't be blank"
```

```rust
/// AC-books-1 (product/features/books.md)
#[tokio::test]
async fn logged_in_user_can_add_a_book() { … }
```

- IDs are `AC-<feature file stem>-<n>`. They are never renumbered or reused. A dropped criterion is struck through (`~~AC-books-3~~`), not deleted.
- A criterion is ticked `[x]` only when a test citing it passes on both databases.
- The mechanical check that `/offrails:check` runs:

```sh
comm -23 <(grep -rhoE 'AC-[a-z0-9_-]+-[0-9]+' product/ | sort -u) \
         <(grep -rhoE 'AC-[a-z0-9_-]+-[0-9]+' tests/ src/ web/src/ | sort -u)
```

Any output is an untested criterion, and the DoD fails.

## 2. The required test set by archetype

Find the app's archetype from its `AGENTS.md` header and the product README. An app can match more than one, in which case every matching row applies.

| Archetype | When | Required suites |
|-----------|------|-----------------|
| **Web app** (`htmx`) | default | Model, policy, request (HTML), security, jobs/mailers if present, migration |
| **React app** (`inertia`) | `FRONTEND_MODE: inertia` | Everything in Web app, with request tests asserting **props** rather than HTML. Plus component tests (Vitest) and E2E for critical flows (Playwright) |
| **SPA + API** (`spa`) | `FRONTEND_MODE: spa` | Model, policy, **API contract**, security. Plus component tests and E2E for critical flows |
| **Public API** | Third parties or mobile apps call `/api` | Everything in SPA + API, plus **frozen contract snapshots**, versioning tests, auth-token tests, and pagination/limit tests |
| **API-only** | `FRONTEND_MODE: api` | Everything in Public API, plus: every error path is JSON (no HTML outside `/dev`, `/admin`); bearer-token matrix (missing, garbage, revoked, expired → the same 401 with `WWW-Authenticate: Bearer`; a read-only token can't write anything); per-token and per-IP rate limits; CORS allowlist (preflight and real request, no credentials, foreign origins refused) and CSRF still refusing a foreign `Origin`; the committed `docs/openapi.json` matches and covers every `/api/v1` route ([api](api.md#testing)) |
| **GraphQL** | `API: graphql \| both` | Every query and mutation anonymously → `UNAUTHENTICATED`; another user's record → `NOT_FOUND`; read-only token on mutations → `FORBIDDEN`; validation failures as payload `errors` (no top-level errors); the N+1 guard for every association (1 vs 20 rows, same query count); depth and complexity limits reject abuse **and the standard introspection query passes**; the committed `docs/schema.graphql` matches; with `API: both`, REST and GraphQL agree on the same record ([graphql](graphql.md#required-tests-graphql-archetype)) |
| **Background-heavy** | Specs with schedules, imports, or emails at volume | Every worker: success, retry on error, idempotency (run twice → same result), uniqueness, cron entry fires |
| **Realtime** | Channels exist | Signals: broadcast emitted on change, subscriber authz (can't subscribe to others' streams). Signals resume: a reconnect with a cursor gets what it missed (or `refresh`), a page reached by a boosted navigation still updates. Rooms ([channels](channels.md#testing)): join authz (401 logged out, 403 foreign Origin, others' private topics refused), delivery + storage, presence join/leave and two tabs = one person, resume with `since`, room rate limit, frame size/flood/garbage closes, typed events (`bad_event`), text shown as typed. Once per app (`wire_test.rs`): offrails.v1 conformance, every transport, and the connection-security rules (logout/password change close connections, expiry, re-authorized pushes, per-user caps, cross-site on every transport). Postgres apps add the 3-node cluster case. Browser: the downtime test and the transport-fallback test (`tests/e2e/realtime.spec.ts`) |
| **Accounts** | `TENANCY` isn't `none` | **Tenant isolation suite + guard test** (§4). The most important test files in the app; `teams` adds membership rules to it |
| **Money** | Payments specced | Exact-amount tests in cents, rounding, refunds, idempotent webhooks (same event twice → one effect), signature verification |

### What each suite contains

**Model** (unit, inline `#[cfg(test)]` plus DB tests in `tests/app/models/`):
- `validate()`: every rule, with its boundary values (`max 200` → 200 passes, 201 fails), empty string, whitespace-only and Unicode.
- Queries: ordering and pagination stability (ties broken by `id`), soft-delete exclusion, scopes.
- Uniqueness: duplicate insert → validation error, not a 500 (`sql_err()` mapping).
- Process functions (`order::checkout`): every branch named in the spec, plus failure in the middle rolling back (the transaction is atomic).

**Policy** (unit, inline in `src/policies/<plural>.rs`): a full truth table. For each action, every relevant actor (owner, other user, admin, other tenant) gets a row. Anonymous visitors never reach a policy, because `RequireUser` stops them first; the access matrix (§3) covers them.

```rust
#[test]
fn review_policy_truth_table() {
    let (r, b) = (review(1), book(2));              // review by user 1, book owned by user 2
    for (who, id, create, edit, destroy) in [
        ("author",       1, true, true,  true),
        ("book owner",   2, true, false, false),
        ("other reader", 3, true, false, false),
    ] {
        let u = user(id);
        assert_eq!(can_create(&u, &b), create, "{who} create");
        assert_eq!(can_edit(&u, &r), edit, "{who} edit");
        assert_eq!(can_destroy(&u, &r), destroy, "{who} destroy");
    }
}
```

**Request** (integration, one file per resource). For **every** route, all of these:

| Case | Expected |
|------|----------|
| Happy path | 200 / 303 to the right place / 201; persisted; flash or props correct |
| Invalid input | 422 (htmx, react) or redirect back with `errors` (inertia); nothing persisted; message shown |
| Anonymous | Redirect to login (HTML) / 401 (API), unless the route is public |
| Wrong user | 404 for records outside the actor's scope (don't leak existence); 403 only when the actor can see the record but not perform the action |
| Missing record | 404 |
| Side effects | Job enqueued (`assert_enqueued`), email delivered after drain, broadcast emitted |

**API contract** (`spa` / public API): snapshot each endpoint's JSON with [`insta`](https://insta.rs), redacting volatile fields.

```rust
insta::assert_json_snapshot!(res.json(), { ".id" => "[id]", ".created_at" => "[ts]" });
```

- `spa` mode: snapshots may change freely, because ts-rs + `tsc` catch frontend drift.
- **Public API: snapshots are frozen.** Changing one is a breaking change. Log it in `docs/decisions.md` and tell the human, or add a new version instead. Also test error shape (`{ "errors": { field: [msg] } }`), pagination limits and caps, and that unknown fields are ignored.

**Migration**: CI runs `db migrate → db rollback --steps all → db migrate` on both databases. A data migration (one that transforms rows) gets its own test: seed rows in the old shape, migrate, assert the new shape.

**Jobs / mailers**: every worker gets `perform_job` with a realistic payload, plus the failure path (error → `retryable`, attempt count, then `discarded` after `max_attempts`). Every mailer gets subject, recipient, a key line of body text, and the links in it pointing to working routes (follow them with `app.get`).

**Frontend** (`inertia` / `spa` / islands):
- Vitest component tests for every component with logic (forms, conditional rendering, client validation). Pure presentational components need none. React: `@testing-library/react`. Svelte: `@testing-library/svelte`, with runes-based fakes in `*.svelte.test.ts` ([frontend-svelte](frontend-svelte.md#shared-rules-every-mode)).
- Playwright E2E for each **critical flow**: sign-up/login, the app's primary create→view→edit→delete loop, and anything involving money. Run against `app serve` with a seeded SQLite database. Keep these few, because they're slow.
- `htmx` mode: E2E is optional. Request tests assert the `HX-*` response headers and the partials (`hx-request: true` → fragment, not layout).

## 3. Security tests (every app)

These live in `tests/app/security_test.rs`. Most iterate over `ROUTES`, so new routes are covered automatically.

| Test | How |
|------|-----|
| **Access matrix** | Every `ROUTES` entry declares an access level: `public`, `user`, `admin` or `dev` ([routing](routing.md)). Request every non-public route anonymously → never 2xx. Every `admin` route as a normal user → 403/404. Every `dev` route with production config → 404. Substitute `1` for path params. **Streaming routes** (SSE: `/streams/{stream}`, `/socket/sse`, `/dev/mailbox/events`, anything answering `text/event-stream`) never end, so a sweep that reads the body hangs forever: request them with `TestApp::request_head` (status and headers, body unread), chosen by a `STREAMING: &[&str]` list of route templates next to the test, and a test that every route whose handler returns `Sse<…>` is on it |
| CSRF | A cross-site POST to every unsafe `ROUTES` entry → 403, while logged in, so a 403 can't come from auth |
| Escaping | Create a record whose text fields are `<script>alert(1)</script>`. The page shows it escaped (htmx: `assert_text(evil)` plus `assert_not_contains(evil)`; Askama 0.16 uses numeric entities such as `&#60;`), or the props contain it raw and React escapes it (inertia/spa). Markdown fields go through ammonia |
| Session revocation | Logout invalidates the server session (the old cookie no longer works). Password change or reset ends **every** session of the user, including **open sockets and streams** ([channels](channels.md)). Signing in over another session ends it (and its connections). Reset and magic-link tokens are single-use and expire; emailed links are capped per address per day; the first proof of the address claims an unverified account ([auth](auth.md#claiming-an-unverified-account-pre-hijacking)) |
| Session cookie | Production: the cookie is `__Host-session_token` with `Secure; HttpOnly; SameSite=Lax; Path=/` and no `Domain`, and a planted plain `session_token` cookie doesn't log anyone in |
| **Header matrix** | `tests/app/headers_test.rs`: one request per response class (HTML, authenticated page, JSON, SSE via `request_head`, the WebSocket 101 and the cross-origin 403 over a real socket, a kernel page, 404/403/422/500, a static asset, a redirect), each asserting the full set from [security](security.md#the-header-matrix), in development **and** production (https `APP_URL`). Plus: authenticated responses are `no-store`, every production cookie is `Secure` and `HttpOnly`, and production over plain http sends no TLS-only directives |
| Rate limit | 11th login attempt in the window → 429 (test env uses the production limits; development's 1000 is pinned in `kernel_test.rs`); refused attempts don't extend the window |
| Mass assignment | Posting `owner_id`/`role`/`account_id` in a form doesn't change them |
| Uploads (if any) | Over the spec's limit → 422 with the field message. Over the route's `DefaultBodyLimit` → 413. Wrong type, including a claimed type that doesn't match the magic bytes and SVG → 422. Served with `Content-Disposition`, `nosniff` and an owner check, per [storage](storage.md) |
| Admin as a plain user | Every `Access::Admin` route requested by a logged-in non-admin → never 2xx |
| CSRF fallback | Without fetch metadata (`app.without_fetch_metadata().with_origin(…)`): a foreign `Origin` → 403, `APP_URL` → passes |

**Scanner gates (CI, every push; locally via `/offrails:check`):**
- `cargo deny check` is green;
- gitleaks over the full history is green;
- the OWASP ZAP baseline against the production image has no untriaged WARN (`.zap/rules.tsv`);
- `pnpm audit --prod` in TS modes.

See [security](security.md#automated-enforcement). A failing scanner is a failing test: fix it, or triage it in the committed config with a reason (and, for advisories, a recheck date).

**Every mounted route has a `ROUTES` row.** The access matrix only covers what `ROUTES` lists, so a route mounted without a row is silently untested. Guard it by parsing the router sources:

```rust
#[test]
fn every_mounted_route_is_in_the_routes_table() {
    // Every file that calls .route(...): the app router plus the kernel routers it nests.
    let sources = [
        include_str!("../../src/routes.rs"),
        include_str!("../../src/controllers/dev.rs"),
        include_str!("../../src/kernel/mailbox.rs"),
        include_str!("../../src/kernel/jobs/web.rs"),
    ];
    let mounted: Vec<&str> = sources
        .iter()
        .flat_map(|src| src.split(".route(").skip(1))
        .filter_map(|rest| rest.trim_start().strip_prefix('"')?.split('"').next())
        .collect();
    // If this fails the parser went blind (formatting changed); fix the parser, don't lower the bar.
    assert!(mounted.len() >= 20, "parsed the routers: {mounted:?}");
    for path in mounted {
        assert!(ROUTES.iter().any(|r| r.1 == path), "{path} is routed but has no ROUTES row (add it with its Access)");
    }
}
```

Adjust `sources` to the files in your app that call `.route(`, and the threshold to roughly 80% of your route count.

## 4. Tenant isolation suite (every app with accounts)

Every app with accounts (`TENANCY: per-user | teams | shared`) has two files, generated by `new` (or by `authentication` when accounts arrive later) and kept by every later change. `offrails check` fails when either is missing.

**`tests/app/tenancy_test.rs`** creates two tenants, A and B, each owning one record of **every** tenant-owned model. Acting as A, it requests **every route in `ROUTES` with `Access::User` that takes an id**, using B's ids. The default answer is **404**. A route whose records the spec shows to other tenants is listed in `EXPECTED` with its status and the AC id that allows it (`200` for a shared read, `403` for "visible but not yours"). Ids that aren't records (reset tokens, connection ids) are listed in `NOT_TENANT_IDS` with where they're tested instead. Because the list comes from `ROUTES`, a new route with an id fails the suite until `fill` learns whose id it takes: the agent can't forget it.

The template below is the reference app's file (`TENANCY: per-user`, shared reads). Keep its shape, and replace the five numbered parts: (1) the `Tenant` struct and `tenant()` (one record of every tenant-owned model), (2) `EXPECTED` (from the spec's ACs; empty for strict isolation), (3) `NOT_TENANT_IDS`, (4) `fill`'s prefixes, and (5) `call`'s bodies (send a body that would succeed if allowed, multipart where the route takes `Multipart`, or the extractor answers 400/415 before the scope is checked). With `teams`, the tenants are two teams with one member each, and add `a_mixed_chain_is_404` for nested URLs (A's board with B's list id).

```rust
//! Tenant isolation suite (.offrails/guides/test-standards.md §4), generated from `ROUTES`.
use axum::http::StatusCode;
use my_app::{models::{Book, Review, User}, routes::{Access, ROUTES}};
use crate::{factories::*, support::TestApp};

// (1) One record of every tenant-owned model.
struct Tenant { user: User, book: Book, review: Review }

async fn tenant(app: &TestApp, name: &str) -> Tenant {
    let db = app.db();
    let user = create_user_with_email(db, &format!("{name}@example.com")).await;
    let book = create_book_titled(db, &user, &format!("Secret of {name}")).await;
    let review = create_review(db, &book, &user).await;
    Tenant { user, book, review }
}

// (2) (method, route) → status when A uses B's ids, and the AC that says so. Everything else: 404.
const EXPECTED: &[(&str, &str, StatusCode, &str)] = &[
    ("GET", "/books/{id}", StatusCode::OK, "AC-books-4"),
    ("POST", "/books/{id}", StatusCode::FORBIDDEN, "AC-books-5"),
    ("GET", "/books/{id}/edit", StatusCode::FORBIDDEN, "AC-books-5"),
    ("POST", "/books/{id}/delete", StatusCode::FORBIDDEN, "AC-books-5"),
    ("POST", "/reviews/{id}", StatusCode::FORBIDDEN, "AC-reviews-4"),
    ("GET", "/streams/{stream}", StatusCode::OK, "AC-reviews-5"),
    // …one line per shared route
];

// (3) Ids that aren't a tenant's record, and where they're tested instead.
const NOT_TENANT_IDS: &[(&str, &str)] = &[
    ("/passwords/{token}", "a reset token, not a record: accounts_test"),
    ("/socket/sse/{conn}", "a connection id bound to its session: wire_test"),
];

// (4) Whose id each placeholder is. Panics on one it doesn't know.
fn fill(template: &str, t: &Tenant) -> String {
    let path = template
        .replace("{book_id}", &t.book.id.to_string())
        .replace("{stream}", &format!("book:{}", t.book.id));
    let path = if template.starts_with("/books/") {
        path.replace("{id}", &t.book.id.to_string())
    } else if template.starts_with("/reviews/") {
        path.replace("{id}", &t.review.id.to_string())
    } else {
        path
    };
    assert!(
        !path.contains('{'),
        "{template} takes an id this suite doesn't know. Teach `fill` in \
         tests/app/tenancy_test.rs whose record it is, or list it in NOT_TENANT_IDS with a reason."
    );
    path
}

fn in_scope(template: &str, access: Access) -> bool {
    access == Access::User
        && template.contains('{')
        && !NOT_TENANT_IDS.iter().any(|(t, _)| *t == template)
}

// (5) A request that would succeed if it were allowed.
async fn call(app: &TestApp, method: &str, template: &str, path: &str) -> StatusCode {
    match (method, template) {
        ("GET", t) if t.starts_with("/streams/") => app.open_stream(path).await.0, // endless body
        ("GET", _) => app.get(path).await.status,
        ("POST", "/books/{id}") => {
            let fields = [("title", "Taken"), ("author", "Mallory")];
            app.post_multipart(path, &fields, &[]).await.status
        }
        ("POST", _) => {
            let fields = [("title", "Taken"), ("body", "Mine now"), ("stars", "1")];
            app.post_form(path, &fields).await.status
        }
        (m, _) => panic!("{m} {template}: teach `call` this method"),
    }
}

#[tokio::test]
async fn another_tenants_ids_answer_404_or_their_specd_status_on_every_route() {
    let app = TestApp::new().await;
    let a = tenant(&app, "alice").await;
    let b = tenant(&app, "bob").await;
    app.login_as(&a.user).await;
    let mut checked = 0;
    for (method, template, handler, access) in ROUTES {
        if !in_scope(template, *access) {
            continue;
        }
        let (want, why) = EXPECTED
            .iter()
            .find(|(m, t, _, _)| m == method && t == template)
            .map(|(_, _, s, why)| (*s, *why))
            .unwrap_or((StatusCode::NOT_FOUND, "B's records don't exist for A"));
        let got = call(&app, method, template, &fill(template, &b)).await;
        assert_eq!(got, want, "{method} {template} ({handler}) with another tenant's ids answered \
            {got}, want {want} ({why}). Look records up with `<model>::find_for(db, &user, id)`.");
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} routes were checked; is ROUTES complete?"); // ~80% of id routes
    // Nothing of B's changed.
    assert_eq!(reload_book(app.db(), b.book.id).await, b.book);
    assert_eq!(reload_review(app.db(), b.review.id).await, b.review);
}

#[tokio::test]
async fn every_expected_entry_is_a_real_route() {
    for (method, template, _, why) in EXPECTED {
        assert!(ROUTES.iter().any(|(m, t, _, a)| m == method && t == template && in_scope(t, *a)),
            "EXPECTED lists {method} {template} ({why}), which is not an in-scope route");
    }
    for (template, why) in NOT_TENANT_IDS {
        assert!(ROUTES.iter().any(|(_, t, _, _)| t == template),
            "NOT_TENANT_IDS lists {template} ({why}), which is not a route");
    }
}

#[tokio::test]
async fn own_ids_work_so_the_404s_are_not_a_blanket() {
    let app = TestApp::new().await;
    let a = tenant(&app, "alice").await;
    app.login_as(&a.user).await;
    for (method, template, _, access) in ROUTES {
        if *method == "GET" && in_scope(template, *access) {
            let got = call(&app, method, template, &fill(template, &a)).await;
            assert!(got.is_success() || got.is_redirection(), "GET {template} with A's own ids answered {got}");
        }
    }
}
```

Add one more test per list page and export: as A, B's titles never appear (`assert_not_contains("Secret of bob")`), unless `EXPECTED`'s ACs make that list shared. When a model is added, add it to `Tenant` in the same change.

**Prove the suite bites** once, when you write it: comment out one policy check or one owner filter, watch the suite fail with the route's name, and restore it.

**`tests/app/guard_test.rs`** reads the handler sources and fails on lookups that skip the actor ([models](models.md#scoped-lookups-tenancy)). It uses only `std` and is the same in every app; copy it verbatim:

```rust
//! Tenancy guard (.offrails/guides/models.md#scoped-lookups-tenancy). Generated by `new`; keep it.
//!
//! Handlers reach data only through actor-scoped model functions (`find_for(db, &user, id)`).
//! The compiler stops most unscoped calls (raw `find`/`destroy` are `pub(in crate::models)`);
//! this test catches the rest by reading the handler sources. A line that is unscoped on
//! purpose ends with `// unscoped: <AC id or reason>`.

use std::{fs, path::Path};

/// Where request handlers live. Missing directories are skipped.
const HANDLER_DIRS: &[&str] = &["src/controllers", "src/channels", "src/graphql", "src/api"];

/// (pattern, what to do instead)
const FORBIDDEN: &[(&str, &str)] = &[
    (
        "RequireUser(_",
        "the handler requires a user and then ignores it: scope the query with it",
    ),
    (
        "ApiUser(_",
        "the handler ignores the token's user: scope the query with it",
    ),
    (
        "::find(",
        "look records up with `<model>::find_for(db, &user, id)`",
    ),
    (
        "::destroy(",
        "delete with `<model>::destroy_for(db, &user, record)`",
    ),
    ("::all(", "list with `<model>::all_for(db, &user, …)`"),
    (
        "::find_system(",
        "`find_system` is for jobs and tasks; a handler has an actor",
    ),
    (
        "Entity::find",
        "query through the model's scoped functions, not the entity",
    ),
];

const MARKER: &str = "// unscoped:";

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn handlers_only_use_scoped_lookups() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in HANDLER_DIRS {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(
        !files.is_empty(),
        "no handler sources found under {HANDLER_DIRS:?}"
    );
    let mut hits = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file).expect("read source");
        for (n, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || line.contains(MARKER) {
                continue;
            }
            for (pattern, fix) in FORBIDDEN {
                if line.contains(pattern) {
                    let rel = file.strip_prefix(root).unwrap_or(file).display();
                    hits.push(format!("{rel}:{}: `{pattern}`: {fix}", n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "unscoped data access in handlers (end the line with `{MARKER} <AC id or reason>` \
         only when the spec makes it public):\n{}",
        hits.join("\n")
    );
}

#[test]
fn every_unscoped_marker_gives_a_reason() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in HANDLER_DIRS {
        rust_files(&root.join(dir), &mut files);
    }
    for file in &files {
        let src = fs::read_to_string(file).expect("read source");
        for (n, line) in src.lines().enumerate() {
            if let Some((_, reason)) = line.split_once(MARKER) {
                assert!(
                    reason.trim().len() >= 8,
                    "{}:{}: `{MARKER}` needs a reason (an AC id or why the data is public)",
                    file.display(),
                    n + 1
                );
            }
        }
    }
}
```

## 5. Coverage and test strength

| Gate | Tool | Threshold | Where |
|------|------|-----------|-------|
| Line coverage, whole crate | `cargo llvm-cov nextest` | **≥ 80%** | CI (SQLite job) |
| Line coverage, `src/models`, `src/policies` | same, exported as JSON and checked with `jq` ([testing](testing.md#coverage-and-mutation-testing)) | **≥ 90%** | CI |
| Mutation score, `src/policies` + `src/models` | `cargo mutants --file 'src/policies/**' --file 'src/models/**'` | **0 surviving mutants in policies.** Review survivors in models | Weekly CI job and `/offrails:check --deep` |
| Frontend (TS modes) | `vitest --coverage` | ≥ 70% of `web/src/components`, `web/src/pages` or `routes` | CI |

- Coverage is a floor, not a goal. Never write a test just to execute lines, and never exclude code from coverage to pass the gate without a `docs/decisions.md` entry.
- **The one standing exclusion is `src/main.rs`** (CLI glue: argument parsing, `serve`, signals): `--ignore-filename-regex 'src/main\.rs$'`. Everything else counts, the kernel included.
- **The bare `new` skeleton measures ~76%.** Its jobs kernel needs `tests/app/kernel_jobs_test.rs` (new.md step 7) to pass 80%. Generate that test with the kernel; never lower the gate. Measured on the reference app after auth and two resources: 88.8% overall, models 92–97% per file, policies 100%.
- A surviving mutant in a policy means the truth table is missing a row. Add the row, don't silence the mutant.
- **Equivalent mutants** are the exception: a mutation that can't change behavior. Example: the spec says any reader may review any book, so `can_create → true` *is* the function. Exclude each one by name in `.cargo/mutants.toml`, with the reason in a comment, and log it in `docs/decisions.md`:
  ```toml
  exclude_re = [
      # Spec (reviews.md): any logged-in reader may review any book, so this policy is constant `true`.
      "src/policies/reviews.rs:.*replace can_create -> bool with true",
  ]
  ```
- **Mutants are slow.** cargo-mutants rebuilds once per mutant. On the reference app, policies gave 14 mutants and took 11 minutes; models give ~200, which means hours. Locally, `/offrails:check --deep` runs policies only. Models run in the weekly CI job.

## 6. Performance guards

- **N+1 queries:** `TestApp` counts SQL statements (SeaORM's metric callback, [testing](testing.md)). Every index page, list endpoint and show page with a child list gets one test. It seeds 1 record, then 19 more, and asserts the GET's query count doesn't grow:
  ```rust
  app.assert_query_count_stable("/books", async |n| {
      for _ in 0..n { create_book(app.db(), &create_user(app.db()).await).await; }
  }).await;
  ```
  Load related rows with `find_also_related`, or in one `is_in` batch (`blob::by_ids`), never per row.
- **Suite time:** the SQLite suite should stay under ~60s. A test over 2s is slow. Nextest's `slow-timeout` reports it. Fix it, or move it to `#[ignore]` with a decision entry. The usual culprit is password hashing in factories: hash the factory password **once** per test binary and reuse the digest ([auth gotchas](auth.md#gotchas-agents-get-wrong)). In the reference app that cut a 47s test to under 1s.
- **Postgres test databases** are named `test_<crate>_<tag>_tpl` (the template) and `test_<crate>_<tag>_<n>` (the slots, reset before each test), and `app task test_db_cleanup` drops only this app's databases that have no open connections ([testing](testing.md#postgres-test-databases)). So it's safe on a shared server and while a suite runs. Still, never call it from a test.

## 7. Test quality rules

1. **Behavior, not implementation.** Assert what the user, the client or the database sees. Never assert on private functions or log lines.
2. **Real database, both backends.** No mocked DB and no mocked SeaORM. External services (SMTP, S3, payment APIs) are replaced at the kernel boundary (`Mailer::Memory`, local `object_store`, a fake HTTP server via `tokio` + `axum` on a random port), never by mocking app code.
3. **Deterministic.** No sleeps, no real time (pass `now`), no randomness without a fixed seed, no network, no ordering assumptions without `ORDER BY`. **A flaky test is a P0 bug.** CI runs nextest with retries disabled.
4. **Isolated.** Each test builds its own database through `TestApp::new()`. No shared fixtures and no test-order dependence. Factories set only the fields the test cares about.
5. **One behavior per test**, named after it. Several asserts are fine if they describe one behavior.
6. **Bug fix = regression test first.** Reproduce the bug in a failing test (`regression_<short_description>`, citing the issue or spec), then fix it.
7. **Never weaken a test to make it pass.** Change a test's expectation only when the spec changed, and cite the spec change in the commit.
8. **Never disable a security check in tests** (CSRF, auth). Use the helpers that satisfy it.

## 8. Compliance: how this is enforced

| Layer | Enforces |
|-------|----------|
| Definition of Done ([workflow](../workflow.md#definition-of-done)) | All suites green on SQLite and Postgres; AC traceability check; coverage gates |
| CI (`.github/workflows/ci.yml`, [testing](testing.md#ci)) | The same gates on every push, plus migration round-trips and the Postgres run; weekly mutation job |
| `/offrails:check` | Runs the DoD and writes **`docs/test-report.md`** (below) |
| `/offrails:doctor` | Audits test *quality*: missing archetype suites, missing access-matrix rows, tenant setup incomplete, weakened assertions, `#[ignore]` without a decision |
| Plugin hooks | The Stop gate keeps the tree compiling. The convention check flags `sleep(` in tests |
| Real-browser smoke (§9) | Every UI-affecting change, before calling it done |

### `docs/test-report.md` (generated, committed)

This is the human's view of what is proven. `/offrails:check` regenerates it:

```markdown
# Test report (2026-10-12, a1b2c3d)

| Suite | SQLite | Postgres |
|-------|--------|----------|
| Rust (212 tests) | ✅ | ✅ |
| Frontend (48) | ✅ | n/a |
| E2E (5) | ✅ | n/a |

Coverage: 86% (models 94%, policies 100%) · Mutants: 0 surviving (policies)

| Feature | Criteria | Proven |
|---------|----------|--------|
| books.md | 7 | 7 ✅ |
| reviews.md | 5 | 4 ⚠️ AC-reviews-5 untested |
```

## 9. Real-browser smoke

There are two complementary smokes:

- **The app's own flows** (this section): `tests/e2e/` or `web/e2e/`, written per app, covering the spec's critical flows. A fresh app from `new` has no flows yet, so the package is created with the **first critical flow** (`generators/authentication.md`, or the first `scaffold` when there are no accounts). Until then the kernel smoke below covers the browser.
- **The kernel pages** (`/dev`, `/dev/mailbox`, `/admin/jobs`): `.offrails/tools/browser-smoke/run.sh`, shipped by the framework ([dev-tools](dev-tools.md#browser-smoke)). It checks styling after boosted navigation and Back/Forward behavior, and needs no per-app code.
- **Every page after a boosted link** (htmx mode): `.offrails/tools/browser-smoke/smoke-boosted.mjs` clicks each page's live elements after a full load and again after a boosted link, and fails on one that only works on the full load ([testing](testing.md#browser-smoke-boosted-navigation)). It reads `docs/routes.md`, so it needs no per-app code either; `cargo offrails verify --full` runs it when `BASE` is set.

Run both after any UI-affecting change.

`oneshot` request tests can't see styling, hx-boost swaps, `<head>` changes, history (Back/Forward, bfcache), `EventSource` or console errors. These bugs were all found in real apps with every HTTP test green:

| Bug | Why the HTTP tests missed it |
|-----|---------------------|
| Live reviews never appeared: SSE events were sent with empty `data` | Browsers silently drop such events. The test helper accepted them; it now requires `data` the way `EventSource` does ([realtime](realtime.md)) |
| After Back/Forward, a page's live updates stopped | htmx restored a DOM snapshot without a live `EventSource` |
| Back after deleting a record showed it again | htmx's history snapshot (fix: `historyCacheSize: 0`, [frontend-htmx](frontend-htmx.md)) |
| Kernel pages (`/dev/mailbox`) rendered unstyled when reached by an `hx-boost` link | A boosted swap keeps the app's `<head>`; links to kernel pages need `hx-boost="false"` |
| A console 404 for `/favicon.ico` on every first visit | No icon link in the layout |
| Magic-link login "succeeded" (303, account created) and bounced back to the login page | `bin/dev` served a **self-signed** certificate; after "Proceed (unsafe)" Chrome drops `Set-Cookie`. The old smoke ran Chrome with `--ignore-certificate-errors`, which hides it ([dev-tools](dev-tools.md#https-in-development)) |
| Dark mode unreadable: `bg-white` cards with light text, 1.09:1 | The body flipped with the OS; the cards had no `dark:` partner. Lighthouse checks one scheme ([§10](#10-accessibility-wcag-22-aa-in-every-theme), [§11](#11-theme-and-css-guards)) |

**Rule:** any change that affects what the browser shows or does (templates, layout, JS, CSS, routes that pages link to, realtime) requires a real-browser smoke of the changed flow before it's done. Assert, at minimum:

1. **Styling is applied:** the computed style of a known element (for example, the primary button's `backgroundColor` is not `rgba(0, 0, 0, 0)`).
2. **The flow works through the UI:** fill and submit the forms, upload files, follow links and boosted navigation.
3. **History:** state after Back and Forward, including Back after a delete (expect the 404 page, not the old record) **and Forward from that 404** (expect the page you came from, not a stale 404).
4. **Realtime**, if present: two browser contexts, one acting and one watching.
5. **Console:** no errors, uncaught exceptions or CSP violations (collect `console` errors and `pageerror`). Assert this before any step that deliberately loads a 404.
6. **Login sticks** (apps with accounts): log in through the real form, then load a fresh page and assert you're still logged in. **Never** launch the browser with `--ignore-certificate-errors`, `ignoreHTTPSErrors: true` or `acceptInsecureCerts` for this: run against the URL `bin/dev` printed (a trusted HTTPS certificate, or plain http). `.offrails/tools/browser-smoke/run.sh login` does exactly this and fails on an untrusted certificate before it starts ([dev-tools](dev-tools.md#browser-smoke)).

**How:**

| Situation | Tool |
|-----------|------|
| Interactive, while building (an agent with the chrome-devtools MCP) | Drive the flow with the MCP: `new_page` with an `isolatedContext` per user, `fill_form`, `upload_file` (files must sit inside the MCP's workspace roots), `wait_for`, `list_network_requests`, `list_console_messages`, `evaluate_script` |
| Repeatable, committed, and whenever the MCP can't attach (for example, the user's Chrome profile is locked) | **Playwright Test against the installed Chrome**: `channel: "chrome"`, with no browser download (`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` on install). Playwright always uses a throwaway profile |

Where the committed smoke lives:

- **`inertia` / `spa` / islands:** `web/e2e/smoke.spec.ts`. Playwright is already a blessed dev dependency ([stack](../stack.md)).
- **`htmx`:** `tests/e2e/`, a separate dev-only package (`package.json` with only `@playwright/test`, `playwright.config.ts`, `smoke.spec.ts`; `node_modules/` and `test-results/` are git-ignored). This is the one sanctioned Node in an htmx app. It never touches the app's build, Docker image or runtime, and the app runs without it.
  - Why not `pnpm dlx`: a spec file must `import { test } from "@playwright/test"`, and a `dlx` install can't be resolved from the spec's directory.
  - Why not a puppeteer-core script: Playwright is already the framework's browser tool, and its runner gives web-server startup, traces on failure and auto-waiting assertions for free.

`playwright.config.ts` (htmx; verified with Playwright 1.64 and Chrome on 2026-10-12):

```ts
import { defineConfig } from "@playwright/test";
const PORT = 3018;                                   // its own port; never one the human uses
export default defineConfig({
  testDir: ".", retries: 0, workers: 1, timeout: 60_000,
  use: { baseURL: `http://localhost:${PORT}`, channel: "chrome", headless: true, trace: "retain-on-failure" },
  webServer: {
    command: "rm -f storage/e2e.sqlite3* && cargo run -q -- db migrate && cargo run -q -- serve",
    cwd: "../..", url: `http://localhost:${PORT}/up`, timeout: 300_000, reuseExistingServer: false,
    env: { APP_ENV: "development", PORT: String(PORT), APP_URL: `http://localhost:${PORT}`,
           DATABASE_URL: "sqlite://storage/e2e.sqlite3?mode=rwc", STORAGE_PATH: "storage/e2e-files", RUST_LOG: "warn" },
  },
});
```

Run it with `pnpm --dir tests/e2e install` (once), then `pnpm --dir tests/e2e exec playwright test`. In CI it's optional: add `npx playwright install chrome` (or use a runner with Chrome) and run it after the Rust jobs.

**Process hygiene:** your own port, stop only your own PID, never `pkill` by name ([cli](../cli.md#running-the-app-from-an-agent), the canonical rules).


## 10. Accessibility (WCAG 2.2 AA), in every theme

Accessibility is checked like security: automatically, on every route, and in **every color scheme the app ships**. Lighthouse alone isn't enough. It audits **one** page in **one** scheme (the default light), so a dark-theme-only or light-theme-only contrast bug passes with a 100. A real one: rustoffrails.org's "first prompt" terminal card had near-white text on a white card in the light theme, with Lighthouse at 100.

**What every app with a UI runs** (in `tests/e2e/` or `web/e2e/`, the same Playwright package as §9; `@axe-core/playwright`, versions via `pnpm add`):

```ts
import AxeBuilder from "@axe-core/playwright";
const ROUTES = ["/", "/start", "/books", "/books/1"];          // every public route, plus logged-in ones with a fixture user
for (const scheme of ["light", "dark"] as const)
  for (const width of [1280, 390])
    for (const route of ROUTES)
      test(`a11y ${route} ${scheme} ${width}`, async ({ page }) => {
        await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" });
        await page.setViewportSize({ width, height: 900 });
        // If the app has a theme toggle stored in a cookie, also set it, so the toggle's paths are covered too
        await page.goto(route);
        const r = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
        expect(r.violations, JSON.stringify(r.violations.map(v => [v.id, v.nodes.length]))).toEqual([]);
      });
```

- **Zero violations.** An exception needs a `docs/decisions.md` entry naming the rule, the element and why, plus the human's OK. Never disable a rule globally.
- **Keyboard:** Tab reaches every interactive element in a sensible order, with a **visible** focus ring (no `outline: none` without a replacement). The skip link moves focus to `<main>`. Menus and dialogs open with Enter/Space, close with Escape, and return focus.
- **Motion:** under `prefers-reduced-motion: reduce`, no animations or transitions (View Transitions included).
- **Forms:** every input has a `<label>`; errors are linked with `aria-describedby` and announced (`aria-live` or focus); invalid fields get `aria-invalid`.
- **Contrast** includes code blocks, terminal cards, placeholders, disabled states, focus rings and text over images, in both themes. Decorative-but-readable text (like a terminal transcript) still has to meet 4.5:1.
- **CI:** an `a11y` job boots the release build and runs the suite. It's a required check.
- **Kernel pages** (`/dev`, `/dev/mailbox`, `/admin/jobs`) are held to the same standard: they're the framework's own UI.
- **The framework's own smoke runs it too:** `SCENARIO=tests/e2e/smoke.scenario.mjs BASE=… bash .offrails/tools/browser-smoke/run.sh a11y` runs axe (axe-core 4.13.0, WCAG 2.2 AA tags) on the scenario's `a11yPaths` (and `a11yAuthPaths` after its `login`) in light, then dark. No Node in the app; no certificate overrides. In the reference app it found a light-mode `link-in-text-block` failure (a `.link` with no underline) on the first run, and, with one `dark:bg-*` partner removed, a 1.07:1 dark-mode `color-contrast` failure while light passed.
- **Use a palette that is correct in both schemes from the start** ([views](views.md#dark-mode)); [§11](#11-theme-and-css-guards) catches a missing partner in every template, including the pages no smoke visits.

`/offrails:check` runs it with the e2e suite, and `/offrails:doctor` reports any public route missing from `ROUTES` above.

## 11. Theme and CSS guards

Two template-level guards, in `tests/app/theme_test.rs` (every app with HTML templates). They run in milliseconds and cover every template, including pages no browser check visits:

1. **Both color schemes.** The layout's body turns dark with the OS ([views](views.md#dark-mode)), so in every `class="…"` (and every `@apply` list in `assets/app.css`):
   - **(a)** a light surface (`bg-white`, `bg-<color>-50/100/200`, with any variant like `hover:`) needs a `dark:<same variants>bg-*` partner;
   - **(b)** dark text (`text-black`, `text-<color>-700…950`) needs a `dark:text-*` partner, because the surface under it turns dark;
   - **(c)** a faint tone (`text-<gray family>-400…600`, `text-<color>-500/600`, e.g. `text-slate-500`, `text-red-600`) needs a `dark:text-*` partner;
   - exempt: text on a background that is mid or dark in both schemes (`bg-<color>-300…950` with no `dark:bg-*`, e.g. a badge). Askama tags inside an attribute are dropped, so both branches of an `{% if %}` form one list: put each branch's partners in that branch.
2. **Stale CSS.** Every class a template uses has its selector in the built `static/app.css` (escaped like `CSS.escape`: `dark:bg-gray-950` → `.dark\:bg-gray-950`, `md:w-1/2` → `.md\:w-1\/2`). Tailwind emits only the classes it saw when it last ran, so a template edited while no watcher ran renders with a silently missing class (a `dark:` partner that does nothing). Skipped when the CSS isn't built. **For each class it lists, in this order:** a class nothing styles or uses (a leftover from an earlier design, a typo) is **deleted from the template**; a real utility means the CSS is stale, so rebuild it; only a class that's a hook for JS, htmx or tests goes in `kernel::css::HOOK_CLASSES`, each with its reason. Never allowlist a dead class to make the guard pass.

**TS modes** (`inertia`, `spa`, islands): the same rules apply to `className="…"`/`class="…"` in `web/src/**/*.{tsx,svelte}` (add those files to `class_lists` and match both attribute names; the HTML shell and error pages stay in `templates/`). The stale-CSS check is htmx-only: Vite builds the CSS with the bundle. Every mode keeps the palette in [views](views.md#dark-mode).

Proven in the reference app: the guard listed 75 unpaired tones in a light-only app (layout, nav, flash, errors, forms, lists, the mailbox hint, auth pages and the component layer); after fixing them, the stale-CSS guard listed the 67 new `dark:` classes until Tailwind ran; removing one partner fails rule (a) at that line.

The shared reading code lives in the kernel, because `/dev/introspect.json` and the development CSS banner use it too:

```rust
//! What the templates ask of the CSS. Tailwind only emits the classes it saw when it last ran, so
//! a template edited while no watcher ran renders with silently missing styles (a `dark:` partner
//! that does nothing). Used by the theme guard tests, `/dev/introspect.json` and the development
//! CSS banner. Spec: .offrails/guides/views.md#dark-mode, test-standards.md#11-theme-and-css-guards

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Rendered with static/app.css. Kernel pages and mailers carry their own styles.
pub const TEMPLATES: &str = "templates";
pub const SKIP_DIRS: &[&str] = &["templates/kernel", "templates/mailers"];
/// The component layer: `@apply` lists follow the same color rules as class attributes.
pub const CSS_SOURCE: &str = "assets/app.css";
pub const CSS_BUILT: &str = "static/app.css";
/// Classes that are hooks for JS, htmx or tests, never styles. Each needs its reason.
pub const HOOK_CLASSES: &[(&str, &str)] = &[
    // ("errors", "the error summary's hook in tests"),
];

/// Template classes with no selector in the built CSS, as "file:line: class".
/// `None` when the CSS isn't built (nothing to compare).
pub fn missing_classes() -> Option<Vec<String>> {
    let css = fs::read_to_string(CSS_BUILT).ok()?;
    let mut missing = Vec::new();
    for (at, tokens) in class_lists(false) {
        for token in tokens {
            if !HOOK_CLASSES.iter().any(|(c, _)| *c == token) && !has_selector(&css, &token) {
                missing.push(format!("{at}: {token}"));
            }
        }
    }
    Some(missing)
}

/// Tailwind's selector for a class: `CSS.escape` rules (`md:w-1/2` → `md\:w-1\/2`, `2xl:` → `\32 xl\:`).
pub fn css_escape(class: &str) -> String {
    let mut out = String::new();
    for (i, c) in class.chars().enumerate() {
        if i == 0 && c.is_ascii_digit() {
            out.push_str(&format!("\\3{c} "));
        } else if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

/// `.class` followed by something that ends the identifier (`.p-4` doesn't count for `.p-40`).
pub fn has_selector(css: &str, class: &str) -> bool {
    let selector = format!(".{}", css_escape(class));
    css.match_indices(&selector).any(|(i, m)| {
        css[i + m.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '\\')))
    })
}

/// Every `class="…"` in the templates (and, with `with_apply`, every `@apply …;` in the CSS
/// source) as ("file:line", tokens). Askama tags inside an attribute are dropped, so both
/// branches of an `{% if %}` form one list.
pub fn class_lists(with_apply: bool) -> Vec<(String, Vec<String>)> {
    let mut files = Vec::new();
    html_files(Path::new(TEMPLATES), &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        let mut offset = 0;
        while let Some(i) = src[offset..].find("class=\"") {
            let start = offset + i + 7;
            let value = attribute_value(&src[start..]);
            let line = src[..start].lines().count().max(1);
            out.push((
                format!("{}:{line}", file.display()),
                tokens(&strip_askama(value)),
            ));
            offset = start + value.len();
        }
    }
    if with_apply && let Ok(css) = fs::read_to_string(CSS_SOURCE) {
        for (n, line) in css.lines().enumerate() {
            if let Some(i) = line.find("@apply ") {
                let list = line[i + 7..].split(';').next().unwrap_or("");
                out.push((format!("{CSS_SOURCE}:{}", n + 1), tokens(list)));
            }
        }
    }
    out
}

fn html_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if SKIP_DIRS.iter().any(|s| path.starts_with(s)) {
            continue;
        }
        if path.is_dir() {
            html_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

/// The attribute value up to its closing quote, skipping quotes inside `{{ }}` / `{% %}`.
fn attribute_value(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'{', Some(b'{' | b'%')) => (depth, i) = (depth + 1, i + 1),
            (b'}' | b'%', Some(b'}')) if depth > 0 => (depth, i) = (depth - 1, i + 1),
            (b'"', _) if depth == 0 => return &s[..i],
            _ => {}
        }
        i += 1;
    }
    s
}

/// Drops `{{ … }}`, `{% … %}` and `{# … #}`, keeping the text between them.
pub fn strip_askama(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let open = &rest[i..];
        let close = match open.as_bytes().get(1) {
            Some(b'{') => "}}",
            Some(b'%') => "%}",
            Some(b'#') => "#}",
            _ => {
                out.push_str(&rest[..=i]);
                rest = &rest[i + 1..];
                continue;
            }
        };
        out.push_str(&rest[..i]);
        out.push(' ');
        rest = open.find(close).map_or("", |j| &open[j + 2..]);
    }
    out.push_str(rest);
    out
}

fn tokens(list: &str) -> Vec<String> {
    list.split_whitespace().map(str::to_owned).collect()
}
```

The test (replace `<crate>` with the app's crate name):

```rust
//! Theme and CSS guards (.offrails/guides/test-standards.md#11-theme-and-css-guards). Generated by `new`; keep it.
//!
//! 1. Both color schemes: the layout's body turns dark with the OS, so every light surface, dark
//!    text and faint tone in a template needs its `dark:` partner, or it renders unreadable
//!    (a `bg-white` card with light text: 1.09:1). axe in both schemes finds it per page; this finds
//!    it in every template, including the ones no smoke visits.
//! 2. Stale CSS: every class a template uses has its selector in the built `static/app.css`
//!    (Tailwind only emits classes it saw when it last ran). Skipped when the CSS isn't built.

use <crate>::kernel::css::{
    CSS_BUILT, CSS_SOURCE, class_lists, css_escape, has_selector, missing_classes, strip_askama,
};

const GRAYS: &[&str] = &["slate", "gray", "zinc", "neutral", "stone"];

#[test]
fn templates_pair_every_light_tone_with_a_dark_partner() {
    let mut problems = Vec::new();
    for (at, tokens) in class_lists(true) {
        let fixed_surface = tokens.iter().any(|t| is_fixed_surface(t))
            && !tokens.iter().any(|t| t.starts_with("dark:bg-"));
        for token in &tokens {
            let (variants, utility) = split_variants(token);
            if variants.contains("dark:") {
                continue;
            }
            let has = |what: &str| {
                let partner = format!("dark:{variants}{what}");
                tokens.iter().any(|t| t.starts_with(&partner))
            };
            if is_light_surface(utility) && !has("bg-") {
                problems.push(format!(
                    "{at}: `{token}` needs a `dark:{variants}bg-*` partner (rule a: a light surface stays light while the body turns dark)"
                ));
            }
            if fixed_surface {
                continue; // its own mid/dark background in both schemes: text on it doesn't flip
            }
            if is_dark_text(utility) && !has("text-") {
                problems.push(format!(
                    "{at}: `{token}` needs a `dark:{variants}text-*` partner (rule b: dark text on a surface that turns dark)"
                ));
            } else if is_faint_text(utility) && !has("text-") {
                problems.push(format!(
                    "{at}: `{token}` needs a `dark:{variants}text-*` partner (rule c: a faint tone fails contrast on a dark surface)"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "colors that break one scheme (palette: .offrails/guides/views.md#dark-mode):\n{}",
        problems.join("\n")
    );
}

#[test]
fn built_css_has_every_class_the_templates_use() {
    let Some(missing) = missing_classes() else {
        eprintln!(
            "skipped: {CSS_BUILT} isn't built (bin/tailwindcss -i {CSS_SOURCE} -o {CSS_BUILT})"
        );
        return;
    };
    assert!(
        missing.is_empty(),
        "classes with no rule in {CSS_BUILT}: delete dead classes from the template, rebuild the CSS \
         (bin/dev runs `bin/tailwindcss --watch`), or list a JS/test hook in kernel::css::HOOK_CLASSES \
         with its reason:\n{}",
        missing.join("\n")
    );
}

// --- the rules ---------------------------------------------------------------------------------

/// `hover:dark:bg-x` → ("hover:dark:", "bg-x"); a leading `!` (important) is dropped.
fn split_variants(token: &str) -> (&str, &str) {
    let cut = token.rfind(':').map_or(0, |i| i + 1);
    (&token[..cut], token[cut..].trim_start_matches('!'))
}

/// `bg-red-50/80` → ("red", 50) for prefix "bg-".
fn color_shade<'a>(utility: &'a str, prefix: &str) -> Option<(&'a str, u32)> {
    let rest = utility.strip_prefix(prefix)?;
    let rest = rest.split('/').next()?;
    let (color, shade) = rest.rsplit_once('-')?;
    Some((color, shade.parse().ok()?))
}

fn is_light_surface(u: &str) -> bool {
    u == "bg-white" || color_shade(u, "bg-").is_some_and(|(_, s)| s <= 200)
}

/// A background that is mid or dark in both schemes (a badge, a primary button).
fn is_fixed_surface(token: &str) -> bool {
    let (variants, u) = split_variants(token);
    variants.is_empty() && (u == "bg-black" || color_shade(u, "bg-").is_some_and(|(_, s)| s >= 300))
}

fn is_dark_text(u: &str) -> bool {
    u == "text-black" || color_shade(u, "text-").is_some_and(|(_, s)| s >= 700)
}

fn is_faint_text(u: &str) -> bool {
    color_shade(u, "text-").is_some_and(|(c, s)| {
        if GRAYS.contains(&c) {
            (400..=600).contains(&s)
        } else {
            (500..=600).contains(&s)
        }
    })
}

#[test]
fn the_guards_themselves() {
    assert!(is_light_surface("bg-white") && is_light_surface("bg-amber-50"));
    assert!(!is_light_surface("bg-indigo-600") && !is_light_surface("bg-white/0x"));
    assert!(is_dark_text("text-gray-900") && is_dark_text("text-red-800"));
    assert!(is_faint_text("text-slate-500") && is_faint_text("text-red-600"));
    assert!(!is_faint_text("text-gray-300") && !is_faint_text("text-white"));
    assert_eq!(split_variants("dark:hover:bg-x"), ("dark:hover:", "bg-x"));
    assert_eq!(css_escape("md:w-1/2"), "md\\:w-1\\/2");
    assert_eq!(css_escape("2xl:p-4"), "\\32 xl\\:p-4");
    assert!(has_selector(".p-4{x}.p-40{y}", "p-4") && !has_selector(".p-40{y}", "p-4"));
    assert_eq!(
        strip_askama("a {% if x %}b{% else %}c{% endif %} {{ d }}")
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}
```

The development layout banner and `/dev/introspect.json` (`css.stale`, `css.missing_classes`) report the same missing classes while you work ([dev-tools](dev-tools.md#introspection)).
