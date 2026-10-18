//! Proactive analysis (opt-in, `DASHBOARD_AI_PROACTIVE=true`, development only): a watcher that
//! checks the dashboard's deterministic signals every `WATCH_EVERY` and, when one fires, posts an
//! **incident card** to the overview and Insights. With a provider configured it also starts one
//! agent run to investigate (debounced per signal, at most `PER_HOUR` an hour, inside the day's
//! budget); without one, the card is the deterministic finding alone.
//!
//! Also here: the "since you were away" digest on the overview (deterministic, no AI).
//!
//! The signals (APP: tune the constants, or add yours to `detect`):
//!
//! | Signal | Fires when |
//! |--------|-----------|
//! | latency spike | the last minute's p95 is at least `SPIKE_FACTOR` × the 14 minutes before, and over the warn threshold |
//! | error burst | `ERROR_BURST` or more 5xx in the last minute |
//! | new N+1 | an N+1 fingerprint the watcher hadn't seen |
//! | memory growth | resident memory grew past the warn threshold since boot |
//! | job failures | `JOB_FAILURES` more retryable or discarded jobs than at the last check |
//! | schema drift | the migrations and the database disagree (checked once a minute) |
//! | failing tests | a new test run with failures |
//!
//! GUIDE: .offrails/guides/dev-tools.md

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::ai::{self, Tab};
use super::ai_agent::{self, Kind};
use super::series::Range;
use super::web::{HEALTH, micros};
use super::{HUB, lock, migrations, schema, sql, testruns};
use crate::state::AppState;

/// APP: how often the watcher looks, how long one signal stays quiet after it fired, how many
/// auto-investigations an hour, how many incident cards are kept.
pub const WATCH_EVERY: Duration = Duration::from_secs(10);
pub const DEBOUNCE: Duration = Duration::from_secs(10 * 60);
pub const PER_HOUR: usize = 6;
pub const INCIDENTS: usize = 20;
/// APP: the boot's warm-up: the first look this long after boot (the charts need a baseline,
/// and a demo's or a restart's backlog of signals fires all at once otherwise), and at most
/// `BOOT_INVESTIGATIONS` investigations in the first `BOOT_WINDOW`, the most severe first.
// WHY: right after boot the watcher saw every signal at once and spent most of a day's budget
// in its first minutes.
pub const WARMUP: Duration = Duration::from_secs(60);
pub const BOOT_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const BOOT_INVESTIGATIONS: usize = 2;
/// APP: the signals' thresholds.
pub const SPIKE_FACTOR: f64 = 2.0;
pub const ERROR_BURST: f64 = 10.0;
pub const JOB_FAILURES: i64 = 3;

/// A deterministic signal: what fired, its evidence (with citation tags), where to look.
#[derive(Clone, Debug)]
pub struct Signal {
    /// Debounce key (`latency_spike`, `n_plus_one:1f2e…`).
    pub key: String,
    pub kind: &'static str,
    pub title: String,
    pub evidence: String,
    pub href: String,
    /// `high`, `medium`, `low`.
    pub severity: &'static str,
    /// The page whose context the investigation starts from.
    pub tab: Tab,
}

#[derive(Clone, Debug)]
pub struct Incident {
    pub id: String,
    pub at: DateTime<Utc>,
    pub signal: Signal,
    /// The agent run investigating it.
    pub run: Option<String>,
    /// Why there's no run (no provider, budget, the hourly cap).
    pub note: String,
}

#[derive(Default)]
pub struct WatchState {
    fired: HashMap<String, Instant>,
    seen_n1: HashSet<u64>,
    investigations: VecDeque<Instant>,
    incidents: VecDeque<Incident>,
    seen_tests: HashSet<String>,
    drift_checked: Option<Instant>,
    jobs_failed: Option<i64>,
    boot_investigations: usize,
}

pub static WATCH: LazyLock<Mutex<WatchState>> = LazyLock::new(|| Mutex::new(WatchState::default()));

