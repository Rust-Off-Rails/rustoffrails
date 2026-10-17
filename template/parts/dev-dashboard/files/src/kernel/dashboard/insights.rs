//! Insights: an automatic app doctor. Each rule turns what the dashboard measured (requests, SQL,
//! traces, the schema, the series, CSP reports) into findings ranked by impact, each with its
//! evidence, a fix hint with the framework guide that explains it, and the page that proves it.
//!
//! APP: add your own rules in `custom_rules`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::series::Range;
use super::{HUB, lock, panels, schema, sql};
use crate::state::AppState;

/// Thresholds. APP: tune them to your app.
pub const SLOW_ROUTE_P95_US: u64 = 300_000;
pub const SLOW_QUERY_P95_US: u64 = 100_000;
pub const SLOW_RENDER_US: u64 = 50_000;
/// A table this large is worth a look (and a scan of it is a finding).
pub const LARGE_TABLE_ROWS: i64 = 100_000;
pub const SCAN_MIN_ROWS: i64 = 500;
/// RSS growth over the window that suggests a leak.
pub const LEAK_BYTES: f64 = 64.0 * 1024.0 * 1024.0;
/// EXPLAINs per refresh, at most (each is cached for a minute).
const EXPLAIN_BUDGET: usize = 20;
/// CSP reports kept.
pub const MAX_CSP: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn key(self) -> &'static str {
        match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }
}

/// One thing worth fixing.
pub struct Finding {
    /// Stable across refreshes (dismissal is by key).
    pub key: String,
    pub kind: &'static str,
    pub severity: Severity,
    /// Ranking within a severity: higher first.
    pub score: u64,
    pub title: String,
    pub evidence: String,
    pub fix: String,
    /// `.offrails/guides/…#…` (and its public URL).
    pub guide: &'static str,
    pub guide_url: &'static str,
    /// The page that proves it.
    pub href: String,
}

/// A CSP violation a browser reported.
pub struct CspReport {
    pub page: String,
    pub directive: String,
    pub blocked: String,
    pub source: String,
    pub count: u64,
    pub last: DateTime<Utc>,
}

#[derive(Default)]
pub struct InsightState {
    explains: Mutex<HashMap<u64, (Instant, Vec<String>)>>,
    first_counts: Mutex<HashMap<String, (DateTime<Utc>, i64)>>,
    pub csp: Mutex<VecDeque<CspReport>>,
}

/// Only the path of a URL: query strings and fragments carry tokens.
// SECURITY: a reported URL is cut to scheme, host and path before it is stored or shown
// (test: csp_reports_are_collected_without_query_strings).
pub fn strip_url(u: &str) -> String {
    let cut = u.split(['?', '#']).next().unwrap_or_default();
    crate::kernel::ui::truncate(cut, 200)
}

