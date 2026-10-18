//! Methods: a profiler-style view of the spans in the traces kept (wall clock, so an `await` on the
//! database counts, which a CPU profiler can't see). Per method: calls, total and self time,
//! p50/p95/max per call, its share of all request time, the routes that call it, its callers and
//! callees. And the icicle (a flame graph, top down) of one route's traces merged, also used for
//! the CPU profile's stacks (`profiler.rs`).
//!
//! A "method" is any span: `#[tracing::instrument]` on a function, an `info_span!` around a block,
//! the kernel's handler and render spans (README "Instrumenting your own methods").

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::hist::Hist;
use super::trace::Trace;
use super::waterfall::{span_kind, sql_summary};

/// One method's numbers over the traces given.
pub struct MethodStat {
    /// `name` + module: what makes two spans the same method.
    pub key: String,
    pub name: &'static str,
    pub module: &'static str,
    pub loc: String,
    pub kind: &'static str,
    pub calls: u64,
    pub total_us: u64,
    pub self_us: u64,
    /// Per call.
    pub hist: Hist,
    /// Traces it ran in, and their total duration (its share is `total_us` over all traces').
    pub traces: u64,
    /// The most times it ran in one trace.
    pub max_per_trace: u32,
    pub routes: BTreeMap<&'static str, u64>,
    pub callers: HashMap<String, u64>,
    pub callees: HashMap<String, u64>,
}

