//! The state of a merged tab: a virtual list over the merged order.
//!
//! The list is a window over [`MergedStore`]: only the rows in view (plus a
//! little overscan) are fetched, per source, by the [`Fetcher`] thread and
//! kept in a bounded cache keyed by the packed `(source, line)` entry. Rows
//! are a fixed height, so scrolling is plain index arithmetic (the pure
//! helpers below, unit-tested). The UI thread never reads a file: a missing
//! row is drawn as a placeholder until the fetcher's answer arrives.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Galley;
use oxtail_core::Line;
use oxtail_search::{CaseMode, Query as SearchQuery, QueryKind};
use oxtail_time::{MergedEntry, jiff::tz::TimeZone};

use crate::highlight::HighlightState;
use crate::merge::{
    Fetcher, MergeOptions, MergeSource, MergeWorker, MergedSearch, MergedStore, plan_reads,
};

/// Rows fetched beyond the visible ones, above and below.
pub const OVERSCAN: usize = 40;
/// Cached lines before the cache is dropped.
pub const CACHE_LIMIT: usize = 60_000;

/// A merged line as fetched.
#[derive(Debug, Clone)]
pub struct MergedLine {
    /// Source index.
    pub source: usize,
    /// The line of the source.
    pub line: Line,
}

/// Clamps a top row index so the window stays inside `len` rows.
pub fn clamp_top(top: usize, len: usize, rows: usize) -> usize {
    top.min(len.saturating_sub(rows.max(1)))
}

/// The top row that shows the last `rows` rows of `len`.
pub fn tail_top(len: usize, rows: usize) -> usize {
    len.saturating_sub(rows.max(1))
}

/// Turns accumulated scroll pixels into whole rows; the remainder stays in
/// `pending`. Positive is down.
pub fn take_rows(pending: &mut f32, row_h: f32) -> i64 {
    if !pending.is_finite() {
        *pending = 0.0;
        return 0;
    }
    if row_h <= 0.0 {
        return 0;
    }
    let rows = (*pending / row_h).trunc();
    *pending -= rows * row_h;
    rows as i64
}

/// Moves `top` by `rows` (negative is up) and clamps.
pub fn scroll_rows(top: usize, rows: i64, len: usize, visible: usize) -> usize {
    let t = if rows < 0 {
        top.saturating_sub(rows.unsigned_abs() as usize)
    } else {
        top.saturating_add(rows as usize)
    };
    clamp_top(t, len, visible)
}

/// The source name for the badge: its index as a letter (`A`, `B`, ...).
pub fn badge_letter(source: usize) -> char {
    char::from(b'A' + (source % 26) as u8)
}

/// The find bar of a merged tab.
pub struct MergedFind {
    /// The bar is open.
    pub open: bool,
    /// Focus the field on the next frame.
    pub focus: bool,
    /// The text.
    pub text: String,
    /// Regular expression.
    pub regex: bool,
    /// Case handling.
    pub case: CaseMode,
    /// The running search.
    pub search: Option<MergedSearch>,
    /// Why the query is invalid.
    pub problem: Option<String>,
    /// The current match (a merged index).
    pub current: Option<usize>,
    /// Restart the search at this time (debounce).
    pub restart_at: Option<Instant>,
}

impl Default for MergedFind {
    fn default() -> Self {
        Self {
            open: false,
            focus: false,
            text: String::new(),
            regex: false,
            case: CaseMode::Smart,
            search: None,
            problem: None,
            current: None,
            restart_at: None,
        }
    }
}