#[derive(Deserialize)]
struct LegacyReport {
    #[serde(rename = "csp-report")]
    report: LegacyBody,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacyBody {
    #[serde(rename = "document-uri")]
    document_uri: String,
    #[serde(rename = "violated-directive")]
    violated: String,
    #[serde(rename = "effective-directive")]
    effective: String,
    #[serde(rename = "blocked-uri")]
    blocked: String,
    #[serde(rename = "source-file")]
    source: String,
    #[serde(rename = "line-number")]
    line: u64,
}

#[derive(Deserialize)]
struct ReportTo {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    body: ReportToBody,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ReportToBody {
    #[serde(rename = "documentURL")]
    document_url: String,
    effective_directive: String,
    #[serde(rename = "blockedURL")]
    blocked_url: String,
    source_file: String,
    line_number: u64,
}

/// Stores the violations in a report body (the `report-uri` or the Reporting API format);
/// how many it found.
pub fn record_csp(body: &[u8]) -> usize {
    let mut found: Vec<(String, String, String, String)> = Vec::new();
    // WHY: by the first byte: serde reads a struct from a JSON array too, so trying the legacy
    // shape first would take a Reporting API array for an empty legacy report.
    let array = body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[');
    if array {
        if let Ok(list) = serde_json::from_slice::<Vec<ReportTo>>(body) {
            for r in list
                .into_iter()
                .filter(|r| r.kind == "csp-violation")
                .take(20)
            {
                let b = r.body;
                found.push((
                    b.document_url,
                    b.effective_directive,
                    b.blocked_url,
                    if b.source_file.is_empty() {
                        String::new()
                    } else {
                        format!("{}:{}", strip_url(&b.source_file), b.line_number)
                    },
                ));
            }
        }
    } else if let Ok(r) = serde_json::from_slice::<LegacyReport>(body) {
        let b = r.report;
        let directive = if b.effective.is_empty() {
            b.violated
        } else {
            b.effective
        };
        found.push((
            b.document_uri,
            directive,
            b.blocked,
            if b.source.is_empty() {
                String::new()
            } else {
                format!("{}:{}", strip_url(&b.source), b.line)
            },
        ));
    }
    found.retain(|(_, directive, _, _)| !directive.is_empty());
    let n = found.len();
    let mut csp = lock(&HUB.insights.csp);
    for (page, directive, blocked, source) in found {
        let page = strip_url(&page);
        let page = page
            .split_once("://")
            .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
            .unwrap_or(page);
        let directive = crate::kernel::ui::truncate(
            directive.split_whitespace().next().unwrap_or_default(),
            60,
        );
        let blocked = strip_url(&blocked);
        if let Some(r) = csp
            .iter_mut()
            .find(|r| r.page == page && r.directive == directive && r.blocked == blocked)
        {
            r.count += 1;
            r.last = Utc::now();
            continue;
        }
        // INVARIANT: bounded (test: the_dashboard_stays_within_its_memory_bound).
        if csp.len() == MAX_CSP {
            csp.pop_front();
        }
        csp.push_back(CspReport {
            page,
            directive,
            blocked,
            source,
            count: 1,
            last: Utc::now(),
        });
    }
    n
}

fn micros(us: u64) -> String {
    super::web::micros(us)
}

/// Every finding, highest impact first.
pub async fn findings(state: &AppState) -> Vec<Finding> {
    let mut out = Vec::new();
    n_plus_one(&mut out);
    slow_routes_and_queries(&mut out);
    rising_errors(&mut out);
    slow_renders(&mut out);
    memory_and_pool(&mut out);
    csp(&mut out);
    let db = &state.db;
    let tables = schema::tables(db).await.unwrap_or_default();
    schema_rules(&tables, &mut out);
    if state.config.is_development() {
        scans(state, &tables, &mut out).await;
    }
    if let Ok(pending) = crate::controllers::migrations::pending(db).await
        && !pending.is_empty()
    {
        out.push(Finding {
            key: "pending-migrations".into(),
            kind: "migrations",
            severity: Severity::High,
            score: pending.len() as u64,
            title: format!(
                "{} pending migration{}",
                pending.len(),
                if pending.len() == 1 { "" } else { "s" }
            ),
            evidence: pending.join(", "),
            fix: "Run `app db migrate` (bin/dev does it on every rebuild).".into(),
            guide: ".offrails/guides/dev-tools.md#pending-migrations",
            guide_url: "https://rustoffrails.org/docs/dev-tools#pending-migrations",
            href: "/dev/dashboard/schema/migrations".into(),
        });
    }
    if state.config.is_development() {
        // Drift: what the migration files imply versus the catalog; files edited after applying.
        let report = super::migrations::report(db, &tables, std::path::Path::new(".")).await;
        out.extend(super::migrations::findings(&report));
    }
    jobs(state, &mut out).await;
    out.extend(custom_rules(state));
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.score.cmp(&a.score)));
    out
}

/// APP: your own rules. Return findings; they are ranked with the others.
///
/// ```ignore
/// if lock(&HUB.http).stats().any(|r| r.route == "/checkout" && r.s5 > 0) {
///     out.push(Finding { key: "checkout-5xx".into(), kind: "custom", severity: Severity::High, … });
/// }
/// ```
pub fn custom_rules(_state: &AppState) -> Vec<Finding> {
    Vec::new()
}

fn n_plus_one(out: &mut Vec<Finding>) {
    let s = lock(&HUB.sql);
    for n in s.n_plus_one() {
        out.push(Finding {
            key: format!("n1-{}-{}", n.route, sql::hex(n.fp)),
            kind: "n_plus_one",
            severity: if n.count >= 20 || n.times >= 10 { Severity::High } else { Severity::Medium },
            score: u64::from(n.count) * n.times,
            title: format!("N+1 on {}", n.route),
            evidence: format!("{} runs {}× in one request ({} requests so far): {}", n.route, n.count, n.times, crate::kernel::ui::truncate(&n.sql, 140)),
            fix: "Load the children in one query (`load_many`, or `filter(Column::ParentId.is_in(ids))`), then add an `assert_query_count_stable` test for the page.".into(),
            guide: ".offrails/guides/models.md#associations",
            guide_url: "https://rustoffrails.org/docs/models#associations",
            href: format!("/dev/dashboard/traces/{}", n.trace),
        });
    }
}

fn slow_routes_and_queries(out: &mut Vec<Finding>) {
    for r in lock(&HUB.http).rows() {
        if r.count >= 3 && r.p95 >= SLOW_ROUTE_P95_US && r.route != super::http::UNMATCHED {
            out.push(Finding {
                key: format!("slow-route-{} {}", r.method, r.route),
                kind: "slow_route",
                severity: if r.p95 >= 3 * SLOW_ROUTE_P95_US { Severity::High } else { Severity::Medium },
                score: r.p95 / 1000 * r.count,
                title: format!("Slow route {} {}", r.method, r.route),
                evidence: format!("p95 {}, p99 {}, max {} over {} requests", micros(r.p95), micros(r.p99), micros(r.max), r.count),
                fix: "Open its slowest trace: the waterfall shows whether the time goes to SQL, a call out or rendering.".into(),
                guide: ".offrails/guides/test-standards.md#6-performance-guards",
                guide_url: "https://rustoffrails.org/docs/test-standards#6-performance-guards",
                href: r.slowest.first().map_or_else(|| "/dev/dashboard/requests".into(), |(id, _, _)| format!("/dev/dashboard/traces/{id}")),
            });
        }
    }
    for q in lock(&HUB.sql).rows(sql::Sort::P95).into_iter().take(10) {
        if q.p95 >= SLOW_QUERY_P95_US {
            out.push(Finding {
                key: format!("slow-sql-{}", q.id),
                kind: "slow_query",
                severity: if q.p95 >= 10 * SLOW_QUERY_P95_US { Severity::High } else { Severity::Medium },
                score: q.total / 1000,
                title: "Slow query".into(),
                evidence: format!("p95 {}, {} runs, {} in total: {}", micros(q.p95), q.count, micros(q.total), crate::kernel::ui::truncate(&q.sql, 140)),
                fix: "EXPLAIN it from its SQL page; add the index the plan asks for, or fetch less (a LIMIT, fewer columns, a cached count).".into(),
                guide: ".offrails/guides/migrations.md#rules",
                guide_url: "https://rustoffrails.org/docs/migrations#rules",
                href: format!("/dev/dashboard/sql/{}", q.id),
            });
        }
    }
}

fn rising_errors(out: &mut Vec<Finding>) {
    let now = Utc::now().timestamp().div_euclid(60);
    for r in lock(&HUB.http).stats() {
        let (recent, before, errors) = r.error_trend(now, 5);
        if errors >= 3 && recent > before * 2.0 + 0.01 {
            out.push(Finding {
                key: format!("errors-{} {}", r.method, r.route),
                kind: "errors",
                severity: if recent >= 0.25 { Severity::High } else { Severity::Medium },
                score: u64::from(errors) * 10,
                title: format!("Rising errors on {} {}", r.method, r.route),
                evidence: format!("{errors} server errors in the last 5 minutes: {:.0}% of requests, up from {:.0}%", recent * 100.0, before * 100.0),
                fix: "Filter the live log by level=error for the cause; every 500 is logged once, with its request id.".into(),
                guide: ".offrails/guides/errors-and-logging.md#response-mapping",
                guide_url: "https://rustoffrails.org/docs/errors-and-logging#response-mapping",
                href: "/dev/dashboard/logs?level=error".into(),
            });
        }
    }
}

fn slow_renders(out: &mut Vec<Finding>) {
    let mut by_route: HashMap<&'static str, (u64, u64, u64)> = HashMap::new();
    for t in lock(&HUB.traces).list(false) {
        for s in t.spans.iter().filter(|s| s.name == "render") {
            let e = by_route.entry(t.route).or_insert((0, 0, t.id));
            e.0 += 1;
            if s.dur_us > e.1 {
                *e = (e.0, s.dur_us, t.id);
            }
        }
    }
    for (route, (_, worst, trace)) in by_route {
        if worst >= SLOW_RENDER_US {
            out.push(Finding {
                key: format!("render-{route}"),
                kind: "render",
                severity: Severity::Low,
                score: worst / 1000,
                title: format!("Slow render on {route}"),
                evidence: format!("a render span took {}", micros(worst)),
                fix: "Compute in the handler, not the template; paginate long lists; render partials once.".into(),
                guide: ".offrails/guides/views.md#layout--partials",
                guide_url: "https://rustoffrails.org/docs/views#layout--partials",
                href: format!("/dev/dashboard/traces/{trace}"),
            });
        }
    }
}

/// Least-squares slope of (t, v), per second.
pub fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = points.iter().fold((0.0, 0.0), |a, p| {
        (a.0 + (p.0 - mx) * (p.1 - my), a.1 + (p.0 - mx).powi(2))
    });
    if den == 0.0 { 0.0 } else { num / den }
}

