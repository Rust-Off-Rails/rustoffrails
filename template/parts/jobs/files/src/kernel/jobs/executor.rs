//! The worker runtime: claim, execute, record; producers (one per queue) plus the plugins; and
//! the deterministic `drain` used by tests.
//! GUIDE: .offrails/guides/jobs.md#worker-runtime-executorrs

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use futures::FutureExt;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, TransactionSession, TransactionTrait,
    sea_query::{Expr, ExprTrait, LockBehavior, LockType},
};
use serde_json::{Value, json};
use tokio::{
    sync::{Notify, Semaphore, watch},
    task::AbortHandle,
};
use tracing::{Instrument, error, info, info_span, warn};

use crate::kernel::redact;

use super::{
    DispatchFn, JobInfo, JobState, Outcome, QueueSpec, WorkerConfigFn, job, job_queue,
    plugins::{self, Recurring},
    wake, wake_receiver,
};
use crate::kernel::db;

/// Each error entry keeps at most this many bytes of message and of detail (start kept).
const ERROR_CAP_BYTES: usize = 4096;
/// Errors kept per job (the newest).
const ERRORS_KEPT: usize = 20;

/// Everything `run` needs. Built from config by `crate::jobs::worker_options`.
pub struct WorkerOptions {
    /// `APP_QUEUES` / `app worker --queues`.
    pub queues: Vec<QueueSpec>,
    /// `APP_JOBS_NODE`, else `kernel::jobs::node_id()`.
    pub node: String,
    /// `APP_JOBS_PRUNE_DAYS`.
    pub prune_after: chrono::Duration,
    /// `APP_JOBS_SHUTDOWN_GRACE`.
    pub shutdown_grace: Duration,
    pub schedule: &'static [Recurring],
}

// ---------------------------------------------------------------------------
// Claim
// ---------------------------------------------------------------------------

/// Claims up to `limit` due jobs of `queue` (any queue when `None`) for `node`, in priority
/// order, and returns them as `executing` with `attempt` already incremented.
///
/// Two statements in one transaction, on both backends:
/// 1. `SELECT id FROM jobs WHERE state = 'available' AND queue = ? AND scheduled_at <= ?
///     ORDER BY priority, scheduled_at, id LIMIT n FOR UPDATE SKIP LOCKED`
/// 2. `UPDATE jobs SET state = 'executing', … WHERE id IN (…) AND state = 'available' RETURNING *`
///
/// - Postgres: `FOR UPDATE SKIP LOCKED` makes concurrent claimers take disjoint rows without
///   waiting on each other; the locks last until the transaction commits.
/// - SQLite: no row locks (sea-query omits the clause), but `kernel::db::begin` is
///   `BEGIN IMMEDIATE`: the claim holds SQLite's single write lock from the SELECT on, so
///   claimers are serialized.
///
/// WHY: not one `UPDATE … WHERE id IN (SELECT … LIMIT n FOR UPDATE SKIP LOCKED)`: Postgres may
/// re-run that subquery while updating, and with SKIP LOCKED each run returns new rows, so one
/// claim took 12 jobs with `LIMIT 3` (test: rate_limited_queues_start_at_most_n_per_period).
///
/// INVARIANT: a job is claimed by one worker only (test: concurrent_claims_run_each_job_exactly_once)
/// GUIDE: .offrails/guides/jobs.md#postgres-only-optimizations-later-behind-a-backend-match-inside-the-kernel
pub async fn claim<C>(
    db: &C,
    queue: Option<&str>,
    limit: u64,
    node: &str,
    now: DateTime<Utc>,
) -> Result<Vec<job::Model>, DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db::begin(db).await?;
    let mut due = job::Entity::find()
        .select_only()
        .column(job::Column::Id)
        .filter(job::Column::State.eq(JobState::Available))
        .filter(job::Column::ScheduledAt.lte(now));
    if let Some(queue) = queue {
        due = due.filter(job::Column::Queue.eq(queue));
    }
    let ids: Vec<i64> = due
        .order_by_asc(job::Column::Priority)
        .order_by_asc(job::Column::ScheduledAt)
        .order_by_asc(job::Column::Id)
        .limit(limit)
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
        .into_tuple()
        .all(&txn)
        .await?;
    if ids.is_empty() {
        txn.commit().await?;
        return Ok(Vec::new());
    }
    let mut claimed = job::Entity::update_many()
        .col_expr(
            job::Column::State,
            Expr::value(JobState::Executing.to_value()),
        )
        .col_expr(job::Column::Attempt, Expr::col(job::Column::Attempt).add(1))
        .col_expr(job::Column::AttemptedAt, Expr::value(now))
        .col_expr(job::Column::AttemptedBy, Expr::value(node.to_owned()))
        .col_expr(job::Column::Progress, Expr::value(Option::<i16>::None))
        .filter(job::Column::Id.is_in(ids))
        // WHY: re-checked, so a row an operator changed in between is never claimed.
        .filter(job::Column::State.eq(JobState::Available))
        .exec_with_returning(&txn)
        .await?;
    txn.commit().await?;
    claimed.sort_by_key(|j| (j.priority, j.scheduled_at, j.id));
    Ok(claimed)
}