/// Whether the watcher runs: opted in, and development.
// SECURITY: auto-investigation sends data without a click, so it is opt-in and development only
// (test: the_watcher_posts_incidents_and_investigates_within_its_limits)
pub fn enabled(state: &AppState) -> bool {
    state.config.is_development() && state.config.dashboard.ai.proactive
}

/// Starts the watcher (from `serve_started`).
pub fn spawn(state: AppState) {
    if !enabled(&state) {
        return;
    }
    tokio::spawn(async move {
        // WHY: the first look one interval after boot: the charts have a baseline by then, and a
        // local provider started alongside the app is up.
        let mut every = tokio::time::interval_at(tokio::time::Instant::now() + WARMUP, WATCH_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            super::http::quietly(tick(&state)).await;
        }
    });
}

/// The incident cards, newest first.
pub fn incidents() -> Vec<Incident> {
    lock(&WATCH).incidents.iter().rev().cloned().collect()
}

/// One round: detect, then post (and maybe investigate) each signal that isn't debounced.
/// Returns the incidents it posted.
pub async fn tick(state: &AppState) -> Vec<Incident> {
    let mut signals = detect(state).await;
    // The most severe first: under the hourly and boot caps they are the ones investigated.
    let rank = |s: &Signal| match s.severity {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    };
    signals.sort_by_key(rank);
    let mut posted = Vec::new();
    for s in signals {
        let fresh = {
            let mut w = lock(&WATCH);
            let quiet = w.fired.get(&s.key).is_some_and(|t| t.elapsed() < DEBOUNCE);
            if !quiet {
                w.fired.insert(s.key.clone(), Instant::now());
            }
            !quiet
        };
        if !fresh {
            continue;
        }
        let (run, note) = investigate(state, &s).await;
        let inc = Incident {
            id: ai::random_id()[..12].to_owned(),
            at: Utc::now(),
            signal: s,
            run,
            note,
        };
        let mut w = lock(&WATCH);
        while w.incidents.len() >= INCIDENTS {
            w.incidents.pop_front();
        }
        w.incidents.push_back(inc.clone());
        posted.push(inc);
    }
    posted
}

/// Starts the agent on a signal, when a provider is set and the limits allow it.
async fn investigate(state: &AppState, s: &Signal) -> (Option<String>, String) {
    if state.config.dashboard.ai.provider.is_none() {
        return (
            None,
            "Deterministic finding (set DASHBOARD_AI_PROVIDER to have the agent investigate)."
                .into(),
        );
    }
    {
        let mut w = lock(&WATCH);
        while w
            .investigations
            .front()
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(3600))
        {
            w.investigations.pop_front();
        }
        if w.investigations.len() >= PER_HOUR {
            return (
                None,
                format!("Not investigated: {PER_HOUR} auto-investigations this hour (PER_HOUR)."),
            );
        }
        let booted = (Utc::now() - *crate::kernel::ui::BOOTED_AT)
            .to_std()
            .unwrap_or_default();
        if booted < BOOT_WINDOW {
            w.boot_investigations += 1;
            if w.boot_investigations > BOOT_INVESTIGATIONS {
                return (
                    None,
                    format!(
                        "Not investigated: at most {BOOT_INVESTIGATIONS} investigations in the first {} minutes after boot (BOOT_INVESTIGATIONS); open it and ask if it matters.",
                        BOOT_WINDOW.as_secs() / 60
                    ),
                );
            }
        }
        w.investigations.push_back(Instant::now());
    }
    if let Err(e) = ai::budget(&state.config.dashboard.ai).reserve(ai::today(), Instant::now()) {
        return (None, format!("Not investigated: {}", e.message()));
    }
    let context = ai_context_for(state, s.tab).await;
    let p = ai::store(s.tab, None, context);
    let question = format!(
        "The dashboard's watcher saw this signal: {}. Evidence: {}. Investigate the cause with your tools, say how bad it is, and what to do first. Keep it short and cite your evidence.",
        s.title, s.evidence
    );
    let spec = ai_agent::Spec {
        tab: s.tab,
        item: None,
        kind: Kind::Incident,
        first: ai::user_message(&p, &question),
        system: p.system,
        question,
        owner_cookie: None,
        ro: false,
        base: "/dev/dashboard",
        reserved: true,
    };
    match ai_agent::start(state, spec) {
        Ok(run) => (Some(run.id.clone()), String::new()),
        Err(e) => (None, format!("Not investigated: {}", e.message())),
    }
}

