//! The merged view's engine: several documents interleaved by timestamp.
//!
//! * [`MergeWorker`] reads every source in chunks on a worker thread, learns
//!   each source's timestamp format, feeds `oxtail_time::MergeBuilder` and
//!   appends the final entries to a shared [`MergedStore`] (8 bytes per merged
//!   line). Sources are marked live once they have caught up with their file
//!   and are then followed: lines that arrive later are pushed and merged in.
//!   A truncation or rotation of any source (a new generation) rebuilds the
//!   whole merge.
//! * [`plan_reads`] turns a window of entries into per-source line ranges, and
//!   [`Fetcher`] reads them on its own thread, so the UI never touches a file.
//! * [`MergedSearch`] finds text across the merged order on a worker.
//!
//! Nothing here touches egui; `mergeview` and `mergepaint` do.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use oxtail_core::{DocState, Document, Line};
use oxtail_search::{LinePredicate, Matcher, Query as SearchQuery};
use oxtail_time::jiff::Timestamp;
use oxtail_time::{MAX_SOURCES, MergeBuilder, MergeConfig, MergedEntry, TimeContext, TimeParser};

/// Lines read per request while building.
const BUILD_CHUNK: u64 = 1000;
/// Lines of a source used to learn its timestamp format.
const LEARN_LINES: usize = 200;
/// How long the worker sleeps when there is nothing to do.
const IDLE_SLEEP: Duration = Duration::from_millis(40);

/// One source of a merged view.
#[derive(Clone)]
pub struct MergeSource {
    /// Name shown in badges and the status bar.
    pub name: String,
    /// The document.
    pub doc: Arc<Document>,
    /// The file, if there is one (for saving the merged tab).
    pub path: Option<PathBuf>,
    /// The merged view opened the document itself and must drain its
    /// events (a document that is also shown in a tab is drained by that
    /// tab's view).
    pub owns: bool,
}

/// The merged order, shared between the builder (writer) and the UI and the
/// search (readers).
#[derive(Default)]
pub struct MergedStore {
    entries: RwLock<Vec<MergedEntry>>,
    epoch: AtomicU64,
    caught_up: AtomicBool,
    pushed: AtomicU64,
}

impl MergedStore {
    /// Number of merged lines.
    pub fn len(&self) -> usize {
        self.entries.read().map_or(0, |e| e.len())
    }

    /// Whether nothing has been merged yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Increases whenever the merge is rebuilt from scratch (entries were
    /// discarded); cached rows must be dropped then.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Every source has been read to its end at least once.
    pub fn caught_up(&self) -> bool {
        self.caught_up.load(Ordering::Acquire)
    }