/// `claim` for a rate-limited queue: counts the queue's starts in the window and claims at most
/// what is left, inside one transaction that holds the queue's row lock (Postgres) or SQLite's
/// write lock, so nodes count and claim one at a time.
/// INVARIANT: a rate-limited queue never starts more than `allowed` jobs per period across all
/// nodes (test: rate_limited_queues_start_at_most_n_per_period)
pub async fn claim_rate_limited(
    db: &DatabaseConnection,
    spec: &QueueSpec,
    free: u64,
    node: &str,
    now: DateTime<Utc>,
) -> Result<Vec<job::Model>, DbErr> {
    let Some(rate) = spec.rate else {
        return claim(db, Some(&spec.name), free, node, now).await;
    };
    let txn = db::begin(db).await?;
    job_queue::Entity::insert(job_queue::ActiveModel {
        name: sea_orm::ActiveValue::Set(spec.name.clone()),
        paused: sea_orm::ActiveValue::Set(false),
        updated_at: sea_orm::ActiveValue::Set(now),
    })
    .on_conflict_do_nothing_on([job_queue::Column::Name])
    .exec_without_returning(&txn)
    .await?;
    // WHY: the row lock serializes counters across nodes on Postgres; without it two nodes
    // could each see room for N and start 2N.
    job_queue::Entity::find_by_id(spec.name.clone())
        .lock_exclusive()
        .one(&txn)
        .await?;
    let used = job::Entity::find()
        .filter(job::Column::Queue.eq(spec.name.as_str()))
        .filter(job::Column::AttemptedAt.gt(now - rate.period))
        .count(&txn)
        .await?;
    let left = u64::from(rate.allowed).saturating_sub(used);
    let claimed = claim(&txn, Some(&spec.name), std::cmp::min(free, left), node, now).await?;
    txn.commit().await?;
    Ok(claimed)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub(crate) fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_owned();
    }
    let mut end = max;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &input[..end])
}

/// What went wrong in one attempt.
#[derive(Debug, Clone, Default)]
pub struct AttemptError {
    /// One line per cause: `{e:#}` ("sending: connection refused").
    pub message: String,
    /// The full report: `{e:?}` with the cause list, and the backtrace when
    /// `RUST_LIB_BACKTRACE=1` captured one. Shown on the dashboard's job page only.
    pub detail: Option<String>,
    pub duration_ms: Option<u64>,
}

impl AttemptError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }
}

