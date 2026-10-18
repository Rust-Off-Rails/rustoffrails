//! What the "Ask AI" assistant may tell a provider about each dashboard page: one builder per tab,
//! each a plain-text summary built from data the dashboard already holds **in redacted form**.
//!
//! The rules every builder follows (and a new one must):
//! - SQL is the recorded fingerprint (literals stripped), parameters as type and length only;
//! - paths are the recorded ones (`kernel::redact::path` already applied), routes are patterns;
//! - free text (log lines, failure output, error messages) goes through `kernel::redact::text`;
//! - schema is names, types, keys and counts, never a row;
//! - other samples' tables (the mailbox, the job queue) are read by name, when they exist, and
//!   only as shapes: a mail's subject, masked addresses, sizes and link hosts (never its body or
//!   a code); a job's worker, state, attempts and redacted error lines (argument names and types,
//!   never values);
//! - never env values, config, cookies, headers, tokens, or the load lab's rows; GitHub only as
//!   workflow names, conclusions, branches and counts (`ai_tools::github_status`);
//! - the read-only production view (`ro`) gets less: no parameters, no log events, no span fields.
//!
//! Each builder is bounded (top N rows); `ai::cap` cuts the whole at `MAX_CONTEXT_CHARS`.

use std::fmt::Write as _;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;

use super::ai::Tab;
use super::web::{micros, route_findings, route_problems};
use super::{HUB, insights, lock, migrations, route_explorer as rx, schema, sql, testruns};
use crate::kernel::redact;
use crate::state::AppState;

/// Free text, redacted (secrets, and email addresses' local parts) and shortened.
pub(super) fn clean(s: &str, max: usize) -> String {
    crate::kernel::ui::truncate(&mask_emails(&redact::text(s)), max)
}

/// "write to ann@example.com" → "write to [redacted]@example.com": people's addresses are
/// personal data a provider doesn't need.
pub fn mask_emails(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match word.find('@') {
            Some(at) if at > 0 && word[at + 1..].contains('.') => {
                let start = word[..at]
                    .rfind(|c: char| {
                        !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-'))
                    })
                    .map_or(0, |i| i + 1);
                out.push_str(&word[..start]);
                out.push_str("[redacted]");
                out.push_str(&word[at..]);
            }
            _ => out.push_str(word),
        }
        word.clear();
    };
    for c in s.chars() {
        if c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"' | '\'' | '(' | ')') {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The context for `tab` (and one item of it). `Err` when the item is gone.
pub async fn build(
    state: &AppState,
    ro: bool,
    tab: Tab,
    item: Option<&str>,
) -> Result<String, String> {
    match (tab, item) {
        (Tab::Overview, _) => Ok(overview()),
        (Tab::Insights, None) => Ok(insights_all(state).await),
        (Tab::Insights, Some(key)) => finding(state, key).await,
        (Tab::Requests, _) => Ok(requests()),
        (Tab::Traces, None) => Ok(traces()),
        (Tab::Traces, Some(id)) => trace(id, ro),
        (Tab::Sql, None) => Ok(sql_all(ro)),
        (Tab::Sql, Some(id)) => sql_one(state, id, ro).await,
        (Tab::Logs, _) => Ok(logs()),
        (Tab::Schema, None) => Ok(schema_all(state).await),
        (Tab::Schema, Some(t)) => schema_one(state, t).await,
        (Tab::Migrations, _) => Ok(migrations_ctx(state).await),
        (Tab::Routes, _) => Ok(routes(state).await),
        (Tab::Tests, _) => Ok(tests()),
        (Tab::Dev, _) => Ok(dev_home(state).await),
        (Tab::Mailbox, None) => Ok(mailbox(state).await),
        (Tab::Mailbox, Some(id)) => email(state, id).await,
        (Tab::Jobs, None) => Ok(jobs(state).await),
        (Tab::Jobs, Some("errors")) => Ok(job_errors(state, 15).await),
        (Tab::Jobs, Some(id)) => job(state, id).await,
        (Tab::Sockets, _) => Ok(sockets(state).await),
        (Tab::Upstreams, _) => Ok(super::upstreams::list_text()),
        (Tab::Security, _) => Ok(super::threats::list_text(ro)),
        (Tab::Frontend, _) => Ok(super::rum::list_text()),
    }
}

/// APP: the overview: the latest second's numbers, the busiest and slowest routes and queries.
pub(super) fn overview() -> String {
    let mut s = String::from("## Latest numbers (one-second sample)\n");
    if let Some(l) = lock(&HUB.series).latest() {
        let _ = writeln!(
            s,
            "requests/s {:.1}, 4xx/s {:.1}, 5xx/s {:.1}, latency p50 {} p95 {} p99 {}, SQL/s {:.1}, \
             memory {} MB, CPU {:.0}%, pool {} of {} (idle {}), tokio tasks {}",
            l.req,
            l.e4,
            l.e5,
            micros(u64::from(l.p50)),
            micros(u64::from(l.p95)),
            micros(u64::from(l.p99)),
            l.sql,
            l.rss / 1_048_576,
            l.cpu,
            l.pool_size,
            l.pool_max,
            l.pool_idle,
            l.tasks
        );
    } else {
        s.push_str("(no sample yet)\n");
    }
    s.push_str("\n## Slowest routes (p95)\n");
    let mut rows = lock(&HUB.http).rows();
    rows.sort_by_key(|r| std::cmp::Reverse(r.p95));
    for r in rows.iter().take(8) {
        let _ = writeln!(
            s,
            "- {} {}: {} requests, p95 {}, 5xx {}, 4xx {}",
            r.method,
            r.route,
            r.count,
            micros(r.p95),
            r.s5,
            r.s4
        );
    }
    s.push_str("\n## Most expensive SQL (total time)\n");
    let stats = lock(&HUB.sql);
    for q in stats.rows(sql::Sort::Total).iter().take(8) {
        let _ = writeln!(
            s,
            "- id {}: {}× total {} p95 {}: {}",
            q.id,
            q.count,
            micros(q.total),
            micros(q.p95),
            clean(&q.sql, 300)
        );
    }
    let n = stats.n_plus_one();
    let _ = writeln!(s, "\nN+1 patterns flagged: {}", n.len());
    for x in n.iter().take(5) {
        let _ = writeln!(
            s,
            "- {} ran sql {} {}× in one request (trace {})",
            x.route,
            sql::hex(x.fp),
            x.count,
            x.trace
        );
    }
    s
}

/// APP: every finding, as the Insights page ranks them.
pub(super) async fn insights_all(state: &AppState) -> String {
    let mut all = insights::findings(state).await;
    all.extend(route_findings(state).await);
    all.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.score.cmp(&a.score)));
    let mut s = String::from("## Findings, most important first\n");
    for f in all.iter().take(30) {
        let _ = writeln!(
            s,
            "- finding {} [{}] {} ({}). Evidence: {}. Suggested fix: {}. Guide: {}",
            super::web::finding_id(&f.key),
            f.severity.key(),
            clean(&f.title, 200),
            f.kind,
            clean(&f.evidence, 400),
            clean(&f.fix, 300),
            f.guide
        );
    }
    if all.is_empty() {
        s.push_str("(no findings)\n");
    }
    s
}

