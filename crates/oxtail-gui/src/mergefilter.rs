//! The filter view of a merged tab: the merged indices whose lines pass a
//! [`FilterStack`], computed on a worker thread.
//!
//! [`MergedFilter`] walks the merged order in blocks. For each block it reads
//! the source lines in per-source runs ([`plan_reads`], the same approach as
//! `MergedSearch`), evaluates the stack and appends the passing merged indices
//! (one `u32` each, ascending; no line text is kept) to a shared list. When it
//! reaches the end of the merged order it keeps polling, so lines merged in
//! later (live growth) are filtered as they arrive.
//!
//! Cancellation: the job owns a generation token. Dropping the
//! [`MergedFilter`] bumps it and the worker stops at its next check. A rebuild
//! of the merge (a new store epoch: rotation, truncation, re-decoding) makes
//! the worker end by itself; the owner sees `epoch != store.epoch()` and starts
//! a new job.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use oxtail_core::Document;
use oxtail_search::{FilterStack, LinePredicate};

use crate::merge::{MergedStore, read_entries_text};

/// Merged lines evaluated per block.
const BLOCK: usize = 2048;
/// How long the worker sleeps when it has caught up with the merge.
const IDLE_POLL: Duration = Duration::from_millis(40);

/// A running (or finished and idle) filter over a merged store. Dropping it
/// stops the worker.
pub struct MergedFilter {
    token: Arc<AtomicU64>,
    rows: Arc<RwLock<Vec<u32>>>,
    scanned: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    truncated: Arc<AtomicBool>,
    /// The store epoch the job was started in.
    pub epoch: u64,
}

impl Drop for MergedFilter {
    fn drop(&mut self) {
        // A new generation: the worker's copy of the token is stale.
        self.token.fetch_add(1, Ordering::AcqRel);
    }
}

struct Shared {
    docs: Vec<Arc<Document>>,
    store: Arc<MergedStore>,
    stack: FilterStack,
    epoch: u64,
    /// Merged indices at or above this are not filtered.
    limit: usize,
    token: Arc<AtomicU64>,
    generation: u64,
    rows: Arc<RwLock<Vec<u32>>>,
    scanned: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    truncated: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn stale(&self) -> bool {
        self.token.load(Ordering::Acquire) != self.generation || self.store.epoch() != self.epoch
    }
}

impl MergedFilter {
    /// Starts filtering `store` (whose lines live in `docs`) with `stack`.
    /// The context sizes of the stack are ignored. `wake` requests a repaint
    /// when rows were added.
    pub fn start(
        docs: Vec<Arc<Document>>,
        store: Arc<MergedStore>,
        stack: FilterStack,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> MergedFilter {
        Self::start_limited(docs, store, stack, wake, u32::MAX as usize)
    }

    /// Like [`MergedFilter::start`], but lines from merged index `limit` on
    /// are not filtered (the job ends `truncated`). The limit is `u32::MAX`
    /// for real jobs; tests use a small one.
    pub(crate) fn start_limited(
        docs: Vec<Arc<Document>>,
        store: Arc<MergedStore>,
        stack: FilterStack,
        wake: Arc<dyn Fn() + Send + Sync>,
        limit: usize,
    ) -> MergedFilter {
        let token = Arc::new(AtomicU64::new(0));
        let rows = Arc::new(RwLock::new(Vec::new()));
        let scanned = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let truncated = Arc::new(AtomicBool::new(false));
        let epoch = store.epoch();
        let shared = Shared {
            docs,
            store,
            stack,
            epoch,
            limit,
            token: Arc::clone(&token),
            generation: 0,
            rows: Arc::clone(&rows),
            scanned: Arc::clone(&scanned),
            done: Arc::clone(&done),
            truncated: Arc::clone(&truncated),
            wake,
        };
        let spawned = std::thread::Builder::new()
            .name("oxtail-merge-filter".into())
            .spawn(move || filter_loop(&shared));
        if let Err(e) = spawned {
            tracing::warn!("cannot start the merge filter thread: {e}");
        }
        MergedFilter {
            token,
            rows,
            scanned,
            done,
            truncated,
            epoch,
        }
    }

    /// Number of passing lines found so far.
    pub fn count(&self) -> usize {
        self.rows.read().map_or(0, |r| r.len())
    }

    /// Merged lines evaluated so far.
    pub fn scanned(&self) -> usize {
        usize::try_from(self.scanned.load(Ordering::Relaxed)).unwrap_or(usize::MAX)
    }

    /// Every merged line so far has been evaluated and the merge has caught
    /// up with its sources (more may follow when a source grows).
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// The merge grew past what a `u32` can index: later lines are not
    /// filtered.
    pub fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Relaxed)
    }

    /// The merged index of filtered row `row`.
    pub fn get(&self, row: usize) -> Option<usize> {
        self.rows.read().ok()?.get(row).map(|&i| i as usize)
    }

    /// The merged indices of filtered rows `range` (clamped).
    pub fn slice(&self, range: std::ops::Range<usize>) -> Vec<usize> {
        let Ok(r) = self.rows.read() else {
            return Vec::new();
        };
        let end = range.end.min(r.len());
        let start = range.start.min(end);
        r[start..end].iter().map(|&i| i as usize).collect()
    }

    /// The first filtered row whose merged index is `>= index` (the row
    /// count if there is none).
    pub fn lower_bound(&self, index: usize) -> usize {
        self.rows
            .read()
            .map_or(0, |r| r.partition_point(|&i| (i as usize) < index))
    }

    /// The filtered row of merged index `index`, if that line passes.
    pub fn row_of(&self, index: usize) -> Option<usize> {
        let r = self.rows.read().ok()?;
        let i = u32::try_from(index).ok()?;
        r.binary_search(&i).ok()
    }
}

