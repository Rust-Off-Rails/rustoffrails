---
name: check
description: Run the Rust off Rails Definition of Done (fmt, clippy, tests on SQLite and Postgres, frontend checks, checklist) and fix what fails. Use before saying work is done, or when the user asks "is it green?", "run the tests", or "check everything".
argument-hint: "[--deep]"
---

# /offrails:check — the Definition of Done

Only for Rust off Rails apps (a `.offrails/` directory exists).

1. Read the app's `AGENTS.md` for `FRONTEND_MODE` and `ISLANDS`. For Postgres: use `TEST_DATABASE_URL` if set (or in `.env`). Otherwise, if Docker is available, run `docker compose up -d db`. If neither works, say that Postgres was skipped. Never claim it passed.
2. Run `cargo offrails verify --full` (apps before 0.7.2 have no `verify`: run `sh .offrails/tools/offrails-check`, then every command in `.offrails/workflow.md#definition-of-done` for that mode, in order). It runs `offrails-check`, fmt, the CSS, clippy, both suites, coverage (§5 of test-standards, the SQLite run instrumented), cargo-deny, gitleaks, routes and the TS checks in parallel, and prints one line per step. Every `FAIL` line is a failure; every `skip` line is said in the report. `offrails-check` covers the header, AC traceability, Security-table tests, the tenancy suites and unscoped lookups mechanically, so don't re-judge those by eye. With a running app (`bin/dev`), pass `BASE=<its URL>` so the browser smoke runs too.
3. Fix failures per "When something fails" in `workflow.md`, then rerun: `cargo nextest run <test>` for one test, `cargo offrails verify` (fast) while fixing, `--full` once more at the end.
4. The gates no command proves, from `.offrails/guides/test-standards.md`:
   - **frontend coverage** in TS modes;
   - **accessibility** (§10): axe on every route in light and dark, desktop and mobile (apps with a UI);
   - with `$ARGUMENTS` containing `--deep`, also **mutation testing** (§5).
5. Walk the checklist under the commands, running each item's command, and check it against the diff (`git diff`, plus untracked files). Also check that the change added the tests its archetype requires (test-standards §2–4).
6. Regenerate `docs/test-report.md` (test-standards §8).
7. Report a pass/fail line per step. Never call it green while any step is red or skipped without saying so.
