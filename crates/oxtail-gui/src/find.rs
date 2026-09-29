//! Find (Ctrl+F): the query, the background search job and next/previous
//! navigation. Pure state machine plus calls into `oxtail-search`; no egui.
//!
//! * Typing restarts the job after a short debounce ([`DEBOUNCE`]); the job
//!   starts scanning at the byte offset of the view's top line so the first
//!   hit near the viewport arrives first.
//! * Next/previous ask the running job (`next_after` / `prev_before`); while
//!   the chunks that decide the answer are not scanned yet the lookup is
//!   `Pending` and is retried on the next frame.
//! * When the document grows, the job is extended; on truncation or rotation
//!   (a new generation) it is restarted.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::ReadAt;
use oxtail_search::{
    CaseMode, Lookup, MatchSet, Matcher, Query, QueryKind, SearchError, SearchHandle, SearchJob,
    SearchOptions, SearchStatus,
};

/// How long typing must pause before the search restarts.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Maximum number of remembered queries.
pub const HISTORY_LIMIT: usize = 50;

/// Direction of a next/previous request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Towards the end of the file.
    Next,
    /// Towards the start.
    Prev,
}

/// A query that failed to compile, for inline display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryProblem {
    /// What is wrong.
    pub message: String,
    /// Where in the pattern, when known.
    pub span: Option<Range<usize>>,
}

impl QueryProblem {
    fn from_error(e: &SearchError) -> Self {
        match e {
            SearchError::InvalidRegex { message, span } => Self {
                message: message.clone(),
                span: span.clone(),
            },
            other => Self {
                message: other.to_string(),
                span: None,
            },
        }
    }
}

struct ActiveSearch {
    handle: SearchHandle,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nav {
    /// The first match at or after `from` (incremental search while typing).
    First { from: u64 },
    /// One step from `from`.
    Step {
        dir: Dir,
        from: u64,
        wrapped: bool,
        /// Give up wrapping (used when searching from the very start/end).
        can_wrap: bool,
    },
}

/// What the view should do after a [`FindState::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindEvent {
    /// Scroll to the line starting at this offset.
    Jump(u64),
    /// There is no match at all in the scanned range.
    NoMatch,
}

/// State of the find bar and its search.
pub struct FindState {
    /// The bar is visible.
    pub open: bool,
    /// Ask the text field to take keyboard focus on the next frame.
    pub focus: bool,
    /// The pattern as typed.
    pub text: String,
    /// Treat the pattern as a regular expression.
    pub regex: bool,
    /// Case handling (smart by default).
    pub case: CaseMode,
    /// Whole words only.
    pub whole_word: bool,
    /// Why the current pattern does not compile.
    pub problem: Option<QueryProblem>,
    /// The compiled matcher (for painting matches).
    pub matcher: Option<Arc<Matcher>>,
    /// The offset of the line holding the current match.
    pub current: Option<u64>,
    /// Latest job status.
    pub status: SearchStatus,
    /// Everything found so far.
    pub matches: Arc<MatchSet>,
    /// A step is waiting for the scan to reach the answer.
    pub searching: bool,
    /// Bumped whenever the query changes (invalidates painted matches).
    pub epoch: u64,
    active: Option<ActiveSearch>,
    debounce_until: Option<Instant>,
    nav: Option<Nav>,
    hint: u64,
    /// Index into the history while browsing with Up/Down.
    pub history_pos: Option<usize>,
}

impl Default for FindState {
    fn default() -> Self {
        Self {
            open: false,
            focus: false,
            text: String::new(),
            regex: false,
            case: CaseMode::Smart,
            whole_word: false,
            problem: None,
            matcher: None,
            current: None,
            status: idle_status(),
            matches: Arc::new(MatchSet::new()),
            searching: false,
            epoch: 0,
            active: None,
            debounce_until: None,
            nav: None,
            hint: 0,
            history_pos: None,
        }
    }
}

fn idle_status() -> SearchStatus {
    SearchStatus {
        progress: 1.0,
        done: true,
        matches_found: 0,
        truncated: false,
        io_errors: 0,
    }
}

