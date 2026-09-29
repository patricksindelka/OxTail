//! Background search and filter jobs.
//!
//! A job splits `[0, end)` into fixed-size chunks and scans them on the
//! `rayon` pool, nearest to the viewport first (the hint chunk, then alternating
//! forward and backward). Results stream into per-chunk slots; the handle
//! answers `next_after` / `prev_before` as soon as the chunks it depends on
//! are done, and hands out cheap [`MatchSet`] snapshots at any time.
//!
//! Cancellation is a flag checked between chunks (a chunk is a few MB of
//! memchr-speed scanning, so it is near-instant) and is triggered by
//! [`SearchHandle::cancel`] and by dropping the handle.
//!
//! `extend` never does I/O on the caller's thread: it queues an internal task
//! that finds the (possibly partial) last line, invalidates the chunks that
//! may have changed (including context lines before the new data) and queues
//! them together with the new chunks.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use oxtail_core::ReadAt;
use parking_lot::Mutex;

use crate::matchset::{MatchSet, Segment};
use crate::predicate::{FilterStack, LinePredicate};
use crate::scan::{back_lines, scan_chunk};

/// Default chunk size: 4 MiB.
pub const DEFAULT_CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// Upper bound on the number of chunks, to bound per-job bookkeeping.
const MAX_CHUNKS: u64 = 4_000_000;

/// Tuning knobs for a job.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Byte offset of the viewport; chunks near it are scanned first.
    pub viewport_hint: u64,
    /// Chunk size in bytes (at least 1; raised if it would create more than
    /// four million chunks).
    pub chunk_size: u64,
    /// Stop after this many entries. Chunks finish in viewport order, so the
    /// kept entries are the first found in scan order, not the first in the
    /// file. The status reports `truncated`.
    pub max_matches: u64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            viewport_hint: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_matches: u64::MAX,
        }
    }
}

/// A snapshot of a job's progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchStatus {
    /// Fraction of chunks finished, in `0.0..=1.0`.
    pub progress: f64,
    /// No more results will arrive (finished, hit `max_matches`, or
    /// cancelled). `extend` starts more work.
    pub done: bool,
    /// Entries found so far (context entries included for filter jobs).
    pub matches_found: u64,
    /// `max_matches` was reached and scanning stopped early.
    pub truncated: bool,
    /// Number of chunks that could not be read (I/O errors).
    pub io_errors: u64,
}

/// The answer to [`SearchHandle::next_after`] / [`SearchHandle::prev_before`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// The requested neighbour.
    Found(u64),
    /// There is none in the scanned range.
    None,
    /// The chunks that decide the answer are not scanned yet; ask again.
    Pending,
}

/// Starts searches.
pub struct SearchJob;

impl SearchJob {
    /// Scans `source[..end]` for lines matching `predicate` in the
    /// background.
    pub fn start(
        source: Arc<dyn ReadAt>,
        end: u64,
        predicate: Arc<dyn LinePredicate>,
        opts: SearchOptions,
    ) -> SearchHandle {
        start_job(source, end, predicate, 0, 0, opts)
    }
}

/// Starts filter-view jobs.
pub struct FilterJob;

impl FilterJob {
    /// Scans `source[..end]` for lines passing `stack`, marking context lines
    /// (`stack.context_before` / `context_after`) in the resulting set.
    pub fn start(
        source: Arc<dyn ReadAt>,
        end: u64,
        stack: FilterStack,
        opts: SearchOptions,
    ) -> SearchHandle {
        let (before, after) = (stack.context_before, stack.context_after);
        start_job(source, end, Arc::new(stack), before, after, opts)
    }
}

/// A running (or finished) search or filter job. Dropping it cancels the job.
pub struct SearchHandle {
    shared: Arc<Shared>,
}

struct Shared {
    source: Arc<dyn ReadAt>,
    pred: Arc<dyn LinePredicate>,
    before: u32,
    after: u32,
    chunk_size: u64,
    max_matches: u64,
    threads: usize,
    cancelled: AtomicBool,
    /// Serialises `Extend` tasks.
    extend_lock: Mutex<()>,
    inner: Mutex<Inner>,
}