/// APP: one finding, with the data behind it when it points at a query or a trace.
pub(super) async fn finding(state: &AppState, key: &str) -> Result<String, String> {
    let mut all = insights::findings(state).await;
    all.extend(route_findings(state).await);
    let f = all
        .into_iter()
        .find(|f| super::web::finding_id(&f.key) == key)
        .ok_or("That finding is gone (fixed, or the data aged out).")?;
    let mut s = format!(
        "## Finding\n[{}] {} ({})\nEvidence: {}\nSuggested fix: {}\nGuide: {}\n",
        f.severity.key(),
        clean(&f.title, 200),
        f.kind,
        clean(&f.evidence, 800),
        clean(&f.fix, 400),
        f.guide
    );
    if let Some(id) = f.href.strip_prefix("/dev/dashboard/sql/")
        && let Ok(q) = sql_one(state, id, false).await
    {
        s.push('\n');
        s.push_str(&q);
    }
    Ok(s)
}

/// APP: requests per route.
pub(super) fn requests() -> String {
    let mut s = String::from("## Requests per route\n");
    let mut rows = lock(&HUB.http).rows();
    rows.sort_by_key(|r| std::cmp::Reverse(r.count));
    for r in rows.iter().take(40) {
        let _ = writeln!(
            s,
            "- {} {}: {} hits (2xx {}, 3xx {}, 4xx {}, 5xx {}), p50 {} p95 {} p99 {} max {}, {} bytes total, N+1 in {}",
            r.method,
            r.route,
            r.count,
            r.s2,
            r.s3,
            r.s4,
            r.s5,
            micros(r.p50),
            micros(r.p95),
            micros(r.p99),
            micros(r.max),
            r.bytes,
            r.n_plus_one
        );
    }
    s
}

/// APP: the recent traces, notable ones first.
pub(super) fn traces() -> String {
    let mut s = String::from("## Recent requests (notable first: 5xx, N+1, slow)\n");
    let traces = lock(&HUB.traces);
    let mut list = traces.list(true);
    list.extend(traces.list(false).into_iter().take(20));
    list.dedup_by_key(|t| t.id);
    for t in list.iter().take(40) {
        let _ = writeln!(
            s,
            "- trace {}: {} {} ({}) → {} in {}, {} SQL statements ({}){}",
            t.id,
            t.method,
            t.route,
            clean(&t.path, 120),
            t.status,
            micros(t.dur_us),
            t.statements,
            micros(t.sql_us),
            if t.n_plus_one.is_empty() {
                ""
            } else {
                ", N+1 flagged"
            }
        );
    }
    s
}

