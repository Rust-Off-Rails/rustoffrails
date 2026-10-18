//! The waterfall: one trace as a collapsible tree on a shared time axis, the same component
//! everywhere a trace shows (the trace page, the explorer's rows, a route's drawer, a SQL
//! fingerprint's runs, N+1 findings, a job's page, the mail that a request sent, AI citations).
//!
//! The tree: the request (or job) at the root; spans nested by parent; each statement under the
//! span that ran it; log events as markers on their span's track. Then three passes make it
//! readable:
//!
//! - **Repeats fold.** Siblings with the same SQL fingerprint or the same method (`GROUP_MIN` or
//!   more) become one row, `SELECT lab_order_items ×20 · 640µs total`, with a mini-bar per
//!   occurrence on its track, expandable to every occurrence. An N+1 is one row, not two screens.
//! - **Framework noise steps aside.** What runs before the handler outside any span of the app's
//!   (the pending-migrations guard's checks) folds into one collapsed `framework` row.
//! - **Time is accounted.** Each row's self time (its duration minus its children's), the
//!   uninstrumented gaps inside a span (hatched: where to add a span), the critical path (the
//!   chain of rows the request actually waited on), and time by kind for the header's stacked bar.
//!
//! WHY server-side: the same tree renders without JavaScript (every row, `?all=1`), in tests, and
//! for the AI; `dashboard-trace.js` only folds rows, shows the side panel and moves the keyboard.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::trace::Trace;
use super::web::micros;

/// Siblings repeated this many times or more fold into one row. APP.
pub const GROUP_MIN: usize = 3;
/// Rows a waterfall renders at most (the rest are counted). A loop of 2,000 method spans folds
/// into one row anyway; this caps what a page sends.
pub const MAX_ROWS: usize = 1_500;

/// What a row is, for its color and the legend.
pub const KINDS: [(&str, &str); 11] = [
    ("request", "request"),
    ("wait", "waiting (queue)"),
    ("handler", "handler"),
    ("method", "method"),
    ("sql", "SQL"),
    ("template", "template"),
    ("job", "job / mail"),
    ("http", "outbound HTTP"),
    ("middleware", "middleware"),
    ("framework", "framework"),
    ("event", "log event"),
];

/// A span's kind from its name, module and fields: the conventions in README "Instrumenting your
/// own methods".
pub fn span_kind(name: &str, target: &str, fields: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower == "handler" || lower.starts_with("handler") {
        "handler"
    } else if lower == "render" || lower.starts_with("render") || lower == "template" {
        "template"
    } else if target.starts_with("reqwest")
        || target.starts_with("hyper")
        || lower == "http"
        || lower.starts_with("http_")
        || lower == "outbound"
        || lower == "upstream_call"
        || fields.contains("http.url")
    {
        "http"
    } else if super::trace::background_kind(name).is_some() || lower.ends_with("_job") {
        "job"
    } else if target.starts_with("tower")
        || target.starts_with("axum")
        || lower == "middleware"
        || target.starts_with("sea_orm")
        || target.starts_with("sqlx")
    {
        "middleware"
    } else {
        "method"
    }
}

/// A node of the tree.
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: &'static str,
    /// One line: `load_orders`, `SELECT lab_order_items WHERE order_id = ?`, `framework`.
    pub label: String,
    /// The module (`offrails_host::kernel::dashboard::lab`) or, for SQL, nothing.
    pub module: String,
    /// `src/kernel/dashboard/lab.rs:291`.
    pub loc: String,
    /// The full text: the normalized SQL, or the span's fields.
    pub full: String,
    pub start_us: u64,
    pub dur_us: u64,
    /// Duration minus the union of its children's.
    pub self_us: u64,
    /// Sum of its occurrences' durations (a folded group; equals `dur_us` otherwise).
    pub total_us: u64,
    pub count: u32,
    pub fp: Option<u64>,
    /// Part of an N+1.
    pub flagged: bool,
    pub failed: bool,
    pub critical: bool,
    pub group: bool,
    /// Occurrences on the track: (start, duration).
    pub occ: Vec<(u64, u64)>,
    /// Uninstrumented gaps inside it: (start, duration).
    pub gaps: Vec<(u64, u64)>,
    /// Log events on its track: (at, level, message).
    pub events: Vec<(u64, &'static str, String)>,
    pub children: Vec<Node>,
    /// Outbound HTTP: the API's host (`api.example.com`), shown as a chip.
    pub host: String,
    /// Outbound HTTP: the call's short name in notes (`payments`); the label otherwise.
    pub short: String,
    /// Outbound HTTP: the call (or an attempt) timed out.
    pub timeout: bool,
    /// Outbound HTTP: one try of a call that retries (`http_attempt`), not a call of its own.
    pub attempt: bool,
    span_id: Option<u64>,
}

