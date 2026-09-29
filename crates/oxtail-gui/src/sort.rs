//! Sorting a filtered view by a column (PLAN.md section 8.3).
//!
//! Sorting a whole 100M-line file interactively is not realistic, so it is
//! offered on **filtered views** up to `Settings::sort_max_rows` rows (default
//! one million). The work runs on a worker thread ([`SortJob`]):
//!
//! 1. the lines of the filter set are read by offset in chunks
//!    (`Document::read_offsets_blocking_with_generation`, never on the UI
//!    thread),
//! 2. each line is parsed with the tab's parser and the sort column becomes a
//!    typed [`SortKey`],
//! 3. the *records* are sorted stably: a line the parser does not accept (a
//!    continuation line such as a stack trace) belongs to the record before
//!    it in the filter set and travels with it, so the sort orders groups, not
//!    lines (PLAN.md section 8.2),
//! 4. the result is a [`SortedOrder`], the third row space of the view (see
//!    [`crate::scroll::RowSpace::Sorted`]).
//!
//! A line the parser does not accept joins the previous *row of the filter
//! set* (not the previous line of the file): with context lines or a plain
//! text filter a stack line whose record is not in the view attaches to
//! whatever record row precedes it. Lines cut at the display limit are
//! parsed as far as they go, and sort last when even that fails.
//!
//! Memory (bounded by `Settings::sort_max_rows`, at most 10M): the finished
//! order takes 24 bytes per row (8 for the display-order offset, 16 for the
//! offset-to-position index). While a job runs it also holds, per row, the
//! file-order offset (8), the group key (about 48 plus the text of a text
//! key, at most 64 characters, twice when its case differs) and the sort
//! permutation (4).
//!
//! Cancellation: every job holds a generation token (`Arc<AtomicU64>` shared
//! with the [`SortState`]); starting another job, [`SortState::cancel`] or
//! dropping the state bumps it and the worker stops at the next chunk. While a
//! new order is computed the previous one keeps being shown (when it still
//! describes the same filter), otherwise the unsorted filtered view is.
//!
//! [`SortState::tick`] is the per-frame driver: it decides whether a sort is
//! wanted, (re)starts jobs (debounced when only the filter set grew, for
//! instance while following a file) and installs finished orders. Everything
//! but the worker itself is plain data and unit tested.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use oxtail_columns::{ColumnKind, SortKey, sort_key};
use oxtail_core::Document;
use oxtail_search::MatchSet;

use crate::qfilter::{QueryContext, TimeAdapter};

/// Default value of `Settings::sort_max_rows`.
pub const DEFAULT_MAX_ROWS: u64 = 1_000_000;
/// How long a changed filter set must wait before the view is sorted again
/// (a followed file adds matches all the time).
pub const RESORT_DEBOUNCE: Duration = Duration::from_millis(300);
/// The longest a filter set that keeps changing can postpone a re-sort.
pub const RESORT_MAX_WAIT: Duration = Duration::from_secs(3);
/// Lines read per request.
const CHUNK: usize = 2000;

/// Which column is sorted, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    /// Schema index of the column.
    pub col: usize,
    /// Largest first.
    pub descending: bool,
}

/// What the user asked for in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortChoice {
    /// Sort ascending.
    Ascending,
    /// Sort descending.
    Descending,
    /// Back to file order.
    Clear,
}

/// The next choice when the header of `col` is clicked: ascending, then
/// descending, then off.
pub fn cycle(current: Option<SortSpec>, col: usize) -> SortChoice {
    match current {
        Some(s) if s.col == col && !s.descending => SortChoice::Descending,
        Some(s) if s.col == col => SortChoice::Clear,
        _ => SortChoice::Ascending,
    }
}

/// The spec a [`SortChoice`] on `col` leads to.
pub fn apply_choice(col: usize, choice: SortChoice) -> Option<SortSpec> {
    match choice {
        SortChoice::Ascending => Some(SortSpec {
            col,
            descending: false,
        }),
        SortChoice::Descending => Some(SortSpec {
            col,
            descending: true,
        }),
        SortChoice::Clear => None,
    }
}

/// Whether sorting is offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortOffer {
    /// The header offers it.
    Available,
    /// Disabled; the text says why (shown as a tooltip).
    Unavailable(String),
}

/// Formats `n` with thousands separators (`1,000,000`).
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Decides whether sorting is offered for a view: it needs the filtered view
/// and at most `max_rows` rows.
pub fn offer(filtered: bool, rows: u64, max_rows: u64) -> SortOffer {
    if !filtered {
        return SortOffer::Unavailable(
            "Sorting needs a filtered view: add a filter first. \
             For whole files use Go to time."
                .into(),
        );
    }
    if rows > max_rows {
        return SortOffer::Unavailable(format!(
            "Sorting is available on filtered views up to {} rows; \
             use Go to time for whole files",
            group_digits(max_rows)
        ));
    }
    SortOffer::Available
}

// ------------------------------------------------------------------- order

/// The rows of a sorted view: line start offsets in display order, with an
/// offset-to-position index.
///
/// Memory is proportional to the number of rows (bounded by the sort limit),
/// two words per row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortedOrder {
    /// Line offsets in display order.
    offsets: Vec<u64>,
    /// `(offset, position)` sorted by offset.
    by_offset: Vec<(u64, u32)>,
}