/// Appends `{attempt, at, error, detail, node, duration_ms}`, capped at 4 KB per text and the
/// last 20 entries.
pub fn push_error(
    existing: &Value,
    attempt: i32,
    now: DateTime<Utc>,
    node: Option<&str>,
    error: &AttemptError,
) -> Value {
    let mut errors = existing.as_array().cloned().unwrap_or_default();
    // SECURITY: an error often quotes the URL that failed, credentials and tokens included; the
    // stored text (shown on the dashboard, searched, printed by `app jobs`) is redacted first
    // (test: job_errors_are_stored_redacted)
    let message = redact::text(&error.message);
    let mut entry = json!({
        "attempt": attempt,
        "at": now.to_rfc3339(),
        "error": truncate(&message, ERROR_CAP_BYTES),
    });
    if let Some(detail) = error.detail.as_deref().filter(|d| *d != error.message) {
        entry["detail"] = json!(truncate(&redact::text(detail), ERROR_CAP_BYTES));
    }
    if let Some(node) = node {
        entry["node"] = json!(node);
    }
    if let Some(ms) = error.duration_ms {
        entry["duration_ms"] = json!(ms);
    }
    errors.push(entry);
    if errors.len() > ERRORS_KEPT {
        let drop = errors.len() - ERRORS_KEPT;
        errors.drain(..drop);
    }
    Value::Array(errors)
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        format!("panicked: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panicked: {s}")
    } else {
        "panicked".to_owned()
    }
}

/// `true` when the job's unique key is freed at a final state (`UniqueStates::Incomplete`).
pub(crate) fn releases_unique(model: &job::Model) -> bool {
    model.unique_key.is_some()
        && model.meta.get("unique_states").and_then(Value::as_str) != Some("all")
}

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

/// Writes an attempt's result and returns the new state, or `None` when the job was no longer
/// this node's (cancelled, rescued or deleted meanwhile).
/// INVARIANT: a result never overwrites a job that was cancelled or rescued while it ran
/// (test: a_result_never_resurrects_a_cancelled_job)
pub async fn record(
    db: &DatabaseConnection,
    model: &job::Model,
    node: &str,
    outcome: Result<Outcome, AttemptError>,
    backoff: fn(i32) -> chrono::Duration,
) -> Result<Option<JobState>, DbErr> {
    let now = Utc::now();
    let mut update = job::Entity::update_many()
        .filter(job::Column::Id.eq(model.id))
        .filter(job::Column::State.eq(JobState::Executing))
        .filter(job::Column::AttemptedBy.eq(node));
    let set_state = |s: JobState| Expr::value(s.to_value());

    let state = match outcome {
        Ok(Outcome::Done) => {
            update = update
                .col_expr(job::Column::State, set_state(JobState::Completed))
                .col_expr(job::Column::CompletedAt, Expr::value(now))
                .col_expr(job::Column::Progress, Expr::value(Some(100_i16)));
            JobState::Completed
        }
        Ok(Outcome::Snooze(delay)) => {
            let mut meta = model.meta.clone();
            let snoozed = meta.get("snoozed").and_then(Value::as_i64).unwrap_or(0) + 1;
            if let Value::Object(map) = &mut meta {
                map.insert("snoozed".into(), json!(snoozed));
            }
            // WHY: a snooze isn't a failure, so it gives the attempt back.
            update = update
                .col_expr(job::Column::State, set_state(JobState::Scheduled))
                .col_expr(job::Column::ScheduledAt, Expr::value(now + delay))
                .col_expr(
                    job::Column::MaxAttempts,
                    Expr::value(model.max_attempts + 1),
                )
                .col_expr(job::Column::Meta, Expr::value(meta));
            JobState::Scheduled
        }
        Ok(Outcome::Cancel(reason)) => {
            let errors = push_error(
                &model.errors,
                model.attempt,
                now,
                Some(node),
                &AttemptError::new(format!("cancelled: {reason}")),
            );
            update = update
                .col_expr(job::Column::State, set_state(JobState::Cancelled))
                .col_expr(job::Column::CancelledAt, Expr::value(now))
                .col_expr(job::Column::Errors, Expr::value(errors));
            JobState::Cancelled
        }
        Ok(Outcome::Discard(reason)) => {
            let errors = push_error(
                &model.errors,
                model.attempt,
                now,
                Some(node),
                &AttemptError::new(reason),
            );
            update = update
                .col_expr(job::Column::State, set_state(JobState::Discarded))
                .col_expr(job::Column::DiscardedAt, Expr::value(now))
                .col_expr(job::Column::Errors, Expr::value(errors));
            JobState::Discarded
        }
        Err(error) => {
            let errors = push_error(&model.errors, model.attempt, now, Some(node), &error);
            update = update.col_expr(job::Column::Errors, Expr::value(errors));
            if model.attempt < model.max_attempts {
                update = update
                    .col_expr(job::Column::State, set_state(JobState::Retryable))
                    .col_expr(
                        job::Column::ScheduledAt,
                        Expr::value(now + backoff(model.attempt)),
                    );
                JobState::Retryable
            } else {
                update = update
                    .col_expr(job::Column::State, set_state(JobState::Discarded))
                    .col_expr(job::Column::DiscardedAt, Expr::value(now));
                JobState::Discarded
            }
        }
    };

    if state.is_final() && releases_unique(model) {
        update = update.col_expr(job::Column::UniqueKey, Expr::value(Option::<String>::None));
    }
    if update.exec(db).await?.rows_affected == 0 {
        return Ok(None);
    }
    if state.is_final()
        && let Some(batch) = model.batch_id
    {
        plugins::finish_batch_if_done(db, batch).await?;
    }
    Ok(Some(state))
}

