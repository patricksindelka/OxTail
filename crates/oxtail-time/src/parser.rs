//! [`TimeParser`]: learn a format from sample lines, then parse quickly.

use std::{collections::HashMap, ops::Range};

use jiff::{Timestamp, tz::TimeZone};

use crate::detect::{TimeContext, TimestampFormat, scan};

/// Parses the timestamp of log lines.
///
/// Typical use: call [`learn`](Self::learn) with a sample of lines (majority
/// vote over detected formats), then [`parse`](Self::parse) every line. With a
/// learned format only that format's scanner runs (fast path); if it does not
/// match, full detection is tried so mixed files still work.
#[derive(Clone, Debug)]
pub struct TimeParser {
    format: Option<TimestampFormat>,
    ctx: TimeContext,
}

impl TimeParser {
    /// A parser with no learned format (every line is fully detected).
    pub fn new(ctx: TimeContext) -> Self {
        Self { format: None, ctx }
    }

    /// A parser fixed to `format` (e.g. from a profile).
    pub fn with_format(ctx: TimeContext, format: TimestampFormat) -> Self {
        Self {
            format: Some(format),
            ctx,
        }
    }

    /// Learns the format by majority vote over `lines`; ties go to the
    /// format that sorts first. Lines whose token is not a valid instant do
    /// not vote. Keeps the previous format if nothing matched. Returns the
    /// format in effect afterwards.
    pub fn learn<'a>(
        &mut self,
        lines: impl IntoIterator<Item = &'a str>,
    ) -> Option<TimestampFormat> {
        let mut votes: HashMap<TimestampFormat, usize> = HashMap::new();
        for line in lines {
            if let Some((_, f, p)) = scan(line.as_bytes(), None)
                && p.resolve(&self.ctx).is_some()
            {
                *votes.entry(f).or_default() += 1;
            }
        }
        let best = votes
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(f, _)| f);
        if best.is_some() {
            self.format = best;
        }
        self.format
    }

    /// The learned or fixed format.
    pub fn format(&self) -> Option<TimestampFormat> {
        self.format
    }

    /// The context used for zone-less timestamps.
    pub fn context(&self) -> &TimeContext {
        &self.ctx
    }

    /// Changes the zone used for zone-less timestamps.
    pub fn set_timezone(&mut self, tz: TimeZone) {
        self.ctx.tz = tz;
    }

    /// Parses the first timestamp in `line`.
    pub fn parse(&self, line: &str) -> Option<Timestamp> {
        self.parse_with_range(line).map(|(ts, _)| ts)
    }

    /// Like [`parse`](Self::parse) but also returns the token's byte range.
    pub fn parse_with_range(&self, line: &str) -> Option<(Timestamp, Range<usize>)> {
        let b = line.as_bytes();
        if let Some(f) = self.format
            && let Some((r, _, p)) = scan(b, Some(f))
            && let Some(ts) = p.resolve(&self.ctx)
        {
            return Some((ts, r));
        }
        let (r, _, p) = scan(b, None)?;
        p.resolve(&self.ctx).map(|ts| (ts, r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::Date;

    fn ctx() -> TimeContext {
        TimeContext::with_reference_date(TimeZone::UTC, Date::new(2026, 9, 29).expect("date"))
    }

    #[test]
    fn learns_majority_and_parses() {
        let mut p = TimeParser::new(ctx());
        let lines = [
            "2026-09-29 10:00:00,001 A",
            "2026-09-29 10:00:01,001 B",
            "junk",
            "Sep 29 10:00:02 host x",
        ];
        assert_eq!(p.learn(lines), Some(TimestampFormat::Iso8601Comma));
        let a = p.parse("2026-09-29 10:00:00,500 x").expect("parse");
        assert_eq!(a.subsec_nanosecond(), 500_000_000);
        // Fallback to detection for another format.
        assert!(p.parse("Sep 29 10:00:02 host").is_some());
        assert!(p.parse("nothing here").is_none());
    }

    #[test]
    fn iso_dot_line_with_comma_learned() {
        let p = TimeParser::with_format(ctx(), TimestampFormat::Iso8601Comma);
        assert!(p.parse("2026-09-29 10:00:00.5 x").is_some());
    }

    #[test]
    fn learn_keeps_previous_on_no_data() {
        let mut p = TimeParser::with_format(ctx(), TimestampFormat::Syslog);
        assert_eq!(p.learn(["nope"]), Some(TimestampFormat::Syslog));
    }

    #[test]
    fn range_reported() {
        let p = TimeParser::new(ctx());
        let l = "[2026-09-29 10:00:00] x";
        let (_, r) = p.parse_with_range(l).expect("parse");
        assert_eq!(&l[r], "2026-09-29 10:00:00");
    }
}
