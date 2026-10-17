//! The tools the "Ask AI" agent may call: read-only views over the dashboard's own, already
//! redacted data, plus `read_source` / `search_source` over an allowlist of the app's code, and
//! `propose_action`, which only records a suggestion for the human to click.
//!
//! Each tool is a row in `TOOLS` (name, the step's title on the page, a description written for
//! small models, a JSON schema) and an arm in `call`. Results are plain text, capped at
//! `MAX_TOOL_CHARS`; `ai_agent` wraps them as untrusted data before they go to the provider.
//!
//! APP: to add a tool, add a `ToolDef` row, an arm in `call` returning redacted text, and a case
//! in `every_tool_answers_within_its_cap` (tests/app/dev_dashboard_test.rs). Keep it read-only:
//! a tool that changes anything belongs behind a button, as a `propose_action` kind.
//!
//! GUIDE: .offrails/guides/dev-tools.md

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::ai_context as ctx;
use super::web::micros;
use super::{HUB, lab, lock, route_explorer as rx, sql};
use crate::config::AiProvider;
use crate::kernel::redact;
use crate::state::AppState;

/// APP: a tool result's size cap, in characters; more is cut with a marker.
pub const MAX_TOOL_CHARS: usize = 6_000;
/// APP: `read_source`: the folders it may read (relative to the app root), the file types, the
/// largest file, the most lines per call.
pub const SOURCE_ROOTS: &[&str] = &["src", "migration", "templates"];
pub const SOURCE_EXTENSIONS: &[&str] = &["rs", "html", "sql"];
pub const MAX_SOURCE_BYTES: u64 = 512 * 1024;
pub const MAX_SOURCE_LINES: usize = 160;
/// APP: `search_source`: the most hits returned and files scanned.
pub const MAX_SEARCH_HITS: usize = 20;
const MAX_SEARCH_FILES: usize = 3_000;

/// A tool, as the provider sees it.
pub struct ToolDef {
    pub name: &'static str,
    /// The step's title on the page ("Looking at the slowest routes").
    pub title: &'static str,
    pub description: &'static str,
    pub params: fn() -> Value,
    /// In the small-model set (`DASHBOARD_AI_TOOLS=core`).
    pub core: bool,
    /// Available in the read-only production view (no source, no logs, no schema, no EXPLAIN).
    pub read_only_view: bool,
}

