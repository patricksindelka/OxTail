//! K-way merge of several log sources by timestamp (PLAN.md §9).
//!
//! # Ordering
//!
//! Each source yields `(line_no, Option<Timestamp>)` in file order. A line
//! without a timestamp inherits the effective timestamp of its predecessor, so
//! stack traces stay attached to their header. Lines before the first
//! timestamp of a source sort first. If a source goes backwards in time, its
//! effective timestamps are clamped to the running maximum: the merge never
//! reorders lines within one source. The merged order is
//! `(effective timestamp, source index, line number)`.
//!
//! # Incremental policy (following)
//!
//! Lines are pushed as they are discovered. A candidate (the smallest pending
//! line) becomes *final*, and is returned by [`MergeBuilder::drain`], once
//! every source that has nothing pending either
//!
//! 1. is closed, or
//! 2. has already advanced past the candidate (its latest effective timestamp
//!    is greater, or equal with a higher source index, so it cannot still
//!    produce an earlier entry), or
//! 3. is *live* (see [`MergeBuilder::mark_live`]) and has been idle (nothing
//!    pushed) for at least [`MergeConfig::idle_delay`].
//!
//! Sources start **not live**: during the initial load a source that has not
//! delivered its next line yet is merely slow, not idle, so the merge keeps
//! waiting for it (otherwise periodic drains would degenerate into
//! concatenating the sources). The initial load must therefore end with
//! [`MergeBuilder::close_source`] for every source that is finished, or
//! [`MergeBuilder::mark_live`] once a source has caught up with its file and
//! is only being followed.
//!
//! Rule 3 trades strict correctness for liveness: if an idle source later
//! delivers a line older than entries already emitted, that line is emitted
//! next, *after* them (final entries are never rewritten). Callers that need a
//! perfect order can rebuild the merge from scratch.
//!
//! Memory: only pending (not yet final) lines are buffered.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
    time::{Duration, Instant},
};

use jiff::Timestamp;

/// Maximum number of merged sources (source index has 8 bits).
pub const MAX_SOURCES: usize = 256;
/// Largest line number a [`MergedEntry`] can hold (56 bits).
pub const MAX_LINE: u64 = (1 << 56) - 1;

/// One line of a merged view: source index (8 bits) and line number (56 bits)
/// packed into a `u64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MergedEntry(u64);

impl MergedEntry {
    /// Packs `source` and `line`. Returns `None` if `source >= 256` or
    /// `line > MAX_LINE`.
    pub fn new(source: usize, line: u64) -> Option<Self> {
        if source >= MAX_SOURCES || line > MAX_LINE {
            return None;
        }
        Some(Self(((source as u64) << 56) | line))
    }

    /// Source index.
    pub fn source(self) -> usize {
        (self.0 >> 56) as usize
    }

    /// 0-based line number within the source.
    pub fn line(self) -> u64 {
        self.0 & MAX_LINE
    }

    /// The packed representation.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// Rebuilds an entry from [`raw`](Self::raw).
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Tuning for [`MergeBuilder`].
#[derive(Clone, Copy, Debug)]
pub struct MergeConfig {
    /// How long a source may stay silent before it stops holding back
    /// entries from other sources (rule 3 in the module docs).
    pub idle_delay: Duration,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            idle_delay: Duration::from_millis(500),
        }
    }
}

struct Source {
    queue: VecDeque<(u64, Timestamp)>,
    last: Option<Timestamp>,
    closed: bool,
    live: bool,
    last_activity: Instant,
}

/// Incremental k-way merge. See the [module docs](self).
pub struct MergeBuilder {
    sources: Vec<Source>,
    heap: BinaryHeap<Reverse<(Timestamp, usize, u64)>>,
    config: MergeConfig,
}

impl MergeBuilder {
    /// A merge over `source_count` sources (at most [`MAX_SOURCES`]; extra
    /// sources are dropped). `now` starts every source's idle clock.
    pub fn new(source_count: usize, config: MergeConfig, now: Instant) -> Self {
        let n = source_count.min(MAX_SOURCES);
        Self {
            sources: (0..n)
                .map(|_| Source {
                    queue: VecDeque::new(),
                    last: None,
                    closed: false,
                    live: false,
                    last_activity: now,
                })
                .collect(),
            heap: BinaryHeap::new(),
            config,
        }
    }

    /// Number of sources.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Number of lines buffered and not yet final.
    pub fn pending(&self) -> usize {
        self.sources.iter().map(|s| s.queue.len()).sum()
    }

    /// Adds the next line of `source`. Lines must be pushed in file order.
    /// Out-of-range sources or line numbers are ignored.
    pub fn push(&mut self, source: usize, line: u64, ts: Option<Timestamp>, now: Instant) {
        if line > MAX_LINE {
            return;
        }
        let Some(s) = self.sources.get_mut(source) else {
            return;
        };
        let floor = s.last.unwrap_or(Timestamp::MIN);
        let eff = ts.map_or(floor, |t| t.max(floor));
        s.last = Some(eff);
        s.last_activity = now;
        s.closed = false;
        if s.queue.is_empty() {
            self.heap.push(Reverse((eff, source, line)));
        }
        s.queue.push_back((line, eff));
    }

