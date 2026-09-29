//! Column query filters: an adapter that lets an `oxtail_columns::Query`
//! run as an `oxtail_search::LinePredicate`, so it plugs into the same
//! filter job as text and regex filters (and runs on the same worker
//! threads, never on the UI thread).
//!
//! The predicate parses each line with the tab's parser inside `matches`
//! and evaluates the query on the record. Timestamp terms use the tab's
//! [`TimeParser`]. Lines the parser does not accept (continuation lines)
//! have no columns: column terms do not match them, plain-text terms still
//! search the whole line.

use std::ops::Range;
use std::sync::Arc;

use oxtail_columns::{Parser, Query, QueryError, Schema, TimestampParser};
use oxtail_search::LinePredicate;
use oxtail_time::TimeParser;

/// What a query needs from the tab: the parser and the time parser.
#[derive(Clone)]
pub struct QueryContext {
    /// The tab's column parser.
    pub parser: Arc<Parser>,
    /// The tab's time parser.
    pub time: Arc<TimeParser>,
}

impl QueryContext {
    /// A context identity string: changes when the parser or the zone does
    /// (used to restart filter jobs).
    pub fn signature(&self) -> String {
        format!(
            "{:?}|{:?}",
            self.parser.spec(),
            self.time.context().tz.iana_name()
        )
    }
}

/// Adapts the tab's [`TimeParser`] to the query's timestamp hook.
pub struct TimeAdapter(pub Arc<TimeParser>);

impl TimestampParser for TimeAdapter {
    fn parse_ns(&self, value: &str) -> Option<i64> {
        let ts = self.0.parse(value)?;
        i64::try_from(ts.as_nanosecond()).ok()
    }
}

/// A query as a line predicate.
pub struct QueryPredicate {
    query: Query,
    ctx: Option<Arc<QueryContext>>,
    ts: TimeAdapter,
}

impl QueryPredicate {
    /// Wraps `query`. Without a context (plain text tab) only plain-text
    /// terms can match.
    pub fn new(query: Query, ctx: Option<Arc<QueryContext>>) -> Self {
        let time = ctx.as_ref().map_or_else(
            || Arc::new(TimeParser::new(oxtail_time::TimeContext::utc())),
            |c| Arc::clone(&c.time),
        );
        Self {
            query,
            ctx,
            ts: TimeAdapter(time),
        }
    }
}

impl LinePredicate for QueryPredicate {
    fn matches(&self, line: &[u8]) -> bool {
        let text = String::from_utf8_lossy(line);
        let rec = self.ctx.as_ref().and_then(|c| c.parser.parse(&text));
        let schema = self.ctx.as_ref().map(|c| c.parser.schema());
        self.query.matches(&text, rec.as_ref(), schema, &self.ts)
    }
}

/// A problem with a query, for showing under the text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIssue {
    /// What is wrong.
    pub message: String,
    /// Where in the text (byte range), when known.
    pub span: Option<Range<usize>>,
}

impl QueryIssue {
    fn from_error(text: &str, e: &QueryError) -> Self {
        // Highlight the token at the reported position.
        let start = e.position.min(text.len());
        let start = (0..=start)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        let rest = &text[start..];
        let len = rest
            .char_indices()
            .find(|(i, c)| *i > 0 && (c.is_whitespace() || matches!(c, ':' | '(' | ')')))
            .map_or(rest.len(), |(i, _)| i)
            .max(rest.chars().next().map_or(0, char::len_utf8));
        QueryIssue {
            message: e.message.clone(),
            span: (len > 0).then_some(start..start + len),
        }
    }
}

/// Compiles `text` as a query. Returns the query, or the problems: a syntax
/// error (one issue) or column names the schema does not have.
pub fn compile_query(text: &str, schema: Option<&Schema>) -> Result<Query, Vec<QueryIssue>> {
    let q = Query::parse(text).map_err(|e| vec![QueryIssue::from_error(text, &e)])?;
    if let Some(schema) = schema {
        let errs = q.check(schema);
        if !errs.is_empty() {
            return Err(errs
                .iter()
                .map(|e| QueryIssue::from_error(text, e))
                .collect());
        }
    }
    Ok(q)
}

