//! The AI command center: the overview's hero, expanded by default.
//!
//! - **Briefing** (no key needed): the app's state in 3 to 5 plain bullets built from what the
//!   dashboard already measured (insights, N+1s, the series, incidents, test runs), each with its
//!   evidence link and a sparkline, under a verdict (Quiet, Healthy, Watch, Unhealthy).
//! - **AI briefing** (with a key): the agent's latest health summary, streamed live; generated
//!   once per boot on the first visit when `DASHBOARD_AI_PROACTIVE=true` and the budget allows,
//!   otherwise a button.
//! - **Ask**: one prompt box with suggestions derived from the current findings, and quick links.
//! - **What you can do here**: a card per capability, linking to its tab.
//! - **Graphs**: requests, p95, errors and memory over 15 minutes, with the briefing's findings
//!   marked on them (numbered like the bullets) and the charts' own markers.
//!
//! Everything here is a pure function of `Facts` (tests build them by hand), except `facts`,
//! which reads the hub, and `ai_part`, which may start the boot briefing.
//!
//! GUIDE: .offrails/guides/dev-tools.md

use std::sync::{LazyLock, Mutex};

use chrono::Utc;

use super::ai::{self, Tab};
use super::ai_agent::{self, Kind};
use super::ai_web::{self, RunView};
use super::charts::{self, Spark};
use super::insights::{Finding, Severity};
use super::series::{Annotation, Range, Sample};
use super::web::{HEALTH, Mode, bytes, micros};
use super::{HUB, http, lock, testruns};
use crate::kernel::ui;
use crate::state::AppState;

/// APP: how many bullets the briefing shows (problems first, then what is fine).
pub const MAX_BULLETS: usize = 5;
pub const MIN_BULLETS: usize = 3;
/// APP: how many suggested questions the prompt box offers.
pub const MAX_SUGGESTIONS: usize = 6;

// ---- facts ---------------------------------------------------------------------------------------

/// An N+1 as the briefing reads it.
#[derive(Clone, Debug)]
pub struct N1 {
    pub route: String,
    pub count: u32,
    pub times: u64,
    pub trace: u64,
    pub last: i64,
}

/// A test run: when, and how it went.
#[derive(Clone, Debug)]
pub struct TestFact {
    pub at: i64,
    pub passed: u32,
    pub failed: u32,
}

#[derive(Clone, Debug)]
pub struct IncidentFact {
    pub title: String,
    pub high: bool,
    pub at: i64,
}

/// What the command center is built from.
pub struct Facts {
    pub now: i64,
    pub base: &'static str,
    pub ro: bool,
    /// The last 15 minutes, one sample a second, oldest first.
    pub samples: Vec<Sample>,
    pub boot_rss: u64,
    /// Insights, highest severity first (the overview's own list, dismissed ones left out).
    pub findings: Vec<Finding>,
    pub n_plus_one: Vec<N1>,
    /// Newest first.
    pub tests: Vec<TestFact>,
    /// Newest first.
    pub incidents: Vec<IncidentFact>,
}

/// A finding as the overview lists it, back in the shape the briefing reads.
pub fn finding_of(v: &super::web::FindingView) -> Finding {
    Finding {
        key: v.key.clone(),
        kind: v.kind,
        severity: match v.severity {
            "high" => Severity::High,
            "medium" => Severity::Medium,
            _ => Severity::Low,
        },
        score: 0,
        title: v.title.clone(),
        evidence: v.evidence.clone(),
        fix: v.fix.clone(),
        guide: v.guide,
        guide_url: v.guide_url,
        href: v.href.clone(),
    }
}

/// Reads the hub, the test runs and the incidents. `findings` come from the overview.
pub fn facts(mode: Mode, findings: Vec<Finding>) -> Facts {
    let now = Utc::now().timestamp();
    let samples = lock(&HUB.series).window(Range::M15, now);
    let n_plus_one = lock(&HUB.sql)
        .n_plus_one()
        .into_iter()
        .take(3)
        .map(|n| N1 {
            route: n.route.to_owned(),
            count: n.count,
            times: n.times,
            trace: n.trace,
            last: n.last.timestamp(),
        })
        .collect();
    let (tests, incidents) = if mode.ro {
        (Vec::new(), Vec::new())
    } else {
        let mut runs: Vec<TestFact> = testruns::load_runs(&testruns::root().join(testruns::DIR))
            .iter()
            .map(|r| TestFact {
                at: r.at.timestamp(),
                passed: r.suites.iter().map(|s| s.passed).sum(),
                failed: r.failed(),
            })
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.at));
        runs.truncate(10);
        let inc = super::ai_watch::incidents()
            .into_iter()
            .take(5)
            .map(|i| IncidentFact {
                title: i.signal.title.clone(),
                high: i.signal.severity == "high",
                at: i.at.timestamp(),
            })
            .collect();
        (runs, inc)
    };
    Facts {
        now,
        base: mode.base,
        ro: mode.ro,
        samples,
        boot_rss: HUB.boot_rss.load(std::sync::atomic::Ordering::Relaxed),
        findings,
        n_plus_one,
        tests,
        incidents,
    }
}

