//! Queries and actions shared by `app jobs` and the `/admin/jobs` dashboard, so the browser and
//! the terminal can't disagree.
//! GUIDE: .offrails/guides/jobs-dashboard.md#kerneljobsadmin-the-api
//!
//! SECURITY: every filter value is bound as a parameter by the query builder; nothing here
//! formats user input into SQL (test: filters_and_search_are_parameterized_against_injection).

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Timelike, Utc};
use sea_orm::{
    ActiveEnum,
    ActiveValue::Set,
    ColumnTrait, Condition, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
    sea_query::{Alias, Expr, ExprTrait, Func, LikeExpr, OnConflict},
};
use serde::Serialize;
use serde_json::Value;

use super::{
    JobState,
    executor::{AttemptError, push_error, releases_unique},
    job, job_batch, job_node, job_queue,
    plugins::{self, NODE_ALIVE},
    recurring_execution, wake,
};

/// Most jobs a "matching" bulk action touches at once.
pub const BULK_CAP: u64 = 10_000;
/// Ids per statement in bulk updates (SQLite's bind limit is far above this).
const ID_CHUNK: usize = 500;
/// The most rows the metrics read in one window (beyond this, sample counts are lower bounds).
const METRICS_ROWS: u64 = 100_000;

// ---------------------------------------------------------------------------
// Counts, queues, nodes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    /// All seven states, in operator order, zeros included.
    pub by_state: Vec<(JobState, u64)>,
    pub by_queue: BTreeMap<String, HashMap<JobState, u64>>,
}

impl Stats {
    pub fn count(&self, state: JobState) -> u64 {
        self.by_state
            .iter()
            .find(|(s, _)| *s == state)
            .map_or(0, |(_, n)| *n)
    }

    pub fn total(&self) -> u64 {
        self.by_state.iter().map(|(_, n)| n).sum()
    }
}

