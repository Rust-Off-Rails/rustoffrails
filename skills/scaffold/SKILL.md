---
name: scaffold
description: Generate a full resource (migration, model, policy, controller, routes, views or pages, tests) in a Rust off Rails app, like `rails g scaffold`. Use when the user asks to scaffold or add a CRUD resource with fields, e.g. "scaffold Book title:string rating:integer".
argument-hint: "<Model> field:type[?|!] …"
---

# /offrails:scaffold

Only for Rust off Rails apps (a `.offrails/` directory exists). If there isn't one, suggest `/offrails:new` and stop.

Request: $ARGUMENTS

1. If not already done this session, read `.offrails/AGENTS.md` and its load order, and note `FRONTEND_MODE` from the app's `AGENTS.md`.
2. Ask the feature-level questions from `.offrails/interview.md` that the request leaves open (ownership, deletion, who can see it), and only those.
3. `htmx` mode: run `cargo offrails g resource <plural> field:type…` (`--dry-run` first; field syntax: `.offrails/generators/README.md#fields`; for a one-word public CRUD to read first, `cargo offrails snippet show resource`), commit, then do what it printed and what `.offrails/generators/scaffold.md` leaves to you (spec ACs into the tests, the `// APP:` lines, seeds, the browser smoke). Other modes: `cargo offrails g model …` (and `g api …` for a public API), then the rest of `scaffold.md` for the app's `FRONTEND_MODE` by hand.
4. Finish with `cargo offrails verify` (`--full` if this is the last slice; `.offrails/workflow.md#definition-of-done`), then report per "Talking to the human".
