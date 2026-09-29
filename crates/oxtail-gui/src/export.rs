//! Exporting the current (filtered) view with chosen columns to CSV or JSON
//! Lines. The save dialog runs on a worker (rfd async, like the open dialog)
//! and a second worker streams the file: lines are read in chunks, parsed,
//! and handed to `oxtail_columns::export_csv` / `export_jsonl` through a
//! lazy iterator, so memory stays constant. Progress streams back; cancelling
//! stops the scan and removes the partial file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use oxtail_columns::{Parser, Record, export_csv, export_jsonl};
use oxtail_core::{Document, Line};
use oxtail_search::MatchSet;

use crate::docscan::{CHUNK_LINES, ScanEnd};

/// The output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    /// Comma-separated values with a header row.
    #[default]
    Csv,
    /// One JSON object per line.
    JsonLines,
}

impl ExportFormat {
    /// The file extension.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::JsonLines => "jsonl",
        }
    }
}

/// What to export.
#[derive(Clone)]
pub struct ExportSpec {
    /// The output file.
    pub path: PathBuf,
    /// The format.
    pub format: ExportFormat,
    /// Schema indices of the columns to write, in output order.
    pub columns: Vec<usize>,
    /// Only the lines of this filter view (`None`: every line).
    pub filter: Option<Arc<MatchSet>>,
}

/// A message from the export worker.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportMsg {
    /// Progress so far.
    Progress {
        /// Lines read.
        scanned: u64,
        /// Lines the scan covers.
        total: u64,
        /// Records written.
        written: u64,
    },
    /// Finished.
    Done {
        /// Records written.
        written: u64,
        /// The file.
        path: PathBuf,
    },
    /// Stopped by the user; the partial file was removed.
    Cancelled,
    /// Failed.
    Failed(String),
}

/// A running export.
pub struct ExportJob {
    rx: Receiver<ExportMsg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl ExportJob {
    /// Starts the worker.
    pub fn start(
        doc: Arc<Document>,
        parser: Arc<Parser>,
        spec: ExportSpec,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> ExportJob {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("oxtail-export".into())
            .spawn(move || {
                let msg = run(&doc, &parser, &spec, &flag, &tx, &*wake);
                let _ = tx.send(msg);
                wake();
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the export thread: {e}");
        }
        ExportJob { rx, cancel }
    }

    /// Asks the worker to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The next message, if any. Never blocks.
    pub fn try_recv(&self) -> Option<ExportMsg> {
        self.rx.try_recv().ok()
    }
}

/// A lazy stream of the records to export: reads chunks of lines, applies the
/// filter, parses, and reports progress. Ends early when cancelled.
struct RecordStream<'a> {
    doc: &'a Document,
    parser: &'a Parser,
    filter: Option<&'a MatchSet>,
    cancel: &'a AtomicBool,
    tx: &'a Sender<ExportMsg>,
    wake: &'a (dyn Fn() + Send + Sync),
    generation: u64,
    total: u64,
    next_line: u64,
    buffer: std::collections::VecDeque<Line>,
    written: u64,
    ended: Option<ScanEnd>,
    last_sent: Instant,
}

impl<'a> RecordStream<'a> {
    fn fill(&mut self) -> bool {
        while self.buffer.is_empty() {
            if self.next_line >= self.total {
                self.ended = Some(ScanEnd::Done);
                return false;
            }
            if self.cancel.load(Ordering::Relaxed) {
                self.ended = Some(ScanEnd::Cancelled);
                return false;
            }
            let want = usize::try_from((self.total - self.next_line).min(CHUNK_LINES as u64))
                .unwrap_or(CHUNK_LINES);
            let (g, lines) = self
                .doc
                .read_lines_blocking_with_generation(self.next_line, want);
            if g != self.generation || self.doc.generation() != self.generation {
                self.ended = Some(ScanEnd::Changed);
                return false;
            }
            if lines.is_empty() {
                self.ended = Some(ScanEnd::Done);
                return false;
            }
            self.next_line += lines.len() as u64;
            self.buffer.extend(lines);
            if self.last_sent.elapsed() >= Duration::from_millis(250) {
                self.last_sent = Instant::now();
                let _ = self.tx.send(ExportMsg::Progress {
                    scanned: self.next_line,
                    total: self.total,
                    written: self.written,
                });
                (self.wake)();
            }
        }
        true
    }
}

impl Iterator for RecordStream<'_> {
    type Item = Record<'static>;

    fn next(&mut self) -> Option<Record<'static>> {
        loop {
            if !self.fill() {
                return None;
            }
            let line = self.buffer.pop_front()?;
            if let Some(set) = self.filter
                && !set.contains(line.offset)
            {
                continue;
            }
            if let Some(rec) = self.parser.parse(&line.text) {
                self.written += 1;
                return Some(rec.into_owned());
            }
        }
    }
}