/// Runs one claimed job: dispatch under the worker's timeout, panics caught, then `record`.
pub async fn execute(
    db: &DatabaseConnection,
    dispatch: &DispatchFn,
    config: WorkerConfigFn,
    node: &str,
    model: job::Model,
) -> Result<Option<JobState>, DbErr> {
    let cfg = config(&model.worker);
    let info = JobInfo::from_model(&model, db, node);
    let queue_ms = model
        .attempted_at
        .map(|a| std::cmp::max((a - model.scheduled_at).num_milliseconds(), 0))
        .unwrap_or(0);
    let span = info_span!("job", job_id = model.id, worker = %model.worker, queue = %model.queue, attempt = model.attempt);

    async {
        info!(queue_ms, "job.start");
        let started = Instant::now();
        let fut = dispatch(model.worker.clone(), model.args.clone(), info);
        // WHY: catch_unwind inside the timeout: a panicking worker becomes a retryable error
        // instead of killing the producer, and a hanging one is cut off at `TIMEOUT`.
        let result = tokio::time::timeout(cfg.timeout, AssertUnwindSafe(fut).catch_unwind()).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        let outcome = match result {
            Err(_) => Err(AttemptError::new(format!(
                "timed out after {:?}",
                cfg.timeout
            ))),
            Ok(Err(panic)) => Err(AttemptError::new(panic_message(panic))),
            Ok(Ok(Err(e))) => Err(AttemptError {
                message: format!("{e:#}"),
                detail: Some(format!("{e:?}")),
                duration_ms: None,
            }),
            Ok(Ok(Ok(outcome))) => Ok(outcome),
        }
        .map_err(|e| AttemptError {
            // The log line below carries the message too: redacted like the stored copy.
            message: redact::text(&e.message),
            duration_ms: Some(duration_ms),
            ..e
        });
        match &outcome {
            Ok(o) => info!(duration_ms, outcome = ?o, "job.stop"),
            Err(e) if model.attempt < model.max_attempts => {
                warn!(duration_ms, error = %e.message, "job.exception (will retry)")
            }
            Err(e) => error!(duration_ms, error = %e.message, "job.exception (discarded)"),
        }
        record(db, &model, node, outcome, cfg.backoff).await
    }
    .instrument(span)
    .await
}

// ---------------------------------------------------------------------------
// Draining (tests, one-off tasks)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Drained {
    pub completed: usize,
    pub snoozed: usize,
    pub cancelled: usize,
    pub discarded: usize,
    pub retryable: usize,
}

impl Drained {
    pub fn total(&self) -> usize {
        self.completed + self.snoozed + self.cancelled + self.discarded + self.retryable
    }
}

