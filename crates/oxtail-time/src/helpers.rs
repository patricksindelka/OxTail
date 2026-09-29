//! Gap detection and time formatting helpers.

use jiff::{SignedDuration, Timestamp, fmt::strtime, tz::TimeZone};

/// Default display pattern: `2026-09-29 10:01:02.123`.
pub const DEFAULT_PATTERN: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// A stall between two consecutive lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// How long the pause lasted.
    pub duration: SignedDuration,
}

/// Returns the gap between `prev` and `cur` if it is at least `threshold`.
/// Backwards jumps (non-monotonic logs) are not gaps.
pub fn detect_gap(prev: Timestamp, cur: Timestamp, threshold: SignedDuration) -> Option<Gap> {
    let d = cur.duration_since(prev);
    (d >= threshold && d.is_positive()).then_some(Gap { duration: d })
}

/// Formats a signed delta compactly: `+00:00.153` (under 1 h, `mm:ss.mmm`),
/// `-1h02m` (under 1 day) and `+2d03h` beyond that.
pub fn format_relative(delta: SignedDuration) -> String {
    let sign = if delta.is_negative() { '-' } else { '+' };
    let total_ms = delta.as_millis().unsigned_abs();
    let total_s = total_ms / 1000;
    if total_s < 3600 {
        format!(
            "{sign}{:02}:{:02}.{:03}",
            total_s / 60,
            total_s % 60,
            total_ms % 1000
        )
    } else if total_s < 86_400 {
        format!("{sign}{}h{:02}m", total_s / 3600, (total_s % 3600) / 60)
    } else {
        format!(
            "{sign}{}d{:02}h",
            total_s / 86_400,
            (total_s % 86_400) / 3600
        )
    }
}

/// Relative time from `earlier` to `ts` using [`format_relative`].
pub fn format_relative_between(earlier: Timestamp, ts: Timestamp) -> String {
    format_relative(ts.duration_since(earlier))
}

/// Formats `ts` in `tz` with a `strftime`-style `pattern` (jiff syntax, e.g.
/// `%H:%M:%S%.3f`). An invalid pattern falls back to [`DEFAULT_PATTERN`].
pub fn format_in_zone(ts: Timestamp, tz: &TimeZone, pattern: &str) -> String {
    let z = ts.to_zoned(tz.clone());
    strtime::format(pattern, &z)
        .or_else(|_| strtime::format(DEFAULT_PATTERN, &z))
        .unwrap_or_else(|_| ts.to_string())
}

/// Formats `ts` in UTC with `pattern`.
pub fn format_utc(ts: Timestamp, pattern: &str) -> String {
    format_in_zone(ts, &TimeZone::UTC, pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: i64, ms: i64) -> Timestamp {
        Timestamp::from_millisecond(s * 1000 + ms).expect("in range")
    }

    #[test]
    fn relative() {
        let ms = SignedDuration::from_millis;
        assert_eq!(format_relative(ms(153)), "+00:00.153");
        assert_eq!(format_relative(ms(-153)), "-00:00.153");
        assert_eq!(format_relative(ms(61_500)), "+01:01.500");
        assert_eq!(format_relative(ms(-(3600 + 120) * 1000)), "-1h02m");
        assert_eq!(
            format_relative(ms((2 * 86_400 + 3 * 3600) * 1000)),
            "+2d03h"
        );
        assert_eq!(format_relative(SignedDuration::ZERO), "+00:00.000");
        assert_eq!(
            format_relative(SignedDuration::MIN).chars().next(),
            Some('-')
        );
    }

    #[test]
    fn gaps() {
        let th = SignedDuration::from_secs(5);
        assert!(detect_gap(ts(0, 0), ts(4, 999), th).is_none());
        assert_eq!(
            detect_gap(ts(0, 0), ts(5, 0), th).map(|g| g.duration),
            Some(th)
        );
        assert!(detect_gap(ts(10, 0), ts(0, 0), th).is_none());
    }

    #[test]
    fn formatting() {
        let t = ts(1_790_676_062, 123);
        assert_eq!(format_utc(t, DEFAULT_PATTERN), "2026-09-29 10:01:02.123");
        let ams = TimeZone::get("Europe/Amsterdam").expect("bundled");
        assert_eq!(format_in_zone(t, &ams, "%H:%M:%S %Z"), "12:01:02 CEST");
        // Invalid pattern falls back.
        assert_eq!(format_utc(t, "%Q%"), "2026-09-29 10:01:02.123");
    }
}
