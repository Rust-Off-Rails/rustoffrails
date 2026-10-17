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
3. Execute `.offrails/generators/scaffold.md` exactly, following only the app's `FRONTEND_MODE` branch.
4. Finish with `cargo offrails verify` (`--full` if this is the last slice; `.offrails/workflow.md#definition-of-done`), then report per "Talking to the human".
