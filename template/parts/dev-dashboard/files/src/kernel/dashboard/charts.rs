//! Charts as server-rendered SVG: this module computes the geometry, `_chart.html` draws it.
//!
//! WHY server-rendered SVG and not a vendored chart library: one implementation in Rust (tested
//! here, no license or checksum to track), it works without JavaScript, the strict CSP needs
//! nothing (SVG attributes aren't styles), and every chart ships its own data table as the text
//! alternative. Live updates re-render the fragment on the server and `dashboard.js` swaps it in;
//! the same script adds the hover readout from the `data-` attributes.

use super::series::{Annotation, LATENCY_BUCKETS, LATENCY_LABELS, Sample};
use super::web::{bytes, micros};

/// The plot area inside the SVG's viewBox.
pub const W: u32 = 640;
pub const H: u32 = 200;
const LEFT: f64 = 52.0;
const RIGHT: f64 = 8.0;
const TOP: f64 = 10.0;
const BOTTOM: f64 = 22.0;

/// How a chart's numbers read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Percent,
    PerSecond,
    Micros,
    Count,
}

impl Unit {
    pub fn format(self, v: f64) -> String {
        match self {
            Unit::Bytes => bytes(v.max(0.0) as u64),
            Unit::Percent => format!("{v:.0}%"),
            Unit::PerSecond => {
                if v >= 10.0 || v == 0.0 {
                    format!("{v:.0}/s")
                } else {
                    format!("{v:.1}/s")
                }
            }
            Unit::Micros => micros(v.max(0.0) as u64),
            Unit::Count => format!("{v:.0}"),
        }
    }
}

pub struct Tick {
    pub pos: String,
    pub label: String,
}

pub struct Line {
    pub name: &'static str,
    /// CSS class: `s1`…`s4` (colors per theme in dashboard.css).
    pub class: &'static str,
    pub d: String,
    pub area: String,
    /// The window's summary for the legend: average and peak for rates, the request-weighted
    /// percentile for latency, now and peak for gauges; "—" without data.
    pub summary: String,
    /// Comma-separated values for the readout.
    pub values: String,
}

/// A vertical marker: a boot, a deploy, a load-lab run, an error burst.
pub struct Mark {
    pub x: String,
    pub kind: &'static str,
    pub label: String,
    pub when: String,
}

/// Everything `_chart.html` needs.
pub struct ChartView {
    pub key: &'static str,
    pub title: &'static str,
    pub summary: String,
    pub w: u32,
    pub h: u32,
    pub x0: String,
    pub x1: String,
    pub y0: String,
    pub y1: String,
    pub grid: Vec<Tick>,
    pub xticks: Vec<Tick>,
    pub lines: Vec<Line>,
    pub marks: Vec<Mark>,
    pub table_head: Vec<&'static str>,
    pub table: Vec<Vec<String>>,
    pub empty: bool,
    /// The window drawn (unix seconds), and the time of each point (for the readout and zoom).
    pub from: i64,
    pub to: i64,
    pub times: String,
    pub unit: &'static str,
    /// Some of it is the demo's synthetic history.
    pub demo: bool,
    /// A click on a moment opens the traces of that minute.
    pub drill: bool,
}

/// A series to plot: name and one value per sample.
pub struct SeriesSpec {
    pub name: &'static str,
    pub value: fn(&Sample) -> f64,
}

/// What the charts of a page draw: a time window with its samples and markers.
pub struct Window {
    pub from: i64,
    pub to: i64,
    pub samples: Vec<Sample>,
    pub marks: Vec<Annotation>,
    pub demo: bool,
}

/// 1, 2, 2.5 or 5 × 10^k at or above `v`.
pub fn nice_ceiling(v: f64) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 1.0;
    }
    let mag = 10_f64.powf(v.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if v <= m * mag {
            return m * mag;
        }
    }
    10.0 * mag
}