// ---- numbers -------------------------------------------------------------------------------------

/// The last `secs` seconds of samples.
fn last(f: &Facts, secs: i64) -> &[Sample] {
    let from = f.now - secs;
    let at = f
        .samples
        .iter()
        .position(|s| s.t > from)
        .unwrap_or(f.samples.len());
    &f.samples[at..]
}

/// Requests and server errors in `w` (rates are per second, one sample a second).
fn totals(w: &[Sample]) -> (f64, f64) {
    (
        w.iter().map(|s| f64::from(s.req)).sum(),
        w.iter().map(|s| f64::from(s.e5)).sum(),
    )
}

/// The request-weighted p95 of `w`, µs (0 without traffic).
fn weighted_p95(w: &[Sample]) -> f64 {
    let (req, _) = totals(w);
    if req <= 0.0 {
        return 0.0;
    }
    w.iter()
        .map(|s| f64::from(s.p95) * f64::from(s.req))
        .sum::<f64>()
        / req
}

fn peak_at(w: &[Sample], f: fn(&Sample) -> f64) -> Option<(f64, i64)> {
    w.iter()
        .map(|s| (f(s), s.t))
        .filter(|(v, _)| *v > 0.0)
        .max_by(|a, b| a.0.total_cmp(&b.0))
}

fn spark(w: &[Sample], f: fn(&Sample) -> f64) -> Option<Spark> {
    // At most 60 points: a sparkline is a shape, not a measurement.
    let step = (w.len() / 60).max(1);
    let v: Vec<f64> = w
        .chunks(step)
        .map(|c| c.iter().map(f).fold(0.0, f64::max))
        .collect();
    (v.len() >= 2).then(|| charts::sparkline(&v))
}

fn ago(now: i64, t: i64) -> String {
    let d = (now - t).max(0);
    match d {
        d if d < 60 => "just now".into(),
        d if d < 3_600 => format!("{} min ago", d / 60),
        d if d < 172_800 => format!("{} h ago", d / 3_600),
        d => format!("{} days ago", d / 86_400),
    }
}

/// "GET /orders" from a finding's title ("Slow route GET /orders").
fn route_in(title: &str, prefix: &str) -> Option<String> {
    title.strip_prefix(prefix).map(|r| r.trim().to_owned())
}

// ---- verdict -------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No traffic and nothing flagged.
    Quiet,
    Healthy,
    Watch,
    Unhealthy,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Quiet => "Quiet",
            Verdict::Healthy => "Healthy",
            Verdict::Watch => "Watch",
            Verdict::Unhealthy => "Unhealthy",
        }
    }

    pub fn tone(self) -> &'static str {
        match self {
            Verdict::Quiet => "idle",
            Verdict::Healthy => "ok",
            Verdict::Watch => "warn",
            Verdict::Unhealthy => "err",
        }
    }
}

/// APP: the verdict. Unhealthy: a high finding, a high incident in the last 15 minutes, the
/// latest test run failing, or the last 5 minutes over a red threshold (`HEALTH`). Watch: a
/// medium finding, any recent incident, or an amber threshold. Quiet: no traffic and nothing else.
pub fn verdict(f: &Facts) -> Verdict {
    let w = last(f, 300);
    let (req, e5) = totals(w);
    let rate = if req > 0.0 { e5 / req } else { 0.0 };
    let p95 = weighted_p95(w);
    let recent = |i: &&IncidentFact| i.at >= f.now - 900;
    let tests_failing = f.tests.first().is_some_and(|t| t.failed > 0);
    let has = |s: Severity| f.findings.iter().any(|x| x.severity == s);
    // A handful of requests isn't a rate: 1 error in 3 requests reads as a warning, not red.
    let enough = req >= 20.0;
    if has(Severity::High)
        || f.incidents.iter().filter(recent).any(|i| i.high)
        || tests_failing
        || (enough && f64::from(HEALTH.errors.1) <= rate)
        || p95 >= f64::from(HEALTH.p95.1)
    {
        return Verdict::Unhealthy;
    }
    if has(Severity::Medium)
        || f.incidents.iter().any(|i| recent(&i))
        || e5 > 0.0 && (f64::from(HEALTH.errors.0) <= rate || !enough)
        || p95 >= f64::from(HEALTH.p95.0)
    {
        return Verdict::Watch;
    }
    if totals(&f.samples).0 <= 0.0 && f.findings.is_empty() {
        return Verdict::Quiet;
    }
    Verdict::Healthy
}

// ---- briefing ------------------------------------------------------------------------------------

