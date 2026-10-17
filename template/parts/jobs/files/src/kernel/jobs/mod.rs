//! A database-backed job queue in the spirit of Oban: jobs are rows with an explicit state
//! machine, finished jobs are kept (and pruned later), queues have limits and can be paused or
//! rate-limited, and every node runs the same idempotent plugins.
//! GUIDE: .offrails/guides/jobs.md
//!
//! ```text
//! mod.rs        Outcome, JobInfo (+ progress), WorkerConfig, backoff, QueueSpec, node ids, redaction
//! entities.rs   jobs, job_queues, job_nodes, recurring_executions, job_batches
//! builder.rs    JobBuilder, Unique, insert, insert_all, Batch
//! executor.rs   claim (SKIP LOCKED / single writer), execute, record, drain (tests), run (the worker)
//! plugins.rs    Heartbeat, Stager, Lifeline, Pruner, Cron, batch completion
//! admin.rs      queries + actions shared by `app jobs` and the dashboard sample
//! ```
//!
//! The kernel never names an app type: it gets the app's dispatcher as a `DispatchFn` and
//! per-worker settings through a `WorkerConfigFn` (both built in `src/jobs/mod.rs`).

pub mod admin;
mod builder;
mod entities;
pub mod executor;
pub mod plugins;

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, sea_query::Expr};
use serde_json::Value;
use tokio::sync::watch;

pub use builder::{Batch, BatchInserted, Inserted, JobBuilder, JobDefaults, Unique, UniqueStates};
pub use builder::{insert_all, unique_key};
pub use entities::{job, job_batch, job_node, job_queue, recurring_execution};
pub use executor::{Drained, WorkerOptions, drain, run};
pub use job::JobState;
pub use plugins::{
    Recurring, cron_tick, describe_cron, next_run, previous_run, prune, rescue_orphans, stage,
};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// What a worker returns from `perform`. `Err(_)` means "retry with backoff".
/// GUIDE: .offrails/guides/jobs.md#the-worker-trait-and-dispatcher-srcjobsmodrs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Not now: back to `scheduled`; doesn't consume an attempt.
    Snooze(chrono::Duration),
    /// The work no longer matters (the record was deleted) → cancelled.
    Cancel(String),
    /// Permanently failed; retrying cannot help → discarded.
    Discard(String),
}

/// What a running job knows about itself.
#[derive(Clone)]
pub struct JobInfo {
    pub id: i64,
    pub worker: String,
    pub queue: String,
    pub attempt: i32,
    pub max_attempts: i32,
    pub inserted_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub meta: Value,
    pub batch_id: Option<i64>,
    progress: Option<(DatabaseConnection, String)>,
}

impl std::fmt::Debug for JobInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobInfo")
            .field("id", &self.id)
            .field("worker", &self.worker)
            .field("attempt", &self.attempt)
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

impl JobInfo {
    /// A `JobInfo` for a job that isn't in the database (unit tests of one worker:
    /// `TestApp::perform_job`). `progress` does nothing.
    pub fn synthetic(worker: &str, queue: &str) -> Self {
        Self {
            id: 0,
            worker: worker.to_owned(),
            queue: queue.to_owned(),
            attempt: 1,
            max_attempts: 1,
            inserted_at: Utc::now(),
            tags: Vec::new(),
            meta: Value::Object(Default::default()),
            batch_id: None,
            progress: None,
        }
    }

    pub(crate) fn from_model(model: &job::Model, db: &DatabaseConnection, node: &str) -> Self {
        Self {
            id: model.id,
            worker: model.worker.clone(),
            queue: model.queue.clone(),
            attempt: model.attempt,
            max_attempts: model.max_attempts,
            inserted_at: model.inserted_at,
            tags: model.tag_list(),
            meta: model.meta.clone(),
            batch_id: model.batch_id,
            progress: Some((db.clone(), node.to_owned())),
        }
    }

    /// Reports how far along the job is (0-100, clamped); the dashboard shows it live.
    /// Cheap enough to call per batch of work, not per item. Errors are logged, never returned:
    /// progress is a display, and a failed write must not fail the job.
    pub async fn progress(&self, percent: u8) {
        let Some((db, node)) = &self.progress else {
            return;
        };
        // WHY: filtered by this node and `executing`, so a cancelled or rescued job isn't touched.
        let res = job::Entity::update_many()
            .col_expr(
                job::Column::Progress,
                Expr::value(i16::from(percent.min(100))),
            )
            .filter(job::Column::Id.eq(self.id))
            .filter(job::Column::State.eq(JobState::Executing))
            .filter(job::Column::AttemptedBy.eq(node.as_str()))
            .exec(db)
            .await;
        if let Err(e) = res {
            tracing::warn!(job_id = self.id, error = %e, "could not record job progress");
        }
    }
}

/// Per-worker runtime settings. Scheduling arithmetic is `chrono`, because it is added to
/// `DateTimeUtc` values stored in the database; the timeout is `std`, because tokio consumes it.
#[derive(Clone, Copy)]
pub struct WorkerConfig {
    pub timeout: Duration,
    pub backoff: fn(attempt: i32) -> chrono::Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            backoff: default_backoff,
        }
    }
}