impl FindState {
    /// The query for the current controls, if the pattern is not empty.
    pub fn query(&self) -> Option<Query> {
        if self.text.is_empty() {
            return None;
        }
        Some(Query {
            pattern: self.text.clone(),
            kind: if self.regex {
                QueryKind::Regex
            } else {
                QueryKind::Literal
            },
            case: self.case,
            whole_word: self.whole_word,
        })
    }

    /// Called when the user edited the pattern or a toggle: restarts the
    /// search after the debounce.
    pub fn query_changed(&mut self, now: Instant) {
        self.debounce_until = Some(now + DEBOUNCE);
        self.history_pos = None;
    }

    /// Restarts the search immediately (e.g. after a generation change).
    pub fn restart_now(&mut self, now: Instant) {
        self.debounce_until = Some(now);
    }

    /// The remaining debounce time, so the caller can schedule a repaint.
    pub fn debounce_remaining(&self, now: Instant) -> Option<Duration> {
        self.debounce_until
            .map(|t| t.saturating_duration_since(now))
    }

    /// Whether a search job exists.
    pub fn is_running(&self) -> bool {
        self.active.is_some()
    }

    /// Stops the job and forgets results (keeps the query text).
    pub fn cancel(&mut self) {
        self.active = None;
        self.matches = Arc::new(MatchSet::new());
        self.status = idle_status();
        self.current = None;
        self.nav = None;
        self.searching = false;
    }

    /// Asks for the next/previous match relative to `from` (the current
    /// match if there is one, else the top of the view).
    pub fn step(&mut self, dir: Dir, top_offset: u64) {
        if self.text.is_empty() {
            return;
        }
        let from = self.current.unwrap_or(match dir {
            // With no current match, "next" includes the top line and
            // "previous" excludes it.
            Dir::Next => top_offset.saturating_sub(1),
            Dir::Prev => top_offset,
        });
        let first_line_counts = self.current.is_none() && dir == Dir::Next && top_offset == 0;
        self.nav = Some(if first_line_counts {
            Nav::First { from: 0 }
        } else {
            Nav::Step {
                dir,
                from,
                wrapped: false,
                can_wrap: true,
            }
        });
        self.searching = true;
    }

    /// Updates the viewport hint (byte offset of the top line).
    pub fn set_hint(&mut self, offset: u64) {
        if offset.abs_diff(self.hint) > 64 * 1024 {
            self.hint = offset;
            if let Some(a) = &self.active {
                a.handle.set_viewport_hint(offset);
            }
        }
    }