fn none() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// APP: the tool list. Descriptions are what a small model reads to choose: say what it
/// returns, when to use it, and what each parameter does.
pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "get_overview",
        title: "Reading the overview",
        description: "The dashboard's overview cards right now (memory, CPU, requests/s, latency p95, server errors, SQL, DB pool, Tokio tasks, uptime), each with its health tone (ok, warn, err) and the thresholds behind it, plus the slowest routes and most expensive SQL. Call this first for any 'is it healthy' question.",
        params: none,
        core: true,
        read_only_view: true,
    },
    ToolDef {
        name: "list_insights",
        title: "Reading the insights",
        description: "Every finding the dashboard's detectors produced, most important first: N+1 queries, slow routes and queries, rising errors, unindexed foreign keys, full scans, pending migrations, drift, failing jobs, memory growth, CSRF or gate problems. Each has an id (cite it as [finding:<id>]), evidence, a suggested fix and a guide.",
        params: || json!({ "type": "object", "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 30, "description": "How many findings (default 15)." } } }),
        core: true,
        read_only_view: false,
    },
    ToolDef {
        name: "query_requests",
        title: "Looking at the routes",
        description: "Per-route request numbers since the server started: hits, status classes, p50/p95/p99/max latency, bytes, N+1 count, the slowest example traces. Sort to find the slowest or most failing routes. With `minutes`, also each route's requests and 5xx per minute over the last N minutes (up to 30).",
        params: || {
            json!({ "type": "object", "properties": {
            "sort": { "type": "string", "enum": ["p95", "p99", "max", "count", "errors"], "description": "Order (default p95)." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 40, "description": "How many routes (default 10)." },
            "route": { "type": "string", "description": "Only routes whose pattern contains this text, e.g. \"/orders\"." },
            "minutes": { "type": "integer", "minimum": 1, "maximum": 30, "description": "Add per-minute requests and 5xx for the last N minutes." }
        } })
        },
        core: true,
        read_only_view: true,
    },
    ToolDef {
        name: "compare_windows",
        title: "Comparing before and after",
        description: "What changed between two time windows: requests/s, 5xx/s, latency p95, SQL/s, CPU and memory for the whole app, and per route requests, 5xx and p95 (from recorded traces), with the biggest changes first. Default: the last `minutes` against the `minutes` before them. Or give before_from/before_to/after_from/after_to as UTC clock times \"HH:MM\". Use it for 'what changed in the last 10 minutes?' or 'did the deploy make it slower?'.",
        params: || {
            json!({ "type": "object", "properties": {
            "minutes": { "type": "integer", "minimum": 1, "maximum": 120, "description": "Window length for the default comparison (default 10)." },
            "before_from": { "type": "string", "description": "UTC \"HH:MM\"." },
            "before_to": { "type": "string", "description": "UTC \"HH:MM\"." },
            "after_from": { "type": "string", "description": "UTC \"HH:MM\"." },
            "after_to": { "type": "string", "description": "UTC \"HH:MM\"." }
        } })
        },
        core: true,
        read_only_view: true,
    },
    ToolDef {
        name: "get_series",
        title: "Reading a chart",
        description: "One chart's series over a window: min, average, max and last value, about 24 evenly spaced points (UTC clock time and value), and the markers in it (boots, deploys, load-lab runs, error bursts). Series: latency (p95, µs), req (requests/s), errors (5xx/s), cpu (%), memory (bytes), sql (statements/s), sqlp95 (µs), pool (connections in use), tasks. Cite the window you mean as [chart:<series> HH:MM-HH:MM].",
        params: || {
            json!({ "type": "object", "required": ["series"], "properties": {
            "series": { "type": "string", "enum": ["latency", "req", "errors", "cpu", "memory", "sql", "sqlp95", "pool", "tasks"] },
            "minutes": { "type": "integer", "minimum": 1, "maximum": 1440, "description": "The last N minutes (default 15)." },
            "from": { "type": "string", "description": "UTC \"HH:MM\" (with `to`, instead of minutes)." },
            "to": { "type": "string", "description": "UTC \"HH:MM\"." }
        } })
        },
        core: false,
        read_only_view: true,
    },
    ToolDef {
        name: "list_traces",
        title: "Listing traces",
        description: "Recent requests as traces (newest first): id (cite as [trace:<id>]), method, route, status, duration, SQL count and time, N+1 flag. Filter to the notable ones (5xx, N+1 or slower than 0.5 s), errors only, slow only, N+1 only, or one route.",
        params: || {
            json!({ "type": "object", "properties": {
            "filter": { "type": "string", "enum": ["recent", "notable", "errors", "slow", "n_plus_one"], "description": "Which traces (default notable)." },
            "route": { "type": "string", "description": "Exact route pattern, e.g. \"/orders/{id}\"." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 30, "description": "Default 10." }
        } })
        },
        core: true,
        read_only_view: true,
    },
    ToolDef {
        name: "get_trace",
        title: "Opening a trace",
        description: "One request's waterfall: its spans with start and duration, every SQL statement in order (fingerprints, literals stripped) with timings, the N+1 patterns in it, and (development) its log events. Use it to explain why one request was slow.",
        params: || json!({ "type": "object", "required": ["id"], "properties": { "id": { "type": "string", "description": "The trace id, digits only, e.g. \"1298\"." } } }),
        core: true,
        read_only_view: true,
    },
    ToolDef {
        name: "query_sql",
        title: "Looking at the SQL",
        description: "SQL statements grouped by fingerprint (literals stripped): id (cite as [sql:<id>]), count, total, mean, p95 and max time, failures, the routes that ran it, parameters as type and length only. Also the N+1 list (the same statement more than 5 times in one request) and writes during a GET. Filter by a table name.",
        params: || {
            json!({ "type": "object", "properties": {
            "sort": { "type": "string", "enum": ["total", "p95", "max", "count"], "description": "Default total." },
            "table": { "type": "string", "description": "Only statements that mention this table." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 30, "description": "Default 10." }
        } })
        },
        core: true,
        read_only_view: true,
    },
    ToolDef {
        name: "explain_query",
        title: "Explaining a query plan",
        description: "The database's query plan (SQLite EXPLAIN QUERY PLAN, Postgres EXPLAIN; never ANALYZE, nothing runs) for a recorded SELECT, by its fingerprint id, with the numbers and routes. Use it to see a full table scan or a missing index.",
        params: || json!({ "type": "object", "required": ["id"], "properties": { "id": { "type": "string", "description": "The fingerprint id from query_sql (hex)." } } }),
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "search_logs",
        title: "Searching the logs",
        description: "The live log tail's lines (newest last, secrets redacted): time, level, target, message, fields, and the trace id of the request that logged it. Filter by minimum level, text and the last N minutes.",
        params: || {
            json!({ "type": "object", "properties": {
            "level": { "type": "string", "enum": ["trace", "debug", "info", "warn", "error"], "description": "Minimum level (default info)." },
            "contains": { "type": "string", "description": "Only lines containing this text (case-insensitive)." },
            "minutes": { "type": "integer", "minimum": 1, "maximum": 60, "description": "Only the last N minutes." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 60, "description": "Default 30." }
        } })
        },
        core: true,
        read_only_view: false,
    },
    ToolDef {
        name: "get_schema",
        title: "Reading the schema",
        description: "Every table of the live database: columns with types and nullability, primary keys, indexes, foreign keys with ON DELETE, and row counts. Never row data.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "get_table",
        title: "Reading a table",
        description: "One table: columns, indexes, foreign keys, the tables that reference it, the recorded queries that touch it, and the migration that created or altered it. Cite as [table:<name>].",
        params: || json!({ "type": "object", "required": ["name"], "properties": { "name": { "type": "string", "description": "The table name, e.g. \"orders\"." } } }),
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "list_migrations_and_drift",
        title: "Checking migrations and drift",
        description: "Every migration (oldest first) with its area, applied or pending, edited after it was applied, and what it did (derived from its source); then the drift between the migrations and the live database.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "list_routes_with_problems",
        title: "Checking the routes",
        description: "Routes with problems and where their handlers are: mutating routes without CSRF, admin or dev paths without a gate, routes open to anyone in production, routes missing from docs/routes.md, routes no test mentions, overlapping patterns, 5xx and slow routes, each with the handler's source file and line (for read_source).",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "jobs_summary",
        title: "Checking the job queue",
        description: "The job queue (when the jobs sample is installed): jobs per worker, queue and state, the oldest waiting job, and the failures grouped by cause.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "job_error_groups",
        title: "Grouping job failures",
        description: "Job failures grouped by worker and cause (the first line of the error, numbers stripped, secrets redacted), most frequent first, with example job ids (cite as [job:<id>]).",
        params: || json!({ "type": "object", "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Default 10." } } }),
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "get_job",
        title: "Opening a job",
        description: "One job: worker, queue, state, attempts, times, its arguments' names and types (never values), and its latest errors (redacted).",
        params: || json!({ "type": "object", "required": ["id"], "properties": { "id": { "type": "string", "description": "The job id, digits only." } } }),
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "test_results",
        title: "Reading the test results",
        description: "The latest test run on SQLite and Postgres: passed, failed, skipped and flaky counts, each failure with its (redacted) output, and the slowest tests.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "github_status",
        title: "Checking CI on GitHub",
        description: "The repository's CI (when GITHUB_REPO is set): the latest workflow runs with their conclusion, branch and duration, the success rate, open pull requests and issues, the latest release, and the number of security alerts.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "read_source",
        title: "Reading the code",
        description: "Lines of one of the app's source files, numbered, with secrets redacted: only under src/, migration/ or templates/, only .rs, .html or .sql, never dotfiles. Use it to point at the exact code behind a problem (find the file and line with list_routes_with_problems or search_source first), and cite it as [file:<path>:<line>]. At most 160 lines per call.",
        params: || {
            json!({ "type": "object", "required": ["path"], "properties": {
            "path": { "type": "string", "description": "Relative to the app root, e.g. \"src/controllers/orders.rs\"." },
            "start_line": { "type": "integer", "minimum": 1, "description": "First line (default 1)." },
            "end_line": { "type": "integer", "minimum": 1, "description": "Last line (default start_line + 119)." }
        } })
        },
        core: true,
        read_only_view: false,
    },
    ToolDef {
        name: "search_source",
        title: "Searching the code",
        description: "Finds a literal text (3 to 80 characters, case-insensitive) in the app's source under src/, migration/ and templates/: up to 20 hits as path:line and the line. Use it to find the handler, model or template behind a route, query or table, then read_source around the hit.",
        params: || {
            json!({ "type": "object", "required": ["query"], "properties": {
            "query": { "type": "string", "description": "Literal text, e.g. \"fn orders_n_plus_one\" or \"lab_order_items\"." }
        } })
        },
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "mailbox_summary",
        title: "Looking at the mailbox",
        description: "The dev mailbox (when installed): the latest emails as shapes: id (cite as [mail:<id>]), subject, masked addresses, sizes. Never a body, a link or a code.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "get_email",
        title: "Opening an email",
        description: "One email from the dev mailbox: subject, masked addresses, header names, text and HTML sizes, link hosts, attachments, and checks (text part, Message-ID, plain-http links). Never the body or a code.",
        params: || json!({ "type": "object", "required": ["id"], "properties": { "id": { "type": "string", "description": "The message id, digits only." } } }),
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "app_snapshot",
        title: "Reading the app's snapshot",
        description: "What the app is: name, version, framework version, frontend mode, migrations applied and pending, CSS build state, route count, and each installed sample's section (realtime connections, the mailbox…), scalars only.",
        params: none,
        core: false,
        read_only_view: false,
    },
    ToolDef {
        name: "propose_action",
        title: "Proposing an action",
        description: "Suggests one action to the developer, shown as a button they may click (nothing runs now): run_scenario (target: a load-lab scenario: n-plus-one, slow-query, slow-endpoint, error-burst, panic, memory-spike) to reproduce a problem; run_tests (target: sqlite, postgres or both); open_page (target: a dashboard path such as /dev/dashboard/sql/<id>). Give a one-sentence reason.",
        params: || {
            json!({ "type": "object", "required": ["kind", "target", "reason"], "properties": {
            "kind": { "type": "string", "enum": ["run_scenario", "run_tests", "open_page"] },
            "target": { "type": "string" },
            "reason": { "type": "string", "description": "Why, in one sentence." }
        } })
        },
        core: true,
        read_only_view: true,
    },
];

