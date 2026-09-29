//! The filter view (Ctrl+Shift+F): stacked include/exclude filters with
//! context lines, run as a `FilterJob`, plus the "hide" rules of the active
//! profile (folded into the stack as an exclude filter).
//!
//! # Query grammar
//!
//! Filters are stored in sessions and accepted by `--filter` as short strings:
//!
//! ```text
//! ERROR            include lines containing "ERROR" (smart case)
//! -health          exclude lines containing "health"
//! /user=\d+/r      include, regex        (flags after the last '/': r i s w)
//! -/GET \/ping/    exclude, literal text "GET \/ping"
//! ```
//!
//! Flags: `r` regex, `i` ignore case, `s` case sensitive, `w` whole word.
//!
//! A `?` right after the optional `-` marks a *column query* (the small typed
//! language of `oxtail-columns`, e.g. `?level:ERROR status>=500`); its text
//! runs to the end of the string:
//!
//! ```text
//! ?level:(ERROR|FATAL)     include lines matching the query
//! -?path:/health           exclude them
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::ReadAt;
use oxtail_highlight::{CompiledRules, Rule};
use oxtail_search::{
    CaseMode, Filter, FilterJob, FilterMode, FilterStack, LinePredicate, MatchSet, Matcher, Query,
    QueryKind, SearchHandle, SearchOptions, SearchStatus,
};

use crate::find::QueryProblem;
use crate::qfilter::{QueryContext, QueryPredicate, compile_query};

/// How long typing must pause before the filter job restarts.
pub const EDIT_DEBOUNCE: Duration = Duration::from_millis(300);

/// One line of the filter panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEntry {
    /// The pattern.
    pub text: String,
    /// Regular expression instead of literal text.
    pub regex: bool,
    /// Case handling.
    pub case: CaseMode,
    /// Whole words only.
    pub whole_word: bool,
    /// Include (keep matching lines) or exclude (drop them).
    pub include: bool,
    /// Disabled entries are ignored.
    pub enabled: bool,
    /// The text is a column query (`oxtail-columns`), not a literal/regex.
    pub query: bool,
}

impl Default for FilterEntry {
    fn default() -> Self {
        Self {
            text: String::new(),
            regex: false,
            case: CaseMode::Smart,
            whole_word: false,
            include: true,
            enabled: true,
            query: false,
        }
    }
}

