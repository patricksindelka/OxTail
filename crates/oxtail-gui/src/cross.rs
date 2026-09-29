//! Search across the open tabs: one `SearchJob` per tab, run one after the
//! other on a worker thread, with the results grouped by file. Each tab
//! reports how many lines match and its first matches (line number and text,
//! read on the worker). Clicking a hit jumps to it in its tab.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use oxtail_core::Document;
use oxtail_search::{CaseMode, LinePredicate, Matcher, Query, QueryKind, SearchJob, SearchOptions};

/// Matches listed per tab.
pub const HITS_PER_TAB: usize = 100;
/// Matches counted per tab before the scan stops.
pub const MAX_COUNTED: u64 = 1_000_000;

/// One tab to search.
#[derive(Clone)]
pub struct CrossTarget {
    /// The tab.
    pub tab_id: u64,
    /// Its title.
    pub title: String,
    /// Its document.
    pub doc: Arc<Document>,
}

/// One match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Start offset of the line.
    pub offset: u64,
    /// 0-based line number.
    pub line: u64,
    /// The line text.
    pub text: String,
}

/// The results for one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabResult {
    /// The tab.
    pub tab_id: u64,
    /// Its title.
    pub title: String,
    /// Matching lines counted so far.
    pub total: u64,
    /// The first matches.
    pub hits: Vec<Hit>,
    /// This tab is finished.
    pub done: bool,
}

/// What the worker reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrossMsg {
    /// Results for one tab (partial or final).
    Tab(TabResult),
    /// Every tab is finished.
    Finished,
}

/// A running search across tabs. Dropping it stops the worker.
pub struct CrossJob {
    rx: Receiver<CrossMsg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for CrossJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl CrossJob {
    /// Starts searching `targets` for `query`. An invalid query is reported
    /// before anything starts.
    pub fn start(
        targets: Vec<CrossTarget>,
        query: &Query,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<CrossJob, String> {
        let matcher: Arc<dyn LinePredicate> =
            Arc::new(Matcher::compile(query).map_err(|e| e.to_string())?);
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("oxtail-cross-search".into())
            .spawn(move || {
                for t in targets {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    search_tab(&t, &matcher, &flag, &|r| {
                        let _ = tx.send(CrossMsg::Tab(r));
                        wake();
                    });
                }
                let _ = tx.send(CrossMsg::Finished);
                wake();
            });
        if let Err(e) = spawned {
            return Err(format!("cannot start the search thread: {e}"));
        }
        Ok(CrossJob { rx, cancel })
    }

    /// The next message, if any. Never blocks.
    pub fn try_recv(&self) -> Option<CrossMsg> {
        self.rx.try_recv().ok()
    }
}

fn search_tab(
    t: &CrossTarget,
    matcher: &Arc<dyn LinePredicate>,
    cancel: &AtomicBool,
    report: &dyn Fn(TabResult),
) {
    let mut result = TabResult {
        tab_id: t.tab_id,
        title: t.title.clone(),
        total: 0,
        hits: Vec::new(),
        done: false,
    };
    let generation = t.doc.generation();
    let len = t.doc.snapshot().utf8_len;
    let handle = SearchJob::start(
        t.doc.source(),
        len,
        Arc::clone(matcher),
        SearchOptions {
            max_matches: MAX_COUNTED,
            ..SearchOptions::default()
        },
    );
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            handle.cancel();
            return;
        }
        let status = handle.poll();
        result.total = status.matches_found;
        if status.done {
            break;
        }
        if last.elapsed() > Duration::from_millis(250) {
            last = Instant::now();
            report(result.clone());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let set = handle.matches();
    result.total = set.len() as u64;
    for offset in set.iter().take(HITS_PER_TAB) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok((g, pos)) = t.doc.line_of_offset_with_generation(offset) else {
            continue;
        };
        if g != generation {
            break;
        }
        // The line's text (the read starts at the line containing the offset).
        let text = t
            .doc
            .read_lines_blocking(pos.line, 1)
            .into_iter()
            .next()
            .map(|l| l.text)
            .unwrap_or_default();
        result.hits.push(Hit {
            offset: pos.start.min(offset),
            line: pos.line,
            text,
        });
    }
    result.done = true;
    report(result);
}

/// The dialog's state.
pub struct CrossSearchState {
    /// The window is open.
    pub open: bool,
    /// The query.
    pub text: String,
    /// Regular expression.
    pub regex: bool,
    /// Case handling.
    pub case: CaseMode,
    /// Focus the field on the next frame.
    pub focus: bool,
    /// The running search.
    pub job: Option<CrossJob>,
    /// The results, one entry per tab, in tab order.
    pub results: Vec<TabResult>,
    /// The search is still running.
    pub running: bool,
    /// Why the last search could not start.
    pub error: Option<String>,
}

impl Default for CrossSearchState {
    fn default() -> Self {
        Self {
            open: false,
            text: String::new(),
            regex: false,
            case: CaseMode::Smart,
            focus: false,
            job: None,
            results: Vec::new(),
            running: false,
            error: None,
        }
    }
}

impl CrossSearchState {
    /// The query from the fields.
    pub fn query(&self) -> Query {
        Query {
            pattern: self.text.clone(),
            kind: if self.regex {
                QueryKind::Regex
            } else {
                QueryKind::Literal
            },
            case: self.case,
            whole_word: false,
        }
    }