/// At most `max` points: rates and gauges keep each bucket's largest value (spikes survive),
/// latency buckets are summed for the heatmap.
pub fn downsample(samples: &[Sample], max: usize) -> Vec<Sample> {
    if samples.len() <= max || max == 0 {
        return samples.to_vec();
    }
    let per = samples.len().div_ceil(max);
    samples
        .chunks(per)
        .filter_map(|c| {
            let mut s = *c.last()?;
            s.rss = c.iter().map(|x| x.rss).max().unwrap_or(s.rss);
            s.cpu = c.iter().map(|x| x.cpu).fold(0.0, f32::max);
            s.req = c.iter().map(|x| x.req).fold(0.0, f32::max);
            s.e4 = c.iter().map(|x| x.e4).fold(0.0, f32::max);
            s.e5 = c.iter().map(|x| x.e5).fold(0.0, f32::max);
            s.p50 = c.iter().map(|x| x.p50).max().unwrap_or(s.p50);
            s.p95 = c.iter().map(|x| x.p95).max().unwrap_or(s.p95);
            s.p99 = c.iter().map(|x| x.p99).max().unwrap_or(s.p99);
            s.sql = c.iter().map(|x| x.sql).fold(0.0, f32::max);
            s.tasks = c.iter().map(|x| x.tasks).max().unwrap_or(s.tasks);
            let mut lat = [0_u16; LATENCY_BUCKETS.len() + 1];
            for x in c {
                for (a, b) in lat.iter_mut().zip(x.lat.iter()) {
                    *a = a.saturating_add(*b);
                }
            }
            s.lat = lat;
            Some(s)
        })
        .collect()
}

/// "now", "-12s", "-5m", "-3h" relative to `to`.
fn ago(secs: i64) -> String {
    match secs {
        0 => "now".to_owned(),
        s if s < 120 => format!("-{s}s"),
        s if s < 3_600 => format!("-{}m", s / 60),
        s => format!("-{}h", s / 3_600),
    }
}

/// "14:05" or "14:05:30" (UTC) for a zoomed window, which isn't "now"-relative.
fn clock(t: i64, seconds: bool) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| {
            d.format(if seconds { "%H:%M:%S" } else { "%H:%M" })
                .to_string()
        })
        .unwrap_or_default()
}

fn label_at(t: i64, w: &Window, live: bool) -> String {
    if live {
        ago(w.to - t)
    } else {
        clock(t, w.to - w.from < 600)
    }
}

/// The legend text of one series over the window.
fn legend(unit: Unit, samples: &[Sample], value: fn(&Sample) -> f64) -> String {
    if samples.is_empty() {
        return "—".to_owned();
    }
    let peak = samples.iter().map(value).fold(0.0, f64::max);
    match unit {
        Unit::PerSecond => {
            let avg = samples.iter().map(value).sum::<f64>() / samples.len() as f64;
            format!("avg {} · peak {}", unit.format(avg), unit.format(peak))
        }
        Unit::Micros => {
            // Weighted by each moment's requests: a quiet second doesn't count as fast.
            let (sum, weight) =
                samples
                    .iter()
                    .filter(|s| value(s) > 0.0)
                    .fold((0.0, 0.0), |(a, n), s| {
                        let w = f64::from(s.req.max(0.01));
                        (a + value(s) * w, n + w)
                    });
            if weight == 0.0 {
                "—".to_owned()
            } else {
                format!("{} · peak {}", unit.format(sum / weight), unit.format(peak))
            }
        }
        _ => {
            let now = samples.last().map_or(0.0, value);
            format!("now {} · peak {}", unit.format(now), unit.format(peak))
        }
    }
}