struct Inner {
    end: u64,
    epoch: u64,
    slots: Vec<Slot>,
    queue: VecDeque<Task>,
    active: usize,
    pending_extends: usize,
    done_count: usize,
    matches: u64,
    io_errors: u64,
    truncated: bool,
    snapshot: Option<Arc<MatchSet>>,
}

struct Slot {
    epoch: u64,
    done: bool,
    seg: Option<Arc<Segment>>,
}

enum Task {
    Scan { chunk: usize, epoch: u64, end: u64 },
    Extend(u64),
}

fn chunk_count(end: u64, cs: u64) -> usize {
    usize::try_from(end.div_ceil(cs)).unwrap_or(usize::MAX)
}

/// Chunk indices ordered by distance from `hint`, forward first.
fn viewport_order(n: usize, hint: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return out;
    }
    let h = hint.min(n - 1);
    out.push(h);
    let mut d = 1;
    while h + d < n || d <= h {
        if h + d < n {
            out.push(h + d);
        }
        if d <= h {
            out.push(h - d);
        }
        d += 1;
    }
    out
}

fn start_job(
    source: Arc<dyn ReadAt>,
    end: u64,
    pred: Arc<dyn LinePredicate>,
    before: u32,
    after: u32,
    opts: SearchOptions,
) -> SearchHandle {
    let chunk_size = opts.chunk_size.max(1).max(end / MAX_CHUNKS + 1);
    let n = chunk_count(end, chunk_size);
    let hint = usize::try_from(opts.viewport_hint / chunk_size).unwrap_or(usize::MAX);
    let queue = viewport_order(n, hint)
        .into_iter()
        .map(|chunk| Task::Scan {
            chunk,
            epoch: 0,
            end,
        })
        .collect();
    let shared = Arc::new(Shared {
        source,
        pred,
        before,
        after,
        chunk_size,
        max_matches: opts.max_matches,
        threads: rayon::current_num_threads().max(1),
        cancelled: AtomicBool::new(false),
        extend_lock: Mutex::new(()),
        inner: Mutex::new(Inner {
            end,
            epoch: 0,
            slots: (0..n)
                .map(|_| Slot {
                    epoch: 0,
                    done: false,
                    seg: None,
                })
                .collect(),
            queue,
            active: 0,
            pending_extends: 0,
            done_count: 0,
            matches: 0,
            io_errors: 0,
            truncated: false,
            snapshot: None,
        }),
    });
    let spawn = {
        let mut g = shared.inner.lock();
        shared.workers_to_spawn(&mut g)
    };
    shared.spawn_workers(spawn);
    SearchHandle { shared }
}

impl Shared {
    /// Decides (under the queue lock) how many workers to start.
    fn workers_to_spawn(&self, g: &mut Inner) -> usize {
        let want = g.queue.len().min(self.threads);
        let extra = want.saturating_sub(g.active);
        g.active += extra;
        extra
    }

    fn spawn_workers(self: &Arc<Self>, count: usize) {
        for _ in 0..count {
            let me = Arc::clone(self);
            rayon::spawn(move || me.worker());
        }
    }

    fn worker(self: Arc<Self>) {
        loop {
            let task = {
                let mut g = self.inner.lock();
                loop {
                    if self.cancelled.load(Ordering::Relaxed) {
                        g.active -= 1;
                        return;
                    }
                    match g.queue.pop_front() {
                        None => {
                            g.active -= 1;
                            return;
                        }
                        Some(Task::Scan { chunk, epoch, end }) => {
                            if g.slots.get(chunk).is_some_and(|s| s.epoch == epoch) {
                                break Task::Scan { chunk, epoch, end };
                            }
                            // Stale (superseded by an extend): skip.
                        }
                        Some(t @ Task::Extend(_)) => break t,
                    }
                }
            };
            match task {
                Task::Scan { chunk, epoch, end } => self.run_scan(chunk, epoch, end),
                Task::Extend(new_end) => self.run_extend(new_end),
            }
        }
    }

