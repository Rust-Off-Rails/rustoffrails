//! `app task auth_cleanup`: deletes expired sessions, used-up tokens, quiet throttles and audit
//! events older than a year. Idempotent; run it daily (cron, or the jobs sample's scheduler).
// APP: schedule it. Without it the tables only grow (expired rows are never *accepted*, though).

use chrono::Utc;

use crate::models::{auth_event, auth_throttle, session, user_token};
use crate::state::AppState;

pub async fn run(state: &AppState, _args: Vec<String>) -> anyhow::Result<()> {
    let now = Utc::now();
    let sessions = session::delete_expired(&state.db, now).await?;
    let tokens = user_token::delete_expired(&state.db, now).await?;
    let throttles = auth_throttle::delete_stale(&state.db, now).await?;
    let events = auth_event::delete_older_than_a_year(&state.db, now).await?;
    #[allow(unused_mut)] // samples add their own tables below
    let mut more: u64 = 0;
    // samples: auth cleanup
    tracing::info!(
        sessions,
        tokens,
        throttles,
        events,
        more,
        "auth cleanup done"
    );
    Ok(())
}
