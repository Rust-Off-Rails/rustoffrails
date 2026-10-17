//! `db/seeds.rb`: idempotent per record, in one transaction (.offrails/cli.md#seeds). Empty until
//! the app has models worth seeding.

use crate::kernel::db;
use crate::state::AppState;

pub async fn run(state: &AppState, _args: Vec<String>) -> anyhow::Result<()> {
    let txn = db::begin(&state.db).await?;
    txn.commit().await?;
    Ok(())
}
