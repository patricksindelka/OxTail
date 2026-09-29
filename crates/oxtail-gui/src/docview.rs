//! The view state of one open document: position, follow mode, line cache,
//! outstanding reads, selection, bookmarks, marks, search, filter and
//! highlighting.
//!
//! Everything here is driven by plain calls ([`DocView::pump`] once per frame,
//! then [`DocView::update_rows`] with the viewport size) and contains no
//! painting, so it can be tested with an in-memory `Document`. The UI thread
//! never blocks: all reads are `Document::request_lines` calls whose answers
//! arrive as `DocEvent::Lines`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::{
    DocEvent, DocSnapshot, DocState, Document, EncodingChoice, Line, LineRequest, RequestId,
};
use oxtail_search::MatchSet;

use crate::filter::FilterState;
use crate::find::{Dir, FindEvent, FindState};
use crate::highlight::HighlightState;
use crate::linecache::{DEFAULT_CAPACITY, LineCache};
use crate::scroll::{
    Fetch, OVERSCAN_ROWS, Pos, RowSpace, Visible, layout_rows, plan_fetch, scroll_by, tail_position,
};
use crate::viewport::{ScrollSpace, bytes_target, lines_target};

/// How long a read may stay outstanding before it is asked for again.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest number of lines a copy collects.
pub const MAX_COPY_LINES: usize = 1_000_000;
/// Largest number of characters a copy collects.
pub const MAX_COPY_BYTES: usize = 256 * 1024 * 1024;

/// Viewport size in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Height of the text area.
    pub view_h: f32,
    /// Height of one (unwrapped) row.
    pub row_h: f32,
}

impl Metrics {
    /// Rows needed to fill the viewport.
    pub fn view_rows(&self) -> usize {
        if self.row_h <= 0.0 || !self.view_h.is_finite() {
            return 1;
        }
        ((self.view_h / self.row_h).ceil() as usize).max(1)
    }
}

/// A line the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelPoint {
    /// Start offset of the line.
    pub offset: u64,
    /// Its line number when `exact`.
    pub number: u64,
    /// Whether `number` is exact.
    pub exact: bool,
}

impl SelPoint {
    fn of(line: &Line) -> Self {
        Self {
            offset: line.offset,
            number: line.number,
            exact: line.number_exact,
        }
    }
}

/// A range of selected lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started.
    pub anchor: SelPoint,
    /// The end the user moves.
    pub cursor: SelPoint,
}

impl Selection {
    /// First and last selected line.
    pub fn bounds(&self) -> (SelPoint, SelPoint) {
        if self.anchor.offset <= self.cursor.offset {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Whether the line at `offset` is selected.
    pub fn contains(&self, offset: u64) -> bool {
        let (lo, hi) = self.bounds();
        lo.offset <= offset && offset <= hi.offset
    }
}

/// What a banner says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerKind {
    /// The file shrank.
    Truncated,
    /// The path names another file now.
    Rotated,
    /// The path no longer exists.
    Removed,
    /// The encoding was changed by the user.
    EncodingChanged,
    /// An error reported by the document.
    Error(String),
}

/// A message shown above the log until dismissed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// What happened.
    pub kind: BannerKind,
    /// Wall-clock time as `HH:MM:SS` (UTC), for the message.
    pub at: String,
}

/// Formats a `SystemTime` as `HH:MM:SS` (UTC; there is no time zone database
/// in the GUI crate and the banner only needs to tell events apart).
pub fn clock(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day / 60) % 60, day % 60)
}

/// A user bookmark.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BookmarkInfo {
    /// Optional label.
    pub label: String,
    /// Start offset of the line, once known (for the minimap).
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Jump {
    /// Show the first returned line at the top; remember `target` as the
    /// cursor line.
    Land {
        target: Option<u64>,
        /// Source line number to make the cursor line, when known.
        number: Option<u64>,
    },
    /// The line at `off` was requested to learn its number; then read around
    /// it so that it ends up in the middle (when the number is exact).
    Locate { off: u64, center: bool },
    /// A source line (number) was fetched to be found in the filtered view.
    ToFilter { center: bool },
}

#[derive(Debug, Clone, PartialEq)]
enum ReqKind {
    Fetch((u8, u64)),
    Tail,
    Refresh,
    Sniff,
    Jump(Jump),
    Copy,
}

struct Pending {
    kind: ReqKind,
    sent: Instant,
}

struct CopyJob {
    with_numbers: bool,
    lo: SelPoint,
    hi: SelPoint,
    /// Offsets to read (filtered view), else `None` and the chain is walked.
    list: Option<Vec<u64>>,
    idx: usize,
    next: u64,
    out: String,
    lines: usize,
    number_width: usize,
    truncated: bool,
    requested: bool,
}

/// What [`DocView::pump`] wants the caller to do.
#[derive(Debug, Default)]
pub struct PumpOutput {
    /// Text to put on the clipboard.
    pub copied: Option<String>,
    /// A copy was cut short at the size limit.
    pub copy_truncated: bool,
    /// Something changed that needs a repaint soon.
    pub repaint: bool,
}

/// Options for [`DocView::new`].
#[derive(Debug, Clone, Default)]
pub struct ViewInit {
    /// Start following the end of the file.
    pub follow: bool,
    /// Wrap long lines.
    pub wrap: bool,
    /// Start with the last N lines (top of the view N lines above the end)
    /// instead of following.
    pub start_lines: Option<usize>,
    /// Scroll to this 0-based line when the document is ready.
    pub initial_line: Option<u64>,
}

/// The view state of a document.
pub struct DocView {
    /// The document.
    pub doc: Arc<Document>,
    /// The latest snapshot (refreshed by [`DocView::pump`]).
    pub snapshot: Arc<DocSnapshot>,
    /// Cached lines.
    pub cache: LineCache,
    /// Scroll position (start offset of the top line, plus pixels).
    pub pos: Pos,
    /// Stick to the end of the file.
    pub follow: bool,
    /// Wrap long lines instead of scrolling horizontally.
    pub wrap: bool,
    /// Horizontal scroll in pixels (no-wrap mode).
    pub h_scroll: f32,
    /// Selected lines.
    pub selection: Option<Selection>,
    /// The line the user last jumped to or clicked (start offset).
    pub cursor: Option<u64>,
    /// Visual separators, keyed by the document length when they were added.
    pub marks: BTreeSet<u64>,
    /// Bookmarks by exact line number.
    pub bookmarks: BTreeMap<u64, BookmarkInfo>,
    /// A message above the log.
    pub banner: Option<Banner>,
    /// A short-lived message.
    pub toast: Option<(String, Instant)>,
    /// Find state.
    pub find: FindState,
    /// Filter state.
    pub filter: FilterState,
    /// Highlighting state.
    pub hl: Rc<RefCell<HighlightState>>,
    /// Cache of laid-out lines, shared with the painting code (which also
    /// needs it to measure wrapped lines while the view is borrowed).
    pub galleys: Rc<RefCell<crate::logview::GalleyCache>>,
    /// Widest line drawn so far, in pixels (horizontal scroll range).
    pub max_text_w: f32,
    /// Minimap bins.
    pub minimap: crate::logview::MinimapCache,
    /// A bookmark whose label is being edited (line number).
    pub editing_bookmark: Option<u64>,
    /// Last known viewport metrics.
    pub metrics: Metrics,
    /// Rows drawn in the last frame (for tests and status).
    pub last_rows: Vec<Arc<Line>>,
    /// The scrollbar thumb is being dragged at this position.
    pub thumb_drag: Option<f64>,
    pending: HashMap<RequestId, Pending>,
    back_attempts: HashMap<u64, u32>,
    pending_px: f32,
    resume_on_bottom: bool,
    paused_at_lines: u64,
    jump_wanted: Option<f64>,
    initial_line: Option<u64>,
    start_lines: Option<usize>,
    copy: Option<CopyJob>,
    bookmark_cursor: Option<u64>,
    sniffed: Option<Vec<String>>,
    last_len: u64,
    was_exact: bool,
    /// Next line number the alert scan has not looked at (`None` until the
    /// initial index is complete).
    pub alert_next: Option<u64>,
}

