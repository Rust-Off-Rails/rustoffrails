# Generators: `controller` and `page` (`rails generate controller`)

> **TL;DR**
> - `cargo offrails g controller reports index summary`: `src/controllers/reports/{mod,index,summary}.rs` (one GET handler per file), `src/views/reports.rs`, `templates/reports/<action>.html`, `src/routes/reports.rs` (the routes `/reports`, `/reports/summary`, `paths::reports()` / `paths::reports_summary()`, the `ROUTES` rows), docs/routes.md, and `tests/app/reports_test.rs`.
> - Path parameters: `g controller blog index show:/blog/{slug}` (or `--path /blog/{slug}` for a one-action controller): `Path(slug): Path<String>` in the handler, `slug` on the page struct, `paths::blog_show(slug: &str)`, a test that requests `paths::blog_show("example")`. Ids (`{id}`, `{x_id}`) are `i64`, the rest `String`.
> - With accounts the pages need a signed-in user (`Access::User`, `RequireUser`); `--public` makes them public. `--nav` adds a nav link (the first action, without parameters).
> - `cargo offrails g page about`: a public page, its own files (`src/controllers/pages/about.rs`, `src/views/pages/about.rs`, `templates/pages/about.html`, `src/routes/about.rs`, `tests/app/pages/about_test.rs`), a sitemap entry. `--path /people/{handle}` gives it a parameter (and no sitemap entry).
> - Every shared file gets one line per spot, marked `// reports` (`{# reports #}` in the nav): `destroy` removes them, and so can you.
>
> Read the rest for what to fill in.

```
cargo offrails g controller reports index summary [--public] [--nav] [--layout sidebar]
cargo offrails g controller blog index show:/blog/{slug}
cargo offrails g page about [--nav] [--path /about-us] [--layout docs]
cargo offrails g page profile --path /people/{handle}
```

Each page is a `ui::page_header` and a placeholder paragraph in `layouts/application.html` (the top nav); `--layout sidebar|marketing|auth|docs` extends another ([views](../guides/views.md#layouts)).

## Controller

Each action renders a page that reads nothing yet: the handler is marked `// unscoped: reads nothing yet (APP: scope what it loads with the user)`, which the guard test accepts. Fill it in this order:

1. Load the data in the handler through scoped model functions (`<model>::all_for(&s.db, &user, page)`), then drop the `_` from `RequireUser(_user)` and the `// unscoped:` marker.
2. Add the fields to the page struct in `src/views/reports.rs`; compute display strings there, never in the template. A path parameter is already a field (untrusted text: Askama escapes it; look records up by it with a scoped query, and answer 404 for one that doesn't exist).
3. Write the template from the component kit (`ui::card`, `ui::table`, `ui::stat`…: [views](../guides/views.md#components)); a class list of your own has every light surface and dark text with its `dark:` partner.
4. Replace the generated "renders" test with the spec's acceptance criteria; keep the signed-out test.

An action that changes data is a POST to a noun, not a verb ([routing](../guides/routing.md#the-resource-shape)): generate a `resource` for it, or add the handler by hand next to these.

## Pages

`g page` is for content that is the same for everyone: about, pricing, terms. Each page is its own handful of files; the shared `pages` modules (`src/controllers/pages/mod.rs`, `src/views/pages/mod.rs`, `tests/app/pages/mod.rs`) get one `pub mod <page>; // <page>` line each, and destroying the last page removes them. Give each page a description (`ctx.titled("About").described("…")`) for search results and link previews ([views](../guides/views.md#meta-favicon-and-social-cards-every-app-with-pages)).
