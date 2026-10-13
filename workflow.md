# Agent Workflow

## The human interface: `product/`

Humans describe the app in `product/README.md` and `product/features/*.md` (template: `blueprints/feature-spec.md`). They can also just talk to you. When they do, **write or update the spec file first**, then build. The spec is the durable record. Chat is not.

Before building anything new, run the relevant part of [`interview.md`](interview.md). Ask what only the human can answer, assume everything else, and write it all down.

## The loop

```
0. ASK      interview.md: new app → full interview; new feature → feature-level checklist
1. READ     spec → resources, fields, rules, side effects, acceptance criteria
2. PLAN     list files to create/change (use generators/ recipes); show the plan for new apps/big features
3. MIGRATE  migration + entity together; `cargo run -- db migrate`; prove rollback
4. MODEL    entity, params, validate(), queries
            (inertia/spa: derive TS on Params + view props; tests export them)
5. TEST     failing integration tests from the acceptance criteria
6. BUILD    controller, routes, views (templates / Inertia pages / SPA routes), jobs, mailers
7. CHECK    cargo check → fix → repeat (the compiler is your pair)
            running app: bin/dev rebuilds, restarts and reloads open tabs on save;
            ask it before guessing: curl -s $URL/dev/introspect.json (migrations, css, realtime, url)
8. VERIFY   Definition of Done, including running the app
9. RECORD   docs/routes.md, docs/decisions.md, spec checkboxes
```

Work in **vertical slices**: one resource or behavior end-to-end, green, then the next. Never leave the tree not compiling between slices.

## Definition of Done

All must pass, in this order. Gates and thresholds are defined in [`guides/test-standards.md`](guides/test-standards.md).

```sh
sh .offrails/tools/offrails-check                   # header, every AC cited, Security-table tests exist, tenancy suites, no unscoped lookups (exit 0)
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run                                   # SQLite; also re-exports ts-rs types
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:${DB_PORT:-5432}/postgres cargo nextest run   # Postgres (docker compose up -d db): required locally when reachable; CI always runs it

# coverage gate (test-standards.md §5): ≥ 80% lines overall, ≥ 90% for src/models and src/policies
cargo llvm-cov nextest --summary-only --ignore-filename-regex 'src/main\.rs$' --fail-under-lines 80

# security gates (guides/security.md#automated-enforcement)
cargo deny check                                    # advisories, licenses, banned crates, sources
gitleaks git . --redact --no-banner                 # secrets anywhere in history (.gitleaks.toml)
pnpm --dir web audit --prod                         # TS modes only

# htmx mode
bin/tailwindcss -i assets/app.css -o static/app.css --minify

# inertia / react / islands
pnpm --dir web exec biome check --write .
pnpm --dir web typecheck                           # React: `tsc -b` (never `tsc --noEmit`); Svelte: svelte-check
pnpm --dir web test --run
pnpm --dir web build
```