impl DocView {
    /// A view of `doc`.
    pub fn new(doc: Arc<Document>, init: &ViewInit) -> Self {
        let snapshot = doc.snapshot();
        let generation = snapshot.generation;
        let follow = init.follow && init.start_lines.is_none() && init.initial_line.is_none();
        Self {
            doc,
            snapshot,
            cache: LineCache::new(generation, DEFAULT_CAPACITY),
            pos: Pos::default(),
            follow,
            wrap: init.wrap,
            h_scroll: 0.0,
            selection: None,
            cursor: None,
            marks: BTreeSet::new(),
            bookmarks: BTreeMap::new(),
            banner: None,
            toast: None,
            find: FindState::default(),
            filter: FilterState::default(),
            hl: Rc::new(RefCell::new(HighlightState::with_defaults())),
            galleys: Rc::new(RefCell::new(crate::logview::GalleyCache::default())),
            max_text_w: 0.0,
            minimap: crate::logview::MinimapCache::default(),
            editing_bookmark: None,
            metrics: Metrics {
                view_h: 400.0,
                row_h: 16.0,
            },
            last_rows: Vec::new(),
            thumb_drag: None,
            pending: HashMap::new(),
            back_attempts: HashMap::new(),
            pending_px: 0.0,
            resume_on_bottom: false,
            paused_at_lines: 0,
            jump_wanted: None,
            initial_line: init.initial_line,
            start_lines: init.start_lines,
            copy: None,
            bookmark_cursor: None,
            sniffed: None,
            last_len: 0,
            was_exact: false,
            alert_next: None,
        }
    }

    // ----------------------------------------------------------------- state

    /// The filtered lines, when the filtered view is showing.
    pub fn active_set(&self) -> Option<Arc<MatchSet>> {
        self.filter
            .view_active()
            .then(|| Arc::clone(&self.filter.set))
    }

    /// Whether the filtered view is showing.
    pub fn is_filtered(&self) -> bool {
        self.filter.view_active()
    }

    /// Whether the document is still being opened.
    pub fn is_opening(&self) -> bool {
        matches!(self.snapshot.state, DocState::Opening)
    }

    /// Sets a toast message.
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// The start offset of the line the user is "at": the selection cursor,
    /// the last jump target, or the top row.
    pub fn cursor_offset(&self) -> u64 {
        self.selection
            .map(|s| s.cursor.offset)
            .or(self.cursor)
            .unwrap_or(self.pos.top)
    }

    fn view_rows(&self) -> usize {
        self.metrics.view_rows()
    }

    fn send(&mut self, req: LineRequest, kind: ReqKind) -> RequestId {
        let id = self.doc.request_lines(req);
        self.pending.insert(
            id,
            Pending {
                kind,
                sent: Instant::now(),
            },
        );
        id
    }

    fn has_pending(&self, f: impl Fn(&ReqKind) -> bool) -> bool {
        self.pending.values().any(|p| f(&p.kind))
    }

    fn jump_pending(&self) -> bool {
        self.has_pending(|k| matches!(k, ReqKind::Jump(_)))
    }

    // ------------------------------------------------------------------ pump

    /// Drains document events, refreshes the snapshot and advances search,
    /// filter, copy and follow bookkeeping. Call once per frame, before
    /// [`DocView::update_rows`].
    pub fn pump(&mut self, now: Instant) -> PumpOutput {
        let mut out = PumpOutput::default();
        self.snapshot = self.doc.snapshot();
        self.sync_generation();

        while let Ok(ev) = self.doc.events().try_recv() {
            out.repaint = true;
            self.on_event(ev);
        }
        // Events may have bumped the generation.
        self.snapshot = self.doc.snapshot();
        self.sync_generation();

        // Index completion: estimated numbers become exact.
        let exact = self.snapshot.lines.exact;
        if exact && !self.was_exact {
            self.cache.drop_inexact();
        }
        self.was_exact = exact;

        self.pending
            .retain(|_, p| now.duration_since(p.sent) < REQUEST_TIMEOUT);
        if let Some((_, at)) = &self.toast
            && now.duration_since(*at) > Duration::from_secs(4)
        {
            self.toast = None;
        }

        self.tick_search_and_filter(now, &mut out);
        self.step_copy(&mut out);
        out
    }

    fn sync_generation(&mut self) {
        let g = self.snapshot.generation;
        if g == self.cache.generation() {
            return;
        }
        self.cache.reset(g);
        self.pending.clear();
        self.back_attempts.clear();
        self.pos = Pos::default();
        self.pending_px = 0.0;
        self.h_scroll = 0.0;
        self.selection = None;
        self.cursor = None;
        self.marks.clear();
        self.bookmarks.clear();
        self.copy = None;
        self.jump_wanted = None;
        self.thumb_drag = None;
        self.hl.borrow_mut().clear_cache();
        self.galleys.borrow_mut().clear();
        self.max_text_w = 0.0;
        self.alert_next = None;
        self.was_exact = false;
        self.last_len = 0;
        self.find.restart_now(Instant::now());
        self.find.cancel();
        self.find.restart_now(Instant::now());
    }

