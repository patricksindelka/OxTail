//! Bounded-memory column statistics.
//!
//! Per column: exact count / min / max / mean, an approximate distinct count
//! (HyperLogLog, 4 KiB), the most frequent values (Space-Saving with a fixed
//! number of counters) and, for numeric columns, percentiles from a bounded
//! reservoir sample. Everything is mergeable, so statistics can be built in
//! parallel over chunks of a file and combined.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::level::Level;
use crate::model::{ColumnKind, Record, Schema};
use crate::quantity::{parse_bytes, parse_duration_ns};

/// Number of Space-Saving counters kept per column.
const TOP_COUNTERS: usize = 300;
/// Longest value (in bytes) kept as a top-K key; longer values are truncated.
const MAX_KEY_LEN: usize = 256;
/// Reservoir size for percentile estimation.
const RESERVOIR: usize = 100_000;
const HLL_P: u32 = 12;
const HLL_M: usize = 1 << HLL_P;

/// A value and how often it occurred (an upper bound when the column has more
/// distinct values than counters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopValue {
    /// The value text (level columns show the normalised label).
    pub value: String,
    /// Occurrence count.
    pub count: u64,
}

/// Estimated percentiles of a numeric column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Percentiles {
    /// Median.
    pub p50: f64,
    /// 90th percentile.
    pub p90: f64,
    /// 99th percentile.
    pub p99: f64,
}

#[derive(Debug, Clone)]
struct Hll {
    regs: Vec<u8>,
}

impl Hll {
    fn new() -> Self {
        Hll {
            regs: vec![0; HLL_M],
        }
    }

    fn add(&mut self, s: &str) {
        let mut h = DefaultHasher::new();
        s.hash(&mut h);
        // Extra mixing: SipHash output is good, but be safe with the top bits.
        let x = h.finish();
        let idx = (x >> (64 - HLL_P)) as usize;
        let w = (x << HLL_P) | (1 << (HLL_P - 1));
        let rank = w.leading_zeros() as u8 + 1;
        if self.regs[idx] < rank {
            self.regs[idx] = rank;
        }
    }

    fn merge(&mut self, o: &Hll) {
        for (a, b) in self.regs.iter_mut().zip(&o.regs) {
            *a = (*a).max(*b);
        }
    }

    fn estimate(&self) -> u64 {
        let m = HLL_M as f64;
        let sum: f64 = self.regs.iter().map(|&r| 2f64.powi(-i32::from(r))).sum();
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let raw = alpha * m * m / sum;
        let zeros = self.regs.iter().filter(|&&r| r == 0).count();
        let est = if raw <= 2.5 * m && zeros > 0 {
            m * (m / zeros as f64).ln()
        } else {
            raw
        };
        est.round() as u64
    }
}

#[derive(Debug, Clone, Default)]
struct Numeric {
    n: u64,
    sum: f64,
    min: f64,
    max: f64,
    sample: Vec<f64>,
    rng: u64,
}

impl Numeric {
    fn add(&mut self, v: f64) {
        if self.n == 0 {
            self.min = v;
            self.max = v;
            self.rng = 0x9E37_79B9_7F4A_7C15;
        }
        self.n += 1;
        self.sum += v;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        if self.sample.len() < RESERVOIR {
            self.sample.push(v);
        } else {
            // Algorithm R.
            let j = self.next_rand() % self.n;
            if (j as usize) < RESERVOIR {
                self.sample[j as usize] = v;
            }
        }
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn merge(&mut self, o: &Numeric) {
        if o.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = o.clone();
            return;
        }
        let (na, nb) = (self.n as f64, o.n as f64);
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
        self.sum += o.sum;
        if self.sample.len() + o.sample.len() <= RESERVOIR {
            self.sample.extend_from_slice(&o.sample);
        } else {
            // Take proportionally from each side (approximately uniform).
            let ka = ((RESERVOIR as f64 * na / (na + nb)).round() as usize).min(self.sample.len());
            let kb = (RESERVOIR - ka).min(o.sample.len());
            let ka = (RESERVOIR - kb).min(self.sample.len());
            self.sample.truncate(ka);
            self.sample.extend_from_slice(&o.sample[..kb]);
        }
        self.n += o.n;
    }

    fn percentiles(&self) -> Option<Percentiles> {
        if self.sample.is_empty() {
            return None;
        }
        let mut s = self.sample.clone();
        s.sort_by(f64::total_cmp);
        let at = |p: f64| {
            let rank = (p * s.len() as f64).ceil() as usize;
            s[rank.clamp(1, s.len()) - 1]
        };
        Some(Percentiles {
            p50: at(0.50),
            p90: at(0.90),
            p99: at(0.99),
        })
    }
}

/// Statistics for one column. See the module docs.
#[derive(Debug, Clone)]
pub struct ColumnStats {
    kind: ColumnKind,
    count: u64,
    missing: u64,
    hll: Hll,
    /// Space-Saving counters: value -> (count, overestimation error).
    top: HashMap<String, (u64, u64)>,
    numeric: Numeric,
}

