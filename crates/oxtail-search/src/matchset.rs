//! [`MatchSet`]: sorted, deduplicated line-start offsets.
//!
//! # Storage choice
//!
//! Offsets are stored as sorted `u64` vectors, split into per-chunk segments
//! that are shared through `Arc`. A `RoaringTreemap` would compress dense
//! sets, but line-start offsets are sparse in a 64-bit space (a match every
//! ~100 bytes at best), so a bitmap gains little, while a plain sorted vector
//! gives O(1) `get(i)` and O(log n) `rank`, which is exactly what a virtual
//! list needs. Segments make streaming cheap: a snapshot of a running search
//! clones one `Arc` per finished chunk instead of copying every offset, and a
//! late chunk never forces a re-merge. Worst case is 8 bytes per match
//! (10M matches = 80 MB), as budgeted in PLAN.md section 6.2.

use std::sync::Arc;

/// One chunk's worth of entries. `context` is either empty (no context
/// entries) or has the same length as `offsets`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Segment {
    pub offsets: Vec<u64>,
    pub context: Vec<bool>,
}

/// A sorted, deduplicated set of line-start offsets, optionally with some
/// entries marked as context lines (filter views with `-A/-B/-C`).
///
/// Cloning is cheap (segments are shared).
#[derive(Debug, Clone, Default)]
pub struct MatchSet {
    segments: Vec<Arc<Segment>>,
    /// `prefix[i]` is the number of entries before segment `i`;
    /// `prefix.len() == segments.len() + 1`.
    prefix: Vec<usize>,
}

