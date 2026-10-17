//! Enqueueing: `JobBuilder`, uniqueness, `insert_all` and batches.
//! GUIDE: .offrails/guides/jobs.md#enqueueing-the-builder

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveValue::{NotSet, Set},
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, TransactionSession,
    TransactionTrait,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{JobState, job, job_batch, wake};
use crate::kernel::db;

/// Rows per `insert_all` statement: 22 columns × 500 rows stays far below SQLite's bind limit.
const INSERT_CHUNK: usize = 500;

// ---------------------------------------------------------------------------
// Uniqueness
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueStates {
    /// The key is released when the job reaches a final state (default).
    Incomplete,
    /// The key lives until the Pruner deletes the row.
    All,
}

/// "At most one of these": by all args, some args, or the worker alone, optionally per period.
/// GUIDE: .offrails/guides/jobs.md#uniqueness
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unique {
    keys: Option<Vec<String>>,
    period: Option<chrono::Duration>,
    states: UniqueStates,
}

impl Unique {
    /// Unique on all args.
    pub fn args() -> Self {
        Self {
            keys: None,
            period: None,
            states: UniqueStates::Incomplete,
        }
    }

    /// Unique on a subset of args (`&["user_id"]`).
    pub fn args_keys(keys: &[&str]) -> Self {
        Self {
            keys: Some(keys.iter().map(|k| (*k).to_owned()).collect()),
            ..Self::args()
        }
    }

    /// Unique on the worker (and queue) alone.
    pub fn worker() -> Self {
        Self {
            keys: Some(Vec::new()),
            ..Self::args()
        }
    }

    /// One job per period bucket (`floor(now / period)`): a debounce.
    pub fn period(mut self, period: chrono::Duration) -> Self {
        self.period = Some(period);
        self
    }

    pub fn states(mut self, states: UniqueStates) -> Self {
        self.states = states;
        self
    }
}

