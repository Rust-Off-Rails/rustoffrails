//! Frontend signals (real-user monitoring): what people hit in the browser. A small beacon
//! (`static/dashboard-rum.js`, loaded once from the app layout so it survives boosted navigation
//! and htmx swaps) reports rage clicks, dead clicks, clicks followed by an error, JavaScript errors
//! and unhandled rejections, htmx errors, slow interactions and the Core Web Vitals (LCP, CLS,
//! INP) per page. This module validates each report, aggregates it in bounded maps, and turns it
//! into the Frontend tab, insights and an Ask AI tool.
//!
//! On by default in development; in production only with `DASHBOARD_RUM=true` (and the read-only
//! dashboard mounted to read it), sampled by `DASHBOARD_RUM_SAMPLE` (the share of page views that
//! load the beacon).
//!
//! **Privacy.** The beacon never sends text content, input values or query strings: elements are
//! named by a selector built from `id`, `data-testid`/`data-rum`, `name`, `role` and the tag path.
//! The server keeps only the fields it knows, validates every selector against a strict
//! character set, cuts the page to its path, and cleans messages and stacks (control characters
//! out, secrets and email addresses redacted, URLs cut to their path).
//!
//! GUIDE: .offrails/guides/frontend-htmx.md

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use askama::Template;
use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::hist::Hist;
use super::insights::{Finding, Severity};
use super::web::{Dash, Mode, chrome, dash, micros};
use super::{HUB, ai, lock};
use crate::config::Config;
use crate::controllers::application::ClientIp;
use crate::kernel::flash::RequestFlash;
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui;
use crate::state::AppState;

/// The beacon's endpoint in development, and in production (`DASHBOARD_RUM=true`).
pub const DEV_ENDPOINT: &str = "/dev/dashboard/rum";
pub const PROD_ENDPOINT: &str = "/rum";
/// A report's body, at most. APP.
pub const MAX_BODY: usize = 16 * 1024;
/// Events per report, at most (the rest are dropped). APP.
pub const MAX_EVENTS: usize = 40;
/// Reports per client address per minute. APP.
pub const PER_MINUTE: u32 = 60;
/// Events the whole app accepts per second (a flood is dropped and counted). APP.
pub const PER_SECOND: u64 = 500;

/// Rage clicks: bursts on one element before it is a finding. APP.
pub const RAGE_MIN: u32 = 2;
/// Dead clicks on one element before it is a finding. APP.
pub const DEAD_MIN: u32 = 3;
/// A dead click's share after a boosted navigation that points at a script that never ran. APP.
pub const BOOSTED_SHARE: f64 = 0.8;
/// Web Vitals: (good, poor) thresholds, as Google defines them. LCP and INP in ms; CLS × 1000.
pub const LCP_MS: (u64, u64) = (2_500, 4_000);
pub const INP_MS: (u64, u64) = (200, 500);
pub const CLS_X1000: (u64, u64) = (100, 250);
/// Samples before a page's vitals become a finding. APP.
pub const VITALS_MIN: u64 = 3;
/// An interaction slower than this (event → next paint) is slow. APP.
pub const SLOW_INTERACTION_MS: u64 = 200;

/// Entries per map (the least recently seen goes first).
pub const MAX_CLICKS: usize = 300;
pub const MAX_ERRORS: usize = 200;
pub const MAX_PAGES: usize = 200;

static ON: AtomicBool = AtomicBool::new(false);
static PRODUCTION: AtomicBool = AtomicBool::new(false);
/// The share of page views that load the beacon, × 1,000,000.
static SAMPLE: AtomicU64 = AtomicU64::new(1_000_000);
static SCRIPT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// This second's accepted events: (unix second, count).
static BUDGET: std::sync::Mutex<(i64, u64)> = std::sync::Mutex::new((0, 0));

/// Whether the beacon runs: development (unless `DASHBOARD_RUM=false`), or production with
/// `DASHBOARD_RUM=true` and the read-only dashboard mounted.
pub fn enabled_for(config: &Config) -> bool {
    match (config.is_development(), config.is_production()) {
        (true, _) => config.dashboard.rum.unwrap_or(true),
        (_, true) => {
            config.dashboard.rum == Some(true) && super::access::mounted_in_production(config)
        }
        _ => false,
    }
}

pub fn configure(config: &Config) {
    ON.store(enabled_for(config), Ordering::Relaxed);
    PRODUCTION.store(config.is_production(), Ordering::Relaxed);
    let default = if config.is_production() { 0.1 } else { 1.0 };
    let share = config
        .dashboard
        .rum_sample
        .unwrap_or(default)
        .clamp(0.0, 1.0);
    SAMPLE.store((share * 1_000_000.0) as u64, Ordering::Relaxed);
}

/// What the app layout renders for the beacon (`ctx.dashboard_rum`).
#[derive(Clone, Debug)]
pub struct Beacon {
    pub src: String,
    pub endpoint: &'static str,
    /// This page's request id: the beacon's reports link to its trace.
    pub rid: String,
}

