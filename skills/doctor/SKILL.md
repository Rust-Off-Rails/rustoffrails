---
name: doctor
description: Audit a Rust off Rails app against the framework's conventions (structure, stack, banned crates, portability, security, tests, docs drift) and report or fix violations. Use when the user asks to review, audit, lint, or "doctor" an Off Rails app, or after a human edited code by hand.
argument-hint: "[--fix]"
context: fork
---

# /offrails:doctor — convention audit

Only for Rust off Rails apps (a `.offrails/` directory exists). Mode: $ARGUMENTS (with `--fix`, fix the violations; otherwise only report them).

**Start with the script:** `sh .offrails/tools/offrails-check` (apps before 0.6.0: `sh ${CLAUDE_PLUGIN_ROOT}/tools/offrails-check`). Report its `FAIL` lines as violations verbatim; they are mechanical facts (header consistency, uncited ACs, missing Security-table tests, missing tenancy suites, unscoped lookups in handlers). Then read `.offrails/AGENTS.md`, `structure.md`, `stack.md` and `workflow.md`, plus the app's `AGENTS.md`, and check each remaining item and record a verdict (ok / violation with `file:line` / n/a):

**Structure** (`structure.md`)
- No top-level directories beyond the tree; nothing outside the app's `FRONTEND_MODE`.
- Naming table followed (tables, modules, handlers, routes, templates, jobs, migrations).
- Module rules: models import no controllers or views; views do no DB access; no `services/` directory.

**Stack** (`stack.md`)
- `Cargo.toml` / `web/package.json` contain no banned crates or packages (`cargo deny check bans` enforces the list mechanically via `deny.toml`; check direct dependencies by hand too). Any unlisted crate has a `docs/decisions.md` entry.
- `.offrails/` is unmodified (compare with `.offrails/VERSION` and the plugin copy if available).

**Data**
- No raw SQL outside `src/kernel/` or logged escape hatches (`guides/database.md`).
- Every migration has a working `down`. Foreign keys are indexed.
- The CI workflow runs tests on both SQLite and Postgres.

**HTTP & security** (`guides/routing.md`, `guides/security.md`)
- `ROUTES`, `paths::` and `routes.rs` agree. `docs/routes.md` is current (`cargo run -- routes`).
- Every mutating handler that needs auth takes `RequireUser` and calls a policy.
- CSRF, security headers, rate limit on login, and cookie flags are all present.

**Code hygiene** (Definition of Done checklist)
- No `unwrap()`/`expect()` outside tests and startup code. No `todo!()`. No `// TODO` without a linked open question.
- `web/src/types/` matches what ts-rs exports, and no hand-written TS duplicates a Rust type.

**Tests** (`guides/test-standards.md`)
- The app's archetypes (§2) are identified, and each one's required suites exist.
- `tests/app/security_test.rs` has the access matrix over `ROUTES`, and every `ROUTES` entry declares its access level.
- Apps with accounts: `tenancy_test.rs`'s `Tenant` holds a record of every tenant-owned model, and its `EXPECTED` entries each cite an AC that really makes that route shared (the script checks the files exist, not that they're complete).
- Public API: contract snapshots exist and none changed without a decision entry.
- No `sleep` in tests, no `#[ignore]` without a decision, and no assertions weakened in recent history (`git log -p tests/`).

**Accessibility** (`guides/test-standards.md` §10)
- An axe suite exists, covers every public route in both color schemes and both widths, and runs in CI as a required check.
- No global rule disables; every exception has a decision entry.
- Spot-check contrast of code blocks, terminal cards and focus rings in both themes.

**Specs ↔ code**
- Env vars agree across `Config`, `.env.example` and the deploy config.
- Code that looks human-written (check `git log`) has its intent captured in a spec or decision.

Finish with `/offrails:check` (with `--fix`), or run the DoD commands read-only.

Report: a table of findings grouped by area (most severe first), then the top 3 fixes. With `--fix`, apply them in vertical slices, keep the tree green, and log the decisions.
