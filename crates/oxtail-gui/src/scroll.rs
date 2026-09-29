//! Vertical positioning of the log view: pure functions over a
//! [`LineCache`], independent of egui.
//!
//! The view position is the **start offset of the top line** plus a pixel
//! offset into that line ([`Pos`]). Rows are found by walking from there:
//! forward through `offset + len`, backward through the cache (or, in a
//! filtered view, through the match set). This works identically while the
//! file is still being indexed (line numbers are then only estimates), and for
//! filtered views, which are just another way to get from a row to its
//! neighbour ([`RowSpace`]).

use std::cmp::Ordering;
use std::sync::Arc;

use oxtail_core::{Line, LineRequest};
use oxtail_search::MatchSet;

use crate::linecache::LineCache;

/// Which rows the view shows.
#[derive(Clone, Copy)]
pub enum RowSpace<'a> {
    /// Every line of the document.
    Full {
        /// Length of the UTF-8 view in bytes.
        utf8_len: u64,
    },
    /// Only the lines of a match set (filter view).
    Filtered {
        /// The lines to show.
        set: &'a MatchSet,
    },
}

/// The row before a given row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prev {
    /// It starts at this offset.
    At(u64),
    /// There is none: this is the first row.
    Start,
    /// It is not known without reading more lines.
    Unknown,
}

impl RowSpace<'_> {
    /// Start offset of the row after `line`, or `None` at the last row.
    pub fn next_start(&self, line: &Line) -> Option<u64> {
        match self {
            RowSpace::Full { utf8_len } => {
                let n = line.offset.saturating_add(line.len);
                (n < *utf8_len).then_some(n)
            }
            RowSpace::Filtered { set } => set.next_after(line.offset),
        }
    }

    /// The row before the row starting at `offset`.
    pub fn prev_start(&self, cache: &LineCache, offset: u64) -> Prev {
        match self {
            RowSpace::Full { .. } => match cache.prev(offset) {
                Some(l) => Prev::At(l.offset),
                None if offset == 0 => Prev::Start,
                None => Prev::Unknown,
            },
            RowSpace::Filtered { set } => match set.prev_before(offset) {
                Some(o) => Prev::At(o),
                None => Prev::Start,
            },
        }
    }

    /// Start offset of the first row, if there is one.
    pub fn first_start(&self) -> Option<u64> {
        match self {
            RowSpace::Full { utf8_len } => (*utf8_len > 0).then_some(0),
            RowSpace::Filtered { set } => set.get(0),
        }
    }

    /// Start offset of the last row when it is known without the cache
    /// (filtered views only).
    pub fn last_start(&self) -> Option<u64> {
        match self {
            RowSpace::Full { .. } => None,
            RowSpace::Filtered { set } => set.len().checked_sub(1).and_then(|i| set.get(i)),
        }
    }
}

/// The scroll position: top line plus how many pixels of it are scrolled out.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pos {
    /// Start offset of the top row.
    pub top: u64,
    /// Pixels of the top row above the viewport, `0 <= sub_px < height`.
    pub sub_px: f32,
}

impl Pos {
    /// A position at the start of the line at `top`.
    pub fn at(top: u64) -> Self {
        Self { top, sub_px: 0.0 }
    }

    /// Orders positions top to bottom.
    pub fn cmp_pos(&self, other: &Pos) -> Ordering {
        self.top
            .cmp(&other.top)
            .then(self.sub_px.total_cmp(&other.sub_px))
    }
}

/// Result of [`scroll_by`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollOutcome {
    /// Pixels that could not be applied because lines are not cached yet.
    /// Keep them and apply them again when data arrives.
    pub remainder: f32,
    /// Data is missing after the position (`true`) / before it (`false`),
    /// when `remainder != 0`.
    pub forward: bool,
}