/// The tools this run may use: the read-only view gets its subset, `core` the small-model set.
pub fn enabled(ro: bool, core_only: bool) -> Vec<&'static ToolDef> {
    TOOLS
        .iter()
        .filter(|t| !ro || t.read_only_view)
        .filter(|t| !core_only || t.core)
        .collect()
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The `tools` array in the provider's shape (Anthropic: `input_schema`; OpenAI-compatible:
/// `{"type": "function", "function": {name, description, parameters}}`).
pub fn definitions(provider: Option<AiProvider>, tools: &[&ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| match provider {
                Some(AiProvider::Anthropic) => json!({
                    "name": t.name, "description": t.description, "input_schema": (t.params)(),
                }),
                _ => json!({ "type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": (t.params)(),
                } }),
            })
            .collect(),
    )
}

/// An action the model proposed: rendered as a button (a normal CSRF-checked POST, or a link).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Action {
    /// `run_scenario`, `run_tests` or `open_page`.
    pub kind: String,
    pub target: String,
    pub reason: String,
}

impl Action {
    /// The button's label.
    pub fn label(&self) -> String {
        match self.kind.as_str() {
            "run_scenario" => lab::Scenario::parse(&self.target).map_or_else(
                || "Run a scenario".to_owned(),
                |s| format!("Run the {} scenario", s.label()),
            ),
            "run_tests" => format!("Run the tests ({})", self.target),
            _ => format!("Open {}", self.target),
        }
    }
}

/// A tool's answer.
pub struct Output {
    pub text: String,
    pub is_error: bool,
    pub action: Option<Action>,
}

impl Output {
    fn ok(text: String) -> Self {
        let (text, _) = cap(text);
        Output {
            text,
            is_error: false,
            action: None,
        }
    }

    fn err(text: impl Into<String>) -> Self {
        Output {
            text: text.into(),
            is_error: true,
            action: None,
        }
    }
}

/// Cuts a tool result at `MAX_TOOL_CHARS`, on a line, saying so.
pub fn cap(text: String) -> (String, bool) {
    if text.chars().count() <= MAX_TOOL_CHARS {
        return (text, false);
    }
    let mut out = String::new();
    for line in text.lines() {
        if out.chars().count() + line.chars().count() + 1 > MAX_TOOL_CHARS - 60 {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("[… cut: the result was over MAX_TOOL_CHARS; narrow the call]\n");
    (out, true)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// An integer argument, clamped (small models send "10" as often as 10).
fn arg_n(args: &Value, key: &str, default: u64, max: u64) -> u64 {
    args.get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .unwrap_or(default)
        .clamp(1, max)
}

/// One of `allowed`, or an error naming them.
fn arg_enum<'a>(
    args: &'a Value,
    key: &str,
    allowed: &[&'a str],
    default: &'a str,
) -> Result<&'a str, String> {
    match arg_str(args, key) {
        None => Ok(default),
        Some(v) => allowed
            .iter()
            .find(|a| **a == v)
            .copied()
            .ok_or_else(|| format!("`{key}` must be one of {}; got {v:?}.", allowed.join(", "))),
    }
}

/// "query_requests(sort=p95, minutes=15)": the call as the page shows it.
pub fn describe_call(name: &str, args: &Value) -> String {
    let parts: Vec<String> = args
        .as_object()
        .map(|o| {
            o.iter()
                .take(6)
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => crate::kernel::ui::truncate(s, 60),
                        other => crate::kernel::ui::truncate(&other.to_string(), 60),
                    };
                    format!("{k}={v}")
                })
                .collect()
        })
        .unwrap_or_default();
    format!("{name}({})", parts.join(", "))
}