/// A merged tab.
pub struct MergedView {
    /// The merged sources.
    pub sources: Vec<MergeSource>,
    /// The merged order.
    pub store: Arc<MergedStore>,
    /// Follow the end.
    pub follow: bool,
    /// Index of the top row.
    pub top: usize,
    /// The selected rows (anchor, cursor) as merged indices.
    pub selection: Option<(usize, usize)>,
    /// Rows that fit in the view (set by the painter).
    pub visible_rows: usize,
    /// Row height in pixels (set by the painter).
    pub row_h: f32,
    /// The rows drawn in the last frame: index and line, if fetched.
    pub last_rows: Vec<(usize, Option<Arc<MergedLine>>)>,
    /// Fetched lines by packed entry.
    pub cache: HashMap<u64, Arc<MergedLine>>,
    /// Laid-out rows by `(source, line)`, with the epoch they were built for.
    pub galleys: HashMap<(usize, u64), (u64, Arc<Galley>)>,
    /// One highlight state per source (their caches are keyed by offset).
    pub hls: Vec<HighlightState>,
    /// The find bar.
    pub find: MergedFind,
    /// A short message.
    pub toast: Option<(String, Instant)>,
    /// Pixels scrolled but not yet applied.
    pub pending_px: f32,
    /// Horizontal scroll in pixels.
    pub h_scroll: f32,
    /// Widest text drawn so far (horizontal scroll range).
    pub max_text_w: f32,
    /// Time zone for zone-less timestamps (informational).
    pub time_zone: TimeZone,
    fetcher: Fetcher,
    _worker: MergeWorker,
    epoch: u64,
    requested: std::collections::HashSet<u64>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl MergedView {
    /// Starts merging `sources`. `wake` requests a repaint from workers.
    pub fn new(
        sources: Vec<MergeSource>,
        time_zone: TimeZone,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::with_options(
            sources,
            MergeOptions {
                time_zone: time_zone.clone(),
                ..MergeOptions::default()
            },
            time_zone,
            wake,
        )
    }

    /// Like [`MergedView::new`] with explicit merge options (tests use a short
    /// idle delay).
    pub fn with_options(
        sources: Vec<MergeSource>,
        opts: MergeOptions,
        time_zone: TimeZone,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let docs: Vec<_> = sources.iter().map(|s| Arc::clone(&s.doc)).collect();
        let store = Arc::new(MergedStore::default());
        let worker = MergeWorker::start(docs.clone(), Arc::clone(&store), opts, Arc::clone(&wake));
        let fetcher = Fetcher::start(docs, Arc::clone(&wake));
        let hls = sources
            .iter()
            .map(|_| HighlightState::with_defaults())
            .collect();
        Self {
            epoch: store.epoch(),
            sources,
            store,
            follow: true,
            top: 0,
            selection: None,
            visible_rows: 20,
            row_h: 16.0,
            last_rows: Vec::new(),
            cache: HashMap::new(),
            galleys: HashMap::new(),
            hls,
            find: MergedFind::default(),
            toast: None,
            pending_px: 0.0,
            h_scroll: 0.0,
            max_text_w: 0.0,
            time_zone,
            fetcher,
            _worker: worker,
            requested: std::collections::HashSet::new(),
            wake,
        }
    }

    /// The title of the tab.
    pub fn title(&self) -> String {
        let names: Vec<&str> = self.sources.iter().map(|s| s.name.as_str()).collect();
        format!("{} (merged)", names.join(" + "))
    }

    /// Number of merged lines.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether nothing has been merged yet.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Whether the merge is still catching up with the sources.
    pub fn loading(&self) -> bool {
        !self.store.caught_up()
    }

    /// Sets a toast message.
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Scrolls by `dy` pixels (positive is down). Scrolling up pauses
    /// following.
    pub fn scroll_px(&mut self, dy: f32) {
        if !dy.is_finite() || dy == 0.0 {
            return;
        }
        if dy < 0.0 {
            self.follow = false;
        }
        self.pending_px += dy;
    }

    /// One page up or down.
    pub fn page(&mut self, down: bool) {
        let page = (self.visible_rows.saturating_sub(1)).max(1) as f32 * self.row_h;
        self.scroll_px(if down { page } else { -page });
    }

    /// Jumps to the first row.
    pub fn jump_top(&mut self) {
        self.follow = false;
        self.pending_px = 0.0;
        self.top = 0;
    }

    /// Jumps to the last row and follows.
    pub fn jump_bottom(&mut self) {
        self.follow = true;
        self.pending_px = 0.0;
    }

    /// Toggles following.
    pub fn toggle_follow(&mut self) {
        if self.follow {
            self.follow = false;
        } else {
            self.jump_bottom();
        }
    }

    /// Scrolls so that merged row `index` is in the middle of the view.
    pub fn jump_to_row(&mut self, index: usize) {
        self.follow = false;
        self.pending_px = 0.0;
        let half = self.visible_rows / 2;
        self.top = clamp_top(index.saturating_sub(half), self.len(), self.visible_rows);
    }

    /// Selects a row (or extends the selection to it).
    pub fn select(&mut self, index: usize, extend: bool) {
        self.selection = Some(match (extend, self.selection) {
            (true, Some((a, _))) => (a, index),
            _ => (index, index),
        });
    }

    /// The selected range as `(first, last)` merged indices.
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        self.selection.map(|(a, b)| (a.min(b), a.max(b)))
    }

