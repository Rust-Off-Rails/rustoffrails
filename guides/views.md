# Views (Action View → Askama)

In `htmx` mode, every HTML page is an Askama template (`templates/`) paired with a Rust struct (`src/views/<plural>.rs`). The compiler checks that every variable a template uses exists with the right type. Mailers use Askama in **all** modes.

> **Version check.** Askama's syntax and derive attributes have changed across versions: 0.12 → 0.13 was a big change, and `askama_axum` was removed. The snippets here were verified with askama 0.16.1. Before writing unfamiliar syntax (custom filters especially), read the docs for the version in `Cargo.lock`.

## Rendering

```rust
// src/views/mod.rs
pub fn render(t: impl askama::Template) -> Result<axum::response::Html<String>, AppError> {
    Ok(axum::response::Html(t.render()?))     // askama::Error → AppError::Template
}
```

Don't use `askama_axum` or any `IntoResponse` derive. Call `views::render(...)` everywhere.

## Page structs

```rust
// src/views/posts.rs
use askama::Template;

#[derive(Template)]
#[template(path = "posts/index.html")]          // relative to templates/
pub struct PostsIndex { pub ctx: Ctx, pub posts: Vec<Post> }

#[derive(Template)]
#[template(path = "posts/new.html")]
pub struct PostsNew { pub ctx: Ctx, pub params: PostParams, pub errors: Errors }

/// Fragment for htmx swaps; same file as the {% include %} partial.
#[derive(Template)]
#[template(path = "posts/_post.html")]
pub struct PostPartial { pub post: Post }
```

**Every page struct has `ctx: Ctx`.** `Ctx` is defined in `src/views/mod.rs` and built by the `Ctx` extractor in `controllers/application.rs`. The layout reads only from `ctx`. It carries:

- `title: String`, defaulted to the app name; change it with `ctx.titled("…")`;
- `flash: Option<kernel::flash::Flash>`;
- `assets`: digested asset paths, see `frontend-htmx.md`;
- `dev: bool`, which shows the "Dev tools" link in development;
- `current_user: Option<User>` and `dev_mailbox: bool` (show the [dev mailbox hint](auth.md#dev-mailbox-hint-all-variants)), added by the authentication generator.

Verified with askama 0.16.1:

- `{% extends %}`, `{% block %}` and `{% include %}` work as shown;
- `{% if let Some(flash) = ctx.flash %}` binds, and method calls like `{% if flash.is_alert() %}` work;
- `{% for msg in errors.full_messages() %}` iterates;
- `{# comments #}` are fine.

## Layout & partials

```html
{# templates/layouts/application.html #}
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{% block title %}{{ ctx.title }}{% endblock %}</title>
  <meta name="htmx-config" content='{"allowEval":false,"includeIndicatorStyles":false,"historyCacheSize":0,"refreshOnHistoryMiss":true,"responseHandling":[…]}'>  {# full value: frontend-htmx.md #}
  <link rel="icon" href="/static/favicon.svg" type="image/svg+xml">
  <link rel="stylesheet" href="{{ ctx.assets.css }}">
  <script src="{{ ctx.assets.htmx }}" defer></script>
  <script src="{{ ctx.assets.app_js }}" defer></script>
  {% block head %}{% endblock %}   {# per-page scripts, e.g. the sse extension on pages that stream #}
</head>
<body hx-boost="true" class="…">
  {% include "shared/_nav.html" %}
  <main>
    {% include "shared/_flash.html" %}
    {% block content %}{% endblock %}
  </main>
</body>
</html>
```

```html
{# templates/posts/index.html #}
{% extends "layouts/application.html" %}
{% block content %}
  <h1>Posts</h1>
  <div id="posts">
    {% for post in posts %}{% include "posts/_post.html" %}{% endfor %}
  </div>
{% endblock %}
```

- `{% include %}` sees the including template's variables, so a partial's variable names must match at every include site (`post` inside a `for post in posts` loop).
- A partial that htmx also swaps on its own gets a standalone struct (`PostPartial { post }`) with the same field names.
- Shared partials: `templates/shared/_flash.html`, `_errors.html`, `_nav.html`, `_pagination.html`.

## Forms

```html
{# templates/posts/_form.html: used by new.html and edit.html #}
<form method="post" action="{{ action }}">
  {% include "shared/_errors.html" %}
  <label for="post_title">Title</label>
  <input id="post_title" name="title" value="{{ params.title }}"
         {% if !errors.on("title").is_empty() %}aria-invalid="true"{% endif %}>
  {% for msg in errors.on("title") %}<p class="field-error">Title {{ msg }}</p>{% endfor %}
  <button>Save</button>
</form>
```

- `name` attributes match `PostParams` field names exactly.
- `action` comes from `paths::` helpers: set it on the view struct, or with `{% let action = paths::books() %}` before the `{% include %}`. Never hard-code URLs.
- Delete is a separate small form: `<form method="post" action="{{ delete_path }}">` with a `hx-confirm` / `data-confirm`.
- Re-render with `params` (what the user typed), never with the DB row, after a failed submit.

## Helpers (`src/views/helpers.rs`)

Rails helpers become plain functions, or Askama custom filters. Check the resolved version's docs for filter registration syntax.

| Rails helper | Off Rails |
|--------------|-----------|
| `time_ago_in_words` | `helpers::time_ago(dt)` |
| `number_to_currency` | `helpers::money(cents, "USD")` |
| `pluralize` | `helpers::pluralize(n, "post")` |
| `link_to` | plain `<a href="{{ path }}">`. No helper |
| `simple_format` / `sanitize` | `helpers::markdown(src)` (below) |
| `dom_id(post)` | `helpers::dom_id("post", post.id)` → `post_42` |

Timezone conversion and formatting happen here, never in models.

## Rich text

Action Text becomes a Markdown `TEXT` column:

1. Store raw Markdown, as the user typed it.
2. Render on display: `pulldown-cmark` → HTML → **`ammonia::clean`** → `helpers::markdown(src) -> String`.
3. Output with `{{ post.body_html|safe }}`. This is the **only** sanctioned `|safe`: the value must come from `helpers::markdown` (or `helpers::markdown_trusted`, below), computed in the handler or view struct.

**User content vs trusted content.** `ammonia::clean` strips `id` and `class`. For user-written Markdown that's what you want: user-supplied ids can clobber page elements. Content the app ships itself (docs, a `content/` directory, the vendored `.offrails/` a docs site renders) needs heading anchors and code-block classes, so give it its own helper:

- `helpers::markdown_trusted(src)`: pulldown-cmark with `ENABLE_TABLES | ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS`. Buffer each heading's events, slug its text GitHub-style (lowercase, drop punctuation except `-`/`_`, spaces → `-`, duplicates get `-1`, `-2`), and emit `<hN id="slug">`. pulldown-cmark generates no ids itself; `ENABLE_HEADING_ATTRIBUTES` only parses explicit `{#id}`.
- Sanitize with an `ammonia::Builder` that additionally allows `id` on `h1`–`h6` and `class` on `code`/`pre`/`span`, and nothing else. Still sanitize: "trusted" means written by the app's authors, not exempt from the CSP.
- Test that every intra-doc link and `#anchor` resolves.

This is verified in the Rust off Rails website (pulldown-cmark 0.13, ammonia 4).

## Meta, favicon and social cards (every app with pages)

Every app with pages (`htmx`, `inertia`, `spa`) ships these **from `new`**, not later. A link pasted into Slack or iMessage shows a card, the tab shows an icon, and search engines get a title and a description. `api` mode has none of it, only `/robots.txt` with `Disallow: /` ([api](api.md)).

**Brand assets**, generated once by the framework's tool and committed (needs Chrome at generation time, nothing at runtime):

```sh
bash .offrails/tools/brand/render.sh "Bookshelf" "#4f46e5" "Track the books you've read." static
# → static/favicon.svg (the editable source), favicon.ico (48px PNG in an ICO), apple-touch-icon.png (180),
#   icon-512.png, og.png (1200×630, from assets/brand/og.html: restyle that file and re-run)
```

**Meta lives in `Ctx`** (htmx; in TS modes the Rust side fills the same fields into the HTML shell):

```rust
pub const DEFAULT_DESCRIPTION: &str = "One sentence: what the app does, for whom.";

pub struct Ctx {
    pub title: String,        // page title; "" or APP_NAME on the home page
    pub description: String,  // DEFAULT_DESCRIPTION unless the page sets one
    pub path: String,         // the request path, for canonical URLs
    pub app_url: String,      // config.app_url: absolute, no trailing slash
    // … flash, assets, dev, current_user …
}

impl Ctx {
    pub fn titled(mut self, t: &str) -> Self { self.title = t.to_owned(); self }
    /// Collapses whitespace and caps at 300 chars (search engines show ~160).
    pub fn described(mut self, d: &str) -> Self { /* … */ self }
    /// "Books · Bookshelf", or "Bookshelf: <tagline>" on the home page.
    pub fn page_title(&self) -> String { /* … */ }
    pub fn canonical(&self) -> String { format!("{}{}", self.app_url, self.path) }
    pub fn absolute(&self, path: &str) -> String { format!("{}{path}", self.app_url) }
}
```

Handlers set them per page: `ctx.titled(&book.title).described(&book.blurb)`. Pages behind login keep the defaults and add `<meta name="robots" content="noindex">` (`ctx.noindex()`).

**The layout's `<head>`** (htmx `templates/layouts/application.html`; the same tags in the inertia/spa HTML shell):

```html
<title>{{ ctx.page_title() }}</title>
<meta name="description" content="{{ ctx.description }}">
<link rel="canonical" href="{{ ctx.canonical() }}">
<meta name="theme-color" content="#4f46e5">
<meta property="og:type" content="website">
<meta property="og:site_name" content="{{ crate::config::APP_NAME }}">
<meta property="og:title" content="{{ ctx.page_title() }}">
<meta property="og:description" content="{{ ctx.description }}">
<meta property="og:url" content="{{ ctx.canonical() }}">
<meta property="og:image" content="{{ ctx.absolute("/static/og.png") }}">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:image" content="{{ ctx.absolute("/static/og.png") }}">
<link rel="icon" href="{{ ctx.assets.path("favicon.svg") }}" type="image/svg+xml">
<link rel="icon" href="/favicon.ico" sizes="48x48">
<link rel="apple-touch-icon" href="{{ ctx.assets.path("apple-touch-icon.png") }}">
```

- `og:image` and `og:url` must be **absolute** (scrapers don't resolve relative URLs), so production needs a real `APP_URL`.
- `/favicon.ico` is routed to `static/favicon.ico` (browsers request it at the root whatever the `<link>` says). Without the file, every first visit logs a 404.
- **`/robots.txt`** (allow all, `Sitemap:` line) and **`/sitemap.xml`** (public pages from `paths::` helpers, absolute URLs) are routes with tests, `Access::Public`.
- The smoke test asserts every tag above on the home page, and that `/favicon.ico`, `/static/og.png`, `/robots.txt` and `/sitemap.xml` answer 200 ([new](../generators/new.md#7-tests)).

Content and marketing sites also need:

- **Self-hosted fonts** (`static/fonts/*.woff2`, `font-display: swap`). The default CSP (`font-src 'self'`, `style-src 'self'`) blocks Google Fonts, and loosening the CSP for fonts isn't worth it. Preload only the one or two faces above the fold.
- **Lighthouse ≥ 95** in all four categories, mobile and desktop, against a release build. Lighthouse checks one color scheme only, so it doesn't replace the axe suite in both themes ([test-standards §10](test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)). Run the CLI and record the scores in `docs/test-report.md`.

## Dark mode

The default layout follows the OS: `<meta name="color-scheme" content="light dark">` plus Tailwind `dark:` variants on the base surfaces (body background and text, nav, cards, form fields, flash, errors). Tailwind 4's default `dark:` variant is the `prefers-color-scheme` media query, so this needs no JavaScript. The kernel pages (`/dev`, `/admin/jobs`, the mailbox) already do this in their own CSS.

When the spec asks for a **manual toggle**, avoid the flash of the wrong theme without an inline script (the CSP forbids those):

- Store the choice in a `theme` cookie (`light` | `dark`; absent = system), set by a tiny `POST /theme` form (an `Access::Public` route) or from `static/app.js`.
- The `Ctx` extractor reads the cookie, and the layout renders `<html data-theme="dark">` (or `light`) on the server, so the first paint is already right.
- In `assets/app.css`, one `dark:` variant covers the cookie and the OS fallback (verified with Tailwind 4.3.3):
  ```css
  @custom-variant dark {
    &:where([data-theme=dark], [data-theme=dark] *) { @slot; }
    @media (prefers-color-scheme: dark) {
      &:where(:root:not([data-theme=light]), :root:not([data-theme=light]) *) { @slot; }
    }
  }
  ```

## Mailer templates

`templates/mailers/<mailer>/<action>.html` and `.txt`, each with its own struct in `src/mailers/<mailer>.rs` (see `mailers.md`). The email layout is `templates/mailers/layout.html` ([mailers](mailers.md)). Use absolute URLs built from `config.app_url`.

## Gotchas agents get wrong

- `use askama_axum::…`. That crate is gone. Render to `String` and wrap it in `Html`.
- Template paths with a leading `templates/` or `/`. They're relative to `templates/`.
- Using `|safe` on user content. Banned. Only `helpers::markdown` output.
- Calling a DB function from a template or view struct method. Load the data in the handler.
- Jinja-isms that Askama lacks (`{% set x = … %}` scoping, Python methods like `.items()`). Write Rust expressions or add a field.
- Option handling: `{% if let Some(u) = ctx.current_user %}`. Not `{% if ctx.current_user %}`.
- Forgetting `ctx` on a new page struct. The layout won't compile, which is good, so just add it.
- **Askama 0.16 passes field and variable arguments by reference.** `{{ paths::book(book.id) }}` fails with "expected `i64`, found `&i64`". Write `paths::book(*book.id)` for `Copy` arguments. `&str` parameters take `&String` fine (`paths::password(token)`).
- **`{% let x = row.book.id %}` binds a reference**, and `{% let x = *row.book.id %}` doesn't compile. To bind a value, use `{% let x = row.book.id.clone() %}`, then pass `*x` in calls.
- **Shared partials resolve names in the module of each struct that includes them.** In `_nav.html`, `_list.html` and friends, write full paths (`crate::routes::paths::books()`, `crate::views::helpers::dom_id(…)`). Page templates can use `paths::` if their view module imports it.
- **Escaping uses numeric entities:** `<` → `&#60;`, `>` → `&#62;`, `&` → `&#38;`, `"` → `&#34;`, `'` → `&#39;`. Tests compare visible text with `assert_text` ([testing](testing.md)), not hand-escaped strings.
