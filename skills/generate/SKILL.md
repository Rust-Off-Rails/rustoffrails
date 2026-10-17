---
name: generate
description: Run a Rust off Rails generator other than scaffold, like `rails generate`: model, resource, migration, job, mailer, channel, authentication, task. Use when the user asks to add a background job, an email, live updates, login, a migration, or a model to a Rust off Rails app.
argument-hint: "<model|resource|migration|job|mailer|channel|authentication|task> <name> [args]"
---

# /offrails:generate

Only for Rust off Rails apps (a `.offrails/` directory exists).

Request: $ARGUMENTS

1. Read `.offrails/AGENTS.md` and its load order if not already done this session.
2. Pick the recipe from the table in `.offrails/generators/README.md`. If the request is in plain English ("email users a weekly digest"), map it to recipes yourself; it may need several (job + mailer).
3. Write or update the matching `product/features/*.md` spec first.
4. Execute the recipe(s) in full, then run `cargo offrails verify` (`--full` when the work is done; `.offrails/workflow.md#definition-of-done`).
