//! The request middleware: per matched route, the count, status classes, a latency histogram
//! (p50/p95/p99), bytes and the slowest examples; and the request context that the SQL callback
//! and the tracing layer attach to, which becomes the request's trace.
//!
//! Mounted at the router's `outer layers` anchor: outside the panic, timeout and error layers, so
//! a panic's 500 and a timeout's 503 are counted like any other response.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, Utc};
use futures::StreamExt;

use super::hist::Hist;
use super::trace::{self, EventRec, SpanRec, SqlRec, Trace};
use super::{HUB, lock};
use crate::routes::ROUTES;

/// Paths the dashboard never records. APP: add your own noise (a health probe hit every second).
// WHY: the dashboard's own pages and streams would otherwise fill every chart with themselves;
// `/dev/reload` is the dev-reload poll, once a second per tab.
pub const IGNORED: &[&str] = &[
    "/dev/dashboard",
    "/admin/dashboard",
    "/dev/reload",
    // The lab's simulated APIs stand for other services: they show as Upstreams, not as routes.
    "/dev/lab/upstream/",
    // The browser beacon's reports (production's endpoint; development's is under /dev/dashboard).
    "/rum",
    "/static/",
    "/favicon.ico",
];

/// The same SQL fingerprint more than this many times in one request is an N+1.
pub const N_PLUS_ONE: u32 = 5; // APP: lower to catch smaller loops

/// Per trace, at most this many spans, SQL statements and events are kept (counts stay exact; the
/// waterfall says how many more ran). APP: a loop over method spans can open thousands.
pub const MAX_SPANS: usize = 2_000;
pub const MAX_SQL: usize = 1_000;
pub const MAX_EVENTS: usize = 100;
/// Slowest examples kept per route.
pub const SLOWEST: usize = 3;

tokio::task_local! {
    static CURRENT: Arc<RequestCtx>;
    static QUIET: ();
}

/// The request being served on this task, if the dashboard is recording it.
pub fn current() -> Option<Arc<RequestCtx>> {
    CURRENT.try_with(Arc::clone).ok()
}

/// True inside the dashboard's own requests (their SQL isn't recorded either).
pub fn quiet() -> bool {
    QUIET.try_with(|_| ()).is_ok()
}

/// Runs `f` without recording its SQL (the dashboard's own EXPLAIN).
pub async fn quietly<F: Future>(f: F) -> F::Output {
    QUIET.scope((), f).await
}

fn ignored(path: &str) -> bool {
    IGNORED.iter().any(|p| {
        path.strip_prefix(p)
            .is_some_and(|rest| p.ends_with('/') || rest.is_empty() || rest.starts_with(['/', '.']))
    })
}

fn method_str(m: &Method) -> &'static str {
    match *m {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// A `ROUTES` path as segments, with how many are literal.
struct Pattern {
    method: &'static str,
    path: &'static str,
    segs: Vec<&'static str>,
    literal: usize,
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    ROUTES
        .iter()
        .map(|(method, path, _, _)| {
            let segs: Vec<&'static str> = path.trim_end_matches('/').split('/').collect();
            let literal = segs.iter().filter(|s| !s.starts_with('{')).count();
            Pattern {
                method,
                path,
                segs,
                literal,
            }
        })
        .collect()
});

/// Requests that match no route share one bucket.
// SECURITY: never one bucket per raw path: a scanner requesting random URLs would grow the map
// without bound (test: the_dashboard_stays_within_its_memory_bound).
pub const UNMATCHED: &str = "(unmatched)";

/// The `ROUTES` pattern a request matched (`/dev/mailbox/{id}`), the most literal one when several
/// do; `(unmatched)` otherwise.
pub fn route_of(method: &str, path: &str) -> &'static str {
    if path.starts_with("/static/") {
        return "/static/*";
    }
    let trimmed = path.trim_end_matches('/');
    let n = trimmed.split('/').count();
    let fits = |p: &Pattern| {
        p.segs.len() == n
            && p.segs
                .iter()
                .zip(trimmed.split('/'))
                .all(|(want, got)| want.starts_with('{') || *want == got)
    };
    let same_method = |p: &Pattern| p.method == method || (method == "HEAD" && p.method == "GET");
    let best = |any_method: bool| {
        PATTERNS
            .iter()
            .filter(|p| (any_method || same_method(p)) && fits(p))
            .max_by_key(|p| p.literal)
            .map(|p| p.path)
    };
    best(false).or_else(|| best(true)).unwrap_or(UNMATCHED)
}