    /// Applies pending scroll, follows the tail, and returns the window of
    /// rows to show as `(top, count)`. Call once per frame after
    /// `visible_rows` and `row_h` are set.
    pub fn update_window(&mut self) -> (usize, usize) {
        let len = self.len();
        let rows = self.visible_rows.max(1);
        let dr = take_rows(&mut self.pending_px, self.row_h);
        if dr != 0 {
            self.top = scroll_rows(self.top, dr, len, rows);
            // Scrolled down to the end: follow again.
            if dr > 0 && self.top >= tail_top(len, rows) {
                self.follow = true;
            }
        }
        if self.follow {
            self.top = tail_top(len, rows);
        } else {
            self.top = clamp_top(self.top, len, rows);
        }
        (self.top, rows.min(len.saturating_sub(self.top)))
    }

    /// The cached line of merged row `index`.
    pub fn line_at(&self, index: usize) -> Option<Arc<MergedLine>> {
        let e = self.store.get(index)?;
        self.cache.get(&e.raw()).cloned()
    }

    /// Drops everything derived from the old merge when the store was
    /// rebuilt.
    fn sync_epoch(&mut self) {
        let e = self.store.epoch();
        if e != self.epoch {
            self.epoch = e;
            self.cache.clear();
            self.galleys.clear();
            self.requested.clear();
            self.selection = None;
            self.top = 0;
            self.find.search = None;
            self.find.current = None;
            self.find.restart_at = Some(Instant::now());
            for h in &mut self.hls {
                h.clear_cache();
            }
        }
    }

    /// Picks up fetched lines and asks for the missing rows around the
    /// window. Never blocks. Returns whether a repaint is wanted.
    pub fn pump(&mut self, now: Instant) -> bool {
        let mut repaint = false;
        self.sync_epoch();
        // Documents nobody else looks at keep queueing events: drop them.
        for s in self.sources.iter().filter(|s| s.owns) {
            while s.doc.events().try_recv().is_ok() {}
        }
        while let Some(f) = self.fetcher.try_recv() {
            repaint = true;
            if f.epoch != self.epoch {
                continue;
            }
            if self.cache.len() + f.lines.len() > CACHE_LIMIT {
                self.cache.clear();
                self.galleys.clear();
            }
            for (source, line) in f.lines {
                if let Some(e) = MergedEntry::new(source, line.number) {
                    self.requested.remove(&e.raw());
                    self.cache
                        .insert(e.raw(), Arc::new(MergedLine { source, line }));
                }
            }
        }
        if let Some((_, at)) = &self.toast
            && now.duration_since(*at) > Duration::from_secs(4)
        {
            self.toast = None;
        }
        self.request_missing();
        self.pump_find(now);
        repaint || self.fetcher.in_flight || !self.store.caught_up()
    }

    fn request_missing(&mut self) {
        if self.fetcher.in_flight {
            return;
        }
        let start = self.top.saturating_sub(OVERSCAN);
        let end = self.top + self.visible_rows + OVERSCAN;
        let missing: Vec<MergedEntry> = self
            .store
            .slice(start..end)
            .into_iter()
            .filter(|e| !self.cache.contains_key(&e.raw()) && !self.requested.contains(&e.raw()))
            .collect();
        if missing.is_empty() {
            return;
        }
        for e in &missing {
            self.requested.insert(e.raw());
        }
        self.fetcher.request(self.epoch, plan_reads(&missing));
    }

    // ------------------------------------------------------------------ find

    /// The query of the find bar.
    fn find_query(&self) -> SearchQuery {
        SearchQuery {
            pattern: self.find.text.clone(),
            kind: if self.find.regex {
                QueryKind::Regex
            } else {
                QueryKind::Literal
            },
            case: self.find.case,
            whole_word: false,
        }
    }

    /// The find text or its options changed: search again shortly.
    pub fn find_changed(&mut self, now: Instant) {
        self.find.restart_at = Some(now + Duration::from_millis(250));
    }

