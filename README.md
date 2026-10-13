<p align="center"><img src="brand/lockup-on-light.svg" alt="Rust off Rails" height="72"></p>

# Rust off Rails

> The opinionated web stack for the AI era.

**Rust off Rails** is a web framework that ships **zero lines of framework code**. It's a set of Markdown files that tell AI coding agents how to build a full-stack Rust web app: one blessed stack, one directory layout, one way to do each thing.

The convention lives in the prose, and the agent generates the code.

## Standing on giants

We take the best ideas of two great ecosystems and bring them together in one stack.

- **From Ruby on Rails:**
  - convention over configuration;
  - generators (`new`, `scaffold`), Active Record-style models and migrations;
  - Rails 8 authentication;
  - the Solid trifecta: jobs, cache and pub/sub in the database, no Redis;
  - Turbo 8-style page-refresh broadcasts;
  - mailer previews, `bin/rails` tasks, and Kamal-ready Docker with `/up`.
- **From Elixir and Phoenix:**
  - **Oban** persistent jobs and the **Oban Web** dashboard;
  - **Swoosh**'s local mailbox;
  - **phx.gen.auth** magic links and the "check your mailbox" hint;
  - **LiveDashboard**-style dev tools;
  - **Channels, Presence and PubSub**;
  - multi-node clustering and "let it crash" supervision;
  - Ecto's explicit data layer (no callbacks);
  - the pending-migrations page.
- **And also:** htmx, Inertia.js, Tailwind, shadcn-style owned components, and end-to-end types from Rust to TypeScript.

