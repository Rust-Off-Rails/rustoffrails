# Generators (`rails generate` → `cargo offrails generate`)

> **TL;DR**
> - **Run the generator before hand-writing anything it covers:** `cargo offrails g <kind> <name> [field:type…]` (`cargo offrails g --help` lists the kinds, field types and options). It writes the files *and* the wiring (mod lines, router, `paths::`, `ROUTES`, docs/routes.md, factories, test registration), formatted, and records them in `docs/generated.toml`.
> - `--dry-run` first when unsure: it prints every file and anchor and writes nothing. Re-running is a no-op; `--force` regenerates over files nobody changed; `cargo offrails destroy <kind> <name>` undoes it (unchanged files only, and it says what it kept).
> - Then customize the lines marked `// APP:`, write the spec's acceptance criteria into the generated tests, and run the next steps it printed (migrate, Tailwind, `cargo offrails verify --fast`).
> - Kernel needs (mail, jobs, sign-in, sockets) are **parts**, not generators: `cargo offrails parts`, `cargo offrails add <part>`, `cargo offrails update-part <part> --framework <newer>`.
>
> Read the rest for the kinds, the field syntax, and what each generator decides for you.

## The commands

| Rails | Off Rails | Writes | Recipe (the why, what to customize) |
|-------|-----------|--------|--------|
| `rails new` | `offrails-gen new` (after the interview) | the app from `template/` | [new.md](new.md) |
| `rails g migration` | `cargo offrails g migration create_x\|add_y_to_x\|remove_y_from_x\|<any> [field:type…]` | `migration/src/m<stamp>_<name>.rs` (with its `down`), registered | [migration.md](migration.md) |
| `rails g model` | `cargo offrails g model Post title:string …` | migration + entity, `Params` → `validate()` → `New`, scoped queries + factory + model tests | [scaffold.md#model](scaffold.md#model) |
| `rails g scaffold` | `cargo offrails g resource posts title:string …` | the model + htmx CRUD (7 actions, pages, forms with errors, flash), routes, nav link, request tests, tenancy suites | [scaffold.md](scaffold.md) |
| `rails g controller` | `cargo offrails g controller reports index summary` | handlers, page structs, templates, routes, request tests | [controller.md](controller.md) |
| (a static page) | `cargo offrails g page about [--nav] [--path /x]` | route, handler, template, sitemap entry, test | [controller.md#pages](controller.md#pages) |
| (`rails g scaffold --api`) | `cargo offrails g api notes title:string …` | `/api/v1` JSON (tokens, scopes, OpenAPI, 201/204/404/422) + the API kernel on first use | [api.md](api.md) |
| `rails g job` | `cargo offrails g job NotifyPostPublished post_id:bigint [--queue q]` | `Worker`, its registration, a test | [job.md](job.md) |
| `rails g mailer` | `cargo offrails g mailer post_mailer published …` | pure mailer fns, HTML + text templates, previews, tests | [mailer.md](mailer.md) |
| `rails g channel` | `cargo offrails g channel post` | a realtime topic: authorization, broadcast, tests | [channel.md](channel.md) |
| `rails g test_unit:…` | `cargo offrails g test request\|model\|job\|unit <name>` | a registered test file | — |
| `rails destroy …` | `cargo offrails destroy <kind> <name>` | removes what that generation wrote, if unchanged | — |
| `rails g authentication` | `cargo offrails add auth-core auth-magic-link` (a part) | accounts, sessions, sign-in | [authentication.md](authentication.md) |
| `rails app:update` | `cargo offrails update-part <part> --framework <newer>`, then re-vendor | parts merged 3-way, then the framework | [upgrade.md](upgrade.md) |

`job`, `mailer` and `channel` need their part (`jobs`, `mailer`, `realtime`); `api` and scoped resources need accounts (`auth-core` + a method). The generator says so, with the `add` command, instead of half-generating.

## Fields

`name:type`, then any of: `?` or `:optional` (nullable; zsh globs an unquoted `?`, so prefer `:optional` or quote the argument), `:unique` (`!` works too), `:index`.

| Type | Column | Rust | Form | Validation | Factory |
|------|--------|------|------|------------|---------|
| `string` | `string` | `String` | text, `maxlength=255` | can't be blank, ≤ 255 | `"Title {n}"` (emails, slugs, urls look like one) |
| `text` | `text` | `String` (`Text`) | textarea | can't be blank, ≤ 100 000 | a sentence |
| `integer` / `bigint` | `integer` / `big_integer` | `i32` / `i64` | number | is not a number | `42` |
| `boolean` | `boolean` | `bool` | checkbox (`?`: Yes/No/Not set) | — | `true` |
| `decimal{p.s}` | `decimal_len(p, s)` | `Decimal` (adds `rust_decimal` + sea-orm's feature) | text, `inputmode=decimal` | at most `s` places, `p − s` digits | `12.55` |
| `money` | `big_integer` `<name>_cents` | `i64` cents | text `0.00` | is not an amount (two decimals) | `19.99` |
| `datetime` | `timestamp_with_time_zone` | `DateTimeUtc` | `datetime-local` (UTC; `// APP:` for the reader's zone) | is not a date and time | `2026-01-15T09:30` |
| `date` | `date` | `Date` | `date` | is not a date | `2026-01-15` |
| `uuid` | `uuid` | `Uuid` | text (mono) | is not a UUID | a new v4 |
| `json` | `json` | `Json` | textarea (mono) | is not valid JSON | `{"key": "value"}` |
| `references{model}` | `big_integer` `<name>_id` + FK + index | `i64` + `belongs_to`, and `has_many` on a generated parent | `<select>` of the actor's records | must exist (and be the actor's) | the parent's factory |

`decimal{8,3}` works quoted; unquoted, a shell brace-expands it, so write `decimal{8.3}`. Ids are always 64-bit (`big_pk_auto`, `big_integer`: `offrails-check [ids]`). A `references{users}` field on a scoped model is its owner (set from the session, never from the form). `:unique` on a scoped model is unique **per owner**, so one user can't probe another's values.

## What the generators decide (so you don't)

- **Scope.** With accounts installed, records belong to the signed-in user: `find_for(db, &user, id)`, `all_for`, `update_for`, `destroy_for`; raw `find` is `pub(in crate::models)`; another user's id is 404; routes are `Access::User`; `tests/app/guard_test.rs` and `tenancy_test.rs` are written (or extended at their anchors). Without accounts the generator refuses until you add them or pass `--public` (anyone may read and write; the next steps say so).
- **Shape.** Exactly the guides: one model file per table (entity, params, validation, queries), handlers that extract → scope → model → render or redirect, 303 after a POST, 422 re-render with the input and the errors, `#[tracing::instrument(skip_all)]` on every action and model fn, no `unwrap`/`expect` outside tests, templates in the template's tokens with every `dark:` partner (`theme_test`).
- **Wiring.** Inserted above the `samples:` anchors the parts use ([new.md#anchors](new.md#anchors)), plus generator anchors in the files generators write (`samples: columns`, `relations`, `related` in a model; `tenant …` in `tenancy_test.rs`; `page …`; `api paths`, `api schemas`). Part files without anchors (`src/jobs/mod.rs`, `src/channels/mod.rs`) are wired next to the lines they extend. A missing anchor is a refusal that names the file and the spot, and nothing is written. A `migration/src/lib.rs` that `sea-orm-cli` rewrote gets its anchors back.

## Rules

1. **Check `FRONTEND_MODE`.** `resource`, `controller` and `page` write htmx pages and refuse other modes (follow [scaffold.md](scaffold.md)'s mode branch by hand); `model`, `migration`, `api`, `job`, `mailer`, `channel` and `test` work in every mode.
2. **Spec first, then generate, then customize.** Generated tests prove the generic contract (happy paths, validation, sign-in, tenancy, escaping, N+1); the spec's acceptance criteria are yours to add and cite (`/// AC-posts-1`).
3. **Never edit `docs/generated.toml`.** Edit the generated files freely: destroy then keeps them and says so.
4. **Finish green.** The next steps it prints end with `cargo offrails verify --fast`; the Definition of Done is still the bar.
