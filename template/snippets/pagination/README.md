# Snippet: pagination

> **TL;DR**
> - `views::pager::PageLinks::new(&paths::notes(), page, more)` and `{% include "shared/_pager.html" %}`: the component kit's `ui::pagination` (‹ 1 2 [3] 4 ›) for any paged list, `?per=` kept in the links.
> - The `Page` extractor (`src/models/mod.rs`) clamps `?page=` to 1..=`Page::MAX_PAGE` and `?per=` to 1..=100, so `?page=18446744073709551615` is an empty page, not an overflow; the pager's links stay in that range. (Before 0.7.3 the offset overflowed: a 500 on every generated index.)
> - Generated indexes call `ui::pagination` themselves (`pages()`, `page_base()` on the page struct); this is the same thing, shared, for lists you write by hand, plus `?per=`.

## When to use it

- Any list longer than one page: the generated `all`/`all_for` already return `(rows, more)`.
- Not for: infinite scroll or cursors (`?after=<id>`), which need a different query; feeds (`Feed::MAX`).

## Files

| Path | What it is |
|------|------------|
| `src/views/pager.rs` | `PageLinks { number, pages, base }`, `PageLinks::new`, `is_needed`, with unit tests (links, `per`, the clamp, the partial) |
| `templates/shared/_pager.html` | one `ui::pagination` call (`templates/components/ui.html`) |

## Wiring

- `src/views/mod.rs` at `samples: view mods`: `pub mod pager; // pagination`

## After adding

1. The page struct gets `pub pager: PageLinks`; the handler fills it: `pager: PageLinks::new(&paths::notes(), page, more)` (with `page: Page` among its extractors).
2. The template: `{% include "shared/_pager.html" %}` where the links go (a generated index: in place of its `ui::pagination` call; then drop its `pages()`/`page_base()`).
3. A request test that `?page=2` lists the second page and a huge `?page=` answers 200. `cargo offrails verify`.

## Adapting it

- Other query parameters (a filter, a search): add them to `base` the way `per` is (`q=…&`, URL-encoded: `helpers::query_string`).
- A total count (every page number, not just the next): `PaginatorTrait::count` is one more query per page; set `pages` from it.

## Remove it

`cargo offrails snippet remove pagination`, or delete the two files and the line marked `// pagination` (and any `{% include "shared/_pager.html" %}`).
