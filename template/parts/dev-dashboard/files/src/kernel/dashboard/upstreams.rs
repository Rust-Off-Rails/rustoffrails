//! Upstreams: the APIs this app calls, from the outbound wrapper (`kernel::outbound`). Per
//! service, method and route template: calls, p50/p95/p99, error and timeout rates, retries,
//! which of the app's routes call it (and how much of their time it takes), a per-minute trend;
//! a dependency map (app routes → upstreams, edges as thick as their calls, colored by health);
//! and the insights: a slow upstream (past its SLO), a failing one, a retry storm, a call with no
//! timeout, and calls made one after another that could run concurrently.
//!
//! The wrapper reports each finished call to `observe` (registered in `instrument`), from the
//! caller's task, so the request (or job) that made it is known. Its spans (`http`,
//! `http_attempt`) are what the waterfall draws.
//!
//! GUIDE: .offrails/guides/errors-and-logging.md#logging

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::hist::Hist;
use super::insights::{Finding, Severity};
use super::trace::Trace;
use super::waterfall;
use super::web::{Dash, Mode, chrome, count, dash, micros};
use super::{HUB, ai, lock};
use crate::kernel::flash::RequestFlash;
use crate::kernel::outbound::{CallRecord, Outcome};
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui;
use crate::state::AppState;

/// An upstream whose p95 is past this is slow. APP: per service in `SLOS`.
pub const SLO_P95_US: u64 = 300_000;
/// APP: per-service SLOs (p95, µs), first match wins; others get `SLO_P95_US`.
pub const SLOS: &[(&str, u64)] = &[("payments", 300_000), ("shipping", 400_000)];
/// An upstream failing this share of its calls (or more) is failing. APP.
pub const FAILING_SHARE: f64 = 0.10;
/// Calls before a rate means anything. APP.
pub const MIN_CALLS: u64 = 5;
/// Extra attempts per call past this is a retry storm. APP.
pub const RETRY_STORM: f64 = 0.30;
/// Calls run one after another that would save at least this much if run together. APP.
pub const SEQUENTIAL_SAVING_US: u64 = 20_000;

/// Distinct upstreams kept (service × method × route); past it, new ones are counted, not kept.
pub const MAX_UPSTREAMS: usize = 100;
/// App routes kept per upstream.
pub const MAX_CALLERS: usize = 20;
/// Minutes of per-upstream trend.
pub const MINUTES: usize = 30;

/// The SLO (p95, µs) of a service.
pub fn slo(service: &str) -> u64 {
    SLOS.iter()
        .find(|(s, _)| *s == service)
        .map_or(SLO_P95_US, |(_, us)| *us)
}

/// An app route that calls an upstream.
#[derive(Clone)]
pub struct Caller {
    pub method: &'static str,
    pub route: &'static str,
    pub kind: &'static str,
    pub calls: u64,
    pub total_us: u64,
}

/// One upstream endpoint.
pub struct Upstream {
    pub service: &'static str,
    pub method: &'static str,
    pub host: String,
    pub route: String,
    pub calls: u64,
    pub errors: u64,
    pub timeouts: u64,
    /// Extra attempts (a call that took 3 tries adds 2).
    pub retries: u64,
    /// Calls made with no timeout at all.
    pub no_timeout: u64,
    /// The timeout of the last call, in ms (0: none).
    pub timeout_ms: u64,
    pub bytes: u64,
    pub hist: Hist,
    pub callers: Vec<Caller>,
    /// (unix minute, calls, errors, total µs), the last `MINUTES`.
    pub minutes: VecDeque<(i64, u32, u32, u64)>,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    /// The slowest calls' traces: (trace, µs), slowest first.
    pub slowest: Vec<(u64, u64)>,
    /// The last failure: (trace, outcome, when).
    pub last_error: Option<(u64, &'static str, DateTime<Utc>)>,
}

impl Upstream {
    pub fn key(&self) -> String {
        format!("{} {} {}", self.service, self.method, self.route)
    }

    pub fn error_rate(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.errors as f64 / self.calls as f64
        }
    }

    pub fn timeout_rate(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.timeouts as f64 / self.calls as f64
        }
    }

    /// `ok`, `warn` or `err`.
    pub fn health(&self) -> &'static str {
        let p95 = self.hist.quantile(0.95);
        let slo = slo(self.service);
        if self.calls >= MIN_CALLS && (self.error_rate() >= FAILING_SHARE || p95 >= 3 * slo) {
            "err"
        } else if self.error_rate() >= 0.02
            || (self.calls >= 3 && p95 >= slo)
            || self.timeouts > 0
            || self.no_timeout > 0
        {
            "warn"
        } else {
            "ok"
        }
    }
}