fn memory_and_pool(out: &mut Vec<Finding>) {
    let now = Utc::now().timestamp();
    let window = lock(&HUB.series).window(Range::H1, now);
    let spiking = HUB.lab.spiking.load(std::sync::atomic::Ordering::Relaxed);
    if window.len() >= 10 && !spiking {
        let pts: Vec<(f64, f64)> = window.iter().map(|s| (s.t as f64, s.rss as f64)).collect();
        let per_sec = slope(&pts);
        let span = (window.last().map_or(0, |s| s.t) - window.first().map_or(0, |s| s.t)) as f64;
        let growth = per_sec * span;
        if growth >= LEAK_BYTES {
            out.push(Finding {
                key: "memory-growth".into(),
                kind: "memory",
                severity: Severity::Medium,
                score: (growth / 1e6) as u64,
                title: "Memory keeps growing".into(),
                evidence: format!("RSS grew {} over the last {} at a steady rate ({}/min)", super::web::bytes(growth as u64), crate::kernel::ui::duration((span * 1000.0) as i64), super::web::bytes((per_sec * 60.0) as u64)),
                fix: "Look for a cache or a Vec that only grows (bound it, like the dashboard bounds itself), or a task per request that never ends.".into(),
                guide: ".offrails/guides/caching.md#key-design",
                guide_url: "https://rustoffrails.org/docs/caching#key-design",
                href: "/dev/dashboard/charts?range=1h#chart-memory".into(),
            });
        }
    }
    let recent = lock(&HUB.series).window(Range::M5, now);
    let saturated = recent
        .iter()
        .filter(|s| s.pool_max > 0 && s.pool_in_use() >= s.pool_max)
        .count();
    if saturated >= 3 {
        out.push(Finding {
            key: "pool-saturated".into(),
            kind: "pool",
            severity: if saturated >= 60 { Severity::High } else { Severity::Medium },
            score: saturated as u64,
            title: "The database pool is saturated".into(),
            evidence: format!("every connection was in use for {saturated} of the last {} seconds; requests waited for one (sqlx doesn't count waiters, so the dashboard counts the seconds with none idle)", recent.len()),
            fix: "Raise DATABASE_POOL, shorten transactions, and never hold a connection across an await on something slow.".into(),
            guide: ".offrails/guides/database.md#kerneldbrs-spec",
            guide_url: "https://rustoffrails.org/docs/database",
            href: "/dev/dashboard/charts#chart-pool".into(),
        });
    }
}

