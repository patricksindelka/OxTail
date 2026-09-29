//! Go to time (Ctrl+Shift+G): parsing what the user typed, a `LineAccess`
//! adapter over a document, and the worker that runs
//! `oxtail_time::find_time_cancellable` (a binary search over lines with a
//! linear fallback for non-monotonic logs) off the UI thread.
//!
//! Input forms:
//!
//! * absolute: `2026-09-29 10:00`, `2026-09-29T10:00:00Z`, `2026-09-29`,
//!   `10:00[:05[.25]]` (today), and whatever the timestamp detector knows
//!   (syslog style, Apache style, epoch seconds ...);
//! * relative to the end of the file: `-15m`, `-1h30m`, `-90s`;
//! * relative to the start of the file: `+1h`, `+2d`.
//!
//! Zone-less input is read in the configured time zone.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use oxtail_core::Document;
use oxtail_time::jiff::civil::{Date, DateTime, Time};
use oxtail_time::jiff::tz::TimeZone;
use oxtail_time::jiff::{SignedDuration, Timestamp};
use oxtail_time::{LineAccess, TimeParser, find_time_cancellable};

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeTarget {
    /// A point in time.
    Absolute(Timestamp),
    /// This much before (negative) or after the last timestamp of the file.
    FromEnd(SignedDuration),
    /// This much after (positive) or before the first timestamp of the file.
    FromStart(SignedDuration),
}

/// Parses a span such as `15m`, `1h30m`, `2.5h`, `90s`, `250ms`, `1d`, `2w`.
/// Returns the (unsigned) length, or `None` for anything else.
pub fn parse_span(s: &str) -> Option<SignedDuration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut rest = s;
    let mut total_ns: f64 = 0.0;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let n: f64 = rest[..num_end].parse().ok()?;
        rest = &rest[num_end..];
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = rest[..unit_end].to_ascii_lowercase();
        rest = rest[unit_end..].trim_start();
        let ns = match unit.as_str() {
            "ms" => 1e6,
            "s" | "sec" | "secs" => 1e9,
            "m" | "min" | "mins" => 60e9,
            "h" | "hr" | "hrs" => 3600e9,
            "d" | "day" | "days" => 86_400e9,
            "w" | "wk" | "week" | "weeks" => 604_800e9,
            _ => return None,
        };
        total_ns += n * ns;
    }
    if !total_ns.is_finite() || total_ns > 1e18 {
        return None;
    }
    Some(SignedDuration::from_nanos(total_ns as i64))
}

/// Interprets a civil date-time in `tz` (a gap or overlap resolves to the
/// usual "compatible" choice).
fn to_timestamp(dt: DateTime, tz: &TimeZone) -> Option<Timestamp> {
    tz.to_ambiguous_timestamp(dt).compatible().ok()
}

/// Parses an absolute time; `now` supplies today's date for time-only input.
pub fn parse_absolute(input: &str, tz: &TimeZone, now: Timestamp) -> Option<Timestamp> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    // RFC 3339 with an offset or Z.
    if let Ok(ts) = t.parse::<Timestamp>() {
        return Some(ts);
    }
    // Civil date-time (ISO with `T`, or with a space) in the configured zone.
    let iso = t.replacen(' ', "T", 1);
    if let Ok(dt) = iso.parse::<DateTime>() {
        return to_timestamp(dt, tz);
    }
    if let Ok(d) = t.parse::<Date>() {
        return to_timestamp(d.to_datetime(Time::midnight()), tz);
    }
    // `HH:MM[:SS[.fff]]` today.
    if t.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && t.contains(':')
        && !t.contains(['-', '/', 'T'])
        && let Ok(time) = t.parse::<Time>()
    {
        let today = now.to_zoned(tz.clone()).date();
        return to_timestamp(today.to_datetime(time), tz);
    }
    // Whatever the timestamp detector understands (syslog, Apache, epoch ...).
    let mut ctx = oxtail_time::TimeContext::new(tz.clone());
    ctx.reference_date = now.to_zoned(tz.clone()).date();
    ctx.reference_year = ctx.reference_date.year();
    TimeParser::new(ctx).parse(t)
}

/// Parses the go-to-time input. `-15m` counts back from the end of the file,
/// `+1h` forward from its start.
pub fn parse_goto_time(input: &str, tz: &TimeZone, now: Timestamp) -> Result<TimeTarget, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("Enter a time, e.g. 2026-09-29 10:00, 10:15, -15m or +1h".into());
    }
    if let Some(rest) = t.strip_prefix('-')
        && rest.starts_with(|c: char| c.is_ascii_digit())
        && let Some(d) = parse_span(rest)
    {
        return Ok(TimeTarget::FromEnd(SignedDuration::ZERO.saturating_sub(d)));
    }
    if let Some(rest) = t.strip_prefix('+')
        && let Some(d) = parse_span(rest)
    {
        return Ok(TimeTarget::FromStart(d));
    }
    parse_absolute(t, tz, now)
        .map(TimeTarget::Absolute)
        .ok_or_else(|| {
            format!("Cannot read \u{201c}{t}\u{201d} as a time. Try 2026-09-29 10:00, 10:15, -15m or +1h")
        })
}

