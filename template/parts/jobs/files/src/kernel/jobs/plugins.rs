//! The plugins every node runs: Heartbeat, Stager, Lifeline, Pruner, Cron; plus batch
//! completion. All idempotent, no leader election. Each is a plain `async fn(db, now, …)`, so
//! tests call it directly instead of waiting for a timer.
//! GUIDE: .offrails/guides/jobs.md#plugins-pluginsrs-every-node-runs-all-of-them-all-idempotent-no-leader-election

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use croner::{
    Cron,
    parser::{CronParser, Seconds, Year},
};
use sea_orm::{
    ActiveEnum,
    ActiveValue::{NotSet, Set},
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, TransactionSession,
    sea_query::{Expr, Func, OnConflict},
};
use serde_json::{Value, json};

use super::{
    JobBuilder, JobState, QueueSpec,
    executor::{AttemptError, Shared, push_error, releases_unique},
    job, job_batch, job_node, job_queue, recurring_execution, wake,
};
use crate::kernel::db;

const PRUNE_BATCH: u64 = 10_000;
/// A node is alive while its heartbeat is younger than this.
pub const NODE_ALIVE: chrono::Duration = chrono::Duration::seconds(60);
/// Nodes silent for longer are forgotten.
const NODE_FORGET: chrono::Duration = chrono::Duration::minutes(5);
/// How late a cron run may start: a node booting at 09:40 doesn't replay 08:00.
const CRON_GRACE: chrono::Duration = chrono::Duration::seconds(60);

/// A recurring job. `cron` is a standard 5-field expression (`min hour day month weekday`) or a
/// nickname (`@hourly`, `@daily`, `@weekly`, `@monthly`), evaluated in **UTC**. `job` is a
/// non-capturing closure, so `SCHEDULE` stays a `const`; each run is tagged `cron` with
/// `meta.cron = key`.
/// GUIDE: .offrails/guides/jobs.md#recurring-jobs-srcjobsrecurringrs-the-oban-cron-plugin
pub struct Recurring {
    /// APP: permanent; `recurring_executions` and the dashboard reference it.
    pub key: &'static str,
    pub cron: &'static str,
    pub job: fn() -> JobBuilder,
}

// ---------------------------------------------------------------------------
// Heartbeat
// ---------------------------------------------------------------------------

/// The node's queue settings as stored in `job_nodes.queues`.
pub fn queues_json(queues: &[QueueSpec]) -> Value {
    Value::Object(
        queues
            .iter()
            .map(|q| {
                let mut v = json!({ "limit": q.limit });
                if let Some(rate) = q.rate {
                    v["rate"] = json!(rate.label());
                }
                (q.name.clone(), v)
            })
            .collect(),
    )
}

/// Registers this node, makes sure its queues exist, refreshes the paused set, and runs the
/// cancel check: of this node's in-flight jobs, those no longer `executing` here (cancelled,
/// rescued, deleted) get aborted.
/// INVARIANT: cancelling an executing job stops its future (test: cancelling_an_executing_job_aborts_it)
pub(crate) async fn heartbeat(
    shared: &Shared,
    queues: &[QueueSpec],
    started_at: DateTime<Utc>,
) -> Result<(), DbErr> {
    let now = Utc::now();
    job_node::Entity::insert(job_node::ActiveModel {
        node: Set(shared.node.clone()),
        queues: Set(queues_json(queues)),
        started_at: Set(started_at),
        heartbeat_at: Set(now),
    })
    .on_conflict(
        OnConflict::column(job_node::Column::Node)
            .update_columns([job_node::Column::Queues, job_node::Column::HeartbeatAt])
            .to_owned(),
    )
    .exec_without_returning(&shared.db)
    .await?;

    for spec in queues {
        job_queue::Entity::insert(job_queue::ActiveModel {
            name: Set(spec.name.clone()),
            paused: Set(false),
            updated_at: Set(now),
        })
        .on_conflict_do_nothing_on([job_queue::Column::Name])
        .exec_without_returning(&shared.db)
        .await?;
    }

    let paused: HashSet<String> = job_queue::Entity::find()
        .filter(job_queue::Column::Paused.eq(true))
        .all(&shared.db)
        .await?
        .into_iter()
        .map(|r| r.name)
        .collect();
    if let Ok(mut set) = shared.paused.lock() {
        *set = paused;
    }

    let ids: Vec<i64> = shared
        .in_flight
        .lock()
        .map(|m| m.keys().copied().collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(());
    }
    let still_mine: HashSet<i64> = job::Entity::find()
        .select_only()
        .column(job::Column::Id)
        .filter(job::Column::Id.is_in(ids.clone()))
        .filter(job::Column::State.eq(JobState::Executing))
        .filter(job::Column::AttemptedBy.eq(shared.node.clone()))
        .into_tuple::<i64>()
        .all(&shared.db)
        .await?
        .into_iter()
        .collect();
    if let Ok(mut map) = shared.in_flight.lock() {
        for id in ids.into_iter().filter(|id| !still_mine.contains(id)) {
            if let Some(handle) = map.remove(&id) {
                tracing::info!(
                    job_id = id,
                    "aborting a job that is no longer executing here"
                );
                handle.abort();
            }
        }
    }
    Ok(())
}