/// Moves `pos` by `dy` pixels (positive scrolls down). Stops silently at the
/// first or last row; stops with a remainder when a needed line is not cached.
pub fn scroll_by(
    pos: &mut Pos,
    dy: f32,
    space: &RowSpace<'_>,
    cache: &LineCache,
    height_of: &dyn Fn(&Line) -> f32,
) -> ScrollOutcome {
    let mut out = ScrollOutcome::default();
    if dy == 0.0 || !dy.is_finite() {
        return out;
    }
    let mut sub = pos.sub_px + dy;
    if dy > 0.0 {
        loop {
            let Some(line) = cache.get(pos.top) else {
                out = ScrollOutcome {
                    remainder: sub - pos.sub_px,
                    forward: true,
                };
                sub = pos.sub_px;
                break;
            };
            let h = height_of(line).max(1.0);
            if sub < h {
                break;
            }
            let stay = (h - 1.0).max(0.0);
            match space.next_start(line) {
                Some(next) if cache.get(next).is_some() => {
                    pos.top = next;
                    sub -= h;
                }
                Some(_) => {
                    // The next line is not cached: stop at the bottom of this
                    // one and keep the rest for when it arrives.
                    out = ScrollOutcome {
                        remainder: sub - stay,
                        forward: true,
                    };
                    sub = stay;
                    break;
                }
                None => {
                    // Last row: nothing below.
                    sub = stay;
                    break;
                }
            }
        }
    } else {
        while sub < 0.0 {
            match space.prev_start(cache, pos.top) {
                Prev::At(o) => match cache.get(o) {
                    Some(l) => {
                        pos.top = o;
                        sub += height_of(l).max(1.0);
                    }
                    None => {
                        out = ScrollOutcome {
                            remainder: sub,
                            forward: false,
                        };
                        sub = 0.0;
                        break;
                    }
                },
                Prev::Start => {
                    sub = 0.0;
                    break;
                }
                Prev::Unknown => {
                    out = ScrollOutcome {
                        remainder: sub,
                        forward: false,
                    };
                    sub = 0.0;
                    break;
                }
            }
        }
    }
    pos.sub_px = sub.max(0.0);
    out
}

/// The position that shows the last row flush with the bottom of a viewport
/// `view_h` pixels high, or `None` while the needed lines are not cached.
pub fn tail_position(
    space: &RowSpace<'_>,
    cache: &LineCache,
    view_h: f32,
    height_of: &dyn Fn(&Line) -> f32,
) -> Option<Pos> {
    let last = match space {
        RowSpace::Full { utf8_len } => Arc::clone(cache.last_line_ending_at(*utf8_len)?),
        RowSpace::Filtered { set } => {
            let o = set.len().checked_sub(1).and_then(|i| set.get(i))?;
            Arc::clone(cache.get(o)?)
        }
    };
    let mut acc = height_of(&last).max(1.0);
    let mut cur = last;
    while acc < view_h {
        match space.prev_start(cache, cur.offset) {
            Prev::At(o) => {
                cur = Arc::clone(cache.get(o)?);
                acc += height_of(&cur).max(1.0);
            }
            Prev::Start => return Some(Pos::at(cur.offset)),
            Prev::Unknown => return None,
        }
    }
    Some(Pos {
        top: cur.offset,
        sub_px: (acc - view_h).max(0.0),
    })
}

/// One row to draw.
#[derive(Debug, Clone)]
pub struct Row {
    /// The line.
    pub line: Arc<Line>,
    /// Top of the row relative to the top of the viewport (negative for the
    /// partly scrolled-out first row).
    pub y: f32,
    /// Row height.
    pub height: f32,
}

/// What [`layout_rows`] found.
#[derive(Debug, Clone, Default)]
pub struct Visible {
    /// Rows from the top of the viewport downward.
    pub rows: Vec<Row>,
    /// The top line is not cached.
    pub top_missing: bool,
    /// The start offset of a row after the last drawn one that is needed but
    /// not cached.
    pub missing_next: Option<u64>,
    /// The last row of the view is among `rows`.
    pub reaches_end: bool,
}

/// Collects the rows that fill a viewport of height `view_h`.
pub fn layout_rows(
    pos: &Pos,
    space: &RowSpace<'_>,
    cache: &LineCache,
    view_h: f32,
    height_of: &dyn Fn(&Line) -> f32,
) -> Visible {
    let mut v = Visible::default();
    let Some(mut line) = cache.get(pos.top).cloned() else {
        v.top_missing = true;
        return v;
    };
    let mut y = -pos.sub_px;
    loop {
        let h = height_of(&line).max(1.0);
        v.rows.push(Row {
            line: Arc::clone(&line),
            y,
            height: h,
        });
        y += h;
        let next = space.next_start(&line);
        if y >= view_h {
            v.reaches_end = next.is_none();
            break;
        }
        match next {
            None => {
                v.reaches_end = true;
                break;
            }
            Some(o) => match cache.get(o) {
                Some(l) => line = Arc::clone(l),
                None => {
                    v.missing_next = Some(o);
                    break;
                }
            },
        }
        if v.rows.len() > 100_000 {
            break;
        }
    }
    v
}