/// One `GROUP BY queue, state` query.
pub async fn stats(db: &DatabaseConnection) -> Result<Stats, DbErr> {
    let rows: Vec<(String, JobState, i64)> = job::Entity::find()
        .select_only()
        .column(job::Column::Queue)
        .column(job::Column::State)
        .column_as(Expr::expr(Func::count(Expr::col(job::Column::Id))), "n")
        .group_by(job::Column::Queue)
        .group_by(job::Column::State)
        .into_tuple()
        .all(db)
        .await?;
    let mut totals: HashMap<JobState, u64> = HashMap::new();
    let mut by_queue: BTreeMap<String, HashMap<JobState, u64>> = BTreeMap::new();
    for (queue, state, n) in rows {
        *totals.entry(state).or_default() += n as u64;
        *by_queue.entry(queue).or_default().entry(state).or_default() += n as u64;
    }
    let by_state = JobState::ALL
        .into_iter()
        .map(|s| (s, totals.get(&s).copied().unwrap_or(0)))
        .collect();
    Ok(Stats { by_state, by_queue })
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueRow {
    pub name: String,
    pub paused: bool,
    /// Sum of the limits over live nodes.
    pub limit: usize,
    pub nodes: usize,
    /// The rate limit a live node runs it with (`"60/1m"`).
    pub rate: Option<String>,
    pub executing: u64,
    pub available: u64,
    pub scheduled: u64,
    pub retryable: u64,
    pub discarded: u64,
    pub completed: u64,
}

impl QueueRow {
    /// No live node runs this queue: its jobs just wait.
    pub fn orphaned(&self) -> bool {
        self.nodes == 0
    }
}

/// `job_queues` ∪ queues seen in `jobs` ∪ live nodes' queues.
pub async fn queues(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<Vec<QueueRow>, DbErr> {
    let stats = stats(db).await?;
    let paused: HashMap<String, bool> = job_queue::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|q| (q.name, q.paused))
        .collect();
    let live: Vec<NodeRow> = nodes(db, now)
        .await?
        .into_iter()
        .filter(|n| n.alive)
        .collect();

    let mut names: BTreeMap<String, ()> = BTreeMap::new();
    names.extend(paused.keys().map(|k| (k.clone(), ())));
    names.extend(stats.by_queue.keys().map(|k| (k.clone(), ())));
    for node in &live {
        names.extend(node.queues.iter().map(|q| (q.name.clone(), ())));
    }

    Ok(names
        .into_keys()
        .map(|name| {
            let counts = stats.by_queue.get(&name);
            let n = |s: JobState| counts.and_then(|c| c.get(&s)).copied().unwrap_or(0);
            let (mut limit, mut node_count, mut rate) = (0, 0, None);
            for node in &live {
                if let Some(q) = node.queues.iter().find(|q| q.name == name) {
                    limit += q.limit;
                    node_count += 1;
                    rate = rate.or_else(|| q.rate.clone());
                }
            }
            QueueRow {
                paused: paused.get(&name).copied().unwrap_or(false),
                limit,
                nodes: node_count,
                rate,
                executing: n(JobState::Executing),
                available: n(JobState::Available),
                scheduled: n(JobState::Scheduled),
                retryable: n(JobState::Retryable),
                discarded: n(JobState::Discarded),
                completed: n(JobState::Completed),
                name,
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeQueue {
    pub name: String,
    pub limit: usize,
    pub rate: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeRow {
    pub node: String,
    pub queues: Vec<NodeQueue>,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    /// Jobs executing on this node right now.
    pub executing: u64,
    pub alive: bool,
}

pub async fn nodes(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<Vec<NodeRow>, DbErr> {
    let executing: Vec<(Option<String>, i64)> = job::Entity::find()
        .select_only()
        .column(job::Column::AttemptedBy)
        .column_as(Expr::expr(Func::count(Expr::col(job::Column::Id))), "n")
        .filter(job::Column::State.eq(JobState::Executing))
        .group_by(job::Column::AttemptedBy)
        .into_tuple()
        .all(db)
        .await?;
    let executing: HashMap<String, u64> = executing
        .into_iter()
        .filter_map(|(node, n)| node.map(|node| (node, n as u64)))
        .collect();

    let mut rows: Vec<NodeRow> = job_node::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|n| {
            let mut queues: Vec<NodeQueue> = n
                .queues
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(name, v)| NodeQueue {
                            name: name.clone(),
                            // Older rows stored a bare number.
                            limit: v
                                .get("limit")
                                .and_then(Value::as_u64)
                                .or_else(|| v.as_u64())
                                .unwrap_or(0) as usize,
                            rate: v.get("rate").and_then(Value::as_str).map(str::to_owned),
                        })
                        .collect()
                })
                .unwrap_or_default();
            queues.sort_by(|a, b| a.name.cmp(&b.name));
            NodeRow {
                alive: now - n.heartbeat_at < NODE_ALIVE,
                executing: executing.get(&n.node).copied().unwrap_or(0),
                node: n.node,
                queues,
                started_at: n.started_at,
                heartbeat_at: n.heartbeat_at,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.alive.cmp(&a.alive).then(a.node.cmp(&b.node)));
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Metrics: throughput, queue latency, per-worker success
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub minute: DateTime<Utc>,
    pub completed: u64,
    /// Failed attempts: discarded, plus retryable jobs whose last attempt was in this minute.
    pub failed: u64,
    /// Queue wait (attempted_at − scheduled_at) of the jobs started in this minute.
    pub wait_p50_ms: Option<i64>,
    pub wait_p95_ms: Option<i64>,
}

/// The `p` (0-100) percentile of a sorted slice (nearest rank).
pub fn percentile(sorted: &[i64], p: u32) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((f64::from(p) / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// The last `minutes`: one bucket per minute, and queue wait over the whole window.
#[derive(Debug, Clone, Serialize)]
pub struct Throughput {
    pub buckets: Vec<Bucket>,
    /// Queue wait of every job started in the window (not of one minute), `None` when none did.
    pub wait_p50_ms: Option<i64>,
    pub wait_p95_ms: Option<i64>,
}

/// Per-minute completed/failed counts and queue-wait p50/p95 for the last `minutes`, and the
/// window's own wait percentiles. Selects only timestamps and buckets in Rust: portable, fine up
/// to ~100k jobs per window.
pub async fn throughput(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    minutes: i64,
) -> Result<Throughput, DbErr> {
    let minutes = minutes.clamp(1, 24 * 60);
    let end = now
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(now);
    let start = end - chrono::Duration::minutes(minutes - 1);
    let index = |t: DateTime<Utc>| -> Option<usize> {
        let m = (t - start).num_seconds().div_euclid(60);
        (0..minutes).contains(&m).then_some(m as usize)
    };
    let mut completed = vec![0_u64; minutes as usize];
    let mut failed = vec![0_u64; minutes as usize];
    let mut waits: Vec<Vec<i64>> = vec![Vec::new(); minutes as usize];

    let done: Vec<Option<DateTime<Utc>>> = job::Entity::find()
        .select_only()
        .column(job::Column::CompletedAt)
        .filter(job::Column::CompletedAt.gte(start))
        .limit(METRICS_ROWS)
        .into_tuple()
        .all(db)
        .await?;
    for t in done.into_iter().flatten() {
        if let Some(i) = index(t) {
            completed[i] += 1;
        }
    }
    let discarded: Vec<Option<DateTime<Utc>>> = job::Entity::find()
        .select_only()
        .column(job::Column::DiscardedAt)
        .filter(job::Column::DiscardedAt.gte(start))
        .limit(METRICS_ROWS)
        .into_tuple()
        .all(db)
        .await?;
    let retried: Vec<Option<DateTime<Utc>>> = job::Entity::find()
        .select_only()
        .column(job::Column::AttemptedAt)
        .filter(job::Column::State.eq(JobState::Retryable))
        .filter(job::Column::AttemptedAt.gte(start))
        .limit(METRICS_ROWS)
        .into_tuple()
        .all(db)
        .await?;
    for t in discarded.into_iter().chain(retried).flatten() {
        if let Some(i) = index(t) {
            failed[i] += 1;
        }
    }
    let started: Vec<(Option<DateTime<Utc>>, DateTime<Utc>)> = job::Entity::find()
        .select_only()
        .column(job::Column::AttemptedAt)
        .column(job::Column::ScheduledAt)
        .filter(job::Column::AttemptedAt.gte(start))
        .limit(METRICS_ROWS)
        .into_tuple()
        .all(db)
        .await?;
    for (attempted, scheduled) in started {
        if let Some(attempted) = attempted
            && let Some(i) = index(attempted)
        {
            waits[i].push(std::cmp::max((attempted - scheduled).num_milliseconds(), 0));
        }
    }
    let mut all: Vec<i64> = waits.iter().flatten().copied().collect();
    all.sort_unstable();
    let buckets = (0..minutes as usize)
        .map(|i| {
            waits[i].sort_unstable();
            Bucket {
                minute: start + chrono::Duration::minutes(i as i64),
                completed: completed[i],
                failed: failed[i],
                wait_p50_ms: percentile(&waits[i], 50),
                wait_p95_ms: percentile(&waits[i], 95),
            }
        })
        .collect();
    Ok(Throughput {
        buckets,
        // INVARIANT: a summary of the window is computed over the window's values, never taken
        // from one minute (test: chart_legends_summarize_the_whole_window)
        wait_p50_ms: percentile(&all, 50),
        wait_p95_ms: percentile(&all, 95),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerStat {
    pub worker: String,
    pub completed: u64,
    pub failed: u64,
    /// completed / (completed + failed), as 0-100.
    pub success_pct: Option<f64>,
    pub run_p50_ms: Option<i64>,
    pub run_p95_ms: Option<i64>,
    pub wait_p95_ms: Option<i64>,
}

/// Per-worker outcomes and run times over `[now - window, now]`, busiest first.
pub async fn worker_stats(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    window: chrono::Duration,
) -> Result<Vec<WorkerStat>, DbErr> {
    let start = now - window;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        JobState,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
    )> = job::Entity::find()
        .select_only()
        .column(job::Column::Worker)
        .column(job::Column::State)
        .column(job::Column::AttemptedAt)
        .column(job::Column::ScheduledAt)
        .column(job::Column::CompletedAt)
        .filter(job::Column::AttemptedAt.gte(start))
        .filter(job::Column::State.is_in([
            JobState::Completed,
            JobState::Discarded,
            JobState::Retryable,
        ]))
        .limit(METRICS_ROWS)
        .into_tuple()
        .all(db)
        .await?;
    #[derive(Default)]
    struct Acc {
        completed: u64,
        failed: u64,
        runs: Vec<i64>,
        waits: Vec<i64>,
    }
    let mut by_worker: BTreeMap<String, Acc> = BTreeMap::new();
    for (worker, state, attempted, scheduled, completed) in rows {
        let acc = by_worker.entry(worker).or_default();
        if let Some(a) = attempted {
            acc.waits
                .push(std::cmp::max((a - scheduled).num_milliseconds(), 0));
        }
        match state {
            JobState::Completed => {
                acc.completed += 1;
                if let (Some(a), Some(c)) = (attempted, completed) {
                    acc.runs.push(std::cmp::max((c - a).num_milliseconds(), 0));
                }
            }
            _ => acc.failed += 1,
        }
    }
    let mut out: Vec<WorkerStat> = by_worker
        .into_iter()
        .map(|(worker, mut acc)| {
            acc.runs.sort_unstable();
            acc.waits.sort_unstable();
            let total = acc.completed + acc.failed;
            WorkerStat {
                worker,
                completed: acc.completed,
                failed: acc.failed,
                success_pct: (total > 0).then(|| acc.completed as f64 * 100.0 / total as f64),
                run_p50_ms: percentile(&acc.runs, 50),
                run_p95_ms: percentile(&acc.runs, 95),
                wait_p95_ms: percentile(&acc.waits, 95),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        (b.completed + b.failed)
            .cmp(&(a.completed + a.failed))
            .then(a.worker.cmp(&b.worker))
    });
    Ok(out)
}

/// Jobs executing right now, longest-running first (the "now running" lane).
pub async fn running(db: &DatabaseConnection, limit: u64) -> Result<Vec<job::Model>, DbErr> {
    job::Entity::find()
        .filter(job::Column::State.eq(JobState::Executing))
        .order_by_asc(job::Column::AttemptedAt)
        .order_by_asc(job::Column::Id)
        .limit(limit)
        .all(db)
        .await
}

// ---------------------------------------------------------------------------
// Error groups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ErrorGroup {
    pub worker: String,
    /// The root cause and the host it involved: `connection refused (os error N) · hooks.example.com`.
    pub message: String,
    /// The innermost error of the chain, numbers replaced by `N`.
    pub root: String,
    /// The host or URL host the error names, when it names one.
    pub host: Option<String>,
    /// What differs between the group's errors in their outer context (`order.paid`,
    /// `order.refunded`), at most `MAX_VARIANTS`, in first-seen order.
    pub variants: Vec<String>,
    /// How many distinct outer contexts the group has (may exceed `variants.len()`).
    pub variant_count: usize,
    pub count: u64,
    pub retryable: u64,
    pub discarded: u64,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    /// The newest job in the group.
    pub example_id: i64,
    /// A literal fragment of the message for the list's `error` filter, when one is usable.
    pub search: Option<String>,
}

/// Variants listed per error group; the count says how many more there are.
pub const MAX_VARIANTS: usize = 8;

/// `(outer context, innermost cause)` of an error chain as `{e:#}` prints it
/// (`"POST …: connecting to …: connection refused"`). Without a chain, the context is empty.
pub fn split_root_cause(message: &str) -> (&str, &str) {
    let line = message.lines().next().unwrap_or_default().trim();
    match line.rfind(": ") {
        Some(at) if at + 2 < line.len() => (&line[..at], &line[at + 2..]),
        _ => ("", line),
    }
}

/// The host an error names: the host of its first URL, else the first `name.tld:port` token.
pub fn error_host(message: &str) -> Option<String> {
    let line = message.lines().next().unwrap_or_default();
    if let Some(at) = line.find("://") {
        let host: String = line[at + 3..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            .collect();
        if host.contains('.') {
            return Some(host.to_lowercase());
        }
    }
    line.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '"' | '\''))
        .filter_map(|token| {
            let (host, port) = token.trim_end_matches(':').rsplit_once(':')?;
            (host.contains('.')
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
            .then(|| host.to_lowercase())
        })
        .next()
}

/// The part of each string that differs from the others (their common prefix and suffix cut
/// at word boundaries): `["… (order.paid): x", "… (order.refunded): x"]` → `["order.paid",
/// "order.refunded"]`. Empty strings stay empty.
pub fn distinct_parts(values: &[String]) -> Vec<String> {
    if values.len() < 2 {
        return values.to_vec();
    }
    let word = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-');
    let chars: Vec<Vec<char>> = values.iter().map(|v| v.chars().collect()).collect();
    let shortest = chars.iter().map(Vec::len).min().unwrap_or(0);
    let mut prefix = 0;
    while prefix < shortest && chars.iter().all(|c| c[prefix] == chars[0][prefix]) {
        prefix += 1;
    }
    // Back to a word boundary, so "order.paid" / "order.refunded" keep "order.".
    while prefix > 0 && word(chars[0][prefix - 1]) {
        prefix -= 1;
    }
    let mut suffix = 0;
    while suffix < shortest - prefix
        && chars
            .iter()
            .all(|c| c[c.len() - 1 - suffix] == chars[0][chars[0].len() - 1 - suffix])
    {
        suffix += 1;
    }
    while suffix > 0 && word(chars[0][chars[0].len() - suffix]) {
        suffix -= 1;
    }
    chars
        .iter()
        .map(|c| {
            let part: String = c[prefix..c.len() - suffix].iter().collect();
            part.trim().to_owned()
        })
        .collect()
}

/// First line, numbers → `N`, at most 160 characters: "same error, different ids".
/// Digits inside a word (`v2`, `utf8`) and HTTP status codes (`HTTP 503`) are kept, so a 503
/// and a 429 stay two groups.
pub fn error_signature(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !c.is_ascii_digit() {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let in_word = start > 0 && chars[start - 1].is_ascii_alphabetic();
        let status = run.len() == 3 && out.ends_with("HTTP ");
        if in_word || status {
            out.push_str(&run);
        } else {
            out.push('N');
        }
    }
    super::executor::truncate(out.trim(), 160)
}

/// The longest literal run of `message` that every error of its group shares: it stops at
/// numbers the signature replaces by `N` (ids vary; in-word digits and HTTP statuses don't) and
/// at characters the stored JSON escapes (quotes, backslashes, control characters).
fn search_fragment(message: &str) -> Option<String> {
    let chars: Vec<char> = message.lines().next().unwrap_or_default().chars().collect();
    let mut runs: Vec<String> = vec![String::new()];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '"' | '\\') || c.is_control() {
            runs.push(String::new());
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            let current = runs.last_mut().map(|r| r.as_str()).unwrap_or_default();
            let in_word = start > 0 && chars[start - 1].is_ascii_alphabetic();
            let status = run.len() == 3 && current.ends_with("HTTP ");
            if in_word || status {
                if let Some(r) = runs.last_mut() {
                    r.push_str(&run);
                }
            } else {
                runs.push(String::new());
            }
            continue;
        }
        if let Some(r) = runs.last_mut() {
            r.push(c);
        }
        i += 1;
    }
    let best = runs
        .into_iter()
        .map(|r| r.trim().chars().take(80).collect::<String>())
        .max_by_key(|r| r.chars().count())?;
    (best.chars().count() >= 6).then_some(best)
}

/// Failing jobs (retryable and discarded) grouped by worker and the **root cause** of their last
/// error (the innermost error of the chain, numbers ignored, plus the host it names), largest
/// group first. The outer context (which request, which record) becomes the group's variants,
/// so one outage reads as one group. Reads the newest `limit` failing jobs.
/// INVARIANT: the same root cause with different outer contexts is one group
/// (test: one_root_cause_is_one_group_with_its_variants)
pub async fn error_groups(db: &DatabaseConnection, limit: u64) -> Result<Vec<ErrorGroup>, DbErr> {
    let rows: Vec<(i64, String, JobState, Value)> = job::Entity::find()
        .select_only()
        .column(job::Column::Id)
        .column(job::Column::Worker)
        .column(job::Column::State)
        .column(job::Column::Errors)
        .filter(job::Column::State.is_in([JobState::Retryable, JobState::Discarded]))
        .order_by_desc(job::Column::Id)
        .limit(limit)
        .into_tuple()
        .all(db)
        .await?;
    let mut groups: BTreeMap<(String, String), ErrorGroup> = BTreeMap::new();
    let mut contexts: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (id, worker, state, errors) in rows {
        let last = errors.as_array().and_then(|a| a.last());
        let message = last
            .and_then(|e| e.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("(no error recorded)");
        let at = last
            .and_then(|e| e.get("at"))
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        let (context, cause) = split_root_cause(message);
        let root = error_signature(cause);
        let host = error_host(message);
        let key = format!("{root}\u{1f}{}", host.as_deref().unwrap_or_default());
        let context = error_signature(context);
        let group = groups
            .entry((worker.clone(), key))
            .or_insert_with(|| ErrorGroup {
                worker,
                message: match &host {
                    Some(h) => format!("{root} · {h}"),
                    None => root.clone(),
                },
                root,
                host,
                variants: Vec::new(),
                variant_count: 0,
                count: 0,
                retryable: 0,
                discarded: 0,
                first_seen: None,
                last_seen: None,
                example_id: id,
                search: search_fragment(cause),
            });
        contexts
            .entry((group.worker.clone(), group.message.clone()))
            .or_default()
            .push(context);
        group.count += 1;
        match state {
            JobState::Retryable => group.retryable += 1,
            _ => group.discarded += 1,
        }
        if let Some(at) = at {
            group.first_seen = Some(group.first_seen.map_or(at, |f| std::cmp::min(f, at)));
            group.last_seen = Some(group.last_seen.map_or(at, |l| std::cmp::max(l, at)));
        }
    }
    let mut out: Vec<ErrorGroup> = groups.into_values().collect();
    for group in &mut out {
        let mut distinct: Vec<String> = Vec::new();
        for c in contexts
            .remove(&(group.worker.clone(), group.message.clone()))
            .unwrap_or_default()
        {
            if !distinct.contains(&c) {
                distinct.push(c);
            }
        }
        group.variant_count = if distinct.len() > 1 {
            distinct.len()
        } else {
            0
        };
        if group.variant_count > 0 {
            let shown: Vec<String> = distinct.into_iter().take(MAX_VARIANTS).collect();
            group.variants = distinct_parts(&shown)
                .into_iter()
                .map(|v| super::executor::truncate(&v, 60))
                .collect();
        }
    }
    out.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_seen.cmp(&a.last_seen)));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Listing and search
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct JobFilter {
    pub state: Option<JobState>,
    pub queue: Option<String>,
    pub worker: Option<String>,
    pub tag: Option<String>,
    /// An id (all digits), or text in the worker or queue name, or in the args as every view
    /// shows them (redacted: a secret's value is never searchable).
    pub q: Option<String>,
    /// Text found in the recorded errors.
    pub error: Option<String>,
}

impl JobFilter {
    /// A bulk action on an empty filter would touch everything: those are refused.
    pub fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.queue.is_none()
            && self.worker.is_none()
            && self.tag.is_none()
            && self.q.as_deref().is_none_or(|q| q.trim().is_empty())
            && self.error.as_deref().is_none_or(|q| q.trim().is_empty())
    }
}

/// `%…%`, lowercased, with `\`, `%` and `_` escaped for `LIKE … ESCAPE '\'`.
/// SECURITY: the pattern is a bound value, and its wildcards are escaped, so a search for
/// `%` or `_` matches those characters instead of everything
/// (test: filters_and_search_are_parameterized_against_injection).
pub fn like_pattern(needle: &str) -> String {
    let mut out = String::from("%");
    for c in needle.to_lowercase().chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// `LOWER(CAST(col AS text)) LIKE $1 ESCAPE '\'`, portable on both backends.
fn text_like(col: job::Column, pattern: String) -> sea_orm::sea_query::SimpleExpr {
    Expr::expr(Func::lower(Func::cast_as(
        Expr::col(col),
        Alias::new("text"),
    )))
    .like(LikeExpr::new(pattern).escape('\\'))
}

/// How many of the newest jobs whose raw args contain the search text are checked again on their
/// redacted args. APP: raise it if your operators search far back.
const ARGS_SEARCH_SCAN: u64 = 5_000;

/// Ids of jobs whose **redacted** args contain `q` (case-insensitive), among the newest
/// `ARGS_SEARCH_SCAN` whose raw args do (the SQL narrows, the redacted copy decides).
// SECURITY: matching raw args would let a search probe a redacted value character by character
// ("does any job's api_key start with sk_live_a?"): only what the views show is searchable
// (test: search_never_matches_redacted_values)
async fn ids_with_text_in_redacted_args(
    db: &DatabaseConnection,
    q: &str,
) -> Result<Vec<i64>, DbErr> {
    let rows: Vec<(i64, serde_json::Value)> = job::Entity::find()
        .select_only()
        .column(job::Column::Id)
        .column(job::Column::Args)
        .filter(text_like(job::Column::Args, like_pattern(q)))
        .order_by_desc(job::Column::Id)
        .limit(ARGS_SEARCH_SCAN)
        .into_tuple()
        .all(db)
        .await?;
    let needle = q.to_lowercase();
    Ok(rows
        .into_iter()
        .filter(|(_, args)| {
            super::redact(args)
                .to_string()
                .to_lowercase()
                .contains(&needle)
        })
        .map(|(id, _)| id)
        .collect())
}

async fn condition(db: &DatabaseConnection, filter: &JobFilter) -> Result<Condition, DbErr> {
    let mut cond = Condition::all();
    if let Some(state) = filter.state {
        cond = cond.add(job::Column::State.eq(state));
    }
    if let Some(queue) = &filter.queue {
        cond = cond.add(job::Column::Queue.eq(queue.as_str()));
    }
    if let Some(worker) = &filter.worker {
        cond = cond.add(job::Column::Worker.eq(worker.as_str()));
    }
    if let Some(tag) = filter
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        // The quoted element, so `user:4` doesn't match `user:42`.
        cond = cond.add(text_like(
            job::Column::Tags,
            like_pattern(&format!("\"{tag}\"")),
        ));
    }
    if let Some(q) = filter.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        match q.parse::<i64>() {
            Ok(id) if q.chars().all(|c| c.is_ascii_digit()) => {
                cond = cond.add(job::Column::Id.eq(id));
            }
            _ => {
                // WHY: values, not JSON fragments: Postgres may re-render JSON with spaces.
                let in_args = ids_with_text_in_redacted_args(db, q).await?;
                cond = cond.add(
                    Condition::any()
                        .add(job::Column::Id.is_in(in_args))
                        .add(text_like(job::Column::Worker, like_pattern(q)))
                        .add(text_like(job::Column::Queue, like_pattern(q))),
                );
            }
        }
    }
    if let Some(e) = filter
        .error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        cond = cond.add(text_like(job::Column::Errors, like_pattern(e)));
    }
    Ok(cond)
}

/// Newest first, keyset paginated (`id < before_id`).
pub async fn list(
    db: &DatabaseConnection,
    filter: &JobFilter,
    before_id: Option<i64>,
    limit: u64,
) -> Result<Vec<job::Model>, DbErr> {
    let mut query = job::Entity::find().filter(condition(db, filter).await?);
    if let Some(before) = before_id {
        query = query.filter(job::Column::Id.lt(before));
    }
    query
        .order_by_desc(job::Column::Id)
        .limit(limit.clamp(1, 500))
        .all(db)
        .await
}

pub async fn count_matching(db: &DatabaseConnection, filter: &JobFilter) -> Result<u64, DbErr> {
    job::Entity::find()
        .filter(condition(db, filter).await?)
        .count(db)
        .await
}

/// Counts per state for a filter (ignoring its own state): the list's state tabs.
pub async fn state_counts(
    db: &DatabaseConnection,
    filter: &JobFilter,
) -> Result<HashMap<JobState, u64>, DbErr> {
    let without_state = JobFilter {
        state: None,
        ..filter.clone()
    };
    let rows: Vec<(JobState, i64)> = job::Entity::find()
        .select_only()
        .column(job::Column::State)
        .column_as(Expr::expr(Func::count(Expr::col(job::Column::Id))), "n")
        .filter(condition(db, &without_state).await?)
        .group_by(job::Column::State)
        .into_tuple()
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|(s, n)| (s, n as u64)).collect())
}

/// Distinct worker names, for the filter select.
pub async fn workers(db: &DatabaseConnection) -> Result<Vec<String>, DbErr> {
    job::Entity::find()
        .select_only()
        .column(job::Column::Worker)
        .distinct()
        .order_by_asc(job::Column::Worker)
        .into_tuple()
        .all(db)
        .await
}

pub async fn get(db: &DatabaseConnection, id: i64) -> Result<Option<job::Model>, DbErr> {
    job::Entity::find_by_id(id).one(db).await
}

/// The batch a job belongs to, with its members' counts per state.
pub async fn batch(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<(job_batch::Model, HashMap<JobState, u64>)>, DbErr> {
    let Some(batch) = job_batch::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let filter = Condition::all().add(job::Column::BatchId.eq(id));
    let rows: Vec<(JobState, i64)> = job::Entity::find()
        .select_only()
        .column(job::Column::State)
        .column_as(Expr::expr(Func::count(Expr::col(job::Column::Id))), "n")
        .filter(filter)
        .group_by(job::Column::State)
        .into_tuple()
        .all(db)
        .await?;
    Ok(Some((
        batch,
        rows.into_iter().map(|(s, n)| (s, n as u64)).collect(),
    )))
}

// ---------------------------------------------------------------------------
// Actions. Each row update is filtered by the state it was read in (or a state set), so an
// action never races a running node into an impossible state.
// ---------------------------------------------------------------------------

/// What an operator can do to a job in a given state (the dashboard shows only these buttons;
/// the actions re-check it in SQL).
pub fn can_retry(state: JobState) -> bool {
    !matches!(state, JobState::Executing | JobState::Available)
}
pub fn can_run_now(state: JobState) -> bool {
    matches!(state, JobState::Scheduled | JobState::Retryable)
}
pub fn can_cancel(state: JobState) -> bool {
    !state.is_final()
}
pub fn can_discard(state: JobState) -> bool {
    !state.is_final()
}
pub fn can_delete(state: JobState) -> bool {
    state != JobState::Executing
}

const RETRYABLE_FROM: [JobState; 5] = [
    JobState::Scheduled,
    JobState::Retryable,
    JobState::Completed,
    JobState::Discarded,
    JobState::Cancelled,
];

/// Anything but executing/available → available now. Keeps the errors (the history matters),
/// clears the final timestamps and makes sure at least one attempt is left.
pub async fn retry(db: &DatabaseConnection, ids: &[i64]) -> Result<u64, DbErr> {
    let now = Utc::now();
    let mut changed = 0;
    for chunk in ids.chunks(ID_CHUNK) {
        // At least one attempt left: max_attempts = attempt + 1 where it's used up.
        job::Entity::update_many()
            .col_expr(
                job::Column::MaxAttempts,
                Expr::col(job::Column::Attempt).add(1),
            )
            .filter(job::Column::Id.is_in(chunk.to_vec()))
            .filter(job::Column::State.is_in(RETRYABLE_FROM))
            .filter(Expr::col(job::Column::MaxAttempts).lte(Expr::col(job::Column::Attempt)))
            .exec(db)
            .await?;
        changed += job::Entity::update_many()
            .col_expr(
                job::Column::State,
                Expr::value(JobState::Available.to_value()),
            )
            .col_expr(job::Column::ScheduledAt, Expr::value(now))
            .col_expr(
                job::Column::CompletedAt,
                Expr::value(Option::<DateTime<Utc>>::None),
            )
            .col_expr(
                job::Column::DiscardedAt,
                Expr::value(Option::<DateTime<Utc>>::None),
            )
            .col_expr(
                job::Column::CancelledAt,
                Expr::value(Option::<DateTime<Utc>>::None),
            )
            .filter(job::Column::Id.is_in(chunk.to_vec()))
            .filter(job::Column::State.is_in(RETRYABLE_FROM))
            .exec(db)
            .await?
            .rows_affected;
    }
    if changed > 0 {
        wake();
    }
    log_action("retry", changed);
    Ok(changed)
}

/// `scheduled` / `retryable` → available now.
pub async fn run_now(db: &DatabaseConnection, ids: &[i64]) -> Result<u64, DbErr> {
    let mut changed = 0;
    for chunk in ids.chunks(ID_CHUNK) {
        changed += job::Entity::update_many()
            .col_expr(
                job::Column::State,
                Expr::value(JobState::Available.to_value()),
            )
            .col_expr(job::Column::ScheduledAt, Expr::value(Utc::now()))
            .filter(job::Column::Id.is_in(chunk.to_vec()))
            .filter(job::Column::State.is_in([JobState::Scheduled, JobState::Retryable]))
            .exec(db)
            .await?
            .rows_affected;
    }
    if changed > 0 {
        wake();
    }
    log_action("run_now", changed);
    Ok(changed)
}

/// Non-final → cancelled. An executing job is aborted by its node's next heartbeat (≤ 1s).
pub async fn cancel(db: &DatabaseConnection, ids: &[i64]) -> Result<u64, DbErr> {
    let changed = finalize(db, ids, JobState::Cancelled, "cancelled: by an operator").await?;
    log_action("cancel", changed);
    Ok(changed)
}

/// Non-final → discarded ("give up on it"), like a worker's `Outcome::Discard`.
pub async fn discard(db: &DatabaseConnection, ids: &[i64]) -> Result<u64, DbErr> {
    let changed = finalize(db, ids, JobState::Discarded, "discarded: by an operator").await?;
    log_action("discard", changed);
    Ok(changed)
}

async fn finalize(
    db: &DatabaseConnection,
    ids: &[i64],
    to: JobState,
    reason: &str,
) -> Result<u64, DbErr> {
    let now = Utc::now();
    let mut changed = 0;
    let mut batches = Vec::new();
    for chunk in ids.chunks(ID_CHUNK) {
        let rows = job::Entity::find()
            .filter(job::Column::Id.is_in(chunk.to_vec()))
            .all(db)
            .await?;
        for j in rows.into_iter().filter(|j| !j.state.is_final()) {
            let errors = push_error(&j.errors, j.attempt, now, None, &AttemptError::new(reason));
            let stamp = match to {
                JobState::Cancelled => job::Column::CancelledAt,
                _ => job::Column::DiscardedAt,
            };
            let mut update = job::Entity::update_many()
                .col_expr(job::Column::State, Expr::value(to.to_value()))
                .col_expr(stamp, Expr::value(now))
                .col_expr(job::Column::Errors, Expr::value(errors))
                .filter(job::Column::Id.eq(j.id))
                .filter(job::Column::State.eq(j.state));
            if releases_unique(&j) {
                update =
                    update.col_expr(job::Column::UniqueKey, Expr::value(Option::<String>::None));
            }
            let n = update.exec(db).await?.rows_affected;
            changed += n;
            if n > 0
                && let Some(b) = j.batch_id
            {
                batches.push(b);
            }
        }
    }
    batches.sort_unstable();
    batches.dedup();
    for b in batches {
        plugins::finish_batch_if_done(db, b).await?;
    }
    Ok(changed)
}

/// Deletes jobs in any state except executing.
pub async fn delete(db: &DatabaseConnection, ids: &[i64]) -> Result<u64, DbErr> {
    let mut changed = 0;
    for chunk in ids.chunks(ID_CHUNK) {
        changed += job::Entity::delete_many()
            .filter(job::Column::Id.is_in(chunk.to_vec()))
            .filter(job::Column::State.ne(JobState::Executing))
            .exec(db)
            .await?
            .rows_affected;
    }
    log_action("delete", changed);
    Ok(changed)
}

/// The ids a "matching" bulk action touches: newest first, capped at `BULK_CAP`. An empty filter
/// matches nothing.
/// INVARIANT: a bulk action with no filter changes nothing (test: bulk_actions_refuse_an_empty_filter)
pub async fn matching_ids(db: &DatabaseConnection, filter: &JobFilter) -> Result<Vec<i64>, DbErr> {
    if filter.is_empty() {
        return Ok(Vec::new());
    }
    job::Entity::find()
        .select_only()
        .column(job::Column::Id)
        .filter(condition(db, filter).await?)
        .order_by_desc(job::Column::Id)
        .limit(BULK_CAP)
        .into_tuple()
        .all(db)
        .await
}

/// A bulk action by name, on explicit ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Retry,
    RunNow,
    Cancel,
    Discard,
    Delete,
}

impl Action {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "retry" => Some(Self::Retry),
            "run" | "run_now" => Some(Self::RunNow),
            "cancel" => Some(Self::Cancel),
            "discard" => Some(Self::Discard),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::RunNow => "run",
            Self::Cancel => "cancel",
            Self::Discard => "discard",
            Self::Delete => "delete",
        }
    }

    /// "Retry", "Run now", …
    pub fn label(self) -> &'static str {
        match self {
            Self::Retry => "Retry",
            Self::RunNow => "Run now",
            Self::Cancel => "Cancel",
            Self::Discard => "Discard",
            Self::Delete => "Delete",
        }
    }

    /// "Retried", "Ran", …
    pub fn past(self) -> &'static str {
        match self {
            Self::Retry => "Retried",
            Self::RunNow => "Queued",
            Self::Cancel => "Cancelled",
            Self::Discard => "Discarded",
            Self::Delete => "Deleted",
        }
    }

    /// Whether this action applies to a job in `state`.
    pub fn allowed(self, state: JobState) -> bool {
        match self {
            Self::Retry => can_retry(state),
            Self::RunNow => can_run_now(state),
            Self::Cancel => can_cancel(state),
            Self::Discard => can_discard(state),
            Self::Delete => can_delete(state),
        }
    }

    pub async fn apply(self, db: &DatabaseConnection, ids: &[i64]) -> Result<u64, DbErr> {
        match self {
            Self::Retry => retry(db, ids).await,
            Self::RunNow => run_now(db, ids).await,
            Self::Cancel => cancel(db, ids).await,
            Self::Discard => discard(db, ids).await,
            Self::Delete => delete(db, ids).await,
        }
    }

    /// The same action on every job matching `filter` (capped, refuses an empty filter).
    pub async fn apply_matching(
        self,
        db: &DatabaseConnection,
        filter: &JobFilter,
    ) -> Result<u64, DbErr> {
        let ids = matching_ids(db, filter).await?;
        self.apply(db, &ids).await
    }
}

