# Generators (`rails generate` → `cargo offrails generate`)

> **TL;DR**
> - Generators are one of two tested ways to start app code; [snippets](../template/snippets/README.md) are the other (`cargo offrails snippets`). Neither is mandatory: start from whichever fits, hand-write the rest. Once written, the code is the app's.
> - `cargo offrails g <kind> <name> [field:type…]` (`cargo offrails g --help` lists the kinds, field types and options) writes a feature's files *and* their wiring, formatted, and records them in `docs/generated.toml`.
> - Small files per feature, and one marked line per shared spot: a resource is `src/controllers/posts/` (a file per action pair), `src/models/post/` (entity, params, queries), `src/routes/posts.rs` (router, `paths::`, `ROUTES` rows), `templates/posts/`, `tests/app/posts/`; each line it adds to a shared file ends in `// posts` (`{# posts #}` in HTML, or the line above it in a list). Removing a feature is deleting its folders and those lines, which is what `cargo offrails destroy <kind> <name>` does (unchanged files only; it says what it kept).
> - `--dry-run` first when unsure (every file and anchor, nothing written). Re-running is a no-op; `--force` regenerates over files nobody changed. **Commit after each run**, before you edit: your changes are then a diff of their own.
> - Then the lines marked `// APP:`, the spec's acceptance criteria in the generated tests, and the next steps it printed (migrate, Tailwind, `cargo offrails verify --fast`).
> - Kernel needs (mail, jobs, sign-in, sockets) are **parts**, not generators: `cargo offrails parts`, `cargo offrails add <part>`.
>
> Read the rest for the kinds, the file layout, the field syntax, and what each generator decides for you.

## The commands

