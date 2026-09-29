//! Query compilation and the line-level [`Matcher`].
//!
//! # Engines
//!
//! * Case-sensitive literal: `memchr::memmem` (SIMD).
//! * ASCII case-insensitive literal, or many literals: `aho-corasick`.
//! * Non-ASCII case-insensitive literal: a `regex` of the escaped literal
//!   (Unicode case folding).
//! * Regex: `regex::bytes`. Its meta engine extracts required literals and
//!   uses memchr/Teddy prefilters, so candidate lines are found quickly.
//!
//! Chunks are scanned as a whole ([`LinePredicate::scan_lines`]): the engine
//! finds a candidate anywhere in the buffer, the surrounding line is located,
//! and the candidate is verified on that single line. That verification is
//! what guarantees a regex never matches across `\n` (a class like `\s` or
//! `[^x]` could otherwise span lines in the whole-buffer scan).
//!
//! Regexes using `\A`, `\z` or `$` behave differently on a whole buffer
//! (`$` would stop at the `\r` of a CRLF terminator), so those are scanned
//! line by line instead.
//!
//! # Whole word
//!
//! Literals: the match must not be preceded or followed by a word character
//! (Unicode alphanumeric or `_`), like `grep -w`. Regexes: the pattern is
//! wrapped as `\b(?:pattern)\b`.

use std::ops::Range;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};
use regex_syntax::ast::{self, Ast, ClassSetItem};
use regex_syntax::hir::Look;

use crate::predicate::{LinePredicate, scan_each_line, trim_cr};

/// How the pattern text is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QueryKind {
    /// Plain text.
    #[default]
    Literal,
    /// A regular expression (Rust `regex` syntax).
    Regex,
}

/// Case sensitivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaseMode {
    /// Insensitive unless the pattern contains an uppercase character.
    #[default]
    Smart,
    /// Exact case.
    Sensitive,
    /// Ignore case.
    Insensitive,
}

/// A user search query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    /// The pattern text.
    pub pattern: String,
    /// Literal or regex.
    pub kind: QueryKind,
    /// Case handling.
    pub case: CaseMode,
    /// Only match whole words.
    pub whole_word: bool,
}

impl Query {
    /// A literal query with smart case.
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            kind: QueryKind::Literal,
            case: CaseMode::Smart,
            whole_word: false,
        }
    }

    /// A regex query with smart case.
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            kind: QueryKind::Regex,
            ..Self::literal(pattern)
        }
    }

    /// Sets the case mode.
    pub fn with_case(mut self, case: CaseMode) -> Self {
        self.case = case;
        self
    }

    /// Sets whole-word matching.
    pub fn with_whole_word(mut self, whole_word: bool) -> Self {
        self.whole_word = whole_word;
        self
    }
}

/// Why a query could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// The pattern is empty.
    #[error("the search pattern is empty")]
    EmptyPattern,
    /// A literal contains a line break, which can never match inside a line.
    #[error("a literal search cannot contain a line break")]
    ContainsNewline,
    /// The regex is invalid.
    #[error("invalid regular expression: {message}")]
    InvalidRegex {
        /// A short user-facing explanation, e.g. "unclosed group".
        message: String,
        /// Byte range in the pattern where the problem is, when known.
        span: Option<Range<usize>>,
    },
}

/// A compiled query. Cheap to clone, `Send + Sync`.
#[derive(Clone)]
pub struct Matcher {
    engine: Engine,
    whole_word: bool,
}

#[derive(Clone)]
enum Engine {
    Memmem(Box<memmem::Finder<'static>>),
    Aho(AhoCorasick),
    /// A regex used purely as a literal finder (Unicode case folding).
    Fold(Regex),
    Regex(RegexEngine),
}

#[derive(Clone)]
struct RegexEngine {
    /// Multi-line flavour, used to find candidates in a whole buffer.
    buffer: Regex,
    /// Plain flavour, run on a single line to verify and to locate spans.
    line: Regex,
    /// Whether whole-buffer candidate scanning is valid for this pattern.
    buffer_scan: bool,
}

impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.engine {
            Engine::Memmem(_) => "memmem",
            Engine::Aho(_) => "aho-corasick",
            Engine::Fold(_) => "fold",
            Engine::Regex(_) => "regex",
        };
        f.debug_struct("Matcher")
            .field("engine", &kind)
            .field("whole_word", &self.whole_word)
            .finish()
    }
}

impl Matcher {
    /// Compiles `query`.
    pub fn compile(query: &Query) -> Result<Matcher, SearchError> {
        if query.pattern.is_empty() {
            return Err(SearchError::EmptyPattern);
        }
        match query.kind {
            QueryKind::Literal => {
                Self::compile_literals(&[query.pattern.as_str()], query.case, query.whole_word)
            }
            QueryKind::Regex => compile_regex(query),
        }
    }

