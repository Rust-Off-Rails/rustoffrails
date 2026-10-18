# Snippet: pagination

> **TL;DR**
> - `views::pager::Pager::new(&paths::notes(), page, more)` and `{% include "shared/_pager.html" %}`: "← Newer · Page 2 · Older →" for any paged list, `?per=` kept in the links.
> - The `Page` extractor (`src/models/mod.rs`) clamps `?page=` to 1..=`Page::MAX_PAGE` and `?per=` to 1..=100, so `?page=18446744073709551615` is an empty page, not an overflow; the pager's links stay in that range. (Before 0.7.3 the offset overflowed: a 500 on every generated index.)
> - Generated resources have their own `newer()`/`older()`; this is the same thing, shared, for lists you write by hand or want to keep consistent.

## When to use it

- Any list longer than one page: the generated `all`/`all_for` already return `(rows, more)`.
- Not for: infinite scroll or cursors (`?after=<id>`), which need a different query; feeds (`Feed::MAX`).

## Files

| Path | What it is |
|------|------------|
| `src/views/pager.rs` | `Pager { number, newer, older }`, `Pager::new`, `is_needed`, with unit tests (links, `per`, the clamp, the partial) |
| `templates/shared/_pager.html` | the links, in the template's tokens (every `dark:` partner) |

## Wiring

- `src/views/mod.rs` at `samples: view mods`: `pub mod pager; // pagination`

## After adding

1. The page struct gets `pub pager: Pager`; the handler fills it: `pager: Pager::new(&paths::notes(), page, more)` (with `page: Page` among its extractors).
2. The template: `{% include "shared/_pager.html" %}` where the links go (a generated index: in place of its `<nav aria-label="Pages">` block; then drop its `newer()`/`older()`).
3. A request test that `?page=2` lists the second page and a huge `?page=` answers 200. `cargo offrails verify`.

## Adapting it

- Other query parameters (a filter, a search): pass them in `base` (`format!("{}?q={q}", paths::notes())` needs `&` instead of `?` in `link`: change the one `format!`).
- A total count ("Page 2 of 7"): `PaginatorTrait::count` is one more query per page; add `pub last: Option<u64>` if the product needs it.

## Remove it

`cargo offrails snippet remove pagination`, or delete the two files and the line marked `// pagination` (and any `{% include "shared/_pager.html" %}`).
