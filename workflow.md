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
8. VERIFY   Definition of Done, including running the app
9. RECORD   docs/routes.md, docs/decisions.md, spec checkboxes
```

Work in **vertical slices**: one resource or behavior end-to-end, green, then the next. Never leave the tree not compiling between slices.

## Definition of Done

All must pass, in this order. Gates and thresholds are defined in [`guides/test-standards.md`](guides/test-standards.md).

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run                                   # SQLite; also re-exports ts-rs types
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run   # Postgres (docker compose up -d postgres): required locally when reachable; CI always runs it

# acceptance-criteria traceability: must print nothing (test-standards.md §1)
comm -23 <(grep -rhoE 'AC-[a-z0-9_-]+-[0-9]+' product/ | sort -u) \
         <(grep -rhoE 'AC-[a-z0-9_-]+-[0-9]+' tests/ src/ web/src/ 2>/dev/null | sort -u)

# coverage gate (test-standards.md §5): ≥ 80% lines overall, ≥ 90% for src/models and src/policies
cargo llvm-cov nextest --summary-only --ignore-filename-regex 'src/main\.rs$' --fail-under-lines 80

# htmx mode
bin/tailwindcss -i assets/app.css -o static/app.css --minify

# inertia / react / islands
pnpm --dir web exec biome check --write .
pnpm --dir web typecheck                           # React: `tsc -b` (never `tsc --noEmit`); Svelte: svelte-check
pnpm --dir web test --run
pnpm --dir web build
```

Plus:

- [ ] Every acceptance criterion (`AC-<feature>-<n>`) has a test citing its ID, and is ticked only when that test passes on both databases.
- [ ] New migrations roll back cleanly (`db rollback` then `db migrate`).
- [ ] No `unwrap()`/`expect()` outside tests and startup code in `main.rs`/`config.rs`.
- [ ] No raw SQL outside `src/kernel/` and logged escape hatches.
- [ ] (inertia/spa) Nothing under `web/src/types/` was edited by hand, and no TS type duplicates a Rust type.
- [ ] No `todo!()`, `unimplemented!()`, or `// TODO` without a linked spec "Open question".
- [ ] New routes appear in `docs/routes.md` (`cargo run -- routes`).
- [ ] New env vars appear in `.env.example`, in `Config`, and in the deploy config.
- [ ] Decisions not covered by `.offrails/` are logged in `docs/decisions.md`.
- [ ] If the change is user-visible, you ran the app and exercised it, not just the tests. **UI-affecting changes need a real-browser smoke of the changed flow** ([test-standards §9](guides/test-standards.md#9-real-browser-smoke)): styling applied, Back/Forward, console free of errors and CSP violations. curl alone isn't enough. Emails: check `/dev/mailbox/latest.json`. Jobs: `app jobs list --state discarded` is empty ([dev-tools](guides/dev-tools.md)).

## When something fails

- **Compile error in a crate API:** check the version in `Cargo.lock`, read that version's docs, and fix the call. Don't swap crates.
- **"no such table" / "relation does not exist" at runtime:** run `cargo run -- db migrate`.
- **Passes on SQLite, fails on Postgres (or the reverse):** you broke a portability rule in `guides/database.md`. Fix the code, not the test.
- **The same error three times:** stop, reread the relevant guide, and reduce the change to the smallest failing piece.
- **Genuinely blocked** (needs a credential, an external account, or a product decision): stop and tell the human exactly what you need and why. Don't stub around it silently.

## Human code edits

The rule is zero handwritten code, but humans will occasionally paste a fix. When you notice one (via `git log`/`git blame` or because they say so):

1. Don't revert it.
2. Make it conform to these conventions if it doesn't.
3. Capture the *intent* in the relevant spec or `docs/decisions.md`, so a future regeneration keeps it.

## Commits

One commit per vertical slice. Message format: `<resource>: <what changed>` plus a body listing the spec file(s) implemented. Commit only when the human has asked you to manage commits. The one exception is the baseline commit at the end of `generators/new.md`.

## Talking to the human

Report in product terms ("Books can now be reviewed; owners get an email"), then list files changed, then any assumptions made. Never tell the human to edit code. Tell them which spec to edit, or just to tell you.
