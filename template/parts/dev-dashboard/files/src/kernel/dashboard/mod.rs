//! The dev dashboard (Phoenix LiveDashboard, in-process): metrics, charts, requests, traces, SQL,
//! logs, test runs, CI and a load lab at `/dev/dashboard`. Development only by default; a
//! read-only subset can be mounted at `/admin/dashboard` in production behind admin access.
//!
//! GUIDE: .offrails/guides/dev-tools.md
//! GUIDE: .offrails/guides/errors-and-logging.md#logging
//!
//! Everything lives in this process, in fixed-size memory: no Prometheus, no Grafana, no Jaeger.
//!
//! | Part | Where it hooks in | File |
//! |------|-------------------|------|
//! | Collector: memory, CPU, file descriptors, Tokio, DB pool, every second | a task started by `serve` | `collector.rs` |
//! | Requests: count, status classes, latency histogram, bytes, slowest examples per route | a middleware outside the router | `http.rs` |
//! | SQL: fingerprints, durations, redacted params, N+1, EXPLAIN | SeaORM's metric callback | `sql.rs` |
//! | Traces and live logs | a `tracing-subscriber` layer | `trace.rs` |
//! | Load lab: fake data, traffic, scenarios | `/dev/dashboard/lab`, `/dev/lab/*`, `app bench` | `lab.rs` |
//! | Insights, routes explorer, schema | everything above, the catalog, CSP reports | `insights.rs`, `route_explorer.rs`, `schema.rs` |
//! | Schema diagram (ERD), migrations timeline and drift | the catalog, `seaql_migrations`, `migration/src/*.rs` | `erd.rs`, `migrations.rs` |
//! | Ask AI (optional, your own provider): an agent with read-only tools, live runs, history, incidents | the above, already redacted | `ai.rs`, `ai_context.rs`, `ai_tools.rs`, `ai_agent.rs`, `ai_watch.rs`, `ai_explain.rs`, `ai_web.rs` |
//! | Test runs and CI | `storage/test-runs/`, the GitHub API | `testruns.rs`, `github.rs` |
//!
//! WHY one process-wide hub (`HUB`) and not a field on `AppState`: the tracing layer is installed
//! before any state exists, and the SQL callback runs inside SeaORM without access to it.

pub mod access;
// samples: dashboard mods
pub mod ai;
pub mod ai_agent;
pub mod ai_context;
pub mod ai_explain;
pub mod ai_tools;
pub mod ai_watch;
pub mod ai_web;
pub mod charts;
pub mod cli;
pub mod collector;
pub mod demo;
pub mod erd;
pub mod github;
pub mod hist;
pub mod http;
pub mod insights;
pub mod lab;
pub mod migrations;
pub mod panels;
pub mod route_explorer;
pub mod schema;
pub mod series;
pub mod sql;
pub mod testruns;
pub mod trace;
pub mod web;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

use sea_orm::DatabaseConnection;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::config::Config;
use hist::Hist;
use series::Series;

/// The dashboard's state for this process.
pub static HUB: LazyLock<Hub> = LazyLock::new(Hub::default);

pub struct Hub {
    enabled: AtomicBool,
    /// Development keeps the last parameters of each SELECT (never shown) so EXPLAIN can run it.
    keep_values: AtomicBool,
    next_id: AtomicU64,
    pub series: Mutex<Series>,
    pub http: Mutex<http::HttpStats>,
    pub sql: Mutex<sql::SqlStats>,
    pub traces: Mutex<trace::Traces>,
    pub logs: Mutex<trace::Logs>,
    /// New log lines for the live tail (a slow tab misses lines; it never slows the app).
    pub log_tx: broadcast::Sender<std::sync::Arc<trace::LogLine>>,
    /// This second's counters, drained by the collector.
    pub tick: Mutex<Tick>,
    pub lab: lab::LabState,
    pub tests: testruns::RunnerState,
    pub github: github::Cache,
    pub insights: insights::InsightState,
    /// The address `serve` listens on (`http://127.0.0.1:3000`), for the load lab.
    pub serving: OnceLock<String>,
    /// Resident memory at the collector's first sample ("since boot" on the memory card).
    pub boot_rss: AtomicU64,
}

/// Per-second accumulators.
#[derive(Default)]
pub struct Tick {
    pub requests: u64,
    pub e4: u64,
    pub e5: u64,
    pub latency: Hist,
    pub statements: u64,
    pub sql_latency: Hist,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            keep_values: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            series: Mutex::new(Series::default()),
            http: Mutex::new(http::HttpStats::default()),
            sql: Mutex::new(sql::SqlStats::default()),
            traces: Mutex::new(trace::Traces::default()),
            logs: Mutex::new(trace::Logs::default()),
            log_tx: broadcast::channel(1024).0,
            tick: Mutex::new(Tick::default()),
            lab: lab::LabState::default(),
            tests: testruns::RunnerState::default(),
            github: github::Cache::default(),
            insights: insights::InsightState::default(),
            serving: OnceLock::new(),
            boot_rss: AtomicU64::new(0),
        }
    }
}