/// A read the view wants to start.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    /// `count` consecutive lines starting at the line at `from`.
    Forward {
        /// Start offset of the first wanted line.
        from: u64,
        /// How many lines.
        count: usize,
    },
    /// `count` consecutive lines ending right before the line at `before`.
    Backward {
        /// Start offset of the line after the last wanted one.
        before: u64,
        /// How many lines.
        count: usize,
    },
    /// Exactly the lines at these offsets (filter views).
    Offsets(Vec<u64>),
}

impl Fetch {
    /// A key identifying the wanted data, used to avoid asking twice.
    pub fn key(&self) -> (u8, u64) {
        match self {
            Fetch::Forward { from, .. } => (0, *from),
            Fetch::Backward { before, .. } => (1, *before),
            Fetch::Offsets(v) => (2, v.first().copied().unwrap_or(0)),
        }
    }

    /// The document request that fetches it. `back_attempts` doubles the
    /// look-back distance of a backward read that cannot use line numbers.
    pub fn to_request(
        &self,
        cache: &LineCache,
        utf8_len: u64,
        back_attempts: u32,
    ) -> Option<LineRequest> {
        match self {
            Fetch::Offsets(v) => Some(LineRequest::AtOffsets(v.clone())),
            Fetch::Forward { from, count } => {
                if *from == 0 {
                    return Some(LineRequest::Range {
                        first: 0,
                        count: *count,
                    });
                }
                match cache.prev(*from) {
                    Some(p) if p.number_exact => Some(LineRequest::Range {
                        first: p.number + 1,
                        count: *count,
                    }),
                    _ => Some(byte_request(*from, utf8_len, *count)),
                }
            }
            Fetch::Backward { before, count } => {
                if *before == 0 {
                    return None;
                }
                match cache.get(*before) {
                    Some(l) if l.number_exact => {
                        let n = l.number;
                        let take = (*count as u64).min(n);
                        (take > 0).then(|| LineRequest::Range {
                            first: n - take,
                            count: take as usize,
                        })
                    }
                    _ => {
                        let avg = average_line_len(cache, *before).max(8);
                        let back = (*count as u64 / 2)
                            .saturating_mul(avg)
                            .saturating_mul(1u64 << back_attempts.min(20));
                        let start = before.saturating_sub(back);
                        Some(byte_request(start, utf8_len, *count))
                    }
                }
            }
        }
    }
}

/// A `ByteFraction` request that starts at the line containing `offset`.
fn byte_request(offset: u64, utf8_len: u64, count: usize) -> LineRequest {
    let len = utf8_len.max(1) as f64;
    // The half byte keeps the result inside the byte despite f64 rounding.
    let fraction = ((offset as f64 + 0.5) / len).clamp(0.0, 1.0);
    LineRequest::ByteFraction { fraction, count }
}

/// Average length of the cached lines near `offset` (100 if none).
fn average_line_len(cache: &LineCache, offset: u64) -> u64 {
    let start = cache.chain_end(offset);
    let n = cache.run_from(offset, 50) as u64;
    if n == 0 {
        return 100;
    }
    (start.saturating_sub(offset) / n).max(1)
}

/// Lines wanted around the viewport, as a multiple of its row count.
pub const OVERSCAN_ROWS: usize = 60;
/// Smallest read.
pub const MIN_FETCH: usize = 120;