/// The beacon for this page view, or `None` (off, or not in the sample).
pub fn beacon(state: &AppState, headers: &HeaderMap) -> Option<Beacon> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    let share = SAMPLE.load(Ordering::Relaxed);
    if share < 1_000_000 && rand::random_range(0..1_000_000) >= share {
        return None;
    }
    let src = {
        let mut s = lock(&SCRIPT);
        if s.is_empty() {
            *s = state.assets.path("dashboard-rum.js");
        }
        s.clone()
    };
    Some(Beacon {
        src,
        endpoint: if PRODUCTION.load(Ordering::Relaxed) {
            PROD_ENDPOINT
        } else {
            DEV_ENDPOINT
        },
        rid: headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .filter(|v| rid_ok(v))
            .unwrap_or_default()
            .to_owned(),
    })
}

fn rid_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

// ---------------------------------------------------------------------------------------------
// A report, as the beacon sends it (unknown fields are ignored)
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Report {
    #[serde(deserialize_with = "number")]
    pub v: f64,
    #[serde(deserialize_with = "text")]
    pub page: String,
    #[serde(deserialize_with = "text")]
    pub rid: String,
    /// `load` (a full page load) or `boosted` (an hx-boost navigation).
    #[serde(deserialize_with = "text")]
    pub nav: String,
    pub events: Vec<RawEvent>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct RawEvent {
    /// `rage`, `dead`, `error_click`, `js_error`, `rejection`, `htmx`, `slow`, `vital`.
    #[serde(deserialize_with = "text")]
    pub t: String,
    #[serde(deserialize_with = "text")]
    pub sel: String,
    #[serde(deserialize_with = "number")]
    pub n: f64,
    #[serde(deserialize_with = "number")]
    pub ms: f64,
    #[serde(deserialize_with = "text")]
    pub msg: String,
    #[serde(deserialize_with = "text")]
    pub stack: String,
    #[serde(deserialize_with = "text")]
    pub src: String,
    #[serde(deserialize_with = "text")]
    pub kind: String,
    #[serde(deserialize_with = "number")]
    pub status: f64,
    #[serde(deserialize_with = "text")]
    pub name: String,
    #[serde(deserialize_with = "number")]
    pub value: f64,
    #[serde(deserialize_with = "text")]
    pub rid: String,
}

// WHY lenient: a report is whatever a browser (or anyone) sent; a field of the wrong type is
// dropped, not the whole report.
fn text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s,
        _ => String::new(),
    })
}

fn number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(serde_json::Value::deserialize(d)?
        .as_f64()
        .filter(|v| v.is_finite())
        .unwrap_or(0.0))
}

/// A selector the beacon built (`#publish-button`, `button[name=save]`, `main > form > button`),
/// or `None` when it holds anything else.
// SECURITY: only these characters, at most 160 of them, no quotes: a report can't smuggle text,
// a value or markup in through the selector (test: rum_reports_keep_no_text_values_or_queries).
pub fn clean_selector(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()
        && s.len() <= 160
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '#' | '.' | '-' | '_' | ':' | '>' | '[' | ']' | '=' | ' ' | '(' | ')'
                )
        }))
    .then(|| s.to_owned())
}

/// The page as kept: its path only (no query, no fragment, no origin), redacted.
pub fn clean_page(page: &str) -> Option<String> {
    let p = page.split(['?', '#']).next().unwrap_or_default();
    let p = match p.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => p,
    };
    (p.starts_with('/') && !p.starts_with("//") && p.len() <= 300)
        .then(|| ui::truncate(&super::threats::clean_path(p), 120))
}