**Postgres on another port:** if 5432 is taken (another app's Postgres, a native install), never stop it: run the compose `db` service elsewhere with `DB_PORT=54329 docker compose up -d db` and test with `DB_PORT=54329` (or set `TEST_DATABASE_URL` to any Postgres you may create databases in; the suite makes one per test). Check first with `lsof -nP -iTCP:5432 -sTCP:LISTEN`.

Plus, each with the command that proves it (paste its output, or say which item you skipped and why):

| Check | Command (prints nothing / exits 0 when it passes) |
|-------|---------------------------------------------------|
| Accessibility, apps with a UI: zero violations on every route, light **and** dark, desktop and mobile ([test-standards §10](guides/test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)) | `pnpm --dir web exec playwright test a11y` (TS modes) · `pnpm --dir tests/e2e exec playwright test a11y` (htmx), against a running app |
| Every `AC-<feature>-<n>` has a test citing it; ticked only when that test passes on both databases | `sh .offrails/tools/offrails-check` (the `[ac]` lines) |
| New migrations roll back cleanly | `cargo run -- db rollback --steps <n> && cargo run -- db migrate` |
| No `unwrap()`/`expect()` outside tests and startup | `cargo clippy -q --lib --bins -- -W clippy::unwrap_used -W clippy::expect_used` (non-test code only); every hit is startup code with a message saying why it can't fail |
| No raw SQL outside `src/kernel/` and logged escape hatches | `grep -rn -e 'Statement::from_string' -e 'execute_unprepared' src/ \| grep -v '^src/kernel/'` |
| (inertia/spa) `web/src/types/` untouched by hand; no TS type duplicates a Rust type | `cargo test --lib export_bindings && git diff --exit-code web/src/types/` |
| No `todo!()`, `unimplemented!()`, or `// TODO` without a linked spec "Open question" | `grep -rn -e 'todo!(' -e 'unimplemented!(' -e '// TODO' src/ web/src/ 2>/dev/null` |
| New routes are in `docs/routes.md` | `cargo run -- routes && git diff --exit-code docs/routes.md` |
| New env vars are in `.env.example` (commented out is fine), `Config` and the deploy config | `sh .offrails/tools/offrails-check` (`[env]`: every `"SCREAMING_SNAKE"` literal in `src/config.rs`, however it's read, must be in `.env.example`); the deploy config by hand |
| Decisions not covered by `.offrails/` are logged | `git diff --stat docs/decisions.md` shows a line for each new choice ([conventions](#decisions)) |
| **Security-relevant changes** (auth, sessions, permissions, tenancy, payments, uploads, realtime, APIs, webhooks, outbound fetches, anything new in `headers_test.rs`) | `/offrails:security` or its checklist; the spec's `## Security` STRIDE table names an **attack test** per threat (failing first, then fixed), and `offrails-check` finds each one. A Non-negotiable ([security](guides/security.md#non-negotiables)) is never weakened without a decision **and** the human's explicit OK. ZAP runs in CI; run it locally when you add routes or change headers |
| User-visible changes: you ran the app and exercised it. **UI changes: a real-browser smoke** of the changed flow ([test-standards §9](guides/test-standards.md#9-real-browser-smoke)) | `BASE=<the URL bin/dev printed> bash .offrails/tools/browser-smoke/run.sh` (styling, Back/Forward, no console errors or CSP violations); accounts: `SCENARIO=tests/e2e/smoke.scenario.mjs … run.sh login` (no certificate overrides: the session must survive a fresh page load); `… run.sh a11y` (axe, light + dark); live pages: `… run.sh live-tabs`. `curl -s $BASE/dev/introspect.json \| jq .css.stale,.migrations.current` prints `false`, `true`. Emails: `curl -s $BASE/dev/mailbox/latest.json`. Jobs: `cargo run -- jobs list --state discarded` is empty |
| Templates correct in both color schemes; the built CSS has every class (htmx) | `cargo nextest run theme_test` ([test-standards §11](guides/test-standards.md#11-theme-and-css-guards)) |

## When something fails

- **Compile error in a crate API:** check the version in `Cargo.lock`, read that version's docs, and fix the call. Don't swap crates.
- **"no such table" / "relation does not exist" at runtime:** run `cargo run -- db migrate`.
- **Passes on SQLite, fails on Postgres (or the reverse):** you broke a portability rule in `guides/database.md`. Fix the code, not the test.
- **The same error three times:** stop, reread the relevant guide, and reduce the change to the smallest failing piece.
- **Evidence first, workarounds last.** Before you add a retry, a timeout, a watchdog, a reload cap, a `sleep`, a polling fallback or a raised limit, capture the evidence of the actual failure: the request and its status (`curl -skv`, the browser's network panel, `chrome://net-export`), the server log line with its `request_id`, the protocol (`%{http_version}`). Then fix the cause. A workaround that stays needs a `docs/decisions.md` line citing that evidence and the cause it covers for. Without one, the next agent deletes it. (A 0.3 app shipped a stall watchdog and reload caps around what was HTTP/1.1 connection starvation; [realtime](guides/realtime.md#many-tabs-stall-http11-vs-http2).)
- **Genuinely blocked** (needs a credential, an external account, or a product decision): stop and tell the human exactly what you need and why. Don't stub around it silently.

## Decisions

`docs/decisions.md` is the memory between agents. One dated line per choice `.offrails/` didn't dictate, newest last:

```
2026-10-13: board cards load lazily past 50 (because boards with 400 cards took 2 s; spec: product/features/boards.md)
```

- **Superseding:** never edit or delete an old line. Add a new one that names it: `2026-11-02: supersedes 2026-10-13 (lazy cards): HTTP/2 fixed the stall, cards load eagerly again`. A reader takes the newest line on a topic.
- **Workarounds** (retries, watchdogs, caps, fallbacks, raised limits) cite the evidence and the cause they cover for ([evidence first](#when-something-fails)). When the cause is fixed, the line is superseded and the workaround removed in the same change.
- **Upgrades** reconcile this file: a decision that a new framework release makes obsolete gets a superseding line ([upgrade](generators/upgrade.md)).

## Human code edits

The rule is zero handwritten code, but humans will occasionally paste a fix. When you notice one (via `git log`/`git blame` or because they say so):

1. Don't revert it.
2. Make it conform to these conventions if it doesn't.
3. Capture the *intent* in the relevant spec or `docs/decisions.md`, so a future regeneration keeps it.

## Commits

One commit per vertical slice. Message format: `<resource>: <what changed>` plus a body listing the spec file(s) implemented. Commit only when the human has asked you to manage commits. The one exception is the baseline commit at the end of `generators/new.md`.

## Talking to the human

Report in product terms ("Books can now be reviewed; owners get an email"), then list files changed, then any assumptions made. Never tell the human to edit code. Tell them which spec to edit, or just to tell you.
