# Generator: `migration` (`rails generate migration`)

> **TL;DR**
> - `cargo offrails g migration <name> [field:type…]`: `create_<table>`, `add_<x>_to_<table>`, `remove_<x>_from_<table>`, or any name for a blank one. It writes `migration/src/m<stamp>_<name>.rs` with a `down` that undoes `up`, and registers it at the anchors in `migration/src/lib.rs` (restoring them if `sea-orm-cli` removed them).
> - Then `cargo run -q -- db migrate && cargo run -q -- db rollback && cargo run -q -- db migrate`.
> - Committed migrations are append-only: change a table with a new migration ([migrations](../guides/migrations.md#rules)).
>
> Read the rest for what each shape writes.

```
cargo offrails g migration create_widgets name:string size:integer:optional
cargo offrails g migration add_subtitle_to_posts subtitle:string:optional:index
cargo offrails g migration remove_legacy_from_posts legacy:string:optional
cargo offrails g migration backfill_slugs
```

- **`create_<table>`**: `big_pk_auto` id, the columns, `created_at`/`updated_at`, foreign keys (`<table>_<col>_fkey`, cascade; `SetNull` when nullable) with indexes, unique indexes `<table>_<col>_key`. `down` drops the table. For a table with a model, use `g model` (it writes this migration too).
- **`add_<x>_to_<table>`**: one `ALTER` per column (SQLite runs one change per statement). A required column needs a default for existing rows: strings `""`, numbers `0`, booleans `false`; a required datetime, date, uuid, json or reference is refused (add it `:optional`, backfill, then tighten in another migration). A reference gets an index but no `FOREIGN KEY` (SQLite can't add one to an existing table; the model checks the reference). When the table's model came from a generator, the entity gets the new columns at its `samples: columns` anchor; add them to `Params`, `validate()`, the form and the factory if people edit them.
- **`remove_<x>_from_<table>`**: drops the columns (and their indexes); `down` adds them back. Remove them from the entity, params, forms and tests yourself.
- **Anything else**: an empty `up`/`down` with `// APP:` lines. Write the change and its exact undo; a data migration gets its own test ([test-standards](../guides/test-standards.md#what-each-suite-contains)).

**Customize:** names of indexes and constraints are part of the schema; keep them. Never edit an applied migration: generate another.
