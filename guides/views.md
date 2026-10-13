# Views (Action View → Askama)

> **TL;DR**
> - htmx mode: every page is an Askama template plus a Rust struct in `src/views/`; the compiler checks templates. Mailers use Askama in every mode.
> - No DB calls from views; `|safe` only on helper output (ammonia); render with `.render()?` + `Html`, never `askama_axum`.
> - Every app with pages ships meta, favicon and social cards, and dark mode via `prefers-color-scheme` (an optional cookie toggle).
> - Askama 0.16 passes fields by reference (`*book.id` for `Copy` args) and escapes with numeric entities.
>
> Read the rest when writing templates, forms, layouts, helpers, meta tags, or dark mode.

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
  <meta name="color-scheme" content="light dark">
  <title>{% block title %}{{ ctx.title }}{% endblock %}</title>
  <meta name="htmx-config" content='{"allowEval":false,"includeIndicatorStyles":false,"historyCacheSize":0,"refreshOnHistoryMiss":true,"responseHandling":[…]}'>  {# full value: frontend-htmx.md #}
  <link rel="icon" href="/static/favicon.svg" type="image/svg+xml">
  <link rel="stylesheet" href="{{ ctx.assets.css }}">
  <script src="{{ ctx.assets.htmx }}" defer></script>
  <script src="{{ ctx.assets.app_js }}" defer></script>
  {% if ctx.dev %}<script src="/static/dev-reload.js" defer></script>{% endif %}   {# dev-tools.md#hot-reload #}
  {% block head %}{% endblock %}   {# per-page scripts, e.g. the sse extension on pages that stream #}
</head>
<body hx-boost="true" class="min-h-screen bg-white text-gray-900 dark:bg-gray-950 dark:text-gray-100 antialiased">
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

The default layout follows the OS: `<meta name="color-scheme" content="light dark">` (layout **and** the standalone error pages) plus Tailwind `dark:` variants. Tailwind 4's default `dark:` variant is the `prefers-color-scheme` media query, so this needs no JavaScript. The kernel pages (`/dev`, `/admin/jobs`, the mailbox) already do this in their own CSS.

**Every class in every template is correct in both schemes, or the page is unreadable.** A generated Kanban shipped `bg-white` cards and panels with no `dark:` partner while the body turned dark with light text: 1.09:1. The rule is mechanical: every light surface, dark text and faint tone carries its `dark:` partner **in the same class list** ([test-standards §11](test-standards.md#11-theme-and-css-guards) enforces it; axe runs in both schemes, [§10](test-standards.md#10-accessibility-wcag-22-aa-in-every-theme)). Use this palette (gray family; swap `gray` for the design's neutral, keep the shade pairs). Verified with axe (WCAG 2.2 AA) in light and dark in the reference app:

| Role | Light | Dark partner |
|---|---|---|
| Page (body) | `bg-white text-gray-900` | `dark:bg-gray-950 dark:text-gray-100` |
| Card, panel, table row, `<pre>`, placeholder | `bg-gray-50` / `bg-gray-100` | `dark:bg-gray-900` / `dark:bg-gray-800` |
| Hover surface | `hover:bg-gray-50` | `dark:hover:bg-gray-800` |
| Secondary text | `text-gray-800` / `text-gray-700` | `dark:text-gray-200` / `dark:text-gray-300` |
| Muted text (hints, meta, timestamps) | `text-gray-600` / `text-gray-500` | `dark:text-gray-400` |
| Nav link | `text-gray-700 hover:text-gray-900` | `dark:text-gray-300 dark:hover:text-white` |
| Borders, dividers | `border-gray-200` / `border-gray-300` / `divide-gray-200` | `dark:border-gray-800` / `dark:border-gray-700` / `dark:divide-gray-800` |
| Link | `text-indigo-700 underline` | `dark:text-indigo-300` (underlined in both: links inside text need more than color, axe `link-in-text-block`) |
| Accent text | `text-indigo-600` | `dark:text-indigo-400` |
| Flash notice | `bg-green-50 text-green-800` | `dark:bg-green-950 dark:text-green-200` |
| Flash alert, error summary | `bg-red-50 text-red-800 border-red-200` | `dark:bg-red-950 dark:text-red-200 dark:border-red-900` |
| Field error, destructive link | `text-red-700` | `dark:text-red-300` |
| Dev mailbox hint, warnings | `bg-amber-50 text-amber-900 border-amber-300` | `dark:bg-amber-950 dark:text-amber-100 dark:border-amber-800` |
| Primary button | `bg-indigo-600 text-white hover:bg-indigo-500` | none needed (mid surface in both schemes); `dark:focus-visible:ring-offset-gray-950` |

The skeletons `new` generates, in this palette:

```html
<body hx-boost="true" class="min-h-screen bg-white text-gray-900 dark:bg-gray-950 dark:text-gray-100 antialiased">
<nav class="border-b border-gray-200 dark:border-gray-800">…
  <a href="/dev" hx-boost="false" class="text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white">Dev tools</a>
{# shared/_flash.html: partners inside each branch #}
<div role="status" class="mb-6 rounded-md px-4 py-3 text-sm {% if flash.is_alert() %}bg-red-50 dark:bg-red-950 text-red-800 dark:text-red-200{% else %}bg-green-50 dark:bg-green-950 text-green-800 dark:text-green-200{% endif %}">
{# shared/_errors.html and errors/_invalid.html #}
<div class="errors rounded-md border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950 p-4 text-sm text-red-800 dark:text-red-200">
{# shared/_dev_mailbox_hint.html #}
<aside class="mb-6 rounded-md border border-amber-300 dark:border-amber-800 bg-amber-50 dark:bg-amber-950 px-4 py-3 text-sm text-amber-900 dark:text-amber-100" data-dev-mailbox-hint>
{# lists (scaffold index, reviews) #}
<ul class="mt-6 divide-y divide-gray-200 dark:divide-gray-800">  <p class="text-sm text-gray-600 dark:text-gray-400">…meta…</p>
{# auth pages: forms use the component classes below; footers #}
<p class="mt-6 text-sm text-gray-600 dark:text-gray-400">Already have an account? <a href="…" class="link">Log in</a></p>
```

The component layer in `assets/app.css` (forms and buttons; the templates use these names, and §11 checks the `@apply` lists too):

```css
@layer components {
  .label { @apply block text-sm font-medium text-gray-700 dark:text-gray-300; }
  .input { @apply mt-1 block w-full rounded-md bg-white text-gray-900 dark:bg-gray-900 dark:text-gray-100 border border-gray-300 dark:border-gray-700 px-3 py-2 text-sm shadow-sm focus:border-indigo-500 focus:outline-none focus:ring-2 focus:ring-indigo-500; }
  .input[aria-invalid="true"] { @apply border-red-500; }
  .field-error { @apply mt-1 text-sm text-red-700 dark:text-red-300; }
  .btn { @apply inline-flex items-center rounded-md bg-indigo-600 px-4 py-2 text-sm font-medium text-white hover:bg-indigo-500 focus:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500 focus-visible:ring-offset-2 dark:focus-visible:ring-offset-gray-950; }
  .btn-secondary { @apply inline-flex items-center rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-900 px-4 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-800 focus:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500; }
  .btn-danger { @apply inline-flex items-center rounded-md border border-red-300 dark:border-red-800 bg-white dark:bg-gray-900 px-4 py-2 text-sm font-medium text-red-700 dark:text-red-300 hover:bg-red-50 dark:hover:bg-red-950 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500; }
  .link { @apply text-indigo-700 dark:text-indigo-300 underline underline-offset-2 hover:decoration-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500; }
}
```

**Never** add a light surface without its partner "for now", and never fix a dark-mode bug by removing `color-scheme` or the body's `dark:` classes: that hides the bug for OS-dark users only until the next page. In development the layout's CSS banner (`Ctx.css_warning`) also says when `static/app.css` is missing a class a template uses ([dev-tools](dev-tools.md#introspection)).

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
