# Changelog

Each entry lists convention changes and what an existing app must do about them. `/offrails:upgrade` reads this file, so write the **App migration** notes as instructions for an agent.

## Unreleased

### Realtime: the Off Rails Wire protocol (offrails.v1), resume after downtime, transport fallback, connection security

Spec: `guides/channels.md` (protocol, transports, security), `guides/realtime.md` (resume). Verified in the reference app: 223 tests on SQLite and Postgres 18 (39 realtime: conformance, security, all three transports, a 3-node failover), two Playwright specs (a server restart with changes made while it was down; WebSocket → SSE → long-polling), and a two-context Chrome session.

- **Fixed: signals sent while a tab was disconnected were lost** (a deploy, a restart, a blip): live regions silently went stale. Every signal now carries its id; pages render a cursor (`?since=`); reconnects replay what was missed (or `refresh`); `app.js` carries the cursor across htmx-ext-sse's reconnects.
- **Fixed: live regions were dead after a boosted navigation.** The sse extension was loaded in a page's own `<head>`, which a boosted swap never runs. It's in the layout now.
- **Our own protocol, offrails.v1** (replaces the Phoenix v2 array format): negotiated subprotocol, self-describing JSON object frames (`join`/`leave`/`push`/`ping` up; `ok`/`err`/`event`/`presence`/`pong`/`slow`/`bye` down), stable error codes, `bye` before every close, `seq` resume, WebSocket ping control frames instead of app heartbeats.
- **Typed channels:** rooms declare `In`/`Out` serde enums (`#[derive(TS)]` in TS modes); undeclared events are refused before the room runs.
- **Transport fallback:** WebSocket → SSE + POST → long-polling, chosen automatically, remembered per tab, upgraded back when possible. `APP_REALTIME_TRANSPORTS` lists what the server offers.
- **Connection security:** logout, password change and revocation close that session's sockets and streams at once (4401), across processes and nodes; periodic revalidation closes expired sessions (`APP_REALTIME_REVALIDATE_SECS`); pushes re-authorized against current state; per-user caps (connections, topics, joins/min); presence never carries emails; cross-site refused on every transport.
- **`/dev/sockets`** shows each connection's transport and a live decoded frame log (development only).

