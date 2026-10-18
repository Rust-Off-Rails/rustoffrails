# Agent Workflow

## The human interface: `product/`

Humans describe the app in `product/README.md` and `product/features/*.md` (template: `blueprints/feature-spec.md`). They can also just talk to you. When they do, **write or update the spec file first**, then build. The spec is the durable record. Chat is not.

Before building anything new, run the relevant part of [`interview.md`](interview.md). Ask what only the human can answer, assume everything else, and write it all down.

## The loop

```
0. ASK      interview.md: new app → full interview; new feature → feature-level checklist
1. READ     spec → resources, fields, rules, side effects, acceptance criteria
2. PLAN     where each piece starts: a snippet (cargo offrails snippets; snippet show <name>),
            a generator (cargo offrails g --help; --dry-run lists its files), or the guides;
            a kernel need → a part (cargo offrails parts); show the plan for new apps/big features
3. START    cargo offrails snippet add … / cargo offrails g resource|model|api|… (files + marked
            wiring + tests), or write the files by hand in the same shape (small files per
            feature, one marked line per shared spot); commit right after a generator or snippet
            run; `cargo run -- db migrate`; prove rollback
4. MODEL    customize the `// APP:` lines: validation rules, scopes the spec names, side effects
            (inertia/spa: derive TS on Params + view props; tests export them)
5. TEST     failing tests for the acceptance criteria, next to the generated ones
6. BUILD    controller, routes, views (templates from the component kit, components/ui.html, in a
            layout / Inertia pages / SPA routes), jobs, mailers
7. CHECK    cargo check → fix → repeat (the compiler is your pair); one test: cargo nextest run <name>
            running app: bin/dev rebuilds, restarts and reloads open tabs on save;
            ask it before guessing: curl -s $URL/dev/introspect.json (migrations, css, realtime, url)
8. VERIFY   cargo offrails verify after each slice (seconds); --full once, before you say done;
            pages: BASE=… cargo offrails prove → docs/proof/PROOF.md, linked in your final message
9. RECORD   docs/routes.md, docs/decisions.md, spec checkboxes
```

Work in **vertical slices**: one resource or behavior end-to-end, green, then the next. Never leave the tree not compiling between slices.

### Parallel helpers (subagents)

One Rust crate is one compile unit: an error in any file blocks `cargo check` for every file. So parallel helpers only pay off when the tree already compiles **and** each helper owns separate files.

- **Never run parallel helpers before the skeleton compiles.** Generate it from the template yourself (`generators/new.md`), run one `cargo check`, then make the baseline commit.
- **After that, at most one helper per vertical slice.** Each helper's prompt lists the exact files it owns and says "touch nothing else". Shared files (`src/routes.rs`, `src/kernel/mod.rs`, `src/models/mod.rs`, `migration/src/lib.rs`, `Cargo.toml`) are edited by you, the coordinator, after the helpers report.
- **A helper whose `cargo check` fails because of another helper's file stops and reports.** It doesn't edit that file, stub it out, or build a scratch crate in `/tmp` to type-check its own part. Those workarounds hide integration errors and burn tokens.
- Read-only helpers are always fine: exploring a reference app, mapping routes, drafting specs in `product/`.

## When the product grows a need, add the part

Before you hand-write kernel code (a mailer, a job queue, sessions and sign-in, a socket layer, a dashboard), check whether a part does it: `cargo offrails list-parts`. If one does, add it, and build the feature on top:

```sh
cargo offrails add-part mailer        # e.g. the spec now says "email the owner when…"; dependencies come first
cargo run -q -- db migrate && cargo run -q -- routes
```

Typical moments: the first email (`mailer`, `dev-mailbox`), the first slow, retried or scheduled work (`jobs`, `jobs-dashboard`), the first sign-in (`auth-core` + a method), the first push to other users (`realtime`). The [decision table](template/README.md#choosing-parts) maps spec signals to parts. Tell the human in one line what the part adds (link: <https://github.com/rust-off-rails/samples>, screenshots of every screen), log it (`add-part` writes the `docs/decisions.md` line), then the Definition of Done on both databases. Hand-write it only when no part fits, or the human declined the part (recorded in `docs/decisions.md`).

## Definition of Done

All must pass. Gates and thresholds are defined in [`guides/test-standards.md`](guides/test-standards.md). One command runs them, in parallel where they are independent:

```sh
cargo offrails verify          # --fast, after every slice: fmt --check, the CSS (htmx), clippy -D warnings,
                               # one test build, then the SQLite and Postgres suites at the same time
cargo offrails verify --full   # once, before you say done: also offrails-check, coverage (the SQLite run, instrumented),
                               # cargo deny, gitleaks, the no-unwrap review, routes vs docs/routes.md, the TS checks
                               # (web/), and the browser smokes when BASE=<the URL bin/dev printed> is set (the kernel
                               # pages; every page after a boosted link: "skip: no Chrome" without a browser), brand --check,
                               # and `prove` (screenshots + checks of every page → docs/proof/PROOF.md)
