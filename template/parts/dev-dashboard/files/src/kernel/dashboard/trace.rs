//! Traces and the live log: a `tracing-subscriber` layer that records the spans and events of each
//! request into its trace (request → handler → sql → render → jobs enqueued), and every log line
//! into a ring that the live tail streams.
//!
//! Installed at `main.rs`'s `tracing layers` anchor. It sees what the log filter lets through
//! (`RUST_LOG`; development defaults to debug for the app and tower-http).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use super::http::{self, RequestCtx};
use super::{HUB, lock};

/// Traces kept: the most recent, plus the notable ones (errors, N+1, slow) for longer.
pub const RECENT: usize = 300; // APP: more history, more memory (each trace is a few KB)
pub const NOTABLE: usize = 100;
/// The rings' byte budget: past it the oldest recent traces go first, whatever their count (a
/// trace with 2,000 method spans weighs as much as a hundred small ones). Notable traces older
/// than memory's are still on disk (`history.rs`). APP.
pub const TRACE_BYTES: usize = 8 * 1024 * 1024;
/// A request slower than this is notable.
pub const SLOW_US: u64 = 500_000; // APP: what "slow" means for your app
/// Log lines kept for the live tail's backlog.
pub const LOG_LINES: usize = 1_000;
/// A log message or field list longer than this is cut.
const MAX_TEXT: usize = 2_000;

/// One request (or job, or mail delivery), as recorded.
pub struct Trace {
    pub id: u64,
    pub at: DateTime<Utc>,
    /// `request`, `job`, `mail` or `cron` (`TRACE_KINDS`).
    pub kind: &'static str,
    /// Read back from the history file (an earlier run of the app), not recorded by this process.
    pub from_disk: bool,
    pub method: &'static str,
    pub route: &'static str,
    pub path: String,
    pub status: u16,
    pub dur_us: u64,
    pub bytes: Option<u64>,
    pub request_id: String,
    pub spans: Vec<SpanRec>,
    pub sql: Vec<SqlRec>,
    pub events: Vec<EventRec>,
    /// Fingerprints run more than `http::N_PLUS_ONE` times: (fingerprint, times, SQL).
    pub n_plus_one: Vec<(u64, u32, Arc<str>)>,
    pub statements: u32,
    pub sql_us: u64,
    /// Spans, statements or events over the per-trace caps (counted, not kept).
    pub dropped: u32,
}

impl Trace {
    pub fn notable(&self) -> bool {
        self.status >= 500 || !self.n_plus_one.is_empty() || self.dur_us >= SLOW_US
    }

    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<Trace>()
            + self.path.len()
            + self
                .spans
                .iter()
                .map(|s| std::mem::size_of::<SpanRec>() + s.fields.len())
                .sum::<usize>()
            + self.sql.len() * std::mem::size_of::<SqlRec>()
            // Read back from disk, a trace's SQL strings are its own (live, they're shared).
            + if self.from_disk {
                self.sql.iter().map(|q| q.sql.len()).sum::<usize>() / self.sql.len().max(1)
            } else {
                0
            }
            + self
                .events
                .iter()
                .map(|e| std::mem::size_of::<EventRec>() + e.message.len())
                .sum::<usize>()
    }
}

pub struct SpanRec {
    pub id: u64,
    pub parent: Option<u64>,
    pub name: &'static str,
    /// The module that opened it (`offrails_host::controllers::orders`).
    pub target: &'static str,
    /// Where in the source (`src/controllers/orders.rs`, line), from the span's metadata.
    pub file: Option<&'static str>,
    pub line: Option<u32>,
    pub start_us: u64,
    pub dur_us: u64,
    pub fields: String,
}

/// What a trace is of: a request, a background job, a mail delivery, a scheduled task.
pub const TRACE_KINDS: [&str; 4] = ["request", "job", "mail", "cron"];

pub struct SqlRec {
    pub fp: u64,
    /// The normalized statement, shared with the SQL page.
    pub sql: Arc<str>,
    pub start_us: u64,
    pub dur_us: u64,
    pub parent: Option<u64>,
    pub failed: bool,
}

pub struct EventRec {
    pub at_us: u64,
    pub level: &'static str,
    pub message: String,
    pub parent: Option<u64>,
}