/// A message or a stack frame as kept: no control characters, URLs cut to their path, secrets
/// and email addresses redacted, cut to `max` characters.
// SECURITY: a reported message is attacker-controlled text: it can carry terminal escapes, fake
// log lines, markup or a user's data; it is cleaned before it is stored, shown escaped, and
// handed to Ask AI as data (test: rum_messages_are_cleaned_before_they_are_kept).
pub fn clean_text(s: &str, max: usize) -> String {
    let one_line: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max * 2)
        .collect();
    let no_queries: String = one_line
        .split(' ')
        .map(|w| match w.find("://") {
            Some(at) => {
                // `(https://host/static/app.js?v=9:12:5)` → `(/static/app.js:12:5)`
                let start = w[..at]
                    .rfind(|c: char| !c.is_ascii_alphanumeric())
                    .map_or(0, |i| i + 1);
                let (prefix, url) = w.split_at(start);
                let (before, query) = match url.split_once(['?', '#']) {
                    Some((b, q)) => (b, q),
                    None => (url, ""),
                };
                let rest = before.split_once("://").map_or(before, |(_, r)| r);
                let path = rest.find('/').map_or("", |i| &rest[i..]);
                let run = query.len()
                    - query
                        .trim_end_matches(|c: char| c.is_ascii_digit() || matches!(c, ':' | ')'))
                        .len();
                let tail = &query[query.len() - run..];
                let tail = tail.find(':').map_or("", |i| &tail[i..]);
                format!("{prefix}{path}{tail}")
            }
            None => w.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let clean = super::ai_context::mask_emails(&crate::kernel::redact::text(&no_queries));
    ui::truncate(
        clean
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_str(),
        max,
    )
}

// ---------------------------------------------------------------------------------------------
// The aggregates
// ---------------------------------------------------------------------------------------------

/// Clicks of one kind on one element of one page.
pub struct ClickAgg {
    pub route: &'static str,
    pub page: String,
    pub sel: String,
    pub kind: &'static str,
    /// Reports (bursts for rage, clicks otherwise).
    pub count: u32,
    /// Clicks in all its bursts (rage).
    pub clicks: u32,
    /// Of those, on a page reached by a boosted navigation.
    pub boosted: u32,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    /// The page's request id, and the last request the element made (a slow one explains rage).
    pub page_rid: String,
    pub req_rid: String,
    pub message: String,
}

/// One JavaScript or htmx error, grouped by kind, message and source.
pub struct ErrorAgg {
    pub kind: &'static str,
    pub message: String,
    pub src: String,
    pub stack: String,
    pub status: u16,
    pub count: u32,
    pub boosted: u32,
    pub after_click: u32,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub pages: Vec<(String, u32)>,
    pub rid: String,
}

/// A page's Web Vitals and slow interactions.
pub struct PageAgg {
    pub route: &'static str,
    pub lcp: Hist,
    pub cls: Hist,
    pub inp: Hist,
    pub last: DateTime<Utc>,
    /// Slow interactions: (selector, ms) the worst few.
    pub slow: Vec<(String, u64, u32)>,
}

#[derive(Default)]
pub struct Rum {
    clicks: HashMap<(&'static str, String, &'static str), ClickAgg>,
    errors: HashMap<u64, ErrorAgg>,
    pages: HashMap<&'static str, PageAgg>,
    /// Click bursts and errors per minute, for the tab's timeline: (minute, rage, dead, errors).
    minutes: VecDeque<(i64, u32, u32, u32)>,
    pub reports: u64,
    pub dropped: u64,
}

fn evict<K: Clone + Eq + std::hash::Hash, V>(
    map: &mut HashMap<K, V>,
    last: impl Fn(&V) -> DateTime<Utc>,
) {
    if let Some(k) = map
        .iter()
        .min_by_key(|(_, v)| last(v))
        .map(|(k, _)| k.clone())
    {
        map.remove(&k);
    }
}

impl Rum {
    /// One validated report; how many of its events were kept.
    pub fn ingest(&mut self, r: Report, now: DateTime<Utc>) -> usize {
        let Some(page) = clean_page(&r.page) else {
            self.dropped += 1;
            return 0;
        };
        self.reports += 1;
        let route = super::http::route_of("GET", &page);
        let boosted = r.nav == "boosted";
        let page_rid = if rid_ok(&r.rid) {
            r.rid.clone()
        } else {
            String::new()
        };
        let minute = now.timestamp().div_euclid(60);
        let mut kept = 0;
        for e in r.events.into_iter().take(MAX_EVENTS) {
            let rid = if rid_ok(&e.rid) {
                e.rid.clone()
            } else {
                String::new()
            };
            match e.t.as_str() {
                t @ ("rage" | "dead" | "error_click") => {
                    let Some(sel) = clean_selector(&e.sel) else {
                        continue;
                    };
                    let kind: &'static str = match t {
                        "rage" => "rage",
                        "dead" => "dead",
                        _ => "error_click",
                    };
                    let key = (route, sel.clone(), kind);
                    // INVARIANT: bounded (test: the_signals_stay_within_their_memory_bound).
                    if !self.clicks.contains_key(&key) && self.clicks.len() >= MAX_CLICKS {
                        evict(&mut self.clicks, |c| c.last);
                    }
                    let c = self.clicks.entry(key).or_insert_with(|| ClickAgg {
                        route,
                        page: page.clone(),
                        sel,
                        kind,
                        count: 0,
                        clicks: 0,
                        boosted: 0,
                        first: now,
                        last: now,
                        page_rid: String::new(),
                        req_rid: String::new(),
                        message: String::new(),
                    });
                    c.count += 1;
                    c.clicks += (e.n as u32).clamp(1, 100);
                    c.boosted += u32::from(boosted);
                    c.last = now;
                    c.page.clone_from(&page);
                    if !page_rid.is_empty() {
                        c.page_rid.clone_from(&page_rid);
                    }
                    if !rid.is_empty() {
                        c.req_rid = rid;
                    }
                    if kind == "error_click" && !e.msg.is_empty() {
                        c.message = clean_text(&e.msg, 200);
                    }
                    self.minute(
                        minute,
                        u32::from(kind == "rage"),
                        u32::from(kind == "dead"),
                        0,
                    );
                    kept += 1;
                }
                t @ ("js_error" | "rejection" | "htmx") => {
                    let kind: &'static str = match t {
                        "js_error" => "js",
                        "rejection" => "rejection",
                        _ => "htmx",
                    };
                    let status = e.status.clamp(0.0, 999.0) as u16;
                    let message = if kind == "htmx" {
                        let k: String = e
                            .kind
                            .chars()
                            .filter(char::is_ascii_alphanumeric)
                            .take(24)
                            .collect();
                        format!(
                            "htmx {k}{}",
                            if status > 0 {
                                format!(" {status}")
                            } else {
                                String::new()
                            }
                        )
                    } else {
                        clean_text(&e.msg, 200)
                    };
                    if message.is_empty() {
                        continue;
                    }
                    let src = clean_text(&e.src, 120);
                    let stack = e
                        .stack
                        .lines()
                        .take(5)
                        .map(|l| clean_text(l, 160))
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let sel = clean_selector(&e.sel).unwrap_or_default();
                    let fp = super::threats::keyed((
                        kind,
                        &message,
                        &src,
                        if kind == "htmx" { route } else { "" },
                    ));
                    if !self.errors.contains_key(&fp) && self.errors.len() >= MAX_ERRORS {
                        evict(&mut self.errors, |x| x.last);
                    }
                    let x = self.errors.entry(fp).or_insert_with(|| ErrorAgg {
                        kind,
                        message,
                        src: if src.is_empty() { sel.clone() } else { src },
                        stack,
                        status,
                        count: 0,
                        boosted: 0,
                        after_click: 0,
                        first: now,
                        last: now,
                        pages: Vec::new(),
                        rid: String::new(),
                    });
                    x.count += 1;
                    x.boosted += u32::from(boosted);
                    x.after_click += u32::from(!e.sel.is_empty());
                    x.last = now;
                    if let Some(p) = x.pages.iter_mut().find(|(p, _)| *p == page) {
                        p.1 += 1;
                    } else if x.pages.len() < 10 {
                        x.pages.push((page.clone(), 1));
                    }
                    let r = if rid.is_empty() {
                        page_rid.clone()
                    } else {
                        rid
                    };
                    if !r.is_empty() {
                        x.rid = r;
                    }
                    self.minute(minute, 0, 0, 1);
                    kept += 1;
                }
                "slow" | "vital" => {
                    if !self.pages.contains_key(route) && self.pages.len() >= MAX_PAGES {
                        evict(&mut self.pages, |p| p.last);
                    }
                    let p = self.pages.entry(route).or_insert_with(|| PageAgg {
                        route,
                        lcp: Hist::default(),
                        cls: Hist::default(),
                        inp: Hist::default(),
                        last: now,
                        slow: Vec::new(),
                    });
                    p.last = now;
                    let v = if e.t == "vital" { e.value } else { e.ms };
                    if !v.is_finite() || !(0.0..=600_000.0).contains(&v) {
                        continue;
                    }
                    if e.t == "slow" {
                        let Some(sel) = clean_selector(&e.sel) else {
                            continue;
                        };
                        let ms = v as u64;
                        if let Some(s) = p.slow.iter_mut().find(|s| s.0 == sel) {
                            s.1 = s.1.max(ms);
                            s.2 += 1;
                        } else if p.slow.len() < 10 {
                            p.slow.push((sel, ms, 1));
                        } else if let Some(min) = p.slow.iter_mut().min_by_key(|s| s.1)
                            && min.1 < ms
                        {
                            *min = (sel, ms, 1);
                        }
                        p.inp.record(ms);
                    } else {
                        match e.name.as_str() {
                            "LCP" => p.lcp.record(v as u64),
                            "INP" => p.inp.record(v as u64),
                            "CLS" => p.cls.record((v * 1000.0) as u64),
                            _ => continue,
                        }
                    }
                    kept += 1;
                }
                _ => {}
            }
        }
        kept
    }

    fn minute(&mut self, m: i64, rage: u32, dead: u32, errors: u32) {
        match self.minutes.back_mut() {
            Some(x) if x.0 == m => {
                x.1 += rage;
                x.2 += dead;
                x.3 += errors;
            }
            _ => self.minutes.push_back((m, rage, dead, errors)),
        }
        while self.minutes.len() > 60 {
            self.minutes.pop_front();
        }
    }

    pub fn lens(&self) -> (usize, usize, usize) {
        (self.clicks.len(), self.errors.len(), self.pages.len())
    }

    pub fn approx_bytes(&self) -> usize {
        self.clicks
            .values()
            .map(|c| {
                std::mem::size_of::<ClickAgg>() + c.page.len() + c.sel.len() + c.message.len() + 80
            })
            .sum::<usize>()
            + self
                .errors
                .values()
                .map(|e| {
                    std::mem::size_of::<ErrorAgg>()
                        + e.message.len()
                        + e.src.len()
                        + e.stack.len()
                        + e.pages.len() * 40
                })
                .sum::<usize>()
            + self.pages.len() * (std::mem::size_of::<PageAgg>() + 3 * Hist::BYTES + 200)
    }

    pub fn clicks(&self) -> Vec<&ClickAgg> {
        let mut v: Vec<&ClickAgg> = self.clicks.values().collect();
        v.sort_by(|a, b| b.clicks.cmp(&a.clicks).then(a.sel.cmp(&b.sel)));
        v
    }

    pub fn errors(&self) -> Vec<&ErrorAgg> {
        let mut v: Vec<&ErrorAgg> = self.errors.values().collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then(a.message.cmp(&b.message)));
        v
    }

    pub fn pages(&self) -> Vec<&PageAgg> {
        let mut v: Vec<&PageAgg> = self.pages.values().collect();
        v.sort_by(|a, b| b.lcp.count().cmp(&a.lcp.count()).then(a.route.cmp(b.route)));
        v
    }

    /// Every frontend finding, deterministically.
    pub fn findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        let p95_of = |rid: &str| -> Option<(String, u64)> {
            if rid.is_empty() {
                return None;
            }
            let t = lock(&HUB.traces)
                .list(false)
                .into_iter()
                .find(|t| t.request_id == rid)?;
            let p95 = lock(&HUB.http)
                .get(t.method, t.route)
                .map_or(t.dur_us, |r| r.hist.quantile(0.95));
            Some((format!("{} {}", t.method, t.route), p95))
        };
        for c in self.clicks.values() {
            let boosted_share = f64::from(c.boosted) / f64::from(c.count.max(1));
            match c.kind {
                "rage" if c.count >= RAGE_MIN => {
                    let slow = p95_of(&c.req_rid).filter(|(_, p95)| *p95 >= 300_000);
                    out.push(Finding {
                        key: format!("rum-rage-{} {}", c.route, c.sel),
                        kind: "frontend",
                        severity: if c.clicks >= 10 { Severity::High } else { Severity::Medium },
                        score: u64::from(c.clicks),
                        title: format!("Rage clicks on {} on {} ({} clicks in {} bursts)", c.sel, c.page, c.clicks, c.count),
                        evidence: match &slow {
                            Some((route, p95)) => format!("People click it again and again: the request it makes ({route}) is slow (p95 {}), or the button looks clickable while it isn't.", micros(*p95)),
                            None => "People click it again and again: it may look clickable while it's disabled or doing nothing, or give no feedback while it works.".to_owned(),
                        },
                        fix: "Give it feedback at once (hx-indicator, a disabled state with aria-busy, hx-disabled-elt), make the request it waits for fast, and make sure a disabled look matches a disabled element.".into(),
                        guide: ".offrails/guides/frontend-htmx.md",
                        guide_url: "https://rustoffrails.org/docs/guides/frontend-htmx",
                        href: format!("/dev/dashboard/frontend#c-{}", short_id(&c.sel, c.route)),
                    });
                }
                "dead" if c.count >= DEAD_MIN => {
                    let after_boost = boosted_share >= BOOSTED_SHARE;
                    out.push(Finding {
                        key: format!("rum-dead-{} {}", c.route, c.sel),
                        kind: "frontend",
                        severity: if after_boost { Severity::High } else { Severity::Medium },
                        score: u64::from(c.count),
                        title: if after_boost {
                            format!("Dead clicks on {} on {} after boosted navigation", c.sel, c.page)
                        } else {
                            format!("Dead clicks on {} on {}", c.sel, c.page)
                        },
                        evidence: if after_boost {
                            format!("{} clicks did nothing (no navigation, no change on the page, no request within a second), {:.0}% of them on a page reached by an hx-boost link: the script that makes it work runs on a full page load only.", c.count, boosted_share * 100.0)
                        } else {
                            format!("{} clicks on something that looks clickable did nothing within a second: no navigation, no change on the page, no request.", c.count)
                        },
                        fix: if after_boost {
                            "Load the widget's script once from the layout (the `app head` anchor) instead of the page's head block, and bind with event delegation (`document.addEventListener(\"click\", e => e.target.closest(\"[data-…]\"))`) or on `htmx:load`: a boosted navigation swaps <body> and never runs a page's head scripts.".into()
                        } else {
                            "Wire it up (its handler may not be bound), make it a real link or button that does something, or stop styling it as clickable.".into()
                        },
                        guide: ".offrails/guides/frontend-htmx.md",
                        guide_url: "https://rustoffrails.org/docs/guides/frontend-htmx",
                        href: format!("/dev/dashboard/frontend#c-{}", short_id(&c.sel, c.route)),
                    });
                }
                _ => {}
            }
        }
        for e in self.errors.values() {
            let after_boost = e.kind != "htmx"
                && f64::from(e.boosted) / f64::from(e.count.max(1)) >= BOOSTED_SHARE;
            let pages = e
                .pages
                .iter()
                .map(|(p, _)| p.as_str())
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Finding {
                key: format!("rum-err-{}-{}", e.kind, e.message),
                kind: "frontend",
                severity: if e.kind == "htmx" && e.status < 500 { Severity::Low } else if e.count >= 5 || after_boost { Severity::High } else { Severity::Medium },
                score: u64::from(e.count) * 5,
                title: match (e.kind, after_boost) {
                    ("htmx", _) => format!("{} on {pages}", e.message),
                    (_, true) => format!("JS error after boosted navigation: {}", ui::truncate(&e.message, 80)),
                    _ => format!("JS error: {}", ui::truncate(&e.message, 90)),
                },
                evidence: format!(
                    "{}× on {pages}{}{}{}",
                    e.count,
                    if e.src.is_empty() { String::new() } else { format!(", at {}", e.src) },
                    if e.after_click > 0 { format!(", {} right after a click", e.after_click) } else { String::new() },
                    if after_boost { "; only on pages reached by hx-boost: a page script that expects a full load".to_owned() } else { String::new() }
                ),
                fix: if e.kind == "htmx" {
                    "Open the request's trace (the evidence) for why the server answered that way; give the element an error state (htmx:responseError) instead of silence.".into()
                } else if after_boost {
                    "Initialize on `htmx:load` (it fires for the first page and each swap) and guard against running twice, instead of on DOMContentLoaded.".into()
                } else {
                    "Fix the throw at its source line; an uncaught error stops the rest of that handler, so the click did nothing.".into()
                },
                guide: ".offrails/guides/frontend-htmx.md",
                guide_url: "https://rustoffrails.org/docs/guides/frontend-htmx",
                href: format!("/dev/dashboard/frontend#e-{}", short_id(&e.message, e.kind)),
            });
        }
        for p in self.pages.values() {
            for (name, h, (good, poor), unit) in [
                ("LCP", &p.lcp, LCP_MS, "ms"),
                ("INP", &p.inp, INP_MS, "ms"),
                ("CLS", &p.cls, CLS_X1000, "cls"),
            ] {
                if h.count() < VITALS_MIN {
                    continue;
                }
                let p75 = h.quantile(0.75);
                if p75 <= good {
                    continue;
                }
                let shown = vital(p75, unit);
                out.push(Finding {
                    key: format!("rum-{name}-{}", p.route),
                    kind: "frontend",
                    severity: if p75 > poor { Severity::Medium } else { Severity::Low },
                    score: p75 * 100 / good.max(1),
                    title: format!("{} {name} on {} (p75 {shown})", if p75 > poor { "Poor" } else { "Needs improvement:" }, p.route),
                    evidence: format!("{} page views; good is {} or less, poor is over {}", h.count(), vital(good, unit), vital(poor, unit)),
                    fix: match name {
                        "LCP" => "Make the largest element arrive sooner: a faster handler (its trace), fewer render-blocking styles, sized images.",
                        "INP" => "Keep handlers short: answer the click at once (an indicator), move heavy work off the main thread, and make the request it waits for fast.",
                        _ => "Reserve space for what loads late (width and height on images, min-height on swapped regions).",
                    }
                    .into(),
                    guide: ".offrails/guides/views.md",
                    guide_url: "https://rustoffrails.org/docs/guides/views",
                    href: "/dev/dashboard/frontend#vitals".into(),
                });
            }
        }
        out
    }
}

