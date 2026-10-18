# Rails → Off Rails Map

When a human (or your training data) says "the Rails way", look it up here.

## Frameworks

| Rails                     | Off Rails                                              | Guide |
|---------------------------|--------------------------------------------------------|-------|
| Active Record             | SeaORM entities + query fns in `src/models/`           | [models](guides/models.md) |
| `database.yml` / adapters | `DATABASE_URL`: SQLite or Postgres, same code         | [database](guides/database.md) |
| Active Record Migrations  | `migration/` crate (sea-orm-migration DSL), embedded in the binary | [migrations](guides/migrations.md) |
| Active Model validations  | `<Model>Params::validate() -> Result<New<Model>, Errors>` | [models](guides/models.md#validations) |
| Action Controller         | axum handlers in `src/controllers/`                    | [controllers](guides/controllers.md) |
| Routing (`routes.rb`)     | `src/routes.rs` (one file)                             | [routing](guides/routing.md) |
| Action View / ERB         | Askama templates + view structs                        | [views](guides/views.md) |
| Hotwire (Turbo + Stimulus)| `htmx` mode: htmx 2.x + `htmx-ext-sse` (+ `static/app.js` for small behavior) | [frontend-htmx](guides/frontend-htmx.md) |
| Propshaft / importmap     | `static/` + `kernel/assets.rs` digests, vendored htmx  | [frontend-htmx](guides/frontend-htmx.md) |
| tailwindcss-rails         | Tailwind v4 standalone CLI pinned in `bin/setup-tailwind` (`htmx`, islands) / `@tailwindcss/vite` (`inertia`, `spa`) | [frontend-htmx](guides/frontend-htmx.md) |
| `rails new --api`         | `FRONTEND_MODE: api`: no UI, REST `/api/v1` and/or GraphQL, bearer tokens | [api](guides/api.md) |
| graphql-ruby (+ graphiql-rails, graphql-batch) | async-graphql: `src/graphql/`, `/dev/graphiql`, a DataLoader per association | [graphql](guides/graphql.md) |
| rswag / OpenAPI docs      | utoipa annotations → `/api/openapi.json`, committed `docs/openapi.json`, Scalar at `/dev/api-docs` | [api](guides/api.md#openapi-api-rest--both) |
| `has_secure_token` API tokens / Doorkeeper | `api_tokens` (SHA-256 digests, scopes, expiry), `ApiUser` extractor | [api](guides/api.md#authentication-api-tokens) |
| rack-cors                 | tower-http `CorsLayer` from `APP_CORS_ORIGINS` (exact origins) | [api](guides/api.md#cors) |
| Puma                      | The binary itself: axum on hyper (HTTP/1.1 + h2); optional direct TLS via `APP_TLS_*` | [deployment](guides/deployment.md#direct-tls-optional) |
| `docker compose up` (Rails 7.1+ devcontainer) | `compose.yaml` from `new`: the production image + Postgres, profiles `worker`, `mail` | [deployment](guides/deployment.md#docker-compose-the-whole-app-locally) |
| `favicon.ico`, meta-tags gem | `brand.toml` + `cargo offrails brand` + `Ctx` meta (title, description, canonical, OG, Twitter) from `new` | [views](guides/views.md#brand) |
| ViewComponent, a UI kit, `layout "admin"` | `{% import "components/ui.html" as ui %}` macros; `{% extends "layouts/sidebar.html" %}` (application, sidebar, marketing, auth, docs) | [views](guides/views.md#components) |
| Capybara screenshots, a Percy-style visual review | `BASE=… cargo offrails prove` → `docs/proof/PROOF.md` | [testing](guides/testing.md#proof-cargo-offrails-prove) |
| inertia-rails             | `inertia` mode: generated `kernel/inertia.rs` (Inertia v3 protocol) + `@inertiajs/react` | [frontend-inertia](guides/frontend-inertia.md) |
| jsbundling / vite_rails   | Vite 8 in `web/` (`inertia`, `spa`, islands), manifest read by `kernel/assets.rs` | [frontend-inertia](guides/frontend-inertia.md) |
| Jbuilder / serializers    | `#[derive(Serialize, TS)]` structs in `src/views/`     | [frontend-react](guides/frontend-react.md) |
| Active Job + Solid Queue  | `kernel/jobs/`: Oban-style DB-backed queue, `app worker` | [jobs](guides/jobs.md) |
| Mission Control – Jobs    | `/admin/jobs` dashboard + `app jobs …`                 | [jobs-dashboard](guides/jobs-dashboard.md) |
| Action Mailer             | `lettre` + Askama, delivered via jobs                  | [mailers](guides/mailers.md) |
| letter_opener(_web), mailer previews | `/dev/mailbox` (`Mailer::Local`), with mailer previews in its Previews tab | [mailers](guides/mailers.md), [dev-tools](guides/dev-tools.md) |
| Action Mailbox            | Not included. Webhook controller if a spec needs it    | |
| Action Cable + Solid Cable| SSE + `broadcasts` table (`kernel/pubsub.rs`)          | [realtime](guides/realtime.md) |
| Action Cable channels (two-way) | One WebSocket, topics, room actors (`kernel/hub.rs`, `kernel/channels.rs`) | [channels](guides/channels.md) |
| Active Storage            | `object_store` + `attachments` table                   | [storage](guides/storage.md) |
| Action Text               | Markdown column → `pulldown-cmark` → `ammonia`         | [views](guides/views.md#rich-text) |
| Solid Cache / `Rails.cache` | `kernel/cache.rs`: `moka` L1 + `cache_entries` table | [caching](guides/caching.md) |
| Active Support            | std + `chrono`. No monkey patching exists to port     | |
| `rails generate authentication` | [generators/authentication.md](generators/authentication.md) | [auth](guides/auth.md) |
| Credentials (`credentials.yml.enc`) | Env vars; secrets from the deploy platform     | [config](guides/config.md) |
| Environments (dev/test/prod) | `APP_ENV` + `Config`                               | [config](guides/config.md) |
| Kamal                     | Kamal 2, Fly.io, Render or Railway: the same Docker image everywhere | [deployment](guides/deployment.md) |
| Rails console             | No REPL. Write an `app task`, or use `psql` via `app db console` | [cli](cli.md) |
| Minitest / system tests   | `TestApp` integration tests via `oneshot`, on SQLite + Postgres | [testing](guides/testing.md) |
| Fixtures / FactoryBot     | `tests/app/factories.rs`                                | [testing](guides/testing.md) |
| `rails new` (+ its flags) | [interview.md](interview.md) → [generators/new.md](generators/new.md) | |
| `rails generate <kind> …` | `cargo offrails generate <kind> …` (`g`): migration, model, scaffold (`resource`), controller, page, api, job, mailer, channel, test; optional, like a snippet | [generators](generators/README.md) |
| a Rails guide's code sample, copied | `cargo offrails snippets`, `snippet show <name>`, `snippet add <name> [--as <model>]` (feed, slugs, Markdown, pager, theme toggle, backdated seeds, public page by slug) | [snippets](template/snippets/README.md) |
| `rails destroy <kind> …`  | `cargo offrails destroy <kind> <name>` (`d`): only files nobody changed | [generators](generators/README.md) |
| Engines / gems that add features | parts: `cargo offrails parts`, `add <part>`, `update-part <part>` | [template](template/README.md#choosing-parts) |
| `bin/rails` commands      | the `app` binary + cargo                               | [cli](cli.md) |
| Rack middleware           | `tower` layers in `routes.rs`                          | [routing](guides/routing.md#middleware) |
| I18n                      | English-only by default. If a spec requires locales, add `rust-i18n` and log the decision | |

## Active Record concepts

| Rails                              | Off Rails |
|------------------------------------|-----------|
| `Post.find(id)`                    | `post::find(&db, id).await?` → `AppError::NotFound` on miss |
| `Post.where(...)`, scopes          | Named query fns: `post::published(&db, page)`, `post::by_author(&db, id)` |
| `post.save`                        | `post::create(&db, author_id, new)` / `post::update(&db, post, new)` |
| `has_many :comments`               | `Relation::Comments` + `comment::for_post(&db, post_id)` |
| `belongs_to :author`               | `author_id: i64` + FK + `Relation::Author`; `post.find_related(user::Entity)` |
| `has_many :through`                | Join entity + `Related` impl with `via()` |
| `includes(:comments)` (N+1 fix)    | `find_also_related` (1:1) or `LoaderTrait::load_many` (1:N) |
| Validations                        | `Params::validate()`, plus DB constraints as the backstop |
| `validates :email, uniqueness:`    | `UNIQUE` index + map the unique violation to a field error |
| Callbacks (`after_create`)         | **Banned.** Call the side effect explicitly in the model fn |
| Concerns                           | Traits only when ≥3 models share behavior; otherwise duplicate |
| STI                                | A `kind` column + `DeriveActiveEnum` |
| Polymorphic associations           | Avoid. Separate nullable FKs with a `CHECK` that exactly one is set |
| `enum status: {...}`               | Text column + `CHECK` constraint + `DeriveActiveEnum` |
| `counter_cache`                    | Compute with `COUNT(*)`; denormalize only for a measured reason |
| `dependent: :destroy`              | `on_delete(Cascade)` on the FK in the migration |
| Soft delete (`paranoia`)           | `discarded_at TIMESTAMPTZ` + queries filter it |
| `touch: true`, `updated_at`        | Set `updated_at = Set(Utc::now())` in every update fn |
| Transactions                       | `let txn = kernel::db::begin(db).await?; … txn.commit().await?` (BEGIN IMMEDIATE on SQLite) |
| `find_each`                        | Keyset pagination loop (`WHERE id > $1 ORDER BY id LIMIT 1000`) |
| Pagination (pagy/kaminari)         | `Page` struct in `models/mod.rs` → `.limit().offset()` |
| Search (pg_search/ransack)         | Portable `LIKE`; full-text via the escape hatch (tsvector / FTS5) |

## Action Controller concepts

| Rails                         | Off Rails |
|-------------------------------|-----------|
| `before_action :authenticate` | `RequireUser` extractor as a handler argument |
| `params.require(:post).permit(...)` | `Form<PostParams>`. The struct *is* the permit list |
| `redirect_to posts_path, notice:` | `Flash::notice(jar, "…")` + `Redirect::to(&paths::posts())` |
| `render :new, status: :unprocessable_entity` | Re-render form view with errors, status 422 |
| `respond_to { format.json }`  | Separate API handlers under `/api/v1`; no content negotiation |
| `rescue_from`                 | `AppError` → `IntoResponse` in `error.rs` |
| `helper_method :current_user` | `CurrentUser` extractor passed into the view struct |
| Strong parameters             | Typed `Deserialize` structs; unknown fields ignored |
| `posts_path`, `post_path(@p)` | `paths::posts()`, `paths::post(id)` functions in `routes.rs` |
| Pundit / CanCanCan            | `policies/<resource>.rs` pure fns: `can_edit(user, post) -> bool` |

## Borrowed from Phoenix / Elixir

Rails isn't the only source. Where Elixir did it better, we copied it:

| Phoenix / Elixir | Off Rails | Guide |
|------------------|-----------|-------|
| Oban (jobs as rows with a state machine, kept history, queue limits, pause/resume, snooze/cancel, unique jobs) | `kernel/jobs/` | [jobs](guides/jobs.md) |
| Oban Web | `/admin/jobs` (live, htmx-polled) | [jobs-dashboard](guides/jobs-dashboard.md) |
| Oban Cron plugin | `src/jobs/recurring.rs` + `recurring_executions` | [jobs](guides/jobs.md#recurring-jobs-srcjobsrecurringrs-the-oban-cron-plugin) |
| Oban Lifeline / Pruner / Stager plugins | Kernel plugins run by every worker | [jobs](guides/jobs.md#plugins-pluginsrs-every-node-runs-all-of-them-all-idempotent-no-leader-election) |
| `Oban.Testing` (`assert_enqueued`, `perform_job`, `drain_queue`) | `TestApp` helpers | [jobs](guides/jobs.md#testing-obantesting-equivalents) |
| Swoosh local adapter + `/dev/mailbox` | `Mailer::Local` + `/dev/mailbox` | [mailers](guides/mailers.md) |
| Phoenix LiveDashboard | `/dev` (modest: mailbox, jobs, sockets, routes, system) | [dev-tools](guides/dev-tools.md) |
| Phoenix.Channel + phoenix.js | Inspired by Phoenix Channels; **our own protocol**, offrails.v1: `/socket` (+ SSE and long-polling fallbacks), typed `Room` actors, `seq` resume, `static/channels.js` | [channels](guides/channels.md) |
| Phoenix.Presence | `Hub::track`/`untrack`, `presence` frames (`here`, `joins`/`leaves`); people, not tabs; minimal metadata | [channels](guides/channels.md) |
| Phoenix.PubSub | per-topic in-process broadcast + Postgres `NOTIFY` between nodes | [channels](guides/channels.md) |
| libcluster / Horde | `kernel/cluster.rs`: `cluster_nodes` heartbeats, rendezvous ownership, failover rebuilt from the DB | [channels](guides/channels.md#more-than-one-server-kernelclusterrs) |
| Phoenix / Rails pending-migrations page (`ActiveRecord::PendingMigrationError`) | Dev-only page with a **Run migrations** button; `/up` 503 in production while pending | [dev-tools](guides/dev-tools.md#pending-migrations) |
| Ecto changesets | `<Model>Params::validate() -> Result<New<Model>, Errors>` | [models](guides/models.md#validations) |
| `mix phx.routes` | `app routes` / `/dev/routes` | [cli](cli.md) |
