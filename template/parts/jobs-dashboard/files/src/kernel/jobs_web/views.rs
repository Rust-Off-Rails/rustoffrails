//! What the dashboard's templates read: plain structs of strings and numbers, built from
//! `kernel::jobs::admin` results. All formatting happens here, so templates stay logic-free.
//!
//! SECURITY: args, meta and errors reach templates as strings and Askama escapes them; args and
//! meta are redacted first (`kernel::jobs::redact`). Charts are SVG built from numbers only, in
//! templates, with no `|safe` anywhere (test: job_data_is_escaped_and_redacted_everywhere).

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::kernel::{
    jobs::{
        JobState,
        admin::{self, Bucket, ErrorGroup, JobFilter, NodeRow, QueueRow, Throughput, WorkerStat},
        job, redact,
    },
    ui,
};

pub const BASE: &str = "/admin/jobs";

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// `?a=1&b=x%20y` from non-empty pairs ("" when none).
pub fn query(pairs: &[(&str, &str)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", crate::kernel::cookie::encode(v)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

pub fn state_label(state: JobState) -> &'static str {
    match state {
        JobState::Executing => "Executing",
        JobState::Available => "Available",
        JobState::Scheduled => "Scheduled",
        JobState::Retryable => "Retryable",
        JobState::Discarded => "Discarded",
        JobState::Cancelled => "Cancelled",
        JobState::Completed => "Completed",
    }
}

/// One line for the counters' tooltips and the empty states.
pub fn state_help(state: JobState) -> &'static str {
    match state {
        JobState::Executing => "running on a node right now",
        JobState::Available => "due, waiting for a free slot",
        JobState::Scheduled => "waiting for their time",
        JobState::Retryable => "failed, will retry with backoff",
        JobState::Discarded => "failed for good",
        JobState::Cancelled => "stopped on purpose",
        JobState::Completed => "done",
    }
}