fn csp(out: &mut Vec<Finding>) {
    for r in lock(&HUB.insights.csp).iter() {
        out.push(Finding {
            key: format!("csp-{}-{}-{}", r.page, r.directive, r.blocked),
            kind: "csp",
            severity: Severity::High,
            score: r.count,
            title: format!(
                "CSP blocked {} on {}",
                if r.blocked.is_empty() {
                    "something"
                } else {
                    &r.blocked
                },
                r.page
            ),
            evidence: format!(
                "{} violated {}, {}× (last {}){}",
                r.blocked,
                r.directive,
                r.count,
                crate::kernel::ui::ago(r.last, Utc::now()),
                if r.source.is_empty() {
                    String::new()
                } else {
                    format!(" from {}", r.source)
                }
            ),
            fix: "Move inline script or style into a file under static/; never loosen the CSP."
                .into(),
            guide: ".offrails/guides/security.md#the-header-matrix",
            guide_url: "https://rustoffrails.org/docs/security#the-header-matrix",
            href: "/dev/dashboard/insights#csp".into(),
        });
    }
}

fn schema_rules(tables: &[schema::Table], out: &mut Vec<Finding>) {
    let mut first = lock(&HUB.insights.first_counts);
    let now = Utc::now();
    for t in tables {
        for fk in t.unindexed_fks() {
            out.push(Finding {
                key: format!("fk-index-{}-{}", t.name, fk.columns.join("-")),
                kind: "index",
                severity: if t.rows.unwrap_or(0) >= SCAN_MIN_ROWS { Severity::High } else { Severity::Medium },
                score: t.rows.unwrap_or(0) as u64 + 1,
                title: format!("No index on {}.{}", t.name, fk.columns.join(", ")),
                evidence: format!("{}.{} references {}, and no index starts with it: every lookup by {} (and every delete of a {} row) scans {} rows", t.name, fk.columns.join(", "), fk.table, fk.table, fk.table, t.rows.unwrap_or(0)),
                fix: format!("A migration with `Index::create().table({}).col({})`.", t.name, fk.columns.join(").col(")),
                guide: ".offrails/guides/migrations.md#rules",
                guide_url: "https://rustoffrails.org/docs/migrations#rules",
                href: format!("/dev/dashboard/schema#table-{}", t.name),
            });
        }
        let Some(rows) = t.rows else { continue };
        let (seen_at, seen_rows) = *first.entry(t.name.clone()).or_insert((now, rows));
        let grew = rows - seen_rows;
        if rows >= LARGE_TABLE_ROWS || (grew >= 1_000 && grew * 2 >= seen_rows) {
            out.push(Finding {
                key: format!("table-size-{}", t.name),
                kind: "table",
                severity: Severity::Low,
                score: rows as u64,
                title: if grew >= 1_000 { format!("{} is growing fast", t.name) } else { format!("{} is large", t.name) },
                evidence: format!("{} rows{}{}", crate::kernel::ui::thousands(rows as u64), t.bytes.map(|b| format!(", {}", super::web::bytes(b as u64))).unwrap_or_default(), if grew > 0 { format!(", +{} since {}", crate::kernel::ui::thousands(grew as u64), crate::kernel::ui::ago(seen_at, now)) } else { String::new() }),
                fix: "Make sure every query on it uses an index and a LIMIT; prune old rows with a recurring job.".into(),
                guide: ".offrails/guides/jobs.md#recurring-jobs-srcjobsrecurringrs-the-oban-cron-plugin",
                guide_url: "https://rustoffrails.org/docs/jobs",
                href: format!("/dev/dashboard/schema#table-{}", t.name),
            });
        }
    }
}