/// A line chart of `series` over the window `w`. `live` windows end now and label time relative
/// to it; zoomed ones label the clock.
pub fn line_chart(
    key: &'static str,
    title: &'static str,
    unit: Unit,
    w: &Window,
    series: &[SeriesSpec],
    live: bool,
) -> ChartView {
    let samples = &w.samples;
    let (pw, ph) = (f64::from(W) - LEFT - RIGHT, f64::from(H) - TOP - BOTTOM);
    let max = samples
        .iter()
        .flat_map(|s| series.iter().map(move |spec| (spec.value)(s)))
        .fold(0.0, f64::max);
    // Bytes get binary steps (10 MiB, not 9.5 MiB).
    let ymax = if unit == Unit::Bytes {
        let mib = 1024.0 * 1024.0;
        nice_ceiling(max / mib) * mib
    } else {
        nice_ceiling(max)
    };
    let span = (w.to - w.from).max(1);
    let x = |t: i64| LEFT + ((t - w.from).clamp(0, span) as f64 / span as f64) * pw;
    let y = |v: f64| TOP + ph - (v / ymax).clamp(0.0, 1.0) * ph;
    let grid = (0..=4)
        .map(|i| {
            let v = ymax * f64::from(i) / 4.0;
            Tick {
                pos: format!("{:.1}", y(v)),
                label: unit.format(v),
            }
        })
        .collect();
    let xticks = (0..=4)
        .map(|i| {
            let t = w.from + span * i / 4;
            Tick {
                pos: format!("{:.1}", x(t)),
                label: label_at(t, w, live),
            }
        })
        .collect();
    let classes = palette(key);
    let lines = series
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let mut d = String::new();
            for (n, s) in samples.iter().enumerate() {
                let cmd = if n == 0 { 'M' } else { 'L' };
                d.push_str(&format!("{cmd}{:.1},{:.1}", x(s.t), y((spec.value)(s))));
            }
            let area = match (samples.first(), samples.last()) {
                (Some(a), Some(b)) => format!(
                    "{d}L{:.1},{:.1}L{:.1},{:.1}Z",
                    x(b.t),
                    TOP + ph,
                    x(a.t),
                    TOP + ph
                ),
                _ => String::new(),
            };
            Line {
                name: spec.name,
                class: classes[i % classes.len()],
                d,
                area,
                summary: legend(unit, samples, spec.value),
                values: samples
                    .iter()
                    .map(|s| format!("{:.2}", (spec.value)(s)))
                    .collect::<Vec<_>>()
                    .join(","),
            }
        })
        .collect::<Vec<_>>();
    let marks = w
        .marks
        .iter()
        .filter(|m| m.t >= w.from && m.t <= w.to)
        .map(|m| Mark {
            x: format!("{:.1}", x(m.t)),
            kind: m.kind,
            label: m.label.clone(),
            when: label_at(m.t, w, live),
        })
        .collect();
    // The text alternative: about 12 rows across the window, newest first.
    let every = samples.len().div_ceil(12).max(1);
    let table = samples
        .iter()
        .rev()
        .step_by(every)
        .take(12)
        .map(|s| {
            let mut row = vec![label_at(s.t, w, live)];
            row.extend(series.iter().map(|spec| unit.format((spec.value)(s))));
            row
        })
        .collect();
    let mut table_head = vec!["When"];
    table_head.extend(series.iter().map(|s| s.name));
    let summary = if samples.is_empty() {
        format!("{title}: no data yet")
    } else {
        let parts: Vec<String> = lines
            .iter()
            .map(|l| format!("{} {}", l.name, l.summary))
            .collect();
        format!("{title}: {}", parts.join("; "))
    };
    ChartView {
        key,
        title,
        summary,
        w: W,
        h: H,
        x0: format!("{LEFT:.1}"),
        x1: format!("{:.1}", f64::from(W) - RIGHT),
        y0: format!("{TOP:.1}"),
        y1: format!("{:.1}", TOP + ph),
        grid,
        xticks,
        lines,
        marks,
        table_head,
        table,
        empty: samples.is_empty(),
        from: w.from,
        to: w.to,
        times: samples
            .iter()
            .map(|s| s.t.to_string())
            .collect::<Vec<_>>()
            .join(","),
        unit: match unit {
            Unit::Bytes => "bytes",
            Unit::Percent => "percent",
            Unit::PerSecond => "rate",
            Unit::Micros => "micros",
            Unit::Count => "count",
        },
        demo: w.demo,
        drill: matches!(key, "req" | "latency" | "errors"),
    }
}

/// One cell of the latency heatmap.
pub struct Cell {
    pub x: String,
    pub y: String,
    pub w: String,
    pub h: String,
    /// 1..=6, the share of that moment's requests (0 is not drawn).
    pub level: u8,
    pub title: String,
}

