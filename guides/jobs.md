# Jobs (Active Job + Solid Queue, done the Oban way → `kernel/jobs/`)

> **TL;DR**
> - A persistent, database-backed queue in `src/kernel/jobs/`, modeled on Oban: states, retries with backoff, uniqueness, snooze/cancel/discard, cron, Stager/Lifeline/Pruner plugins, safe on every node.
> - Args hold ids, never records or secrets (args are visible in `/admin/jobs`); enqueue inside the transaction that creates the data.
> - Jobs are idempotent (at-least-once delivery); size `MAX_ATTEMPTS` to the work, and transactional mail expires instead of retrying for weeks.
> - Test with `drain_jobs`, `assert_enqueued`, `perform_job`; no sleeps.
>
> Read the rest when adding a worker, a recurring job, a queue, or touching the kernel queue.

**Use the part.** The `jobs` [parts](samples.md#the-samples) are this guide built, tested on SQLite and Postgres and security-reviewed, with more on top: batches, rate-limited queues, priorities, timeouts, and args and errors redacted wherever they're shown or searched. When the product needs this, add the part rather than writing it from this guide: `cargo offrails add-part jobs` (dependencies come first; [workflow](../workflow.md#when-the-product-grows-a-need-add-the-part)). Keep its `// SECURITY:` markers and the tests its CHECKS.md cites. This guide stays the spec: hand-write from it only when no part fits, or the human declined the part.

Background work runs from a persistent, database-backed queue that you generate into `src/kernel/jobs/`. It's modeled on Elixir's **Oban**:

- jobs are rows with an explicit state machine;
- finished jobs are **kept** (and pruned later), so you have history to inspect;
- queues have concurrency limits and can be paused;
- a dashboard and a CLI sit on top ([jobs-dashboard](jobs-dashboard.md)).

The same queue runs on SQLite and Postgres, so you need no Redis and no extra service. Everything below was compiled and tested on both (sea-orm 2.0.4, axum 0.8.9, croner 4.0.1, rand 0.10.3; 2026-10-12).

| Rails / Oban | Off Rails |
|--------------|-----------|
| `class SendWelcomeEmailJob < ApplicationJob` / `use Oban.Worker` | `struct SendWelcomeEmailJob { user_id: i64 }` + `impl Worker` in `src/jobs/send_welcome_email.rs` |
| `perform_later(args)` / `Oban.insert(job)` | `jobs::enqueue(&txn, SendWelcomeEmailJob { user_id }).await?` |
| `set(wait: 1.hour)` / `schedule_in: 3600` | `job.into_job().schedule_in(chrono::Duration::hours(1)).insert(&txn).await?` |
| `queue_as :mailers` / `queue: :mailers` | `const QUEUE: &'static str = "mailers";` |
| `retry_on` / `max_attempts:` | `const MAX_ATTEMPTS: i32` + `backoff()`; return `Err` to retry |
| `discard_on` / `{:cancel, reason}` | `Ok(Outcome::Cancel("post deleted".into()))` |
| `{:snooze, seconds}` | `Ok(Outcome::Snooze(Duration::minutes(5)))` |
| `unique: [period: 60]` / `limits_concurrency` | `.unique(Unique::args().period(Duration::minutes(1)))` |
| Solid Queue / Oban | `jobs`, `job_queues`, `job_nodes`, `recurring_executions` tables + `kernel/jobs/` |
| `config/recurring.yml` / `Oban.Plugins.Cron` | `src/jobs/recurring.rs` (`SCHEDULE: &[Recurring]`) |
| `bin/jobs` | `app worker` |
| `SOLID_QUEUE_IN_PUMA` | `APP_INLINE_WORKER=true` |
| Mission Control – Jobs / Oban Web | `/admin/jobs` + `app jobs …` ([jobs-dashboard](jobs-dashboard.md)) |
| `perform_enqueued_jobs` / `Oban.Testing` | `app.drain_jobs()`, `app.assert_enqueued::<W>(…)`, `app.perform_job(w)` |


**Time types:** scheduling and backoff use `chrono::Duration` (`TimeDelta`; `schedule_in`, `Unique::period`, `WorkerConfig::backoff`), because they're added to `DateTimeUtc` values stored in the database. Timeouts and shutdown grace use `std::time::Duration` (`Worker::TIMEOUT`, `WorkerConfig::timeout`, `shutdown_grace`), because tokio consumes them.
## Files

```
src/kernel/jobs/mod.rs       entities (jobs, job_queues, job_nodes, recurring_executions), JobState, Outcome,
                             JobInfo, WorkerConfig, DispatchFn, the builder (JobDefaults, JobBuilder, Unique), insert_all
src/kernel/jobs/executor.rs  WorkerOptions, run (producers + shutdown), claim, record, drain (tests)
src/kernel/jobs/plugins.rs   Recurring, Heartbeat, stage, rescue_orphans, prune, cron_tick, next_run/previous_run
src/kernel/jobs/admin.rs     queries + actions for the dashboard and the CLI (jobs-dashboard.md)
src/kernel/jobs/web.rs       /admin/jobs (jobs-dashboard.md)
src/kernel/ui.rs             shared chrome for kernel pages (jobs dashboard, /dev, mailbox)
src/jobs/mod.rs              the app's Worker trait, enqueue, dispatch, worker_config, dispatch_fn
src/jobs/recurring.rs        SCHEDULE
src/jobs/<name>.rs           one file per worker (deliver_email.rs, cleanup.rs from `new`)
```

The kernel never names an app type: it gets the app's dispatcher as a `DispatchFn` and per-worker settings through a `WorkerConfigFn`.

## The state machine

```
                insert (scheduled_at > now)        insert (due now)
                         │                               │
                         ▼                               ▼
                    scheduled ──── Stager (due) ───▶ available ◀──── retry / run now (dashboard)
                                                         │
                   retryable ──── Stager (due) ──────────┤ claim (attempt += 1)
                       ▲                                 ▼
                       │  Err, attempt < max          executing ──── Ok(Done) ──────────▶ completed
                       └──────────────────────────────── │ ──────── Ok(Snooze) ─────────▶ scheduled
                                                         │ ──────── Ok(Cancel) / cancel ─▶ cancelled
                                                         └ Err at max / Ok(Discard) ────▶ discarded
```

`completed`, `cancelled` and `discarded` are **final**. Rows stay until the Pruner deletes them (`APP_JOBS_PRUNE_DAYS`, default 7). The dashboard needs that history, and so do you when debugging.

A job becomes runnable only when it is `available` **and** `scheduled_at <= now`. The Stager flips `scheduled`/`retryable` rows to `available` once their `scheduled_at` passes; operator actions ("retry", "run now") set both.

## Tables (migration `create_jobs`, generated by `generators/new.md`; every app has jobs)

```
jobs:
  id            big_pk_auto
  state         string            // CHECK in (available, scheduled, executing, retryable, completed, discarded, cancelled)
  queue         string            default "default"
  worker        string            // Worker::NAME. Permanent: rows reference it
  args          json              // serde_json::Value of the worker struct
  meta          json              default {}   // cron key, snoozed count, unique_states, anything the app adds
  tags          json              default []   // ["digest", "user:42"]: filterable in the dashboard
  errors        json              default []   // [{attempt, at, error}], one per failed attempt
  attempt       integer           default 0
  max_attempts  integer           default 20
  priority      small_integer     default 0    // 0 = highest … 9 = lowest
  unique_key    string_null       // see "Uniqueness"
  attempted_by  string_null       // node id: "<hostname>:<pid>"
  inserted_at   timestamp_with_time_zone
  scheduled_at  timestamp_with_time_zone     // when it becomes runnable
  attempted_at  timestamp_with_time_zone_null
  completed_at  timestamp_with_time_zone_null
  cancelled_at  timestamp_with_time_zone_null
  discarded_at  timestamp_with_time_zone_null
index jobs_fetch_idx     (state, queue, priority, scheduled_at, id)
index jobs_worker_idx    (worker, state)
index jobs_inserted_idx  (inserted_at)
unique index jobs_unique_key_key (unique_key)   // NULLs never collide (verified on SQLite and Postgres)

job_queues:     name string pk, paused boolean default false, updated_at timestamptz      // pause/resume across nodes
job_nodes:      node string pk, queues json ({"default": 10}), started_at, heartbeat_at   // Heartbeat + Lifeline + dashboard
recurring_executions: id big_pk_auto, key string, run_at timestamptz; unique index (key, run_at)
```

Store `state` as a `DeriveActiveEnum` string (`rename_all = "snake_case"`) with the `CHECK` constraint, like any enum ([models](models.md)). The non-auto-increment string primary keys need `#[sea_orm(primary_key, auto_increment = false)]`.

## The `Worker` trait and dispatcher (`src/jobs/mod.rs`)

The trait and dispatcher live in the app because they know `AppState`.

```rust
pub use crate::kernel::jobs::{Inserted, JobBuilder, JobInfo, Outcome, Unique, UniqueStates};

pub trait Worker: Serialize + DeserializeOwned + Send + 'static {
    const NAME: &'static str;                          // permanent, snake_case: "send_welcome_email"
    const QUEUE: &'static str = "default";
    const MAX_ATTEMPTS: i32 = 20;
    const PRIORITY: i16 = 0;
    const TIMEOUT: Duration = Duration::from_secs(300); // std::time::Duration

    /// Default uniqueness for every insert of this worker.
    fn unique() -> Option<Unique> { None }

    fn backoff(attempt: i32) -> chrono::Duration { kjobs::default_backoff(attempt) }

    fn perform(self, job: &JobInfo, state: &AppState)
        -> impl Future<Output = anyhow::Result<Outcome>> + Send;

    /// The builder with this worker's defaults.
    fn into_job(self) -> JobBuilder where Self: Sized {
        JobBuilder::new(
            JobDefaults { worker: Self::NAME, queue: Self::QUEUE, max_attempts: Self::MAX_ATTEMPTS,
                          priority: Self::PRIORITY, unique: Self::unique() },
            serde_json::to_value(&self),
        )
    }
}

/// `perform_later` with the worker's defaults. Pass the transaction that created the job's data.
pub async fn enqueue<W: Worker>(db: &impl ConnectionTrait, w: W) -> Result<Inserted, AppError> {
    Ok(w.into_job().insert(db).await?)
}

/// Explicit, compile-checked registry: every worker appears here exactly once.
pub async fn dispatch(state: AppState, worker: String, args: Value, job: JobInfo) -> anyhow::Result<Outcome> {
    match worker.as_str() {
        DeliverEmailJob::NAME => run::<DeliverEmailJob>(&state, args, &job).await,
        CleanupJob::NAME      => run::<CleanupJob>(&state, args, &job).await,
        other => Ok(Outcome::Discard(format!("unknown worker: {other}"))),
    }
}

/// Same match as `dispatch`, for the kernel's timeout and backoff.
pub fn worker_config(worker: &str) -> WorkerConfig {
    match worker {
        DeliverEmailJob::NAME => config::<DeliverEmailJob>(),
        CleanupJob::NAME      => config::<CleanupJob>(),
        _ => WorkerConfig::default(),
    }
}

fn config<W: Worker>() -> WorkerConfig { WorkerConfig { timeout: W::TIMEOUT, backoff: W::backoff } }

async fn run<W: Worker>(state: &AppState, args: Value, job: &JobInfo) -> anyhow::Result<Outcome> {
    match serde_json::from_value::<W>(args) {
        Ok(w) => w.perform(job, state).await,
        Err(e) => Ok(Outcome::Discard(format!("args don't deserialize: {e}"))),   // retrying won't fix it
    }
}

/// The dispatcher as the function the kernel calls.
pub fn dispatch_fn(state: AppState) -> DispatchFn {
    Arc::new(move |worker, args, job| Box::pin(dispatch(state.clone(), worker, args, job)))
}
```

In the kernel (`kernel/jobs/mod.rs`):

```rust
pub enum Outcome {
    Done,
    Snooze(chrono::Duration),  // not now: back to `scheduled`; doesn't consume an attempt
    Cancel(String),            // the work no longer matters (record deleted) → cancelled
    Discard(String),           // permanently failed, retrying can't help (invalid address) → discarded
}
// Err(e) → retryable with backoff, or discarded at max_attempts.

pub struct JobInfo { pub id: i64, pub attempt: i32, pub max_attempts: i32, pub inserted_at: DateTimeUtc, pub tags: Vec<String>, pub meta: Value }
pub struct WorkerConfig { pub timeout: std::time::Duration, pub backoff: fn(i32) -> chrono::Duration }   // Default: 300s, default_backoff
pub type DispatchFn = Arc<dyn Fn(String, Value, JobInfo) -> BoxFuture<anyhow::Result<Outcome>> + Send + Sync>;
pub type WorkerConfigFn = fn(&str) -> WorkerConfig;
```

**`NAME` strings are permanent.** Rows already in the queue (and the dashboard's history) reference them. Renaming a struct is fine, but keep its `NAME`. When changing a struct's fields, keep old rows decodable: `#[serde(default)]` on new fields, or `#[serde(flatten)]` when wrapping the old shape (see `DeliverEmailJob` in [mailers](mailers.md#delivering)).

## Enqueueing: the builder

```rust
// defaults
jobs::enqueue(&txn, NotifyAuthorJob { comment_id }).await?;

// everything else
SendWeeklyDigestJob { user_id }
    .into_job()
    .queue("digests")                                    // override W::QUEUE
    .priority(3)                                         // clamped to 0..=9
    .schedule_in(chrono::Duration::hours(1))             // or .schedule_at(dt)
    .max_attempts(5)                                     // at least 1
    .tags(["digest", &format!("user:{user_id}")])        // replaces; .tag("x") appends
    .meta(json!({"source": "settings_page"}))            // merged into meta
    .unique(Unique::args().period(chrono::Duration::hours(24)))
    .insert(&txn)                                        // &DatabaseConnection or &DatabaseTransaction
    .await?;                                             // -> Inserted { id, duplicate: bool }
```

- `JobBuilder` is **not generic** (it holds the serialized args), so mixed workers batch together and recurring entries can be `fn() -> JobBuilder`.
- `insert` takes any `ConnectionTrait`. **Pass the transaction** that created the data the job needs, so the job is committed with it (after-commit semantics, for free).
- `state` is `scheduled` if `scheduled_at > now`, else `available`. Timestamps come from `Utc::now()` in Rust, never the database clock.
- Args that fail to serialize make `insert` return `Err` (no row).
- `kernel::jobs::insert_all(db, Vec<JobBuilder>)` does a batch insert (fan-out) in one statement. It refuses unique jobs (insert those one by one).
- An insert wakes this process's producers immediately (a `tokio::sync::Notify`), so inline-worker apps feel instant.

### Uniqueness

`Unique::args()`, `Unique::args_keys(&["user_id"])` or `Unique::worker()`, plus optional `.period(chrono::Duration)` and `.states(UniqueStates::Incomplete | UniqueStates::All)`:

1. `unique_key = hex(sha256("{worker}|{queue}|{selected args as JSON}|{bucket}"))`, where `bucket = floor(now / period)` (empty without a period: unique "while it exists"). serde_json's map is a `BTreeMap`, so the JSON is canonical.
2. Insert with `job::Entity::insert(row).on_conflict_do_nothing_on([job::Column::UniqueKey]).exec(db)`. In sea-orm 2 that returns `TryInsertResult::{Inserted, Conflicted, Empty}`, **not** a `RecordNotInserted` error (and `.do_nothing()` is deprecated). On `Conflicted`, load the row by `unique_key` and return `Inserted { id: existing, duplicate: true }`. Atomic on both backends, no advisory locks.
3. The builder records `meta.unique_states = "incomplete" | "all"`. With `Incomplete` (the default), the kernel sets `unique_key = NULL` when the job reaches a final state (worker outcome, operator cancel, Lifeline discard), so an identical job can be inserted afterwards. With `All`, the key lives until the Pruner deletes the row.

Use uniqueness for "at most one pending digest per user", or a debounce ("re-index this post at most once a minute"). It's not a substitute for idempotency.

## Kernel spec

### Worker runtime (`executor.rs`)

```rust
pub struct WorkerOptions {
    pub queues: Vec<(String, usize)>,            // APP_QUEUES / app worker --queues
    pub node: String,                            // kernel::jobs::node_id(): "<hostname>:<pid>"
    pub prune_after: chrono::Duration,           // APP_JOBS_PRUNE_DAYS
    pub shutdown_grace: std::time::Duration,     // APP_JOBS_SHUTDOWN_GRACE
    pub schedule: &'static [Recurring],          // crate::jobs::recurring::SCHEDULE
}
pub async fn run(db: DatabaseConnection, opts: WorkerOptions, dispatch: DispatchFn,
                 config: WorkerConfigFn, shutdown: impl Future<Output = ()>);
```

`run` sends one heartbeat (registers the node, reads paused queues), spawns one **producer per queue** and the plugin loop, then waits for `shutdown` (Ctrl-C / SIGTERM).

Each producer (concurrency `limit`, a `Semaphore` of that size):

1. **Paused?** Skip fetching while the queue is in the paused set (refreshed by the Heartbeat every second). In-flight jobs keep running.
2. **Fetch:** `free = available permits`. Select up to `free` **ids** where `state = 'available' AND queue = ? AND scheduled_at <= now`, ordered by `priority, scheduled_at, id`.
3. **Claim optimistically, one at a time:** `update_many` set `state = 'executing'`, `attempt = attempt + 1`, `attempted_at = now`, `attempted_by = node`, filtered by `id = ? AND state = 'available'`. **The claim succeeded only if `rows_affected == 1`.** Then load the row. Portable; no `SKIP LOCKED`.
4. **Execute** on a `JoinSet` task holding a permit: `tokio::time::timeout(TIMEOUT, AssertUnwindSafe(dispatch(...)).catch_unwind())` (`futures::FutureExt`). Keep the task's `AbortHandle` in a shared `HashMap<i64, AbortHandle>` (cancellation).
5. **Record**, with the update filtered by `id = ? AND state = 'executing' AND attempted_by = node`, so a job cancelled or rescued meanwhile isn't overwritten:

| Result | Update |
|--------|--------|
| `Ok(Done)` | `completed`, `completed_at = now` |
| `Ok(Snooze(d))` | `scheduled`, `scheduled_at = now + d`, `max_attempts += 1`, `meta.snoozed += 1` |
| `Ok(Cancel(reason))` | `cancelled`, `cancelled_at = now`, error `"cancelled: <reason>"` |
| `Ok(Discard(reason))` | `discarded`, `discarded_at = now`, error `<reason>` |
| `Err(e)` / panic / timeout, `attempt < max_attempts` | `retryable`, `scheduled_at = now + backoff(attempt)`, error `format!("{e:#}")` (panic: `"panicked: <payload>"`, timeout: `"timed out after 300s"`, from `{:?}` of the `Duration`) |
| same, `attempt >= max_attempts` | `discarded`, `discarded_at = now`, the error |

   Each error is pushed as `{attempt, at, error}`, **redacted first** (`kernel::redact::text`: an error often quotes the URL that failed, credentials and tokens included; [errors-and-logging](errors-and-logging.md#redacting-stored-error-text)), then capped at 4 KB (start kept) and the last 20 entries. The `job.exception` log line carries the same redacted text. In final states, clear `unique_key` unless `meta.unique_states == "all"`.
6. **Wake-ups:** after a fetch that found nothing, wait for the first of: a local insert (`Notify`), a job finishing, 1s ± 20% jitter, or shutdown. When every permit is taken, wait for a job to finish.

- `default_backoff(attempt) = attempt⁴ + 15s + rand::random_range(0..=30 × attempt)` seconds (rand 0.10: `rand::random_range`). ~16s, ~31s, ~96s, ~271s… 20 attempts cover about three weeks; pick `MAX_ATTEMPTS` per worker (see mailers' `DeliverEmailJob`).


### Observability

One `tracing` span per execution: `info_span!("job", job_id, worker, queue, attempt)`, logging `job.start` (`queue_ms` = attempted_at − scheduled_at), then `job.stop` (`duration_ms`, `outcome`), or `job.exception` (`warn!` when retryable, `error!` when discarded). Plugins log `debug!(plugin, count)` when they changed rows and `error!` when they fail; Lifeline logs `warn!(rescued)`. That's all the telemetry: the dashboard reads the `jobs` table directly.

### Plugins (`plugins.rs`; every node runs all of them; all idempotent; no leader election)

Each is a plain `async fn(db, now, …)` so tests call it directly instead of waiting for a timer. The plugin loop ticks every second.

| Plugin | Every | `fn` | Does |
|--------|-------|------|------|
| **Heartbeat** | 1s | `heartbeat` (crate-private) | Upsert this node's `job_nodes` row (`on_conflict(node).update_columns([queues, heartbeat_at])`). Insert missing `job_queues` rows (`on_conflict_do_nothing_on([name])`). Refresh the paused set. **Cancel check:** of this node's in-flight ids, those no longer `executing` on this node get their `AbortHandle::abort()`ed |
| **Stager** | 1s | `stage(db, now) -> u64` | `scheduled`/`retryable` with `scheduled_at <= now` → `available` |
| **Lifeline** | 30s | `rescue_orphans(db, now) -> u64` | `executing` jobs whose `attempted_by` has no `job_nodes` row with `heartbeat_at > now - 60s`: back to `available` (`scheduled_at = now`) if `attempt < max_attempts`, else `discarded`; error `"orphaned: node <id> stopped"`. Then delete `job_nodes` silent for > 5 min |
| **Pruner** | 60s | `prune(db, now, older_than) -> u64` | Delete final jobs whose `completed_at`/`cancelled_at`/`discarded_at` < `now - older_than`, by selected ids in batches of 10,000 (short SQLite write locks) |
| **Cron** | 30s | `cron_tick(db, schedule, now) -> Vec<i64>` | See below |

### Shutdown

On `shutdown`: stop fetching, wait up to `shutdown_grace` (default 25s; must be below the platform's kill timeout) for in-flight jobs, abort the rest, stop the plugins, delete this node's `job_nodes` row. Aborted jobs stay `executing` on a node that no longer exists; Lifeline (on any node, or on the next boot) puts them back to `available` within 30s. Delivery is at-least-once, by design.

### Postgres-only optimizations (later, behind a backend `match` inside the kernel)

`FOR UPDATE SKIP LOCKED` claims and `LISTEN/NOTIFY` wake-ups. Only when measured. The portable path stays the default and stays tested.

## Recurring jobs (`src/jobs/recurring.rs`), the Oban Cron plugin

```rust
use crate::kernel::jobs::Recurring;   // pub struct Recurring { pub key: &'static str, pub cron: &'static str, pub job: fn() -> JobBuilder }

pub const SCHEDULE: &[Recurring] = &[
    Recurring { key: "cleanup",       cron: "0 * * * *",   job: || CleanupJob.into_job() },
    Recurring { key: "weekly_digest", cron: "0 8 * * MON", job: || FanOutWeeklyDigestJob.into_job().priority(5) },
];
```

`job` is a non-capturing closure, which coerces to `fn() -> JobBuilder`, so `SCHEDULE` stays a `const`. Exactly how a tick works:

1. `run_at = Cron::from_str(cron)?.find_previous_occurrence(&now, true)` (croner 4; UTC). An invalid expression logs an error and is skipped (a test asserts every `SCHEDULE` entry parses).
2. Skip unless `now - run_at <= 60s`. A node booting at 09:40 does **not** replay the 08:00 run.
3. In one transaction: insert `(key, run_at)` into `recurring_executions` with `on_conflict_do_nothing_on([key, run_at])`. If nothing was inserted, another node owns this run: roll back.
4. Otherwise insert `(job)()` with **`.tag("cron")`** (appended to the entry's own tags) and **`.meta(json!({"cron": key}))`**, then commit.

Each run happens exactly once across all nodes. The dashboard's cron page shows each entry's next run (`plugins::next_run`), its last run (latest `recurring_executions.run_at`) and the job it enqueued (the newest job tagged `cron` with `meta.cron == key`).

**Built-in `CleanupJob`** (hourly, generated by `new`) deletes: expired `cache_entries`, `recurring_executions` older than 7 days, and in development `mailbox_messages` beyond the newest 500. Generators that add framework tables add their cleanup here (`sessions`, `password_resets`, `broadcasts`). Job rows are the Pruner's job, not CleanupJob's.

## Queues & configuration

`APP_QUEUES=default:10,mailers:5` sets which queues this process runs, and each queue's concurrency **per node**. `app worker --queues default:10` overrides it per process (dedicated worker roles; see [deployment](deployment.md)). The default is `default:10,mailers:5`.

A job inserted into a queue that no node runs just waits. The dashboard flags such queues as "no nodes".

## Rules

1. **Args hold ids, not records.** `{ "post_id": 42 }`, never a serialized `Post`. Reload in `perform`. If the record is gone, return `Ok(Outcome::Cancel("post 42 deleted".into()))`. Exception: `DeliverEmailJob` carries the rendered email (see [mailers](mailers.md)).
2. **Enqueue inside the transaction** that creates the data the job needs.
3. **Jobs must be idempotent.** At-least-once delivery means a job can run twice (crash after the side effect, before it's marked completed). Guard external side effects with a DB marker (`sent_at`).
4. Use `?` freely, and wrap with `.context("...")` so the dashboard's error history explains itself.
5. Return `Discard` only when retrying can't help (bad input). Return `Cancel` when the work no longer matters. Return `Snooze` for "not yet" (rate limited by a provider, waiting on another record).
6. **Size `MAX_ATTEMPTS` to the work.** The default (20, ~3 weeks) suits syncs and imports. Transactional mail must expire instead: `DeliverEmailJob` uses 10; `mailers::deliver_later_until` caps at 5 and discards late mail; token emails use a dedicated job with 5 attempts that discards stale requests.
7. **No secrets in args.** Args are shown in `/admin/jobs` and `app jobs show`. Pass ids and let the worker create or load secrets (tokens are minted in the job, and only their digest is stored; see [auth](auth.md)).
8. Long jobs: break them into batches that enqueue the next batch, or a fan-out job that `insert_all`s one job per item. No job should run longer than its `TIMEOUT` (5 min default).
9. Tag jobs with what an operator will search for (`user:42`, `import:7`).

## Gotchas agents get wrong

- Forgetting to add a new worker to `dispatch` **and** `worker_config`. The job is discarded as "unknown worker". The generator recipe makes it a checklist step.
- Using `tokio::spawn` for "background work" in a handler. That isn't durable. Use a job.
- Doing the claim as SELECT-then-UPDATE without checking `rows_affected`, which double-runs jobs.
- Recording a result without filtering by `state = 'executing' AND attempted_by = node`, which resurrects cancelled or rescued jobs.
- Comparing `scheduled_at` with a DB-side `now()`. Pass `Utc::now()` from Rust.
- Expecting `DbErr::RecordNotInserted` from `on_conflict_do_nothing_on`. sea-orm 2 returns `TryInsertResult::Conflicted` (or, with `exec_without_returning`, `Inserted(0)` on some backends: treat both as "not inserted").
- `job::ActiveModel::default()` is ambiguous in sea-orm 2 (`ActiveModelTrait::default` vs `Default::default`). Write `<job::ActiveModel as Default>::default()` or `job::ActiveModel { ..Default::default() }`.
- `select().offset(n)` without `.limit(..)` is a **syntax error on SQLite**. Find a cutoff row with `offset(n).limit(1)` instead.
- Text search over `args` must search values, not JSON fragments: Postgres `jsonb` reformats (`{"n": 1}` with a space). Search `LOWER(CAST(args AS text)) LIKE '%needle%'` (sea-query: `Func::lower(Func::cast_as(Expr::col(..), "text"))`) with `LikeExpr::new(..).escape('\\')`.
- Deleting completed jobs on success (the Solid Queue habit). Keep them; the Pruner owns deletion.
- Running Stager/Pruner "only on one node" with leader election. Every plugin is idempotent, so let all nodes run them.

## Testing (Oban.Testing equivalents)

Tests run in **manual mode**: inserts write rows, and nothing executes until the test says so. There's no inline mode, because enqueueing happens inside the caller's transaction. Running the job there would execute it before the commit, which breaks the after-commit guarantee that production relies on.

| Helper on `TestApp` | Does |
|---------------------|------|
| `app.drain_jobs().await -> Drained` | Runs every **due** job in all queues through the real kernel path (claim → dispatch → record), repeatedly until none are due, so jobs that enqueue jobs are covered. `Drained { completed, snoozed, cancelled, discarded, retryable }` + `.total()` |
| `app.drain_queue("mailers").await` | The same, for one queue |
| `app.drain_all_jobs().await` | Also runs `scheduled` jobs as if their time had come (a job that snoozes runs once per drain, so it always ends; `retryable` jobs are never forced) |
| `app.assert_enqueued::<W>(\|w\| w.user_id == 1).await` | Panics, listing what *is* enqueued, unless an `available`/`scheduled` `W` matches |
| `app.refute_enqueued::<W>().await` | Panics if any `available`/`scheduled` `W` exists |
| `app.enqueued::<W>().await -> Vec<(job::Model, W)>` | Rows + decoded args, for assertions on schedule, tags or priority |
| `app.perform_job(w).await -> anyhow::Result<Outcome>` | Round-trips `w` through JSON (catching serde mistakes), then calls `perform` with a synthetic `JobInfo`. Unit-tests one worker |
| `wait_until("what", async \|\| { … }).await` (support.rs, `f: impl AsyncFn() -> bool`) | Awaits a condition (polled every 20ms, 10s deadline). The only way runtime tests wait; never `sleep` |

Tests assert on **outcomes** (email sent, row changed). Assert on the queue only when scheduling is the spec ("digest is sent at 8am", "retry in an hour").

**Kernel tests** (`tests/app/kernel_jobs_test.rs`, generated with the kernel, run on both backends) use their own `DispatchFn` (workers named `ok`, `fail`, `snooze`, `cancel`, `discard`, `panic`, `hang`), so they don't depend on app workers. Required: racing claims run each job exactly once (4 concurrent drains over 30 jobs); retry with backoff then discard at max attempts; snooze; cancel/discard/panic/timeout outcomes; unknown worker and bad args; error caps; errors stored redacted (`job_errors_are_stored_redacted`: a worker failing with `https://deploy:not-a-password@api.example.com/v1/charges?token=not-a-token` stores neither secret, keeps the host and the cause readable, and an `error` search for `not-a-password` finds nothing); uniqueness (args, args_keys, worker, `All`, period); `insert_all`; Lifeline rescues only dead nodes' jobs; Pruner keeps recent and non-final jobs; cron fires once across two concurrent ticks and skips stale due times; **the runtime**: a worker runs a job and deregisters on shutdown, cancelling an executing job aborts it (a drop guard proves the future was dropped), a paused queue isn't fetched until resumed, shutdown leaves unfinished jobs to Lifeline; and the admin API. To simulate time passing, update the job's `scheduled_at` into the past, then call `stage(db, Utc::now())`. Don't stage "in the future", because fetch also requires `scheduled_at <= now`.