/// Runs one tool. Unknown names and bad arguments are errors the model can recover from (the
/// message says what to send instead); nothing here writes anything.
// SECURITY: every tool reads; the only "action" is a validated proposal the human clicks, so an
// injected instruction in app data can't make the agent change anything
// (test: a_prompt_injection_in_app_data_has_no_side_effect_and_leaks_nothing)
pub async fn call(state: &AppState, ro: bool, core_only: bool, name: &str, args: &Value) -> Output {
    let allowed = enabled(ro, core_only);
    if !allowed.iter().any(|t| t.name == name) {
        return Output::err(format!(
            "There is no tool named {:?} here. Available: {}.",
            crate::kernel::ui::truncate(name, 60),
            allowed
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !args.is_object() {
        return Output::err("The arguments must be a JSON object matching the tool's schema.");
    }
    let r: Result<String, String> = match name {
        "get_overview" => Ok(overview()),
        "list_insights" => Ok(insights(state, arg_n(args, "limit", 15, 30) as usize).await),
        "query_requests" => query_requests(args),
        "compare_windows" => compare_windows(args),
        "get_series" => get_series(args),
        "list_traces" => list_traces(args),
        "get_trace" => match arg_str(args, "id") {
            Some(id) if id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()) => {
                ctx::trace(id, ro)
            }
            _ => Err("`id` must be a trace id (digits), e.g. from list_traces.".into()),
        },
        "query_sql" => query_sql(args, ro),
        "explain_query" => explain(state, args).await,
        "search_logs" => search_logs(args),
        "get_schema" => Ok(ctx::schema_all(state).await),
        "get_table" => match arg_str(args, "name") {
            Some(n) if super::schema::plain_ident(n) => ctx::schema_one(state, n).await,
            _ => Err("`name` must be a table name (letters, digits, underscores).".into()),
        },
        "list_migrations_and_drift" => Ok(ctx::migrations_ctx(state).await),
        "list_routes_with_problems" => Ok(routes_with_problems(state).await),
        "jobs_summary" => Ok(ctx::jobs(state).await),
        "job_error_groups" => {
            Ok(ctx::job_errors(state, arg_n(args, "limit", 10, 20) as usize).await)
        }
        "get_job" => match arg_str(args, "id") {
            Some(id) if super::ai::Tab::Jobs.valid_item(id) && id != "errors" => {
                ctx::job(state, id).await
            }
            _ => Err("`id` must be a job id (digits).".into()),
        },
        "test_results" => Ok(ctx::tests()),
        "github_status" => Ok(github_status(state).await),
        "read_source" => read_source(
            &app_root(),
            arg_str(args, "path").unwrap_or_default(),
            arg_n(args, "start_line", 1, 1_000_000) as usize,
            args.get("end_line")
                .and_then(Value::as_u64)
                .map(|n| n as usize),
        ),
        "search_source" => search_source(&app_root(), arg_str(args, "query").unwrap_or_default()),
        "mailbox_summary" => Ok(ctx::mailbox(state).await),
        "get_email" => match arg_str(args, "id") {
            Some(id) if super::ai::Tab::Mailbox.valid_item(id) => ctx::email(state, id).await,
            _ => Err("`id` must be a message id (digits).".into()),
        },
        "app_snapshot" => Ok(format!(
            "{}{}",
            ctx::app_snapshot(state).await,
            ctx::source_text(state, "sockets").await
        )),
        "propose_action" => {
            return match proposal(args, ro) {
                Ok(a) => Output {
                    text: format!(
                        "Proposed to the developer as a button: {}. Nothing has run; continue your answer.",
                        a.label()
                    ),
                    is_error: false,
                    action: Some(a),
                },
                Err(e) => Output::err(e),
            };
        }
        _ => Err("Not implemented.".into()),
    };
    match r {
        Ok(t) => Output::ok(t),
        Err(e) => Output::err(e),
    }
}

/// The app's root (where `src/` is): the test runner's root, `.` outside tests.
fn app_root() -> PathBuf {
    super::testruns::root()
}

// ---- the dashboard's numbers -------------------------------------------------------------------

fn overview() -> String {
    let mut s = String::from("## Cards (last 10 seconds; tone from the thresholds)\n");
    for c in super::web::cards(super::web::Mode {
        ro: false,
        base: "/dev/dashboard",
    }) {
        let _ = writeln!(s, "- {}: {} ({}) [{}]", c.label, c.value, c.sub, c.tone);
    }
    let h = &super::web::HEALTH;
    let _ = writeln!(
        s,
        "\n## Thresholds (warn, err)\nCPU {:.0}%/{:.0}%, latency p95 {}/{}, server errors {:.0}%/{:.0}% of requests, pool in use {:.0}%/{:.0}%, memory growth since boot {}/{}, SQL p95 {}/{}\n",
        h.cpu.0,
        h.cpu.1,
        micros(u64::from(h.p95.0)),
        micros(u64::from(h.p95.1)),
        h.errors.0 * 100.0,
        h.errors.1 * 100.0,
        h.pool.0 * 100.0,
        h.pool.1 * 100.0,
        super::web::bytes(h.memory_growth.0),
        super::web::bytes(h.memory_growth.1),
        micros(u64::from(h.sql_p95.0)),
        micros(u64::from(h.sql_p95.1)),
    );
    s.push_str(&ctx::overview());
    s
}

async fn insights(state: &AppState, limit: usize) -> String {
    let mut all = super::insights::findings(state).await;
    all.extend(super::web::route_findings(state).await);
    all.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.score.cmp(&a.score)));
    let mut s = format!("## {} findings, most important first\n", all.len());
    for f in all.iter().take(limit) {
        let _ = writeln!(
            s,
            "- finding {} [{}] {} ({}). Evidence: {}. Fix: {}. Guide: {}. Page: {}",
            super::web::finding_id(&f.key),
            f.severity.key(),
            ctx::clean(&f.title, 200),
            f.kind,
            ctx::clean(&f.evidence, 300),
            ctx::clean(&f.fix, 240),
            f.guide,
            f.href
        );
    }
    if all.is_empty() {
        s.push_str("(no findings)\n");
    }
    s
}