    fn pump_find(&mut self, now: Instant) {
        if let Some(at) = self.find.restart_at
            && now >= at
        {
            self.find.restart_at = None;
            self.find.search = None;
            self.find.current = None;
            self.find.problem = None;
            if !self.find.text.is_empty() && self.find.open {
                let docs = self.sources.iter().map(|s| Arc::clone(&s.doc)).collect();
                match MergedSearch::start(
                    docs,
                    Arc::clone(&self.store),
                    &self.find_query(),
                    Arc::clone(&self.wake),
                ) {
                    Ok(s) => self.find.search = Some(s),
                    Err(e) => self.find.problem = Some(e),
                }
            }
        }
    }

    /// Steps to the next or previous match from the current position.
    pub fn find_step(&mut self, forward: bool) {
        let Some(s) = &self.find.search else {
            return;
        };
        let from = self
            .find
            .current
            .unwrap_or(self.top + self.visible_rows / 2);
        let hit = if forward {
            s.next_from(from + usize::from(self.find.current.is_some()))
        } else {
            s.prev_before(from)
        };
        match hit {
            Some(i) => {
                self.find.current = Some(i);
                self.jump_to_row(i);
            }
            None => self.toast("No matches"),
        }
    }

    /// Whether merged row `index` is a search match.
    pub fn is_match(&self, index: usize) -> bool {
        self.find.open && self.find.search.as_ref().is_some_and(|s| s.contains(index))
    }

    // ------------------------------------------------------------------ copy