    /// Lines read from the sources so far.
    pub fn lines_read(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// Bytes the merged order occupies.
    pub fn bytes(&self) -> usize {
        self.len() * std::mem::size_of::<MergedEntry>()
    }

    /// The entries in `range` (clamped to what exists).
    pub fn slice(&self, range: std::ops::Range<usize>) -> Vec<MergedEntry> {
        let Ok(e) = self.entries.read() else {
            return Vec::new();
        };
        let end = range.end.min(e.len());
        let start = range.start.min(end);
        e[start..end].to_vec()
    }

    /// The entry at `index`.
    pub fn get(&self, index: usize) -> Option<MergedEntry> {
        self.entries.read().ok()?.get(index).copied()
    }

    fn append(&self, mut more: Vec<MergedEntry>) {
        if let Ok(mut e) = self.entries.write() {
            e.append(&mut more);
        }
    }

    fn reset(&self) {
        if let Ok(mut e) = self.entries.write() {
            e.clear();
            e.shrink_to_fit();
        }
        self.caught_up.store(false, Ordering::Release);
        self.pushed.store(0, Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

/// Options of [`MergeWorker`].
#[derive(Clone)]
pub struct MergeOptions {
    /// Zone for zone-less timestamps.
    pub time_zone: oxtail_time::jiff::tz::TimeZone,
    /// How long a live source may stay silent before it stops holding back
    /// the others.
    pub idle_delay: Duration,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            time_zone: oxtail_time::jiff::tz::TimeZone::UTC,
            idle_delay: MergeConfig::default().idle_delay,
        }
    }
}

/// The builder thread. Dropping it stops the thread.
pub struct MergeWorker {
    cancel: Arc<AtomicBool>,
}

impl Drop for MergeWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl MergeWorker {
    /// Starts merging `sources` into `store`. `wake` requests a repaint when
    /// entries were added.
    pub fn start(
        sources: Vec<Arc<Document>>,
        store: Arc<MergedStore>,
        opts: MergeOptions,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> MergeWorker {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("oxtail-merge".into())
            .spawn(move || build_loop(&sources, &store, &opts, &flag, &*wake));
        if let Err(e) = spawned {
            tracing::warn!("cannot start the merge thread: {e}");
        }
        MergeWorker { cancel }
    }
}

/// The state of one source while building.
struct Feed {
    next: u64,
    generation: u64,
    parser: Option<TimeParser>,
    live: bool,
}

fn build_loop(
    docs: &[Arc<Document>],
    store: &MergedStore,
    opts: &MergeOptions,
    cancel: &AtomicBool,
    wake: &(dyn Fn() + Send + Sync),
) {
    let n = docs.len().min(MAX_SOURCES);
    let fresh = || -> (Vec<Feed>, MergeBuilder) {
        let now = Instant::now();
        (
            docs.iter()
                .take(n)
                .map(|d| Feed {
                    next: 0,
                    generation: d.generation(),
                    parser: None,
                    live: false,
                })
                .collect(),
            MergeBuilder::new(
                n,
                MergeConfig {
                    idle_delay: opts.idle_delay,
                },
                now,
            ),
        )
    };
    let (mut feeds, mut builder) = fresh();
    while !cancel.load(Ordering::Relaxed) {
        let mut progressed = false;
        let mut rebuild = false;
        for (i, doc) in docs.iter().take(n).enumerate() {
            let snap = doc.snapshot();
            if snap.generation != feeds[i].generation {
                rebuild = true;
                break;
            }
            if matches!(snap.state, DocState::Opening) {
                continue;
            }
            let known = snap.lines.known;
            if feeds[i].next < known {
                let want = usize::try_from((known - feeds[i].next).min(BUILD_CHUNK))
                    .unwrap_or(BUILD_CHUNK as usize);
                let (g, lines) = doc.read_lines_blocking_with_generation(feeds[i].next, want);
                if g != feeds[i].generation || doc.generation() != feeds[i].generation {
                    rebuild = true;
                    break;
                }
                if lines.is_empty() {
                    continue;
                }
                if feeds[i].parser.is_none() {
                    let mut p = TimeParser::new(TimeContext::new(opts.time_zone.clone()));
                    p.learn(lines.iter().take(LEARN_LINES).map(|l| l.text.as_str()));
                    feeds[i].parser = Some(p);
                }
                let now = Instant::now();
                for l in &lines {
                    let ts: Option<Timestamp> =
                        feeds[i].parser.as_ref().and_then(|p| p.parse(&l.text));
                    builder.push(i, l.number, ts, now);
                    feeds[i].next = l.number + 1;
                }
                store
                    .pushed
                    .fetch_add(lines.len() as u64, Ordering::Relaxed);
                progressed = true;
            } else if snap.lines.exact && matches!(snap.state, DocState::Ready) && !feeds[i].live {
                builder.mark_live(i, Instant::now());
                feeds[i].live = true;
            }
        }
        if rebuild {
            store.reset();
            (feeds, builder) = fresh();
            continue;
        }
        let out = builder.drain(Instant::now());
        if !out.is_empty() {
            store.append(out);
            wake();
        }
        if !store.caught_up() && feeds.iter().all(|f| f.live) {
            store.caught_up.store(true, Ordering::Release);
            wake();
        }
        if !progressed {
            std::thread::sleep(IDLE_SLEEP);
        }
    }
}

/// A read of consecutive lines of one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadPlan {
    /// Source index.
    pub source: usize,
    /// First line (0-based).
    pub first: u64,
    /// Number of lines.
    pub count: usize,
}

/// Groups `entries` into per-source runs of consecutive line numbers, ordered
/// by source and line: a window of an interleaved view becomes a handful of
/// range reads instead of one read per line.
pub fn plan_reads(entries: &[MergedEntry]) -> Vec<ReadPlan> {
    let mut per_source: HashMap<usize, Vec<u64>> = HashMap::new();
    for e in entries {
        per_source.entry(e.source()).or_default().push(e.line());
    }
    let mut sources: Vec<usize> = per_source.keys().copied().collect();
    sources.sort_unstable();
    let mut plans = Vec::new();
    for s in sources {
        let Some(lines) = per_source.get_mut(&s) else {
            continue;
        };
        lines.sort_unstable();
        lines.dedup();
        let mut iter = lines.iter().copied();
        let Some(first) = iter.next() else { continue };
        let (mut start, mut end) = (first, first);
        for l in iter {
            if l == end + 1 {
                end = l;
            } else {
                plans.push(ReadPlan {
                    source: s,
                    first: start,
                    count: usize::try_from(end - start + 1).unwrap_or(usize::MAX),
                });
                (start, end) = (l, l);
            }
        }
        plans.push(ReadPlan {
            source: s,
            first: start,
            count: usize::try_from(end - start + 1).unwrap_or(usize::MAX),
        });
    }
    plans
}

/// Lines read by the [`Fetcher`].
pub struct Fetched {
    /// What was asked for.
    pub epoch: u64,
    /// `(source, line)` pairs.
    pub lines: Vec<(usize, Line)>,
}

/// Reads lines of the sources on its own thread. The UI sends a batch of
/// [`ReadPlan`]s and later picks up the answer with `try_recv`.
pub struct Fetcher {
    tx: Sender<(u64, Vec<ReadPlan>)>,
    rx: Receiver<Fetched>,
    /// A request is on its way (not answered yet).
    pub in_flight: bool,
}

impl Fetcher {
    /// Starts the thread; it ends when the `Fetcher` is dropped.
    pub fn start(docs: Vec<Arc<Document>>, wake: Arc<dyn Fn() + Send + Sync>) -> Fetcher {
        let (tx, req_rx) = unbounded::<(u64, Vec<ReadPlan>)>();
        let (res_tx, rx) = unbounded::<Fetched>();
        let spawned = std::thread::Builder::new()
            .name("oxtail-merge-fetch".into())
            .spawn(move || {
                while let Ok((epoch, plans)) = req_rx.recv() {
                    let mut lines = Vec::new();
                    for p in plans {
                        let Some(doc) = docs.get(p.source) else {
                            continue;
                        };
                        for l in doc.read_lines_blocking(p.first, p.count) {
                            lines.push((p.source, l));
                        }
                    }
                    if res_tx.send(Fetched { epoch, lines }).is_err() {
                        return;
                    }
                    wake();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the fetch thread: {e}");
        }
        Fetcher {
            tx,
            rx,
            in_flight: false,
        }
    }

    /// Asks for lines. Never blocks.
    pub fn request(&mut self, epoch: u64, plans: Vec<ReadPlan>) {
        if plans.is_empty() {
            return;
        }
        if self.tx.send((epoch, plans)).is_ok() {
            self.in_flight = true;
        }
    }

    /// The answer, if it has arrived. Never blocks.
    pub fn try_recv(&mut self) -> Option<Fetched> {
        let f = self.rx.try_recv().ok()?;
        self.in_flight = false;
        Some(f)
    }
}

/// Retries (with [`READ_RETRY_SLEEP`]) when a source returns nothing for lines
/// the merge says exist (a source that is being rotated).
const READ_RETRIES: u32 = 40;
/// Sleep between such retries.
const READ_RETRY_SLEEP: Duration = Duration::from_millis(25);

/// Reads the text of `entries`, aligned with them; a line that cannot be read
/// stays `None`.
///
/// A read of a document is capped in size, so a run of long lines comes back
/// as a prefix: each run is read again from the first line not yet received
/// until it is complete. Only a read that returns nothing is retried after a
/// pause, a bounded number of times. A document whose generation changed
/// under the read (truncated, rotated, re-decoded: the merge is about to be
/// rebuilt) is not read further. `stop` is polled between reads.
pub fn read_entries_text(
    docs: &[Arc<Document>],
    entries: &[MergedEntry],
    stop: &dyn Fn() -> bool,
) -> Vec<Option<String>> {
    let mut text: HashMap<(usize, u64), String> = HashMap::new();
    'runs: for p in plan_reads(entries) {
        let Some(doc) = docs.get(p.source) else {
            continue;
        };
        let end = p.first.saturating_add(p.count as u64);
        let mut next = p.first;
        let mut empty_reads = 0;
        while next < end {
            if stop() {
                break 'runs;
            }
            let want = usize::try_from(end - next).unwrap_or(usize::MAX);
            let (g, lines) = doc.read_lines_blocking_with_generation(next, want);
            if g != doc.generation() {
                break;
            }
            let Some(last) = lines.last().map(|l| l.number) else {
                empty_reads += 1;
                if empty_reads > READ_RETRIES {
                    break;
                }
                std::thread::sleep(READ_RETRY_SLEEP);
                continue;
            };
            empty_reads = 0;
            for l in lines {
                text.insert((p.source, l.number), l.text);
            }
            next = last + 1;
        }
    }
    entries
        .iter()
        .map(|e| text.remove(&(e.source(), e.line())))
        .collect()
}

/// A text search across the merged order, run on a worker. Matches are
/// indices into the store, ascending. The worker keeps following the store,
/// so lines that are merged later are searched too. Dropping it stops the
/// worker.
pub struct MergedSearch {
    cancel: Arc<AtomicBool>,
    matches: Arc<Mutex<Vec<usize>>>,
    scanned: Arc<AtomicU64>,
    /// The store epoch the search was started in.
    pub epoch: u64,
}

impl Drop for MergedSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl MergedSearch {
    /// Starts searching `store` for `query`.
    pub fn start(
        docs: Vec<Arc<Document>>,
        store: Arc<MergedStore>,
        query: &SearchQuery,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<MergedSearch, String> {
        let matcher = Matcher::compile(query).map_err(|e| e.to_string())?;
        let cancel = Arc::new(AtomicBool::new(false));
        let matches = Arc::new(Mutex::new(Vec::new()));
        let scanned = Arc::new(AtomicU64::new(0));
        let epoch = store.epoch();
        let (c, m, s) = (
            Arc::clone(&cancel),
            Arc::clone(&matches),
            Arc::clone(&scanned),
        );
        let spawned = std::thread::Builder::new()
            .name("oxtail-merge-search".into())
            .spawn(move || search_loop(&docs, &store, &matcher, epoch, &c, &m, &s, &*wake));
        if let Err(e) = spawned {
            return Err(format!("cannot start the search thread: {e}"));
        }
        Ok(MergedSearch {
            cancel,
            matches,
            scanned,
            epoch,
        })
    }

    /// Number of matches found so far.
    pub fn count(&self) -> usize {
        self.matches.lock().map_or(0, |m| m.len())
    }

    /// Merged lines searched so far.
    pub fn scanned(&self) -> u64 {
        self.scanned.load(Ordering::Relaxed)
    }

    /// The first match at or after `index`, wrapping around.
    pub fn next_from(&self, index: usize) -> Option<usize> {
        let m = self.matches.lock().ok()?;
        let i = m.partition_point(|&x| x < index);
        m.get(i).or_else(|| m.first()).copied()
    }

    /// The last match before `index`, wrapping around.
    pub fn prev_before(&self, index: usize) -> Option<usize> {
        let m = self.matches.lock().ok()?;
        let i = m.partition_point(|&x| x < index);
        (i.checked_sub(1).and_then(|j| m.get(j)))
            .or_else(|| m.last())
            .copied()
    }

    /// The first (`forward`) or last of `candidates` (ascending merged
    /// indices) that is a match, taking the lock once.
    pub fn first_match_in(&self, candidates: &[usize], forward: bool) -> Option<usize> {
        let m = self.matches.lock().ok()?;
        let hit = |i: &&usize| m.binary_search(i).is_ok();
        if forward {
            candidates.iter().find(hit).copied()
        } else {
            candidates.iter().rev().find(hit).copied()
        }
    }

    /// Whether `index` is a match.
    pub fn contains(&self, index: usize) -> bool {
        self.matches
            .lock()
            .is_ok_and(|m| m.binary_search(&index).is_ok())
    }
}

#[allow(clippy::too_many_arguments)] // a thread body: the pieces are separate handles
fn search_loop(
    docs: &[Arc<Document>],
    store: &MergedStore,
    matcher: &Matcher,
    epoch: u64,
    cancel: &AtomicBool,
    matches: &Mutex<Vec<usize>>,
    scanned: &AtomicU64,
    wake: &(dyn Fn() + Send + Sync),
) {
    const BLOCK: usize = 2048;
    let mut pos = 0usize;
    while !cancel.load(Ordering::Relaxed) {
        if store.epoch() != epoch {
            return; // the merge was rebuilt: the owner starts a new search
        }
        let len = store.len();
        if pos >= len {
            std::thread::sleep(Duration::from_millis(150));
            continue;
        }
        let end = (pos + BLOCK).min(len);
        let entries = store.slice(pos..end);
        let texts = read_entries_text(docs, &entries, &|| {
            cancel.load(Ordering::Relaxed) || store.epoch() != epoch
        });
        if cancel.load(Ordering::Relaxed) || store.epoch() != epoch {
            return;
        }
        let mut found = Vec::new();
        for (k, t) in texts.iter().enumerate() {
            if let Some(t) = t
                && matcher.matches(t.as_bytes())
            {
                found.push(pos + k);
            }
        }
        if !found.is_empty()
            && let Ok(mut m) = matches.lock()
        {
            m.extend(found);
            wake();
        }
        pos = end;
        scanned.store(pos as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};
    use oxtail_core::MemSource;
    use oxtail_search::{CaseMode, QueryKind};

    fn e(s: usize, l: u64) -> MergedEntry {
        MergedEntry::new(s, l).unwrap()
    }

    #[test]
    fn reads_are_grouped_into_runs_per_source() {
        let entries = [
            e(0, 5),
            e(1, 2),
            e(0, 6),
            e(1, 3),
            e(0, 7),
            e(0, 20),
            e(1, 3),
        ];
        assert_eq!(
            plan_reads(&entries),
            vec![
                ReadPlan {
                    source: 0,
                    first: 5,
                    count: 3
                },
                ReadPlan {
                    source: 0,
                    first: 20,
                    count: 1
                },
                ReadPlan {
                    source: 1,
                    first: 2,
                    count: 2
                },
            ]
        );
        assert!(plan_reads(&[]).is_empty());
    }

    fn ts_line(sec: u32, text: &str) -> String {
        format!("2026-09-29 10:00:{sec:02} {text}\n")
    }

    fn two_sources() -> (Arc<Document>, Arc<Document>) {
        // a: 0, 2, 4, 6 ...; b: 1, 3, 5, 7 ...
        let mut a = String::new();
        let mut b = String::new();
        for i in 0..20u32 {
            a.push_str(&ts_line(i * 2, &format!("a{i}")));
            b.push_str(&ts_line(i * 2 + 1, &format!("b{i}")));
        }
        (doc(&a), doc(&b))
    }

    fn opts() -> MergeOptions {
        MergeOptions {
            idle_delay: Duration::from_millis(30),
            ..MergeOptions::default()
        }
    }

    fn wait_len(store: &MergedStore, n: usize) {
        wait("merged entries", || (store.len() >= n).then_some(()));
    }

    #[test]
    fn two_sources_interleave_by_timestamp() {
        let (a, b) = two_sources();
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(vec![a, b], Arc::clone(&store), opts(), Arc::new(|| {}));
        wait_len(&store, 40);
        wait("caught up", || store.caught_up().then_some(()));
        let all = store.slice(0..40);
        for (k, entry) in all.iter().enumerate() {
            // a0 b0 a1 b1 ...
            assert_eq!(entry.source(), k % 2, "{k}");
            assert_eq!(entry.line(), (k / 2) as u64, "{k}");
        }
        assert_eq!(store.bytes(), 40 * 8);
        assert_eq!(store.lines_read(), 40);
    }

    #[test]
    fn new_lines_are_merged_in_while_following() {
        let mem_a = Arc::new(MemSource::new(ts_line(0, "a0").into_bytes()));
        let mem_b = Arc::new(MemSource::new(ts_line(1, "b0").into_bytes()));
        let a = Arc::new(Document::from_source(mem_a.clone(), "a.log"));
        let b = Arc::new(Document::from_source(mem_b.clone(), "b.log"));
        let store = Arc::new(MergedStore::default());
        // A quiet source holds the others back only for the idle delay,
        // counted from when it went live. So append while `a` is still inside
        // it: right after a0 is merged, while b0 waits for `a`. Waiting for b0
        // first meant waiting out the delay, after which b1 could be merged
        // before a1 was noticed (CI runs 15 and 26, macos-latest).
        let opts = MergeOptions {
            idle_delay: Duration::from_secs(5),
            ..opts()
        };
        let _w = MergeWorker::start(vec![a, b], Arc::clone(&store), opts, Arc::new(|| {}));
        wait_len(&store, 1);
        mem_b.append(ts_line(3, "b1").as_bytes());
        mem_a.append(ts_line(2, "a1").as_bytes());
        wait_len(&store, 4);
        let all = store.slice(0..4);
        let order: Vec<(usize, u64)> = all.iter().map(|x| (x.source(), x.line())).collect();
        assert_eq!(order, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn lines_without_timestamps_stay_with_their_predecessor() {
        let a = doc(&format!(
            "{}    at Foo.bar\n{}",
            ts_line(0, "a0"),
            ts_line(10, "a1")
        ));
        let b = doc(&ts_line(5, "b0"));
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(vec![a, b], Arc::clone(&store), opts(), Arc::new(|| {}));
        wait_len(&store, 4);
        let order: Vec<(usize, u64)> = store
            .slice(0..4)
            .iter()
            .map(|x| (x.source(), x.line()))
            .collect();
        assert_eq!(order, vec![(0, 0), (0, 1), (1, 0), (0, 2)]);
    }

    #[test]
    fn a_rotation_rebuilds_the_merge() {
        let mem_a = Arc::new(MemSource::new(
            [ts_line(0, "a0"), ts_line(2, "a1")].concat().into_bytes(),
        ));
        let a = Arc::new(Document::from_source(mem_a, "a.log"));
        let b = doc(&ts_line(1, "b0"));
        let store = Arc::new(MergedStore::default());
        let _w = MergeWorker::start(
            vec![Arc::clone(&a), b],
            Arc::clone(&store),
            opts(),
            Arc::new(|| {}),
        );
        wait_len(&store, 3);
        let epoch = store.epoch();
        // Re-decoding bumps the generation: the merge starts over.
        a.set_encoding(oxtail_core::EncodingChoice::Auto);
        wait("rebuild", || (store.epoch() > epoch).then_some(()));
        wait_len(&store, 3);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn the_fetcher_reads_the_planned_ranges() {
        let (a, b) = two_sources();
        let mut f = Fetcher::start(vec![a, b], Arc::new(|| {}));
        assert!(!f.in_flight);
        f.request(7, plan_reads(&[e(0, 3), e(0, 4), e(1, 9)]));
        assert!(f.in_flight);
        let got = wait("fetch", || f.try_recv());
        assert!(!f.in_flight);
        assert_eq!(got.epoch, 7);
        let mut names: Vec<(usize, u64, String)> = got
            .lines
            .iter()
            .map(|(s, l)| (*s, l.number, l.text.clone()))
            .collect();
        names.sort();
        assert_eq!(names.len(), 3);
        assert!(names[0].2.ends_with("a3"));
        assert!(names[2].2.ends_with("b9"));
        // Nothing to ask for: nothing sent.
        f.request(8, Vec::new());
        assert!(!f.in_flight);
    }

    #[test]
    fn the_search_finds_matches_in_merged_order() {
        let (a, b) = two_sources();
        let store = Arc::new(MergedStore::default());
        let docs = vec![Arc::clone(&a), Arc::clone(&b)];
        let _w = MergeWorker::start(docs.clone(), Arc::clone(&store), opts(), Arc::new(|| {}));
        wait_len(&store, 40);
        let q = SearchQuery {
            pattern: "b1".into(),
            kind: QueryKind::Literal,
            case: CaseMode::Smart,
            whole_word: false,
        };
        let s = MergedSearch::start(docs, Arc::clone(&store), &q, Arc::new(|| {})).unwrap();
        // b1, b10..b19: 11 matches.
        wait("search", || {
            (s.scanned() >= 40 && s.count() == 11).then_some(())
        });
        // b1 is the 4th merged line (index 3), b10 at index 21.
        assert_eq!(s.next_from(0), Some(3));
        assert_eq!(s.next_from(4), Some(21));
        assert!(s.contains(3) && !s.contains(4));
        // Wrapping in both directions.
        assert_eq!(s.next_from(40), Some(3));
        assert_eq!(s.prev_before(3), s.prev_before(0));
        assert_eq!(s.prev_before(22), Some(21));
        let bad = SearchQuery {
            pattern: "(".into(),
            kind: QueryKind::Regex,
            ..q
        };
        assert!(MergedSearch::start(vec![], store, &bad, Arc::new(|| {})).is_err());
    }
}
