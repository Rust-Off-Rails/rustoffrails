//! Traces, methods and hot functions: the pages.
//!
//! - `…/traces`: the explorer. Filters as dropdowns (route with hit counts, kind, status, minimum
//!   duration, N+1, errors, time range synced with the charts, text on the path, a method), all in
//!   the URL so a view is a link. A duration scatter above the list (brush it to narrow the time;
//!   click a dot to open it); rows that open inline into their waterfall; two rows compared; new
//!   traces streaming in (held while a row is open or the list is being scrolled).
//! - `…/traces/{id}`: one trace's waterfall: the permalink, and the no-JavaScript view.
//! - `…/traces/{id}/waterfall`: the same waterfall as a fragment (rows, drawers, other pages).
//! - `…/traces/{id}/json`: the trace, redacted as recorded (copy as JSON).
//! - `…/traces/compare?cmp=a&cmp=b`: two traces side by side, with what differs.
//! - `…/traces/lookup?job=…|mail=…`: the trace of a job run, or of the request or job that sent a
//!   mail (the jobs dashboard's and the mailbox's pages embed it).
//! - `…/methods`: every span aggregated (profiler style, wall clock) and one route's icicle.
//! - `/dev/dashboard/profile`: hot functions from the CPU sampler and its flame graph.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Form};
use chrono::Utc;
use serde::Deserialize;

use super::methods::{self, MethodStat};
use super::trace::Trace;
use super::waterfall::{self, View};
use super::web::{Dash, Mode, RangeLink, back, chrome, dash, micros};
use super::{HUB, ai, history, lock, profiler, sql};
use crate::kernel::cookie;
use crate::kernel::flash::{Flash, RequestFlash};
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui;
use crate::state::AppState;

/// Rows the explorer lists at most.
pub const LIST: usize = 200;
/// Dots the scatter draws at most.
const DOTS: usize = 600;

// ---------------------------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------------------------

/// The explorer's filters, all optional, all in the URL.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct TraceQuery {
    pub route: Option<String>,
    /// `request`, `job`, `mail`, `cron`.
    pub kind: Option<String>,
    /// `2xx`, `3xx`, `4xx`, `5xx`; the old `n1` still works.
    pub status: Option<String>,
    /// Milliseconds.
    pub min: Option<u64>,
    pub n1: Option<String>,
    pub err: Option<String>,
    /// `5m`, `15m`, `1h`, `24h`, `7d`; empty: everything kept.
    pub range: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Text in the path.
    pub q: Option<String>,
    /// A method (span name) the trace ran.
    #[serde(rename = "fn")]
    pub method: Option<String>,
    pub notable: Option<String>,
    /// An outbound API the trace called (its service name, `payments`).
    pub upstream: Option<String>,
    /// The request id (a browser's report links to the trace of its page or fetch).
    pub rid: Option<String>,
    /// Unix minute (a click on a chart).
    pub minute: Option<i64>,
    pub all: Option<String>,
}

impl TraceQuery {
    fn on(v: &Option<String>) -> bool {
        matches!(v.as_deref(), Some("1" | "on" | "true"))
    }

    /// The time window: an explicit zoom, a chart's minute, or the last `range`.
    pub fn window(&self, now: i64) -> (Option<i64>, Option<i64>) {
        if let Some(m) = self.minute {
            return (Some(m * 60), Some(m * 60 + 59));
        }
        match (self.from, self.to) {
            (Some(f), Some(t)) if t > f => return (Some(f), Some(t)),
            _ => {}
        }
        let secs = match self.range.as_deref() {
            Some("5m") => 300,
            Some("15m") => 900,
            Some("1h") => 3_600,
            Some("24h") => 86_400,
            Some("7d") => 7 * 86_400,
            _ => return (None, None),
        };
        (Some(now - secs), None)
    }

    fn status_class(&self) -> Option<&str> {
        match self.status.as_deref() {
            Some(s @ ("2xx" | "3xx" | "4xx" | "5xx")) => Some(s),
            _ => None,
        }
    }

    fn n_plus_one(&self) -> bool {
        Self::on(&self.n1) || self.status.as_deref() == Some("n1")
    }

    pub fn matches(&self, t: &Trace, now: i64) -> bool {
        let (from, to) = self.window(now);
        let at = t.at.timestamp();
        fn nonempty(v: &Option<String>) -> Option<&str> {
            v.as_deref().filter(|s| !s.is_empty())
        }
        from.is_none_or(|f| at >= f)
            && to.is_none_or(|x| at <= x)
            && nonempty(&self.route).is_none_or(|r| t.route == r)
            && nonempty(&self.kind).is_none_or(|k| t.kind == k)
            && self.status_class().is_none_or(|c| {
                c.as_bytes().first().map(|d| u16::from(d - b'0')) == Some(t.status / 100)
            })
            && self.min.is_none_or(|ms| t.dur_us >= ms * 1_000)
            && (!self.n_plus_one() || !t.n_plus_one.is_empty())
            && (!Self::on(&self.err) || t.status >= 500)
            && (!Self::on(&self.notable) || t.notable())
            && nonempty(&self.upstream).is_none_or(|u| {
                t.spans.iter().any(|s| {
                    s.name == "http"
                        && s.fields.split(' ').any(|kv| {
                            kv.strip_prefix("service=")
                                .is_some_and(|v| v.trim_matches('"') == u)
                        })
                })
            })
            && nonempty(&self.rid).is_none_or(|r| t.request_id == r)
            && nonempty(&self.q).is_none_or(|q| {
                t.path.to_lowercase().contains(&q.to_lowercase())
                    || t.route.to_lowercase().contains(&q.to_lowercase())
            })
            && nonempty(&self.method).is_none_or(|m| {
                t.spans.iter().any(|s| {
                    s.name == m
                        || m.strip_prefix("handler ").is_some_and(|h| {
                            s.name == "handler"
                                && s.fields.split(' ').any(|kv| {
                                    kv.strip_prefix("handler=")
                                        .is_some_and(|v| v.trim_matches('"') == h)
                                })
                        })
                })
            })
    }