impl MethodStat {
    pub fn p50(&self) -> u64 {
        self.hist.quantile(0.50)
    }
    pub fn p95(&self) -> u64 {
        self.hist.quantile(0.95)
    }
    pub fn max(&self) -> u64 {
        self.hist.max()
    }
    /// Calls per trace that ran it.
    pub fn per_trace(&self) -> f64 {
        self.calls as f64 / self.traces.max(1) as f64
    }
    pub fn top(map: &HashMap<String, u64>, n: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = map.iter().map(|(k, c)| (k.clone(), *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }
}

/// The union length of intervals, clipped to `[lo, hi)`.
pub fn covered(mut iv: Vec<(u64, u64)>, lo: u64, hi: u64) -> u64 {
    iv.retain(|(s, e)| e > s);
    iv.sort_unstable();
    let mut total = 0;
    let mut cursor = lo;
    for (s, e) in iv {
        let (s, e) = (s.max(cursor), e.min(hi));
        if e > s {
            total += e - s;
            cursor = e;
        }
    }
    total
}

/// Aggregates every span of `traces`, slowest total first. Also returns the traces' total time.
pub fn aggregate(traces: &[Arc<Trace>]) -> (Vec<MethodStat>, u64) {
    let mut by: HashMap<String, MethodStat> = HashMap::new();
    let mut all_us = 0_u64;
    for t in traces {
        all_us += t.dur_us;
        let names: HashMap<u64, &'static str> = t.spans.iter().map(|s| (s.id, s.name)).collect();
        let mut kids: HashMap<u64, Vec<(u64, u64)>> = HashMap::new();
        let mut callee_names: HashMap<u64, Vec<String>> = HashMap::new();
        for s in &t.spans {
            if let Some(p) = s.parent {
                kids.entry(p)
                    .or_default()
                    .push((s.start_us, s.start_us + s.dur_us));
                callee_names.entry(p).or_default().push(s.name.to_owned());
            }
        }
        for q in &t.sql {
            if let Some(p) = q.parent {
                kids.entry(p)
                    .or_default()
                    .push((q.start_us, q.start_us + q.dur_us));
                callee_names
                    .entry(p)
                    .or_default()
                    .push(format!("sql: {}", sql_summary(&q.sql)));
            }
        }
        let mut seen: HashMap<String, u32> = HashMap::new();
        for s in &t.spans {
            // A handler span names its handler (`handler=lab::orders`): one row per handler.
            let name = if s.name == "handler" {
                s.fields
                    .split(' ')
                    .find_map(|kv| kv.strip_prefix("handler="))
                    .map(|h| super::history::intern(&format!("handler {}", h.trim_matches('"'))))
                    .unwrap_or(s.name)
            } else {
                s.name
            };
            let key = format!("{}@{}", name, s.target);
            let m = by.entry(key.clone()).or_insert_with(|| MethodStat {
                key: key.clone(),
                name,
                module: s.target,
                loc: match (s.file, s.line) {
                    (Some(f), Some(l)) => format!("{f}:{l}"),
                    _ => String::new(),
                },
                kind: span_kind(s.name, s.target, &s.fields),
                calls: 0,
                total_us: 0,
                self_us: 0,
                hist: Hist::default(),
                traces: 0,
                max_per_trace: 0,
                routes: BTreeMap::new(),
                callers: HashMap::new(),
                callees: HashMap::new(),
            });
            let end = s.start_us + s.dur_us;
            let inner = covered(
                kids.get(&s.id).cloned().unwrap_or_default(),
                s.start_us,
                end,
            );
            m.calls += 1;
            m.total_us += s.dur_us;
            m.self_us += s.dur_us.saturating_sub(inner);
            m.hist.record(s.dur_us);
            let caller =
                s.parent
                    .and_then(|p| names.get(&p).copied())
                    .unwrap_or(if t.kind == "request" {
                        "(request)"
                    } else {
                        "(job)"
                    });
            *m.callers.entry(caller.to_owned()).or_default() += 1;
            for c in callee_names.get(&s.id).into_iter().flatten() {
                *m.callees.entry(c.clone()).or_default() += 1;
            }
            *seen.entry(key).or_default() += 1;
        }
        for (key, n) in seen {
            if let Some(m) = by.get_mut(&key) {
                m.traces += 1;
                m.max_per_trace = m.max_per_trace.max(n);
                *m.routes.entry(t.route).or_default() += 1;
            }
        }
    }
    let mut v: Vec<MethodStat> = by.into_values().collect();
    v.sort_by(|a, b| b.total_us.cmp(&a.total_us).then(a.key.cmp(&b.key)));
    (v, all_us)
}

// ---------------------------------------------------------------------------------------------
// Icicles (flame graphs, top down)
// ---------------------------------------------------------------------------------------------

/// A frame of an icicle: a name on a path from the root, with the time (or samples) under it.
#[derive(Default, Clone, Debug)]
pub struct Frame {
    pub name: String,
    pub kind: &'static str,
    pub value: u64,
    pub count: u64,
    pub children: BTreeMap<String, Frame>,
}

impl Frame {
    pub fn add(&mut self, path: &[(String, &'static str)], value: u64) {
        self.value += value;
        self.count += 1;
        if let Some(((name, kind), rest)) = path.split_first() {
            let child = self.children.entry(name.clone()).or_insert_with(|| Frame {
                name: name.clone(),
                kind,
                ..Frame::default()
            });
            child.add(rest, value);
        }
    }

    /// The frame at `path` ("a;b;c"), or self.
    pub fn at(&self, path: &str) -> &Frame {
        let mut f = self;
        for part in path.split(';').filter(|p| !p.is_empty()) {
            match f.children.get(part) {
                Some(c) => f = c,
                None => break,
            }
        }
        f
    }

    pub fn depth(&self) -> usize {
        1 + self.children.values().map(Frame::depth).max().unwrap_or(0)
    }
}

/// One route's traces merged into an icicle: request → spans → statements, by name, the value the
/// time spent (each statement and span counted once, at its place in the tree).
pub fn icicle(traces: &[Arc<Trace>]) -> Frame {
    let mut root = Frame {
        name: "all".into(),
        kind: "request",
        ..Frame::default()
    };
    for t in traces {
        let by_id: HashMap<u64, &super::trace::SpanRec> =
            t.spans.iter().map(|s| (s.id, s)).collect();
        let path_of = |mut at: Option<u64>| {
            let mut path: Vec<(String, &'static str)> = Vec::new();
            let mut guard = HashSet::new();
            while let Some(id) = at {
                if !guard.insert(id) {
                    break;
                }
                let Some(s) = by_id.get(&id) else {
                    break;
                };
                path.push((s.name.to_owned(), span_kind(s.name, s.target, &s.fields)));
                at = s.parent;
            }
            path.reverse();
            path
        };
        // The request's own time: what no span covers.
        let top: Vec<(u64, u64)> = t
            .spans
            .iter()
            // A span whose parent wasn't recorded (an outer layer's, closed after the trace was
            // built) is a top-level one too.
            .filter(|s| s.parent.is_none_or(|p| !by_id.contains_key(&p)))
            .map(|s| (s.start_us, s.start_us + s.dur_us))
            .chain(
                t.sql
                    .iter()
                    .filter(|q| q.parent.is_none_or(|p| !by_id.contains_key(&p)))
                    .map(|q| (q.start_us, q.start_us + q.dur_us)),
            )
            .collect();
        root.add(&[], t.dur_us.saturating_sub(covered(top, 0, t.dur_us)));
        for s in &t.spans {
            let mut kids: Vec<(u64, u64)> = t
                .spans
                .iter()
                .filter(|c| c.parent == Some(s.id))
                .map(|c| (c.start_us, c.start_us + c.dur_us))
                .collect();
            kids.extend(
                t.sql
                    .iter()
                    .filter(|q| q.parent == Some(s.id))
                    .map(|q| (q.start_us, q.start_us + q.dur_us)),
            );
            let own = s
                .dur_us
                .saturating_sub(covered(kids, s.start_us, s.start_us + s.dur_us));
            root.add(&path_of(Some(s.id)), own);
        }
        for q in &t.sql {
            let mut path = path_of(q.parent.filter(|p| by_id.contains_key(p)));
            path.push((format!("sql: {}", sql_summary(&q.sql)), "sql"));
            root.add(&path, q.dur_us);
        }
    }
    root
}

/// One drawn cell of an icicle, in percent of the focused frame's width.
pub struct Cell {
    pub x: String,
    pub y: u32,
    pub w: String,
    pub label: String,
    pub title: String,
    pub kind: &'static str,
    /// The frame's path, for zooming in (`?focus=`).
    pub path: String,
    /// Wide enough to carry its label.
    pub labeled: bool,
}

/// Cells for `root` focused at `focus`: each frame's width its share of the focused frame's value,
/// children left to right, widest first. Frames narrower than 0.2% aren't drawn.
pub fn cells(root: &Frame, focus: &str, unit: fn(u64) -> String) -> (Vec<Cell>, u32) {
    let top = root.at(focus);
    let mut w = Walk {
        total: top.value.max(1) as f64,
        unit,
        out: Vec::new(),
        depth: 0,
    };
    w.walk(top, 0.0, 0, focus.to_owned());
    (w.out, w.depth)
}

struct Walk {
    total: f64,
    unit: fn(u64) -> String,
    out: Vec<Cell>,
    depth: u32,
}

impl Walk {
    fn walk(&mut self, f: &Frame, x: f64, y: u32, path: String) {
        let w = f.value as f64 / self.total * 100.0;
        if w < 0.2 || self.out.len() > 2_000 {
            return;
        }
        self.depth = self.depth.max(y + 1);
        self.out.push(Cell {
            x: format!("{x:.3}%"),
            y: y * 20,
            w: format!("{:.3}%", (w - 0.05).max(0.05)),
            label: crate::kernel::ui::truncate(&f.name, ((w * 1.4) as usize).max(4)),
            title: format!(
                "{}: {} ({:.1}%), {} samples",
                f.name,
                (self.unit)(f.value),
                w,
                f.count
            ),
            kind: f.kind,
            path: path.clone(),
            labeled: w >= 4.0,
        });
        let mut kids: Vec<&Frame> = f.children.values().collect();
        kids.sort_by(|a, b| b.value.cmp(&a.value).then(a.name.cmp(&b.name)));
        // Children sit under the parent left to right; the parent's own time is the rest.
        let mut cx = x;
        for c in kids {
            let p = if path.is_empty() {
                c.name.clone()
            } else {
                format!("{path};{}", c.name)
            };
            self.walk(c, cx, y + 1, p);
            cx += c.value as f64 / self.total * 100.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_time_is_the_union_of_intervals() {
        assert_eq!(covered(vec![(0, 10), (5, 15), (20, 25)], 0, 100), 20);
        assert_eq!(covered(vec![(0, 10), (5, 15)], 8, 12), 4);
        assert_eq!(covered(vec![], 0, 10), 0);
    }

    #[test]
    fn an_icicle_adds_up() {
        let mut f = Frame::default();
        f.add(&[("a".into(), "method"), ("b".into(), "sql")], 10);
        f.add(&[("a".into(), "method")], 5);
        f.add(&[], 1);
        assert_eq!(f.value, 16);
        assert_eq!(f.at("a").value, 15);
        assert_eq!(f.at("a;b").value, 10);
        let (cells, depth) = cells(&f, "", |v| v.to_string());
        assert_eq!(depth, 3);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[1].w, "93.700%");
    }
}