    fn run_scan(self: &Arc<Self>, chunk: usize, epoch: u64, end: u64) {
        let a = chunk as u64 * self.chunk_size;
        let b = (a + self.chunk_size).min(end);
        let result = scan_chunk(
            self.source.as_ref(),
            a,
            b,
            end,
            self.pred.as_ref(),
            self.before,
            self.after,
        );
        let mut g = self.inner.lock();
        let Some(slot) = g.slots.get_mut(chunk) else {
            return;
        };
        if slot.epoch != epoch || slot.done {
            return;
        }
        let seg = match result {
            Ok(seg) => seg,
            Err(e) => {
                tracing::warn!(chunk, error = %e, "search chunk could not be read");
                g.io_errors += 1;
                Segment::default()
            }
        };
        let count = seg.offsets.len() as u64;
        let slot = &mut g.slots[chunk];
        slot.done = true;
        slot.seg = (count > 0).then(|| Arc::new(seg));
        g.done_count += 1;
        g.matches += count;
        g.snapshot = None;
        if g.matches >= self.max_matches && !g.truncated {
            g.truncated = true;
            g.queue.clear();
        }
    }

    fn run_extend(self: &Arc<Self>, new_end: u64) {
        let _serial = self.extend_lock.lock();
        let old_end = self.inner.lock().end;
        if new_end <= old_end {
            self.inner.lock().pending_extends -= 1;
            return;
        }
        // Find where re-scanning must begin: the start of the last line if it
        // was unterminated (it may have grown), then back over the context
        // lines that new matches could now claim.
        let resume = self.tail_line_start(old_end);
        let dirty_from = match resume.and_then(|r| back_lines(self.source.as_ref(), r, self.before))
        {
            Ok(off) => off,
            Err(e) => {
                tracing::warn!(error = %e, "extend could not probe the tail; rescanning last chunk");
                old_end.saturating_sub(1)
            }
        };
        let mut g = self.inner.lock();
        g.pending_extends -= 1;
        g.end = new_end;
        g.epoch += 1;
        let epoch = g.epoch;
        let old_n = g.slots.len();
        let new_n = chunk_count(new_end, self.chunk_size);
        let first_dirty = usize::try_from(dirty_from / self.chunk_size)
            .unwrap_or(usize::MAX)
            .min(old_n);
        for i in first_dirty..old_n {
            let slot = &mut g.slots[i];
            let was_done = slot.done;
            let count = slot.seg.as_ref().map_or(0, |s| s.offsets.len() as u64);
            slot.epoch = epoch;
            slot.done = false;
            slot.seg = None;
            if was_done {
                g.done_count -= 1;
            }
            g.matches -= count;
        }
        for _ in old_n..new_n {
            g.slots.push(Slot {
                epoch,
                done: false,
                seg: None,
            });
        }
        // Follow updates are urgent: run them before the remaining backlog.
        for chunk in (first_dirty..new_n).rev() {
            g.queue.push_front(Task::Scan {
                chunk,
                epoch,
                end: new_end,
            });
        }
        if g.truncated && g.matches < self.max_matches {
            g.truncated = false;
        }
        g.snapshot = None;
        let spawn = self.workers_to_spawn(&mut g);
        drop(g);
        self.spawn_workers(spawn);
    }

    /// Start of the last line before `old_end` if it lacks a terminator,
    /// else `old_end` itself.
    fn tail_line_start(&self, old_end: u64) -> std::io::Result<u64> {
        if old_end == 0 {
            return Ok(0);
        }
        let mut b = [0u8; 1];
        self.source.read_exact_at(old_end - 1, &mut b)?;
        if b[0] == b'\n' {
            return Ok(old_end);
        }
        // Walk back to the previous newline.
        let mut hi = old_end;
        let mut buf = [0u8; 8192];
        while hi > 0 {
            let lo = hi.saturating_sub(buf.len() as u64);
            let n = usize::try_from(hi - lo).unwrap_or(buf.len());
            self.source.read_exact_at(lo, &mut buf[..n])?;
            if let Some(i) = memchr::memrchr(b'\n', &buf[..n]) {
                return Ok(lo + i as u64 + 1);
            }
            hi = lo;
        }
        Ok(0)
    }
}