    /// Declares that `source` will not produce more lines (until it is pushed
    /// to again), so it no longer holds back other sources.
    pub fn close_source(&mut self, source: usize) {
        if let Some(s) = self.sources.get_mut(source) {
            s.closed = true;
        }
    }

    /// Declares that `source` has caught up with its file and is now only
    /// followed: from now on it may be treated as idle (rule 3) after
    /// [`MergeConfig::idle_delay`] without new lines, counted from `now`.
    pub fn mark_live(&mut self, source: usize, now: Instant) {
        if let Some(s) = self.sources.get_mut(source) {
            s.live = true;
            s.last_activity = now;
        }
    }

    fn can_emit(&self, key: Timestamp, source: usize, now: Instant) -> bool {
        self.sources.iter().enumerate().all(|(t, s)| {
            !s.queue.is_empty()
                || s.closed
                || s.last
                    .is_some_and(|le| le > key || (le == key && t > source))
                || (s.live
                    && now.saturating_duration_since(s.last_activity) >= self.config.idle_delay)
        })
    }

    fn pop_min(&mut self) -> Option<MergedEntry> {
        let Reverse((_, s, l)) = self.heap.pop()?;
        let src = &mut self.sources[s];
        src.queue.pop_front();
        if let Some(&(nl, nt)) = src.queue.front() {
            self.heap.push(Reverse((nt, s, nl)));
        }
        MergedEntry::new(s, l)
    }

    /// Returns the entries that became final since the last call, in merged
    /// order.
    pub fn drain(&mut self, now: Instant) -> Vec<MergedEntry> {
        let mut out = Vec::new();
        while let Some(&Reverse((k, s, _))) = self.heap.peek() {
            if !self.can_emit(k, s, now) {
                break;
            }
            out.extend(self.pop_min());
        }
        out
    }

    /// Closes all sources and returns everything still pending, in order.
    pub fn finish(mut self) -> Vec<MergedEntry> {
        let mut out = Vec::with_capacity(self.pending());
        while let Some(e) = self.pop_min() {
            out.push(e);
        }
        out
    }

    /// Convenience: merges complete sources in one go (buffers everything;
    /// use the incremental API for large inputs).
    pub fn merge_all<I>(sources: Vec<I>) -> Vec<MergedEntry>
    where
        I: IntoIterator<Item = (u64, Option<Timestamp>)>,
    {
        let now = Instant::now();
        let mut b = Self::new(sources.len(), MergeConfig::default(), now);
        for (i, src) in sources.into_iter().enumerate() {
            for (line, ts) in src {
                b.push(i, line, ts, now);
            }
        }
        b.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: i64) -> Option<Timestamp> {
        Some(Timestamp::from_second(1_700_000_000 + s).expect("range"))
    }

    #[test]
    fn entry_packing() {
        let e = MergedEntry::new(255, MAX_LINE).expect("fits");
        assert_eq!((e.source(), e.line()), (255, MAX_LINE));
        assert_eq!(MergedEntry::from_raw(e.raw()), e);
        assert!(MergedEntry::new(256, 0).is_none());
        assert!(MergedEntry::new(0, MAX_LINE + 1).is_none());
        let e = MergedEntry::new(3, 42).expect("fits");
        assert_eq!((e.source(), e.line()), (3, 42));
    }

