# Frontend: `htmx` mode (Hotwire → htmx)

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

The version lives in the filename. `kernel/assets.rs` finds the files by prefix (`htmx-2`, `htmx-ext-sse-`). Verified 2026-10: htmx 2.0.11, htmx-ext-sse 2.2.4.

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
// static/app.js: each behavior is one delegated listener, keyed by a data attribute
document.addEventListener("click", (e) => {
  const el = e.target.closest("[data-toggle]");
  if (el) document.getElementById(el.dataset.toggle)?.classList.toggle("hidden");
});
document.addEventListener("htmx:afterSwap", () => { /* re-init anything that needs it */ });
```

If a behavior outgrows ~50 lines or needs real client state, it's an island.

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

- At boot, hash every file under `static/` (SHA-256, first 8 hex chars). `asset_path(prefix)` returns `/static/<file>?v=<hash>` for the first file whose relative path starts with `prefix`, so versioned vendor files match by prefix: `css` = `app.css`, `htmx` = `vendor/htmx-`, `htmx_sse` = `vendor/htmx-ext-sse-`, `app_js` = `app.js`, `admin_css` = `kernel/admin.css` (the kernel pages' stylesheet).
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

- `kernel/assets.rs` reads `web/dist/.vite/manifest.json` for `src/islands/mount.tsx` (`file`, plus `css` if any) and emits one `<script type="module">`. Lazy island chunks load themselves. In dev, it emits the React refresh preamble script and `http://localhost:5173/@vite/client` + `http://localhost:5173/src/islands/mount.tsx`, exactly as in `inertia` mode.
- Islands talk to the server through JSON endpoints under `/api` (`controllers.md#json-api`).

## Gotchas agents get wrong

- **A form clicked before htmx initializes still works.** It submits as a normal POST (303 → full page), so boosted forms degrade safely. During the reference app's first browser run, one sign-up click appeared aborted. It didn't reproduce, either with a normal click or with a click fired at `DOMContentLoaded`. If a browser smoke shows `htmx:abort`, look for a second navigation racing the click (another tool driving the same tab) before changing app code.

- **A boosted form that JavaScript sends some other way** (a channel room's message box, [channels](channels.md#the-js-client)). `preventDefault` in your listener doesn't stop htmx's boosted submit, so the page also navigates (to `?body=…`). Put `hx-boost="false"` on the form. Found by the browser check: the message posted *and* the page reloaded.
- Missing the 422 `responseHandling` config, so invalid forms look like dead buttons. The same goes for 429 (rate limits) and for 5xx on boosted requests (the `app.js` error-page handler above).
- **Dropping the CSP pairing from `htmx-config`.** The CSP (`style-src 'self'; script-src 'self'`) and htmx's config must agree: `includeIndicatorStyles:false` (otherwise htmx injects a `<style>` at load and Chrome logs "Applying inline style violates … style-src 'self'"; verified with htmx 2.0.11 + Chrome, 2026-10-12) and `allowEval:false` (no violation on load, but it turns `hx-on`, trigger filters `[…]` and `js:` values into silent no-ops instead of CSP errors; never use those). Every layout that loads htmx, including the kernel layout, carries both.
- **Boosting links into kernel pages.** `<body hx-boost="true">` swaps only `<body>`, so a boosted link to `/dev`, `/dev/mailbox` or `/admin/jobs` (pages with their own `<head>` and stylesheet) renders unstyled until a hard refresh. Put `hx-boost="false"` on every such link ([dev-tools](dev-tools.md#rules)). The same applies to any page that doesn't use the app layout.
- Returning a fragment to an `hx-boost` request, which replaces the whole body with a fragment.
- Loading htmx from a CDN. It's vendored, and the CSP forbids third-party scripts.
- Writing htmx 1.x syntax (`hx-ws`, `hx-sse` attributes). htmx 2 uses extensions: `hx-ext="sse"` + `sse-connect` + `sse-swap`, with the separately vendored `htmx-ext-sse` script loaded after htmx.
- Pulling htmx `@latest`/`next`. That could silently move to htmx 4. Resolve `@2`.
- Fetching Tailwind from `releases/latest` in `bin/setup`. Pin `TAILWIND_VERSION` so dev, CI and Docker build identical CSS.
- Inline `onclick=` handlers. The CSP blocks them. Use `static/app.js`.
- Forgetting to add new template directories to Tailwind's `@source`, so classes go missing in production.
