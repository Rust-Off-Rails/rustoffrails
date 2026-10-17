//! APP: example workers, one per feature of the queue: progress, every `Outcome`, retries with
//! a context chain, uniqueness, a dedicated rate-limited queue, and a batch callback. They make
//! no network calls (the "work" is simulated), and the demo task and the tests use them.
//! Keep this file as a reference while you write your own workers, then delete it together
//! with its arms in `jobs::dispatch` / `worker_config` / `WORKERS`, its `SCHEDULE` entries and
//! the `webhooks` queue in `APP_QUEUES`.
//! GUIDE: .offrails/guides/jobs.md#rules

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{
    jobs::{JobInfo, Outcome, Unique, Worker},
    state::AppState,
};

/// Builds a report in ten chunks, reporting progress after each (`job.progress(…)`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportReportJob {
    /// WHY: an id, never the record: args are stored and shown on the dashboard, and the
    /// record may change before the job runs (reload it in `perform`).
    pub report_id: i64,
    pub rows: u32,
}

impl Worker for ExportReportJob {
    const NAME: &'static str = "export_report";
    const MAX_ATTEMPTS: i32 = 5;
    const PRIORITY: i16 = 3;
    const TIMEOUT: Duration = Duration::from_secs(600);

    async fn perform(self, job: &JobInfo, _state: &AppState) -> anyhow::Result<Outcome> {
        if self.report_id <= 0 {
            // Retrying can't make a bad id valid.
            return Ok(Outcome::Discard(format!(
                "report {} doesn't exist",
                self.report_id
            )));
        }
        for chunk in 1..=10_u8 {
            // Simulated work: ~0.2 ms per row.
            tokio::time::sleep(Duration::from_micros(u64::from(self.rows) * 20)).await;
            job.progress(chunk * 10).await;
        }
        Ok(Outcome::Done)
    }
}

/// Delivers a webhook. Simulated: hosts under `.invalid` refuse the connection (an `Err`, so it
/// retries with backoff and lands in `discarded` after `MAX_ATTEMPTS`), hosts starting with
/// `busy.` answer "429 Too Many Requests" (a `Snooze`), and `http://` URLs are refused for good
/// (a `Discard`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliverWebhookJob {
    pub endpoint: String,
    pub event: String,
}

impl Worker for DeliverWebhookJob {
    const NAME: &'static str = "deliver_webhook";
    /// The rate-limited queue: `webhooks:3:30/1m` in `APP_QUEUES` (3 at a time per node, at most
    /// 30 starts a minute across all nodes).
    const QUEUE: &'static str = "webhooks";
    const MAX_ATTEMPTS: i32 = 8;
    const TIMEOUT: Duration = Duration::from_secs(30);

    /// The same event to the same endpoint at most once a minute (a debounce).
    fn unique() -> Option<Unique> {
        Some(Unique::args().period(chrono::Duration::minutes(1)))
    }

    async fn perform(self, _job: &JobInfo, _state: &AppState) -> anyhow::Result<Outcome> {
        let Some(host) = self.endpoint.strip_prefix("https://") else {
            return Ok(Outcome::Discard(format!(
                "endpoint {} is not an https:// URL",
                self.endpoint
            )));
        };
        let host = host.split('/').next().unwrap_or_default();
        if host.starts_with("busy.") {
            return Ok(Outcome::Snooze(chrono::Duration::seconds(30)));
        }
        if host.ends_with(".invalid") {
            // `.context` builds the chain the dashboard shows: "POST …: connecting: refused".
            return Err(anyhow::anyhow!("connection refused (os error 61)"))
                .context(format!("connecting to {host}:443"))
                .context(format!("POST {} ({})", self.endpoint, self.event));
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        Ok(Outcome::Done)
    }
}

/// A batch's callback: runs once after every job of the batch is final, with the counts in
/// `meta.batch` (see `kernel::jobs::Batch`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportBatchDoneJob {
    pub report_id: i64,
}

impl Worker for ReportBatchDoneJob {
    const NAME: &'static str = "report_batch_done";

    async fn perform(self, job: &JobInfo, _state: &AppState) -> anyhow::Result<Outcome> {
        let batch = job.meta.get("batch").cloned().unwrap_or_default();
        tracing::info!(report_id = self.report_id, %batch, "report batch finished");
        Ok(Outcome::Done)
    }
}
