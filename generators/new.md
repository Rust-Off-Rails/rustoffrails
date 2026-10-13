# Generator: `new` (`rails new`)

> **TL;DR**
> - Run only after the interview and a confirmed plan; it builds about 60 files and the full kernel (jobs + dashboard, mailer + dev mailbox, cache, `/dev`, security headers) for every app, never stubbed.
> - Vendor a **release** into `.offrails/` with the anchored `rsync` excludes; write `.offrails/VERSION`; add every crate with `cargo add`, one per line.
> - Generate in the listed order so the tree compiles at every step; the first full build takes about 10 minutes.
> - Finish with the smoke tests, the Definition of Done on both databases, a running app (`/up`, `/`), and the one baseline commit.
>
> Read it all, in order, whenever you create an app; skipping steps is how stubs ship.

Creates a complete, deployable, empty app: about 60 files. It's the longest recipe and sets every later convention, so run it top to bottom without skipping. Every command below was executed on 2026-10-12 to generate the `bookshelf` reference app (htmx, SQLite + Postgres), which compiled, passed clippy and passed its tests on both databases. When something here disagrees with a resolved crate's docs, the docs for the version in `Cargo.lock` win. Fix the code, then log it in `docs/decisions.md`.

**One profile, every app.** `new` builds the same kernel for every app: the job queue and its dashboard, the mailer and dev mailbox, cache, `/dev` tools. That includes a content site that sends no email today. This is deliberate, as in Rails, where every app gets Active Job and Action Mailer:

- The second feature of almost every app needs one of them (a contact form, a digest, a cleanup).
- Generating them later means migrations and wiring in an app that's already live.
- A single profile is what makes upgrades, the reference app and these docs line up.

The cost is generation time, not runtime: an idle queue polls once a second, and the kernel's tests come with it. Don't trim it. If the interview says "no email", the mailer still exists and `EMAIL` records `prod: not yet (logged only)`.

## 0. Preconditions