#[derive(Default)]
pub struct Traces {
    recent: VecDeque<Arc<Trace>>,
    notable: VecDeque<Arc<Trace>>,
    /// `approx_bytes` of every trace held (shared ones once per ring).
    bytes: usize,
}

impl Traces {
    pub fn push(&mut self, t: Arc<Trace>) {
        // INVARIANT: both rings are bounded, by count and by bytes (test:
        // the_dashboard_stays_within_its_memory_bound).
        let size = t.approx_bytes();
        if t.notable() {
            if self.notable.len() == NOTABLE
                && let Some(old) = self.notable.pop_front()
            {
                self.bytes = self.bytes.saturating_sub(old.approx_bytes());
            }
            self.bytes += size;
            self.notable.push_back(t.clone());
        }
        // A trace read back from disk (an earlier run's) only fills the notable ring.
        if !t.from_disk {
            if self.recent.len() == RECENT
                && let Some(old) = self.recent.pop_front()
            {
                self.bytes = self.bytes.saturating_sub(old.approx_bytes());
            }
            self.bytes += size;
            self.recent.push_back(t);
        }
        while self.bytes > TRACE_BYTES {
            let old = if self.recent.len() > 1 {
                self.recent.pop_front()
            } else if self.notable.len() > 1 {
                self.notable.pop_front()
            } else {
                None
            };
            let Some(old) = old else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.approx_bytes());
        }
    }

    pub fn find(&self, id: u64) -> Option<Arc<Trace>> {
        self.recent
            .iter()
            .chain(self.notable.iter())
            .find(|t| t.id == id)
            .cloned()
    }

    /// Newest first: recent ones, or only the notable ones.
    pub fn list(&self, notable_only: bool) -> Vec<Arc<Trace>> {
        let mut out: Vec<Arc<Trace>> = if notable_only {
            self.notable.iter().cloned().collect()
        } else {
            let mut all: Vec<Arc<Trace>> = self.recent.iter().cloned().collect();
            for t in &self.notable {
                if !all.iter().any(|r| r.id == t.id) {
                    all.push(t.clone());
                }
            }
            all
        };
        out.sort_by_key(|t| std::cmp::Reverse(t.id));
        out
    }

    pub fn lens(&self) -> (usize, usize) {
        (self.recent.len(), self.notable.len())
    }

    pub fn approx_bytes(&self) -> usize {
        self.bytes
    }
}

/// A trace by id, wherever it is kept: the rings, a route's slowest examples, or the history file
/// (a notable trace from an earlier run).
pub fn find(id: u64) -> Option<Arc<Trace>> {
    let mem = lock(&HUB.traces).find(id);
    mem.or_else(|| lock(&HUB.http).example(id))
        .or_else(|| super::history::trace(id))
}

/// One line of the live tail: a log event, or a finished request.
pub struct LogLine {
    pub id: u64,
    pub at: DateTime<Utc>,
    pub level: &'static str,
    pub target: String,
    pub message: String,
    pub fields: String,
    /// The request it belongs to (its trace id), when it ran inside one.
    pub trace: Option<u64>,
    /// Set on request lines: (status, duration µs).
    pub request: Option<(u16, u64)>,
}

impl LogLine {
    pub fn level_rank(level: &str) -> u8 {
        match level {
            "ERROR" => 5,
            "WARN" => 4,
            "INFO" => 3,
            "DEBUG" => 2,
            _ => 1,
        }
    }
}

#[derive(Default)]
pub struct Logs {
    lines: VecDeque<Arc<LogLine>>,
}