fn run(
    doc: &Document,
    parser: &Parser,
    spec: &ExportSpec,
    cancel: &AtomicBool,
    tx: &Sender<ExportMsg>,
    wake: &(dyn Fn() + Send + Sync),
) -> ExportMsg {
    let generation = doc.generation();
    // Wait for the index so the scan covers the whole file.
    let total = loop {
        if cancel.load(Ordering::Relaxed) {
            return ExportMsg::Cancelled;
        }
        let snap = doc.snapshot();
        if snap.generation != generation {
            return ExportMsg::Failed("the file changed before the export started".into());
        }
        if crate::docscan::index_complete(&snap) {
            break snap.lines.known;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let file = match File::create(&spec.path) {
        Ok(f) => f,
        Err(e) => return ExportMsg::Failed(format!("cannot create {}: {e}", spec.path.display())),
    };
    let mut out = BufWriter::new(file);
    let mut stream = RecordStream {
        doc,
        parser,
        filter: spec.filter.as_deref(),
        cancel,
        tx,
        wake,
        generation,
        total,
        next_line: 0,
        buffer: Default::default(),
        written: 0,
        ended: None,
        last_sent: Instant::now(),
    };
    let schema = parser.schema();
    let result = match spec.format {
        ExportFormat::Csv => export_csv(&mut out, schema, &spec.columns, &mut stream),
        ExportFormat::JsonLines => export_jsonl(&mut out, schema, &spec.columns, &mut stream),
    };
    let ended = stream.ended;
    let written = stream.written;
    let flushed = out.flush();
    drop(out);
    let discard = |msg: ExportMsg| {
        let _ = std::fs::remove_file(&spec.path);
        msg
    };
    match (result, flushed, ended) {
        (Err(e), _, _) | (_, Err(e), _) => discard(ExportMsg::Failed(format!("write failed: {e}"))),
        (_, _, Some(ScanEnd::Cancelled)) => discard(ExportMsg::Cancelled),
        (_, _, Some(ScanEnd::Changed)) => discard(ExportMsg::Failed(
            "the file changed during the export".into(),
        )),
        _ => ExportMsg::Done {
            written,
            path: spec.path.clone(),
        },
    }
}

/// The state of the export dialog and its running job.
#[derive(Default)]
pub struct ExportState {
    /// The dialog is open.
    pub open: bool,
    /// The format.
    pub format: ExportFormat,
    /// Which columns to write (by schema index).
    pub selected: Vec<bool>,
    /// Only the filtered lines (when a filter view is active).
    pub filtered_only: bool,
    /// The running job.
    pub job: Option<ExportJob>,
    /// Progress (scanned, total).
    pub progress: (u64, u64),
    /// The last result, for showing.
    pub result: Option<ExportMsg>,
    /// A save dialog is open on its worker.
    pub choosing: bool,
    /// The path the save dialog returned, waiting to be picked up.
    pub chosen: Option<Receiver<Option<PathBuf>>>,
}

impl ExportState {
    /// Makes sure `selected` matches a schema of `n` columns (all selected
    /// when it had a different size).
    pub fn sync_columns(&mut self, n: usize) {
        if self.selected.len() != n {
            self.selected = vec![true; n];
        }
    }

    /// The selected columns in schema order.
    pub fn columns(&self, order: &[usize]) -> Vec<usize> {
        order
            .iter()
            .copied()
            .filter(|&c| self.selected.get(c).copied().unwrap_or(false))
            .collect()
    }

    /// Applies the worker's messages; returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(msg) = self.job.as_ref().and_then(ExportJob::try_recv) {
            changed = true;
            match msg {
                ExportMsg::Progress { scanned, total, .. } => self.progress = (scanned, total),
                other => {
                    self.result = Some(other);
                    self.job = None;
                }
            }
        }
        changed
    }
}