    fn on_event(&mut self, ev: DocEvent) {
        match ev {
            DocEvent::Lines {
                id,
                generation,
                lines,
            } => self.on_lines(id, generation, lines),
            DocEvent::Grew { utf8_len } => {
                self.on_grew(utf8_len);
            }
            DocEvent::IndexProgress { .. } => {}
            DocEvent::Truncated { at, .. } => {
                self.banner = Some(Banner {
                    kind: BannerKind::Truncated,
                    at: clock(at),
                });
            }
            DocEvent::Rotated { at, .. } => {
                self.banner = Some(Banner {
                    kind: BannerKind::Rotated,
                    at: clock(at),
                });
            }
            DocEvent::Removed { at } => {
                self.banner = Some(Banner {
                    kind: BannerKind::Removed,
                    at: clock(at),
                });
            }
            DocEvent::EncodingChanged { .. } => {
                self.banner = Some(Banner {
                    kind: BannerKind::EncodingChanged,
                    at: clock(std::time::SystemTime::now()),
                });
            }
            DocEvent::Error(msg) => {
                self.banner = Some(Banner {
                    kind: BannerKind::Error(msg),
                    at: clock(std::time::SystemTime::now()),
                });
            }
        }
    }

    fn on_grew(&mut self, utf8_len: u64) {
        // The previously last line may have been unterminated: read it again.
        let old = self.last_len.min(utf8_len);
        if !self.follow
            && let Some(l) = self.cache.last_line_ending_at(old).cloned()
            && old > 0
            && !self.has_pending(|k| matches!(k, ReqKind::Refresh))
        {
            self.send(LineRequest::AtOffsets(vec![l.offset]), ReqKind::Refresh);
        }
        self.last_len = utf8_len;
    }

    fn on_lines(&mut self, id: RequestId, generation: u64, lines: Vec<Line>) {
        let kind = self.pending.remove(&id).map(|p| p.kind);
        if generation != self.cache.generation() {
            return;
        }
        // Reset the "asked already" memory of backward reads that made progress.
        if let Some(first) = lines.first() {
            self.back_attempts.remove(&first.offset);
        }
        match kind {
            Some(ReqKind::Jump(j)) => {
                let copy = lines.clone();
                self.cache.insert(generation, lines);
                if copy.is_empty() {
                    self.toast("Nothing to show at that position");
                } else {
                    self.finish_jump(j, &copy);
                }
            }
            Some(ReqKind::Sniff) => {
                self.sniffed = Some(lines.iter().map(|l| l.text.clone()).collect());
                self.cache.insert(generation, lines);
            }
            Some(ReqKind::Tail) => {
                if self.start_lines.take().is_some()
                    && let Some(first) = lines.first()
                {
                    let top = first.offset;
                    self.pos = Pos::at(top);
                    self.follow = false;
                    self.resume_on_bottom = true;
                }
                self.cache.insert(generation, lines);
            }
            _ => {
                self.cache.insert(generation, lines);
            }
        }
    }

    fn finish_jump(&mut self, j: Jump, lines: &[Line]) {
        let Some(first) = lines.first() else { return };
        match j {
            Jump::Land { target, number } => {
                self.pos = Pos::at(first.offset);
                self.pending_px = 0.0;
                self.follow = false;
                self.cursor = target.or_else(|| {
                    number.and_then(|n| {
                        lines
                            .iter()
                            .find(|l| l.number_exact && l.number == n)
                            .map(|l| l.offset)
                    })
                });
            }
            Jump::Locate { off, center } => {
                if center && first.number_exact && first.number > 0 {
                    let half = (self.view_rows() / 2) as u64;
                    let start = first.number.saturating_sub(half);
                    let count = self.view_rows() + OVERSCAN_ROWS;
                    self.send(
                        LineRequest::Range {
                            first: start,
                            count,
                        },
                        ReqKind::Jump(Jump::Land {
                            target: Some(off),
                            number: None,
                        }),
                    );
                } else {
                    self.pos = Pos::at(first.offset);
                    self.pending_px = 0.0;
                    self.follow = false;
                    self.cursor = Some(off);
                }
            }
            Jump::ToFilter { center } => {
                self.jump_offset(first.offset, center);
            }
        }
    }

    fn tick_search_and_filter(&mut self, now: Instant, out: &mut PumpOutput) {
        let source = self.doc.source();
        let len = self.snapshot.utf8_len;
        let generation = self.snapshot.generation;
        let hide = self.hl.borrow().hide.clone();
        self.filter
            .tick(now, &source, len, generation, self.pos.top, hide.as_ref());

        let set = self.active_set();
        let accept = move |o: u64| set.as_ref().is_none_or(|s| s.contains(o));
        let top = self.pos.top;
        self.find.set_hint(top);
        if let Some(ev) = self.find.tick(now, &source, len, generation, top, &accept) {
            match ev {
                FindEvent::Jump(o) => {
                    self.jump_offset(o, true);
                    out.repaint = true;
                }
                FindEvent::NoMatch => {
                    out.repaint = true;
                }
            }
        }
        if self.find.searching || self.find.debounce_remaining(now).is_some() {
            out.repaint = true;
        }
    }

    // ------------------------------------------------------------- positions

    /// Updates the position (follow, pending scroll), lays out the visible
    /// rows and starts the reads that are still needed.
    pub fn update_rows(&mut self, m: Metrics, height_of: &dyn Fn(&Line) -> f32) -> Visible {
        self.metrics = m;
        let set = self.active_set();
        let utf8_len = self.snapshot.utf8_len;
        let space = match &set {
            Some(s) => RowSpace::Filtered { set: s },
            None => RowSpace::Full { utf8_len },
        };
        let view_rows = m.view_rows();

        self.apply_initial();

        // A filtered view can only start at one of its own lines.
        if let Some(s) = &set
            && !self.follow
            && !s.is_empty()
            && !s.contains(self.pos.top)
        {
            let idx = s.rank(self.pos.top).min(s.len() - 1);
            if let Some(o) = s.get(idx) {
                self.pos = Pos::at(o);
            }
        }

        if self.follow {
            self.pending_px = 0.0;
            match tail_position(&space, &self.cache, m.view_h, height_of) {
                Some(p) => self.pos = p,
                None => self.request_tail(&space, view_rows),
            }
        } else if self.pending_px != 0.0 {
            let out = scroll_by(
                &mut self.pos,
                self.pending_px,
                &space,
                &self.cache,
                height_of,
            );
            self.pending_px = out.remainder.clamp(-1e6, 1e6);
            if self.pending_px >= 0.0
                && let Some(t) = tail_position(&space, &self.cache, m.view_h, height_of)
                && self.pos.cmp_pos(&t) == std::cmp::Ordering::Greater
            {
                self.pos = t;
                self.pending_px = 0.0;
            }
        }

        let vis = layout_rows(&self.pos, &space, &self.cache, m.view_h, height_of);

        // Reached the bottom by scrolling: resume following.
        if !self.follow && self.resume_on_bottom && vis.reaches_end && self.at_bottom(&vis, m) {
            self.follow = true;
            self.resume_on_bottom = false;
        }

        self.request_missing(&space, &vis, view_rows);
        self.send_wanted_jump(&space);
        self.cache.trim(self.pos.top);
        self.last_rows = vis.rows.iter().map(|r| Arc::clone(&r.line)).collect();
        vis
    }

