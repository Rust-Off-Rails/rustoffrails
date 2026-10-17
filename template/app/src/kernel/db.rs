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

/// Like [`drop_idle_test_databases`], but keeps the `<prefix><keep>*` ones: the test harness
/// drops the idle template and slots of an older migration set when it builds a new one.
pub async fn drop_idle_stale_test_databases(
    admin: &impl ConnectionTrait,
    prefix: &str,
    keep: &str,
) -> Result<u64, DbErr> {
    let rows = admin
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT d.datname FROM pg_database d \
             WHERE d.datname LIKE $1 AND d.datname NOT LIKE $2 \
               AND NOT EXISTS (SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname)",
            [
                format!("{}%", prefix.replace('_', "\\_")).into(),
                format!("{}{}%", prefix, keep).replace('_', "\\_").into(),
            ],
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

/// Test harness (Postgres): does database `name` exist?
pub async fn database_exists(admin: &impl ConnectionTrait, name: &str) -> Result<bool, DbErr> {
    let row = admin
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS one FROM pg_database WHERE datname = $1",
            [name.into()],
        ))
        .await?;
    Ok(row.is_some())
}

/// Test harness (Postgres): `CREATE DATABASE <name> TEMPLATE <template>`, a file-level copy of a
/// migrated database (no migrations run). Fails while anyone is connected to `template`.
pub async fn create_database_from(
    admin: &impl ConnectionTrait,
    name: &str,
    template: &str,
) -> Result<(), DbErr> {
    let (name, template) = (database_ident(name)?, database_ident(template)?);
    admin
        .execute_unprepared(&format!(
            "CREATE DATABASE \"{name}\" TEMPLATE \"{template}\""
        ))
        .await
        .map(|_| ())
}

/// Test harness (Postgres): a test database commits without waiting for the disk
/// (`synchronous_commit = off` for every session on it). Visibility is unchanged; only a crash
/// could lose the last commits, which a test database never needs.
pub async fn skip_commit_flush(admin: &impl ConnectionTrait, name: &str) -> Result<(), DbErr> {
    let name = database_ident(name)?;
    admin
        .execute_unprepared(&format!(
            "ALTER DATABASE \"{name}\" SET synchronous_commit = off"
        ))
        .await
        .map(|_| ())
}

/// Test harness (Postgres): `ALTER DATABASE <from> RENAME TO <to>` (atomic: a half-built
/// template never carries the final name).
pub async fn rename_database(
    admin: &impl ConnectionTrait,
    from: &str,
    to: &str,
) -> Result<(), DbErr> {
    let (from, to) = (database_ident(from)?, database_ident(to)?);
    admin
        .execute_unprepared(&format!("ALTER DATABASE \"{from}\" RENAME TO \"{to}\""))
        .await
        .map(|_| ())
}

/// Test harness (Postgres): a session-level advisory lock; waits for it. The connection must be
/// a single-connection pool, so the unlock (or the disconnect) happens on the same session.
pub async fn advisory_lock(admin: &impl ConnectionTrait, key: i64) -> Result<(), DbErr> {
    admin
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_lock($1)",
            [key.into()],
        ))
        .await
        .map(|_| ())
}

/// Test harness (Postgres): takes the advisory lock if it is free; never waits.
pub async fn try_advisory_lock(admin: &impl ConnectionTrait, key: i64) -> Result<bool, DbErr> {
    let row = admin
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_try_advisory_lock($1) AS locked",
            [key.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("pg_try_advisory_lock returned no row".into()))?;
    row.try_get("", "locked")
}

/// Test harness (Postgres): releases [`advisory_lock`] / [`try_advisory_lock`].
pub async fn advisory_unlock(admin: &impl ConnectionTrait, key: i64) -> Result<(), DbErr> {
    admin
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_unlock($1)",
            [key.into()],
        ))
        .await
        .map(|_| ())
}

/// Test harness (Postgres): run once on a freshly migrated template database. Snapshots the
/// rows the migrations wrote (seed data) and every sequence, and installs
/// `offrails_test.reset()`, which brings the database back to exactly that state: every table
/// of the current schema except `seaql_migrations` truncated, the seed rows put back (in an
/// order the foreign keys accept), every sequence where the migrations left it. The
/// `offrails_test` schema is outside `current_schema()`, so the app never sees it. The reset
/// refuses (an error) when the schema no longer matches the template's (a test rolled back a
/// migration or created a table): the harness then drops the database and clones it again.
pub async fn install_test_reset(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    db.execute_unprepared(TEST_RESET_SQL).await.map(|_| ())
}

/// Test harness (Postgres): `SELECT offrails_test.reset()` (see [`install_test_reset`]).
pub async fn reset_test_database(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    db.execute_unprepared("SELECT offrails_test.reset()")
        .await
        .map(|_| ())
}