[`RAILS_MAP.md`](RAILS_MAP.md) maps every Rails concept to its Off Rails equivalent, plus a [Borrowed from Phoenix / Elixir](RAILS_MAP.md#borrowed-from-phoenix--elixir) section.

## The deal

| Rails gave you…                     | Rust off Rails gives your agent…                     |
|------------------------------------|------------------------------------------------------|
| A gem with 300k lines of Ruby      | ~30 Markdown files                                   |
| `rails generate scaffold`          | `generators/scaffold.md`: a recipe the agent runs    |
| Convention over configuration      | Convention over *deliberation*: the agent never debates which crate to use |
| "Optimized for programmer happiness" | Optimized for agent correctness: compiler-checked everything |
| Magic                              | No magic. Explicit code is cheap when you don't type it. |

## Rules of the framework

1. **All code is written by AI. No exceptions.** Humans write intent: specs in Markdown, interview answers and feedback. Agents write every line of Rust, SQL, HTML, CSS, TypeScript, config and scripts. If a human edits code anyway, the agent treats the edit as a spec change, records it, and owns that code from then on.
2. **Strong opinions, aimed at agents.** Every decision an agent would otherwise improvise (crate choice, file location, naming, error handling, testing) is already made in these docs.
3. **Everything Rails has, mapped.** Active Record, Action Controller, Action View, Active Job, Action Mailer, Action Cable, Active Storage, Solid Cache/Queue/Cable, generators, `bin/rails`, credentials, Kamal. See [`RAILS_MAP.md`](RAILS_MAP.md).

## Using it

### With Claude Code (recommended): install the plugin

```
/plugin marketplace add Rust-Off-Rails/rustoffrails      # or a local path: /plugin marketplace add ~/dev/rustoffrails
/plugin install offrails@offrails
```

Then, in an empty directory:

```
/offrails:new bookshelf: people track the books they've read and review them
```

The agent interviews you first (accounts? who owns data? htmx or React? where will it run?), using recommended defaults so you can just say "you decide". It confirms a plan, then builds. The skills also trigger on plain requests ("make me a new Rust web app…").

| Skill | Rails equivalent | What it does |
|-------|------------------|--------------|
| `/offrails:new [name] [pitch]` | `rails new` | Interview → plan → app skeleton → planned features, green |
| `/offrails:feature [request]` | — | Spec first, then code. With no argument: build what changed in `product/` |
| `/offrails:scaffold Book title:string …` | `rails g scaffold` | Full resource in the app's frontend mode |
| `/offrails:generate job\|mailer\|channel\|authentication\|model\|migration\|task …` | `rails g …` | Any other generator recipe |
| `/offrails:check` | `bin/rails test` + CI | Definition of Done: fmt, clippy, tests on SQLite **and** Postgres, frontend checks |
| `/offrails:doctor [--fix]` | `rubocop` + review | Audits the app against every convention in these docs |
| `/offrails:deploy [fly\|kamal\|railway\|render]` | `kamal setup` | Deploy config, verified image; confirms before anything outward-facing |
| `/offrails:upgrade` | `rails app:update` | Moves the app's vendored `.offrails/` to the plugin's version and migrates conventions |

The plugin also installs hooks. They only act inside Off Rails apps (directories with `.offrails/`):

- **Edit guard:** blocks edits to `.offrails/` (read-only) and `web/src/types/` (generated by ts-rs).
- **Convention check:** after each edit, flags raw SQL outside the kernel, banned crates or packages, and `/:id` routes.
- **Stop gate:** the agent can't end a turn while `cargo fmt --check` or `cargo check` fails.
- **Session start:** orients the agent and notices when the app's framework version is behind the plugin's.

Each app still vendors the docs into `.offrails/`, pinned to a version, so the app keeps working with any agent, with or without the plugin.

### With Codex, Cursor, or any agent that reads `AGENTS.md`

```
Read /path/to/rustoffrails/AGENTS.md and make me a new app called "bookshelf".
```

Generated apps carry `AGENTS.md` and `.offrails/`, so later sessions need no setup.

### Then

Describe features in plain language, or write them as specs in `product/`:

```
Scaffold Book with title:string, author:string, published_on:date, rating:integer.
Users must be logged in to create books. Send the owner an email when a book gets a review.
```

## Repository layout

```
AGENTS.md          entry point for agents: read this first
interview.md       the questions agents ask before building
doctrine.md        the philosophy, rewritten for machines
RAILS_MAP.md       every Rails concept and its Off Rails equivalent
stack.md           the blessed crates (the "Gemfile")
structure.md       directory layout and naming conventions
cli.md             the app binary and the bin/rails equivalents
workflow.md        how an agent works: spec → code → green → done
guides/            one guide per framework component
generators/        recipes that replace `rails generate`
blueprints/        files copied into every new app (AGENTS.md, spec template…)
.claude-plugin/    Claude Code plugin + marketplace manifests
skills/            /offrails:* skills (thin entry points into the docs above)
hooks/             convention guards (no-ops outside Off Rails apps)
evals/             `claude plugin eval` cases: does the framework steer agents right?
tools/             optional helper scripts vendored with the docs (browser-smoke for the kernel pages)
VERSION, CHANGELOG.md   framework version; apps record theirs in .offrails/VERSION
```

## Developing the framework

```sh
claude --plugin-dir .                       # load this checkout instead of the installed plugin
claude plugin validate . --strict           # manifests, skills, hooks
claude plugin eval . --runs 1               # run the eval cases
```

Every doc change goes under `## Unreleased` in `CHANGELOG.md` the moment it's made, with **App migration** notes written for an agent. Apps detect drift by content fingerprint (`hooks/fingerprint.sh`), not version alone, so even unreleased edits show up as an upgrade.

Releasing:
1. Rename `Unreleased` to the new version.
2. Bump `VERSION` and `.claude-plugin/plugin.json` together.
3. Run `claude plugin tag --push`.

Installed plugins update only when the version changes.

## The stack in one breath

Axum on hyper (the binary *is* the web server: HTTP/1.1 + HTTP/2, no app server or nginx needed) · Tokio · SeaORM (**SQLite locally, Postgres in production, same code**) · Tailwind · a frontend mode chosen at birth:

- **`htmx`** (default): Askama (compile-time checked templates) + htmx. No Node. Optional React or Svelte islands.
- **`inertia`**: React or **Svelte** pages with Rails-style server routing, via a generated Inertia adapter. The default when you want a JS UI.
- **`spa`**: a Vite SPA (React Router or SvelteKit in SPA mode) + a JSON API.
- **`api`**: no UI, for mobile backends and public APIs (in progress).

`UI_LIBRARY: react | svelte` picks the JS library. Props and API types are generated from Rust by `ts-rs`, so the type chain runs from the database to the browser.

Plus a database-backed job queue, cache and pub/sub (the "Solid trifecta") · lettre · object_store · tracing · one binary, one Docker image.

Yes, the acronym is still RoR. That's on purpose, and it's a thank-you.