impl FilterEntry {
    /// An include filter for `text` (literal, smart case).
    pub fn include(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// A column-query entry (`level:ERROR`).
    pub fn column_query(text: impl Into<String>, include: bool) -> Self {
        Self {
            text: text.into(),
            include,
            query: true,
            ..Self::default()
        }
    }

    /// The search query of this entry.
    pub fn search_query(&self) -> Query {
        Query {
            pattern: self.text.clone(),
            kind: if self.regex {
                QueryKind::Regex
            } else {
                QueryKind::Literal
            },
            case: self.case,
            whole_word: self.whole_word,
        }
    }

    /// Serialises to the query grammar (see the module docs).
    pub fn to_query_string(&self) -> String {
        if self.query {
            let sign = if self.include { "" } else { "-" };
            return format!("{sign}?{}", self.text);
        }
        let mut flags = String::new();
        if self.regex {
            flags.push('r');
        }
        match self.case {
            CaseMode::Smart => {}
            CaseMode::Sensitive => flags.push('s'),
            CaseMode::Insensitive => flags.push('i'),
        }
        if self.whole_word {
            flags.push('w');
        }
        let sign = if self.include { "" } else { "-" };
        let plain = flags.is_empty()
            && !self.text.starts_with(['-', '/', '!', '?'])
            && self.text == self.text.trim();
        if plain {
            format!("{sign}{}", self.text)
        } else {
            format!("{sign}/{}/{flags}", self.text)
        }
    }

    /// Parses the query grammar. Never fails: anything unrecognised is a
    /// plain literal.
    pub fn from_query_string(s: &str) -> FilterEntry {
        let mut e = FilterEntry::default();
        let mut rest = s;
        if let Some(r) = rest.strip_prefix('-').or_else(|| rest.strip_prefix('!')) {
            e.include = false;
            rest = r;
        }
        if let Some(q) = rest.strip_prefix('?') {
            e.query = true;
            e.text = q.to_string();
            return e;
        }
        if let Some(inner) = rest.strip_prefix('/')
            && let Some(close) = inner.rfind('/')
        {
            let flags = &inner[close + 1..];
            if flags.chars().all(|c| matches!(c, 'r' | 'i' | 's' | 'w')) {
                e.text = inner[..close].to_string();
                for c in flags.chars() {
                    match c {
                        'r' => e.regex = true,
                        'i' => e.case = CaseMode::Insensitive,
                        's' => e.case = CaseMode::Sensitive,
                        'w' => e.whole_word = true,
                        _ => {}
                    }
                }
                return e;
            }
        }
        e.text = rest.to_string();
        e
    }
}

/// A `LinePredicate` that is true for lines a hide rule folds away.
struct HidePredicate(Arc<CompiledRules>);

impl LinePredicate for HidePredicate {
    fn matches(&self, line: &[u8]) -> bool {
        let text = String::from_utf8_lossy(line);
        self.0.highlight(&text, None).hidden
    }
}

/// Compiles the hide rules of `rules` into a predicate, if there are any.
pub fn hide_predicate(rules: &[Rule]) -> Option<Arc<dyn LinePredicate>> {
    let hide: Vec<Rule> = rules
        .iter()
        .filter(|r| r.enabled && r.actions.hide)
        .cloned()
        .collect();
    if hide.is_empty() {
        return None;
    }
    let compiled = CompiledRules::compile(&hide).ok()?;
    Some(Arc::new(HidePredicate(Arc::new(compiled))))
}

struct ActiveFilter {
    handle: SearchHandle,
    generation: u64,
    /// Identifies the configuration the job was started for.
    signature: String,
}

/// State of the filter panel and its job.
pub struct FilterState {
    /// The panel is visible.
    pub open: bool,
    /// The stacked filters.
    pub entries: Vec<FilterEntry>,
    /// Context lines before each match.
    pub before: u32,
    /// Context lines after each match.
    pub after: u32,
    /// The user wants the filtered view (when there is anything to filter).
    pub enabled: bool,
    /// Show lines that hide rules would fold away.
    pub show_hidden: bool,
    /// Problems per entry index (invalid regex, ...).
    pub problems: Vec<(usize, QueryProblem)>,
    /// The lines of the view (empty until the job produces some).
    pub set: Arc<MatchSet>,
    /// Latest job status.
    pub status: SearchStatus,
    /// Focus the text field of the last entry on the next frame.
    pub focus_last: bool,
    /// While set and in the future, edits do not restart the job yet
    /// (the user is still typing).
    pub hold_until: Option<Instant>,
    /// Bumped whenever the job restarts.
    pub epoch: u64,
    /// What column queries run against (the tab's parser and time parser).
    columns: Option<Arc<QueryContext>>,
    active: Option<ActiveFilter>,
}

impl Default for FilterState {
    fn default() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            before: 0,
            after: 0,
            enabled: true,
            show_hidden: false,
            problems: Vec::new(),
            set: Arc::new(MatchSet::new()),
            status: SearchStatus {
                progress: 1.0,
                done: true,
                matches_found: 0,
                truncated: false,
                io_errors: 0,
            },
            focus_last: false,
            hold_until: None,
            epoch: 0,
            columns: None,
            active: None,
        }
    }
}

impl FilterState {
    /// Sets what column queries run against; the job restarts when a query
    /// filter is in use.
    pub fn set_columns(&mut self, ctx: Option<Arc<QueryContext>>) {
        self.columns = ctx;
    }

    /// The parser context queries run against.
    pub fn columns(&self) -> Option<&Arc<QueryContext>> {
        self.columns.as_ref()
    }

    /// Problems of one query text (syntax errors, unknown columns), for the
    /// panel; does not depend on the job.
    pub fn check_query(&self, text: &str) -> Vec<QueryProblem> {
        match compile_query(text, self.columns.as_ref().map(|c| c.parser.schema())) {
            Ok(_) => Vec::new(),
            Err(issues) => issues
                .into_iter()
                .map(|i| QueryProblem {
                    message: i.message,
                    span: i.span,
                })
                .collect(),
        }
    }

