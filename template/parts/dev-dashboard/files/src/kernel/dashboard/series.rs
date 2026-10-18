//! The metric time series in memory: one `Sample` per second for 15 minutes and one per minute for
//! the last two hours, in fixed-size rings. Nothing grows with traffic or uptime.
//!
//! WHY only two hours of minutes here: older history lives on disk (`history.rs`, compressed, and
//! it survives restarts). Memory keeps what the live pages redraw every second or ten (the 5 min,
//! 15 min and 1 h ranges, the overview's sparklines, the insights) so they never wait on a read.

use std::collections::VecDeque;

use serde::Serialize;

use super::hist::Hist;

/// 15 minutes at 1 s.
pub const SECOND_POINTS: usize = 15 * 60;
/// The last two hours at 1 min (the 1 h range plus slack; everything older is on disk).
pub const MINUTE_POINTS: usize = 2 * 60;

/// Everything the collector measures in one interval. Rates are per second; latencies are µs.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct Sample {
    /// Unix seconds at the end of the interval.
    pub t: i64,
    /// Resident memory, bytes.
    pub rss: u64,
    /// Virtual memory, bytes.
    pub virt: u64,
    /// Process CPU, percent of one core (200 = two cores busy).
    pub cpu: f32,
    /// Open file descriptors (`None` where the OS doesn't say).
    pub fds: Option<u32>,
    pub workers: u32,
    pub tasks: u32,
    pub queue: u32,
    /// Tokio workers busy, percent of the interval.
    pub busy: f32,
    pub pool_size: u32,
    pub pool_idle: u32,
    pub pool_max: u32,
    pub req: f32,
    pub e4: f32,
    pub e5: f32,
    pub p50: u32,
    pub p95: u32,
    pub p99: u32,
    pub sql: f32,
    pub sql_p95: u32,
    /// Requests per latency bucket (`LATENCY_BUCKETS`), for the heatmap.
    pub lat: [u16; LATENCY_BUCKETS.len() + 1],
}

/// The heatmap's latency buckets: upper bounds in µs (the last bucket is everything slower).
pub const LATENCY_BUCKETS: [u64; 10] = [
    1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000,
];

/// The buckets' labels, in order (the last is open-ended).
pub const LATENCY_LABELS: [&str; 11] = [
    "≤1ms", "≤2.5ms", "≤5ms", "≤10ms", "≤25ms", "≤50ms", "≤100ms", "≤250ms", "≤500ms", "≤1s", ">1s",
];

/// A marker on the charts: a boot, a deploy, a load-lab run, an error burst.
#[derive(Clone, Debug, Serialize)]
pub struct Annotation {
    pub t: i64,
    /// `boot`, `deploy`, `lab`, `errors`, `spike`.
    pub kind: &'static str,
    pub label: String,
    /// Part of the demo's synthetic history.
    pub demo: bool,
}

/// Annotations kept.
pub const ANNOTATIONS: usize = 200;

impl Sample {
    pub fn pool_in_use(&self) -> u32 {
        self.pool_size.saturating_sub(self.pool_idle)
    }
}

/// The view's time range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    M5,
    M15,
    H1,
    H24,
    D7,
    D30,
}

impl Range {
    pub const ALL: [Range; 6] = [
        Range::M5,
        Range::M15,
        Range::H1,
        Range::H24,
        Range::D7,
        Range::D30,
    ];

    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("5m") => Range::M5,
            Some("1h") => Range::H1,
            Some("24h") => Range::H24,
            Some("7d") => Range::D7,
            Some("30d") => Range::D30,
            _ => Range::M15,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Range::M5 => "5m",
            Range::M15 => "15m",
            Range::H1 => "1h",
            Range::H24 => "24h",
            Range::D7 => "7d",
            Range::D30 => "30d",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::M5 => "5 min",
            Range::M15 => "15 min",
            Range::H1 => "1 hour",
            Range::H24 => "24 hours",
            Range::D7 => "7 days",
            Range::D30 => "30 days",
        }
    }

    pub fn seconds(self) -> i64 {
        match self {
            Range::M5 => 300,
            Range::M15 => 900,
            Range::H1 => 3_600,
            Range::H24 => 86_400,
            Range::D7 => 7 * 86_400,
            Range::D30 => 30 * 86_400,
        }
    }

    /// Seconds per point read (charts then draw at most 300 of them).
    pub fn step(self) -> i64 {
        match self {
            Range::M5 | Range::M15 => 1,
            Range::H1 | Range::H24 | Range::D7 => 60,
            Range::D30 => 3_600,
        }
    }

    /// How often a live view of this range re-renders, in seconds.
    pub fn refresh(self) -> u64 {
        match self {
            Range::M5 | Range::M15 => 1,
            Range::H1 => 10,
            Range::H24 => 60,
            Range::D7 | Range::D30 => 300,
        }
    }
}