fn log_action(action: &str, count: u64) {
    tracing::info!(action, count, "jobs admin");
}

// ---------------------------------------------------------------------------
// Queues
// ---------------------------------------------------------------------------

pub async fn pause(db: &DatabaseConnection, queue: &str) -> Result<(), DbErr> {
    set_paused(db, queue, true).await
}

pub async fn resume(db: &DatabaseConnection, queue: &str) -> Result<(), DbErr> {
    set_paused(db, queue, false).await
}

/// Pauses every known queue (a deploy switch). Returns how many queues.
pub async fn pause_all(db: &DatabaseConnection) -> Result<usize, DbErr> {
    let names: Vec<String> = queues(db, Utc::now())
        .await?
        .into_iter()
        .map(|q| q.name)
        .collect();
    for name in &names {
        set_paused(db, name, true).await?;
    }
    Ok(names.len())
}

/// Resumes every paused queue. Returns how many.
pub async fn resume_all(db: &DatabaseConnection) -> Result<u64, DbErr> {
    let res = job_queue::Entity::update_many()
        .col_expr(job_queue::Column::Paused, Expr::value(false))
        .col_expr(job_queue::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(job_queue::Column::Paused.eq(true))
        .exec(db)
        .await?;
    wake();
    Ok(res.rows_affected)
}

async fn set_paused(db: &DatabaseConnection, queue: &str, paused: bool) -> Result<(), DbErr> {
    job_queue::Entity::insert(job_queue::ActiveModel {
        name: Set(queue.to_owned()),
        paused: Set(paused),
        updated_at: Set(Utc::now()),
    })
    .on_conflict(
        OnConflict::column(job_queue::Column::Name)
            .update_columns([job_queue::Column::Paused, job_queue::Column::UpdatedAt])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    tracing::info!(queue, paused, "jobs admin: queue");
    if !paused {
        wake();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

/// Cron key → its latest run and the job that run enqueued.
pub async fn last_runs(
    db: &DatabaseConnection,
) -> Result<HashMap<String, (DateTime<Utc>, Option<i64>)>, DbErr> {
    let rows = recurring_execution::Entity::find()
        .order_by_asc(recurring_execution::Column::RunAt)
        .order_by_asc(recurring_execution::Column::Id)
        .all(db)
        .await?;
    // Ascending, so the latest run per key wins.
    Ok(rows
        .into_iter()
        .map(|r| (r.key, (r.run_at, r.job_id)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern("Ab"), "%ab%");
        assert_eq!(like_pattern("100%_x\\"), "%100\\%\\_x\\\\%");
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v: Vec<i64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50), Some(50));
        assert_eq!(percentile(&v, 95), Some(95));
        assert_eq!(percentile(&[7], 95), Some(7));
        assert_eq!(percentile(&[], 50), None);
    }

    #[test]
    fn error_signatures_group_by_shape() {
        assert_eq!(
            error_signature("order 42 failed: HTTP 503\nCaused by: x"),
            "order N failed: HTTP 503"
        );
        assert_eq!(
            error_signature("GET /v2/items/77 timed out after 30s"),
            "GET /v2/items/N timed out after Ns"
        );
        assert_eq!(
            error_signature("order 7 failed: HTTP 503"),
            error_signature("order 99 failed: HTTP 503")
        );
        assert_eq!(
            search_fragment("connection refused (os error 61)").as_deref(),
            Some("connection refused (os error")
        );
        assert_eq!(search_fragment("42"), None);
        assert_eq!(
            search_fragment("HTTP 503 Service Unavailable").as_deref(),
            Some("HTTP 503 Service Unavailable")
        );
        assert_eq!(
            search_fragment("order 42 is locked").as_deref(),
            Some("is locked")
        );
    }

    #[test]
    fn root_causes_hosts_and_variants() {
        let msg = "POST https://hooks.partner.invalid/v2/events (order.paid): connecting to hooks.partner.invalid:443: connection refused (os error 61)";
        let (context, cause) = split_root_cause(msg);
        assert_eq!(cause, "connection refused (os error 61)");
        assert!(context.ends_with("connecting to hooks.partner.invalid:443"));
        assert_eq!(error_host(msg).as_deref(), Some("hooks.partner.invalid"));
        assert_eq!(
            error_host("connecting to db.internal.example:5432: timed out").as_deref(),
            Some("db.internal.example")
        );
        assert_eq!(error_host("charging card 42: declined"), None);
        assert_eq!(
            split_root_cause("timed out after 30s"),
            ("", "timed out after 30s")
        );
        let parts = distinct_parts(&[
            "POST https://h.invalid/vN/events (order.paid): connecting".to_owned(),
            "POST https://h.invalid/vN/events (order.refunded): connecting".to_owned(),
            "POST https://h.invalid/vN/events (customer.updated): connecting".to_owned(),
        ]);
        assert_eq!(parts, ["order.paid", "order.refunded", "customer.updated"]);
    }

    #[test]
    fn empty_filters_are_detected() {
        assert!(JobFilter::default().is_empty());
        assert!(
            JobFilter {
                q: Some("  ".into()),
                ..Default::default()
            }
            .is_empty()
        );
        assert!(
            !JobFilter {
                worker: Some("w".into()),
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn actions_parse_and_know_their_states() {
        assert_eq!(Action::parse("run"), Some(Action::RunNow));
        assert_eq!(Action::parse("drop table"), None);
        assert!(Action::Retry.allowed(JobState::Discarded));
        assert!(!Action::Retry.allowed(JobState::Executing));
        assert!(!Action::Delete.allowed(JobState::Executing));
        assert!(Action::Cancel.allowed(JobState::Executing));
        assert!(!Action::Cancel.allowed(JobState::Completed));
    }
}