#[derive(Default)]
pub struct Upstreams {
    map: HashMap<(&'static str, &'static str, String), Upstream>,
    /// Calls to upstreams past `MAX_UPSTREAMS` (counted, not kept).
    pub dropped: u64,
}

impl Upstreams {
    pub fn record(
        &mut self,
        c: &CallRecord,
        caller: Option<(&'static str, &'static str, &'static str, u64)>,
    ) {
        let key = (c.service, c.method, c.route.clone());
        // INVARIANT: bounded (test: the_signals_stay_within_their_memory_bound).
        if !self.map.contains_key(&key) && self.map.len() >= MAX_UPSTREAMS {
            self.dropped += 1;
            return;
        }
        let now = Utc::now();
        let u = self.map.entry(key).or_insert_with(|| Upstream {
            service: c.service,
            method: c.method,
            host: c.host.clone(),
            route: c.route.clone(),
            calls: 0,
            errors: 0,
            timeouts: 0,
            retries: 0,
            no_timeout: 0,
            timeout_ms: 0,
            bytes: 0,
            hist: Hist::default(),
            callers: Vec::new(),
            minutes: VecDeque::with_capacity(MINUTES),
            first: now,
            last: now,
            slowest: Vec::new(),
            last_error: None,
        });
        let us = c.dur.as_micros() as u64;
        let error = c.outcome.is_error(c.status);
        u.calls += 1;
        u.errors += u64::from(error);
        u.timeouts += u64::from(c.outcome == Outcome::Timeout);
        u.retries += u64::from(c.attempts.saturating_sub(1));
        u.no_timeout += u64::from(c.timeout.is_none());
        u.timeout_ms = c.timeout.map_or(0, |t| t.as_millis() as u64);
        u.bytes += c.bytes;
        u.hist.record(us);
        u.last = now;
        u.host.clone_from(&c.host);
        let minute = now.timestamp().div_euclid(60);
        match u.minutes.back_mut() {
            Some(m) if m.0 == minute => {
                m.1 += 1;
                m.2 += u32::from(error);
                m.3 += us;
            }
            _ => {
                if u.minutes.len() == MINUTES {
                    u.minutes.pop_front();
                }
                u.minutes.push_back((minute, 1, u32::from(error), us));
            }
        }
        if let Some((method, route, kind, trace)) = caller {
            if let Some(x) = u
                .callers
                .iter_mut()
                .find(|x| x.method == method && x.route == route)
            {
                x.calls += 1;
                x.total_us += us;
            } else if u.callers.len() < MAX_CALLERS {
                u.callers.push(Caller {
                    method,
                    route,
                    kind,
                    calls: 1,
                    total_us: us,
                });
            }
            if u.slowest.len() < 3 || u.slowest.last().is_some_and(|s| s.1 < us) {
                u.slowest.retain(|s| s.0 != trace);
                u.slowest.push((trace, us));
                u.slowest.sort_by_key(|s| std::cmp::Reverse(s.1));
                u.slowest.truncate(3);
            }
            if error {
                u.last_error = Some((trace, c.outcome.key(), now));
            }
        }
    }

    /// Busiest first.
    pub fn list(&self) -> Vec<&Upstream> {
        let mut v: Vec<&Upstream> = self.map.values().collect();
        v.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.key().cmp(&b.key())));
        v
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn approx_bytes(&self) -> usize {
        self.map
            .values()
            .map(|u| {
                std::mem::size_of::<Upstream>()
                    + Hist::BYTES
                    + u.host.len()
                    + u.route.len()
                    + u.callers.len() * std::mem::size_of::<Caller>()
                    + u.minutes.len() * 32
            })
            .sum()
    }
}

/// The outbound wrapper's observer: every finished call, from the caller's task.
pub fn observe(c: &CallRecord) {
    if !HUB.enabled() {
        return;
    }
    let caller = super::trace::current_ctx().map(|r| (r.method, r.route, r.kind, r.id));
    lock(&HUB.upstreams).record(c, caller);
}

/// The trace id for an outgoing `traceparent`: the request id, as 32 hex digits.
pub fn trace_id() -> Option<String> {
    super::trace::current_ctx().and_then(|r| hex_id(&r.request_id))
}