impl SortedOrder {
    /// An order over `offsets` (display order). The offsets must be distinct
    /// and fewer than `u32::MAX`; anything beyond that limit is dropped.
    pub fn new(mut offsets: Vec<u64>) -> Self {
        offsets.truncate(u32::MAX as usize);
        let mut by_offset: Vec<(u64, u32)> = offsets
            .iter()
            .enumerate()
            .map(|(i, &o)| (o, i as u32))
            .collect();
        by_offset.sort_unstable();
        Self { offsets, by_offset }
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The offset of the row at `position`.
    pub fn get(&self, position: usize) -> Option<u64> {
        self.offsets.get(position).copied()
    }

    /// The position of the row starting at `offset` (binary search, so
    /// bounded by `log(rows)`).
    pub fn position(&self, offset: u64) -> Option<usize> {
        let i = self.by_offset.partition_point(|&(o, _)| o < offset);
        match self.by_offset.get(i) {
            Some(&(o, p)) if o == offset => Some(p as usize),
            _ => None,
        }
    }

    /// Whether the row at `offset` is part of the order.
    pub fn contains(&self, offset: u64) -> bool {
        self.position(offset).is_some()
    }

    /// The row after the one at `offset`, or `None` at the last row (and for
    /// an offset that is not in the order).
    pub fn next_after(&self, offset: u64) -> Option<u64> {
        self.get(self.position(offset)? + 1)
    }

    /// The row before the one at `offset`, or `None` at the first row (and for
    /// an offset that is not in the order).
    pub fn prev_before(&self, offset: u64) -> Option<u64> {
        self.get(self.position(offset)?.checked_sub(1)?)
    }

    /// The offsets of the rows at positions `lo..=hi` (clamped).
    pub fn slice(&self, lo: usize, hi: usize) -> &[u64] {
        let hi = hi.min(self.offsets.len().saturating_sub(1));
        if self.offsets.is_empty() || lo > hi {
            return &[];
        }
        &self.offsets[lo..=hi]
    }

    /// All offsets in display order.
    pub fn offsets(&self) -> &[u64] {
        &self.offsets
    }
}

// ------------------------------------------------------------- minimap

/// Most items the minimap of a sorted view looks at per refresh, so the work
/// on the UI thread stays bounded whatever the size of the file or the search.
pub const MINIMAP_WORK_CAP: usize = 100_000;
/// Fewest time between two refreshes of a sorted minimap.
pub const MINIMAP_REFRESH: Duration = Duration::from_millis(250);

/// Counts, per minimap bin, the rows of `order` whose offset is in `marks`,
/// binned by position in the order. Iterates the smaller of the two sides
/// (each item costs one binary search) and returns `None` when even that is
/// more than `cap` items: the strip then shows nothing rather than stalling
/// the frame.
pub fn position_bins(
    order: &SortedOrder,
    marks: &MatchSet,
    bins: usize,
    cap: usize,
) -> Option<Vec<u32>> {
    let total = order.len() as u64;
    let mut out = vec![0u32; bins];
    if bins == 0 || total == 0 {
        return Some(out);
    }
    if order.len().min(marks.len()) > cap {
        return None;
    }
    let mut add = |p: usize| {
        let b = crate::minimap::bin_of(p as u64, total, bins);
        out[b] = out[b].saturating_add(1);
    };
    if marks.len() <= order.len() {
        for o in marks.iter() {
            if let Some(p) = order.position(o) {
                add(p);
            }
        }
    } else {
        for (p, &o) in order.offsets().iter().enumerate() {
            if marks.contains(o) {
                add(p);
            }
        }
    }
    Some(out)
}

// --------------------------------------------------------------- worker

/// What a sort job works on.
#[derive(Clone)]
pub struct SortInput {
    /// The document to read lines from.
    pub doc: Arc<Document>,
    /// The tab's parser and time parser.
    pub ctx: Arc<QueryContext>,
    /// The rows to sort (a snapshot of the filter set).
    pub set: Arc<MatchSet>,
    /// The column and direction.
    pub spec: SortSpec,
    /// Refuse sets larger than this.
    pub max_rows: u64,
    /// The document generation the set belongs to.
    pub generation: u64,
}

/// How a sort ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortEnd {
    /// The sorted rows.
    Done(SortedOrder),
    /// The token went stale (a newer request superseded this one).
    Stale,
    /// The document changed generation while reading.
    Changed,
    /// The set has more rows than the limit.
    TooLarge(u64),
    /// The column does not exist in the parser's schema.
    BadColumn,
    /// Reading lines failed repeatedly.
    Failed(String),
}

/// How often a failed read is retried before the sort gives up.
const READ_RETRIES: usize = 3;

/// Reads, parses and sorts. `stale` is polled between chunks; `progress` gets
/// `(rows read, rows total)` after each chunk. Never panics on odd data:
/// lines that cannot be read count as continuation lines.
pub fn compute(
    input: &SortInput,
    stale: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> SortEnd {
    let n = input.set.len();
    if n as u64 > input.max_rows {
        return SortEnd::TooLarge(n as u64);
    }
    let parser = &input.ctx.parser;
    let Some(info) = parser.schema().columns.get(input.spec.col) else {
        return SortEnd::BadColumn;
    };
    let kind: ColumnKind = info.kind;
    let col = input.spec.col;
    let ts = TimeAdapter(Arc::clone(&input.ctx.time));

    // File order; groups start at the accepted lines.
    let offsets: Vec<u64> = input.set.iter().collect();
    let mut groups: Vec<(SortKey, u32)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if stale() {
            return SortEnd::Stale;
        }
        let end = (i + CHUNK).min(n);
        let want = &offsets[i..end];
        let mut attempt = 0;
        let (g, lines) = loop {
            let (g, r) = input.doc.read_offsets_blocking_with_generation(want);
            match r {
                Ok(lines) => break (g, lines),
                Err(e) if attempt >= READ_RETRIES => {
                    return SortEnd::Failed(format!("Reading the lines failed: {e}"));
                }
                Err(_) => {
                    attempt += 1;
                    if stale() {
                        return SortEnd::Stale;
                    }
                }
            }
        };
        if g != input.generation || input.doc.generation() != input.generation {
            return SortEnd::Changed;
        }
        let mut consumed = 0usize;
        let mut li = 0usize;
        for (k, &off) in want.iter().enumerate() {
            match lines.get(li) {
                Some(l) if l.offset == off => {
                    li += 1;
                    consumed = k + 1;
                    match parser.parse(&l.text) {
                        Some(rec) => {
                            let key = sort_key(kind, rec.get(col).unwrap_or(""), Some(&ts));
                            groups.push((key, (i + k) as u32));
                        }
                        // Cut at the display limit: not a continuation line
                        // of some other record. It has no usable key.
                        None if l.truncated => groups.push((SortKey::Missing, (i + k) as u32)),
                        None => {}
                    }
                }
                // A later line came back, so this one could not be read: it
                // stays with the record before it.
                Some(_) => consumed = k + 1,
                // Out of lines: the response was cut short (size limit) or the
                // rest is past the end. Ask again from here.
                None => break,
            }
        }
        if consumed == 0 {
            // An empty answer: skip this one offset (unreadable or past the
            // end), not the whole chunk.
            consumed = 1;
        }
        i += consumed;
        progress(i as u64, n as u64);
    }
    if stale() {
        return SortEnd::Stale;
    }
    // Lines before the first record have no key: they form a group that sorts
    // last, like any other value that is missing.
    if n > 0 && groups.first().is_none_or(|g| g.1 != 0) {
        groups.insert(0, (SortKey::Missing, 0));
    }
    let mut idx: Vec<u32> = (0..groups.len() as u32).collect();
    let desc = input.spec.descending;
    // `sort_by` is stable: equal keys keep their file order.
    idx.sort_by(|&a, &b| {
        groups[a as usize]
            .0
            .cmp_directed(&groups[b as usize].0, desc)
    });
    let mut out = Vec::with_capacity(n);
    for g in idx {
        let start = groups[g as usize].1 as usize;
        let stop = groups.get(g as usize + 1).map_or(n, |next| next.1 as usize);
        out.extend_from_slice(&offsets[start..stop]);
    }
    SortEnd::Done(SortedOrder::new(out))
}

