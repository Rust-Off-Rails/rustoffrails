# Generator: `api` (a public, versioned JSON API)

> **TL;DR**
> - `cargo offrails g api notes title:string body:text`: `/api/v1/notes` (index with `page` info, show, create 201, update by `PATCH` with the fields sent, destroy 204), bearer tokens with `read`/`write` scopes, another user's record 404, `ApiJson` 422s, utoipa annotations, and `tests/app/api_notes_test.rs` (the request matrix of [api](../guides/api.md#testing)).
> - The first run adds the API kernel: `api_tokens` (digest only), `src/controllers/api.rs` (`ApiUser`, `ApiJson`, `ApiDoc`, `GET /api/v1/me`, `GET /api/openapi.json`), `app task tokens create|list|revoke`, `tests/app/api_test.rs` (one 401 for every bad token, the OpenAPI drift test), and `cargo add utoipa`.
> - Needs accounts. Reuses the model when `g model`/`g resource` made it (`cargo offrails g api posts` with no fields).
>
> Read the rest for the next steps and what stays yours.

```
cargo offrails g api notes title:string body:text done:boolean
cargo offrails g api posts          # the model `g resource posts …` made, its fields reused
```

After generating:

1. `cargo run -q -- db migrate` (the `api_tokens` table on first use).
2. `UPDATE_SCHEMAS=1 cargo nextest run openapi`, then commit `docs/openapi.json`: it is the contract, and the drift test fails when the served document changes without it. Changing it later is a decision: log it and tell the human ([versioning](../guides/api.md#versioning)).
3. Set `API: rest` in `AGENTS.md`.
4. A token: `cargo run -q -- task tokens create <email> [name] [--read-only] [--days N]`, then `curl -H "Authorization: Bearer <token>" localhost:3000/api/v1/notes`.

**Decided for you:** JSON bodies are `views::<plural>_json` structs, never the entity; input goes through the same `Params::validate` as the forms (`<Model>Input::onto`), so the rules can't drift; ids are numbers; field names are snake_case; 401s carry `WWW-Authenticate: Bearer`; tokens are rate-limited (300/min each). Handlers live in `src/controllers/api_v1_<plural>.rs` (flat: some part suites read `src/controllers` one level deep).

**Yours:** sign-up and token creation over the API (`POST /api/v1/tokens`, [api](../guides/api.md#authentication-api-tokens)), CORS for browser clients on other origins (`APP_CORS_ORIGINS`), GraphQL ([graphql](../guides/graphql.md)), and insta snapshots of the bodies once clients depend on them.