/// A closed minute with its latencies, on its way to the history file.
pub struct Minute {
    pub sample: Sample,
    pub hist: Hist,
    pub sql_hist: Hist,
}

/// The rings, plus the minute being accumulated.
pub struct Series {
    annotations: VecDeque<Annotation>,
    /// Until when (unix seconds) the rings hold the demo's synthetic history.
    pub demo_until: Option<i64>,
    seconds: VecDeque<Sample>,
    minutes: VecDeque<Sample>,
    minute: Vec<Sample>,
    minute_hist: Hist,
    minute_sql_hist: Hist,
}

impl Default for Series {
    fn default() -> Self {
        Self {
            annotations: VecDeque::new(),
            demo_until: None,
            seconds: VecDeque::with_capacity(SECOND_POINTS),
            minutes: VecDeque::with_capacity(MINUTE_POINTS),
            minute: Vec::with_capacity(60),
            minute_hist: Hist::default(),
            minute_sql_hist: Hist::default(),
        }
    }
}

/// One point for a run of samples: averages for rates and gauges, the largest queue, the last fd
/// count and pool size, percentiles from the merged histograms (never an average of
/// percentiles), and the heatmap's counts summed. Minutes roll up seconds; hours roll up minutes.
pub fn rollup(samples: &[Sample], hist: &Hist, sql_hist: &Hist, t: i64) -> Option<Sample> {
    let last = samples.last().copied()?;
    let n = samples.len() as f32;
    let avg = |f: fn(&Sample) -> f32| samples.iter().map(f).sum::<f32>() / n;
    let avg_u =
        |f: fn(&Sample) -> u64| (samples.iter().map(f).sum::<u64>() as f64 / f64::from(n)) as u64;
    Some(Sample {
        t,
        rss: avg_u(|s| s.rss),
        virt: avg_u(|s| s.virt),
        cpu: avg(|s| s.cpu),
        fds: last.fds,
        workers: last.workers,
        tasks: avg_u(|s| u64::from(s.tasks)) as u32,
        queue: samples.iter().map(|s| s.queue).max().unwrap_or(0),
        busy: avg(|s| s.busy),
        pool_size: avg_u(|s| u64::from(s.pool_size)) as u32,
        pool_idle: avg_u(|s| u64::from(s.pool_idle)) as u32,
        pool_max: last.pool_max,
        req: avg(|s| s.req),
        e4: avg(|s| s.e4),
        e5: avg(|s| s.e5),
        p50: hist.quantile(0.50) as u32,
        p95: hist.quantile(0.95) as u32,
        p99: hist.quantile(0.99) as u32,
        sql: avg(|s| s.sql),
        sql_p95: sql_hist.quantile(0.95) as u32,
        lat: {
            let mut sum = [0_u16; LATENCY_BUCKETS.len() + 1];
            for s in samples {
                for (a, b) in sum.iter_mut().zip(s.lat.iter()) {
                    *a = a.saturating_add(*b);
                }
            }
            sum
        },
    })
}

impl Series {
    /// Adds one second. `hist`/`sql_hist` are that second's latencies, folded into the minute.
    /// Returns the minute this second closed, for the history file.
    pub fn push(&mut self, s: Sample, hist: &Hist, sql_hist: &Hist) -> Option<Minute> {
        // INVARIANT: the rings never exceed their size (test: the_dashboard_stays_within_its_memory_bound).
        if self.seconds.len() == SECOND_POINTS {
            self.seconds.pop_front();
        }
        self.seconds.push_back(s);
        let mut closed = None;
        if let Some(first) = self.minute.first()
            && first.t.div_euclid(60) != s.t.div_euclid(60)
        {
            closed = self.close_minute();
        }
        self.minute.push(s);
        self.minute_hist.merge(hist);
        self.minute_sql_hist.merge(sql_hist);
        closed
    }