/// APP: one trace: its spans, its SQL in order, and (development only) its log events.
pub(super) fn trace(id: &str, ro: bool) -> Result<String, String> {
    let t = id
        .parse::<u64>()
        .ok()
        .and_then(super::trace::find)
        .ok_or("That trace is no longer kept.")?;
    let mut s = format!(
        "## Trace {}\n{} {} (path {}) → {} in {}; {} SQL statements taking {}\n\n### Spans (start, duration)\n",
        t.id,
        t.method,
        t.route,
        clean(&t.path, 160),
        t.status,
        micros(t.dur_us),
        t.statements,
        micros(t.sql_us)
    );
    for sp in t.spans.iter().take(60) {
        let _ = writeln!(
            s,
            "- {} ({}) at +{} for {}",
            sp.name,
            sp.target,
            micros(sp.start_us),
            micros(sp.dur_us)
        );
    }
    s.push_str("\n### SQL, in order (fingerprints: literals stripped)\n");
    for q in t.sql.iter().take(80) {
        let _ = writeln!(
            s,
            "- +{} {}: {}",
            micros(q.start_us),
            micros(q.dur_us),
            clean(&q.sql, 300)
        );
    }
    for (_, n, q) in &t.n_plus_one {
        let _ = writeln!(s, "\nN+1: ran {n} times in this request: {}", clean(q, 300));
    }
    if !ro && !t.events.is_empty() {
        s.push_str("\n### Log events (redacted)\n");
        for e in t.events.iter().take(40) {
            let _ = writeln!(
                s,
                "- +{} {}: {}",
                micros(e.at_us),
                e.level,
                clean(&e.message, 300)
            );
        }
    }
    Ok(s)
}

/// APP: every statement by fingerprint, the N+1 list, writes during a GET.
pub(super) fn sql_all(ro: bool) -> String {
    let stats = lock(&HUB.sql);
    let mut s = String::from("## SQL by fingerprint (total time first)\n");
    for q in stats.rows(sql::Sort::Total).iter().take(30) {
        let _ = writeln!(
            s,
            "- id {}: {}× total {} mean {} p95 {} max {}, failed {}, routes {}{}: {}",
            q.id,
            q.count,
            micros(q.total),
            micros(q.mean),
            micros(q.p95),
            micros(q.max),
            q.failed,
            q.routes.join(" "),
            if ro || q.params.is_empty() {
                String::new()
            } else {
                format!(", parameters {}", q.params.join(", "))
            },
            clean(&q.sql, 400)
        );
    }
    s.push_str("\n## N+1 (the same statement more than 5 times in one request)\n");
    for n in stats.n_plus_one().iter().take(20) {
        let _ = writeln!(
            s,
            "- {} ran it {}× in one request ({} requests did): {}",
            n.route,
            n.count,
            n.times,
            clean(&n.sql, 300)
        );
    }
    drop(stats);
    let writes = lock(&HUB.http).get_writes().len();
    let _ = writeln!(s, "\nWrites during a GET: {writes}");
    s
}

/// APP: one statement: its numbers, the routes that run it, and (development) its plan, with
/// literals stripped from the plan (a Postgres plan can quote a bound value).
pub(super) async fn sql_one(state: &AppState, id: &str, ro: bool) -> Result<String, String> {
    let fp = sql::parse_hex(id).ok_or("No such query.")?;
    let q = lock(&HUB.sql)
        .get(fp)
        .ok_or("That query is no longer kept.")?;
    let mut s = format!(
        "## Query {}\n{}\n\n{}× total {} mean {} p95 {} max {}, failed {}\nRoutes: {}\n",
        q.id,
        clean(&q.sql, 2_000),
        q.count,
        micros(q.total),
        micros(q.mean),
        micros(q.p95),
        micros(q.max),
        q.failed,
        q.routes.join(", ")
    );
    if !ro && !q.params.is_empty() {
        let _ = writeln!(
            s,
            "Parameters (type and length only): {}",
            q.params.join(", ")
        );
    }
    if let Some(n) = lock(&HUB.sql).n_plus_one().iter().find(|n| n.fp == fp) {
        let _ = writeln!(
            s,
            "Flagged N+1: {} ran it {}× in one request.",
            n.route, n.count
        );
    }
    if !ro
        && q.select
        && let Ok(plan) = sql::explain(&state.db, fp).await
    {
        s.push_str("\n### Query plan\n");
        for line in plan.iter().take(40) {
            let _ = writeln!(s, "{}", sql::fingerprint(line));
        }
    }
    Ok(s)
}

/// APP: the live log tail's latest lines, redacted.
pub(super) fn logs() -> String {
    let mut s = String::from("## Latest log lines (newest last, redacted)\n");
    let lines = lock(&HUB.logs).lines();
    let start = lines.len().saturating_sub(60);
    for l in &lines[start..] {
        let _ = writeln!(
            s,
            "- {} {} {}: {}{}",
            l.at.format("%H:%M:%S"),
            l.level,
            l.target,
            clean(&l.message, 300),
            if l.fields.is_empty() {
                String::new()
            } else {
                format!(" [{}]", clean(&l.fields, 200))
            }
        );
    }
    s
}