/// One line of the briefing.
pub struct Bullet {
    /// 1-based; the graphs' markers carry the same number.
    pub n: usize,
    /// `err`, `warn`, `ok`, `info`.
    pub tone: &'static str,
    pub text: String,
    /// What to do about it ("fix with load_many"), or empty.
    pub fix: String,
    pub href: String,
    pub link: &'static str,
    pub spark: Option<Spark>,
    /// The graph to mark (`req`, `latency`, `errors`, `memory`) and when.
    pub chart: Option<&'static str>,
    pub at: Option<i64>,
}

fn bullet(tone: &'static str, text: String, href: String, link: &'static str) -> Bullet {
    Bullet {
        n: 0,
        tone,
        text,
        fix: String::new(),
        href,
        link,
        spark: None,
        chart: None,
        at: None,
    }
}

fn rank(tone: &str) -> u8 {
    match tone {
        "err" => 0,
        "warn" => 1,
        "info" => 2,
        _ => 3,
    }
}

/// The briefing: problems first (worst first), then what is fine, `MIN_BULLETS` to `MAX_BULLETS`.
pub fn briefing(f: &Facts) -> Vec<Bullet> {
    let b = f.base;
    let mut out: Vec<Bullet> = Vec::new();
    let w5 = last(f, 300);
    let (req5, e5) = totals(w5);
    let (req15, e15) = totals(&f.samples);
    let req_of = |s: &Sample| f64::from(s.req);
    let e5_of = |s: &Sample| f64::from(s.e5);
    let p95_of = |s: &Sample| f64::from(s.p95);
    let rss_of = |s: &Sample| s.rss as f64;

    // Errors: the rising route if insights found one, else the total.
    if e5 > 0.0 {
        let rising = f.findings.iter().find(|x| x.kind == "errors");
        let peak = peak_at(w5, e5_of);
        let rate = peak.map_or(0.0, |p| p.0);
        let text = match rising.and_then(|x| route_in(&x.title, "Rising errors on")) {
            Some(route) => format!(
                "Errors rose to {} in the last 5 min on {route}",
                charts::Unit::PerSecond.format(rate)
            ),
            None => format!(
                "{} server error{} in the last 5 min (peak {})",
                ui::thousands(e5 as u64),
                if e5 as u64 == 1 { "" } else { "s" },
                charts::Unit::PerSecond.format(rate)
            ),
        };
        let share = if req5 > 0.0 { e5 / req5 } else { 1.0 };
        let mut x = bullet(
            if share >= f64::from(HEALTH.errors.1) {
                "err"
            } else {
                "warn"
            },
            text,
            rising.map_or_else(|| format!("{b}/requests?sort=errors"), |r| r.href.clone()),
            "See the errors",
        );
        x.fix = "the log has each 500 with its request id".into();
        x.spark = spark(&f.samples, e5_of);
        x.chart = Some("errors");
        x.at = peak.map(|p| p.1);
        out.push(x);
    }
    // N+1s.
    for n in f.n_plus_one.iter().take(2) {
        let mut x = bullet(
            if n.count >= 20 || n.times >= 10 {
                "err"
            } else {
                "warn"
            },
            format!(
                "N+1 on {} ({} queries per request, {} request{} so far)",
                n.route,
                n.count,
                n.times,
                if n.times == 1 { "" } else { "s" }
            ),
            format!("{b}/traces/{}", n.trace),
            "Open the trace",
        );
        x.fix = "fix with load_many (one query for the children)".into();
        x.spark = spark(&f.samples, |s| f64::from(s.sql));
        x.chart = Some("req");
        x.at = Some(n.last).filter(|t| *t >= f.now - 900);
        out.push(x);
    }
    // Recent incidents.
    for i in f.incidents.iter().filter(|i| i.at >= f.now - 900).take(1) {
        let mut x = bullet(
            if i.high { "err" } else { "warn" },
            format!("Incident {}: {}", ago(f.now, i.at), i.title),
            format!("{b}/insights#incidents"),
            "Open the incident",
        );
        x.at = Some(i.at);
        x.chart = Some("latency");
        out.push(x);
    }
    // The other findings, in the insights' order (N+1s and errors are above already).
    for x in f
        .findings
        .iter()
        .filter(|x| !matches!(x.kind, "n_plus_one" | "errors"))
        .take(3)
    {
        let mut y = bullet(
            if x.severity == Severity::High {
                "err"
            } else if x.severity == Severity::Medium {
                "warn"
            } else {
                "info"
            },
            match x.kind {
                "slow_route" => match route_in(&x.title, "Slow route") {
                    Some(r) => format!(
                        "{r} is slow: {}",
                        x.evidence.split(", p99").next().unwrap_or_default()
                    ),
                    None => x.title.clone(),
                },
                _ => format!("{}: {}", x.title, super::ai::truncate_md(&x.evidence, 90)),
            },
            x.href.clone(),
            "See the evidence",
        );
        // The hint's first clause ("Open its slowest trace: …" → "open its slowest trace").
        y.fix = ui::truncate(
            x.fix
                .replace('`', "")
                .split([':', ';'])
                .next()
                .unwrap_or_default()
                .split(". ")
                .next()
                .unwrap_or_default()
                .trim_end_matches('.'),
            70,
        )
        .to_lowercase_first();
        match x.kind {
            "slow_route" | "slow_query" | "render" => {
                y.spark = spark(&f.samples, p95_of);
                y.chart = Some("latency");
                y.at = peak_at(&f.samples, p95_of).map(|p| p.1);
            }
            "memory" | "leak" => {
                y.spark = spark(&f.samples, rss_of);
                y.chart = Some("memory");
            }
            _ => {}
        }
        out.push(y);
    }
    // The latest test run, failing.
    if let Some(t) = f.tests.first().filter(|t| t.failed > 0) {
        let mut x = bullet(
            "err",
            format!(
                "{} test{} failed {}",
                t.failed,
                if t.failed == 1 { "" } else { "s" },
                ago(f.now, t.at)
            ),
            format!("{b}/tests"),
            "See the failures",
        );
        x.spark = test_spark(&f.tests);
        out.push(x);
    }
    out.sort_by_key(|x| rank(x.tone));
    out.truncate(MAX_BULLETS);

    // What is fine, until there are MIN_BULLETS.
    let mut fine: Vec<Bullet> = Vec::new();
    if req15 > 0.0 {
        let mut x = bullet(
            "ok",
            format!(
                "{} requests in 15 min, {} now{}",
                ui::thousands(req15 as u64),
                charts::Unit::PerSecond.format(last(f, 10).iter().map(req_of).sum::<f64>() / 10.0),
                if e15 == 0.0 {
                    ", no server errors".to_owned()
                } else {
                    String::new()
                }
            ),
            format!("{b}/requests"),
            "Requests",
        );
        x.spark = spark(&f.samples, req_of);
        fine.push(x);
        let p95 = weighted_p95(w5);
        if p95 > 0.0 && p95 < f64::from(HEALTH.p95.0) {
            let mut x = bullet(
                "ok",
                format!(
                    "Latency is fine: p95 {} over the last 5 min",
                    micros(p95 as u64)
                ),
                format!("{b}/charts#chart-latency"),
                "Latency chart",
            );
            x.spark = spark(&f.samples, p95_of);
            fine.push(x);
        }
    } else {
        fine.push(bullet(
            "info",
            "No requests in the last 15 minutes: browse the app, or start the load lab".into(),
            if f.ro {
                format!("{b}/charts")
            } else {
                format!("{b}/lab")
            },
            if f.ro { "Charts" } else { "Load lab" },
        ));
    }
    if let Some(t) = f.tests.first().filter(|t| t.failed == 0) {
        let mut x = bullet(
            "ok",
            format!(
                "All {} tests passed {}",
                ui::thousands(u64::from(t.passed)),
                ago(f.now, t.at)
            ),
            format!("{b}/tests"),
            "Test runs",
        );
        x.spark = test_spark(&f.tests);
        fine.push(x);
    }
    if let Some(s) = f.samples.last().filter(|s| s.rss > 0) {
        let growth = s.rss.saturating_sub(f.boot_rss);
        if f.boot_rss == 0 || growth < HEALTH.memory_growth.0 {
            let mut x = bullet(
                "ok",
                format!("Memory steady at {}", bytes(s.rss)),
                format!("{b}/charts#chart-memory"),
                "Memory chart",
            );
            x.spark = spark(&f.samples, rss_of);
            fine.push(x);
        }
    }
    let all_fine = out.is_empty();
    if all_fine && req15 > 0.0 {
        fine.insert(
            0,
            bullet(
                "ok",
                "Nothing flagged: no N+1s, slow routes, errors or failing checks".into(),
                format!("{b}/insights"),
                "Insights",
            ),
        );
        if f.ro {
            fine[0].href = format!("{b}/requests");
            fine[0].link = "Requests";
        }
    }
    // All fine: the good news, up to four lines; otherwise enough to reach MIN_BULLETS, and one
    // line of context when there's room.
    let take = if all_fine {
        4
    } else {
        MIN_BULLETS
            .saturating_sub(out.len())
            .max(usize::from(out.len() < MAX_BULLETS))
    };
    out.extend(fine.into_iter().take(take));
    out.truncate(MAX_BULLETS);
    for (i, x) in out.iter_mut().enumerate() {
        x.n = i + 1;
    }
    out
}