**App migration** (reconcile, don't re-create; apps without channels do steps 1–3 only):

1. **SSE resume.** Detect `pub struct Signal { pub stream, pub event }` in `src/kernel/pubsub.rs`. Final shape: `Signal { id, stream, event }` (row id), plus `pubsub::high_water` and `pubsub::missed` (realtime.md#kernel-spec). Converge: add the field (set it in `poll_once`), add the two functions; in `channels::stream` take `Query<StreamQuery>` + `HeaderMap`, subscribe first, replay `missed` with ids (or `refresh`), send `ready` with the cursor, add `.id(sig.id)` to live events, skip only exact replayed ids. No migration (the `broadcasts.id` column already exists).
2. **Pages render a cursor.** For every page with `sse-connect`: read `pubsub::high_water(&db)` before the page's data and render `sse-connect="…?since={cursor}"`. Add the `createEventSource` override and the `bye` listener to `static/app.js` (realtime.md#client-side). Inertia/SPA: pass the cursor as a prop and use it in `?since=`.
3. **The sse extension in the layout.** Detect `htmx_sse` inside a template's `{% block head %}`: move the script tag into `templates/layouts/application.html` (after htmx) and delete the block. Add the "every page loads the live-update extension" test.
4. **Channels: switch client and server together** (the formats are incompatible, and old clients now get `bye` `protocol` "subprotocol offrails.v1 required" + close 1002). Replace `kernel/hub.rs`, `kernel/channels.rs` and `static/channels.js` (or `web/src/lib/socket.ts`) per channels.md; convert each room to typed `In`/`Out` enums, `joined(.., since)` returning `Joined`, `handle_in(.., msg: Self::In, out: &mut Outbox<Self::Out>)` returning `Result<Payload, Rejection>`, `out.emit(seq, …)`; rename events on the page (`ch.channel(topic, { since })`, `ch.on("<out event>")`, `ch.push("<in event>")`, errors are `{code, message}`). `cluster.rs` only changes `String` errors to `Rejection`.
5. **Connection security.** `Member` gains `#[serde(skip)] session: Option<i64>`; add `SessionAndUser`; `session::destroy`/`destroy_all_for_user` write the `member:<id>` / `session-ended` signal; `AppState::new` calls `hub.watch_revocations(pubsub.subscribe())` and `hub.log_frames(is_development)`; `Channels` gains `session_valid`; the SSE stream ends with `bye` on `session_ended`. Config: `APP_REALTIME_REVALIDATE_SECS` (60), `APP_REALTIME_TRANSPORTS` (all), in `Config` and `.env.example`.
6. **Fallback transports.** Add the four routes (`/socket/sse`, `/socket/sse/{conn}`, `/socket/poll`, `/socket/poll/{conn}` GET+POST) with `ROUTES` rows, and the handlers calling `kchannels::sse_open` / `relay_post` / `poll_open` / `poll_get`.
7. **Tests:** port `support::Ws` to offrails.v1, add `wire_test.rs` and the resume/session cases in `kernel_pubsub_test.rs`, and `tests/e2e/realtime.spec.ts` (channels.md#testing).

## 0.3.0 (2026-10-13)

The open-source release. Rust off Rails is public at https://github.com/Rust-Off-Rails/rustoffrails and installs with one line: `curl -fsSL https://rustoffrails.org/install.sh | sh`.

### One-line install and the `offrails` command
- **`install.sh`** (POSIX sh): shows its plan first, `--dry-run`, `--yes`; Rust via rustup (no sudo); sea-orm-cli, cargo-nextest and cargo-watch as prebuilt binaries via cargo-binstall; a SHA-256-verified framework release in `~/.offrails/<version>` with a `current` symlink; installs the Claude Code plugin when `claude` is present; prints the AGENTS.md prompt for Codex and Cursor. Idempotent, no telemetry. See [INSTALL.md](INSTALL.md).
- **`offrails`**: `new <name> "<pitch>"`, `doctor`, `version`, `update`, `uninstall`.
- New apps also get `.cursor/rules/offrails.mdc` and `.github/copilot-instructions.md`, pointing to `AGENTS.md`.

**App migration:** optional. Add the two agent pointer files above.

### Open source
- Dual licensed **MIT OR Apache-2.0**; `SECURITY.md` (GitHub private vulnerability reporting), `CONTRIBUTING.md` (contributions are intent plus evidence), Code of Conduct, issue forms ("Agent got stuck" first), and CI: links, hook tests, plugin validation, gitleaks, installer tests, OpenSSF Scorecard.
- Brand: the `\R/` derailing mark and the hopping "off" wordmark, drawn as code (`brand/`).
- Doctrine §0, **"All code is written by AI"**, is now the founding rule.
- Plugin: `/offrails:security` skill; hooks for security tripwires, banned crates in `Cargo.toml`, append-only committed migrations, raw `.begin()`, and no `pkill`; content-fingerprint drift detection; `/offrails:upgrade` reconciles instead of re-creating.


### Security as the gold standard: Non-negotiables, the header matrix, automated enforcement

Spec: `guides/security.md` (Non-negotiables, the header matrix, Automated enforcement, Gotchas), plus `blueprints/feature-spec.md` (a `## Security` STRIDE section), `workflow.md`, `guides/test-standards.md` §3, `guides/testing.md` CI, `generators/new.md`, `stack.md`, `structure.md`, `guides/routing.md` and `guides/auth.md`. Plugin: the `/offrails:security` skill and security tripwires in the hooks. Verified on the reference app (branch `security`): 207 tests on SQLite and Postgres 18; cargo-deny `advisories/bans/licenses/sources ok`; gitleaks clean over the history; ZAP baseline 0 FAIL, 0 WARN, 61 PASS; checked in Chrome.

- **`kernel/security.rs`**, one middleware outside the panic and timeout layers:
  - the full header matrix, adding `object-src 'none'`, COOP and CORP to what was there;
  - `upgrade-insecure-requests` and HSTS only with an https `APP_URL`;
  - `no-store` on every authenticated response;
  - production cookies forced to `Secure; HttpOnly; SameSite=Lax`.
- **Session cookie:** `__Host-session_token` in production, and production ignores the plain name.
- **CI:**
  - a `security` job (cargo-deny + gitleaks over the full history);
  - the Docker job boots the production image and runs the ZAP baseline;
  - Dependabot.
- **Findings fixed:**
  - the flash cookie had no `Secure` in production;
  - authenticated pages were cacheable;
  - panic 500s and timeout 408s had no security headers.

**App migration** (reconcile, don't re-create):
1. **Header middleware.** Detect: `src/kernel/security.rs`.
   - Missing: generate it per `guides/security.md#the-header-matrix`. In `routes.rs`, replace the per-header `SetResponseHeaderLayer`s and the `CSP` const with one `.layer(from_fn_with_state(security::Hardening::new(&config), security::middleware))` placed **after** (outside) `CatchPanicLayer`.
   - If a route sets its own CSP (the mailbox viewer), keep it. The middleware only fills in missing headers.
2. **Session cookie.** Detect `PROD_COOKIE` in `kernel/session.rs`. Missing: add `name(secure)` and make `read(headers, secure)` accept only `name(secure)`. Update `cookie`/`clear_cookie`, the `load_session` caller and any WebSocket upgrade that reads the cookie. Tests use `ksession::name(config.is_production())`. Deploying logs everyone out once, because the cookie name changes. Say so to the human.
3. **`tests/app/headers_test.rs`.** Add it (one row per response class the app has), plus `TestApp::request_head`. Production rows use an `https://` `APP_URL`. Make it fail first if the middleware is off.
4. **Scanner configs.** Add `deny.toml` (copy the reference in `security.md`; keep any `[bans]` the app already has), with `publish = false` in both crates. Add `.gitleaks.toml`, `.zap/rules.tsv` (+ `.zap/.gitignore`) and `.github/dependabot.yml`. Run each scanner locally. Fix findings, or triage them **with a reason** in the config.
5. **CI.** Add the `security` job and the Docker job's boot + ZAP steps from `guides/testing.md#ci`. Lint with actionlint.
6. **Feature specs.** Add a `## Security` section to every `product/features/*.md` (`blueprints/feature-spec.md`). Each threat gets a control and a test. A threat without a test is a finding.

### API-only apps, the `API` axis (REST + OpenAPI, GraphQL), Docker compose, day-one meta and favicons

Spec: `guides/api.md` and `guides/graphql.md` (new), `tools/brand/render.sh` (new), plus `interview.md`, `blueprints/AGENTS.md`, `generators/new.md`, `generators/scaffold.md`, `guides/views.md`, `guides/deployment.md`, `guides/auth.md`, `guides/controllers.md`, `guides/config.md`, `guides/test-standards.md`, `stack.md`, `structure.md`, `cli.md`, `RAILS_MAP.md`. Verified with the `offrails-api-demo` app (`FRONTEND_MODE: api`, `API: both`): 151 tests green on SQLite and Postgres 18, clippy clean, GraphiQL and the Scalar reference checked in Chrome, `docker compose up --build` from scratch plus the `mail` and `worker` profiles, and direct TLS (h2 via ALPN).

- **`FRONTEND_MODE: api`** (`rails new --api`): no UI, JSON errors everywhere except `/dev` and `/admin`, `GET /` as a JSON index, `/robots.txt` disallowing all.
- **`API: none | rest | graphql | both`**, a separate header line, chosen in the interview. REST lives under `/api/v1`:
  - bearer API tokens (`api_tokens`, SHA-256 digests, scopes, expiry);
  - `ApiJson` (JSON error shape for bad bodies) and `{"data": …}` envelopes;
  - an OpenAPI document via utoipa, committed as `docs/openapi.json` with a drift test, plus Scalar at `/dev/api-docs`.
- GraphQL lives at `POST /graphql`: async-graphql, DataLoaders, depth (≥ 13, or introspection breaks) and complexity limits, payload `UserError`s, `docs/schema.graphql` with a drift test, GraphiQL at `/dev/graphiql`.
- CORS comes from `APP_CORS_ORIGINS`: exact origins, also trusted by CSRF, no credentials. Per-token rate limits.
- **`compose.yaml` runs the whole app**: the production image plus Postgres with healthchecks, and profiles `worker` and `mail`. New `app health` subcommand for container healthchecks.
- **Day-one brand and meta for every mode with pages:**
  - `tools/brand/render.sh` writes `favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, `icon-512.png` and `og.png`;
  - `Ctx` carries the title, description and canonical URL;
  - the layout has Open Graph and Twitter tags;
  - `/robots.txt` and `/sitemap.xml`;
  - smoke tests assert all of it.
- **Web server**: documented as axum on hyper (HTTP/1.1 + h2c); optional direct TLS via `APP_TLS_CERT`/`APP_TLS_KEY` (axum-server + rustls).
- **Fixed:** `serde_html_form` is a kernel dependency (the jobs dashboard's filter links). `new` adds it again, which corrects the earlier doc-fix pass.

**App migration** (reconcile, don't re-create):
1. **Header.** If `AGENTS.md` has no `API:` line, add `API: none` under `ISLANDS:`. An app that already serves a public API (tokens, `/api/v1`) records `rest`, and follows `guides/api.md` for anything it lacks.
2. **compose.yaml.** Detect: `grep -q '^  app:' compose.yaml`.
   - If it has only `postgres`/`db`, replace it with the shape in `guides/deployment.md#docker-compose-the-whole-app-locally`. Keep the existing Postgres major and volume name (renaming the volume loses local data; say so).
   - Add `app health` to `src/main.rs` if missing.
   - Verify with `docker compose up --build`, then `down -v`.
3. **Brand and meta** (modes with pages).
   - Detect: `static/og.png`, `static/apple-touch-icon.png`, and `og:image` in the layout or shell.
   - Missing: run `bash .offrails/tools/brand/render.sh "<APP_NAME>" "<accent from the layout>" "<pitch from product/README.md>" static`, add the `<head>` tags and the `Ctx` meta fields from `guides/views.md#meta-favicon-and-social-cards-every-app-with-pages`, route `/favicon.ico`, `/robots.txt`, `/sitemap.xml`, and add the smoke-test assertions.
   - An existing hand-made favicon or OG image wins. Don't overwrite it; only fill in what's missing.
4. **serde_html_form.** Ensure it's in `Cargo.toml` (`cargo add serde_html_form`). It's harmless if already present.
5. Adding `API: rest | graphql` to an existing app is a feature, never part of an upgrade. Spec it, then follow `guides/api.md` / `guides/graphql.md` and `generators/scaffold.md` step 9b per resource.

### Svelte as a UI library; the `react` mode is renamed `spa`

Spec: `guides/frontend-svelte.md` (new); `stack.md`, `interview.md`, `blueprints/AGENTS.md`, `structure.md`, `generators/new.md` and `generators/scaffold.md` updated. Verified in Chrome:
- Inertia + Svelte 5 on the reference app: every protocol check, 409 redeploy reload, HMR, Playwright, 75 Rust tests on SQLite and Postgres;
- a SvelteKit SPA served by Rust under a strict hashed CSP;
- a Svelte island.

- **The app header gains `UI_LIBRARY: react | svelte | none`.** `FRONTEND_MODE` values are now `htmx | inertia | spa | api` (`api` is specified by the API-only work).
- **`FRONTEND_MODE: react` is now `spa`.** Same architecture; the name no longer implies the library.
- **The DoD's frontend type-check is `pnpm --dir web typecheck`.** React keeps `"typecheck": "tsc -b"`; Svelte uses `svelte-check --fail-on-warnings`.

**App migration** (existing apps; reconcile, don't regenerate):
1. Detect `FRONTEND_MODE:` in `AGENTS.md`.
   - If it says `react`, change it to `spa` and add `UI_LIBRARY: react` under it.
   - If it says `inertia`, or `ISLANDS: yes`, add `UI_LIBRARY: react`.
   - Otherwise add `UI_LIBRARY: none`.
   No code changes. Log the header change in `docs/decisions.md`.
2. If `web/package.json` has no `typecheck` script, add `"typecheck": "tsc -b"`. CI and the DoD now call `pnpm --dir web typecheck`; update `.github/workflows/ci.yml` if it runs `exec tsc -b` directly.
3. Switching an existing app to Svelte is a human decision, never part of an upgrade. If asked, follow `guides/frontend-svelte.md`: the Rust side changes only `VITE_ENTRY` and drops the React refresh preamble route.

### Realtime: channels, presence, the hub, the cluster layer

Spec: `guides/channels.md` (new), plus changes to `guides/realtime.md`. Verified in the reference app (book page: "reading now" avatars and a live discussion), with 21 tests on SQLite and Postgres 18 including a 3-node failover, a Playwright smoke, and a manual server-restart check in Chrome.

- **Signals got fast.** `pubsub::broadcast` now wakes the poller: `NOTIFY` at commit on Postgres (238 ms → 2.1 ms) and an in-process `Notify` on SQLite (→ 0.24 ms). The 250ms poll remains as a backstop. New `PubSub::start_with(poll_every)`.
- **Channels:**
  - `GET /socket`: one WebSocket per tab, Phoenix v2 wire format, topics joined with authorization;
  - `kernel/hub.rs`: a room actor per topic, supervised; serialize-once fan-out; slow consumers dropped; presence by person;
  - `kernel/channels.rs`: Origin check, auth at upgrade, frame and rate limits;
  - `static/channels.js`, and `/dev/sockets`.
- **Cluster (opt-in, Postgres):** `kernel/cluster.rs`, with `cluster_nodes` heartbeats, a `NOTIFY` bus, rendezvous ownership, forwarded calls, presence cleanup and failover. Settings: `APP_CLUSTER`, `APP_NODE`.
- **Fixed:** the reference app's `ROUTES` table had no rows for books, reviews, blobs or streams, so the access matrix never tested them. New test `every_mounted_route_is_in_the_routes_table` parses `src/routes.rs` and fails on any mounted path missing from `ROUTES`.

**App migration** (an agent upgrading an existing app; reconcile, don't re-create):

1. **Every app: route table drift.**
   - *Detect:* any `.route("…")` path in `src/routes.rs` without a `ROUTES` row.
   - *Converge:* add the rows with their real `Access` (check each handler's extractor). Then add the `every_mounted_route_is_in_the_routes_table` test from `guides/test-standards.md` §3 (`tests/app/security_test.rs`), and run the security suite, which may now fail on routes it never covered. Fix those routes, not the test.
2. **Apps with SSE channels: faster signals.**
   - *Detect:* `kernel/pubsub.rs` without `pg_notify` / `WAKE`.
   - *Converge:* add the `NOTIFY` (Postgres) and `WAKE` (SQLite) calls in `broadcast`, `start_with`, and the dedicated listener task, per realtime.md's kernel spec.
   - *Tests:* the wake-up test (`start_with(60s)`) and the rollback test.
   - No schema change.
3. **Apps that need rooms or presence:** only when a spec asks for them. Run `generators/channel.md` → room recipe.
   - *Detect:* an existing hand-rolled `axum::extract::ws` handler.
   - *Converge:* move its topics into `Room` impls behind `/socket`. Keep the old route as long as clients depend on it, and log the deviation.

## 0.2.0 (2026-10-12)

The kernel UIs, verified end to end in a real app (bookshelf, htmx, SQLite + Postgres 18, Chrome): the full Oban-style queue and `/admin/jobs`, a Swoosh-grade dev mailbox, `/dev`, and a pending-migrations page. Specs: `guides/jobs.md`, `guides/jobs-dashboard.md`, `guides/mailers.md`, `guides/dev-tools.md`.

- **Jobs:** producers per queue with semaphores, an optimistic claim, timeouts, panic capture, cancellation of executing jobs (Heartbeat aborts), snooze/cancel/discard, the Stager/Lifeline/Pruner/Cron plugins as plain functions, uniqueness via `on_conflict_do_nothing_on` → `TryInsertResult::Conflicted` (sea-orm 2), and graceful shutdown. `JobBuilder` is non-generic (`JobDefaults` + serialized args). `Recurring { key, cron, job: fn() -> JobBuilder }`; cron jobs are tagged `cron` with `meta.cron = key`.
- **`/admin/jobs`:** overview (counters, throughput SVG, queues, nodes), filtered list with bulk actions and an "all matching" confirmation page, job timeline + error history, cron page, live polling, flashes, `no-store`.
- **Dev mailbox:** two panes (Inbox/Previews), Cc/Bcc/Reply-To, attachments, HTML/Text/Raw, Desktop/Mobile, links, `.eml` at `/dev/mailbox/{id}/message.eml`, new-mail banner, test-email button, JSON for agents. `/dev/mailers` → Previews tab.
- **Mailer selection:** development is always `Mailer::Local` (`MAILER=smtp|local|log` overrides); `Log` is only the production fallback. Selection moved from the kernel into `state::select_mailer`.
- **Email:** `Email` gains `cc`, `bcc`, `reply_to`, `attachments` (all `#[serde(default)]`). `DeliverEmailJob` becomes `{ #[serde(flatten)] email, expires_at }` with `MAX_ATTEMPTS = 10`; `mailers::deliver_later_until` for time-limited mail without secrets. Token emails use dedicated ids-only jobs (no secrets in job args). The one mail layout is `templates/mailers/layout.html`.
- **Back/Forward and hx-boost:** kernel routers are `no-store`, the kernel layout has `historyCacheSize: 0` and `static/kernel/admin.js` (pageshow reload), gone records redirect with a flash, and app links into kernel pages carry `hx-boost="false"`.
- **Pending migrations:** a dev-only page with a Run button, `/up` 503 while pending, the worker waits, and `bin/dev` migrates.
- **Config:** in development `APP_URL` defaults to `http://localhost:{PORT}`. New `MAILER`, `APP_REVISION`.
- **Migration crate:** `sea-orm-migration` without its `cli` feature; no `migration/src/main.rs`.
- **Tooling:** `.offrails/tools/browser-smoke/` (optional; puppeteer-core cached outside the app).
- **SQLite transactions:** every transaction starts with `kernel::db::begin(&db)` (`BEGIN IMMEDIATE` on SQLite). A plain `db.begin()` is deferred and fails with "database is locked" when the job worker writes between your read and your write. Tests missed it because the in-memory test database has one connection; a regression test now uses a real file. The plugin flags `.begin().await` outside the kernel.
- **Migrations are append-only once committed.** Edited applied migrations never reach existing databases. CI fails a change that modifies a committed migration, and the plugin blocks such edits.
- **Errors on boosted pages are visible.** `static/app.js` shows the server's error page for 5xx on boosted requests, and the layout's `responseHandling` swaps 422 and 429.
- **Auth:**
  - magic-link login is a first-class variant (`ACCOUNTS: magic-link | magic-link-invite`), recommended in the interview. It's fully specified in auth.md: POST to consume, single use, 15 minutes, enumeration-safe, rate-limited per IP and per email;
  - the dev "check your mailbox" hint on every auth screen that sends email;
  - every auth email has a mailer preview;
  - failed admin Basic-auth passwords are rate-limited (10 per 3 minutes per IP);
  - `TENANCY: per-user | teams | shared | none`.
- **Live updates (SSE):** events carry non-empty `data` (browsers drop empty ones); one `/streams/{stream}` route; streams end on shutdown; the app layout uses `historyCacheSize: 0` and `refreshOnHistoryMiss: true`.
- **CSS check:** development warns and shows a banner when `static/app.css` is missing or empty; production refuses to boot without it.
- **Errors:** the 422 fallback renders `templates/errors/_invalid.html` (never HTML from `format!`). The router fallback marks its 404 with `x-route-fallback: 1`, so the routability test is exact for parameterized routes.
- **Tests:** `test-standards.md` (AC IDs, archetype suites, the access matrix over `ROUTES` with an `Access` column, tenant isolation, coverage and mutation gates with a per-path `jq` check, query-count guard, real-browser smoke). `TestApp` gains `assert_text`, `without_fetch_metadata`, `post_multipart`, `get_htmx` and SSE helpers. Postgres test databases are `test_<crate>_<hex>`, and `test_db_cleanup` drops only this app's idle ones.
- **New guidance:** dark mode (OS-following by default; a no-flash cookie toggle with a verified Tailwind 4 variant); public pages (meta, OG image, canonical, robots, sitemap, self-hosted fonts, Lighthouse ≥ 95); trusted vs user Markdown (heading ids, ammonia builder); runtime-read directories in the Dockerfile; how an agent runs, checks and stops the app (never `pkill` by name).
- **Plugin:** drift is detected by content fingerprint, not just the version line. Hooks block `pkill`/`killall` by name and edits to committed migrations, and flag `.begin()` outside the kernel and `sleep` in tests. `/offrails:upgrade` reconciles instead of recreating.
- **Docs fixes:** sea-orm 2 pitfalls (`ActiveModel::default()` ambiguity, OFFSET without LIMIT on SQLite, jsonb text search), axum's whole-segment params, the CSP ⇄ htmx-config pairing (verified in Chrome), lettre quoted-printable links.

**App migration (reconcile, don't recreate).** For each item: detect what the app already has, compare with the final shape, then converge. Classify each as *new*, *present-conforming*, *present-divergent* or *conflicting*. Never edit or re-run an applied migration; add a new one. Commit per item, green on both databases.

1. **`mailbox_messages` table.** Detect: the table exists (any `m…_create_mailbox_messages`, applied). Final columns: `id, to_addr, from_addr, cc json [] , bcc json [], reply_to string null, subject, html text, text text, headers json, eml text, attachments json [], read_at timestamptz null, created_at`. Converge: generate `m<ts>_alter_mailbox_messages` that adds only the missing columns (SQLite-safe: one `ALTER TABLE … ADD COLUMN` per column, with defaults; nullable `read_at`). If the app named the raw message something else (e.g. `raw_eml`), keep the column and map the entity field with `#[sea_orm(column_name = "raw_eml")] pub eml: String` rather than renaming. Missing table: *new*, so generate `create_mailbox_messages`.
2. **`Email` and delivery.** Detect: `src/kernel/mailer.rs::Email` fields, `Mailer` variants, whether selection lives in the kernel (`Mailer::from_config`). Converge: add the four `#[serde(default)]` fields and `Attachment`; extend `build()` (Cc/Bcc/Reply-To, multipart/mixed); add constructors `Mailer::smtp/local/memory`; move selection to `state::select_mailer` with the table in mailers.md. **Delete any file-writing/"logged locally" dev mailer**: development must be `Local`. Update the `EMAIL` header line to `dev: local mailbox (/dev/mailbox); prod: …`.
3. **`DeliverEmailJob`.** Detect: tuple struct `DeliverEmailJob(Email)`. Converge: `struct DeliverEmailJob { #[serde(flatten)] email: Email, #[serde(default)] expires_at }` (rows already queued as a bare `Email` still decode; add the decode test), `MAX_ATTEMPTS = 10`, expiry discard; add `mailers::deliver_later_until` for time-limited mail **without secrets**. Any email carrying a token (password reset, login link, confirmation, invitation) must not be enqueued rendered: detect `deliver_later(…)` calls whose email embeds a token, and replace each with a dedicated ids-only job (5 attempts, discard when stale) that mints the token, stores its digest and delivers (guides/auth.md).
4. **Dev mailbox UI.** Detect: `kernel/mailbox.rs` routes (`/dev/mailbox`, `/dev/mailbox/{id}`, any `/{id}.eml` or `/{id}/raw`), templates under `templates/kernel/mailbox/` or elsewhere, a separate `/dev/mailers` page. Final: the route table in mailers.md (`/dev/mailbox`, `/{id}`, `/{id}/message.eml`, `/{id}/html`, `/{id}/attachments/{n}`, `/{id}/delete`, `/clear`, `/test`, `/previews`, `/previews/{*name}`, `.json`, `/latest.json`), `kernel::mailbox::router(PREVIEWS)` with `.layer(ui::no_store())`, the templates and `kernel/ui.rs` chrome. Converge: replace the module and templates with the spec (the table is the only state). Keep any existing `.eml`/raw URL as a redirect to `/message.eml` if anything links to it. Move previews: create `src/mailers/previews.rs` with `PREVIEWS` (port each existing preview fn), make `GET /dev/mailers` a 301 to `/dev/mailbox/previews`, and delete the old previews page and templates.
5. **Links into kernel pages.** Detect: `grep -rn 'href="/dev\|href="/admin' templates/ web/src/` (the footer "Dev tools" link, mailbox hints on `/session/new`, `/login-links/sent`, nav to `/admin/jobs`). Converge: add `hx-boost="false"` (htmx) or use a plain `<a>` (TS modes); use `paths::dev_mailbox()`; add `app_links_into_kernel_pages_are_not_boosted`.
6. **Jobs kernel.** Detect: `src/kernel/jobs/` files, the `jobs` columns (compare with jobs.md, Tables), `job_queues`/`job_nodes`/`recurring_executions`. Converge: missing tables or columns via a new migration; then replace `kernel/jobs/` with `mod.rs`/`executor.rs`/`plugins.rs`/`admin.rs`/`web.rs` per the spec. Adapt `src/jobs/mod.rs` (`into_job` → `JobBuilder::new(JobDefaults{…}, serde_json::to_value(&self))`, `fn unique()`, `worker_config` → `WorkerConfig::default()` fallback), `recurring.rs` to `&[Recurring]`, and `main.rs`'s `WorkerOptions` and `app jobs …` CLI. Keep every existing `Worker::NAME`.
7. **`/admin/jobs`, `/dev`, pending migrations.** Detect: `kernel/jobs/web.rs`, `controllers/dev.rs` routes, `controllers/migrations.rs`. Converge to jobs-dashboard.md and dev-tools.md: `route_layer(AdminAccess)`, `no_store()` on each kernel router, the test buttons, `/dev/system` via `get_*_migrations_read_only`, the `migrations::guard` layer (development only), `/up` 503 while pending, the worker waiting for migrations, `bin/dev` migrating first. Add every new route to `ROUTES` (`Access::Admin` / `Access::Dev`).
8. **Assets and layout.** Add `admin_js` to `Assets`, `static/kernel/admin.js`, `historyCacheSize: 0` in `templates/kernel/layout.html`'s htmx-config, `templates/mailers/layout.html` (move an existing `templates/layouts/mailer.html` there and update `{% extends %}`).
9. **Config.** `APP_URL` defaults to `dev_app_url(PORT)` in development (parse `PORT` first); add `mailer` (`MAILER`) and `revision` (`APP_REVISION`) to `Config`, `.env.example` and the deploy config.
10. **Migration crate.** If `migration/Cargo.toml` uses default features: set `default-features = false` and delete `migration/src/main.rs` (check nothing runs `cargo run -p migration`).
11. **Transactions.** Detect: `grep -rn '\.begin()\.await' src/ | grep -v src/kernel/`. Converge: add `kernel::db::begin` (database.md: `begin_with_options` with `SqliteTransactionMode::Immediate`; Postgres ignores the mode) and replace every call site. Add the file-backed SQLite regression test (a write racing a read-then-write transaction).
12. **Append-only migrations.** Detect: `git log --diff-filter=M -- migration/src/m*.rs` (edits to migrations after their first commit). Any hit is *present-divergent*: restore the committed original and move the change into a new `alter` migration. Then add the CI guard step from testing.md (fails when a committed migration file is modified).
13. **Boosted errors.** Detect: `static/app.js` lacks an `htmx:responseError`/`htmx:beforeSwap` handler for 5xx; the layout's `htmx-config` lacks `responseHandling` for 422/429. Converge to frontend-htmx.md (both pieces), and check in a browser that a forced 500 on a boosted form shows the error page.
14. **Auth (only if `ACCOUNTS` isn't `none`).** Detect the variant actually built (password tables, `login_links`/`login_tokens`). Record it in `ACCOUNTS` (an improvised passwordless flow = `magic-link`). Converge to auth.md's variant: token digest only, single use, the TTL, GET → confirm page → POST consumes, the same response for known and unknown emails, rate limits per IP and per email, and the token minted **inside** its job. Add a mailer preview for every auth email, and the dev mailbox hint (`templates/shared/_dev_mailbox_hint.html`, development + `Mailer::Local` only) with its present-in-dev/absent-in-prod test. Admin Basic auth: add the failed-attempt limiter (security.md).
15. **SSE.** Detect: `Event::default().event(…)` without `.data(…)`, or per-resource stream routes. Converge to realtime.md: non-empty data, one `/streams/{stream}` route, `take_until(pubsub.closed())`, and the app layout's `historyCacheSize: 0` + `refreshOnHistoryMiss: true`.
16. **Errors and routing.** Converge `controllers::errors::not_found` to return the `x-route-fallback` marker, have `shape_errors` keep it, and tighten `every_route_is_routable_in_development` to use it. Render the 422 fallback with `templates/errors/_invalid.html`. Delete any `format!`-built HTML.
17. **Postgres test databases.** Detect: `test_<hex>` names in `tests/app/support.rs`. Converge to `test_<crate>_<hex>`, the joined-thread `Drop`, and the scoped, idle-only `test_db_cleanup` (testing.md#database-per-test).
18. **Env files.** In `.env.example` (and the human's `.env`, if present and if they agree), comment out `APP_URL` for development; keep it required in production.
19. **CSS check.** Converge `kernel/assets.rs` to warn and show a banner only when `static/app.css` is missing or empty (never by timestamp: Tailwind 4 doesn't rewrite unchanged output), and to fail boot in production.
20. **Tests.** Bring `tests/app/kernel_jobs_test.rs` and `kernel_web_test.rs` to the lists in new.md step 7 (both backends), and `support.rs` to the job helpers + `wait_until`. Then the Definition of Done, and, if Node ≥ 22.12 and Chrome are present, `bash .offrails/tools/browser-smoke/run.sh`.

## 0.1.0 (2026-10-12)

First release:

- The framework docs.
- The Claude Code plugin:
  - skills: `new`, `feature`, `scaffold`, `generate`, `check`, `doctor`, `deploy`, `upgrade`;
  - the convention hooks.
- Borrowed from Phoenix/Elixir:
  - Oban-style persistent jobs: a state machine, history, queues with pause/resume, unique jobs, snooze/cancel/discard, the Stager/Lifeline/Pruner/Cron helpers, and `Oban.Testing`-style helpers;
  - the `/admin/jobs` dashboard (Oban Web);
  - the Swoosh-style `/dev/mailbox`;
  - a `/dev` toolbox.

**App migration:** none (baseline).