    /// Starts a search over `targets`, replacing a running one.
    pub fn start(&mut self, targets: Vec<CrossTarget>, wake: Arc<dyn Fn() + Send + Sync>) {
        self.job = None;
        self.results.clear();
        self.error = None;
        if self.text.is_empty() {
            self.running = false;
            return;
        }
        match CrossJob::start(targets, &self.query(), wake) {
            Ok(j) => {
                self.job = Some(j);
                self.running = true;
            }
            Err(e) => {
                self.error = Some(e);
                self.running = false;
            }
        }
    }

    /// Applies the worker's messages; returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(msg) = self.job.as_ref().and_then(CrossJob::try_recv) {
            changed = true;
            match msg {
                CrossMsg::Tab(r) => match self.results.iter_mut().find(|x| x.tab_id == r.tab_id) {
                    Some(slot) => *slot = r,
                    None => self.results.push(r),
                },
                CrossMsg::Finished => self.running = false,
            }
        }
        changed
    }

    /// Total matching lines over all tabs.
    pub fn total(&self) -> u64 {
        self.results.iter().map(|r| r.total).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};

    fn target(id: u64, title: &str, text: &str) -> CrossTarget {
        CrossTarget {
            tab_id: id,
            title: title.into(),
            doc: doc(text),
        }
    }

    fn lit(p: &str) -> Query {
        Query {
            pattern: p.into(),
            kind: QueryKind::Literal,
            case: CaseMode::Smart,
            whole_word: false,
        }
    }

    fn run(targets: Vec<CrossTarget>, q: &Query) -> Vec<TabResult> {
        let job = CrossJob::start(targets, q, Arc::new(|| {})).unwrap();
        let mut results: Vec<TabResult> = Vec::new();
        wait("cross search", || {
            while let Some(m) = job.try_recv() {
                match m {
                    CrossMsg::Tab(r) => {
                        results.retain(|x| x.tab_id != r.tab_id);
                        results.push(r);
                    }
                    CrossMsg::Finished => return Some(()),
                }
            }
            None
        });
        results.sort_by_key(|r| r.tab_id);
        results
    }

    #[test]
    fn results_are_grouped_per_tab_with_line_numbers_and_text() {
        let a = "one\nneedle here\ntwo\nanother needle\n";
        let b = "nothing\nat all\n";
        let c = "needle first\nx\n";
        let r = run(
            vec![
                target(1, "a.log", a),
                target(2, "b.log", b),
                target(3, "c.log", c),
            ],
            &lit("needle"),
        );
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|x| x.done));
        assert_eq!((r[0].total, r[1].total, r[2].total), (2, 0, 1));
        assert_eq!(r[0].hits[0].line, 1);
        assert_eq!(r[0].hits[0].text, "needle here");
        assert_eq!(r[0].hits[0].offset, 4);
        assert_eq!(r[0].hits[1].line, 3);
        assert_eq!(r[0].hits[1].text, "another needle");
        assert!(r[1].hits.is_empty());
        assert_eq!(r[2].hits[0].offset, 0);
    }

    #[test]
    fn only_the_first_hits_are_listed_but_all_are_counted() {
        let text: String = (0..500).map(|i| format!("hit {i}\n")).collect();
        let r = run(vec![target(1, "big.log", &text)], &lit("hit"));
        assert_eq!(r[0].total, 500);
        assert_eq!(r[0].hits.len(), HITS_PER_TAB);
        assert_eq!(r[0].hits[99].line, 99);
    }

    #[test]
    fn invalid_queries_are_rejected_up_front() {
        let q = Query {
            pattern: "(".into(),
            kind: QueryKind::Regex,
            case: CaseMode::Smart,
            whole_word: false,
        };
        assert!(CrossJob::start(Vec::new(), &q, Arc::new(|| {})).is_err());
        let mut st = CrossSearchState {
            text: "(".into(),
            regex: true,
            ..CrossSearchState::default()
        };
        st.start(Vec::new(), Arc::new(|| {}));
        assert!(st.error.is_some() && !st.running);
    }

    #[test]
    fn the_state_collects_results_and_totals() {
        let mut st = CrossSearchState {
            text: "needle".into(),
            ..CrossSearchState::default()
        };
        st.start(
            vec![
                target(1, "a", "needle\nneedle\n"),
                target(2, "b", "x\nneedle\n"),
            ],
            Arc::new(|| {}),
        );
        assert!(st.running);
        wait("state", || {
            st.poll();
            (!st.running).then_some(())
        });
        assert_eq!(st.results.len(), 2);
        assert_eq!(st.total(), 3);
        // Empty text starts nothing.
        st.text.clear();
        st.start(Vec::new(), Arc::new(|| {}));
        assert!(st.job.is_none() && st.results.is_empty());
    }

    #[test]
    fn dropping_the_job_stops_the_worker() {
        let job = CrossJob::start(
            vec![target(1, "a", "needle\n")],
            &lit("needle"),
            Arc::new(|| {}),
        )
        .unwrap();
        let flag = Arc::clone(&job.cancel);
        drop(job);
        assert!(flag.load(Ordering::Relaxed));
    }
}