trait LowerFirst {
    fn to_lowercase_first(&self) -> String;
}

impl LowerFirst for String {
    fn to_lowercase_first(&self) -> String {
        // "Open its trace" → "open its trace"; "EXPLAIN it" stays.
        let mut c = self.chars();
        match (c.next(), self.chars().nth(1)) {
            (Some(f), Some(n)) if !n.is_uppercase() => f.to_lowercase().chain(c).collect(),
            _ => self.clone(),
        }
    }
}

/// Failures per run, oldest first, as a sparkline (flat at zero when every run passed).
fn test_spark(tests: &[TestFact]) -> Option<Spark> {
    let v: Vec<f64> = tests.iter().rev().map(|t| f64::from(t.failed)).collect();
    (v.len() >= 2).then(|| charts::sparkline(&v))
}

// ---- suggestions ---------------------------------------------------------------------------------

/// APP: the prompt box's suggestions, derived from what the dashboard found (the most specific
/// first), then general ones. Each is a question the agent can answer with its tools.
pub fn suggestions(f: &Facts) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for x in &f.findings {
        match x.kind {
            "errors" => {
                if let Some(r) = route_in(&x.title, "Rising errors on") {
                    push(format!("Why is {r} failing?"));
                }
            }
            "slow_route" => {
                if let Some(r) = route_in(&x.title, "Slow route") {
                    push(format!("Why is {r} slow?"));
                }
            }
            "slow_query" | "scan" | "fk_index" => push("Which queries need an index?".into()),
            "memory" | "leak" => push("Is memory growing because of a leak?".into()),
            _ => {}
        }
    }
    if let Some(n) = f.n_plus_one.first() {
        push(format!("How do I fix the N+1 on {}?", n.route));
    }
    if f.tests.first().is_some_and(|t| t.failed > 0) {
        push("Why are the tests failing?".into());
    }
    if let Some(i) = f.incidents.first() {
        push(format!(
            "What caused this: {}?",
            ui::truncate(i.title.trim_end_matches('.'), 60)
        ));
    }
    if totals(&f.samples).1 > 0.0 {
        push("Where do the server errors come from?".into());
    }
    push("What changed in the last 10 minutes?".into());
    push("Where is this app spending its time?".into());
    if !f.ro {
        push("Explain the data model".into());
        push("Which queries need an index?".into());
    }
    push("Is anything here worth worrying about?".into());
    out.truncate(MAX_SUGGESTIONS);
    out
}

