# Generators: `controller` and `page` (`rails generate controller`)

> **TL;DR**
> - `cargo offrails g controller reports index summary`: `src/controllers/reports.rs` (one GET handler per action), `src/views/reports.rs`, `templates/reports/<action>.html`, the routes (`/reports`, `/reports/summary`), `paths::reports()` / `paths::reports_summary()`, `ROUTES` rows, docs/routes.md, and `tests/app/reports_test.rs`.
> - With accounts the pages need a signed-in user (`Access::User`, `RequireUser`); `--public` makes them public. `--nav` adds a nav link.
> - `cargo offrails g page about`: a public static page in the shared `controllers/pages.rs` / `views/pages.rs`, with a sitemap entry and a test in `tests/app/pages_test.rs`.
>
> Read the rest for what to fill in.

```
cargo offrails g controller reports index summary [--public] [--nav]
cargo offrails g page about [--nav] [--path /about-us]
```

## Controller

Each action renders a page that reads nothing yet: the handler is marked `// unscoped: reads nothing yet (APP: scope what it loads with the user)`, which the guard test accepts. Fill it in this order:

1. Load the data in the handler through scoped model functions (`<model>::all_for(&s.db, &user, page)`), then drop the `_` from `RequireUser(_user)` and the `// unscoped:` marker.
2. Add the fields to the page struct in `src/views/reports.rs`; compute display strings there, never in the template.
3. Write the template (every light surface and dark text with its `dark:` partner).
4. Replace the generated "renders" test with the spec's acceptance criteria; keep the signed-out test.

An action that changes data is a POST to a noun, not a verb ([routing](../guides/routing.md#the-resource-shape)): generate a `resource` for it, or add the handler by hand next to these.

## Pages

`g page` is for content that is the same for everyone: about, pricing, terms. The shared files (`controllers/pages.rs`, `views/pages.rs`, `tests/app/pages_test.rs`) grow at their `samples: page …` anchors; destroying the last page removes them. Give each page a description (`ctx.titled("About").described("…")`) for search results and link previews ([views](../guides/views.md#meta-favicon-and-social-cards-every-app-with-pages)).
