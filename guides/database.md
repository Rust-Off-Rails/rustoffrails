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

`pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, DbErr>`. One path for both backends: `sea_orm::Database::connect(ConnectOptions)`. Verified with SeaORM 2.0.4 / sqlx 0.9:

```rust
use std::time::Duration;
use sea_orm::sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};   // the re-export; never a separate sqlx dependency
use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};

pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, DbErr> {
    let mut opts = ConnectOptions::new(url.to_owned());
    opts.sqlx_logging(false);
    if url.starts_with("sqlite:") {
        // Every connection to an in-memory database is a different, empty database.
        let in_memory = url.contains(":memory:") || url.contains("mode=memory");
        opts.max_connections(if in_memory { 1 } else { max_connections })
            .min_connections(if in_memory { 1 } else { 0 });
        opts.map_sqlx_sqlite_opts(move |o| {
            let o = o
                .create_if_missing(true)
                .foreign_keys(true)                       // critical: SQLite ignores FKs otherwise
                .busy_timeout(Duration::from_secs(5))
                .synchronous(SqliteSynchronous::Normal);
            if in_memory { o } else { o.journal_mode(SqliteJournalMode::Wal) }
        });
    } else {
        opts.max_connections(max_connections);
    }
    Database::connect(opts).await
}

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

- `max_connections` comes from `DATABASE_POOL` (default 10). **Always set it:** SeaORM defaults SQLite pools to 1 connection.
- The pragmas are applied to every pooled connection (`map_sqlx_sqlite_opts`), not once per pool.
- `TestApp` calls `connect` and then `db.set_metric_callback(...)` to count queries. The callback needs `&mut`, so set it before the connection is cloned into `AppState`.
- A test asserts `PRAGMA foreign_keys` returns 1 (`query_one_raw(Statement::from_string(DbBackend::Sqlite, "PRAGMA foreign_keys"))`).

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