// ---- what you can do here ------------------------------------------------------------------------

pub struct GuideCard {
    /// The icon's key in `_command.html`.
    pub icon: &'static str,
    pub title: &'static str,
    pub how: &'static str,
    pub href: String,
    pub cta: &'static str,
}

/// APP: the capability cards (the read-only view gets the tabs it has).
pub fn guide(base: &str, ro: bool, ai_on: bool) -> Vec<GuideCard> {
    let card = |icon, title, how, path: &str, cta| GuideCard {
        icon,
        title,
        how,
        href: format!("{base}{path}"),
        cta,
    };
    let mut v = vec![
        card(
            "trace",
            "Investigate a slow request",
            "Open Traces, pick a slow or failed one: the waterfall puts its SQL, spans and logs on one timeline.",
            "/traces?notable=1",
            "Notable traces",
        ),
        card(
            "n1",
            "Find N+1 queries",
            "SQL groups every statement by shape; the same query 5+ times in one request is flagged with its route.",
            "/sql#n-plus-one",
            "N+1 list",
        ),
    ];
    if ro {
        v.push(card(
            "chart",
            "Read the charts",
            "Requests, latency, errors, memory and the pool over time; drag to zoom, click a minute for its traces.",
            "/charts",
            "Charts",
        ));
        v.push(card(
            "route",
            "Compare routes",
            "Every route with hits, status classes and p50/p95/p99; sort by what hurts.",
            "/requests?sort=p95",
            "Requests",
        ));
    } else {
        v.push(card(
            "schema",
            "Explore the schema",
            "The live database as a diagram: click a table for its neighbours; the migrations timeline catches drift.",
            "/schema",
            "Schema",
        ));
        v.push(card(
            "logs",
            "Watch live logs",
            "Requests and log lines as they happen; filter by level, target or text, pause to read.",
            "/logs",
            "Live logs",
        ));
        v.push(card(
            "lab",
            "Load-test with the lab",
            "Send traffic or trigger a scenario (N+1, slow query, panic, error burst) and watch the panels react.",
            "/lab",
            "Load lab",
        ));
    }
    v.push(card(
        "ai",
        "Ask the AI",
        if ai_on {
            "Ask in plain words: an agent investigates with read-only tools, shows each step and cites its evidence."
        } else {
            "Bring your own key (Anthropic, OpenRouter, a local model) and an agent investigates for you."
        },
        if ai_on { "#cc-ask" } else { "/ai?tab=overview" },
        if ai_on { "Ask a question" } else { "Set it up" },
    ));
    v
}

/// The quick links under the prompt box.
pub fn quick(base: &str, ro: bool) -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("Notable traces", format!("{base}/traces?notable=1")),
        ("N+1 queries", format!("{base}/sql#n-plus-one")),
        ("Charts, last hour", format!("{base}/charts?range=1h")),
    ];
    if !ro {
        v.push(("All findings", format!("{base}/insights")));
        v.push(("Errors in the log", format!("{base}/logs?level=error")));
    }
    v.push(("What am I looking at?", format!("{base}/explain")));
    v
}

// ---- graphs --------------------------------------------------------------------------------------

