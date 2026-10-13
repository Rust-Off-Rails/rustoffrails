---
name: doctor
description: Audit a Rust off Rails app against the framework's conventions (structure, stack, banned crates, portability, security, tests, docs drift) and report or fix violations. Use when the user asks to review, audit, lint, or "doctor" an Off Rails app, or after a human edited code by hand.
argument-hint: "[--fix]"
context: fork
---

# /offrails:doctor — convention audit

Only for Rust off Rails apps (a `.offrails/` directory exists). Mode: $ARGUMENTS (with `--fix`, fix the violations; otherwise only report them).

Read `.offrails/AGENTS.md`, `structure.md`, `stack.md` and `workflow.md`, plus the app's `AGENTS.md`. Then check each item and record a verdict (ok / violation with `file:line` / n/a):

**Structure** (`structure.md`)
- No top-level directories beyond the tree; nothing outside the app's `FRONTEND_MODE`.
- Naming table followed (tables, modules, handlers, routes, templates, jobs, migrations).
- Module rules: models import no controllers or views; views do no DB access; no `services/` directory.

**Stack** (`stack.md`)
- `Cargo.toml` / `web/package.json` contain no banned crates or packages. Any unlisted crate has a `docs/decisions.md` entry.
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
- Multi-tenant apps: the tenant isolation suite covers every tenant-scoped model.
- Public API: contract snapshots exist and none changed without a decision entry.
- No `sleep` in tests, no `#[ignore]` without a decision, and no assertions weakened in recent history (`git log -p tests/`).

**Specs ↔ code**
- Every `AC-…` ID in `product/features/*.md` is cited by a test (test-standards §1). List those that aren't.
- Env vars agree across `Config`, `.env.example` and the deploy config.
- Code that looks human-written (check `git log`) has its intent captured in a spec or decision.

Finish with `/offrails:check` (with `--fix`), or run the DoD commands read-only.

Report: a table of findings grouped by area (most severe first), then the top 3 fixes. With `--fix`, apply them in vertical slices, keep the tree green, and log the decisions.
