---
name: generate
description: Run a Rust off Rails generator other than scaffold, like `rails generate`: model, resource, migration, job, mailer, channel, authentication, task. Use when the user asks to add a background job, an email, live updates, login, a migration, or a model to a Rust off Rails app.
argument-hint: "<model|resource|migration|job|mailer|channel|authentication|task> <name> [args]"
---

# /offrails:generate

Only for Rust off Rails apps (a `.offrails/` directory exists).

Request: $ARGUMENTS

1. Read `.offrails/AGENTS.md` and its load order if not already done this session.
2. Pick the generator(s) from the table in `.offrails/generators/README.md` (`cargo offrails g --help`), or a snippet when the request is a recipe (a feed, slugs, Markdown, a pager: `cargo offrails snippets`). If the request is in plain English ("email users a weekly digest"), map it to generators yourself; it may need several (`mailer` + `job`). A kernel need (mail, a queue, sign-in, sockets) is a part: `cargo offrails parts`, `cargo offrails add <part>`.
3. Write or update the matching `product/features/*.md` spec first.
4. Run each generator (`cargo offrails g <kind> … --dry-run` first when unsure, then without) or `cargo offrails snippet add <name>`, commit, then follow the recipe's "what to customize" and the next steps it printed: the `// APP:` lines, the spec's tests, migrate. Then run `cargo offrails verify` (`--full` when the work is done; `.offrails/workflow.md#definition-of-done`).