impl Logs {
    pub fn push(&mut self, line: Arc<LogLine>) {
        if self.lines.len() == LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Oldest first.
    pub fn lines(&self) -> Vec<Arc<LogLine>> {
        self.lines.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn approx_bytes(&self) -> usize {
        self.lines
            .iter()
            .map(|l| {
                std::mem::size_of::<LogLine>() + l.target.len() + l.message.len() + l.fields.len()
            })
            .sum()
    }
}

fn level_str(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Adds a line to the ring and to every open live tail.
pub fn push_log(line: LogLine) {
    let line = Arc::new(line);
    lock(&HUB.logs).push(line.clone());
    // No receiver (no tab open) is fine.
    let _ = HUB.log_tx.send(line);
}

/// The request's own line in the live tail: `GET /dev/lab/orders 200 in 3.2ms`.
pub fn request_line(t: &Trace) {
    let level = match t.status {
        500.. => "ERROR",
        400..=499 => "WARN",
        _ => "INFO",
    };
    let mut fields = format!("route={}", t.route);
    if t.statements > 0 {
        let _ = write!(fields, " sql={}", t.statements);
    }
    if !t.n_plus_one.is_empty() {
        fields.push_str(" n_plus_one=true");
    }
    push_log(LogLine {
        id: HUB.next_id(),
        at: Utc::now(),
        level,
        target: "request".to_owned(),
        message: format!(
            "{} {} {} in {}",
            t.method,
            t.path,
            t.status,
            super::web::micros(t.dur_us)
        ),
        fields,
        trace: Some(t.id),
        request: Some((t.status, t.dur_us)),
    });
}

/// APP: the OpenTelemetry hook, off by default. To send traces to a collector (Jaeger, Tempo,
/// Honeycomb), `cargo add opentelemetry opentelemetry_sdk opentelemetry-otlp`, build a tracer
/// provider once in `main`, and turn each `Trace` into spans here: the request as the root span
/// (`t.method`, `t.route`, `t.status`), `t.spans` as children by `parent`, `t.sql` as client spans
/// with `db.statement = sql` (normalized: it carries no values). Keep it off in tests.
pub const OTLP_EXPORT: bool = false;

/// Called once per finished request.
pub fn export(t: &Trace) {
    if !OTLP_EXPORT {
        return;
    }
    tracing::debug!(
        trace = t.id,
        "otlp export is enabled but not wired (trace.rs)"
    );
}

/// Field names whose values never reach the dashboard.
// SECURITY: a token or password logged as a field shows as [redacted] in the live tail and
// traces (test: secrets_in_log_fields_are_redacted).
const SECRET_FIELDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "cookie",
    "api_key",
    "apikey",
    "session",
    "credential",
];

pub fn is_secret_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_FIELDS.iter().any(|s| n.contains(s))
}

/// Collects an event's message and fields as text.
#[derive(Default)]
struct Text {
    message: String,
    fields: String,
}

impl Text {
    fn push(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
            return;
        }
        if self.fields.len() >= MAX_TEXT {
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        if is_secret_field(field.name()) {
            let _ = write!(self.fields, "{}=[redacted]", field.name());
        } else {
            let _ = write!(self.fields, "{}={value:?}", field.name());
        }
    }

    fn finish(mut self) -> (String, String) {
        cut(&mut self.message);
        cut(&mut self.fields);
        (self.message, self.fields)
    }
}

fn cut(s: &mut String) {
    if s.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
}

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.push(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field, value);
    }
}

/// A span's start and the trace it belongs to.
struct SpanData {
    start: Instant,
    req: Arc<RequestCtx>,
    fields: String,
    /// It opened a background trace (a job, a mail delivery): closing it finishes the trace.
    root: bool,
}

/// Root spans (outside any request) that become a trace of their own: the jobs sample's `job`
/// span, a mailer's `mail` span, a scheduler's `cron` span. APP: name your own background work's
/// outermost span one of these and it shows in Traces with its methods and SQL.
pub fn background_kind(name: &str) -> Option<&'static str> {
    match name {
        "job" | "perform" => Some("job"),
        "mail" | "deliver" | "deliver_mail" => Some("mail"),
        "cron" | "cron_tick" | "recurring" => Some("cron"),
        _ => None,
    }
}

/// `worker=Foo job_id=5 …` → `Foo`.
fn field<'a>(fields: &'a str, name: &str) -> Option<&'a str> {
    fields.split(' ').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.trim_matches('"'))
    })
}

/// The trace the current span belongs to, outside a request (a job's SQL): the nearest span with
/// one, looked up in the subscriber's registry.
pub fn background_current() -> Option<Arc<RequestCtx>> {
    tracing::dispatcher::get_default(|d| {
        let id = d.current_span().id()?.clone();
        let reg = d.downcast_ref::<tracing_subscriber::Registry>()?;
        let span = reg.span(&id)?;
        span.scope()
            .find_map(|s| s.extensions().get::<SpanData>().map(|x| x.req.clone()))
    })
}

/// The trace for code running now: the request's, else a background trace's.
pub fn current_ctx() -> Option<Arc<RequestCtx>> {
    http::current().or_else(background_current)
}