/// The latency heatmap: time × latency bucket, each cell's shade the share of that moment's
/// requests, under the percentile lines.
pub struct Heatmap {
    pub cells: Vec<Cell>,
    pub rows: Vec<Tick>,
    pub xticks: Vec<Tick>,
    pub table_head: Vec<&'static str>,
    pub table: Vec<Vec<String>>,
    pub empty: bool,
    pub demo: bool,
}

pub fn heatmap(w: &Window, live: bool) -> Heatmap {
    let cols = downsample(&w.samples, 60);
    let buckets = LATENCY_BUCKETS.len() + 1;
    let (pw, ph) = (f64::from(W) - LEFT - RIGHT, 132.0);
    let cw = pw / cols.len().max(1) as f64;
    let rh = ph / buckets as f64;
    let mut cells = Vec::new();
    for (i, s) in cols.iter().enumerate() {
        let total: u32 = s.lat.iter().map(|c| u32::from(*c)).sum();
        if total == 0 {
            continue;
        }
        for (b, c) in s.lat.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let share = f64::from(*c) / f64::from(total);
            let level = ((share.sqrt() * 6.0).ceil() as u8).clamp(1, 6);
            cells.push(Cell {
                x: format!("{:.1}", LEFT + i as f64 * cw),
                y: format!("{:.1}", TOP + ph - (b + 1) as f64 * rh),
                w: format!("{:.1}", (cw - 1.0).max(1.0)),
                h: format!("{:.1}", rh - 1.0),
                level,
                title: format!(
                    "{}: {} of {} requests {}",
                    label_at(s.t, w, live),
                    c,
                    total,
                    LATENCY_LABELS[b]
                ),
            });
        }
    }
    let rows = [0, 2, 4, 6, 8, 10]
        .iter()
        .map(|b| Tick {
            pos: format!("{:.1}", TOP + ph - (*b as f64 + 0.5) * rh + 3.0),
            label: LATENCY_LABELS[*b].to_owned(),
        })
        .collect();
    let span = (w.to - w.from).max(1);
    let xticks = (0..=4)
        .map(|i| {
            let t = w.from + span * i / 4;
            Tick {
                pos: format!("{:.1}", LEFT + (span * i / 4) as f64 / span as f64 * pw),
                label: label_at(t, w, live),
            }
        })
        .collect();
    let mut table_head = vec!["When"];
    table_head.extend(LATENCY_LABELS);
    let every = cols.len().div_ceil(10).max(1);
    let table = cols
        .iter()
        .rev()
        .step_by(every)
        .take(10)
        .map(|s| {
            let mut row = vec![label_at(s.t, w, live)];
            row.extend(s.lat.iter().map(|c| c.to_string()));
            row
        })
        .collect();
    Heatmap {
        empty: cells.is_empty(),
        cells,
        rows,
        xticks,
        table_head,
        table,
        demo: w.demo,
    }
}

/// Series colors per chart: errors are amber and red, everything else starts with the accent.
fn palette(key: &str) -> [&'static str; 4] {
    match key {
        "errors" => ["s3", "s4", "s1", "s2"],
        "latency" => ["s2", "s1", "s4", "s3"],
        _ => ["s1", "s2", "s3", "s4"],
    }
}

/// A sparkline for an overview card (viewBox 0 0 120 32).
pub struct Spark {
    pub d: String,
    pub area: String,
}

pub fn sparkline(values: &[f64]) -> Spark {
    let max = nice_ceiling(values.iter().copied().fold(0.0, f64::max));
    let n = values.len().max(2) - 1;
    let mut d = String::new();
    for (i, v) in values.iter().enumerate() {
        let x = i as f64 * 120.0 / n as f64;
        let y = 30.0 - (v / max).clamp(0.0, 1.0) * 28.0;
        d.push_str(&format!("{}{x:.1},{y:.1}", if i == 0 { 'M' } else { 'L' }));
    }
    let area = if values.is_empty() {
        String::new()
    } else {
        format!(
            "{d}L{:.1},32L0,32Z",
            (values.len() - 1) as f64 * 120.0 / n as f64
        )
    };
    Spark { d, area }
}

