# Frontend: Svelte (`UI_LIBRARY: svelte`)

> **TL;DR**
> - `UI_LIBRARY: svelte`: Svelte 5 in inertia, spa (SvelteKit in SPA mode) and islands; the mode still decides the architecture.
> - Type-check with `svelte-check --fail-on-warnings` (`tsc -b` skips `.svelte` files); Biome needs its full-Svelte support turned on.
> - Inertia: use `useForm(data)` + `form.submit(method, url)`, not the precognition signature; read `page.flash` directly.
> - `spa` mode is client-only: no `+page.server.ts`, `+server.ts`, form actions or server hooks; Rust is the only server.
>
> Read the rest when writing Svelte pages, islands, or SvelteKit routing.

Svelte 5 is the second blessed UI library, next to React. The **mode** (`FRONTEND_MODE`) still decides the architecture, and the **library** (`UI_LIBRARY`) decides what the components are written in:

| `FRONTEND_MODE` | `UI_LIBRARY: react` | `UI_LIBRARY: svelte` |
|-----------------|---------------------|----------------------|
| `inertia` | `@inertiajs/react` ([frontend-inertia](frontend-inertia.md)) | `@inertiajs/svelte` ([below](#inertia-mode)) |
| `spa` | Vite + React Router + TanStack Query ([frontend-react](frontend-react.md)) | **SvelteKit in SPA mode** + TanStack Query ([below](#spa-mode-sveltekit)) |
| `htmx` + islands | React islands ([frontend-htmx](frontend-htmx.md#islands)) | Svelte islands ([below](#islands)) |

Everything server-side is identical across libraries: the Rust routes, controllers, ts-rs types, the Inertia protocol, the `/api` contract, sessions and the CSP. Only `web/` differs.

**When to choose Svelte:** the human asks for it, or a small bundle matters. Measured on the same Inertia app: **54 KB gzipped (Svelte) vs 108 KB (React)**, and a Svelte island's runtime is about 11 KB gzipped. **React stays the default** when the human has no preference, because more component examples and libraries exist for it.

Verified 2026-10-12 with Node 24, pnpm 12, Vite 8.3, Svelte 5.57, `@sveltejs/vite-plugin-svelte` 7.3, `@inertiajs/svelte` 3.9.1, SvelteKit 3.0 + `@sveltejs/adapter-static` 4.0, `@tanstack/svelte-query` 6.3, svelte-check 4.7, `@testing-library/svelte` 5.4, Biome 2.5.

## Shared rules (every mode)

- **Svelte 5 runes only:** `$props()`, `$state()`, `$derived()`, `{@render children()}`, and `onclick={…}` event attributes. No `export let`, no `$:`, no `on:click`, and no slots.
- **Type-check with `svelte-check`**, never `tsc` alone, because `tsc` can't see inside `.svelte` files. The `typecheck` script is `svelte-check --tsconfig ./tsconfig.app.json --fail-on-warnings && tsc -p tsconfig.node.json`. It was proven to catch type errors in both `.svelte` and `.ts` files. The Definition of Done runs `pnpm --dir web typecheck` in every TS mode.
- **Lint and format with Biome, with its full Svelte support turned on.** In its default mode Biome sees only the `<script>` block, so variables used only in the template get flagged as unused (false positives). Add this to `biome.json`:

  ```json
  "html": {
    "experimentalFullSupportEnabled": true,
    "formatter": { "enabled": true, "indentScriptAndStyle": true }
  }
  ```

  With that, Biome lints the template (including a11y rules such as `useButtonType`) and formats it. It reported zero false positives on the reference app, and its output compiles. Its markup formatting is whitespace-preserving like Prettier's (`>` may wrap onto the next line). That's expected; don't fight it. **No Prettier and no ESLint.**
- **Tests:** Vitest + `@testing-library/svelte` + `@testing-library/jest-dom`. Add the `svelteTesting()` plugin from `@testing-library/svelte/vite` to `vite.config.ts`. In Vitest it resolves Svelte's browser build and cleans the DOM after each test, so `test-setup.ts` only imports `@testing-library/jest-dom/vitest`. Name a test file `*.svelte.test.ts` when it needs runes (`$state`) for reactive fakes.
- **Types:** the same `ts-rs` exports as React (`TS_RS_LARGE_INT = "number"` in `.cargo/config.toml`). Inertia mode puts them in `web/src/types/`, SvelteKit in `web/src/lib/types/`.
- **Components own their state.** A child must not write into a parent's `$state` (for example through `bind:value={form.title}` on a prop). Svelte warns `ownership_invalid_mutation` in the browser console, and tests don't catch it. Either the component that binds the inputs owns the state (the form pattern below), or the parent passes it with `bind:` to a `$bindable()` prop.
- **Initial values from props:** `let count = $state(start)` deliberately captures a prop once and makes svelte-check warn `state_referenced_locally`. When that's the intent, as with island props or form initial values, put `// svelte-ignore state_referenced_locally` on the line above, with a comment saying why.

## `inertia` mode

Everything in [frontend-inertia.md](frontend-inertia.md) applies (the protocol, the Rust adapter, the shell, `paths.ts`, the `_errors` cookie, the 409 version check, tests asserting props) except the files below. Scaffold with `pnpm create vite@latest web --template svelte-ts --no-interactive`, then:

- Packages: `pnpm add @inertiajs/svelte @inertiajs/core svelte` and `pnpm add -D @sveltejs/vite-plugin-svelte svelte-check @tsconfig/svelte @tailwindcss/vite tailwindcss @biomejs/biome vitest jsdom @testing-library/svelte @testing-library/jest-dom @testing-library/user-event @playwright/test`. Keep the template's `svelte.config.js` (`export default {}`).
- **Rust side:** `VITE_ENTRY = "src/main.ts"`. There is **no** refresh preamble and no `/dev/vite-preamble.js` route: that's React Fast Refresh, and Svelte's HMR needs nothing. In development the shell links only `@vite/client` and the entry.
- `vite.config.ts`: `plugins: [svelte(), tailwindcss(), svelteTesting()]`, `build: { manifest: true, rolldownOptions: { input: "src/main.ts" } }`, and the same `server` block and `@` alias as React. `test.include: ["src/**/*.test.ts"]`.
- `tsconfig.app.json` extends `@tsconfig/svelte/tsconfig.json`, keeps `strict`, `noUncheckedIndexedAccess` and the `@/*` path, and includes `src/**/*.ts` and `src/**/*.svelte`. The `inertia.d.ts` augmentation of `@inertiajs/core` (`sharedPageProps`, `flashDataType: Partial<Flash>`, `errorValueType: string[]`) is unchanged.
- `web/src/main.ts`:

```ts
import "./app.css";
import { createInertiaApp } from "@inertiajs/svelte";
import { type Component, mount } from "svelte";

const pages = import.meta.glob<{ default: Component }>("./pages/**/*.svelte", {
  eager: true,
});

createInertiaApp({
  resolve: (name) => {
    const page = pages[`./pages/${name}.svelte`];
    if (!page) throw new Error(`Unknown page: ${name}`);
    return page;
  },
  setup({ el, App, props }) {
    if (el) mount(App, { target: el, props });
  },
  progress: { includeCSS: false }, // its injected <style> violates the CSP; the bar's CSS lives in app.css
});
```

- **`page` is a rune object, not a store:** `import { page } from "@inertiajs/svelte"`, then `page.flash`, `page.props`. Never `$page`. `components/Flash.svelte`:

```svelte
<script lang="ts">
  import { page } from "@inertiajs/svelte";

  // page.flash is {} when there is no flash.
  const tone = $derived(
    page.flash.kind === "alert"
      ? "bg-red-50 text-red-800 dark:bg-red-950 dark:text-red-200"
      : "bg-green-50 text-green-800 dark:bg-green-950 dark:text-green-200",
  );
</script>

{#if page.flash.message}
  <div role="status" class="mb-6 rounded-md px-4 py-3 text-sm {tone}">
    {page.flash.message}
  </div>
{/if}
```

- `components/Layout.svelte` takes `let { children }: { children: Snippet } = $props()` and ends `<Flash />{@render children()}`. Links are `<Link href={paths.books()} class="…">` from `@inertiajs/svelte`. The "Dev tools" link to a kernel page stays a plain `<a>`, so it's a full page load.
- Pages: `web/src/pages/<plural_snake>/<Action>.svelte` read typed props with `let { books }: BooksIndexProps = $props();`. Deleting is `router.delete(paths.book(book.id))` after `confirm(…)`.
- **Forms: the form component owns `useForm`.** Use the **plain `useForm(data)`** overload and submit with `form.submit(method, url)`. Fields live directly on the form (`form.title`), so inputs use `bind:value={form.title}`:

```svelte
<!-- components/books/BookForm.svelte (excerpt) -->
<script lang="ts">
  import type { Method } from "@inertiajs/core";
  import { useForm } from "@inertiajs/svelte";
  import type { BookParams } from "@/types/BookParams";

  type Props = { method: Method; url: string; book: BookParams; submitLabel: string };
  let { method, url, book, submitLabel }: Props = $props();

  // The component that binds the inputs owns the form (see "Components own their state").
  const form = useForm(() => ({ ...book }));

  function submit(e: SubmitEvent) {
    e.preventDefault();
    form.submit(method, url);
  }
</script>

<form onsubmit={submit} novalidate>
  <label for="title">Title</label>
  <input id="title" bind:value={form.title} />
  {#each form.errors.title ?? [] as msg (msg)}<p class="text-red-700 dark:text-red-300">{msg}</p>{/each}
  <button type="submit" disabled={form.processing}>{submitLabel}</button>
</form>
```

  Pages just place it: `<BookForm method="patch" url={paths.book(id)} {book} submitLabel="Save" />`.
- **Component tests** mock `@inertiajs/svelte` at the boundary. For a form, `useForm` returns a `$state` fake from a `*.svelte.test.ts` file, so `bind:value` round-trips; assert `form.submit` was called with `("patch", "/books/1")`.

## `spa` mode (SvelteKit)

**SvelteKit, in SPA mode only.** It's the canonical Svelte router, and the one models know. Rust stays the only server, so there's no SSR, no prerendering, no form actions, and no `+page.server.ts`, `+server.ts` or `hooks.server.ts`. Data comes from `/api` through TanStack Query, exactly as in the React SPA ([frontend-react](frontend-react.md): `/api` contract, `api/client.ts` as the only `fetch` caller, cookie sessions, Vite dev proxy).

- Scaffold: `pnpm dlx sv@latest create web --template minimal --types ts --no-add-ons --install pnpm`, then `pnpm add -D @sveltejs/adapter-static tailwindcss @tailwindcss/vite @biomejs/biome` and `pnpm add @tanstack/svelte-query`. Remove `@sveltejs/adapter-auto`.
- SvelteKit 3 is configured **inside `vite.config.ts`**. There's no `svelte.config.js`:

```ts
import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

const rust = "http://localhost:3000";

export default defineConfig({
  plugins: [
    tailwindcss(),
    sveltekit({
      compilerOptions: { runes: true },
      // SPA mode: no SSR, no prerendering. Rust serves dist/ and falls back to index.html.
      adapter: adapter({ pages: "dist", assets: "dist", fallback: "index.html" }),
      appDir: "assets", // hashed output under /assets/immutable/ (Rust caches it forever)
    }),
  ],
  // Development: browse Vite (:5173); everything the Rust server owns is proxied, so it's same-origin.
  server: {
    port: 5173,
    strictPort: true,
    proxy: { "/api": rust, "/up": rust, "/streams": rust, "/blobs": rust, "/dev": rust },
  },
});
```

- `src/routes/+layout.ts`: `export const ssr = false; export const prerender = false;`. `src/routes/+layout.svelte` wraps `{@render children()}` in `<QueryClientProvider client={queryClient}>` and imports `../app.css` (`@import "tailwindcss";`).
- Files: pages are `src/routes/<plural_kebab>/+page.svelte` and `src/routes/<plural_kebab>/[id]/+page.svelte`, reading `page.params.id` from `$app/state`. `src/lib/api/client.ts` (the only `fetch`), `src/lib/api/<resource>.ts` (query factories), `src/lib/types/` (ts-rs), and `src/lib/paths.ts`. Import through the package's subpath imports **with the extension**: `import { paths } from "#lib/paths.ts"`. Without it, `svelte-check` can't resolve the module.
- Queries are accessor-based in TanStack Query v6, so they stay reactive to route params:

```ts
export const createBook = (id: () => number) =>
  createQuery(() => ({
    queryKey: bookKeys.one(id()),
    queryFn: () => api<BookJson>("GET", `/books/${id()}`),
  }));
```

  In the page: `const book = createBook(() => Number(page.params.id));`, then `{#if book.isPending}…{:else if book.isError}…{:else}{book.data.title}{/if}`.
- Every page sets `<svelte:head><title>…</title></svelte:head>`. SvelteKit's route announcer reads it aloud on navigation, and says "untitled page" otherwise.
- **`src/app.html` must have no inline `style="…"` or `<script>`** (and no literal `<script>` text, even in a comment). The scaffold's `<div style="display: contents">` is replaced by `<div id="svelte">%sveltekit.body%</div>`.

### Serving it from Rust: strict CSP with SvelteKit's inline bootstrap

SvelteKit's SPA fallback page starts the app from **one inline `<script>`**. Its route announcer (an `aria-live` region) is also rendered with a static `style="…"` attribute compiled into the JS. A strict CSP (`script-src 'self'; style-src 'self'`) blocks both. Don't add `'unsafe-inline'`. Allow exactly those two by hash, computed once at boot from the build output, in `kernel/assets.rs`:

```rust
/// The SPA shell plus the CSP that lets exactly its inline bootstrap run.
pub struct SpaShell { pub html: String, pub csp: String }

impl SpaShell {
    /// Reads `web/dist/index.html` and every built JS file once, at boot.
    pub fn load(dist: &str) -> anyhow::Result<Self> {
        let html = std::fs::read_to_string(format!("{dist}/index.html"))?;
        let mut js = Vec::new();
        collect_js(std::path::Path::new(&format!("{dist}/assets")), &mut js)?; // recursive *.js
        Ok(Self { csp: spa_csp(&html, &js), html })
    }
}

/// Every `open…close` body in `text`, as CSP `'sha256-…'` sources (sorted, deduplicated).
fn hashes(text: &str, open: &str, close: &str) -> Vec<String> { /* sha256 → base64 */ }

pub fn spa_csp(html: &str, js: &[String]) -> String {
    let scripts = hashes(html, "<script>", "</script>");
    let mut styles: Vec<String> = js.iter().flat_map(|j| hashes(j, "style=\"", "\"")).collect();
    styles.sort();
    styles.dedup();
    let style_src = if styles.is_empty() {
        "'self'".to_owned()
    } else {
        format!("'self' 'unsafe-hashes' {}", styles.join(" "))
    };
    format!(
        "default-src 'self'; script-src 'self' {}; style-src {style_src}; img-src 'self' data:; \
         connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        scripts.join(" ")
    )
}
```

- The SPA fallback (any `GET` not under `/api`, `/assets` or `/up`) returns `shell.html` with `shell.csp` and `Cache-Control: no-cache`. Every other response keeps the strict default CSP. `/api` misses stay JSON 404s.
- `/assets/immutable/*` is `public, max-age=31536000, immutable`. `/assets/version.json` isn't hashed, so it's served without that header.
- Production refuses to boot without `web/dist/index.html`, as it does without the Vite manifest in other modes.
- Unit-test `spa_csp` with a fake page and JS. The required browser check: load a deep link, navigate client-side and reload, with **zero** console messages (verified: one script hash plus one `'unsafe-hashes'` style hash, the announcer's, and nothing else).

## Islands

Same contract as React islands ([frontend-htmx](frontend-htmx.md#islands)): one entry, lazy chunks, `data-island` plus HTML-escaped JSON `data-props`, mount on `DOMContentLoaded` and `htmx:afterSwap`, unmount on `htmx:beforeSwap`. `web/` is a plain Vite + `@sveltejs/vite-plugin-svelte` project (no SvelteKit, no Inertia, no `@tailwindcss/vite`). `build: { manifest: true, rolldownOptions: { input: "src/islands/mount.ts" } }`, and development uses `vite build --watch` with the manifest, never the dev server, exactly like React islands ([why](frontend-htmx.md#islands)).

`web/src/islands/mount.ts` (verified: props round-trip, the lazy CSS chunk loads, and the swap remounts):

```ts
import { type Component, mount, unmount } from "svelte";

const islands = import.meta.glob<{ default: Component<Record<string, unknown>> }>("./*.svelte");
const mounted = new Map<Element, Record<string, unknown>>();

async function mountAll(scope: ParentNode = document) {
  for (const el of scope.querySelectorAll<HTMLElement>("[data-island]")) {
    if (mounted.has(el)) continue;
    const load = islands[`./${el.dataset.island}.svelte`];
    if (!load) throw new Error(`Unknown island: ${el.dataset.island}`);
    const { default: Island } = await load();
    const props = JSON.parse(el.dataset.props ?? "{}") as Record<string, unknown>;
    mounted.set(el, mount(Island, { target: el, props }));
  }
}

function unmountWithin(scope: Element) {
  for (const [el, instance] of mounted) {
    if (scope.contains(el)) {
      void unmount(instance);
      mounted.delete(el);
    }
  }
}

document.addEventListener("DOMContentLoaded", () => void mountAll());
document.addEventListener("htmx:afterSwap", (e) => void mountAll(e.target as Element));
document.addEventListener("htmx:beforeSwap", (e) => unmountWithin(e.target as Element));
```

A component's `<style>` block compiles to its own CSS file, which Vite loads with the island's chunk. That's allowed under `style-src 'self'`.

## Gotchas agents get wrong

- **`useForm(method, url, data)` is a Laravel Precognition form.** It keeps only the *first* error per field, as a string, so `{#each form.errors.title as msg (msg)}` iterates its characters and crashes with `each_key_duplicate`. Use `useForm(data)` + `form.submit(method, url)`.
- **A child binding inputs to a parent's form** triggers `ownership_invalid_mutation` in the browser console only; svelte-check and Vitest stay green. Let the form component own `useForm`. This is one reason the real-browser smoke (test-standards §9) is required.
- **`$page` / `page.subscribe`:** Inertia v3's Svelte `page` is a rune object. Read `page.flash` directly.
- **`tsc -b` as the type-check:** it skips `.svelte` files. Use `svelte-check --fail-on-warnings`.
- **Biome without `html.experimentalFullSupportEnabled`:** template-only usage looks unused, and agents "fix" it by deleting code.
- **SvelteKit server features in `spa` mode** (`+page.server.ts`, `+server.ts`, form actions, `hooks.server.ts`, `load` functions that need a server): they can't run, because Rust is the server. Fetch through `/api`.
- **`'unsafe-inline'` to make SvelteKit boot:** hash the bootstrap script and the announcer style instead. Literal `<script>` text in `app.html`, even inside a comment, breaks the hash scan.
- **`#lib/…` imports without `.ts`:** Vite resolves them, but svelte-check reports "Cannot find module".
