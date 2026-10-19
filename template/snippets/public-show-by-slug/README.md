# Snippet: public-show-by-slug

> **TL;DR**
> - `GET /read/notes/{slug}`: a public page for one record, found by a slug in the path (`/read/notes/hello-world`), next to the resource's own `/notes/{id}` pages.
> - A route with a path parameter, end to end: the routes file (`/read/notes/{slug}`, `paths::note_page(slug)`), the handler (`Path(slug): Path<String>`), the query, the page, tests (found, 404, a taken slug gets `-2`, markup escaped).
> - Builds on the `resource` snippet (notes) and the `slug` snippet; adds a nullable, unique `notes.slug` column. Add all three with the same `--as` (`--as article` → `/read/articles/{slug}`).
> - **Not for a generated, owner-scoped resource** (`g resource posts …` in an app with accounts): `snippet add` refuses without the resource snippet, and its code calls the public notes' `note::find` and `NoteView`, which a scoped model keeps private (`find_for(db, &user, id)`). There, write it from this one as the worked example: `cargo offrails g controller blog show:/blog/{slug} --public`, a `slug` column (the `slug` snippet's `unique`), and a public query that says what is public (`published_at <= now`) marked `// unscoped:` at its call site.

## When to use it

- Public pages with readable URLs: a blog post, a product, a profile, while the CRUD stays where it is (and can move behind sign-in).
- Not for: private records (a slug is guessable; scope the lookup to the user and use the generated `/notes/{id}`), or pages without a row (`cargo offrails g controller blog show --path /blog/{slug}` writes a page with a path param and no query).

## Files

| Path | What it is |
|------|------------|
| `migration/src/m20261018_000200_add_slug_to_notes.rs` | `notes.slug`, nullable, with the unique index `notes_slug_key` |
| `src/models/note_slugs.rs` | `assign(db, note)` (slug from the title, unique, never changed once set), `find_public(db, slug)` |
| `src/controllers/note_pages.rs` | `GET /read/notes/{slug}` |
| `src/views/note_pages.rs`, `templates/note_pages/show.html` | the page (the resource's `NoteView`) |
| `src/routes/note_pages.rs` | the route, `paths::note_page(slug)`, its `ROUTES` row (public) |
| `tests/app/note_pages_test.rs` | found by slug, 404, `same-title-2`, a slug never moves, markup as text |

## Wiring

One line each (two for the column), marked `public-show-by-slug`:

- `migration/src/lib.rs` at `samples: migration mods` and `samples: migrations`
- `src/models/note/mod.rs` at `samples: columns`: `#[sea_orm(unique)] pub slug: Option<String>,`
- `src/models/mod.rs` at `samples: model mods`; `src/controllers/mod.rs`, `src/views/mod.rs` at their `mods` anchors
- `src/routes.rs` at `samples: route mods`, `samples: routes`, `samples: paths`, `samples: route tables`
- `tests/app/main.rs` at `samples: test mods`

## After adding

1. `cargo run -q -- db migrate` (then `db rollback && db migrate`).
2. Give new notes their slug: in `src/controllers/notes/new.rs`, after `note::create`, `let record = note_slugs::assign(&s.db, record).await?;`. Existing rows: a one-off task that calls `assign` on each.
3. Link to the page: `{{ paths::note_page(slug) }}` where a note has one (`NoteView` gets a `slug` field).
4. The sitemap: public slugs belong in it; `src/controllers/meta.rs`'s `public_paths()` is static, so add a query there (or a sitemap per resource). `cargo offrails verify`.

## Adapting it

- Drafts: add `published_at` (a migration) and filter on it in `find_public`; test that a draft is 404.
- Slugs per owner (`/u/{user}/{slug}`): a two-parameter path (`Path((user, slug)): Path<(String, String)>`), the unique index on `(user_id, slug)`, and `slug::unique` with the owner in its query.
- A slug that follows the title: call a variant of `assign` on update that keeps the old slug as a redirect (a `note_slug_redirects` table), or links break.

## Remove it

`cargo offrails snippet remove public-show-by-slug` (roll the migration back first if it ran: `cargo run -q -- db rollback`), or delete the paths above and the lines marked `public-show-by-slug`.