/// A bar chart of values (oldest first) in a 0 0 W H viewBox: (x, y, width, height, class).
pub struct Bar {
    pub x: String,
    pub y: String,
    pub w: String,
    pub h: String,
    pub class: &'static str,
    pub label: String,
}

/// Bars for a trend: each value with its class (`ok`, `bad`, `warn`), scaled to the largest.
pub fn bars(values: &[(f64, &'static str, String)], w: f64, h: f64) -> Vec<Bar> {
    let max = nice_ceiling(values.iter().map(|v| v.0).fold(0.0, f64::max));
    let n = values.len().max(1) as f64;
    let slot = w / n;
    values
        .iter()
        .enumerate()
        .map(|(i, (v, class, label))| {
            let bh = (v / max).clamp(0.0, 1.0) * (h - 2.0);
            Bar {
                x: format!("{:.1}", i as f64 * slot + slot * 0.15),
                y: format!("{:.1}", h - bh),
                w: format!("{:.1}", slot * 0.7),
                h: format!("{:.1}", bh.max(1.0)),
                class,
                label: label.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_round_up_to_nice_numbers() {
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(7.0), 10.0);
        assert_eq!(nice_ceiling(180.0), 200.0);
        assert_eq!(nice_ceiling(2_100.0), 2_500.0);
        assert_eq!(nice_ceiling(42_000_000.0), 50_000_000.0);
    }

    #[test]
    fn a_chart_has_a_text_alternative_and_stays_in_its_box() {
        let samples: Vec<Sample> = (0..60)
            .map(|t| Sample {
                t: 1_000 + t,
                req: (t % 7) as f32,
                ..Sample::default()
            })
            .collect();
        let w = Window {
            from: 1_000,
            to: 1_060,
            samples: samples.clone(),
            marks: vec![Annotation {
                t: 1_030,
                kind: "boot",
                label: "server started".into(),
                demo: false,
            }],
            demo: false,
        };
        let c = line_chart(
            "req",
            "Requests",
            Unit::PerSecond,
            &w,
            &[SeriesSpec {
                name: "req/s",
                value: |s| f64::from(s.req),
            }],
            true,
        );
        assert!(!c.empty);
        assert!(
            c.summary.contains("req/s avg 2.9/s · peak 6.0/s"),
            "{}",
            c.summary
        );
        assert_eq!(c.marks.len(), 1);
        assert!(c.table.len() <= 12 && !c.table.is_empty());
        assert_eq!(c.table_head, ["When", "req/s"]);
        for num in c.lines[0]
            .d
            .split(['M', 'L', ','])
            .filter(|s| !s.is_empty())
        {
            let v: f64 = num.parse().unwrap();
            assert!((0.0..=f64::from(W)).contains(&v), "{v}");
        }
        assert_eq!(downsample(&samples, 10).len(), 10);
        let empty = Window {
            samples: Vec::new(),
            ..w
        };
        let quiet = line_chart(
            "latency",
            "Latency",
            Unit::Micros,
            &empty,
            &[SeriesSpec {
                name: "p95",
                value: |s| f64::from(s.p95),
            }],
            true,
        );
        assert_eq!(
            quiet.lines[0].summary, "—",
            "no data reads as a dash, never 0µs"
        );
    }

    #[test]
    fn the_latency_heatmap_shades_each_moment() {
        let mut s = Sample {
            t: 10,
            req: 4.0,
            ..Sample::default()
        };
        s.lat[0] = 3;
        s.lat[7] = 1;
        let w = Window {
            from: 0,
            to: 20,
            samples: vec![s],
            marks: Vec::new(),
            demo: true,
        };
        let h = heatmap(&w, true);
        assert_eq!(h.cells.len(), 2);
        assert!(h.cells[0].level > h.cells[1].level);
        assert!(h.demo && !h.empty);
        assert_eq!(h.table[0][1], "3");
    }
}