async fn ai_context_for(state: &AppState, tab: Tab) -> String {
    super::ai_context::build(state, false, tab, None)
        .await
        .unwrap_or_default()
}

/// The request-weighted p95 of `samples`, µs (0 without traffic).
fn weighted_p95(samples: &[super::series::Sample]) -> f64 {
    let req: f64 = samples.iter().map(|s| f64::from(s.req)).sum();
    if req <= 0.0 {
        return 0.0;
    }
    samples
        .iter()
        .map(|s| f64::from(s.p95) * f64::from(s.req))
        .sum::<f64>()
        / req
}

/// Every signal that is on now (debouncing is `tick`'s job).
pub async fn detect(state: &AppState) -> Vec<Signal> {
    let now = Utc::now().timestamp();
    let mut out = Vec::new();
    let window = lock(&HUB.series).window(Range::M15, now);
    let split = window
        .iter()
        .position(|s| s.t > now - 60)
        .unwrap_or(window.len());
    let (before, recent) = window.split_at(split);
    let clock = |t: i64| ai::clock(t);
    // Latency spike.
    let (r, b) = (weighted_p95(recent), weighted_p95(before));
    let warn = f64::from(HEALTH.p95.0);
    if r >= warn && (b == 0.0 && r >= f64::from(HEALTH.p95.1) || b > 0.0 && r >= SPIKE_FACTOR * b) {
        out.push(Signal {
            key: "latency_spike".into(),
            kind: "latency spike",
            title: format!("Latency p95 jumped to {}", micros(r as u64)),
            evidence: format!(
                "p95 {} in the last minute against {} in the 14 minutes before [chart:latency {}-{}]",
                micros(r as u64),
                if b == 0.0 { "no traffic".into() } else { micros(b as u64) },
                clock(now - 15 * 60),
                clock(now)
            ),
            href: "/dev/dashboard/charts?range=15m#chart-latency".into(),
            severity: "high",
            tab: Tab::Requests,
        });
    }
    // Error burst.
    let e5: f64 = recent.iter().map(|s| f64::from(s.e5)).sum();
    if e5 >= ERROR_BURST {
        out.push(Signal {
            key: "error_burst".into(),
            kind: "error burst",
            title: format!("{e5:.0} server errors in the last minute"),
            evidence: format!(
                "{e5:.0} responses with a 5xx status since {} [chart:errors {}-{}]",
                clock(now - 60),
                clock(now - 15 * 60),
                clock(now)
            ),
            href: "/dev/dashboard/requests?sort=errors".into(),
            severity: "high",
            tab: Tab::Requests,
        });
    }
    // New N+1 patterns (at most three a round).
    let n1: Vec<(u64, &'static str, u32, u64)> = lock(&HUB.sql)
        .n_plus_one()
        .iter()
        .map(|n| (n.fp, n.route, n.count, n.trace))
        .collect();
    {
        let mut w = lock(&WATCH);
        for (fp, route, count, trace) in n1 {
            if w.seen_n1.insert(fp) && out.iter().filter(|s| s.kind == "new N+1").count() < 3 {
                out.push(Signal {
                    key: format!("n_plus_one:{}", sql::hex(fp)),
                    kind: "new N+1",
                    title: format!("New N+1 on {route}"),
                    evidence: format!(
                        "[sql:{}] ran {count}× in one request [trace:{trace}]",
                        sql::hex(fp)
                    ),
                    href: format!("/dev/dashboard/sql/{}", sql::hex(fp)),
                    severity: "medium",
                    tab: Tab::Sql,
                });
            }
        }
    }
    // Memory growth.
    let boot = HUB.boot_rss.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(last) = window.last()
        && boot > 0
        && last.rss.saturating_sub(boot) >= HEALTH.memory_growth.0
    {
        out.push(Signal {
            key: "memory".into(),
            kind: "memory growth",
            title: format!(
                "Memory grew by {} since boot",
                super::web::bytes(last.rss - boot)
            ),
            evidence: format!(
                "resident {} now, {} at boot [chart:memory {}-{}]",
                super::web::bytes(last.rss),
                super::web::bytes(boot),
                clock(now - 15 * 60),
                clock(now)
            ),
            href: "/dev/dashboard/charts#chart-memory".into(),
            severity: "medium",
            tab: Tab::Overview,
        });
    }
    // Job failures.
    if super::panels::table_exists(&state.db, "jobs").await {
        let rows = super::ai_context::text_rows(
            state,
            "SELECT CAST(COUNT(*) AS TEXT) AS n FROM jobs WHERE state IN ('retryable', 'discarded')",
            vec![],
            &["n"],
        )
        .await;
        let n: i64 = rows.first().and_then(|r| r[0].parse().ok()).unwrap_or(0);
        let prev = lock(&WATCH).jobs_failed.replace(n);
        let grew = match prev {
            Some(p) => n - p >= JOB_FAILURES,
            None => n >= JOB_FAILURES * 2,
        };
        if grew {
            out.push(Signal {
                key: "jobs".into(),
                kind: "job failures",
                title: format!(
                    "{n} job{} failing (retryable or discarded)",
                    if n == 1 { " is" } else { "s are" }
                ),
                evidence: format!(
                    "{n} jobs retryable or discarded, {} at the last check",
                    prev.unwrap_or(0)
                ),
                href: "/admin/jobs/errors".into(),
                severity: "medium",
                tab: Tab::Jobs,
            });
        }
    }
    // Schema drift, once a minute.
    let check_drift = {
        let mut w = lock(&WATCH);
        let due = w
            .drift_checked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
        if due {
            w.drift_checked = Some(Instant::now());
        }
        due
    };
    if check_drift {
        let tables = schema::tables(&state.db).await.unwrap_or_default();
        let report = migrations::report(&state.db, &tables, Path::new(".")).await;
        if let Some(d) = report.drift.first() {
            out.push(Signal {
                key: "drift".into(),
                kind: "schema drift",
                title: format!("Schema drift: {}", super::ai_context::clean(&d.title, 120)),
                evidence: format!(
                    "{} difference(s) between the migrations and the database; first: {}",
                    report.drift.len(),
                    super::ai_context::clean(&d.detail, 200)
                ),
                href: "/dev/dashboard/schema/migrations".into(),
                severity: "medium",
                tab: Tab::Migrations,
            });
        }
    }
    // Failing tests.
    let runs = testruns::load_runs(&testruns::root().join(testruns::DIR));
    if let Some(run) = runs.first()
        && run.failed() > 0
        && lock(&WATCH).seen_tests.insert(run.id.clone())
    {
        out.push(Signal {
            key: format!("tests:{}", run.id),
            kind: "failing tests",
            title: format!(
                "{} test{} failed in the latest run",
                run.failed(),
                if run.failed() == 1 { "" } else { "s" }
            ),
            evidence: format!("run {} ({}): {} failed", run.id, run.source, run.failed()),
            href: "/dev/dashboard/tests".into(),
            severity: "high",
            tab: Tab::Tests,
        });
    }
    out
}

// ---- "since you were away" ---------------------------------------------------------------------

/// The overview's cookie: when this browser last looked.
pub const SEEN_COOKIE: &str = "dashboard_seen";
/// APP: the digest's window without a cookie, and its bounds.
pub const DEFAULT_AWAY: i64 = 30 * 60;

pub struct DigestLine {
    pub text: String,
    pub href: Option<String>,
    pub tone: &'static str,
}

pub struct Digest {
    pub title: String,
    pub lines: Vec<DigestLine>,
}

/// What happened since `since` (unix seconds): traffic and errors, the latency peak, markers
/// (boots, deploys, load-lab runs, error bursts), N+1s first seen, incidents, test runs.
pub fn digest(since: Option<i64>, now: i64) -> Digest {
    let from = since
        .filter(|s| *s < now)
        .unwrap_or(now - DEFAULT_AWAY)
        .clamp(now - 86_400, now - 60);
    let mins = (now - from) / 60;
    let title = match since {
        Some(_) => format!("Since you were last here ({})", ago(mins)),
        None => format!("The last {}", ago(mins)),
    };
    let slice = super::history::between(from, now);
    let (samples, marks) = (slice.samples, slice.marks);
    let mut lines = Vec::new();
    // Per-second samples hold rates; per-minute ones (older than 15 minutes) too, per second.
    let step = |i: usize| {
        if i + 1 < samples.len() {
            (samples[i + 1].t - samples[i].t).max(1) as f64
        } else {
            1.0
        }
    };
    let req: f64 = samples
        .iter()
        .enumerate()
        .map(|(i, s)| f64::from(s.req) * step(i))
        .sum();
    let e5: f64 = samples
        .iter()
        .enumerate()
        .map(|(i, s)| f64::from(s.e5) * step(i))
        .sum();
    let peak = samples.iter().filter(|s| s.req > 0.0).map(|s| s.p95).max();
    lines.push(DigestLine {
        text: format!(
            "{} requests, {} server errors{}",
            crate::kernel::ui::thousands(req as u64),
            crate::kernel::ui::thousands(e5 as u64),
            peak.map(|p| format!(", latency p95 peaked at {}", micros(u64::from(p))))
                .unwrap_or_default()
        ),
        href: Some(format!("/dev/dashboard/charts?from={from}&to={now}")),
        tone: if e5 > 0.0 { "warn" } else { "ok" },
    });
    for m in marks.iter().rev().take(6) {
        lines.push(DigestLine {
            text: format!(
                "{} {}: {}",
                ai::clock(m.t),
                m.kind,
                super::ai_context::clean(&m.label, 100)
            ),
            href: Some(format!(
                "/dev/dashboard/charts?from={}&to={}",
                (m.t - 300).max(from),
                (m.t + 300).min(now)
            )),
            tone: match m.kind {
                "errors" | "spike" => "err",
                "deploy" | "boot" => "info",
                _ => "muted",
            },
        });
    }
    let since_dt = DateTime::<Utc>::from_timestamp(from, 0).unwrap_or_else(Utc::now);
    for n in lock(&HUB.sql)
        .n_plus_one()
        .iter()
        .filter(|n| n.last >= since_dt)
        .take(3)
    {
        lines.push(DigestLine {
            text: format!("N+1 on {}: {}× in one request", n.route, n.count),
            href: Some(format!("/dev/dashboard/traces/{}", n.trace)),
            tone: "warn",
        });
    }
    for i in incidents().iter().filter(|i| i.at >= since_dt).take(5) {
        lines.push(DigestLine {
            text: format!("Incident: {}", i.signal.title),
            href: Some("/dev/dashboard/insights#incidents".into()),
            tone: if i.signal.severity == "high" {
                "err"
            } else {
                "warn"
            },
        });
    }
    for r in testruns::load_runs(&testruns::root().join(testruns::DIR))
        .iter()
        .filter(|r| r.at >= since_dt)
        .take(3)
    {
        lines.push(DigestLine {
            text: format!("Test run {}: {} failed", r.id, r.failed()),
            href: Some("/dev/dashboard/tests".into()),
            tone: if r.failed() > 0 { "err" } else { "ok" },
        });
    }
    if lines.len() == 1 && req == 0.0 {
        lines[0].text = "Quiet: no requests recorded in this window.".into();
        lines[0].tone = "muted";
    }
    Digest { title, lines }
}

fn ago(mins: i64) -> String {
    match mins {
        m if m < 1 => "a moment".into(),
        1 => "1 minute".into(),
        m if m < 120 => format!("{m} minutes"),
        m => format!("{} hours", m / 60),
    }
}
