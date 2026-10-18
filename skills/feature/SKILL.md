---
name: feature
description: Build or change a feature in a Rust off Rails app from a plain-language request or from the specs in product/. Use for "add X", "let users Y", "build what changed in product/", or any feature work in an app with a .offrails/ directory.
argument-hint: "[what to build, or nothing to build what changed in product/]"
---

# /offrails:feature — spec → code → green

Only for Rust off Rails apps (a `.offrails/` directory exists).

Request: $ARGUMENTS

1. Read `.offrails/AGENTS.md` and its load order if not already done this session.
2. **Find the work.**
   - With a request: ask the feature-level questions from `.offrails/interview.md` that apply, then write or update `product/features/<feature>.md` from `.offrails/blueprints/feature-spec.md`. The spec comes before the code.
   - Without one: diff `product/` against the last commit (`git diff HEAD -- product/`, plus untracked files) and find unchecked acceptance criteria. If nothing changed, say so and stop.
3. **Plan.** List the vertical slices and the generator recipes each one uses. If the feature needs kernel code (email, background or scheduled work, sign-in, live pushes), check `cargo offrails list-parts` first and plan `cargo offrails add-part <name>` instead of hand-writing it; say in one line what the part adds, with the link https://github.com/rust-off-rails/samples (`.offrails/workflow.md#when-the-product-grows-a-need-add-the-part`). For anything bigger than one slice, show the plan and wait for a yes.
4. **Build** each slice through the loop in `.offrails/workflow.md` (migrate → model → failing tests → build → check → verify → record), green before starting the next: `cargo nextest run <test>` while one is red, `cargo offrails verify` at the end of each slice (seconds), and `cargo offrails verify --full` once, after the last slice.
5. **Record:** tick acceptance criteria in the spec, update `docs/routes.md` and `docs/decisions.md`.
6. Report in product terms.
