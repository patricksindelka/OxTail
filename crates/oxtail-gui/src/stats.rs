//! Column statistics on demand: a worker thread scans the document (the whole
//! file or the current filter's lines), parses each line with the tab's
//! parser and accumulates an `oxtail_columns::TableStats`. Progress streams
//! to the UI as snapshots; dropping the job or calling [`StatsJob::cancel`]
//! stops the worker.
//!
//! The panel shows top values with counts and, for numeric columns,
//! min/max/mean and percentiles; clicking a value adds `column:"value"` as an
//! include filter (see [`crate::qfilter::equals_query`]).

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use oxtail_columns::{ColumnKind, Parser, TableStats, format};
use oxtail_core::Document;
use oxtail_search::MatchSet;

use crate::docscan::{ScanEnd, scan};

/// Minimum time between progress snapshots.
const SNAPSHOT_EVERY: Duration = Duration::from_millis(250);

/// What a statistics run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatsScope {
    /// Every line of the file.
    #[default]
    File,
    /// Only the lines of the current filter view.
    Filter,
}

/// A message from the worker.
pub enum StatsMsg {
    /// Partial results.
    Progress {
        /// Lines read so far.
        scanned: u64,
        /// Lines the scan covers.
        total: u64,
        /// The statistics so far.
        table: Box<TableStats>,
    },
    /// The run is complete.
    Done {
        /// Lines read.
        scanned: u64,
        /// Final statistics.
        table: Box<TableStats>,
        /// Lines the parser did not accept (continuation lines).
        skipped: u64,
    },
    /// The run stopped early (cancelled, or the document changed).
    Stopped {
        /// Why.
        reason: &'static str,
    },
}

/// A running statistics job.
pub struct StatsJob {
    rx: Receiver<StatsMsg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for StatsJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl StatsJob {
    /// Starts the worker. `filter` restricts the scan to the lines of a
    /// filter view. `wake` requests a repaint when a message is sent.
    pub fn start(
        doc: Arc<Document>,
        parser: Arc<Parser>,
        filter: Option<Arc<MatchSet>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> StatsJob {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("oxtail-stats".into())
            .spawn(move || run(&doc, &parser, filter.as_deref(), &flag, &tx, &*wake));
        if let Err(e) = spawned {
            tracing::warn!("cannot start the statistics thread: {e}");
        }
        StatsJob { rx, cancel }
    }

    /// Asks the worker to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The next message, if any. Never blocks.
    pub fn try_recv(&self) -> Option<StatsMsg> {
        self.rx.try_recv().ok()
    }
}

fn run(
    doc: &Document,
    parser: &Parser,
    filter: Option<&MatchSet>,
    cancel: &AtomicBool,
    tx: &crossbeam_channel::Sender<StatsMsg>,
    wake: &(dyn Fn() + Send + Sync),
) {
    let table = RefCell::new(TableStats::new(parser.schema()));
    let skipped = Cell::new(0u64);
    let scanned = Cell::new(0u64);
    let mut last_sent = Instant::now();
    let end = scan(
        doc,
        cancel,
        |line| {
            if let Some(set) = filter
                && !set.contains(line.offset)
            {
                return true;
            }
            match parser.parse(&line.text) {
                Some(rec) => table.borrow_mut().add_record(&rec),
                None => skipped.set(skipped.get() + 1),
            }
            true
        },
        |done, total| {
            scanned.set(done);
            if last_sent.elapsed() >= SNAPSHOT_EVERY {
                last_sent = Instant::now();
                let _ = tx.send(StatsMsg::Progress {
                    scanned: done,
                    total,
                    table: Box::new(table.borrow().clone()),
                });
                wake();
            }
        },
    );
    let msg = match end {
        ScanEnd::Done => StatsMsg::Done {
            scanned: scanned.get(),
            table: Box::new(table.into_inner()),
            skipped: skipped.get(),
        },
        ScanEnd::Cancelled => StatsMsg::Stopped {
            reason: "cancelled",
        },
        ScanEnd::Changed => StatsMsg::Stopped {
            reason: "the file changed while scanning",
        },
    };
    let _ = tx.send(msg);
    wake();
}

/// The panel's state: the running job and the latest results.
#[derive(Default)]
pub struct StatsState {
    /// The panel is open.
    pub open: bool,
    /// What to scan.
    pub scope: StatsScope,
    /// The running job.
    pub job: Option<StatsJob>,
    /// The latest (possibly partial) statistics.
    pub table: Option<TableStats>,
    /// Lines read and to read.
    pub progress: (u64, u64),
    /// The run finished.
    pub done: bool,
    /// Lines that were not records.
    pub skipped: u64,
    /// Why the last run stopped early.
    pub stopped: Option<&'static str>,
    /// The column shown (schema index).
    pub column: usize,
}

impl StatsState {
    /// Starts a run (cancelling a running one).
    pub fn start(
        &mut self,
        doc: &Arc<Document>,
        parser: &Arc<Parser>,
        filter: Option<Arc<MatchSet>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) {
        self.table = None;
        self.done = false;
        self.skipped = 0;
        self.stopped = None;
        self.progress = (0, 0);
        self.job = Some(StatsJob::start(
            Arc::clone(doc),
            Arc::clone(parser),
            filter,
            wake,
        ));
    }

    /// Stops the running job.
    pub fn cancel(&mut self) {
        if let Some(j) = self.job.take() {
            j.cancel();
        }
    }

    /// Whether a job is running.
    pub fn running(&self) -> bool {
        self.job.is_some() && !self.done
    }

    /// Applies the worker's messages. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(msg) = self.job.as_ref().and_then(StatsJob::try_recv) {
            changed = true;
            match msg {
                StatsMsg::Progress {
                    scanned,
                    total,
                    table,
                } => {
                    self.progress = (scanned, total);
                    self.table = Some(*table);
                }
                StatsMsg::Done {
                    scanned,
                    table,
                    skipped,
                } => {
                    self.progress = (scanned, scanned);
                    self.table = Some(*table);
                    self.skipped = skipped;
                    self.done = true;
                }
                StatsMsg::Stopped { reason } => {
                    self.stopped = Some(reason);
                    self.done = true;
                }
            }
        }
        changed
    }
}