/// What one request collects while it runs.
pub struct RequestCtx {
    pub id: u64,
    pub started: Instant,
    pub at: DateTime<Utc>,
    pub method: &'static str,
    pub route: &'static str,
    pub path: String,
    pub request_id: String,
    /// Where the link came from: a path on this app, or another site's host.
    pub referrer: Option<String>,
    /// `request`, or a background trace's kind (`job`, `mail`, `cron`).
    pub kind: &'static str,
    /// A background trace saw an error (it has no status code of its own).
    pub failed: std::sync::atomic::AtomicBool,
    /// Its spans open under another of its spans (`trace::flush_open` looks only when some are).
    pub open: std::sync::atomic::AtomicU32,
    inner: Mutex<CtxInner>,
}

#[derive(Default)]
struct CtxInner {
    spans: Vec<SpanRec>,
    sql: Vec<SqlRec>,
    events: Vec<EventRec>,
    /// Statements per fingerprint (the N+1 detector), exact even when `sql` is full.
    counts: HashMap<u64, (u32, Arc<str>)>,
    statements: u32,
    sql_us: u64,
    dropped: u32,
}

impl RequestCtx {
    pub fn new(method: &'static str, route: &'static str, path: &str, request_id: String) -> Self {
        Self {
            id: HUB.next_id(),
            started: Instant::now(),
            at: Utc::now(),
            method,
            route,
            // SECURITY: an emailed link's token in the path never reaches a trace, a log line or
            // the 404 list (test: not_found_paths_keep_the_nearest_route_and_a_safe_referrer).
            path: crate::kernel::ui::truncate(&crate::kernel::redact::path(path), 200),
            request_id,
            referrer: None,
            kind: "request",
            failed: std::sync::atomic::AtomicBool::new(false),
            open: std::sync::atomic::AtomicU32::new(0),
            inner: Mutex::new(CtxInner::default()),
        }
    }

    /// A trace that isn't a request: a job, a mail delivery, a scheduled task (`trace.rs` opens
    /// one when a root span with that name starts outside a request).
    pub fn background(kind: &'static str, name: &'static str, label: &str, id: String) -> Self {
        let mut ctx = Self::new(
            match kind {
                "mail" => "MAIL",
                "cron" => "CRON",
                _ => "JOB",
            },
            name,
            label,
            id,
        );
        ctx.kind = kind;
        ctx
    }

    /// Microseconds from the start of the request to `at`.
    pub fn offset_us(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.started).as_micros() as u64
    }

    pub fn push_span(&self, span: SpanRec) {
        let mut inner = lock(&self.inner);
        if inner.spans.len() < MAX_SPANS {
            inner.spans.push(span);
        } else {
            inner.dropped += 1;
        }
    }

    /// Spans its spans, statements and events name as their parent that it hasn't recorded: still
    /// open (`trace::flush_open`).
    pub fn unrecorded_parents(&self) -> Vec<u64> {
        let inner = lock(&self.inner);
        let recorded: std::collections::HashSet<u64> = inner.spans.iter().map(|s| s.id).collect();
        let mut out: Vec<u64> = inner
            .spans
            .iter()
            .filter_map(|s| s.parent)
            .chain(inner.sql.iter().filter_map(|q| q.parent))
            .chain(inner.events.iter().filter_map(|e| e.parent))
            .filter(|p| !recorded.contains(p))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn push_sql(&self, rec: SqlRec) {
        let mut inner = lock(&self.inner);
        inner.statements += 1;
        inner.sql_us += rec.dur_us;
        let entry = inner.counts.entry(rec.fp).or_insert((0, rec.sql.clone()));
        entry.0 += 1;
        if inner.sql.len() < MAX_SQL {
            inner.sql.push(rec);
        } else {
            inner.dropped += 1;
        }
    }

    pub fn push_event(&self, event: EventRec) {
        let mut inner = lock(&self.inner);
        if inner.events.len() < MAX_EVENTS {
            inner.events.push(event);
        } else {
            inner.dropped += 1;
        }
    }
}

/// One route's numbers.
pub struct RouteStats {
    pub method: &'static str,
    pub route: &'static str,
    pub count: u64,
    pub s2: u64,
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub hist: Hist,
    pub bytes: u64,
    pub n_plus_one: u64,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    /// The slowest requests seen, slowest first.
    pub slowest: Vec<Arc<Trace>>,
    /// Per minute: (unix minute, requests, 5xx), the last `MINUTES` (rising error rates).
    pub minutes: std::collections::VecDeque<(i64, u32, u32)>,
}

/// Minutes of per-route history kept for trends.
pub const MINUTES: usize = 30;

