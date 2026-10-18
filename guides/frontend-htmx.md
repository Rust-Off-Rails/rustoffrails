# Frontend: `htmx` mode (Hotwire → htmx)

> **TL;DR**
> - Askama HTML + htmx 2 (vendored) + Tailwind standalone; no Node except islands.
> - The layout's `htmx-config` is required: swap 422/429/5xx (`responseHandling`), `allowEval:false` + `includeIndicatorStyles:false` (CSP), `historyCacheSize:0` + `refreshOnHistoryMiss:true`.
> - Extensions (sse) load in the layout, never a page head; links into kernel pages carry `hx-boost="false"`; a 422 re-render sends `HX-Push-Url: false`.
> - Fragments for `HX-Request`, full pages otherwise; small behaviors in `static/app.js` (no inline scripts); assets are digested and immutable.
>
> - Optional: [instant navigation for content sites](#instant-navigation-for-content-sites-optional-recipe) (View Transitions, a CSS progress bar, a versioned stale-while-revalidate service worker, prefetch on intent).
>
> Read the rest when building pages or partials, adding JS behavior, or using islands.

Server-rendered HTML, made dynamic by htmx attributes. No Node, no bundler, no build step for JS. CSS comes from the Tailwind standalone binary.

## Setup (done by `generators/new.md`)

**htmx:** vendor the latest **2.x** (this framework's syntax and config are htmx 2; htmx 4 is on npm's `next` tag and is not adopted). **Never type a version from memory.** unpkg resolves the major for you:

```sh
mkdir -p static/vendor
for pkg in htmx.org/dist/htmx.min.js htmx-ext-sse/dist/sse.min.js; do
  url=$(curl -sSLf -o /dev/null -w '%{url_effective}' "https://unpkg.com/${pkg%%/*}@2/${pkg#*/}")   # → …/htmx.org@2.0.11/dist/htmx.min.js
  name=${pkg%%/*}; ver=$(printf '%s' "$url" | sed -E "s|.*/${name}@([^/]+)/.*|\1|")
  curl -sSLf "$url" -o "static/vendor/${name%.org}-$ver.min.js"                                    # htmx-2.0.11.min.js, htmx-ext-sse-2.2.4.min.js
done
```

The version lives in the filename. `kernel/assets.rs` finds each file by an exact key (`vendor/htmx-2.`, `vendor/htmx-ext-sse-`) that a test proves matches exactly one file ([assets](#assets--cache-busting-kernelassetsrs)). Verified 2026-10: htmx 2.0.11, htmx-ext-sse 2.2.4.

**Tailwind (v4 standalone CLI):** the agent looks up the current v4 tag once (`curl -sSLf -o /dev/null -w '%{url_effective}' https://github.com/tailwindlabs/tailwindcss/releases/latest` → `…/tag/v4.3.3`) and writes it into **`bin/setup-tailwind`** as `TAILWIND_VERSION=v4.3.3`, so every machine, CI and the Docker build get the same binary. `bin/setup` and the Dockerfile both call `bin/setup-tailwind`, which downloads and verifies `bin/tailwindcss` (git-ignored, and in `.dockerignore`). It's `#!/bin/sh`, `set -eu`, then:

```sh
os=$(uname -s | sed 's/Darwin/macos/;s/Linux/linux/'); arch=$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/')
asset="tailwindcss-$os-$arch"                    # macos-arm64, macos-x64, linux-x64, linux-arm64 (-musl variants exist for Alpine)
base="https://github.com/tailwindlabs/tailwindcss/releases/download/$TAILWIND_VERSION"
bin/tailwindcss --help 2>/dev/null | grep -q "${TAILWIND_VERSION#v}" && exit 0   # already the pinned version
curl -sSLf "$base/$asset" -o bin/tailwindcss && chmod +x bin/tailwindcss
curl -sSLf "$base/sha256sums.txt" | grep "/$asset\$" | awk '{print $1"  bin/tailwindcss"}' | shasum -a 256 -c -
```

```css
/* assets/app.css */
@import "tailwindcss";
@source "../templates";          /* Askama templates; @source paths are relative to this file */
@source "../static/app.js";
@source "../web/src/islands";    /* ISLANDS: yes only */

/* htmx's own indicator styles, because includeIndicatorStyles is off (CSP) */
.htmx-indicator { opacity: 0; }
.htmx-request .htmx-indicator, .htmx-request.htmx-indicator { opacity: 1; transition: opacity 200ms ease-in; }
```

Build: `bin/tailwindcss -i assets/app.css -o static/app.css --minify` (`--watch` in `bin/dev`; `--watch=always` if stdin may close). Verified with v4.3.3: classes inside Askama `{% if %}` blocks are picked up.

## Hotwire → htmx

| Hotwire | htmx |
|---------|------|
| Turbo Drive | `hx-boost="true"` on `<body>`. Links and forms become AJAX with history |
| Turbo Frame | `hx-get`/`hx-post` + `hx-target="#id"` + `hx-select="#id"` (or return a partial) |
| Turbo Stream (append/replace/remove) | `hx-swap-oob="true"` elements in the response, or `hx-swap="beforeend"` |
| `data-turbo-confirm` | `hx-confirm="Delete this post?"` |
| `turbo_stream_from` (broadcasts) | SSE extension, see `realtime.md` |
| Stimulus controllers | `static/app.js` with event delegation (below) |
| Lazy-loaded frame | `hx-get="…" hx-trigger="load"` (or `revealed`) |
| Turbo morphing refresh | `hx-get` the region on an SSE signal (`realtime.md`) |

## Required config: swap 422 responses

By default htmx **does not swap 4xx responses**, so failed forms would silently do nothing. Put this in the layout `<head>`. The defaults it extends (verified in htmx 2.0.11) are `204` no swap, `[23]..` swap, `[45]..` error. `allowEval: false` and `includeIndicatorStyles: false` keep htmx inside the CSP (no `eval`, no injected `<style>`; the indicator CSS is in `app.css`):

```html
<meta name="htmx-config" content='{"allowEval":false,"includeIndicatorStyles":false,"historyCacheSize":0,"refreshOnHistoryMiss":true,"responseHandling":[
  {"code":"204","swap":false},
  {"code":"[23]..","swap":true},
  {"code":"422","swap":true},
  {"code":"429","swap":true},
  {"code":"[45]..","swap":false,"error":true}
]}'>
<link rel="icon" href="/static/favicon.svg" type="image/svg+xml">
```

**`historyCacheSize: 0` + `refreshOnHistoryMiss: true`: every Back/Forward is a real page load.** With `hx-boost`, htmx otherwise restores a DOM snapshot from its own history cache. The reference app's real-browser smoke caught two bugs from this, both invisible to request tests:

- Back after deleting a book showed the deleted book again, from the snapshot.
- A page reached by Back/Forward kept its `sse-connect` markup but had no live `EventSource`, so live updates silently stopped.

A fresh load costs one request and is always correct (Turbo also re-fetches after a restore). Add the favicon link too, or every first visit logs a console 404 for `/favicon.ico`.

**`429` swaps too:** rate-limited forms re-render with 429 and a "Try again later." flash (guides/security.md). Without the rule, the browser shows nothing.

**Every other error on a boosted request shows the server's error page.** htmx doesn't swap `[45]..`, so a boosted link or form that hits a 500, 403 or 404 would leave the page silently unchanged. That's what users see as a dead button. The reference app's browser click-through hit exactly this: a password reset returned 500 and the form just sat there. `static/app.js` handles it (CSP-safe, no inline script):

```js
// A boosted navigation or form that fails (500, 403, 404…) would otherwise do nothing visible:
// htmx doesn't swap error responses. Show the server's error page instead, like a normal browser
// navigation would. Fragment requests (hx-get on a part of the page) keep htmx's behavior.
document.addEventListener("htmx:responseError", (e) => {
  if (!e.detail.boosted) return;
  const doc = new DOMParser().parseFromString(e.detail.xhr.responseText, "text/html");
  if (!doc.body) return;
  document.title = doc.title;
  document.body.replaceWith(document.adoptNode(doc.body));
  window.htmx?.process(document.body);
});
```

Request tests can't see this: they get the 500 and assert on it. Check it in the browser smoke: submit a boosted form to an unknown route, and the 404 page must appear.

### Keep the URL on a 422

A boosted form that comes back 422 is swapped in (above), and htmx then pushes the **POST** URL into the address bar (`/books/5` showing the edit form). Reloading or sharing that URL then shows a different page. Every 422 re-render for an htmx request sends **`HX-Push-Url: false`**, which htmx 2 honours by skipping the history update (verified in htmx 2.0.11's source), so the address bar stays on `/books/5/edit`. Do it once, at the end of `error::shape_errors` ([errors-and-logging](errors-and-logging.md)): it sees every 422, whether a handler rendered it or `AppError::Invalid` did.

## Full page vs fragment

Handlers that serve both normal and htmx requests take `HxRequest(is_htmx)`:

```rust
if is_htmx && hx_target == Some("posts") {
    return Ok(views::render(PostsList { posts })?.into_response());   // fragment only
}
Ok(views::render(PostsIndex { ctx, posts })?.into_response())          // full page
```

Rules:
- With `hx-boost`, return **full pages**. htmx swaps the `<body>`. Return fragments only for explicit `hx-get`/`hx-post` targets.
- Redirects work as-is: htmx follows the 303 and swaps the result. To force a full navigation, set the `HX-Redirect` header.
- `RequireUser` failures on htmx requests send `HX-Redirect: /session/new` (handled in `error.rs`).
- Add `Vary: HX-Request` to responses that differ by it.

## Small behaviors: `static/app.js` (the Stimulus replacement)

One plain-JS file using event delegation and `data-` attributes. No Alpine, no frameworks.

```js
// static/app.js: each behavior is one delegated listener on `document`, keyed by a data attribute
document.addEventListener("click", (e) => {
  const el = e.target.closest("[data-toggle]");
  if (el) document.getElementById(el.dataset.toggle)?.classList.toggle("hidden");
});
document.addEventListener("htmx:afterSwap", () => { /* re-init anything that needs it */ });
```

If a behavior outgrows ~50 lines or needs real client state, it's an island.

### Boosted navigation

**The rule: page JS loads from the app layout's `<head>` (or the body) and binds by delegation on `document`. Never in a page's `{% block head %}`, never on `DOMContentLoaded`.** The layout boosts every link (`<body hx-boost="true">`). A boosted click fetches the next page and swaps `<body>`: nothing in that page's `<head>` loads or runs, and `DOMContentLoaded` doesn't fire again. So a script in a page's head block, or one that finds its elements on `DOMContentLoaded`, works on a fresh load, in request tests and in screenshots, and is dead after a click. The real case: the sign-in page's password **Show** button worked when the page was loaded directly and did nothing after clicking **Sign in** in the nav.

- Every script loads from `templates/layouts/application.html`: the app's in `static/app.js`, a part's at the `samples: app head` anchor. It runs once, on the first full load. A page with nothing to enhance costs one cached request.
- Each behavior is one listener on `document` that finds its element with `e.target.closest("[data-…]")`. It works on the first page, on every boosted page and on every htmx swap, with nothing to bind again. Code that must start per element (a live room's socket) also listens to `htmx:load`, which htmx fires for each swapped-in element, and cleans up on `htmx:beforeSwap`.
- **The live-element convention** (what the browser smoke clicks): every element page JS reacts to on click is a `<button type="button">`, or carries `data-js` (or `data-toggle`, or `role="button"`). Give a non-button clickable element `data-js="<behavior>"`. `data-smoke-skip` leaves one out.

Two guards hold the rule. `tests/app/smoke_test.rs` (every app, both databases): `app_pages_load_no_scripts_or_stylesheets_in_their_head_block` and `page_scripts_never_bind_only_on_dom_content_loaded` (a script in `static/` outside `vendor/` and `kernel/` that uses `DOMContentLoaded` or `load` without `htmx:load`). And the browser smoke, which proves the behavior: for each GET page in `docs/routes.md` it clicks the live elements after a full load, then again after a boosted link from `/`, and fails on any element that only works on the full load, on a page-specific script in `<head>`, and on any console error ([testing](testing.md#browser-smoke-boosted-navigation)):

```sh
BASE=<the URL bin/dev printed> node .offrails/tools/browser-smoke/smoke-boosted.mjs   # or: cargo offrails verify --full
```

### No full reload after a mutation

**Never `location.reload()` (or `window.location = …` to the same page) after the user changes something.** A generated Kanban's board JS reloaded the page after every card drop: the whole page re-rendered and every stream on it reconnected for each move, under load that was the slowest request in the app, and the user lost scroll and focus. Instead:

1. **Move the DOM optimistically** (the card is already where the user dropped it; keep it there).
2. **Send the mutation** (`fetch` POST with the CSRF-safe same-origin headers, or an htmx request), then **re-fetch the live region in place**, the same GET a live signal makes: `htmx.ajax("GET", region.getAttribute("hx-get"), { target: region, swap: "outerHTML" })`. On success that confirms the optimistic move; on a 4xx it puts the card back where the server says it is (and the region shows the flash or error).
3. Other viewers update through the signal as usual ([realtime](realtime.md#client-side)), served from the shared render ([render once per version](realtime.md#render-once-per-version-the-thundering-herd)).

```js
// static/app.js: a drop on a board column (drag source: [data-card-id], drop target: [data-column-id])
document.addEventListener("drop", async (e) => {
  const column = e.target.closest("[data-column-id]");
  const card = document.querySelector(`[data-card-id="${e.dataTransfer.getData("text/plain")}"]`);
  if (!column || !card) return;
  e.preventDefault();
  column.querySelector("[data-cards]").append(card);                       // 1. optimistic
  const body = new URLSearchParams({ column_id: column.dataset.columnId });
  await fetch(`/cards/${card.dataset.cardId}/move`, { method: "POST", body }).catch(() => {});  // 2. mutate
  const region = column.closest("[hx-get]");                               //    then the live region's own GET
  window.htmx.ajax("GET", region.getAttribute("hx-get"), { target: region, swap: "outerHTML" });
});
```

The move handler answers `204` (or `303` for no-JS forms); the page never navigates. A `pageshow` reload for back/forward-cache restores ([realtime](realtime.md#client-side)) is not a mutation reload and stays.

## Assets & cache busting (`kernel/assets.rs`)

```rust
/// Digested URLs the layouts need. Built from an `AssetCatalog` (in `AppState`).
#[derive(Debug, Clone, Default)]
pub struct Assets { pub css: String, pub htmx: String, pub htmx_sse: String, pub app_js: String, pub admin_css: String }

pub struct AssetCatalog { /* root, recompute: bool, digests: Arc<HashMap<String, String>> */ }
impl AssetCatalog {
    pub fn load(root: &str, recompute: bool) -> Self;   // AppState::new: AssetCatalog::load("static", config.is_development())
    pub fn assets(&self) -> Assets;                     // recomputes per call in development (--watch output is picked up)
}
pub async fn cache_headers(req: Request, next: Next) -> Response;   // middleware for /static
```

- At boot, hash every file under `static/` (SHA-256, first 8 hex chars) **once**, into the `AssetCatalog` held by `AppState`. Never build the catalog in an extractor or per request (one app re-hashed every static file, images included, twice per request, in production). Development may rescan per call for `--watch` output; production never does.
- `asset_path(key)` returns `/static/<file>?v=<hash>` for the file whose relative path starts with `key`. **Each key must match exactly one file:** `css` = `app.css`, `htmx` = **`vendor/htmx-2.`** (never `vendor/htmx-`, which also matches `htmx-ext-sse-…`), `htmx_sse` = `vendor/htmx-ext-sse-`, `app_js` = `app.js`, `admin_css` = `kernel/admin.css`. Keep the keys in one `const ASSET_KEYS` array and test it:

  ```rust
  #[test]
  fn every_asset_key_matches_exactly_one_static_file() {
      let files = scan(Path::new(env!("CARGO_MANIFEST_DIR")).join("static").as_path());
      for key in ASSET_KEYS {
          let hits: Vec<_> = files.keys().filter(|f| f.starts_with(key)).collect();
          assert!(hits.len() <= 1, "asset key {key:?} matches {hits:?}: make it exact");
          assert!(key == "app.css" || hits.len() == 1, "asset key {key:?} matches no file"); // app.css is built
      }
  }
  ```

  The bug this prevents: with the loose `vendor/htmx-` key and `HashMap` iteration order, some renders linked the SSE extension as htmx core. Those pages had no `window.htmx` (dead page, live badge stuck on "connecting…"), at random, after any restart. A retry or reload in `app.js` can't fix it, because the wrong URL is chosen server-side.
- `Ctx.assets` carries `Assets` to the app layout; kernel pages read `state.assets.assets()` through `kernel::ui::Chrome`.
- `/static` is a nested router, so the cache middleware wraps only static files:

  ```rust
  let static_files = Router::new()
      .fallback_service(ServeDir::new("static"))
      .layer(from_fn(assets::cache_headers));
  app.nest_service("/static", static_files)
  ```

  `cache_headers` sets `Cache-Control: public, max-age=31536000, immutable` on successful responses whose query has `v=`, and `no-cache` on everything else.
- In development `assets()` rescans on every call, so a rebuilt file gets a new `?v=` without a restart.

### "CSS not built" (an unstyled app with no explanation)

`static/app.css` is built output and git-ignored, so a fresh checkout that runs `serve` without `bin/setup`/`bin/dev` would serve an unstyled app. `kernel::assets::css_problem(output) -> Option<String>` returns why, when `static/app.css` is **missing or empty** (`app_css_problem()` uses the app's path). **Don't compare mtimes** with `assets/app.css` or the templates: Tailwind 4 doesn't rewrite its output when it's unchanged, so after any template edit that adds no new class the file keeps its old mtime. The banner then can't be cleared by the rebuild it asks for. The reference app shipped that version, and a browser click-through caught it. `bin/dev`'s watcher keeps the CSS current:

| Where | Behavior |
|-------|----------|
| `serve` at boot | Development: one `warn!` with the reason and the fix: `CSS not built: run bin/dev, or bin/tailwindcss -i assets/app.css -o static/app.css`. **Production: boot fails** with the same message (the Docker image builds the CSS, so this means a broken build) |
| Every page (development) | `Ctx.css_warning` (set by the `Ctx` extractor only when `env == development`) renders a banner at the top of the layout. It's readable even unstyled, because it's plain text first in `<main>` |

Tests: a unit test (tempdir) for missing, empty and built (including an old mtime, which must not warn); an integration test that the banner follows `app_css_problem()` and never appears in production. TS modes mirror this for Vite in [frontend-inertia](frontend-inertia.md): development shows a banner when the Vite dev server isn't reachable, and production boot fails when `web/dist/.vite/manifest.json` is missing.

## Instant navigation for content sites (optional recipe)

For public, mostly-read sites (docs, marketing, blogs) where every visitor sees the same pages. **Not** for apps with accounts or per-user pages: a service worker that caches HTML is a privacy bug waiting to happen there. Verified on rustoffrails.org (htmx 2.0.11, Chrome, 2026-10-13): after the first visit, a boosted click is answered by the worker in ~3 ms and settles in ~50 ms, offline pages still render, and Lighthouse stays at 100.

1. **View Transitions, no JS.** In `assets/app.css`: `@view-transition { navigation: auto; }` (full-page navigations, including the reload htmx does on a history miss), plus `"globalViewTransitions":true` in the layout's `htmx-config` (boosted swaps). Name what should stay put (`.site-header { view-transition-name: site-header; }` with `::view-transition-group(site-header) { animation: none; }`) and animate `#main` (`view-transition-name: page`). Under `prefers-reduced-motion: reduce`: `::view-transition-group(*), ::view-transition-old(*), ::view-transition-new(*) { animation: none !important; }`.
2. **Progress bar, CSS only.** `<body hx-boost="true" hx-indicator="#nav-progress">` and an empty `<div id="nav-progress" class="nav-progress" aria-hidden="true">` first in `<body>`. htmx adds `.htmx-request` to it while a request runs; animate `transform: scaleX()` with a ~180 ms `animation-delay`, so instant navigations never flash it.
3. **Focus after a boosted navigation.** On `htmx:afterSettle` with `e.detail.boosted`: scroll to `location.hash`'s target if there is one, else give the new `main h1` `tabindex="-1"` and `focus({ preventScroll: true })` (screen readers announce the page). `main h1[tabindex="-1"]:focus:not(:focus-visible) { outline: none; }`.
4. **A service worker at `/sw.js`**, rendered by a route at boot (served from the root, so its scope is `/` and no `Service-Worker-Allowed` header is needed; `text/javascript`, `Cache-Control: no-cache`; `script-src 'self'` covers it, since `worker-src` falls back to it). Register it from `static/app.js` after `load`, in `requestIdleCallback`, and **not in development** (stale-while-revalidate hides template edits): the layout adds `data-sw="/sw.js"` to the `app.js` tag only outside development.
   - **Version per deploy:** bake a digest of `APP_REVISION`, the running binary (size + mtime), every `static/` file digest and the framework version into `const VERSION`. Caches are named `pages-<VERSION>` / `assets-<VERSION>`; `activate` deletes every other name, then `clients.claim()`; `install` ends with `skipWaiting()`.
   - **Precache** the main pages and the offline page, plus the digested CSS, JS, htmx and core fonts, one `fetch` + `put` each with errors swallowed (one 404 must not fail the install).
   - **Pages:** intercept only GET navigations (`request.mode === "navigate"`) and htmx **boosted** requests (`HX-Boosted: true`), never plain `HX-Request` fragments. Key the cache by path + query and match with `ignoreVary: true` (pages `Vary: Accept`). Serve the cached copy at once and refresh it in the background (`event.waitUntil`); with no copy, go to the network, and on failure return the cached offline page. Cap runtime-cached pages (≈80, delete-then-put keeps least-recently-fetched order).
   - **Only cache** `200`, non-redirected, `type === "basic"`, `text/html` responses without `no-store`/`private`. A worker **can't see `Set-Cookie`**, so the server must mark every cookie-setting response `no-store` (add it in `kernel/security.rs`). Bypass `/dev`, `/admin`, `/up`, `/sw.js`, API and non-HTML paths entirely.
   - **Digested assets** (`/static/*?v=`) are cache-first; undigested ones pass through.
   - **Theme cookie:** if the server renders `<html data-theme>` from a cookie, a cached page carries whatever theme it was fetched with. Rewrite that one attribute in the worker when serving from the cache, using `cookieStore.get("theme")` where available, else the value `app.js` posts (`navigator.serviceWorker.controller.postMessage({ theme })`) on every load and toggle, persisted in a small `meta` cache.
5. **Prefetch on intent** (in `app.js`, ~20 lines): on `pointerover` (after ~65 ms), `focusin` and `touchstart` of a same-origin link, `fetch(path, { headers: { "HX-Request": "true", "HX-Boosted": "true" } })` once per page view, only when a worker controls the page and not under Save-Data. That warms the worker's cache, so the click is local. The htmx preload extension and Speculation Rules don't feed the worker's cache (and inline `speculationrules` needs a CSP hash), so they add nothing here.
6. **Fast clicks (gotcha found in CI):** htmx's `globalViewTransitions` calls `document.startViewTransition` and drops the returned object. When a second navigation starts before the first transition finishes, the browser skips the first one and rejects its `ready`, `finished` and `updateCallbackDone` promises with `AbortError: Transition was skipped`, which nobody handles: a console error on every quick double click (it only showed on CI's slower runner). In `app.js`, before htmx loads, wrap `document.startViewTransition` once: call the original, attach a `.catch` to those three promises that swallows `AbortError` only and rethrows anything else, and return the transition. Don't allowlist the error in tests; add a regression test that fires two boosted navigations 0, 50 and 120 ms apart (also under CDP `Emulation.setCPUThrottlingRate`) and expects zero console errors. While you're there, make the last click win: number boosted requests in `htmx:beforeRequest` and set `e.detail.shouldSwap = false` in `htmx:beforeSwap` for any response older than the newest navigation (the worker answers in milliseconds, so responses can arrive out of order).
7. **Test it in a real browser** (test-standards §9): worker in control and caches named for the current version only; boosted clicks answered in < 50 ms; Back/Forward; offline (`context.setOffline(true)`): a cached page renders and an uncached one shows the offline page; no console errors. DevTools' "Offline" emulation does **not** stop the worker's own `fetch`es; stop the server (by its PID) or use Playwright's `setOffline` to test the fallback.

## Islands

For the rare rich widget (editor, canvas, complex drag-and-drop), mount a React or Svelte component inside an htmx page. Set `ISLANDS: yes` and `UI_LIBRARY` in the app's `AGENTS.md`. This section is the React version; for Svelte islands (same contract, `mount()`/`unmount()` from `svelte`), see [frontend-svelte#islands](frontend-svelte.md#islands).

- `web/` is a Vite project set up like [frontend-inertia.md](frontend-inertia.md)'s "Vite wiring" (cleanup, Biome, `tsconfig`, `tsc -b`) but with only `react`, `react-dom` and the test packages: no Inertia, no `@tailwindcss/vite` (the standalone CLI styles islands through `@source "../web/src/islands"`).
- **One entry:** `build: { manifest: true, rolldownOptions: { input: "src/islands/mount.tsx" } }`. Each island is `web/src/islands/<Name>.tsx` with a default export, and becomes its own lazy chunk through `import.meta.glob("./*.tsx")` in `mount.tsx`, so pages only download the islands they use.
- Markup: `<div data-island="Editor" data-props="{{ props_json }}"></div>`. `props_json` is `serde_json::to_string` of a `#[derive(Serialize, TS)]` struct in `src/views/`, HTML-attribute-escaped by Askama (never `|safe`).
- `web/src/islands/mount.tsx` (verified to type-check and build):

```tsx
import type { ComponentType } from "react";
import { createRoot, type Root } from "react-dom/client";

const islands = import.meta.glob<{ default: ComponentType<Record<string, unknown>> }>("./*.tsx");
const roots = new Map<Element, Root>();

async function mountAll(scope: ParentNode = document) {
  for (const el of scope.querySelectorAll<HTMLElement>("[data-island]")) {
    if (roots.has(el)) continue;
    const load = islands[`./${el.dataset.island}.tsx`];
    if (!load) throw new Error(`Unknown island: ${el.dataset.island}`);
    const { default: Island } = await load();
    const root = createRoot(el);
    root.render(<Island {...JSON.parse(el.dataset.props ?? "{}")} />);
    roots.set(el, root);
  }
}

function unmountWithin(scope: Element) {
  for (const [el, root] of roots) {
    if (scope.contains(el)) {
      root.unmount();
      roots.delete(el);
    }
  }
}

document.addEventListener("DOMContentLoaded", () => void mountAll());
document.addEventListener("htmx:afterSwap", (e) => void mountAll(e.target as Element));
document.addEventListener("htmx:beforeSwap", (e) => unmountWithin(e.target as Element));
```

- `kernel/assets.rs` reads `web/dist/.vite/manifest.json` for `src/islands/mount.tsx` (`file`, plus `css` if any) and emits one `<script type="module">`. Lazy island chunks load themselves. **The same in development:** `/assets/*` serves `web/dist/assets/` and the manifest is re-read per request in development only.
- **Development loop: `vite build --watch`, not the Vite dev server.** `bin/dev` runs `pnpm --dir web exec vite build --watch --mode development` next to Tailwind's watch, and you reload the page after an island change. Decided because:
  - **the dev CSP stays the production CSP.** The dev server needs `http://localhost:5173` in `script-src`/`style-src`/`connect-src`, `ws://localhost:5173` and `'unsafe-inline'` styles ([security](security.md)); with islands the rest of the app is server-rendered htmx, so that loosening would cover every page to speed up one widget, and CSP bugs would only show in production;
  - **one origin, one scheme:** the URL `bin/dev` prints (HTTPS + HTTP/2 with a trusted certificate, [realtime](realtime.md#many-tabs-stall-http11-vs-http2)); a second plain-http origin on :5173 brings mixed-content and certificate questions for no gain;
  - **one code path.** Dev and production both read the manifest, so the path that ships is the path you test;
  - **the cost is small:** islands are a few widgets, a rebuild takes about a second, and HMR state preservation matters little for them.

  `inertia` and `spa` keep the Vite dev server: there the whole UI is JS and HMR pays for the dev-only CSP additions.
- Islands talk to the server through JSON endpoints under `/api` (`controllers.md#json-api`).

## Gotchas agents get wrong

- **htmx extensions (sse, …) in a page's own `{% block head %}`.** A boosted navigation swaps only `<body>`, so the new page's head scripts never run, and its live regions stay dead with no error. Load every extension in the layout, next to htmx ([realtime#client-side](realtime.md#client-side)).

- **A form clicked before htmx initializes still works.** It submits as a normal POST (303 → full page), so boosted forms degrade safely. During the reference app's first browser run, one sign-up click appeared aborted. It didn't reproduce, either with a normal click or with a click fired at `DOMContentLoaded`. If a browser smoke shows `htmx:abort`, look for a second navigation racing the click (another tool driving the same tab) before changing app code.

- **A boosted form that JavaScript sends some other way** (a channel room's message box, [channels](channels.md#the-js-client)). `preventDefault` in your listener doesn't stop htmx's boosted submit, so the page also navigates (to `?body=…`). Put `hx-boost="false"` on the form. Found by the browser check: the message posted *and* the page reloaded.
- Missing the 422 `responseHandling` config, so invalid forms look like dead buttons. The same goes for 429 (rate limits) and for 5xx on boosted requests (the `app.js` error-page handler above).
- **Dropping the CSP pairing from `htmx-config`.** The CSP (`style-src 'self'; script-src 'self'`) and htmx's config must agree: `includeIndicatorStyles:false` (otherwise htmx injects a `<style>` at load and Chrome logs "Applying inline style violates … style-src 'self'"; verified with htmx 2.0.11 + Chrome, 2026-10-12) and `allowEval:false` (no violation on load, but it turns `hx-on`, trigger filters `[…]` and `js:` values into silent no-ops instead of CSP errors; never use those). Every layout that loads htmx, including the kernel layout, carries both.
- **An `hx-trigger` filter never fires.** `hx-trigger="keyup[key=='Enter']"` (any `[…]` after the event) is evaluated with `Function()`, which `allowEval:false` forbids, so htmx drops the trigger with no error and the request is never sent; request tests can't see it. Use a plain event (`keyup changed delay:300ms`, `search`, `change`), or a delegated listener in `static/app.js` that checks the key and calls `htmx.trigger(el, "enter-pressed")` for `hx-trigger="enter-pressed"`. `offrails-check` warns on a filter in `templates/` (`[htmx]`).
- **Boosting links into kernel pages.** `<body hx-boost="true">` swaps only `<body>`, so a boosted link to `/dev`, `/dev/mailbox` or `/admin/jobs` (pages with their own `<head>` and stylesheet) renders unstyled until a hard refresh. Put `hx-boost="false"` on every such link ([dev-tools](dev-tools.md#rules)). The same applies to any page that doesn't use the app layout.
- Returning a fragment to an `hx-boost` request, which replaces the whole body with a fragment.
- Loading htmx from a CDN. It's vendored, and the CSP forbids third-party scripts.
- Writing htmx 1.x syntax (`hx-ws`, `hx-sse` attributes). htmx 2 uses extensions: `hx-ext="sse"` + `sse-connect` + `sse-swap`, with the separately vendored `htmx-ext-sse` script loaded after htmx.
- Pulling htmx `@latest`/`next`. That could silently move to htmx 4. Resolve `@2`.
- Fetching Tailwind from `releases/latest` in `bin/setup`. Pin `TAILWIND_VERSION` so dev, CI and Docker build identical CSS.
- Inline `onclick=` handlers. The CSP blocks them. Use `static/app.js`.
- **`location.reload()` after a mutation** (a drop, a toggle, a reorder): re-renders the whole page and reconnects its streams per action. Move the DOM optimistically, then re-fetch the live region in place ([above](#no-full-reload-after-a-mutation)).
- Forgetting to add new template directories to Tailwind's `@source`, so classes go missing in production.