impl Node {
    fn leaf(kind: &'static str, label: String, start_us: u64, dur_us: u64) -> Self {
        Self {
            kind,
            label,
            module: String::new(),
            loc: String::new(),
            full: String::new(),
            start_us,
            dur_us,
            self_us: dur_us,
            total_us: dur_us,
            count: 1,
            fp: None,
            flagged: false,
            failed: false,
            critical: false,
            group: false,
            occ: vec![(start_us, dur_us)],
            gaps: Vec::new(),
            events: Vec::new(),
            children: Vec::new(),
            host: String::new(),
            short: String::new(),
            timeout: false,
            attempt: false,
            span_id: None,
        }
    }

    fn end(&self) -> u64 {
        self.start_us + self.dur_us
    }

    /// What folds with what: a fingerprint, or a method's name and module.
    fn key(&self) -> Option<String> {
        match (self.kind, self.fp) {
            (_, Some(fp)) => Some(format!("sql:{fp}")),
            ("event" | "framework" | "wait", _) => None,
            _ if self.group => None,
            _ => Some(format!("span:{}:{}", self.label, self.module)),
        }
    }
}

/// The tree of a trace. `details: false` (the production view) leaves out span fields and log
/// events, as the trace page always has.
pub fn tree(t: &Trace, details: bool) -> Node {
    let mut root = Node::leaf(
        if t.kind == "request" {
            "request"
        } else {
            "job"
        },
        format!("{} {}", t.method, t.route),
        0,
        t.dur_us,
    );
    root.full = format!("{} → {}", t.path, t.status);
    root.failed = t.status >= 500;
    let flagged: Vec<u64> = t.n_plus_one.iter().map(|(fp, _, _)| *fp).collect();
    // Spans, in start order, attached to their parents (a parent always starts first).
    let mut spans: Vec<&super::trace::SpanRec> = t.spans.iter().collect();
    spans.sort_by_key(|s| (s.start_us, std::cmp::Reverse(s.dur_us)));
    let mut nodes: Vec<Node> = Vec::with_capacity(spans.len());
    let mut index: HashMap<u64, usize> = HashMap::new();
    for s in &spans {
        let kind = span_kind(s.name, s.target, &s.fields);
        let mut n = Node::leaf(kind, s.name.to_owned(), s.start_us, s.dur_us);
        n.module = s.target.to_owned();
        n.loc = match (s.file, s.line) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.to_owned(),
            _ => String::new(),
        };
        if details {
            n.full = s.fields.clone();
            // A handler span names its handler (`handler=lab::orders`): show that.
            if kind == "handler"
                && let Some(h) = field(&s.fields, "handler")
            {
                n.label = format!("handler {h}");
            }
        }
        if kind == "http" {
            outbound_label(&mut n, s.name, &s.fields);
        }
        n.span_id = Some(s.id);
        index.insert(s.id, nodes.len());
        nodes.push(n);
    }
    // Statements and events, keyed to their span (or the root).
    let mut sql_of: HashMap<Option<usize>, Vec<Node>> = HashMap::new();
    for q in &t.sql {
        let mut n = Node::leaf("sql", sql_summary(&q.sql), q.start_us, q.dur_us);
        n.full = q.sql.to_string();
        n.fp = Some(q.fp);
        n.flagged = flagged.contains(&q.fp);
        n.failed = q.failed;
        let at = q.parent.and_then(|p| index.get(&p).copied());
        sql_of.entry(at).or_default().push(n);
    }
    let mut events_of: HashMap<Option<usize>, Vec<(u64, &'static str, String)>> = HashMap::new();
    if details {
        for e in &t.events {
            let at = e.parent.and_then(|p| index.get(&p).copied());
            events_of
                .entry(at)
                .or_default()
                .push((e.at_us, e.level, e.message.clone()));
        }
    }
    // Build bottom-up: children before parents (reverse start order works: a child starts after
    // its parent).
    let parents: Vec<Option<usize>> = spans
        .iter()
        .map(|s| s.parent.and_then(|p| index.get(&p).copied()))
        .collect();
    let mut kids: Vec<Vec<Node>> = vec![Vec::new(); nodes.len()];
    let mut root_kids: Vec<Node> = Vec::new();
    for i in (0..nodes.len()).rev() {
        let mut n = nodes[i].clone();
        let mut ch = std::mem::take(&mut kids[i]);
        ch.extend(sql_of.remove(&Some(i)).unwrap_or_default());
        n.events = events_of.remove(&Some(i)).unwrap_or_default();
        n.children = ch;
        match parents[i] {
            Some(p) if p < i => kids[p].push(n),
            _ => root_kids.push(n),
        }
    }
    root_kids.extend(sql_of.remove(&None).unwrap_or_default());
    root.events = events_of.remove(&None).unwrap_or_default();
    root.children = root_kids;
    sort_rec(&mut root);
    fold_framework(&mut root);
    if t.kind == "request" {
        mark_waiting(&mut root);
    }
    fold_repeats(&mut root);
    account(&mut root);
    mark_critical(&mut root);
    root
}

