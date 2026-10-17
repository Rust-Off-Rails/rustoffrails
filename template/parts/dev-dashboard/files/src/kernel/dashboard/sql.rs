//! SQL: every statement SeaORM runs, through its metric callback (verified in sea-orm 2.0.4:
//! `DatabaseConnection::set_metric_callback`, `metric::Info { elapsed, statement, failed }`),
//! grouped by a fingerprint with the literals stripped. Count, total, p95, max, last seen and
//! redacted parameters per fingerprint; the N+1 list; EXPLAIN for SELECTs in development.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value, Values};

use super::hist::Hist;
use super::http;
use super::trace::{SqlRec, Trace};
use super::{HUB, lock};

/// Distinct fingerprints kept; the least recently seen goes first.
pub const MAX_FINGERPRINTS: usize = 500; // APP: more statements, more memory (~1.5 KB each)
/// N+1 findings kept (one per route and fingerprint).
pub const MAX_N_PLUS_ONE: usize = 100;
/// A fingerprint longer than this is cut (a giant generated IN list).
const MAX_SQL_CHARS: usize = 2_000;

pub struct FpStats {
    pub id: u64,
    pub sql: Arc<str>,
    pub count: u64,
    pub failed: u64,
    pub hist: Hist,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    /// The routes it ran under (at most 5).
    pub routes: Vec<&'static str>,
    /// Redacted descriptions of the last parameters ("text (9 chars)", "int").
    pub params: Vec<String>,
    /// Development only, never rendered: the last statement with its values, for EXPLAIN.
    sample: Option<Statement>,
}

pub struct NPlusOne {
    pub route: &'static str,
    pub fp: u64,
    pub sql: Arc<str>,
    /// Statements of that fingerprint in the worst request.
    pub count: u32,
    /// How many requests did it.
    pub times: u64,
    pub trace: u64,
    pub last: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Total,
    P95,
    Max,
    Count,
    Recent,
}

impl Sort {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("p95") => Sort::P95,
            Some("max") => Sort::Max,
            Some("count") => Sort::Count,
            Some("recent") => Sort::Recent,
            _ => Sort::Total,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Sort::Total => "total",
            Sort::P95 => "p95",
            Sort::Max => "max",
            Sort::Count => "count",
            Sort::Recent => "recent",
        }
    }
}

/// A fingerprint as the SQL page lists it.
pub struct SqlRow {
    pub id: String,
    pub sql: Arc<str>,
    pub count: u64,
    pub failed: u64,
    pub total: u64,
    pub mean: u64,
    pub p95: u64,
    pub max: u64,
    pub last: DateTime<Utc>,
    pub routes: Vec<&'static str>,
    pub params: Vec<String>,
    pub select: bool,
}

#[derive(Default)]
pub struct SqlStats {
    map: HashMap<u64, FpStats>,
    n_plus_one: VecDeque<NPlusOne>,
}

impl SqlStats {
    fn row(s: &FpStats) -> SqlRow {
        SqlRow {
            id: hex(s.id),
            sql: s.sql.clone(),
            count: s.count,
            failed: s.failed,
            total: s.hist.sum(),
            mean: s.hist.mean(),
            p95: s.hist.quantile(0.95),
            max: s.hist.max(),
            last: s.last,
            routes: s.routes.clone(),
            params: s.params.clone(),
            select: is_select(&s.sql),
        }
    }

    pub fn rows(&self, sort: Sort) -> Vec<SqlRow> {
        let mut rows: Vec<SqlRow> = self.map.values().map(Self::row).collect();
        match sort {
            Sort::Total => rows.sort_by_key(|x| std::cmp::Reverse(x.total)),
            Sort::P95 => rows.sort_by_key(|x| std::cmp::Reverse(x.p95)),
            Sort::Max => rows.sort_by_key(|x| std::cmp::Reverse(x.max)),
            Sort::Count => rows.sort_by_key(|x| std::cmp::Reverse(x.count)),
            Sort::Recent => rows.sort_by_key(|x| std::cmp::Reverse(x.last)),
        }
        rows
    }

    pub fn get(&self, id: u64) -> Option<SqlRow> {
        self.map.get(&id).map(Self::row)
    }

