# Cheatsheet

The rules agents need most, on two pages. Read this first; open a full guide only when your task touches its topic ([AGENTS.md](AGENTS.md#load-order) maps topics to guides). When this page and a guide disagree, the guide wins.

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
src/{main,lib,config,state,routes,error}.rs   src/{models,controllers,views,jobs,mailers,channels,policies,tasks,kernel}/
migration/   templates/   static/   web/ (TS modes)   tests/app/ (ONE test binary)   bin/{setup,dev,docker-entrypoint}
```

**Naming:** tables plural snake (`blog_posts`) · model module singular · `<Model>Params` (untrusted) → `validate()` → `New<Model>` · controllers plural, 7 Rails actions · routes plural kebab (`/blog-posts/{id}`) · jobs `<Verb><Noun>Job` · FKs `<singular>_id` · timestamps `created_at`/`updated_at` · prefer `published_at` over booleans · env vars `SCREAMING_SNAKE`, app ones `APP_`.

## Definition of Done ([workflow.md](workflow.md#definition-of-done))

```sh
sh .offrails/tools/offrails-check                        # header, ACs cited, Security tests, tenancy suites, scoped lookups
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo nextest run                                         # SQLite
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:${DB_PORT:-5432}/postgres cargo nextest run   # Postgres (docker compose up -d db)
cargo llvm-cov nextest --fail-under-lines 80 …            # ≥80% total, ≥90% models + policies
cargo deny check && gitleaks git . --redact --no-banner   # + pnpm audit --prod in TS modes
# htmx: bin/tailwindcss -i assets/app.css -o static/app.css --minify
# TS:   pnpm --dir web exec biome check --write . && pnpm --dir web typecheck && pnpm --dir web test --run && pnpm --dir web build
```

Plus (each has its command in the DoD table): every `AC-<feature>-<n>` cited by a test · rollback then migrate works · no `unwrap`/`expect` outside tests and startup · no raw SQL outside `src/kernel/` · axe a11y suite in light **and** dark ([§10](guides/test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)) · UI changes checked in a real browser ([§9](guides/test-standards.md#9-real-browser-smoke)) · decisions logged.

## Security non-negotiables ([security.md](guides/security.md#non-negotiables))

Never weaken without a decision entry **and** the human's OK: strict CSP (no `unsafe-inline`/`unsafe-eval`) · the full header matrix on every response · CSRF/Origin checks (WebSocket handshake too) · escaping (raw HTML only via ammonia) · authorization on every route, join and push · tenant scoping · rate limits on login, magic links, tokens, email, sockets · tokens random, digest-only, expiring, minted inside jobs · session revocation closes sockets and streams too · cookies HttpOnly + SameSite=Lax + Secure, `__Host-` session in prod · secrets only in env · cargo-deny green.

## Gotchas that cost the most ([full lists in each guide](AGENTS.md#load-order))

1. **SQLite transactions:** use `kernel::db::begin` (`BEGIN IMMEDIATE`). A deferred one fails with "database is locked" when the job worker writes ([database](guides/database.md)).
2. **Committed migrations are append-only.** A repair migration's `down` restores the predecessor's schema ([migrations](guides/migrations.md#rules)).
3. **Postgres:** a caught unique violation aborts the transaction. Use `on_conflict_do_nothing` / `try_insert` inside transactions ([models](guides/models.md)).
4. **`hx-boost="false"`** on every link into kernel pages (`/dev`, mailbox, `/admin/jobs`), or they render unstyled ([dev-tools](guides/dev-tools.md#rules)).
5. **htmx sse extension in the layout**, never a page's head; one `sse-connect` per page; never reconnect by editing the attribute; `hx-sync` on live regions ([realtime](guides/realtime.md#client-side)).
6. **htmx history cache off** (`historyCacheSize: 0`, `refreshOnHistoryMiss: true`) plus the `pageshow` reload guard ([frontend-htmx](guides/frontend-htmx.md)).
7. **Signals carry a cursor** (`?since=`); without it, anything sent during a disconnect is lost ([realtime](guides/realtime.md#resume)).
8. **SSE events need non-empty data**, or browsers drop them.
9. **No secrets in job args.** Jobs mint tokens themselves; `/admin/jobs` shows args ([mailers](guides/mailers.md)).
10. **Route params are whole segments:** `/{id}.eml` panics at startup; use `/{id}/message.eml` ([routing](guides/routing.md)).
11. **`TS_RS_LARGE_INT=number`**, or ids become `bigint` in TS ([frontend-inertia](guides/frontend-inertia.md)).
12. **React type-check is `tsc -b`**, never `tsc --noEmit` (it passes with errors under project references).
13. **Read emails via `/dev/mailbox/latest.json`**, never by grepping raw `.eml` (quoted-printable breaks links).
14. **Never `pkill`/`killall` by name.** Stop your own PID, or `lsof -ti tcp:$PORT -sTCP:LISTEN | xargs kill` ([cli](cli.md#running-the-app-from-an-agent)).
15. **Lighthouse checks one color scheme.** Run axe in light and dark ([§10](guides/test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)).
16. **Live pages need HTTP/2:** over HTTP/1.1, realtime breaks at ~6 tabs. `bin/dev` serves HTTPS; size per-user connection caps for tabs (128) ([realtime](guides/realtime.md#many-tabs-stall-http11-vs-http2)).
17. **Never select files by loose prefix, never depend on `HashMap` order, never hash files per request.** `"vendor/htmx-"` also matched `htmx-ext-sse`: pages randomly had no htmx. Exact keys, a uniqueness test, one catalog at boot ([frontend-htmx](guides/frontend-htmx.md#assets--cache-busting-kernelassetsrs)).
18. **Every lookup is scoped to the actor:** `board::find_for(db, &user, id)`; raw `find` is `pub(in crate::models)` so a controller can't compile an unscoped call ([models](guides/models.md#scoped-lookups-tenancy)).

## Commands ([cli.md](cli.md))

```
app serve | worker | health | routes
app db migrate | rollback [--steps N] | status | seed | setup | reset | console
app jobs stats|list|show|retry|cancel|delete|run|pause|resume|prune      app task <name> [args…]
bin/setup   bin/dev   docker compose up                      offrails new | check | upgrade [--check] | update | doctor
```