fn field<'a>(fields: &'a str, name: &str) -> Option<&'a str> {
    fields.split(' ').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.trim_matches('"'))
    })
}

/// An outbound call's row, from the outbound wrapper's span fields (`kernel::outbound`):
/// `POST payments /charge → 200`, its host as a chip, attempts as `attempt 2 → 503`, a timeout in
/// red. Shown in the production view too: service, method, host and route template are no
/// personal data (span fields stay hidden there).
fn outbound_label(n: &mut Node, name: &str, fields: &str) {
    let status = field(fields, "http.status");
    let outcome = field(fields, "outcome");
    let result = match (status, outcome) {
        (Some(s), _) => s.to_owned(),
        (None, Some("timeout")) => "timeout".to_owned(),
        (None, Some(o)) => o.replace('_', " "),
        (None, None) => String::new(),
    };
    n.timeout = outcome == Some("timeout");
    n.failed = n.timeout
        || matches!(
            outcome,
            Some("server_error" | "connect" | "failed" | "too_large")
        )
        || status
            .and_then(|s| s.parse::<u16>().ok())
            .is_some_and(|s| s >= 500);
    let arrow = if result.is_empty() {
        String::new()
    } else {
        format!(" → {result}")
    };
    if name == "http_attempt" {
        n.label = format!("attempt {}{arrow}", field(fields, "attempt").unwrap_or("?"));
        n.short = "attempt".to_owned();
        n.attempt = true;
        return;
    }
    let service = field(fields, "service");
    match (
        service,
        field(fields, "http.method"),
        field(fields, "http.route"),
    ) {
        (Some(svc), Some(m), Some(r)) => {
            let attempts = field(fields, "attempts")
                .and_then(|a| a.parse::<u32>().ok())
                .filter(|a| *a > 1)
                .map(|a| format!(" · {a} attempts"))
                .unwrap_or_default();
            n.label = crate::kernel::ui::truncate(&format!("{m} {svc} {r}{arrow}{attempts}"), 90);
            n.short = svc.to_owned();
            n.host = field(fields, "http.host").unwrap_or_default().to_owned();
        }
        _ => {
            n.short = service.unwrap_or(name).to_owned();
        }
    }
}

fn sort_rec(n: &mut Node) {
    n.children
        .sort_by_key(|c| (c.start_us, std::cmp::Reverse(c.dur_us)));
    for c in &mut n.children {
        sort_rec(c);
    }
}

/// SQL a framework runs, not the app: the pending-migrations guard, catalog checks.
fn framework_sql(sql: &str) -> bool {
    let s = sql.to_ascii_lowercase();
    s.contains("seaql_migrations")
        || s.contains("sqlite_master")
        || s.contains("pg_catalog")
        || s.contains("information_schema")
}