    pub fn n_plus_one(&self) -> Vec<&NPlusOne> {
        let mut v: Vec<&NPlusOne> = self.n_plus_one.iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.last));
        v
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn n_plus_one_len(&self) -> usize {
        self.n_plus_one.len()
    }

    fn sample(&self, id: u64) -> Option<Statement> {
        self.map.get(&id).and_then(|s| s.sample.clone())
    }

    pub fn flag_n_plus_one(&mut self, t: &Trace) {
        for (fp, count, sql) in &t.n_plus_one {
            if let Some(n) = self
                .n_plus_one
                .iter_mut()
                .find(|n| n.fp == *fp && n.route == t.route)
            {
                n.times += 1;
                n.last = t.at;
                if *count >= n.count {
                    n.count = *count;
                    n.trace = t.id;
                }
                continue;
            }
            // INVARIANT: bounded (test: the_dashboard_stays_within_its_memory_bound).
            if self.n_plus_one.len() == MAX_N_PLUS_ONE {
                self.n_plus_one.pop_front();
            }
            self.n_plus_one.push_back(NPlusOne {
                route: t.route,
                fp: *fp,
                sql: sql.clone(),
                count: *count,
                times: 1,
                trace: t.id,
                last: t.at,
            });
        }
    }

    fn record(
        &mut self,
        id: u64,
        sql: &Arc<str>,
        micros: u64,
        failed: bool,
        route: &'static str,
        statement: Option<&Statement>,
    ) {
        let now = Utc::now();
        if !self.map.contains_key(&id) && self.map.len() >= MAX_FINGERPRINTS {
            // INVARIANT: bounded; the least recently seen fingerprint goes (test: the_dashboard_stays_within_its_memory_bound).
            if let Some(oldest) = self.map.values().min_by_key(|s| s.last).map(|s| s.id) {
                self.map.remove(&oldest);
            }
        }
        let s = self.map.entry(id).or_insert_with(|| FpStats {
            id,
            sql: sql.clone(),
            count: 0,
            failed: 0,
            hist: Hist::default(),
            first: now,
            last: now,
            routes: Vec::new(),
            params: Vec::new(),
            sample: None,
        });
        s.count += 1;
        if failed {
            s.failed += 1;
        }
        s.hist.record(micros);
        s.last = now;
        if s.routes.len() < 5 && !s.routes.contains(&route) {
            s.routes.push(route);
        }
        if let Some(stmt) = statement {
            if let Some(values) = &stmt.values {
                s.params = redact(values);
            }
            if HUB.keeps_values() && is_select(&stmt.sql) {
                s.sample = Some(stmt.clone());
            }
        }
    }

    pub fn approx_bytes(&self) -> usize {
        self.map
            .values()
            .map(|s| {
                std::mem::size_of::<FpStats>()
                    + Hist::BYTES
                    + s.sql.len()
                    + s.params.iter().map(String::len).sum::<usize>()
                    + s.sample.as_ref().map_or(0, |st| st.sql.len() * 2)
            })
            .sum::<usize>()
            + self.n_plus_one.len() * std::mem::size_of::<NPlusOne>()
    }
}

pub fn hex(id: u64) -> String {
    format!("{id:016x}")
}

pub fn parse_hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

/// The callback SeaORM calls after every statement (`instrument` installs it).
pub fn record(info: &sea_orm::metric::Info<'_>) {
    if !HUB.enabled() || http::quiet() {
        return;
    }
    let normalized = fingerprint(&info.statement.sql);
    let id = fingerprint_id(&normalized);
    let sql: Arc<str> = Arc::from(normalized);
    let micros = info.elapsed.as_micros() as u64;
    let req = http::current();
    let route = req.as_ref().map_or("(background)", |r| r.route);
    {
        let mut t = lock(&HUB.tick);
        t.statements += 1;
        t.sql_latency.record(micros);
    }
    lock(&HUB.sql).record(id, &sql, micros, info.failed, route, Some(info.statement));
    if let Some(req) = &req
        && matches!(req.method, "GET" | "HEAD")
        && is_write(&sql)
    {
        lock(&HUB.http).get_write(req.route, &sql, req.id);
    }
    if let Some(req) = req {
        let end = req.offset_us(Instant::now());
        req.push_sql(SqlRec {
            fp: id,
            sql,
            start_us: end.saturating_sub(micros),
            dur_us: micros,
            parent: tracing::Span::current().id().map(|i| i.into_u64()),
            failed: info.failed,
        });
    }
}

pub fn fingerprint_id(normalized: &str) -> u64 {
    let mut h = DefaultHasher::new();
    normalized.hash(&mut h);
    h.finish()
}