/// A poisoned lock only means a panic elsewhere mid-update; the numbers are still usable.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Hub {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn keeps_values(&self) -> bool {
        self.keep_values.load(Ordering::Relaxed)
    }

    /// Turns recording on (`instrument` does this).
    pub fn enable(&self, keep_values: bool) {
        self.keep_values.store(keep_values, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Turns recording off (the overhead test compares both).
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Forgets requests, SQL, traces, N+1 and logs (the charts keep their history).
    pub fn reset(&self) {
        *lock(&self.http) = http::HttpStats::default();
        *lock(&self.sql) = sql::SqlStats::default();
        *lock(&self.traces) = trace::Traces::default();
        *lock(&self.logs) = trace::Logs::default();
    }

    /// What the dashboard holds, in bytes (shown on Config; bounded by a test).
    pub fn approx_bytes(&self) -> usize {
        let series = {
            let s = lock(&self.series);
            let (a, b) = s.lens();
            (a + b + 60) * Series::SAMPLE_BYTES + 2 * Hist::BYTES
        };
        series
            + lock(&self.http).approx_bytes()
            + lock(&self.sql).approx_bytes()
            + lock(&self.traces).approx_bytes()
            + lock(&self.logs).approx_bytes()
    }
}

/// Whether this process records at all: development, or production with the read-only dashboard
/// mounted (`DASHBOARD_IN_PRODUCTION=true` and an admin gate). Tests (`APP_ENV=test`) don't.
pub fn enabled_for(config: &Config) -> bool {
    config.is_development() || access::mounted_in_production(config)
}

/// Called from `AppState::new`: turns recording on and installs the SQL callback.
///
/// WHY here: SeaORM's metric callback needs `&mut DatabaseConnection`, so it must be set before
/// the connection is cloned into the state. It replaces any earlier callback: in a test with a
/// development config, `TestApp::query_count()` no longer counts (use `HUB.sql` there).
pub fn instrument(db: DatabaseConnection, config: &Config) -> DatabaseConnection {
    if !enabled_for(config) {
        return db;
    }
    HUB.enable(config.is_development());
    // The Ask AI bar on other kernel pages (the kernel layout's wire asks `ai_web::bar`).
    ai_web::BAR.store(
        ai::allowed(config, false),
        std::sync::atomic::Ordering::Relaxed,
    );
    let mut db = db;
    db.set_metric_callback(sql::record);
    db
}

/// Called by `serve` once it is about to listen: starts the collector and tells the load lab
/// where the app answers (the plain-http port when development serves both).
pub fn serve_started(state: &crate::state::AppState) {
    if !HUB.enabled() {
        return;
    }
    let c = &state.config;
    let addr = match (c.dev_http_port, c.tls.is_some()) {
        (Some(port), _) => format!("http://127.0.0.1:{port}"),
        (None, true) => format!("https://127.0.0.1:{}", c.port),
        (None, false) => format!("http://127.0.0.1:{}", c.port),
    };
    let _ = HUB.serving.set(addr);
    {
        let now = chrono::Utc::now().timestamp();
        let mut series = lock(&HUB.series);
        if c.dashboard.demo_history {
            demo::backfill(&mut series, now);
        }
        series.annotate(series::Annotation {
            t: now,
            kind: "boot",
            label: "server started".into(),
            demo: false,
        });
    }
    collector::spawn(state.clone());
    // DASHBOARD_AI_PROACTIVE=true (development): the watcher and its incident cards.
    ai_watch::spawn(state.clone());
}

/// The numbers an agent asks for first (`/dev/dashboard.json`, `app introspect`).
pub fn snapshot() -> Value {
    let latest = lock(&HUB.series).latest().unwrap_or_default();
    let routes: Vec<Value> = lock(&HUB.http)
        .rows()
        .into_iter()
        .take(10)
        .map(|r| {
            json!({ "method": r.method, "route": r.route, "count": r.count, "p95_ms": r.p95 as f64 / 1000.0,
                    "errors": r.s4 + r.s5 })
        })
        .collect();
    let (slow, n_plus_one) = {
        let s = lock(&HUB.sql);
        let slow: Vec<Value> = s
            .rows(sql::Sort::Total)
            .into_iter()
            .take(10)
            .map(|q| json!({ "id": q.id, "sql": q.sql, "count": q.count, "p95_ms": q.p95 as f64 / 1000.0, "max_ms": q.max as f64 / 1000.0 }))
            .collect();
        let n: Vec<Value> = s
            .n_plus_one()
            .iter()
            .take(10)
            .map(|n| json!({ "route": n.route, "sql": n.sql, "count": n.count, "times": n.times, "trace": n.trace }))
            .collect();
        (slow, n)
    };
    json!({
        "enabled": HUB.enabled(),
        "latest": latest,
        "routes": routes,
        "slow_sql": slow,
        "n_plus_one": n_plus_one,
        "memory_bytes": HUB.approx_bytes(),
        "pages": "/dev/dashboard",
    })
}
