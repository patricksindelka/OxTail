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

use crate::filter::{FilterEntry, FilterState};
use crate::highlight::HighlightState;
use crate::merge::{
    Fetcher, MergeOptions, MergeSource, MergeWorker, MergedSearch, MergedStore, plan_reads,
};
use crate::mergefilter::MergedFilter;

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
    /// The selected rows (anchor, cursor) as merged indices (not filtered
    /// rows, so the selection survives switching the filter).
    pub selection: Option<(usize, usize)>,
    /// Rows that fit in the view (set by the painter).
    pub visible_rows: usize,
    /// Row height in pixels (set by the painter).
    pub row_h: f32,
    /// The rows drawn in the last frame: merged index and line, if fetched.
    pub last_rows: Vec<(usize, Option<Arc<MergedLine>>)>,
    /// Fetched lines by packed entry.
    pub cache: HashMap<u64, Arc<MergedLine>>,
    /// Laid-out rows by `(source, line)`, with the epoch they were built for.
    pub galleys: HashMap<(usize, u64), (u64, Arc<Galley>)>,
    /// One highlight state per source (their caches are keyed by offset).
    pub hls: Vec<HighlightState>,
    /// The find bar.
    pub find: MergedFind,
    /// The filter panel and its entries (column queries are refused: the
    /// sources may have different parsers).
    pub filter: FilterState,
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
    filter_job: Option<MergedFilter>,
    filter_sig: String,
    /// Find continues here (probe, rows examined, forward) after a step ran
    /// out of its budget.
    find_resume: Option<(usize, usize, bool)>,
    /// The merged line to bring back into view once the filter has reached it.
    pending_anchor: Option<usize>,
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
        let mut filter = FilterState::default();
        filter.no_queries = true;
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
            filter,
            toast: None,
            pending_px: 0.0,
            h_scroll: 0.0,
            max_text_w: 0.0,
            time_zone,
            fetcher,
            filter_job: None,
            filter_sig: String::new(),
            find_resume: None,
            pending_anchor: None,
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

    /// Number of merged lines (all of them, whatever the filter shows).
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// The running filter job, when its results are the ones to show: the
    /// user wants the filtered view and the job belongs to the current merge.
    fn active_job(&self) -> Option<&MergedFilter> {
        if !self.filter.enabled {
            return None;
        }
        self.filter_job
            .as_ref()
            .filter(|j| j.epoch == self.store.epoch())
    }

    /// Whether the rows shown are the filtered ones.
    pub fn filter_view_active(&self) -> bool {
        self.active_job().is_some()
    }

    /// Number of rows in the view: the filtered ones while the filter view
    /// is active, else every merged line.
    pub fn row_count(&self) -> usize {
        match self.active_job() {
            Some(j) => j.count(),
            // The job belongs to a merge that was rebuilt: nothing to show
            // until the new job starts.
            None if self.filter.enabled && self.filter_job.is_some() => 0,
            None => self.store.len(),
        }
    }

    /// Passing lines found so far and whether the filter has finished, while
    /// the filter view is active.
    pub fn filter_progress(&self) -> Option<(usize, bool)> {
        self.active_job().map(|j| (j.count(), j.done()))
    }

    /// The filter gave up on lines beyond what it can index (shown in the
    /// status bar).
    pub fn filter_truncated(&self) -> bool {
        self.active_job().is_some_and(MergedFilter::truncated)
    }

    /// The merged index of view row `row`.
    pub fn merged_index(&self, row: usize) -> Option<usize> {
        match self.active_job() {
            Some(j) => j.get(row),
            None => (row < self.store.len()).then_some(row),
        }
    }

    /// The view row of merged index `index`: its row if it is shown, else the
    /// next shown one.
    pub fn row_of(&self, index: usize) -> usize {
        match self.active_job() {
            Some(j) => j.lower_bound(index),
            None => index,
        }
    }

    /// Whether merged index `index` is one of the rows shown.
    pub fn is_shown(&self, index: usize) -> bool {
        match self.active_job() {
            Some(j) => j.row_of(index).is_some(),
            None => index < self.store.len(),
        }
    }

    /// The rows `rows` of the view as `(merged index, entry)`.
    pub fn entries_in(&self, rows: std::ops::Range<usize>) -> Vec<(usize, MergedEntry)> {
        match self.active_job() {
            Some(j) => j
                .slice(rows)
                .into_iter()
                .filter_map(|i| self.store.get(i).map(|e| (i, e)))
                .collect(),
            None => {
                let start = rows.start;
                self.store
                    .slice(rows)
                    .into_iter()
                    .enumerate()
                    .map(|(k, e)| (start + k, e))
                    .collect()
            }
        }
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
        self.pending_anchor = None;
        self.pending_px += dy;
    }

    /// One page up or down.
    pub fn page(&mut self, down: bool) {
        let page = (self.visible_rows.saturating_sub(1)).max(1) as f32 * self.row_h;
        self.scroll_px(if down { page } else { -page });
    }

    /// Jumps to the first row.
    pub fn jump_top(&mut self) {
        self.pending_anchor = None;
        self.follow = false;
        self.pending_px = 0.0;
        self.top = 0;
    }

    /// Jumps to the last row and follows.
    pub fn jump_bottom(&mut self) {
        self.pending_anchor = None;
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

    /// Scrolls so that merged line `index` is in the middle of the view (in
    /// the filter view: the nearest shown row at or after it).
    pub fn jump_to_row(&mut self, index: usize) {
        self.pending_anchor = None;
        self.find_resume = None;
        self.follow = false;
        self.pending_px = 0.0;
        let half = self.visible_rows / 2;
        self.top = clamp_top(
            self.row_of(index).saturating_sub(half),
            self.row_count(),
            self.visible_rows,
        );
    }

    /// Selects every row shown.
    pub fn select_all(&mut self) {
        let n = self.row_count();
        if let (Some(first), Some(last)) = (
            self.merged_index(0),
            n.checked_sub(1).and_then(|l| self.merged_index(l)),
        ) {
            self.selection = Some((first, last));
        }
    }

    /// The merged index the user is at: the selection's cursor, else the
    /// middle row of the view.
    fn cursor_index(&self) -> Option<usize> {
        self.selection.map(|(_, c)| c).or_else(|| {
            let mid = self.top + self.visible_rows / 2;
            self.merged_index(mid)
                .or_else(|| self.merged_index(self.row_count().saturating_sub(1)))
        })
    }

    /// Switches between the filtered and the full view. Going back to the
    /// full view keeps the line the user is at in view; following continues.
    pub fn set_filter_view(&mut self, enabled: bool) {
        if self.filter.enabled == enabled {
            return;
        }
        let at = self.cursor_index();
        let was_following = self.follow;
        self.filter.enabled = enabled;
        if !enabled {
            self.filter_job = None;
            self.filter_sig.clear();
        }
        self.pending_px = 0.0;
        self.find_resume = None;
        self.pending_anchor = None;
        if was_following {
            self.follow = true;
        } else if let Some(i) = at {
            if enabled {
                // The filter has to reach the line first: `pump` brings it
                // into view then.
                self.pending_anchor = Some(i);
            } else {
                self.jump_to_row(i);
            }
        }
        self.filter.changed();
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
        let len = self.row_count();
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
            // The job belongs to the old merge; `pump_filter` starts a new one.
            self.filter_job = None;
            self.filter_sig.clear();
            self.find_resume = None;
            self.pending_anchor = None;
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
        let filtering = self.pump_filter(now);
        self.resolve_anchor();
        self.request_missing();
        self.pump_find(now);
        repaint || filtering || self.fetcher.in_flight || !self.store.caught_up()
    }

    // ---------------------------------------------------------------- filter

    /// The filter entries changed (typing): the job restarts after a pause.
    pub fn filter_edited(&mut self, now: Instant) {
        self.filter.edited(now);
    }

    /// Opens or closes the filter panel; opening it makes sure there is an
    /// entry to type into.
    pub fn toggle_filter_panel(&mut self) {
        self.filter.open = !self.filter.open;
        if self.filter.open {
            if self.filter.entries.is_empty() {
                self.filter.entries.push(FilterEntry::default());
            }
            self.filter.focus_last = true;
        }
    }

    /// Starts, restarts or stops the filter job to match the entries and the
    /// merge. Returns whether the UI should keep repainting (the job is
    /// running, or a restart is waiting for the typing pause).
    fn pump_filter(&mut self, now: Instant) -> bool {
        if !self.filter.enabled || !self.filter.has_entries() {
            self.filter.problems.clear();
            if self.filter_job.take().is_some() {
                self.filter_sig.clear();
            }
            return false;
        }
        let sig = self.filter.entries_signature();
        // `filter_sig` is what the current job (or the decision not to run one)
        // was made for; it is cleared whenever that has to be redone.
        if sig != self.filter_sig {
            if let Some(t) = self.filter.hold_until
                && now < t
            {
                return true;
            }
            self.filter.hold_until = None;
            // Keep the user's place across the restart (the old rows are
            // replaced by an empty set that fills up again).
            if !self.follow && self.pending_anchor.is_none() {
                self.pending_anchor = self.cursor_index();
            }
            let (stack, problems) = self.filter.build_stack(None);
            self.filter.problems = problems;
            self.filter.epoch += 1;
            self.filter_sig = sig;
            if stack.filters.is_empty() {
                // Nothing valid to filter by: the full view.
                self.filter_job = None;
                return false;
            }
            let docs = self.sources.iter().map(|s| Arc::clone(&s.doc)).collect();
            self.filter_job = Some(MergedFilter::start(
                docs,
                Arc::clone(&self.store),
                stack,
                Arc::clone(&self.wake),
            ));
        }
        self.filter_job.as_ref().is_some_and(|j| !j.done())
    }

    /// Brings the remembered line back into view once the filter has looked
    /// at it (or at everything).
    fn resolve_anchor(&mut self) {
        let Some(a) = self.pending_anchor else { return };
        let Some(j) = self.active_job() else {
            if !self.filter.enabled {
                self.pending_anchor = None;
            }
            return;
        };
        if j.done() || j.scanned() > a {
            self.jump_to_row(a);
        }
    }

    /// When the typing pause of the filter ends, if a restart is waiting.
    pub fn filter_hold_until(&self) -> Option<Instant> {
        self.filter.hold_until
    }

    fn request_missing(&mut self) {
        if self.fetcher.in_flight {
            return;
        }
        let start = self.top.saturating_sub(OVERSCAN);
        let end = self.top + self.visible_rows + OVERSCAN;
        let missing: Vec<MergedEntry> = self
            .entries_in(start..end)
            .into_iter()
            .map(|(_, e)| e)
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
        self.find_resume = None;
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

    /// Steps to the next or previous match from the current position. In the
    /// filter view matches in hidden rows are skipped; the walk over the
    /// filtered rows is bounded per call (a further press continues it).
    pub fn find_step(&mut self, forward: bool) {
        let from = self.find.current.unwrap_or_else(|| {
            let mid = self.top + self.visible_rows / 2;
            self.merged_index(mid).unwrap_or(0)
        });
        let probe = if forward {
            from + usize::from(self.find.current.is_some())
        } else {
            from
        };
        let resume = self.find_resume.take().filter(|r| r.2 == forward);
        let outcome = {
            let Some(s) = &self.find.search else {
                return;
            };
            match self.active_job() {
                None => match if forward {
                    s.next_from(probe)
                } else {
                    s.prev_before(probe)
                } {
                    Some(i) => FindProbe::Hit(i),
                    None => FindProbe::Exhausted,
                },
                Some(j) => {
                    let (probe, examined) = resume.map_or((probe, 0), |r| (r.0, r.1));
                    probe_filtered(s, j, forward, probe, examined, FIND_BATCH, FIND_BUDGET)
                }
            }
        };
        match outcome {
            FindProbe::Hit(i) => {
                self.find.current = Some(i);
                self.jump_to_row(i);
            }
            FindProbe::Budget { probe, examined } => {
                self.find_resume = Some((probe, examined, forward));
                self.toast("Still searching\u{2026} press again to continue");
            }
            FindProbe::Exhausted => {
                let searching = self
                    .find
                    .search
                    .as_ref()
                    .is_some_and(|s| s.scanned() < self.store.len() as u64)
                    || self.filter_progress().is_some_and(|(_, done)| !done);
                self.toast(if searching {
                    "Still searching\u{2026}"
                } else {
                    "No matches"
                });
            }
        }
    }

    /// Whether merged row `index` is a search match.
    pub fn is_match(&self, index: usize) -> bool {
        self.find.open && self.find.search.as_ref().is_some_and(|s| s.contains(index))
    }

    // ------------------------------------------------------------------ copy

    /// The text of the selected rows that are cached, one line each. In the
    /// filter view only the rows shown are copied.
    pub fn selection_text(&self) -> Option<String> {
        let (a, b) = self.selected_range()?;
        let (first, end) = (self.row_of(a), self.row_of(b.saturating_add(1)));
        let mut out = String::new();
        for (i, _) in self.entries_in(first..end.min(first + 100_000)) {
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

/// Filtered rows examined per Find step.
const FIND_BUDGET: usize = 100_000;
/// Filtered rows fetched (one lock) per batch.
const FIND_BATCH: usize = 4096;

/// The result of one Find step over the filtered rows.
enum FindProbe {
    /// The merged index of the match.
    Hit(usize),
    /// Every row was examined: no visible match.
    Exhausted,
    /// The per-step budget ran out; continue from `probe` (a merged index)
    /// having examined `examined` rows so far.
    Budget { probe: usize, examined: usize },
}

/// Looks for the next (`forward`) or previous match among the rows the filter
/// shows, starting at merged index `probe` and wrapping round once. Each
/// batch takes each lock once.
fn probe_filtered(
    s: &MergedSearch,
    j: &MergedFilter,
    forward: bool,
    probe: usize,
    mut examined: usize,
    batch: usize,
    budget: usize,
) -> FindProbe {
    let n = j.count();
    let start = j.lower_bound(probe);
    // Forward: rows `cursor..`; backward: rows before `cursor`.
    let mut cursor = if forward {
        if start >= n { 0 } else { start }
    } else if start == 0 {
        n
    } else {
        start
    };
    let mut spent = 0;
    while examined < n {
        if spent >= budget {
            // `cursor` is the next unexamined row (forward) or the row after
            // the next one (backward); either way its index restarts here.
            let probe = j
                .get(cursor)
                .unwrap_or(if forward { 0 } else { usize::MAX });
            return FindProbe::Budget { probe, examined };
        }
        let take = batch.max(1).min(n - examined);
        let (lo, hi) = if forward {
            (cursor, (cursor + take).min(n))
        } else {
            (cursor.saturating_sub(take), cursor)
        };
        let rows = j.slice(lo..hi);
        if rows.is_empty() {
            break;
        }
        if let Some(i) = s.first_match_in(&rows, forward) {
            return FindProbe::Hit(i);
        }
        examined += rows.len();
        spent += rows.len();
        cursor = if forward {
            if hi >= n { 0 } else { hi }
        } else if lo == 0 {
            n
        } else {
            lo
        };
    }
    FindProbe::Exhausted
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

    /// Shows only the lines of source a (" a" matches the tag).
    fn only_a(v: &mut MergedView, rows: usize) {
        v.filter.entries = vec![FilterEntry::include(" a")];
        v.filter.changed();
        wait("filter done", || {
            v.update_window();
            v.pump(Instant::now());
            (v.filter_progress() == Some((rows, true))).then_some(())
        });
    }

    fn fetch_window(v: &mut MergedView) {
        wait("rows in view", || {
            let (top, n) = v.update_window();
            v.pump(Instant::now());
            (top..top + n)
                .all(|r| v.merged_index(r).is_some_and(|i| v.line_at(i).is_some()))
                .then_some(())
        });
    }

    #[test]
    fn filtered_rows_scroll_follow_select_and_copy() {
        let mut v = view(10);
        settle(&mut v, 20);
        v.visible_rows = 4;
        only_a(&mut v, 10);
        assert!(v.filter_view_active());
        assert_eq!((v.row_count(), v.len()), (10, 20));
        // Following: the tail of the filtered rows (a6..a9 = merged 12..18).
        assert_eq!(v.update_window(), (6, 4));
        let shown: Vec<usize> = v.entries_in(6..10).iter().map(|(i, _)| *i).collect();
        assert_eq!(shown, vec![12, 14, 16, 18]);
        fetch_window(&mut v);
        assert!(v.line_at(12).unwrap().line.text.ends_with("a6"));
        // Scrolling works in rows, and up pauses following.
        v.scroll_px(-2.0 * v.row_h);
        assert_eq!(v.update_window().0, 4);
        assert!(!v.follow);
        v.jump_top();
        assert_eq!(v.update_window().0, 0);
        v.jump_bottom();
        assert_eq!(v.update_window().0, 6);
        // Selection is by merged index; copying skips the hidden rows.
        v.jump_top();
        fetch_window(&mut v);
        v.select(2, false);
        v.select(6, true);
        let text = v.selection_text().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].ends_with("a1") && lines[2].ends_with("a3"));
        // Select all covers the rows shown only.
        v.select_all();
        assert_eq!(v.selected_range(), Some((0, 18)));
        v.visible_rows = 20;
        v.jump_top();
        fetch_window(&mut v);
        let text = v.selection_text().unwrap();
        assert_eq!(text.lines().count(), 10);
        assert!(text.lines().all(|l| l.contains(" a")));
        // Jumping to a merged line centres its row.
        v.visible_rows = 4;
        v.jump_to_row(14);
        assert_eq!(v.top, 5);
    }

    #[test]
    fn switching_the_filter_off_keeps_the_position() {
        let mut v = view(50);
        settle(&mut v, 100);
        v.visible_rows = 10;
        only_a(&mut v, 50);
        v.jump_to_row(60);
        v.update_window();
        let at = v.cursor_index().unwrap();
        assert!((58..=62).contains(&at), "{at}");
        v.set_filter_view(false);
        assert!(!v.filter_view_active());
        assert!(!v.follow);
        let (top, n) = v.update_window();
        assert!(top <= at && at < top + n, "{at} not in {top}..{}", top + n);
        assert_eq!(v.row_count(), 100);
        // A selection survives both ways.
        v.select(61, false);
        v.set_filter_view(true);
        only_a_wait(&mut v, 50);
        assert_eq!(v.selected_range(), Some((61, 61)));
        // Following stays on across the switch.
        v.jump_bottom();
        v.set_filter_view(false);
        assert!(v.follow);
        assert_eq!(v.update_window().0, 90);
    }

    fn only_a_wait(v: &mut MergedView, rows: usize) {
        wait("filter done", || {
            v.update_window();
            v.pump(Instant::now());
            (v.filter_progress() == Some((rows, true))).then_some(())
        });
    }

    #[test]
    fn find_skips_rows_the_filter_hides() {
        let mut v = view(10);
        settle(&mut v, 20);
        v.visible_rows = 20;
        only_a(&mut v, 10);
        v.find.open = true;
        v.find.regex = true;
        v.find.text = "[ab]3".into();
        v.find_changed(Instant::now() - Duration::from_secs(1));
        wait("search", || {
            v.pump(Instant::now());
            v.find
                .search
                .as_ref()
                .is_some_and(|s| s.scanned() >= 20 && s.count() == 2)
                .then_some(())
        });
        // a3 is merged 6, b3 is merged 7 and hidden.
        v.find_step(true);
        assert_eq!(v.find.current, Some(6));
        v.find_step(true);
        assert_eq!(v.find.current, Some(6));
        v.find_step(false);
        assert_eq!(v.find.current, Some(6));
        // Only hidden matches: nothing to step to.
        v.find.current = None;
        v.find.text = "b3".into();
        v.find.regex = false;
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
        assert_eq!(v.find.current, None);
        assert!(v.toast.is_some());
    }

    fn wait_search(v: &mut MergedView, pattern: &str, regex: bool, count: usize) {
        v.find.open = true;
        v.find.current = None;
        v.find.regex = regex;
        v.find.text = pattern.into();
        v.find_changed(Instant::now() - Duration::from_secs(1));
        wait("search", || {
            v.pump(Instant::now());
            v.find
                .search
                .as_ref()
                .is_some_and(|s| s.scanned() >= 20 && s.count() == count)
                .then_some(())
        });
    }

    #[test]
    fn the_filtered_find_walk_wraps_and_is_bounded() {
        let mut v = view(10);
        settle(&mut v, 20);
        only_a(&mut v, 10);
        wait_search(&mut v, "[ab]3", true, 2);
        let (s, j) = (v.find.search.as_ref().unwrap(), v.active_job().unwrap());
        // Wrapping forward and backward finds a3 (merged 6, the b3 is hidden).
        assert!(matches!(
            probe_filtered(s, j, true, 8, 0, 2, 100),
            FindProbe::Hit(6)
        ));
        assert!(matches!(
            probe_filtered(s, j, false, 4, 0, 2, 100),
            FindProbe::Hit(6)
        ));
        assert!(matches!(
            probe_filtered(s, j, true, 0, 0, 2, 100),
            FindProbe::Hit(6)
        ));
        // Only hidden matches: a small budget stops early and resumes.
        wait_search(&mut v, "b3", false, 1);
        let (s, j) = (v.find.search.as_ref().unwrap(), v.active_job().unwrap());
        let (mut probe, mut examined) = (0, 0);
        let mut steps = 0;
        loop {
            steps += 1;
            match probe_filtered(s, j, true, probe, examined, 2, 4) {
                FindProbe::Budget {
                    probe: p,
                    examined: e,
                } => {
                    assert!(e > examined && e <= 10);
                    (probe, examined) = (p, e);
                }
                FindProbe::Exhausted => break,
                FindProbe::Hit(i) => panic!("hidden match {i} reported"),
            }
            assert!(steps < 10);
        }
        assert!(steps >= 3, "{steps}");
        // Through find_step: "still searching" is not "no matches" while the
        // search has not finished, and the resume state is dropped on a
        // change of text.
        v.find_step(true);
        assert_eq!(v.find.current, None);
        assert!(v.toast.as_ref().is_some_and(|t| t.0 == "No matches"));
        v.find_changed(Instant::now());
        assert!(v.find_resume.is_none());
    }

    #[test]
    fn the_place_is_kept_across_a_filter_restart() {
        let mut v = view(50);
        settle(&mut v, 100);
        v.visible_rows = 10;
        v.jump_to_row(60);
        assert_eq!(v.update_window().0, 55);
        // Not following: the filter brings the same line back into the middle
        // (merged 60 is row 30 of the 50 shown).
        only_a(&mut v, 50);
        assert_eq!(v.update_window().0, 25);
        assert!(!v.follow && v.pending_anchor.is_none());
        // Off and on again: the place survives both ways.
        v.select(80, false);
        v.set_filter_view(false);
        assert_eq!(v.update_window().0, 75);
        v.selection = None;
        v.jump_to_row(20);
        v.set_filter_view(true);
        only_a_wait(&mut v, 50);
        assert_eq!(v.update_window().0, 5);
        // Scrolling before the filter gets there cancels the jump.
        v.set_filter_view(false);
        v.jump_to_row(60);
        v.set_filter_view(true);
        v.scroll_px(3.0 * v.row_h);
        assert!(v.pending_anchor.is_none());
    }

    #[test]
    fn column_queries_are_refused_and_edits_are_debounced() {
        let mut v = view(10);
        settle(&mut v, 20);
        v.filter.entries = vec![
            FilterEntry::column_query("level:ERROR", true),
            FilterEntry::include(" a"),
        ];
        let now = Instant::now();
        v.filter_edited(now);
        // Still typing: no job yet.
        assert!(v.pump(now));
        assert!(!v.filter_view_active() && v.filter_hold_until().is_some());
        wait("job after the pause", || {
            v.update_window();
            v.pump(Instant::now());
            (v.filter_progress() == Some((10, true))).then_some(())
        });
        assert_eq!(v.filter.problems.len(), 1);
        assert_eq!(v.filter.problems[0].0, 0);
        // Only invalid entries: the full view.
        v.filter.entries = vec![FilterEntry::column_query("level:ERROR", true)];
        v.filter.changed();
        v.pump(Instant::now());
        assert!(!v.filter_view_active());
        assert_eq!(v.row_count(), 20);
        // Clearing the entries stops the job.
        v.filter.entries.clear();
        v.pump(Instant::now());
        assert!(v.filter_progress().is_none() && v.filter.problems.is_empty());
    }

    #[test]
    fn a_rebuild_restarts_the_filter() {
        let mut v = view(10);
        settle(&mut v, 20);
        only_a(&mut v, 10);
        let epoch = v.store.epoch();
        v.sources[0]
            .doc
            .set_encoding(oxtail_core::EncodingChoice::Auto);
        wait("rebuild and refilter", || {
            v.update_window();
            v.pump(Instant::now());
            (v.store.epoch() > epoch && v.filter_progress() == Some((10, true))).then_some(())
        });
        assert_eq!(v.row_count(), 10);
    }

    #[test]
    fn the_title_lists_the_sources() {
        let v = view(1);
        assert_eq!(v.title(), "a.log + b.log (merged)");
        assert_eq!(MergedView::BYTES_PER_LINE, 8);
    }
}