/// Recorded SELECTs whose plan scans a table without an index (EXPLAIN, development only).
async fn scans(state: &AppState, tables: &[schema::Table], out: &mut Vec<Finding>) {
    let candidates: Vec<sql::SqlRow> = lock(&HUB.sql)
        .rows(sql::Sort::Count)
        .into_iter()
        .filter(|q| q.select)
        .take(EXPLAIN_BUDGET)
        .collect();
    for q in candidates {
        let Some(id) = sql::parse_hex(&q.id) else {
            continue;
        };
        let cached = lock(&HUB.insights.explains)
            .get(&id)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(60))
            .map(|(_, plan)| plan.clone());
        let plan = match cached {
            Some(p) => p,
            None => {
                let Ok(p) = sql::explain(&state.db, id).await else {
                    continue;
                };
                lock(&HUB.insights.explains).insert(id, (Instant::now(), p.clone()));
                p
            }
        };
        for table in scanned_tables(&plan) {
            let rows = tables
                .iter()
                .find(|t| t.name == table)
                .and_then(|t| t.rows)
                .unwrap_or(0);
            if rows < SCAN_MIN_ROWS {
                continue;
            }
            out.push(Finding {
                key: format!("scan-{}-{table}", q.id),
                kind: "scan",
                severity: if rows >= 10_000 {
                    Severity::High
                } else {
                    Severity::Medium
                },
                score: rows as u64 * q.count / 100 + 1,
                title: format!("Full scan of {table}"),
                evidence: format!(
                    "EXPLAIN shows a scan of {table} ({} rows) for a query run {} times: {}",
                    crate::kernel::ui::thousands(rows as u64),
                    q.count,
                    crate::kernel::ui::truncate(&q.sql, 120)
                ),
                fix: "Index the columns in its WHERE (and ORDER BY), in that order.".into(),
                guide: ".offrails/guides/migrations.md#rules",
                guide_url: "https://rustoffrails.org/docs/migrations#rules",
                href: format!("/dev/dashboard/sql/{}", q.id),
            });
        }
    }
}

