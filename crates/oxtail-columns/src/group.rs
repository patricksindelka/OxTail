//! Multi-line records: deciding whether a line starts a new record or
//! continues the previous one (stack traces, wrapped messages).

use std::sync::OnceLock;

use regex::Regex;

use crate::error::ColumnsError;
use crate::parsers::Parser;

/// Decides, line by line, whether a line starts a new record.
///
/// A line that does not start a record is a *continuation* and belongs to the
/// previous record (it spans all columns in the UI).
#[derive(Debug, Clone)]
pub struct RecordGrouper {
    mode: Mode,
}

#[derive(Debug, Clone)]
enum Mode {
    /// A line starts a record iff the parser matches it.
    Parser(Box<Parser>),
    /// A line starts a record iff the regex matches it.
    Start(Regex),
    /// A line starts a record iff it begins like a timestamped log line and
    /// does not look like a continuation.
    Heuristic,
}

fn timestamp_start() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // INVARIANT: constant pattern, exercised by tests.
        Regex::new(
            r"^(?:\[?\d{4}[-/.]\d{2}[-/.]\d{2}|\[?\d{2}[-/.]\d{2}[-/.]\d{2,4}|\[?\d{2}:\d{2}:\d{2}|\[?\d{10,13}\b|<\d{1,3}>|[A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}|\{)",
        )
        .expect("constant pattern is valid")
    })
}

impl RecordGrouper {
    /// Records are exactly the lines the parser accepts.
    pub fn from_parser(parser: &Parser) -> Self {
        RecordGrouper {
            mode: Mode::Parser(Box::new(parser.clone())),
        }
    }

    /// Records start at lines matching `pattern` (for example
    /// `^\d{4}-\d{2}-\d{2}`).
    pub fn with_start_pattern(pattern: &str) -> Result<Self, ColumnsError> {
        Ok(RecordGrouper {
            mode: Mode::Start(Regex::new(pattern)?),
        })
    }

    /// Timestamp-prefix heuristic for text logs with no parser.
    pub fn heuristic() -> Self {
        RecordGrouper {
            mode: Mode::Heuristic,
        }
    }

    /// True if `line` starts a new record; false if it continues the previous one.
    pub fn is_record_start(&self, line: &str) -> bool {
        match &self.mode {
            Mode::Parser(p) => p.is_match(line),
            Mode::Start(re) => re.is_match(line),
            Mode::Heuristic => {
                !Self::looks_like_continuation(line) && timestamp_start().is_match(line)
            }
        }
    }

    /// Heuristic for typical continuation lines: blank or indented lines,
    /// Java/Python stack frames, `Caused by:`, `... 12 more`.
    pub fn looks_like_continuation(line: &str) -> bool {
        let Some(first) = line.chars().next() else {
            return true;
        };
        if first == ' ' || first == '\t' {
            return true;
        }
        line.starts_with("at ")
            || line.starts_with("Caused by:")
            || line.starts_with("Suppressed:")
            || line.starts_with("Traceback (most recent call last)")
            || line.starts_with("File \"")
            || line.starts_with("... ")
            || line.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::ParserSpec;

    #[test]
    fn heuristic_groups_stack_traces() {
        let g = RecordGrouper::heuristic();
        let lines = [
            ("2026-09-29 10:00:00 ERROR boom", true),
            ("java.lang.IllegalStateException: bad", false),
            ("\tat com.example.Foo.bar(Foo.java:12)", false),
            ("Caused by: java.io.IOException", false),
            ("\t... 12 more", false),
            ("", false),
            ("2026-09-29 10:00:01 INFO next", true),
            ("[2026-09-29 10:00:01] INFO next", true),
            ("Oct 11 22:14:15 host x: y", true),
            ("{\"a\":1}", true),
        ];
        for (l, expect) in lines {
            assert_eq!(g.is_record_start(l), expect, "{l:?}");
        }
    }

    #[test]
    fn parser_and_pattern_modes() {
        let p = ParserSpec::Regex {
            pattern: r"^(?P<ts>\d{4}-\d\d-\d\d) (?P<msg>.*)$".into(),
            kinds: Default::default(),
        }
        .compile()
        .unwrap();
        let g = RecordGrouper::from_parser(&p);
        assert!(g.is_record_start("2026-01-01 hello"));
        assert!(!g.is_record_start("  at foo"));
        let g = RecordGrouper::with_start_pattern(r"^\[").unwrap();
        assert!(g.is_record_start("[x] y"));
        assert!(!g.is_record_start("y"));
        assert!(RecordGrouper::with_start_pattern("(").is_err());
    }
}