- [ ] [`interview.md`](../interview.md) Rounds 1–2 are done (or the human said "you decide"), and the plan is confirmed.
- [ ] You know: the crate name (snake_case; make it distinctive, since other apps on the machine may be stopped by binary name) and the product name (`config::APP_NAME`, human-readable), `FRONTEND_MODE`, `UI_LIBRARY`, `ISLANDS`, `API`, `PROD_DATABASE`, `DEPLOY_TARGET`, `ACCOUNTS`, `EMAIL`.
- [ ] Tools: `rustc`/`cargo` stable, `sea-orm-cli` (its major must match `sea-orm`'s), `cargo-nextest` and `cargo-watch`. Install any that are missing with `cargo install --locked <name>`. `sqlite3` is used by `app db console`.
- [ ] TS modes (`inertia`/`spa`, or `ISLANDS: yes`): **Node 24 LTS** (22.22 is the hard floor) and pnpm via `corepack enable pnpm` ([stack](../stack.md#blessed-js-packages-inertia-spa-islands)).
- [ ] Docker is optional. It's only needed to test Postgres locally and to check the image builds.

## 1. Create the project

`$FW` is the framework you're reading: the plugin root when run through `/offrails:new`, `~/.offrails/current` after the one-line install, or the path the human gave you. **Vendor a release, never a moving working tree:** if `$FW` is a git checkout, export the tag matching its `VERSION` (`git -C "$FW" archive v$(head -n1 "$FW/VERSION") | tar -x -C <tmp>`, then use `<tmp>` as `$FW`). The excludes are anchored with `/`, because unanchored names also drop nested paths such as `tools/brand/`.

```sh
cargo new <dir> --name <crate_name> && cd <dir>   # also runs git init (edition 2024); --name only if the dir differs
rsync -a --delete --exclude /.git --exclude /.claude-plugin --exclude /skills --exclude /hooks --exclude /evals --exclude /brand --exclude /.in_use "$FW"/ .offrails/
date -u +"vendored %F" >> .offrails/VERSION    # first line stays the framework version; dates are UTC everywhere (VERSION, docs/decisions.md, CHANGELOG)
```

The plugin's `skills/` and `hooks/` stay out of the app. They're loaded from the installed plugin, which works for every app at once. Keep exactly these exclusions: `hooks/fingerprint.sh` compares `.offrails/` with the plugin by content using the same list, so excluding anything else (the framework's `.gitignore`, the README's "Developing the framework" section) would report permanent drift. Those files are harmless in `.offrails/`.

## 2. Agent & product files

| File | Source |
|------|--------|
| `AGENTS.md` | [`blueprints/AGENTS.md`](../blueprints/AGENTS.md), with every `{{…}}` in the header filled in from the interview |
| `CLAUDE.md` | a single line: `@AGENTS.md` |
| `.cursor/rules/offrails.mdc` | Cursor already reads `AGENTS.md`; this pins it in every chat. Frontmatter `alwaysApply: true`, body: `Follow AGENTS.md and .offrails/AGENTS.md. All code is written by AI; humans edit product/.` (`.mdc`, not `.md`: Cursor ignores `.md` files in `.cursor/rules/`) |
| `.github/copilot-instructions.md` | GitHub Copilot: the same body as the Cursor rule (`Follow AGENTS.md and .offrails/AGENTS.md. All code is written by AI; humans edit product/.`), in plain Markdown with no frontmatter |
| `product/README.md` | [`blueprints/product-README.md`](../blueprints/product-README.md), filled in from the interview |
| `product/features/` | one spec per feature from the confirmed plan ([`blueprints/feature-spec.md`](../blueprints/feature-spec.md)). They're built after step 9 |
| `docs/decisions.md` | [`blueprints/decisions.md`](../blueprints/decisions.md) |

## 3. Workspace & dependencies

**Migration crate.**

```sh
sea-orm-cli migrate init                        # creates ./migration with an example migration
```

Then fix what it generated:

1. `migration/Cargo.toml`:
   - **delete the `rust-version` line.** Edition 2024's MSRV-aware resolver would otherwise downgrade dependencies to match it;
   - set `sea-orm-migration` to `default-features = false` with features `["runtime-tokio-rustls", "sqlx-sqlite", "sqlx-postgres"]`. The default `cli` feature pulls clap and sea-orm-cli's runner into the app's tree, and the app binary already has `db migrate|rollback|status`.
2. Delete `migration/src/main.rs` (it needs the `cli` feature; `sea-orm-cli migrate generate` only writes files and doesn't use it), and delete `migration/src/m20220101_000001_create_table.rs` with its `mod` line in `migration/src/lib.rs`. Also delete `migration/README.md` (it documents `cargo run -- generate`, which no longer exists) and the migration crate's `tokio` dependency (only `main.rs` used it).
3. Leave the migrations list as an **empty multi-line** `vec!`, with the opening `vec![` and the closing `]` on separate lines. `sea-orm-cli migrate generate` only registers new migrations in that shape. A one-line `vec![]` makes it silently skip registration, which leaves `mod` lines but no `Box::new(...)`.

```rust
pub use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
        ]
    }
}
```

**Workspace.** Append this to the root `Cargo.toml`, then add the migration crate as a dependency:

```toml
[workspace]
members = [".", "migration"]

[profile.dev.package."*"]
opt-level = 1          # faster tests, still-fast builds
```

```sh
cargo add --path migration
```

**Crates.** Run them one per line as written. Don't loop over a quoted list in zsh: it doesn't word-split `$var`, so `for c in $crates` passes the whole list as one argument (use bash, or one `cargo add` per line). These are the commands verified for `htmx` mode. The rest of [`stack.md`](../stack.md#blessed-crates) arrives later, with the feature that needs it.

```sh
cargo add tokio --features full
cargo add tokio-stream --features sync
cargo add futures
cargo add axum --features multipart
cargo add axum-extra --features cookie,typed-header,form
cargo add tower
cargo add tower-http --features fs,trace,compression-gzip,set-header,timeout,request-id,catch-panic,csrf
cargo add axum-server --features tls-rustls          # direct TLS: HTTP/2 for realtime, in dev and on proxy-less boxes
cargo add rustls --no-default-features --features aws_lc_rs,std
cargo add sea-orm --features sqlx-sqlite,sqlx-postgres,runtime-tokio-rustls,macros,with-chrono,with-json,with-uuid
cargo add askama serde_json thiserror anyhow tracing dotenvy rand base64 sha2 croner serde_html_form
cargo add serde --features derive
cargo add chrono --features serde
cargo add uuid --features v4,serde
cargo add tracing-subscriber --features env-filter,json
cargo add clap --features derive,env
cargo add moka --features future
cargo add lettre --no-default-features --features builder,smtp-transport,pool,hostname,tokio1-rustls-tls
cargo add --dev tempfile http-body-util
cargo add --dev tower --features util
cargo add --dev insta --features json,redactions
# TS modes (inertia / spa / ISLANDS: yes) only:
cargo add ts-rs --features chrono-impl,serde-json-impl
# API: rest | both (guides/api.md), and CORS for any API:
cargo add utoipa --features axum_extras,chrono
cargo add tower-http --features cors
# API: graphql | both (guides/graphql.md):
cargo add async-graphql --features chrono,dataloader
cargo add async-graphql-axum
```

`serde_html_form` is a kernel dependency (the jobs dashboard's filter links), so every app gets it in `new`, whatever the mode.

`cargo add` accepts several crates in one call only when no `--features` flag is given. Mixing them fails with "feature `x` must be qualified".

**Framework migrations.** Generate them, then replace each generated body; it contains `todo!()`:

```sh
sea-orm-cli migrate generate create_jobs                # jobs, job_queues, job_nodes, recurring_executions (guides/jobs.md)
sea-orm-cli migrate generate create_mailbox_messages    # guides/mailers.md
sea-orm-cli migrate generate create_cache_entries       # guides/caching.md (the cache kernel is generated in step 4)
```

The tables other kernels need (`sessions`, `blobs`, `broadcasts`) come with the generator that adds those kernels.

`cargo check` now. The crate graph builds once, which takes a few minutes the first time.

## 4. Rust skeleton

Generate these in order. Each module compiles against the ones before it. The order is: config → kernel → error/models → state → jobs → views → controllers → routes → tasks → main.

1. `src/config.rs`: `Config`, `AppEnv`, `LogFormat`, `parse_queues`, `Config::test()` ([config](../guides/config.md))
2. `src/kernel/mod.rs`, `kernel/db.rs` ([database](../guides/database.md#kerneldbrs-spec))
3. `src/error.rs`: `AppError`, `ResponseFlavor` and the `shape_errors` middleware ([errors-and-logging](../guides/errors-and-logging.md))
4. `src/models/mod.rs`: `Errors`, `Page` and the model aliases ([models](../guides/models.md))
5. The kernel, now. Each item's spec is in the linked guide:
   - `security.rs`: **the header matrix middleware** (CSP, HSTS and `upgrade-insecure-requests` only with an https `APP_URL`, nosniff, Referrer-Policy, Permissions-Policy, COOP, CORP, X-Frame-Options, `no-store` for authenticated responses, production cookie hardening), mounted outside the panic and timeout layers ([security](../guides/security.md#the-header-matrix));
   - `flash.rs` and `rate_limit.rs`;
   - `assets.rs`;
   - `mailer.rs` (constructors only; selection lives in `state.rs`) and `mailbox.rs`: **the full dev mailbox** ([mailers](../guides/mailers.md#the-dev-mailbox-kernelmailboxrs-swooshs-local-mailbox)): storage, the two-pane inbox, Previews tab, message view (headers, HTML/Text/Raw, Desktop/Mobile, attachments, links), `/message.eml`, test-email button, JSON endpoints;
   - `cache.rs`;
   - `ui.rs`: the shared chrome for kernel pages (`Chrome`, `KernelFlash`, `no_store()`, `page()`, time and number helpers);
   - `jobs/` (`mod.rs`, `executor.rs`, `plugins.rs`, `admin.rs`, `web.rs`): **the full Oban-style queue and the `/admin/jobs` dashboard** ([jobs](../guides/jobs.md), [jobs-dashboard](../guides/jobs-dashboard.md));
   - `inertia.rs`, only if `FRONTEND_MODE: inertia`. Generate it together with its pieces in other files: the `Vite` manifest/preamble half of `assets.rs`; `StashedErrors` and the `_errors` cookie in `flash.rs`; `Errors` deriving `Deserialize` + `TS`, and `Flash`/`FlashKind` deriving `TS`; the redirect-back and `errors/Show` branches in `shape_errors`; the `version` layer outside the flash layer ([frontend-inertia](../guides/frontend-inertia.md#the-adapters-rust-surface)).

   CSRF is **not** generated. It's tower-http's `CsrfLayer`, configured in `routes.rs` ([security](../guides/security.md#csrf)).

   Later, on first use: `pubsub` (channel generator), `storage` (first attachment field), `session` (authentication generator; its cookie is `__Host-session_token` in production, [auth](../guides/auth.md#sessions)).

   None of these is a stub or "later": the mailbox, the jobs dashboard and `/dev` are part of every app from day one, and their tests (step 7) prove it.

   The kernel owns `Outcome`, `JobInfo`, `JobDefaults`, `JobBuilder`, `Unique`, `WorkerConfig`, `DispatchFn` and `Recurring`, because it records results without knowing `AppState`. The app re-exports what it needs from `src/jobs/mod.rs`.
6. `src/state.rs`: `AppState { config: Arc<Config>, db, mailer, cache, assets, rate_limit }` (plus `vite: Vite` in TS modes) with `AppState::new(config, db) -> anyhow::Result<Self>`, and `select_mailer(&Config, &db)`: development is **always** `Mailer::Local` unless `MAILER` says otherwise ([mailers](../guides/mailers.md#kernel-spec-srckernelmailerrs)). Tests build state the same way.
7. `src/jobs/mod.rs`: the `Worker` trait, `enqueue`, `dispatch`, `worker_config` and `dispatch_fn`. Then `jobs/deliver_email.rs` (`DeliverEmailJob`), `jobs/cleanup.rs` (`CleanupJob`) and `jobs/recurring.rs`. Also `src/mailers/mod.rs` with `deliver_later` and `deliver_later_until`, `src/mailers/previews.rs` (`PREVIEWS`, empty), and the mail layout `templates/mailers/layout.html` ([mailers](../guides/mailers.md)).
8. `src/views/mod.rs` with `render` and `Ctx`, the layout context struct, **including the page meta** (`title`, `description`, `path`, `app_url`; `page_title()`, `canonical()`, `absolute()`, `titled()`, `described()`: [views](../guides/views.md#meta-favicon-and-social-cards-every-app-with-pages)). Then `views/helpers.rs` and `views/home.rs`. `Ctx` has no `current_user` until the authentication generator adds it.
   `FRONTEND_MODE: api`: `views/` holds JSON bodies instead (`#[derive(Serialize, ToSchema)]`, a `PageInfo` envelope); no `Ctx`, no `render` ([api](../guides/api.md)).
9. `src/controllers/`, each with its spec:
   - `mod.rs`;
   - `application.rs`: the `Ctx` extractor, `HxRequest` and `AdminAccess`;
   - `home.rs`: index (`api` mode: `root.rs` with `GET /` as a JSON index of the API, and `GET /robots.txt` → `Disallow: /`);
   - `api` mode, or any `API`: `application.rs` also gets `ApiUser`/`OptionalApiUser` once the authentication generator adds tokens; `controllers/api/mod.rs` (`ApiJson`, `ApiDoc`, `ErrorBody`, `InvalidBody`, the `/dev/api-docs` page) for `API: rest | both`; `controllers/graphql.rs` and `src/graphql/` for `API: graphql | both` ([api](../guides/api.md), [graphql](../guides/graphql.md));
   - `health.rs`: `/up`;
   - `errors.rs`: the `not_found` fallback;
   - `dev.rs`: the `/dev` index (with the Try-it buttons), routes, system, `POST /dev/jobs/test`, `POST /dev/migrations`, and the `/dev/mailers` → Previews redirect ([dev-tools](../guides/dev-tools.md));
   - `migrations.rs`: `pending()`, the development pending-migrations guard and its Run button ([dev-tools](../guides/dev-tools.md#pending-migrations)). `/up` uses `pending()` too (503 while any is pending).
10. `src/routes.rs` ([routing](../guides/routing.md)). `pub fn router(state) -> anyhow::Result<Router>` contains:
    - `/`, `/up`, and `/robots.txt` + `/sitemap.xml` (UI modes: [views](../guides/views.md#meta-favicon-and-social-cards-every-app-with-pages); `api`: `/robots.txt` only);
    - with `API`: `/api/openapi.json`, `/api/v1/…`, `POST /graphql` (with `Extension(graphql::schema())`), and in development `/dev/api-docs` and `/dev/graphiql`; the CORS layer from `APP_CORS_ORIGINS` and the same origins as CSRF trusted origins ([api](../guides/api.md#cors));
    - `/static` with cache headers;
    - the fallback;
    - the middleware stack;
    - `paths` (including `new_session`, used by `error.rs` before auth exists, and `dev_mailbox`);
    - `ROUTES` with an `Access` level for every route;
    - `/admin/jobs`: `jobs::web::router().route_layer(from_extractor_with_state::<AdminAccess, _>(state.clone()))`, mounted when the app is in development or has an admin access method;
    - `dev::router()` plus `kernel::mailbox::router(crate::mailers::previews::PREVIEWS)`, merged only when `APP_ENV=development`, then the `migrations::guard` layer (development only).

    Each kernel router (jobs web, mailbox, dev) ends with `.layer(kernel::ui::no_store())`.
11. `src/tasks/mod.rs`, then `tasks/seed.rs` (empty and idempotent) and `tasks/test_db_cleanup.rs` (calls `kernel::db::drop_idle_test_databases`, which drops this app's leftover, idle `test_<crate>_<hex>` Postgres databases; see [testing](../guides/testing.md#database-per-test)). `kernel/db.rs` also has `ping(db)` for `/up`, so no SQL lives outside the kernel
12. `src/lib.rs` (`pub mod` lines only), `src/main.rs`: the full CLI from [`cli.md`](../cli.md). `serve` uses `into_make_service_with_connect_info::<SocketAddr>()`, which `ClientIp` needs for rate limiting, and shuts down gracefully on Ctrl-C or SIGTERM. With `APP_TLS_CERT`/`APP_TLS_KEY` unset it serves plain HTTP with `axum::serve` (behind a TLS proxy). **With them set, it serves HTTPS itself, h2 + http/1.1 via ALPN** ([deployment](../guides/deployment.md#direct-tls-optional)). This is part of `new` in every app, because any app with live updates needs HTTP/2 in the browser, in development too ([realtime](../guides/realtime.md#many-tabs-stall-http11-vs-http2)). Copy this branch exactly:

    ```rust
    match config.tls.clone() {
        None => axum::serve(listener, service).with_graceful_shutdown(stop).await?,
        Some(tls) => {
            // sqlx/lettre bring ring, axum-server brings aws-lc-rs: rustls can't pick a provider alone.
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            let cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key).await
                .with_context(|| format!("loading APP_TLS_CERT={} / APP_TLS_KEY={}", tls.cert, tls.key))?;
            let handle = axum_server::Handle::new();
            let graceful = handle.clone();
            tokio::spawn(async move { stop.await; graceful.graceful_shutdown(Some(Duration::from_secs(30))); });
            axum_server::bind_rustls(addr, cfg).handle(handle).serve(service).await?;
        }
    }
    ```

    `service` is `router.into_make_service_with_connect_info::<SocketAddr>()` and `stop` is the shutdown future (signal, then close pubsub and sockets). Include `app health` (the container healthcheck: `GET /up` on 127.0.0.1:$PORT, no curl needed in the image). With TLS on, it only checks that the port accepts a connection, because a plain-text probe can't talk to a TLS listener.

## 5. Frontend by mode

- **`htmx`:**
  - Templates: `templates/layouts/application.html` (with the `htmx-config` meta from [frontend-htmx](../guides/frontend-htmx.md#required-config-swap-422-responses)), `templates/shared/_flash.html`, `_errors.html`, `_nav.html`, `templates/home/index.html`, and `templates/errors/404.html`, `403.html`, `500.html`, plus the `_invalid.html` fragment (the 422 fallback list; HTML is never built with `format!`). The error pages are standalone documents with no layout and no `ctx`.
  - Assets: `assets/app.css` and `static/app.js`.
  - Vendor htmx and the sse extension into `static/vendor/` ([frontend-htmx](../guides/frontend-htmx.md#setup-done-by-generatorsnewmd)).
- **`inertia`:**
  - `pnpm create vite web --template react-ts`, then follow the "Vite wiring" section of [frontend-inertia](../guides/frontend-inertia.md#vite-wiring-verified-with-vite-8-vitejsplugin-react-6-tailwind-4) exactly, including `.cargo/config.toml`, `test-setup.ts` and `app.css` with the progress-bar CSS.
  - Rust: `views/home.rs` (`HomeIndexProps`), `SharedProps` + `type Inertia` in `controllers/application.rs`, and `home::index` rendering `"home/Index"`. There is no `views::Ctx` and no `templates/shared/`.
  - Web: `web/src/inertia.d.ts`, `paths.ts`, `pages/home/Index.tsx`, `pages/errors/Show.tsx`, `components/Layout.tsx` and `components/Flash.tsx` with `Flash.test.tsx`.
  - Templates: the HTML shell `templates/layouts/application.html`, plus the standalone `templates/errors/{404,403,500}.html` for first visits and the `_invalid.html` fragment.
  - Run `cargo test --lib export_bindings` once, so `web/src/types/` exists before `pnpm --dir web typecheck`. Commit `web/src/types/`.
  - `UI_LIBRARY: svelte`: scaffold with `--template svelte-ts` instead, and follow [frontend-svelte#inertia-mode](../guides/frontend-svelte.md#inertia-mode): `main.ts` entry (`VITE_ENTRY = "src/main.ts"`, **no** React refresh preamble or `/dev/vite-preamble.js` route), `.svelte` pages and components, `svelte-check` as `typecheck`, Biome with `html.experimentalFullSupportEnabled`.
- **`spa`:** the same Vite scaffold, set up per [frontend-react](../guides/frontend-react.md) with `api/client.ts`, the router, the QueryClient and the home route.
  - `UI_LIBRARY: svelte`: SvelteKit in SPA mode instead, per [frontend-svelte#spa-mode-sveltekit](../guides/frontend-svelte.md#spa-mode-sveltekit): `sv create`, `adapter-static` with `fallback: "index.html"` and `appDir: "assets"`, `ssr = false`, and `kernel/assets.rs` `SpaShell` serving `web/dist/index.html` with the hashed CSP.
- **`api`:** no app templates, no `assets/`, no Tailwind (`bin/setup-tailwind` and the Dockerfile's Tailwind line are left out), no `web/`. Only the kernel's `templates/kernel/`, `templates/mailers/` and the standalone `templates/errors/` pages, plus `static/kernel/` and the vendored htmx they use. Errors are JSON everywhere except `/dev` and `/admin` ([api](../guides/api.md#responses)).
- **`ISLANDS: yes`:** add `web/` in islands configuration ([frontend-htmx#islands](../guides/frontend-htmx.md#islands); Svelte islands: [frontend-svelte#islands](../guides/frontend-svelte.md#islands)).
- Any TS mode: `.cargo/config.toml` with `TS_RS_EXPORT_DIR` and `TS_RS_LARGE_INT = "number"` ([frontend-inertia](../guides/frontend-inertia.md#vite-wiring-verified-with-vite-8-vitejsplugin-react-6-tailwind-4)).
- **All modes:**
  - `static/vendor/htmx-<ver>.min.js`: the kernel pages use it even in `inertia`/`spa`;
  - `static/kernel/admin.css`, `static/kernel/admin.js` (the `pageshow` reload), `templates/kernel/layout.html` and `templates/kernel/{jobs,mailbox,dev}/`;
  - a dev-only "Dev tools" link to `/dev` in the layout or `Layout` component (`ctx.dev` in htmx), **with `hx-boost="false"`** (htmx mode; a plain `<a href>` in TS modes), like every link into a kernel page ([dev-tools](../guides/dev-tools.md#rules)).
- **Every mode with pages (`htmx`, `inertia`, `spa`): brand assets and social meta from day one.** Run `bash .offrails/tools/brand/render.sh "<APP_NAME>" "<accent>" "<one-line pitch>" static` (needs Chrome at generation time; it writes `favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, `icon-512.png`, `og.png`, and keeps the source at `assets/brand/og.html`). Commit them. The layout (or the HTML shell in TS modes) links them and carries `<title>`, description, canonical, `theme-color`, Open Graph and Twitter tags, per [views](../guides/views.md#meta-favicon-and-social-cards-every-app-with-pages). No Chrome: ship `favicon.svg` only and log the missing PNGs in `docs/decisions.md`.
- Design: follow any references from the interview. Otherwise use a clean, neutral Tailwind design with a top nav, a centered max-width container, and visible focus states.

## 6. Scripts & ops files

| File | Content |
|------|---------|
| `.gitignore` | `/target`, `.env`, `/storage/*`, `!/storage/.keep`, `/static/app.css`, `/bin/tailwindcss`, `/mutants.out*/`, `/lcov.info`, `web/node_modules`, `web/dist` |
| `.dockerignore` | `target/`, `storage/`, `.env`, `.git/`, `bin/tailwindcss`, `web/node_modules/`, `web/dist/` |
| `storage/.keep` | empty, so the directory exists in git |
| `.env.example` | Every `Config` var, with dev defaults ([config](../guides/config.md#envexample)) |
| `bin/setup-tailwind` | htmx mode or islands. Pinned `TAILWIND_VERSION`, downloads and verifies `bin/tailwindcss` ([frontend-htmx](../guides/frontend-htmx.md#setup-done-by-generatorsnewmd)) |
| `bin/setup` | `set -euo pipefail`. In order: <ul><li>check tools (install nextest/watch if missing)</li><li>copy `.env.example` to `.env` if missing</li><li>`mkdir -p storage`</li><li>`bin/setup-tailwind && bin/tailwindcss -i assets/app.css -o static/app.css --minify` (htmx), or `pnpm --dir web install` (TS modes)</li><li>print `Compiling (the first build takes several minutes)…`: a cold build of this stack takes about 10 minutes</li><li>`cargo run -- db setup`</li></ul> |
| `bin/dev` | **HTTPS + HTTP/2 by default:** unless `APP_TLS=off`, run `bin/dev-cert` if `storage/dev-certs/cert.pem` is missing and export `APP_TLS_CERT`/`APP_TLS_KEY`, and print `https://…`. Then: first `cargo run -q -- db migrate` (prints "Compiling and migrating…"), then the server via `cargo watch -q -w src -w templates -w migration -x 'run -q -- db migrate' -x 'run -- serve'` (migrates on every rebuild), plus `bin/tailwindcss … --watch=always` or Vite, as background jobs with `trap 'kill 0' EXIT`. Prints `→ http://localhost:${PORT:-3000}` and `…/dev (mailbox, jobs, routes)` |
| `bin/dev-cert` | A local certificate for HTTPS in development: `mkcert` when installed (a locally trusted CA), else `openssl req -x509` self-signed with SAN `localhost`, `127.0.0.1`, `::1`, written to `storage/dev-certs/` (git-ignored with `storage/`). Copy it from the [realtime](../guides/realtime.md#many-tabs-stall-http11-vs-http2) guide |
| `bin/docker-entrypoint` | `#!/bin/sh`, `set -e`, `app db migrate` unless `SKIP_MIGRATIONS=true`, then `exec "$@"` ([deployment](../guides/deployment.md)) |
| `compose.yaml` | **The whole app, production-shaped**: `docker compose up --build` runs the `app` service (built from the `Dockerfile`, `APP_ENV=production`, healthcheck `app health`) against a `db` service (Postgres, the production major, healthcheck `pg_isready`, a named volume, published on `127.0.0.1:${DB_PORT:-5432}` so `TEST_DATABASE_URL` works from the host). Profiles: `worker` (a separate `app worker` container; run with `APP_INLINE_WORKER=false`) and `mail` (Mailpit; `SMTP_URL=smtp://mailpit:1025`). Ports overridable (`APP_PORT`, `DB_PORT`, `MAILPIT_PORT`). The full file is in [deployment](../guides/deployment.md#docker-compose-the-whole-app-locally). `bin/dev` stays the fast native loop |
| `Dockerfile` | Per [deployment](../guides/deployment.md#dockerfile-shape) (trixie images). Delete the `web` stage in plain htmx and `api` modes, and the Tailwind line in `api` mode |
| Deploy config | Only for the chosen `DEPLOY_TARGET`. Skip it if "decide later" |
| `.github/workflows/ci.yml` | Per [testing](../guides/testing.md#ci): SQLite, Postgres, frontend (TS modes), **security** (cargo-deny + gitleaks over the full history) and Docker jobs. The Docker job boots the production image and runs the **ZAP baseline** against it ([security](../guides/security.md#automated-enforcement)) |
| `deny.toml` | cargo-deny: advisories (`yanked = "deny"`, `unmaintained = "all"`), permissive licenses, `[licenses.private] ignore = true`, the [stack](../stack.md) "Banned" list as `[bans] deny` (plus `openssl`, `openssl-sys`, `native-tls`), crates.io-only sources. Set `publish = false` in `Cargo.toml` and `migration/Cargo.toml`. The full file is in [security](../guides/security.md#automated-enforcement) |
| `.gitleaks.toml` | `[extend] useDefault = true` and an allowlist of build/runtime paths only (`target/`, `storage/`, `node_modules/`, `web/dist/`, `static/vendor/`) |
| `.zap/rules.tsv` | The triaged ZAP baseline rules (htmx `eval` 10110, anti-CSRF tokens 10202, COEP 90004 → `IGNORE`; 10049/10111/10112 → `INFO`), each with its reason. Plus `.zap/.gitignore` for scan output (`/zap-report.*`, `/zap.yaml`, `/gen.conf`) |
| `.github/dependabot.yml` | Weekly, grouped updates for `cargo` and `github-actions` (plus `npm` with `directory: /web` in TS modes) |

`chmod +x bin/*`.

## 7. Tests

The layout is one binary, `tests/app/` ([testing](../guides/testing.md)):

| File | Content |
|------|---------|
| `main.rs` | `mod errors_test; mod factories; mod headers_test; mod kernel_jobs_test; mod kernel_test; mod kernel_web_test; mod security_test; mod smoke_test;` plus `#[allow(dead_code)] mod support;`. Not every test file uses every helper, and clippy `-D warnings` would fail on that |
| `support.rs` | `TestApp` and `TestResponse`, including the query counter, per-test Postgres databases, the job helpers (`drain_jobs`/`drain_queue`/`drain_all_jobs` → `Drained`, `enqueued`, `assert_enqueued`, `refute_enqueued`, `perform_job`) and `wait_until` ([jobs](../guides/jobs.md#testing-obantesting-equivalents)) |
| `factories.rs` | Empty for now. Each model adds one |
| `smoke_test.rs` | See the list below |
| `security_test.rs` | The access matrix over `ROUTES` ([test-standards §3](../guides/test-standards.md#3-security-tests-every-app)) |
| `headers_test.rs` | **The header matrix** ([security](../guides/security.md#the-header-matrix)): one request per response class in development and production (https `APP_URL`), each asserting the full header set; production over plain http sends no TLS-only directives. `TestApp::request_head` reads status and headers without the body, for SSE. Grows with the app: authenticated page, SSE and WebSocket rows arrive with auth and channels |
| `kernel_test.rs` | Cache fetch/hit, delete, `delete_prefix` (a `_` in a key is literal), expiry with `Duration::ZERO`; the rate limiter's 11th hit → `TooManyRequests`; `Flash::notice` sets the cookie attributes |
| `errors_test.rs` | Every `AppError` variant × flavor, through `error::shape_errors` on a small test router: HTML pages never leak internals; dev 500 shows the chain; `Unauthorized` → 303 or `HX-Redirect`; `/api` JSON shapes (insta); `ClientIp` honors `X-Forwarded-For` only with `APP_TRUSTED_PROXY`; tasks run by name. Never run `test_db_cleanup` from a Postgres test |

`smoke_test.rs` covers:

- the home page returns 200 and renders the layout;
- `/up` returns 200;
- an unknown path returns the 404 page, and an unknown `/api/...` path returns `{"error":"not_found"}` (an insta inline snapshot);
- security headers are present (CSP, nosniff, frame options, referrer policy, `x-request-id`). The full matrix is `headers_test.rs`;
- the SQLite connection has `foreign_keys = 1`;
- an email sent with `deliver_later` is delivered only after `drain_jobs()` (memory mailer);
- with development config (`Mailer::Local`), an email sent with `deliver_later` and drained lands in `mailbox_messages`, **`GET /dev/mailbox` shows it, and `GET /dev/mailbox/{id}/html` renders its HTML** with the iframe CSP;
- a flash cookie is rendered once, then cleared. A fresh app has nothing that sets a flash yet, so the test sets the `_flash` cookie itself: base64url (no padding) of `{"kind":"notice","message":"Saved"}`;
- the home page's query count is stable (`assert_query_count_stable`);
- **meta and brand assets** (modes with pages): the home page has `<title>`, `meta name="description"`, `link rel="canonical"` (absolute, from `APP_URL`), `og:title`/`og:description`/`og:image` (absolute URL), `twitter:card`, `link rel="icon"` and `apple-touch-icon`; `/favicon.ico`, `/static/og.png`, `/robots.txt` and `/sitemap.xml` return 200.

`api` mode replaces the page checks: `GET /` is the JSON index, `/robots.txt` disallows everything, any unknown path is `{"error":"not_found"}`, there is no flash test, and `errors_test.rs` checks HTML error pages only under `/dev` (everything else is JSON). Its API suites come from [api](../guides/api.md#testing) and [graphql](../guides/graphql.md#required-tests-graphql-archetype).

`security_test.rs` covers:

- every `ROUTES` entry is routable in development, parameterized paths included (`{id}` → `1`). A handler's own 404 (no such record) is fine; the router fallback marks its response with `x-route-fallback: 1` ([errors-and-logging](../guides/errors-and-logging.md)), so "not routed" is exact;
- non-public routes never return 2xx anonymously (production config with an admin password);
- `Dev` routes return 404 with production config;
- `/admin/jobs` returns 404 in production with no access method, accepts the admin password, and rejects a wrong one;
- a cross-site POST and a foreign-`Origin` POST both get 403.

Also, generated with the kernel and never skipped:

- `tests/app/kernel_jobs_test.rs`: the full list in [jobs](../guides/jobs.md#testing-obantesting-equivalents) (claims race, outcomes, retries, uniqueness, `insert_all`, Lifeline, Pruner, Cron once across nodes, the runtime: run + shutdown, cancel-while-executing aborts, pause/resume, shutdown leaves work to Lifeline; the admin API).
- `tests/app/kernel_web_test.rs`: the dashboard pages, fragments, actions, bulk confirmation, flashes and gone-job redirect ([jobs-dashboard](../guides/jobs-dashboard.md#mounting-and-access)); the mailbox (tabs, iframe CSP, `/message.eml`, attachments, Cc/Bcc, new-mail banner, previews, JSON, mailer selection, `deliver_later_until`) ([mailers](../guides/mailers.md#testing)); `/dev` pages; `no-store` everywhere; app links into kernel pages carry `hx-boost="false"`; the pending-migrations page and `/up` 503 ([dev-tools](../guides/dev-tools.md#pending-migrations)); and the axum fact that `/{id}.eml` panics.

## 8. Accounts

If `ACCOUNTS` isn't "none", run [`authentication.md`](authentication.md) now.

## 9. Verify & report

1. `bin/setup` (creates `.env`, builds the CSS, migrates and seeds the dev DB).
2. The [Definition of Done](../workflow.md#definition-of-done), on SQLite **and** Postgres. Without a local Postgres, run `docker run -d --name pg -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:<major>`, then `TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run`.
3. Prove the migrations are reversible on both databases: `cargo run -- db rollback --steps <n>` then `cargo run -- db migrate`. For Postgres, set `DATABASE_URL` to a scratch database.
4. Run the app: `cargo run -- serve`, then:
   - `curl -i localhost:3000/up` returns 200;
   - `/` shows digested `/static/...?v=` URLs, and fetching one returns `cache-control: …immutable`;
   - `/dev` returns 200, and `curl -X POST -H 'Sec-Fetch-Site: same-origin' localhost:3000/dev/mailbox/test` then `curl -s localhost:3000/dev/mailbox/latest.json` shows the test email;
   - `curl -X POST -H 'Origin: https://evil.example' localhost:3000/` returns 403.

   In a TS mode, `bin/dev` runs Vite next to the server. Open `http://localhost:3000` (not :5173) and check that the browser console is clean, which means no CORS or CSP errors. Then `pnpm --dir web build` and `APP_ENV=production … serve`: the shell links `/assets/…` from the manifest, and `/assets` responses are `immutable`.
5. Optional, when Node ≥ 22.12 and Chrome are installed: `BASE=http://localhost:3000 bash .offrails/tools/browser-smoke/run.sh` (styled navigation into the kernel pages, Back/Forward, no CSP violations; [dev-tools](../guides/dev-tools.md#browser-smoke)).
6. If Docker is available: `docker compose up --build` from a clean checkout brings `app` and `db` to `healthy` (migrations run on boot), `curl localhost:3000/up` answers `ok`, then `docker compose down -v`. Port 3000 or 5432 taken: `APP_PORT=… DB_PORT=… docker compose up --build`.
7. Log the generation in `docs/decisions.md`: `YYYY-MM-DD: app generated (mode=…, prod db=…, deploy=…)`.
8. `git add -A && git commit -m "new: <app_name> skeleton"`. This is the one commit you make unprompted, because it's the baseline.
9. Tell the human, in product terms:
   - what exists;
   - how to run it (`bin/setup && bin/dev`);
   - that http://localhost:3000/dev has the mailbox and jobs dashboard;
   - what you'll build next from the plan.