/// A marker on a small chart: a briefing finding (numbered) or one of the charts' own events.
pub struct MiniMark {
    pub x: String,
    pub label: String,
    pub title: String,
    pub finding: bool,
}

pub struct Mini {
    pub key: &'static str,
    pub title: &'static str,
    pub value: String,
    pub sub: String,
    /// `s1`…`s4`: the series colour.
    pub class: &'static str,
    pub d: String,
    pub area: String,
    pub marks: Vec<MiniMark>,
    pub summary: String,
    pub href: String,
    pub empty: bool,
}

/// The graphs' viewBox (`0 0 W H`).
pub const MINI_W: f64 = 240.0;
pub const MINI_H: f64 = 56.0;

/// Requests, p95, errors and memory over `from..to`, with the bullets that have a time marked on
/// their graph and the series' own annotations on every one.
pub fn minis(
    samples: &[Sample],
    marks: &[Annotation],
    (from, to): (i64, i64),
    bullets: &[Bullet],
    base: &str,
) -> Vec<Mini> {
    type Get = fn(&Sample) -> f64;
    let specs: [(&'static str, &'static str, &'static str, Get, &str); 4] = [
        (
            "req",
            "Requests",
            "s1",
            |s| f64::from(s.req),
            "/charts#chart-req",
        ),
        (
            "latency",
            "Latency p95",
            "s3",
            |s| f64::from(s.p95),
            "/charts#chart-latency",
        ),
        (
            "errors",
            "Server errors",
            "s4",
            |s| f64::from(s.e5),
            "/charts#chart-errors",
        ),
        (
            "memory",
            "Memory",
            "s2",
            |s| s.rss as f64,
            "/charts#chart-memory",
        ),
    ];
    let span = (to - from).max(1) as f64;
    let x_of = |t: i64| ((t - from) as f64 / span * MINI_W).clamp(0.0, MINI_W);
    specs
        .into_iter()
        .map(|(key, title, class, get, path)| {
            // Latency only where there was traffic (an idle second has no p95).
            let pts: Vec<(i64, f64)> = samples
                .iter()
                .filter(|s| key != "latency" || s.req > 0.0)
                .map(|s| (s.t, get(s)))
                .collect();
            let max = charts::nice_ceiling(pts.iter().map(|p| p.1).fold(0.0, f64::max));
            let y_of = |v: f64| MINI_H - 2.0 - (v / max).clamp(0.0, 1.0) * (MINI_H - 6.0);
            let mut d = String::new();
            for (i, (t, v)) in pts.iter().enumerate() {
                d.push_str(&format!(
                    "{}{:.1},{:.1}",
                    if i == 0 { 'M' } else { 'L' },
                    x_of(*t),
                    y_of(*v)
                ));
            }
            let area = match (pts.first(), pts.last()) {
                (Some(a), Some(z)) if pts.len() >= 2 => {
                    format!("{d}L{:.1},{MINI_H}L{:.1},{MINI_H}Z", x_of(z.0), x_of(a.0))
                }
                _ => String::new(),
            };
            let fmt = |v: f64| match key {
                "req" | "errors" => charts::Unit::PerSecond.format(v),
                "latency" => micros(v as u64),
                _ => bytes(v as u64),
            };
            let now = pts.last().map(|p| p.1);
            let peak = pts.iter().map(|p| p.1).fold(0.0, f64::max);
            let mut mm: Vec<MiniMark> = marks
                .iter()
                .filter(|m| m.t >= from && m.t <= to)
                .map(|m| MiniMark {
                    x: format!("{:.1}", x_of(m.t)),
                    label: String::new(),
                    title: format!("{} {}: {}", ai::clock(m.t), m.kind, m.label),
                    finding: false,
                })
                .collect();
            for b in bullets.iter().filter(|b| b.chart == Some(key)) {
                if let Some(t) = b.at.filter(|t| *t >= from && *t <= to) {
                    mm.push(MiniMark {
                        x: format!("{:.1}", x_of(t).clamp(8.0, MINI_W - 8.0)),
                        label: b.n.to_string(),
                        title: format!("{}. {} ({})", b.n, b.text, ai::clock(t)),
                        finding: true,
                    });
                }
            }
            let noted: Vec<String> = mm
                .iter()
                .filter(|m| m.finding)
                .map(|m| m.title.clone())
                .collect();
            Mini {
                key,
                title,
                value: now.map_or_else(|| "—".to_owned(), fmt),
                sub: if pts.is_empty() {
                    "no data yet".into()
                } else {
                    format!("peak {}", fmt(peak))
                },
                class,
                summary: format!(
                    "{title} over the last 15 minutes: {}{}",
                    if pts.is_empty() {
                        "no data".to_owned()
                    } else {
                        format!(
                            "now {}, peak {}",
                            now.map_or_else(|| "—".to_owned(), fmt),
                            fmt(peak)
                        )
                    },
                    if noted.is_empty() {
                        String::new()
                    } else {
                        format!(". Marked: {}", noted.join("; "))
                    }
                ),
                empty: pts.len() < 2,
                d,
                area,
                marks: mm,
                href: format!("{base}{path}"),
            }
        })
        .collect()
}

// ---- the snapshot the AI briefing starts from ------------------------------------------------------

/// A dashboard link as the citation tag the model can repeat (`/dev/dashboard/traces/77` →
/// `[trace:77]`), when it is one.
pub fn cite_of(href: &str) -> Option<String> {
    let p = href
        .trim_start_matches("/dev/dashboard")
        .trim_start_matches("/admin/dashboard");
    let (p, _) = p.split_once(['?', '#']).unwrap_or((p, ""));
    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["traces", id] if id.bytes().all(|b| b.is_ascii_digit()) => Some(format!("[trace:{id}]")),
        ["sql", id] if super::sql::parse_hex(id).is_some() => Some(format!("[sql:{id}]")),
        ["schema", "tables", t] if super::schema::plain_ident(t) => Some(format!("[table:{t}]")),
        _ => None,
    }
}

