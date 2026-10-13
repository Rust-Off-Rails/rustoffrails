# Frontend: `inertia` mode (inertia-rails → `kernel/inertia.rs`)

> **TL;DR**
> - React (or Svelte) pages with Rails-style server routing; the adapter is your own `kernel/inertia.rs`, generated from the protocol spec here. No client router, no client fetching: data arrives as props.
> - Props are `#[derive(Serialize, TS)]` structs exported by ts-rs with `TS_RS_LARGE_INT=number`; never hand-write TS types.
> - `useForm` sends JSON (take `Json<…>`); validation errors redirect back with the `_errors` cookie; PATCH/DELETE redirect with 303.
> - Send `Vary: X-Inertia`; asset-version mismatch on GET is 409; type-check with `tsc -b`, never `tsc --noEmit`.
>
> Read the rest when wiring Inertia, Vite, props, or Inertia tests.

React (or Svelte: `UI_LIBRARY: svelte`, see [frontend-svelte](frontend-svelte.md#inertia-mode), which replaces only the `web/` files) + TypeScript pages, with **server-side routing, controllers, sessions and validation kept Rails-style**. No client router, no API layer, no client data fetching. A handler returns a *page*: a component name plus typed props.

There is no Inertia crate. You generate `kernel/inertia.rs` (~200 lines) from the protocol spec below.

> **Verified 2026-10-12** end to end in the reference app (branch `inertia`) with a real browser: first visit, Inertia navigation, `useForm` validation round-trip, flash, PATCH/DELETE, partial reloads, the 409 on a stale deploy, error pages, HMR through the Rust server, production build behind the strict CSP, and the Docker image. Versions: `@inertiajs/react` + `@inertiajs/core` 3.9.1, React 19.2, Vite 8.3, `@vitejs/plugin-react` 6.1, Tailwind 4.3, TypeScript 6.0, Vitest 5.0, Playwright 1.64, ts-rs 12.0. Protocol: https://inertiajs.com/the-protocol. If `web/package.json` resolves a different Inertia major, read that version's protocol page and upgrade guide first, follow the official protocol where they disagree, and log the difference in `docs/decisions.md`.

## The protocol (what `kernel/inertia.rs` implements)

**Page object** (field names are exact):

```json
{ "component": "posts/Index", "props": { "errors": {}, … }, "url": "/posts?page=2", "version": "a1b2c3d4",
  "flash": { "kind": "notice", "message": "Post created." } }
```

- `component`, `props`, `url`, `version` are always present. `props.errors` is always present (`{}` when empty).
- `flash` is a **top-level page field** in v3, not a prop. Include it only when there is one. The client defaults it to `{}` when absent (so `usePage().flash` is never null, and a falsy check on it is always false) and fires a `flash` event when it's non-empty.
- Optional fields the adapter may emit later: `deferredProps` (`{ "group": ["prop", …] }`), `mergeProps`, `encryptHistory`/`clearHistory` (emit only when `true`). Don't emit fields you don't implement.

| Situation | Server response |
|-----------|-----------------|
| First visit (no `X-Inertia` header) | `200` HTML: `templates/layouts/application.html` with `<script data-page="app" type="application/json">{{ page_json\|safe }}</script><div id="app"></div>`. `page_json` is `serde_json::to_string(&page)?.replace('<', "\\u003c")`, so `</script>` and `<!--` can't break out. **Don't HTML-escape it.** Entities aren't decoded inside `<script>`. `data-page` as a `div` attribute is v2 and is no longer read |
| Inertia visit (`X-Inertia: true`) | `200` JSON page object, with headers `X-Inertia: true` and `Vary: X-Inertia` (also add `Vary: X-Inertia` to the HTML response) |
| `GET` with `X-Inertia-Version` ≠ current version | `409` with `X-Inertia-Location: <request path + query>` and `X-Inertia-Version: <current>`. No `X-Inertia` header. Non-GET requests never get a version 409. **Keep the flash cookie** on a 409 (don't consume it), so it survives the reload. The client then does a full page load of that URL |
| Redirect after `POST`/`PUT`/`PATCH`/`DELETE` | Must be `303` (`Redirect::to` already is) |
| Validation failure (`AppError::Invalid`) | `303` back to the same-origin `Referer` (else `/`) with the errors stashed in the `_errors` cookie. Never a 422 ([below](#validation-errors-redirect-back)) |
| `403`/`404`/`500` on an Inertia visit | The `errors/Show` page as JSON (`X-Inertia: true`) **with the real status**: `{ "component": "errors/Show", "props": { "status": 404, "errors": {} }, … }`. Without this Inertia shows the raw HTML error response in a modal. First visits keep the standalone `templates/errors/*.html` documents |
| `401` | `303` to the login page, as in every mode |
| Redirect to an external URL / force a full reload | `409` + `X-Inertia-Location: <url>` (`inertia::location(url)`) |
| Partial reload: `X-Inertia-Partial-Component` == this component | `X-Inertia-Partial-Data` (comma-separated) keeps only those props; then `X-Inertia-Partial-Except` removes props. `errors` is an *always* prop and ignores both filters. If the component differs, ignore the partial headers and send everything |

**Asset version:** the first 16 hex chars of the SHA-256 of `web/dist/.vite/manifest.json`, computed once at startup. In development, the constant `"dev"`. Without a manifest (tests that never built the frontend), `"none"`.

**CSRF header:** Inertia's built-in HTTP client (v3 dropped axios) sends `X-XSRF-TOKEN` only if an `XSRF-TOKEN` cookie exists. We set none; `CsrfLayer`'s `Sec-Fetch-Site`/`Origin` check covers it (see below).

## The adapter's Rust surface

`src/kernel/inertia.rs`:

```rust
/// `P` is the app's shared props, built by its own extractor (keeps app types out of the kernel).
pub struct Inertia<P> { is_inertia: bool, url: String, partial: Option<Partial>, flash: Option<Flash>, vite: Vite, shared: P }

impl<P: FromRequestParts<AppState> + Send> FromRequestParts<AppState> for Inertia<P> { /* headers, url, Flash extension, state.vite */ }

impl<P: Serialize> Inertia<P> {
    /// Serializes props to a JSON object, merges them over the shared props (page props win),
    /// adds `errors: {}` if absent, applies partial-reload filtering, responds JSON or HTML.
    pub fn render(self, component: &str, props: impl Serialize) -> Result<Response, AppError>;
    /// `InertiaRails.optional`: true on full renders, or when a partial reload asks for `prop`.
    pub fn wants(&self, prop: &str) -> bool;
}

pub fn location(url: &str) -> Response;                                       // 409 + X-Inertia-Location
pub fn error_page(version: &str, url: &str, status: StatusCode) -> Response;  // errors/Show, used by shape_errors
pub async fn version(State(state): State<AppState>, req: Request, next: Next) -> Response; // the 409 middleware

#[derive(Serialize, ts_rs::TS)] #[ts(export)]
pub struct ErrorPageProps { pub status: u16 }
```

The HTML shell is an Askama template struct inside the kernel: `#[template(path = "layouts/application.html")] struct Shell<'a> { page_json: String, vite: &'a Vite }`.

In the app, `controllers/application.rs`:

```rust
/// `inertia_share`: props every page gets. The authentication generator adds `current_user`.
/// The flash is not here: v3 sends it as the page's top-level `flash`.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SharedProps {
    pub errors: Errors,
}

impl FromRequestParts<AppState> for SharedProps {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, Self::Rejection> {
        Ok(Self {
            errors: parts.extensions.get::<StashedErrors>().map(|e| e.0.clone()).unwrap_or_default(),
        })
    }
}

/// What handlers extract: `inertia.render("books/Index", props)`.
pub type Inertia = kernel::inertia::Inertia<SharedProps>;
```

Handlers (the verified `controllers/books.rs` shape):

```rust
pub async fn index(State(s): State<AppState>, inertia: Inertia, page: Page) -> Result<Response, AppError> {
    let books = book::recent(&s.db, page).await?;
    inertia.render("books/Index", BooksIndexProps { books: books.iter().map(BookJson::from).collect() })
}

pub async fn create(
    State(s): State<AppState>,
    Json(params): Json<BookParams>, // useForm sends JSON, not a form body; body extractor LAST
) -> Result<Response, AppError> {
    let new = params.validate().map_err(AppError::Invalid)?; // shape_errors turns this into the redirect back
    let book = book::create(&s.db, new).await?;
    Ok((Flash::notice("Book added."), Redirect::to(&paths::book(book.id))).into_response())
}
// update: find (404 first) → validate → book::update → Flash + Redirect::to(&paths::book(id))
// destroy: find → book::destroy → Flash + Redirect::to(&paths::books())
```

Wiring in `routes.rs` (layers run bottom-up; the version check must sit **outside** the flash middleware, so a 409 never consumes the flash):

```rust
.layer(from_fn(flash::middleware))
.layer(from_fn_with_state(state.clone(), inertia::version))
.layer(csrf)
.layer(from_fn_with_state(state.clone(), error::shape_errors))
```

`AppState` gets `vite: Vite`: `Vite::dev()` in development, otherwise `Vite::from_manifest()?`. `from_manifest` never fails on a missing file (tests build `AppState` with production config and no frontend build); instead `main.rs`'s `serve` refuses to start in production when `!state.vite.is_built()`.

Client typing, `web/src/inertia.d.ts` (the only hand-written `.d.ts`; it only *references* generated types). Without it, `usePage().props.current_user` is `unknown`:

```ts
import type { Flash } from "@/types/Flash";
import type { SharedProps } from "@/types/SharedProps";

declare module "@inertiajs/core" {
  export interface InertiaConfig {
    sharedPageProps: SharedProps;
    flashDataType: Partial<Flash>; // the client defaults page.flash to {} when the server sends none
    errorValueType: string[]; // Errors is BTreeMap<String, Vec<String>>
  }
}
```

`Flash`, `FlashKind` (in `kernel/flash.rs`) and `Errors` (in `models/mod.rs`) derive `ts_rs::TS` with `#[ts(export)]`. `Errors` also derives `Deserialize`, because it travels in the `_errors` cookie.

## Validation errors: redirect back

`useForm` expects the Rails/Laravel flow: on failure, **redirect back** and expose errors as the `errors` prop.

- `kernel/flash.rs` owns both read-once cookies: `_flash` (`Flash`) and `_errors` (`StashedErrors(Errors)`, which implements `IntoResponseParts`). Its middleware puts each into the request extensions, and clears each one after a *rendered* response. It never clears on a redirect or a `409`.
- `error::shape_errors` records the same-origin `Referer` path **before** running the request (strip the `APP_URL` prefix; anything else, or `//…`, becomes `/`). On an `ErrorReport` with errors for an Inertia request, it returns `(StashedErrors(errors), Redirect::to(&back))`.
- The next render's `SharedProps` extractor reads `StashedErrors`, so `form.errors.title` lights up on the client, and the form keeps its input (same component, so `useForm` state survives).
- Don't return 422 to Inertia requests. Return a page or a redirect.

## Routes

Inertia's router sends real verbs, so resources in this mode use them (unlike `htmx` mode's POST-only shape):

| Action | Route |
|--------|-------|
| index / create | `GET /posts` / `POST /posts` |
| new | `GET /posts/new` |
| show / update / destroy | `GET /posts/{id}` / `PATCH /posts/{id}` / `DELETE /posts/{id}` |
| edit | `GET /posts/{id}/edit` |

`.route("/posts/{id}", get(posts::show).patch(posts::update).delete(posts::destroy))`. `ROUTES` lists each verb.

## Props & pages

- Props structs live in `src/views/<plural>.rs` (`PostsIndexProps`, `PostsEditProps`), deriving `Serialize` + `ts_rs::TS` with `#[ts(export)]`. Nested data uses `views::<Model>Json` structs. **Never put an entity `Model` in props.** Forms get the params struct: `PostsNewProps { post: PostParams }`, `PostsEditProps { id: i64, post: PostParams }` (`PostParams::from(&model)` prefills).
- Pages: component `"posts/Index"` ↔ `web/src/pages/posts/Index.tsx`, default export, typed with generated types:

```tsx
import { Link } from "@inertiajs/react";
import { Layout } from "@/components/Layout";
import { paths } from "@/paths";
import type { PostsIndexProps } from "@/types/PostsIndexProps";

export default function Index({ posts }: PostsIndexProps) {
  return (
    <Layout>
      <ul>{posts.map((p) => <li key={p.id}><Link href={paths.post(p.id)}>{p.title}</Link></li>)}</ul>
    </Layout>
  );
}
```

- **Only pages live under `pages/`.** The eager page glob turns every `.tsx` there into a page, and tests can't live there. The shared form is `web/src/components/<plural>/<Model>Form.tsx` (named export, takes `form: InertiaForm<PostParams>`, `submitLabel`, `onSubmit`), with its test next to it.
- `New.tsx`/`Edit.tsx`: `const form = useForm(post);` (typed from the generated `PostParams`), then `form.post(paths.posts())` / `form.patch(paths.post(id))`. Delete: `router.delete(paths.post(id))` after a `confirm()`.
- Error page: `web/src/pages/errors/Show.tsx`, typed with the generated `ErrorPageProps`, rendered in the `Layout` with a link home.
- Layout: `web/src/components/Layout.tsx` renders the nav, `<Flash />` and `usePage().props.current_user`. Pages wrap themselves in it, or assign a persistent layout as `Index.layout = [Layout]`. In v3 an arrow function assigned directly to `.layout` no longer works.
- `components/Flash.tsx`: `const { flash } = usePage(); if (!flash.message) return null;`. Check `message`, never `flash` itself, which is `{}` when empty.
- Paths: mirror `routes::paths` as `web/src/paths.ts`, one camelCase function per helper (`paths.editBook(id)`).

## Vite wiring (verified with Vite 8, `@vitejs/plugin-react` 6, Tailwind 4)

Scaffold: `pnpm create vite web --template react-ts` (Node 24 LTS; pnpm via `corepack enable pnpm`). Then:

- Delete the template's demo files: `src/App.*`, `src/index.css`, `src/assets/`, `public/`, `README.md`, and `index.html` (the Rust layout is the HTML shell).
- `pnpm remove oxlint` (the template's linter), then:
  - `pnpm add @inertiajs/react @inertiajs/core` (core at the **same version** as the React adapter; the type augmentation needs it as a direct dependency under pnpm)
  - `pnpm add -D tailwindcss @tailwindcss/vite @biomejs/biome vitest jsdom @testing-library/react @testing-library/jest-dom @testing-library/user-event`
- Pin the package manager: `pnpm pkg set "packageManager=pnpm@$(pnpm --version)"`.
- `pnpm exec biome init`, then in `biome.json` set `"files": { "includes": ["**", "!dist", "!src/types", "!test-results", "!playwright-report"] }`, `formatter.indentStyle: "space"`, and `vcs.enabled: false` (web/ has no .git of its own, so Biome can't read `.gitignore`: everything generated must be excluded by hand).
- `tsconfig.app.json` `compilerOptions`: add `"strict": true`, `"noUncheckedIndexedAccess": true`, and `"paths": { "@/*": ["./src/*"] }`. The template's root `tsconfig.json` uses project references, so **type-check with `tsc -b`**. Plain `tsc --noEmit` at the root checks nothing and always passes.
- `package.json` scripts: **replace** the template's (its `lint` runs oxlint): `"dev": "vite"`, `"build": "tsc -b && vite build"`, `"typecheck": "tsc -b"`, `"test": "vitest"`, `"lint": "biome check ."`.
- `.cargo/config.toml` (repo root):

```toml
[env]
TS_RS_EXPORT_DIR = { value = "web/src/types", relative = true }
# i64 ids arrive as JSON numbers, not BigInt (ts-rs defaults i64 to `bigint`, which type-checks but is wrong at runtime).
TS_RS_LARGE_INT = "number"
```

- **When `.cargo/config.toml` can't be written** (a sandbox that blocks dotfiles, a monorepo whose root config belongs to someone else): put both settings on the types instead, and log it in `docs/decisions.md`. Checked against ts-rs 12.0.1 (`ts-rs-macros` field attributes `type`/`as`; `export_to` is relative to `./bindings` when `TS_RS_EXPORT_DIR` is unset):

  ```rust
  #[derive(Serialize, TS)]
  #[ts(export, export_to = "../web/src/types/")]   // = web/src/types/BookJson.ts (SvelteKit: "../web/src/lib/types/")
  pub struct BookJson {
      #[ts(type = "number")] pub id: i64,
      #[ts(type = "number | null")] pub cover_blob_id: Option<i64>,
      #[ts(type = "Array<number>")] pub tag_ids: Vec<i64>,
      pub title: String,
  }
  ```

  Every `i64`/`u64` field of every exported type needs its override, or that one field is `bigint`. Check after each export: `grep -rn bigint web/src/types/` prints nothing.
- **Commit `web/src/types/`.** ts-rs writes it during `cargo test`, but the Docker `web` stage and a fresh checkout run `tsc -b` before any Rust test. CI checks it's current with `git diff --exit-code web/src/types` after `cargo nextest run`.

`vite.config.ts`:

```ts
import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: {
    port: 5173,
    strictPort: true,
    origin: "http://localhost:5173", // asset URLs in CSS/JS point at Vite, not the Rust server
    // No `cors` option: Vite's default already allows any localhost origin, so any PORT works.
  },
  build: { manifest: true, rolldownOptions: { input: "src/main.tsx" } }, // Vite 8: rollupOptions is deprecated
  test: { environment: "jsdom", setupFiles: ["src/test-setup.ts"], include: ["src/**/*.test.{ts,tsx}"] },
});
```

Don't hard-code `cors: { origin: "http://localhost:3000" }`: with any other `PORT`, every module script is blocked by CORS and the page stays blank. The `include` keeps Vitest away from `e2e/` (Playwright specs).

`web/src/test-setup.ts` (test globals are off, so Testing Library can't register its own cleanup; without this, renders leak between tests and queries find duplicates):

```ts
import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(cleanup);
```

`web/src/main.tsx` (manual setup; we don't use the optional `@inertiajs/vite` plugin, which targets Laravel's Vite plugin):

```tsx
import "./app.css";
import { createInertiaApp } from "@inertiajs/react";
import type { ComponentType } from "react";
import { createRoot } from "react-dom/client";

const pages = import.meta.glob<{ default: ComponentType }>("./pages/**/*.tsx", { eager: true });

createInertiaApp({
  resolve: (name) => {
    const page = pages[`./pages/${name}.tsx`];
    if (!page) throw new Error(`Unknown page: ${name}`);
    return page;
  },
  setup({ el, App, props }) {
    createRoot(el).render(<App {...props} />);
  },
  progress: { includeCSS: false }, // its injected <style> violates the CSP; the bar's CSS lives in app.css
});
```

`web/src/app.css`: `@import "tailwindcss";`, followed by the progress bar CSS that `includeCSS: false` turns off. This is copied from `@inertiajs/core` 3.9 (`injectCSS`), with the color as a Tailwind variable. The bar positions itself through `element.style`, which the CSP allows:

```css
#nprogress { pointer-events: none; background: none; border: none; margin: 0; padding: 0; overflow: visible; inset: unset; width: 100%; height: 0; position: fixed; top: 0; left: 0; }
#nprogress::backdrop { display: none; }
#nprogress .bar { background: var(--color-indigo-600); position: fixed; z-index: 1031; top: 0; left: 0; width: 100%; height: 2px; }
#nprogress .peg { display: block; position: absolute; right: 0; width: 100px; height: 100%; box-shadow: 0 0 10px var(--color-indigo-600), 0 0 5px var(--color-indigo-600); opacity: 1; transform: rotate(3deg) translate(0, -4px); }
#nprogress .spinner { display: block; position: fixed; z-index: 1031; top: 15px; right: 15px; }
#nprogress .spinner-icon { width: 18px; height: 18px; box-sizing: border-box; border: solid 2px transparent; border-top-color: var(--color-indigo-600); border-left-color: var(--color-indigo-600); border-radius: 50%; animation: nprogress-spinner 400ms linear infinite; }
.nprogress-custom-parent { overflow: hidden; position: relative; }
.nprogress-custom-parent #nprogress .spinner, .nprogress-custom-parent #nprogress .bar { position: absolute; }
@keyframes nprogress-spinner { 0% { transform: rotate(0deg); } 100% { transform: rotate(360deg); } }
```

### The HTML shell and `kernel/assets.rs`

`templates/layouts/application.html` (the whole file):

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Bookshelf</title>
  <link rel="icon" href="data:,">
  {% for href in vite.styles %}<link rel="stylesheet" href="{{ href }}">
  {% endfor %}{% for href in vite.preloads %}<link rel="modulepreload" href="{{ href }}">
  {% endfor %}{% for src in vite.scripts %}<script type="module" src="{{ src }}"></script>
  {% endfor %}
</head>
<body class="min-h-screen bg-white text-gray-900 dark:bg-gray-950 dark:text-gray-100 antialiased">
  <script data-page="app" type="application/json">{{ page_json|safe }}</script>
  <div id="app"></div>
</body>
</html>
```

(`data:,` stops the browser's `/favicon.ico` 404; replace it with a real icon under `static/` when there is one.)

`kernel/assets.rs` adds, next to the htmx asset catalog (which the kernel pages still use):

- `pub struct Vite { styles: Vec<String>, preloads: Vec<String>, scripts: Vec<String>, version: String }`.
- **Development:** `Vite::dev()`. The scripts are, in this order:
  1. `/dev/vite-preamble.js`: the React refresh preamble, served from a const string by `assets::vite_preamble` (`Content-Type: text/javascript`) and mounted **in development only**, next to the other `/dev` routes. It's `import RefreshRuntime from "http://localhost:5173/@react-refresh"; RefreshRuntime.injectIntoGlobalHook(window); window.$RefreshReg$ = () => {}; window.$RefreshSig$ = () => (type) => type; window.__vite_plugin_react_preamble_installed__ = true;`. As a same-origin file it keeps the CSP free of inline scripts. Importing `@vitejs/plugin-react/preamble` from `main.tsx` does **not** work: the eager page glob is hoisted above it.
  2. `http://localhost:5173/@vite/client`
  3. `http://localhost:5173/src/main.tsx`

  `version` is `"dev"`. You **browse :3000** (the Rust server). Vite on :5173 serves modules and HMR.
- **Production/test:** `Vite::from_manifest()` reads `web/dist/.vite/manifest.json` for the entry `src/main.tsx`:
  - `scripts` is `/` + its `file`;
  - `styles` is the `css` of the entry and of every chunk reachable through `imports`, recursively;
  - `preloads` are the imported chunks' `file`s;
  - `version` is the manifest hash.
- `routes.rs` serves `web/dist/assets` at `/assets` through `Router::new().fallback_service(ServeDir::new("web/dist/assets")).layer(from_fn(assets::immutable))`, which sends `Cache-Control: public, max-age=31536000, immutable`.
- Dev CSP adds `http://localhost:5173` to `script-src`, `style-src` and `connect-src`, `ws://localhost:5173` to `connect-src`, and `'unsafe-inline'` to `style-src` (Vite injects CSS as `<style>` in dev) ([security](security.md)). Production CSP has none of these.

`bin/setup` runs `pnpm --dir web install --frozen-lockfile` (after checking for `node` and `pnpm`). `bin/dev` runs `pnpm --dir web dev &` next to `cargo watch`. The Dockerfile's `web` stage builds `web/dist` ([deployment](deployment.md#dockerfile-shape)).

## Testing in `inertia` mode

Request tests assert on the **page object**, not HTML ([test-standards](test-standards.md)). `TestApp` (tests/app/support.rs) adds:

```rust
app.inertia_get("/books").await                       // X-Inertia + the app's current version
app.inertia_send("PATCH", "/books/1", "/books/1/edit", json!({ … })).await
                                                      // JSON body, X-Inertia, same-origin, Referer = APP_URL + from_page
res.page()                                            // the page object (from JSON, or parsed out of the shell)
res.props()                                           // page["props"]
res.assert_component("books/Index")
```

- The validation flow is two requests: `inertia_send(...)` → `assert_redirect_to(form_page)`, then `inertia_get(form_page)` → `props()["errors"]`. A third request proves the errors are read once.
- Flash: `inertia_get(next_page).page()["flash"]`.
- Always send `app.state.vite.version`, never a literal. It's `"none"` without `web/dist` and a hash with it, and a stale value gets a 409.
- Protocol tests (`tests/app/inertia_test.rs`), generated with the kernel:
  - JSON headers and `Vary`;
  - 409 with the flash cookie untouched, and no 409 on POST;
  - partial `only`/`except`/other component;
  - an off-origin `Referer` redirects to `/`;
  - `errors/Show` with a 404;
  - the dev shell and the preamble;
  - a production CSP without `unsafe-inline`.
- Vitest: the `<Model>Form` (values, errors under fields, `setData` on typing, submit, disabled while `processing`) and `Flash` (renders nothing for `{}`). Mock `@inertiajs/react`'s `usePage` with `vi.hoisted` + `vi.mock`.
- Playwright (the critical flow): `web/playwright.config.ts` with `testDir: "e2e"`, `retries: 0`, and a `webServer` that runs `pnpm build && cd .. && rm -f storage/e2e.sqlite3* && cargo run --quiet -- db migrate && cargo run --quiet -- serve`. Its env is `APP_ENV=test`, `PORT=3200`, `APP_URL=http://localhost:3200` and `DATABASE_URL=sqlite://storage/e2e.sqlite3?mode=rwc`, and its `url` is `/up`. Add `"e2e": "playwright test"` and list `playwright.config.ts` and `e2e` in `tsconfig.node.json`'s `include`. Fail the spec on any console error, which catches CSP violations.

## CSRF

Same-origin cookie sessions + `CsrfLayer` (`Sec-Fetch-Site`/`Origin`, `security.md`). No `XSRF-TOKEN` cookie is set, so Inertia's HTTP layer sends none, and none is needed.

## inertia-rails → Off Rails

| inertia-rails | Off Rails |
|---------------|-----------|
| `render inertia: "Posts/Index", props: {…}` | `inertia.render("posts/Index", PostsIndexProps { … })` |
| `inertia_share` | `SharedProps` (errors, + current_user) + top-level `flash` |
| `redirect_to … , inertia: { errors: }` | `AppError::Invalid` → `_errors` cookie + 303 back (in `shape_errors`) |
| `InertiaRails.optional` / `defer` | Partial reloads; expensive props as `Option<T>` filled only when `inertia.wants("prop")`. Deferred props (`deferredProps`) only when a spec needs them |
| `inertia_location` | `inertia::location(url)` → 409 + `X-Inertia-Location` |
| error page via `rescue_from` | `inertia::error_page` → `errors/Show` with the real status |
| version from ViteRuby | hash of `.vite/manifest.json` |

## Gotchas agents get wrong

- Hand-writing TS prop types. They come from `ts-rs`, in `web/src/types/`, which is committed.
- ts-rs's default `bigint` for `i64`. Set `TS_RS_LARGE_INT = "number"`, or every id is typed as `bigint` while the runtime has a `number`.
- Fetching data with `fetch`/TanStack Query in a page. In Inertia mode, data arrives as props. Reload with `router.reload({ only: [...] })`.
- Taking `Form<…>` in a handler. `useForm` sends JSON: take `Json<…>`.
- Returning 302 after PATCH/DELETE. It must be 303, or the browser repeats the verb.
- Forgetting `Vary: X-Inertia`, so caches serve JSON to a full page load.
- Embedding the page as a `data-page` attribute on `<div id="app">` (v2). v3 reads `<script data-page="app" type="application/json">`. HTML-escaping that JSON breaks it; replacing `<` with `<` is what keeps it safe.
- Mounting the version middleware inside the flash middleware: the 409 then eats the flash.
- Sending HTML error pages to Inertia visits (they show up in a modal). Use `errors/Show`.
- Forgetting `progress: { includeCSS: false }`, so the CSP blocks the progress bar's injected styles.
- Hard-coding Vite's `cors` origin to `:3000`. Leave Vite's default.
- Type-checking with `tsc --noEmit` (it checks nothing with project references). Use `tsc -b`.
- Putting flash in props, or testing `if (!flash)`. In v3 it's the page's `flash` field (`usePage().flash`), `{}` when empty.
- Putting the form (or any test) under `pages/`.
- Requiring the Vite manifest in `AppState::new`. That breaks every test that builds a production config. Check it in `serve`.
- Component name casing drift: `"posts/Index"` must match the file path exactly.
