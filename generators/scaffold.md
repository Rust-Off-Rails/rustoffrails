# Generator: `resource` (scaffold) / `model`

> **TL;DR**
> - `cargo offrails g resource posts title:string slug:string:unique body:text published_at:datetime:optional` writes steps 2–10 below (migration, model, factory, model and request tests, controller, routes, htmx pages, nav, tenancy suites) and prints the rest. `--dry-run` shows the plan; `cargo offrails destroy resource posts` undoes it.
> - `cargo offrails g model Post title:string …` = the migration, model, factory and model tests only (join tables, internal records).
> - Yours: the spec with AC ids first (step 1), then the `// APP:` lines (validation limits, the label, what each page shows), the spec's tests, seeds (step 11), the browser smoke (step 12).
>
> Read the rest for what each step produces (and how to do it by hand in the modes the generator doesn't write).

```
cargo offrails g resource books title:string author:references isbn:string:unique published_on:date:optional
cargo offrails g model Tag name:string:unique
```

The field types, their columns, form controls, validations and factory values are in [the generators README](README.md#fields). Records belong to the signed-in user when the app has accounts (`--public` otherwise; the generator refuses to guess).

## Field syntax (by hand, and in the other modes)

`name:type` plus optional modifiers: `?` (or `:optional`) makes it nullable (default is required), `!` (or `:unique`) makes it unique. Example: `email:string!`, `bio:text?`. The generator also takes `:index`, `decimal{p.s}`, `uuid`, `references{model}` ([README](README.md#fields)); `rich_text`, `enum{…}` and `attachment` below are by hand.

| Type | Migration helper | Rust type | Form control / TS type |
|------|------------------|-----------|------------------------|
| `string` | `string` | `String` | text input / `string` |
| `text` | `text` | `String` | textarea / `string` |
| `rich_text` | `text` | `String` (Markdown) | textarea + rendered via `views::helpers::markdown` |
| `integer` | `integer` | `i32` | number input / `number` |
| `bigint` | `big_integer` | `i64` | number input / `number` |
| `money` | `big_integer` (`<name>_cents`) | `i64` | decimal input parsed to cents / `number` |
| `boolean` | `boolean` | `bool` | checkbox (absent = false) / `boolean` |
| `date` | `date` | `chrono::NaiveDate` | date input / `string` |
| `datetime` | `timestamp_with_time_zone` | `DateTimeUtc` | datetime-local input interpreted in the user's zone / `string` |
| `enum{a,b,c}` | `string` + `CHECK` | `DeriveActiveEnum` | select / string union |
| `references` | `big_integer` `<name>_id` + FK + index | `i64` + `Relation` | select, or set from the current user if the name is `owner`/`user`/`author` |
| `attachment` | `big_integer_null` `<name>_blob_id` FK → `blobs` (`SetNull`) + index | `Option<i64>` | file input in a `multipart/form-data` form; the handlers take `Multipart` + `storage::read_multipart`, and the two form routes get `DefaultBodyLimit::max(..)` ([storage](../guides/storage.md); on first use add the storage kernel, `create_blobs`, `models/blob.rs`, `PurgeBlobJob` and `GET /blobs/{key}`) |
| `json` | `json` | `serde_json::Value` | not form-editable by default |

Field names that imply ownership (`owner`, `user`, `author` + `references`) are **never** in `Params`. They're set from `RequireUser`, and the policy restricts edits to the owner unless the spec says otherwise.

## Scaffold steps

`generate resource` does 2, 3 (per-user scope; the `has_many` side when the parent was generated), 5, 6 (the standard set, not your ACs), 7, 8, 9 (`htmx`), 10 and the tenancy suites, then runs `cargo fmt` and rewrites docs/routes.md. In `inertia`/`spa`/`api` mode run `generate model` (and `generate api`), then do 6–10 by hand from this list.

1. **Spec:** make sure `product/features/<plural>.md` exists. If not, write it from the request, with acceptance criteria for list/create/show/edit/delete, validation rules, and who can do what (interview feature checklist).
2. **Migration:** `cargo offrails g migration create_<plural> field:type…` (never `sea-orm-cli migrate generate`: it rewrites `migration/src/lib.rs` without the anchors) → table per [migrations](../guides/migrations.md) (id, fields, timestamps, FKs with indexes, unique indexes for `!`, `CHECK`s for enums). Run `cargo run -- db migrate`, then prove rollback.
3. **Model:** `src/models/<singular>/` (`mod.rs` the entity, `params.rs`, `queries.rs`; one `<singular>.rs` is fine by hand) per [models](../guides/models.md): entity, relations (both sides: add `has_many` + `Related` to the parent model too), `Params`, `New`, `validate()` (required fields → `can't be blank`; strings → max length 255 unless text; integers → range if the spec says; enums → `is not included in the list`), `find`, the list query (paginated, `order_by_desc(created_at)` then `id`), `create`, `update`, `destroy`. Plus the alias in `models/mod.rs`.
   - **`TENANCY` isn't `none`:** `find` and `destroy` are `pub(in crate::models)`. The public surface is `find_for(db, &user, id)`, `all_for(db, &user, page)` and `destroy_for(db, &user, record)` ([scoped lookups](../guides/models.md#scoped-lookups-tenancy)). Add a `reload_<singular>` helper to `tests/app/factories.rs` (tests read through the entity), add the model to `Tenant` in `tests/app/tenancy_test.rs`, teach `fill` its routes, and list its shared routes in `EXPECTED` with their AC ids. Check: `cargo nextest run tenancy_test guard_test`.
4. **Policy:** `src/policies/<plural>.rs`: `can_view`, `can_edit`, `can_destroy` from the spec (default: anyone logged in views, owner edits/destroys), plus the inline truth-table test ([test-standards §2](../guides/test-standards.md#what-each-suite-contains)).
5. **Factory:** `<singular>(&db, …)` in `tests/app/factories/<singular>.rs` (wired into `factories.rs` by `mod <singular>; pub use <singular>::*;`), going through `Params::validate` + the model's `create` (so factories can't build invalid rows).
6. **Tests first** (`tests/app/<plural>/`: `mod.rs` plus `read_test.rs`, `write_test.rs`, `access_test.rs`, the shape `g resource` writes): one test per acceptance criterion, plus the standard set: index lists records; show 404s for missing ids; create with valid params redirects and persists; create with invalid params re-renders 422 with messages (htmx), or redirects back with errors (inertia), or returns 422 JSON (spa); update; destroy; non-owner gets 403 (or 404 when the record is private and scoped: `find_for` answers that); logged-out gets redirected to login (if auth exists); `assert_query_count_stable` on index (and on show if it lists children); escaping of a `<script>` title. Each test cites its `AC-<plural>-<n>`. Full list: [test-standards](../guides/test-standards.md). In `inertia` mode assert on props with `app.inertia_get`/`inertia_send` ([frontend-inertia](../guides/frontend-inertia.md#testing-in-inertia-mode)), and add a Vitest test for the `<Model>Form`.
7. **Controller:** `src/controllers/<plural>/` with all 7 actions (`index.rs`, `show.rs`, `new.rs` new + create, `edit.rs` edit + update, `destroy.rs`, re-exported by `mod.rs`), its routes in `src/routes/<plural>.rs` ([routing](../guides/routing.md#a-features-routes-file)) (`spa`: `controllers/api/<plural>.rs` with index/show/create/update/destroy) per [controllers](../guides/controllers.md). `inertia`: handlers take `Inertia` and `Json(params)`, and validation is just `params.validate().map_err(AppError::Invalid)?`, because `shape_errors` redirects back ([frontend-inertia](../guides/frontend-inertia.md#the-adapters-rust-surface)).
8. **Routes:** resource routes for the mode ([routing](../guides/routing.md)), `paths::` helpers, `ROUTES` rows.
9. **Views by mode:**
   - `htmx`: `views/<plural>.rs` structs + `templates/<plural>/{index,show,new,edit,_form}.html`, built from the component kit (`{% import "components/ui.html" as ui %}`: `page_header`, `table` + `empty_state` + `pagination`, `card`, `confirm_button`, `field`/`select`/`checkbox` + `form_actions`; [views](../guides/views.md#components)); any class of your own from the two-scheme palette ([views](../guides/views.md#dark-mode); `theme_test.rs` must stay green). Askama passes field args by reference: write `paths::book(*book.id)` ([views gotchas](../guides/views.md#gotchas-agents-get-wrong)).
   - `inertia`:
     - `views/<plural>.rs`: `<Model>Json` and `<Plural>{Index,Show,New,Edit}Props` (`Serialize, TS`, `#[ts(export)]`). New/Edit carry `<Model>Params`, and Edit also carries `id`.
     - `<Model>Params` derives `TS` too.
     - Pages: `web/src/pages/<plural>/{Index,Show,New,Edit}.tsx`.
     - The shared form is `web/src/components/<plural>/<Model>Form.tsx` with `<Model>Form.test.tsx` next to it. It goes **not** under `pages/`: the page glob would register it as a page, and tests can't live there.
     - Add the helpers to `web/src/paths.ts`.
     - Run `cargo test --lib export_bindings` before `pnpm --dir web typecheck` ([frontend-inertia](../guides/frontend-inertia.md#props--pages)).
     - `UI_LIBRARY: svelte`: the same files as `.svelte` (pages read `$props()`). The form component **owns** its `useForm(data)` and takes `method`/`url`/initial values as props (`<BookForm method="patch" url={paths.book(id)} {book} …/>`). Its test is `<Model>Form.svelte.test.ts` ([frontend-svelte](../guides/frontend-svelte.md#inertia-mode)).
   - `spa`: `views/<plural>.rs` JSON (`Serialize, TS`) + `web/src/api/<plural>.ts` hooks + `web/src/routes/<plural>/{Index,Show,New,Edit,Form}.tsx` + router entries
     - `UI_LIBRARY: svelte` (SvelteKit SPA): `web/src/lib/api/<plural>.ts` query factories + `web/src/routes/<plural_kebab>/{+page.svelte,new/+page.svelte,[id]/+page.svelte,[id]/edit/+page.svelte}` + a form component in `web/src/lib/components/`. No `+page.server.ts` or `+server.ts` ([frontend-svelte](../guides/frontend-svelte.md#spa-mode-sveltekit)).
   - `api`: no pages. `views/<plural>.rs` holds `<Model>Json` (`Serialize, ToSchema`), `<Model>Body` (`{"data": …}`) and `<Plural>Page` (`data` + `PageInfo`) ([api](../guides/api.md#responses)).
9b. **Public API surfaces** (when `API` isn't `none`; in `api` mode this replaces steps 7–9's HTML parts):
   - `rest` / `both`: `controllers/api/v1/<plural>.rs` with index/show/create/update/destroy, each with `#[utoipa::path(...)]` (tag, `params(Page)`, `request_body = <Model>Params`, `security(("bearer" = []))`, every response with its body). Bodies via `ApiJson<<Model>Params>`; the caller via `ApiUser`, with `me.require(Scope::Read)` or the policy's scope checks; another user's record is 404. `<Model>Params` derives `ToSchema`. Add the paths to `ApiDoc` in `controllers/api/mod.rs`, the routes under `/api/v1`, and `ROUTES` rows (`Access::User`). Then `UPDATE_SCHEMAS=1 cargo nextest run openapi` and commit `docs/openapi.json`.
   - `graphql` / `both`: in `src/graphql/types.rs` a `<Model>Object` (`#[graphql(name = "<Model>")]`, `From<Model>`), a `<Model>Input` (`From<…> for <Model>Params`), `<Model>Payload { <singular>, errors }` and `Delete<Model>Payload`; `query.rs` gets `<plural>(page, per)` (with the per-size complexity) and `<singular>(id)`; `mutation.rs` gets `create<Model>`, `update<Model>`, `delete<Model>`. Every association field resolves through a `Loader` in `loaders.rs`. Then `UPDATE_SCHEMAS=1 cargo nextest run schema_snapshot` and commit `docs/schema.graphql` ([graphql](../guides/graphql.md)).
   - Tests: the REST request matrix (`tests/app/<plural>_test.rs` with `post_json`/`patch_json`/`delete`), insta contract snapshots of `<Plural>Page` and `<Model>Body` (`api_contract_test.rs`), and the GraphQL rows (`graphql_test.rs`: anonymous, another user's record, read-only token, payload `errors`, N+1 guard per association, REST and GraphQL agreeing on a record).
10. **Nav:** add a link in the layout if the resource is top-level.
11. **Seeds:** add 3–5 realistic records to `tasks/seed.rs`: find-or-create each one by a natural key (email, slug, name), inside the seed's one `kernel::db::begin` transaction. Never gate the whole seed on "no rows yet" ([cli](../cli.md#seeds)).
12. **`cargo offrails verify`** (the fast Definition of Done; `--full` once, when the last slice is done), then the real-browser smoke: create → edit → delete, Back after the delete (expect 404), and the console clean ([test-standards §9](../guides/test-standards.md#9-real-browser-smoke)).

## Resource

`resource` = scaffold steps 1–8 with no views. Handlers return what the spec needs (often JSON, or a redirect). Use it for things like webhooks, or models only reachable through a parent page.

## Model

`cargo offrails g model <Name> field:type…` = steps 2, 3 and 5, plus model-level tests (validation unit tests, each query fn against a real DB). Use it for join tables, internal records, and things managed by other controllers.

## Nested resources

`scaffold Review body:text stars:integer book:references --nested-in books`: index and create live under `/books/{book_id}/reviews`, member actions under `/reviews/{id}` (shallow). The parent's show page lists children and embeds the new-child form. Load the parent first and 404 if it's missing, and authorize against the parent. A failed create re-renders the **parent's** page (with the child form's errors) through one shared page-building function. "One child per user per parent" (a review per reader) is a composite unique index, mapped to a `base` error ([models](../guides/models.md#validations)). If the children should update live, follow with [`channel`](channel.md). Executed for `Review` in the reference app on 2026-10-12.
