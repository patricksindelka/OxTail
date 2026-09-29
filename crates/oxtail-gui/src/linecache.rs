//! The UI's cache of display lines.
//!
//! Lines are keyed by their **start offset** in the document's UTF-8 view,
//! which is a stable identity within one generation (line *numbers* are only
//! estimates until the index is complete). A line's successor starts at
//! `offset + len`, so runs of consecutive lines can be walked without knowing
//! any line number; that is how the view scrolls while the file is still being
//! indexed.
//!
//! The cache is bounded: when it grows past its capacity, the entries farthest
//! from the current view position are dropped. Results from an older
//! generation are ignored and a generation change clears everything.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use oxtail_core::Line;

/// Default number of cached lines.
pub const DEFAULT_CAPACITY: usize = 20_000;

/// A bounded cache of lines keyed by start offset.
#[derive(Debug)]
pub struct LineCache {
    generation: u64,
    lines: BTreeMap<u64, Arc<Line>>,
    by_number: HashMap<u64, u64>,
    capacity: usize,
}

impl LineCache {
    /// An empty cache for `generation`.
    pub fn new(generation: u64, capacity: usize) -> Self {
        Self {
            generation,
            lines: BTreeMap::new(),
            by_number: HashMap::new(),
            capacity: capacity.max(16),
        }
    }