/// `hex(sha256("{worker}|{queue}|{selected args}|{bucket}"))`.
/// WHY: serde_json's map is a `BTreeMap` (no `preserve_order` feature), so the JSON is canonical:
/// `{"a":1,"b":2}` and `{"b":2,"a":1}` give the same key.
pub fn unique_key(
    unique: &Unique,
    worker: &str,
    queue: &str,
    args: &Value,
    now: DateTime<Utc>,
) -> String {
    let selected = match (&unique.keys, args) {
        (None, args) => args.clone(),
        (Some(keys), Value::Object(map)) => Value::Object(
            map.iter()
                .filter(|(k, _)| keys.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        (Some(_), _) => Value::Null,
    };
    let bucket = unique
        .period
        .map(|p| now.timestamp().div_euclid(p.num_seconds().max(1)));
    let canonical = format!(
        "{worker}|{queue}|{selected}|{}",
        bucket.map_or(String::new(), |b| b.to_string())
    );
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inserted {
    pub id: i64,
    /// A unique job that already existed: `id` is the existing row.
    pub duplicate: bool,
}

/// What a `Worker` impl contributes: its constants and default uniqueness.
#[derive(Debug, Clone)]
pub struct JobDefaults {
    pub worker: &'static str,
    pub queue: &'static str,
    pub max_attempts: i32,
    pub priority: i16,
    pub unique: Option<Unique>,
}

/// Not generic: it holds the serialized args, so mixed workers batch together and recurring
/// entries can be `fn() -> JobBuilder`.
#[derive(Debug, Clone)]
pub struct JobBuilder {
    worker: String,
    args: Result<Value, String>,
    queue: String,
    priority: i16,
    max_attempts: i32,
    scheduled_at: Option<DateTime<Utc>>,
    tags: Vec<String>,
    meta: serde_json::Map<String, Value>,
    unique: Option<Unique>,
    batch_id: Option<i64>,
}

impl JobBuilder {
    pub fn new(defaults: JobDefaults, args: serde_json::Result<Value>) -> Self {
        Self {
            worker: defaults.worker.to_owned(),
            args: args.map_err(|e| e.to_string()),
            queue: defaults.queue.to_owned(),
            priority: defaults.priority.clamp(0, 9),
            max_attempts: defaults.max_attempts.max(1),
            scheduled_at: None,
            tags: Vec::new(),
            meta: serde_json::Map::new(),
            unique: defaults.unique,
            batch_id: None,
        }
    }

    /// The worker this builder inserts (the cron page lists it).
    pub fn worker_name(&self) -> &str {
        &self.worker
    }

    pub fn queue_name(&self) -> &str {
        &self.queue
    }

    /// Overrides `Worker::QUEUE`.
    pub fn queue(mut self, queue: &str) -> Self {
        self.queue = queue.to_owned();
        self
    }

    /// Clamped to 0..=9 (0 runs first).
    pub fn priority(mut self, priority: i16) -> Self {
        self.priority = priority.clamp(0, 9);
        self
    }

    /// At least 1.
    pub fn max_attempts(mut self, max_attempts: i32) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    pub fn schedule_in(mut self, delay: chrono::Duration) -> Self {
        self.scheduled_at = Some(Utc::now() + delay);
        self
    }

    pub fn schedule_at(mut self, at: DateTime<Utc>) -> Self {
        self.scheduled_at = Some(at);
        self
    }

    /// Replaces the tags.
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Appends one tag.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Merges the object's entries into the job's `meta`. Non-objects are ignored.
    pub fn meta(mut self, meta: Value) -> Self {
        if let Value::Object(map) = meta {
            self.meta.extend(map);
        }
        self
    }

    pub fn unique(mut self, unique: Unique) -> Self {
        self.unique = Some(unique);
        self
    }

    /// Drops the worker's default uniqueness for this insert (fan-out with `insert_all`, where
    /// every row is wanted).
    pub fn without_unique(mut self) -> Self {
        self.unique = None;
        self
    }

    fn into_active_model(
        self,
        now: DateTime<Utc>,
    ) -> Result<(job::ActiveModel, Option<String>), DbErr> {
        // INVARIANT: args that don't serialize never become a row (test: args_that_fail_to_serialize_insert_nothing)
        let args = self
            .args
            .map_err(|e| DbErr::Custom(format!("job args do not serialize: {e}")))?;
        let scheduled_at = self.scheduled_at.unwrap_or(now);
        // WHY: the clock is Rust's `Utc::now()`, never the database's: SQLite and Postgres
        // disagree on `now()` precision and time zones, and tests pass their own `now`.
        let state = if scheduled_at > now {
            JobState::Scheduled
        } else {
            JobState::Available
        };
        let mut meta = self.meta;
        let key = self.unique.as_ref().map(|u| {
            meta.insert(
                "unique_states".into(),
                json!(match u.states {
                    UniqueStates::Incomplete => "incomplete",
                    UniqueStates::All => "all",
                }),
            );
            unique_key(u, &self.worker, &self.queue, &args, now)
        });
        let model = job::ActiveModel {
            id: NotSet,
            state: Set(state),
            queue: Set(self.queue),
            worker: Set(self.worker),
            args: Set(args),
            meta: Set(Value::Object(meta)),
            tags: Set(json!(self.tags)),
            errors: Set(json!([])),
            attempt: Set(0),
            max_attempts: Set(self.max_attempts),
            priority: Set(self.priority),
            progress: Set(None),
            batch_id: Set(self.batch_id),
            unique_key: Set(key.clone()),
            attempted_by: Set(None),
            inserted_at: Set(now),
            scheduled_at: Set(scheduled_at),
            attempted_at: Set(None),
            completed_at: Set(None),
            cancelled_at: Set(None),
            discarded_at: Set(None),
        };
        Ok((model, key))
    }

    /// Inserts the job. Pass the transaction that created the data the job needs, so the job
    /// commits (or rolls back) with it: after-commit semantics for free.
    /// GUIDE: .offrails/guides/jobs.md#enqueueing-the-builder
    pub async fn insert(self, db: &impl ConnectionTrait) -> Result<Inserted, DbErr> {
        let (model, key) = self.into_active_model(Utc::now())?;
        let Some(key) = key else {
            let res = job::Entity::insert(model).exec(db).await?;
            wake();
            return Ok(Inserted {
                id: res.last_insert_id,
                duplicate: false,
            });
        };

        // INVARIANT: two inserts of the same unique job leave one row, on both backends, even
        // concurrently (test: unique_jobs_insert_once_even_concurrently)
        // WHY: `on_conflict_do_nothing_on`, not a caught unique violation: on Postgres a failed
        // statement aborts the caller's transaction.
        let attempt = job::Entity::insert(model)
            .on_conflict_do_nothing_on([job::Column::UniqueKey])
            .exec(db)
            .await?;
        match attempt {
            sea_orm::TryInsertResult::Inserted(res) => {
                wake();
                Ok(Inserted {
                    id: res.last_insert_id,
                    duplicate: false,
                })
            }
            _ => {
                let existing = job::Entity::find()
                    .filter(job::Column::UniqueKey.eq(key))
                    .one(db)
                    .await?
                    .ok_or(DbErr::RecordNotFound("the duplicate job vanished".into()))?;
                Ok(Inserted {
                    id: existing.id,
                    duplicate: true,
                })
            }
        }
    }

    /// The callback form stored on a batch (`job_batches.callback`).
    fn to_callback(&self) -> Result<Value, DbErr> {
        let args = self
            .args
            .clone()
            .map_err(|e| DbErr::Custom(format!("callback args do not serialize: {e}")))?;
        Ok(json!({
            "worker": self.worker,
            "args": args,
            "queue": self.queue,
            "priority": self.priority,
            "max_attempts": self.max_attempts,
            "tags": self.tags,
            "meta": self.meta,
        }))
    }

    pub(crate) fn from_callback(raw: &Value) -> Option<Self> {
        let worker = raw.get("worker")?.as_str()?.to_owned();
        Some(Self {
            worker,
            args: Ok(raw.get("args").cloned().unwrap_or(Value::Null)),
            queue: raw.get("queue")?.as_str()?.to_owned(),
            priority: raw.get("priority").and_then(Value::as_i64).unwrap_or(0) as i16,
            max_attempts: raw
                .get("max_attempts")
                .and_then(Value::as_i64)
                .unwrap_or(20) as i32,
            scheduled_at: None,
            tags: raw
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            meta: raw
                .get("meta")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            unique: None,
            batch_id: None,
        })
    }
}

/// Inserts many jobs in as few statements as possible (fan-out). Returns how many.
/// Refuses unique jobs: insert those one by one, so duplicates are detected.
pub async fn insert_all(
    db: &impl ConnectionTrait,
    builders: Vec<JobBuilder>,
) -> Result<u64, DbErr> {
    if builders.iter().any(|b| b.unique.is_some()) {
        return Err(DbErr::Custom(
            "insert_all refuses unique jobs: insert them one by one".into(),
        ));
    }
    let now = Utc::now();
    let mut rows = Vec::with_capacity(builders.len());
    for builder in builders {
        rows.push(builder.into_active_model(now)?.0);
    }
    let count = rows.len() as u64;
    let mut rows = rows.into_iter().peekable();
    while rows.peek().is_some() {
        let chunk: Vec<_> = rows.by_ref().take(INSERT_CHUNK).collect();
        job::Entity::insert_many(chunk)
            .exec_without_returning(db)
            .await?;
    }
    if count > 0 {
        wake();
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

/// A group of jobs that finishes as one: when every member reaches a final state (completed,
/// discarded or cancelled), the callback job is enqueued exactly once, with
/// `meta.batch = {id, completed, discarded, cancelled}`.
///
/// ```ignore
/// let batch = Batch::new("import 42")
///     .on_complete(ImportFinishedJob { import_id: 42 }.into_job())
///     .insert(&txn, rows.iter().map(|r| ImportRowJob { row_id: r.id }.into_job()).collect())
///     .await?;
/// ```
pub struct Batch {
    name: String,
    callback: Option<JobBuilder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchInserted {
    pub id: i64,
    pub jobs: u64,
}

impl Batch {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            callback: None,
        }
    }

    pub fn on_complete(mut self, callback: JobBuilder) -> Self {
        self.callback = Some(callback);
        self
    }

    /// Inserts the batch row and its jobs atomically (a savepoint inside the caller's
    /// transaction). An empty batch finishes at once.
    pub async fn insert<C>(self, db: &C, jobs: Vec<JobBuilder>) -> Result<BatchInserted, DbErr>
    where
        C: TransactionTrait,
    {
        let txn = db::begin(db).await?;
        let callback = self
            .callback
            .as_ref()
            .map(JobBuilder::to_callback)
            .transpose()?;
        let batch = job_batch::Entity::insert(job_batch::ActiveModel {
            id: NotSet,
            name: Set(self.name),
            total: Set(jobs.len() as i32),
            callback: Set(callback),
            callback_job_id: Set(None),
            inserted_at: Set(Utc::now()),
            finished_at: Set(None),
        })
        .exec(&txn)
        .await?
        .last_insert_id;
        let count = insert_all(
            &txn,
            jobs.into_iter()
                .map(|mut j| {
                    j.batch_id = Some(batch);
                    j
                })
                .collect(),
        )
        .await?;
        if count == 0 {
            super::plugins::finish_batch_if_done(&txn, batch).await?;
        }
        txn.commit().await?;
        Ok(BatchInserted {
            id: batch,
            jobs: count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> JobDefaults {
        JobDefaults {
            worker: "w",
            queue: "default",
            max_attempts: 20,
            priority: 0,
            unique: None,
        }
    }

    #[test]
    fn unique_keys_are_canonical_and_scoped() {
        let now = Utc::now();
        let a = unique_key(&Unique::args(), "w", "q", &json!({"a":1,"b":2}), now);
        let b = unique_key(&Unique::args(), "w", "q", &json!({"b":2,"a":1}), now);
        assert_eq!(a, b);
        assert_ne!(
            a,
            unique_key(&Unique::args(), "w", "other", &json!({"a":1,"b":2}), now)
        );
        let keyed = Unique::args_keys(&["a"]);
        assert_eq!(
            unique_key(&keyed, "w", "q", &json!({"a":1,"b":2}), now),
            unique_key(&keyed, "w", "q", &json!({"a":1,"b":3}), now)
        );
        let worker = Unique::worker();
        assert_eq!(
            unique_key(&worker, "w", "q", &json!({"a":1}), now),
            unique_key(&worker, "w", "q", &json!({"a":2}), now)
        );
    }

    #[test]
    fn periods_bucket_time() {
        let u = Unique::args().period(chrono::Duration::minutes(1));
        let t = DateTime::parse_from_rfc3339("2026-10-16T10:00:10Z")
            .unwrap()
            .with_timezone(&Utc);
        let same = t + chrono::Duration::seconds(30);
        let next = t + chrono::Duration::seconds(60);
        let k = |at| unique_key(&u, "w", "q", &json!({}), at);
        assert_eq!(k(t), k(same));
        assert_ne!(k(t), k(next));
    }

    #[test]
    fn the_builder_clamps_priority_and_attempts() {
        let b = JobBuilder::new(defaults(), Ok(json!({})))
            .priority(42)
            .max_attempts(0);
        assert_eq!(b.priority, 9);
        assert_eq!(b.max_attempts, 1);
        assert_eq!(
            JobBuilder::new(defaults(), Ok(json!({})))
                .priority(-3)
                .priority,
            0
        );
    }

    #[test]
    fn callbacks_round_trip() {
        let b = JobBuilder::new(defaults(), Ok(json!({"x": 1})))
            .queue("q")
            .priority(3)
            .tag("t")
            .meta(json!({"m": true}));
        let back = JobBuilder::from_callback(&b.to_callback().unwrap()).unwrap();
        assert_eq!(back.worker, "w");
        assert_eq!(back.queue, "q");
        assert_eq!(back.priority, 3);
        assert_eq!(back.tags, ["t"]);
        assert_eq!(back.args.unwrap(), json!({"x": 1}));
    }
}
