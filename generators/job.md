# Generator: `job` (`rails generate job`) and `task`

> **TL;DR**
> - `cargo offrails g job SendWeeklyDigest user_id:bigint [--queue mailers]` writes `src/jobs/send_weekly_digest.rs` (a `Worker` with id-only args, `MAX_ATTEMPTS = 5`), its arms in `dispatch`, `worker_config` and `WORKERS`, and `tests/app/send_weekly_digest_job_test.rs`; it needs the `jobs` part (`cargo offrails add jobs`). Priority, uniqueness and cron (steps 1 and 7) are yours, in that file and `src/jobs/recurring.rs`.
> - Choose outcomes deliberately (`Done`, `Err` → retry, `Snooze`, `Discard`, `Cancel`); make jobs idempotent; enqueue from the model fn in the same transaction.
> - A new queue goes into `APP_QUEUES`, `.env.example` and every worker role; `--every` adds a `Recurring` entry.
>
> Read the rest when adding a job or a task (`app task <name>`).

## Job

```
cargo offrails g job SendWeeklyDigest user_id:bigint --queue mailers
# by hand after it: PRIORITY, unique(), a Recurring entry for "0 8 * * MON", the perform body
```

1. `src/jobs/<verb>_<noun>.rs`: a `#[derive(Debug, Clone, Serialize, Deserialize)] pub struct SendWeeklyDigestJob { pub user_id: i64 }` with `impl Worker` (`NAME = "send_weekly_digest"`, plus `QUEUE`/`PRIORITY`/`MAX_ATTEMPTS`/`TIMEOUT` only if they differ from the defaults, and `fn unique() -> Option<Unique> { Some(Unique::args_keys(&["user_id"]).period(chrono::Duration::hours(24))) }` for `--unique user_id:24h`). Size `MAX_ATTEMPTS` to the work: the default 20 (~3 weeks) is for syncs and imports, not for anything time-limited. See [guides/jobs.md](../guides/jobs.md).
2. **Args = ids, not records.** Load fresh data in `perform`. If the record is gone, return `Ok(Outcome::Cancel(...))`.
3. **Pick the outcome deliberately:** `Done` normally; `Err` (via `?`) for transient failures you want retried; `Snooze` for "not yet"; `Discard` for inputs that will never work. Add `.context(...)` to errors so the dashboard's error history explains them.
4. **Idempotent:** a job may run twice (crash after the side effect, before completion). Guard side effects (for example, record `sent_at` and skip if it's set).
5. Register it in `dispatch` **and** `worker_config` in `src/jobs/mod.rs` (`pub mod` + `pub use` too), and in `WORKERS`. The generator does this.
6. A new queue (`--queue digests`) must be added to the `APP_QUEUES` default in `config.rs`, to `.env.example`, and to every worker role in the deploy config. Otherwise its jobs wait forever (the dashboard shows "no nodes").
7. `--every`: add `Recurring { key: "weekly_digest", cron: "0 8 * * MON", job: || FanOutWeeklyDigestJob.into_job() }` to `SCHEDULE` in `src/jobs/recurring.rs` (unique key, UTC, croner syntax). The Cron plugin tags the job `cron` and sets `meta.cron = key`; don't add those yourself. A recurring job that fans out over many records is a `FanOut…Job` that `kernel::jobs::insert_all`s one `JobBuilder` per record.
8. Enqueue from the owning model fn, in the same transaction as the data change. Add tags an operator would search for (`user:{id}`).
9. Tests: trigger through the model fn and `drain_jobs()`, then assert the effect. Test the record-gone path with `perform_job` (expect `Outcome::Cancel`). If the spec is about timing ("sent at 8am", "within an hour"), assert it with `assert_enqueued::<W>(|w| …)` / `enqueued::<W>()` (rows + decoded args). Never `sleep` in a test; runtime tests use `wait_until`.

## Tasks

```
task backfill_slugs
```

1. `src/tasks/backfill_slugs.rs` with `pub async fn run(state: &AppState, args: Vec<String>) -> anyhow::Result<()>`.
2. Register it in the `match` in `src/tasks/mod.rs`.
3. Batch work with keyset pagination (1000 rows per batch). Log progress with `tracing::info!`. Make it idempotent and safe to re-run.
4. Destructive tasks take `--dry-run` by default and need `--execute` to write.
5. Run it locally against a seeded DB first, then in production via the deploy platform's one-liner ([deployment](../guides/deployment.md)).
6. If it touches more than ~10k rows, or calls external services, make it a job fan-out instead, so progress and failures are visible in `/admin/jobs`.