/// APP: what the AI briefing starts from, inside the overview's page data: the verdict, the
/// briefing's lines and the top findings, each with the ids to cite. With it a health summary
/// needs few tool calls (cheap models used all eight without it).
pub fn snapshot(f: &Facts, v: Verdict, bullets: &[Bullet]) -> String {
    let mut s = format!(
        "## The dashboard's own briefing (deterministic, computed now)\nVerdict: {}\n",
        v.label()
    );
    for b in bullets {
        let tag = cite_of(&b.href)
            .map(|t| format!(" {t}"))
            .unwrap_or_default();
        let fix = if b.fix.is_empty() {
            String::new()
        } else {
            format!(" (fix: {})", super::ai_context::clean(&b.fix, 100))
        };
        s.push_str(&format!(
            "{}. [{}] {}{fix}{tag}\n",
            b.n,
            b.tone,
            super::ai_context::clean(&b.text, 200)
        ));
    }
    if !f.findings.is_empty() {
        s.push_str("## Top findings (cite them as [finding:<id>])\n");
        for x in f.findings.iter().take(8) {
            let tag = cite_of(&x.href)
                .map(|t| format!(" {t}"))
                .unwrap_or_default();
            s.push_str(&format!(
                "- [finding:{}] {} {}: {}{tag}\n",
                super::web::finding_id(&x.key),
                x.severity.key(),
                super::ai_context::clean(&x.title, 120),
                super::ai_context::clean(&x.evidence, 200),
            ));
        }
    }
    for n in &f.n_plus_one {
        s.push_str(&format!(
            "- N+1 on {}: {} statements in one request, {} requests [trace:{}]\n",
            super::ai_context::clean(&n.route, 120),
            n.count,
            n.times,
            n.trace
        ));
    }
    s
}

// ---- the AI part ---------------------------------------------------------------------------------

/// The boot briefing's run, once started (one per process).
static BOOT_RUN: LazyLock<Mutex<Option<Option<String>>>> = LazyLock::new(|| Mutex::new(None));

pub struct AiPart {
    /// The assistant may answer here (development, or the production view with it turned on).
    pub allowed: bool,
    pub configured: bool,
    pub prepared: String,
    /// The first request for a typed question, and for the briefing.
    pub preview: String,
    pub health_preview: String,
    /// The latest health summary (live or finished), if any.
    pub run: Option<RunView>,
    /// "Generated 3 min ago", or why there's no automatic one.
    pub note: String,
    pub left: String,
    pub provider: String,
}