pub(super) fn table_text(t: &schema::Table, s: &mut String) {
    let _ = writeln!(
        s,
        "### {} ({} rows)",
        t.name,
        t.rows.map_or_else(|| "?".into(), |n| n.to_string())
    );
    for c in &t.columns {
        let _ = writeln!(
            s,
            "- {} {}{}{}",
            c.name,
            c.ty,
            if c.nullable { " null" } else { " not null" },
            if c.pk { " primary key" } else { "" }
        );
    }
    for i in &t.indexes {
        let _ = writeln!(
            s,
            "- index {} ({}){}",
            i.name,
            i.columns.join(", "),
            if i.unique { " unique" } else { "" }
        );
    }
    for fk in &t.fks {
        let _ = writeln!(
            s,
            "- foreign key ({}) → {}({}) on delete {}",
            fk.columns.join(", "),
            fk.table,
            fk.to.join(", "),
            fk.on_delete
        );
    }
}

/// APP: the schema: names, types, keys, indexes, foreign keys and row counts. Never a row.
pub(super) async fn schema_all(state: &AppState) -> String {
    let tables = schema::tables(&state.db).await.unwrap_or_default();
    let mut s = format!("## Schema ({}, {} tables)\n", backend(state), tables.len());
    for t in &tables {
        table_text(t, &mut s);
    }
    s
}

/// APP: one table, the queries that touch it, the migration that made it.
pub(super) async fn schema_one(state: &AppState, name: &str) -> Result<String, String> {
    let tables = schema::tables(&state.db).await.unwrap_or_default();
    let t = tables
        .iter()
        .find(|t| t.name == name)
        .ok_or("No such table.")?;
    let mut s = format!("## Table ({})\n", backend(state));
    table_text(t, &mut s);
    let referencing: Vec<String> = tables
        .iter()
        .filter(|o| o.fks.iter().any(|fk| fk.table == name))
        .map(|o| o.name.clone())
        .collect();
    if !referencing.is_empty() {
        let _ = writeln!(s, "Referenced by: {}", referencing.join(", "));
    }
    s.push_str("\n### Recent queries touching it\n");
    for q in super::web::queries_touching(name, 10) {
        let _ = writeln!(s, "- {}× p95 {}: {}", q.count, q.p95, clean(&q.sql, 300));
    }
    let report = migrations::report(&state.db, &tables, Path::new(".")).await;
    for m in &report.migrations {
        for e in &m.effects {
            if matches!(e, migrations::Effect::CreateTable { table, .. } | migrations::Effect::AlterTable { table, .. } if table == name)
            {
                let _ = writeln!(s, "Migration {}: {}", m.name, e.text());
            }
        }
    }
    Ok(s)
}

