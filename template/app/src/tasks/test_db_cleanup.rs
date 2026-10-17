//! Drops idle `test_<crate>_*` Postgres databases: the test template and slots (rebuilt on the
//! next run), and any `test_<crate>_<hex>` database an older harness leaked.
//!
//! Usage: `app task test_db_cleanup postgres://postgres:postgres@localhost:5432/postgres`
//!
//! Only **this app's** databases (the crate name is in the prefix), and only those with **no
//! open connections**, so it is safe on a shared server and while another suite is running.
//! The SQL (`pg_database`, `pg_stat_activity`, `DROP DATABASE`) lives in `kernel::db`.
//! Never call it from a test.

use tracing::info;

use crate::{kernel::db, state::AppState};

/// `test_offrails_app_`, from the crate name (`-` becomes `_`).
fn prefix() -> String {
    format!("test_{}_", env!("CARGO_PKG_NAME").replace('-', "_"))
}

pub async fn run(_state: &AppState, args: Vec<String>) -> anyhow::Result<()> {
    let url = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: app task test_db_cleanup <postgres admin url>"))?;
    if !url.starts_with("postgres") {
        anyhow::bail!("test_db_cleanup only applies to Postgres");
    }
    let prefix = prefix();
    let admin = db::connect(url, 2).await?;
    let dropped = db::drop_idle_test_databases(&admin, &prefix).await?;
    info!(dropped, prefix = %prefix, "dropped leftover test databases");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prefix;

    #[test]
    fn the_prefix_is_this_apps_crate_name() {
        assert_eq!(prefix(), "test_offrails_app_");
    }
}