impl ColumnStats {
    /// Creates empty statistics for a column of the given kind. Number,
    /// Duration (in nanoseconds) and Bytes columns also get numeric summaries.
    pub fn new(kind: ColumnKind) -> Self {
        ColumnStats {
            kind,
            count: 0,
            missing: 0,
            hll: Hll::new(),
            top: HashMap::new(),
            numeric: Numeric::default(),
        }
    }

    /// Adds one cell; `None` counts as missing.
    pub fn add(&mut self, value: Option<&str>) {
        let Some(v) = value.filter(|v| !v.is_empty()) else {
            self.missing += 1;
            return;
        };
        self.count += 1;
        let key: &str = if self.kind == ColumnKind::Level {
            match Level::parse(v) {
                Some(l) => l.as_str(),
                None => v,
            }
        } else {
            v
        };
        let mut end = key.len().min(MAX_KEY_LEN);
        while !key.is_char_boundary(end) {
            end -= 1;
        }
        let key = &key[..end];
        self.hll.add(key);
        self.bump(key);
        let num = match self.kind {
            ColumnKind::Number => v.trim().parse::<f64>().ok(),
            ColumnKind::Duration => parse_duration_ns(v),
            ColumnKind::Bytes => parse_bytes(v),
            _ => None,
        };
        if let Some(n) = num.filter(|n| n.is_finite()) {
            self.numeric.add(n);
        }
    }