```

It prints one line per step with its time and test counts, the tail of each failure (full logs in `target/verify/`), and exits non-zero on any failure. Postgres comes from `TEST_DATABASE_URL` (the environment or `.env`), else the compose `db` URL when something listens on `localhost:${DB_PORT:-5432}`; with neither, the Postgres run is reported **skipped**, never passed. `cargo fmt --all` fixes what `fmt --check` reports.

**Iterate fast, verify fully once.** While a slice is red, run only what you're fixing: `cargo check`, then `cargo nextest run <test name>`. At the end of each slice, `cargo offrails verify` (measured after a one-line change, M2, load ~10: a bare app 5.5–6.7 s, an app with every part 33–37 s, most of it the Postgres suite). Never run coverage, `cargo deny`, gitleaks or the full browser smokes per iteration: they belong to `--full`, once, when the work is done. A real-browser smoke of the flow you changed is still due for UI changes (the table below), against the app `bin/dev` is already serving.

What `verify` runs, for running one step by hand:

```sh
cargo fmt --all --check
bin/tailwindcss -i assets/app.css -o static/app.css --minify            # htmx mode
CARGO_TARGET_DIR=target/clippy cargo clippy --all-targets -- -D warnings  # its own dir, so it compiles beside the tests
cargo nextest run                                   # SQLite; also re-exports ts-rs types
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:${DB_PORT:-5432}/postgres cargo nextest run -E 'kind(test)'   # Postgres: required locally when reachable; CI always runs it
# --full adds:
sh .offrails/tools/offrails-check                   # header, every AC cited, Security-table tests exist, tenancy suites, no unscoped lookups (exit 0)
cargo llvm-cov nextest --summary-only --ignore-filename-regex 'src/main\.rs$' --fail-under-lines 80   # test-standards.md §5: ≥ 80% lines overall, ≥ 90% for src/models and src/policies
cargo deny check                                    # advisories, licenses, banned crates, sources (guides/security.md#automated-enforcement)
gitleaks git . --redact --no-banner                 # secrets anywhere in history (.gitleaks.toml)
pnpm --dir web exec biome check . && pnpm --dir web typecheck && pnpm --dir web test --run && pnpm --dir web build && pnpm --dir web audit --prod   # TS modes; React: `tsc -b` (never `tsc --noEmit`); Svelte: svelte-check
```

**Postgres on another port:** if 5432 is taken (another app's Postgres, a native install), never stop it: run the compose `db` service elsewhere with `DB_PORT=54329 docker compose up -d db` and test with `DB_PORT=54329` (or set `TEST_DATABASE_URL` to any Postgres you may create databases in; the suite makes a template and one reusable slot per concurrent test). Check first with `lsof -nP -iTCP:5432 -sTCP:LISTEN`.

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
| User-visible changes: you ran the app and exercised it. **UI changes: a real-browser smoke** of the changed flow ([test-standards §9](guides/test-standards.md#9-real-browser-smoke)) | `BASE=<the URL bin/dev printed> bash .offrails/tools/browser-smoke/run.sh` (styling, Back/Forward, no console errors or CSP violations; Node ≥ 22.12 and Chrome, else it exits 77 and `verify` reports a skip); every page after a boosted link: `BASE=… node .offrails/tools/browser-smoke/smoke-boosted.mjs` ([testing](guides/testing.md#browser-smoke-boosted-navigation); both run in `verify --full` with `BASE` set; with password sign-in it also signs in, as `demo@example.com` from `cargo run -- task auth_demo`, or as `SMOKE_EMAIL`/`SMOKE_PASSWORD` when an app's own seed account differs: `SMOKE_EMAIL=me@example.com SMOKE_PASSWORD=… BASE=… cargo offrails verify --full`; pages with parameters: `SMOKE_PATHS=/posts/1,/blog/hello`); accounts: `SCENARIO=tests/e2e/smoke.scenario.mjs … run.sh login` (no certificate overrides: the session must survive a fresh page load); `… run.sh a11y` (axe, light + dark); live pages: `… run.sh live-tabs`. `curl -s $BASE/dev/introspect.json \| jq .css.stale,.migrations.current` prints `false`, `true`. Emails: `curl -s $BASE/dev/mailbox/latest.json`. Jobs: `cargo run -- jobs list --state discarded` is empty |
| Templates correct in both color schemes; the built CSS has every class (htmx) | `cargo nextest run theme_test` ([test-standards §11](guides/test-standards.md#11-theme-and-css-guards)) |
| **Proof** (apps with pages): every page screenshotted and checked, each acceptance criterion traced to its passing tests and pages. **Link `docs/proof/PROOF.md` in your final message** | `BASE=<the URL bin/dev printed> cargo offrails prove` ([testing](guides/testing.md#proof-cargo-offrails-prove); `verify --full` runs it with `BASE` set) |

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

**After a generator or snippet run, commit before you edit** (when commits are yours to make): `git add -A && git commit -m "generate resource posts"`, then customize. The generated code and your changes are separate diffs: review sees what you decided, a bad edit reverts without losing the scaffold, and `destroy` still recognizes the files you left alone. When the human makes the commits, say which files a generator wrote in your report.

## Talking to the human

Report in product terms ("Books can now be reviewed; owners get an email"), then link the proof (`docs/proof/PROOF.md`: what each criterion's tests and screenshots show), then list files changed, then any assumptions made. Never tell the human to edit code. Tell them which spec to edit, or just to tell you.
