# Database: one codebase, SQLite and Postgres

> **TL;DR**
> - `DATABASE_URL` picks SQLite or Postgres at runtime; both drivers are always compiled in; tests run on both.
> - Everything goes through SeaORM; backend-specific SQL only in `src/kernel/` or behind an explicit backend `match`.
> - SQLite: WAL, `foreign_keys = ON`, `busy_timeout`; in-memory test DBs use one connection.
> - Write transactions on SQLite start with `kernel::db::begin` (`BEGIN IMMEDIATE`), or they fail with "database is locked" when the job worker writes.
>
> Read the rest when touching connections, the kernel's db module, portability, or moving data between backends.

`DATABASE_URL`'s scheme picks the backend at runtime. Nothing is recompiled. Both drivers are always compiled in.

| `DATABASE_URL` | Backend |
|----------------|---------|
| `sqlite://storage/development.sqlite3?mode=rwc` | SQLite file (dev default; `storage/` is git-ignored) |
| `sqlite::memory:` | SQLite in-memory (tests) |
| `postgres://user:pass@host:5432/db` | Postgres |

## Which backend where

- **Development:** SQLite. There's nothing to install and `bin/setup` just works. Switch to Postgres locally with `docker compose up -d db` and the Postgres URL in `.env` when debugging a Postgres-only issue.
- **Test:** SQLite in-memory by default (fast, isolated). `TEST_DATABASE_URL=postgres://…` runs the whole suite on Postgres. **CI runs both**, and both must pass.
- **Production:** Postgres (managed, from the deploy platform). Single-server apps may run SQLite in production, as Rails 8 does, if the [interview](../interview.md) chose it: the DB lives on a persistent volume and is backed up (Litestream or the platform's volume snapshots).

## `kernel/db.rs` spec

`pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, DbErr>`. One path for both backends: `sea_orm::Database::connect(ConnectOptions)`. **The file is [`template/app/src/kernel/db.rs`](../template/app/src/kernel/db.rs)** (in an app: `src/kernel/db.rs`, from `.offrails/template/app/`), verified with SeaORM 2.0.4 / sqlx 0.9. What it does and why:

- SQLite: `create_if_missing`, `foreign_keys(true)` (critical: SQLite ignores FKs otherwise), `busy_timeout(5 s)`, `synchronous(Normal)`, WAL for file databases, a 1 MiB `cache_size` per connection, idle connections closed after 60 s, and a single connection for in-memory databases (each connection to `:memory:` is a different, empty database).
- `sea_orm::sqlx` is the re-export; never a separate sqlx dependency.
- `begin` is the only way a transaction starts:

```rust
/// Every transaction starts here, never with `db.begin()`.
pub async fn begin<C: TransactionTrait + ?Sized>(db: &C) -> Result<C::Transaction, DbErr> {
    db.begin_with_options(TransactionOptions {
        sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
        ..Default::default()
    })
    .await
}
```

- **Transactions are `BEGIN IMMEDIATE` on SQLite** (`kernel::db::begin`; Postgres ignores the mode, and on a transaction it nests as a savepoint). A plain `db.begin()` is deferred: it reads first, and when it then writes after another connection wrote in between (the job worker's heartbeat writes every few seconds), SQLite fails it with `SQLITE_BUSY_SNAPSHOT` ("database is locked", code 517). `busy_timeout` can't retry that. Rails 8 makes the same choice. The in-memory test database has a single connection and can't show the bug, so a kernel test opens a **file** database with a pool of 4, reads in a transaction, writes from another connection, then writes in the transaction. It fails with `db.begin()` and passes with `kernel::db::begin`.

- `max_connections` comes from `DATABASE_POOL` (default 4 on SQLite, 10 on Postgres). **Always set it:** SeaORM defaults SQLite pools to 1 connection.
- **`opts.acquire_timeout(Duration::from_secs(5))`** (sea-orm 2.0.4 `ConnectOptions::acquire_timeout`; sqlx's default is 30 s). A request that can't get a connection in 5 s fails with `DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)`, which `AppError` maps to **503 + `Retry-After`** ([errors](errors-and-logging.md#response-mapping)), instead of hanging until the request timeout. Live re-fetches are additionally capped to a third of the pool ([realtime](realtime.md#keep-re-fetches-from-starving-user-requests)).
- The pragmas are applied to every pooled connection (`map_sqlx_sqlite_opts`), not once per pool.
- `TestApp` calls `connect` and then `db.set_metric_callback(...)` to count queries. The callback needs `&mut`, so set it before the connection is cloned into `AppState`.
- A test asserts `PRAGMA foreign_keys` returns 1 (`query_one_raw(Statement::from_string(DbBackend::Sqlite, "PRAGMA foreign_keys"))`).

## SQLite memory

Small apps are lean by default ([template](../template/README.md#layout), memory):

- **`cache_size` is 1 MiB per connection** (`PRAGMA cache_size = -1024`; SQLite's default is 2 MiB). Each pooled connection keeps its own page cache, so the pool size multiplies it. The OS page cache still holds the database file, so a small app loses little. Raise it (e.g. `-8192` for 8 MiB) when a read-heavy app's database outgrows the cache and `/dev/dashboard` or `EXPLAIN` shows the same pages read again and again.
- **The pool is 4 connections on SQLite** (`DATABASE_POOL`; 10 on Postgres). SQLite has one writer at a time, and WAL readers rarely need more. Idle connections close after 60 s, so a burst's extra caches don't stay.
- Postgres is unaffected: its memory is the server's, and `DATABASE_POOL` sizes the client pool.
- Measured on the bare template (release, macOS, 1000 requests): 15 MiB RSS ([`tools/ci/memory-budget.sh`](../tools/ci/memory-budget.sh)). Runtime threads are the other lever: `APP_WORKER_THREADS` and `APP_BLOCKING_THREADS` ([config](config.md#srcconfigrs)).

## Portability rules

1. **All app queries go through SeaORM's builder.** It quotes, parameterizes, and emits the right dialect.
2. **Use only portable types:** `i64`, `i32`, `bool`, `String`, `DateTimeUtc`, `Date`, `Uuid`, `Json` (opaque), `Vec<u8>`. No arrays, no `JSONB` operators, no `INET`, no native enums, no `NUMERIC` money.
3. **No DB-side logic:** no triggers, stored procedures, or views. Defaults and timestamps are set in Rust.
4. **Case-insensitive matching:** lowercase in Rust on write, compare lowercased on read. Never `ILIKE`.
5. **Ordering is explicit.** Always `order_by` a unique column last (`id`). Backends order ties differently.
6. **No reliance on error message text.** Use `DbErr::sql_err()` variants.
7. **Booleans and timestamps round-trip** through SeaORM correctly on both backends. Don't compare them with raw SQL literals.

## Escape hatch

When a spec needs something the builder can't express portably (full-text search, `SKIP LOCKED`, upserts with complex conflict targets):

```rust
match db.get_database_backend() {
    DbBackend::Postgres => {
        let stmt = Statement::from_sql_and_values(DbBackend::Postgres, "SELECT … WHERE x = $1", [value.into()]);
        db.query_all_raw(stmt).await?        // SeaORM 2: *_raw for Statements (execute_raw, query_one_raw, query_all_raw)
    }
    DbBackend::Sqlite => { /* SQLite SQL with `?` binds, or a simpler portable fallback */ }
    _ => unreachable!("only SQLite and Postgres are supported"),
}
```

- Both branches are required, and both are tested (the CI matrix covers it).
- Values are always bound (`$1` for Postgres, `?` for SQLite). Never `format!` values into SQL.
- Log every escape-hatch use in `docs/decisions.md`.
- Full-text search: Postgres `tsvector` generated column + GIN index; SQLite FTS5 virtual table kept in sync by the model fn that writes the row (not by triggers).

## Kernel features per backend

| Kernel | SQLite | Postgres |
|--------|--------|----------|
| jobs | Portable optimistic claim (`UPDATE … SET state = 'executing' WHERE id = ? AND state = 'available'`, check rows affected) | Same; may switch to `FOR UPDATE SKIP LOCKED` behind the escape hatch at scale |
| cache | `cache_entries` table | same |
| pubsub | `broadcasts` table polling | same (optionally woken by `LISTEN/NOTIFY`) |
| sessions | `sessions` table | same |

## Moving data between backends

Not automated. Schemas are identical, so for a one-time move (SQLite prod → Postgres), write an `app task copy_db <from_url> <to_url>` that streams each table in FK order with keyset pagination. Log it in decisions.