    /// The text of the selected rows that are cached, one line each.
    pub fn selection_text(&self) -> Option<String> {
        let (a, b) = self.selected_range()?;
        let mut out = String::new();
        for i in a..=b.min(a + 100_000) {
            if let Some(l) = self.line_at(i) {
                out.push_str(&l.line.text);
                out.push('\n');
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// A tab that shared `doc` was closed: from now on this view drains the
    /// document's events itself.
    pub fn adopt(&mut self, doc: &Arc<oxtail_core::Document>) {
        for s in &mut self.sources {
            if Arc::ptr_eq(&s.doc, doc) {
                s.owns = true;
            }
        }
    }

    /// Bytes per merged line (the memory cost shown in the status bar).
    pub const BYTES_PER_LINE: usize = std::mem::size_of::<MergedEntry>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};

    #[test]
    fn scroll_arithmetic_is_clamped() {
        assert_eq!(clamp_top(50, 100, 10), 50);
        assert_eq!(clamp_top(99, 100, 10), 90);
        assert_eq!(clamp_top(5, 3, 10), 0);
        assert_eq!(tail_top(100, 10), 90);
        assert_eq!(tail_top(4, 10), 0);
        assert_eq!(scroll_rows(10, -20, 100, 10), 0);
        assert_eq!(scroll_rows(10, 5, 100, 10), 15);
        assert_eq!(scroll_rows(10, 500, 100, 10), 90);
    }

    #[test]
    fn pixels_become_whole_rows() {
        let mut p = 50.0;
        assert_eq!(take_rows(&mut p, 16.0), 3);
        assert!((p - 2.0).abs() < 1e-4);
        let mut p = -35.0;
        assert_eq!(take_rows(&mut p, 16.0), -2);
        assert!((p + 3.0).abs() < 1e-4);
        let mut p = f32::NAN;
        assert_eq!(take_rows(&mut p, 16.0), 0);
        assert_eq!(p, 0.0);
        let mut p = 10.0;
        assert_eq!(take_rows(&mut p, 0.0), 0);
    }

    #[test]
    fn badges_are_letters() {
        assert_eq!(badge_letter(0), 'A');
        assert_eq!(badge_letter(3), 'D');
        assert_eq!(badge_letter(26), 'A');
    }

    fn view(lines: u32) -> MergedView {
        let mut a = String::new();
        let mut b = String::new();
        for i in 0..lines {
            let (sa, sb) = (i * 2, i * 2 + 1);
            a.push_str(&format!(
                "2026-09-29 10:{:02}:{:02} a{i}\n",
                sa / 60,
                sa % 60
            ));
            b.push_str(&format!(
                "2026-09-29 10:{:02}:{:02} b{i}\n",
                sb / 60,
                sb % 60
            ));
        }
        let sources = vec![
            MergeSource {
                name: "a.log".into(),
                doc: doc(&a),
                path: None,
                owns: false,
            },
            MergeSource {
                name: "b.log".into(),
                doc: doc(&b),
                path: None,
                owns: false,
            },
        ];
        MergedView::with_options(
            sources,
            MergeOptions {
                idle_delay: Duration::from_millis(30),
                ..MergeOptions::default()
            },
            TimeZone::UTC,
            Arc::new(|| {}),
        )
    }

    fn settle(v: &mut MergedView, n: usize) {
        wait("merged rows", || {
            v.update_window();
            v.pump(Instant::now());
            (v.len() >= n).then_some(())
        });
    }

    #[test]
    fn rows_are_fetched_lazily_around_the_window() {
        // 100 lines per source: 200 merged rows, far more than window plus
        // overscan.
        let mut v = view(100);
        settle(&mut v, 200);
        v.visible_rows = 10;
        v.jump_top();
        wait("rows in view", || {
            let (top, n) = v.update_window();
            v.pump(Instant::now());
            (0..n).all(|k| v.line_at(top + k).is_some()).then_some(())
        });
        assert_eq!(v.top, 0);
        // Interleaved: a0 b0 a1 b1 ...
        assert!(v.line_at(0).unwrap().line.text.ends_with("a0"));
        assert!(v.line_at(1).unwrap().line.text.ends_with("b0"));
        assert_eq!(v.line_at(3).unwrap().source, 1);
        // Rows far from both the tail (followed while settling) and the top
        // are not fetched.
        assert!(v.line_at(100).is_none());
        assert!(v.cache.len() < 150, "{} of 200 rows cached", v.cache.len());
    }

    #[test]
    fn following_shows_the_tail_and_scrolling_up_pauses() {
        let mut v = view(20);
        settle(&mut v, 40);
        v.visible_rows = 10;
        let (top, n) = v.update_window();
        assert_eq!((top, n), (30, 10));
        assert!(v.follow);
        v.scroll_px(-2.5 * v.row_h);
        let (top, _) = v.update_window();
        assert!(!v.follow);
        assert_eq!(top, 28);
        v.scroll_px(100.0 * v.row_h);
        v.update_window();
        assert!(v.follow, "scrolling to the end resumes following");
        v.jump_top();
        assert_eq!(v.update_window().0, 0);
        v.jump_bottom();
        assert_eq!(v.update_window().0, 30);
    }

    #[test]
    fn selection_and_copy_use_cached_rows() {
        let mut v = view(10);
        settle(&mut v, 20);
        v.visible_rows = 20;
        v.jump_top();
        wait("rows", || {
            let (top, n) = v.update_window();
            v.pump(Instant::now());
            (0..n).all(|k| v.line_at(top + k).is_some()).then_some(())
        });
        v.select(2, false);
        v.select(4, true);
        assert_eq!(v.selected_range(), Some((2, 4)));
        let text = v.selection_text().unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.lines().next().unwrap().ends_with("a1"));
    }

    #[test]
    fn find_steps_through_matches_and_wraps() {
        let mut v = view(10);
        settle(&mut v, 20);
        v.visible_rows = 5;
        v.find.open = true;
        v.find.text = "b3".into();
        v.find_changed(Instant::now() - Duration::from_secs(1));
        wait("search", || {
            v.pump(Instant::now());
            v.find
                .search
                .as_ref()
                .is_some_and(|s| s.scanned() >= 20 && s.count() == 1)
                .then_some(())
        });
        v.find_step(true);
        // b3 is merged index 7.
        assert_eq!(v.find.current, Some(7));
        assert!(v.is_match(7) && !v.is_match(6));
        assert!(!v.follow);
        // The only match: stepping again stays on it.
        v.find_step(true);
        assert_eq!(v.find.current, Some(7));
        v.find_step(false);
        assert_eq!(v.find.current, Some(7));
        // An invalid regex is reported.
        v.find.regex = true;
        v.find.text = "(".into();
        v.find_changed(Instant::now() - Duration::from_secs(1));
        v.pump(Instant::now());
        assert!(v.find.problem.is_some());
    }

    #[test]
    fn the_title_lists_the_sources() {
        let v = view(1);
        assert_eq!(v.title(), "a.log + b.log (merged)");
        assert_eq!(MergedView::BYTES_PER_LINE, 8);
    }
}
