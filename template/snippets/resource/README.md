# Snippet: resource

> **TL;DR**
> - htmx CRUD for one model: list, show, new/create, edit/update, delete, for `notes` (`title`, `body`), public (no accounts needed).
> - It is the code `cargo offrails generate resource notes title:string body:text --public` writes, as plain files: read it, copy it, rename it. A test in offrails-gen keeps the two the same.
> - `cargo offrails snippet add resource --as article` copies it as `articles` (one-word names; for others, run the generator).

## When to use it

- You want to see, or start from, exactly what a scaffolded resource is, file by file, before deciding whether to run the generator.
- A small public CRUD (an internal tool, a demo, a list nobody owns). Records that belong to users want the generator with accounts installed (`find_for`, tenancy tests); the generator writes those, this snippet doesn't.
- Not for: a JSON API (`g api`), pages without a table (`g controller`, `g page`).

## Files

| Path | What it is |
|------|------------|
| `migration/src/m20261018_000100_create_notes.rs` | the table: `id` (64-bit), `title`, `body`, timestamps |
| `src/models/note/mod.rs` | the entity, its relations, its label; re-exports the two below |
| `src/models/note/params.rs` | `NoteParams` (the form, all strings) → `validate()` → `NewNote`, with unit tests |
| `src/models/note/queries.rs` | `find`, `all` (paged), `choices`, `create`, `update`, `destroy` |
| `src/controllers/notes/{mod,index,show,new,edit,destroy}.rs` | one file per action pair |
| `src/views/notes.rs` | `NoteView` (display strings) and the page structs |
| `src/routes/notes.rs` | the router, `paths::notes()`, `paths::note(id)`…, the `ROUTES` rows |
| `templates/notes/{index,show,new,edit,_form}.html` | the pages, in the template's tokens (every `dark:` partner) |
| `tests/app/notes/{mod,read_test,write_test,model_test}.rs` | request tests (happy paths, 422, escaping, N+1, a huge `?page`) and the queries on both databases |
| `tests/app/factories/note.rs` | `factories::note(db)`, `note_params`, `note_with`, `reload_note` |

## Wiring

One line at each anchor, marked `// notes` (`{# notes #}` in the nav):

- `migration/src/lib.rs` at `samples: migration mods` and `samples: migrations`
- `src/models/mod.rs` at `samples: model mods` (`pub mod note;`) and `samples: model aliases` (`pub type Note`)
- `src/controllers/mod.rs` at `samples: controller mods`, `src/views/mod.rs` at `samples: view mods`
- `src/routes.rs` at `samples: route mods`, `samples: routes`, `samples: paths`, `samples: route tables`
- `templates/shared/_nav.html` at `samples: nav links`, `src/controllers/meta.rs` at `samples: sitemap paths`
- `tests/app/factories.rs` at `samples: factories` (two lines), `tests/app/main.rs` at `samples: test mods`

## After adding

1. `cargo run -q -- db migrate`, then prove `down()`: `cargo run -q -- db rollback && cargo run -q -- db migrate`.
2. `bin/tailwindcss -i assets/app.css -o static/app.css --minify` (the pages' classes; `bin/dev` does it live).
3. Write `product/features/notes.md` and cite its AC ids on the tests; then `cargo offrails verify`.

## Adapting it

- Every `// APP:` line is a decision: what a note is, the validation limits (`params.rs`), the label, what each page shows.
- More fields: a migration (`cargo offrails g migration add_x_to_notes x:string`), then the entity, `NoteParams`, `validate()`, the form and the factory. The generator does all of that at once for a new resource.
- SECURITY: public means anyone can create, change and delete. Before real data, add accounts and scope it (`find_for`), or regenerate it with the generator.

## Remove it

`cargo offrails snippet remove resource` (with `--as <model>` if you added it so) deletes the files you didn't change and the marked lines; it lists what it kept. By hand: delete the paths above and every line marked `// notes`. Roll the migration back first if it ran (`cargo run -q -- db rollback`).