/// Decides which reads are needed so that the viewport and some overscan are
/// cached. `visible` is the result of [`layout_rows`] for the current position.
pub fn plan_fetch(
    pos: &Pos,
    visible: &Visible,
    space: &RowSpace<'_>,
    cache: &LineCache,
    view_rows: usize,
    is_pending: &dyn Fn(&Fetch) -> bool,
) -> Vec<Fetch> {
    let mut out = Vec::new();
    let mut push = |f: Fetch| {
        if !is_pending(&f) && !out.contains(&f) {
            out.push(f);
        }
    };
    let want_after = view_rows + OVERSCAN_ROWS;
    let want_before = OVERSCAN_ROWS;
    match space {
        RowSpace::Full { utf8_len } => {
            if visible.top_missing {
                push(Fetch::Forward {
                    from: pos.top,
                    count: want_after.max(MIN_FETCH),
                });
                return out;
            }
            let run = cache.run_from(pos.top, want_after);
            if run < want_after {
                let end = cache.chain_end(pos.top);
                if end < *utf8_len {
                    push(Fetch::Forward {
                        from: end,
                        count: (want_after - run).max(MIN_FETCH),
                    });
                }
            }
            let run_b = cache.run_before(pos.top, want_before);
            if run_b < want_before {
                let start = cache.chain_start_before(pos.top);
                if start > 0 {
                    push(Fetch::Backward {
                        before: start,
                        count: (want_before - run_b).max(MIN_FETCH),
                    });
                }
            }
        }
        RowSpace::Filtered { set } => {
            let mut missing = Vec::new();
            let mut collect = |o: u64| {
                if cache.get(o).is_none() && !missing.contains(&o) {
                    missing.push(o);
                }
            };
            // Forward from the top row.
            let mut at = Some(pos.top);
            for _ in 0..want_after {
                let Some(o) = at else { break };
                collect(o);
                at = set.next_after(o);
            }
            // Backward.
            let mut at = pos.top;
            for _ in 0..want_before {
                match set.prev_before(at) {
                    Some(o) => {
                        collect(o);
                        at = o;
                    }
                    None => break,
                }
            }
            missing.sort_unstable();
            for chunk in missing.chunks(2000) {
                push(Fetch::Offsets(chunk.to_vec()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linecache::testutil::synthetic;

    const W: u64 = 10;

    fn cache(first: u64, count: u64) -> LineCache {
        let mut c = LineCache::new(0, 10_000);
        c.insert(0, synthetic(first, count, W));
        c
    }

    fn h(_: &Line) -> f32 {
        10.0
    }

    fn full(lines: u64) -> RowSpace<'static> {
        RowSpace::Full {
            utf8_len: lines * W,
        }
    }

    #[test]
    fn scrolls_down_by_whole_and_partial_rows() {
        let c = cache(0, 100);
        let space = full(100);
        let mut p = Pos::at(0);
        let o = scroll_by(&mut p, 25.0, &space, &c, &h);
        assert_eq!(o.remainder, 0.0);
        assert_eq!(p.top, 20);
        assert_eq!(p.sub_px, 5.0);
        let o = scroll_by(&mut p, -12.0, &space, &c, &h);
        assert_eq!(o.remainder, 0.0);
        assert_eq!(p.top, 10);
        assert_eq!(p.sub_px, 3.0);
    }

    #[test]
    fn scrolling_up_stops_at_the_first_row() {
        let c = cache(0, 10);
        let mut p = Pos {
            top: 20,
            sub_px: 4.0,
        };
        let o = scroll_by(&mut p, -1000.0, &full(10), &c, &h);
        assert_eq!(o, ScrollOutcome::default());
        assert_eq!(p, Pos::at(0));
    }

    #[test]
    fn scrolling_down_stops_at_the_last_row() {
        let c = cache(0, 10);
        let mut p = Pos::at(0);
        let o = scroll_by(&mut p, 10_000.0, &full(10), &c, &h);
        assert_eq!(o.remainder, 0.0);
        assert_eq!(p.top, 90);
        assert!(p.sub_px <= 10.0);
    }

    #[test]
    fn missing_lines_leave_a_remainder() {
        let c = cache(0, 5);
        let space = full(100);
        let mut p = Pos::at(0);
        let o = scroll_by(&mut p, 100.0, &space, &c, &h);
        assert!(o.forward);
        assert!(o.remainder > 0.0);
        // The position advanced only as far as the data goes.
        assert_eq!(p.top, 40);
        // Re-applying the remainder after the data arrives finishes the job.
        let mut c = c;
        c.insert(0, synthetic(5, 50, W));
        let o2 = scroll_by(&mut p, o.remainder, &space, &c, &h);
        assert_eq!(o2.remainder, 0.0);
        assert_eq!(p, Pos::at(100));
    }

    #[test]
    fn scrolling_up_into_the_unknown_reports_backward() {
        let c = cache(10, 5);
        let mut p = Pos::at(100);
        let o = scroll_by(&mut p, -30.0, &full(100), &c, &h);
        assert!(!o.forward);
        assert!(o.remainder < 0.0);
        assert_eq!(p.top, 100);
    }

    #[test]
    fn layout_fills_the_viewport() {
        let c = cache(0, 100);
        let p = Pos {
            top: 30,
            sub_px: 4.0,
        };
        let v = layout_rows(&p, &full(100), &c, 55.0, &h);
        assert!(!v.top_missing);
        assert_eq!(v.rows.first().unwrap().y, -4.0);
        assert_eq!(v.rows.first().unwrap().line.number, 3);
        // -4, 6, 16, 26, 36, 46 -> six rows cover 55px.
        assert_eq!(v.rows.len(), 6);
        assert!(!v.reaches_end);
        assert!(v.missing_next.is_none());
    }

    #[test]
    fn layout_reports_missing_rows() {
        let c = cache(0, 3);
        let v = layout_rows(&Pos::at(0), &full(100), &c, 100.0, &h);
        assert_eq!(v.rows.len(), 3);
        assert_eq!(v.missing_next, Some(30));
        let v = layout_rows(&Pos::at(500), &full(100), &c, 100.0, &h);
        assert!(v.top_missing);
    }

    #[test]
    fn layout_marks_the_end() {
        let c = cache(0, 4);
        let v = layout_rows(&Pos::at(0), &full(4), &c, 1000.0, &h);
        assert_eq!(v.rows.len(), 4);
        assert!(v.reaches_end);
    }

    #[test]
    fn tail_position_aligns_the_last_row_with_the_bottom() {
        let c = cache(0, 100);
        let p = tail_position(&full(100), &c, 55.0, &h).unwrap();
        // Rows of 10px: 5.5 rows visible, so the top row is 4.5 rows above
        // the last one, scrolled out by 5px.
        assert_eq!(p.top, 94 * W - 0);
        assert_eq!(p.sub_px, 5.0);
        let v = layout_rows(&p, &full(100), &c, 55.0, &h);
        assert!(v.reaches_end);
        let last = v.rows.last().unwrap();
        assert_eq!(last.y + last.height, 55.0);
    }

    #[test]
    fn tail_position_of_a_short_file_is_the_start() {
        let c = cache(0, 3);
        let p = tail_position(&full(3), &c, 500.0, &h).unwrap();
        assert_eq!(p, Pos::at(0));
    }

    #[test]
    fn tail_position_needs_the_tail_in_cache() {
        let c = cache(0, 10);
        assert!(tail_position(&full(100), &c, 55.0, &h).is_none());
    }

    #[test]
    fn plan_asks_for_the_top_when_missing() {
        let c = LineCache::new(0, 100);
        let p = Pos::at(300);
        let v = layout_rows(&p, &full(100), &c, 100.0, &h);
        let plan = plan_fetch(&p, &v, &full(100), &c, 10, &|_| false);
        assert_eq!(
            plan,
            vec![Fetch::Forward {
                from: 300,
                count: MIN_FETCH.max(10 + OVERSCAN_ROWS)
            }]
        );
    }

    #[test]
    fn plan_reads_ahead_and_behind_but_not_past_the_ends() {
        let c = cache(50, 20);
        let p = Pos::at(50 * W);
        let v = layout_rows(&p, &full(1000), &c, 100.0, &h);
        let plan = plan_fetch(&p, &v, &full(1000), &c, 10, &|_| false);
        assert!(plan.contains(&Fetch::Forward {
            from: 70 * W,
            count: MIN_FETCH
        }));
        assert!(
            plan.iter()
                .any(|f| matches!(f, Fetch::Backward { before, .. } if *before == 50 * W))
        );
        // At the start of the file there is nothing to read backwards.
        let c = cache(0, 200);
        let p = Pos::at(0);
        let v = layout_rows(&p, &full(200), &c, 100.0, &h);
        let plan = plan_fetch(&p, &v, &full(200), &c, 10, &|_| false);
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn pending_reads_are_not_repeated() {
        let c = LineCache::new(0, 100);
        let p = Pos::at(0);
        let v = layout_rows(&p, &full(100), &c, 100.0, &h);
        let plan = plan_fetch(&p, &v, &full(100), &c, 10, &|f| f.key() == (0, 0));
        assert!(plan.is_empty());
    }

    #[test]
    fn forward_fetch_uses_line_numbers_when_known() {
        let c = cache(0, 10);
        let f = Fetch::Forward {
            from: 100,
            count: 50,
        };
        assert_eq!(
            f.to_request(&c, 10_000, 0),
            Some(LineRequest::Range {
                first: 10,
                count: 50
            })
        );
        // Unknown predecessor: fall back to a byte position inside the line.
        let f = Fetch::Forward {
            from: 500,
            count: 50,
        };
        match f.to_request(&c, 1000, 0) {
            Some(LineRequest::ByteFraction { fraction, count }) => {
                assert_eq!(count, 50);
                assert!(((fraction * 1000.0) as u64) == 500);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            Fetch::Forward { from: 0, count: 5 }.to_request(&c, 1000, 0),
            Some(LineRequest::Range { first: 0, count: 5 })
        );
    }

    #[test]
    fn backward_fetch_uses_numbers_and_clamps() {
        let c = cache(30, 10);
        let f = Fetch::Backward {
            before: 30 * W,
            count: 100,
        };
        assert_eq!(
            f.to_request(&c, 10_000, 0),
            Some(LineRequest::Range {
                first: 0,
                count: 30
            })
        );
        let c = cache(0, 10);
        assert_eq!(
            Fetch::Backward {
                before: 0,
                count: 5
            }
            .to_request(&c, 100, 0),
            None
        );
    }

    #[test]
    fn inexact_backward_fetch_looks_further_back_on_retries() {
        let mut c = LineCache::new(0, 100);
        let mut lines = synthetic(200, 10, W);
        for l in &mut lines {
            l.number_exact = false;
        }
        c.insert(0, lines);
        let f = Fetch::Backward {
            before: 200 * W,
            count: 100,
        };
        let frac = |r: Option<LineRequest>| match r {
            Some(LineRequest::ByteFraction { fraction, .. }) => fraction,
            other => panic!("{other:?}"),
        };
        let a = frac(f.to_request(&c, 100_000, 0));
        let b = frac(f.to_request(&c, 100_000, 3));
        assert!(b < a, "{b} !< {a}");
    }

    #[test]
    fn filtered_rows_walk_the_match_set() {
        // Matches at lines 2, 5, 9; the cache holds all lines.
        let set = MatchSet::from_offsets(vec![2 * W, 5 * W, 9 * W]);
        let c = cache(0, 20);
        let space = RowSpace::Filtered { set: &set };
        let v = layout_rows(&Pos::at(2 * W), &space, &c, 100.0, &h);
        let numbers: Vec<u64> = v.rows.iter().map(|r| r.line.number).collect();
        assert_eq!(numbers, vec![2, 5, 9]);
        assert!(v.reaches_end);
        let mut p = Pos::at(9 * W);
        scroll_by(&mut p, -10.0, &space, &c, &h);
        assert_eq!(p.top, 5 * W);
        let t = tail_position(&space, &c, 15.0, &h).unwrap();
        assert_eq!(t.top, 5 * W);
        assert_eq!(t.sub_px, 5.0);
    }

    #[test]
    fn filtered_plan_asks_for_missing_offsets_only() {
        let set = MatchSet::from_offsets((0..500).map(|i| i * 3 * W).collect());
        let mut c = LineCache::new(0, 10_000);
        c.insert(0, synthetic(0, 1, W));
        let space = RowSpace::Filtered { set: &set };
        let p = Pos::at(0);
        let v = layout_rows(&p, &space, &c, 100.0, &h);
        let plan = plan_fetch(&p, &v, &space, &c, 10, &|_| false);
        assert_eq!(plan.len(), 1);
        match &plan[0] {
            Fetch::Offsets(o) => {
                assert_eq!(o.len(), 10 + OVERSCAN_ROWS - 1);
                assert!(!o.contains(&0));
            }
            other => panic!("{other:?}"),
        }
    }
}