    /// Builds a stack from the enabled, compilable entries, plus the hide
    /// rules unless `show_hidden`. Returns the stack and per-entry problems.
    pub fn build_stack(
        &self,
        hide: Option<&Arc<dyn LinePredicate>>,
    ) -> (FilterStack, Vec<(usize, QueryProblem)>) {
        let mut stack = FilterStack::new().with_context(self.before, self.after);
        let mut problems = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if !e.enabled || e.text.is_empty() {
                continue;
            }
            if e.query {
                match compile_query(&e.text, self.columns.as_ref().map(|c| c.parser.schema())) {
                    Ok(q) => {
                        let p: Arc<dyn LinePredicate> =
                            Arc::new(QueryPredicate::new(q, self.columns.clone()));
                        stack = stack.with(if e.include {
                            Filter::include(p)
                        } else {
                            Filter::exclude(p)
                        });
                    }
                    Err(issues) => {
                        if let Some(first) = issues.into_iter().next() {
                            problems.push((
                                i,
                                QueryProblem {
                                    message: first.message,
                                    span: first.span,
                                },
                            ));
                        }
                    }
                }
                continue;
            }
            match Matcher::compile(&e.search_query()) {
                Ok(m) => {
                    let p: Arc<dyn LinePredicate> = Arc::new(m);
                    stack = stack.with(if e.include {
                        Filter::include(p)
                    } else {
                        Filter::exclude(p)
                    });
                }
                Err(err) => {
                    let problem = match &err {
                        oxtail_search::SearchError::InvalidRegex { message, span } => {
                            QueryProblem {
                                message: message.clone(),
                                span: span.clone(),
                            }
                        }
                        other => QueryProblem {
                            message: other.to_string(),
                            span: None,
                        },
                    };
                    problems.push((i, problem));
                }
            }
        }
        if !self.show_hidden
            && let Some(h) = hide
        {
            stack = stack.with(Filter::exclude(Arc::clone(h)));
        }
        (stack, problems)
    }

    /// A string that changes whenever the effective configuration does.
    pub fn signature(&self, has_hide: bool) -> String {
        let mut s = format!(
            "{}/{}/{}/{}|",
            self.before,
            self.after,
            self.show_hidden || !has_hide,
            self.enabled
        );
        for e in self
            .entries
            .iter()
            .filter(|e| e.enabled && !e.text.is_empty())
        {
            s.push_str(&e.to_query_string());
            s.push('\u{1}');
        }
        // Column queries depend on the parser (and the time zone).
        if self
            .entries
            .iter()
            .any(|e| e.enabled && e.query && !e.text.is_empty())
        {
            s.push_str(&self.columns.as_ref().map_or(String::new(), |c| c.signature()));
        }
        s
    }

    /// Whether the filtered view is the one to show right now: a job exists
    /// and the user has not switched to the full view.
    pub fn view_active(&self) -> bool {
        self.enabled && self.active.is_some()
    }

    /// Whether a job exists (even when the user switched to the full view).
    pub fn has_job(&self) -> bool {
        self.active.is_some()
    }

    /// Whether `offset` is one of the lines of the current set.
    pub fn contains(&self, offset: u64) -> bool {
        self.set.contains(offset)
    }

    /// The user is typing: restart the job only after [`EDIT_DEBOUNCE`].
    pub fn edited(&mut self, now: Instant) {
        self.hold_until = Some(now + EDIT_DEBOUNCE);
    }

    /// The configuration changed in a discrete way (toggle, add, remove):
    /// restart right away.
    pub fn changed(&mut self) {
        self.hold_until = None;
    }

    /// Stops the job.
    pub fn cancel(&mut self) {
        self.active = None;
        self.set = Arc::new(MatchSet::new());
    }

    /// Advances the job: (re)starts it when the configuration or the
    /// document generation changed, extends it as the document grows and
    /// refreshes the status and the set. Call once per frame.
    pub fn tick(
        &mut self,
        now: Instant,
        source: &Arc<dyn ReadAt>,
        utf8_len: u64,
        generation: u64,
        hint: u64,
        hide: Option<&Arc<dyn LinePredicate>>,
    ) {
        if !self.wants_job(hide.is_some()) {
            if self.active.is_some() {
                self.cancel();
            }
            return;
        }
        let signature = self.signature(hide.is_some());
        let stale = self
            .active
            .as_ref()
            .is_none_or(|a| a.generation != generation || a.signature != signature);
        let held = self.hold_until.is_some_and(|t| now < t);
        if stale && !held {
            self.hold_until = None;
            let (stack, problems) = self.build_stack(hide);
            self.problems = problems;
            self.epoch += 1;
            if stack.filters.is_empty() {
                // Nothing valid to filter by.
                self.cancel();
                return;
            }
            let handle = FilterJob::start(
                Arc::clone(source),
                utf8_len,
                stack,
                SearchOptions {
                    viewport_hint: hint,
                    ..SearchOptions::default()
                },
            );
            self.active = Some(ActiveFilter {
                handle,
                generation,
                signature,
            });
        }
        if let Some(a) = &self.active {
            if utf8_len > a.handle.end() {
                a.handle.extend(utf8_len);
            }
            self.status = a.handle.poll();
            self.set = a.handle.matches();
        }
    }

    /// Whether a job should exist: there is an enabled filter, or hide rules
    /// apply.
    fn wants_job(&self, has_hide: bool) -> bool {
        let user = self.entries.iter().any(|e| e.enabled && !e.text.is_empty());
        user || (has_hide && !self.show_hidden)
    }

    /// The entries as query strings (for the session file).
    pub fn to_query_strings(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| !e.text.is_empty())
            .map(FilterEntry::to_query_string)
            .collect()
    }

    /// Replaces the entries from query strings.
    pub fn set_from_query_strings(&mut self, queries: &[String]) {
        self.entries = queries
            .iter()
            .map(|q| FilterEntry::from_query_string(q))
            .collect();
        self.hold_until = None;
    }
}