impl SearchHandle {
    /// Current progress; cheap, never blocks on scanning.
    pub fn poll(&self) -> SearchStatus {
        let g = self.shared.inner.lock();
        let total = g.slots.len();
        let cancelled = self.shared.cancelled.load(Ordering::Relaxed);
        let finished = g.done_count == total && g.pending_extends == 0;
        SearchStatus {
            progress: if total == 0 {
                1.0
            } else {
                g.done_count as f64 / total as f64
            },
            done: finished || g.truncated || cancelled,
            matches_found: g.matches,
            truncated: g.truncated,
            io_errors: g.io_errors,
        }
    }

    /// A snapshot of everything found so far. Cheap: shares the per-chunk
    /// segments and is cached until new results arrive.
    pub fn matches(&self) -> Arc<MatchSet> {
        let mut g = self.shared.inner.lock();
        if let Some(s) = &g.snapshot {
            return Arc::clone(s);
        }
        let set = Arc::new(MatchSet::from_segments(
            g.slots.iter().filter_map(|s| s.seg.clone()).collect(),
        ));
        g.snapshot = Some(Arc::clone(&set));
        set
    }

    /// The first entry with offset strictly greater than `offset`, answered
    /// as soon as the chunks between `offset` and that entry are scanned.
    pub fn next_after(&self, offset: u64) -> Lookup {
        let g = self.shared.inner.lock();
        let n = g.slots.len();
        if n == 0 || offset.saturating_add(1) >= g.end {
            return if g.pending_extends > 0 {
                Lookup::Pending
            } else {
                Lookup::None
            };
        }
        let start = usize::try_from(offset / self.shared.chunk_size)
            .unwrap_or(n)
            .min(n);
        for slot in &g.slots[start..] {
            if !slot.done {
                return Lookup::Pending;
            }
            if let Some(seg) = &slot.seg {
                let i = seg.offsets.partition_point(|&o| o <= offset);
                if let Some(&o) = seg.offsets.get(i) {
                    return Lookup::Found(o);
                }
            }
        }
        Lookup::None
    }

    /// The last entry with offset strictly smaller than `offset`, answered
    /// as soon as the chunks between that entry and `offset` are scanned.
    pub fn prev_before(&self, offset: u64) -> Lookup {
        let g = self.shared.inner.lock();
        let n = g.slots.len();
        if n == 0 || offset == 0 {
            return Lookup::None;
        }
        let start = usize::try_from(offset / self.shared.chunk_size)
            .unwrap_or(n - 1)
            .min(n - 1);
        for slot in g.slots[..=start].iter().rev() {
            if !slot.done {
                return Lookup::Pending;
            }
            if let Some(seg) = &slot.seg {
                let i = seg.offsets.partition_point(|&o| o < offset);
                if let Some(&o) = i.checked_sub(1).and_then(|i| seg.offsets.get(i)) {
                    return Lookup::Found(o);
                }
            }
        }
        Lookup::None
    }

    /// Scans data appended since the job started (or last extended), up to
    /// `new_end`. Does no I/O on the calling thread. A partial last line is
    /// re-evaluated when it grows. Shrinking is not supported: start a new
    /// job after truncation or rotation.
    pub fn extend(&self, new_end: u64) {
        if self.shared.cancelled.load(Ordering::Relaxed) {
            return;
        }
        let spawn = {
            let mut g = self.shared.inner.lock();
            if new_end <= g.end && g.pending_extends == 0 {
                return;
            }
            g.pending_extends += 1;
            g.queue.push_front(Task::Extend(new_end));
            self.shared.workers_to_spawn(&mut g)
        };
        self.shared.spawn_workers(spawn);
    }