const TEST_RESET_SQL: &str = r#"
CREATE SCHEMA offrails_test;
CREATE TABLE offrails_test.seed_rows (ord bigserial PRIMARY KEY, tbl text NOT NULL, data jsonb NOT NULL);
CREATE TABLE offrails_test.seed_seqs (name text NOT NULL, last_value bigint, start_value bigint NOT NULL);
-- The schema, as text: relations, columns, defaults, indexes, constraints, types, functions.
CREATE FUNCTION offrails_test.fingerprint() RETURNS text LANGUAGE sql STABLE AS $$
  SELECT md5(coalesce(string_agg(x, E'\n' ORDER BY x), '')) FROM (
    SELECT 'c ' || relname || ' ' || relkind::text FROM pg_class
      WHERE relnamespace = current_schema()::regnamespace
    UNION ALL
    SELECT 'a ' || c.relname || '.' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod)
           || ' ' || a.attnotnull::text || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '')
      FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
      LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
      WHERE c.relnamespace = current_schema()::regnamespace AND a.attnum > 0 AND NOT a.attisdropped
    UNION ALL
    SELECT 'i ' || pg_get_indexdef(i.indexrelid) FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid
      WHERE c.relnamespace = current_schema()::regnamespace
    UNION ALL
    SELECT 'k ' || conrelid::regclass::text || ' ' || conname || ' ' || pg_get_constraintdef(oid)
      FROM pg_constraint WHERE connamespace = current_schema()::regnamespace
    UNION ALL
    SELECT 't ' || typname || ' ' || typtype::text FROM pg_type
      WHERE typnamespace = current_schema()::regnamespace AND typtype IN ('c', 'd', 'e', 'r')
    UNION ALL
    SELECT 'p ' || proname || '(' || pg_get_function_identity_arguments(oid) || ') ' || md5(prosrc)
      FROM pg_proc WHERE pronamespace = current_schema()::regnamespace
    UNION ALL
    SELECT 'm ' || coalesce(string_agg(version, ',' ORDER BY version), '') FROM seaql_migrations
  ) s(x)
$$;
CREATE TABLE offrails_test.meta AS SELECT offrails_test.fingerprint() AS fingerprint;
DO $$
DECLARE t text;
BEGIN
  FOR t IN SELECT tablename FROM pg_tables
           WHERE schemaname = current_schema() AND tablename <> 'seaql_migrations' ORDER BY 1 LOOP
    EXECUTE format('INSERT INTO offrails_test.seed_rows (tbl, data) SELECT %L, to_jsonb(x) FROM %I.%I x',
                   t, current_schema(), t);
  END LOOP;
  INSERT INTO offrails_test.seed_seqs
    SELECT format('%I.%I', schemaname, sequencename), last_value, start_value
    FROM pg_sequences WHERE schemaname = current_schema();
END $$;
CREATE FUNCTION offrails_test.reset() RETURNS void LANGUAGE plpgsql AS $$
DECLARE
  tables text;
  pending text[];
  t text;
  cols text;
  rcols text;
  progress boolean;
BEGIN
  IF offrails_test.fingerprint() IS DISTINCT FROM (SELECT fingerprint FROM offrails_test.meta) THEN
    RAISE EXCEPTION 'offrails_test.reset: the schema changed (a test rolled back a migration or made a table)';
  END IF;
  -- Only the tables with rows: truncating an empty table still swaps its files (slow).
  FOR t IN SELECT tablename FROM pg_tables
           WHERE schemaname = current_schema() AND tablename <> 'seaql_migrations' LOOP
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I.%I)', current_schema(), t) INTO progress;
    IF progress THEN
      tables := concat_ws(', ', tables, format('%I.%I', current_schema(), t));
    END IF;
  END LOOP;
  IF tables IS NOT NULL THEN
    EXECUTE 'TRUNCATE ' || tables || ' CASCADE';
  END IF;
  SELECT array_agg(DISTINCT tbl) INTO pending FROM offrails_test.seed_rows;
  WHILE coalesce(cardinality(pending), 0) > 0 LOOP
    progress := false;
    FOREACH t IN ARRAY pending LOOP
      SELECT string_agg(quote_ident(attname), ', ' ORDER BY attnum),
             string_agg('r.' || quote_ident(attname), ', ' ORDER BY attnum) INTO cols, rcols
        FROM pg_attribute
        WHERE attrelid = format('%I.%I', current_schema(), t)::regclass
          AND attnum > 0 AND NOT attisdropped AND attgenerated = '';
      BEGIN
        EXECUTE format(
          'INSERT INTO %I.%I (%s) OVERRIDING SYSTEM VALUE SELECT %s FROM offrails_test.seed_rows s, '
          'jsonb_populate_record(NULL::%I.%I, s.data) r WHERE s.tbl = %L ORDER BY s.ord',
          current_schema(), t, cols, rcols, current_schema(), t, t);
        pending := array_remove(pending, t);
        progress := true;
      EXCEPTION WHEN foreign_key_violation THEN
        NULL; -- a table it references isn't back yet: next round
      END;
    END LOOP;
    IF NOT progress THEN
      RAISE EXCEPTION 'offrails_test.reset: cannot restore the seed rows of %', pending;
    END IF;
  END LOOP;
  PERFORM setval(name, coalesce(last_value, start_value), last_value IS NOT NULL)
    FROM offrails_test.seed_seqs;
END $$;
"#;

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