    fn at_bottom(&self, vis: &Visible, m: Metrics) -> bool {
        vis.rows
            .last()
            .is_some_and(|r| r.y + r.height <= m.view_h + 1.0)
    }

    fn apply_initial(&mut self) {
        if self.is_opening() {
            return;
        }
        if let Some(n) = self.initial_line.take() {
            self.jump_to_line(n, true);
        }
        if let Some(n) = self.start_lines
            && !self.has_pending(|k| matches!(k, ReqKind::Tail))
            && self.snapshot.utf8_len > 0
        {
            self.send(LineRequest::Tail { count: n.max(1) }, ReqKind::Tail);
        }
    }

    fn request_tail(&mut self, space: &RowSpace<'_>, view_rows: usize) {
        if self.snapshot.utf8_len == 0 && !self.is_filtered() {
            return;
        }
        match space {
            RowSpace::Full { .. } => {
                if self.has_pending(|k| matches!(k, ReqKind::Tail)) || self.start_lines.is_some() {
                    return;
                }
                self.send(
                    LineRequest::Tail {
                        count: view_rows + OVERSCAN_ROWS,
                    },
                    ReqKind::Tail,
                );
            }
            RowSpace::Filtered { set } => {
                // Anchor at the last row so the generic read planning loads it.
                if let Some(last) = space.last_start() {
                    if self.cache.get(last).is_none() || self.pos.top != last {
                        self.pos = Pos::at(last);
                    }
                } else if set.is_empty() {
                    self.pos = Pos::default();
                }
            }
        }
    }

    fn request_missing(&mut self, space: &RowSpace<'_>, vis: &Visible, view_rows: usize) {
        if self.follow && matches!(space, RowSpace::Full { .. }) {
            // The tail read covers the view; older lines are read lazily
            // below once it has arrived.
            if vis.top_missing {
                return;
            }
        }
        if self.jump_pending() {
            return;
        }
        let pending_keys: Vec<(u8, u64)> = self
            .pending
            .values()
            .filter_map(|p| match &p.kind {
                ReqKind::Fetch(k) => Some(*k),
                _ => None,
            })
            .collect();
        let plan = plan_fetch(&self.pos, vis, space, &self.cache, view_rows, &|f| {
            pending_keys.contains(&f.key())
        });
        let len = self.snapshot.utf8_len;
        for f in plan {
            let attempts = match &f {
                Fetch::Backward { before, .. } => {
                    let a = self.back_attempts.entry(*before).or_insert(0);
                    let cur = *a;
                    *a += 1;
                    cur
                }
                _ => 0,
            };
            if let Some(req) = f.to_request(&self.cache, len, attempts) {
                let key = f.key();
                self.send(req, ReqKind::Fetch(key));
            }
        }
    }

    // --------------------------------------------------------------- actions

    /// Scrolls by `dy` pixels (positive is down). Scrolling up pauses
    /// following; scrolling down to the end resumes it.
    pub fn scroll_px(&mut self, dy: f32) {
        if dy == 0.0 || !dy.is_finite() {
            return;
        }
        self.bookmark_cursor = None;
        if dy < 0.0 && self.follow {
            self.pause_follow();
        }
        if dy > 0.0 && !self.follow {
            self.resume_on_bottom = true;
        }
        self.pending_px += dy;
    }

    /// One page up/down.
    pub fn page(&mut self, down: bool) {
        let page = (self.metrics.view_h - self.metrics.row_h).max(self.metrics.row_h);
        self.scroll_px(if down { page } else { -page });
    }

    /// Stops following (the pill counts new lines from now on).
    pub fn pause_follow(&mut self) {
        if self.follow {
            self.follow = false;
            self.paused_at_lines = self.snapshot.lines.estimated_total;
            self.resume_on_bottom = true;
        }
    }

    /// Follows the end of the file.
    pub fn resume_follow(&mut self) {
        self.follow = true;
        self.resume_on_bottom = false;
        self.pending_px = 0.0;
        self.jump_wanted = None;
    }

    /// Toggles following.
    pub fn toggle_follow(&mut self) {
        if self.follow {
            self.pause_follow();
        } else {
            self.resume_follow();
        }
    }

    /// Lines that arrived while paused (approximate while indexing).
    pub fn new_lines_while_paused(&self) -> u64 {
        if self.follow {
            0
        } else {
            self.snapshot
                .lines
                .estimated_total
                .saturating_sub(self.paused_at_lines)
        }
    }

    /// Jumps to the first line.
    pub fn jump_top(&mut self) {
        self.bookmark_cursor = None;
        self.follow = false;
        self.pending_px = 0.0;
        self.resume_on_bottom = true;
        if let Some(first) = self.row_space_first() {
            self.pos = Pos::at(first);
            self.cursor = Some(first);
        } else if !self.is_filtered() {
            self.pos = Pos::default();
        }
    }

    fn row_space_first(&self) -> Option<u64> {
        match self.active_set() {
            Some(s) => s.get(0),
            None => (self.snapshot.utf8_len > 0).then_some(0),
        }
    }

    /// Jumps to a 0-based source line. In a filtered view this lands on the
    /// nearest shown line.
    pub fn jump_to_line(&mut self, line: u64, center: bool) {
        self.bookmark_cursor = None;
        self.follow = false;
        self.pending_px = 0.0;
        self.resume_on_bottom = true;
        let half = if center {
            (self.view_rows() / 2) as u64
        } else {
            0
        };
        let first = line.saturating_sub(half);
        let count = self.view_rows() + OVERSCAN_ROWS;
        if self.is_filtered() {
            // Read the line itself to learn its offset.
            self.send(
                LineRequest::Range {
                    first: line,
                    count: 1,
                },
                ReqKind::Jump(Jump::ToFilter { center }),
            );
        } else {
            self.send(
                LineRequest::Range { first, count },
                ReqKind::Jump(Jump::Land {
                    target: None,
                    number: Some(line),
                }),
            );
        }
    }

    /// Scrolls so the line starting at `offset` is visible (in the middle
    /// when `center`).
    pub fn jump_offset(&mut self, offset: u64, center: bool) {
        self.bookmark_cursor = None;
        self.follow = false;
        self.pending_px = 0.0;
        self.resume_on_bottom = true;
        self.cursor = Some(offset);
        if let Some(set) = self.active_set() {
            let rank = set.rank(offset);
            let half = if center { self.view_rows() / 2 } else { 0 };
            let idx = rank.saturating_sub(half).min(set.len().saturating_sub(1));
            if let Some(o) = set.get(idx) {
                self.pos = Pos::at(o);
            }
            return;
        }
        // Already on screen and cached: only move when it is not visible.
        let len = self.snapshot.utf8_len.max(1);
        let fraction = ((offset as f64 + 0.5) / len as f64).clamp(0.0, 1.0);
        self.send(
            LineRequest::ByteFraction { fraction, count: 1 },
            ReqKind::Jump(Jump::Locate {
                off: offset,
                center,
            }),
        );
    }