/// A message from a sort worker.
#[derive(Debug)]
pub enum SortMsg {
    /// Rows read so far.
    Progress {
        /// Rows read.
        done: u64,
        /// Rows to read.
        total: u64,
    },
    /// The job ended.
    Finished(SortEnd),
}

/// A running sort. Dropping it does not stop the worker by itself: the token
/// of the owning [`SortState`] does.
pub struct SortJob {
    rx: Receiver<(u64, SortMsg)>,
    id: u64,
}

impl SortJob {
    /// Starts a worker. Bumps `token`, which makes every earlier job stale.
    /// `wake` requests a repaint when a message is sent. `None` when the
    /// thread cannot be spawned.
    pub fn start(
        input: SortInput,
        token: Arc<AtomicU64>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Self> {
        let id = token.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = unbounded();
        let spawned = std::thread::Builder::new()
            .name("oxtail-sort".into())
            .spawn(move || {
                let stale = || token.load(Ordering::Relaxed) != id;
                let mut last = Instant::now();
                let end = compute(&input, &stale, &mut |done, total| {
                    if last.elapsed() >= Duration::from_millis(50) {
                        last = Instant::now();
                        let _ = tx.send((id, SortMsg::Progress { done, total }));
                        wake();
                    }
                });
                let _ = tx.send((id, SortMsg::Finished(end)));
                wake();
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the sort thread: {e}");
            return None;
        }
        Some(Self { rx, id })
    }

    /// The next message of this job, if any. Never blocks.
    pub fn try_recv(&self) -> Option<SortMsg> {
        while let Ok((id, msg)) = self.rx.try_recv() {
            if id == self.id {
                return Some(msg);
            }
        }
        None
    }
}

// ---------------------------------------------------------------- state

/// What an order (or a running job) was computed for.
#[derive(Clone)]
struct Target {
    spec: SortSpec,
    /// The filter's epoch (bumped whenever its job restarts).
    epoch: u64,
    generation: u64,
    /// Parser and time zone identity.
    signature: Arc<str>,
    /// Rows of the filter set that were sorted. Within one filter epoch a set
    /// only grows, so the count identifies its content (the `Arc` does not:
    /// the search hands out a new one whenever a chunk finishes).
    rows: usize,
}

impl Target {
    /// Same request, ignoring growth of the set.
    fn same_request(&self, o: &Target) -> bool {
        self.spec == o.spec
            && self.epoch == o.epoch
            && self.generation == o.generation
            && self.signature == o.signature
    }

    fn same(&self, o: &Target) -> bool {
        self.same_request(o) && self.rows == o.rows
    }
}

/// What [`SortState::tick`] looks at.
pub struct TickInput<'a> {
    /// Frame time.
    pub now: Instant,
    /// The document.
    pub doc: &'a Arc<Document>,
    /// Wakes the UI when the worker has news.
    pub wake: &'a Arc<dyn Fn() + Send + Sync>,
    /// The rows of the filtered view, when it is showing.
    pub set: Option<&'a Arc<MatchSet>>,
    /// The table view is showing.
    pub table: bool,
    /// The tab's parser context.
    pub ctx: Option<&'a Arc<QueryContext>>,
    /// The filter's epoch.
    pub filter_epoch: u64,
    /// The document generation.
    pub generation: u64,
}

/// What [`SortState::tick`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickOutcome {
    /// Something changed that needs a repaint (or a poll soon).
    pub repaint: bool,
    /// A new order was installed.
    pub installed: bool,
    /// The installed order is the first one for a freshly requested sort: the
    /// view should show its top.
    pub scroll_top: bool,
}

/// The last answer of [`SortState::offer`] and what it was computed for.
type OfferCache = ((bool, u64, u64), Arc<SortOffer>);

/// The sort of one tab.
pub struct SortState {
    /// The requested sort.
    pub spec: Option<SortSpec>,
    /// Largest filtered view that is sorted (`Settings::sort_max_rows`).
    pub max_rows: u64,
    /// The view is bigger than `max_rows`: rows in the view when refused.
    pub refused: Option<u64>,
    /// Rows read and to read while a job runs.
    pub progress: Option<(u64, u64)>,
    /// Why the last job failed.
    pub problem: Option<String>,
    order: Option<Arc<SortedOrder>>,
    order_target: Option<Target>,
    job: Option<SortJob>,
    job_target: Option<Target>,
    /// The set had this many rows when a re-sort was last considered.
    seen_rows: Option<usize>,
    dirty_first: Option<Instant>,
    dirty_last: Option<Instant>,
    /// A request that failed: not retried until something changes.
    failed: Option<Target>,
    /// The parser signature, cached per context.
    sig: Option<(Arc<QueryContext>, Arc<str>)>,
    scroll_top: bool,
    token: Arc<AtomicU64>,
    offer: std::cell::RefCell<Option<OfferCache>>,
}