    /// Moves the viewport hint: remaining chunks are re-ordered so the ones
    /// nearest `offset` run first.
    pub fn set_viewport_hint(&self, offset: u64) {
        let hint = usize::try_from(offset / self.shared.chunk_size).unwrap_or(usize::MAX);
        let mut g = self.shared.inner.lock();
        let mut tasks: Vec<Task> = g.queue.drain(..).collect();
        let key = |t: &Task| match t {
            Task::Extend(_) => (0usize, 0u8),
            Task::Scan { chunk, .. } => (chunk.abs_diff(hint) + 1, u8::from(*chunk < hint)),
        };
        tasks.sort_by_key(key);
        g.queue = tasks.into();
    }

    /// Stops the job. Idempotent; also happens on drop.
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Relaxed);
        self.shared.inner.lock().queue.clear();
    }

    /// Whether the job was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Relaxed)
    }

    /// The end offset currently being scanned.
    pub fn end(&self) -> u64 {
        self.shared.inner.lock().end
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Filter, Matcher, Query};
    use oxtail_core::MemSource;
    use std::time::{Duration, Instant};

    fn wait(h: &SearchHandle) -> SearchStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = h.poll();
            if s.done {
                return s;
            }
            assert!(Instant::now() < deadline, "job did not finish");
            std::thread::yield_now();
        }
    }

    fn lit(p: &str) -> Arc<dyn LinePredicate> {
        Arc::new(Matcher::compile(&Query::literal(p)).unwrap())
    }

    #[test]
    fn viewport_order_alternates_outward() {
        assert_eq!(viewport_order(7, 3), vec![3, 4, 2, 5, 1, 6, 0]);
        assert_eq!(viewport_order(4, 0), vec![0, 1, 2, 3]);
        assert_eq!(viewport_order(4, 9), vec![3, 2, 1, 0]);
        assert!(viewport_order(0, 0).is_empty());
    }

    #[test]
    fn finds_matches_across_tiny_chunks() {
        let data = b"one\ntwo error\nthree\nerror four\r\nfive\nerror";
        let src = Arc::new(MemSource::new(data.to_vec()));
        for cs in [1, 2, 3, 5, 7, 16, 1000] {
            let h = SearchJob::start(
                src.clone(),
                data.len() as u64,
                lit("error"),
                SearchOptions {
                    chunk_size: cs,
                    ..Default::default()
                },
            );
            wait(&h);
            assert_eq!(
                h.matches().iter().collect::<Vec<_>>(),
                vec![4, 20, 37],
                "chunk size {cs}"
            );
        }
    }

    #[test]
    fn next_and_prev_answer_from_partial_results() {
        let mut data = Vec::new();
        for i in 0..200 {
            data.extend_from_slice(
                format!("line {i} {}\n", if i % 50 == 0 { "hit" } else { "-" }).as_bytes(),
            );
        }
        let src = Arc::new(MemSource::new(data.clone()));
        let h = SearchJob::start(
            src,
            data.len() as u64,
            lit("hit"),
            SearchOptions {
                chunk_size: 64,
                viewport_hint: 1000,
                ..Default::default()
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let first = loop {
            match h.next_after(0) {
                Lookup::Pending => {
                    assert!(Instant::now() < deadline);
                    std::thread::yield_now();
                }
                other => break other,
            }
        };
        assert!(matches!(first, Lookup::Found(_)));
        wait(&h);
        assert_eq!(first, h.next_after(0));
        let all = h.matches();
        assert_eq!(all.len(), 4);
        assert_eq!(h.next_after(0), Lookup::Found(all.get(1).unwrap()));
        assert_eq!(h.next_after(all.get(3).unwrap()), Lookup::None);
        assert_eq!(
            h.prev_before(all.get(3).unwrap()),
            Lookup::Found(all.get(2).unwrap())
        );
        assert_eq!(h.prev_before(all.get(0).unwrap()), Lookup::None);
    }

    #[test]
    fn max_matches_truncates() {
        let data: Vec<u8> = (0..100).flat_map(|_| b"x\n".to_vec()).collect();
        let h = SearchJob::start(
            Arc::new(MemSource::new(data.clone())),
            data.len() as u64,
            lit("x"),
            SearchOptions {
                chunk_size: 10,
                max_matches: 5,
                ..Default::default()
            },
        );
        let s = wait(&h);
        assert!(s.truncated);
        assert!(s.matches_found >= 5);
    }

    #[test]
    fn empty_source_is_done_immediately() {
        let h = SearchJob::start(
            Arc::new(MemSource::new(Vec::new())),
            0,
            lit("x"),
            SearchOptions::default(),
        );
        let s = h.poll();
        assert!(s.done && s.progress == 1.0 && s.matches_found == 0);
        assert_eq!(h.next_after(0), Lookup::None);
        assert_eq!(h.prev_before(10), Lookup::None);
    }

    #[test]
    fn extend_picks_up_appended_and_grown_lines() {
        let src = Arc::new(MemSource::new(b"a hit\nb\npartial h".to_vec()));
        let end = src.len().unwrap();
        let h = SearchJob::start(
            src.clone(),
            end,
            lit("hit"),
            SearchOptions {
                chunk_size: 4,
                ..Default::default()
            },
        );
        wait(&h);
        assert_eq!(h.matches().iter().collect::<Vec<_>>(), vec![0]);
        src.append(b"it\nnew hit line\nlast");
        h.extend(src.len().unwrap());
        wait(&h);
        assert_eq!(h.matches().iter().collect::<Vec<_>>(), vec![0, 8, 20]);
        src.append(b" hit");
        h.extend(src.len().unwrap());
        wait(&h);
        assert_eq!(h.matches().iter().collect::<Vec<_>>(), vec![0, 8, 20, 33]);
    }

    #[test]
    fn extend_updates_context_before_new_match() {
        let src = Arc::new(MemSource::new(b"a\nb\nc\n".to_vec()));
        let stack = FilterStack::new()
            .with(Filter::include(lit("hit")))
            .with_context(2, 0);
        let h = FilterJob::start(
            src.clone(),
            6,
            stack,
            SearchOptions {
                chunk_size: 2,
                ..Default::default()
            },
        );
        wait(&h);
        assert!(h.matches().is_empty());
        src.append(b"hit\n");
        h.extend(10);
        wait(&h);
        assert_eq!(
            h.matches().iter_entries().collect::<Vec<_>>(),
            vec![(2, true), (4, true), (6, false)]
        );
    }

    #[test]
    fn cancel_stops_work_promptly() {
        struct Slow(std::sync::atomic::AtomicU64);
        impl LinePredicate for Slow {
            fn matches(&self, _: &[u8]) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
                false
            }
        }
        let data: Vec<u8> = (0..20_000).flat_map(|_| b"line\n".to_vec()).collect();
        let pred = Arc::new(Slow(Default::default()));
        let h = SearchJob::start(
            Arc::new(MemSource::new(data.clone())),
            data.len() as u64,
            pred.clone(),
            SearchOptions {
                chunk_size: 50,
                ..Default::default()
            },
        );
        h.cancel();
        assert!(h.poll().done && h.is_cancelled());
        drop(h);
        // Workers release their reference to the predicate when they exit.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&pred) > 1 {
            assert!(Instant::now() < deadline, "workers did not stop");
            std::thread::yield_now();
        }
        assert!(
            pred.0.load(Ordering::Relaxed) < 20_000,
            "job ran to completion despite cancel"
        );
    }

    #[test]
    fn dropping_the_handle_cancels() {
        let data: Vec<u8> = (0..1000).flat_map(|_| b"line\n".to_vec()).collect();
        let pred = lit("nomatch");
        let h = SearchJob::start(
            Arc::new(MemSource::new(data.clone())),
            data.len() as u64,
            pred.clone(),
            SearchOptions {
                chunk_size: 10,
                ..Default::default()
            },
        );
        drop(h);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&pred) > 1 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
}