/// Blocks of lines cached by [`DocLines`].
const BLOCK: u64 = 64;
/// Most blocks kept.
const MAX_BLOCKS: usize = 16;

/// A document as `LineAccess`: line `n` is read in blocks of 64 lines with
/// `read_lines_blocking_with_generation`, so use it from a worker thread
/// only. Returns `None` for every line once the document's generation
/// changed.
pub struct DocLines<'a> {
    doc: &'a Document,
    generation: u64,
    count: u64,
    blocks: RefCell<Vec<(u64, Vec<String>)>>,
}

impl<'a> DocLines<'a> {
    /// Access to the first `count` lines of `doc` as of `generation`.
    pub fn new(doc: &'a Document, generation: u64, count: u64) -> Self {
        Self {
            doc,
            generation,
            count,
            blocks: RefCell::new(Vec::new()),
        }
    }
}

impl LineAccess for DocLines<'_> {
    fn line_count(&self) -> u64 {
        self.count
    }

    fn line(&self, n: u64) -> Option<String> {
        if n >= self.count {
            return None;
        }
        let start = n - n % BLOCK;
        let idx = usize::try_from(n - start).ok()?;
        if let Some((_, v)) = self.blocks.borrow().iter().find(|(s, _)| *s == start) {
            return v.get(idx).cloned();
        }
        let (g, lines) = self
            .doc
            .read_lines_blocking_with_generation(start, BLOCK as usize);
        if g != self.generation {
            return None;
        }
        let texts: Vec<String> = lines.into_iter().map(|l| l.text).collect();
        let out = texts.get(idx).cloned();
        let mut blocks = self.blocks.borrow_mut();
        if blocks.len() >= MAX_BLOCKS {
            blocks.remove(0);
        }
        blocks.push((start, texts));
        out
    }
}

/// How many lines are probed for a first or last timestamp.
const PROBE: u64 = 1000;

/// The first timestamp among the first lines.
pub fn first_timestamp(a: &dyn LineAccess, p: &TimeParser) -> Option<Timestamp> {
    (0..a.line_count().min(PROBE)).find_map(|n| a.line(n).and_then(|l| p.parse(&l)))
}

/// The last timestamp among the last lines.
pub fn last_timestamp(a: &dyn LineAccess, p: &TimeParser) -> Option<Timestamp> {
    let count = a.line_count();
    (count.saturating_sub(PROBE)..count)
        .rev()
        .find_map(|n| a.line(n).and_then(|l| p.parse(&l)))
}

/// What the worker found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GotoMsg {
    /// The first line at or after the target (0-based).
    Found {
        /// The line.
        line: u64,
        /// Its timestamp, when it has one.
        ts: Option<Timestamp>,
    },
    /// Every line is before the target.
    PastEnd {
        /// The last line.
        last_line: u64,
    },
    /// The file has no readable timestamps (or is empty).
    NoTimestamps,
    /// The file changed while searching.
    Changed,
    /// Stopped by the user.
    Cancelled,
}