/// Builds the overview's prepared context (shared by the prompt box and the briefing button) and
/// finds the latest health summary; with `DASHBOARD_AI_PROACTIVE=true`, starts the boot briefing
/// on the first visit (once per process, within the budget).
pub async fn ai_part(state: &AppState, mode: Mode, owner: Option<&str>, snapshot: &str) -> AiPart {
    let cfg = &state.config.dashboard.ai;
    let allowed = ai::allowed(&state.config, mode.ro);
    let configured = cfg.provider.is_some();
    let mut p = AiPart {
        allowed,
        configured,
        prepared: String::new(),
        preview: String::new(),
        health_preview: String::new(),
        run: None,
        note: String::new(),
        left: String::new(),
        provider: if configured {
            format!(
                "{} · {}",
                if cfg.provider == Some(crate::config::AiProvider::Anthropic) {
                    "Anthropic"
                } else {
                    "OpenAI-compatible"
                },
                cfg.model
            )
        } else {
            String::new()
        },
    };
    if !allowed || !configured {
        return p;
    }
    let ctx = http::quietly(super::ai_context::build(
        state,
        mode.ro,
        Tab::Overview,
        None,
    ))
    .await
    .unwrap_or_default();
    // The briefing first: the model builds on it instead of rediscovering it with tools.
    let ctx = format!("{snapshot}\n{ctx}");
    let prepared = ai::store(Tab::Overview, None, ctx);
    p.preview = serde_json::to_string_pretty(&ai_web::first_body(
        state,
        mode,
        &prepared,
        ai_web::QUESTION_SLOT,
    ))
    .unwrap_or_default();
    p.health_preview = serde_json::to_string_pretty(&ai_web::first_body(
        state,
        mode,
        &prepared,
        ai::HEALTH_QUESTION,
    ))
    .unwrap_or_default();
    // SECURITY: the boot briefing sends data without a click, so it runs only when the developer
    // opted in to proactive mode, in development, once per process, inside the budget
    // (test: the_command_center_briefs_without_a_key_and_with_one)
    if !mode.ro && super::ai_watch::enabled(state) {
        let start = {
            let mut b = lock(&BOOT_RUN);
            if b.is_none() {
                *b = Some(None);
                true
            } else {
                false
            }
        };
        if start {
            let spec = ai_agent::Spec {
                tab: Tab::Overview,
                item: None,
                kind: Kind::Health,
                first: ai::user_message(&prepared, ai::HEALTH_QUESTION),
                system: prepared.system.clone(),
                question: ai::HEALTH_QUESTION.to_owned(),
                owner_cookie: None,
                ro: false,
                base: mode.base,
                reserved: false,
            };
            match ai_agent::start(state, spec) {
                Ok(run) => *lock(&BOOT_RUN) = Some(Some(run.id.clone())),
                Err(e) => p.note = format!("No automatic briefing this time: {}", e.message()),
            }
        }
    }
    p.prepared = prepared.id;
    p.left = ai::budget(cfg).left(ai::today());
    // The latest health summary: this boot's, else the newest in the history.
    let boot = lock(&BOOT_RUN).clone().flatten();
    let rec = boot.as_deref().and_then(ai_agent::find_record).or_else(|| {
        ai_agent::history()
            .into_iter()
            .filter(|r| r.kind == Kind::Health && r.base == mode.base)
            .max_by_key(|r| r.started)
    });
    if let Some(rec) = rec {
        let live_run = ai_agent::get(&rec.id).filter(|r| !r.record().status.finished());
        let streams = live_run.as_ref().is_some_and(|r| r.owned_by(owner));
        let rec = live_run.map_or(rec, |r| r.record());
        if p.note.is_empty() {
            p.note = if boot.as_deref() == Some(rec.id.as_str()) {
                format!(
                    "Generated automatically {} (proactive mode)",
                    ui::ago(rec.started, Utc::now())
                )
            } else {
                format!("Generated {}", ui::ago(rec.started, Utc::now()))
            };
        }
        p.run = Some(ai_web::run_view(&rec, streams));
    }
    p
}

// ---- the whole --------------------------------------------------------------------------------

pub struct CommandCenter {
    pub base: &'static str,
    pub ro: bool,
    pub verdict: &'static str,
    pub tone: &'static str,
    pub headline: String,
    pub bullets: Vec<Bullet>,
    pub suggestions: Vec<String>,
    pub quick: Vec<(&'static str, String)>,
    pub guide: Vec<GuideCard>,
    pub minis: Vec<Mini>,
    pub demo: bool,
    pub ai: AiPart,
}

/// APP: the line under the verdict.
pub fn headline(v: Verdict, bullets: &[Bullet]) -> String {
    let problems = bullets
        .iter()
        .filter(|b| b.tone == "err" || b.tone == "warn")
        .count();
    match v {
        Verdict::Quiet => "Nothing is happening yet: here is what you can do.".into(),
        Verdict::Healthy => "Everything the dashboard checks looks fine.".into(),
        Verdict::Watch | Verdict::Unhealthy if problems == 0 => {
            "Something needs a look: see the findings.".into()
        }
        Verdict::Watch => format!(
            "{problems} thing{} to keep an eye on.",
            if problems == 1 { "" } else { "s" }
        ),
        Verdict::Unhealthy => format!(
            "{problems} problem{} worth fixing now.",
            if problems == 1 { "" } else { "s" }
        ),
    }
}

/// Everything the command center shows.
pub async fn build(
    state: &AppState,
    mode: Mode,
    findings: Vec<Finding>,
    owner: Option<&str>,
) -> CommandCenter {
    let f = facts(mode, findings);
    let v = verdict(&f);
    let bullets = briefing(&f);
    let (win, _) = super::web::window(Range::M15, None, None);
    let minis = minis(
        &win.samples,
        &win.marks,
        (win.from, win.to),
        &bullets,
        mode.base,
    );
    let ai = ai_part(state, mode, owner, &snapshot(&f, v, &bullets)).await;
    CommandCenter {
        base: mode.base,
        ro: mode.ro,
        verdict: v.label(),
        tone: v.tone(),
        headline: headline(v, &bullets),
        suggestions: suggestions(&f),
        quick: quick(mode.base, mode.ro),
        guide: guide(mode.base, mode.ro, ai.allowed && ai.configured),
        demo: win.demo,
        minis,
        bullets,
        ai,
    }
}