    /// The thumb of the vertical scrollbar was moved to `position`
    /// (`0.0..=1.0`).
    pub fn scroll_to_thumb(&mut self, position: f64, space: ScrollSpace) {
        self.thumb_drag = Some(position);
        if position >= 0.999_999 {
            self.resume_follow();
            return;
        }
        if self.follow {
            self.pause_follow();
        }
        self.resume_on_bottom = true;
        self.pending_px = 0.0;
        match space {
            ScrollSpace::Lines { total, visible, .. } => {
                let idx = lines_target(position, total, visible);
                if let Some(set) = self.active_set() {
                    if let Some(o) = set.get(idx as usize) {
                        self.pos = Pos::at(o);
                    }
                } else {
                    self.jump_wanted = Some(idx as f64);
                }
            }
            ScrollSpace::Bytes {
                visible_bytes,
                utf8_len,
                ..
            } => {
                let f = bytes_target(position, visible_bytes, utf8_len);
                // Encode as a negative value to tell it apart from a line.
                self.jump_wanted = Some(-(f + 1.0));
            }
        }
    }

    /// Jumps to a byte fraction (minimap click).
    pub fn jump_fraction(&mut self, fraction: f64) {
        self.follow = false;
        self.resume_on_bottom = true;
        self.pending_px = 0.0;
        if let Some(set) = self.active_set() {
            let idx = ((fraction * set.len() as f64) as usize).min(set.len().saturating_sub(1));
            if let Some(o) = set.get(idx) {
                self.pos = Pos::at(o);
            }
            return;
        }
        self.send(
            LineRequest::ByteFraction {
                fraction: fraction.clamp(0.0, 1.0),
                count: self.view_rows() + OVERSCAN_ROWS,
            },
            ReqKind::Jump(Jump::Land {
                target: None,
                number: None,
            }),
        );
    }

    fn send_wanted_jump(&mut self, _space: &RowSpace<'_>) {
        if self.jump_pending() {
            return;
        }
        let Some(w) = self.jump_wanted.take() else {
            return;
        };
        let count = self.view_rows() + OVERSCAN_ROWS;
        let req = if w < 0.0 {
            LineRequest::ByteFraction {
                fraction: (-w - 1.0).clamp(0.0, 1.0),
                count,
            }
        } else {
            LineRequest::Range {
                first: w as u64,
                count,
            }
        };
        self.send(
            req,
            ReqKind::Jump(Jump::Land {
                target: None,
                number: None,
            }),
        );
    }

    /// The scrollbar geometry for the rows in `vis`.
    pub fn scroll_space(&self, vis: &Visible) -> ScrollSpace {
        let visible = vis.rows.len() as u64;
        if let Some(set) = self.active_set() {
            return ScrollSpace::Lines {
                total: set.len() as u64,
                top: set.rank(self.pos.top) as u64,
                visible,
            };
        }
        let top_line = self.cache.get(self.pos.top);
        if self.snapshot.lines.exact
            && let Some(l) = top_line
            && l.number_exact
        {
            return ScrollSpace::Lines {
                total: self.snapshot.lines.known,
                top: l.number,
                visible,
            };
        }
        ScrollSpace::Bytes {
            top_offset: self.pos.top,
            visible_bytes: vis.rows.iter().map(|r| r.line.len).sum(),
            utf8_len: self.snapshot.utf8_len,
        }
    }

    /// Changes the text encoding (rebuilds the view; a new generation).
    pub fn set_encoding(&self, choice: EncodingChoice) {
        self.doc.set_encoding(choice);
    }

    /// Reads the first lines of the document (for profile selection); the
    /// answer is picked up with [`DocView::take_sniffed`].
    pub fn request_sniff(&mut self) {
        if !self.has_pending(|k| matches!(k, ReqKind::Sniff)) {
            self.send(LineRequest::Range { first: 0, count: 50 }, ReqKind::Sniff);
        }
    }

    /// The first lines requested by [`DocView::request_sniff`], once read.
    pub fn take_sniffed(&mut self) -> Option<Vec<String>> {
        self.sniffed.take()
    }

    /// Switches between the filtered and the full view, keeping the line the
    /// user is at in view.
    pub fn set_filter_view(&mut self, enabled: bool) {
        let off = self.cursor_offset();
        let was_following = self.follow;
        self.filter.enabled = enabled;
        self.jump_offset(off, true);
        if was_following {
            self.resume_follow();
        }
    }

    // ------------------------------------------------------------- selection