/// Tables a plan reads without an index: SQLite's `SCAN t` (not `… USING INDEX`), Postgres'
/// `Seq Scan on t`.
pub fn scanned_tables(plan: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for line in plan {
        let l = line.trim();
        let name = if let Some(rest) = l.strip_prefix("SCAN ") {
            if rest.contains("USING")
                || rest.starts_with("CONSTANT")
                || rest.contains("SUBQUERY")
                || rest.contains("CTE")
            {
                None
            } else {
                rest.split_whitespace().next()
            }
        } else if let Some(at) = l.find("Seq Scan on ") {
            l[at + 12..].split_whitespace().next()
        } else {
            None
        };
        if let Some(n) = name.map(|n| n.trim_matches('"').to_owned())
            && schema::plain_ident(&n)
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
    out
}

async fn jobs(state: &AppState, out: &mut Vec<Finding>) {
    use sea_orm::{ConnectionTrait, Statement};
    let db = &state.db;
    if !panels::table_exists(db, "jobs").await {
        return;
    }
    let Ok(rows) = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT worker, COUNT(*) AS n FROM jobs WHERE state IN ('retryable', 'discarded') \
             GROUP BY worker HAVING COUNT(*) >= 3 ORDER BY n DESC",
        ))
        .await
    else {
        return;
    };
    for r in rows.iter().take(5) {
        let worker: String = r.try_get("", "worker").unwrap_or_default();
        let n: i64 = r.try_get("", "n").unwrap_or(0);
        out.push(Finding {
            key: format!("jobs-{worker}"),
            kind: "jobs",
            severity: if n >= 20 { Severity::High } else { Severity::Medium },
            score: n as u64,
            title: format!("{worker} keeps failing"),
            evidence: format!("{n} jobs of {worker} are retrying or discarded"),
            fix: "Open its errors on the jobs dashboard; a job that fails on every attempt needs a fix, not more retries.".into(),
            guide: ".offrails/guides/jobs.md#observability",
            guide_url: "https://rustoffrails.org/docs/jobs#observability",
            href: format!("/admin/jobs/list?state=retryable&worker={worker}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_reveal_full_scans() {
        let sqlite = vec![
            "SCAN lab_orders".to_owned(),
            "SEARCH lab_order_items USING INDEX x (order_id=?)".to_owned(),
        ];
        assert_eq!(scanned_tables(&sqlite), ["lab_orders"]);
        let pg = vec!["Seq Scan on lab_orders  (cost=0.00..1.20 rows=20 width=8)".to_owned()];
        assert_eq!(scanned_tables(&pg), ["lab_orders"]);
        assert!(scanned_tables(&["SCAN t USING COVERING INDEX i".to_owned()]).is_empty());
    }

    #[test]
    fn a_steady_climb_has_a_positive_slope() {
        let pts: Vec<(f64, f64)> = (0..10)
            .map(|i| (f64::from(i), f64::from(i) * 2.0 + 1.0))
            .collect();
        assert!((slope(&pts) - 2.0).abs() < 1e-9);
        assert_eq!(slope(&[(0.0, 1.0)]), 0.0);
    }
}
