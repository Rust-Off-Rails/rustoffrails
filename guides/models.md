# Models (Active Record → SeaORM)

> **TL;DR**
> - One file per table: the SeaORM entity, `Params` (untrusted) → `validate()` → `New`, and every query for that table; no callbacks, side effects are explicit in model fns.
> - Queries take `db: &impl ConnectionTrait`; transactions start with `kernel::db::begin`, never `db.begin()`.
> - Set timestamps in Rust; return `AppError`; no raw SQL; unique violations become validation errors via `sql_err()`.
> - Inside a Postgres transaction use `on_conflict_do_nothing` / `try_insert`: a caught unique violation aborts the transaction.
>
> Read the rest when writing models, validations, associations, enums/money, or pagination.

A model is one module in `src/models/` holding the SeaORM entity for a table, its params, its validation, and every query that touches the table. The same code runs on SQLite and Postgres. Read [`database.md`](database.md) for the portability rules.

**Its files.** What generators and snippets write is a folder, so no file grows past a screen or two: `src/models/post/mod.rs` (the entity, its relations, its label, and `pub use params::*; pub use queries::*;`), `params.rs` (`PostParams` → `validate()` → `NewPost`, the edit form's `From<&Model>`, validation unit tests) and `queries.rs` (`find_for`, `all_for`, `create`…). Callers see one module either way: `post::find_for`, `post::PostParams`, `Post`. A model that is one file, `src/models/post.rs` (a part's `user.rs`, or one written by hand), is the same code in one place; both are fine, and the generators read both. The anatomy below is the one-file form; split it at the `// ----------` lines.

> **Entity format: "compact"** (`DeriveEntityModel` + `Relation` enum + `impl Related`). Verified with SeaORM 2.0.4, where it's still `sea-orm-cli generate entity`'s default. 2.x also offers "dense" and "expanded" formats; don't use them, so every model in every app reads the same. Check `Cargo.lock`: if `sea-orm`'s major differs from 2, read its migration guide before writing entities.

## Anatomy of a model (`src/models/post.rs`, or `post/{mod,params,queries}.rs`)

```rust
use chrono::Utc;
use sea_orm::{entity::prelude::*, ActiveValue::Set, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::models::{Errors, Page};

// ---------- Entity: mirrors the migration exactly ----------

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub author_id: i64,
    pub title: String,
    #[sea_orm(column_type = "Text")]
    pub body: String,
    pub status: PostStatus,
    pub published_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(belongs_to = "super::user::Entity", from = "Column::AuthorId", to = "super::user::Column::Id", on_delete = "Cascade")]
    Author,
    #[sea_orm(has_many = "super::comment::Entity")]
    Comments,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef { Relation::Author.def() }
}
impl Related<super::comment::Entity> for Entity {
    fn to() -> RelationDef { Relation::Comments.def() }
}

/// Must stay empty. Callbacks are banned (see "Side effects").
impl ActiveModelBehavior for ActiveModel {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PostStatus {
    Draft,          // stored as "draft"
    Published,
}

// ---------- Params & validation (Active Model validations) ----------

/// Untrusted input. This struct IS the permit list. All strings so bad input can be re-rendered.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
// inertia/spa modes: also derive ts_rs::TS with #[ts(export)]
pub struct PostParams {
    #[serde(default)] pub title: String,
    #[serde(default)] pub body: String,
}

/// Validated, typed data. Only `validate()` can produce one.
pub struct NewPost { pub title: String, pub body: String }

impl PostParams {
    pub fn validate(&self) -> Result<NewPost, Errors> {
        let mut errors = Errors::default();
        let title = self.title.trim();
        if title.is_empty() { errors.add("title", "can't be blank"); }
        else if title.chars().count() > 200 { errors.add("title", "is too long (maximum is 200 characters)"); }
        if self.body.trim().is_empty() { errors.add("body", "can't be blank"); }
        errors.into_result(|| NewPost { title: title.to_owned(), body: self.body.trim().to_owned() })
    }
}

impl From<&Model> for PostParams {
    fn from(p: &Model) -> Self { Self { title: p.title.clone(), body: p.body.clone() } }
}

// ---------- Queries ("scopes" and persistence) ----------

pub async fn find(db: &impl ConnectionTrait, id: i64) -> Result<Model, AppError> {
    Entity::find_by_id(id).one(db).await?.ok_or(AppError::NotFound)
}

/// A scope. Name it after the business concept.
pub async fn published(db: &impl ConnectionTrait, page: Page) -> Result<Vec<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::Status.eq(PostStatus::Published))
        .order_by_desc(Column::PublishedAt)
        .limit(page.per()).offset(page.offset())
        .all(db).await?)
}

pub async fn create(db: &impl ConnectionTrait, author_id: i64, new: NewPost) -> Result<Model, AppError> {
    let now = Utc::now();
    Ok(ActiveModel {
        author_id: Set(author_id),
        title: Set(new.title),
        body: Set(new.body),
        status: Set(PostStatus::Draft),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db).await?)
}

pub async fn update(db: &impl ConnectionTrait, post: Model, new: NewPost) -> Result<Model, AppError> {
    let mut am: ActiveModel = post.into();
    am.title = Set(new.title);
    am.body = Set(new.body);
    am.updated_at = Set(Utc::now());
    Ok(am.update(db).await?)
}

pub async fn destroy(db: &impl ConnectionTrait, id: i64) -> Result<(), AppError> {
    Entity::delete_by_id(id).exec(db).await?;
    Ok(())
}
```

