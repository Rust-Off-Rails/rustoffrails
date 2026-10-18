# Snippet: atom-feed

> **TL;DR**
> - `GET /feed.xml`: an Atom feed (RFC 4287) any feed reader understands, and a `<link rel="alternate">` in every page's head so browsers and readers find it.
> - The XML is an Askama template, so every title and summary is escaped; dates are RFC 3339; the newest 20 entries.
> - It ships empty: you load the entries in one function (`entries` in `src/controllers/feed.rs`).

## When to use it

- A blog, a changelog, release notes, anything people follow: "subscribe" without accounts or email.
- Not for: private data (a feed is public; readers send no cookie), or a JSON API.

## Files

| Path | What it is |
|------|------------|
| `src/controllers/feed.rs` | `GET /feed.xml`; `entries()` is where you load your rows (`// APP:`) |
| `src/views/feed.rs` | `Feed::new(title, base_url, entries)`: sorts, caps at `Feed::MAX`, makes links absolute; unit tests (escaping, order, dates) |
| `templates/feed/atom.xml` | the feed |
| `src/routes/feed.rs` | the route, `paths::feed()`, its `ROUTES` row (public) |
| `tests/app/feed_test.rs` | the feed answers as Atom; every page links to it |

## Wiring

One line each, marked `// atom-feed` (`{# atom-feed #}` in the layout):

- `src/controllers/mod.rs` at `samples: controller mods`, `src/views/mod.rs` at `samples: view mods`
- `src/routes.rs` at `samples: route mods`, `samples: routes`, `samples: paths`, `samples: route tables`
- `templates/layouts/application.html` at `samples: app head` (the `<link rel="alternate">`)
- `tests/app/main.rs` at `samples: test mods`

## After adding

1. Fill `entries` in `src/controllers/feed.rs`: query the newest public records and map each to an `Entry` (`path` from `paths::`, `updated` from the row, `summary` plain text).
2. Add a test that a record shows in the feed (`assert_text(&record.title)`), citing your AC.
3. `cargo offrails verify`.

## Adapting it

- Markdown bodies: put a plain-text summary in `summary` (strip the markup), never rendered HTML: an Atom `<summary>` here is text.
- Several feeds (per tag, per author): copy the route with a path param (`/tags/{tag}/feed.xml`) and pass the filtered entries to the same `Feed::new`.
- `APP_URL` must be the public URL in production: links in the feed are absolute.

## Remove it

`cargo offrails snippet remove atom-feed` deletes the files you didn't change and the marked lines. By hand: delete the five paths above and every line marked `atom-feed`.
