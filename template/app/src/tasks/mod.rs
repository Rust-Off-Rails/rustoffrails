//! `lib/tasks`: `app task <name> [args…]`.

pub mod seed;
pub mod test_db_cleanup;
// samples: task mods

use crate::state::AppState;

pub async fn run(state: &AppState, name: &str, args: Vec<String>) -> anyhow::Result<()> {
    match name {
        "seed" => seed::run(state, args).await,
        "test_db_cleanup" => test_db_cleanup::run(state, args).await,
        // samples: tasks
        other => anyhow::bail!("unknown task: {other}"),
    }
}