/// Formats a numeric statistic of a column of `kind`: durations (stored in
/// nanoseconds) and sizes are humanised.
pub fn format_stat(kind: ColumnKind, v: f64) -> String {
    match kind {
        ColumnKind::Duration => format::format_duration_ns(v),
        ColumnKind::Bytes => format::format_bytes(v),
        _ => format::format_number(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};
    use oxtail_columns::ParserSpec;

    fn logfmt() -> Arc<Parser> {
        Arc::new(
            ParserSpec::Logfmt {
                columns: vec!["level".into(), "took".into(), "path".into()],
                kinds: [("took".to_string(), ColumnKind::Number)]
                    .into_iter()
                    .collect(),
            }
            .compile()
            .unwrap(),
        )
    }

    fn sample(n: usize) -> String {
        (0..n)
            .map(|i| {
                let level = if i % 10 == 0 { "error" } else { "info" };
                format!("level={level} took={} path=/p{}\n", i + 1, i % 3)
            })
            .collect()
    }

    fn finish(job: &StatsJob) -> (Box<TableStats>, u64, u64) {
        wait("stats", || match job.try_recv() {
            Some(StatsMsg::Done {
                table,
                scanned,
                skipped,
            }) => Some((table, scanned, skipped)),
            Some(StatsMsg::Stopped { reason }) => panic!("stopped: {reason}"),
            _ => None,
        })
    }

    #[test]
    fn counts_top_values_and_numeric_summaries() {
        let d = doc(&sample(1000));
        let job = StatsJob::start(d, logfmt(), None, Arc::new(|| {}));
        let (t, scanned, skipped) = finish(&job);
        assert_eq!((scanned, skipped), (1000, 0));
        assert_eq!(t.records, 1000);
        let level = &t.columns[0];
        let top = level.top(5);
        assert_eq!(top[0].value, "INFO");
        assert_eq!(top[0].count, 900);
        assert_eq!(top[1].count, 100);
        let took = &t.columns[1];
        assert_eq!(took.min(), Some(1.0));
        assert_eq!(took.max(), Some(1000.0));
        assert!((took.mean().unwrap() - 500.5).abs() < 1e-9);
        let p = took.percentiles().unwrap();
        assert!((p.p50 - 500.0).abs() <= 2.0, "{p:?}");
        assert!((p.p99 - 990.0).abs() <= 12.0, "{p:?}");
    }

    #[test]
    fn a_filter_restricts_the_scan_to_its_lines() {
        let text = sample(300);
        let d = doc(&text);
        // The filter keeps the error lines only.
        let mut offsets = Vec::new();
        let mut at = 0u64;
        for (i, l) in text.split_inclusive('\n').enumerate() {
            if i % 10 == 0 {
                offsets.push(at);
            }
            at += l.len() as u64;
        }
        let set = Arc::new(MatchSet::from_offsets(offsets));
        let job = StatsJob::start(d, logfmt(), Some(set), Arc::new(|| {}));
        let (t, ..) = finish(&job);
        assert_eq!(t.records, 30);
        assert_eq!(t.columns[0].top(3)[0].value, "ERROR");
        assert_eq!(t.columns[0].top(3).len(), 1);
    }

    #[test]
    fn lines_the_parser_rejects_are_counted_as_skipped() {
        let text =
            "level=info took=1 path=/a\n    at Foo.bar(Foo.java:1)\n\nlevel=error took=2 path=/b\n";
        let parser = Arc::new(
            ParserSpec::JsonLines {
                columns: vec!["level".into()],
                kinds: Default::default(),
            }
            .compile()
            .unwrap(),
        );
        let d = doc(text);
        let job = StatsJob::start(d, parser, None, Arc::new(|| {}));
        let (t, scanned, skipped) = finish(&job);
        assert_eq!((t.records, scanned, skipped), (0, 4, 4));
    }

    #[test]
    fn dropping_the_job_cancels_the_worker() {
        let d = doc(&sample(10));
        let job = StatsJob::start(d, logfmt(), None, Arc::new(|| {}));
        let flag = Arc::clone(&job.cancel);
        drop(job);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn state_follows_the_job_messages() {
        let d = doc(&sample(500));
        let mut st = StatsState::default();
        st.start(&d, &logfmt(), None, Arc::new(|| {}));
        assert!(st.running());
        wait("state done", || {
            st.poll();
            st.done.then_some(())
        });
        assert_eq!(st.progress, (500, 500));
        assert_eq!(st.table.as_ref().unwrap().records, 500);
        assert!(!st.running());
        // Cancelling drops the job.
        st.cancel();
        assert!(st.job.is_none());
    }

    #[test]
    fn stats_are_formatted_by_kind() {
        assert_eq!(format_stat(ColumnKind::Duration, 1.5e6), "1.5 ms");
        assert_eq!(format_stat(ColumnKind::Bytes, 2048.0), "2 KiB");
        assert_eq!(format_stat(ColumnKind::Number, 1234.5), "1,234.5");
    }
}