/// Runs every due job through the real claim → dispatch → record path, repeatedly, until none
/// are due. `include_scheduled` also runs `scheduled` jobs as if their time had come (a job that
/// snoozes runs once per drain, so it always ends; `retryable` jobs are never forced).
/// GUIDE: .offrails/guides/jobs.md#testing-obantesting-equivalents
pub async fn drain(
    db: &DatabaseConnection,
    dispatch: &DispatchFn,
    config: WorkerConfigFn,
    queue: Option<&str>,
    include_scheduled: bool,
) -> Result<Drained, DbErr> {
    drain_as(db, dispatch, config, queue, include_scheduled, "drain").await
}

/// `drain` under a node name of your choosing (concurrency tests run several at once).
pub async fn drain_as(
    db: &DatabaseConnection,
    dispatch: &DispatchFn,
    config: WorkerConfigFn,
    queue: Option<&str>,
    include_scheduled: bool,
    node: &str,
) -> Result<Drained, DbErr> {
    let mut drained = Drained::default();
    let mut forced_once: HashSet<i64> = HashSet::new();
    for _ in 0..1000 {
        let now = Utc::now();
        plugins::stage_queue(db, now, queue).await?;
        if include_scheduled {
            // Force scheduled jobs forward, each only once per drain.
            let mut pending = job::Entity::find()
                .select_only()
                .column(job::Column::Id)
                .filter(job::Column::State.eq(JobState::Scheduled));
            if let Some(queue) = queue {
                pending = pending.filter(job::Column::Queue.eq(queue));
            }
            let ids: Vec<i64> = pending
                .into_tuple::<i64>()
                .all(db)
                .await?
                .into_iter()
                .filter(|id| forced_once.insert(*id))
                .collect();
            for chunk in ids.chunks(500) {
                job::Entity::update_many()
                    .col_expr(
                        job::Column::State,
                        Expr::value(JobState::Available.to_value()),
                    )
                    .col_expr(job::Column::ScheduledAt, Expr::value(now))
                    .filter(job::Column::Id.is_in(chunk.to_vec()))
                    .filter(job::Column::State.eq(JobState::Scheduled))
                    .exec(db)
                    .await?;
            }
        }

        let claimed = claim(db, queue, 100, node, now).await?;
        if claimed.is_empty() {
            break;
        }
        for model in claimed {
            match execute(db, dispatch, config, node, model).await? {
                Some(JobState::Completed) => drained.completed += 1,
                Some(JobState::Cancelled) => drained.cancelled += 1,
                Some(JobState::Discarded) => drained.discarded += 1,
                Some(JobState::Retryable) => drained.retryable += 1,
                Some(JobState::Scheduled) => drained.snoozed += 1,
                _ => {}
            }
        }
    }
    Ok(drained)
}

// ---------------------------------------------------------------------------
// The long-running worker
// ---------------------------------------------------------------------------

pub(crate) struct Shared {
    pub db: DatabaseConnection,
    pub dispatch: DispatchFn,
    pub config: WorkerConfigFn,
    pub node: String,
    pub paused: Mutex<HashSet<String>>,
    pub in_flight: Mutex<HashMap<i64, AbortHandle>>,
}

