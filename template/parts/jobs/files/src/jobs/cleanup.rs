//! Hourly housekeeping (`recurring.rs`). Job rows themselves are the Pruner's job, not this one's.
//! GUIDE: .offrails/guides/jobs.md#recurring-jobs-srcjobsrecurringrs-the-oban-cron-plugin

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::{
    jobs::{JobInfo, Outcome, Worker},
    kernel::cache::Cache,
    state::AppState,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupJob;

impl Worker for CleanupJob {
    const NAME: &'static str = "cleanup";
    const MAX_ATTEMPTS: i32 = 3;
    // WHY: unique while pending: a backlog never stacks up several cleanups.
    fn unique() -> Option<crate::jobs::Unique> {
        Some(crate::jobs::Unique::worker())
    }

    async fn perform(self, _job: &JobInfo, state: &AppState) -> anyhow::Result<Outcome> {
        let cache = Cache::delete_expired(&state.db).await?;
        // APP: add the cleanups your tables need (expired sessions, tokens, the dev mailbox).
        info!(cache, "cleanup done");
        Ok(Outcome::Done)
    }
}