/// A running search.
pub struct GotoJob {
    rx: Receiver<GotoMsg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for GotoJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl GotoJob {
    /// Starts the worker.
    pub fn start(
        doc: Arc<Document>,
        parser: Arc<TimeParser>,
        target: TimeTarget,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> GotoJob {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("oxtail-goto-time".into())
            .spawn(move || {
                let msg = run(&doc, &parser, target, &flag);
                let _ = tx.send(msg);
                wake();
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the go-to-time thread: {e}");
        }
        GotoJob { rx, cancel }
    }

    /// Asks the worker to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The answer, once there is one. Never blocks.
    pub fn try_recv(&self) -> Option<GotoMsg> {
        self.rx.try_recv().ok()
    }
}

fn run(doc: &Document, parser: &TimeParser, target: TimeTarget, cancel: &AtomicBool) -> GotoMsg {
    let generation = doc.generation();
    let count = loop {
        if cancel.load(Ordering::Relaxed) {
            return GotoMsg::Cancelled;
        }
        let snap = doc.snapshot();
        if snap.generation != generation {
            return GotoMsg::Changed;
        }
        if crate::docscan::index_complete(&snap) {
            break snap.lines.known;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let access = DocLines::new(doc, generation, count);
    let target_ts = match target {
        TimeTarget::Absolute(t) => Some(t),
        TimeTarget::FromEnd(d) => {
            last_timestamp(&access, parser).and_then(|t| t.checked_add(d).ok())
        }
        TimeTarget::FromStart(d) => {
            first_timestamp(&access, parser).and_then(|t| t.checked_add(d).ok())
        }
    };
    let Some(target_ts) = target_ts else {
        return GotoMsg::NoTimestamps;
    };
    let is_cancelled = || cancel.load(Ordering::Relaxed) || doc.generation() != generation;
    match find_time_cancellable(&access, parser, target_ts, &is_cancelled) {
        Ok(Some(line)) => GotoMsg::Found {
            line,
            ts: access.line(line).and_then(|l| parser.parse(&l)),
        },
        Ok(None) => GotoMsg::PastEnd {
            last_line: count.saturating_sub(1),
        },
        Err(_) if doc.generation() != generation => GotoMsg::Changed,
        Err(_) => GotoMsg::Cancelled,
    }
}

/// The state of the go-to-time dialog.
#[derive(Default)]
pub struct GotoTimeDialog {
    /// The input.
    pub text: String,
    /// Why the input was rejected.
    pub error: Option<String>,
    /// Focus the text field on the next frame.
    pub focus: bool,
    /// The running search and the tab that asked.
    pub job: Option<(u64, GotoJob)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docscan::testutil::{doc, wait};
    use oxtail_time::TimeContext;

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    fn now() -> Timestamp {
        "2026-09-29T12:00:00Z".parse().unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn spans() {
        let ms = SignedDuration::from_millis;
        assert_eq!(parse_span("15m"), Some(ms(900_000)));
        assert_eq!(parse_span("1h30m"), Some(ms(5_400_000)));
        assert_eq!(parse_span("2.5h"), Some(ms(9_000_000)));
        assert_eq!(parse_span("250ms"), Some(ms(250)));
        assert_eq!(parse_span("90s"), Some(ms(90_000)));
        assert_eq!(parse_span("1d"), Some(ms(86_400_000)));
        assert_eq!(parse_span("2w"), Some(ms(2 * 7 * 86_400_000)));
        for bad in ["", "m", "15", "15x", "1h 30", "1..5h", "9999999999999999w"] {
            assert_eq!(parse_span(bad), None, "{bad}");
        }
    }

    #[test]
    fn relative_and_absolute_input() {
        let z = utc();
        assert_eq!(
            parse_goto_time("-15m", &z, now()),
            Ok(TimeTarget::FromEnd(SignedDuration::from_secs(-900)))
        );
        assert_eq!(
            parse_goto_time("+1h", &z, now()),
            Ok(TimeTarget::FromStart(SignedDuration::from_secs(3600)))
        );
        let abs = |s: &str| match parse_goto_time(s, &z, now()) {
            Ok(TimeTarget::Absolute(t)) => t,
            other => panic!("{s}: {other:?}"),
        };
        assert_eq!(abs("2026-09-29 10:00"), ts("2026-09-29T10:00:00Z"));
        assert_eq!(abs("2026-09-29T10:00:05"), ts("2026-09-29T10:00:05Z"));
        assert_eq!(abs("2026-09-29T10:00:00Z"), ts("2026-09-29T10:00:00Z"));
        assert_eq!(abs("2026-09-29T12:00:00+02:00"), ts("2026-09-29T10:00:00Z"));
        assert_eq!(abs("2026-09-29"), ts("2026-09-29T00:00:00Z"));
        // Time only: today (per `now`) in the zone.
        assert_eq!(abs("10:15"), ts("2026-09-29T10:15:00Z"));
        assert_eq!(abs("10:15:30.5"), ts("2026-09-29T10:15:30.5Z"));
        // Other formats through the detector.
        assert_eq!(abs("Sep 29 10:00:00"), ts("2026-09-29T10:00:00Z"));
        assert_eq!(abs("1790676062"), ts("2026-09-29T10:01:02Z"));
    }

    #[test]
    fn zoneless_input_uses_the_configured_zone() {
        let ams = TimeZone::get("Europe/Amsterdam").unwrap();
        match parse_goto_time("2026-09-29 10:00", &ams, now()) {
            Ok(TimeTarget::Absolute(t)) => assert_eq!(t, ts("2026-09-29T08:00:00Z")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bad_input_is_explained() {
        let z = utc();
        for bad in ["", "   ", "yesterday", "-x", "+", "2026-13-40", "-"] {
            let e = parse_goto_time(bad, &z, now()).unwrap_err();
            assert!(!e.is_empty(), "{bad}");
        }
        // A minus sign followed by a date is not a negative span.
        assert!(parse_goto_time("-2026-09-29", &z, now()).is_err());
    }

    fn log(n: usize) -> String {
        // One line per 10 seconds starting at 10:00:00; every 50th line is a
        // stack-trace continuation without a timestamp.
        let mut s = String::new();
        for i in 0..n {
            if i % 50 == 49 {
                s.push_str("    at com.example.Foo.bar(Foo.java:42)\n");
            } else {
                let secs = i * 10;
                s.push_str(&format!(
                    "2026-09-29 {:02}:{:02}:{:02} INFO line {i}\n",
                    10 + secs / 3600,
                    (secs / 60) % 60,
                    secs % 60
                ));
            }
        }
        s
    }

    fn parser() -> Arc<TimeParser> {
        let mut p = TimeParser::new(TimeContext::new(utc()));
        p.learn(["2026-09-29 10:00:00 INFO x"]);
        Arc::new(p)
    }

    fn run_job(d: &Arc<Document>, target: TimeTarget) -> GotoMsg {
        let job = GotoJob::start(Arc::clone(d), parser(), target, Arc::new(|| {}));
        wait("go to time", || job.try_recv())
    }

    #[test]
    fn the_adapter_reads_lines_by_number() {
        let text = log(300);
        let d = doc(&text);
        let a = DocLines::new(&d, d.generation(), 300);
        assert_eq!(a.line_count(), 300);
        assert_eq!(
            a.line(0).as_deref(),
            Some("2026-09-29 10:00:00 INFO line 0")
        );
        assert_eq!(
            a.line(149).as_deref(),
            Some("    at com.example.Foo.bar(Foo.java:42)")
        );
        assert_eq!(a.line(299).unwrap(), text.lines().last().unwrap());
        assert_eq!(a.line(300), None);
        // A stale generation reads nothing.
        let stale = DocLines::new(&d, d.generation() + 1, 300);
        assert_eq!(stale.line(3), None);
    }

    #[test]
    fn an_absolute_time_finds_the_first_line_at_or_after_it() {
        let d = doc(&log(1000));
        // Line i is at 10:00:00 + 10 s * i (lines 49, 99, ... have none).
        let target = TimeTarget::Absolute(ts("2026-09-29T10:25:00Z")); // line 150
        match run_job(&d, target) {
            GotoMsg::Found { line, ts: t } => {
                assert_eq!(line, 150);
                assert_eq!(t, Some(ts("2026-09-29T10:25:00Z")));
            }
            other => panic!("{other:?}"),
        }
        // Just after a line: the next one.
        let target = TimeTarget::Absolute(ts("2026-09-29T10:25:01Z"));
        assert!(matches!(
            run_job(&d, target),
            GotoMsg::Found { line: 151, .. }
        ));
        // Before the first line: line 0.
        let target = TimeTarget::Absolute(ts("2020-01-01T00:00:00Z"));
        assert!(matches!(
            run_job(&d, target),
            GotoMsg::Found { line: 0, .. }
        ));
        // After the last line.
        let target = TimeTarget::Absolute(ts("2030-01-01T00:00:00Z"));
        assert_eq!(run_job(&d, target), GotoMsg::PastEnd { last_line: 999 });
    }

    #[test]
    fn relative_targets_count_from_the_ends() {
        let d = doc(&log(1000));
        // Line 999 is a continuation line, so the last timestamp is line 998's:
        // 10:00:00 + 9980 s. Ten minutes earlier is line 938 exactly.
        match run_job(&d, TimeTarget::FromEnd(SignedDuration::from_secs(-600))) {
            GotoMsg::Found { ts: Some(t), line } => {
                assert_eq!(line, 938);
                assert_eq!(t, ts("2026-09-29T12:36:20Z"));
            }
            other => panic!("{other:?}"),
        }
        match run_job(&d, TimeTarget::FromStart(SignedDuration::from_secs(3600))) {
            GotoMsg::Found { line, .. } => assert_eq!(line, 360),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn files_without_timestamps_say_so() {
        let d = doc("hello\nworld\n");
        let target = TimeTarget::FromEnd(SignedDuration::from_secs(-60));
        assert_eq!(run_job(&d, target), GotoMsg::NoTimestamps);
        let empty = doc("");
        assert_eq!(run_job(&empty, target), GotoMsg::NoTimestamps);
    }

    #[test]
    fn a_cancelled_search_reports_it() {
        let d = doc(&log(200));
        let cancel = AtomicBool::new(true);
        let msg = run(&d, &parser(), TimeTarget::Absolute(now()), &cancel);
        assert_eq!(msg, GotoMsg::Cancelled);
    }

    #[test]
    fn dropping_the_job_sets_the_cancel_flag() {
        let d = doc(&log(10));
        let job = GotoJob::start(d, parser(), TimeTarget::Absolute(now()), Arc::new(|| {}));
        let flag = Arc::clone(&job.cancel);
        drop(job);
        assert!(flag.load(Ordering::Relaxed));
    }
}