    /// Selects `line` (or extends the selection to it).
    pub fn select(&mut self, line: &Line, extend: bool) {
        self.bookmark_cursor = None;
        let p = SelPoint::of(line);
        self.cursor = Some(line.offset);
        self.selection = Some(match (extend, self.selection) {
            (true, Some(s)) => Selection {
                anchor: s.anchor,
                cursor: p,
            },
            _ => Selection {
                anchor: p,
                cursor: p,
            },
        });
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Selects every line.
    pub fn select_all(&mut self) {
        let (first, last) = match self.active_set() {
            Some(set) => (
                set.get(0),
                set.len().checked_sub(1).and_then(|i| set.get(i)),
            ),
            None => (
                (self.snapshot.utf8_len > 0).then_some(0),
                self.cache
                    .last_line_ending_at(self.snapshot.utf8_len)
                    .map(|l| l.offset),
            ),
        };
        let (Some(first), Some(last)) = (first, last) else {
            self.toast("Scroll to the end once to select everything");
            return;
        };
        let p = |offset: u64, number: u64, exact: bool| SelPoint {
            offset,
            number,
            exact,
        };
        let total = self.snapshot.lines.known.saturating_sub(1);
        self.selection = Some(Selection {
            anchor: p(first, 0, true),
            cursor: p(last, total, self.snapshot.lines.exact),
        });
    }

    /// Number of selected lines, and whether the count is exact.
    pub fn selection_count(&self) -> Option<(u64, bool)> {
        let s = self.selection?;
        let (lo, hi) = s.bounds();
        if let Some(set) = self.active_set() {
            let n = set.rank(hi.offset).saturating_sub(set.rank(lo.offset)) + 1;
            return Some((n as u64, true));
        }
        if lo.exact && hi.exact {
            return Some((hi.number - lo.number + 1, true));
        }
        Some((1.max(hi.number.saturating_sub(lo.number) + 1), false))
    }

    /// Starts collecting the selected lines' text; the result arrives from
    /// [`DocView::pump`] as `copied`.
    pub fn copy_selection(&mut self, with_numbers: bool) {
        let Some(s) = self.selection else {
            self.toast("Nothing selected");
            return;
        };
        let (lo, hi) = s.bounds();
        let list = self.active_set().map(|set| {
            let a = set.rank(lo.offset);
            let b = set.rank(hi.offset);
            (a..=b).filter_map(|i| set.get(i)).collect::<Vec<u64>>()
        });
        let digits = crate::viewport::digits(hi.number.saturating_add(1)) + 1;
        self.copy = Some(CopyJob {
            with_numbers,
            lo,
            hi,
            list,
            idx: 0,
            next: lo.offset,
            out: String::new(),
            lines: 0,
            number_width: digits,
            truncated: false,
            requested: false,
        });
    }

    fn step_copy(&mut self, out: &mut PumpOutput) {
        let Some(mut job) = self.copy.take() else {
            return;
        };
        loop {
            let wanted = match &job.list {
                Some(l) => match l.get(job.idx) {
                    Some(o) => *o,
                    None => {
                        self.finish_copy(job, out);
                        return;
                    }
                },
                None => job.next,
            };
            let Some(line) = self.cache.get(wanted).cloned() else {
                // Not cached: ask for it (once per stall) and retry later.
                if !job.requested && !self.has_pending(|k| matches!(k, ReqKind::Copy)) {
                    self.request_for_copy(&job, wanted);
                    job.requested = true;
                }
                if job.requested && !self.has_pending(|k| matches!(k, ReqKind::Copy)) {
                    // The read returned; allow one more attempt next frame.
                    job.requested = false;
                }
                self.copy = Some(job);
                return;
            };
            job.requested = false;
            if job.with_numbers {
                if line.number_exact {
                    let n = (line.number + 1).to_string();
                    job.out
                        .push_str(&format!("{n:>w$}  ", w = job.number_width));
                } else {
                    let n = format!("\u{2248}{}", line.number + 1);
                    job.out
                        .push_str(&format!("{n:>w$}  ", w = job.number_width));
                }
            }
            job.out.push_str(&line.text);
            job.out.push('\n');
            job.lines += 1;
            job.idx += 1;
            job.next = line.offset + line.len;
            let done = line.offset >= job.hi.offset;
            if job.lines >= MAX_COPY_LINES || job.out.len() >= MAX_COPY_BYTES {
                job.truncated = !done;
                self.finish_copy(job, out);
                return;
            }
            if done {
                self.finish_copy(job, out);
                return;
            }
        }
    }

    fn request_for_copy(&mut self, job: &CopyJob, wanted: u64) {
        let len = self.snapshot.utf8_len;
        let req = match &job.list {
            Some(l) => {
                let chunk: Vec<u64> = l[job.idx..].iter().take(2000).copied().collect();
                LineRequest::AtOffsets(chunk)
            }
            None => {
                let fetch = Fetch::Forward {
                    from: wanted,
                    count: 4000,
                };
                match fetch.to_request(&self.cache, len, 0) {
                    Some(r) => r,
                    None => return,
                }
            }
        };
        self.send(req, ReqKind::Copy);
    }

    fn finish_copy(&mut self, job: CopyJob, out: &mut PumpOutput) {
        let _ = (job.lo, job.hi);
        out.copy_truncated = job.truncated;
        out.copied = Some(job.out);
        out.repaint = true;
    }

    // ------------------------------------------------- bookmarks and marks

    /// Adds a visual mark after the current last line.
    pub fn add_mark(&mut self) {
        self.marks.insert(self.snapshot.utf8_len);
    }

    /// Toggles a bookmark on the current line.
    pub fn toggle_bookmark(&mut self) {
        let off = self.cursor_offset();
        let Some(line) = self.cache.get(off).cloned() else {
            self.toast("Line not loaded yet");
            return;
        };
        if !line.number_exact {
            self.toast("The index is still being built; try again in a moment");
            return;
        }
        if self.bookmarks.remove(&line.number).is_none() {
            self.bookmarks.insert(
                line.number,
                BookmarkInfo {
                    label: String::new(),
                    offset: Some(line.offset),
                },
            );
        }
    }

    /// Jumps to the next/previous bookmark relative to the current line.
    pub fn goto_bookmark(&mut self, dir: Dir) {
        if self.bookmarks.is_empty() {
            self.toast("No bookmarks (Ctrl+F2 adds one)");
            return;
        }
        let current = self.bookmark_cursor.or_else(|| {
            self.cache
                .get(self.cursor_offset())
                .filter(|l| l.number_exact)
                .map(|l| l.number)
        });
        let target = match (dir, current) {
            (Dir::Next, Some(c)) => self
                .bookmarks
                .range(c + 1..)
                .next()
                .or_else(|| self.bookmarks.iter().next()),
            (Dir::Next, None) => self.bookmarks.iter().next(),
            (Dir::Prev, Some(c)) => self
                .bookmarks
                .range(..c)
                .next_back()
                .or_else(|| self.bookmarks.iter().next_back()),
            (Dir::Prev, None) => self.bookmarks.iter().next_back(),
        };
        if let Some((&n, _)) = target {
            self.jump_to_line(n, true);
            self.bookmark_cursor = Some(n);
        }
    }

    /// Adds automatic bookmarks for the given lines (from `bookmark` rules).
    pub fn add_bookmarks(&mut self, lines: impl IntoIterator<Item = (u64, u64)>) {
        for (number, offset) in lines {
            self.bookmarks.entry(number).or_insert(BookmarkInfo {
                label: "auto".into(),
                offset: Some(offset),
            });
        }
    }

    /// Records that the bookmark at `number` sits at `offset` (learned while
    /// painting), for the minimap.
    pub fn learn_bookmark_offset(&mut self, number: u64, offset: u64) {
        if let Some(b) = self.bookmarks.get_mut(&number)
            && b.offset != Some(offset)
        {
            b.offset = Some(offset);
        }
    }

    /// The lines of the document that were not yet looked at for alerts.
    /// Returns `(generation, first_line, count)`; `None` until the initial
    /// index is complete and while nothing new arrived.
    pub fn take_new_lines(&mut self) -> Option<(u64, u64, u64)> {
        let known = self.snapshot.lines.known;
        if !self.snapshot.lines.exact {
            return None;
        }
        match self.alert_next {
            None => {
                self.alert_next = Some(known);
                None
            }
            Some(next) if known > next => {
                self.alert_next = Some(known);
                Some((self.snapshot.generation, next, known - next))
            }
            Some(next) if known < next => {
                self.alert_next = Some(known);
                None
            }
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_core::MemSource;

    fn doc_with(lines: usize) -> (Arc<Document>, Arc<MemSource>) {
        let mut text = String::new();
        for i in 0..lines {
            text.push_str(&format!("line {i:05} some text\n"));
        }
        let mem = Arc::new(MemSource::new(text.into_bytes()));
        let doc = Arc::new(Document::from_source(mem.clone(), "test.log"));
        (doc, mem)
    }

    const M: Metrics = Metrics {
        view_h: 160.0,
        row_h: 16.0,
    };

    fn h(_: &Line) -> f32 {
        16.0
    }

    /// Pumps and lays out until `done` says so (max 10 s).
    fn run_until(v: &mut DocView, mut done: impl FnMut(&DocView, &Visible) -> bool) -> Visible {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            v.pump(Instant::now());
            let vis = v.update_rows(M, &h);
            if done(v, &vis) {
                return vis;
            }
            assert!(
                Instant::now() < deadline,
                "view did not settle: pos={:?} follow={} pending={} rows={:?} bm={:?}",
                v.pos,
                v.follow,
                v.pending.len(),
                numbers(&vis),
                v.bookmarks
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn numbers(vis: &Visible) -> Vec<u64> {
        vis.rows.iter().map(|r| r.line.number).collect()
    }

    fn init(follow: bool) -> ViewInit {
        ViewInit {
            follow,
            ..ViewInit::default()
        }
    }

    #[test]
    fn opens_at_the_tail_and_follows_growth() {
        let (doc, mem) = doc_with(200);
        let mut v = DocView::new(doc, &init(true));
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.last().is_some_and(|r| r.line.number == 199)
        });
        assert!(vis.reaches_end);
        assert_eq!(vis.rows.len(), 10);
        assert!(v.follow);
        mem.append(b"line 00200 appended\n");
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.last().is_some_and(|r| r.line.number == 200)
        });
        assert_eq!(vis.rows.last().unwrap().line.text, "line 00200 appended");
        assert!(v.follow);
    }

    #[test]
    fn scrolling_up_pauses_and_counts_new_lines() {
        let (doc, mem) = doc_with(300);
        let mut v = DocView::new(doc, &init(true));
        run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && vis.rows.last().is_some_and(|r| r.line.number == 299)
        });
        v.scroll_px(-160.0);
        assert!(!v.follow);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.last().is_some_and(|r| r.line.number < 299)
        });
        assert_eq!(numbers(&vis).first(), Some(&280));
        mem.append(b"a\nb\nc\n");
        run_until(&mut v, |v, _| v.new_lines_while_paused() >= 3);
        assert_eq!(v.new_lines_while_paused(), 3);
        // The view did not move.
        assert_eq!(v.last_rows.first().unwrap().number, 280);
        // Scrolling back down to the end resumes following.
        v.scroll_px(10_000.0);
        run_until(&mut v, |v, _| v.follow);
        assert_eq!(v.new_lines_while_paused(), 0);
    }

    #[test]
    fn open_at_top_and_page_down() {
        let (doc, _mem) = doc_with(500);
        let mut v = DocView::new(doc, &init(false));
        let vis = run_until(&mut v, |_, vis| vis.rows.len() == 10);
        assert_eq!(numbers(&vis)[0], 0);
        v.page(true);
        let vis = run_until(&mut v, |v, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 0) && v.pending_px == 0.0
        });
        assert_eq!(numbers(&vis)[0], 9);
        v.jump_top();
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number == 0)
        });
        assert_eq!(numbers(&vis)[0], 0);
    }

    #[test]
    fn goto_line_centers_the_target() {
        let (doc, _mem) = doc_with(1000);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && !vis.rows.is_empty()
        });
        v.jump_to_line(500, true);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 400)
        });
        let n = numbers(&vis);
        assert!(n.contains(&500), "{n:?}");
        assert_eq!(n[0], 495);
        assert!(!v.follow);
    }

    #[test]
    fn jump_offset_finds_the_line() {
        let (doc, _mem) = doc_with(1000);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && !vis.rows.is_empty()
        });
        let off = ("line 00000 some text\n".len() * 700) as u64;
        v.jump_offset(off, true);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 600)
        });
        let n = numbers(&vis);
        assert!(n.contains(&700), "{n:?}");
        assert_eq!(v.cursor, Some(off));
    }

    #[test]
    fn byte_fraction_jump_lands_near_the_middle() {
        let (doc, _mem) = doc_with(1000);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |_, vis| !vis.rows.is_empty());
        v.jump_fraction(0.5);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 400)
        });
        let first = vis.rows[0].line.number;
        assert!((490..=510).contains(&first), "{first}");
    }

    #[test]
    fn scrollbar_space_is_line_based_when_exact() {
        let (doc, _mem) = doc_with(1000);
        let mut v = DocView::new(doc, &init(false));
        let vis = run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && !vis.rows.is_empty()
        });
        match v.scroll_space(&vis) {
            ScrollSpace::Lines {
                total,
                top,
                visible,
            } => {
                assert_eq!(total, 1000);
                assert_eq!(top, 0);
                assert_eq!(visible, 10);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dragging_the_thumb_reads_the_target_region() {
        let (doc, _mem) = doc_with(1000);
        let mut v = DocView::new(doc, &init(false));
        let vis = run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && !vis.rows.is_empty()
        });
        let space = v.scroll_space(&vis);
        v.scroll_to_thumb(0.5, space);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 100)
        });
        let first = vis.rows[0].line.number;
        assert!((490..=500).contains(&first), "{first}");
        // Dragging to the very bottom follows the end.
        let space = v.scroll_space(&vis);
        v.scroll_to_thumb(1.0, space);
        assert!(v.follow);
    }

    #[test]
    fn selection_and_copy_of_cached_lines() {
        let (doc, _mem) = doc_with(50);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |_, vis| vis.rows.len() == 10);
        let a = v.last_rows[2].clone();
        let b = v.last_rows[4].clone();
        v.select(&a, false);
        v.select(&b, true);
        assert_eq!(v.selection_count(), Some((3, true)));
        assert!(v.selection.unwrap().contains(a.offset + 1));
        v.copy_selection(true);
        let mut copied = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while copied.is_none() {
            let out = v.pump(Instant::now());
            copied = out.copied;
            assert!(Instant::now() < deadline);
        }
        let text = copied.unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].trim_start().starts_with("3  line 00002"), "{text}");
        v.copy_selection(false);
        let out = v.pump(Instant::now());
        assert_eq!(
            out.copied.unwrap().lines().next().unwrap(),
            "line 00002 some text"
        );
    }

    #[test]
    fn copy_reads_lines_that_are_not_cached() {
        let (doc, _mem) = doc_with(3000);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && vis.rows.len() == 10
        });
        v.select_all();
        // Only the first lines are cached; everything must be read.
        v.select_all();
        let deadline = Instant::now() + Duration::from_secs(10);
        // Select-all needs the last line cached, so use explicit points.
        let first = v.last_rows[0].clone();
        let mut last = (*first).clone();
        last.offset = ("line 00000 some text\n".len() * 2999) as u64;
        last.number = 2999;
        last.len = "line 00000 some text\n".len() as u64;
        v.select(&first, false);
        v.select(&last, true);
        v.copy_selection(false);
        let mut copied = None;
        while copied.is_none() {
            let out = v.pump(Instant::now());
            v.update_rows(M, &h);
            copied = out.copied;
            assert!(Instant::now() < deadline, "copy did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(copied.unwrap().lines().count(), 3000);
    }

    #[test]
    fn bookmarks_toggle_and_navigate() {
        let (doc, _mem) = doc_with(400);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && vis.rows.len() == 10
        });
        let l = v.last_rows[3].clone();
        v.select(&l, false);
        v.toggle_bookmark();
        assert!(v.bookmarks.contains_key(&3));
        v.toggle_bookmark();
        assert!(v.bookmarks.is_empty());
        v.add_bookmarks([(100, 0), (300, 0)]);
        v.pos = Pos::at(0);
        v.selection = None;
        v.cursor = None;
        v.goto_bookmark(Dir::Next);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 50)
        });
        assert!(numbers(&vis).contains(&100), "{:?}", numbers(&vis));
        v.goto_bookmark(Dir::Next);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 250)
        });
        assert!(numbers(&vis).contains(&300));
        v.goto_bookmark(Dir::Prev);
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number < 200)
        });
        assert!(numbers(&vis).contains(&100));
    }

    #[test]
    fn marks_record_the_end_of_the_document() {
        let (doc, _mem) = doc_with(10);
        let mut v = DocView::new(doc, &init(true));
        run_until(&mut v, |v, _| v.snapshot.utf8_len > 0);
        v.add_mark();
        assert!(v.marks.contains(&v.snapshot.utf8_len));
    }

    #[test]
    fn truncation_resets_the_view() {
        let (doc, mem) = doc_with(100);
        let mut v = DocView::new(doc, &init(true));
        run_until(&mut v, |_, vis| {
            vis.rows.last().is_some_and(|r| r.line.number == 99)
        });
        v.add_mark();
        mem.replace(b"fresh 1\nfresh 2\n".to_vec());
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.last().is_some_and(|r| r.line.text == "fresh 2")
        });
        assert_eq!(vis.rows.len(), 2);
        assert!(v.marks.is_empty());
        assert!(v.banner.is_some());
        assert!(v.follow);
    }

    #[test]
    fn start_lines_positions_the_top() {
        let (doc, _mem) = doc_with(500);
        let mut v = DocView::new(
            doc,
            &ViewInit {
                follow: true,
                start_lines: Some(50),
                ..ViewInit::default()
            },
        );
        let vis = run_until(&mut v, |_, vis| vis.rows.len() == 10);
        assert_eq!(numbers(&vis)[0], 450);
        assert!(!v.follow);
        // Scrolling to the end resumes following.
        v.scroll_px(100_000.0);
        run_until(&mut v, |v, _| v.follow);
    }

    #[test]
    fn initial_line_is_restored() {
        let (doc, _mem) = doc_with(800);
        let mut v = DocView::new(
            doc,
            &ViewInit {
                follow: false,
                initial_line: Some(300),
                ..ViewInit::default()
            },
        );
        let vis = run_until(&mut v, |_, vis| {
            vis.rows.first().is_some_and(|r| r.line.number > 200)
        });
        assert!(numbers(&vis).contains(&300));
    }

    #[test]
    fn filtered_view_shows_only_matching_lines() {
        let (doc, _mem) = doc_with(300);
        let mut v = DocView::new(doc, &init(false));
        v.filter.entries = vec![crate::filter::FilterEntry::include("0004")];
        v.filter.changed();
        let vis = run_until(&mut v, |v, vis| {
            v.is_filtered() && v.filter.status.done && vis.rows.len() == 10
        });
        // Lines 40..49 and nothing else (line 4, 14, ... contain "0004"? no: "line 00004" does).
        let texts: Vec<&str> = vis.rows.iter().map(|r| r.line.text.as_str()).collect();
        assert!(texts.iter().all(|t| t.contains("0004")), "{texts:?}");
        assert!(matches!(v.scroll_space(&vis), ScrollSpace::Lines { .. }));
    }

    #[test]
    fn filtered_view_keeps_the_cursor_line() {
        let (doc, _mem) = doc_with(300);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |v, vis| {
            v.snapshot.lines.exact && !vis.rows.is_empty()
        });
        v.filter.entries = vec![crate::filter::FilterEntry::include("00042")];
        v.filter.changed();
        run_until(&mut v, |v, vis| {
            v.is_filtered() && v.filter.status.done && !vis.rows.is_empty()
        });
        let off = v.last_rows[0].offset;
        v.filter.enabled = false;
        v.jump_offset(off, false);
        let vis = run_until(&mut v, |v, vis| {
            !v.is_filtered() && vis.rows.first().is_some_and(|r| r.line.number == 42)
        });
        assert_eq!(numbers(&vis)[0], 42);
    }

    #[test]
    fn stale_generation_results_are_ignored() {
        let (doc, _mem) = doc_with(20);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |_, vis| !vis.rows.is_empty());
        let before = v.cache.len();
        v.on_lines(
            RequestId(999),
            12345,
            vec![Line {
                number: 0,
                number_exact: true,
                offset: 99_999,
                len: 3,
                text: "x".into(),
                truncated: false,
            }],
        );
        assert_eq!(v.cache.len(), before);
    }

    #[test]
    fn metrics_view_rows() {
        assert_eq!(M.view_rows(), 10);
        assert_eq!(
            Metrics {
                view_h: 0.0,
                row_h: 16.0
            }
            .view_rows(),
            1
        );
        assert_eq!(
            Metrics {
                view_h: 100.0,
                row_h: 0.0
            }
            .view_rows(),
            1
        );
    }

    #[test]
    fn selection_bounds_are_ordered() {
        let p = |o| SelPoint {
            offset: o,
            number: o,
            exact: true,
        };
        let s = Selection {
            anchor: p(10),
            cursor: p(3),
        };
        assert_eq!(s.bounds().0.offset, 3);
        assert!(s.contains(5) && s.contains(3) && s.contains(10));
        assert!(!s.contains(11));
    }

    #[test]
    fn clock_formats_utc() {
        let t = std::time::UNIX_EPOCH + Duration::from_secs(3600 * 13 + 60 * 5 + 9);
        assert_eq!(clock(t), "13:05:09");
    }

    #[test]
    fn new_lines_for_alerts_start_after_the_initial_index() {
        let (doc, mem) = doc_with(10);
        let mut v = DocView::new(doc, &init(false));
        run_until(&mut v, |v, _| {
            v.snapshot.lines.exact && v.snapshot.lines.known == 10
        });
        assert_eq!(v.take_new_lines(), None);
        assert_eq!(v.alert_next, Some(10));
        mem.append(b"x\ny\n");
        run_until(&mut v, |v, _| v.snapshot.lines.known == 12);
        assert_eq!(v.take_new_lines(), Some((0, 10, 2)));
        assert_eq!(v.take_new_lines(), None);
    }
}