    /// Compiles a set of literals into one matcher that matches a line if it
    /// contains any of them (one `aho-corasick` automaton).
    ///
    /// Smart case is insensitive unless any literal has an uppercase
    /// character.
    pub fn compile_literals(
        patterns: &[&str],
        case: CaseMode,
        whole_word: bool,
    ) -> Result<Matcher, SearchError> {
        if patterns.is_empty() || patterns.iter().any(|p| p.is_empty()) {
            return Err(SearchError::EmptyPattern);
        }
        if patterns.iter().any(|p| p.contains('\n')) {
            return Err(SearchError::ContainsNewline);
        }
        let insensitive = match case {
            CaseMode::Sensitive => false,
            CaseMode::Insensitive => true,
            CaseMode::Smart => !patterns.iter().any(|p| p.chars().any(char::is_uppercase)),
        };
        let engine = if !insensitive && patterns.len() == 1 {
            Engine::Memmem(Box::new(
                memmem::Finder::new(patterns[0].as_bytes()).into_owned(),
            ))
        } else if !insensitive || patterns.iter().all(|p| p.is_ascii()) {
            let ac = AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .ascii_case_insensitive(insensitive)
                .build(patterns.iter().map(|p| p.as_bytes()))
                .map_err(|e| SearchError::InvalidRegex {
                    message: e.to_string(),
                    span: None,
                })?;
            Engine::Aho(ac)
        } else {
            let alt = patterns
                .iter()
                .map(|p| regex::escape(p))
                .collect::<Vec<_>>()
                .join("|");
            let re = RegexBuilder::new(&alt)
                .case_insensitive(true)
                .build()
                .map_err(|e| SearchError::InvalidRegex {
                    message: e.to_string(),
                    span: None,
                })?;
            Engine::Fold(re)
        };
        Ok(Matcher { engine, whole_word })
    }

    /// Returns `true` if `line` (no terminator) matches.
    pub fn is_match(&self, line: &[u8]) -> bool {
        match &self.engine {
            Engine::Regex(r) => r.line.is_match(line),
            _ => self.find_literal(line, 0).is_some(),
        }
    }

    /// Byte ranges of the non-overlapping, non-empty matches in `line`, for
    /// painting highlights.
    pub fn find_iter(&self, line: &[u8]) -> impl Iterator<Item = Range<usize>> + '_ {
        let mut out = Vec::new();
        match &self.engine {
            Engine::Regex(r) => {
                out.extend(
                    r.line
                        .find_iter(line)
                        .map(|m| m.range())
                        .filter(|r| !r.is_empty()),
                );
            }
            _ => {
                let mut pos = 0;
                while pos < line.len() {
                    let Some(r) = self.find_literal(line, pos) else {
                        break;
                    };
                    pos = r.end;
                    out.push(r);
                }
            }
        }
        out.into_iter()
    }

    /// Finds the next literal-engine hit at or after `from`, honouring
    /// whole-word rules.
    fn find_literal(&self, hay: &[u8], from: usize) -> Option<Range<usize>> {
        let mut pos = from;
        while pos <= hay.len() {
            let r = match &self.engine {
                Engine::Memmem(f) => f
                    .find(&hay[pos..])
                    .map(|i| pos + i..pos + i + f.needle().len()),
                Engine::Aho(ac) => ac
                    .find(aho_corasick::Input::new(hay).span(pos..hay.len()))
                    .map(|m| m.range()),
                Engine::Fold(re) => re.find_at(hay, pos).map(|m| m.range()),
                Engine::Regex(_) => return None,
            }?;
            if !self.whole_word || boundaries_ok(hay, &r) {
                return Some(r);
            }
            pos = r.start + 1;
        }
        None
    }

    /// The first candidate position at or after `pos` in a whole buffer.
    fn candidate(&self, buf: &[u8], pos: usize) -> Option<usize> {
        match &self.engine {
            Engine::Regex(r) => r.buffer.find_at(buf, pos).map(|m| m.start()),
            _ => self.find_literal(buf, pos).map(|r| r.start),
        }
    }
}

impl LinePredicate for Matcher {
    fn matches(&self, line: &[u8]) -> bool {
        self.is_match(line)
    }