/// `0b8a7e2c-1f2d-4c3b-9a8e-7d6c5b4a3f21` → `0b8a7e2c1f2d4c3b9a8e7d6c5b4a3f21`.
pub fn hex_id(request_id: &str) -> Option<String> {
    let hex: String = request_id.chars().filter(|c| *c != '-').collect();
    (hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------------------------
// Calls one after another (the waterfall's note and the insight)
// ---------------------------------------------------------------------------------------------

/// Outbound calls under one span that ran one after another, each starting as the previous one
/// ended: they look independent, so running them together would save `saving_us`.
#[derive(Clone, Debug)]
pub struct Sequential {
    /// Where they ran (`handler lab::checkout`).
    pub parent: String,
    /// Each call: (label, start, duration), in order.
    pub calls: Vec<(String, u64, u64)>,
    pub total_us: u64,
    pub longest_us: u64,
    pub saving_us: u64,
}

impl Sequential {
    /// "inventory 41ms → pricing 62ms → payments 1.18s → shipping 80ms".
    pub fn path(&self) -> String {
        self.calls
            .iter()
            .map(|(l, _, d)| format!("{l} {}", micros(*d)))
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

/// A gap this small between two calls means nothing ran between them (no data dependency
/// shows). APP.
pub const SEQUENTIAL_GAP_US: u64 = 5_000;

/// The run of sequential outbound calls with the largest saving in a trace's tree.
pub fn sequential(root: &waterfall::Node) -> Option<Sequential> {
    fn walk(n: &waterfall::Node, best: &mut Option<Sequential>) {
        // INVARIANT: the attempts of one call are sequential by nature (a retry waits for the
        // failure before it): a call's own rows are never a run (test: retry_attempts_are_not_sequential_calls).
        if n.kind == "http" {
            return;
        }
        let mut calls: Vec<(String, u64, u64)> = Vec::new();
        for c in n.children.iter().filter(|c| c.kind == "http" && !c.attempt) {
            if c.group {
                for (s, d) in &c.occ {
                    calls.push((c.short.clone(), *s, *d));
                }
            } else {
                calls.push((c.short.clone(), c.start_us, c.dur_us));
            }
        }
        calls.sort_by_key(|c| c.1);
        let mut run: Vec<(String, u64, u64)> = Vec::new();
        let flush = |run: &mut Vec<(String, u64, u64)>, best: &mut Option<Sequential>| {
            if run.len() >= 2 {
                let total: u64 = run.iter().map(|c| c.2).sum();
                let longest = run.iter().map(|c| c.2).max().unwrap_or(0);
                let saving = total - longest;
                if saving >= SEQUENTIAL_SAVING_US
                    && best.as_ref().is_none_or(|b| saving > b.saving_us)
                {
                    *best = Some(Sequential {
                        parent: n.label.clone(),
                        calls: run.clone(),
                        total_us: total,
                        longest_us: longest,
                        saving_us: saving,
                    });
                }
            }
            run.clear();
        };
        // A call that overlapped another ran in a concurrent group (a `join!`); whatever comes
        // right after the group waits for all of it on purpose, so a group breaks a run.
        let overlapped: Vec<bool> = calls
            .iter()
            .enumerate()
            .map(|(i, a)| {
                calls
                    .iter()
                    .enumerate()
                    .any(|(j, b)| i != j && a.1 < b.1 + b.2 && b.1 < a.1 + a.2)
            })
            .collect();
        for (c, grouped) in calls.into_iter().zip(overlapped) {
            if grouped {
                flush(&mut run, best);
                continue;
            }
            let follows = run
                .last()
                .is_some_and(|p| c.1 >= p.1 + p.2 && c.1 - (p.1 + p.2) <= SEQUENTIAL_GAP_US);
            if !follows {
                flush(&mut run, best);
            }
            run.push(c);
        }
        flush(&mut run, best);
        for c in &n.children {
            walk(c, best);
        }
    }
    let mut best = None;
    walk(root, &mut best);
    best
}

/// Outbound calls in a trace that overlapped (ran concurrently): how many, at most at once.
pub fn concurrent(root: &waterfall::Node) -> usize {
    fn walk(n: &waterfall::Node, best: &mut usize) {
        if n.kind == "http" {
            return;
        }
        let calls: Vec<(u64, u64)> = n
            .children
            .iter()
            .filter(|c| c.kind == "http" && !c.group && !c.attempt)
            .map(|c| (c.start_us, c.start_us + c.dur_us))
            .collect();
        for (s, _) in &calls {
            let at_once = calls.iter().filter(|(a, b)| a <= s && s < b).count();
            *best = (*best).max(at_once);
        }
        for c in &n.children {
            walk(c, best);
        }
    }
    let mut best = 0;
    walk(root, &mut best);
    best
}

// ---------------------------------------------------------------------------------------------
// Insights
// ---------------------------------------------------------------------------------------------

const GUIDE: &str = ".offrails/guides/errors-and-logging.md#logging";
const GUIDE_URL: &str = "https://rustoffrails.org/docs/guides/errors-and-logging#logging";

/// The calling route's share of time spent in this upstream (0..=1).
fn share_of(c: &Caller) -> f64 {
    let http = lock(&HUB.http);
    let total = http.get(c.method, c.route).map_or(0, |r| r.hist.sum());
    if total == 0 {
        0.0
    } else {
        (c.total_us as f64 / total as f64).min(1.0)
    }
}

/// Every upstream finding (ranked with the others by `insights::findings`).
pub fn findings(traces: &[Arc<Trace>]) -> Vec<Finding> {
    let mut out = Vec::new();
    struct Snap {
        key: String,
        service: &'static str,
        method: &'static str,
        route: String,
        calls: u64,
        errors: u64,
        timeouts: u64,
        retries: u64,
        no_timeout: u64,
        p95: u64,
        top: Option<Caller>,
        slow_trace: Option<u64>,
    }
    let ups: Vec<Snap> = {
        let u = lock(&HUB.upstreams);
        u.list()
            .into_iter()
            .map(|x| Snap {
                key: x.key(),
                service: x.service,
                method: x.method,
                route: x.route.clone(),
                calls: x.calls,
                errors: x.errors,
                timeouts: x.timeouts,
                retries: x.retries,
                no_timeout: x.no_timeout,
                p95: x.hist.quantile(0.95),
                top: x.callers.iter().max_by_key(|c| c.total_us).cloned(),
                slow_trace: x.slowest.first().map(|s| s.0),
            })
            .collect()
    };
    for Snap {
        key,
        service,
        method,
        route,
        calls,
        errors,
        timeouts,
        retries,
        no_timeout,
        p95,
        top,
        slow_trace,
    } in ups
    {
        let name = format!("{service} {method} {route}");
        let href = format!(
            "/dev/dashboard/upstreams?service={}",
            crate::kernel::cookie::encode(service)
        );
        let traces_href = slow_trace.map_or_else(
            || {
                format!(
                    "/dev/dashboard/traces?upstream={}",
                    crate::kernel::cookie::encode(service)
                )
            },
            |t| format!("/dev/dashboard/traces/{t}"),
        );
        let by = top.as_ref().map_or(String::new(), |c| {
            format!(
                " — called by {} {}, {:.0}% of its time",
                c.method,
                c.route,
                share_of(c) * 100.0
            )
        });
        let target = slo(service);
        if calls >= 3 && p95 >= target {
            out.push(Finding {
                key: format!("up-slow-{key}"),
                kind: "upstream",
                severity: if p95 >= 3 * target { Severity::High } else { Severity::Medium },
                score: p95 / 1000 * calls.min(100),
                title: format!("Slow upstream: {name} p95 {} (SLO {})", micros(p95), micros(target)),
                evidence: format!("{calls} calls, p95 {}{by}", micros(p95)),
                fix: format!("Ask {service}'s owners about it, cache what it answers when you can, give the call a timeout near its SLO, and run it concurrently with the others (tokio::join!) so it is not on the critical path of every request."),
                guide: GUIDE,
                guide_url: GUIDE_URL,
                href: traces_href.clone(),
            });
        }
        if calls >= MIN_CALLS && errors as f64 / calls as f64 >= FAILING_SHARE {
            out.push(Finding {
                key: format!("up-fail-{key}"),
                kind: "upstream",
                severity: if errors * 4 >= calls { Severity::High } else { Severity::Medium },
                score: errors * 10,
                title: format!("Failing upstream: {name} fails {:.0}% of calls", errors as f64 * 100.0 / calls as f64),
                evidence: format!("{errors} of {calls} calls failed ({timeouts} timed out){by}"),
                fix: "Handle its failure in the caller (a clear error, a fallback, or a job that retries later) instead of a 500; check its status page; retry only what is safe to send twice.".into(),
                guide: ".offrails/guides/errors-and-logging.md#response-mapping",
                guide_url: "https://rustoffrails.org/docs/guides/errors-and-logging#response-mapping",
                href: href.clone(),
            });
        }
        if calls >= MIN_CALLS && retries as f64 / calls as f64 >= RETRY_STORM {
            out.push(Finding {
                key: format!("up-retry-{key}"),
                kind: "upstream",
                severity: Severity::Medium,
                score: retries,
                title: format!("Retry storm on {name}: {:.1} attempts per call", 1.0 + retries as f64 / calls as f64),
                evidence: format!("{retries} retries for {calls} calls: every retry is more load on an API that is already struggling"),
                fix: "Keep retries to one or two with backoff and jitter (the wrapper's default), retry only timeouts and 429/502/503/504, and stop retrying when it keeps failing (a circuit breaker: skip calls for a minute after N failures).".into(),
                guide: GUIDE,
                guide_url: GUIDE_URL,
                href: href.clone(),
            });
        }
        if no_timeout > 0 {
            out.push(Finding {
                key: format!("up-no-timeout-{key}"),
                kind: "upstream",
                severity: Severity::Medium,
                score: no_timeout,
                title: format!("Missing timeout: {name}"),
                evidence: format!("{no_timeout} of {calls} calls ran with no timeout: a hung {service} would hold the request, and a connection, forever"),
                fix: "Drop `without_timeout()`: the outbound wrapper's default (10 s) applies, or set one near the API's SLO with `.timeout(Duration::from_millis(…))`.".into(),
                guide: GUIDE,
                guide_url: GUIDE_URL,
                href: href.clone(),
            });
        }
    }
    // Calls one after another: the most recent trace per route that shows it.
    let mut seen: Vec<&'static str> = Vec::new();
    for t in traces
        .iter()
        .filter(|t| t.spans.iter().any(|s| s.name == "http"))
        .take(200)
    {
        if seen.contains(&t.route) {
            continue;
        }
        let root = waterfall::tree(t, false);
        if let Some(seq) = sequential(&root) {
            seen.push(t.route);
            out.push(Finding {
                key: format!("up-seq-{} {}", t.method, t.route),
                kind: "upstream",
                severity: if seq.saving_us >= 200_000 { Severity::High } else { Severity::Medium },
                score: seq.saving_us / 1000,
                title: format!("Sequential calls that could be parallel on {} {}", t.method, t.route),
                evidence: format!(
                    "{} calls run one after another ({}); running them concurrently would save ~{} ({} instead of {})",
                    seq.calls.len(),
                    seq.path(),
                    micros(seq.saving_us),
                    micros(seq.longest_us),
                    micros(seq.total_us)
                ),
                fix: "Start the independent calls together: `let (a, b, c) = tokio::join!(inventory.get(…).send(), pricing.get(…).send(), shipping.post(…).send());` (or `try_join!`), then use the answers.".into(),
                guide: GUIDE,
                guide_url: GUIDE_URL,
                href: format!("/dev/dashboard/traces/{}", t.id),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------------------------

pub struct UpRow {
    pub service: &'static str,
    pub method: &'static str,
    pub host: String,
    pub route: String,
    pub calls: u64,
    pub p50: String,
    pub p95: String,
    pub p99: String,
    pub slo: String,
    pub over_slo: bool,
    pub errors: String,
    pub timeouts: String,
    pub retries: u64,
    pub timeout: String,
    pub health: &'static str,
    pub callers: Vec<(String, String, String)>,
    pub spark: super::charts::Spark,
    pub traces: String,
    pub last: String,
}

/// One node of the dependency map.
pub struct MapNode {
    pub x: String,
    pub y: String,
    pub w: String,
    pub tx: String,
    pub ty: String,
    pub label: String,
    pub sub: String,
    pub href: String,
    pub class: &'static str,
}

/// One edge: a cubic curve from a route to an upstream.
pub struct MapEdge {
    pub d: String,
    pub width: String,
    pub class: &'static str,
    pub title: String,
}

/// The dependency map: app routes on the left, upstreams on the right (server-rendered SVG, like
/// the charts and the schema diagram).
pub struct DepMap {
    pub height: u32,
    pub routes: Vec<MapNode>,
    pub ups: Vec<MapNode>,
    pub edges: Vec<MapEdge>,
    /// The text alternative: "GET /dev/lab/checkout → payments POST /charge: 40 calls, p95 1.2s".
    pub lines: Vec<String>,
}

pub const MAP_W: u32 = 760;
const ROW_H: u32 = 52;

pub fn dep_map(list: &[&Upstream], base: &str) -> DepMap {
    let mut routes: Vec<(&'static str, &'static str)> = Vec::new();
    for u in list {
        for c in &u.callers {
            if !routes.contains(&(c.method, c.route)) {
                routes.push((c.method, c.route));
            }
        }
    }
    routes.sort();
    let rows = routes.len().max(list.len()).max(1) as u32;
    let height = rows * ROW_H + 16;
    let y_of = |i: usize, n: usize| -> f64 {
        // Centered vertically when one column is shorter.
        let pad = (rows as f64 - n as f64) * ROW_H as f64 / 2.0;
        8.0 + pad + i as f64 * ROW_H as f64 + ROW_H as f64 / 2.0
    };
    let (lx, lw, rx, rw) = (8.0, 250.0, 470.0, 282.0);
    let route_nodes: Vec<MapNode> = routes
        .iter()
        .enumerate()
        .map(|(i, (m, r))| {
            let y = y_of(i, routes.len());
            MapNode {
                x: format!("{lx:.0}"),
                y: format!("{:.1}", y - 18.0),
                w: format!("{lw:.0}"),
                tx: format!("{:.0}", lx + 10.0),
                ty: format!("{:.1}", y - 2.0),
                label: ui::truncate(&format!("{m} {r}"), 34),
                sub: "app route".into(),
                href: format!("{base}/traces?route={}", crate::kernel::cookie::encode(r)),
                class: "route",
            }
        })
        .collect();
    let max_calls = list
        .iter()
        .flat_map(|u| u.callers.iter().map(|c| c.calls))
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let mut edges = Vec::new();
    let mut lines = Vec::new();
    let up_nodes: Vec<MapNode> = list
        .iter()
        .enumerate()
        .map(|(j, u)| {
            let y2 = y_of(j, list.len());
            for c in &u.callers {
                let Some(i) = routes.iter().position(|r| *r == (c.method, c.route)) else {
                    continue;
                };
                let y1 = y_of(i, routes.len());
                let (x1, x2) = (lx + lw, rx);
                let mid = (x1 + x2) / 2.0;
                let w = 1.5 + 7.0 * ((c.calls as f64).ln_1p() / max_calls.ln_1p());
                edges.push(MapEdge {
                    d: format!(
                        "M{x1:.1},{y1:.1} C{mid:.1},{y1:.1} {mid:.1},{y2:.1} {x2:.1},{y2:.1}"
                    ),
                    width: format!("{w:.1}"),
                    class: u.health(),
                    title: format!(
                        "{} {} → {} {} {}: {}",
                        c.method,
                        c.route,
                        u.service,
                        u.method,
                        u.route,
                        count(c.calls, "call", "calls")
                    ),
                });
                lines.push(format!(
                    "{} {} → {} {} {}: {}, p95 {}, {} ({})",
                    c.method,
                    c.route,
                    u.service,
                    u.method,
                    u.route,
                    count(c.calls, "call", "calls"),
                    micros(u.hist.quantile(0.95)),
                    count(u.errors, "error", "errors"),
                    match u.health() {
                        "err" => "failing or very slow",
                        "warn" => "slow or flaky",
                        _ => "healthy",
                    }
                ));
            }
            MapNode {
                x: format!("{rx:.0}"),
                y: format!("{:.1}", y2 - 18.0),
                w: format!("{rw:.0}"),
                tx: format!("{:.0}", rx + 10.0),
                ty: format!("{:.1}", y2 - 2.0),
                label: ui::truncate(&format!("{} {} {}", u.service, u.method, u.route), 36),
                sub: format!(
                    "{} · p95 {} · {}",
                    ui::truncate(&u.host, 22),
                    micros(u.hist.quantile(0.95)),
                    count(u.calls, "call", "calls")
                ),
                href: format!(
                    "{base}/traces?upstream={}",
                    crate::kernel::cookie::encode(u.service)
                ),
                class: u.health(),
            }
        })
        .collect();
    DepMap {
        height,
        routes: route_nodes,
        ups: up_nodes,
        edges,
        lines,
    }
}

fn pct(v: f64) -> String {
    if v > 0.0 && v < 0.01 {
        format!("{:.1}%", v * 100.0)
    } else {
        format!("{:.0}%", v * 100.0)
    }
}

pub fn rows(list: &[&Upstream], base: &str) -> Vec<UpRow> {
    let now_minute = Utc::now().timestamp().div_euclid(60);
    list.iter()
        .map(|u| {
            let slo = slo(u.service);
            let p95 = u.hist.quantile(0.95);
            // Calls per minute over the last 30 minutes (zeros where none).
            let trend: Vec<f64> = (0..MINUTES as i64)
                .rev()
                .map(|ago| {
                    let m = now_minute - ago;
                    u.minutes
                        .iter()
                        .find(|x| x.0 == m)
                        .map_or(0.0, |x| f64::from(x.1))
                })
                .collect();
            UpRow {
                service: u.service,
                method: u.method,
                host: u.host.clone(),
                route: u.route.clone(),
                calls: u.calls,
                p50: micros(u.hist.quantile(0.50)),
                p95: micros(p95),
                p99: micros(u.hist.quantile(0.99)),
                slo: micros(slo),
                over_slo: u.calls >= 3 && p95 >= slo,
                errors: pct(u.error_rate()),
                timeouts: pct(u.timeout_rate()),
                retries: u.retries,
                timeout: if u.no_timeout > 0 {
                    "none".into()
                } else {
                    micros(u.timeout_ms * 1000)
                },
                health: u.health(),
                callers: u
                    .callers
                    .iter()
                    .map(|c| {
                        (
                            format!("{} {}", c.method, c.route),
                            format!(
                                "{base}/traces?route={}",
                                crate::kernel::cookie::encode(c.route)
                            ),
                            format!("×{} · {:.0}% of its time", c.calls, share_of(c) * 100.0),
                        )
                    })
                    .collect(),
                spark: super::charts::sparkline(&trend),
                traces: format!(
                    "{base}/traces?upstream={}",
                    crate::kernel::cookie::encode(u.service)
                ),
                last: ui::ago(u.last, Utc::now()),
            }
        })
        .collect()
}

#[derive(Deserialize, Default)]
pub struct UpQuery {
    pub service: Option<String>,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/upstreams.html")]
struct UpstreamsPage {
    chrome: ui::Chrome,
    d: Dash,
    base: &'static str,
    rows: Vec<UpRow>,
    map: DepMap,
    map_w: u32,
    service: String,
    services: Vec<(&'static str, String, bool)>,
    seq: Vec<(String, String, String)>,
    dropped: u64,
}

/// `GET …/upstreams`: every upstream, the dependency map, and the calls that could run together.
pub async fn upstreams_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<UpQuery>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let service = q.service.unwrap_or_default();
    let (rows, map, services, dropped) = {
        let u = lock(&HUB.upstreams);
        let all = u.list();
        let mut services: Vec<&'static str> = all.iter().map(|x| x.service).collect();
        services.sort_unstable();
        services.dedup();
        let shown: Vec<&Upstream> = all
            .iter()
            .copied()
            .filter(|x| service.is_empty() || x.service == service)
            .collect();
        (
            rows(&shown, mode.base),
            dep_map(&shown, mode.base),
            services
                .into_iter()
                .map(|s| {
                    (
                        s,
                        format!(
                            "{}/upstreams?service={}",
                            mode.base,
                            crate::kernel::cookie::encode(s)
                        ),
                        s == service,
                    )
                })
                .collect(),
            u.dropped,
        )
    };
    let seq = {
        let traces = lock(&HUB.traces).list(false);
        let mut out = Vec::new();
        let mut seen: Vec<&'static str> = Vec::new();
        for t in traces
            .iter()
            .filter(|t| t.spans.iter().any(|s| s.name == "http"))
            .take(200)
        {
            if seen.contains(&t.route) {
                continue;
            }
            if let Some(s) = sequential(&waterfall::tree(t, false)) {
                seen.push(t.route);
                out.push((
                    format!("{} {}", t.method, t.route),
                    format!(
                        "{} calls one after another ({}): concurrently ~{} instead of {}, saving ~{}",
                        s.calls.len(),
                        s.path(),
                        micros(s.longest_us),
                        micros(s.total_us),
                        micros(s.saving_us)
                    ),
                    format!("{}/traces/{}", mode.base, t.id),
                ));
            }
        }
        out
    };
    ui::page(UpstreamsPage {
        chrome: chrome(&state, "Upstreams", "dashboard", flash, theme),
        d: dash(mode, "/upstreams", None, 0).ai(&state.config, ai::Tab::Upstreams, None),
        base: mode.base,
        rows,
        map,
        map_w: MAP_W,
        service,
        services,
        seq,
        dropped,
    })
}

// ---------------------------------------------------------------------------------------------
// For Ask AI (already redacted: hosts, route templates and numbers)
// ---------------------------------------------------------------------------------------------

/// `list_upstreams`: one line per upstream, slowest p95 first.
pub fn list_text() -> String {
    let u = lock(&HUB.upstreams);
    let mut list = u.list();
    if list.is_empty() {
        return "No outbound calls recorded yet. The app calls APIs through `kernel::outbound` (the load lab's Checkout fan-out makes some).".into();
    }
    list.sort_by_key(|x| std::cmp::Reverse(x.hist.quantile(0.95)));
    let mut out = String::from(
        "service method route (host): calls, p50/p95/p99, SLO, errors, timeouts, retries, timeout, health, top callers\n",
    );
    for x in list.iter().take(30) {
        let callers = x
            .callers
            .iter()
            .take(3)
            .map(|c| {
                format!(
                    "[route:{} {}] ×{} ({:.0}% of its time)",
                    c.method,
                    c.route,
                    c.calls,
                    share_of(c) * 100.0
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "- {} {} {} ({}): {} calls, {}/{}/{}, SLO {}, {} errors ({}), {} timeouts, {} retries, timeout {}, {}; called by {}",
            x.service,
            x.method,
            x.route,
            x.host,
            x.calls,
            micros(x.hist.quantile(0.5)),
            micros(x.hist.quantile(0.95)),
            micros(x.hist.quantile(0.99)),
            micros(slo(x.service)),
            x.errors,
            pct(x.error_rate()),
            x.timeouts,
            x.retries,
            if x.no_timeout > 0 {
                "NONE".to_owned()
            } else {
                micros(x.timeout_ms * 1000)
            },
            match x.health() {
                "err" => "failing/very slow",
                "warn" => "slow or flaky",
                _ => "healthy",
            },
            if callers.is_empty() {
                "(outside a request)".into()
            } else {
                callers
            },
        );
    }
    out
}

/// `get_upstream`: one service (every route of it): callers with their share, the trend, the
/// slowest traces, the last failure.
pub fn get_text(service: &str) -> Result<String, String> {
    let u = lock(&HUB.upstreams);
    let list: Vec<&Upstream> = u
        .list()
        .into_iter()
        .filter(|x| x.service == service)
        .collect();
    if list.is_empty() {
        let known: Vec<&str> = u.list().iter().map(|x| x.service).collect();
        return Err(format!(
            "No upstream named {:?}. Known: {}.",
            ui::truncate(service, 40),
            if known.is_empty() {
                "none yet".to_owned()
            } else {
                known.join(", ")
            }
        ));
    }
    let mut out = String::new();
    for x in list {
        let _ = writeln!(
            out,
            "{} {} {} at {}: {} calls, p50 {}, p95 {} (SLO {}), p99 {}, max {}, {} errors, {} timeouts, {} retries, timeout {}, {} received",
            x.service,
            x.method,
            x.route,
            x.host,
            x.calls,
            micros(x.hist.quantile(0.5)),
            micros(x.hist.quantile(0.95)),
            micros(slo(x.service)),
            micros(x.hist.quantile(0.99)),
            micros(x.hist.max()),
            x.errors,
            x.timeouts,
            x.retries,
            if x.no_timeout > 0 {
                "NONE".to_owned()
            } else {
                micros(x.timeout_ms * 1000)
            },
            super::web::bytes(x.bytes),
        );
        for c in &x.callers {
            let _ = writeln!(
                out,
                "  called by [route:{} {}] ({}): {} calls, {} in total, {:.0}% of that route's time",
                c.method,
                c.route,
                c.kind,
                c.calls,
                micros(c.total_us),
                share_of(c) * 100.0
            );
        }
        let trend: Vec<String> = x
            .minutes
            .iter()
            .rev()
            .take(10)
            .rev()
            .map(|(m, n, e, us)| {
                format!(
                    "{} {n} calls {e} errors avg {}",
                    DateTime::from_timestamp(m * 60, 0)
                        .map_or_else(String::new, |t| t.format("%H:%M").to_string()),
                    micros(us / u64::from((*n).max(1)))
                )
            })
            .collect();
        let _ = writeln!(out, "  per minute (UTC): {}", trend.join("; "));
        let slow: Vec<String> = x
            .slowest
            .iter()
            .map(|(t, us)| format!("[trace:{t}] {}", micros(*us)))
            .collect();
        if !slow.is_empty() {
            let _ = writeln!(out, "  slowest calls: {}", slow.join(", "));
        }
        if let Some((t, outcome, at)) = x.last_error {
            let _ = writeln!(
                out,
                "  last failure: {outcome} in [trace:{t}], {}",
                ui::ago(at, Utc::now())
            );
        }
    }
    Ok(out)
}