impl RouteStats {
    /// Errors per request in the last `recent` minutes and in the minutes before (within the
    /// history), with the error count of the recent window.
    pub fn error_trend(&self, now_minute: i64, recent: i64) -> (f64, f64, u32) {
        let (mut rn, mut re, mut on, mut oe) = (0_u32, 0_u32, 0_u32, 0_u32);
        for (m, n, e) in &self.minutes {
            if now_minute - m < recent {
                rn += n;
                re += e;
            } else {
                on += n;
                oe += e;
            }
        }
        let rate = |e: u32, n: u32| {
            if n == 0 {
                0.0
            } else {
                f64::from(e) / f64::from(n)
            }
        };
        (rate(re, rn), rate(oe, on), re)
    }
}

/// A route as the Requests page lists it.
pub struct RouteRow {
    pub method: &'static str,
    pub route: &'static str,
    pub count: u64,
    pub s2: u64,
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub mean: u64,
    pub bytes: u64,
    pub n_plus_one: u64,
    pub last: DateTime<Utc>,
    pub slowest: Vec<(u64, u64, u16)>,
}

/// Every route's numbers. Keys come from `ROUTES` (plus `(unmatched)` and `/static/*`), so the map
/// is bounded by the route table.
#[derive(Default)]
pub struct HttpStats {
    routes: HashMap<(&'static str, &'static str), RouteStats>,
    not_found: std::collections::VecDeque<NotFound>,
    get_writes: Vec<GetWrite>,
}

/// A path that matched no route.
pub struct NotFound {
    pub method: &'static str,
    pub path: String,
    pub count: u64,
    pub last: DateTime<Utc>,
    pub referrer: Option<String>,
}

/// A write statement run while serving a GET.
pub struct GetWrite {
    pub route: &'static str,
    pub sql: Arc<str>,
    pub count: u64,
    pub trace: u64,
}

/// Distinct 404 paths kept.
pub const MAX_NOT_FOUND: usize = 100;

/// APP: writes a GET may do (a cache fill, a session's last-seen time). A fingerprint starting
/// with one of these isn't flagged.
pub const GET_WRITES_ALLOWED: &[&str] = &[
    "INSERT INTO \"cache_entries\"",
    "UPDATE \"cache_entries\"",
    "DELETE FROM \"cache_entries\"",
];

/// The referrer as the explorer shows it: a path on this app, or only the other site's host.
// SECURITY: a referrer's query string can carry tokens; only a path or a host is kept
// (test: not_found_paths_keep_the_nearest_route_and_a_safe_referrer).
pub fn referrer_of(raw: &str) -> String {
    let no_query = raw.split(['?', '#']).next().unwrap_or_default();
    match no_query.split_once("://") {
        Some((_, rest)) => {
            let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
            if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
                crate::kernel::ui::truncate(&crate::kernel::redact::path(&format!("/{path}")), 120)
            } else {
                crate::kernel::ui::truncate(host, 80)
            }
        }
        None => crate::kernel::ui::truncate(no_query, 120),
    }
}

impl HttpStats {
    pub fn record(&mut self, trace: &Arc<Trace>) {
        let now = trace.at;
        let s = self
            .routes
            .entry((trace.method, trace.route))
            .or_insert_with(|| RouteStats {
                method: trace.method,
                route: trace.route,
                count: 0,
                s2: 0,
                s3: 0,
                s4: 0,
                s5: 0,
                hist: Hist::default(),
                bytes: 0,
                n_plus_one: 0,
                first: now,
                last: now,
                slowest: Vec::new(),
                minutes: std::collections::VecDeque::with_capacity(MINUTES),
            });
        s.count += 1;
        match trace.status {
            500.. => s.s5 += 1,
            400..=499 => s.s4 += 1,
            300..=399 => s.s3 += 1,
            _ => s.s2 += 1,
        }
        s.hist.record(trace.dur_us);
        s.bytes += trace.bytes.unwrap_or(0);
        if !trace.n_plus_one.is_empty() {
            s.n_plus_one += 1;
        }
        s.last = now;
        let minute = now.timestamp().div_euclid(60);
        let error = u32::from(trace.status >= 500);
        match s.minutes.back_mut() {
            Some(m) if m.0 == minute => {
                m.1 += 1;
                m.2 += error;
            }
            _ => {
                if s.minutes.len() == MINUTES {
                    s.minutes.pop_front();
                }
                s.minutes.push_back((minute, 1, error));
            }
        }
        if s.slowest.len() < SLOWEST || s.slowest.last().is_some_and(|t| t.dur_us < trace.dur_us) {
            s.slowest.push(trace.clone());
            s.slowest.sort_by_key(|x| std::cmp::Reverse(x.dur_us));
            s.slowest.truncate(SLOWEST);
        }
    }