fn filter_loop(sh: &Shared) {
    let mut pos = 0usize;
    while !sh.stale() {
        // Read the flag before the length: everything merged before the flag
        // was set is then in `len`.
        let caught_up = sh.store.caught_up();
        let len = sh.store.len();
        if pos >= len {
            sh.done.store(caught_up, Ordering::Release);
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        sh.done.store(false, Ordering::Release);
        let end = (pos + BLOCK).min(len);
        let entries = sh.store.slice(pos..end);
        let texts = read_entries_text(&sh.docs, &entries, &|| sh.stale());
        if sh.stale() {
            return;
        }
        let mut found: Vec<u32> = Vec::new();
        let mut overflow = false;
        for (k, t) in texts.iter().enumerate() {
            let Some(t) = t else { continue };
            if sh.stack.matches(t.as_bytes()) {
                match u32::try_from(pos + k) {
                    Ok(i) if pos + k < sh.limit => found.push(i),
                    _ => {
                        overflow = true;
                        break;
                    }
                }
            }
        }
        if !found.is_empty()
            && let Ok(mut r) = sh.rows.write()
        {
            r.extend(found);
        }
        if overflow {
            // Terminal: later lines are not filtered, and the job says so.
            sh.truncated.store(true, Ordering::Relaxed);
            sh.done.store(true, Ordering::Release);
            (sh.wake)();
            return;
        }
        pos = end;
        sh.scanned.store(pos as u64, Ordering::Relaxed);
        (sh.wake)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};
    use crate::merge::{MergeOptions, MergeWorker};
    use oxtail_core::MemSource;
    use oxtail_search::{CaseMode, Filter, Matcher, Query, QueryKind};

    fn ts_line(sec: u32, text: &str) -> String {
        format!("2026-09-29 10:00:{sec:02} {text}\n")
    }

    fn pred(text: &str) -> Arc<dyn LinePredicate> {
        Arc::new(
            Matcher::compile(&Query {
                pattern: text.into(),
                kind: QueryKind::Literal,
                case: CaseMode::Smart,
                whole_word: false,
            })
            .unwrap(),
        )
    }

    fn opts() -> MergeOptions {
        MergeOptions {
            idle_delay: Duration::from_millis(30),
            ..MergeOptions::default()
        }
    }

    /// a0 b0 a1 b1 ... with "hot" on every third line of each source.
    fn sources() -> (Arc<Document>, Arc<Document>) {
        let mut a = String::new();
        let mut b = String::new();
        for i in 0..12u32 {
            let tag = if i % 3 == 0 { "hot" } else { "cold" };
            a.push_str(&ts_line(i * 2, &format!("{tag} a{i}")));
            b.push_str(&ts_line(i * 2 + 1, &format!("{tag} b{i}")));
        }
        (doc(&a), doc(&b))
    }

    fn wait_done(f: &MergedFilter, scanned: usize) {
        wait("filter done", || {
            (f.scanned() >= scanned && f.done()).then_some(())
        });
    }

    #[test]
    fn include_and_exclude_give_merged_indices_in_order() {
        let (a, b) = sources();
        let docs = vec![a, b];
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(docs.clone(), Arc::clone(&store), opts(), Arc::new(|| {}));
        wait("merged", || (store.len() == 24).then_some(()));
        // Include "hot": a0 b0 a3 b3 a6 b6 a9 b9 = merged 0 1 6 7 12 13 18 19.
        let stack = FilterStack::new().with(Filter::include(pred("hot")));
        let f = MergedFilter::start(docs.clone(), Arc::clone(&store), stack, Arc::new(|| {}));
        wait_done(&f, 24);
        assert_eq!(f.slice(0..100), vec![0, 1, 6, 7, 12, 13, 18, 19]);
        assert_eq!(f.count(), 8);
        assert_eq!(f.get(2), Some(6));
        assert_eq!(f.row_of(12), Some(4));
        assert_eq!(f.row_of(2), None);
        assert_eq!(f.lower_bound(2), 2);
        assert_eq!(f.lower_bound(100), 8);
        // Include "hot", exclude "b": only source a.
        let stack = FilterStack::new()
            .with(Filter::include(pred("hot")))
            .with(Filter::exclude(pred(" b")));
        let g = MergedFilter::start(docs, Arc::clone(&store), stack, Arc::new(|| {}));
        wait_done(&g, 24);
        assert_eq!(g.slice(0..100), vec![0, 6, 12, 18]);
    }

    #[test]
    fn growth_of_a_source_adds_passing_lines() {
        let mem_a = Arc::new(MemSource::new(ts_line(0, "hot a0").into_bytes()));
        let mem_b = Arc::new(MemSource::new(ts_line(1, "cold b0").into_bytes()));
        let a = Arc::new(Document::from_source(mem_a.clone(), "a.log"));
        let b = Arc::new(Document::from_source(mem_b.clone(), "b.log"));
        let docs = vec![a, b];
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(docs.clone(), Arc::clone(&store), opts(), Arc::new(|| {}));
        wait("merged", || (store.len() == 2).then_some(()));
        let stack = FilterStack::new().with(Filter::include(pred("hot")));
        let f = MergedFilter::start(docs, Arc::clone(&store), stack, Arc::new(|| {}));
        wait_done(&f, 2);
        assert_eq!(f.slice(0..10), vec![0]);
        mem_b.append(ts_line(3, "hot b1").as_bytes());
        wait("merged growth", || (store.len() == 3).then_some(()));
        wait("new passing line", || (f.count() == 2).then_some(()));
        assert_eq!(f.slice(0..10), vec![0, 2]);
        mem_a.append(ts_line(4, "cold a1").as_bytes());
        wait("scanned", || (f.scanned() == 4).then_some(()));
        assert_eq!(f.count(), 2);
    }

    #[test]
    fn a_rebuild_ends_the_job_and_dropping_stops_it() {
        let mem_a = Arc::new(MemSource::new(
            [ts_line(0, "hot a0"), ts_line(2, "hot a1")]
                .concat()
                .into_bytes(),
        ));
        let a = Arc::new(Document::from_source(mem_a, "a.log"));
        let b = doc(&ts_line(1, "hot b0"));
        let docs = vec![Arc::clone(&a), b];
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(docs.clone(), Arc::clone(&store), opts(), Arc::new(|| {}));
        wait("merged", || (store.len() == 3).then_some(()));
        let stack = FilterStack::new().with(Filter::include(pred("hot")));
        let f = MergedFilter::start(docs, Arc::clone(&store), stack, Arc::new(|| {}));
        wait_done(&f, 3);
        let before = Arc::strong_count(&store);
        // Re-decoding bumps the generation: the merge starts over and the
        // worker of the old epoch ends (it drops its store handle).
        let epoch = store.epoch();
        a.set_encoding(oxtail_core::EncodingChoice::Auto);
        wait("rebuild", || (store.epoch() > epoch).then_some(()));
        assert_ne!(f.epoch, store.epoch());
        wait("worker ended", || {
            (Arc::strong_count(&store) < before).then_some(())
        });
        // Dropping a job of the current epoch stops its worker too.
        let g = MergedFilter::start(
            vec![],
            Arc::clone(&store),
            FilterStack::new(),
            Arc::new(|| {}),
        );
        let with_g = Arc::strong_count(&store);
        drop(g);
        wait("dropped job ended", || {
            (Arc::strong_count(&store) < with_g).then_some(())
        });
    }

    #[test]
    fn a_job_past_its_limit_ends_truncated_and_done() {
        let (a, b) = sources();
        let docs = vec![a, b];
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(docs.clone(), Arc::clone(&store), opts(), Arc::new(|| {}));
        wait("merged", || (store.len() == 24).then_some(()));
        let stack = FilterStack::new().with(Filter::include(pred("hot")));
        let f = MergedFilter::start_limited(docs, Arc::clone(&store), stack, Arc::new(|| {}), 10);
        wait("terminal", || f.done().then_some(()));
        assert!(f.truncated());
        assert_eq!(f.slice(0..100), vec![0, 1, 6, 7]);
    }

    /// Lines of ~8 KB: a 2048-line block is far over the reply cap of one
    /// read, which comes back as a prefix.
    fn long_doc(prefix: &str, first_sec: u32, lines: u32) -> Arc<Document> {
        let pad = "x".repeat(8000);
        let mut t = String::new();
        for i in 0..lines {
            let tag = if i % 500 == 499 { "needle" } else { "hay" };
            let s = first_sec + i * 2;
            t.push_str(&format!(
                "2026-09-29 {:02}:{:02}:{:02} {tag} {prefix}{i} {pad}\n",
                s / 3600,
                (s / 60) % 60,
                s % 60
            ));
        }
        doc(&t)
    }

    #[test]
    fn long_lines_beyond_the_reply_cap_are_all_filtered_and_found() {
        let a = long_doc("a", 0, 2600);
        let b = long_doc("b", 1, 2600);
        wait("indexed", || {
            (a.snapshot().lines.known >= 2600).then_some(())
        });
        // The premise: one read of a block comes back as a prefix.
        assert!(a.read_lines_blocking(0, 2048).len() < 2048);
        let docs = vec![a, b];
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(docs.clone(), Arc::clone(&store), opts(), Arc::new(|| {}));
        wait("merged", || (store.len() == 5200).then_some(()));
        // 5 needles per source (lines 499, 999, ..., 2499), in merged order.
        let mut expected = Vec::new();
        for k in 0..5usize {
            let line = 499 + 500 * k;
            expected.push(2 * line);
            expected.push(2 * line + 1);
        }
        let stack = FilterStack::new().with(Filter::include(pred("needle")));
        let f = MergedFilter::start(docs.clone(), Arc::clone(&store), stack, Arc::new(|| {}));
        wait("filter done", || {
            (f.scanned() >= 5200 && f.done()).then_some(())
        });
        assert_eq!(f.slice(0..100), expected);
        // The merged Find reads the same way.
        let q = Query {
            pattern: "needle".into(),
            kind: QueryKind::Literal,
            case: CaseMode::Smart,
            whole_word: false,
        };
        let s = crate::merge::MergedSearch::start(docs, Arc::clone(&store), &q, Arc::new(|| {}))
            .unwrap();
        wait("search done", || {
            (s.scanned() >= 5200 && s.count() == 10).then_some(())
        });
        assert_eq!(s.next_from(0), Some(expected[0]));
        assert!(s.contains(expected[9]));
    }
}