/// Root-level statements and middleware spans that finish before the handler starts (or, without
/// a handler span, catalog checks) fold into one `framework` row.
fn fold_framework(root: &mut Node) {
    let handler_start = root
        .children
        .iter()
        .filter(|c| matches!(c.kind, "handler" | "method" | "template" | "job"))
        .map(|c| c.start_us)
        .min();
    let is_noise = |c: &Node| match handler_start {
        Some(h) => {
            (c.kind == "sql" || c.kind == "middleware") && c.end() <= h
                || c.fp.is_some() && framework_sql(&c.full)
        }
        None => c.fp.is_some() && framework_sql(&c.full),
    };
    let (noise, keep): (Vec<Node>, Vec<Node>) = std::mem::take(&mut root.children)
        .into_iter()
        .partition(is_noise);
    root.children = keep;
    if noise.is_empty() {
        return;
    }
    let start = noise.iter().map(|n| n.start_us).min().unwrap_or(0);
    let end = noise.iter().map(Node::end).max().unwrap_or(start);
    let total: u64 = noise.iter().map(|n| n.dur_us).sum();
    let queries = noise.iter().filter(|n| n.kind == "sql").count();
    let mut g = Node::leaf(
        "framework",
        format!(
            "framework · {} before the handler",
            plural(queries, "query", "queries")
        ),
        start,
        end - start,
    );
    g.module = "kernel middleware (the app's own checks)".to_owned();
    g.full = "Statements the framework runs on every request before your handler: in development, \
              the pending-migrations guard. Not your code."
        .to_owned();
    g.group = true;
    g.total_us = total;
    g.count = noise.len() as u32;
    g.occ = noise.iter().map(|n| (n.start_us, n.dur_us)).collect();
    g.children = noise;
    root.children.insert(0, g);
    sort_rec(root);
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A stretch of waiting shorter than this (or than 2% of the request) isn't drawn: scheduling
/// noise. APP.
pub const WAIT_ROW_US: u64 = 5_000;

/// The time a request spent before any of the app's code ran, outside the framework's own
/// checks: from its arrival at the outermost layer (the dashboard's middleware) to the first
/// handler, method, statement or call. Under load that is waiting for a worker, a semaphore or a
/// connection, not missing instrumentation, so it gets its own kind (`wait`), its own color and its
/// own share in the time-by-kind bar.
fn mark_waiting(root: &mut Node) {
    let Some(first) = root
        .children
        .iter()
        .filter(|c| c.kind != "framework")
        .map(|c| c.start_us)
        .min()
    else {
        return;
    };
    let mut busy: Vec<(u64, u64)> = root
        .children
        .iter()
        .filter(|c| c.kind == "framework")
        .flat_map(|c| c.occ.iter().map(|(s, d)| (*s, s + d)))
        .collect();
    busy.sort_unstable();
    let mut cursor = 0;
    let mut gaps = Vec::new();
    for (s, e) in busy {
        if s >= first {
            break;
        }
        if s > cursor {
            gaps.push((cursor, s - cursor));
        }
        cursor = cursor.max(e);
    }
    if first > cursor {
        gaps.push((cursor, first - cursor));
    }
    let floor = WAIT_ROW_US.max(root.dur_us / 50);
    for (s, d) in gaps.into_iter().filter(|(_, d)| *d >= floor) {
        let mut n = Node::leaf("wait", "waiting to run".to_owned(), s, d);
        n.module = "before the handler: a free worker, a permit, a connection".to_owned();
        n.full = "The request had arrived (the outermost layer saw it) but none of the app's code                   ran yet. Under load this is queueing: every Tokio worker busy, a concurrency                   limit, or a layer waiting for a database connection. Compare with Tokio busy                   and the pool on Charts."
            .to_owned();
        root.children.push(n);
    }
    sort_rec(root);
}

/// How long a request waited before its code ran (the `wait` rows of its tree).
pub fn waited_us(root: &Node) -> u64 {
    root.children
        .iter()
        .filter(|c| c.kind == "wait")
        .map(|c| c.total_us)
        .sum()
}

/// Siblings with the same key, `GROUP_MIN` or more, become one group row (recursively).
fn fold_repeats(n: &mut Node) {
    for c in &mut n.children {
        fold_repeats(c);
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for c in &n.children {
        if let Some(k) = c.key() {
            *counts.entry(k).or_default() += 1;
        }
    }
    if counts.values().all(|v| *v < GROUP_MIN) {
        return;
    }
    let mut out: Vec<Node> = Vec::new();
    let mut groups: HashMap<String, usize> = HashMap::new();
    for c in std::mem::take(&mut n.children) {
        let k = c
            .key()
            .filter(|k| counts.get(k).copied().unwrap_or(0) >= GROUP_MIN);
        let Some(k) = k else {
            out.push(c);
            continue;
        };
        match groups.get(&k) {
            Some(&at) => out[at].children.push(c),
            None => {
                let mut g = c.clone();
                g.group = true;
                g.children = vec![c];
                groups.insert(k, out.len());
                out.push(g);
            }
        }
    }
    for at in groups.values() {
        let g = &mut out[*at];
        let start = g.children.iter().map(|c| c.start_us).min().unwrap_or(0);
        let end = g.children.iter().map(Node::end).max().unwrap_or(start);
        g.start_us = start;
        g.dur_us = end - start;
        g.total_us = g.children.iter().map(|c| c.dur_us).sum();
        g.count = g.children.len() as u32;
        g.occ = g.children.iter().map(|c| (c.start_us, c.dur_us)).collect();
        g.flagged = g.children.iter().any(subtree_flagged);
        g.failed = g.children.iter().any(|c| c.failed);
        g.events = Vec::new();
        g.gaps = Vec::new();
        g.span_id = None;
    }
    n.children = out;
}

fn subtree_flagged(n: &Node) -> bool {
    n.flagged || n.children.iter().any(subtree_flagged)
}

/// Self time and uninstrumented gaps, bottom-up.
fn account(n: &mut Node) {
    for c in &mut n.children {
        account(c);
    }
    if n.group {
        n.self_us = 0;
        return;
    }
    let mut iv: Vec<(u64, u64)> = n
        .children
        .iter()
        .flat_map(|c| {
            if c.group {
                c.occ.iter().map(|(s, d)| (*s, s + d)).collect::<Vec<_>>()
            } else {
                vec![(c.start_us, c.end())]
            }
        })
        .map(|(s, e)| (s.max(n.start_us), e.min(n.end())))
        .filter(|(s, e)| e > s)
        .collect();
    iv.sort_unstable();
    let mut covered = 0_u64;
    let mut cursor = n.start_us;
    let mut gaps = Vec::new();
    for (s, e) in iv {
        if s > cursor {
            gaps.push((cursor, s - cursor));
        }
        if e > cursor {
            covered += e - s.max(cursor);
            cursor = e;
        }
    }
    if n.end() > cursor && !n.children.is_empty() {
        gaps.push((cursor, n.end() - cursor));
    }
    n.self_us = n.dur_us.saturating_sub(covered);
    // Only a span with children has "unexplained" time: a leaf's self time is its own work.
    // Gaps under 2% of the span are noise (scheduling, the span's own bookkeeping).
    n.gaps = if n.children.is_empty() {
        Vec::new()
    } else {
        gaps.into_iter()
            .filter(|(_, d)| *d * 50 >= n.dur_us.max(1) && *d >= 50)
            .collect()
    };
}

/// The critical path: from the end, the child that finished last, then the one that finished
/// last before that one started, and so on, recursively.
fn mark_critical(n: &mut Node) {
    n.critical = true;
    let mut cursor = n.end();
    loop {
        let pick = n
            .children
            .iter()
            .enumerate()
            .filter(|(_, c)| c.end() <= cursor && !c.critical && c.kind != "framework")
            .max_by_key(|(_, c)| c.end())
            .map(|(i, _)| i);
        let Some(i) = pick else {
            break;
        };
        cursor = n.children[i].start_us;
        mark_critical(&mut n.children[i]);
        if cursor <= n.start_us {
            break;
        }
    }
}

/// Self time by kind across the tree (folded groups count their occurrences).
pub fn time_by_kind(root: &Node) -> Vec<(&'static str, u64)> {
    fn walk(n: &Node, acc: &mut HashMap<&'static str, u64>) {
        if n.group {
            for c in &n.children {
                walk(c, acc);
            }
            return;
        }
        let kind = if n.kind == "request" { "other" } else { n.kind };
        *acc.entry(kind).or_default() += n.self_us;
        for c in &n.children {
            walk(c, acc);
        }
    }
    let mut acc = HashMap::new();
    walk(root, &mut acc);
    let mut v: Vec<(&'static str, u64)> = acc.into_iter().filter(|(_, us)| *us > 0).collect();
    v.sort_by_key(|(_, us)| std::cmp::Reverse(*us));
    v
}

/// The slowest single row that isn't the request (its self time is what a fix would win).
pub fn slowest(root: &Node) -> Option<(String, u64)> {
    fn walk<'a>(n: &'a Node, best: &mut Option<&'a Node>) {
        for c in &n.children {
            if !c.group && c.kind != "framework" && best.is_none_or(|b| c.dur_us > b.dur_us) {
                *best = Some(c);
            }
            walk(c, best);
        }
    }
    let mut best = None;
    walk(root, &mut best);
    best.map(|b| (b.label.clone(), b.dur_us))
}

/// Whether other pages may embed a trace (development, the dashboard recording).
pub fn embeds_on() -> bool {
    super::HUB.enabled() && super::HUB.keeps_values()
}

/// The component's stylesheet and script for a kernel page that embeds a trace (the jobs
/// dashboard's job page, the mailbox), development only.
pub fn embed_assets(path_and_query: &str) -> Option<(String, String)> {
    if !embeds_on()
        || path_and_query.starts_with("/dev/dashboard")
        || !(path_and_query.starts_with("/admin/jobs")
            || path_and_query.starts_with("/dev/mailbox"))
    {
        return None;
    }
    Some((
        super::ai_web::asset("kernel/dashboard-trace.css"),
        super::ai_web::asset("kernel/dashboard-trace.js"),
    ))
}

// ---------------------------------------------------------------------------------------------
// SQL as one line, and highlighted
// ---------------------------------------------------------------------------------------------

/// `SELECT "lab_order_items"."id", … FROM "lab_order_items" WHERE "lab_order_items"."order_id" = ?`
/// → `SELECT lab_order_items WHERE order_id = ?`: the verb, the table, the condition's columns.
pub fn sql_summary(sql: &str) -> String {
    let unquote = |s: &str| s.replace(['"', '`'], "");
    let words: Vec<&str> = sql.split_whitespace().collect();
    let Some(verb) = words.first() else {
        return String::new();
    };
    let upper: Vec<String> = words.iter().map(|w| w.to_ascii_uppercase()).collect();
    // `SELECT COUNT(*) FROM (SELECT … FROM lab_orders …)` reads as `COUNT lab_orders`.
    let verb = if upper.first().is_some_and(|v| v == "SELECT")
        && upper.get(1).is_some_and(|w| w.starts_with("COUNT("))
    {
        "COUNT".to_owned()
    } else {
        verb.to_ascii_uppercase()
    };
    // The first `kw <name>` that names a table (not a subquery's parenthesis).
    let after = |kw: &str| {
        upper
            .iter()
            .enumerate()
            .filter(|(_, w)| *w == kw)
            .filter_map(|(i, _)| words.get(i + 1))
            .find(|t| !t.starts_with('('))
            .map(|t| {
                let t = unquote(t.trim_end_matches([',', ';', '(', ')']));
                t.rsplit('.').next().unwrap_or(&t).to_owned()
            })
    };
    let table = match verb.as_str() {
        "SELECT" | "DELETE" | "COUNT" => after("FROM"),
        "INSERT" => after("INTO"),
        "UPDATE" => words.get(1).map(|t| unquote(t)),
        "WITH" => Some("(cte)".to_owned()),
        _ => None,
    };
    let cond = upper.iter().position(|w| w == "WHERE").map(|i| {
        let end = upper[i + 1..]
            .iter()
            .position(|w| {
                matches!(
                    w.as_str(),
                    "ORDER" | "GROUP" | "LIMIT" | "RETURNING" | "HAVING"
                )
            })
            .map_or(words.len(), |p| i + 1 + p);
        let text = words[i + 1..end].join(" ");
        // `"t"."col"` → `col`.
        let mut out = String::new();
        for tok in text.split(' ') {
            if !out.is_empty() {
                out.push(' ');
            }
            let t = unquote(tok);
            out.push_str(match t.rsplit_once('.') {
                Some((_, col)) if !t.starts_with(char::is_numeric) => col,
                _ => &t,
            });
        }
        out
    });
    let mut s = verb;
    if let Some(t) = table {
        let _ = write!(s, " {t}");
    }
    if let Some(c) = cond {
        let _ = write!(s, " WHERE {c}");
    }
    crate::kernel::ui::truncate(&s, 90)
}

/// A token of highlighted SQL: (class, text). Classes: `kw`, `str`, `num`, `ph` (placeholder),
/// `id` (quoted identifier), `pun`, `` (plain), `nl` (a line break before a clause).
pub fn sql_tokens(sql: &str) -> Vec<(&'static str, String)> {
    const KW: &[&str] = &[
        "SELECT",
        "FROM",
        "WHERE",
        "AND",
        "OR",
        "NOT",
        "IN",
        "IS",
        "NULL",
        "AS",
        "ON",
        "JOIN",
        "LEFT",
        "RIGHT",
        "INNER",
        "OUTER",
        "ORDER",
        "BY",
        "GROUP",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "INSERT",
        "INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE",
        "RETURNING",
        "WITH",
        "RECURSIVE",
        "UNION",
        "ALL",
        "DISTINCT",
        "COUNT",
        "DESC",
        "ASC",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "EXISTS",
        "LIKE",
        "BETWEEN",
        "CONFLICT",
        "DO",
        "NOTHING",
    ];
    const BREAK: &[&str] = &[
        "FROM",
        "WHERE",
        "ORDER",
        "GROUP",
        "LIMIT",
        "HAVING",
        "JOIN",
        "LEFT",
        "VALUES",
        "SET",
        "RETURNING",
        "UNION",
    ];
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '`' || c == '\'' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                j += 1;
            }
            let tok: String = chars[i..(j + 1).min(chars.len())].iter().collect();
            out.push((if c == '\'' { "str" } else { "id" }, tok));
            i = j + 1;
        } else if c == '?' || (c == '$' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            out.push(("ph", chars[i..j].iter().collect()));
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
                j += 1;
            }
            out.push(("num", chars[i..j].iter().collect()));
            i = j;
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let w: String = chars[i..j].iter().collect();
            let up = w.to_ascii_uppercase();
            if BREAK.contains(&up.as_str()) && !out.is_empty() {
                out.push(("nl", String::new()));
            }
            out.push((if KW.contains(&up.as_str()) { "kw" } else { "" }, w));
            i = j;
        } else {
            let mut j = i;
            while j < chars.len()
                && !(chars[j].is_alphanumeric()
                    || matches!(chars[j], '"' | '`' | '\'' | '?' | '$' | '_'))
            {
                j += 1;
            }
            let tok: String = chars[i..j.max(i + 1)].iter().collect();
            i = j.max(i + 1);
            // A line break eats the space before it.
            if out.last().is_some_and(|(k, _)| *k == "nl") {
                let t = tok.trim_start().to_owned();
                if !t.is_empty() {
                    out.push(("pun", t));
                }
            } else {
                out.push(("pun", tok));
            }
        }
    }
    // Drop the space before a break.
    let mut cleaned: Vec<(&'static str, String)> = Vec::with_capacity(out.len());
    for (k, t) in out {
        if k == "nl"
            && let Some(last) = cleaned.last_mut()
            && last.0 == "pun"
        {
            last.1 = last.1.trim_end().to_owned();
        }
        cleaned.push((k, t));
    }
    cleaned
}