`src/models/mod.rs` declares each module and exports a friendly alias: `pub type Post = post::Model;` (generated lines end in `// posts`, the owner's marker). Call sites read `post::find(&db, id)` and `Post`.

## Rules

1. **The entity mirrors the migration.** Every column, type, nullability and FK. Write both in the same step. Factories in the test suite insert and read back a row of every entity on both backends, which is how mismatches get caught.
2. **Queries take `db: &impl ConnectionTrait`.** That accepts both `&DatabaseConnection` and `&DatabaseTransaction`.
3. **Transactions:** `let txn = kernel::db::begin(db).await?; … txn.commit().await?;`, never `db.begin()` (on SQLite that's a deferred transaction that fails with "database is locked" under concurrent writes; see [database](database.md#kerneldbrs-spec)). Multi-step writes happen in one transaction inside the model fn that owns the process.
4. **Set `created_at`/`updated_at` in Rust** (`Utc::now()`), in every insert and update. Don't rely on DB defaults (they differ by backend) or on `ActiveModelBehavior` hooks.
5. **Return `AppError`.** `DbErr` converts via `?` (see `errors-and-logging.md`).
6. **No raw SQL in models.** SeaORM's query builder emits correct SQL per backend. If you truly need SQL, see `database.md#escape-hatch`.
7. **Lookups by id are scoped to the actor** when the app has accounts (`TENANCY` isn't `none`). See the next section.
8. **Inserts that may hit a unique index use `ON CONFLICT DO NOTHING`, never insert-then-catch.** On Postgres the failed insert aborts the whole transaction ("current transaction is aborted"), so a caught duplicate breaks every statement after it; SQLite doesn't, so only the Postgres run shows it. Get-or-create, dedupe, "join if not a member", first login: `Entity::insert(am).on_conflict_do_nothing_on([Column::Email]).exec_without_returning(&txn).await?` returns `TryInsertResult::{Inserted, Conflicted}` (SeaORM 2.0.4), then select the row. A form whose duplicate is a user error (`has already been taken`) may still map the violation with `sql_err()` **outside** a larger transaction ([validations](#validations)).
9. **Public model fns that do I/O are `#[tracing::instrument(skip_all)]`** (add `fields(id)` for an id at most; a helper called per row: `level = "debug"`, [the one rule](errors-and-logging.md#spans)); never secrets or personal data in span fields.

## Scoped lookups (tenancy)

An unscoped `find(db, id)` in one handler is enough for one user to read another user's data. So the compiler, not your memory, enforces the scope: **the only public lookups are the ones that take the actor.**

| Function | Visibility | Who calls it |
|----------|-----------|--------------|
| `find_for(db, &user, id)`, `all_for(db, &user, …)` | `pub` | handlers, channels, resolvers |
| `update_for` / `destroy_for(db, &user, record)` | `pub` | handlers: re-check the policy, `Forbidden` (403) if it says no |
| `find(db, id)`, `destroy(db, record)` | `pub(in crate::models)` | other model fns only |
| `find_system(db, id)` (only if needed) | `pub` | jobs and tasks, which act without a request (args carry ids) |

`find_for` returns `AppError::NotFound` for a record the actor may not see, so a hidden id looks the same as a missing one (404). A record the actor may see but not change is a 403 from the policy ([auth](auth.md#authorization)).

```rust
// src/models/board.rs  (TENANCY: per-user: filter by owner)
/// Unscoped. Model-private, so a handler can't compile a lookup that skips the actor.
pub(in crate::models) async fn find(db: &impl ConnectionTrait, id: i64) -> Result<Model, AppError> {
    Entity::find_by_id(id).one(db).await?.ok_or(AppError::NotFound)
}

/// The board if `user` may see it, else `NotFound`.
pub async fn find_for(db: &impl ConnectionTrait, user: &User, id: i64) -> Result<Model, AppError> {
    Entity::find_by_id(id)
        .filter(Column::OwnerId.eq(user.id))
        .one(db).await?
        .ok_or(AppError::NotFound)
}

/// Deletes `board` if `user` may (`policies::boards::can_destroy`), else `Forbidden`.
pub async fn destroy_for(db: &DatabaseConnection, user: &User, board: Model) -> Result<(), AppError> {
    crate::policies::authorize(crate::policies::boards::can_destroy(user, &board))?;
    destroy(db, board).await
}
```

- **`TENANCY: teams`:** scope through memberships. `find_for` loads the row, then calls `team::authorize(db, user, row.team_id)` (`NotFound` unless the user is a member). `all_for` filters with `Column::TeamId.is_in(membership::team_ids(db, user.id).await?)`. Child records (lists, cards) scope through their parent's `find_for`, never through their own id alone.
- **Shared reads** (the spec lets every user see every record, like the reference app's books): `find_for` still takes the actor and checks `policies::<plural>::can_view`. The signature stays the same, so a later spec change touches one function.
- **Tests** are another crate: they read rows with `<model>::Entity::find_by_id` (a `reload_<singular>` factory helper), never through `find`.
- **Handlers that read nothing private** (a `new` form, a public index) mark the line `// unscoped: <AC id or reason>`. The guard test below accepts the marker and nothing else.

**Mechanical checks** (generated by `new` when `TENANCY` isn't `none`):

1. **The compiler:** raw `find`/`destroy` are model-private.
2. **`tests/app/guard_test.rs`:** reads `src/controllers`, `src/channels`, `src/graphql` and `src/api`, and fails on `RequireUser(_`, `ApiUser(_`, `::find(`, `::destroy(`, `::all(`, `::find_system(` and `Entity::find` on any line without the `// unscoped:` marker. Template: [test-standards §4](test-standards.md#4-tenant-isolation-suite-every-app-with-accounts).
3. **`tests/app/tenancy_test.rs`:** requests every id-taking route in `ROUTES` with another tenant's ids ([test-standards §4](test-standards.md#4-tenant-isolation-suite-every-app-with-accounts)).
4. **`offrails check`** fails when `TENANCY` isn't `none` and either test file is missing ([cli](../cli.md)).

## Side effects (no callbacks)

`ActiveModelBehavior` hooks (`before_save`, `after_save`) are banned. Side effects are explicit calls inside the model fn, in the same transaction:

```rust
pub async fn create(db: &DatabaseConnection, post: &post::Model, author: &user::Model, new: NewComment) -> Result<Model, AppError> {
    let txn = kernel::db::begin(db).await?;
    let comment = ActiveModel { /* … */ }.insert(&txn).await?;
    jobs::enqueue(&txn, NotifyAuthorJob { comment_id: comment.id }).await?;  // atomic with the insert
    txn.commit().await?;
    Ok(comment)
}
```

Enqueuing inside the transaction gives you `after_commit` semantics for free: if the transaction rolls back, the job never existed.

## Validations

Shared helpers in `src/models/mod.rs`:

```rust
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]   // inertia/spa: + ts_rs::TS (#[ts(export)])
pub struct Errors(pub std::collections::BTreeMap<String, Vec<String>>);   // Deserialize: inertia round-trips it through the `_errors` cookie

impl Errors {
    pub fn add(&mut self, field: &str, msg: &str);
    pub fn single(field: &str, msg: &str) -> Self;
    pub fn on(&self, field: &str) -> &[String];         // used by templates
    pub fn is_empty(&self) -> bool;
    pub fn full_messages(&self) -> Vec<String>;         // "Title can't be blank"
    pub fn into_result<T>(self, ok: impl FnOnce() -> T) -> Result<T, Errors>;
}
```

- Use Rails wording: `can't be blank`, `is too long (maximum is N characters)`, `has already been taken`, `is invalid`, `must be greater than N`, `is not included in the list`.
- **The DB is the backstop.** Anything expressible as `NOT NULL`, `UNIQUE`, `CHECK` or FK is *also* a constraint in the migration.
- **Uniqueness:** don't pre-query (that's a race). Insert, then catch the violation portably:

```rust
match am.insert(db).await {
    Ok(m) => Ok(m),
    Err(e) if matches!(e.sql_err(), Some(sea_orm::SqlErr::UniqueConstraintViolation(_))) =>
        Err(AppError::Invalid(Errors::single("email", "has already been taken"))),
    Err(e) => Err(e.into()),
}
```

- **Inside a transaction on Postgres, don't rely on catching the violation:** the failed statement aborts the whole transaction, and every later statement in it fails with "current transaction is aborted". When the insert is one step of a larger transaction (sign-up on first login, get-or-create), use `Entity::insert(am).on_conflict_do_nothing_on([Column::Email]).exec_without_returning(&txn)` (or `try_insert` and match `TryInsertResult::Conflicted`), then select the row. SQLite doesn't have this failure mode, so only the Postgres run catches it.
- **Composite uniqueness** ("one review per reader per book") works the same way: a unique index on `(book_id, author_id)`, and the violation maps to a `base` error (`Errors::single("base", "You have already reviewed this book")`). Return early **before** any broadcast or enqueue in the same transaction, so nothing announces the failed insert.
- Validations that need the DB ("author must be active") go in the model's `create` and return `AppError::Invalid`.
- **Optional fields arrive as strings.** `Params` fields are all `String` (with `#[serde(default)]`), so a re-rendered form shows exactly what was typed. `validate` turns `""` into `None`, parses with `NaiveDate::parse_from_str(s, "%Y-%m-%d")` (an `<input type="date">` sends that format) or `s.parse::<i32>()`, and reports `is not a date` or `must be between 1 and 5`. `From<&Model> for Params` prefills edit forms (`published_on.map(|d| d.to_string())`).
- **File fields** are validated in the same `validate(&self, upload: Option<Upload>)` call ([storage](storage.md#the-model-modelsblobrs)), so every error shows on one re-render.

## Associations

| Rails                     | SeaORM |
|---------------------------|--------|
| `belongs_to :author`      | FK column + `Relation::Author` + `impl Related<user::Entity>` |
| `has_many :comments`      | `Relation::Comments` + `impl Related` + `comment::for_post(db, post_id)` in `comment.rs` |
| `post.author`             | `post.find_related(user::Entity).one(db)` |
| `has_many :through`       | Join entity `post_tag` + `impl Related<tag::Entity> for post::Entity` with `via()` |
| `includes(:author)` (N+1) | `Entity::find().find_also_related(user::Entity)` (1:1, verified). It yields `(Model, Option<user::Model>)`; drop the `None`s with `filter_map` or treat them as `NotFound` |
| `includes(:comments)` | `posts.load_many(comment::Entity, db)` (`LoaderTrait`, 1:N), or a child query filtered by `parent_id` |
| `includes(:cover_blob)` on a page | Collect the FK ids, then one `Entity::find().filter(Column::Id.is_in(ids))` into a `HashMap` (`blob::by_ids`) |
| `dependent: :destroy`     | `ON DELETE CASCADE` FK in the migration |

**N+1 is a bug.** Never query inside a loop over rows. Every list page has an `assert_query_count_stable` test ([test-standards §6](test-standards.md#6-performance-guards)).

`impl Related` both ways: when `review` `belongs_to` `book`, `book.rs` gets `#[sea_orm(has_many = "super::review::Entity")] Reviews` plus `impl Related<super::review::Entity>`. Verified on SeaORM 2.0.4, entities in the same `models` module refer to each other as `super::<model>::Entity`.

## Enums, money, time, IDs

- **Enums:** `DeriveActiveEnum` with `rs_type = "String", db_type = "String(StringLen::None)", rename_all = "snake_case"` (verified in SeaORM 2.0.4; it matches the migration's `string(...)` column), stored as text plus a `CHECK` constraint. Never native DB enums (SQLite has none). Filter with the enum itself: `Column::Status.eq(PostStatus::Published)`. For `update_many().col_expr(...)`, use `Expr::value(PostStatus::Published.to_value())`.
- **Expression helpers (sea-query 1.0):** `Expr::col(Column::Attempt).add(1)` needs `use sea_orm::sea_query::ExprTrait;`. Import it **inside the function** that uses it, because a module-level import makes `i32::max`/`min` ambiguous.
- **Money:** `i64` cents. Never floats or decimals.
- **Time:** `DateTimeUtc` for instants, `Date` (chrono `NaiveDate`) for calendar dates.
- **IDs:** `i64` auto-increment PKs. For URL-exposed IDs where enumeration matters, add `public_id: Uuid` (unique, set to `Uuid::new_v4()` in Rust) and route by it.
- **JSON:** `Json` (`serde_json::Value`) columns work on both backends, but you can't query inside them portably. Use them only for opaque blobs.

## Pagination & search

- `Page` (in `models/mod.rs`): deserialized from `?page=`, `per` clamped to 1..=100 (default 25), with `per()`/`offset()`. A garbage `?page=x` → 422. To know there's a next page without a `COUNT(*)`, fetch `per() + 1` rows, set `more = rows.len() > per`, then `truncate(per)`; the page shows `ui::pagination(page, page + more, base)` ([views](views.md#components)). Order by `created_at DESC, id DESC`, so ties are stable. For big or infinite lists, use keyset pagination: `.filter(Column::Id.lt(cursor)).order_by_desc(Column::Id).limit(per)`.
- Search: portable `LIKE` via `Expr::expr(Func::lower(Expr::col(Column::Title))).like(LikeExpr::new(format!("%{}%", like_escape(&q.to_lowercase()))).escape('\\'))`, where `like_escape` (in `models/mod.rs`) prefixes `%`, `_` and `\` with `\`. Without it `100%` matches everything, `a_b` matches `axb`, and a `%` typed by a user turns the query into a leading-wildcard scan. (`lower()` is ASCII-only on SQLite and Unicode on Postgres: for exact matching lowercase in Rust on write, [database](database.md).) For real full-text search, see `database.md#escape-hatch` (Postgres `tsvector` / SQLite FTS5, both branches required).