/// A stable short id for anchors (`#c-…`, `#e-…`).
pub fn short_id(a: &str, b: &str) -> String {
    format!("{:08x}", super::threats::keyed((a, b)) & 0xffff_ffff)
}

/// `2400` ms → "2.40 s", `120` (CLS × 1000) → "0.12".
pub fn vital(v: u64, unit: &str) -> String {
    match unit {
        "cls" => format!("{:.2}", v as f64 / 1000.0),
        _ if v >= 1000 => format!("{:.2} s", v as f64 / 1000.0),
        _ => format!("{v} ms"),
    }
}

fn grade(v: u64, (good, poor): (u64, u64)) -> &'static str {
    if v <= good {
        "ok"
    } else if v <= poor {
        "warn"
    } else {
        "err"
    }
}

/// Frontend findings for Insights.
pub fn findings() -> Vec<Finding> {
    lock(&HUB.rum).findings()
}

// ---------------------------------------------------------------------------------------------
// The endpoint
// ---------------------------------------------------------------------------------------------

/// `POST /dev/dashboard/rum` (development) and `POST /rum` (production, `DASHBOARD_RUM=true`): a
/// batch of the beacon's events.
// SECURITY: same-origin only (Fetch Metadata, else Origin; the CSRF layer applies too), at most
// `MAX_BODY` bytes, `PER_MINUTE` reports per address and `PER_SECOND` events for the whole app;
// a flood is dropped and counted, never queued (test: the_rum_endpoint_is_same_origin_capped_and_rate_limited).
pub async fn collect(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !ON.load(Ordering::Relaxed) || !HUB.enabled() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    let origin_ok = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|o| {
            let app = state.config.app_url.trim_end_matches('/');
            o == app
                || o.strip_prefix("http://localhost:")
                    .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()))
                    && state.config.is_development()
                || o.strip_prefix("http://127.0.0.1:")
                    .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()))
                    && state.config.is_development()
        });
    let same_origin = match site {
        Some(s) => s == "same-origin" && origin_ok,
        None => headers.contains_key("origin") && origin_ok,
    };
    if !same_origin {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.len() > MAX_BODY {
        lock(&HUB.rum).dropped += 1;
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if state
        .rate_limit
        .check(
            &ip.key("dashboard-rum"),
            PER_MINUTE,
            std::time::Duration::from_secs(60),
        )
        .await
        .is_err()
    {
        lock(&HUB.rum).dropped += 1;
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(report) = serde_json::from_slice::<Report>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if report.v != 1.0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let n = report.events.len().min(MAX_EVENTS) as u64;
    let now = Utc::now();
    {
        let mut b = lock(&BUDGET);
        if b.0 != now.timestamp() {
            *b = (now.timestamp(), 0);
        }
        if b.1 + n > PER_SECOND {
            drop(b);
            lock(&HUB.rum).dropped += 1;
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        b.1 += n;
    }
    lock(&HUB.rum).ingest(report, now);
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------------------------

pub struct ClickRow {
    pub id: String,
    pub kind: &'static str,
    pub label: &'static str,
    pub page: String,
    pub sel: String,
    pub count: u32,
    pub clicks: u32,
    pub boosted: String,
    pub heat: u64,
    pub last: String,
    pub trace: String,
    pub message: String,
}

pub struct ErrorRow {
    pub id: String,
    pub kind: &'static str,
    pub message: String,
    pub src: String,
    pub stack: String,
    pub count: u32,
    pub boosted: u32,
    pub after_click: u32,
    pub first: String,
    pub last: String,
    pub pages: String,
    pub trace: String,
}

pub struct VitalRow {
    pub route: &'static str,
    pub views: u64,
    pub lcp: (String, &'static str),
    pub cls: (String, &'static str),
    pub inp: (String, &'static str),
    pub slow: Vec<(String, String, u32)>,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/frontend.html")]
struct FrontendPage {
    chrome: ui::Chrome,
    d: Dash,
    base: &'static str,
    on: bool,
    production: bool,
    clicks: Vec<ClickRow>,
    errors: Vec<ErrorRow>,
    vitals: Vec<VitalRow>,
    reports: u64,
    dropped: u64,
    totals: (u32, u32, u32),
}

fn trace_link(base: &str, rid: &str) -> String {
    if rid.is_empty() {
        String::new()
    } else {
        format!("{base}/traces?rid={}", crate::kernel::cookie::encode(rid))
    }
}

/// `GET …/frontend`: clicks that went wrong, errors, Web Vitals.
pub async fn frontend_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let now = Utc::now();
    let (clicks, errors, vitals, reports, dropped) = {
        let r = lock(&HUB.rum);
        let list = r.clicks();
        let max = list.iter().map(|c| c.clicks).max().unwrap_or(1).max(1);
        let clicks: Vec<ClickRow> = list
            .iter()
            .take(60)
            .map(|c| ClickRow {
                id: short_id(&c.sel, c.route),
                kind: c.kind,
                label: match c.kind {
                    "rage" => "rage",
                    "dead" => "dead",
                    _ => "error",
                },
                page: c.page.clone(),
                sel: c.sel.clone(),
                count: c.count,
                clicks: c.clicks,
                boosted: if c.boosted > 0 {
                    format!(
                        "{:.0}%",
                        f64::from(c.boosted) * 100.0 / f64::from(c.count.max(1))
                    )
                } else {
                    "—".into()
                },
                heat: u64::from(c.clicks) * 100 / u64::from(max),
                last: ui::ago(c.last, now),
                trace: trace_link(
                    mode.base,
                    if c.req_rid.is_empty() {
                        &c.page_rid
                    } else {
                        &c.req_rid
                    },
                ),
                message: c.message.clone(),
            })
            .collect();
        let errors: Vec<ErrorRow> = r
            .errors()
            .iter()
            .take(50)
            .map(|e| ErrorRow {
                id: short_id(&e.message, e.kind),
                kind: e.kind,
                message: e.message.clone(),
                src: e.src.clone(),
                stack: e.stack.clone(),
                count: e.count,
                boosted: e.boosted,
                after_click: e.after_click,
                first: ui::ago(e.first, now),
                last: ui::ago(e.last, now),
                pages: e
                    .pages
                    .iter()
                    .map(|(p, n)| format!("{p} ×{n}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                trace: trace_link(mode.base, &e.rid),
            })
            .collect();
        let vitals: Vec<VitalRow> = r
            .pages()
            .iter()
            .map(|p| {
                let cell = |h: &Hist, t: (u64, u64), unit: &str| {
                    if h.count() == 0 {
                        ("—".to_owned(), "muted")
                    } else {
                        let v = h.quantile(0.75);
                        (vital(v, unit), grade(v, t))
                    }
                };
                VitalRow {
                    route: p.route,
                    views: p.lcp.count().max(p.cls.count()),
                    lcp: cell(&p.lcp, LCP_MS, "ms"),
                    cls: cell(&p.cls, CLS_X1000, "cls"),
                    inp: cell(&p.inp, INP_MS, "ms"),
                    slow: p
                        .slow
                        .iter()
                        .filter(|s| s.1 >= SLOW_INTERACTION_MS)
                        .map(|(s, ms, n)| (s.clone(), vital(*ms, "ms"), *n))
                        .collect(),
                }
            })
            .collect();
        (clicks, errors, vitals, r.reports, r.dropped)
    };
    let totals = (
        clicks
            .iter()
            .filter(|c| c.kind == "rage")
            .map(|c| c.clicks)
            .sum(),
        clicks
            .iter()
            .filter(|c| c.kind == "dead")
            .map(|c| c.count)
            .sum(),
        errors.iter().map(|e| e.count).sum(),
    );
    ui::page(FrontendPage {
        chrome: chrome(&state, "Frontend", "dashboard", flash, theme),
        d: dash(mode, "/frontend", None, 0).ai(&state.config, ai::Tab::Frontend, None),
        base: mode.base,
        on: ON.load(Ordering::Relaxed),
        production: state.config.is_production(),
        clicks,
        errors,
        vitals,
        reports,
        dropped,
        totals,
    })
}

// ---------------------------------------------------------------------------------------------
// For Ask AI
// ---------------------------------------------------------------------------------------------

/// `list_frontend_issues`: clicks that went wrong, errors, poor vitals.
pub fn list_text() -> String {
    let r = lock(&HUB.rum);
    if r.reports == 0 {
        return "No browser reports yet. The beacon (static/dashboard-rum.js) loads on app pages in development; the load lab's UX trouble page makes some.".into();
    }
    let mut out = format!(
        "{} reports from browsers ({} dropped).\n## Clicks that went wrong (kind, element, page, count)\n",
        r.reports, r.dropped
    );
    for c in r.clicks().iter().take(15) {
        let _ = writeln!(
            out,
            "- {} clicks on `{}` on {} ([route:GET {}]): {}, {}, {} after a boosted navigation{}",
            c.kind,
            c.sel,
            c.page,
            c.route,
            super::web::count(c.count, "report", "reports"),
            super::web::count(c.clicks, "click", "clicks"),
            c.boosted,
            if c.message.is_empty() {
                String::new()
            } else {
                format!("; error: {}", super::ai_context::clean(&c.message, 300))
            }
        );
    }
    out.push_str("## Errors (kind, message, where, count)\n");
    for e in r.errors().iter().take(15) {
        let _ = writeln!(
            out,
            "- {} `{}` at {}: {}×, {} after a boosted navigation, {} right after a click, pages {}",
            e.kind,
            super::ai_context::clean(&e.message, 300),
            if e.src.is_empty() { "?" } else { &e.src },
            e.count,
            e.boosted,
            e.after_click,
            e.pages
                .iter()
                .map(|(p, n)| format!("{p} ×{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out.push_str("## Web Vitals per page (p75; good LCP ≤ 2.5 s, INP ≤ 200 ms, CLS ≤ 0.1)\n");
    for p in r.pages().iter().take(15) {
        let _ = writeln!(
            out,
            "- [route:GET {}]: LCP {} ({} views), INP {}, CLS {}; slow interactions: {}",
            p.route,
            if p.lcp.count() > 0 {
                vital(p.lcp.quantile(0.75), "ms")
            } else {
                "—".into()
            },
            p.lcp.count(),
            if p.inp.count() > 0 {
                vital(p.inp.quantile(0.75), "ms")
            } else {
                "—".into()
            },
            if p.cls.count() > 0 {
                vital(p.cls.quantile(0.75), "cls")
            } else {
                "—".into()
            },
            p.slow
                .iter()
                .map(|(s, ms, n)| format!("`{s}` {ms} ms ×{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_pages_and_messages_are_cleaned() {
        assert_eq!(
            clean_selector("#publish-button").as_deref(),
            Some("#publish-button")
        );
        assert_eq!(
            clean_selector("main > form > button[name=save]").as_deref(),
            Some("main > form > button[name=save]")
        );
        assert!(clean_selector("input[value=\"hunter2\"]").is_none());
        assert!(clean_selector("<script>").is_none());
        assert_eq!(
            clean_page("https://app.example.com/orders/7?token=abc#x").as_deref(),
            Some("/orders/7")
        );
        assert!(clean_page("//evil.example/x").is_none());
        let m = clean_text(
            "boom\u{1b}[31m at https://app.example.com/static/a.js?v=1:12:5 for bob@example.com",
            200,
        );
        assert!(
            !m.contains('\u{1b}') && !m.contains("v=1") && !m.contains("bob@"),
            "{m}"
        );
        assert!(m.contains("/static/a.js"), "{m}");
    }
}