    /// As a query string (the stream re-renders the same list; links keep the view).
    pub fn encode(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut put = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                parts.push(format!("{k}={}", cookie::encode(&v)));
            }
        };
        put("route", self.route.clone());
        put("kind", self.kind.clone());
        put("status", self.status.clone());
        put("min", self.min.map(|m| m.to_string()));
        put("n1", self.n1.clone());
        put("err", self.err.clone());
        put("range", self.range.clone());
        put("from", self.from.map(|v| v.to_string()));
        put("to", self.to.map(|v| v.to_string()));
        put("q", self.q.clone());
        put("fn", self.method.clone());
        put("notable", self.notable.clone());
        put("upstream", self.upstream.clone());
        put("rid", self.rid.clone());
        put("minute", self.minute.map(|v| v.to_string()));
        parts.join("&")
    }
}

/// The traces a query selects, newest first: memory's, then the history file's notable ones that
/// memory no longer holds (marked "from disk").
pub fn select(q: &TraceQuery, limit: usize) -> Vec<Arc<Trace>> {
    let now = Utc::now().timestamp();
    let mem: Vec<Arc<Trace>> = lock(&HUB.traces).list(false);
    let mut out: Vec<Arc<Trace>> = mem.iter().filter(|t| q.matches(t, now)).cloned().collect();
    out.truncate(limit);
    if out.len() < limit
        && history::is_open()
        && q.method.as_deref().is_none_or(str::is_empty)
        && q.upstream.as_deref().is_none_or(str::is_empty)
        && q.rid.as_deref().is_none_or(str::is_empty)
    {
        let (from, to) = q.window(now);
        let disk = history::traces(history::TraceFilter {
            from,
            to,
            route: q.route.clone().filter(|r| !r.is_empty()),
            kind: q.kind.clone().filter(|k| !k.is_empty()),
            status: q.status_class().map(str::to_owned),
            min_us: q.min.map(|m| m * 1_000),
            n_plus_one: q.n_plus_one(),
            limit: limit - out.len(),
            exclude: mem.iter().map(|t| t.id).collect(),
            request_id: None,
        });
        out.extend(disk.into_iter().filter(|t| q.matches(t, now)));
        out.sort_by_key(|t| std::cmp::Reverse(t.id));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The explorer
// ---------------------------------------------------------------------------------------------

pub struct Opt {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

fn opts(values: &[(&str, &str)], current: Option<&str>) -> Vec<Opt> {
    values
        .iter()
        .map(|(v, l)| Opt {
            value: (*v).to_owned(),
            label: (*l).to_owned(),
            selected: current.unwrap_or("") == *v,
        })
        .collect()
}

pub struct Row {
    pub id: u64,
    pub when: String,
    pub clock: String,
    pub kind: &'static str,
    pub method: &'static str,
    pub route: &'static str,
    pub path: String,
    pub status: u16,
    pub tone: &'static str,
    pub dur: String,
    pub bar: u64,
    pub sql: u32,
    pub n_plus_one: bool,
    pub notable: bool,
    pub from_disk: bool,
}

pub fn rows(traces: &[Arc<Trace>]) -> Vec<Row> {
    let now = Utc::now();
    let max = traces.iter().map(|t| t.dur_us).max().unwrap_or(1).max(1);
    traces
        .iter()
        .map(|t| Row {
            id: t.id,
            when: ui::ago(t.at, now),
            clock: t.at.format("%H:%M:%S").to_string(),
            kind: t.kind,
            method: t.method,
            route: t.route,
            path: t.path.clone(),
            status: t.status,
            tone: tone(t.status),
            dur: micros(t.dur_us),
            bar: (t.dur_us * 100 / max).max(1),
            sql: t.statements,
            n_plus_one: !t.n_plus_one.is_empty(),
            notable: t.notable(),
            from_disk: t.from_disk,
        })
        .collect()
}

fn tone(status: u16) -> &'static str {
    match status {
        500.. => "err",
        400..=499 => "warn",
        300..=399 => "info",
        _ => "ok",
    }
}

/// A dot of the scatter (percent coordinates: no inline styles under the CSP).
pub struct Dot {
    pub id: u64,
    pub x: String,
    pub y: String,
    pub tone: &'static str,
    pub big: bool,
    pub title: String,
}

pub struct Scatter {
    pub dots: Vec<Dot>,
    pub from: i64,
    pub to: i64,
    pub yticks: Vec<(String, String)>,
    pub xticks: Vec<(String, String)>,
    pub summary: String,
}

/// x: time across the window; y: duration on a log scale (1 ms … the slowest), so a 3 ms request
/// and a 3 s one are both visible.
pub fn scatter(traces: &[Arc<Trace>], from: i64, to: i64) -> Scatter {
    let span = (to - from).max(1) as f64;
    let max_us = traces
        .iter()
        .map(|t| t.dur_us)
        .max()
        .unwrap_or(1_000)
        .max(10_000) as f64;
    let lo = 100_f64.ln();
    let hi = max_us.ln();
    let y = |us: f64| 100.0 - ((us.max(100.0).ln() - lo) / (hi - lo).max(0.1) * 92.0 + 4.0);
    let dots = traces
        .iter()
        .take(DOTS)
        .map(|t| {
            let x = ((t.at.timestamp_millis() as f64 / 1_000.0 - from as f64) / span * 100.0)
                .clamp(0.5, 99.5);
            Dot {
                id: t.id,
                x: format!("{x:.2}%"),
                y: format!("{:.2}%", y(t.dur_us as f64)),
                tone: tone(t.status),
                big: t.notable(),
                title: format!(
                    "{} {} → {} in {} at {}",
                    t.method,
                    t.path,
                    t.status,
                    micros(t.dur_us),
                    t.at.format("%H:%M:%S")
                ),
            }
        })
        .collect::<Vec<_>>();
    let mut yticks = Vec::new();
    for us in [1_000_u64, 10_000, 100_000, 1_000_000, 10_000_000] {
        if (us as f64) <= max_us * 1.01 {
            yticks.push((format!("{:.2}%", y(us as f64)), micros(us)));
        }
    }
    let clock = if to - from < 1_800 {
        "%H:%M:%S"
    } else {
        "%H:%M"
    };
    let xticks = (0..=4)
        .map(|i| {
            let t = from + (to - from) * i / 4;
            (
                format!("{}%", i * 25),
                chrono::DateTime::from_timestamp(t, 0)
                    .map(|d| d.format(clock).to_string())
                    .unwrap_or_default(),
            )
        })
        .collect();
    Scatter {
        summary: format!(
            "{} traces from {} to {} UTC; slowest {}",
            dots.len(),
            chrono::DateTime::from_timestamp(from, 0)
                .map(|d| d.format("%H:%M").to_string())
                .unwrap_or_default(),
            chrono::DateTime::from_timestamp(to, 0)
                .map(|d| d.format("%H:%M").to_string())
                .unwrap_or_default(),
            micros(max_us as u64)
        ),
        dots,
        from,
        to,
        yticks,
        xticks,
    }
}

#[derive(Template)]
#[template(path = "kernel/dashboard/_traces.html")]
pub struct ListFragment {
    pub rows: Vec<Row>,
    pub base: &'static str,
    pub scatter: Scatter,
    pub count: usize,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/traces.html")]
struct ExplorerPage {
    chrome: ui::Chrome,
    d: Dash,
    base: &'static str,
    routes: Vec<Opt>,
    kinds: Vec<Opt>,
    statuses: Vec<Opt>,
    mins: Vec<Opt>,
    ranges: Vec<Opt>,
    q: TraceQuery,
    list_html: String,
    n1: bool,
    err: bool,
    notable: bool,
    text: String,
    method: String,
    kept: String,
    minute: String,
    reset: String,
    methods_href: String,
}

/// The list and scatter for a query (the page and the live stream).
pub fn list(mode: Mode, q: &TraceQuery) -> ListFragment {
    let traces = select(q, LIST);
    let now = Utc::now().timestamp();
    let (from, to) = q.window(now);
    let from = from.unwrap_or_else(|| {
        traces
            .iter()
            .map(|t| t.at.timestamp())
            .min()
            .unwrap_or(now - 900)
    });
    let to = to.unwrap_or(now);
    ListFragment {
        rows: rows(&traces),
        base: mode.base,
        scatter: scatter(&traces, from, to),
        count: traces.len(),
    }
}

pub async fn explorer(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<TraceQuery>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let hits: BTreeMap<&'static str, u64> = lock(&HUB.http)
        .stats()
        .map(|s| (s.route, s.count))
        .fold(BTreeMap::new(), |mut m, (r, c)| {
            *m.entry(r).or_default() += c;
            m
        });
    let mut routes: Vec<Opt> = vec![Opt {
        value: String::new(),
        label: "Any route".into(),
        selected: q.route.as_deref().unwrap_or("").is_empty(),
    }];
    let mut patterns: Vec<&'static str> = crate::routes::ROUTES.iter().map(|r| r.1).collect();
    patterns.extend(hits.keys().copied());
    patterns.sort_unstable();
    patterns.dedup();
    // Routes with traffic first (by hits), then the rest alphabetically.
    patterns.sort_by(|a, b| {
        hits.get(b)
            .unwrap_or(&0)
            .cmp(hits.get(a).unwrap_or(&0))
            .then(a.cmp(b))
    });
    for p in patterns {
        let n = hits.get(p).copied().unwrap_or(0);
        routes.push(Opt {
            value: p.to_owned(),
            label: if n > 0 {
                format!("{p} ({})", ui::thousands(n))
            } else {
                p.to_owned()
            },
            selected: q.route.as_deref() == Some(p),
        });
    }
    let (recent, notable) = lock(&HUB.traces).lens();
    let disk = if history::is_open() {
        " (older notable ones from the history file)"
    } else {
        ""
    };
    let list = list(mode, &q);
    let enc = q.encode();
    let live = format!(
        "{}/stream?view=traces&tq={}",
        mode.base,
        cookie::encode(&enc)
    );
    ui::page(ExplorerPage {
        chrome: chrome(&state, "Traces", "dashboard", flash, theme),
        d: dash(mode, "/traces", Some(live), 2).ai(&state.config, ai::Tab::Traces, None),
        base: mode.base,
        list_html: list.render().unwrap_or_default(),
        routes,
        kinds: opts(
            &[
                ("", "Any kind"),
                ("request", "Requests"),
                ("job", "Jobs"),
                ("mail", "Mail"),
                ("cron", "Scheduled"),
            ],
            q.kind.as_deref(),
        ),
        statuses: opts(
            &[
                ("", "Any status"),
                ("2xx", "2xx"),
                ("3xx", "3xx"),
                ("4xx", "4xx"),
                ("5xx", "5xx"),
            ],
            q.status_class(),
        ),
        mins: opts(
            &[
                ("", "Any duration"),
                ("10", "≥ 10 ms"),
                ("50", "≥ 50 ms"),
                ("100", "≥ 100 ms"),
                ("500", "≥ 500 ms"),
                ("1000", "≥ 1 s"),
            ],
            q.min.map(|m| m.to_string()).as_deref(),
        ),
        ranges: opts(
            &[
                ("", "Everything kept"),
                ("5m", "Last 5 min"),
                ("15m", "Last 15 min"),
                ("1h", "Last hour"),
                ("24h", "Last 24 hours"),
                ("7d", "Last 7 days"),
            ],
            q.range.as_deref(),
        ),
        n1: q.n_plus_one(),
        err: TraceQuery::on(&q.err),
        notable: TraceQuery::on(&q.notable),
        text: q.q.clone().unwrap_or_default(),
        method: q.method.clone().unwrap_or_default(),
        kept: format!("{recent} recent and {notable} notable in memory{disk}"),
        minute: q
            .minute
            .and_then(|m| chrono::DateTime::from_timestamp(m * 60, 0))
            .map(|d| d.format("%H:%M UTC").to_string())
            .or_else(|| match (q.from, q.to) {
                (Some(f), Some(t)) if t > f => Some(format!(
                    "{} – {} UTC",
                    chrono::DateTime::from_timestamp(f, 0)
                        .map(|d| d.format("%H:%M:%S").to_string())
                        .unwrap_or_default(),
                    chrono::DateTime::from_timestamp(t, 0)
                        .map(|d| d.format("%H:%M:%S").to_string())
                        .unwrap_or_default()
                )),
                _ => None,
            })
            .unwrap_or_default(),
        reset: format!("{}/traces", mode.base),
        methods_href: format!("{}/methods", mode.base),
        q,
    })
}

/// A list (and its scatter) of given traces: another page's "traces where this ran".
pub fn list_html(mode: Mode, traces: &[Arc<Trace>]) -> String {
    let now = Utc::now().timestamp();
    let from = traces
        .iter()
        .map(|t| t.at.timestamp())
        .min()
        .unwrap_or(now - 900);
    ListFragment {
        rows: rows(traces),
        base: mode.base,
        scatter: scatter(traces, from - 1, now),
        count: traces.len(),
    }
    .render()
    .unwrap_or_default()
}

/// The stream's `traces` fragment: the same list for the query the page was opened with.
pub fn live_fragment(mode: Mode, tq: Option<&str>) -> Option<String> {
    let q: TraceQuery = serde_html_form::from_str(tq.unwrap_or_default()).unwrap_or_default();
    list(mode, &q).render().ok()
}

// ---------------------------------------------------------------------------------------------
// One trace
// ---------------------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "kernel/dashboard/trace.html")]
struct TracePage {
    chrome: ui::Chrome,
    d: Dash,
    t: Arc<Trace>,
    tone: &'static str,
    when: String,
    w: View,
    similar: String,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/_waterfall_fragment.html")]
pub struct WaterfallFragment {
    pub w: View,
}

#[derive(Debug, Default, Deserialize)]
pub struct OneQuery {
    all: Option<String>,
}

fn ask_href(mode: Mode, id: u64) -> String {
    format!("{}/ai?tab=traces&item={id}", mode.base)
}

pub fn view_of(state: &AppState, mode: Mode, t: &Trace, compact: bool, all: bool) -> View {
    // SECURITY: the production view shows timings and normalized SQL, never log events or span
    // fields (what the app logged: emails, ids, amounts) (test: the_production_trace_page_hides_events_and_fields)
    waterfall::view(
        t,
        !mode.ro,
        mode.base,
        compact,
        all,
        ai::allowed(&state.config, mode.ro),
        ask_href(mode, t.id),
    )
}

pub async fn trace_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Path(id): Path<String>,
    Query(q): Query<OneQuery>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let Some(t) = id.parse::<u64>().ok().and_then(super::trace::find) else {
        return back(
            &format!("{}/traces", mode.base),
            Flash::alert(
                "That trace is no longer kept (the dashboard keeps the latest ones, and notable ones in its history file).",
            ),
        );
    };
    let all = TraceQuery::on(&q.all);
    ui::page(TracePage {
        chrome: chrome(&state, "Trace", "dashboard", flash, theme),
        d: dash(mode, "/traces", None, 0).ai(
            &state.config,
            ai::Tab::Traces,
            Some(t.id.to_string()),
        ),
        tone: tone(t.status),
        when: ui::utc(t.at),
        w: view_of(&state, mode, &t, false, all),
        similar: format!(
            "{}/traces?route={}&kind={}",
            mode.base,
            cookie::encode(t.route),
            t.kind
        ),
        t,
    })
}

pub async fn waterfall_fragment(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Path(id): Path<String>,
) -> Response {
    match id.parse::<u64>().ok().and_then(super::trace::find) {
        Some(t) => fragment(&state, mode, &t),
        None => gone(),
    }
}

fn fragment(state: &AppState, mode: Mode, t: &Trace) -> Response {
    match (WaterfallFragment {
        w: view_of(state, mode, t, true, false),
    })
    .render()
    {
        Ok(html) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn gone() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<p class=\"hint\">That trace is no longer kept.</p>",
    )
        .into_response()
}

/// `…/traces/{id}/json`: the trace as recorded (already redacted); the production view leaves out
/// span fields and log events, as its pages do.
pub async fn trace_json(Extension(mode): Extension<Mode>, Path(id): Path<String>) -> Response {
    match id.parse::<u64>().ok().and_then(super::trace::find) {
        Some(t) => (
            [(header::CONTENT_TYPE, "application/json")],
            history::trace_json(&t, mode.ro),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct LookupQuery {
    job: Option<i64>,
    mail: Option<i64>,
}

/// The trace of a job run (`job-<id>`), or of whatever delivered a mail (its trace logged
/// `message_id=<id>`), as a waterfall fragment.
pub async fn lookup(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<LookupQuery>,
) -> Response {
    let mem = lock(&HUB.traces).list(false);
    let found = match (q.job, q.mail) {
        (Some(job), _) => {
            let id = format!("job-{job}");
            mem.into_iter().find(|t| t.request_id == id).or_else(|| {
                // Job runs are kept on disk too (memory's ring turns over in seconds under
                // traffic).
                history::traces(history::TraceFilter {
                    request_id: Some(id),
                    limit: 1,
                    ..history::TraceFilter::default()
                })
                .pop()
            })
        }
        (None, Some(mail)) => {
            let needle = format!("message_id={mail}");
            mem.into_iter().find(|t| {
                t.events.iter().any(|e| {
                    e.message
                        .split(' ')
                        .any(|w| w == needle || w.trim_end_matches(',') == needle)
                })
            })
        }
        _ => None,
    };
    match found {
        Some(t) => fragment(&state, mode, &t),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<p class=\"hint\">No trace kept for it (traces are kept for the latest requests and jobs, and notable ones on disk).</p>",
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------------------------
// Compare
// ---------------------------------------------------------------------------------------------

pub struct DiffRow {
    pub label: String,
    pub kind: &'static str,
    pub a: u32,
    pub b: u32,
    pub delta: String,
    pub tone: &'static str,
    pub a_us: String,
    pub b_us: String,
}

/// What differs between two traces: per span name and per statement (by its one-line summary),
/// counts and time, biggest change first; and the headline ("B ran 20 more queries").
pub fn diff(a: &Trace, b: &Trace) -> (Vec<DiffRow>, Vec<String>) {
    let mut by: BTreeMap<(String, &'static str), (u32, u32, u64, u64)> = BTreeMap::new();
    for (side, t) in [(0, a), (1, b)] {
        for s in &t.spans {
            let e = by
                .entry((
                    s.name.to_owned(),
                    waterfall::span_kind(s.name, s.target, &s.fields),
                ))
                .or_default();
            if side == 0 {
                e.0 += 1;
                e.2 += s.dur_us;
            } else {
                e.1 += 1;
                e.3 += s.dur_us;
            }
        }
        for q in &t.sql {
            let e = by
                .entry((waterfall::sql_summary(&q.sql), "sql"))
                .or_default();
            if side == 0 {
                e.0 += 1;
                e.2 += q.dur_us;
            } else {
                e.1 += 1;
                e.3 += q.dur_us;
            }
        }
    }
    let mut rows: Vec<DiffRow> = by
        .into_iter()
        .map(|((label, kind), (ca, cb, ua, ub))| {
            let d = i64::from(cb) - i64::from(ca);
            DiffRow {
                label,
                kind,
                a: ca,
                b: cb,
                delta: if d > 0 {
                    format!("+{d}")
                } else if d < 0 {
                    d.to_string()
                } else {
                    "=".into()
                },
                tone: if d > 0 {
                    "warn"
                } else if d < 0 {
                    "ok"
                } else {
                    "muted"
                },
                a_us: micros(ua),
                b_us: micros(ub),
            }
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse((i64::from(r.a) - i64::from(r.b)).abs()));
    let mut lines = Vec::new();
    let dq = i64::from(b.statements) - i64::from(a.statements);
    if dq != 0 {
        lines.push(format!(
            "#{} ran {} {} {} than #{}",
            b.id,
            dq.abs(),
            if dq > 0 { "more" } else { "fewer" },
            if dq.abs() == 1 { "query" } else { "queries" },
            a.id
        ));
    }
    let dt = b.dur_us as i64 - a.dur_us as i64;
    lines.push(format!(
        "#{} took {} {} than #{} ({} vs {})",
        b.id,
        micros(dt.unsigned_abs()),
        if dt >= 0 { "longer" } else { "less" },
        a.id,
        micros(b.dur_us),
        micros(a.dur_us)
    ));
    let ds = b.spans.len() as i64 - a.spans.len() as i64;
    if ds != 0 {
        lines.push(format!(
            "#{} opened {} {} {}",
            b.id,
            ds.abs(),
            if ds > 0 { "more" } else { "fewer" },
            if ds.abs() == 1 { "span" } else { "spans" }
        ));
    }
    if a.route != b.route {
        lines.push(format!(
            "Different routes ({} vs {}): compare two runs of the same route for a fair diff",
            a.route, b.route
        ));
    }
    (rows, lines)
}

#[derive(Template)]
#[template(path = "kernel/dashboard/trace_compare.html")]
struct ComparePage {
    chrome: ui::Chrome,
    d: Dash,
    a: View,
    b: View,
    ta: Arc<Trace>,
    tb: Arc<Trace>,
    rows: Vec<DiffRow>,
    lines: Vec<String>,
    base: &'static str,
}

pub async fn compare(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    RawQuery(raw): RawQuery,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let ids: Vec<u64> = raw
        .unwrap_or_default()
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| matches!(*k, "cmp" | "a" | "b"))
        .filter_map(|(_, v)| v.parse().ok())
        .take(2)
        .collect();
    let found: Vec<Arc<Trace>> = ids
        .iter()
        .filter_map(|id| super::trace::find(*id))
        .collect();
    let [ta, tb] = found.as_slice() else {
        return back(
            &format!("{}/traces", mode.base),
            Flash::alert("Pick two traces to compare (tick two rows, then Compare)."),
        );
    };
    // The older one on the left.
    let (ta, tb) = if ta.id <= tb.id {
        (ta.clone(), tb.clone())
    } else {
        (tb.clone(), ta.clone())
    };
    let (rows, lines) = diff(&ta, &tb);
    ui::page(ComparePage {
        chrome: chrome(&state, "Compare traces", "dashboard", flash, theme),
        d: dash(mode, "/traces", None, 0).ai(&state.config, ai::Tab::Traces, None),
        a: view_of(&state, mode, &ta, true, false),
        b: view_of(&state, mode, &tb, true, false),
        ta,
        tb,
        rows,
        lines,
        base: mode.base,
    })
}

// ---------------------------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------------------------

pub struct MethodRow {
    pub name: &'static str,
    pub module: &'static str,
    pub loc: String,
    pub kind: &'static str,
    pub calls: String,
    pub per_trace: String,
    pub total: String,
    pub selft: String,
    pub p50: String,
    pub p95: String,
    pub max: String,
    pub share: String,
    pub bar: u64,
    pub routes: Vec<(&'static str, u64)>,
    pub callers: Vec<(String, u64)>,
    pub callees: Vec<(String, u64)>,
    pub href: String,
    pub slow: bool,
}

fn method_rows(stats: &[MethodStat], all_us: u64, base: &str) -> Vec<MethodRow> {
    let max = stats.iter().map(|m| m.total_us).max().unwrap_or(1).max(1);
    stats
        .iter()
        .map(|m| MethodRow {
            name: m.name,
            // The crate's name is the same on every row: leave it out.
            module: m.module.split_once("::").map_or(m.module, |(_, rest)| rest),
            loc: m.loc.clone(),
            kind: m.kind,
            calls: ui::thousands(m.calls),
            per_trace: format!("{:.1}", m.per_trace()),
            total: micros(m.total_us),
            selft: micros(m.self_us),
            p50: micros(m.p50()),
            p95: micros(m.p95()),
            max: micros(m.max()),
            share: format!("{:.1}%", m.total_us as f64 / all_us.max(1) as f64 * 100.0),
            bar: (m.total_us * 100 / max).max(1),
            routes: {
                let mut r: Vec<(&'static str, u64)> =
                    m.routes.iter().map(|(k, v)| (*k, *v)).collect();
                r.sort_by_key(|a| std::cmp::Reverse(a.1));
                r.truncate(5);
                r
            },
            callers: MethodStat::top(&m.callers, 4),
            callees: MethodStat::top(&m.callees, 6),
            href: format!("{base}/traces?fn={}", cookie::encode(m.name)),
            slow: m.p95() >= super::insights::SLOW_METHOD_US,
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
pub struct MethodsQuery {
    sort: Option<String>,
    route: Option<String>,
    focus: Option<String>,
    range: Option<String>,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/methods.html")]
struct MethodsPage {
    chrome: ui::Chrome,
    d: Dash,
    base: &'static str,
    rows: Vec<MethodRow>,
    sorts: Vec<RangeLink>,
    ranges: Vec<Opt>,
    routes: Vec<Opt>,
    icicle: Icicle,
    traces: usize,
    route: String,
}

/// An icicle ready to draw (`_icicle.html`).
pub struct Icicle {
    pub cells: Vec<methods::Cell>,
    pub height: u32,
    pub crumbs: Vec<(String, String)>,
    pub href: String,
    pub empty: bool,
    pub label: String,
}

pub fn icicle_view(
    root: &methods::Frame,
    focus: &str,
    href: &str,
    unit: fn(u64) -> String,
    label: &str,
) -> Icicle {
    let (cells, depth) = methods::cells(root, focus, unit);
    let mut crumbs = vec![("all".to_owned(), String::new())];
    let mut acc = String::new();
    for part in focus.split(';').filter(|p| !p.is_empty()) {
        if !acc.is_empty() {
            acc.push(';');
        }
        acc.push_str(part);
        crumbs.push((part.to_owned(), acc.clone()));
    }
    Icicle {
        empty: root.value == 0,
        height: depth.max(1) * 20,
        cells,
        crumbs,
        href: href.to_owned(),
        label: label.to_owned(),
    }
}

/// Memory's traces in the range (the methods view aggregates what memory holds: the latest).
fn traces_in(range: Option<&str>) -> Vec<Arc<Trace>> {
    let now = Utc::now().timestamp();
    let q = TraceQuery {
        range: range.map(str::to_owned),
        ..TraceQuery::default()
    };
    lock(&HUB.traces)
        .list(false)
        .into_iter()
        .filter(|t| q.matches(t, now))
        .collect()
}

/// The methods over memory's traces, slowest total first (the overview card, the insights).
pub fn method_stats() -> (Vec<MethodStat>, u64) {
    methods::aggregate(&traces_in(None))
}

pub async fn methods_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<MethodsQuery>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let traces = traces_in(q.range.as_deref().filter(|r| !r.is_empty()));
    let (mut stats, all_us) = methods::aggregate(&traces);
    let sort = q.sort.as_deref().unwrap_or("total");
    match sort {
        "self" => stats.sort_by_key(|m| std::cmp::Reverse(m.self_us)),
        "calls" => stats.sort_by_key(|m| std::cmp::Reverse(m.calls)),
        "p95" => stats.sort_by_key(|m| std::cmp::Reverse(m.p95())),
        "max" => stats.sort_by_key(|m| std::cmp::Reverse(m.max())),
        _ => {}
    }
    // The icicle: the chosen route, else the busiest one.
    let mut by_route: HashMap<&'static str, usize> = HashMap::new();
    for t in &traces {
        *by_route.entry(t.route).or_default() += 1;
    }
    let route = q
        .route
        .clone()
        .filter(|r| !r.is_empty())
        .or_else(|| {
            by_route
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(r, _)| (*r).to_owned())
        })
        .unwrap_or_default();
    let of_route: Vec<Arc<Trace>> = traces
        .iter()
        .filter(|t| t.route == route)
        .cloned()
        .collect();
    let frame = methods::icicle(&of_route);
    let focus = q.focus.clone().unwrap_or_default();
    let href = format!(
        "{}/methods?route={}&range={}&focus=",
        mode.base,
        cookie::encode(&route),
        q.range.clone().unwrap_or_default()
    );
    let mut routes: Vec<Opt> = by_route
        .iter()
        .map(|(r, n)| Opt {
            value: (*r).to_owned(),
            label: format!("{r} ({n} traces)"),
            selected: **r == *route,
        })
        .collect();
    routes.sort_by(|a, b| a.value.cmp(&b.value));
    let sorts = [
        ("Total", "total"),
        ("Self", "self"),
        ("Calls", "calls"),
        ("p95", "p95"),
        ("Max", "max"),
    ]
    .iter()
    .map(|(label, key)| RangeLink {
        label,
        href: format!(
            "{}/methods?sort={key}&route={}&range={}",
            mode.base,
            cookie::encode(&route),
            q.range.clone().unwrap_or_default()
        ),
        current: *key == sort,
    })
    .collect();
    ui::page(MethodsPage {
        chrome: chrome(&state, "Methods", "dashboard", flash, theme),
        d: dash(mode, "/methods", None, 0).ai(&state.config, ai::Tab::Traces, None),
        base: mode.base,
        rows: method_rows(&stats, all_us, mode.base),
        sorts,
        ranges: opts(
            &[
                ("", "Every trace in memory"),
                ("5m", "Last 5 min"),
                ("15m", "Last 15 min"),
                ("1h", "Last hour"),
            ],
            q.range.as_deref(),
        ),
        routes,
        icicle: icicle_view(
            &frame,
            &focus,
            &href,
            micros,
            &format!("{route}: {} traces merged", of_route.len()),
        ),
        traces: traces.len(),
        route,
    })
}

// ---------------------------------------------------------------------------------------------
// Hot functions (the CPU profiler)
// ---------------------------------------------------------------------------------------------

pub struct HotRow {
    pub name: String,
    pub short: String,
    pub origin: &'static str,
    pub selfp: String,
    pub totalp: String,
    pub bar: u64,
    pub path: String,
    pub href: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProfileQuery {
    window: Option<usize>,
    filter: Option<String>,
    #[serde(rename = "fn")]
    func: Option<String>,
    focus: Option<String>,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/profile.html")]
struct ProfilePage {
    chrome: ui::Chrome,
    d: Dash,
    base: &'static str,
    available: bool,
    running: bool,
    paused: bool,
    rows: Vec<HotRow>,
    samples: u64,
    windows: Vec<RangeLink>,
    filters: Vec<RangeLink>,
    func: String,
    callers: Vec<(String, u64, String)>,
    callees: Vec<(String, u64, String)>,
    flame: Icicle,
    routes: Vec<(&'static str, u64)>,
    overhead: String,
}

pub async fn profile_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<ProfileQuery>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let window = q.window.unwrap_or(5).clamp(1, profiler::MINUTES);
    let filter = q
        .filter
        .as_deref()
        .filter(|f| matches!(*f, "app" | "deps" | "runtime" | "native"))
        .unwrap_or("all");
    let (hot, samples) = profiler::hot(window, filter);
    let max = hot.iter().map(|h| h.self_n).max().unwrap_or(1).max(1);
    let pct = |n: u64| format!("{:.1}%", n as f64 / samples.max(1) as f64 * 100.0);
    let link = |w: usize, f: &str, func: &str| {
        format!(
            "{}/profile?window={w}&filter={f}{}",
            mode.base,
            if func.is_empty() {
                String::new()
            } else {
                format!("&fn={}", cookie::encode(func))
            }
        )
    };
    let rows = hot
        .iter()
        .take(60)
        .map(|h| HotRow {
            short: profiler::short(&h.name),
            // The page shows names without the sampler's origin mark.
            origin: h.origin,
            selfp: pct(h.self_n),
            totalp: pct(h.total_n),
            bar: (h.self_n * 100 / max).max(1),
            path: h.path.clone(),
            href: link(window, filter, &h.name),
            name: profiler::plain(&h.name).to_owned(),
        })
        .collect();
    let func = q.func.clone().unwrap_or_default();
    let (callers, callees) = if func.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let (up, down) = profiler::neighbours(window, &func);
        let f = |v: Vec<(String, u64)>| {
            v.into_iter()
                .map(|(n, c)| {
                    let href = link(window, filter, &n);
                    (profiler::plain(&n).to_owned(), c, href)
                })
                .collect()
        };
        (f(up), f(down))
    };
    let flame = profiler::flame(window, filter);
    let focus = q.focus.clone().unwrap_or_default();
    // Routes with traffic in the window: what the CPU was serving.
    let now_minute = Utc::now().timestamp().div_euclid(60);
    let mut routes: Vec<(&'static str, u64)> = lock(&HUB.http)
        .stats()
        .map(|s| {
            (
                s.route,
                s.minutes
                    .iter()
                    .filter(|(m, _, _)| now_minute - m < window as i64)
                    .map(|(_, n, _)| u64::from(*n))
                    .sum::<u64>(),
            )
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    routes.sort_by_key(|a| std::cmp::Reverse(a.1));
    routes.truncate(8);
    let fold = profiler::FOLD_US.load(std::sync::atomic::Ordering::Relaxed);
    ui::page(ProfilePage {
        chrome: chrome(&state, "Hot functions", "dashboard", flash, theme),
        d: dash(mode, "/profile", None, 0).ai(&state.config, ai::Tab::Overview, None),
        base: mode.base,
        available: profiler::available(),
        running: profiler::running(),
        paused: profiler::paused(),
        rows,
        samples,
        windows: [1, 5, 15]
            .iter()
            .map(|w| RangeLink {
                label: match w {
                    1 => "1 min",
                    5 => "5 min",
                    _ => "15 min",
                },
                href: link(*w, filter, ""),
                current: *w == window,
            })
            .collect(),
        filters: [
            ("All", "all"),
            ("Your code", "app"),
            ("Dependencies", "deps"),
            ("std / Tokio", "runtime"),
            ("Native (C)", "native"),
        ]
        .iter()
        .map(|(label, key)| RangeLink {
            label,
            href: link(window, key, ""),
            current: *key == filter,
        })
        .collect(),
        func: profiler::plain(&func).to_owned(),
        callers,
        callees,
        flame: icicle_view(
            &flame,
            &focus,
            &format!(
                "{}/profile?window={window}&filter={filter}&focus=",
                mode.base
            ),
            |n| format!("{n} samples"),
            "CPU samples, callers above callees",
        ),
        routes,
        overhead: format!(
            "{} Hz sampling; the last fold (symbolizing {} s of samples) took {}",
            profiler::FREQUENCY,
            profiler::WINDOW.as_secs(),
            micros(fold)
        ),
    })
}

#[derive(Deserialize)]
pub struct ToggleForm {
    paused: Option<String>,
}

pub async fn profile_toggle(Form(f): Form<ToggleForm>) -> Response {
    let pause = f.paused.as_deref() == Some("1");
    profiler::set_paused(pause);
    back(
        "/dev/dashboard/profile",
        Flash::notice(if pause {
            "CPU sampling stopped."
        } else {
            "CPU sampling started: hot functions arrive every 15 seconds."
        }),
    )
}

/// The `sql` module's id as hex (templates).
pub fn hex(fp: u64) -> String {
    sql::hex(fp)
}