/// The statement with every literal replaced by `?`: quoted strings, numbers, `$1` placeholders;
/// whitespace collapsed; lists of placeholders collapsed (`IN (?, ?, ?)` → `IN (?)`), so the same
/// query with other values has the same fingerprint.
pub fn fingerprint(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len().min(MAX_SQL_CHARS));
    let mut chars = sql.chars().peekable();
    let mut prev_word = false;
    while let Some(c) = chars.next() {
        if out.len() >= MAX_SQL_CHARS {
            out.push('…');
            break;
        }
        match c {
            '\'' => {
                // A string literal; '' is an escaped quote.
                while let Some(n) = chars.next() {
                    if n == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                out.push('?');
                prev_word = false;
            }
            '$' if chars.peek().is_some_and(char::is_ascii_digit) => {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
                out.push('?');
                prev_word = false;
            }
            c if c.is_ascii_digit() && !prev_word => {
                while chars
                    .peek()
                    .is_some_and(|n| n.is_ascii_digit() || *n == '.')
                {
                    chars.next();
                }
                out.push('?');
                prev_word = false;
            }
            c if c.is_whitespace() => {
                while chars.peek().is_some_and(|n| n.is_whitespace()) {
                    chars.next();
                }
                if !out.is_empty() {
                    out.push(' ');
                }
                prev_word = false;
            }
            c => {
                out.push(c);
                prev_word = c.is_alphanumeric() || c == '_' || c == '"';
            }
        }
    }
    let mut s = out.trim().to_owned();
    for (from, to) in [("?, ?", "?"), ("(?), (?)", "(?)")] {
        while s.contains(from) {
            s = s.replace(from, to);
        }
    }
    s
}

/// Parameters as type and size, never the value.
// SECURITY: parameters carry emails, password digests, tokens; the dashboard shows their type
// and length only (test: sql_parameters_are_never_shown).
pub fn redact(values: &Values) -> Vec<String> {
    values
        .0
        .iter()
        .take(20)
        .map(|v| match v {
            Value::String(Some(s)) => format!("text ({} chars)", s.chars().count()),
            Value::Bytes(Some(b)) => format!("bytes ({})", b.len()),
            other => {
                let (name, null) = kind(other);
                if null {
                    format!("{name} (null)")
                } else {
                    name.to_owned()
                }
            }
        })
        .collect()
}

/// A value's type, and whether it is NULL; never the value (and no `Debug`, which would format a
/// whole JSON document per statement).
fn kind(v: &Value) -> (&'static str, bool) {
    match v {
        Value::Bool(x) => ("bool", x.is_none()),
        Value::TinyInt(x) => ("int", x.is_none()),
        Value::SmallInt(x) => ("int", x.is_none()),
        Value::Int(x) => ("int", x.is_none()),
        Value::BigInt(x) => ("bigint", x.is_none()),
        Value::TinyUnsigned(x) => ("int", x.is_none()),
        Value::SmallUnsigned(x) => ("int", x.is_none()),
        Value::Unsigned(x) => ("int", x.is_none()),
        Value::BigUnsigned(x) => ("bigint", x.is_none()),
        Value::Float(x) => ("float", x.is_none()),
        Value::Double(x) => ("float", x.is_none()),
        Value::String(x) => ("text", x.is_none()),
        Value::Char(x) => ("char", x.is_none()),
        Value::Bytes(x) => ("bytes", x.is_none()),
        Value::Json(x) => ("json", x.is_none()),
        Value::ChronoDate(x) => ("date", x.is_none()),
        Value::ChronoTime(x) => ("time", x.is_none()),
        Value::ChronoDateTime(x) => ("timestamp", x.is_none()),
        Value::ChronoDateTimeUtc(x) => ("timestamp", x.is_none()),
        Value::ChronoDateTimeLocal(x) => ("timestamp", x.is_none()),
        Value::ChronoDateTimeWithTimeZone(x) => ("timestamp", x.is_none()),
        Value::Uuid(x) => ("uuid", x.is_none()),
        _ => ("value", false),
    }
}

/// An INSERT, UPDATE or DELETE that isn't in `http::GET_WRITES_ALLOWED`.
pub fn is_write(sql: &str) -> bool {
    let head: String = sql
        .trim_start()
        .chars()
        .take(7)
        .collect::<String>()
        .to_ascii_uppercase();
    (head.starts_with("INSERT") || head.starts_with("UPDATE") || head.starts_with("DELETE"))
        && !http::GET_WRITES_ALLOWED.iter().any(|a| sql.starts_with(a))
}