    /// Advances the state machine. Call once per frame.
    ///
    /// * `source`, `utf8_len`, `generation`: the document's current state.
    /// * `top_offset`: start offset of the view's top line.
    /// * `accept`: whether a match at this offset may be jumped to (a filter
    ///   view only shows some lines).
    pub fn tick(
        &mut self,
        now: Instant,
        source: &Arc<dyn ReadAt>,
        utf8_len: u64,
        generation: u64,
        top_offset: u64,
        accept: &dyn Fn(u64) -> bool,
    ) -> Option<FindEvent> {
        // Restart when the generation moved on.
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.generation != generation)
        {
            self.active = None;
            self.current = None;
            self.debounce_until.get_or_insert(now);
        }
        if let Some(t) = self.debounce_until
            && now >= t
        {
            self.debounce_until = None;
            self.restart(source, utf8_len, generation, top_offset);
        }
        let a = self.active.as_ref()?;
        if utf8_len > a.handle.end() {
            a.handle.extend(utf8_len);
        }
        self.status = a.handle.poll();
        self.matches = a.handle.matches();
        self.resolve_nav(accept)
    }

    fn restart(&mut self, source: &Arc<dyn ReadAt>, end: u64, generation: u64, top: u64) {
        self.epoch += 1;
        self.active = None;
        self.matches = Arc::new(MatchSet::new());
        self.status = idle_status();
        self.matcher = None;
        self.problem = None;
        self.current = None;
        self.searching = false;
        let Some(query) = self.query() else {
            self.nav = None;
            return;
        };
        match Matcher::compile(&query) {
            Ok(m) => {
                let m = Arc::new(m);
                self.matcher = Some(Arc::clone(&m));
                self.hint = top;
                let handle = SearchJob::start(
                    Arc::clone(source),
                    end,
                    m,
                    SearchOptions {
                        viewport_hint: top,
                        ..SearchOptions::default()
                    },
                );
                self.active = Some(ActiveSearch { handle, generation });
                self.nav = Some(Nav::First { from: top });
                self.searching = true;
            }
            Err(e) => {
                self.problem = Some(QueryProblem::from_error(&e));
                self.nav = None;
            }
        }
    }

    fn lookup(&self, nav: Nav) -> Lookup {
        let Some(a) = &self.active else {
            return Lookup::None;
        };
        match nav {
            Nav::First { from } => {
                if self.matches.contains(from) {
                    Lookup::Found(from)
                } else {
                    a.handle.next_after(from)
                }
            }
            Nav::Step {
                dir: Dir::Next,
                from,
                ..
            } => a.handle.next_after(from),
            Nav::Step {
                dir: Dir::Prev,
                from,
                ..
            } => a.handle.prev_before(from),
        }
    }

    fn resolve_nav(&mut self, accept: &dyn Fn(u64) -> bool) -> Option<FindEvent> {
        let mut nav = self.nav?;
        // A filter view may reject matches; skip a bounded number per frame.
        for _ in 0..2000 {
            match self.lookup(nav) {
                Lookup::Found(o) if accept(o) => {
                    self.current = Some(o);
                    self.nav = None;
                    self.searching = false;
                    return Some(FindEvent::Jump(o));
                }
                Lookup::Found(o) => {
                    nav = match nav {
                        Nav::First { .. } => Nav::Step {
                            dir: Dir::Next,
                            from: o,
                            wrapped: false,
                            can_wrap: true,
                        },
                        Nav::Step {
                            dir,
                            wrapped,
                            can_wrap,
                            ..
                        } => Nav::Step {
                            dir,
                            from: o,
                            wrapped,
                            can_wrap,
                        },
                    };
                    self.nav = Some(nav);
                }
                Lookup::Pending => {
                    self.nav = Some(nav);
                    self.searching = true;
                    return None;
                }
                Lookup::None => {
                    // Wrap around once.
                    let (dir, wrapped, can_wrap) = match nav {
                        Nav::First { .. } => (Dir::Next, false, true),
                        Nav::Step {
                            dir,
                            wrapped,
                            can_wrap,
                            ..
                        } => (dir, wrapped, can_wrap),
                    };
                    if wrapped || !can_wrap {
                        self.nav = None;
                        self.searching = false;
                        return Some(FindEvent::NoMatch);
                    }
                    let from = match dir {
                        Dir::Next => {
                            // The very first line may itself match.
                            if self.matches.contains(0) && accept(0) {
                                self.current = Some(0);
                                self.nav = None;
                                self.searching = false;
                                return Some(FindEvent::Jump(0));
                            }
                            0
                        }
                        Dir::Prev => u64::MAX,
                    };
                    nav = Nav::Step {
                        dir,
                        from,
                        wrapped: true,
                        can_wrap: true,
                    };
                    self.nav = Some(nav);
                }
            }
        }
        // Too many rejected candidates for one frame: continue next frame.
        self.nav = Some(nav);
        None
    }

    /// The `n` in "n of m": the rank of the current match among the matches
    /// found so far, if the current match is known.
    pub fn current_rank(&self) -> Option<usize> {
        let cur = self.current?;
        self.matches
            .contains(cur)
            .then(|| self.matches.rank(cur) + 1)
    }

    /// Match ranges to paint in `text` (a line's displayed text).
    pub fn ranges_in(&self, text: &str) -> Vec<Range<usize>> {
        match &self.matcher {
            Some(m) => m.find_iter(text.as_bytes()).collect(),
            None => Vec::new(),
        }
    }
}