    /// Bytes streamed after the headers (responses without a Content-Length).
    pub fn add_bytes(&mut self, method: &'static str, route: &'static str, n: usize) {
        if let Some(s) = self.routes.get_mut(&(method, route)) {
            s.bytes += n as u64;
        }
    }

    /// Busiest first.
    pub fn rows(&self) -> Vec<RouteRow> {
        let mut rows: Vec<RouteRow> = self
            .routes
            .values()
            .map(|s| RouteRow {
                method: s.method,
                route: s.route,
                count: s.count,
                s2: s.s2,
                s3: s.s3,
                s4: s.s4,
                s5: s.s5,
                p50: s.hist.quantile(0.50),
                p95: s.hist.quantile(0.95),
                p99: s.hist.quantile(0.99),
                max: s.hist.max(),
                mean: s.hist.mean(),
                bytes: s.bytes,
                n_plus_one: s.n_plus_one,
                last: s.last,
                slowest: s
                    .slowest
                    .iter()
                    .map(|t| (t.id, t.dur_us, t.status))
                    .collect(),
            })
            .collect();
        rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.route.cmp(b.route)));
        rows
    }

    /// A slowest example by trace id (it outlives the trace rings).
    pub fn example(&self, id: u64) -> Option<Arc<Trace>> {
        self.routes
            .values()
            .flat_map(|s| s.slowest.iter())
            .find(|t| t.id == id)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn stats(&self) -> impl Iterator<Item = &RouteStats> {
        self.routes.values()
    }

    pub fn get(&self, method: &str, route: &str) -> Option<&RouteStats> {
        self.routes
            .values()
            .find(|s| s.method == method && s.route == route)
    }

    pub fn not_found(&mut self, method: &'static str, path: &str, referrer: Option<String>) {
        let path = crate::kernel::ui::truncate(path, 200);
        if let Some(n) = self
            .not_found
            .iter_mut()
            .find(|n| n.path == path && n.method == method)
        {
            n.count += 1;
            n.last = Utc::now();
            if referrer.is_some() {
                n.referrer = referrer;
            }
            return;
        }
        // INVARIANT: bounded (test: the_dashboard_stays_within_its_memory_bound).
        if self.not_found.len() == MAX_NOT_FOUND {
            self.not_found.pop_front();
        }
        self.not_found.push_back(NotFound {
            method,
            path,
            count: 1,
            last: Utc::now(),
            referrer,
        });
    }

    pub fn not_found_list(&self) -> Vec<&NotFound> {
        let mut v: Vec<&NotFound> = self.not_found.iter().collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then(b.last.cmp(&a.last)));
        v
    }

    pub fn get_write(&mut self, route: &'static str, sql: &Arc<str>, trace: u64) {
        if let Some(w) = self
            .get_writes
            .iter_mut()
            .find(|w| w.route == route && w.sql == *sql)
        {
            w.count += 1;
            w.trace = trace;
            return;
        }
        if self.get_writes.len() < 100 {
            self.get_writes.push(GetWrite {
                route,
                sql: sql.clone(),
                count: 1,
                trace,
            });
        }
    }

    pub fn get_writes(&self) -> &[GetWrite] {
        &self.get_writes
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn approx_bytes(&self) -> usize {
        self.routes
            .values()
            .map(|s| {
                std::mem::size_of::<RouteStats>()
                    + Hist::BYTES
                    + s.slowest.iter().map(|t| t.approx_bytes()).sum::<usize>()
            })
            .sum()
    }
}