/// Only plain SELECTs may be explained.
pub fn is_select(sql: &str) -> bool {
    let s = sql.trim_start().trim_start_matches('(').trim_start();
    let head: String = s.chars().take(7).collect::<String>().to_ascii_lowercase();
    let body = sql.trim().trim_end_matches(';');
    head.starts_with("select") && !head.starts_with("selectx") && !body.contains(';')
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExplainError {
    /// Unknown fingerprint, or one evicted.
    Gone,
    /// Not a SELECT, or no parameters kept (production).
    NotExplainable,
    Failed(String),
}

/// EXPLAIN (Postgres) or EXPLAIN QUERY PLAN (SQLite) of a fingerprint's last statement, with its
/// last parameters. Development only; the plan is computed, the statement is not run.
pub async fn explain(db: &DatabaseConnection, id: u64) -> Result<Vec<String>, ExplainError> {
    let stmt = {
        let stats = lock(&HUB.sql);
        if stats.get(id).is_none() {
            return Err(ExplainError::Gone);
        }
        stats.sample(id).ok_or(ExplainError::NotExplainable)?
    };
    // SECURITY: SELECT only, one statement, never EXPLAIN ANALYZE, so nothing runs or writes
    // (test: explain_runs_for_selects_only).
    if !is_select(&stmt.sql) {
        return Err(ExplainError::NotExplainable);
    }
    let backend = db.get_database_backend();
    let prefix = match backend {
        DbBackend::Sqlite => "EXPLAIN QUERY PLAN ",
        _ => "EXPLAIN ",
    };
    let explain = Statement {
        sql: format!("{prefix}{}", stmt.sql),
        values: stmt.values.clone(),
        db_backend: backend,
    };
    let rows = http::quietly(db.query_all_raw(explain))
        .await
        .map_err(|e| ExplainError::Failed(e.to_string()))?;
    let mut lines = Vec::new();
    match backend {
        DbBackend::Sqlite => {
            // (id, parent, notused, detail): indent each step under its parent.
            let mut depth: HashMap<i64, usize> = HashMap::new();
            for row in rows {
                let id: i64 = row.try_get("", "id").unwrap_or(0);
                let parent: i64 = row.try_get("", "parent").unwrap_or(0);
                let detail: String = row.try_get("", "detail").unwrap_or_default();
                let d = depth.get(&parent).map_or(0, |d| d + 1);
                depth.insert(id, d);
                lines.push(format!("{}{detail}", "  ".repeat(d)));
            }
        }
        _ => {
            for row in rows {
                let line: String = row.try_get("", "QUERY PLAN").unwrap_or_default();
                lines.push(line);
            }
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_strip_literals_and_collapse_lists() {
        assert_eq!(
            fingerprint("SELECT * FROM \"users\"  WHERE \"id\" = $1 AND name = 'O''Brien'"),
            "SELECT * FROM \"users\" WHERE \"id\" = ? AND name = ?"
        );
        assert_eq!(
            fingerprint("select a from t where id in (1, 2, 3) limit 20"),
            "select a from t where id in (?) limit ?"
        );
        assert_eq!(
            fingerprint("INSERT INTO t (a, b) VALUES (?, ?), (?, ?)"),
            "INSERT INTO t (a, b) VALUES (?)"
        );
        assert_eq!(
            fingerprint("SELECT col1, t2.x FROM t2"),
            "SELECT col1, t2.x FROM t2"
        );
        assert_eq!(
            fingerprint_id(&fingerprint("SELECT 1 WHERE x = 5")),
            fingerprint_id(&fingerprint("SELECT 1 WHERE x = 77"))
        );
    }

    #[test]
    fn only_single_selects_are_explainable() {
        assert!(is_select("SELECT 1"));
        assert!(is_select("  (SELECT a FROM t) UNION (SELECT b FROM u)"));
        assert!(!is_select("DELETE FROM t"));
        assert!(!is_select("SELECT 1; DROP TABLE t"));
        assert!(!is_select(
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x"
        ));
        assert!(is_select("SELECT 1;"));
    }

    #[test]
    fn sql_parameters_are_never_shown() {
        let values = Values(vec![
            Value::String(Some("ada@example.com".into())),
            Value::BigInt(Some(42)),
            Value::Bool(None),
        ]);
        let shown = redact(&values).join(" ");
        assert!(!shown.contains("ada") && !shown.contains("42"), "{shown}");
        assert!(shown.contains("text (15 chars)"), "{shown}");
        assert!(
            shown.contains("bigint") && shown.contains("bool (null)"),
            "{shown}"
        );
    }
}
