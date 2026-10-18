# Snippet: slug

> **TL;DR**
> - `slug::slugify("Café, déjà vu!")` → `"cafe-deja-vu"`: lowercase ASCII words joined by `-`, accents folded, at most 80 characters, `untitled` when nothing is left.
> - `slug::unique(&base, |s| async move { …your query… })` → `base`, `base-2`, `base-3`… the first one your query says is free.
> - Unique **per owner** when your query filters by the owner (two users may both have `hello-world`); global when it doesn't (a public URL).

## When to use it

- URLs from titles (`/blog/hello-world`), file names, anchors.
- Not for: identifiers that must never change (a slug follows its title unless you freeze it), or secrets (slugs are guessable; use a random token).

## Files

| Path | What it is |
|------|------------|
| `src/models/slug.rs` | `slugify`, `unique`, `MAX_LEN`, `FALLBACK`, with unit tests |

## Wiring

- `src/models/mod.rs` at `samples: model mods`: `pub mod slug; // slug`

## After adding

The column, its index and the query are yours (they depend on the model):

1. A migration: `cargo offrails g migration add_slug_to_posts slug:string:optional:index`, then make the index unique **with the owner** when slugs are per owner: `Index::create().name("posts_user_id_slug_key").table(Posts::Table).col(Posts::UserId).col(Posts::Slug).unique()`.
2. The query, in the model's `queries.rs`, scoped like every other lookup:

   ```rust
   /// Whether `user` already has a post with this slug.
   pub async fn slug_taken_for(db: &impl ConnectionTrait, user: &User, slug: &str) -> Result<bool, AppError> {
       Ok(Entity::find()
           .filter(Column::UserId.eq(user.id))
           .filter(Column::Slug.eq(slug))
           .count(db)
           .await? > 0)
   }
   ```

3. Where a post is created: `let slug = slug::unique(&slug::slugify(&new.title), |s| async move { post::slug_taken_for(db, user, &s).await }).await?;` then save it with the row.
4. A test for the collision (`-2`) and, per owner, that another user's slug doesn't count. `cargo offrails verify`.

## Adapting it

- Other scripts: `fold` maps accented Latin letters; add your alphabet's transliteration there, or keep non-Latin titles as `untitled-2`… and let people edit the slug.
- Two requests can pick the same slug at once: the unique index is the real guarantee; map its violation to a 422 like the generated `map_unique` does, or retry once.

## Remove it

`cargo offrails snippet remove slug`, or delete `src/models/slug.rs` and the line marked `// slug`.