/// Runs a job by worker name. Built by the app so it can capture `AppState`.
pub type DispatchFn =
    Arc<dyn Fn(String, Value, JobInfo) -> BoxFuture<anyhow::Result<Outcome>> + Send + Sync>;
pub type WorkerConfigFn = fn(&str) -> WorkerConfig;

/// Exponential backoff with jitter: `15s × 2^(attempt-1)`, capped at 24h, plus up to 25% random
/// jitter so a burst of failures doesn't retry in lockstep. ~15s, ~30s, ~1m, ~2m … 24h; twenty
/// attempts cover about nine days.
/// GUIDE: .offrails/guides/jobs.md#worker-runtime-executorrs
pub fn default_backoff(attempt: i32) -> chrono::Duration {
    exponential_backoff(
        attempt,
        chrono::Duration::seconds(15),
        chrono::Duration::hours(24),
    )
}

/// `base × 2^(attempt-1)`, capped at `cap`, plus `0..=25%` jitter.
pub fn exponential_backoff(
    attempt: i32,
    base: chrono::Duration,
    cap: chrono::Duration,
) -> chrono::Duration {
    let exp = attempt.clamp(1, 31) as u32 - 1;
    let secs = base
        .num_seconds()
        .max(1)
        .saturating_mul(1_i64 << exp.min(40))
        .min(cap.num_seconds().max(1));
    let jitter = rand::random_range(0..=secs / 4);
    chrono::Duration::seconds(secs + jitter)
}

// ---------------------------------------------------------------------------
// Queues
// ---------------------------------------------------------------------------

/// At most `allowed` job starts per `period`, counted across every node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub allowed: u32,
    pub period: chrono::Duration,
}

impl Rate {
    /// `"60/1m"`, `"10/30s"`, `"1000/1h"`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (n, period) = raw
            .split_once('/')
            .ok_or_else(|| format!("rate {raw:?}: expected <count>/<period>, like 60/1m"))?;
        let allowed: u32 = n
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("rate {raw:?}: the count must be a positive number"))?;
        let (amount, unit) = period.split_at(period.len().saturating_sub(1));
        let amount: i64 = amount
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("rate {raw:?}: the period must be like 30s, 1m or 1h"))?;
        let period = match unit {
            "s" => chrono::Duration::seconds(amount),
            "m" => chrono::Duration::minutes(amount),
            "h" => chrono::Duration::hours(amount),
            _ => return Err(format!("rate {raw:?}: the period unit is s, m or h")),
        };
        Ok(Self { allowed, period })
    }

    /// `"60/1m"`.
    pub fn label(&self) -> String {
        let s = self.period.num_seconds();
        let period = if s % 3600 == 0 {
            format!("{}h", s / 3600)
        } else if s % 60 == 0 {
            format!("{}m", s / 60)
        } else {
            format!("{s}s")
        };
        format!("{}/{period}", self.allowed)
    }
}

/// One queue this node runs: its concurrency per node and an optional global rate limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSpec {
    pub name: String,
    pub limit: usize,
    pub rate: Option<Rate>,
}

impl QueueSpec {
    pub fn new(name: &str, limit: usize) -> Self {
        Self {
            name: name.to_owned(),
            limit,
            rate: None,
        }
    }

    pub fn rate(mut self, rate: Rate) -> Self {
        self.rate = Some(rate);
        self
    }

    /// `APP_QUEUES`: `default:10,mailers:5,webhooks:2:60/1m` (name:limit[:rate]).
    pub fn parse_list(raw: &str) -> Result<Vec<Self>, String> {
        let mut out: Vec<Self> = Vec::new();
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let mut fields = part.split(':');
            let name = fields.next().unwrap_or_default();
            if !valid_queue_name(name) {
                return Err(format!(
                    "queue {name:?}: names are 1-64 characters of a-z, 0-9, _ and -"
                ));
            }
            let limit = fields
                .next()
                .ok_or_else(|| format!("queue {name}: expected {name}:<limit>"))?
                .parse::<usize>()
                .ok()
                .filter(|l| (1..=1000).contains(l))
                .ok_or_else(|| format!("queue {name}: the limit must be 1-1000"))?;
            let rate = fields.next().map(Rate::parse).transpose()?;
            if fields.next().is_some() {
                return Err(format!("queue {part:?}: expected name:limit[:rate]"));
            }
            if out.iter().any(|q| q.name == name) {
                return Err(format!("queue {name} is listed twice"));
            }
            out.push(Self {
                name: name.to_owned(),
                limit,
                rate,
            });
        }
        if out.is_empty() {
            return Err("no queues".into());
        }
        Ok(out)
    }
}

/// Queue names reach URLs and the CLI: keep them boring.
pub fn valid_queue_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A synthetic node name, `node-<8 hex>`: unique per process start, so a restart never inherits
/// a stale row.
/// SECURITY: never the hostname. Node names are shown on the dashboard and in logs, and a
/// hostname leaks infrastructure details (test: node_ids_are_synthetic_and_unique).
pub fn node_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("node-{}", &id[..8])
}