fn query_requests(args: &Value) -> Result<String, String> {
    let sort = arg_enum(
        args,
        "sort",
        &["p95", "p99", "max", "count", "errors"],
        "p95",
    )?;
    let limit = arg_n(args, "limit", 10, 40) as usize;
    let filter = arg_str(args, "route");
    let http = lock(&HUB.http);
    let mut rows = http.rows();
    rows.retain(|r| filter.is_none_or(|f| r.route.contains(f)));
    match sort {
        "count" => rows.sort_by_key(|r| std::cmp::Reverse(r.count)),
        "errors" => rows.sort_by_key(|r| std::cmp::Reverse(r.s5 * 1_000 + r.s4)),
        "p99" => rows.sort_by_key(|r| std::cmp::Reverse(r.p99)),
        "max" => rows.sort_by_key(|r| std::cmp::Reverse(r.max)),
        _ => rows.sort_by_key(|r| std::cmp::Reverse(r.p95)),
    }
    let minutes = args
        .get("minutes")
        .map(|_| arg_n(args, "minutes", 10, 30) as i64);
    let now_min = chrono::Utc::now().timestamp().div_euclid(60);
    let mut s = format!("## Routes by {sort} (since the server started)\n");
    for r in rows.iter().take(limit) {
        let _ = writeln!(
            s,
            "- route {} {}: {} hits (2xx {}, 3xx {}, 4xx {}, 5xx {}), p50 {} p95 {} p99 {} max {}, N+1 in {}, slowest traces {}",
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
            r.n_plus_one,
            r.slowest
                .iter()
                .map(|(id, us, st)| format!("{id} ({}, {st})", micros(*us)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let (Some(m), Some(st)) = (minutes, http.get(r.method, r.route)) {
            let per: Vec<String> = st
                .minutes
                .iter()
                .filter(|(min, _, _)| now_min - min < m)
                .map(|(min, n, e)| format!("{} {n} req/{e} 5xx", super::ai::clock(min * 60)))
                .collect();
            if !per.is_empty() {
                let _ = writeln!(s, "  per minute: {}", per.join("; "));
            }
        }
    }
    if rows.is_empty() {
        s.push_str("(no requests recorded yet)\n");
    }
    Ok(s)
}

/// A window's aggregates over the per-second (or per-minute) samples.
struct Agg {
    req: f64,
    e5: f64,
    p95: f64,
    p95_max: u32,
    sql: f64,
    cpu: f64,
    rss: u64,
    n: usize,
}

fn agg(samples: &[super::series::Sample]) -> Agg {
    let n = samples.len().max(1) as f64;
    let reqs: f64 = samples.iter().map(|s| f64::from(s.req)).sum();
    let busy: Vec<_> = samples.iter().filter(|s| s.req > 0.0).collect();
    let weighted = if reqs > 0.0 {
        busy.iter()
            .map(|s| f64::from(s.p95) * f64::from(s.req))
            .sum::<f64>()
            / reqs
    } else {
        0.0
    };
    Agg {
        req: reqs / n,
        e5: samples.iter().map(|s| f64::from(s.e5)).sum::<f64>() / n,
        p95: weighted,
        p95_max: busy.iter().map(|s| s.p95).max().unwrap_or(0),
        sql: samples.iter().map(|s| f64::from(s.sql)).sum::<f64>() / n,
        cpu: samples.iter().map(|s| f64::from(s.cpu)).sum::<f64>() / n,
        rss: samples.last().map_or(0, |s| s.rss),
        n: samples.len(),
    }
}

fn window_args(args: &Value, from_key: &str, to_key: &str, now: i64) -> Option<(i64, i64)> {
    let f = arg_str(args, from_key)?;
    let t = arg_str(args, to_key)?;
    super::ai::chart_range(&format!("{f}-{t}"), now)
}

fn compare_windows(args: &Value) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    let (before, after) = match (
        window_args(args, "before_from", "before_to", now),
        window_args(args, "after_from", "after_to", now),
    ) {
        (Some(b), Some(a)) => (b, a),
        _ => {
            let m = arg_n(args, "minutes", 10, 120) as i64 * 60;
            ((now - 2 * m, now - m), (now - m, now))
        }
    };
    let (b, a) = {
        let series = lock(&HUB.series);
        (
            agg(&series.between(before.0, before.1).0),
            agg(&series.between(after.0, after.1).0),
        )
    };
    let label = |w: (i64, i64)| format!("{}-{}", super::ai::clock(w.0), super::ai::clock(w.1));
    let mut s = format!(
        "## Before {} vs after {} (UTC)\nCite them as [chart:latency {}] and [chart:latency {}].\n\n",
        label(before),
        label(after),
        label(before),
        label(after)
    );
    let change = |x: f64, y: f64| {
        if x <= 0.0 && y <= 0.0 {
            "no change".to_owned()
        } else if x <= 0.0 {
            "new".to_owned()
        } else {
            format!("{:+.0}%", (y - x) * 100.0 / x)
        }
    };
    let _ = writeln!(
        s,
        "- requests/s {:.2} → {:.2} ({})\n- 5xx/s {:.3} → {:.3} ({})\n- latency p95 (request-weighted) {} → {} ({}); peak {} → {}\n- SQL/s {:.1} → {:.1} ({})\n- CPU {:.0}% → {:.0}%\n- memory at the end {} → {}\n- samples {} → {}",
        b.req,
        a.req,
        change(b.req, a.req),
        b.e5,
        a.e5,
        change(b.e5, a.e5),
        micros(b.p95 as u64),
        micros(a.p95 as u64),
        change(b.p95, a.p95),
        micros(u64::from(b.p95_max)),
        micros(u64::from(a.p95_max)),
        b.sql,
        a.sql,
        change(b.sql, a.sql),
        b.cpu,
        a.cpu,
        super::web::bytes(b.rss),
        super::web::bytes(a.rss),
        b.n,
        a.n
    );
    // Per route: requests and 5xx from each route's per-minute history; p95 from the traces kept.
    let mut routes: Vec<(String, u64, u64, u64, u64, u64, u64)> = Vec::new();
    {
        let http = lock(&HUB.http);
        let in_win = |m: i64, w: (i64, i64)| m * 60 >= w.0 - 59 && m * 60 < w.1;
        for st in http.stats() {
            let (mut bn, mut be, mut an, mut ae) = (0_u64, 0_u64, 0_u64, 0_u64);
            for (m, n, e) in &st.minutes {
                if in_win(*m, before) {
                    bn += u64::from(*n);
                    be += u64::from(*e);
                } else if in_win(*m, after) {
                    an += u64::from(*n);
                    ae += u64::from(*e);
                }
            }
            if bn + an > 0 {
                routes.push((format!("{} {}", st.method, st.route), bn, be, an, ae, 0, 0));
            }
        }
    }
    let traces = lock(&HUB.traces).list(false);
    let p95 = |route: &str, w: (i64, i64)| {
        let mut d: Vec<u64> = traces
            .iter()
            .filter(|t| format!("{} {}", t.method, t.route) == route)
            .filter(|t| (w.0..w.1).contains(&t.at.timestamp()))
            .map(|t| t.dur_us)
            .collect();
        d.sort_unstable();
        d.get((d.len() * 95 / 100).min(d.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    };
    for r in &mut routes {
        r.5 = p95(&r.0, before);
        r.6 = p95(&r.0, after);
    }
    routes.sort_by_key(|r| {
        std::cmp::Reverse(
            (r.4 as i64 - r.2 as i64).unsigned_abs() * 1_000_000
                + (r.6 as i64 - r.5 as i64).unsigned_abs()
                + (r.3 as i64 - r.1 as i64).unsigned_abs(),
        )
    });
    s.push_str("\n## Per route, biggest change first (p95 from the recorded traces)\n");
    for (route, bn, be, an, ae, bp, ap) in routes.iter().take(15) {
        let _ = writeln!(
            s,
            "- route {route}: requests {bn} → {an}, 5xx {be} → {ae}, p95 {} → {}",
            if *bp == 0 { "—".into() } else { micros(*bp) },
            if *ap == 0 { "—".into() } else { micros(*ap) },
        );
    }
    if routes.is_empty() {
        s.push_str(
            "(no route traffic in either window; per-route history covers the last 30 minutes)\n",
        );
    }
    let marks = lock(&HUB.series).annotations(before.0, after.1);
    if !marks.is_empty() {
        s.push_str("\n## Markers\n");
        for m in marks.iter().take(10) {
            let _ = writeln!(
                s,
                "- {} {}: {}",
                super::ai::clock(m.t),
                m.kind,
                ctx::clean(&m.label, 100)
            );
        }
    }
    Ok(s)
}

fn get_series(args: &Value) -> Result<String, String> {
    let key = arg_str(args, "series")
        .and_then(super::ai::chart_key)
        .ok_or(
            "`series` must be one of latency, req, errors, cpu, memory, sql, sqlp95, pool, tasks.",
        )?;
    let now = chrono::Utc::now().timestamp();
    let (from, to) = window_args(args, "from", "to", now)
        .unwrap_or_else(|| (now - arg_n(args, "minutes", 15, 1440) as i64 * 60, now));
    let value: fn(&super::series::Sample) -> f64 = match key {
        "latency" => |s| f64::from(s.p95),
        "req" => |s| f64::from(s.req),
        "errors" => |s| f64::from(s.e5),
        "cpu" => |s| f64::from(s.cpu),
        "memory" => |s| s.rss as f64,
        "sql" => |s| f64::from(s.sql),
        "sqlp95" => |s| f64::from(s.sql_p95),
        "pool" => |s| f64::from(s.pool_in_use()),
        _ => |s| f64::from(s.tasks),
    };
    let fmt = |v: f64| match key {
        "latency" | "sqlp95" => micros(v.max(0.0) as u64),
        "memory" => super::web::bytes(v.max(0.0) as u64),
        "cpu" => format!("{v:.0}%"),
        "req" | "errors" | "sql" => format!("{v:.2}/s"),
        _ => format!("{v:.0}"),
    };
    let (samples, marks) = {
        let series = lock(&HUB.series);
        (series.between(from, to).0, series.annotations(from, to))
    };
    let points = super::charts::downsample(&samples, 24);
    let vals: Vec<f64> = samples.iter().map(value).collect();
    let mut s = format!(
        "## {key} {}-{} UTC ({} samples). Cite as [chart:{key} {}-{}].\n",
        super::ai::clock(from),
        super::ai::clock(to),
        samples.len(),
        super::ai::clock(from),
        super::ai::clock(to)
    );
    if vals.is_empty() {
        s.push_str("(no samples in that window)\n");
        return Ok(s);
    }
    let min = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let max = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let avg = vals.iter().sum::<f64>() / vals.len() as f64;
    let _ = writeln!(
        s,
        "min {}, average {}, max {}, last {}",
        fmt(min),
        fmt(avg),
        fmt(max),
        fmt(*vals.last().unwrap_or(&0.0))
    );
    let _ = writeln!(
        s,
        "points: {}",
        points
            .iter()
            .map(|p| format!("{} {}", super::ai::clock(p.t), fmt(value(p))))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for m in marks.iter().take(10) {
        let _ = writeln!(
            s,
            "marker {} {}: {}",
            super::ai::clock(m.t),
            m.kind,
            ctx::clean(&m.label, 100)
        );
    }
    Ok(s)
}

fn list_traces(args: &Value) -> Result<String, String> {
    let filter = arg_enum(
        args,
        "filter",
        &["recent", "notable", "errors", "slow", "n_plus_one"],
        "notable",
    )?;
    let route = arg_str(args, "route");
    let limit = arg_n(args, "limit", 10, 30) as usize;
    let traces = lock(&HUB.traces);
    let list = traces.list(filter == "notable");
    let mut s = format!(
        "## Traces ({filter}{}), newest first\n",
        route.map(|r| format!(", route {r}")).unwrap_or_default()
    );
    let mut n = 0;
    for t in list
        .iter()
        .filter(|t| route.is_none_or(|r| t.route == r))
        .filter(|t| match filter {
            "errors" => t.status >= 500,
            "slow" => t.dur_us >= super::trace::SLOW_US,
            "n_plus_one" => !t.n_plus_one.is_empty(),
            _ => true,
        })
        .take(limit)
    {
        n += 1;
        let _ = writeln!(
            s,
            "- trace {}: {} {} ({}) → {} in {} at {}, {} SQL ({}){}",
            t.id,
            t.method,
            t.route,
            ctx::clean(&t.path, 100),
            t.status,
            micros(t.dur_us),
            t.at.format("%H:%M:%S"),
            t.statements,
            micros(t.sql_us),
            if t.n_plus_one.is_empty() { "" } else { ", N+1" }
        );
    }
    if n == 0 {
        s.push_str("(none match)\n");
    }
    Ok(s)
}

fn query_sql(args: &Value, ro: bool) -> Result<String, String> {
    let sort = sql::Sort::parse(Some(arg_enum(
        args,
        "sort",
        &["total", "p95", "max", "count"],
        "total",
    )?));
    let table = arg_str(args, "table");
    if table.is_some_and(|t| !super::schema::plain_ident(t)) {
        return Err("`table` must be a table name (letters, digits, underscores).".into());
    }
    let limit = arg_n(args, "limit", 10, 30) as usize;
    let stats = lock(&HUB.sql);
    let mut s = format!(
        "## SQL by {} (fingerprints, literals stripped)\n",
        sort.key()
    );
    for q in stats
        .rows(sort)
        .iter()
        .filter(|q| table.is_none_or(|t| q.sql.contains(t)))
        .take(limit)
    {
        let _ = writeln!(
            s,
            "- sql {}: {}× total {} mean {} p95 {} max {}, failed {}, routes {}{}: {}",
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
            ctx::clean(&q.sql, 400)
        );
    }
    s.push_str("\n## N+1 (the same statement more than 5 times in one request)\n");
    for n in stats
        .n_plus_one()
        .iter()
        .filter(|n| table.is_none_or(|t| n.sql.contains(t)))
        .take(10)
    {
        let _ = writeln!(
            s,
            "- {} ran sql {} {}× in one request ({} requests did; worst trace {}): {}",
            n.route,
            sql::hex(n.fp),
            n.count,
            n.times,
            n.trace,
            ctx::clean(&n.sql, 300)
        );
    }
    drop(stats);
    let writes = lock(&HUB.http).get_writes().len();
    let _ = writeln!(s, "\nWrites during a GET: {writes}");
    Ok(s)
}

async fn explain(state: &AppState, args: &Value) -> Result<String, String> {
    let id = arg_str(args, "id").ok_or("`id` is required: a fingerprint id from query_sql.")?;
    if !state.config.is_development() {
        return Err("EXPLAIN is development only.".into());
    }
    ctx::sql_one(state, id, false).await.map(|s| {
        if s.contains("### Query plan") {
            s
        } else {
            format!("{s}\n(no plan: not a SELECT, or no parameters kept for it)\n")
        }
    })
}

fn search_logs(args: &Value) -> Result<String, String> {
    let level = arg_enum(
        args,
        "level",
        &["trace", "debug", "info", "warn", "error"],
        "info",
    )?;
    let min = super::trace::LogLine::level_rank(&level.to_ascii_uppercase());
    let needle =
        arg_str(args, "contains").map(|n| n.chars().take(100).collect::<String>().to_lowercase());
    let since = args.get("minutes").map(|_| {
        chrono::Utc::now() - chrono::Duration::minutes(arg_n(args, "minutes", 10, 60) as i64)
    });
    let limit = arg_n(args, "limit", 30, 60) as usize;
    let lines = lock(&HUB.logs).lines();
    let hits: Vec<_> = lines
        .iter()
        .filter(|l| super::trace::LogLine::level_rank(l.level) >= min)
        .filter(|l| since.is_none_or(|t| l.at >= t))
        .filter(|l| {
            needle.as_deref().is_none_or(|n| {
                l.message.to_lowercase().contains(n) || l.target.to_lowercase().contains(n)
            })
        })
        .collect();
    let start = hits.len().saturating_sub(limit);
    let mut s = format!(
        "## Log lines ({} match; the latest {} shown, redacted)\n",
        hits.len(),
        hits.len() - start
    );
    for l in &hits[start..] {
        let _ = writeln!(
            s,
            "- {} {} {}: {}{}{}",
            l.at.format("%H:%M:%S"),
            l.level,
            l.target,
            ctx::clean(&l.message, 300),
            if l.fields.is_empty() {
                String::new()
            } else {
                format!(" [{}]", ctx::clean(&l.fields, 200))
            },
            l.trace.map(|t| format!(" (trace {t})")).unwrap_or_default()
        );
    }
    Ok(s)
}

async fn routes_with_problems(state: &AppState) -> String {
    let p = super::web::route_problems(state).await;
    let root = app_root();
    let table = rx::table();
    let at = |entry: &str| -> String {
        let (method, path) = entry.split_once(' ').unwrap_or(("", entry));
        table
            .iter()
            .find(|r| r.path == path && (method.is_empty() || r.method == method))
            .and_then(|r| {
                rx::source_of(&root, r.handler)
                    .map(|src| format!("{} → {} at {src}", entry, r.handler))
            })
            .unwrap_or_else(|| entry.to_owned())
    };
    let mut s = String::from("## Route problems (with each handler's source)\n");
    let mut list = |label: &str, v: &[String]| {
        if !v.is_empty() {
            let _ = writeln!(s, "\n### {label}");
            for e in v.iter().take(20) {
                let _ = writeln!(s, "- {}", at(e));
            }
        }
    };
    list("Mutating routes without CSRF", &p.without_csrf);
    list("Admin or dev paths without a gate", &p.ungated);
    list("Open to anyone in production", &p.open_in_production);
    list("Missing from docs/routes.md", &p.missing_docs);
    list("No test mentions them", &p.untested);
    for (a, b) in &p.overlaps {
        let _ = writeln!(s, "- overlapping patterns: {a} and {b}");
    }
    let stats = lock(&HUB.http).rows();
    let mut bad: Vec<_> = stats
        .iter()
        .filter(|r| r.s5 > 0 || r.p95 >= super::insights::SLOW_ROUTE_P95_US)
        .collect();
    bad.sort_by_key(|r| std::cmp::Reverse(r.s5 * 1_000_000 + r.p95));
    if !bad.is_empty() {
        s.push_str("\n### Failing or slow\n");
        for r in bad.iter().take(15) {
            let src = table
                .iter()
                .find(|d| d.path == r.route && d.method == r.method)
                .and_then(|d| {
                    rx::source_of(&root, d.handler).map(|src| format!(" → {} at {src}", d.handler))
                })
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "- route {} {}: 5xx {}, p95 {}{src}",
                r.method,
                r.route,
                r.s5,
                micros(r.p95)
            );
        }
    }
    s
}

async fn github_status(state: &AppState) -> String {
    let cfg = &state.config.dashboard;
    if cfg.github_repo.is_none() {
        return "GitHub isn't configured (GITHUB_REPO=owner/name).\n".into();
    }
    let v = super::github::load(cfg, &HUB.github).await;
    let mut s = String::from("## GitHub (workflow names, conclusions, branches and counts)\n");
    if let Some(r) = v.success_rate {
        let _ = writeln!(s, "CI success rate (recent days): {r:.0}%");
    }
    for r in v.runs.iter().take(10) {
        let _ = writeln!(
            s,
            "- run #{} {}: {} on {} ({}, {}) \"{}\"",
            r.number,
            ctx::clean(&r.name, 60),
            r.status,
            ctx::clean(&r.branch, 60),
            r.duration,
            r.when,
            ctx::clean(&r.title, 80)
        );
    }
    if let Some(p) = &v.open_prs {
        let _ = writeln!(s, "Open pull requests: {p}");
    }
    if let Some(n) = v.open_issues {
        let _ = writeln!(s, "Open issues: {n}");
    }
    if let Some(r) = &v.release {
        let _ = writeln!(s, "Latest release: {} ({})", ctx::clean(&r.tag, 40), r.when);
    }
    match v.alerts {
        super::github::Alerts::Count(n) => {
            let _ = writeln!(s, "Open security alerts: {n}");
        }
        super::github::Alerts::NotPermitted => {
            s.push_str("Security alerts: not visible to this token\n")
        }
        super::github::Alerts::Unknown => {}
    }
    for e in v.errors.iter().take(3) {
        let _ = writeln!(s, "(error: {})", ctx::clean(e, 160));
    }
    s
}

fn proposal(args: &Value, ro: bool) -> Result<Action, String> {
    let kind = arg_enum(
        args,
        "kind",
        &["run_scenario", "run_tests", "open_page"],
        "",
    )?;
    let target = arg_str(args, "target").unwrap_or_default();
    let reason = crate::kernel::ui::truncate(
        &redact::text(arg_str(args, "reason").unwrap_or_default()),
        200,
    );
    let ok = match kind {
        "run_scenario" => !ro && lab::Scenario::parse(target).is_some(),
        "run_tests" => !ro && matches!(target, "sqlite" | "postgres" | "both"),
        "open_page" => action_path_ok(target, ro),
        _ => false,
    };
    if !ok {
        return Err(format!(
            "That proposal isn't valid here. run_scenario takes {}; run_tests takes sqlite, postgres or both; open_page takes a dashboard path (/dev/dashboard/…).",
            lab::Scenario::ALL
                .iter()
                .map(|s| s.key())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Action {
        kind: kind.to_owned(),
        target: target.to_owned(),
        reason,
    })
}

/// A page an `open_page` proposal may link to: a same-site path under the dashboard (or the
/// mailbox and jobs pages in development), no scheme, no `//`, no backslash, plain characters.
// SECURITY: a proposed link can't leave the app or carry markup (test: proposed_actions_are_validated_and_render_as_buttons)
pub fn action_path_ok(path: &str, ro: bool) -> bool {
    let prefixes: &[&str] = if ro {
        &["/admin/dashboard"]
    } else {
        &[
            "/dev/dashboard",
            "/dev/mailbox",
            "/admin/jobs",
            "/dev/sockets",
        ]
    };
    crate::kernel::ui::same_site_path(path).is_some()
        && path.len() <= 200
        && prefixes.iter().any(|p| {
            path == *p
                || path.starts_with(&format!("{p}/"))
                || path.starts_with(&format!("{p}?"))
                || path.starts_with(&format!("{p}#"))
        })
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/-_.?=&#%{}".contains(c))
}

// ---- source ------------------------------------------------------------------------------------

/// A relative path `read_source` may open: under `SOURCE_ROOTS`, a known extension, no `..`, no
/// dotfile or dot-folder, plain characters. Checked again against the real path when opened.
pub fn source_path_ok(path: &str) -> bool {
    let p = Path::new(path);
    let mut parts = p.components();
    let first_ok =
        matches!(parts.next(), Some(Component::Normal(f)) if SOURCE_ROOTS.iter().any(|r| f == *r));
    first_ok
        && path.len() <= 200
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
        && p.components().all(|c| match c {
            Component::Normal(n) => !n.to_string_lossy().starts_with('.'),
            _ => false,
        })
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SOURCE_EXTENSIONS.contains(&e))
}

/// `src/x.rs`, `src/x.rs:12` or `src/x.rs:12-30`: a `[file:…]` citation.
pub fn citable_path(v: &str) -> bool {
    let (path, lines) = v.split_once(':').unwrap_or((v, ""));
    source_path_ok(path)
        && lines
            .split('-')
            .all(|n| n.is_empty() || (n.len() <= 7 && n.bytes().all(|b| b.is_ascii_digit())))
}

/// A line with likely secrets hidden: `redact::text`, and the string literals of a line that
/// names a secret (`let password = "…"`).
pub fn redact_source_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let secretish = [
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
        "private_key",
        "credential",
    ]
    .iter()
    .any(|w| lower.contains(w));
    let mut out = redact::text(line);
    if secretish {
        let mut s = String::with_capacity(out.len());
        let mut in_str = false;
        let mut kept = false;
        for c in out.chars() {
            if c == '"' {
                in_str = !in_str;
                if in_str {
                    kept = false;
                }
                s.push(c);
            } else if in_str {
                if !kept {
                    s.push_str("[redacted]");
                    kept = true;
                }
            } else {
                s.push(c);
            }
        }
        out = s;
    }
    out
}

/// Numbered lines of one allowlisted source file, with secrets redacted.
// SECURITY: only src/, migration/ and templates/, only .rs/.html/.sql, no `..`, no dotfiles (so
// never .env), no symlink out of those folders (the real path is checked), size and line caps,
// secrets redacted (test: read_source_stays_inside_the_allowlist)
pub fn read_source(
    root: &Path,
    path: &str,
    start: usize,
    end: Option<usize>,
) -> Result<String, String> {
    let path = path.trim().trim_start_matches("./");
    if !source_path_ok(path) {
        return Err(format!(
            "read_source only opens .rs, .html or .sql files under {} (relative paths, no dotfiles, no ..); got {:?}.",
            SOURCE_ROOTS.join("/, "),
            crate::kernel::ui::truncate(path, 80)
        ));
    }
    let first = path.split('/').next().unwrap_or_default();
    let allowed_dir = root
        .join(first)
        .canonicalize()
        .map_err(|_| format!("This app has no {first}/ folder."))?;
    let real = root
        .join(path)
        .canonicalize()
        .map_err(|_| format!("No such file: {path}. Find it with search_source."))?;
    if !real.starts_with(&allowed_dir) || !real.is_file() {
        return Err("That path leaves the allowed folders.".into());
    }
    let meta = std::fs::metadata(&real).map_err(|_| "Unreadable file.".to_owned())?;
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "{path} is over {} KB; not read.",
            MAX_SOURCE_BYTES / 1024
        ));
    }
    let text = std::fs::read_to_string(&real).map_err(|_| "Not a text file.".to_owned())?;
    let total = text.lines().count();
    let start = start.max(1).min(total.max(1));
    let end = end
        .unwrap_or(start + 119)
        .max(start)
        .min(start + MAX_SOURCE_LINES - 1)
        .min(total);
    let mut s = format!("## {path}, lines {start}-{end} of {total} (secrets redacted)\n");
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(start - 1)
        .take(end + 1 - start)
    {
        let _ = writeln!(s, "{:>5} | {}", i + 1, redact_source_line(line));
    }
    Ok(s)
}

/// Literal search over the allowlisted source.
pub fn search_source(root: &Path, query: &str) -> Result<String, String> {
    let q = query.trim();
    if q.chars().count() < 3 || q.chars().count() > 80 {
        return Err("`query` must be 3 to 80 characters of literal text.".into());
    }
    let needle = q.to_lowercase();
    let mut files = Vec::new();
    for r in SOURCE_ROOTS {
        walk(root, &root.join(r), &mut files);
    }
    let mut hits = Vec::new();
    for rel in files.iter().take(MAX_SEARCH_FILES) {
        let Ok(meta) = std::fs::metadata(root.join(rel)) else {
            continue;
        };
        if meta.len() > MAX_SOURCE_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(format!(
                    "- {rel}:{}: {}",
                    i + 1,
                    crate::kernel::ui::truncate(redact_source_line(line.trim()).as_str(), 160)
                ));
                if hits.len() >= MAX_SEARCH_HITS {
                    break;
                }
            }
        }
        if hits.len() >= MAX_SEARCH_HITS {
            break;
        }
    }
    let mut s = format!(
        "## {} hits for {:?} (src/, migration/, templates/)\n",
        hits.len(),
        q
    );
    for h in &hits {
        s.push_str(h);
        s.push('\n');
    }
    Ok(s)
}

/// Every allowlisted file under `dir`, relative to `root`, sorted; dot entries and symlinks are
/// skipped.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if name.starts_with('.') || ft.is_symlink() {
            continue;
        }
        let path = e.path();
        if ft.is_dir() {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if source_path_ok(&rel) {
                out.push(rel);
            }
        }
        if out.len() >= MAX_SEARCH_FILES {
            return;
        }
    }
}
