# Snippet: seed-backdated

> **TL;DR**
> - Seed data that looks lived-in: create each record through its model (`note::create`, validation runs), then `seed_support::backdate::<note::Entity>(db, id, days_ago(30))` moves its `created_at`/`updated_at` into the past.
> - Why: the generated `create` stamps `now` (no callbacks, no way to pass a date), so seeds otherwise all say "just now" and every list sorted by date shows them in insertion order.
> - Generic over any generated table (they all have `id`, `created_at`, `updated_at`), owner-scoped or public: no resource or `--as` needed; seeds and tests only.

## When to use it

- `app task seed` for development and demos: posts over the last months, comments a day after their post, an old account.
- Not for: real data. A timestamp in the app is what happened; never backdate from a handler.

## Files

| Path | What it is |
|------|------------|
| `src/tasks/seed_support.rs` | `days_ago(n)`, `backdate::<E>(db, id, at)` (an error when the row isn't there) |
| `tests/app/seed_backdated_test.rs` | a backdated row is a month older and lists after a new one (on a table of a generated table's shape that the test makes, so it needs none of the app's) |

It needs no resource snippet and no `--as`: `backdate::<E>` takes any entity, so it works the same on a generated owner-scoped resource (`backdate::<post::Entity>(&txn, row.id, …)` after `post::create(&txn, &author, new)`), a public one, or the resource snippet's notes.

## Wiring

- `src/tasks/mod.rs` at `samples: task mods`: `pub mod seed_support; // seed-backdated`
- `tests/app/main.rs` at `samples: test mods`

## After adding

In `src/tasks/seed.rs`, inside its transaction, idempotent per record (find by a natural key first, create only when missing):

```rust
use crate::models::note::{self, NoteParams};
use crate::tasks::seed_support::{backdate, days_ago};

for (i, title) in ["Getting started", "A month in", "What we learned"].iter().enumerate() {
    if note::Entity::find().filter(note::Column::Title.eq(*title)).one(&txn).await?.is_some() {
        continue;
    }
    let new = NoteParams { title: (*title).into(), body: "…".into() }.validate().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let row = note::create(&txn, new).await?;
    backdate::<note::Entity>(&txn, row.id, days_ago(90 - 30 * i as i64)).await?;
}
```

Then `cargo run -q -- task seed` twice (the second changes nothing), and `cargo offrails verify`: the seed task also runs in the suite (`tasks_run_by_name_and_refuse_unknown_ones`, under a development config).

## Adapting it

- Other timestamp columns (`published_at`): copy `backdate` and set that column too; keep it in this file.
- Many rows: backdate in one `update_many` per batch if seeding thousands is slow.

## Remove it

`cargo offrails snippet remove seed-backdated`, or delete the two files and the lines marked `seed-backdated`.