/// The layer. Cheap when the dashboard is off: one atomic load per span and event.
pub struct DashboardLayer;

impl<S> Layer<S> for DashboardLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !HUB.enabled() {
            return;
        }
        let Some(span) = ctx.span(id) else {
            return;
        };
        let inherited = http::current().or_else(|| {
            span.parent().and_then(|p| {
                p.scope()
                    .find_map(|s| s.extensions().get::<SpanData>().map(|d| d.req.clone()))
            })
        });
        let mut text = Text::default();
        attrs.record(&mut text);
        let (_, fields) = text.finish();
        let (req, root) = match inherited {
            Some(req) => (req, false),
            None => {
                let Some(kind) = background_kind(attrs.metadata().name()) else {
                    return;
                };
                if http::quiet() {
                    return;
                }
                let name = field(&fields, "worker")
                    .or_else(|| field(&fields, "mailer"))
                    .or_else(|| field(&fields, "name"))
                    .unwrap_or(attrs.metadata().name());
                let id = field(&fields, "job_id")
                    .map(|j| format!("job-{j}"))
                    .unwrap_or_default();
                (
                    Arc::new(RequestCtx::background(
                        kind,
                        super::history::intern(name),
                        &fields,
                        id,
                    )),
                    true,
                )
            }
        };
        span.extensions_mut().insert(SpanData {
            start: Instant::now(),
            req,
            fields,
            root,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut ext = span.extensions_mut();
        if let Some(data) = ext.get_mut::<SpanData>() {
            let mut text = Text::default();
            values.record(&mut text);
            let (_, more) = text.finish();
            if !more.is_empty() && data.fields.len() < MAX_TEXT {
                if !data.fields.is_empty() {
                    data.fields.push(' ');
                }
                data.fields.push_str(&more);
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let ext = span.extensions();
        let Some(data) = ext.get::<SpanData>() else {
            return;
        };
        if data.root {
            // The trace's own row is the root: its spans hang under it.
            super::http::finish_background(&data.req);
            return;
        }
        // The nearest ancestor that belongs to the same trace (and isn't its root).
        let parent = span.scope().skip(1).find_map(|p| {
            p.extensions()
                .get::<SpanData>()
                .filter(|d| d.req.id == data.req.id && !d.root)
                .map(|_| p.id().into_u64())
        });
        let meta = span.metadata();
        data.req.push_span(SpanRec {
            id: id.into_u64(),
            parent,
            name: meta.name(),
            target: meta.target(),
            file: meta.file(),
            line: meta.line(),
            start_us: data.req.offset_us(data.start),
            dur_us: data.start.elapsed().as_micros() as u64,
            fields: data.fields.clone(),
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if !HUB.enabled() {
            return;
        }
        let meta = event.metadata();
        // WHY: tower-http's own "started/finished processing request" lines repeat what the
        // request line says, without the route.
        if meta.target().starts_with("tower_http::trace") {
            return;
        }
        let mut text = Text::default();
        event.record(&mut text);
        let (message, fields) = text.finish();
        let level = level_str(meta.level());
        let span = ctx.event_span(event);
        let req = http::current().or_else(|| {
            span.as_ref().and_then(|s| {
                s.scope()
                    .find_map(|p| p.extensions().get::<SpanData>().map(|d| d.req.clone()))
            })
        });
        if let Some(req) = &req {
            let parent = span
                .as_ref()
                .filter(|s| s.extensions().get::<SpanData>().is_none_or(|d| !d.root))
                .map(|s| s.id().into_u64());
            if req.kind != "request" && (level == "ERROR" || message.contains("exception")) {
                req.failed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            req.push_event(EventRec {
                at_us: req.offset_us(Instant::now()),
                level,
                message: if fields.is_empty() {
                    message.clone()
                } else {
                    format!("{message} {fields}")
                },
                parent,
            });
        }
        push_log(LogLine {
            id: HUB.next_id(),
            at: Utc::now(),
            level,
            target: meta.target().to_owned(),
            message,
            fields,
            trace: req.map(|r| r.id),
            request: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_in_log_fields_are_redacted() {
        assert!(is_secret_field("password"));
        assert!(is_secret_field("reset_token"));
        assert!(is_secret_field("Authorization"));
        assert!(!is_secret_field("user_id"));
    }
}