    /// The generation the cached lines belong to.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of cached lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Drops everything and switches to `generation`.
    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.lines.clear();
        self.by_number.clear();
    }

    /// Adds lines read in `generation`. Returns `false` (and adds nothing)
    /// when the generation is stale.
    pub fn insert(&mut self, generation: u64, lines: Vec<Line>) -> bool {
        if generation != self.generation {
            return false;
        }
        for l in lines {
            self.insert_one(l);
        }
        true
    }

    fn insert_one(&mut self, line: Line) {
        let offset = line.offset;
        if let Some(old) = self.lines.get(&offset)
            && old.number_exact
            && self.by_number.get(&old.number) == Some(&offset)
        {
            self.by_number.remove(&old.number);
        }
        if line.number_exact {
            self.by_number.insert(line.number, offset);
        }
        self.lines.insert(offset, Arc::new(line));
    }

    /// The line starting at `offset`.
    pub fn get(&self, offset: u64) -> Option<&Arc<Line>> {
        self.lines.get(&offset)
    }

    /// The cached line with this (exact) 0-based number.
    pub fn get_number(&self, number: u64) -> Option<&Arc<Line>> {
        self.by_number.get(&number).and_then(|o| self.lines.get(o))
    }

    /// The line right after `line`, if cached.
    pub fn next(&self, line: &Line) -> Option<&Arc<Line>> {
        self.lines.get(&line.offset.saturating_add(line.len))
    }

    /// The line right before the line starting at `offset`, if cached.
    pub fn prev(&self, offset: u64) -> Option<&Arc<Line>> {
        let (_, l) = self.lines.range(..offset).next_back()?;
        (l.offset + l.len == offset).then_some(l)
    }

    /// The cached line that contains byte `offset` (the last line starting at
    /// or before it), if any and if it really covers the byte.
    pub fn containing(&self, offset: u64) -> Option<&Arc<Line>> {
        let (_, l) = self.lines.range(..=offset).next_back()?;
        (offset < l.offset + l.len.max(1)).then_some(l)
    }

    /// The last cached line whose end is at least `end`, i.e. the final line
    /// of a document of length `end`.
    pub fn last_line_ending_at(&self, end: u64) -> Option<&Arc<Line>> {
        let (_, l) = self.lines.range(..).next_back()?;
        (l.offset + l.len >= end).then_some(l)
    }

    /// Number of consecutive cached lines starting at `offset` (following the
    /// chain), at most `limit`.
    pub fn run_from(&self, offset: u64, limit: usize) -> usize {
        let mut n = 0;
        let mut cur = self.lines.get(&offset);
        while let Some(l) = cur {
            n += 1;
            if n >= limit {
                break;
            }
            cur = self.next(l);
        }
        n
    }

    /// Number of consecutive cached lines ending right before `offset`, at
    /// most `limit`.
    pub fn run_before(&self, offset: u64, limit: usize) -> usize {
        let mut n = 0;
        let mut at = offset;
        while n < limit {
            match self.prev(at) {
                Some(l) => {
                    n += 1;
                    at = l.offset;
                }
                None => break,
            }
        }
        n
    }

    /// The offset one past the end of the chain that starts at `offset`
    /// (`offset` itself if that line is not cached).
    pub fn chain_end(&self, offset: u64) -> u64 {
        let mut at = offset;
        while let Some(l) = self.lines.get(&at) {
            at = l.offset + l.len;
        }
        at
    }

    /// The start offset of the earliest line of the chain that ends at
    /// `offset` (`offset` itself if nothing precedes it in cache).
    pub fn chain_start_before(&self, offset: u64) -> u64 {
        let mut at = offset;
        while let Some(l) = self.prev(at) {
            at = l.offset;
        }
        at
    }

    /// Drops every cached line that ends at or after `offset`. Used when the
    /// document grew: a line that was unterminated may have changed.
    pub fn invalidate_from(&mut self, offset: u64) {
        let doomed: Vec<u64> = self
            .lines
            .iter()
            .rev()
            .take_while(|(_, l)| l.offset + l.len >= offset)
            .map(|(o, _)| *o)
            .collect();
        for o in doomed {
            self.remove(o);
        }
    }

    /// Drops lines whose number is only an estimate, so they are re-read with
    /// exact numbers once the index is complete.
    pub fn drop_inexact(&mut self) {
        let doomed: Vec<u64> = self
            .lines
            .iter()
            .filter(|(_, l)| !l.number_exact)
            .map(|(o, _)| *o)
            .collect();
        for o in doomed {
            self.remove(o);
        }
    }

    fn remove(&mut self, offset: u64) {
        if let Some(l) = self.lines.remove(&offset)
            && l.number_exact
            && self.by_number.get(&l.number) == Some(&offset)
        {
            self.by_number.remove(&l.number);
        }
    }

    /// Evicts the entries farthest from `center` until the cache fits.
    pub fn trim(&mut self, center: u64) {
        while self.lines.len() > self.capacity {
            let first = self.lines.keys().next().copied();
            let last = self.lines.keys().next_back().copied();
            let (Some(first), Some(last)) = (first, last) else {
                break;
            };
            let victim = if center.abs_diff(first) >= center.abs_diff(last) {
                first
            } else {
                last
            };
            self.remove(victim);
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// Builds lines `first..first+count` of a document whose line `i` has
    /// text `"line {i}"` padded so that every line is `width` bytes long
    /// including the terminator. Numbers are exact.
    pub fn synthetic(first: u64, count: u64, width: u64) -> Vec<Line> {
        (first..first + count)
            .map(|i| Line {
                number: i,
                number_exact: true,
                offset: i * width,
                len: width,
                text: format!("line {i}"),
                truncated: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::synthetic;
    use super::*;

    fn cache_with(first: u64, count: u64) -> LineCache {
        let mut c = LineCache::new(0, 1000);
        assert!(c.insert(0, synthetic(first, count, 10)));
        c
    }

    #[test]
    fn stale_generation_is_rejected() {
        let mut c = LineCache::new(3, 100);
        assert!(!c.insert(2, synthetic(0, 5, 10)));
        assert!(c.is_empty());
        assert!(c.insert(3, synthetic(0, 5, 10)));
        assert_eq!(c.len(), 5);
        c.reset(4);
        assert!(c.is_empty());
        assert_eq!(c.generation(), 4);
    }

    #[test]
    fn neighbours_follow_the_chain() {
        let c = cache_with(5, 4);
        let l6 = c.get(60).unwrap();
        assert_eq!(c.next(l6).unwrap().number, 7);
        assert_eq!(c.prev(60).unwrap().number, 5);
        assert!(c.prev(50).is_none());
        assert!(c.next(c.get(80).unwrap()).is_none());
        assert_eq!(c.get_number(7).unwrap().offset, 70);
    }

    #[test]
    fn prev_requires_contiguity() {
        let mut c = cache_with(0, 2);
        c.insert(0, synthetic(5, 1, 10));
        // Line 5 starts at 50; the cached line before it ends at 20.
        assert!(c.prev(50).is_none());
        assert_eq!(c.chain_end(0), 20);
        assert_eq!(c.chain_start_before(20), 0);
    }

    #[test]
    fn runs_are_counted() {
        let c = cache_with(0, 10);
        assert_eq!(c.run_from(30, 100), 7);
        assert_eq!(c.run_from(30, 3), 3);
        assert_eq!(c.run_before(30, 100), 3);
        assert_eq!(c.run_from(1000, 5), 0);
    }

    #[test]
    fn containing_and_last() {
        let c = cache_with(0, 3);
        assert_eq!(c.containing(15).unwrap().number, 1);
        assert!(c.containing(30).is_none());
        assert_eq!(c.last_line_ending_at(30).unwrap().number, 2);
        assert!(c.last_line_ending_at(31).is_none());
    }

    #[test]
    fn invalidate_from_drops_the_tail() {
        let mut c = cache_with(0, 5);
        c.invalidate_from(50);
        // The last line ends exactly at 50 and may have been unterminated.
        assert_eq!(c.len(), 4);
        assert!(c.get(40).is_none());
        assert!(c.get_number(4).is_none());
    }

    #[test]
    fn inexact_lines_can_be_dropped() {
        let mut c = LineCache::new(0, 100);
        let mut lines = synthetic(0, 4, 10);
        lines[2].number_exact = false;
        c.insert(0, lines);
        c.drop_inexact();
        assert_eq!(c.len(), 3);
        assert!(c.get(20).is_none());
    }

    #[test]
    fn trim_keeps_the_neighbourhood_of_the_centre() {
        let mut c = LineCache::new(0, 20);
        c.insert(0, synthetic(0, 100, 10));
        c.trim(500);
        assert_eq!(c.len(), 20);
        assert!(c.get(500).is_some());
        assert!(c.get(0).is_none());
        assert!(c.get(990).is_none());
        // Numbers of evicted lines are forgotten too.
        assert!(c.get_number(0).is_none());
        assert!(c.get_number(50).is_some());
    }

    #[test]
    fn reinserting_a_line_replaces_it() {
        let mut c = cache_with(0, 3);
        let mut l = synthetic(1, 1, 10).remove(0);
        l.text = "changed".into();
        c.insert(0, vec![l]);
        assert_eq!(c.get(10).unwrap().text, "changed");
        assert_eq!(c.len(), 3);
    }
}
