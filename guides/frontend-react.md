# Frontend: `spa` mode with React (SPA + JSON API)

> **TL;DR**
> - `spa` mode with React: a Vite SPA in `web/` (React Router 8 + TanStack Query) talking to the Rust API under `/api`. Choose it only for a public API or mobile clients; otherwise use `inertia`.
> - Types come from ts-rs; `web/src/api/client.ts` is the only `fetch` caller; per-resource hooks wrap it.
> - Relative `/api/...` paths only; the SPA fallback must not swallow `/api` 404s.
> - Type-check with `tsc -b`; install `react-router` (not `react-router-dom`).
>
> Read the rest when building SPA routes, the API client, or SPA tests.

This file was `react` mode before 0.3.0; the mode is now `spa`, with `UI_LIBRARY: react`. For `UI_LIBRARY: svelte` (SvelteKit in SPA mode), read [frontend-svelte](frontend-svelte.md#spa-mode-sveltekit), which keeps this file's `/api` contract.

A Vite-built React SPA in `web/`, talking to a Rust JSON API under `/api`. Choose this mode only when the interview found a reason: a public API, or a mobile/third-party client sharing it. Otherwise React apps use [`inertia`](frontend-inertia.md).

Also read: [`controllers.md#json-api`](controllers.md#json-api) for the server side.

## Shape

```
browser ──► Rust binary (:3000)
              ├── /api/*      JSON handlers (controllers/api/)
              ├── /up         health check
              ├── /assets/*   web/dist/assets (hashed, immutable)
              └── GET *       SPA fallback → web/dist/index.html
```

- **Production:** `routes.rs` serves `web/dist` with `ServeDir`, plus a fallback that returns `web/dist/index.html` for any `GET` not under `/api`, `/assets` or `/up` that accepts `text/html`. `/api/*` misses still 404 as JSON.
- **Development:** Vite on `:5173` with `server.proxy` forwarding `/api`, `/up`, `/streams` (SSE), `/blobs` and `/dev` to `http://localhost:3000` (`bin/dev` runs the Rust server without TLS in `spa` mode). Browse `:5173`. Same origin from the browser's view, so cookies just work. Vite proxies over HTTP/1.1, so check many live tabs against the built app ([realtime](realtime.md#many-tabs-stall-http11-vs-http2)).

## Setup (verified with Vite 8, React 19, React Router 8, TanStack Query 5)

Scaffold and convert exactly as in the "Vite wiring" section of [frontend-inertia.md](frontend-inertia.md) (cleanup, Biome, `tsconfig`, scripts, `tsc -b`), with these differences:

- Keep `index.html` (the SPA shell): `<div id="root"></div><script type="module" src="/src/main.tsx"></script>`.
- Packages: `pnpm add react-router @tanstack/react-query` instead of the Inertia packages. **Only `react-router`.** `react-router-dom` is a v7-era compatibility package; don't install it.
- No `build.rolldownOptions.input` (the entry is `index.html`). `build.manifest` isn't needed.
- `vite.config.ts` `server.proxy`: `{ "/api": "http://localhost:3000", "/up": "http://localhost:3000", "/streams": "http://localhost:3000", "/blobs": "http://localhost:3000", "/dev": "http://localhost:3000" }`.

## Types: Rust is the source of truth

- Every request/response type the SPA sees derives `ts_rs::TS` with `#[ts(export)]`: `<Model>Params` (in models), `<Model>Json`, `<Plural>Page`, `Errors` (in views).
- `.cargo/config.toml`:

```toml
[env]
TS_RS_EXPORT_DIR = { value = "web/src/types", relative = true }
# i64 ids arrive as JSON numbers; ts-rs 12 defaults i64 to `bigint`, which type-checks but is wrong at runtime.
TS_RS_LARGE_INT = "number"
```

- `ts-rs` exports during `cargo test`, via the tests it generates. So the Definition of Done runs `cargo nextest run` **before** `pnpm --dir web exec tsc -b`. A renamed Rust field then breaks the TS build. That's the point.
- `web/src/types/` is generated: commit it, never edit it. CI checks it's up to date (`git diff --exit-code web/src/types`).
- Check the resolved `ts-rs` version's docs for attribute names and export behavior. They have changed between versions.

## `web/src/api/client.ts`: the only `fetch`

```ts
import type { Errors } from "@/types/Errors";

export class ApiError extends Error {
  constructor(public status: number, public errors: Errors | null) { super(`API ${status}`); }
}

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    credentials: "same-origin",
    headers: body ? { "Content-Type": "application/json" } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  if (res.status === 204) return undefined as T;
  if (!res.ok) throw new ApiError(res.status, res.status === 422 ? (await res.json()).errors : null);
  return res.json() as Promise<T>;
}
```

A 401 anywhere → redirect to `/login` (handled once in the `QueryClient` default `onError`).

## Per-resource hooks: `web/src/api/<resource>.ts`

```ts
export const postKeys = { all: ["posts"] as const, one: (id: number) => ["posts", id] as const };

export const usePosts = (page = 1) =>
  useQuery({ queryKey: [...postKeys.all, page], queryFn: () => api<PostsPage>("GET", `/posts?page=${page}`) });

export const useCreatePost = () => {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (p: PostParams) => api<PostJson>("POST", "/posts", p),
    onSuccess: () => qc.invalidateQueries({ queryKey: postKeys.all }),
  });
};
```

Pages never call `api()` directly. They use hooks.

## Routing & pages

- `web/src/main.tsx`: data/SPA mode, never framework mode (no `@react-router/dev`, no `react-router.config.ts`):

```tsx
import "./app.css";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter } from "react-router";
import { RouterProvider } from "react-router/dom";
import { routes } from "@/routes";

const queryClient = new QueryClient();
const router = createBrowserRouter(routes);
const root = document.getElementById("root");
if (!root) throw new Error("#root missing");
createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
```

- `web/src/routes/index.tsx` exports `routes: RouteObject[]` (import `RouteObject` from `react-router`), so tests can mount the same tree with `createMemoryRouter(routes, { initialEntries: ["/posts"] })`.
- Pages: `web/src/routes/<plural_kebab>/{Index,Show,New,Edit,Form}.tsx`. Path constants in `web/src/paths.ts`, mirroring `routes::paths`.
- Forms: controlled inputs over a `PostParams` state object. On `ApiError` with `errors`, render `errors[field]` under each input using Rails wording ("Title can't be blank").
- Flash: client-side. After a successful mutation, navigate and pass a message via router state, which `components/Layout.tsx` renders.

## Auth

Same cookie session as every other mode (`auth.md`), via JSON endpoints: `POST /api/session` (login), `DELETE /api/session` (logout), `GET /api/session` (current user or 401). **No JWTs, no localStorage tokens.**

## Realtime

`new EventSource("/streams/posts")` → on each signal, `queryClient.invalidateQueries({ queryKey: postKeys.all })`. Signals, not data (`realtime.md`).

## Testing

- Vitest + Testing Library for components and hooks (mock `api()`, never `fetch` itself). Wrap renders in a fresh `QueryClient` (`retry: false`) and a `createMemoryRouter`.
- Server behavior is tested in Rust (`testing.md`). Playwright E2E is optional, for critical flows only.

## Gotchas agents get wrong

- Running `tsc` before `cargo test`, then "fixing" TS errors by editing generated types.
- Type-checking with `tsc --noEmit`, which checks nothing under the template's project references. Use `tsc -b`.
- Installing `react-router-dom`, or importing `RouterProvider` from `react-router` instead of `react-router/dom` in the browser entry.
- Duplicating a Rust type by hand in TS "just for now".
- Using `localhost:3000` URLs in the SPA. Always use relative `/api/...` paths.
- SPA fallback swallowing `/api` 404s and returning `index.html` with 200.
- Using React Router framework mode or loaders that assume SSR.
- Forgetting `credentials: "same-origin"` in tests' fetch mocks, or adding CORS. Same origin needs no CORS.
