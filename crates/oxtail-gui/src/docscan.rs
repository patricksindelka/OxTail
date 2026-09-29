//! Scanning a document line by line on a worker thread: the shared engine
//! behind column statistics, export and cross-tab search.
//!
//! Lines are read in chunks with `Document::read_lines_blocking_with_generation`
//! (never on the UI thread). The scan covers the lines known when it starts
//! (a followed file that keeps growing is not chased), waits while the line
//! index is still being built, stops promptly when cancelled, and gives up
//! when the document's generation changes (truncation, rotation, re-decode).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use oxtail_core::{DocState, Document, Line};

/// Lines read per request.
pub const CHUNK_LINES: usize = 2000;
/// How long to wait between index polls while the index is incomplete.
const INDEX_POLL: Duration = Duration::from_millis(20);

/// How a scan ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEnd {
    /// Every line was visited.
    Done,
    /// The callback asked to stop, or the cancel flag was set.
    Cancelled,
    /// The document changed underneath the scan (truncated, rotated,
    /// re-decoded): the results would mix generations.
    Changed,
}

/// Whether the snapshot's line count is final: the document has been opened
/// (an unopened one reports an exact count of zero) and indexed.
pub fn index_complete(snap: &oxtail_core::DocSnapshot) -> bool {
    snap.lines.exact && matches!(snap.state, DocState::Ready)
}

/// Visits the lines of `doc` in order. `on_line` returns `false` to stop;
/// `on_progress(scanned, total)` is called after every chunk (`total` is the
/// number of lines the scan will cover, exact once known).
pub fn scan(
    doc: &Document,
    cancel: &AtomicBool,
    mut on_line: impl FnMut(&Line) -> bool,
    mut on_progress: impl FnMut(u64, u64),
) -> ScanEnd {
    let generation = doc.generation();
    // The scan covers the lines that exist once the index is complete.
    let total = loop {
        if cancel.load(Ordering::Relaxed) {
            return ScanEnd::Cancelled;
        }
        let snap = doc.snapshot();
        if snap.generation != generation {
            return ScanEnd::Changed;
        }
        if index_complete(&snap) {
            break snap.lines.known;
        }
        std::thread::sleep(INDEX_POLL);
    };
    let mut first = 0u64;
    while first < total {
        if cancel.load(Ordering::Relaxed) {
            return ScanEnd::Cancelled;
        }
        let want = usize::try_from((total - first).min(CHUNK_LINES as u64)).unwrap_or(CHUNK_LINES);
        let (g, lines) = doc.read_lines_blocking_with_generation(first, want);
        if g != generation || doc.generation() != generation {
            return ScanEnd::Changed;
        }
        if lines.is_empty() {
            // The file shrank without a generation bump, or the read failed:
            // nothing more can be read.
            return ScanEnd::Done;
        }
        first += lines.len() as u64;
        for l in &lines {
            if !on_line(l) {
                return ScanEnd::Cancelled;
            }
        }
        on_progress(first.min(total), total);
    }
    ScanEnd::Done
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use oxtail_core::{Document, MemSource};

    /// A document over `text` whose index is complete.
    pub fn doc(text: &str) -> Arc<Document> {
        let d = Arc::new(Document::from_source(
            Arc::new(MemSource::new(text.as_bytes().to_vec())),
            "t.log",
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::index_complete(&d.snapshot()) {
            assert!(Instant::now() < deadline, "index did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
        d
    }

    /// Waits (at most 10 s) until `f` returns `Some`.
    pub fn wait<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::doc;
    use super::*;

    fn text(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn every_line_is_visited_once_in_order() {
        let d = doc(&text(5000));
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let mut last = (0, 0);
        let end = scan(
            &d,
            &cancel,
            |l| {
                seen.push(l.number);
                true
            },
            |s, t| last = (s, t),
        );
        assert_eq!(end, ScanEnd::Done);
        assert_eq!(seen, (0..5000).collect::<Vec<u64>>());
        assert_eq!(last, (5000, 5000));
    }

    #[test]
    fn the_callback_can_stop_the_scan() {
        let d = doc(&text(100));
        let cancel = AtomicBool::new(false);
        let mut n = 0;
        let end = scan(
            &d,
            &cancel,
            |_| {
                n += 1;
                n < 10
            },
            |_, _| {},
        );
        assert_eq!(end, ScanEnd::Cancelled);
        assert_eq!(n, 10);
    }

    #[test]
    fn a_cancelled_scan_stops_before_reading() {
        let d = doc(&text(100));
        let cancel = AtomicBool::new(true);
        let mut n = 0;
        assert_eq!(
            scan(
                &d,
                &cancel,
                |_| {
                    n += 1;
                    true
                },
                |_, _| {}
            ),
            ScanEnd::Cancelled
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn an_empty_document_is_done_at_once() {
        let d = doc("");
        let cancel = AtomicBool::new(false);
        assert_eq!(scan(&d, &cancel, |_| true, |_, _| {}), ScanEnd::Done);
    }
}