// ---------------------------------------------------------------------------
// Wake-ups
// ---------------------------------------------------------------------------

/// Bumped whenever this process makes a job runnable (insert, retry, stage), so local producers
/// fetch at once instead of on their next poll.
/// WHY: a `watch` channel, not `Notify::notify_waiters`: a producer that is busy when the
/// bump happens still sees it on its next wait, so no wake-up is lost.
static WAKE: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

pub(crate) fn wake() {
    WAKE.send_modify(|n| *n = n.wrapping_add(1));
}

pub(crate) fn wake_receiver() -> watch::Receiver<u64> {
    WAKE.subscribe()
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// APP: argument keys whose values `app jobs` and the dashboard never show. A key matches when it
/// equals an entry, or one of its `_`/`-`/camelCase words does (`api_key`, `resetToken`).
/// Args should never hold secrets at all (GUIDE: .offrails/guides/jobs.md#rules, rule 7); this is
/// the second line of defence.
pub const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "key",
    "apikey",
    "authorization",
    "auth",
    "cookie",
    "credential",
    "credentials",
    "otp",
    "pin",
    "ssn",
];

pub const REDACTED: &str = "[redacted]";

/// Whether an args key holds something an operator must not see.
pub fn is_sensitive_key(key: &str) -> bool {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' {
            words.push(std::mem::take(&mut word));
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut word));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    words.push(word);
    let whole = key.to_lowercase().replace(['_', '-', '.'], "");
    SENSITIVE_KEYS.contains(&whole.as_str())
        || words
            .iter()
            .any(|w| !w.is_empty() && SENSITIVE_KEYS.contains(&w.as_str()))
}

/// A copy of `value` with every sensitive key's value replaced by `"[redacted]"`, at any depth.
/// INVARIANT: the dashboard and `app jobs` only ever print redacted args and meta
/// (test: sensitive_args_are_redacted_in_the_cli_and_admin_views)
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if is_sensitive_key(k) {
                        Value::String(REDACTED.into())
                    } else {
                        redact(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn backoff_grows_exponentially_with_bounded_jitter() {
        for (attempt, base) in [(1, 15), (2, 30), (3, 60), (6, 480)] {
            for _ in 0..50 {
                let d = default_backoff(attempt).num_seconds();
                assert!(
                    (base..=base + base / 4).contains(&d),
                    "attempt {attempt}: {d}s"
                );
            }
        }
        // Capped at 24h (+ jitter), even for absurd attempts.
        let d = default_backoff(500).num_seconds();
        assert!((86_400..=86_400 + 21_600).contains(&d), "{d}");
        assert!(default_backoff(0).num_seconds() >= 15);
    }

    #[test]
    fn queue_specs_parse_limits_and_rates() {
        let specs = QueueSpec::parse_list("default:10, mailers:5,webhooks:2:60/1m").unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[1], QueueSpec::new("mailers", 5));
        assert_eq!(
            specs[2].rate,
            Some(Rate {
                allowed: 60,
                period: chrono::Duration::minutes(1)
            })
        );
        assert_eq!(specs[2].rate.unwrap().label(), "60/1m");
        assert_eq!(Rate::parse("10/30s").unwrap().label(), "10/30s");
        for bad in [
            "",
            "default",
            "default:0",
            "default:x",
            "Default:1",
            "a b:1",
            "q:1:60",
            "q:1:0/1m",
            "q:1:5/1d",
            "q:1:5/1m:x",
            "q:1,q:2",
        ] {
            assert!(QueueSpec::parse_list(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn node_ids_are_synthetic_and_unique() {
        let (a, b) = (node_id(), node_id());
        assert_ne!(a, b);
        assert!(a.starts_with("node-") && a.len() == 13, "{a}");
    }

    #[test]
    fn sensitive_keys_are_recognised() {
        for key in [
            "password",
            "Password",
            "api_key",
            "apiKey",
            "resetToken",
            "client-secret",
            "AUTH",
            "card_pin",
        ] {
            assert!(is_sensitive_key(key), "{key}");
        }
        for key in ["user_id", "keyboard", "monkey", "tokens_used", "email"] {
            assert!(!is_sensitive_key(key), "{key}");
        }
    }

    #[test]
    fn redaction_reaches_nested_values() {
        let args = json!({"user_id": 7, "password": "not-a-real-password",
            "nested": {"api_key": "k", "list": [{"token": "t", "ok": 1}]}});
        let shown = redact(&args);
        assert_eq!(shown["user_id"], 7);
        assert_eq!(shown["password"], REDACTED);
        assert_eq!(shown["nested"]["api_key"], REDACTED);
        assert_eq!(shown["nested"]["list"][0]["token"], REDACTED);
        assert_eq!(shown["nested"]["list"][0]["ok"], 1);
        assert!(!shown.to_string().contains("not-a-real-password"));
    }
}