| Rails | Off Rails | Writes | Recipe (the why, what to customize) |
|-------|-----------|--------|--------|
| `rails new` | `offrails-gen new` (after the interview) | the app from `template/` | [new.md](new.md) |
| `rails g migration` | `cargo offrails g migration create_x\|add_y_to_x\|remove_y_from_x\|<any> [field:type…]` | `migration/src/m<stamp>_<name>.rs` (with its `down`), registered | [migration.md](migration.md) |
| `rails g model` | `cargo offrails g model Post title:string …` | migration + entity, `Params` → `validate()` → `New`, scoped queries + factory + model tests | [scaffold.md#model](scaffold.md#model) |
| `rails g scaffold` | `cargo offrails g resource posts title:string …` | the model + htmx CRUD (7 actions, pages, forms with errors, flash), routes, nav link, request tests, tenancy suites | [scaffold.md](scaffold.md) |
| `rails g controller` | `cargo offrails g controller reports index summary` (path params: `show:/blog/{slug}`) | handlers, page structs, templates, routes, request tests | [controller.md](controller.md) |
| (a static page) | `cargo offrails g page about [--nav] [--path /x]` (`--path /p/{slug}`) | route, handler, template, sitemap entry, test | [controller.md#pages](controller.md#pages) |
| (`rails g scaffold --api`) | `cargo offrails g api notes title:string …` | `/api/v1` JSON (tokens, scopes, OpenAPI, 201/204/404/422) + the API kernel on first use | [api.md](api.md) |
| `rails g job` | `cargo offrails g job NotifyPostPublished post_id:bigint [--queue q]` | `Worker`, its registration, a test | [job.md](job.md) |
| `rails g mailer` | `cargo offrails g mailer post_mailer published …` | pure mailer fns, HTML + text templates, previews, tests | [mailer.md](mailer.md) |
| `rails g channel` | `cargo offrails g channel post` | a realtime topic: authorization, broadcast, tests | [channel.md](channel.md) |
| `rails g test_unit:…` | `cargo offrails g test request\|model\|job\|unit <name>` | a registered test file | — |
| `rails destroy …` | `cargo offrails destroy <kind> <name>` | removes what that generation wrote, if unchanged | — |
| (copy a recipe) | `cargo offrails snippet add <name> [--as <model>]` / `snippet remove <name>` | a snippet's files and its marked lines | [snippets](../template/snippets/README.md) |
| `rails g authentication` | `cargo offrails add auth-core auth-magic-link` (a part) | accounts, sessions, sign-in | [authentication.md](authentication.md) |
| `rails app:update` | `cargo offrails update-part <part> --framework <newer>`, then re-vendor | parts merged 3-way, then the framework | [upgrade.md](upgrade.md) |

`job`, `mailer` and `channel` need their part (`jobs`, `mailer`, `realtime`); `api` and scoped resources need accounts (`auth-core` + a method). The generator says so, with the `add` command, instead of half-generating.

## The files a feature gets

Every generator writes small files under the feature's own folders, and touches a shared file only with one line per spot (two for a factory), marked with the feature's name. `cargo offrails g resource posts title:string body:text`:

```
migration/src/m<stamp>_create_posts.rs
src/models/post/mod.rs            entity, relations, label; pub use params::*, queries::*
src/models/post/params.rs         PostParams → validate() → NewPost, From<&Model>, unit tests
src/models/post/queries.rs        find_for, all_for, choices_for, create, update_for, destroy_for
src/controllers/posts/mod.rs      the modules and their re-exports (+ the form's <select> options)
src/controllers/posts/index.rs    GET /posts
src/controllers/posts/show.rs     GET /posts/{id}
src/controllers/posts/new.rs      GET /posts/new, POST /posts
src/controllers/posts/edit.rs     GET /posts/{id}/edit, POST /posts/{id}
src/controllers/posts/destroy.rs  POST /posts/{id}/delete
src/views/posts.rs                PostView + the page structs
src/routes/posts.rs               router(), paths::{posts, new_post, post, edit_post, delete_post}, ROUTES rows
templates/posts/{index,show,new,edit,_form}.html
tests/app/posts/{mod,read_test,write_test,access_test}.rs
tests/app/factories/post.rs       factories::post, post_params, post_with, reload_post
tests/app/models/post_test.rs     the queries, on both databases

and one marked line at each spot in a shared file:
  src/routes.rs           pub mod posts; // posts · app = app.merge(posts::router()); // posts
                          pub use super::posts::paths::*; // posts · posts::ROUTES, (under a `// posts` line)
  src/models/mod.rs       pub mod post; // posts · pub type Post = post::Model; // posts
  src/controllers/mod.rs, src/views/mod.rs, tests/app/main.rs, tests/app/models/mod.rs: pub mod / mod posts; // posts
  tests/app/factories.rs  // posts · mod post; · pub use post::*;
  migration/src/lib.rs    mod m…_create_posts; // posts · Box::new(m…::Migration), (under a `// posts` line)
  templates/shared/_nav.html  <a …>Posts</a> {# posts #}
  (scoped: tests/app/tenancy_test.rs at its `tenant …` anchors; public: the sitemap)
```

Before 0.7.3 the same command wrote one big file each (`src/controllers/posts.rs`, `src/models/post.rs`, `tests/app/posts_test.rs`) and inserted the routes, `paths::` helpers and `ROUTES` rows into `src/routes.rs` itself.

- **Routes per feature.** `ROUTES` is `src/routes.rs`'s own table (`APP_ROUTES`: the kernel's and the parts' rows) followed by each feature's (`TABLES`), concatenated at compile time, so it is still one `&[Route]` every test and tool iterates. `cargo run -- routes`, docs/routes.md and the security sweeps see every row.
- **Markers.** A one-line statement or declaration ends with `// <feature>`; a list element, a field, a match arm or a block gets a `// <feature>` line above it (rustfmt aligns a comment that follows a trailing one in a list). Declarations go in sorted, so rustfmt never moves a marked line. Search for `// posts` to see everything a feature put into shared files.
- **Path parameters** (`controller`, `page`): `show:/blog/{slug}`, `--path /people/{handle}`; `{id}` and `{…_id}` are `i64`, the rest `String`; the security and smoke sweeps fill any `{param}`.
- **Generations recorded before 0.7.3** (`resource`, `model`, `controller`, `page`, `api`; no `layout = "2"` in docs/generated.toml) can't be re-planned in the old shape: `destroy` and a re-run refuse, listing the files. Delete them and their lines by hand (or keep them: they're yours), then `cargo offrails destroy <kind> <name> --forget` drops the record.

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
- **Shape.** Exactly the guides: one model module per table (entity, params and validation, queries: [the files](#the-files-a-feature-gets)), handlers that extract → scope → model → render or redirect, 303 after a POST, 422 re-render with the input and the errors, `#[tracing::instrument(skip_all)]` on every action and model fn, no `unwrap`/`expect` outside tests, templates in the template's tokens with every `dark:` partner (`theme_test`).
- **Markup.** Every page a generator writes is built from the component kit ([views](../guides/views.md#components)): `ui::page_header`, `ui::table` with `ui::empty_state` for an empty list and `ui::pagination`, `ui::card`, `ui::confirm_button` for delete, `ui::field`/`textarea`/`select`/`checkbox` with their errors and `ui::form_actions`; the layout's notices show the flash. So it follows `brand.toml`, both themes and the kit's accessibility, and `offrails-check [classes]` finds nothing in it. The pages extend `layouts/application.html` (the top nav); `--layout sidebar|marketing|auth|docs` picks another ([views](../guides/views.md#layouts)). The markup lives in two files, `.offrails/tools/offrails-gen/src/generate/files/html/resource.html` and `pages.html` (one `@@ section` per piece, `__TOKENS__` filled in), in one place to change.
- **Wiring.** Inserted above the `samples:` anchors the parts use ([new.md#anchors](new.md#anchors)), plus generator anchors in the files generators write (`samples: columns`, `relations`, `related` in a model; `tenant …` in `tenancy_test.rs`; `page mods`, `page view mods`, `page test mods` in the shared `pages` modules; `api paths`, `api schemas`), each line marked with its owner. Part files without anchors (`src/jobs/mod.rs`, `src/channels/mod.rs`) are wired next to the lines they extend. A missing anchor is a refusal that names the file and the spot, and nothing is written. A `migration/src/lib.rs` that `sea-orm-cli` rewrote gets its anchors back.

## Rules

1. **Check `FRONTEND_MODE`.** `resource`, `controller` and `page` write htmx pages and refuse other modes (follow [scaffold.md](scaffold.md)'s mode branch by hand); `model`, `migration`, `api`, `job`, `mailer`, `channel` and `test` work in every mode.
2. **Spec first; then a snippet, a generator, or the guides; then customize.** Generated tests prove the generic contract (happy paths, validation, sign-in, tenancy, escaping, N+1); the spec's acceptance criteria are yours to add and cite (`/// AC-posts-1`).
3. **Commit after each generator run**, before you change anything: the generated code is then one commit and your edits another (review, revert and `destroy` stay simple).
4. **Never edit `docs/generated.toml`.** Edit the generated files freely: destroy then keeps them and says so.
5. **Finish green.** The next steps it prints end with `cargo offrails verify --fast`; the Definition of Done is still the bar.