    fn close_minute(&mut self) -> Option<Minute> {
        let t = self.minute.last()?.t.div_euclid(60) * 60 + 59;
        let m = rollup(&self.minute, &self.minute_hist, &self.minute_sql_hist, t)?;
        if self.minutes.len() == MINUTE_POINTS {
            self.minutes.pop_front();
        }
        self.minutes.push_back(m);
        self.minute.clear();
        Some(Minute {
            sample: m,
            hist: std::mem::take(&mut self.minute_hist),
            sql_hist: std::mem::take(&mut self.minute_sql_hist),
        })
    }

    /// The minute in progress so far (shutdown writes it, so a clean stop loses nothing).
    pub fn partial_minute(&self) -> Option<Minute> {
        let t = self.minute.last()?.t.div_euclid(60) * 60 + 59;
        Some(Minute {
            sample: rollup(&self.minute, &self.minute_hist, &self.minute_sql_hist, t)?,
            hist: self.minute_hist.clone(),
            sql_hist: self.minute_sql_hist.clone(),
        })
    }

    pub fn annotate(&mut self, a: Annotation) {
        // INVARIANT: bounded (test: the_dashboard_stays_within_its_memory_bound).
        if self.annotations.len() == ANNOTATIONS {
            self.annotations.pop_front();
        }
        self.annotations.push_back(a);
    }

    /// Annotations between `from` and `to` (unix seconds) that this process made.
    pub fn annotations(&self, from: i64, to: i64) -> Vec<Annotation> {
        self.annotations
            .iter()
            .filter(|a| a.t >= from && a.t <= to)
            .cloned()
            .collect()
    }

    /// The samples from `from` to `to` (unix seconds) that memory holds: at 1 s when the seconds
    /// ring covers them, else at 1 min (with the minute in progress). `history::between` adds what
    /// only the disk has.
    pub fn between(&self, from: i64, to: i64) -> (Vec<Sample>, i64) {
        let covered = self.seconds.front().is_some_and(|s| s.t <= from);
        if covered || to - from <= 120 {
            (
                self.seconds
                    .iter()
                    .filter(|s| s.t > from && s.t <= to)
                    .copied()
                    .collect(),
                1,
            )
        } else {
            (self.minutes_between(from, to), 60)
        }
    }

    /// The per-minute samples memory holds in `from..=to`, with the minute in progress.
    pub fn minutes_between(&self, from: i64, to: i64) -> Vec<Sample> {
        let mut out: Vec<Sample> = self
            .minutes
            .iter()
            .filter(|s| s.t > from && s.t <= to)
            .copied()
            .collect();
        if let Some(last) = self.minute.last()
            && last.t > from
            && last.t <= to
            && out.last().is_none_or(|m| m.t < last.t)
        {
            out.push(*last);
        }
        out
    }

    /// The oldest minute memory holds (older ones are only on disk).
    pub fn oldest_minute(&self) -> Option<i64> {
        self.minutes.front().or(self.minute.first()).map(|s| s.t)
    }

    pub fn latest(&self) -> Option<Sample> {
        self.seconds.back().copied()
    }

    /// The samples of `range` that memory holds, oldest first (complete for 5 min, 15 min and
    /// 1 h; `history::window` reads the longer ranges from disk).
    pub fn window(&self, range: Range, now: i64) -> Vec<Sample> {
        let since = now - range.seconds();
        if range.step() == 1 {
            return self
                .seconds
                .iter()
                .filter(|s| s.t > since)
                .copied()
                .collect();
        }
        // The minute in progress, so a fresh process shows something on the long ranges too.
        self.minutes_between(since, i64::MAX)
    }

    pub fn lens(&self) -> (usize, usize) {
        (self.seconds.len(), self.minutes.len())
    }

    pub const SAMPLE_BYTES: usize = std::mem::size_of::<Sample>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_roll_up_into_minutes_and_rings_stay_bounded() {
        let mut s = Series::default();
        let h = Hist::default();
        for t in 0..(SECOND_POINTS as i64 * 3) {
            s.push(
                Sample {
                    t,
                    req: 2.0,
                    rss: 100,
                    ..Sample::default()
                },
                &h,
                &h,
            );
        }
        let (secs, mins) = s.lens();
        assert_eq!(secs, SECOND_POINTS);
        assert_eq!(mins, MINUTE_POINTS.min(SECOND_POINTS * 3 / 60 - 1));
        let w = s.window(Range::H1, SECOND_POINTS as i64 * 3);
        assert!(
            w.iter()
                .all(|m| (m.req - 2.0).abs() < f32::EPSILON && m.rss == 100)
        );
        assert_eq!(Range::parse(Some("24h")), Range::H24);
        assert_eq!(Range::parse(Some("nonsense")), Range::M15);
    }
}
