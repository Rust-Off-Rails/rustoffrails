# Controllers (Action Controller → axum handlers)

> **TL;DR**
> - One file per resource with the seven Rails actions; handlers are thin: extract → authorize → model → render or redirect.
> - Extractor order: `State`, auth/`Ctx`, `Path`/`Query`, then the body (`Form`/`Json`/`Multipart`) last; return `Result<Response, AppError>`.
> - Successful POST → `Redirect::to` (303); validation failure → re-render with 422, keeping the params.
> - Authorize before any write; scope private records by owner (another user's id is 404); no DB calls in views; side effects belong in model fns.
>
> Read the rest when writing handlers, flash, extractors, or JSON endpoints.

A controller is one file in `src/controllers/<plural>.rs` holding plain async functions, one per action. Handlers are thin: extract → authorize → call the model → render or redirect. Business rules live in models, never here.

## The seven actions (`htmx` mode)

```rust
use axum::{extract::{Path, State}, http::StatusCode, response::{IntoResponse, Redirect, Response}, Form};
use crate::{
    controllers::application::RequireUser,
    error::AppError,
    kernel::flash::Flash,
    models::{post, Page, PostParams},
    policies,
    routes::paths,
    state::AppState,
    views::{self, Ctx, posts::*},
};

pub async fn index(State(s): State<AppState>, ctx: Ctx, page: Page) -> Result<Response, AppError> {
    let posts = post::published(&s.db, page).await?;
    Ok(views::render(PostsIndex { ctx, posts })?.into_response())
}

pub async fn show(State(s): State<AppState>, ctx: Ctx, Path(id): Path<i64>) -> Result<Response, AppError> {
    let post = post::find(&s.db, id).await?;
    Ok(views::render(PostsShow { ctx, post })?.into_response())
}

pub async fn new(ctx: Ctx, RequireUser(_user): RequireUser) -> Result<Response, AppError> {
    Ok(views::render(PostsNew { ctx, params: PostParams::default(), errors: Default::default() })?.into_response())
}

pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    RequireUser(user): RequireUser,
    Form(params): Form<PostParams>,              // body extractor LAST
) -> Result<Response, AppError> {
    let result = match params.validate() {
        Ok(new) => post::create(&s.db, user.id, new).await,
        Err(errors) => Err(AppError::Invalid(errors)),
    };
    match result {
        Ok(post) => Ok((Flash::notice("Post was successfully created."), Redirect::to(&paths::post(post.id))).into_response()),
        Err(AppError::Invalid(errors)) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            views::render(PostsNew { ctx, params, errors })?,
        ).into_response()),
        Err(e) => Err(e),
    }
}

pub async fn edit(State(s): State<AppState>, ctx: Ctx, RequireUser(user): RequireUser, Path(id): Path<i64>) -> Result<Response, AppError> {
    let post = post::find(&s.db, id).await?;
    policies::authorize(policies::posts::can_edit(&user, &post))?;   // other users: 403 (auth.md#authorization)
    let params = PostParams::from(&post);          // prefill
    Ok(views::render(PostsEdit { ctx, post, params, errors: Default::default() })?.into_response())
}

// update: same shape as create (find → authorize → validate → post::update → redirect / 422)
// destroy: find → authorize → post::destroy → Flash::notice + Redirect::to(&paths::posts())
```

### Other handler shapes

- **`inertia` mode:** the same seven actions, but `inertia.render("posts/Index", props)` instead of `views::render`. Failed validation **redirects back** with errors (`Json`/`Form` + `map_err(AppError::Invalid)`); there's no 422 re-render. See [frontend-inertia: the adapter's Rust surface](frontend-inertia.md#the-adapters-rust-surface) and [validation errors: redirect back](frontend-inertia.md#validation-errors-redirect-back).
- **`spa` mode:** [JSON API](#json-api) below.
- **Uploads:** `multipart: Multipart` as the last argument, then `storage::read_multipart::<PostParams>(multipart)` and `form.take_file("cover")`. Validate the params and the file together ([storage](storage.md#uploading-controller)).
- **Nested resources** (`POST /books/{book_id}/reviews`): load the parent first (404 if missing) and authorize against it, then create. On a validation failure, re-render the **parent's** page with the child form's errors. Build that page in one shared function (`books::show_page(&s, ctx, &user, id, form)`) so `show` and the failed create can't drift apart.
- **htmx fragment routes** (a live region's refresh, `GET /books/{book_id}/reviews`): take `HxRequest(htmx)`. Return the partial when it's true; otherwise redirect to the parent page.

## Rules

1. **Signature order:** `State`, then `Ctx`/auth extractors, then `Path`/`Query`, then the body extractor (`Form`, `Json`, `Multipart`) **last**. axum rejects anything else at compile time with an unreadable error.
2. **Every handler returns `Result<Response, AppError>`.** Let `?` do the work. Never `unwrap`.
3. **Redirect after every successful POST** with `Redirect::to` (303 See Other). Never render a page from a successful POST.
4. **On validation failure, re-render the form with status 422,** passing back the original `params` (so input is kept) and the `errors`.
5. **Authorization comes before any write**, and before rendering anything not public. See `auth.md#authorization`.
6. **Scope by owner in the query** when a resource is private: `post::find_for_user(&db, user.id, id)` returns `NotFound` for other users' rows. That beats find-then-check because it never leaks existence.
7. **No DB calls in views.** Load everything the view needs in the handler and pass it in.
8. **Side effects belong in the model fn** (enqueue jobs there, in the transaction), not in the handler.
9. **Every action is `#[tracing::instrument(skip_all)]`**, so traces name it (a helper it calls in a loop: `level = "debug"`; [the one rule](errors-and-logging.md#spans)); never put secrets or personal data in span fields.

## Rails → Off Rails

| Rails | Off Rails |
|-------|-----------|
| `before_action :authenticate_user!` | `RequireUser(user): RequireUser` argument |
| `before_action :set_post` | `let post = post::find(&s.db, id).await?;` as the first line. Repetition is fine |
| `params.require(:post).permit(:title, :body)` | `Form<PostParams>`. The struct is the permit list |
| `redirect_to @post, notice: "…"` | `(Flash::notice("…"), Redirect::to(&paths::post(id)))` |
| `render :new, status: :unprocessable_entity` | `(StatusCode::UNPROCESSABLE_ENTITY, views::render(PostsNew { … })?)` |
| `head :no_content` | `StatusCode::NO_CONTENT` |
| `rescue_from ActiveRecord::RecordNotFound` | Automatic: `DbErr::RecordNotFound` / `None` → `AppError::NotFound` → 404 |
| `helper_method :current_user` | `Ctx` carries `current_user` into every view |
| `request.xhr?` / `turbo_frame_request?` | `HxRequest(is_htmx): HxRequest` |
| `ApplicationController` | `controllers/application.rs`: shared extractors only, no inheritance |

## Shared extractors (`controllers/application.rs`)

| Extractor | Yields | Rejection |
|-----------|--------|-----------|
| `CurrentUser(Option<User>)` | the logged-in user, if any (spec in `auth.md`) | never rejects |
| `RequireUser(User)` | the logged-in user | `AppError::Unauthorized` |
| `CurrentSession(session::Model)` | the session row, for logout | `AppError::Unauthorized` |
| `UserAgent(Option<String>)` | the `User-Agent` header, stored on new sessions | never rejects |
| `Ctx` | `views::Ctx { title, flash: Option<Flash>, current_user: Option<User>, assets, dev, dev_mailbox }` for layouts (`htmx`). `current_user` and `dev_mailbox` arrive with the authentication generator. The struct lives in `views/mod.rs`, the `FromRequestParts` impl here | `AppError` (only on DB errors loading the session) |
| `HxRequest(bool)` | `true` when `HX-Request: true` | never rejects |
| `Inertia` | the request's Inertia context (`frontend-inertia.md`) | never rejects |
| `Page` | `?page=&per=` pagination (`models/mod.rs`, implemented via `Query<Page>`) | `AppError::Invalid` (422) on garbage |
| `AdminAccess` | access to `/admin/*`: open in development, `RequireAdmin` when the app has roles, otherwise HTTP Basic with `APP_ADMIN_PASSWORD` | 401 + `WWW-Authenticate: Basic` |
| `ClientIp` | the client IP for rate limiting (`kernel/rate_limit.rs`) | never rejects |

The session lookup behind `Ctx`, `CurrentUser`, `RequireUser` and `CurrentSession` runs once per request and is cached in the request extensions ([auth](auth.md#sessions)). Implement them with `FromRequestParts<AppState>`. **axum 0.8 uses native `async fn` in the trait, so don't add `#[async_trait]`.** Cache expensive lookups (the current user) in `parts.extensions` so `Ctx`, `CurrentUser` and `RequireUser` in one handler hit the DB once.

## Flash (`kernel/flash.rs`)

One type, `kernel::flash::Flash { kind: FlashKind, message: String }`, where `FlashKind` is `Notice` or `Alert` and serializes in snake_case. Inertia/spa modes also derive `ts_rs::TS` on it.

- **Storage:** the `_flash` cookie holds the JSON, **base64url-encoded without padding** (cookie values can't contain `"`, `,` or `;`). Its attributes are `Path=/`, `HttpOnly`, `SameSite=Lax`. It isn't signed, because the value is only ever rendered escaped.
- **Setting:** `Flash::notice("…")` / `Flash::alert("…")` implement `IntoResponseParts`, which appends `Set-Cookie`. Put the flash in the response tuple before the body or redirect: `(Flash::notice("Saved"), Redirect::to(&path))`.
- **Middleware** (`from_fn(kernel::flash::middleware)`):
  - On the request, decode `_flash` into a `Flash` request extension.
  - On the response, append a removal cookie (`make_removal()`) only if the request carried a flash, the response sets no new `_flash`, and the response **isn't a redirect**. This gives read-once semantics, as in Rails.
  - Clearing on a redirect would lose a flash set two hops earlier.
- **Reading:** `Ctx.flash` (`parts.extensions.get::<Flash>().cloned()`).
- **Rendering:** always escaped (Askama default, or React text). Never `|safe`.

## JSON API

`spa` mode, and JSON endpoints for islands. This is the *internal* API your own UI calls (cookie session, types via ts-rs, no versioning). A **public** API for other programs (`API: rest | graphql | both`, or `FRONTEND_MODE: api`) follows [api](api.md) instead: `/api/v1`, bearer tokens, `ApiJson`, OpenAPI, `{"data": …}` envelopes.

Handlers live in `src/controllers/api/<plural>.rs` and are mounted under `/api` with real verbs:

| Action | Route |
|--------|-------|
| index | `GET /api/posts` |
| create | `POST /api/posts` |
| show | `GET /api/posts/{id}` |
| update | `PATCH /api/posts/{id}` |
| destroy | `DELETE /api/posts/{id}` |

```rust
pub async fn create(State(s): State<AppState>, RequireUser(user): RequireUser, Json(params): Json<PostParams>)
    -> Result<(StatusCode, Json<PostJson>), AppError>
{
    let new = params.validate().map_err(AppError::Invalid)?;
    let post = post::create(&s.db, user.id, new).await?;
    Ok((StatusCode::CREATED, Json(PostJson::from(&post))))
}
```

- Response bodies are `views::<Model>Json` / `<Plural>Page` structs deriving `Serialize` + `ts_rs::TS`. **Never serialize an entity `Model` directly**, because new columns (and password digests) would leak.
- Errors are JSON on `/api` paths: `AppError::Invalid` → `422 {"errors": {"title": ["can't be blank"]}}`, `Unauthorized` → `401 {"error":"unauthorized"}`, `NotFound` → `404`, `Forbidden` → `403`. `error.rs` picks the format from the `ResponseFlavor` request extension (Api / Inertia / Htmx / Html), which one middleware sets; see `errors-and-logging.md`.
- Lists return `{ "items": [...], "page": n, "per": n }`.
- Auth is the same cookie session as HTML (`auth.md`). Tokens belong to the public API ([api](api.md#authentication-api-tokens)): set `API` in the app's header rather than adding tokens here.

## Gotchas agents get wrong

- Putting `Form(...)` before `Path(...)` → "the trait `Handler` is not implemented". Move the body extractor last.
- `Redirect::permanent` after a POST. Use `Redirect::to` (303).
- Returning 200 with errors from a failed form. It must be 422, or htmx won't treat it as an error and tests can't tell.
- Forgetting `RequireUser` on `new`/`edit` while protecting only `create`/`update`.
- Re-querying the current user in the handler. Use the extractor.
- `#[async_trait]` on `FromRequestParts` impls. That was axum 0.7; it's gone in 0.8.