/// Removes this node's row (graceful shutdown).
pub(crate) async fn deregister(db: &DatabaseConnection, node: &str) -> Result<(), DbErr> {
    job_node::Entity::delete_by_id(node.to_owned())
        .exec(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Stager
// ---------------------------------------------------------------------------

/// `scheduled` / `retryable` jobs whose time has come → `available`. Returns how many.
pub async fn stage(db: &impl ConnectionTrait, now: DateTime<Utc>) -> Result<u64, DbErr> {
    stage_queue(db, now, None).await
}

pub(crate) async fn stage_queue(
    db: &impl ConnectionTrait,
    now: DateTime<Utc>,
    queue: Option<&str>,
) -> Result<u64, DbErr> {
    let mut update = job::Entity::update_many()
        .col_expr(
            job::Column::State,
            Expr::value(JobState::Available.to_value()),
        )
        .filter(job::Column::State.is_in([JobState::Scheduled, JobState::Retryable]))
        .filter(job::Column::ScheduledAt.lte(now));
    if let Some(queue) = queue {
        update = update.filter(job::Column::Queue.eq(queue));
    }
    Ok(update.exec(db).await?.rows_affected)
}

// ---------------------------------------------------------------------------
// Lifeline and shutdown
// ---------------------------------------------------------------------------

/// `executing` jobs whose node has no heartbeat in the last 60s (it crashed or was killed): back
/// to `available` if attempts remain, else `discarded`, with an `"orphaned: …"` error either way.
/// Then forgets nodes silent for more than 5 minutes. Returns how many it rescued.
/// INVARIANT: only dead nodes' jobs are rescued (test: the_lifeline_rescues_only_dead_nodes_jobs)
pub async fn rescue_orphans(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<u64, DbErr> {
    let live: HashSet<String> = job_node::Entity::find()
        .filter(job_node::Column::HeartbeatAt.gt(now - NODE_ALIVE))
        .all(db)
        .await?
        .into_iter()
        .map(|n| n.node)
        .collect();
    let executing = job::Entity::find()
        .filter(job::Column::State.eq(JobState::Executing))
        .all(db)
        .await?;

    let mut rescued = 0;
    for orphan in executing {
        let node = orphan.attempted_by.clone().unwrap_or_default();
        if live.contains(&node) {
            continue;
        }
        // WHY: a job that keeps killing its node (out of memory, an abort) uses up its
        // attempts and is discarded, instead of taking every node down forever.
        let message = format!("orphaned: node {node} stopped responding while the job ran");
        rescued += hand_back(db, &orphan, now, &message, false).await?;
    }
    job_node::Entity::delete_many()
        .filter(job_node::Column::HeartbeatAt.lt(now - NODE_FORGET))
        .exec(db)
        .await?;
    Ok(rescued)
}

/// Graceful shutdown: this node's still-`executing` jobs (aborted after the grace period) go
/// back to `available` at once, with an `"interrupted: …"` error.
pub(crate) async fn release_node_jobs(
    db: &DatabaseConnection,
    node: &str,
    grace: std::time::Duration,
) -> Result<u64, DbErr> {
    let mine = job::Entity::find()
        .filter(job::Column::State.eq(JobState::Executing))
        .filter(job::Column::AttemptedBy.eq(node))
        .all(db)
        .await?;
    let now = Utc::now();
    let message =
        format!("interrupted: node {node} shut down before the job finished (grace {grace:?})");
    let mut released = 0;
    for job in mine {
        released += hand_back(db, &job, now, &message, true).await?;
    }
    if released > 0 {
        wake();
    }
    Ok(released)
}

/// Puts an executing job back to `available` (or `discarded` when out of attempts and
/// `keep_attempt` is false). Guarded by the job's current state and node.
async fn hand_back(
    db: &DatabaseConnection,
    model: &job::Model,
    now: DateTime<Utc>,
    message: &str,
    keep_attempt: bool,
) -> Result<u64, DbErr> {
    let errors = push_error(
        &model.errors,
        model.attempt,
        now,
        model.attempted_by.as_deref(),
        &AttemptError::new(message),
    );
    let mut update = job::Entity::update_many()
        .filter(job::Column::Id.eq(model.id))
        .filter(job::Column::State.eq(JobState::Executing))
        .col_expr(job::Column::Errors, Expr::value(errors));
    update = match &model.attempted_by {
        Some(node) => update.filter(job::Column::AttemptedBy.eq(node.as_str())),
        None => update.filter(job::Column::AttemptedBy.is_null()),
    };
    let discard = !keep_attempt && model.attempt >= model.max_attempts;
    if discard {
        update = update
            .col_expr(
                job::Column::State,
                Expr::value(JobState::Discarded.to_value()),
            )
            .col_expr(job::Column::DiscardedAt, Expr::value(now));
        if releases_unique(model) {
            update = update.col_expr(job::Column::UniqueKey, Expr::value(Option::<String>::None));
        }
    } else {
        update = update
            .col_expr(
                job::Column::State,
                Expr::value(JobState::Available.to_value()),
            )
            .col_expr(job::Column::ScheduledAt, Expr::value(now))
            .col_expr(job::Column::Progress, Expr::value(Option::<i16>::None));
        if keep_attempt && model.attempt >= model.max_attempts {
            // WHY: a deploy isn't the job's fault: it gets its attempt back.
            update = update.col_expr(
                job::Column::MaxAttempts,
                Expr::value(model.max_attempts + 1),
            );
        }
    }
    let changed = update.exec(db).await?.rows_affected;
    if changed > 0
        && discard
        && let Some(batch) = model.batch_id
    {
        finish_batch_if_done(db, batch).await?;
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Pruner
// ---------------------------------------------------------------------------

/// Deletes final jobs whose `completed_at` / `cancelled_at` / `discarded_at` is older than
/// `now - older_than`, by selected ids in batches (short SQLite write locks). Finished batches
/// whose jobs are all gone go too.
/// INVARIANT: the Pruner never deletes a job that isn't final, or a recent one
/// (test: the_pruner_keeps_recent_and_unfinished_jobs)
pub async fn prune(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    older_than: chrono::Duration,
) -> Result<u64, DbErr> {
    let cutoff = now - older_than;
    let groups = [
        (JobState::Completed, job::Column::CompletedAt),
        (JobState::Cancelled, job::Column::CancelledAt),
        (JobState::Discarded, job::Column::DiscardedAt),
    ];
    let mut deleted = 0;
    for (state, column) in groups {
        loop {
            let ids: Vec<i64> = job::Entity::find()
                .select_only()
                .column(job::Column::Id)
                .filter(job::Column::State.eq(state))
                .filter(column.lt(cutoff))
                .order_by_asc(job::Column::Id)
                .limit(PRUNE_BATCH)
                .into_tuple()
                .all(db)
                .await?;
            if ids.is_empty() {
                break;
            }
            let full = ids.len() as u64 == PRUNE_BATCH;
            for chunk in ids.chunks(1000) {
                deleted += job::Entity::delete_many()
                    .filter(job::Column::Id.is_in(chunk.to_vec()))
                    // WHY: re-checked, so a job an operator retried in between survives.
                    .filter(job::Column::State.eq(state))
                    .exec(db)
                    .await?
                    .rows_affected;
            }
            if !full {
                break;
            }
        }
    }
    job_batch::Entity::delete_many()
        .filter(job_batch::Column::FinishedAt.lt(cutoff))
        .exec(db)
        .await?;
    recurring_execution::Entity::delete_many()
        .filter(recurring_execution::Column::RunAt.lt(cutoff))
        .exec(db)
        .await?;
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

/// When no job of `batch` is left unfinished, marks the batch finished and enqueues its callback,
/// exactly once (the guarded `finished_at IS NULL` update decides who does it).
/// INVARIANT: a batch's callback is enqueued once, after its last job finishes
/// (test: a_batch_enqueues_its_callback_once_when_every_job_is_final)
pub async fn finish_batch_if_done<C>(db: &C, batch: i64) -> Result<Option<i64>, DbErr>
where
    C: ConnectionTrait + sea_orm::TransactionTrait,
{
    let open = job::Entity::find()
        .filter(job::Column::BatchId.eq(batch))
        .filter(job::Column::State.is_not_in([
            JobState::Completed,
            JobState::Discarded,
            JobState::Cancelled,
        ]))
        .count(db)
        .await?;
    if open > 0 {
        return Ok(None);
    }
    let txn = db::begin(db).await?;
    let won = job_batch::Entity::update_many()
        .col_expr(job_batch::Column::FinishedAt, Expr::value(Utc::now()))
        .filter(job_batch::Column::Id.eq(batch))
        .filter(job_batch::Column::FinishedAt.is_null())
        .exec(&txn)
        .await?
        .rows_affected
        == 1;
    if !won {
        txn.rollback().await?;
        return Ok(None);
    }
    let Some(row) = job_batch::Entity::find_by_id(batch).one(&txn).await? else {
        txn.rollback().await?;
        return Ok(None);
    };
    let counts: Vec<(JobState, i64)> = job::Entity::find()
        .select_only()
        .column(job::Column::State)
        .column_as(Expr::expr(Func::count(Expr::col(job::Column::Id))), "n")
        .filter(job::Column::BatchId.eq(batch))
        .group_by(job::Column::State)
        .into_tuple()
        .all(&txn)
        .await?;
    let n = |s: JobState| counts.iter().find(|(st, _)| *st == s).map_or(0, |c| c.1);
    let mut callback_id = None;
    if let Some(callback) = row.callback.as_ref().and_then(JobBuilder::from_callback) {
        let inserted = callback
            .tag("batch")
            .meta(json!({ "batch": {
                "id": batch,
                "name": row.name,
                "total": row.total,
                "completed": n(JobState::Completed),
                "discarded": n(JobState::Discarded),
                "cancelled": n(JobState::Cancelled),
            }}))
            .insert(&txn)
            .await?;
        job_batch::Entity::update_many()
            .col_expr(job_batch::Column::CallbackJobId, Expr::value(inserted.id))
            .filter(job_batch::Column::Id.eq(batch))
            .exec(&txn)
            .await?;
        callback_id = Some(inserted.id);
    }
    txn.commit().await?;
    Ok(callback_id)
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

/// Parses a 5-field expression or a nickname. Seconds and years are rejected: one syntax.
pub fn parse_cron(expr: &str) -> Option<Cron> {
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(expr)
        .ok()
}

/// The most recent due time of `expr` at or before `now` (UTC).
pub fn previous_run(expr: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_cron(expr)?.find_previous_occurrence(&now, true).ok()
}

/// The next due time of `expr` strictly after `now` (UTC).
pub fn next_run(expr: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_cron(expr)?.find_next_occurrence(&now, false).ok()
}

/// "At minute 0 past every hour" (English, from croner), or `None` for an invalid expression.
pub fn describe_cron(expr: &str) -> Option<String> {
    Some(parse_cron(expr)?.describe())
}

/// One cron tick. For each entry whose latest due time is within the last 60s, claims
/// `(key, run_at)` in `recurring_executions` and enqueues the job in the same transaction: only
/// the node whose insert succeeds enqueues, so each run happens once across all nodes. Returns
/// the ids of the jobs enqueued.
pub async fn cron_tick(
    db: &DatabaseConnection,
    schedule: &[Recurring],
    now: DateTime<Utc>,
) -> Result<Vec<i64>, DbErr> {
    cron_tick_skipping(db, schedule, now, &Mutex::new(HashMap::new())).await
}

/// `cron_tick`, skipping runs this node already handled (the worker ticks every second).
pub(crate) async fn cron_tick_skipping(
    db: &DatabaseConnection,
    schedule: &[Recurring],
    now: DateTime<Utc>,
    handled: &Mutex<HashMap<&'static str, DateTime<Utc>>>,
) -> Result<Vec<i64>, DbErr> {
    let mut enqueued = Vec::new();
    for entry in schedule {
        let Some(run_at) = previous_run(entry.cron, now) else {
            tracing::error!(
                key = entry.key,
                cron = entry.cron,
                "invalid cron expression, skipped"
            );
            continue;
        };
        if now - run_at > CRON_GRACE
            || handled
                .lock()
                .is_ok_and(|h| h.get(entry.key) == Some(&run_at))
        {
            continue;
        }
        if let Some(id) = fire(db, entry, run_at, false).await? {
            enqueued.push(id);
        }
        if let Ok(mut h) = handled.lock() {
            h.insert(entry.key, run_at);
        }
    }
    if !enqueued.is_empty() {
        wake();
    }
    Ok(enqueued)
}

/// Claims `(key, run_at)` and enqueues the entry's job in one transaction. `None` when another
/// node (or an earlier tick) owns this run.
async fn fire(
    db: &DatabaseConnection,
    entry: &Recurring,
    run_at: DateTime<Utc>,
    manual: bool,
) -> Result<Option<i64>, DbErr> {
    let txn = db::begin(db).await?;
    let claimed = recurring_execution::Entity::insert(recurring_execution::ActiveModel {
        id: NotSet,
        key: Set(entry.key.to_owned()),
        run_at: Set(run_at),
        job_id: Set(None),
    })
    .on_conflict_do_nothing_on([
        recurring_execution::Column::Key,
        recurring_execution::Column::RunAt,
    ])
    .exec(&txn)
    .await?;
    let execution = match claimed {
        sea_orm::TryInsertResult::Inserted(res) => res.last_insert_id,
        _ => {
            txn.rollback().await?;
            return Ok(None);
        }
    };
    let mut meta = json!({ "cron": entry.key });
    if manual {
        meta["manual"] = json!(true);
    }
    let inserted = (entry.job)().tag("cron").meta(meta).insert(&txn).await?;
    recurring_execution::Entity::update_many()
        .col_expr(recurring_execution::Column::JobId, Expr::value(inserted.id))
        .filter(recurring_execution::Column::Id.eq(execution))
        .exec(&txn)
        .await?;
    txn.commit().await?;
    Ok(Some(inserted.id))
}

/// Runs a cron entry now, outside its schedule (the dashboard's "Run now", `app jobs cron run`).
/// Recorded as an execution at `now`, so it shows as the entry's last run.
pub async fn run_cron_now(db: &DatabaseConnection, entry: &Recurring) -> Result<i64, DbErr> {
    let now = Utc::now();
    let id = fire(db, entry, now, true)
        .await?
        .ok_or_else(|| DbErr::Custom("this cron entry just ran; try again".into()))?;
    wake();
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn cron_expressions_are_utc_and_five_fields() {
        let now = at("2026-10-16T10:17:30Z");
        assert_eq!(
            previous_run("0 * * * *", now),
            Some(at("2026-10-16T10:00:00Z"))
        );
        assert_eq!(next_run("0 * * * *", now), Some(at("2026-10-16T11:00:00Z")));
        assert_eq!(next_run("@daily", now), Some(at("2026-10-17T00:00:00Z")));
        assert_eq!(
            next_run("0 8 * * MON", now),
            Some(at("2026-10-19T08:00:00Z"))
        );
        assert!(next_run("not cron", now).is_none());
        assert!(
            next_run("0 0 * * * *", now).is_none(),
            "seconds are rejected"
        );
        assert!(describe_cron("*/5 * * * *").is_some());
    }

    #[test]
    fn queue_json_lists_limits_and_rates() {
        let q = queues_json(&[
            QueueSpec::new("default", 10),
            QueueSpec::new("hooks", 2).rate(super::super::Rate::parse("60/1m").unwrap()),
        ]);
        assert_eq!(q["default"]["limit"], 10);
        assert_eq!(q["hooks"]["rate"], "60/1m");
        assert!(q["default"].get("rate").is_none());
    }
}
