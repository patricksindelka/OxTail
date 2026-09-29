//! The [`LinePredicate`] trait and stacked filters.

use std::sync::Arc;

use memchr::memchr_iter;

/// Something that can decide whether a line matches.
///
/// Lines are passed without their terminator (`\n` or `\r\n`). Implementors
/// must be cheap to call from many threads at once.
pub trait LinePredicate: Send + Sync {
    /// Returns `true` if `line` (no terminator) matches.
    fn matches(&self, line: &[u8]) -> bool;

    /// Reports every matching line in `buf`, in order, as
    /// `(line_start, line_end)` byte indices into `buf` (`line_end` excludes
    /// the terminator). `buf` holds whole lines separated by `\n`; the last
    /// one may lack its terminator.
    ///
    /// The default implementation calls [`matches`](Self::matches) for every
    /// line. Implementations with a faster whole-buffer scan (literal search)
    /// should override it.
    fn scan_lines(&self, buf: &[u8], sink: &mut dyn FnMut(usize, usize)) {
        scan_each_line(buf, &mut |line| self.matches(line), sink);
    }
}

impl<T: LinePredicate + ?Sized> LinePredicate for Arc<T> {
    fn matches(&self, line: &[u8]) -> bool {
        (**self).matches(line)
    }
    fn scan_lines(&self, buf: &[u8], sink: &mut dyn FnMut(usize, usize)) {
        (**self).scan_lines(buf, sink);
    }
}

/// Strips a `\r` that directly precedes the line end.
pub(crate) fn trim_cr(buf: &[u8], start: usize, end: usize) -> usize {
    if end > start && buf[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

/// Calls `test` for each line of `buf` and reports the ones that pass.
pub(crate) fn scan_each_line(
    buf: &[u8],
    test: &mut dyn FnMut(&[u8]) -> bool,
    sink: &mut dyn FnMut(usize, usize),
) {
    let mut start = 0;
    for nl in memchr_iter(b'\n', buf) {
        let end = trim_cr(buf, start, nl);
        if test(&buf[start..end]) {
            sink(start, end);
        }
        start = nl + 1;
    }
    if start < buf.len() {
        let end = trim_cr(buf, start, buf.len());
        if test(&buf[start..end]) {
            sink(start, end);
        }
    }
}

/// Whether a filter keeps or drops the lines it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    /// A line must match every include filter.
    Include,
    /// A line must match no exclude filter.
    Exclude,
}

/// One entry of a [`FilterStack`].
#[derive(Clone)]
pub struct Filter {
    /// What the filter matches.
    pub predicate: Arc<dyn LinePredicate>,
    /// Include or exclude.
    pub mode: FilterMode,
}

impl Filter {
    /// An include filter.
    pub fn include(predicate: Arc<dyn LinePredicate>) -> Self {
        Self {
            predicate,
            mode: FilterMode::Include,
        }
    }

    /// An exclude filter.
    pub fn exclude(predicate: Arc<dyn LinePredicate>) -> Self {
        Self {
            predicate,
            mode: FilterMode::Exclude,
        }
    }
}

/// Stacked filters plus context lines. A line passes when it matches all
/// include filters (or there are none) and no exclude filter.
///
/// The stack is itself a [`LinePredicate`] (context lines are handled by the
/// [`FilterJob`](crate::FilterJob), not by the predicate).
#[derive(Clone, Default)]
pub struct FilterStack {
    /// The filters, in the order the user stacked them.
    pub filters: Vec<Filter>,
    /// Lines of context shown before each passing line.
    pub context_before: u32,
    /// Lines of context shown after each passing line.
    pub context_after: u32,
}

impl FilterStack {
    /// An empty stack (every line passes).
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a filter and returns the stack (builder style).
    pub fn with(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Sets the context sizes and returns the stack (builder style).
    pub fn with_context(mut self, before: u32, after: u32) -> Self {
        self.context_before = before;
        self.context_after = after;
        self
    }

    fn passes_except(&self, line: &[u8], skip: Option<usize>) -> bool {
        self.filters.iter().enumerate().all(|(i, f)| {
            if Some(i) == skip {
                return true;
            }
            let hit = f.predicate.matches(line);
            match f.mode {
                FilterMode::Include => hit,
                FilterMode::Exclude => !hit,
            }
        })
    }
}

impl LinePredicate for FilterStack {
    fn matches(&self, line: &[u8]) -> bool {
        self.passes_except(line, None)
    }

    fn scan_lines(&self, buf: &[u8], sink: &mut dyn FnMut(usize, usize)) {
        // Let the first include filter find candidates with its fast scan,
        // then check the remaining filters on those lines only.
        let first = self
            .filters
            .iter()
            .position(|f| f.mode == FilterMode::Include);
        match first {
            Some(i) => self.filters[i].predicate.scan_lines(buf, &mut |s, e| {
                if self.passes_except(&buf[s..e], Some(i)) {
                    sink(s, e);
                }
            }),
            None => scan_each_line(buf, &mut |l| self.matches(l), sink),
        }
    }
}

impl std::fmt::Debug for FilterStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterStack")
            .field(
                "filters",
                &self.filters.iter().map(|x| x.mode).collect::<Vec<_>>(),
            )
            .field("context_before", &self.context_before)
            .field("context_after", &self.context_after)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Contains(&'static [u8]);
    impl LinePredicate for Contains {
        fn matches(&self, line: &[u8]) -> bool {
            line.windows(self.0.len()).any(|w| w == self.0)
        }
    }

    fn collect(p: &dyn LinePredicate, buf: &[u8]) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        p.scan_lines(buf, &mut |s, e| v.push((s, e)));
        v
    }

    #[test]
    fn default_scan_handles_crlf_and_unterminated_tail() {
        let got = collect(&Contains(b"b"), b"ab\r\nxx\r\nb");
        assert_eq!(got, vec![(0, 2), (8, 9)]);
    }

    #[test]
    fn stack_semantics() {
        let stack = FilterStack::new()
            .with(Filter::include(Arc::new(Contains(b"ERROR"))))
            .with(Filter::exclude(Arc::new(Contains(b"health"))))
            .with(Filter::include(Arc::new(Contains(b"user=42"))));
        assert!(stack.matches(b"ERROR user=42"));
        assert!(!stack.matches(b"ERROR user=42 health"));
        assert!(!stack.matches(b"ERROR user=7"));
        let buf = b"ERROR user=42\nERROR user=1\nINFO user=42\nERROR health user=42\n";
        assert_eq!(collect(&stack, buf), vec![(0, 13)]);
        assert!(FilterStack::new().matches(b"anything"));
    }

    #[test]
    fn exclude_only_stack() {
        let stack = FilterStack::new().with(Filter::exclude(Arc::new(Contains(b"x"))));
        assert_eq!(collect(&stack, b"a\nx\nb\n"), vec![(0, 1), (4, 5)]);
    }
}