impl MatchSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a set from arbitrary offsets (sorted and deduplicated here).
    pub fn from_offsets(mut offsets: Vec<u64>) -> Self {
        offsets.sort_unstable();
        offsets.dedup();
        Self::from_segments(vec![Arc::new(Segment {
            offsets,
            context: Vec::new(),
        })])
    }

    /// Builds a set from `(offset, is_context)` entries (sorted and
    /// deduplicated here; a non-context entry wins over a context one).
    pub fn from_entries(mut entries: Vec<(u64, bool)>) -> Self {
        entries.sort_unstable();
        let mut offsets: Vec<u64> = Vec::with_capacity(entries.len());
        let mut context: Vec<bool> = Vec::with_capacity(entries.len());
        for (o, c) in entries {
            if offsets.last() == Some(&o) {
                // (o, false) sorts before (o, true), so the first one wins.
                continue;
            }
            offsets.push(o);
            context.push(c);
        }
        if !context.contains(&true) {
            context.clear();
        }
        Self::from_segments(vec![Arc::new(Segment { offsets, context })])
    }

    /// Assembles a set from segments that are already ordered and disjoint.
    pub(crate) fn from_segments(segments: Vec<Arc<Segment>>) -> Self {
        let segments: Vec<_> = segments
            .into_iter()
            .filter(|s| !s.offsets.is_empty())
            .collect();
        let mut prefix = Vec::with_capacity(segments.len() + 1);
        let mut total = 0;
        prefix.push(0);
        for s in &segments {
            total += s.offsets.len();
            prefix.push(total);
        }
        Self { segments, prefix }
    }

    /// Number of entries (context entries included).
    pub fn len(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    /// Whether the set has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn locate(&self, i: usize) -> Option<(&Segment, usize)> {
        if i >= self.len() {
            return None;
        }
        let seg = self.prefix.partition_point(|&p| p <= i) - 1;
        Some((&self.segments[seg], i - self.prefix[seg]))
    }

    /// The `i`-th offset in ascending order.
    pub fn get(&self, i: usize) -> Option<u64> {
        self.locate(i).map(|(s, j)| s.offsets[j])
    }

    /// Whether the `i`-th entry is a context line rather than a real match.
    /// `false` when `i` is out of range.
    pub fn is_context(&self, i: usize) -> bool {
        self.locate(i)
            .is_some_and(|(s, j)| s.context.get(j).copied().unwrap_or(false))
    }

    /// The index of the first entry whose offset is `>= offset`
    /// (`len()` if there is none).
    pub fn rank(&self, offset: u64) -> usize {
        let seg = self
            .segments
            .partition_point(|s| s.offsets.last().is_some_and(|&l| l < offset));
        match self.segments.get(seg) {
            None => self.len(),
            Some(s) => self.prefix[seg] + s.offsets.partition_point(|&o| o < offset),
        }
    }

    /// Whether `offset` is in the set.
    pub fn contains(&self, offset: u64) -> bool {
        self.get(self.rank(offset)) == Some(offset)
    }

    /// The first offset strictly greater than `offset`.
    pub fn next_after(&self, offset: u64) -> Option<u64> {
        match offset.checked_add(1) {
            Some(n) => self.get(self.rank(n)),
            None => None,
        }
    }

    /// The last offset strictly smaller than `offset`.
    pub fn prev_before(&self, offset: u64) -> Option<u64> {
        self.rank(offset).checked_sub(1).and_then(|i| self.get(i))
    }

    /// Iterates the offsets in ascending order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = u64> + '_ {
        self.segments.iter().flat_map(|s| s.offsets.iter().copied())
    }

    /// Iterates `(offset, is_context)` in ascending order.
    pub fn iter_entries(&self) -> impl Iterator<Item = (u64, bool)> + '_ {
        self.segments.iter().flat_map(|s| {
            s.offsets
                .iter()
                .enumerate()
                .map(|(j, &o)| (o, s.context.get(j).copied().unwrap_or(false)))
        })
    }

    /// The union of two sets. An entry that is a real match in either set is
    /// a real match in the result.
    pub fn merge(&self, other: &MatchSet) -> MatchSet {
        let mut a = self.iter_entries().peekable();
        let mut b = other.iter_entries().peekable();
        let mut out: Vec<(u64, bool)> = Vec::with_capacity(self.len() + other.len());
        loop {
            let next = match (a.peek(), b.peek()) {
                (Some(&(x, cx)), Some(&(y, cy))) => {
                    if x < y {
                        a.next();
                        (x, cx)
                    } else if y < x {
                        b.next();
                        (y, cy)
                    } else {
                        a.next();
                        b.next();
                        (x, cx && cy)
                    }
                }
                (Some(&e), None) => {
                    a.next();
                    e
                }
                (None, Some(&e)) => {
                    b.next();
                    e
                }
                (None, None) => break,
            };
            out.push(next);
        }
        MatchSet::from_entries(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(o: &[u64]) -> Arc<Segment> {
        Arc::new(Segment {
            offsets: o.to_vec(),
            context: vec![],
        })
    }

    #[test]
    fn empty_set() {
        let s = MatchSet::new();
        assert!(s.is_empty());
        assert_eq!(s.get(0), None);
        assert_eq!(s.rank(5), 0);
        assert_eq!(s.next_after(0), None);
        assert_eq!(s.prev_before(9), None);
    }

    #[test]
    fn segmented_lookup() {
        let s = MatchSet::from_segments(vec![seg(&[1, 5]), seg(&[]), seg(&[9, 12, 20])]);
        assert_eq!(s.len(), 5);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![1, 5, 9, 12, 20]);
        assert_eq!(s.get(2), Some(9));
        assert_eq!(s.get(5), None);
        assert_eq!(s.rank(0), 0);
        assert_eq!(s.rank(5), 1);
        assert_eq!(s.rank(6), 2);
        assert_eq!(s.rank(20), 4);
        assert_eq!(s.rank(21), 5);
        assert!(s.contains(12) && !s.contains(13));
        assert_eq!(s.next_after(5), Some(9));
        assert_eq!(s.next_after(20), None);
        assert_eq!(s.prev_before(9), Some(5));
        assert_eq!(s.prev_before(1), None);
    }

    #[test]
    fn from_offsets_sorts_and_dedups() {
        let s = MatchSet::from_offsets(vec![9, 1, 9, 4]);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![1, 4, 9]);
    }

    #[test]
    fn context_flags_and_merge() {
        let a = MatchSet::from_entries(vec![(1, false), (4, true), (8, true)]);
        let b = MatchSet::from_entries(vec![(4, false), (6, true), (8, true)]);
        assert!(a.is_context(1) && !a.is_context(0));
        let m = a.merge(&b);
        assert_eq!(
            m.iter_entries().collect::<Vec<_>>(),
            vec![(1, false), (4, false), (6, true), (8, true)]
        );
        assert!(!m.is_context(1));
        assert!(m.is_context(2));
    }
}