/// Runs producers and plugins until `shutdown` resolves. Then: stop fetching, wait up to
/// `shutdown_grace` for in-flight jobs, abort the rest and hand them straight back to the queue
/// (no 60s wait for the Lifeline), and deregister the node.
/// GUIDE: .offrails/guides/jobs.md#shutdown
/// INVARIANT: graceful shutdown never loses a job: unfinished ones go back to `available`
/// (test: shutdown_returns_unfinished_jobs_to_the_queue)
pub async fn run(
    db: DatabaseConnection,
    opts: WorkerOptions,
    dispatch: DispatchFn,
    config: WorkerConfigFn,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = stop_tx.send(true);
    });

    let shared = Arc::new(Shared {
        db: db.clone(),
        dispatch,
        config,
        node: opts.node.clone(),
        paused: Mutex::new(HashSet::new()),
        in_flight: Mutex::new(HashMap::new()),
    });
    let started_at = Utc::now();
    let queue_names: Vec<String> = opts.queues.iter().map(|q| q.name.clone()).collect();
    info!(node = %opts.node, queues = ?queue_names, "worker started");

    // One heartbeat first: registers the node and reads the paused queues before fetching.
    if let Err(e) = plugins::heartbeat(&shared, &opts.queues, started_at).await {
        warn!(error = %e, "initial heartbeat failed");
    }

    let mut plugin_tasks = Vec::new();
    {
        let (shared, queues) = (shared.clone(), opts.queues.clone());
        plugin_tasks.push(tokio::spawn(every(
            Duration::from_secs(1),
            stop_rx.clone(),
            move || {
                let (shared, queues) = (shared.clone(), queues.clone());
                async move {
                    if let Err(e) = plugins::heartbeat(&shared, &queues, started_at).await {
                        warn!(plugin = "heartbeat", error = %e, "heartbeat failed");
                    }
                }
            },
        )));
    }
    {
        // Stager + Cron every second: cron runs fire within a second of their due time.
        let db = db.clone();
        let schedule = opts.schedule;
        let fired: Arc<Mutex<HashMap<&'static str, DateTime<Utc>>>> = Arc::default();
        plugin_tasks.push(tokio::spawn(every(
            Duration::from_secs(1),
            stop_rx.clone(),
            move || {
                let (db, fired) = (db.clone(), fired.clone());
                async move {
                    match plugins::stage(&db, Utc::now()).await {
                        Ok(n) if n > 0 => {
                            tracing::debug!(plugin = "stager", count = n, "staged jobs");
                            wake();
                        }
                        Ok(_) => {}
                        Err(e) => error!(plugin = "stager", error = %e, "stager failed"),
                    }
                    match plugins::cron_tick_skipping(&db, schedule, Utc::now(), &fired).await {
                        Ok(ids) if !ids.is_empty() => {
                            tracing::debug!(plugin = "cron", count = ids.len(), "enqueued");
                        }
                        Ok(_) => {}
                        Err(e) => error!(plugin = "cron", error = %e, "cron failed"),
                    }
                }
            },
        )));
    }
    {
        let db = db.clone();
        plugin_tasks.push(tokio::spawn(every(
            Duration::from_secs(30),
            stop_rx.clone(),
            move || {
                let db = db.clone();
                async move {
                    match plugins::rescue_orphans(&db, Utc::now()).await {
                        Ok(n) if n > 0 => {
                            warn!(plugin = "lifeline", rescued = n, "rescued orphaned jobs");
                            wake();
                        }
                        Ok(_) => {}
                        Err(e) => error!(plugin = "lifeline", error = %e, "lifeline failed"),
                    }
                }
            },
        )));
    }
    {
        let db = db.clone();
        let older_than = opts.prune_after;
        plugin_tasks.push(tokio::spawn(every(
            Duration::from_secs(60),
            stop_rx.clone(),
            move || {
                let db = db.clone();
                async move {
                    match plugins::prune(&db, Utc::now(), older_than).await {
                        Ok(n) if n > 0 => {
                            tracing::debug!(plugin = "pruner", count = n, "pruned jobs")
                        }
                        Ok(_) => {}
                        Err(e) => error!(plugin = "pruner", error = %e, "pruner failed"),
                    }
                }
            },
        )));
    }

    let mut producers = Vec::new();
    for spec in opts.queues.iter().cloned() {
        producers.push(tokio::spawn(producer(
            shared.clone(),
            spec,
            stop_rx.clone(),
            opts.shutdown_grace,
        )));
    }
    for producer in producers {
        let _ = producer.await;
    }

    // Whatever is still running after the grace period is aborted and handed back.
    let aborted: Vec<i64> = match shared.in_flight.lock() {
        Ok(mut map) => map
            .drain()
            .map(|(id, handle)| {
                handle.abort();
                id
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    for task in plugin_tasks {
        task.abort();
    }
    match plugins::release_node_jobs(&db, &opts.node, opts.shutdown_grace).await {
        Ok(n) if n > 0 => warn!(
            released = n,
            aborted = aborted.len(),
            "unfinished jobs returned to the queue"
        ),
        Ok(_) => {}
        Err(e) => error!(error = %e, "could not return unfinished jobs; the Lifeline will"),
    }
    if let Err(e) = plugins::deregister(&db, &opts.node).await {
        warn!(error = %e, "could not remove this node's job_nodes row");
    }
    info!(node = %opts.node, "worker stopped");
}

async fn every<F, Fut>(period: Duration, mut stop: watch::Receiver<bool>, mut tick: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => tick().await,
            _ = stop.changed() => break,
        }
    }
}

