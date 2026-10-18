//! A small log-bucketed latency histogram in microseconds: exact below 8 µs, then 8 buckets per
//! power of two (at most 9% error), up to about 19 hours. 1.1 KiB each, so every route and every
//! SQL fingerprint can keep one inside the dashboard's memory bound.
//!
//! WHY: hdrhistogram at 2 significant figures over 1 µs..60 s is ~26 KiB per histogram; with 500
//! SQL fingerprints that alone would be 13 MB. A dev dashboard needs p50/p95/p99 to within 10%.

/// Sub-buckets per power of two.
const SUB: u64 = 8;
const BUCKETS: usize = 36 * SUB as usize;

#[derive(Clone)]
pub struct Hist {
    counts: Box<[u32; BUCKETS]>,
    total: u64,
    sum: u64,
    max: u64,
}

impl Default for Hist {
    fn default() -> Self {
        Self {
            counts: Box::new([0; BUCKETS]),
            total: 0,
            sum: 0,
            max: 0,
        }
    }
}

fn index(v: u64) -> usize {
    if v < SUB {
        return v as usize;
    }
    let e = 63 - u64::from(v.leading_zeros()); // floor(log2 v), >= 3
    let m = (v - (1 << e)) >> (e - 3);
    ((8 * (e - 2) + m) as usize).min(BUCKETS - 1)
}

/// The midpoint of bucket `i`, the value a percentile in it reports.
fn representative(i: usize) -> u64 {
    if (i as u64) < SUB {
        return i as u64;
    }
    let e = i as u64 / 8 + 2;
    let m = i as u64 % 8;
    let width = 1_u64 << (e - 3);
    (1_u64 << e) + m * width + width / 2
}

impl Hist {
    pub fn record(&mut self, micros: u64) {
        let slot = &mut self.counts[index(micros)];
        *slot = slot.saturating_add(1);
        self.total += 1;
        self.sum = self.sum.saturating_add(micros);
        self.max = self.max.max(micros);
    }

    pub fn count(&self) -> u64 {
        self.total
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    pub fn mean(&self) -> u64 {
        self.sum.checked_div(self.total).unwrap_or(0)
    }

    pub fn sum(&self) -> u64 {
        self.sum
    }

    /// The value at quantile `q` (0.0..=1.0), never above the largest value recorded; 0 when empty.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let target = ((q.clamp(0.0, 1.0) * self.total as f64).ceil() as u64).max(1);
        if target >= self.total {
            return self.max;
        }
        let mut seen = 0_u64;
        for (i, c) in self.counts.iter().enumerate() {
            seen += u64::from(*c);
            if seen >= target {
                return representative(i).min(self.max);
            }
        }
        self.max
    }

    /// Counts per range: values up to `bounds[0]`, up to `bounds[1]`, …, and above the last.
    pub fn bucket_counts<const N: usize>(&self, bounds: &[u64]) -> [u16; N] {
        let mut out = [0_u16; N];
        for (i, c) in self.counts.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let v = representative(i).min(self.max);
            let at = bounds
                .iter()
                .position(|b| v <= *b)
                .unwrap_or(bounds.len())
                .min(N - 1);
            out[at] = out[at].saturating_add((*c).min(u32::from(u16::MAX)) as u16);
        }
        out
    }

    pub fn merge(&mut self, other: &Hist) {
        for (a, b) in self.counts.iter_mut().zip(other.counts.iter()) {
            *a = a.saturating_add(*b);
        }
        self.total += other.total;
        self.sum = self.sum.saturating_add(other.sum);
        self.max = self.max.max(other.max);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The non-empty buckets as (index, count), with the sum and max: what the history file keeps
    /// of a minute's latencies (a few dozen pairs, not 288 counters), so an hour's rollup can merge
    /// its minutes and still report true percentiles.
    pub fn sparse(&self) -> (Vec<(u16, u32)>, u64, u64) {
        let pairs = self
            .counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, c)| (i as u16, *c))
            .collect();
        (pairs, self.sum, self.max)
    }

    /// The inverse of `sparse`. Out-of-range indexes (a damaged file) are dropped.
    pub fn from_sparse(pairs: &[(u16, u32)], sum: u64, max: u64) -> Self {
        let mut h = Self::default();
        for (i, c) in pairs {
            if let Some(slot) = h.counts.get_mut(usize::from(*i)) {
                *slot = slot.saturating_add(*c);
                h.total += u64::from(*c);
            }
        }
        h.sum = sum;
        h.max = max;
        h
    }

    /// Bytes this histogram holds (for the memory bound).
    pub const BYTES: usize = BUCKETS * 4 + 32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_contiguous_and_ordered() {
        let mut last = 0;
        for v in 0..200_000_u64 {
            let i = index(v);
            assert!(i >= last, "{v}");
            last = i;
        }
        assert_eq!(index(7), 7);
        assert_eq!(index(8), 8);
        assert_eq!(index(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn quantiles_are_within_ten_percent() {
        let mut h = Hist::default();
        let mut values: Vec<u64> = (1..=10_000).map(|i| i * 37 % 9_973 + 1).collect();
        for v in &values {
            h.record(*v);
        }
        values.sort_unstable();
        for q in [0.5, 0.95, 0.99] {
            let exact = values[((q * values.len() as f64).ceil() as usize) - 1] as f64;
            let got = h.quantile(q) as f64;
            assert!((got - exact).abs() / exact < 0.10, "q{q}: {got} vs {exact}");
        }
        assert_eq!(h.quantile(1.0), 9_973);
        assert_eq!(h.count(), 10_000);
        assert_eq!(Hist::default().quantile(0.5), 0);
    }
}
