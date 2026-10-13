# Dev Tools (`/dev`, a small Phoenix LiveDashboard)

> **TL;DR**
> - Every app gets `/dev` (index, routes, system, mailbox, mailer previews) and `/admin/jobs`, built by `new`, never stubbed; mounted only in development (`/admin/jobs` needs admin access in production).
> - Kernel pages have their own layout: every link into them carries `hx-boost="false"`, every response is `Cache-Control: no-store`, and they use no `confirm()` dialogs.
> - Pending migrations: in development every HTML request shows a page with a Run migrations button; `/up` is 503 in production while any are pending.
> - Agents read email via `/dev/mailbox/latest.json?to=…`; the kernel-page browser smoke is `.offrails/tools/browser-smoke/run.sh`.
>
> Read the rest when changing a kernel page, debugging pending migrations, or running the browser smoke.

In development the app serves a toolbox at `/dev`. It's for the human clicking around and for the agent curling around. Keep it modest: it shows what the app already knows, with no metrics pipeline and no profiler.

**`generators/new.md` builds every page below in every app.** None is optional, stubbed or "added later".

| Route | What | Spec |
|-------|------|------|
| `GET /dev` | Index: tiles with live numbers. Mailbox (unread / total), Jobs (executing / available / retryable / discarded), Mailer previews (count), Routes (count), System (pending migrations). **Try it** buttons (Send a test email, Enqueue a test job). The agent curl cheat sheet below | here |
| `POST /dev/jobs/test` | Enqueue a harmless `CleanupJob`; redirect to `/admin/jobs/list` with "Enqueued test job N." | here |
| `POST /dev/migrations` | The pending-migrations page's **Run migrations** button | [below](#pending-migrations) |
| `/dev/mailbox` (+ Previews tab at `/dev/mailbox/previews`) | Every email the app sent, rendered; mailer previews in the same viewer; JSON for agents | [mailers](mailers.md#the-dev-mailbox-kernelmailboxrs-swooshs-local-mailbox) |
| `/admin/jobs` | The jobs dashboard (also in production, behind admin access) | [jobs-dashboard](jobs-dashboard.md) |
| `GET /dev/routes?q=` | The `ROUTES` table: method, path, handler, access badge. One GET search box over all three columns | [routing](routing.md) |
| `GET /dev/system` | `APP_ENV`, database backend + redacted host/file, `FRONTEND_MODE`, queues, inline worker, `SMTP_URL` configured or not, crate version + git sha (`APP_REVISION`), uptime; migrations applied and **pending** (`Migrator::get_applied_migrations` / `get_pending_migrations_read_only`, sea-orm-migration 2.0: the read-only variant never creates `seaql_migrations`) | here |
| `GET /dev/mailers` | 301 → `/dev/mailbox/previews` (previews live in the mailbox now). Build it as `(StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "/dev/mailbox/previews")])`: axum's `Redirect::permanent` sends **308** | [mailers](mailers.md#previews) |

## Files

```
src/controllers/dev.rs          router(): /dev, /dev/routes, /dev/system, /dev/sockets (with channels), /dev/mailers (redirect), POST /dev/jobs/test, POST /dev/migrations
src/controllers/migrations.rs   pending(), the development guard middleware, the Run button handler
src/kernel/mailbox.rs           router(PREVIEWS): /dev/mailbox/…
src/kernel/ui.rs                shared chrome (with the jobs dashboard), BOOTED_AT for uptime
templates/kernel/dev/           index.html, routes.html, system.html, pending.html, sockets.html + _sockets.html (polled every 2s)
templates/kernel/mailbox/       index.html, _list.html, _message.html
```

`controllers/dev.rs` holds a `const FRONTEND_MODE: &str` matching the app's `AGENTS.md` header. `Config` gains `revision: Option<String>` (`APP_REVISION`).

## Rules

- **Mounted only when `config.env == AppEnv::Development`**, in `routes.rs`:

  ```rust
  if config.is_development() {
      app = app
          .merge(dev::router())
          .merge(mailbox::router(crate::mailers::previews::PREVIEWS));
  }
  ```

  In production the routes **don't exist** (404, not hidden). Every `/dev` route is in `ROUTES` with `Access::Dev`, and `security_test.rs` requests each one with a production `Config` and expects 404.
- Server-rendered Askama + vendored htmx + `static/kernel/admin.css` + `static/kernel/admin.js` in every `FRONTEND_MODE`, sharing the jobs dashboard's header (tabs: Overview · Jobs · Cron · Dev · Mailbox · Sockets · Routes) and its flash banner.
- **Every link from an app page into a kernel page carries `hx-boost="false"`**: the "Dev tools" link, the auth screens' "Check your mailbox" hint (`paths::dev_mailbox()`), any nav item into `/admin/jobs`. The htmx-mode app layout has `<body hx-boost="true">`, and a boosted navigation swaps only `<body>`, so a kernel page (its own `<head>` and stylesheet) renders **unstyled** until a hard refresh. Verified in Chrome. `kernel_web_test.rs::app_links_into_kernel_pages_are_not_boosted` scans an app page and fails on any boosted `/dev` or `/admin` link. (Inertia/react apps: use a plain `<a href>`, never `<Link>`/router navigation, for kernel pages.)
- **Never cached, never stale on Back:** every `/dev`, `/dev/mailbox` and `/admin/jobs` router ends with `.layer(kernel::ui::no_store())`, so all their responses (pages, fragments, JSON, downloads, redirects) are `Cache-Control: no-store`. The kernel layout sets `historyCacheSize: 0`, and `admin.js` reloads pages restored from the back/forward cache. URLs of things that no longer exist redirect with a flash instead of a bare 404 (deleted message → inbox, deleted job → job list).
- Never display secrets: the database shows backend and host (or file path) only, never credentials (`redact_db`, unit-tested), and `SMTP_URL` shows "configured" or "not set (dev mailbox)".
- In development the app layout's footer (htmx) or `Layout` component (inertia/spa) shows a small "Dev tools" link to `/dev`. Render it only when the server says `env == development` (`Ctx.dev` / a shared prop).
- `bin/dev` migrates first, then prints `→ http://localhost:${PORT}/dev (mailbox, jobs, routes)`.

## No `confirm()` dialogs in kernel pages

Kernel pages aren't boosted, so `hx-confirm` never fires there, and a native `confirm()` dialog would block the browser smoke tool. Destructive kernel actions are either cheap and dev-only (mailbox **Clear all**, deleting a job: no prompt), or go through a **server-rendered confirmation page** ("Retry 1,234 jobs?", which posts back with `confirmed=1`). If an app page needs a quick confirm, use `data-confirm="…"` on the form, handled in `static/app.js` (`submit` listener calling `confirm()`), and make browser tests accept dialogs (`page.on('dialog', d => d.accept())`).

## Pending migrations

Like Rails' pending-migration error page and Phoenix's "There are pending migrations" screen: a database behind the code shows a clear page, not a "no such table" 500.

`src/controllers/migrations.rs` (generated by `new`):

```rust
/// Read-only: never creates seaql_migrations (a fresh database reports every migration pending).
pub async fn pending(db: &DatabaseConnection) -> Result<Vec<String>, DbErr> {
    Ok(Migrator::get_pending_migrations_read_only(db).await?.iter().map(|m| m.name().to_owned()).collect())
}
pub async fn guard(State(s): State<AppState>, OriginalUri(uri): OriginalUri, req: Request, next: Next) -> Response;
pub async fn run(State(s): State<AppState>, Form(f): Form<RunForm>) -> Result<Response, AppError>;   // POST /dev/migrations
```

| Environment | Behavior |
|-------------|----------|
| development | `routes.rs` adds `app.layer(from_fn_with_state(state.clone(), migrations::guard))` **after** the routes and the `.fallback(...)` are added (a layer only wraps what exists when it's applied, so a fallback added later would skip the guard and unknown paths would 404 instead of showing the page). Every request except `/static/*`, `/up` and `POST /dev/migrations` gets a **503 "N pending migrations" page** (kernel layout, so it works before any app table exists) listing them, with a **Run migrations** button: a CSRF-protected form posting `return_to=<current path>` to `/dev/migrations`, which runs `Migrator::up`, flashes "Ran N migrations." and redirects back (only to same-site paths) |
| production | **Never auto-migrates**: the entrypoint runs `app db migrate` before `serve` ([deployment](deployment.md)). The guard and `/dev/migrations` aren't mounted. `GET /up` returns **503 "pending migrations"** while any is pending, so a deploy that skipped migrating never goes healthy |
| worker (`app worker`, inline worker) | Before starting, checks `pending()`. If any are pending it logs **once** `pending migrations: run \`app db migrate\`; worker waiting`, rechecks every 5s (stopping cleanly on shutdown), then logs `migrations applied; worker starting`. It never spams errors against missing tables |

**Nothing in `serve` may read an app table before the listener is up.** The guard renders from the kernel layout and `seaql_migrations` alone; everything else that touches app tables at boot (realtime's pub/sub and cluster, the inline worker, a cache warm-up) waits for `pending()` to be empty ([realtime](realtime.md#kernel-spec-srckernelpubsubrs)). **Fresh-database smoke**, after any change to `serve` or to what starts at boot (`bin/dev` migrates first, so it never shows this):

```sh
rm -f storage/fresh.sqlite3 && PORT=4321 DATABASE_URL='sqlite://storage/fresh.sqlite3?mode=rwc' cargo run -q -- serve & sleep 5
curl -s -o /dev/null -w '%{http_code}\n' http://localhost:4321/          # 503: the pending-migrations page, not a crash
curl -s -o /dev/null -w '%{http_code}\n' -X POST -H 'Sec-Fetch-Site: same-origin' -d return_to=/ http://localhost:4321/dev/migrations   # 303
sleep 6; curl -s -o /dev/null -w '%{http_code}\n' http://localhost:4321/up   # 200; the log says "migrations applied; realtime starting"
lsof -ti tcp:4321 -sTCP:LISTEN | xargs kill; rm -f storage/fresh.sqlite3*
```

`bin/dev` runs `cargo run -- db migrate` before serving, and again on every rebuild (`cargo watch -x 'run -q -- db migrate' -x 'run -- serve'`), so the page is mostly seen after a `git pull` with the server stopped, or after a rollback. Tests (`kernel_web_test.rs`): a fully rolled-back dev DB shows the page (with the migration names, the button and `return_to`), `/up` is 503, the kernel stylesheet still loads, the button applies all and redirects back with the flash, no open redirect; production with one pending: `/up` 503, `/` unaffected, `/dev/migrations` 404.

## Browser smoke

HTTP tests can't see boosted navigation, `<head>` swaps, the back/forward cache or CSP violations. After changing kernel pages (or the app layout's links into them), drive a real browser:

1. **Preferred: the chrome-devtools MCP** (navigate, click, screenshot, `list_console_messages`), in an isolated context.
2. **When it can't attach** (Chrome's profile lock, CI, a script): the framework ships a ready smoke that launches the installed Chrome headless with a throwaway profile:

   ```sh
   BASE=http://localhost:3000 bash .offrails/tools/browser-smoke/run.sh
   # Linux: CHROME=/usr/bin/google-chrome BASE=… bash .offrails/tools/browser-smoke/run.sh
   ```

   It goes app page → "Dev tools" link (styled?) → seeds 3 mails with the **Send a test email** button → opens one (styled, sandboxed iframe) → **Clear all** → Back (no resurrected rows) → Forward → **Enqueue a test job** → opens and deletes it → Back (redirected to the list, flash shown), and fails on any CSP violation. Every step waits with `waitForNavigation`/`waitForSelector`, with no fixed sleeps, and seeds only through the dev-only buttons, so it needs no app routes.

3. **Multi-tab live updates** (any app with live pages): open N tabs in one profile on a live page, change something once, and assert every tab updates. It catches the HTTP/1.1 connection pool and an undersized per-user cap ([realtime](realtime.md#many-tabs-stall-http11-vs-http2)). The app supplies a small scenario module (`path`, `login`, `mutate`, `seen`); the reference app's is `tests/e2e/live-tabs.scenario.mjs`:

   ```sh
   SCENARIO=tests/e2e/live-tabs.scenario.mjs BASE=https://localhost:3000 TABS=12 \
     bash .offrails/tools/browser-smoke/run.sh live-tabs
   ```

   It exits 1 if any tab misses the update, and says whether the page was on HTTP/1.1. Run it with 12 and 40 tabs after any realtime change.

**Why it lives in `.offrails/tools/` and not in the app:** htmx-mode apps have no Node toolchain or `package.json`, and this check isn't part of the Definition of Done or CI. So `run.sh` installs `puppeteer-core` (pinned, verified 2026-10-12: 25.13.0, which needs **Node ≥ 22.12**; Node 24 LTS recommended) once into `~/.cache/offrails/browser-smoke` and runs a copy of the script from there. The app's dependency tree, lockfiles and stack rules are untouched, and inertia/spa apps use the same script. It's optional tooling for agents and humans; the guaranteed checks remain the Rust tests above. Verified against the bookshelf app: 11/11 checks pass. Removing `hx-boost="false"` from the dev link, or omitting `includeIndicatorStyles:false`, makes it fail.

## For agents

These endpoints are your eyes on the running app. Use them in the "ran the app and exercised it" step of the [Definition of Done](../workflow.md#definition-of-done):

```sh
curl -s localhost:3000/dev/mailbox/latest.json?to=ada@example.com   # did the email go out? follow .links[0]
cargo run -- jobs list --state discarded --json                      # did a job fail?
cargo run -- jobs stats                                              # is anything stuck?
curl -s localhost:3000/dev/system | grep -i pending                  # forgot to migrate?
```