    fn bump(&mut self, key: &str) {
        if let Some(e) = self.top.get_mut(key) {
            e.0 += 1;
        } else if self.top.len() < TOP_COUNTERS {
            self.top.insert(key.to_string(), (1, 0));
        } else if let Some((min_key, (min_count, _))) = self
            .top
            .iter()
            .min_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| b.0.cmp(a.0)))
            .map(|(k, v)| (k.clone(), *v))
        {
            self.top.remove(&min_key);
            self.top.insert(key.to_string(), (min_count + 1, min_count));
        }
    }

    /// Merges another accumulator (for parallel accumulation). The other
    /// must be for the same column kind.
    pub fn merge(&mut self, other: &ColumnStats) {
        self.count += other.count;
        self.missing += other.missing;
        self.hll.merge(&other.hll);
        self.numeric.merge(&other.numeric);
        for (k, (c, e)) in &other.top {
            let slot = self.top.entry(k.clone()).or_insert((0, 0));
            slot.0 += c;
            slot.1 += e;
        }
        if self.top.len() > TOP_COUNTERS {
            let mut all: Vec<(String, (u64, u64))> = self.top.drain().collect();
            all.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
            all.truncate(TOP_COUNTERS);
            self.top = all.into_iter().collect();
        }
    }

    /// Number of non-empty values seen.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Number of absent or empty values seen.
    pub fn missing(&self) -> u64 {
        self.missing
    }

    /// Approximate number of distinct values (about 1.6% standard error).
    pub fn distinct_estimate(&self) -> u64 {
        self.hll.estimate().min(self.count)
    }

    /// The `n` most frequent values, most frequent first (ties by value).
    /// Counts are exact while the column has at most 300 distinct values and
    /// upper bounds beyond that.
    pub fn top(&self, n: usize) -> Vec<TopValue> {
        let mut v: Vec<TopValue> = self
            .top
            .iter()
            .map(|(k, (c, _))| TopValue {
                value: k.clone(),
                count: *c,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
        v.truncate(n);
        v
    }

    /// Number of values that parsed as numbers.
    pub fn numeric_count(&self) -> u64 {
        self.numeric.n
    }

    /// Smallest numeric value (nanoseconds for durations, bytes for sizes).
    pub fn min(&self) -> Option<f64> {
        (self.numeric.n > 0).then_some(self.numeric.min)
    }

    /// Largest numeric value.
    pub fn max(&self) -> Option<f64> {
        (self.numeric.n > 0).then_some(self.numeric.max)
    }

    /// Mean of the numeric values.
    pub fn mean(&self) -> Option<f64> {
        (self.numeric.n > 0).then(|| self.numeric.sum / self.numeric.n as f64)
    }

    /// p50 / p90 / p99 estimated from a reservoir of up to 100,000 values
    /// (exact below that).
    pub fn percentiles(&self) -> Option<Percentiles> {
        self.numeric.percentiles()
    }
}

/// Statistics for every column of a schema.
#[derive(Debug, Clone)]
pub struct TableStats {
    /// One accumulator per schema column.
    pub columns: Vec<ColumnStats>,
    /// Number of records added.
    pub records: u64,
}

impl TableStats {
    /// Creates accumulators for each column of `schema`.
    pub fn new(schema: &Schema) -> Self {
        TableStats {
            columns: schema
                .columns
                .iter()
                .map(|c| ColumnStats::new(c.kind))
                .collect(),
            records: 0,
        }
    }

    /// Adds one record.
    pub fn add_record(&mut self, rec: &Record<'_>) {
        self.records += 1;
        for (i, c) in self.columns.iter_mut().enumerate() {
            c.add(rec.get(i));
        }
    }

    /// Merges another table accumulator built from the same schema.
    pub fn merge(&mut self, other: &TableStats) {
        self.records += other.records;
        for (a, b) in self.columns.iter_mut().zip(&other.columns) {
            a.merge(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_and_counts() {
        let mut s = ColumnStats::new(ColumnKind::Level);
        for v in ["info", "INFO", "w", "warning", "error", "info"] {
            s.add(Some(v));
        }
        s.add(None);
        s.add(Some(""));
        assert_eq!(s.count(), 6);
        assert_eq!(s.missing(), 2);
        let top = s.top(10);
        assert_eq!(
            top[0],
            TopValue {
                value: "INFO".into(),
                count: 3
            }
        );
        assert_eq!(
            top[1],
            TopValue {
                value: "WARN".into(),
                count: 2
            }
        );
        assert_eq!(s.distinct_estimate(), 3);
    }

    #[test]
    fn numeric_summary() {
        let mut s = ColumnStats::new(ColumnKind::Number);
        for i in 1..=100 {
            s.add(Some(&i.to_string()));
        }
        s.add(Some("n/a"));
        assert_eq!(s.numeric_count(), 100);
        assert_eq!(s.min(), Some(1.0));
        assert_eq!(s.max(), Some(100.0));
        assert_eq!(s.mean(), Some(50.5));
        let p = s.percentiles().unwrap();
        assert_eq!((p.p50, p.p90, p.p99), (50.0, 90.0, 99.0));
    }

    #[test]
    fn durations_are_nanoseconds() {
        let mut s = ColumnStats::new(ColumnKind::Duration);
        s.add(Some("1s"));
        s.add(Some("500ms"));
        assert_eq!(s.max(), Some(1e9));
        assert_eq!(s.min(), Some(5e8));
    }

    #[test]
    fn memory_is_bounded_and_heavy_hitters_survive() {
        let mut s = ColumnStats::new(ColumnKind::Text);
        for i in 0..50_000u32 {
            // "hot" appears every other row; the rest are unique.
            if i % 2 == 0 {
                s.add(Some("hot"));
            } else {
                s.add(Some(&format!("id-{i}")));
            }
        }
        assert!(s.top.len() <= TOP_COUNTERS);
        let top = s.top(1);
        assert_eq!(top[0].value, "hot");
        assert!(top[0].count >= 25_000);
        let est = s.distinct_estimate() as f64;
        assert!((est - 25_001.0).abs() / 25_001.0 < 0.06, "estimate {est}");
    }

    #[test]
    fn reservoir_is_bounded() {
        let mut s = ColumnStats::new(ColumnKind::Number);
        for i in 0..(RESERVOIR as u64 + 50_000) {
            s.add(Some(&i.to_string()));
        }
        assert_eq!(s.numeric.sample.len(), RESERVOIR);
        let p = s.percentiles().unwrap();
        let n = (RESERVOIR + 50_000) as f64;
        assert!((p.p50 - n / 2.0).abs() < n * 0.02, "{p:?}");
    }

    #[test]
    fn merge_equals_sequential() {
        let vals: Vec<String> = (0..2000).map(|i| (i % 37).to_string()).collect();
        let mut whole = ColumnStats::new(ColumnKind::Number);
        let mut a = ColumnStats::new(ColumnKind::Number);
        let mut b = ColumnStats::new(ColumnKind::Number);
        for (i, v) in vals.iter().enumerate() {
            whole.add(Some(v));
            if i % 3 == 0 {
                a.add(Some(v));
            } else {
                b.add(Some(v));
            }
        }
        a.merge(&b);
        assert_eq!(a.count(), whole.count());
        assert_eq!(a.top(5), whole.top(5));
        assert_eq!(a.min(), whole.min());
        assert_eq!(a.max(), whole.max());
        assert_eq!(a.distinct_estimate(), whole.distinct_estimate());
        assert_eq!(a.percentiles(), whole.percentiles());
        assert!((a.mean().unwrap() - whole.mean().unwrap()).abs() < 1e-9);
    }

    #[test]
    fn table_stats() {
        let schema = Schema::from_names(&["level", "status"], &Default::default());
        let mut t = TableStats::new(&schema);
        let mut r = Record::with_columns(2);
        r.set_owned(0, "info".into());
        r.set_owned(1, "200".into());
        t.add_record(&r);
        t.add_record(&Record::with_columns(2));
        let mut t2 = t.clone();
        t2.merge(&t);
        assert_eq!(t2.records, 4);
        assert_eq!(t2.columns[1].count(), 2);
        assert_eq!(t2.columns[1].missing(), 2);
    }
}