impl Default for SortState {
    fn default() -> Self {
        Self {
            spec: None,
            max_rows: DEFAULT_MAX_ROWS,
            refused: None,
            progress: None,
            problem: None,
            order: None,
            order_target: None,
            job: None,
            job_target: None,
            seen_rows: None,
            dirty_first: None,
            dirty_last: None,
            failed: None,
            sig: None,
            scroll_top: false,
            token: Arc::new(AtomicU64::new(0)),
            offer: std::cell::RefCell::new(None),
        }
    }
}

impl Drop for SortState {
    fn drop(&mut self) {
        self.token.fetch_add(1, Ordering::SeqCst);
    }
}

impl SortState {
    /// Sets the requested sort (`None` clears it). A different request
    /// scrolls to the top of the result when it arrives.
    pub fn set(&mut self, spec: Option<SortSpec>) {
        if spec == self.spec {
            return;
        }
        self.spec = spec;
        self.scroll_top = spec.is_some();
        self.refused = None;
        self.problem = None;
        if spec.is_none() {
            self.clear();
        }
    }

    /// Drops the sort and everything computed for it, and stops the worker.
    pub fn clear(&mut self) {
        self.spec = None;
        self.cancel();
        self.order = None;
        self.order_target = None;
        self.refused = None;
        self.problem = None;
        self.scroll_top = false;
        self.failed = None;
    }

    /// Stops the running job (its result is discarded).
    pub fn cancel(&mut self) {
        self.token.fetch_add(1, Ordering::SeqCst);
        self.job = None;
        self.job_target = None;
        self.progress = None;
        self.dirty_first = None;
        self.dirty_last = None;
        self.seen_rows = None;
    }

