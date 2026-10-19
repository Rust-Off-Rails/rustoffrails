# Views (Action View → Askama)

> **TL;DR**
> - htmx mode: every page is an Askama template plus a Rust struct in `src/views/`; the compiler checks templates. Mailers use Askama in every mode.
> - No DB calls from views; `|safe` only on helper output (ammonia); render with `.render()?` + `Html`, never `askama_axum`.
> - Every app with pages ships meta, favicon and social cards, and dark mode via `prefers-color-scheme` (an optional cookie toggle).
> - Askama 0.16 passes fields by reference (`*book.id` for `Copy` args) and escapes with numeric entities.
> - **Look:** `brand.toml` + `cargo offrails brand` (colors, fonts, radius, logo, favicon set, social card; WCAG AA checked). **Layout:** `{% extends "layouts/<application|sidebar|marketing|auth|docs>.html" %}`. **Components:** `{% import "components/ui.html" as ui %}`, then the [table below](#components); see them all at `/dev/components`.
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
// src/views/posts.rs (the controllers are src/controllers/posts/<action>.rs)
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

Every page extends a layout ([layouts](#layouts)); `templates/layouts/application.html` is the shell they all share: one `<head>` (the `htmx-config` and CSP pairing from [frontend-htmx](frontend-htmx.md#required-config-swap-422-responses), the stylesheet, htmx, `app.js`, `components.js`, `dev-reload.js` in development, and the parts' scripts at `samples: app head`), `<body hx-boost="true">`, the skip link, the icon sprite, the overlays (confirm dialog, modals, toasts) and, inside each layout's `<main id="main">`, `shared/_notices.html` (the development CSS warning, then the flash).

```html
{# templates/posts/index.html #}
{% extends "layouts/application.html" %}
{% import "components/ui.html" as ui %}
{% block content %}
{% call ui::page_header("Posts") %}{% call ui::link_button("New post", paths::new_post(), icon = "plus") %}{% endcall %}{% endcall %}
<div id="posts">
  {% for post in posts %}{% include "posts/_post.html" %}{% endfor %}
</div>
{% endblock %}
```

- Never put scripts in a page's `{% block head %}`: a boosted link swaps only `<body>`, so they never run ([frontend-htmx](frontend-htmx.md#gotchas-agents-get-wrong)). Load them from the layout.
- `{% include %}` sees the including template's variables, so a partial's variable names must match at every include site (`post` inside a `for post in posts` loop). An included template that calls the kit has its own `{% import "components/ui.html" as ui %}`.
- A partial that htmx also swaps on its own gets a standalone struct (`PostPartial { post }`) with the same field names.
- Shared partials: `templates/shared/_flash.html` (the kit's `flash`), `_errors.html` (a form's error summary), `_nav.html` (the top nav: `samples: nav links`), `_sidebar.html`, `_brand.html`, `_notices.html`. Pagination is the kit's `ui::pagination` (a generated index calls it; the [pagination snippet](../template/snippets/pagination/README.md) is the same for lists you write).

## Brand

`brand.toml` at the app root is the app's look in one file: `name`, `tagline` (the only place it lives: `config::APP_TAGLINE` is `brand::TAGLINE`, used by the home page title, the social card and any nav that shows it), `[colors]` `primary` (exactly this on buttons), `accent`, `neutral` (`gray | slate | zinc | neutral | stone`, or a hex to tint grays with), optional `on_primary`; `[fonts]` `sans`, `heading`, `mono` (CSS stacks; self-host web fonts, the CSP blocks font CDNs); `[shape]` `radius`; `[logo]` `mark` (1–2 characters, default the name's initial) or `svg` (a square SVG file) and `tile`. After editing it:

```sh
cargo offrails brand            # renders everything below; `new` runs it; --check: fail if stale (verify --full runs that)
```

| Output | What it is |
|---|---|
| `assets/brand.css` | Tailwind `@theme`: `primary-*`, `accent-*` (50–950, generated in OKLCH), `gray-*` (the neutral), `indigo-*` (= primary, so parts and older templates follow the brand), `on-primary`, `primary-hover`, `--font-sans/heading/mono`, `--radius-*`; semantic `--brand-*` tokens for light and dark. `assets/app.css` imports it |
| `src/brand.rs` | `NAME` (= `config::APP_NAME`), `TAGLINE`, `PRIMARY`, `THEME_COLOR` (the theme-color meta), `brand::email::*` (the email layout's inline colors; brand points the mailer part's header at them) |
| `static/favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, `icon-512.png` | the mark on a primary tile |
| `assets/brand/og.svg`, `static/og.png` | the 1200×630 social card |

**Contrast is checked before anything is written:** every text/surface pair the components use (body, muted, links, badges, button text and its hover; light and dark) must reach WCAG AA 4.5:1, or `brand` refuses with the pair, its ratio and a color that passes. The scales are built so that the shades holding text pass on their surfaces whatever the base color (a yellow brand gets dark button text and a darkened 700). **PNGs** are rasterized with the installed Chrome/Chromium/Edge (headless) or `rsvg-convert`; with neither, the SVGs are current, the PNGs keep their old look and `brand` says so (re-run where a browser is installed). In templates, use `primary-*`/`accent-*` for brand color, `gray-*` for the neutral; never a raw hex.

## Layouts

`templates/layouts/application.html` is the shell every layout extends: the `<head>` (one set of scripts for every page, so [boosted links](frontend-htmx.md#boosted-navigation) keep working; parts add theirs at `samples: app head`), the skip link, the icon sprite, the overlay slots (htmx modals, the confirm dialog, toasts) and the development footer. Its `{% block body %}` is the top-nav layout; the others replace that block. A page picks one with `{% extends %}` and fills `content` (plus the blocks below). Each is responsive (no sideways scroll at 360 px), has dark mode, a skip link to `<main id="main">` and landmarks; see each at `/dev/layouts/<name>`.

| Layout | `{% extends … %}` | For | Blocks besides `content` |
|---|---|---|---|
| Top nav (default) | `layouts/application.html` | most pages | `head` (avoid: boosted links never run it) |
| Sidebar | `layouts/sidebar.html` | dashboards, admin, many sections; folds behind **Menu** on a phone | `sidebar` (links, `.nav-link` + `aria-current="page"`; default `shared/_sidebar.html`, anchor `samples: sidebar links`) |
| Marketing | `layouts/marketing.html` | landing and pricing pages | `hero`, `nav` (`.nav-link`s), `cta`, `footer_links` |
| Auth card | `layouts/auth.html` | sign in, sign up, reset, confirm | `card_footer` |
| Docs | `layouts/docs.html` | documentation, help | `toc` (`.toc-link` anchors); `content` is the article (`.prose-doc`) |

## Components

`templates/components/ui.html` is the kit: Askama macros that say in a few words what would otherwise be a class list, with the right markup (labels, `aria-invalid`, `aria-describedby`, `aria-current`, `aria-sort`), both themes and the brand's colors. Import it once per template and call each macro with `{% call %}…{% endcall %}` (the `endcall` is required; a body between them goes where the table says **body**). Arguments are positional or `name = value`; the ones with `=` have defaults.

```html
{% extends "layouts/application.html" %}
{% import "components/ui.html" as ui %}
{% block content %}
{% call ui::page_header("Books") %}{% call ui::link_button("New book", paths::new_book(), icon = "plus") %}{% endcall %}{% endcall %}
<form method="post" action="{{ paths::books() }}">
  {% call ui::field(errors, "title", "Title", params.title, required = true) %}{% endcall %}
  {% call ui::select(errors, "status", "Status", statuses, params.status) %}{% endcall %}
  {% call ui::form_actions("Save", paths::books()) %}{% endcall %}
</form>
{% endblock %}
```

| Macro | Arguments (defaults) | Notes |
|---|---|---|
| `button` | `label, variant="primary", size="md", type="submit", name="", value="", confirm="", icon="", disabled=false` | variant `primary\|secondary\|danger\|ghost`; size `sm\|md\|lg`; `confirm` asks in the confirm dialog |
| `link_button` | `label, href, variant="primary", size="md", icon=""` | navigation only; changes are POST forms |
| `icon_button` | `icon, label, variant="ghost", toggle="", type="button"` | `label` is the accessible name; `toggle` = id it shows/hides |
| `confirm_button` | `label, action, message, variant="danger", size="md"` | a one-button POST form, confirmed first |
| `icon` | `name, size="size-4"` | sprite `components/_icons.html`: x menu chevron-down/left/right check check-circle info alert x-circle plus search arrow-up/down arrows-up-down inbox home user settings book chart |
| `field` | `errors, name, label, value="", type="text", hint="", required=false, autocomplete="", placeholder="", maxlength=0, inputmode="", step="", mono=false, key=""` | `value` = params (kept on 422; a password never is); id = name; `mono`: monospace, no autocorrect; `key`: the errors' key when it isn't `name` |
| `textarea` | `errors, name, label, value="", rows=4, hint="", required=false, mono=false, key=""` | |
| `select` | `errors, name, label, options, selected="", hint="", required=false, blank="", key=""` | `options`: (value, label) pairs, values strings or ids (`Vec<(i64, String)>`); `blank`: an empty first option; a `book_id` select reports on `key = "book"` |
| `checkbox` | `errors, name, label, checked=false, value="true", hint=""` | |
| `radios` | `errors, name, label, options, selected="", hint=""` | a fieldset with a legend |
| `form_actions` | `submit="Save", cancel_href="", cancel="Cancel"` | **body**: more buttons |
| `alert` | `tone="info", title="", dismissible=false` | tone `info\|success\|warning\|danger`; **body**: the message |
| `flash` | `flash` | `ctx.flash` as a dismissible alert |
| `toast` / `toast_oob` | `message, tone="success", timeout=6000` | `toast_oob` from an htmx response lands in the layout's `#toasts`; `timeout=0` stays |
| `card` | `title="", subtitle=""` | **body**: the card's content |
| `badge` | `label, tone="neutral"` | tone `neutral\|primary\|accent\|success\|warning\|danger` |
| `stat` | `label, value, delta="", trend=""` | trend `up\|down` colors the delta |
| `table` | `caption, columns, empty=false, empty_message="Nothing here yet.", sort="", dir="asc", base="?"` | `columns`: (key, label), a key makes a sort link `{base}sort=key&dir=…`; **body**: the `<tr>` rows |
| `pagination` | `page, pages, base="?"` | any integer types; links `{base}page=n`, gaps as …; a query that knows only whether there's `more` (the generated `all`, no COUNT) passes `pages = page + more` |
| `empty_state` | `title, message="", action_label="", action_href="", icon="inbox"` | |
| `avatar` | `name, src="", size="md"` | initials without `src`; size `sm\|md\|lg` |
| `breadcrumbs` | `items` | (label, href) pairs, the last is the current page |
| `page_header` | `title, subtitle=""` | **body**: the actions |
| `nav_tabs` | `items, current, label="Sections"` | links: (label, href); `current` is an href |
| `tabs` / `tab_panel` | `id, items, selected, label="Sections"` / `id, key, selected` | client-side; items (key, label); **body** of `tabs`: one `tab_panel` per key |
| `dropdown` / `menu_item` | `label, variant="secondary", size="md"` / `label, href` | `<details>` menu; **body**: `menu_item`s |
| `modal` | `id, title, autoshow=false` | native `<dialog>`: inert page behind, focus kept inside, Esc closes, focus returns; **body**: content |
| `modal_button` / `modal_link` / `modal_close` | `label, target, variant="secondary"` / `label, url, variant="secondary"` / `label="Cancel"` | open a modal on the page; load one with htmx (`url` answers `ui::modal(…, autoshow = true)`); close the current one |

For anything the macros don't cover, the same look is a few class names (`assets/app.css`, light + dark built in): `.btn` `.btn-secondary` `.btn-danger` `.btn-ghost` `.btn-sm` `.btn-lg` `.btn-icon` `.link` · `.label` `.input` `.hint` `.field-error` `.check` `.choice` `.form-actions` · `.card` `.card-header` `.card-body` `.card-footer` `.muted` · `.badge(-tone)` · `.alert` `.alert-<tone>` · `.table-wrap` `.table` · `.tabs` `.tab` · `.page-link` · `.menu` `.menu-item` · `.nav-link` `.toc-link` `.prose-doc`. The behaviors live in `static/components.js` (delegated, so they survive boosted links): `data-dropdown`, `[data-tabs]`, `data-modal-open|close`, `data-confirm` (forms and submit buttons) and `hx-confirm` (both ask in `#confirm-dialog`), `data-dismiss`, `data-toast-timeout`; `data-toggle` (show/hide, `aria-expanded` kept in step) is in `app.js`.

**Rules.** Build pages from the kit (the generators and the snippets do: their pages are worked examples); a class list longer than eight utilities on a `<button>`, `<input>`, `<select>`, `<textarea>` or a button-like `<a>` in an app template is an `offrails-check` warning (`[classes]`; end a deliberate one's line with `{# offrails-check: classes ok #}`; a line an installed part ships as it is is skipped, and once you edit one, move it onto the kit). `/dev/components` shows every component in every state (`?theme=light|dark` pins one); `theme_test` renders it and the layout demos and checks every class list for its `dark:` partner and its CSS rule, and `.offrails/tools/browser-smoke/smoke-components.mjs` drives the modals, the confirm dialog and the 422 form in a real browser.

## Forms

```html
{# templates/posts/_form.html: used by new.html and edit.html #}
{% import "components/ui.html" as ui %}
<form method="post" action="{{ action }}" novalidate>
  {% include "shared/_errors.html" %}
  {% call ui::field(errors, "title", "Title", params.title, required = true, maxlength = 200) %}{% endcall %}
  {% call ui::textarea(errors, "body", "Body", params.body, 8) %}{% endcall %}
  {% call ui::select(errors, "author_id", "Author", choices.author, params.author_id, blank = "Choose an author", key = "author") %}{% endcall %}
  {% call ui::checkbox(errors, "published", "Published", params.published == "true") %}{% endcall %}
  {% call ui::form_actions("Save post", paths::posts()) %}{% endcall %}
</form>
```

- `name` attributes match `PostParams` field names exactly; the kit gives each control its label, `aria-invalid`, `aria-describedby` and its error text from `errors.on(name)` (`key` when the errors are under another name, like a `*_id` select's).
- `action` comes from `paths::` helpers: set it on the view struct, or with `{% let action = paths::books() %}` before the `{% include %}`. Never hard-code URLs.
- Delete is a separate small form, confirmed first: `{% call ui::confirm_button("Delete", paths::delete_post(*post.id), "Delete this post?") %}{% endcall %}`.
- Re-render with `params` (what the user typed), never with the DB row, after a failed submit (422).
- `g resource` writes exactly this shape for every field type ([generators](../generators/README.md#what-the-generators-decide-so-you-dont)).

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

**Brand assets** come from `brand.toml` ([Brand](#brand)), rendered by `cargo offrails brand` and committed (rasterizing needs Chrome or `rsvg-convert` at render time, nothing at runtime): `static/favicon.svg`, `favicon.ico` (48px PNG in an ICO), `apple-touch-icon.png` (180), `icon-512.png`, and `og.png` (1200×630, from `assets/brand/og.svg`). (`tools/brand/render.sh` is the 0.7.2 way for apps without a `brand.toml`.)

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
<meta name="theme-color" content="{{ crate::brand::THEME_COLOR }}">
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
- The smoke test asserts every tag above on the home page, and that `/favicon.ico`, `/static/og.png`, `/robots.txt` and `/sitemap.xml` answer 200 ([new](../generators/new.md#tests)).

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

The component layer is in `assets/app.css` (`.btn`, `.input`, `.card`, `.badge`, `.alert`, `.table`…; [Components](#components) lists them); the templates use these names, and §11 checks the `@apply` lists too. Brand color is `primary-*` (`indigo-*` is an alias of it, from `assets/brand.css`), so the palette's indigo rows below follow `brand.toml`.

**Never** add a light surface without its partner "for now", and never fix a dark-mode bug by removing `color-scheme` or the body's `dark:` classes: that hides the bug for OS-dark users only until the next page. In development the layout's CSS banner (`Ctx.css_warning`) also says when `static/app.css` is missing a class a template uses ([dev-tools](dev-tools.md#introspection)).

When the spec asks for a **manual toggle**, avoid the flash of the wrong theme without an inline script (the CSP forbids those):

- Store the choice in the **`offrails_theme`** cookie: `system` | `light` | `dark` (an unknown or missing value means `system`), `Path=/; SameSite=Lax; Max-Age=31536000`, **not** `HttpOnly`, because `static/app.js` writes it to switch instantly. The name is namespaced so it never collides with an app's own `theme` setting, and it's the cookie the [samples](samples.md)' kernel pages read, so app and kernel pages share one switch.
- Without JS, the switch is a `POST /theme` form (`Access::Public`, CSRF-checked like every POST) that sets the cookie and redirects to a **same-site** `return_to` only (a relative path; never `//host` or `/\host`, per [auth](auth.md#return-to-kernelreturn_tors)). With JS, `app.js` writes the same cookie and sets `data-theme` on `<html>` at once.
- The production cookie hardening in `kernel/security.rs` adds `HttpOnly` to every `Set-Cookie`; exempt `offrails_theme` **by exact name** (a `SCRIPT_WRITABLE_COOKIES` list), or the script can't overwrite it and the switch sticks. Test: in production, the `/theme` response's cookie has `Secure` and `SameSite` but no `HttpOnly`, and every other cookie still has `HttpOnly`.
- The `Ctx` extractor reads the cookie, and the layout renders `<html data-theme="system|light|dark">` and `<meta name="color-scheme" content="light dark|light|dark">` on the server, so the first paint is already right with no inline script. The standalone error pages do the same.
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