/// APP: the migrations timeline and the drift.
pub(super) async fn migrations_ctx(state: &AppState) -> String {
    let tables = schema::tables(&state.db).await.unwrap_or_default();
    let report = migrations::report(&state.db, &tables, Path::new(".")).await;
    let mut s = format!("## Migrations ({}), oldest first\n", backend(state));
    for m in &report.migrations {
        let _ = writeln!(
            s,
            "- {} [{}] {}{}: {}",
            m.name,
            m.area,
            if m.applied() { "applied" } else { "PENDING" },
            if m.edited {
                ", edited after it was applied"
            } else {
                ""
            },
            m.effects
                .iter()
                .map(migrations::Effect::text)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    s.push_str("\n## Drift between the migrations and the database\n");
    for d in &report.drift {
        let _ = writeln!(s, "- {}: {}", d.title, d.detail);
    }
    if report.drift.is_empty() {
        s.push_str("(none)\n");
    }
    for (n, _) in &report.unknown_applied {
        let _ = writeln!(s, "- applied but unknown to this code: {n}");
    }
    s
}

/// APP: the route table with live numbers and the detectors' problems.
pub(super) async fn routes(state: &AppState) -> String {
    let p = route_problems(state).await;
    let stats = lock(&HUB.http).rows();
    let mut s = String::from("## Routes (method, pattern, handler, access, hits, p95)\n");
    for r in rx::table() {
        let st = stats
            .iter()
            .find(|x| x.route == r.path && x.method == r.method);
        let _ = writeln!(
            s,
            "- {} {} → {} [{}] {}",
            r.method,
            r.path,
            r.handler,
            rx::access_label(r.access),
            st.map_or_else(
                || "never hit".to_owned(),
                |x| format!("{} hits, p95 {}, 5xx {}", x.count, micros(x.p95), x.s5)
            )
        );
    }
    let list = |s: &mut String, label: &str, v: &[String]| {
        if !v.is_empty() {
            let _ = writeln!(s, "\n{label}: {}", v.join(", "));
        }
    };
    list(&mut s, "Mutating routes without CSRF", &p.without_csrf);
    list(&mut s, "Admin or dev paths without a gate", &p.ungated);
    list(
        &mut s,
        "Open to anyone in production",
        &p.open_in_production,
    );
    list(&mut s, "Missing from docs/routes.md", &p.missing_docs);
    list(&mut s, "No test mentions them", &p.untested);
    for (a, b) in &p.overlaps {
        let _ = writeln!(s, "Overlapping patterns: {a} and {b}");
    }
    s
}

/// APP: the latest test run: per database, failures with (redacted) output, the slowest tests.
pub(super) fn tests() -> String {
    let dir = testruns::root().join(testruns::DIR);
    let runs = testruns::load_runs(&dir);
    let Some(run) = runs.first() else {
        return "No test runs yet (`app test`, or the Run tests button).\n".into();
    };
    let mut s = format!("## Latest test run ({}, {})\n", run.id, run.source);
    for suite in &run.suites {
        let _ = writeln!(
            s,
            "- {:?}: {} passed, {} failed, {} skipped, {} flaky, {} ms",
            suite.db, suite.passed, suite.failed, suite.skipped, suite.flaky, suite.duration_ms
        );
    }
    for db in [testruns::Db::Sqlite, testruns::Db::Postgres] {
        let mut cases = testruns::load_cases(&dir, &run.id, db);
        let failed: Vec<&testruns::Case> = cases
            .iter()
            .filter(|c| matches!(c.outcome, testruns::Outcome::Failed))
            .collect();
        if !failed.is_empty() {
            let _ = writeln!(s, "\n### Failures on {db:?}");
            for c in failed.iter().take(10) {
                let _ = writeln!(
                    s,
                    "- {} ({} ms):\n{}",
                    c.name,
                    c.time_ms,
                    clean(&c.output, 1_200)
                );
            }
        }
        cases.sort_by_key(|c| std::cmp::Reverse(c.time_ms));
        if !cases.is_empty() {
            let _ = writeln!(s, "\n### Slowest on {db:?}");
            for c in cases.iter().take(8) {
                let _ = writeln!(s, "- {} {} ms", c.name, c.time_ms);
            }
        }
    }
    s
}

pub(super) fn backend(state: &AppState) -> &'static str {
    match state.db.get_database_backend() {
        sea_orm::DbBackend::Postgres => "Postgres",
        _ => "SQLite",
    }
}

// ---- other samples' pages (the Ask AI bar) ------------------------------------------------------

/// A boxed future, for `Source`.
pub type SourceFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;

/// APP: a source of page data another sample plugs in through optional wiring (the realtime
/// sample's `ai_realtime.rs`, at `dashboard ai sources`). Its text joins the context of the tab
/// whose key it returns, and the `app_snapshot` tool. Keep it to redacted shapes and counts.
pub trait Source: Send + Sync {
    /// The tab key it adds to (`"sockets"`, `"jobs"`, …).
    fn tab(&self) -> &'static str;
    fn context<'a>(&'a self, state: &'a AppState) -> SourceFuture<'a>;
}

/// The registered sources. Optional wires add theirs above the anchor.
pub fn sources() -> Vec<Box<dyn Source>> {
    vec![
        // samples: dashboard ai sources
    ]
}

/// The sources' text for `tab`.
pub(super) async fn source_text(state: &AppState, tab: &str) -> String {
    let mut s = String::new();
    for src in sources().into_iter().filter(|x| x.tab() == tab) {
        if let Some(t) = src.context(state).await {
            s.push_str(&t);
            if !t.ends_with('\n') {
                s.push('\n');
            }
        }
    }
    s
}

/// Rows of a read-only query, every column as text (`CAST(… AS TEXT)` in the SQL), or none when
/// the table is missing.
pub(super) async fn text_rows(
    state: &AppState,
    sql: &str,
    values: Vec<sea_orm::Value>,
    cols: &[&str],
) -> Vec<Vec<String>> {
    let backend = state.db.get_database_backend();
    let sql = if backend == DbBackend::Postgres {
        let mut n = 0;
        sql.chars()
            .map(|c| {
                if c == '?' {
                    n += 1;
                    format!("${n}")
                } else {
                    c.to_string()
                }
            })
            .collect::<String>()
    } else {
        sql.to_owned()
    };
    let Ok(rows) = state
        .db
        .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
        .await
    else {
        return Vec::new();
    };
    rows.iter()
        .map(|r| {
            cols.iter()
                .map(|c| {
                    r.try_get::<Option<String>>("", c)
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect()
}

/// "Ann <ann@example.com>, b@example.org" → "[redacted]@example.com, [redacted]@example.org".
pub fn mask_addresses(list: &str) -> String {
    list.split(',')
        .filter_map(|a| {
            let a = a.trim();
            let at = a.rfind('@')?;
            let domain: String = a[at + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                .collect();
            Some(format!("[redacted]@{domain}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The scheme and host of every link in `body`, counted ("https://example.com ×2").
fn link_hosts(body: &str) -> (usize, usize, String) {
    let mut hosts: Vec<(String, usize)> = Vec::new();
    let (mut total, mut plain) = (0, 0);
    for scheme in ["https://", "http://"] {
        let mut rest = body;
        while let Some(i) = rest.find(scheme) {
            let tail = &rest[i + scheme.len()..];
            let host: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
                .collect();
            rest = &tail[host.len()..];
            if host.is_empty() {
                continue;
            }
            total += 1;
            if scheme == "http://" {
                plain += 1;
            }
            let key = format!("{scheme}{host}");
            match hosts.iter_mut().find(|(h, _)| *h == key) {
                Some((_, n)) => *n += 1,
                None => hosts.push((key, 1)),
            }
        }
    }
    let list = hosts
        .iter()
        .take(8)
        .map(|(h, n)| format!("{h} ×{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    (total, plain, list)
}

/// APP: the dev mailbox (the mailer sample's `mailbox_messages` table): the latest messages as
/// shapes. Never a body, a link's path or a code.
pub(super) async fn mailbox(state: &AppState) -> String {
    if !super::panels::table_exists(&state.db, "mailbox_messages").await {
        return "The dev mailbox isn't installed (no mailbox_messages table).\n".into();
    }
    let rows = text_rows(
        state,
        "SELECT CAST(id AS TEXT) AS id, subject, from_addr, to_addr, CAST(created_at AS TEXT) AS at, \
         CAST(LENGTH(\"text\") AS TEXT) AS tl, CAST(LENGTH(html) AS TEXT) AS hl \
         FROM mailbox_messages ORDER BY id DESC LIMIT 30",
        vec![],
        &["id", "subject", "from_addr", "to_addr", "at", "tl", "hl"],
    )
    .await;
    let mut s = format!(
        "## Dev mailbox: the latest {} messages, newest first (subjects are app data; addresses masked; bodies never sent)\n",
        rows.len()
    );
    for r in &rows {
        let _ = writeln!(
            s,
            "- mail {}: \"{}\" from {} to {} at {}; text part {} chars, html {} chars",
            r[0],
            clean(&r[1], 140),
            mask_addresses(&r[2]),
            mask_addresses(&r[3]),
            r[4].get(..19).unwrap_or(&r[4]),
            r[5],
            r[6]
        );
    }
    if rows.is_empty() {
        s.push_str("(empty)\n");
    }
    s
}

/// APP: one message: subject, masked addresses, header names, sizes, link hosts, attachments
/// (names and types), and the checks a reviewer would make. Never the body or a code.
pub(super) async fn email(state: &AppState, id: &str) -> Result<String, String> {
    let id: i64 = id.parse().map_err(|_| "No such message.")?;
    let rows = text_rows(
        state,
        "SELECT subject, from_addr, to_addr, CAST(cc AS TEXT) AS cc, reply_to, CAST(headers AS TEXT) AS headers, \
         \"text\" AS body_text, html AS body_html, CAST(attachments AS TEXT) AS attachments, CAST(created_at AS TEXT) AS at \
         FROM mailbox_messages WHERE id = ?",
        vec![id.into()],
        &["subject", "from_addr", "to_addr", "cc", "reply_to", "headers", "body_text", "body_html", "attachments", "at"],
    )
    .await;
    let r = rows
        .first()
        .ok_or("That message is gone (deleted or cleared).")?;
    let names = |v: &Value| -> Vec<String> {
        match v {
            Value::Object(m) => m.keys().cloned().collect(),
            Value::Array(a) => a
                .iter()
                .filter_map(|h| {
                    h.get(0)
                        .or_else(|| h.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    let headers: Value = serde_json::from_str(&r[5]).unwrap_or(Value::Null);
    let header_names = names(&headers);
    let has = |n: &str| header_names.iter().any(|h| h.eq_ignore_ascii_case(n));
    let (links, plain, hosts) = link_hosts(&format!("{} {}", r[6], r[7]));
    let attachments: Value = serde_json::from_str(&r[8]).unwrap_or(Value::Null);
    let mut s = format!(
        "## Mail {id} (masked addresses; the body is never sent)\nSubject: \"{}\"\nFrom {} to {}{}{}\nAt {}\n\
         Text part: {} chars; HTML part: {} chars\nHeaders present: {}\nLinks: {links} ({hosts}){}\n",
        clean(&r[0], 200),
        mask_addresses(&r[1]),
        mask_addresses(&r[2]),
        if r[3].len() > 2 {
            format!(", cc {}", mask_addresses(&r[3]))
        } else {
            String::new()
        },
        if r[4].is_empty() {
            String::new()
        } else {
            format!(", reply-to {}", mask_addresses(&r[4]))
        },
        r[9].get(..19).unwrap_or(&r[9]),
        r[6].chars().count(),
        r[7].chars().count(),
        header_names
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        if plain > 0 {
            format!("; {plain} over plain http")
        } else {
            String::new()
        },
    );
    for a in attachments.as_array().into_iter().flatten().take(10) {
        let name = a
            .get("filename")
            .or_else(|| a.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        let ty = a.get("content_type").and_then(Value::as_str).unwrap_or("?");
        let _ = writeln!(s, "Attachment: {} ({ty})", clean(name, 80));
    }
    s.push_str("\n### Checks\n");
    let checks = [
        (
            r[6].trim().is_empty(),
            "no text/plain part (some clients and filters need it)",
        ),
        (r[0].trim().is_empty(), "an empty subject"),
        (
            !has("Message-ID") && !header_names.is_empty(),
            "no Message-ID header",
        ),
        (plain > 0, "links over plain http"),
    ];
    let mut any = false;
    for (bad, what) in checks {
        if bad {
            any = true;
            let _ = writeln!(s, "- {what}");
        }
    }
    if !any {
        s.push_str("- nothing obviously wrong\n");
    }
    Ok(s)
}

/// The first line of an error with numbers and ids replaced, so retries of one cause group.
pub fn error_signature(e: &str) -> String {
    let first = e.lines().next().unwrap_or_default();
    let mut out = String::new();
    for word in first.split_whitespace().take(24) {
        let w = if word.chars().any(|c| c.is_ascii_digit()) {
            "N"
        } else {
            word
        };
        out.push_str(w);
        out.push(' ');
    }
    mask_emails(&redact::text(out.trim()))
}

/// "email: string (16 chars), count: number" for a job's arguments: names and types only.
pub fn arg_shapes(v: &Value) -> String {
    let shape = |v: &Value| match v {
        Value::String(s) => format!("string ({} chars)", s.chars().count()),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "bool".into(),
        Value::Null => "null".into(),
        Value::Array(a) => format!("array ({} items)", a.len()),
        Value::Object(o) => format!("object ({} keys)", o.len()),
    };
    match v {
        Value::Object(o) => o
            .iter()
            .take(20)
            .map(|(k, v)| format!("{}: {}", clean(k, 40), shape(v)))
            .collect::<Vec<_>>()
            .join(", "),
        other => shape(other),
    }
}

/// APP: the job queue (the jobs sample's `jobs` table): counts per state, queue and worker, the
/// oldest waiting job, the failing workers.
pub(super) async fn jobs(state: &AppState) -> String {
    if !super::panels::table_exists(&state.db, "jobs").await {
        return "The job queue isn't installed (no jobs table).\n".into();
    }
    let rows = text_rows(
        state,
        "SELECT state, queue, worker, CAST(COUNT(*) AS TEXT) AS n FROM jobs GROUP BY state, queue, worker ORDER BY worker",
        vec![],
        &["state", "queue", "worker", "n"],
    )
    .await;
    let mut s = String::from("## Job queue: jobs per worker, queue and state\n");
    for r in &rows {
        let _ = writeln!(s, "- {} on queue {}: {} {}", r[2], r[1], r[3], r[0]);
    }
    if rows.is_empty() {
        s.push_str("(no jobs)\n");
    }
    let oldest = text_rows(
        state,
        "SELECT CAST(MIN(scheduled_at) AS TEXT) AS t FROM jobs WHERE state = 'available'",
        vec![],
        &["t"],
    )
    .await;
    if let Some(t) = oldest
        .first()
        .map(|r| r[0].clone())
        .filter(|t| !t.is_empty())
    {
        let _ = writeln!(
            s,
            "\nOldest available job scheduled at {}",
            t.get(..19).unwrap_or(&t)
        );
    }
    s.push('\n');
    s.push_str(&job_errors(state, 8).await);
    s
}

/// APP: the latest failures grouped by worker and cause (the first line of the error, numbers
/// stripped), most frequent first.
pub(super) async fn job_errors(state: &AppState, limit: usize) -> String {
    if !super::panels::table_exists(&state.db, "jobs").await {
        return "The job queue isn't installed (no jobs table).\n".into();
    }
    let rows = text_rows(
        state,
        "SELECT CAST(id AS TEXT) AS id, worker, state, CAST(errors AS TEXT) AS errors FROM jobs \
         WHERE CAST(errors AS TEXT) <> '[]' ORDER BY id DESC LIMIT 500",
        vec![],
        &["id", "worker", "state", "errors"],
    )
    .await;
    /// A cause: worker, signature, example ids, the states seen, how many jobs.
    struct Group {
        worker: String,
        sig: String,
        ids: Vec<String>,
        states: Vec<String>,
        n: usize,
    }
    let mut groups: Vec<Group> = Vec::new();
    for r in &rows {
        let errors: Value = serde_json::from_str(&r[3]).unwrap_or(Value::Null);
        let Some(last) = errors
            .as_array()
            .and_then(|a| a.last())
            .and_then(|e| e.get("error"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let sig = error_signature(last);
        match groups.iter_mut().find(|g| g.worker == r[1] && g.sig == sig) {
            Some(g) => {
                g.n += 1;
                if g.ids.len() < 3 {
                    g.ids.push(r[0].clone());
                }
                if !g.states.contains(&r[2]) {
                    g.states.push(r[2].clone());
                }
            }
            None => groups.push(Group {
                worker: r[1].clone(),
                sig,
                ids: vec![r[0].clone()],
                states: vec![r[2].clone()],
                n: 1,
            }),
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.n));
    let mut s =
        String::from("## Job failures, grouped by worker and cause (error text redacted)\n");
    for g in groups.iter().take(limit) {
        let _ = writeln!(
            s,
            "- {}: {} jobs ({}), e.g. job {}: {}",
            g.worker,
            g.n,
            g.states.join(", "),
            g.ids.join(", job "),
            crate::kernel::ui::truncate(&g.sig, 220)
        );
    }
    if groups.is_empty() {
        s.push_str("(no failures)\n");
    }
    s
}

/// APP: one job: worker, queue, state, attempts, times, its arguments' names and types, and its
/// latest errors (redacted).
pub(super) async fn job(state: &AppState, id: &str) -> Result<String, String> {
    let id: i64 = id.parse().map_err(|_| "No such job.")?;
    let rows = text_rows(
        state,
        "SELECT worker, queue, state, CAST(attempt AS TEXT) AS attempt, CAST(max_attempts AS TEXT) AS max_attempts, \
         CAST(priority AS TEXT) AS priority, CAST(args AS TEXT) AS args, CAST(errors AS TEXT) AS errors, \
         CAST(inserted_at AS TEXT) AS inserted_at, CAST(scheduled_at AS TEXT) AS scheduled_at, \
         CAST(attempted_at AS TEXT) AS attempted_at, CAST(completed_at AS TEXT) AS completed_at FROM jobs WHERE id = ?",
        vec![id.into()],
        &["worker", "queue", "state", "attempt", "max_attempts", "priority", "args", "errors", "inserted_at", "scheduled_at", "attempted_at", "completed_at"],
    )
    .await;
    let r = rows
        .first()
        .ok_or("That job is gone (pruned, or never existed).")?;
    let short = |t: &String| t.get(..19).unwrap_or(t).to_owned();
    let args: Value = serde_json::from_str(&r[6]).unwrap_or(Value::Null);
    let mut s = format!(
        "## Job {id}\n{} on queue {}: {} (attempt {} of {}, priority {})\nInserted {}, scheduled {}, attempted {}, completed {}\n\
         Arguments (names and types only): {}\n",
        r[0],
        r[1],
        r[2],
        r[3],
        r[4],
        r[5],
        short(&r[8]),
        short(&r[9]),
        short(&r[10]),
        short(&r[11]),
        arg_shapes(&args)
    );
    let errors: Value = serde_json::from_str(&r[7]).unwrap_or(Value::Null);
    let list = errors.as_array().cloned().unwrap_or_default();
    if !list.is_empty() {
        let _ = writeln!(s, "\n### Errors ({} in all, latest last)", list.len());
        for e in list.iter().rev().take(3).rev() {
            let _ = writeln!(
                s,
                "- attempt {} at {}: {}",
                e.get("attempt").and_then(Value::as_u64).unwrap_or(0),
                e.get("at")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .get(..19)
                    .unwrap_or("?"),
                clean(e.get("error").and_then(Value::as_str).unwrap_or(""), 600)
            );
        }
    }
    Ok(s)
}

/// APP: the app as `/dev/introspect.json` describes it: name, versions, mode, migrations, CSS,
/// and each sample's section, scalars only (strings redacted). Never `config`, URLs or assets.
pub(super) async fn app_snapshot(state: &AppState) -> String {
    let Ok(v) =
        crate::kernel::introspect::snapshot(state, crate::controllers::dev::FRONTEND_MODE, false)
            .await
    else {
        return "(the app snapshot is unavailable)\n".into();
    };
    let mut s = String::from("## The app (from /dev/introspect.json, scalars only)\n");
    let scalars = |v: &Value| -> String {
        v.as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, x)| {
                        let val = match x {
                            Value::String(t) => clean(t, 80),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => return None,
                        };
                        Some(format!("{k} {val}"))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    for key in ["app", "migrations", "css"] {
        let _ = writeln!(s, "- {key}: {}", scalars(&v[key]));
    }
    let _ = writeln!(
        s,
        "- routes: {}",
        v["routes"].as_array().map_or(0, Vec::len)
    );
    if let Some(o) = v.as_object() {
        for (k, x) in o {
            if [
                "app",
                "migrations",
                "css",
                "routes",
                "config",
                "url",
                "assets",
                "live",
            ]
            .contains(&k.as_str())
                || !x.is_object()
            {
                continue;
            }
            let _ = writeln!(s, "- {k}: {}", scalars(x));
        }
    }
    s
}

/// APP: `/dev`: the app, the kernel tools installed, the latest numbers.
pub(super) async fn dev_home(state: &AppState) -> String {
    let mut s = app_snapshot(state).await;
    let tabs: Vec<&str> = crate::kernel::ui::TABS.iter().map(|t| t.label).collect();
    let _ = writeln!(s, "\nKernel tools in the header: {}\n", tabs.join(", "));
    s.push_str(&overview());
    s
}

/// APP: `/dev/sockets`: the realtime section of the app snapshot, plus the realtime sample's
/// source when it is installed (topics, connections, rates; never people's names or frames).
pub(super) async fn sockets(state: &AppState) -> String {
    let mut s = app_snapshot(state).await;
    let extra = source_text(state, "sockets").await;
    if extra.is_empty() {
        s.push_str("\n(no realtime source registered: install the realtime sample)\n");
    } else {
        s.push('\n');
        s.push_str(&extra);
    }
    s
}