/// `1,234` below 10k, then `12.3k`, `4.5M`.
pub fn compact(n: u64) -> String {
    match n {
        n if n < 10_000 => ui::thousands(n),
        n if n < 1_000_000 => format!("{:.1}k", n as f64 / 1000.0),
        n => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// What a value with no data shows (a legend, a table cell). APP: the placeholder.
pub const NO_DATA: &str = "—";

pub fn ms(ms: Option<i64>) -> String {
    ms.map_or_else(|| NO_DATA.to_owned(), ui::duration)
}

fn short_args(args: &Value, max: usize) -> String {
    ui::truncate(&redact(args).to_string(), max)
}

/// "started 3s ago", "runs in 4m", "retries in 2m", "failed 2h ago".
pub fn when(j: &job::Model, now: DateTime<Utc>) -> String {
    crate::jobs::cli::when(j, now)
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

pub struct Card {
    pub state: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub count: String,
    pub exact: String,
    pub href: String,
}

pub fn cards(stats: &admin::Stats) -> Vec<Card> {
    stats
        .by_state
        .iter()
        .map(|(s, n)| Card {
            state: s.as_str(),
            label: state_label(*s),
            help: state_help(*s),
            count: compact(*n),
            exact: ui::thousands(*n),
            href: format!("{BASE}/list{}", query(&[("state", s.as_str())])),
        })
        .collect()
}

/// One bar of the throughput chart: SVG coordinates, as strings with two decimals.
pub struct Bar {
    pub x: String,
    pub width: String,
    pub bar_x: String,
    pub bar_width: String,
    pub ok_y: String,
    pub ok_h: String,
    pub err_y: String,
    pub err_h: String,
    pub has_ok: bool,
    pub has_err: bool,
    pub title: String,
}

pub struct Charts {
    pub bars: Vec<Bar>,
    pub completed: String,
    pub failed: String,
    pub per_min: String,
    pub peak: String,
    pub has_data: bool,
    pub summary: String,
    /// Queue wait: polyline points ("x,y x,y …").
    pub p50: String,
    pub p95: String,
    pub wait_peak: String,
    /// Legend values: queue wait over the whole window.
    pub wait_p95: String,
    pub wait_p50: String,
    pub has_waits: bool,
    pub start_label: String,
    pub end_label: String,
}

pub const CHART_W: f64 = 600.0;
pub const CHART_H: f64 = 120.0;

/// The overview's two charts. Legend values summarize the window the chart shows (the last 60
/// minutes), never its last minute, which is often empty; `NO_DATA` when there is none.
pub fn charts(t: &Throughput) -> Charts {
    let buckets = &t.buckets;
    let n = buckets.len().max(1) as f64;
    let slot = CHART_W / n;
    let bar = (slot * 0.72).max(1.0);
    let peak = buckets
        .iter()
        .map(|b| b.completed + b.failed)
        .max()
        .unwrap_or(0);
    let scale = peak.max(1) as f64;
    let usable = CHART_H - 2.0;
    let f = |v: f64| format!("{v:.2}");
    let bars = buckets
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let x = i as f64 * slot;
            let ok_h = b.completed as f64 / scale * usable;
            let err_h = b.failed as f64 / scale * usable;
            let wait = match (b.wait_p50_ms, b.wait_p95_ms) {
                (Some(p50), Some(p95)) => {
                    format!(
                        " · wait p50 {} p95 {}",
                        ui::duration(p50),
                        ui::duration(p95)
                    )
                }
                _ => String::new(),
            };
            Bar {
                x: f(x),
                width: f(slot),
                bar_x: f(x + (slot - bar) / 2.0),
                bar_width: f(bar),
                ok_y: f(CHART_H - ok_h),
                ok_h: f(ok_h),
                err_y: f(CHART_H - ok_h - err_h),
                err_h: f(err_h),
                has_ok: b.completed > 0,
                has_err: b.failed > 0,
                title: format!(
                    "{} UTC · {} completed · {} failed{wait}",
                    b.minute.format("%H:%M"),
                    ui::thousands(b.completed),
                    ui::thousands(b.failed)
                ),
            }
        })
        .collect();
    let completed: u64 = buckets.iter().map(|b| b.completed).sum();
    let failed: u64 = buckets.iter().map(|b| b.failed).sum();

    let wait_peak = buckets
        .iter()
        .filter_map(|b| b.wait_p95_ms)
        .max()
        .unwrap_or(0);
    let wscale = wait_peak.max(1) as f64;
    let line = |pick: fn(&Bucket) -> Option<i64>| {
        buckets
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                pick(b).map(|v| {
                    let x = i as f64 * slot + slot / 2.0;
                    let y = CHART_H - 1.0 - v as f64 / wscale * usable;
                    format!("{x:.2},{y:.2}")
                })
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let minutes = buckets.len() as u64;
    Charts {
        has_data: completed + failed > 0,
        summary: format!(
            "{} completed and {} failed in the last {minutes} minutes; busiest minute {}.",
            ui::thousands(completed),
            ui::thousands(failed),
            ui::thousands(peak)
        ),
        bars,
        completed: ui::thousands(completed),
        failed: ui::thousands(failed),
        per_min: format!("{:.1}", completed as f64 / minutes.max(1) as f64),
        peak: ui::thousands(peak),
        p50: line(|b| b.wait_p50_ms),
        p95: line(|b| b.wait_p95_ms),
        has_waits: buckets.iter().any(|b| b.wait_p95_ms.is_some()),
        wait_peak: ui::duration(wait_peak),
        // INVARIANT: legend values summarize the shown window, not its last minute
        // (test: chart_legends_summarize_the_whole_window)
        wait_p95: ms(t.wait_p95_ms),
        wait_p50: ms(t.wait_p50_ms),
        start_label: buckets
            .first()
            .map(|b| b.minute.format("%H:%M").to_string())
            .unwrap_or_default(),
        end_label: buckets
            .last()
            .map(|b| b.minute.format("%H:%M").to_string())
            .unwrap_or_default(),
    }
}

pub struct RunningView {
    pub id: i64,
    pub href: String,
    pub worker: String,
    pub queue: String,
    pub args: String,
    pub node: String,
    pub attempt: String,
    pub elapsed: String,
    pub progress: Option<i16>,
    pub over_half_timeout: bool,
}

pub fn running(jobs: &[job::Model], now: DateTime<Utc>) -> Vec<RunningView> {
    jobs.iter()
        .map(|j| {
            let elapsed = j
                .attempted_at
                .map(|t| (now - t).num_milliseconds().max(0))
                .unwrap_or(0);
            let timeout = (crate::jobs::worker_config)(&j.worker).timeout.as_millis() as i64;
            RunningView {
                id: j.id,
                href: format!("{BASE}/{}", j.id),
                worker: j.worker.clone(),
                queue: j.queue.clone(),
                args: short_args(&j.args, 60),
                node: j.attempted_by.clone().unwrap_or_default(),
                attempt: format!("{}/{}", j.attempt, j.max_attempts),
                elapsed: ui::duration(elapsed),
                progress: j.progress,
                over_half_timeout: elapsed * 2 > timeout,
            }
        })
        .collect()
}

pub struct QueueView {
    pub name: String,
    pub href: String,
    pub status: &'static str,
    pub status_class: &'static str,
    pub limit: usize,
    pub rate: String,
    pub nodes: usize,
    pub executing: String,
    pub available: String,
    pub scheduled: String,
    pub retryable: String,
    pub discarded: String,
    pub discarded_href: String,
    pub toggle_url: String,
    pub toggle_label: &'static str,
}

pub fn queues(rows: Vec<QueueRow>) -> Vec<QueueView> {
    rows.into_iter()
        .map(|q| {
            let (status, status_class) = match (q.paused, q.orphaned()) {
                (true, _) => ("paused", "warn"),
                (false, true) => ("no nodes", "err"),
                (false, false) => ("running", "ok"),
            };
            let (action, label) = if q.paused {
                ("resume", "Resume")
            } else {
                ("pause", "Pause")
            };
            let enc = crate::kernel::cookie::encode(&q.name);
            QueueView {
                href: format!("{BASE}/list{}", query(&[("queue", &q.name)])),
                discarded_href: format!(
                    "{BASE}/list{}",
                    query(&[("queue", &q.name), ("state", "discarded")])
                ),
                toggle_url: format!("{BASE}/queues/{enc}/{action}"),
                toggle_label: label,
                status,
                status_class,
                limit: q.limit,
                rate: q.rate.clone().unwrap_or_default(),
                nodes: q.nodes,
                executing: ui::thousands(q.executing),
                available: ui::thousands(q.available),
                scheduled: ui::thousands(q.scheduled),
                retryable: ui::thousands(q.retryable),
                discarded: ui::thousands(q.discarded),
                name: q.name,
            }
        })
        .collect()
}

pub struct NodeView {
    pub node: String,
    pub queues: String,
    pub executing: u64,
    pub started: String,
    pub heartbeat: String,
    pub alive: bool,
}

pub fn nodes(rows: Vec<NodeRow>, now: DateTime<Utc>) -> Vec<NodeView> {
    rows.into_iter()
        .map(|n| NodeView {
            queues: n
                .queues
                .iter()
                .map(|q| match &q.rate {
                    Some(r) => format!("{} ×{} ({r})", q.name, q.limit),
                    None => format!("{} ×{}", q.name, q.limit),
                })
                .collect::<Vec<_>>()
                .join(", "),
            executing: n.executing,
            started: ui::ago(n.started_at, now),
            heartbeat: ui::ago(n.heartbeat_at, now),
            alive: n.alive,
            node: n.node,
        })
        .collect()
}

pub struct WorkerView {
    pub worker: String,
    pub href: String,
    pub failed_href: String,
    pub completed: String,
    pub failed: String,
    pub has_failures: bool,
    /// "99.2%", or `NO_DATA`.
    pub success: String,
    /// 0-1000, for `<meter>`.
    pub success_permille: u32,
    pub success_class: &'static str,
    pub run_p50: String,
    pub run_p95: String,
    pub wait_p95: String,
}

pub fn workers(rows: Vec<WorkerStat>) -> Vec<WorkerView> {
    rows.into_iter()
        .map(|w| {
            let pct = w.success_pct.unwrap_or(0.0);
            WorkerView {
                href: format!("{BASE}/list{}", query(&[("worker", &w.worker)])),
                failed_href: format!("{BASE}/errors{}", query(&[("worker", &w.worker)])),
                completed: ui::thousands(w.completed),
                failed: ui::thousands(w.failed),
                has_failures: w.failed > 0,
                success: w
                    .success_pct
                    .map_or_else(|| NO_DATA.into(), |p| format!("{p:.1}%")),
                success_permille: (pct * 10.0).round() as u32,
                success_class: match pct {
                    p if p >= 99.0 => "ok",
                    p if p >= 90.0 => "warn",
                    _ => "err",
                },
                run_p50: ms(w.run_p50_ms),
                run_p95: ms(w.run_p95_ms),
                wait_p95: ms(w.wait_p95_ms),
                worker: w.worker,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

pub struct RowView {
    pub id: i64,
    pub href: String,
    pub state: &'static str,
    pub state_label: &'static str,
    pub worker: String,
    pub args: String,
    pub queue: String,
    pub attempts: String,
    pub when: String,
    pub tags: Vec<String>,
    pub progress: Option<i16>,
}

pub fn rows(jobs: &[job::Model], now: DateTime<Utc>) -> Vec<RowView> {
    jobs.iter()
        .map(|j| RowView {
            id: j.id,
            href: format!("{BASE}/{}", j.id),
            state: j.state.as_str(),
            state_label: state_label(j.state),
            worker: j.worker.clone(),
            args: short_args(&j.args, 90),
            queue: j.queue.clone(),
            attempts: format!("{}/{}", j.attempt, j.max_attempts),
            when: when(j, now),
            tags: j.tag_list(),
            progress: (j.state == JobState::Executing)
                .then_some(j.progress)
                .flatten(),
        })
        .collect()
}

/// The list's filter, as the form shows it (empty strings for "any").
#[derive(Default, Clone)]
pub struct FilterView {
    pub state: String,
    pub queue: String,
    pub worker: String,
    pub tag: String,
    pub q: String,
    pub error: String,
    pub active: bool,
}

impl FilterView {
    pub fn new(f: &JobFilter) -> Self {
        Self {
            state: f.state.map(|s| s.as_str().to_owned()).unwrap_or_default(),
            queue: f.queue.clone().unwrap_or_default(),
            worker: f.worker.clone().unwrap_or_default(),
            tag: f.tag.clone().unwrap_or_default(),
            q: f.q.clone().unwrap_or_default(),
            error: f.error.clone().unwrap_or_default(),
            active: !f.is_empty(),
        }
    }

    /// The filter's query pairs, with `state` replaced.
    pub fn pairs_with_state<'a>(&'a self, state: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("state", state),
            ("queue", &self.queue),
            ("worker", &self.worker),
            ("tag", &self.tag),
            ("q", &self.q),
            ("error", &self.error),
        ]
    }

    pub fn href(&self) -> String {
        format!("{BASE}/list{}", query(&self.pairs_with_state(&self.state)))
    }

    /// The same filter without one field (the active-filter chips' ×).
    pub fn without(&self, field: &str) -> String {
        let mut f = self.clone();
        match field {
            "queue" => f.queue.clear(),
            "worker" => f.worker.clear(),
            "tag" => f.tag.clear(),
            "q" => f.q.clear(),
            "error" => f.error.clear(),
            _ => f.state.clear(),
        }
        f.href()
    }
}

pub struct StateTab {
    pub label: &'static str,
    pub state: &'static str,
    pub count: String,
    pub href: String,
    pub active: bool,
}

pub struct Chip {
    pub label: &'static str,
    pub value: String,
    pub remove_href: String,
}

pub fn chips(f: &FilterView) -> Vec<Chip> {
    [
        ("Queue", "queue", &f.queue),
        ("Worker", "worker", &f.worker),
        ("Tag", "tag", &f.tag),
        ("Search", "q", &f.q),
        ("Error", "error", &f.error),
    ]
    .into_iter()
    .filter(|(_, _, v)| !v.is_empty())
    .map(|(label, field, v)| Chip {
        label,
        value: v.clone(),
        remove_href: f.without(field),
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Job page
// ---------------------------------------------------------------------------

pub struct Step {
    pub label: String,
    pub at: String,
    pub ago: String,
    pub class: &'static str,
    pub note: String,
}

pub struct AttemptView {
    pub attempt: String,
    pub at: String,
    pub ago: String,
    pub node: String,
    pub duration: String,
    pub error: String,
    pub detail: String,
    pub kind: &'static str,
}

pub struct ActionButton {
    pub action: &'static str,
    pub label: &'static str,
    pub class: &'static str,
    pub key: &'static str,
}

pub struct JobView {
    pub id: i64,
    pub state: &'static str,
    pub state_label: &'static str,
    pub worker: String,
    pub worker_href: String,
    pub queue: String,
    pub queue_href: String,
    pub when: String,
    pub attempts: String,
    pub priority: i16,
    pub node: String,
    pub unique_key: String,
    pub tags: Vec<(String, String)>,
    pub args: String,
    pub meta: String,
    pub has_meta: bool,
    pub progress: Option<i16>,
    pub executing: bool,
    pub is_final: bool,
    pub timeline: Vec<Step>,
    pub queue_time: String,
    pub run_time: String,
    pub attempts_list: Vec<AttemptView>,
    pub actions: Vec<ActionButton>,
    pub cli: String,
    pub batch: Option<BatchView>,
    pub cron_key: String,
}

pub struct BatchView {
    pub id: i64,
    pub name: String,
    pub total: i32,
    pub done: u64,
    pub failed: u64,
    pub finished: bool,
    pub callback_href: String,
}

fn parse_at(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub fn job_view(
    j: &job::Model,
    batch: Option<(
        crate::kernel::jobs::job_batch::Model,
        std::collections::HashMap<JobState, u64>,
    )>,
    now: DateTime<Utc>,
) -> JobView {
    let pretty = |v: &Value| serde_json::to_string_pretty(&redact(v)).unwrap_or_default();
    let errors: Vec<Value> = j.errors.as_array().cloned().unwrap_or_default();

    // Timeline: inserted → scheduled → each failed attempt → the current or final state.
    let mut timeline = vec![Step {
        label: "Inserted".into(),
        at: ui::utc(j.inserted_at),
        ago: ui::ago(j.inserted_at, now),
        class: "neutral",
        note: String::new(),
    }];
    // A job scheduled ahead at insert: when it was (or is) due.
    if j.attempt == 0 && (j.scheduled_at - j.inserted_at).num_seconds() > 1 {
        timeline.push(Step {
            label: if j.scheduled_at > now {
                "Runs".into()
            } else {
                "Was due".into()
            },
            at: ui::utc(j.scheduled_at),
            ago: if j.scheduled_at > now {
                ui::until(j.scheduled_at, now)
            } else {
                ui::ago(j.scheduled_at, now)
            },
            class: "scheduled",
            note: String::new(),
        });
    }
    for e in &errors {
        if let Some(at) = parse_at(&e["at"]) {
            let text = e["error"].as_str().unwrap_or_default();
            timeline.push(Step {
                label: format!("Attempt {} failed", e["attempt"]),
                at: ui::utc(at),
                ago: ui::ago(at, now),
                class: if text.starts_with("cancelled") {
                    "cancelled"
                } else {
                    "err"
                },
                note: ui::truncate(text.lines().next().unwrap_or_default(), 120),
            });
        }
    }
    if let Some(at) = j.attempted_at
        && j.state == JobState::Executing
    {
        timeline.push(Step {
            label: format!("Attempt {} started", j.attempt),
            at: ui::utc(at),
            ago: ui::ago(at, now),
            class: "executing",
            note: j
                .attempted_by
                .as_deref()
                .map(|n| format!("on {n}"))
                .unwrap_or_default(),
        });
    }
    let final_step = match j.state {
        JobState::Completed => j.completed_at.map(|t| ("Completed", t, "ok")),
        JobState::Discarded => j.discarded_at.map(|t| ("Discarded", t, "err")),
        JobState::Cancelled => j.cancelled_at.map(|t| ("Cancelled", t, "cancelled")),
        _ => None,
    };
    // Waiting again after a failure or a snooze: the next run closes the timeline.
    if j.attempt > 0
        && matches!(j.state, JobState::Retryable | JobState::Scheduled)
        && j.scheduled_at > now
    {
        timeline.push(Step {
            label: if j.state == JobState::Retryable {
                "Retries".into()
            } else {
                "Runs again".into()
            },
            at: ui::utc(j.scheduled_at),
            ago: ui::until(j.scheduled_at, now),
            class: "scheduled",
            note: String::new(),
        });
    }
    if let Some((label, at, class)) = final_step {
        timeline.push(Step {
            label: label.into(),
            at: ui::utc(at),
            ago: ui::ago(at, now),
            class,
            note: String::new(),
        });
    }

    // WHY: after a retry, scheduled_at is the *next* run, so the last wait isn't knowable.
    let queue_time = match j.attempted_at {
        Some(a) if a >= j.scheduled_at => ui::duration((a - j.scheduled_at).num_milliseconds()),
        _ => NO_DATA.into(),
    };
    let run_time = match (j.attempted_at, j.finished_at()) {
        (Some(a), Some(f)) if j.state == JobState::Completed => {
            ui::duration((f - a).num_milliseconds().max(0))
        }
        (Some(a), _) if j.state == JobState::Executing => {
            format!(
                "{} so far",
                ui::duration((now - a).num_milliseconds().max(0))
            )
        }
        _ => NO_DATA.into(),
    };

    let attempts_list = errors
        .iter()
        .rev()
        .map(|e| {
            let text = e["error"].as_str().unwrap_or_default().to_owned();
            AttemptView {
                attempt: e["attempt"].to_string(),
                at: parse_at(&e["at"]).map(ui::utc).unwrap_or_default(),
                ago: parse_at(&e["at"])
                    .map(|t| ui::ago(t, now))
                    .unwrap_or_default(),
                node: e["node"].as_str().unwrap_or_default().to_owned(),
                duration: e["duration_ms"]
                    .as_i64()
                    .map(ui::duration)
                    .unwrap_or_default(),
                kind: if text.starts_with("cancelled") {
                    "Cancelled"
                } else if text.starts_with("orphaned") || text.starts_with("interrupted") {
                    "Interrupted"
                } else if text.starts_with("timed out") {
                    "Timeout"
                } else if text.starts_with("panicked") {
                    "Panic"
                } else {
                    "Error"
                },
                detail: e["detail"].as_str().unwrap_or_default().to_owned(),
                error: text,
            }
        })
        .collect();

    let mut actions = Vec::new();
    let buttons = [
        (admin::Action::RunNow, "primary", "r"),
        (admin::Action::Retry, "primary", "r"),
        (admin::Action::Cancel, "", "c"),
        (admin::Action::Discard, "danger", "d"),
        (admin::Action::Delete, "danger", ""),
    ];
    for (action, class, key) in buttons {
        // Run now and Retry overlap on scheduled/retryable jobs: show Run now there.
        if action == admin::Action::Retry && admin::can_run_now(j.state) {
            continue;
        }
        if action.allowed(j.state) {
            actions.push(ActionButton {
                action: action.as_str(),
                label: action.label(),
                class,
                key,
            });
        }
    }
    let cli = if admin::can_run_now(j.state) {
        format!("app jobs run {}", j.id)
    } else if admin::can_retry(j.state) {
        format!("app jobs retry {}", j.id)
    } else {
        format!("app jobs show {}", j.id)
    };

    let batch = batch.map(|(b, counts)| {
        let n = |s: JobState| counts.get(&s).copied().unwrap_or(0);
        BatchView {
            id: b.id,
            name: b.name,
            total: b.total,
            done: n(JobState::Completed) + n(JobState::Discarded) + n(JobState::Cancelled),
            failed: n(JobState::Discarded),
            finished: b.finished_at.is_some(),
            callback_href: b
                .callback_job_id
                .map(|id| format!("{BASE}/{id}"))
                .unwrap_or_default(),
        }
    });

    JobView {
        id: j.id,
        state: j.state.as_str(),
        state_label: state_label(j.state),
        worker: j.worker.clone(),
        worker_href: format!("{BASE}/list{}", query(&[("worker", &j.worker)])),
        queue: j.queue.clone(),
        queue_href: format!("{BASE}/list{}", query(&[("queue", &j.queue)])),
        when: when(j, now),
        attempts: format!("{} of {}", j.attempt, j.max_attempts),
        priority: j.priority,
        node: j.attempted_by.clone().unwrap_or_else(|| NO_DATA.into()),
        unique_key: j
            .unique_key
            .as_deref()
            .map(|k| format!("{}…", &k[..k.len().min(12)]))
            .unwrap_or_else(|| NO_DATA.into()),
        tags: j
            .tag_list()
            .into_iter()
            .map(|t| {
                let href = format!("{BASE}/list{}", query(&[("tag", &t)]));
                (t, href)
            })
            .collect(),
        args: pretty(&j.args),
        has_meta: j.meta.as_object().is_some_and(|m| !m.is_empty()),
        meta: pretty(&j.meta),
        progress: j.progress,
        executing: j.state == JobState::Executing,
        is_final: j.state.is_final(),
        timeline,
        queue_time,
        run_time,
        attempts_list,
        actions,
        cli,
        batch,
        cron_key: j
            .meta
            .get("cron")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Errors and cron
// ---------------------------------------------------------------------------

pub struct ErrorGroupView {
    pub worker: String,
    pub worker_href: String,
    pub message: String,
    pub count: String,
    pub retryable: u64,
    pub discarded: u64,
    pub first_seen: String,
    pub last_seen: String,
    pub last_seen_utc: String,
    pub example_href: String,
    pub list_href: String,
    /// Bulk-retry form fields for the group's discarded jobs, when they can be targeted.
    pub retry_error: String,
    /// "3 variants: order.paid, order.refunded, customer.updated" (empty for one variant).
    pub variants: String,
}

pub fn error_groups(groups: Vec<ErrorGroup>, now: DateTime<Utc>) -> Vec<ErrorGroupView> {
    groups
        .into_iter()
        .map(|g| {
            let search = g.search.clone().unwrap_or_default();
            ErrorGroupView {
                worker_href: format!("{BASE}/list{}", query(&[("worker", &g.worker)])),
                list_href: format!(
                    "{BASE}/list{}",
                    query(&[("worker", &g.worker), ("error", &search)])
                ),
                example_href: format!("{BASE}/{}", g.example_id),
                count: ui::thousands(g.count),
                retryable: g.retryable,
                discarded: g.discarded,
                first_seen: g.first_seen.map(|t| ui::ago(t, now)).unwrap_or_default(),
                last_seen: g.last_seen.map(|t| ui::ago(t, now)).unwrap_or_default(),
                last_seen_utc: g.last_seen.map(ui::utc).unwrap_or_default(),
                retry_error: search,
                variants: if g.variant_count > 1 {
                    let more = g.variant_count.saturating_sub(g.variants.len());
                    format!(
                        "{} variants: {}{}",
                        g.variant_count,
                        g.variants
                            .iter()
                            .map(|v| if v.is_empty() { "(none)" } else { v.as_str() })
                            .collect::<Vec<_>>()
                            .join(", "),
                        if more > 0 {
                            format!(" and {more} more")
                        } else {
                            String::new()
                        }
                    )
                } else {
                    String::new()
                },
                message: g.message,
                worker: g.worker,
            }
        })
        .collect()
}

pub struct CronView {
    pub key: String,
    pub cron: String,
    pub description: String,
    pub worker: String,
    pub worker_href: String,
    pub queue: String,
    pub next: String,
    pub next_utc: String,
    pub last: String,
    pub last_href: String,
    pub run_url: String,
    pub valid: bool,
}