/// Opens the save dialog on a worker thread. The chosen path (or `None`)
/// arrives on the returned channel.
pub fn pick_save_path(
    format: ExportFormat,
    suggested: &str,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = unbounded();
    let name = format!("{suggested}.{}", format.extension());
    let spawned = std::thread::Builder::new()
        .name("oxtail-save-dialog".into())
        .spawn(move || {
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("Export")
                .set_file_name(name)
                .add_filter(
                    match format {
                        ExportFormat::Csv => "CSV",
                        ExportFormat::JsonLines => "JSON Lines",
                    },
                    &[format.extension()],
                );
            let picked = pollster::block_on(dialog.save_file());
            let _ = tx.send(picked.map(|h| h.path().to_path_buf()));
            wake();
        });
    if spawned.is_err() {
        // The sender is gone: the receiver reports a closed channel.
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};
    use oxtail_columns::ParserSpec;

    fn logfmt() -> Arc<Parser> {
        Arc::new(
            ParserSpec::Logfmt {
                columns: vec!["level".into(), "n".into(), "msg".into()],
                kinds: [("n".to_string(), oxtail_columns::ColumnKind::Number)]
                    .into_iter()
                    .collect(),
            }
            .compile()
            .unwrap(),
        )
    }

    fn sample(n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            let level = if i % 4 == 0 { "error" } else { "info" };
            s.push_str(&format!(
                "level={level} n={i} msg=\"hello, \\\"world\\\" {i}\"\n"
            ));
            if i % 7 == 0 {
                s.push_str("    continuation line, not a record\n");
            }
        }
        s
    }

    fn run_export(text: &str, spec: ExportSpec) -> ExportMsg {
        let d = doc(text);
        let job = ExportJob::start(d, logfmt(), spec, Arc::new(|| {}));
        wait("export", || match job.try_recv() {
            Some(ExportMsg::Progress { .. }) | None => None,
            Some(m) => Some(m),
        })
    }

    #[test]
    fn csv_round_trips_through_the_delimited_parser() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        let text = sample(200);
        let msg = run_export(
            &text,
            ExportSpec {
                path: path.clone(),
                format: ExportFormat::Csv,
                columns: vec![1, 0, 2],
                filter: None,
            },
        );
        assert_eq!(
            msg,
            ExportMsg::Done {
                written: 200,
                path: path.clone()
            }
        );
        let out = std::fs::read_to_string(&path).unwrap();
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("n,level,msg"));
        let spec = ParserSpec::delimited_from_header_line("n,level,msg", ',');
        let p = spec.compile().unwrap();
        // Rows can contain quoted commas but no newlines here.
        let rows: Vec<&str> = lines.collect();
        assert_eq!(rows.len(), 200);
        let r = p.parse(rows[4]).unwrap();
        assert_eq!(r.get(0), Some("4"));
        assert_eq!(r.get(1), Some("error"));
        assert_eq!(r.get(2), Some("hello, \"world\" 4"));
    }

    #[test]
    fn jsonl_round_trips_through_the_json_parser() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.jsonl");
        let msg = run_export(
            &sample(50),
            ExportSpec {
                path: path.clone(),
                format: ExportFormat::JsonLines,
                columns: vec![0, 1],
                filter: None,
            },
        );
        assert!(
            matches!(msg, ExportMsg::Done { written: 50, .. }),
            "{msg:?}"
        );
        let out = std::fs::read_to_string(&path).unwrap();
        let p = ParserSpec::JsonLines {
            columns: vec!["level".into(), "n".into()],
            kinds: Default::default(),
        }
        .compile()
        .unwrap();
        let recs: Vec<_> = out.lines().map(|l| p.parse(l).unwrap()).collect();
        assert_eq!(recs.len(), 50);
        assert_eq!(recs[8].get(0), Some("error"));
        assert_eq!(recs[8].get(1), Some("8"));
        // The number is a JSON number, not a string.
        assert!(out.lines().nth(8).unwrap().contains("\"n\":8"));
    }

    #[test]
    fn a_filter_limits_the_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.csv");
        let text = sample(30);
        // Keep the lines with n=3 and n=5.
        let mut offs = Vec::new();
        let mut at = 0u64;
        for l in text.split_inclusive('\n') {
            if l.contains(" n=3 ") || l.contains(" n=5 ") {
                offs.push(at);
            }
            at += l.len() as u64;
        }
        let msg = run_export(
            &text,
            ExportSpec {
                path: path.clone(),
                format: ExportFormat::Csv,
                columns: vec![1],
                filter: Some(Arc::new(MatchSet::from_offsets(offs))),
            },
        );
        assert!(matches!(msg, ExportMsg::Done { written: 2, .. }), "{msg:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "n\n3\n5\n");
    }

    #[test]
    fn cancelling_removes_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.csv");
        let d = doc(&sample(20_000));
        let cancel = AtomicBool::new(true);
        let (tx, _rx) = unbounded();
        let spec = ExportSpec {
            path: path.clone(),
            format: ExportFormat::Csv,
            columns: vec![0],
            filter: None,
        };
        let msg = run(&d, &logfmt(), &spec, &cancel, &tx, &|| {});
        assert_eq!(msg, ExportMsg::Cancelled);
        assert!(!path.exists());
    }

    #[test]
    fn cancelling_midway_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.csv");
        let d = doc(&sample(50_000));
        let cancel = AtomicBool::new(false);
        let (tx, _rx) = unbounded();
        let spec = ExportSpec {
            path: path.clone(),
            format: ExportFormat::Csv,
            columns: vec![0],
            filter: None,
        };
        // Cancel once the worker reports its first progress.
        let cancel_ref = &cancel;
        let msg = std::thread::scope(|s| {
            let h = s.spawn(|| run(&d, &logfmt(), &spec, cancel_ref, &tx, &|| {}));
            let deadline = Instant::now() + Duration::from_secs(10);
            while !h.is_finished() && Instant::now() < deadline {
                if path.exists() {
                    cancel_ref.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            h.join().unwrap()
        });
        // A very fast machine may finish before the flag is seen.
        assert!(matches!(msg, ExportMsg::Cancelled | ExportMsg::Done { .. }));
        if msg == ExportMsg::Cancelled {
            assert!(!path.exists());
        }
    }

    #[test]
    fn unwritable_paths_fail_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let msg = run_export(
            &sample(5),
            ExportSpec {
                path: dir.path().join("no/such/dir/x.csv"),
                format: ExportFormat::Csv,
                columns: vec![0],
                filter: None,
            },
        );
        assert!(matches!(msg, ExportMsg::Failed(_)), "{msg:?}");
    }

    #[test]
    fn state_helpers() {
        let mut st = ExportState::default();
        st.sync_columns(3);
        st.selected[1] = false;
        assert_eq!(st.columns(&[2, 1, 0]), vec![2, 0]);
        st.sync_columns(4);
        assert_eq!(st.selected, vec![true; 4]);
        assert_eq!(ExportFormat::JsonLines.extension(), "jsonl");
    }
}
