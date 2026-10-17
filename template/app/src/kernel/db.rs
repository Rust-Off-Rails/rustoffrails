//! Database connection setup. `DATABASE_URL`'s scheme picks the backend at runtime.

use std::time::Duration;

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr,
    SqliteTransactionMode, Statement, TransactionOptions, TransactionTrait,
    sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous},
};

pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, DbErr> {
    let mut opts = ConnectOptions::new(url.to_owned());
    opts.sqlx_logging(false);
    // Overload is a 503 with Retry-After, not a request that waits forever for a connection
    // (errors-and-logging.md#response-mapping): `AppError::Unavailable`.
    opts.acquire_timeout(Duration::from_secs(5));
    if url.starts_with("sqlite:") {
        // Every connection to an in-memory database is a different, empty database.
        let in_memory = url.contains(":memory:") || url.contains("mode=memory");
        opts.max_connections(if in_memory { 1 } else { max_connections })
            .min_connections(if in_memory { 1 } else { 0 })
            // WHY: idle connections close after a minute, so a burst's extra caches don't stay.
            .idle_timeout(Duration::from_secs(60));
        if !in_memory {
            ensure_parent_dir(url);
        }
        // Applied to every pooled connection, not once per pool.
        opts.map_sqlx_sqlite_opts(move |o| {
            let o = o
                .create_if_missing(true)
                .foreign_keys(true) // critical: SQLite ignores FKs otherwise
                .busy_timeout(Duration::from_secs(5))
                .synchronous(SqliteSynchronous::Normal)
                // WHY: each connection keeps its own page cache; 1 MiB (SQLite's default is 2 MiB)
                // keeps a small app's memory flat, and the OS page cache still holds the file.
                // A read-heavy app with a big database raises it (database.md#sqlite-memory).
                .pragma("cache_size", "-1024");
            if in_memory {
                o
            } else {
                o.journal_mode(SqliteJournalMode::Wal)
            }
        });
    } else {
        opts.max_connections(max_connections);
    }
    Database::connect(opts).await
}

/// `sqlite://storage/development.sqlite3?mode=rwc`: make sure `storage/` exists.
fn ensure_parent_dir(url: &str) {
    let path = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:")
        .split('?')
        .next()
        .unwrap_or_default();
    if let Some(dir) = std::path::Path::new(path).parent()
        && !dir.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(dir);
    }
}

/// Every transaction starts here, never with `db.begin()`.
///
/// On SQLite this is `BEGIN IMMEDIATE`. A plain `db.begin()` is deferred: it reads first, and
/// when it then writes after another connection wrote in between (the job worker's heartbeat
/// does, every second), SQLite fails it with `SQLITE_BUSY_SNAPSHOT` ("database is locked"),
/// which `busy_timeout` cannot retry. Postgres ignores the mode.
pub async fn begin<C: TransactionTrait + ?Sized>(db: &C) -> Result<C::Transaction, DbErr> {
    db.begin_with_options(TransactionOptions {
        sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
        ..Default::default()
    })
    .await
}

/// `GET /up`'s database check: a bare `SELECT 1`. Raw SQL stays in the kernel.
pub async fn ping(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    db.execute_unprepared("SELECT 1").await.map(|_| ())
}

/// Postgres identifiers that go into DDL are never bound parameters, so only plain names pass.
fn database_ident(name: &str) -> Result<&str, DbErr> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(name)
    } else {
        Err(DbErr::Custom(format!("unsafe database name: {name:?}")))
    }
}

/// Test harness (Postgres): `CREATE DATABASE <name>` through an admin connection.
pub async fn create_database(admin: &impl ConnectionTrait, name: &str) -> Result<(), DbErr> {
    let name = database_ident(name)?;
    admin
        .execute_unprepared(&format!("CREATE DATABASE \"{name}\""))
        .await
        .map(|_| ())
}

/// Test harness (Postgres): `DROP DATABASE IF EXISTS <name> WITH (FORCE)`.
pub async fn drop_database(admin: &impl ConnectionTrait, name: &str) -> Result<(), DbErr> {
    let name = database_ident(name)?;
    admin
        .execute_unprepared(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
        .await
        .map(|_| ())
}

/// Drops every `<prefix>*` database that has **no open connections** (`pg_stat_activity`), so
/// it is safe on a shared server and while another suite runs. Returns how many it dropped.
/// Backs `app task test_db_cleanup`; never call it from a test.
pub async fn drop_idle_test_databases(
    admin: &impl ConnectionTrait,
    prefix: &str,
) -> Result<u64, DbErr> {
    let rows = admin
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT d.datname FROM pg_database d \
             WHERE d.datname LIKE $1 \
               AND NOT EXISTS (SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname)",
            [format!("{}%", prefix.replace('_', "\\_")).into()],
        ))
        .await?;
    let mut dropped = 0;
    for row in rows {
        let name: String = row.try_get("", "datname")?;
        if !name.starts_with(prefix) || database_ident(&name).is_err() {
            continue;
        }
        drop_database(admin, &name).await?;
        dropped += 1;
    }
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::database_ident;

    #[test]
    fn only_plain_names_reach_ddl() {
        assert!(database_ident("test_offrails_app_0af3").is_ok());
        assert!(database_ident("").is_err());
        assert!(database_ident("x\"; DROP DATABASE postgres; --").is_err());
        assert!(database_ident("has space").is_err());
    }
}
