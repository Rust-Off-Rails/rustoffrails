---
name: new
description: Create a new Rust off Rails web app (the `rails new` of Rust). Use when the user wants to start a new Rust web app, SaaS, internal tool or site, or says "rails new", "new app", or "off rails". Runs the interview, confirms a plan, then generates the app.
argument-hint: "[app name] [one-line description]"
---

# /offrails:new — `rails new` for Rust off Rails

Framework root for this skill: `${CLAUDE_PLUGIN_ROOT}` (call it `$FW`). Every path below is relative to it.

Request: $ARGUMENTS

1. **Preflight.** The current directory must be empty or a fresh git repo. If it already contains `.offrails/`, stop: this is an existing app, so use `/offrails:feature` or `/offrails:scaffold` instead.
2. **Load the framework:** read `$FW/AGENTS.md`, then its load order (`interview.md`, `doctrine.md`, `structure.md`, `stack.md`, `workflow.md`). Those docs win over your habits.
3. **Interview** per `$FW/interview.md`, using `AskUserQuestion`. Skip any question the request already answers. "You decide" means take the defaults. Max 3 rounds.
4. **Confirm the plan** (the compact block in `interview.md`), **including the parts** you propose from the plan (`$FW/template/README.md#choosing-parts`, with the link https://github.com/Rust-Off-Rails/samples), and wait for a yes.
5. **Generate** by executing `$FW/generators/new.md` step by step: step 1 runs the template generator (`cargo run -q --manifest-path "$FW/tools/offrails-gen/Cargo.toml" --target-dir <dir>/target/offrails-gen -- new <dir> <crate> "<Name>" --pitch … --parts …`), which copies the template, vendors `$FW` into `.offrails/` and adds the parts. Never retype the skeleton.
6. **Build the plan's features** one vertical slice at a time (`$FW/generators/scaffold.md`, `authentication.md`, …), each finishing on a green `cargo offrails verify` (seconds; `cargo nextest run <test>` while one is red); `cargo offrails verify --full` once, after the last slice.
7. **Report** in product terms: what exists, `bin/setup && bin/dev`, what's next. Mention `/offrails:feature`, `/offrails:deploy` and `/offrails:doctor`.