/// Adds `query` to the front of `history` (no duplicates, bounded).
pub fn push_history(history: &mut Vec<String>, query: &str) {
    let q = query.trim();
    if q.is_empty() {
        return;
    }
    history.retain(|h| h != q);
    history.insert(0, q.to_string());
    history.truncate(HISTORY_LIMIT);
}

/// Steps through the history with Up (`older = true`) / Down. Returns the
/// text to show; `None` past the newest entry means "the text typed before".
pub fn history_step(
    history: &[String],
    pos: Option<usize>,
    older: bool,
) -> (Option<usize>, Option<String>) {
    if history.is_empty() {
        return (None, None);
    }
    let next = match (pos, older) {
        (None, true) => Some(0),
        (None, false) => None,
        (Some(i), true) => Some((i + 1).min(history.len() - 1)),
        (Some(0), false) => None,
        (Some(i), false) => Some(i - 1),
    };
    let text = next.and_then(|i| history.get(i).cloned());
    (next, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_core::MemSource;

    fn source(text: &str) -> (Arc<dyn ReadAt>, u64) {
        (
            Arc::new(MemSource::new(text.as_bytes().to_vec())),
            text.len() as u64,
        )
    }

    /// Ticks until the nav resolves (max 10 s).
    fn run(f: &mut FindState, src: &Arc<dyn ReadAt>, len: u64, top: u64) -> Option<FindEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(ev) = f.tick(Instant::now(), src, len, 0, top, &|_| true) {
                return Some(ev);
            }
            if !f.searching && f.debounce_until.is_none() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("search did not finish");
    }

    const TEXT: &str = "alpha\nbeta needle\ngamma\ndelta needle\nepsilon\n";

    fn offsets() -> (u64, u64) {
        (
            TEXT.find("beta").unwrap() as u64,
            TEXT.find("delta").unwrap() as u64,
        )
    }

    #[test]
    fn typing_finds_the_first_match_from_the_top() {
        let (src, len) = source(TEXT);
        let mut f = FindState::default();
        f.text = "needle".into();
        f.query_changed(Instant::now());
        // Not started before the debounce elapsed.
        assert!(f.tick(Instant::now(), &src, len, 0, 0, &|_| true).is_none());
        assert!(!f.is_running());
        std::thread::sleep(DEBOUNCE + Duration::from_millis(20));
        let (b, _) = offsets();
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::Jump(b)));
        assert_eq!(f.current, Some(b));
        assert!(f.problem.is_none());
    }

    #[test]
    fn next_and_previous_walk_and_wrap() {
        let (src, len) = source(TEXT);
        let (b, d) = offsets();
        let mut f = FindState::default();
        f.text = "needle".into();
        f.restart_now(Instant::now());
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::Jump(b)));
        f.step(Dir::Next, 0);
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::Jump(d)));
        // Wraps to the first.
        f.step(Dir::Next, 0);
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::Jump(b)));
        // Previous from the first wraps to the last.
        f.step(Dir::Prev, 0);
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::Jump(d)));
        f.step(Dir::Prev, 0);
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::Jump(b)));
    }

    #[test]
    fn counts_come_from_the_job() {
        let (src, len) = source(TEXT);
        let mut f = FindState::default();
        f.text = "needle".into();
        f.restart_now(Instant::now());
        run(&mut f, &src, len, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f.status.done && Instant::now() < deadline {
            f.tick(Instant::now(), &src, len, 0, 0, &|_| true);
        }
        assert_eq!(f.status.matches_found, 2);
        assert_eq!(f.matches.len(), 2);
        assert_eq!(f.current_rank(), Some(1));
    }

    #[test]
    fn no_match_is_reported() {
        let (src, len) = source(TEXT);
        let mut f = FindState::default();
        f.text = "zzz".into();
        f.restart_now(Instant::now());
        assert_eq!(run(&mut f, &src, len, 0), Some(FindEvent::NoMatch));
        assert_eq!(f.current, None);
    }

    #[test]
    fn invalid_regex_is_reported_inline() {
        let (src, len) = source(TEXT);
        let mut f = FindState::default();
        f.text = "(unclosed".into();
        f.regex = true;
        f.restart_now(Instant::now());
        assert!(f.tick(Instant::now(), &src, len, 0, 0, &|_| true).is_none());
        let p = f.problem.clone().expect("problem");
        assert!(!p.message.is_empty());
        assert!(!f.is_running());
        // Fixing the pattern clears the problem.
        f.text = "(closed)".into();
        f.restart_now(Instant::now());
        f.tick(Instant::now(), &src, len, 0, 0, &|_| true);
        assert!(f.problem.is_none());
    }

    #[test]
    fn a_rejecting_filter_skips_matches() {
        let (src, len) = source(TEXT);
        let (b, d) = offsets();
        let mut f = FindState::default();
        f.text = "needle".into();
        f.restart_now(Instant::now());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = None;
        while got.is_none() && Instant::now() < deadline {
            got = f.tick(Instant::now(), &src, len, 0, 0, &|o| o != b);
        }
        assert_eq!(got, Some(FindEvent::Jump(d)));
    }

    #[test]
    fn a_new_generation_restarts_the_search() {
        let (src, len) = source(TEXT);
        let mut f = FindState::default();
        f.text = "needle".into();
        f.restart_now(Instant::now());
        run(&mut f, &src, len, 0);
        let epoch = f.epoch;
        f.tick(Instant::now(), &src, len, 1, 0, &|_| true);
        assert!(f.epoch > epoch);
        assert!(f.is_running());
    }

    #[test]
    fn growth_extends_the_job() {
        let mem = Arc::new(MemSource::new(b"one needle\n".to_vec()));
        let src: Arc<dyn ReadAt> = mem.clone();
        let mut f = FindState::default();
        f.text = "needle".into();
        f.restart_now(Instant::now());
        run(&mut f, &src, 11, 0);
        mem.append(b"two\nthree needle\n");
        let len = mem.len().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.matches.len() < 2 && Instant::now() < deadline {
            f.tick(Instant::now(), &src, len, 0, 0, &|_| true);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(f.matches.len(), 2);
    }

    #[test]
    fn painted_ranges_follow_the_matcher() {
        let (src, len) = source(TEXT);
        let mut f = FindState::default();
        f.text = "ee".into();
        f.restart_now(Instant::now());
        f.tick(Instant::now(), &src, len, 0, 0, &|_| true);
        assert_eq!(f.ranges_in("beet"), vec![1..3]);
        assert!(FindState::default().ranges_in("beet").is_empty());
    }

    #[test]
    fn history_is_bounded_and_deduplicated() {
        let mut h = Vec::new();
        for i in 0..70 {
            push_history(&mut h, &format!("q{i}"));
        }
        assert_eq!(h.len(), HISTORY_LIMIT);
        assert_eq!(h[0], "q69");
        push_history(&mut h, "q60");
        assert_eq!(h[0], "q60");
        assert_eq!(h.iter().filter(|x| *x == "q60").count(), 1);
        push_history(&mut h, "  ");
        assert_eq!(h.len(), HISTORY_LIMIT);
    }

    #[test]
    fn history_navigation() {
        let h = vec!["new".to_string(), "mid".to_string(), "old".to_string()];
        let (p, t) = history_step(&h, None, true);
        assert_eq!((p, t.as_deref()), (Some(0), Some("new")));
        let (p, t) = history_step(&h, p, true);
        assert_eq!((p, t.as_deref()), (Some(1), Some("mid")));
        let (p, _) = history_step(&h, Some(2), true);
        assert_eq!(p, Some(2));
        let (p, t) = history_step(&h, Some(1), false);
        assert_eq!((p, t.as_deref()), (Some(0), Some("new")));
        let (p, t) = history_step(&h, Some(0), false);
        assert_eq!((p, t), (None, None));
        assert_eq!(history_step(&[], None, true), (None, None));
    }
}