// ---------------------------------------------------------------------------------------------
// Rows for the template
// ---------------------------------------------------------------------------------------------

/// One rendered row.
pub struct Row {
    /// `w<trace>-<n>`: DOM id, the URL hash's state, the detail panel's key.
    pub id: String,
    pub parent: String,
    pub depth: usize,
    pub kind: &'static str,
    pub label: String,
    pub module: String,
    pub loc: String,
    pub has_children: bool,
    /// Folded on first view (groups, the framework row).
    pub collapsed: bool,
    /// Hidden on first view (under a folded row).
    pub hidden: bool,
    pub group: bool,
    pub count: u32,
    pub flagged: bool,
    pub failed: bool,
    pub critical: bool,
    pub start: String,
    pub dur: String,
    pub selft: String,
    pub total: String,
    /// The track, in percent of the trace: bars, gaps, event markers.
    pub bars: Vec<(String, String)>,
    pub gaps: Vec<(String, String)>,
    pub marks: Vec<(String, &'static str, String)>,
    /// Where the duration label sits (just right of the bar; left of it near the end).
    pub label_x: String,
    pub label_left: bool,
    /// Tooltip text.
    pub tip: String,
    pub fp: String,
    pub full: String,
    pub sql: Vec<(&'static str, String)>,
    pub events: Vec<(String, &'static str, String)>,
    /// Outbound HTTP: the API's host; the call timed out.
    pub host: String,
    pub timeout: bool,
}

/// A waterfall, ready to render (`_waterfall.html`).
pub struct View {
    /// The trace id (DOM ids are scoped by it, so several waterfalls share a page).
    pub id: u64,
    pub rows: Vec<Row>,
    /// Rows not rendered (over `MAX_ROWS`).
    pub cut: usize,
    pub ticks: Vec<(String, String)>,
    pub total: String,
    /// Time by kind: (kind, label, percent string, share for the stacked bar's x and width).
    pub split: Vec<(&'static str, &'static str, String, String, String)>,
    pub queries: u32,
    pub n_plus_one: Vec<NPlusOneNote>,
    pub slowest: String,
    pub base: String,
    pub details: bool,
    pub ai: bool,
    pub ask: String,
    pub dropped: u32,
    pub from_disk: bool,
    pub kind: &'static str,
    /// Compact: no header, used inside an explorer row or another page.
    pub compact: bool,
    pub all: bool,
    /// The legend's colors.
    pub keys: Vec<(&'static str, &'static str)>,
    /// Outbound calls that ran one after another and look independent.
    pub seq: Option<SeqNote>,
    /// The most outbound calls that ran at once (2 or more: they ran concurrently).
    pub concurrent: usize,
}

/// "These 3 calls run one after another; running them concurrently would save ~X".
pub struct SeqNote {
    pub count: usize,
    pub parent: String,
    pub path: String,
    pub total: String,
    pub longest: String,
    pub saving: String,
}

pub struct NPlusOneNote {
    pub count: u32,
    pub summary: String,
    pub fp: String,
    /// The folded row's id ("Show the 20 queries").
    pub row: String,
    pub table: String,
    pub column: String,
    /// `order_id` → `OrderId` (SeaORM's column variant).
    pub variant: String,
}

fn pct(us: u64, total: u64) -> f64 {
    (us as f64 / total.max(1) as f64 * 100.0).clamp(0.0, 100.0)
}

fn p(v: f64) -> String {
    format!("{v:.2}%")
}

/// Rows for `t`. `all`: nothing folded (the no-JavaScript "every row" view).
#[allow(clippy::too_many_arguments)]
pub fn view(
    t: &Trace,
    details: bool,
    base: &str,
    compact: bool,
    all: bool,
    ai: bool,
    ask: String,
) -> View {
    let root = tree(t, details);
    let total = t.dur_us.max(1);
    let mut rows = Vec::new();
    let mut cut = 0;
    flatten(&root, t.id, "", 0, false, all, total, &mut rows, &mut cut);
    let split = {
        let parts = time_by_kind(&root);
        let sum: u64 = parts.iter().map(|(_, us)| *us).sum::<u64>().max(1);
        let mut x = 0.0;
        parts
            .into_iter()
            .map(|(k, us)| {
                let share = us as f64 / sum as f64 * 100.0;
                let label = KINDS
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map_or("other (uninstrumented)", |(_, l)| *l);
                let item = (k, label, format!("{share:.0}%"), p(x), p(share));
                x += share;
                item
            })
            .collect()
    };
    let n_plus_one = t
        .n_plus_one
        .iter()
        .map(|(fp, n, sql)| {
            let summary = sql_summary(sql);
            let (table, column) = n1_parts(sql);
            NPlusOneNote {
                count: *n,
                summary,
                fp: super::sql::hex(*fp),
                // The folded row of those statements, or of the method that ran them.
                row: rows
                    .iter()
                    .find(|r| r.group && r.fp == super::sql::hex(*fp))
                    .or_else(|| rows.iter().find(|r| r.group && r.flagged))
                    .map(|r| r.id.clone())
                    .unwrap_or_default(),
                table,
                variant: column
                    .split('_')
                    .map(|w| {
                        let mut c = w.chars();
                        c.next()
                            .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                            .unwrap_or_default()
                    })
                    .collect(),
                column,
            }
        })
        .collect();
    View {
        id: t.id,
        rows,
        cut,
        ticks: (0..=4)
            .map(|i| (p(i as f64 * 25.0), micros(t.dur_us * i / 4)))
            .collect(),
        total: micros(t.dur_us),
        split,
        queries: t.statements,
        n_plus_one,
        slowest: slowest(&root)
            .map(|(l, us)| format!("{l} ({})", micros(us)))
            .unwrap_or_default(),
        base: base.to_owned(),
        details,
        ai,
        ask,
        dropped: t.dropped,
        from_disk: t.from_disk,
        kind: t.kind,
        compact,
        all,
        keys: KINDS.to_vec(),
        seq: super::upstreams::sequential(&root).map(|q| SeqNote {
            count: q.calls.len(),
            parent: q.parent.clone(),
            path: q.path(),
            total: micros(q.total_us),
            longest: micros(q.longest_us),
            saving: micros(q.saving_us),
        }),
        concurrent: super::upstreams::concurrent(&root),
    }
}

/// The table and column of an N+1's condition, for the fix snippet.
fn n1_parts(sql: &str) -> (String, String) {
    let s = sql_summary(sql);
    let table = s.split_whitespace().nth(1).unwrap_or("rows").to_owned();
    let column = s
        .split_once("WHERE ")
        .and_then(|(_, c)| c.split_whitespace().next())
        .unwrap_or("parent_id")
        .to_owned();
    (table, column)
}

#[allow(clippy::too_many_arguments)]
fn flatten(
    n: &Node,
    trace: u64,
    parent: &str,
    depth: usize,
    hidden: bool,
    all: bool,
    total: u64,
    out: &mut Vec<Row>,
    cut: &mut usize,
) {
    if out.len() >= MAX_ROWS {
        *cut += 1 + n.children.len();
        return;
    }
    let id = format!("w{trace}-{}", out.len());
    let collapsed = !all && (n.group || n.kind == "framework");
    let bars: Vec<(String, String)> = n
        .occ
        .iter()
        .take(200)
        .map(|(s, d)| (p(pct(*s, total)), p(pct(*d, total).max(0.25))))
        .collect();
    let end_pct = pct(n.end(), total);
    let tip = format!(
        "{}: starts {}, {}{}{}",
        n.label,
        micros(n.start_us),
        micros(n.dur_us),
        if n.group {
            format!(" span, {} total in {} runs", micros(n.total_us), n.count)
        } else {
            String::new()
        },
        if !n.group && !n.children.is_empty() {
            format!(", self {}", micros(n.self_us))
        } else {
            String::new()
        }
    );
    out.push(Row {
        id: id.clone(),
        parent: parent.to_owned(),
        depth,
        kind: n.kind,
        label: if n.group && n.kind != "framework" {
            format!("{} ×{}", n.label, n.count)
        } else {
            n.label.clone()
        },
        module: n.module.clone(),
        loc: n.loc.clone(),
        has_children: !n.children.is_empty(),
        collapsed,
        hidden,
        group: n.group,
        count: n.count,
        flagged: n.flagged,
        failed: n.failed,
        critical: n.critical,
        start: micros(n.start_us),
        dur: micros(n.dur_us),
        selft: micros(n.self_us),
        total: micros(n.total_us),
        bars,
        gaps: n
            .gaps
            .iter()
            .map(|(s, d)| (p(pct(*s, total)), p(pct(*d, total))))
            .collect(),
        marks: n
            .events
            .iter()
            .take(50)
            .map(|(at, level, msg)| {
                (
                    p(pct(*at, total)),
                    if msg.contains("enqueued") {
                        "job"
                    } else {
                        match *level {
                            "ERROR" => "error",
                            "WARN" => "warn",
                            _ => "info",
                        }
                    },
                    crate::kernel::ui::truncate(msg, 160),
                )
            })
            .collect(),
        // Right of the bar; near the end, inside it (right-aligned), or left of it if it's short.
        label_x: p(if end_pct > 86.0 && pct(n.start_us, total) > 14.0 {
            pct(n.start_us, total)
        } else {
            end_pct
        }),
        label_left: end_pct > 86.0,
        tip,
        fp: n.fp.map(super::sql::hex).unwrap_or_default(),
        sql: if n.fp.is_some() {
            sql_tokens(&n.full)
        } else {
            Vec::new()
        },
        full: n.full.clone(),
        events: n
            .events
            .iter()
            .take(50)
            .map(|(at, level, msg)| (micros(*at), *level, msg.clone()))
            .collect(),
        host: n.host.clone(),
        timeout: n.timeout,
    });
    for c in &n.children {
        flatten(
            c,
            trace,
            &id,
            depth + 1,
            hidden || collapsed,
            all,
            total,
            out,
            cut,
        );
    }
}