/// The query for "this column has this value" (a click in the statistics
/// panel): `column:"value"` with quotes escaped.
pub fn equals_query(column: &str, value: &str) -> String {
    let escaped = value.replace('"', "\\\"");
    format!("{column}:\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_columns::ParserSpec;
    use oxtail_time::TimeContext;
    use oxtail_time::jiff::tz::TimeZone;

    fn ctx(spec: ParserSpec) -> Arc<QueryContext> {
        Arc::new(QueryContext {
            parser: Arc::new(spec.compile().unwrap()),
            time: Arc::new(TimeParser::new(TimeContext::new(TimeZone::UTC))),
        })
    }

    fn logfmt() -> Arc<QueryContext> {
        ctx(ParserSpec::Logfmt {
            columns: vec!["ts".into(), "level".into(), "msg".into(), "took".into()],
            kinds: Default::default(),
        })
    }

    fn pred(q: &str, c: Option<Arc<QueryContext>>) -> QueryPredicate {
        QueryPredicate::new(
            compile_query(q, c.as_ref().map(|c| c.parser.schema())).unwrap(),
            c,
        )
    }

    const A: &[u8] = b"ts=2026-09-29T10:00:00Z level=error msg=\"db down\" took=1500ms";
    const B: &[u8] = b"ts=2026-09-29T10:20:00Z level=info msg=ok took=20ms";

    #[test]
    fn column_terms_match_parsed_fields() {
        let c = Some(logfmt());
        assert!(pred("level:error", c.clone()).matches(A));
        assert!(!pred("level:error", c.clone()).matches(B));
        assert!(pred("level:(error|warn) took>1s", c.clone()).matches(A));
        assert!(pred("-level:error", c.clone()).matches(B));
        assert!(pred("msg~\"db\\s+down\"", c.clone()).matches(A));
        // Plain text searches the whole line.
        assert!(pred("down", c.clone()).matches(A));
        assert!(!pred("down", c).matches(B));
    }

    #[test]
    fn time_terms_use_the_tab_time_parser() {
        let c = Some(logfmt());
        assert!(pred("ts>2026-09-29T10:10:00Z", c.clone()).matches(B));
        assert!(!pred("ts>2026-09-29T10:10:00Z", c).matches(A));
    }

    #[test]
    fn continuation_lines_have_no_columns() {
        let c = Some(ctx(ParserSpec::AccessCombined));
        let stack = b"    at com.example.Foo.bar(Foo.java:42)";
        assert!(!pred("status>=500", c.clone()).matches(stack));
        assert!(pred("Foo.java", c).matches(stack));
    }

    #[test]
    fn without_a_parser_only_text_terms_match() {
        let q = Query::parse("level:error").unwrap();
        let p = QueryPredicate::new(q, None);
        assert!(!p.matches(A));
        let p = QueryPredicate::new(Query::parse("error").unwrap(), None);
        assert!(p.matches(A));
    }

    #[test]
    fn scanning_a_buffer_finds_matching_lines() {
        let c = Some(logfmt());
        let p = pred("level:error", c);
        let mut buf = Vec::new();
        buf.extend_from_slice(A);
        buf.push(b'\n');
        buf.extend_from_slice(B);
        buf.push(b'\n');
        buf.extend_from_slice(A);
        let mut hits = Vec::new();
        p.scan_lines(&buf, &mut |s, e| hits.push((s, e)));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 0);
    }

    #[test]
    fn errors_come_with_positions() {
        let schema = logfmt().parser.schema().clone();
        let issues = compile_query("level:error nosuch:1", Some(&schema)).unwrap_err();
        assert_eq!(issues.len(), 1);
        let span = issues[0].span.clone().unwrap();
        assert_eq!(&"level:error nosuch:1"[span], "nosuch");
        assert!(issues[0].message.contains("nosuch"));
        let issues = compile_query("level:(error", Some(&schema)).unwrap_err();
        assert!(!issues[0].message.is_empty());
        assert!(compile_query("level:error", Some(&schema)).is_ok());
        // Without a schema unknown columns cannot be checked.
        assert!(compile_query("whatever:1", None).is_ok());
        // Multi-byte text never panics.
        let _ = compile_query("héllo:(", Some(&schema));
    }

    #[test]
    fn equals_queries_round_trip_through_the_parser() {
        let c = logfmt();
        for v in ["error", "db down", "say \"hi\"", "1500ms"] {
            let q = equals_query("msg", v);
            assert!(compile_query(&q, Some(c.parser.schema())).is_ok(), "{q}");
        }
        let line = b"ts=x level=info msg=\"say \\\"hi\\\"\" took=1";
        let q = equals_query("msg", "say \"hi\"");
        assert!(pred(&q, Some(c)).matches(line), "{q}");
    }

    #[test]
    fn signature_changes_with_the_parser() {
        assert_ne!(
            logfmt().signature(),
            ctx(ParserSpec::AccessCombined).signature()
        );
    }
}