    #[test]
    fn basic_merge_with_inheritance_and_ties() {
        let a = vec![(0, ts(1)), (1, None), (2, ts(5))];
        let b = vec![(0, ts(1)), (1, ts(3)), (2, None), (3, ts(7))];
        let out = MergeBuilder::merge_all(vec![a, b]);
        let got: Vec<(usize, u64)> = out.iter().map(|e| (e.source(), e.line())).collect();
        assert_eq!(
            got,
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (1, 2), (0, 2), (1, 3)]
        );
    }

    #[test]
    fn leading_untimestamped_lines_first() {
        let a = vec![(0, None), (1, ts(5))];
        let b = vec![(0, ts(1))];
        let out = MergeBuilder::merge_all(vec![a, b]);
        assert_eq!((out[0].source(), out[0].line()), (0, 0));
    }

    #[test]
    fn incremental_waits_for_slow_sources() {
        let t0 = Instant::now();
        let cfg = MergeConfig {
            idle_delay: Duration::from_secs(1),
        };
        let mut m = MergeBuilder::new(2, cfg, t0);
        m.mark_live(1, t0);
        m.push(0, 0, ts(10), t0);
        m.push(0, 1, ts(20), t0);
        // Source 1 has said nothing: nothing is final yet.
        assert!(m.drain(t0).is_empty());
        // Source 1 delivers a line at 15: source 1 has advanced past 10.
        m.push(1, 0, ts(15), t0);
        let d = m.drain(t0);
        assert_eq!(
            d.iter().map(|e| (e.source(), e.line())).collect::<Vec<_>>(),
            vec![(0, 0), (1, 0)]
        );
        // Now 20 (source 0) waits for source 1 to pass 20 ...
        assert!(m.drain(t0).is_empty());
        // ... or to be idle.
        let later = t0 + Duration::from_secs(2);
        let d = m.drain(later);
        assert_eq!(
            d.iter().map(|e| (e.source(), e.line())).collect::<Vec<_>>(),
            vec![(0, 1)]
        );
        assert_eq!(m.pending(), 0);
    }

    #[test]
    fn closed_source_does_not_block() {
        let t0 = Instant::now();
        let mut m = MergeBuilder::new(2, MergeConfig::default(), t0);
        m.push(0, 0, ts(1), t0);
        m.close_source(1);
        assert_eq!(m.drain(t0).len(), 1);
    }

    #[test]
    fn initial_load_with_periodic_drains_is_not_concatenation() {
        let t0 = Instant::now();
        let cfg = MergeConfig {
            idle_delay: Duration::from_millis(10),
        };
        let mut m = MergeBuilder::new(2, cfg, t0);
        let mut out = vec![];
        // Source 0 is loaded first (slowly: drains happen long after
        // idle_delay), then source 1, whose lines interleave.
        for i in 0..5u64 {
            let t = t0 + Duration::from_secs(10 * (i + 1));
            m.push(0, i, ts(i as i64 * 2), t);
            out.extend(m.drain(t));
        }
        assert!(out.is_empty(), "source 1 is still loading: {out:?}");
        for i in 0..5u64 {
            let t = t0 + Duration::from_secs(100 + i);
            m.push(1, i, ts(i as i64 * 2 + 1), t);
            out.extend(m.drain(t));
        }
        m.close_source(0);
        m.close_source(1);
        out.extend(m.drain(t0 + Duration::from_secs(200)));
        out.extend(m.finish());
        let got: Vec<(usize, u64)> = out.iter().map(|e| (e.source(), e.line())).collect();
        let want: Vec<(usize, u64)> = (0..10).map(|k| (k % 2, (k / 2) as u64)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn late_line_after_idle_is_emitted_after() {
        let t0 = Instant::now();
        let cfg = MergeConfig {
            idle_delay: Duration::from_millis(10),
        };
        let mut m = MergeBuilder::new(2, cfg, t0);
        m.mark_live(1, t0);
        m.push(0, 0, ts(10), t0);
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(m.drain(t1).len(), 1);
        m.push(1, 0, ts(5), t1);
        let d = m.drain(t1 + Duration::from_secs(1));
        assert_eq!((d[0].source(), d[0].line()), (1, 0));
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        fn naive(sources: &[Vec<(u64, Option<Timestamp>)>]) -> Vec<(usize, u64)> {
            let mut all = vec![];
            for (si, src) in sources.iter().enumerate() {
                let mut last = Timestamp::MIN;
                for (l, t) in src {
                    let eff = t.map_or(last, |t| t.max(last));
                    last = eff;
                    all.push((eff, si, *l));
                }
            }
            all.sort();
            all.into_iter().map(|(_, s, l)| (s, l)).collect()
        }

        proptest! {
            #[test]
            fn merge_matches_naive_sort(
                raw in proptest::collection::vec(
                    proptest::collection::vec(proptest::option::of(0i64..50), 0..40), 1..6)
            ) {
                let sources: Vec<Vec<(u64, Option<Timestamp>)>> = raw.iter()
                    .map(|v| v.iter().enumerate().map(|(i, t)| (i as u64, t.and_then(ts))).collect())
                    .collect();
                let want = naive(&sources);
                let got: Vec<(usize, u64)> = MergeBuilder::merge_all(sources.clone())
                    .iter().map(|e| (e.source(), e.line())).collect();
                prop_assert_eq!(&got, &want);

                // Incremental with interleaved drains and closed sources gives the same order.
                let t0 = Instant::now();
                let mut m = MergeBuilder::new(sources.len(), MergeConfig::default(), t0);
                let mut out = vec![];
                let mut idx = vec![0usize; sources.len()];
                let mut progressed = true;
                while progressed {
                    progressed = false;
                    for (si, src) in sources.iter().enumerate() {
                        if idx[si] < src.len() {
                            let (l, t) = src[idx[si]];
                            m.push(si, l, t, t0);
                            idx[si] += 1;
                            progressed = true;
                            out.extend(m.drain(t0));
                        }
                    }
                }
                for si in 0..sources.len() { m.close_source(si); }
                out.extend(m.drain(t0));
                out.extend(m.finish());
                let got: Vec<(usize, u64)> = out.iter().map(|e| (e.source(), e.line())).collect();
                prop_assert_eq!(got, want);
            }
        }
    }
}
