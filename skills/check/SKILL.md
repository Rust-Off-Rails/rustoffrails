---
name: check
description: Run the Rust off Rails Definition of Done (fmt, clippy, tests on SQLite and Postgres, frontend checks, checklist) and fix what fails. Use before saying work is done, or when the user asks "is it green?", "run the tests", or "check everything".
argument-hint: "[--deep]"
---

# /offrails:check — the Definition of Done

Only for Rust off Rails apps (a `.offrails/` directory exists).

1. Read the app's `AGENTS.md` for `FRONTEND_MODE` and `ISLANDS`.
2. Run every command in `.offrails/workflow.md#definition-of-done` for that mode, in order. For Postgres: use `TEST_DATABASE_URL` if set. Otherwise, if Docker is available, run `docker compose up -d postgres` and use the URL from the app's `AGENTS.md`. If neither works, say that Postgres was skipped. Never claim it passed.
3. Fix failures per "When something fails" in `workflow.md`, then rerun from the first failing step.
4. Run the gates from `.offrails/guides/test-standards.md`:
   - **traceability** (§1): every `AC-…` ID in `product/` is cited by a test;
   - **coverage** (§5): `cargo llvm-cov nextest` thresholds;
   - **frontend coverage** in TS modes;
   - with `$ARGUMENTS` containing `--deep`, also **mutation testing** (§5).
5. Walk the checklist under the commands and check each item against the diff (`git diff`, plus untracked files). Also check that the change added the tests its archetype requires (test-standards §2–4).
6. Regenerate `docs/test-report.md` (test-standards §8).
7. Report a pass/fail line per step. Never call it green while any step is red or skipped without saying so.
