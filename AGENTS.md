# AGENTS.md: Rust off Rails

You are building (or maintaining) a **Rust off Rails** application. This file is your entry point. It is written for you, the agent, not for humans.

## Prime directives

1. **All code is written by AI: you write all of it, and humans write none.** This is the framework's founding rule ([doctrine §0](doctrine.md#0-all-code-is-written-by-ai)). Humans supply intent as Markdown in `product/`. If you find code a human edited by hand, keep it working, record the intent it expresses in the relevant spec, and continue. Never ask a human to write or paste code.
2. **Start app code from a snippet or a generator (both tested, both yours once copied); hand-write the rest.** A [snippet](template/snippets/README.md) is small working code to read or copy (`cargo offrails snippets`, `snippet show <name>`, `snippet add <name>`): a feed, slugs, Markdown, a pager. A [generator](generators/README.md) writes a feature to your names and fields (`cargo offrails g --help`): a CRUD resource, a model, a page with a path param, an API, a job. Neither is mandatory; when neither fits, or editing one would be more work than writing, write it from the guides. Kernel pieces (mail, jobs, sign-in, sockets) come from parts (`cargo offrails parts`, `cargo offrails add <part>`). Whatever you start from: small files per feature, one marked line per shared spot (`// posts`), then the `// APP:` lines. Pages are built from the component kit (`{% import "components/ui.html" as ui %}`, one of five layouts; generated and snippet pages already are: [views](guides/views.md#components)), and what you built is proven with `BASE=… cargo offrails prove` (screenshots and checks of every page, `docs/proof/PROOF.md`: [testing](guides/testing.md#proof-cargo-offrails-prove)).
3. **Do not deliberate on settled questions.** If these docs answer it, follow the answer. Do not compare alternatives, propose other crates, or "consider" a different layout. Opinions here beat your priors. When a guide gives a template or a whole file, copy it and change only the parts it marks.
4. **The compiler is your reviewer, and the test suite is the second one.** Prefer designs where mistakes become compile errors: typed SeaORM queries over SQL strings, Askama over runtime templates, `ts-rs` types over hand-written TS, enums over strings. What types can't prove, tests prove on **both** SQLite and Postgres.
5. **Never type a crate version from memory.** Use `cargo add`. Before using a crate API you are not certain of, check the resolved version in `Cargo.lock` and read its docs (`cargo doc --open` or docs.rs for that exact version). API drift between versions is the main way agents break this stack.
6. **Explicit beats magic.** You don't pay for boilerplate, so write it. No proc-macro frameworks, no reflection, no hidden callbacks.
7. **Green or it didn't happen.** A task is done only when the [Definition of Done](workflow.md#definition-of-done) passes, starting with `sh .offrails/tools/offrails-check`. Every item has a command; run it, don't eyeball it.
8. **Leave a trail.** Every non-obvious decision goes in `docs/decisions.md` in the app (one dated line each). The next agent has no memory but yours.
9. **Copy what's built; write what's yours.** New apps start from the tested [template](template/README.md) (`offrails-gen new`), and kernel needs are met by its parts (`cargo offrails add <name>`), never retyped from a guide. App-specific code (models, controllers, views, jobs) starts from a snippet, a generator or the guides (directive 2). Once copied or generated, the code is the app's: you adapt it and own it.

## Check the frontend mode first

The app's root `AGENTS.md` declares `FRONTEND_MODE: htmx | inertia | spa | api` (plus `ISLANDS: yes` if used) and, when the app has a JS UI (`inertia`, `spa` or islands), `UI_LIBRARY: react | svelte`. Every guide has sections per mode, and the JS guides have sections per library. Follow only the ones that match. Never switch modes unless a human asks, and record it in `docs/decisions.md` if they do. It also declares `API: none | rest | graphql | both`: a public, versioned API for other programs, independent of the UI mode ([api](guides/api.md)); `api` mode always has one.

## Load order

**Start with [`CHEATSHEET.md`](CHEATSHEET.md)** (two pages: stack, layout, naming, the Definition of Done, security non-negotiables, the costliest gotchas). Then read a full guide **only when your task touches its topic** (the table below), and inside a guide, read its **TL;DR** first and stop when it answers your question.

Read these before your first change in a session:

1. [`interview.md`](interview.md): **before building anything new**, ask the right questions
2. [`doctrine.md`](doctrine.md): why things are the way they are (short)
3. [`structure.md`](structure.md): where every file goes
4. [`stack.md`](stack.md): which crates, and which are banned
5. [`workflow.md`](workflow.md): the loop and the Definition of Done

Read on demand:

| When the task involves…                    | Read                                   |
|--------------------------------------------|----------------------------------------|
| "How would Rails do X?"                    | [`RAILS_MAP.md`](RAILS_MAP.md)          |
| Tables, queries, validations, associations | [`guides/models.md`](guides/models.md), [`guides/migrations.md`](guides/migrations.md) |
| SQLite vs Postgres, connections, portability | [`guides/database.md`](guides/database.md) |
| URLs, handlers, params, flash              | [`guides/routing.md`](guides/routing.md), [`guides/controllers.md`](guides/controllers.md) |
| HTML, layouts, components, brand, partials, forms, htmx (`htmx` mode) | [`guides/views.md`](guides/views.md), [`guides/frontend-htmx.md`](guides/frontend-htmx.md) |
| React or Svelte pages with server routing (`inertia` mode) | [`guides/frontend-inertia.md`](guides/frontend-inertia.md) (+ [`guides/frontend-svelte.md`](guides/frontend-svelte.md) when `UI_LIBRARY: svelte`) |
| SPA + JSON API (`spa` mode) or islands | [`guides/frontend-react.md`](guides/frontend-react.md) (React) or [`guides/frontend-svelte.md`](guides/frontend-svelte.md) (Svelte: SvelteKit SPA), [`guides/controllers.md#json-api`](guides/controllers.md#json-api) |
| API-only apps (`api` mode), a public API (`API` header), tokens, CORS, OpenAPI | [`guides/api.md`](guides/api.md) |
| GraphQL (`API: graphql \| both`)          | [`guides/graphql.md`](guides/graphql.md) |
| Login, sessions, permissions               | [`guides/auth.md`](guides/auth.md), [`guides/security.md`](guides/security.md) |
| Background work, scheduled work            | [`guides/jobs.md`](guides/jobs.md)      |
| Jobs dashboard, `app jobs`, inspecting failures | [`guides/jobs-dashboard.md`](guides/jobs-dashboard.md) |
| `/dev` tools, dev mailbox, checking the running app | [`guides/dev-tools.md`](guides/dev-tools.md) |
| Email                                      | [`guides/mailers.md`](guides/mailers.md) |
| Live updates (signals)                     | [`guides/realtime.md`](guides/realtime.md) |
| Chat, presence, websockets, multi-node realtime | [`guides/channels.md`](guides/channels.md) |
| File uploads                               | [`guides/storage.md`](guides/storage.md) |
| Caching                                    | [`guides/caching.md`](guides/caching.md) |
| Env vars, secrets, environments            | [`guides/config.md`](guides/config.md)  |
| Errors, logging, observability             | [`guides/errors-and-logging.md`](guides/errors-and-logging.md) |
| Tests, browser smokes, `cargo offrails prove` | [`guides/testing.md`](guides/testing.md) |
| Shipping it, `docker compose`, direct TLS  | [`guides/deployment.md`](guides/deployment.md) |
| Running a `rails generate` equivalent: `cargo offrails g <kind>` | [`generators/README.md`](generators/README.md) |
| A recipe to copy (feed, slugs, Markdown, pager, theme toggle, backdated seeds, public page by slug): `cargo offrails snippets` | [`template/snippets/README.md`](template/snippets/README.md) |
| Upgrading the app to a newer framework release | [`generators/upgrade.md`](generators/upgrade.md) |
| Starting an app from the template; parts (mailer, jobs, auth, realtime, dev dashboard): proposing, adding, anchors | [`template/README.md`](template/README.md), [`guides/samples.md`](guides/samples.md) |
| Commands (`rails s`, `db:migrate`, …)      | [`cli.md`](cli.md)                      |

## Where the framework lives inside an app

`generators/new.md` (after the interview) copies `template/app/` into the new app and vendors this directory into it at `.offrails/` (so `.offrails/template/` holds the template and parts the app came from, for `add-part` and upgrades). The app's root `AGENTS.md` (from `blueprints/AGENTS.md`) points here. Treat `.offrails/` as read-only. To deviate from it, record the deviation in `docs/decisions.md` and say so in your final message to the human.

## When the docs are silent

1. Find the closest Rails convention in `RAILS_MAP.md` and do the Rust equivalent.
2. If there is none, choose the most boring option: fewest crates, most explicit code, most compile-time checking.
3. Record the choice in `docs/decisions.md`.