    fn scan_lines(&self, buf: &[u8], sink: &mut dyn FnMut(usize, usize)) {
        if let Engine::Regex(r) = &self.engine
            && !r.buffer_scan
        {
            scan_each_line(buf, &mut |l| r.line.is_match(l), sink);
            return;
        }
        let mut pos = 0;
        while pos < buf.len() {
            let Some(hit) = self.candidate(buf, pos) else {
                break;
            };
            let ls = memchr::memrchr(b'\n', &buf[..hit]).map_or(0, |i| i + 1);
            if ls >= buf.len() {
                break; // an empty match after the final terminator is not a line
            }
            let le_raw = memchr::memchr(b'\n', &buf[hit..]).map_or(buf.len(), |i| hit + i);
            let le = trim_cr(buf, ls, le_raw);
            if self.is_match(&buf[ls..le]) {
                sink(ls, le);
            }
            pos = le_raw + 1;
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Checks that the character before and after `r` are not word characters.
fn boundaries_ok(hay: &[u8], r: &Range<usize>) -> bool {
    let before = (1..=4.min(r.start)).find_map(|k| {
        std::str::from_utf8(&hay[r.start - k..r.start])
            .ok()
            .and_then(|s| s.chars().next_back())
    });
    let after = (1..=4.min(hay.len() - r.end)).find_map(|k| {
        std::str::from_utf8(&hay[r.end..r.end + k])
            .ok()
            .and_then(|s| s.chars().next())
    });
    !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
}

fn regex_error(e: &regex_syntax::Error) -> SearchError {
    let (message, span) = match e {
        regex_syntax::Error::Parse(e) => (e.kind().to_string(), Some(e.span())),
        regex_syntax::Error::Translate(e) => (e.kind().to_string(), Some(e.span())),
        other => (other.to_string(), None),
    };
    SearchError::InvalidRegex {
        message,
        span: span.map(|s| s.start.offset..s.end.offset),
    }
}

fn compile_regex(query: &Query) -> Result<Matcher, SearchError> {
    let pat = query.pattern.as_str();
    // Parse with the flags the buffer regex will use, to validate the
    // pattern (with a precise error span) and to inspect its anchors.
    let hir = regex_syntax::ParserBuilder::new()
        .multi_line(true)
        .utf8(false)
        .build()
        .parse(pat)
        .map_err(|e| regex_error(&e))?;
    let look = hir.properties().look_set();
    // `\A`/`\z` mean something else on a whole buffer. `$` before a `\r\n`
    // terminator would not match in a buffer scan (the `\r` is part of the
    // buffer but not of the line), so those patterns are scanned per line.
    let buffer_scan = ![
        Look::Start,
        Look::End,
        Look::EndLF,
        Look::StartCRLF,
        Look::EndCRLF,
    ]
    .iter()
    .any(|l| look.contains(*l));

    let insensitive = match query.case {
        CaseMode::Sensitive => false,
        CaseMode::Insensitive => true,
        CaseMode::Smart => !has_uppercase_literal(pat),
    };
    let source = if query.whole_word {
        format!(r"\b(?:{pat})\b")
    } else {
        pat.to_owned()
    };
    let build = |multi_line: bool| {
        RegexBuilder::new(&source)
            .case_insensitive(insensitive)
            .multi_line(multi_line)
            .build()
            .map_err(|e| SearchError::InvalidRegex {
                message: e.to_string(),
                span: None,
            })
    };
    Ok(Matcher {
        engine: Engine::Regex(RegexEngine {
            buffer: build(true)?,
            line: build(false)?,
            buffer_scan,
        }),
        whole_word: query.whole_word,
    })
}

/// Whether the regex contains an uppercase literal (escapes such as `\S` or
/// `\p{Lu}` do not count). Falls back to a plain scan if the AST parse fails.
fn has_uppercase_literal(pat: &str) -> bool {
    struct V(bool);
    impl ast::Visitor for V {
        type Output = bool;
        type Err = std::convert::Infallible;
        fn finish(self) -> Result<bool, Self::Err> {
            Ok(self.0)
        }
        fn visit_pre(&mut self, a: &Ast) -> Result<(), Self::Err> {
            if let Ast::Literal(l) = a {
                self.0 |= l.c.is_uppercase();
            }
            Ok(())
        }
        fn visit_class_set_item_pre(&mut self, i: &ClassSetItem) -> Result<(), Self::Err> {
            match i {
                ClassSetItem::Literal(l) => self.0 |= l.c.is_uppercase(),
                ClassSetItem::Range(r) => {
                    self.0 |= r.start.c.is_uppercase() || r.end.c.is_uppercase()
                }
                _ => {}
            }
            Ok(())
        }
    }
    match ast::parse::Parser::new().parse(pat) {
        Ok(a) => ast::visit(&a, V(false)).unwrap_or(false),
        Err(_) => pat.chars().any(char::is_uppercase),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(m: &Matcher, buf: &[u8]) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        m.scan_lines(buf, &mut |s, e| v.push((s, e)));
        v
    }

    #[test]
    fn literal_smart_case() {
        let m = Matcher::compile(&Query::literal("error")).unwrap();
        assert!(m.is_match(b"an ERROR here"));
        let m = Matcher::compile(&Query::literal("Error")).unwrap();
        assert!(!m.is_match(b"an ERROR here"));
        assert!(m.is_match(b"an Error here"));
    }

    #[test]
    fn case_modes_and_unicode_folding() {
        let q = Query::literal("straSSe").with_case(CaseMode::Insensitive);
        assert!(Matcher::compile(&q).unwrap().is_match(b"STRASSE"));
        let q = Query::literal("ÉCOLE").with_case(CaseMode::Insensitive);
        assert!(Matcher::compile(&q).unwrap().is_match("l'école".as_bytes()));
        let q = Query::literal("ab").with_case(CaseMode::Sensitive);
        assert!(!Matcher::compile(&q).unwrap().is_match(b"AB"));
    }

    #[test]
    fn regex_smart_case_ignores_escapes() {
        let m = Matcher::compile(&Query::regex(r"\Serror")).unwrap();
        assert!(m.is_match(b"xERROR"));
        let m = Matcher::compile(&Query::regex(r"\p{Lu}rror")).unwrap();
        assert!(m.is_match(b"Error"));
        let m = Matcher::compile(&Query::regex(r"E.ror")).unwrap();
        assert!(!m.is_match(b"error"));
    }

    #[test]
    fn whole_word_literal_and_regex() {
        let q = Query::literal("cat").with_whole_word(true);
        let m = Matcher::compile(&q).unwrap();
        assert!(m.is_match(b"a cat sat"));
        assert!(!m.is_match(b"concatenate"));
        assert!(m.is_match(b"concat cat"));
        assert!(!m.is_match(b"cat_food"));
        assert!(m.is_match("é cat.".as_bytes()));
        assert!(!m.is_match("écat".as_bytes()));
        let q = Query::regex("ca.").with_whole_word(true);
        let m = Matcher::compile(&q).unwrap();
        assert!(m.is_match(b"a cat sat"));
        assert!(!m.is_match(b"concatenate"));
    }

    #[test]
    fn invalid_regex_reports_message_and_span() {
        let err = Matcher::compile(&Query::regex("ab(cd")).unwrap_err();
        match err {
            SearchError::InvalidRegex { message, span } => {
                assert!(message.contains("unclosed"), "{message}");
                assert_eq!(span, Some(2..3));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            Matcher::compile(&Query::literal("")).unwrap_err(),
            SearchError::EmptyPattern
        );
        assert_eq!(
            Matcher::compile(&Query::literal("a\nb")).unwrap_err(),
            SearchError::ContainsNewline
        );
    }

    #[test]
    fn regex_never_matches_across_lines() {
        let m = Matcher::compile(&Query::regex(r"a\s+b")).unwrap();
        assert_eq!(lines(&m, b"a\nb\nc a b\n"), vec![(4, 9)]);
        let m = Matcher::compile(&Query::regex(r"a[^x]b")).unwrap();
        assert!(lines(&m, b"a\nb\n").is_empty());
    }

    #[test]
    fn scan_lines_handles_crlf_anchors_and_empty_lines() {
        let m = Matcher::compile(&Query::regex(r"foo$")).unwrap();
        assert_eq!(lines(&m, b"foo\r\nbar\nxfoo"), vec![(0, 3), (9, 13)]);
        let m = Matcher::compile(&Query::regex(r"^$")).unwrap();
        assert_eq!(lines(&m, b"a\n\nb\n"), vec![(2, 2)]);
        let m = Matcher::compile(&Query::regex(r"^a")).unwrap();
        assert_eq!(lines(&m, b"b\nab\na\n"), vec![(2, 4), (5, 6)]);
        let m = Matcher::compile(&Query::regex(r"\Aa")).unwrap();
        assert_eq!(lines(&m, b"a\na\n"), vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn find_iter_ranges() {
        let m = Matcher::compile(&Query::literal("ab")).unwrap();
        assert_eq!(
            m.find_iter(b"ab xab aab").collect::<Vec<_>>(),
            vec![0..2, 4..6, 8..10]
        );
        let m = Matcher::compile(&Query::regex(r"\d+")).unwrap();
        assert_eq!(m.find_iter(b"a12 b3").collect::<Vec<_>>(), vec![1..3, 5..6]);
        let m = Matcher::compile(&Query::regex(r"x*")).unwrap();
        assert_eq!(m.find_iter(b"axx").collect::<Vec<_>>(), vec![1..3]);
    }

    #[test]
    fn many_literals() {
        let m = Matcher::compile_literals(&["foo", "bar"], CaseMode::Smart, false).unwrap();
        assert_eq!(lines(&m, b"xfoo\nnone\nBAR\n"), vec![(0, 4), (10, 13)]);
    }

    #[test]
    fn no_phantom_line_after_final_newline() {
        let m = Matcher::compile(&Query::regex("$")).unwrap();
        assert_eq!(lines(&m, b"a\nb\n"), vec![(0, 1), (2, 3)]);
    }
}