/// The middleware (`axum::middleware::from_fn`). With the dashboard off it only checks a flag.
pub async fn middleware(req: Request, next: Next) -> Response {
    // Inside the dashboard's own probes (`quietly`), nothing is recorded either.
    if !HUB.enabled() || quiet() {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if ignored(path) {
        // A probe dressed as a static file (`/static/../../etc/passwd`) still counts.
        let probe = super::threats::inspect(&req, UNMATCHED).filter(|p| p.flags != 0);
        let path = super::threats::clean_path(path);
        let res = QUIET.scope((), next.run(req)).await;
        if let Some(p) = probe {
            super::threats::finish(p, res.status().as_u16(), &path, 0);
        }
        return res;
    }
    let method = method_str(req.method());
    let route = route_of(method, path);
    let threat = super::threats::inspect(&req, route);
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .chars()
        .take(64)
        .collect();
    let referrer = req
        .headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .map(referrer_of);
    let mut ctx = RequestCtx::new(method, route, path, request_id);
    ctx.referrer = referrer;
    let ctx = Arc::new(ctx);
    let res = CURRENT.scope(ctx.clone(), next.run(req)).await;
    let len = res
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    finish(&ctx, res.status().as_u16(), len);
    if let Some(p) = threat {
        super::threats::finish(p, res.status().as_u16(), &ctx.path, ctx.id);
    }
    match len {
        Some(_) => res,
        None => count_streamed_bytes(res, method, route),
    }
}

/// Counts a streamed body's bytes as they go out (compressed pages, SSE).
fn count_streamed_bytes(res: Response, method: &'static str, route: &'static str) -> Response {
    let (parts, body) = res.into_parts();
    let stream = body.into_data_stream().inspect(move |chunk| {
        if let Ok(bytes) = chunk {
            lock(&HUB.http).add_bytes(method, route, bytes.len());
        }
    });
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Turns the context into a trace and records it everywhere.
pub fn finish(ctx: &RequestCtx, status: u16, bytes: Option<u64>) -> Arc<Trace> {
    let trace = Arc::new(build(ctx, status, bytes));
    {
        let mut t = lock(&HUB.tick);
        t.requests += 1;
        match status {
            500.. => t.e5 += 1,
            400..=499 => t.e4 += 1,
            _ => {}
        }
        t.latency.record(trace.dur_us);
    }
    if !trace.n_plus_one.is_empty() {
        lock(&HUB.sql).flag_n_plus_one(&trace);
    }
    {
        let mut http = lock(&HUB.http);
        http.record(&trace);
        if ctx.route == UNMATCHED {
            http.not_found(ctx.method, &ctx.path, ctx.referrer.clone());
        }
    }
    lock(&HUB.traces).push(trace.clone());
    // Notable ones (5xx, N+1, slow) also go to the history file, batched off this path.
    super::history::record_trace(&trace);
    trace::request_line(&trace);
    trace::export(&trace);
    trace
}

/// A background trace (a job, a mail delivery) is done: kept with the requests' traces, without
/// counting as a request on the charts.
pub fn finish_background(ctx: &RequestCtx) -> Arc<Trace> {
    let failed = ctx.failed.load(std::sync::atomic::Ordering::Relaxed);
    let trace = Arc::new(build(ctx, if failed { 500 } else { 200 }, None));
    if !trace.n_plus_one.is_empty() {
        lock(&HUB.sql).flag_n_plus_one(&trace);
    }
    lock(&HUB.traces).push(trace.clone());
    super::history::record_trace(&trace);
    trace::export(&trace);
    trace
}

fn build(ctx: &RequestCtx, status: u16, bytes: Option<u64>) -> Trace {
    trace::flush_open(ctx);
    let dur_us = ctx.started.elapsed().as_micros() as u64;
    let inner = std::mem::take(&mut *lock(&ctx.inner));
    let mut n_plus_one: Vec<(u64, u32, Arc<str>)> = inner
        .counts
        .into_iter()
        .filter(|(_, (n, _))| *n > N_PLUS_ONE)
        .map(|(fp, (n, sql))| (fp, n, sql))
        .collect();
    n_plus_one.sort_by_key(|x| std::cmp::Reverse(x.1));
    Trace {
        id: ctx.id,
        at: ctx.at,
        kind: ctx.kind,
        from_disk: false,
        method: ctx.method,
        route: ctx.route,
        path: ctx.path.clone(),
        status,
        dur_us,
        bytes,
        request_id: ctx.request_id.clone(),
        spans: inner.spans,
        sql: inner.sql,
        events: inner.events,
        n_plus_one,
        statements: inner.statements,
        sql_us: inner.sql_us,
        dropped: inner.dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_grouped_by_their_route_pattern() {
        assert_eq!(route_of("GET", "/"), "/");
        assert_eq!(route_of("GET", "/dev/routes"), "/dev/routes");
        assert_eq!(route_of("HEAD", "/dev/routes/"), "/dev/routes");
        assert_eq!(route_of("GET", "/static/app.css"), "/static/*");
        assert_eq!(route_of("GET", "/wp-login.php"), UNMATCHED);
        assert_eq!(
            route_of("GET", "/dev/dashboard/traces/42"),
            "/dev/dashboard/traces/{id}"
        );
    }

    #[test]
    fn the_dashboard_ignores_itself() {
        assert!(ignored("/dev/dashboard"));
        assert!(ignored("/dev/dashboard/sql"));
        assert!(ignored("/dev/dashboard.json"));
        assert!(ignored("/static/kernel/admin.css"));
        assert!(!ignored("/dev/dashboardx"));
        assert!(!ignored("/dev/lab/orders"));
        assert!(!ignored("/"));
    }
}