async fn producer(
    shared: Arc<Shared>,
    spec: QueueSpec,
    mut stop: watch::Receiver<bool>,
    grace: Duration,
) {
    let permits = Arc::new(Semaphore::new(spec.limit));
    // WHY: `notify_one` stores a permit when nobody waits, so a job that finishes while this
    // producer is busy fetching still wakes it.
    let finished = Arc::new(Notify::new());
    let mut wakeups = wake_receiver();
    while !*stop.borrow() {
        let paused = shared.paused.lock().is_ok_and(|p| p.contains(&spec.name));
        let free = permits.available_permits() as u64;
        let mut started = 0;
        if !paused && free > 0 {
            let claimed = if spec.rate.is_some() {
                claim_rate_limited(&shared.db, &spec, free, &shared.node, Utc::now()).await
            } else {
                claim(&shared.db, Some(&spec.name), free, &shared.node, Utc::now()).await
            };
            match claimed {
                Ok(models) => {
                    for model in models {
                        let Ok(permit) = permits.clone().acquire_owned().await else {
                            break;
                        };
                        started += 1;
                        let id = model.id;
                        let task_shared = shared.clone();
                        let task_finished = finished.clone();
                        // WHY: registered under the lock before the task can finish, so its own
                        // `remove` always comes after this `insert` (no stale abort handles).
                        let mut map = match shared.in_flight.lock() {
                            Ok(map) => map,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        let handle = tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(e) = execute(
                                &task_shared.db,
                                &task_shared.dispatch,
                                task_shared.config,
                                &task_shared.node,
                                model,
                            )
                            .await
                            {
                                error!(job_id = id, error = %e, "could not record job result");
                            }
                            if let Ok(mut map) = task_shared.in_flight.lock() {
                                map.remove(&id);
                            }
                            task_finished.notify_one();
                        });
                        map.insert(id, handle.abort_handle());
                    }
                }
                Err(e) => warn!(queue = %spec.name, error = %e, "could not fetch jobs"),
            }
        }
        // Poll again right away only when we just filled slots and more may be waiting.
        if started == 0 || permits.available_permits() == 0 {
            let jitter = 0.8 + rand::random_range(0.0..0.4);
            tokio::select! {
                _ = wakeups.changed() => {}
                _ = finished.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs_f64(jitter)) => {}
                _ = stop.changed() => {}
            }
        }
    }
    // Shutdown: let in-flight jobs finish, up to the grace period.
    let _ = tokio::time::timeout(grace, permits.acquire_many(spec.limit as u32)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_capped_in_size_and_count() {
        let mut errors = json!([]);
        let now = Utc::now();
        for attempt in 1..=25 {
            errors = push_error(
                &errors,
                attempt,
                now,
                Some("node-a"),
                &AttemptError {
                    message: "é".repeat(5000),
                    detail: Some("x".repeat(9000)),
                    duration_ms: Some(12),
                },
            );
        }
        let list = errors.as_array().unwrap();
        assert_eq!(list.len(), 20);
        assert_eq!(list[0]["attempt"], 6);
        assert!(list[19]["error"].as_str().unwrap().len() <= 4096 + 3);
        assert!(list[19]["detail"].as_str().unwrap().len() <= 4096 + 3);
        assert_eq!(list[19]["node"], "node-a");
        assert_eq!(list[19]["duration_ms"], 12);
    }

    #[test]
    fn panic_payloads_become_messages() {
        assert_eq!(panic_message(Box::new("boom")), "panicked: boom");
        assert_eq!(panic_message(Box::new(String::from("x"))), "panicked: x");
        assert_eq!(panic_message(Box::new(42)), "panicked");
    }
}