    /// Whether a job is running.
    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// Whether the state wants another frame soon (a job runs or a re-sort is
    /// waiting out its debounce).
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.dirty_first.is_some()
    }

    /// The order to show: the last computed one, while it still describes the
    /// current request on the current filter.
    pub fn display_order(&self, filter_epoch: u64, generation: u64) -> Option<Arc<SortedOrder>> {
        let spec = self.spec?;
        let t = self.order_target.as_ref()?;
        (t.spec == spec && t.epoch == filter_epoch && t.generation == generation)
            .then(|| self.order.clone())
            .flatten()
    }

    /// The share of rows read while a job runs, in `0.0..=1.0`.
    pub fn fraction(&self) -> Option<f32> {
        let (d, t) = self.progress?;
        Some(if t == 0 { 0.0 } else { d as f32 / t as f32 })
    }

    /// Per-frame driver: starts, restarts, cancels and finishes jobs.
    pub fn tick(&mut self, inp: &TickInput<'_>) -> TickOutcome {
        let mut out = TickOutcome::default();
        // Nothing to do without a request.
        let Some(spec) = self.spec else {
            if self.job.is_some() {
                self.cancel();
            }
            return out;
        };
        // The request only makes sense on a filtered table.
        let (Some(set), Some(ctx)) = (inp.set, inp.ctx) else {
            self.clear();
            out.repaint = true;
            return out;
        };
        if !inp.table || ctx.parser.schema().columns.get(spec.col).is_none() {
            self.clear();
            out.repaint = true;
            return out;
        }
        self.poll(&mut out);

        if set.len() as u64 > self.max_rows {
            // Too big (the followed file grew past the limit, say).
            if self.refused != Some(set.len() as u64) || self.order.is_some() {
                self.cancel();
                self.order = None;
                self.order_target = None;
                self.refused = Some(set.len() as u64);
                out.repaint = true;
            }
            return out;
        }
        self.refused = None;

        let rows = set.len();
        if self.sig.as_ref().is_none_or(|(c, _)| !Arc::ptr_eq(c, ctx)) {
            self.sig = Some((Arc::clone(ctx), ctx.signature().into()));
        }
        let signature = self
            .sig
            .as_ref()
            .map_or_else(|| Arc::from(""), |(_, s)| Arc::clone(s));
        let want = Target {
            spec,
            epoch: inp.filter_epoch,
            generation: inp.generation,
            signature,
            rows,
        };
        if self.failed.as_ref().is_some_and(|t| t.same_request(&want)) {
            return out; // do not retry a failing request every frame
        }
        self.failed = None;
        if let Some(jt) = &self.job_target {
            if jt.same_request(&want) {
                // Never pre-empt a running job for the same request: rows
                // that arrived meanwhile are picked up by the next one.
                out.repaint = true;
                return out;
            }
        } else if self.order_target.as_ref().is_some_and(|t| t.same(&want)) {
            self.dirty_first = None;
            self.dirty_last = None;
            self.seen_rows = None;
            return out;
        }
        // A sort is needed. Only growth of the filter shown is debounced (from
        // the last change, but not for longer than RESORT_MAX_WAIT); another
        // request starts at once.
        let immediate = match &self.order_target {
            Some(r) if self.job_target.is_none() => !r.same_request(&want),
            _ => true,
        };
        if !immediate {
            if self.seen_rows != Some(rows) {
                self.seen_rows = Some(rows);
                self.dirty_last = Some(inp.now);
            }
            let first = *self.dirty_first.get_or_insert(inp.now);
            let last = self.dirty_last.unwrap_or(inp.now);
            if inp.now.duration_since(last) < RESORT_DEBOUNCE
                && inp.now.duration_since(first) < RESORT_MAX_WAIT
            {
                out.repaint = true;
                return out;
            }
        }
        self.dirty_first = None;
        self.dirty_last = None;
        self.seen_rows = None;
        self.progress = Some((0, rows as u64));
        let job = SortJob::start(
            SortInput {
                doc: Arc::clone(inp.doc),
                ctx: Arc::clone(ctx),
                set: Arc::clone(set),
                spec,
                max_rows: self.max_rows,
                generation: inp.generation,
            },
            Arc::clone(&self.token),
            Arc::clone(inp.wake),
        );
        match job {
            Some(job) => {
                self.job = Some(job);
                self.job_target = Some(want);
            }
            None => {
                self.job = None;
                self.job_target = None;
                self.progress = None;
                self.problem = Some("Cannot start the sort thread".into());
                self.failed = Some(want);
            }
        }
        out.repaint = true;
        out
    }

    /// Whether sorting is offered (see [`offer`]); the answer is cached, so
    /// asking every frame does not allocate.
    pub fn offer(&self, filtered: bool, rows: u64) -> Arc<SortOffer> {
        let key = (filtered, rows, self.max_rows);
        let mut c = self.offer.borrow_mut();
        if let Some((k, o)) = c.as_ref()
            && *k == key
        {
            return Arc::clone(o);
        }
        let o = Arc::new(offer(filtered, rows, self.max_rows));
        *c = Some((key, Arc::clone(&o)));
        o
    }

    fn poll(&mut self, out: &mut TickOutcome) {
        while let Some(msg) = self.job.as_ref().and_then(SortJob::try_recv) {
            out.repaint = true;
            match msg {
                SortMsg::Progress { done, total } => self.progress = Some((done, total)),
                SortMsg::Finished(end) => {
                    let target = self.job_target.take();
                    self.job = None;
                    self.progress = None;
                    match end {
                        SortEnd::Done(order) => {
                            self.order = Some(Arc::new(order));
                            self.order_target = target;
                            self.problem = None;
                            out.installed = true;
                            out.scroll_top = std::mem::take(&mut self.scroll_top);
                        }
                        SortEnd::Stale | SortEnd::Changed => {}
                        SortEnd::TooLarge(n) => self.refused = Some(n),
                        SortEnd::BadColumn => {
                            self.problem = Some("The column no longer exists".into());
                            self.failed = target;
                        }
                        SortEnd::Failed(why) => {
                            self.problem = Some(why);
                            self.failed = target;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};
    use oxtail_columns::ParserSpec;
    use oxtail_time::{TimeContext, TimeParser};

    /// A logfmt context for `text` where `n` is a number column.
    fn ctx_for(text: &str) -> (Arc<QueryContext>, Arc<Document>) {
        ctx_with_doc(text, doc(text))
    }

    fn ctx_with_doc(text: &str, d: Arc<Document>) -> (Arc<QueryContext>, Arc<Document>) {
        let _ = text;
        let columns = vec!["n".to_string(), "id".to_string()];
        let mut kinds = std::collections::BTreeMap::new();
        kinds.insert("n".to_string(), ColumnKind::Number);
        let spec = ParserSpec::Logfmt { columns, kinds };
        (
            Arc::new(QueryContext {
                parser: Arc::new(spec.compile().expect("compile")),
                time: Arc::new(TimeParser::new(TimeContext::utc())),
            }),
            d,
        )
    }

    fn input(text: &str, offsets: Vec<u64>, desc: bool) -> SortInput {
        let (ctx, d) = ctx_for(text);
        let col = ctx.parser.schema().find("n").expect("column n");
        SortInput {
            generation: d.generation(),
            doc: d,
            ctx,
            set: Arc::new(MatchSet::from_offsets(offsets)),
            spec: SortSpec {
                col,
                descending: desc,
            },
            max_rows: DEFAULT_MAX_ROWS,
        }
    }

    /// The offsets of every line of `text`.
    fn line_offsets(text: &str) -> Vec<u64> {
        let mut v = Vec::new();
        let mut at = 0u64;
        for l in text.split_inclusive('\n') {
            v.push(at);
            at += l.len() as u64;
        }
        v
    }

    fn run(i: &SortInput) -> SortEnd {
        compute(i, &|| false, &mut |_, _| {})
    }

    fn sorted(i: &SortInput) -> SortedOrder {
        match run(i) {
            SortEnd::Done(o) => o,
            other => panic!("{other:?}"),
        }
    }

    /// The line texts of `order`.
    fn texts(text: &str, o: &SortedOrder) -> Vec<String> {
        let all: Vec<&str> = text.lines().collect();
        let starts = line_offsets(text);
        o.offsets()
            .iter()
            .map(|off| {
                let i = starts.iter().position(|s| s == off).expect("line start");
                all[i].to_string()
            })
            .collect()
    }

    fn ids(text: &str, o: &SortedOrder) -> Vec<String> {
        texts(text, o)
            .iter()
            .map(|t| t.rsplit('=').next().unwrap().to_string())
            .collect()
    }

    const DATA: &str = "n=10 id=a\nn=9 id=b\nn=2 id=c\nn=9 id=d\nn=x id=e\nn=100 id=f\n";

    #[test]
    fn sorts_numerically_ascending_with_stable_ties_and_missing_last() {
        let o = sorted(&input(DATA, line_offsets(DATA), false));
        // 2, 9 (b), 9 (d), 10, 100, then the unparseable one.
        assert_eq!(ids(DATA, &o), ["c", "b", "d", "a", "f", "e"]);
    }

    #[test]
    fn descending_keeps_ties_in_file_order_and_missing_last() {
        let o = sorted(&input(DATA, line_offsets(DATA), true));
        assert_eq!(ids(DATA, &o), ["f", "a", "b", "d", "c", "e"]);
    }

    #[test]
    fn only_the_set_is_sorted() {
        let offs = line_offsets(DATA);
        let o = sorted(&input(DATA, vec![offs[0], offs[2], offs[5]], false));
        assert_eq!(texts(DATA, &o), ["n=2 id=c", "n=10 id=a", "n=100 id=f"]);
    }

    #[test]
    fn continuation_lines_stay_with_their_record() {
        // The indented lines are not logfmt records.
        let text = "n=3 id=a\n  at foo\nn=1 id=b\nn=2 id=c\n  at bar\n  at baz\n";
        let mut i = input(text, line_offsets(text), false);
        assert!(i.ctx.parser.parse("  at foo").is_none());
        assert_eq!(
            texts(text, &sorted(&i)),
            [
                "n=1 id=b", "n=2 id=c", "  at bar", "  at baz", "n=3 id=a", "  at foo"
            ]
        );
        i.spec.descending = true;
        assert_eq!(
            texts(text, &sorted(&i)),
            [
                "n=3 id=a", "  at foo", "n=2 id=c", "  at bar", "  at baz", "n=1 id=b"
            ]
        );
    }

    #[test]
    fn lines_before_the_first_record_sort_last() {
        let text = "  orphan\nn=2 id=a\nn=1 id=b\n";
        let o = sorted(&input(text, line_offsets(text), false));
        assert_eq!(texts(text, &o), ["n=1 id=b", "n=2 id=a", "  orphan"]);
    }

    #[test]
    fn a_stale_token_stops_the_sort() {
        let text: String = (0..6000).map(|i| format!("n={i} id=x\n")).collect();
        let i = input(&text, line_offsets(&text), false);
        let calls = std::cell::Cell::new(0);
        let end = compute(&i, &|| calls.get() >= 1, &mut |_, _| {
            calls.set(calls.get() + 1)
        });
        assert_eq!(end, SortEnd::Stale);
        assert_eq!(calls.get(), 1, "stopped after the first chunk");
    }

    #[test]
    fn a_set_above_the_limit_is_refused() {
        let mut i = input(DATA, line_offsets(DATA), false);
        i.max_rows = 5;
        assert_eq!(run(&i), SortEnd::TooLarge(6));
        i.max_rows = 6;
        assert!(matches!(run(&i), SortEnd::Done(_)));
    }

    #[test]
    fn a_missing_column_is_reported() {
        let mut i = input(DATA, line_offsets(DATA), false);
        i.spec.col = 99;
        assert_eq!(run(&i), SortEnd::BadColumn);
    }

    #[test]
    fn the_worker_reports_and_a_newer_job_supersedes_it() {
        let text: String = (0..3000).rev().map(|i| format!("n={i} id=x\n")).collect();
        let i = input(&text, line_offsets(&text), false);
        let token = Arc::new(AtomicU64::new(0));
        let job = SortJob::start(i.clone(), Arc::clone(&token), Arc::new(|| {})).unwrap();
        let end = wait("the sort", || match job.try_recv() {
            Some(SortMsg::Finished(e)) => Some(e),
            _ => None,
        });
        let SortEnd::Done(o) = end else {
            panic!("{end:?}")
        };
        assert_eq!(o.len(), 3000);
        // Ascending: the last line of the file (n=0) comes first.
        assert_eq!(o.get(0), Some(*line_offsets(&text).last().unwrap()));
        // Starting another job makes the token of the earlier one stale.
        let _newer = SortJob::start(i, Arc::clone(&token), Arc::new(|| {})).unwrap();
        assert_eq!(token.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn order_lookups() {
        let o = SortedOrder::new(vec![50, 10, 30]);
        assert_eq!(o.len(), 3);
        assert_eq!(o.position(10), Some(1));
        assert_eq!(o.position(11), None);
        assert_eq!(o.next_after(50), Some(10));
        assert_eq!(o.next_after(30), None);
        assert_eq!(o.prev_before(30), Some(10));
        assert_eq!(o.prev_before(50), None);
        assert_eq!(o.next_after(99), None);
        assert_eq!(o.slice(1, 9), &[10, 30]);
        assert!(o.slice(5, 9).is_empty());
        assert!(SortedOrder::default().slice(0, 3).is_empty());
    }

    #[test]
    fn clicking_cycles_ascending_descending_off() {
        let a = apply_choice(2, SortChoice::Ascending);
        assert_eq!(cycle(None, 2), SortChoice::Ascending);
        assert_eq!(cycle(a, 2), SortChoice::Descending);
        let d = apply_choice(2, SortChoice::Descending);
        assert_eq!(cycle(d, 2), SortChoice::Clear);
        assert_eq!(apply_choice(2, SortChoice::Clear), None);
        // Another column starts over.
        assert_eq!(cycle(d, 1), SortChoice::Ascending);
    }

    #[test]
    fn the_offer_explains_why_not() {
        assert_eq!(offer(true, 10, 1000), SortOffer::Available);
        assert_eq!(offer(true, 1000, 1000), SortOffer::Available);
        match offer(true, 1_000_001, 1_000_000) {
            SortOffer::Unavailable(m) => {
                assert!(m.contains("1,000,000 rows"), "{m}");
                assert!(m.contains("Go to time"), "{m}");
            }
            o => panic!("{o:?}"),
        }
        assert!(matches!(offer(false, 0, 10), SortOffer::Unavailable(_)));
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    /// Ticks `st` on `set` with the shared test inputs.
    fn tick(
        st: &mut SortState,
        env: &(Arc<QueryContext>, Arc<Document>),
        set: &Arc<MatchSet>,
        now: Instant,
        table: bool,
    ) -> TickOutcome {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        st.tick(&TickInput {
            now,
            doc: &env.1,
            wake: &wake,
            set: Some(set),
            table,
            ctx: Some(&env.0),
            filter_epoch: 1,
            generation: env.1.generation(),
        })
    }

    #[test]
    fn tick_sorts_debounces_growth_and_clears() {
        let env = ctx_for(DATA);
        let g = env.1.generation();
        let col = env.0.parser.schema().find("n").unwrap();
        let offs = line_offsets(DATA);
        let set1 = Arc::new(MatchSet::from_offsets(offs[..4].to_vec()));
        let set2 = Arc::new(MatchSet::from_offsets(offs.clone()));
        let mut st = SortState::default();
        st.set(Some(SortSpec {
            col,
            descending: false,
        }));
        let mut now = Instant::now();
        // The first sort starts at once and installs, scrolling to the top.
        tick(&mut st, &env, &set1, now, true);
        assert!(st.running());
        let out = wait("the first order", || {
            let o = tick(&mut st, &env, &set1, now, true);
            o.installed.then_some(o)
        });
        assert!(out.scroll_top);
        assert_eq!(st.display_order(1, g).unwrap().len(), 4);
        // The set grew: the old order stays until the debounce has passed.
        now += Duration::from_millis(10);
        tick(&mut st, &env, &set2, now, true);
        assert!(!st.running(), "waiting out the debounce");
        assert_eq!(st.display_order(1, g).unwrap().len(), 4);
        now += RESORT_DEBOUNCE + Duration::from_millis(1);
        tick(&mut st, &env, &set2, now, true);
        assert!(st.running());
        let out = wait("the second order", || {
            let o = tick(&mut st, &env, &set2, now, true);
            o.installed.then_some(o)
        });
        assert!(!out.scroll_top, "a re-sort keeps the position");
        assert_eq!(st.display_order(1, g).unwrap().len(), 6);
        // A different filter epoch never shows the old order.
        assert!(st.display_order(2, g).is_none());
        // Turning the table off clears the sort.
        tick(&mut st, &env, &set2, now, false);
        assert!(st.spec.is_none() && st.display_order(1, g).is_none());
    }

    #[test]
    fn tick_refuses_a_view_above_the_limit() {
        let env = ctx_for(DATA);
        let set = Arc::new(MatchSet::from_offsets(line_offsets(DATA)));
        let mut st = SortState::default();
        st.max_rows = 3;
        st.set(Some(SortSpec {
            col: 0,
            descending: false,
        }));
        let out = tick(&mut st, &env, &set, Instant::now(), true);
        assert!(out.repaint);
        assert!(!st.running());
        assert_eq!(st.refused, Some(6));
        assert!(st.display_order(1, env.1.generation()).is_none());
    }

    #[test]
    fn a_truncated_line_is_not_glued_to_the_record_before_it() {
        // Lines are cut at 16 characters; the indented one is not a record,
        // but being cut it must not join `n=2`.
        let text = "n=2 id=a\nn=1 id=b\n    aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        let mem = Arc::new(oxtail_core::MemSource::new(text.as_bytes().to_vec()));
        let d = Arc::new(Document::from_source_with(
            mem,
            "t.log",
            oxtail_core::OpenOptions {
                max_display_len: 16,
                ..Default::default()
            },
        ));
        wait("the index", || {
            crate::docscan::index_complete(&d.snapshot()).then_some(())
        });
        let (ctx, d) = ctx_with_doc(text, d);
        let col = ctx.parser.schema().find("n").unwrap();
        let mut i = SortInput {
            generation: d.generation(),
            doc: d,
            ctx,
            set: Arc::new(MatchSet::from_offsets(line_offsets(text))),
            spec: SortSpec {
                col,
                descending: true,
            },
            max_rows: DEFAULT_MAX_ROWS,
        };
        let o = sorted(&i);
        let starts = line_offsets(text);
        // n=2, n=1, then the cut line (Missing, last), not n=2, cut, n=1.
        assert_eq!(o.offsets(), &[starts[0], starts[1], starts[2]]);
        i.spec.descending = false;
        assert_eq!(sorted(&i).offsets(), &[starts[1], starts[0], starts[2]]);
    }

    #[test]
    fn a_response_cut_by_the_size_limit_is_read_in_several_requests() {
        // 2500 lines of ~4 KB: more than the 8 MB one request may return.
        let pad = "x".repeat(4096);
        let n = 2500;
        let text: String = (0..n)
            .map(|i| format!("n={} pad={pad}\n", n - 1 - i))
            .collect();
        let i = input(&text, line_offsets(&text), false);
        let offs = line_offsets(&text);
        let o = sorted(&i);
        // Ascending by n = file order reversed.
        let want: Vec<u64> = offs.iter().rev().copied().collect();
        assert_eq!(o.offsets(), &want[..]);
    }

    #[test]
    fn position_bins_iterate_the_smaller_side_and_respect_the_cap() {
        let order = SortedOrder::new((0..100u64).rev().map(|i| i * 10).collect());
        let marks = MatchSet::from_offsets(vec![0, 10, 990, 55]);
        // 10 bins over 100 rows: offset 990 is position 0, 0 is position 99,
        // 10 is position 98; 55 is not a row.
        let b = position_bins(&order, &marks, 10, 1000).unwrap();
        assert_eq!(b, vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        // The other direction (more marks than rows) agrees.
        let many = MatchSet::from_offsets((0..500u64).map(|i| i * 10).collect());
        let b2 = position_bins(&order, &many, 10, 1000).unwrap();
        assert_eq!(b2.iter().sum::<u32>(), 100);
        assert!(b2.iter().all(|&c| c == 10));
        // Above the cap on both sides: no work, no result.
        assert!(position_bins(&order, &many, 10, 50).is_none());
        // A small side is enough even against a huge one.
        assert!(position_bins(&order, &marks, 10, 4).is_some());
        assert_eq!(
            position_bins(&SortedOrder::default(), &marks, 4, 10),
            Some(vec![0; 4])
        );
    }

    #[test]
    fn a_new_arc_with_the_same_content_does_not_trigger_a_re_sort() {
        let env = ctx_for(DATA);
        let col = env.0.parser.schema().find("n").unwrap();
        let offs = line_offsets(DATA);
        let mut st = SortState::default();
        st.set(Some(SortSpec {
            col,
            descending: false,
        }));
        let mut now = Instant::now();
        let a = Arc::new(MatchSet::from_offsets(offs.clone()));
        tick(&mut st, &env, &a, now, true);
        wait("the order", || {
            tick(&mut st, &env, &a, now, true).installed.then_some(())
        });
        // The search hands out a new snapshot: same rows, new Arc.
        for _ in 0..5 {
            now += RESORT_DEBOUNCE * 2;
            let b = Arc::new(MatchSet::from_offsets(offs.clone()));
            tick(&mut st, &env, &b, now, true);
            assert!(!st.running() && !st.busy());
        }
    }

    /// Replaces the running job by one that never finishes on its own: the
    /// test sends its result. The job's target stays.
    fn fake_job(st: &mut SortState) -> crossbeam_channel::Sender<(u64, SortMsg)> {
        let (tx, rx) = unbounded();
        st.job = Some(SortJob { rx, id: 7 });
        tx
    }

    #[test]
    fn rows_arriving_during_a_running_job_wait_for_it_and_get_one_re_sort() {
        let env = ctx_for(DATA);
        let g = env.1.generation();
        let col = env.0.parser.schema().find("n").unwrap();
        let offs = line_offsets(DATA);
        let spec = SortSpec {
            col,
            descending: false,
        };
        let mut st = SortState::default();
        st.set(Some(spec));
        let mut now = Instant::now();
        let small = Arc::new(MatchSet::from_offsets(offs[..3].to_vec()));
        // Start a real job to learn the target, then swap in a job we control.
        tick(&mut st, &env, &small, now, true);
        assert!(st.running());
        let tx = fake_job(&mut st);
        // The set grows and time passes far beyond the debounce: the running
        // job is neither cancelled nor replaced.
        let big = Arc::new(MatchSet::from_offsets(offs.clone()));
        let token_before = st.token.load(Ordering::SeqCst);
        for _ in 0..4 {
            now += RESORT_DEBOUNCE * 3;
            tick(&mut st, &env, &big, now, true);
            assert!(st.running());
            assert_eq!(
                st.token.load(Ordering::SeqCst),
                token_before,
                "not pre-empted"
            );
        }
        // It finishes with the old rows and installs.
        let order = SortedOrder::new(offs[..3].to_vec());
        tx.send((7, SortMsg::Finished(SortEnd::Done(order))))
            .unwrap();
        let out = tick(&mut st, &env, &big, now, true);
        assert!(out.installed);
        assert_eq!(st.display_order(1, g).unwrap().len(), 3);
        assert!(!st.running(), "the debounce restarts after the install");
        // Quiet for the debounce: exactly one more sort, over all the rows.
        now += RESORT_DEBOUNCE * 2;
        tick(&mut st, &env, &big, now, true);
        assert!(st.running());
        let out = wait("the second order", || {
            let o = tick(&mut st, &env, &big, now, true);
            o.installed.then_some(o)
        });
        assert!(out.installed);
        assert_eq!(st.display_order(1, g).unwrap().len(), 6);
        now += RESORT_DEBOUNCE * 2;
        tick(&mut st, &env, &big, now, true);
        assert!(!st.running() && !st.busy(), "nothing left to sort");
    }

    #[test]
    fn a_set_that_keeps_growing_is_sorted_after_the_maximum_wait() {
        let text: String = (0..80).map(|i| format!("n={i} id=x\n")).collect();
        let env = ctx_for(&text);
        let col = env.0.parser.schema().find("n").unwrap();
        let offs = line_offsets(&text);
        let mut st = SortState::default();
        st.set(Some(SortSpec {
            col,
            descending: false,
        }));
        let start = Instant::now();
        let mut now = start;
        let s0 = Arc::new(MatchSet::from_offsets(offs[..2].to_vec()));
        tick(&mut st, &env, &s0, now, true);
        wait("the order", || {
            tick(&mut st, &env, &s0, now, true).installed.then_some(())
        });
        // One more row every 100 ms: never quiet for the 300 ms debounce.
        let mut n = 2;
        while !st.running() {
            now += Duration::from_millis(100);
            assert!(n < offs.len(), "never re-sorted");
            n += 1;
            let s = Arc::new(MatchSet::from_offsets(offs[..n].to_vec()));
            tick(&mut st, &env, &s, now, true);
        }
        // It kept waiting while rows arrived, then gave up waiting.
        assert!(now.duration_since(start) >= RESORT_MAX_WAIT);
        assert!(now.duration_since(start) < RESORT_MAX_WAIT + Duration::from_millis(200));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(48))]

            /// `compute` agrees with a naive stable group sort: records start
            /// groups, other lines join the previous row of the set, keys
            /// that are missing go last in both directions, and leading
            /// non-records form a group of their own at the end.
            #[test]
            fn compute_matches_a_naive_group_sort(
                lines in proptest::collection::vec((0u8..14, any::<bool>()), 0..40),
                desc in any::<bool>(),
            ) {
                // 0..=9 a record n=k, 10 a record with n=x, 11..=13 a stack line.
                let text: String = lines
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| match *k {
                        0..=9 => format!("n={k} id=r{i}\n"),
                        10 => format!("n=x id=r{i}\n"),
                        _ => format!("    at frame{i}\n"),
                    })
                    .collect();
                let starts = line_offsets(&text);
                let kept: Vec<usize> =
                    (0..lines.len()).filter(|&i| lines[i].1).collect();
                let mut i = input(&text, kept.iter().map(|&i| starts[i]).collect(), desc);
                i.generation = i.doc.generation();
                let got = sorted(&i);

                // Naive reference over the kept rows.
                let mut groups: Vec<(Option<u8>, Vec<u64>)> = Vec::new();
                for &row in &kept {
                    match lines[row].0 {
                        k @ 0..=9 => groups.push((Some(k), vec![starts[row]])),
                        10 => groups.push((None, vec![starts[row]])),
                        _ => match groups.last_mut() {
                            Some(g) => g.1.push(starts[row]),
                            None => groups.push((None, vec![starts[row]])),
                        },
                    }
                }
                // A leading orphan group is Missing too; stable sort.
                groups.sort_by(|a, b| match (a.0, b.0) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(x), Some(y)) => if desc { y.cmp(&x) } else { x.cmp(&y) },
                });
                let want: Vec<u64> = groups.into_iter().flat_map(|g| g.1).collect();
                prop_assert_eq!(got.offsets(), &want[..]);
            }
        }
    }
}
