//! The demo's synthetic history (`DASHBOARD_DEMO_HISTORY=true`, development only): the last hour,
//! filled in at boot so every range has full charts in a demo or a screenshot. The shape is
//! believable: traffic that rises and falls, a deploy that restarts memory, a traffic spike, and an
//! incident with errors and slow requests. Charts over it carry a "demo data" badge, and the real
//! collector takes over from the first second.

use std::f64::consts::PI;

use super::hist::Hist;
use super::series::{Annotation, LATENCY_BUCKETS, Sample, Series};

/// Seconds of history.
pub const SPAN: i64 = 3_600;

/// A small deterministic noise in -1.0..1.0 (no RNG: the same demo every time).
fn noise(t: i64, salt: u64) -> f64 {
    let mut x =
        (t as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 29;
    (x % 2_001) as f64 / 1_000.0 - 1.0
}

/// Fills the rings with the hour before `now`, and marks it as demo data.
pub fn backfill(series: &mut Series, now: i64) {
    let start = now - SPAN;
    let deploy = start + 1_150;
    let spike = (start + 2_050, start + 2_140);
    let incident = (start + 2_900, start + 3_040);
    for t in start..now {
        let x = (t - start) as f64 / SPAN as f64;
        // Traffic: a slow swell over the hour (a day, compressed), with jitter.
        let mut req = 9.0 + 7.0 * (2.0 * PI * (x - 0.25)).sin().max(-0.6) + 2.0 * noise(t, 1);
        if (spike.0..spike.1).contains(&t) {
            req *= 3.4;
        }
        let req = req.max(0.5);
        let in_incident = (incident.0..incident.1).contains(&t);
        let e5 = if in_incident {
            2.5 + 1.5 * noise(t, 2).abs() * 2.0
        } else {
            0.0
        };
        let e4 = (0.15 + 0.15 * noise(t, 3)).max(0.0) * req / 10.0;
        // Latency: a lognormal-ish spread around a few ms; slower under the spike and the incident.
        let slow = if in_incident {
            9.0
        } else if (spike.0..spike.1).contains(&t) {
            2.2
        } else {
            1.0
        };
        let mut hist = Hist::default();
        let n = req.round().max(1.0) as u64;
        for i in 0..n {
            let r = noise(t, 10 + i).abs();
            let base = 900.0 + 6_000.0 * r * r * r + 30_000.0 * (r.powi(12));
            hist.record((base * slow) as u64);
        }
        let mut sql_hist = Hist::default();
        sql_hist.record((180.0 + 400.0 * noise(t, 4).abs() * slow) as u64);
        // Memory climbs slowly, and the deploy restarts it.
        let since = if t >= deploy {
            t - deploy
        } else {
            t - start + 1_400
        };
        let rss = 48.0
            + 22.0 * (1.0 - (-(since as f64) / 600.0).exp())
            + 0.002 * since as f64
            + 0.6 * noise(t, 5);
        let cpu =
            (req * 0.9 + 3.0 + 2.0 * noise(t, 6)).max(0.5) * if in_incident { 1.4 } else { 1.0 };
        let in_use = ((req / 6.0) * slow.min(4.0) + noise(t, 7))
            .round()
            .clamp(0.0, 10.0) as u32;
        let s = Sample {
            t,
            rss: (rss * 1024.0 * 1024.0) as u64,
            virt: 0,
            cpu: cpu as f32,
            fds: Some(24 + in_use),
            workers: 8,
            tasks: (12.0 + req * 0.8 + 2.0 * noise(t, 8)).max(6.0) as u32,
            queue: u32::from(noise(t, 9) > 0.92),
            busy: (cpu / 8.0) as f32,
            pool_size: 10_u32.min(in_use + 3),
            pool_idle: 3,
            pool_max: 10,
            req: req as f32,
            e4: e4 as f32,
            e5: e5 as f32,
            p50: hist.quantile(0.50) as u32,
            p95: hist.quantile(0.95) as u32,
            p99: hist.quantile(0.99) as u32,
            sql: (req * 3.1) as f32,
            sql_p95: sql_hist.quantile(0.95) as u32,
            lat: hist.bucket_counts(&LATENCY_BUCKETS),
        };
        series.push(s, &hist, &sql_hist);
    }
    for (t, kind, label) in [
        (deploy, "deploy", "deploy v1.4.0 (demo data)"),
        (
            spike.0,
            "spike",
            "traffic spike: a newsletter went out (demo data)",
        ),
        (
            incident.0,
            "errors",
            "incident: payment provider timeouts (demo data)",
        ),
    ] {
        series.annotate(Annotation {
            t,
            kind,
            label: label.to_owned(),
            demo: true,
        });
    }
    series.demo_until = Some(now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::dashboard::series::Range;

    #[test]
    fn the_demo_history_fills_every_range_and_is_marked() {
        let mut s = Series::default();
        backfill(&mut s, 10_000_000);
        assert_eq!(s.window(Range::M5, 10_000_000).len(), 299);
        assert!(s.window(Range::H1, 10_000_000).len() >= 59);
        assert_eq!(s.demo_until, Some(10_000_000));
        let marks = s.annotations(0, i64::MAX);
        assert_eq!(marks.len(), 3);
        assert!(
            marks
                .iter()
                .all(|a| a.demo && a.label.contains("demo data"))
        );
        let w = s.window(Range::H1, 10_000_000);
        assert!(w.iter().any(|m| m.e5 > 1.0), "the incident shows");
        assert!(
            w.iter().map(|m| m.req).fold(0.0, f32::max) > 20.0,
            "the spike shows"
        );
    }
}
