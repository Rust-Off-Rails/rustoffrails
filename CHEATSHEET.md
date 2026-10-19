# Cheatsheet

The rules agents need most, on two pages. Read this first; open a full guide only when your task touches its topic ([AGENTS.md](AGENTS.md#load-order) maps topics to guides). When this page and a guide disagree, the guide wins.

## Start from the template ([template](template/README.md))

- **New app:** after the interview, `cargo run -q --manifest-path "$FW/tools/offrails-gen/Cargo.toml" --target-dir <dir>/target/offrails-gen -- new <dir> <crate> "<Name>" --parts <a,b>` ([new](generators/new.md)). Never retype the skeleton.
- **Parts:** propose the ones the spec implies (dev-dashboard always; mailer+dev-mailbox for email; jobs+jobs-dashboard for slow/scheduled work; auth-core+method for sign-in; realtime for pushed updates), link <https://github.com/rust-off-rails/samples>, the human confirms. Later: `cargo offrails parts`, `cargo offrails add <part>`, `cargo offrails update-part <part> --framework <newer>` (3-way merge). Check for a part before hand-writing kernel code.
- **Features: start from a snippet or a generator, hand-write the rest.** Both are tested, both are yours once copied, neither is mandatory. Snippets: `cargo offrails snippets`, `snippet show <name>` (read or copy), `snippet add <name> [--as <model>]` / `snippet remove <name>` ([snippets](template/snippets/README.md)). Generators: `cargo offrails g resource posts title:string body:text published_at:datetime:optional` (also `migration`, `model`, `controller` (path params: `show:/blog/{slug}`), `page`, `api`, `job`, `mailer`, `channel`, `test`; `cargo offrails g --help`), `--dry-run` shows the plan, `cargo offrails destroy <kind> <name>` undoes it ([generators](generators/README.md)). **Commit after each run**, then edit the `// APP:` lines and add the spec's tests.
- **Pages from the component kit:** `{% import "components/ui.html" as ui %}` then `ui::page_header`, `ui::table`, `ui::field`, `ui::form_actions`, `ui::confirm_button`… (every one at `/dev/components`; signatures: [views](guides/views.md#components)), in a layout (`layouts/application.html` top nav, `sidebar`, `marketing`, `auth`, `docs`; generators take `--layout`). Generated and snippet pages use them already. Colors, fonts, logo: `brand.toml` + `cargo offrails brand`.
- **Small files, marked lines.** A feature lives in its own folders (`src/controllers/posts/`, `src/models/post/`, `src/routes/posts.rs`, `templates/posts/`, `tests/app/posts/`); a shared file gets one line per spot, marked `// posts`. Write hand-written features the same way.
- The copied code is the app's: adapt `// APP:` lines, keep `// SECURITY:`/`// INVARIANT:` lines and their tests.

## Stack, one crate per job ([stack.md](stack.md))

- **Web:** axum 0.8 on hyper (`/{id}`, never `/:id`) · tower-http (CsrfLayer, trace, compression, timeout, catch-panic) · tokio.
- **Data:** SeaORM 2 (SQLite dev/test + Postgres prod, same code) · `sea-orm-migration` in `migration/` · `i64` ids · chrono `DateTimeUtc`.
- **Views:** Askama (htmx mode, mailers, kernel pages) · Tailwind standalone CLI · htmx 2 + sse ext. JS UIs: Vite + pnpm + React or Svelte, ts-rs types (`TS_RS_LARGE_INT=number`).
- **Other:** lettre (`default-features = false`) · moka · croner · argon2 · object_store · pulldown-cmark + ammonia · utoipa / async-graphql (API axis).
- **Banned:** Diesel, raw sqlx, Actix/Rocket/Poem/Loco/Leptos/Dioxus/Yew, Tera/Handlebars/minijinja, `askama_axum`, `lazy_static`/`once_cell`/`async-trait`/`log` as direct deps, Redis/Kafka/RabbitMQ, JWT/localStorage tokens, npm/yarn/Jest/ESLint, axios, Redux, CSS-in-JS, SSR JS servers, Node in htmx mode (except islands). `cargo deny` enforces it.
- **Versions:** `cargo add` / `pnpm add`, never from memory; check `Cargo.lock` + docs.rs for the resolved version.

## Layout ([structure.md](structure.md))

```
AGENTS.md  CLAUDE.md  .offrails/ (read-only)  product/ (human specs)  docs/decisions.md
src/{main,lib,config,state,routes,error}.rs   src/{models,controllers,views,routes,jobs,mailers,channels,policies,tasks,kernel}/
migration/   templates/   static/   web/ (TS modes)   tests/app/ (ONE test binary)   bin/{setup,dev,docker-entrypoint}
```

**Naming:** tables plural snake (`blog_posts`) · model module singular · `<Model>Params` (untrusted) → `validate()` → `New<Model>` · controllers plural, 7 Rails actions · routes plural kebab (`/blog-posts/{id}`) · jobs `<Verb><Noun>Job` · FKs `<singular>_id` · timestamps `created_at`/`updated_at` · prefer `published_at` over booleans · env vars `SCREAMING_SNAKE`, app ones `APP_`.

## Definition of Done ([workflow.md](workflow.md#definition-of-done))

```sh
cargo offrails verify          # after every slice: fmt --check, CSS, clippy, one test build, SQLite + Postgres at once (seconds)
cargo offrails verify --full   # once, at the end: + offrails-check, coverage ≥80% (≥90% models + policies), cargo deny,
                               #   gitleaks, no-unwrap review, routes, TS checks (web/), browser smoke, `prove` (BASE=…)
BASE=… cargo offrails prove    # every page: screenshots (desktop/mobile × light/dark), console, CSP, axe, links, overflow
                               #   → docs/proof/PROOF.md; link it in your final message
cargo nextest run <name>       # while a test is red: just that test (never the full DoD per edit)
```

Plus (each has its command in the DoD table): every `AC-<feature>-<n>` cited by a test · rollback then migrate works · no `unwrap`/`expect` outside tests and startup · no raw SQL outside `src/kernel/` · axe a11y suite in light **and** dark ([§10](guides/test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)) · `theme_test` green ([§11](guides/test-standards.md#11-theme-and-css-guards)) · UI changes checked in a real browser, login with no certificate overrides ([§9](guides/test-standards.md#9-real-browser-smoke)) · decisions logged.

## Security non-negotiables ([security.md](guides/security.md#non-negotiables))

Never weaken without a decision entry **and** the human's OK: strict CSP (no `unsafe-inline`/`unsafe-eval`) · the full header matrix on every response · CSRF/Origin checks (WebSocket handshake too) · escaping (raw HTML only via ammonia) · authorization on every route, join and push · tenant scoping · rate limits on login, magic links, tokens, email, sockets · tokens random, digest-only, expiring, minted inside jobs · session revocation closes sockets and streams too · cookies HttpOnly + SameSite=Lax + Secure, `__Host-` session in prod · secrets only in env · cargo-deny green · development reachable from this machine only: `serve` binds `config.bind` (127.0.0.1 unless `APP_BIND`), `kernel::hosts` refuses foreign `Host` names (DNS rebinding), an unset `APP_ENV` refuses to boot · `APP_TRUSTED_PROXY=true` (+ `_HOPS`) behind every proxy, or all users share one rate-limit key.

## Gotchas that cost the most ([full lists in each guide](AGENTS.md#load-order))

1. **SQLite transactions:** use `kernel::db::begin` (`BEGIN IMMEDIATE`). A deferred one fails with "database is locked" when the job worker writes ([database](guides/database.md)).
2. **Committed migrations are append-only.** A repair migration's `down` restores the predecessor's schema ([migrations](guides/migrations.md#rules)).
3. **Postgres:** a caught unique violation aborts the transaction. Use `on_conflict_do_nothing` / `try_insert` inside transactions ([models](guides/models.md)).
4. **`hx-boost="false"`** on every link into kernel pages (`/dev`, mailbox, `/admin/jobs`), or they render unstyled ([dev-tools](guides/dev-tools.md#rules)).
5. **Page JS loads from the layout and binds by delegation on `document`**: never in a page's `{% block head %}`, never on `DOMContentLoaded`. A boosted link runs nothing from the next page's `<head>`, so the button works on a fresh load and is dead after a click ([frontend-htmx](guides/frontend-htmx.md#boosted-navigation); caught by `smoke-boosted.mjs`). **htmx sse extension in the layout** too; one `sse-connect` per page; never reconnect by editing the attribute; `hx-sync` on live regions ([realtime](guides/realtime.md#client-side)).
6. **htmx history cache off** (`historyCacheSize: 0`, `refreshOnHistoryMiss: true`) plus the `pageshow` reload guard ([frontend-htmx](guides/frontend-htmx.md)).
7. **Signals carry a cursor** (`?since=`); without it, anything sent during a disconnect is lost ([realtime](guides/realtime.md#resume)).
8. **SSE events need non-empty data**, or browsers drop them.
9. **No secrets in job args.** Jobs mint tokens themselves; `/admin/jobs` shows args ([mailers](guides/mailers.md)).
10. **Route params are whole segments:** `/{id}.eml` panics at startup; use `/{id}/message.eml` ([routing](guides/routing.md)).
11. **`TS_RS_LARGE_INT=number`**, or ids become `bigint` in TS ([frontend-inertia](guides/frontend-inertia.md)).
12. **React type-check is `tsc -b`**, never `tsc --noEmit` (it passes with errors under project references).
13. **Read emails via `/dev/mailbox/latest.json`**, never by grepping raw `.eml` (quoted-printable breaks links).
14. **Never `pkill`/`killall` by name.** Stop the PID file you started: `kill "$(cat storage/pids/dev-$PORT.pid)"` (`bin/dev` writes it and stops its whole tree) ([cli](cli.md#running-the-app-from-an-agent)).
15. **Lighthouse checks one color scheme.** Run axe in light and dark ([§10](guides/test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)).
16. **Live pages need HTTP/2:** over HTTP/1.1, realtime breaks at ~6 tabs. `bin/dev` serves HTTPS with a trusted (mkcert) certificate; size per-user connection caps for tabs (128) ([realtime](guides/realtime.md#many-tabs-stall-http11-vs-http2)).
17. **Never select files by loose prefix, never depend on `HashMap` order, never hash files per request.** `"vendor/htmx-"` also matched `htmx-ext-sse`: pages randomly had no htmx. Exact keys, a uniqueness test, one catalog at boot ([frontend-htmx](guides/frontend-htmx.md#assets--cache-busting-kernelassetsrs)).
18. **Every lookup is scoped to the actor:** `board::find_for(db, &user, id)`; raw `find` is `pub(in crate::models)` so a controller can't compile an unscoped call ([models](guides/models.md#scoped-lookups-tenancy)).
19. **Never an untrusted certificate in a browser:** after "Proceed (unsafe)" Chrome drops `Set-Cookie`, so login bounces. No mkcert → `bin/dev` serves http and says so; never `--ignore-certificate-errors` in a smoke ([dev-tools](guides/dev-tools.md#https-in-development)).
20. **Every light surface, dark text and faint tone has its `dark:` partner** in the same class list (palette: [views](guides/views.md#dark-mode)); a template edited without Tailwind running ships missing classes. `theme_test.rs` catches both ([§11](guides/test-standards.md#11-theme-and-css-guards)).
21. **The thundering herd:** N watchers re-fetching after a signal = N renders per change. Render once per version (`try_get_with((stream, version))`, the version bumped in `poll_once`, never at write time); pub/sub senders are per stream ([realtime](guides/realtime.md#render-once-per-version-the-thundering-herd)).
22. **Overload is 503 + `Retry-After`, never 408:** pool `acquire_timeout` 5 s, the `TimeoutLayer` status 503, live re-fetches gated by `X-Live-Refresh`; shutdown drains at most `APP_SHUTDOWN_DRAIN_SECS` ([errors](guides/errors-and-logging.md#response-mapping)).
23. **Never `location.reload()` after a mutation:** move the DOM optimistically, then `htmx.ajax` the live region ([frontend-htmx](guides/frontend-htmx.md#no-full-reload-after-a-mutation)).
24. **Ask the running app before guessing:** `curl -s $URL/dev/introspect.json` (migrations, CSS staleness, protocol, streams), development only ([dev-tools](guides/dev-tools.md#introspection)).
25. **Ids are 64-bit:** `big_pk_auto` and `big_integer` for every `id`/`*_id`, `i64` in entities. A 32-bit id passes on SQLite and is a 500 on every Postgres write; `TestApp` reads `TEST_DATABASE_URL` so the suite runs there (`offrails-check` `[ids]`, `[postgres]`; [migrations](guides/migrations.md#ids-are-64-bit)).
26. **`assert_text` escapes its argument like Askama** (`'` → `&#39;`), so it finds text that came through `{{ … }}`; a literal apostrophe typed in a template stays `'` and needs `assert_contains` ([testing](guides/testing.md#gotchas-agents-get-wrong)).
27. **Spans, one rule:** `#[tracing::instrument(skip_all)]` on actions and public model/service fns; `level = "debug", skip_all` on helpers called per row or in a loop; never without `skip_all` ([errors](guides/errors-and-logging.md#spans)).
28. **No `hx-trigger` filters** (`keyup[key=='Enter']`): they need eval, `allowEval:false` (the CSP) turns them into a trigger that silently never fires. A plain event, or a delegated listener in `static/app.js` calling `htmx.trigger` (`offrails-check` `[htmx]`; [frontend-htmx](guides/frontend-htmx.md#gotchas-agents-get-wrong)).

## Commands ([cli.md](cli.md))

```
app serve | worker | health | routes
app db migrate | rollback [--steps N] | status | seed | setup | reset | console
app jobs stats|list|show|retry|cancel|delete|run|pause|resume|prune      app task <name> [args…]      app introspect
bin/setup   bin/dev   docker compose up                      offrails new | check | upgrade [--check] | update | doctor
```
