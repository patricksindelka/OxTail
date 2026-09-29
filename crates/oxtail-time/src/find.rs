//! Go-to-time: find the first line whose timestamp is at or after a target.
//!
//! The search is a binary search over line numbers, which is only correct if
//! timestamps are (roughly) non-decreasing. Lines without a timestamp
//! (stack-trace continuations) are skipped by probing up to [`PROBE_LIMIT`]
//! following lines. After the search a cheap *verification* checks that the
//! nearest timestamped line before the answer is earlier than the target (and,
//! when nothing was found, that the last timestamped line is earlier). If the
//! check fails the data is not monotonic and a **linear scan** from line 0
//! gives the exact answer, in O(n) line reads. Use
//! [`find_time_cancellable`] so a stale request can stop that scan.

use jiff::{SignedDuration, Timestamp};

use crate::TimeParser;

/// Backwards jumps up to this size between sampled lines are tolerated as
/// clock skew between writers and do not trigger the linear fallback.
const SKEW_TOLERANCE: SignedDuration = SignedDuration::from_secs(1);

/// How many consecutive lines are probed for a timestamp before giving up on
/// a position.
const PROBE_LIMIT: u64 = 256;

/// Random access to lines of a document. Implemented by the GUI over the
/// document's index; tests use a `Vec<String>`.
pub trait LineAccess {
    /// Number of lines.
    fn line_count(&self) -> u64;
    /// The text of line `n` (0-based) or `None` if out of range or unreadable.
    fn line(&self, n: u64) -> Option<String>;
}

impl LineAccess for Vec<String> {
    fn line_count(&self) -> u64 {
        self.len() as u64
    }
    fn line(&self, n: u64) -> Option<String> {
        usize::try_from(n).ok().and_then(|i| self.get(i)).cloned()
    }
}

/// Returned by [`find_time_cancellable`] when the cancel callback fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("search cancelled")]
pub struct Cancelled;

/// First line (0-based) whose timestamp is `>= target`, or `None` if there is
/// none. See the [module docs](self) for the algorithm and its fallback.
pub fn find_time<A: LineAccess + ?Sized>(
    access: &A,
    parser: &TimeParser,
    target: Timestamp,
) -> Option<u64> {
    find_time_cancellable(access, parser, target, &|| false).unwrap_or(None)
}

