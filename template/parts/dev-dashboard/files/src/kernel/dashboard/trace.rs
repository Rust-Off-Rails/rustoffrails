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
/// A request slower than this is notable.
pub const SLOW_US: u64 = 500_000; // APP: what "slow" means for your app
/// Log lines kept for the live tail's backlog.
pub const LOG_LINES: usize = 1_000;
/// A log message or field list longer than this is cut.
const MAX_TEXT: usize = 2_000;

/// One request, as recorded.
pub struct Trace {
    pub id: u64,
    pub at: DateTime<Utc>,
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
    pub target: &'static str,
    pub start_us: u64,
    pub dur_us: u64,
    pub fields: String,
}

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
}

impl Traces {
    pub fn push(&mut self, t: Arc<Trace>) {
        // INVARIANT: both rings are bounded (test: the_dashboard_stays_within_its_memory_bound).
        if t.notable() {
            if self.notable.len() == NOTABLE {
                self.notable.pop_front();
            }
            self.notable.push_back(t.clone());
        }
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(t);
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
        self.recent
            .iter()
            .chain(self.notable.iter())
            .map(|t| t.approx_bytes())
            .sum()
    }
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

/// A span's start and the request it belongs to.
struct SpanData {
    start: Instant,
    req: Arc<RequestCtx>,
    fields: String,
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
        let Some(req) = http::current() else {
            return;
        };
        let mut text = Text::default();
        attrs.record(&mut text);
        let (_, fields) = text.finish();
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanData {
                start: Instant::now(),
                req,
                fields,
            });
        }
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
        // The nearest ancestor that belongs to the same request.
        let parent = span.scope().skip(1).find_map(|p| {
            p.extensions()
                .get::<SpanData>()
                .filter(|d| d.req.id == data.req.id)
                .map(|_| p.id().into_u64())
        });
        let meta = span.metadata();
        data.req.push_span(SpanRec {
            id: id.into_u64(),
            parent,
            name: meta.name(),
            target: meta.target(),
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
        let req = http::current();
        if let Some(req) = &req {
            let parent = ctx.event_span(event).map(|s| s.id().into_u64());
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

/// One row of the waterfall.
pub struct WaterRow {
    pub kind: &'static str,
    pub label: String,
    pub detail: String,
    pub depth: usize,
    pub start_us: u64,
    pub dur_us: u64,
    /// Bar position on a 0..1000 scale (the SVG's viewBox).
    pub x: u64,
    pub w: u64,
    pub fp: Option<u64>,
    pub flagged: bool,
    /// Preformatted for the page.
    pub start: String,
    pub dur: String,
    pub fp_hex: String,
}

/// The trace as a waterfall: the request, then each span with its SQL and events, in start order,
/// indented under its parent. `details: false` (the read-only production view) leaves out log
/// events and span fields: they carry whatever the app logged (emails, ids, amounts).
pub fn waterfall(t: &Trace, details: bool) -> Vec<WaterRow> {
    let total = t.dur_us.max(1);
    let scale = |start: u64, dur: u64| {
        let x = (start.min(total) * 1000) / total;
        let w = ((dur * 1000) / total).max(2).min(1000 - x.min(998));
        (x, w)
    };
    let flagged: Vec<u64> = t.n_plus_one.iter().map(|(fp, _, _)| *fp).collect();
    // (start, order, parent, row)
    let mut items: Vec<(u64, Option<u64>, Option<u64>, WaterRow)> = Vec::new();
    for s in &t.spans {
        let (x, w) = scale(s.start_us, s.dur_us);
        items.push((
            s.start_us,
            Some(s.id),
            s.parent,
            WaterRow {
                kind: "span",
                label: s.name.to_owned(),
                detail: if s.fields.is_empty() || !details {
                    s.target.to_owned()
                } else {
                    format!("{} {}", s.target, s.fields)
                },
                depth: 0,
                start_us: s.start_us,
                dur_us: s.dur_us,
                x,
                w,
                fp: None,
                flagged: false,
                start: String::new(),
                dur: String::new(),
                fp_hex: String::new(),
            },
        ));
    }
    for q in &t.sql {
        let (x, w) = scale(q.start_us, q.dur_us);
        items.push((
            q.start_us,
            None,
            q.parent,
            WaterRow {
                kind: if q.failed { "sql-failed" } else { "sql" },
                label: "sql".to_owned(),
                detail: crate::kernel::ui::truncate(&q.sql, 160),
                depth: 0,
                start_us: q.start_us,
                dur_us: q.dur_us,
                x,
                w,
                fp: Some(q.fp),
                flagged: flagged.contains(&q.fp),
                start: String::new(),
                dur: String::new(),
                fp_hex: String::new(),
            },
        ));
    }
    for e in t.events.iter().filter(|_| details) {
        let (x, _) = scale(e.at_us, 0);
        items.push((
            e.at_us,
            None,
            e.parent,
            WaterRow {
                kind: if e.message.contains("enqueued") {
                    "job"
                } else {
                    "event"
                },
                label: e.level.to_lowercase(),
                detail: e.message.clone(),
                depth: 0,
                start_us: e.at_us,
                dur_us: 0,
                x,
                w: 2,
                fp: None,
                flagged: false,
                start: String::new(),
                dur: String::new(),
                fp_hex: String::new(),
            },
        ));
    }
    items.sort_by_key(|i| i.0);
    // Depth: 1 + the parent span's depth (parents start before their children).
    let mut depth_of: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut rows = vec![WaterRow {
        start: "0µs".to_owned(),
        dur: super::web::micros(t.dur_us),
        fp_hex: String::new(),
        kind: "request",
        label: format!("{} {}", t.method, t.route),
        detail: format!("{} → {}", t.path, t.status),
        depth: 0,
        start_us: 0,
        dur_us: t.dur_us,
        x: 0,
        w: 1000,
        fp: None,
        flagged: false,
    }];
    for (_, own, parent, mut row) in items {
        row.start = super::web::micros(row.start_us);
        row.dur = if row.dur_us > 0 {
            super::web::micros(row.dur_us)
        } else {
            "·".to_owned()
        };
        row.fp_hex = row.fp.map(super::sql::hex).unwrap_or_default();
        let depth = parent.and_then(|p| depth_of.get(&p).copied()).unwrap_or(0) + 1;
        row.depth = depth.min(8);
        if let Some(own) = own {
            depth_of.insert(own, depth);
        }
        rows.push(row);
    }
    rows
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