/// Whether the mode is an include filter.
pub fn is_include(mode: FilterMode) -> bool {
    mode == FilterMode::Include
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // tests build states field by field for readability
mod tests {
    use super::*;
    use oxtail_core::MemSource;
    use oxtail_highlight::{RuleActions, RuleMatcher};
    use std::time::{Duration, Instant};

    fn text() -> (Arc<dyn ReadAt>, u64) {
        let t = "INFO start\nERROR db timeout\nINFO health ok\nERROR user=42 failed\nWARN slow\nERROR health down\n";
        (
            Arc::new(MemSource::new(t.as_bytes().to_vec())),
            t.len() as u64,
        )
    }

    fn offset_of(t: &str, needle: &str) -> u64 {
        t.find(needle).unwrap() as u64
    }

    fn wait_done(f: &mut FilterState, src: &Arc<dyn ReadAt>, len: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            f.tick(Instant::now(), src, len, 0, 0, None);
            if f.status.done && f.has_job() {
                return;
            }
            assert!(Instant::now() < deadline, "filter did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn query_grammar_round_trips() {
        for e in [
            FilterEntry::include("ERROR"),
            FilterEntry {
                include: false,
                ..FilterEntry::include("health")
            },
            FilterEntry {
                regex: true,
                ..FilterEntry::include(r"user=\d+")
            },
            FilterEntry {
                include: false,
                case: CaseMode::Sensitive,
                whole_word: true,
                ..FilterEntry::include("a/b")
            },
            FilterEntry {
                case: CaseMode::Insensitive,
                ..FilterEntry::include("-dash")
            },
            FilterEntry::include("/slashy/"),
            FilterEntry::include(" padded "),
        ] {
            let s = e.to_query_string();
            let back = FilterEntry::from_query_string(&s);
            assert_eq!(back, e, "{s}");
        }
    }

    #[test]
    fn parsing_is_lenient() {
        let e = FilterEntry::from_query_string("plain text");
        assert_eq!(e, FilterEntry::include("plain text"));
        let e = FilterEntry::from_query_string("/unclosed");
        assert_eq!(e.text, "/unclosed");
        let e = FilterEntry::from_query_string("/x/zz");
        assert_eq!(e.text, "/x/zz");
        let e = FilterEntry::from_query_string("!bang");
        assert!(!e.include);
        assert_eq!(e.text, "bang");
        let e = FilterEntry::from_query_string("");
        assert_eq!(e.text, "");
    }

    #[test]
    fn stack_semantics_match_the_panel() {
        let (src, len) = text();
        let mut f = FilterState::default();
        f.entries = vec![
            FilterEntry::include("ERROR"),
            FilterEntry {
                include: false,
                ..FilterEntry::include("health")
            },
        ];
        f.changed();
        wait_done(&mut f, &src, len);
        let t = "INFO start\nERROR db timeout\nINFO health ok\nERROR user=42 failed\nWARN slow\nERROR health down\n";
        let got: Vec<u64> = f.set.iter().collect();
        assert_eq!(
            got,
            vec![offset_of(t, "ERROR db"), offset_of(t, "ERROR user")]
        );
        assert!(f.view_active());
    }

    #[test]
    fn context_lines_are_marked() {
        let (src, len) = text();
        let mut f = FilterState::default();
        f.entries = vec![FilterEntry::include("WARN")];
        f.before = 1;
        f.after = 1;
        f.changed();
        wait_done(&mut f, &src, len);
        assert_eq!(f.set.len(), 3);
        let flags: Vec<bool> = (0..3).map(|i| f.set.is_context(i)).collect();
        assert_eq!(flags, vec![true, false, true]);
    }

    #[test]
    fn invalid_regex_entries_are_reported_and_skipped() {
        let (src, len) = text();
        let mut f = FilterState::default();
        f.entries = vec![
            FilterEntry {
                regex: true,
                ..FilterEntry::include("(bad")
            },
            FilterEntry::include("WARN"),
        ];
        f.changed();
        wait_done(&mut f, &src, len);
        assert_eq!(f.problems.len(), 1);
        assert_eq!(f.problems[0].0, 0);
        assert_eq!(f.set.len(), 1);
    }

    #[test]
    fn clearing_the_filters_stops_the_job() {
        let (src, len) = text();
        let mut f = FilterState::default();
        f.entries = vec![FilterEntry::include("WARN")];
        f.changed();
        wait_done(&mut f, &src, len);
        f.entries.clear();
        f.changed();
        f.tick(Instant::now(), &src, len, 0, 0, None);
        assert!(!f.has_job());
        assert!(f.set.is_empty());
    }

    #[test]
    fn editing_restarts_the_job() {
        let (src, len) = text();
        let mut f = FilterState::default();
        f.entries = vec![FilterEntry::include("WARN")];
        wait_done(&mut f, &src, len);
        let epoch = f.epoch;
        f.entries[0].text = "INFO".into();
        f.changed();
        wait_done(&mut f, &src, len);
        assert!(f.epoch > epoch);
        assert_eq!(f.set.len(), 2);
    }

    #[test]
    fn hide_rules_become_an_exclude_filter() {
        let (src, len) = text();
        let mut rule = Rule::new(
            "health",
            RuleMatcher::Literal {
                text: "health".into(),
                case_sensitive: false,
            },
        );
        rule.actions = RuleActions {
            hide: true,
            ..RuleActions::default()
        };
        let hide = hide_predicate(&[rule.clone()]).expect("hide predicate");
        assert!(hide.matches(b"INFO health ok"));
        assert!(!hide.matches(b"INFO start"));
        let mut f = FilterState::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            f.tick(Instant::now(), &src, len, 0, 0, Some(&hide));
            if f.status.done && f.has_job() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(f.set.len(), 4);
        // "Show hidden" drops the job again.
        f.show_hidden = true;
        f.tick(Instant::now(), &src, len, 0, 0, Some(&hide));
        assert!(!f.has_job());
        // Rules without the hide action produce no predicate.
        rule.actions.hide = false;
        assert!(hide_predicate(&[rule]).is_none());
    }

    #[test]
    fn growth_extends_the_set() {
        let mem = Arc::new(MemSource::new(b"a WARN\nb\n".to_vec()));
        let src: Arc<dyn ReadAt> = mem.clone();
        let mut f = FilterState::default();
        f.entries = vec![FilterEntry::include("WARN")];
        wait_done(&mut f, &src, 9);
        assert_eq!(f.set.len(), 1);
        mem.append(b"c WARN\n");
        let len = mem.len().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.set.len() < 2 {
            f.tick(Instant::now(), &src, len, 0, 0, None);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn query_strings_for_the_session() {
        let mut f = FilterState::default();
        f.entries = vec![
            FilterEntry::include("a"),
            FilterEntry::default(),
            FilterEntry {
                include: false,
                ..FilterEntry::include("b")
            },
        ];
        assert_eq!(f.to_query_strings(), vec!["a", "-b"]);
        let mut g = FilterState::default();
        g.set_from_query_strings(&f.to_query_strings());
        assert_eq!(g.entries.len(), 2);
        assert!(is_include(FilterMode::Include));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn entry() -> impl Strategy<Value = FilterEntry> {
            (
                "[ -~]{0,12}",
                any::<bool>(),
                0u8..3,
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
            )
                .prop_map(|(text, regex, case, whole_word, include, query)| {
                    if query {
                        return FilterEntry::column_query(text, include);
                    }
                    FilterEntry {
                        text,
                        regex,
                        case: match case {
                            0 => CaseMode::Smart,
                            1 => CaseMode::Sensitive,
                            _ => CaseMode::Insensitive,
                        },
                        whole_word,
                        include,
                        enabled: true,
                        query: false,
                    }
                })
        }

        proptest! {
            /// Every entry survives the query grammar.
            #[test]
            fn query_strings_round_trip(e in entry()) {
                let s = e.to_query_string();
                prop_assert_eq!(FilterEntry::from_query_string(&s), e);
            }

            /// Parsing arbitrary text never panics.
            #[test]
            fn parsing_never_panics(s in "\\PC{0,30}") {
                let _ = FilterEntry::from_query_string(&s);
            }
        }
    }
}