/// Like [`find_time`] but polls `is_cancelled` between line reads and returns
/// `Err(Cancelled)` as soon as it reports `true`.
pub fn find_time_cancellable<A: LineAccess + ?Sized>(
    access: &A,
    parser: &TimeParser,
    target: Timestamp,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<u64>, Cancelled> {
    let count = access.line_count();
    let ts_at = |n: u64| -> Option<Timestamp> { access.line(n).and_then(|l| parser.parse(&l)) };

    // First timestamped line in [from, to), at most PROBE_LIMIT lines.
    let probe_fwd = |from: u64, to: u64| -> Result<Option<(u64, Timestamp)>, Cancelled> {
        let end = to.min(from.saturating_add(PROBE_LIMIT));
        for n in from..end {
            if is_cancelled() {
                return Err(Cancelled);
            }
            if let Some(t) = ts_at(n) {
                return Ok(Some((n, t)));
            }
        }
        Ok(None)
    };
    // Last timestamped line in (down_to.., before], scanning backwards.
    let probe_back = |before: u64| -> Result<Option<Timestamp>, Cancelled> {
        let mut n = before;
        let mut left = PROBE_LIMIT;
        while n > 0 && left > 0 {
            n -= 1;
            left -= 1;
            if is_cancelled() {
                return Err(Cancelled);
            }
            if let Some(t) = ts_at(n) {
                return Ok(Some(t));
            }
        }
        Ok(None)
    };

    // Every timestamp seen on the way; checked for monotonicity afterwards.
    let mut samples: Vec<(u64, Timestamp)> = Vec::new();
    if let Some(first) = probe_fwd(0, count)? {
        samples.push(first);
    }
    let (mut lo, mut hi) = (0u64, count);
    let mut best: Option<u64> = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match probe_fwd(mid, hi)? {
            Some((p, t)) if t >= target => {
                samples.push((p, t));
                best = Some(p);
                // Search [lo, p): lines mid..p carry no timestamp.
                hi = p;
            }
            Some((p, t)) => {
                samples.push((p, t));
                lo = p + 1;
            }
            None if mid.saturating_add(PROBE_LIMIT) >= hi => hi = mid,
            None => {
                // No timestamp in the next PROBE_LIMIT lines, and more lines
                // follow: a long timestamp-less block. Decide the direction
                // from the last timestamp before the block instead of
                // discarding the upper half.
                match probe_back(mid)? {
                    Some(t) if t < target => lo = mid + PROBE_LIMIT,
                    _ => hi = mid,
                }
            }
        }
    }

    // Verification of the monotonicity assumption.
    let before = best.unwrap_or(count);
    let mut ok = match probe_back(before)? {
        Some(t) => t < target,
        None => true,
    };
    if let Some(t) = probe_back(count)? {
        samples.push((count, t));
    }
    samples.sort_by_key(|&(n, _)| n);
    ok &= samples
        .windows(2)
        .all(|w| w[1].1.duration_since(w[0].1) >= SignedDuration::ZERO - SKEW_TOLERANCE);
    if ok {
        return Ok(best);
    }
    tracing::debug!("go-to-time: timestamps not monotonic, falling back to linear scan");
    for n in 0..count {
        if n % 256 == 0 && is_cancelled() {
            return Err(Cancelled);
        }
        if ts_at(n).is_some_and(|t| t >= target) {
            return Ok(Some(n));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TimeContext;
    use jiff::tz::TimeZone;

    fn parser() -> TimeParser {
        TimeParser::new(TimeContext::utc())
    }

    fn line(secs: i64) -> String {
        let ts = Timestamp::from_second(1_790_676_062 + secs).expect("range");
        format!(
            "{} INFO message",
            crate::helpers::format_in_zone(ts, &TimeZone::UTC, "%Y-%m-%dT%H:%M:%SZ")
        )
    }

    fn target(secs: i64) -> Timestamp {
        Timestamp::from_second(1_790_676_062 + secs).expect("range")
    }

    fn naive(lines: &[String], p: &TimeParser, t: Timestamp) -> Option<u64> {
        lines
            .iter()
            .position(|l| p.parse(l).is_some_and(|x| x >= t))
            .map(|i| i as u64)
    }

    struct Counting<'a> {
        lines: &'a [String],
        reads: std::cell::Cell<u64>,
    }
    impl LineAccess for Counting<'_> {
        fn line_count(&self) -> u64 {
            self.lines.len() as u64
        }
        fn line(&self, n: u64) -> Option<String> {
            self.reads.set(self.reads.get() + 1);
            self.lines.get(usize::try_from(n).ok()?).cloned()
        }
    }

    #[test]
    fn long_timestampless_block_at_probe_point_stays_logarithmic() {
        let mut lines: Vec<String> = (0..10_000).map(|i| line(i * 10)).collect();
        for l in &mut lines[4800..5400] {
            *l = "    continuation without a timestamp".to_string();
        }
        let p = parser();
        for t in [
            target(80_000),
            target(20_000),
            target(99_990),
            target(53_500),
        ] {
            let acc = Counting {
                lines: &lines,
                reads: std::cell::Cell::new(0),
            };
            assert_eq!(find_time(&acc, &p, t), naive(&lines, &p, t));
            assert!(
                acc.reads.get() < 3_000,
                "linear fallback: {}",
                acc.reads.get()
            );
        }
    }

    #[test]
    fn simple_monotonic() {
        let lines: Vec<String> = (0..100).map(|i| line(i * 10)).collect();
        let p = parser();
        assert_eq!(find_time(&lines, &p, target(0)), Some(0));
        assert_eq!(find_time(&lines, &p, target(-5)), Some(0));
        assert_eq!(find_time(&lines, &p, target(95)), Some(10));
        assert_eq!(find_time(&lines, &p, target(990)), Some(99));
        assert_eq!(find_time(&lines, &p, target(991)), None);
    }

    #[test]
    fn continuation_lines_and_empty() {
        let mut lines = vec![];
        for i in 0..20 {
            lines.push(line(i * 10));
            lines.push("    at com.foo.Bar".to_string());
            lines.push("    at com.foo.Baz".to_string());
        }
        let p = parser();
        assert_eq!(find_time(&lines, &p, target(100)), Some(30));
        assert_eq!(find_time(&lines, &p, target(101)), Some(33));
        let empty: Vec<String> = vec![];
        assert_eq!(find_time(&empty, &p, target(0)), None);
        let none: Vec<String> = vec!["a".into(); 10];
        assert_eq!(find_time(&none, &p, target(0)), None);
    }

    #[test]
    fn duplicates_return_first() {
        let lines: Vec<String> = [0, 10, 10, 10, 10, 20].iter().map(|s| line(*s)).collect();
        assert_eq!(find_time(&lines, &parser(), target(10)), Some(1));
    }

    #[test]
    fn non_monotonic_falls_back_to_linear() {
        // A block of old timestamps in the middle: binary search alone can be fooled.
        let mut secs: Vec<i64> = (0..64).map(|i| i * 10).collect();
        for s in secs.iter_mut().skip(20).take(30) {
            *s -= 1000;
        }
        let lines: Vec<String> = secs.iter().map(|s| line(*s)).collect();
        let p = parser();
        for t in [-500, 0, 100, 250, 400, 630] {
            assert_eq!(
                find_time(&lines, &p, target(t)),
                naive(&lines, &p, target(t)),
                "t={t}"
            );
        }
    }

    #[test]
    fn cancellation() {
        let lines: Vec<String> = (0..100).map(line).collect();
        let r = find_time_cancellable(&lines, &parser(), target(50), &|| true);
        assert_eq!(r, Err(Cancelled));
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn monotonic_matches_naive(
                steps in proptest::collection::vec((0i64..30, 0usize..4), 0..300),
                t in -50i64..4000,
            ) {
                let mut lines = vec![];
                let mut now = 0;
                for (dt, cont) in steps {
                    now += dt;
                    lines.push(line(now));
                    for _ in 0..cont { lines.push("  continuation".to_string()); }
                }
                let p = parser();
                prop_assert_eq!(find_time(&lines, &p, target(t)), naive(&lines, &p, target(t)));
            }

            #[test]
            fn nearly_monotonic_is_close(
                jitters in proptest::collection::vec(-25i64..25, 1..300),
                t in -50i64..4000,
            ) {
                let lines: Vec<String> = jitters.iter().enumerate()
                    .map(|(i, j)| line(i as i64 * 10 + j)).collect();
                let p = parser();
                let got = find_time(&lines, &p, target(t));
                let want = naive(&lines, &p, target(t));
                match (got, want) {
                    (None, None) => {}
                    (Some(g), Some(w)) => {
                        prop_assert!(p.parse(&lines[g as usize]).unwrap() >= target(t));
                        prop_assert!(g.abs_diff(w) <= 6, "got {g} want {w}");
                    }
                    (g, w) => {
                        // A disagreement on existence is only tolerable at the very end.
                        let n = lines.len() as u64;
                        prop_assert!(g.or(w).unwrap() + 6 >= n, "got {g:?} want {w:?}");
                    }
                }
            }
        }
    }
}
