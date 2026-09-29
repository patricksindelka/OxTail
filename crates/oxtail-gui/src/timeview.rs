//! Time in the log view: the relative-time gutter ("+00:00.153 since the
//! previous line / since the selected line") and gap separators between
//! lines that are far apart in time. Pure functions over timestamps, so the
//! rules are unit-tested; `logview` only draws what they return.

use std::collections::HashMap;

use oxtail_time::helpers::{detect_gap, format_relative_between};
use oxtail_time::jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// What the relative-time gutter shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelMode {
    /// No relative-time column.
    #[default]
    Off,
    /// Time since the previous line.
    Previous,
    /// Time since the selected line.
    Selected,
}

impl RelMode {
    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            RelMode::Off => "Off",
            RelMode::Previous => "Since the previous line",
            RelMode::Selected => "Since the selected line",
        }
    }
}

/// Characters reserved for the relative-time column (`+00:00.153`, `-1h02m`,
/// `+2d03h`).
pub const REL_CHARS: usize = 10;

/// The gutter text of a row: the time from the reference line to this one.
/// `None` when either timestamp is unknown.
pub fn relative_label(
    mode: RelMode,
    cur: Option<Timestamp>,
    prev: Option<Timestamp>,
    selected: Option<Timestamp>,
) -> Option<String> {
    let cur = cur?;
    let reference = match mode {
        RelMode::Off => return None,
        RelMode::Previous => prev?,
        RelMode::Selected => selected?,
    };
    Some(format_relative_between(reference, cur))
}

/// The gap between two consecutive rows when it is at least `threshold_secs`
/// (`0` or negative disables the check).
pub fn gap_between(
    prev: Option<Timestamp>,
    cur: Option<Timestamp>,
    threshold_secs: f64,
) -> Option<SignedDuration> {
    if !threshold_secs.is_finite() || threshold_secs <= 0.0 {
        return None;
    }
    let threshold = SignedDuration::try_from_secs_f64(threshold_secs).ok()?;
    detect_gap(prev?, cur?, threshold).map(|g| g.duration)
}

/// Formats a gap for the separator: `2m 05s`, `1h 03m`, `3d 04h`, `450 ms`.
pub fn format_gap(d: SignedDuration) -> String {
    let ms = d.as_millis().unsigned_abs();
    let secs = ms / 1000;
    if secs == 0 {
        format!("{ms} ms")
    } else if secs < 60 {
        format!("{secs}.{:01} s", (ms % 1000) / 100)
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// A bounded cache of parsed timestamps by line offset.
#[derive(Debug, Default)]
pub struct TimeCache {
    map: HashMap<u64, Option<Timestamp>>,
}

impl TimeCache {
    /// Most entries kept.
    pub const LIMIT: usize = 50_000;

    /// The cached timestamp of the line at `offset` (outer `None`: unknown).
    pub fn get(&self, offset: u64) -> Option<Option<Timestamp>> {
        self.map.get(&offset).copied()
    }

    /// Stores a timestamp (or the fact that the line has none).
    pub fn put(&mut self, offset: u64, ts: Option<Timestamp>) {
        if self.map.len() >= Self::LIMIT {
            self.map.clear();
        }
        self.map.insert(offset, ts);
    }

    /// Forgets everything (new time parser, new generation).
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Default threshold used when the gap toggle is switched on.
pub const DEFAULT_GAP_SECS: f64 = 5.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: i64, ms: i64) -> Option<Timestamp> {
        Some(Timestamp::from_millisecond(s * 1000 + ms).unwrap())
    }

    #[test]
    fn relative_labels_by_mode() {
        let (a, b, sel) = (ts(0, 0), ts(0, 153), ts(-2, 0));
        assert_eq!(
            relative_label(RelMode::Previous, b, a, sel).as_deref(),
            Some("+00:00.153")
        );
        assert_eq!(
            relative_label(RelMode::Selected, b, a, sel).as_deref(),
            Some("+00:02.153")
        );
        // Earlier than the reference: negative.
        assert_eq!(
            relative_label(RelMode::Selected, sel, a, b).as_deref(),
            Some("-00:02.153")
        );
        assert_eq!(relative_label(RelMode::Off, b, a, sel), None);
        // Missing timestamps give no label.
        assert_eq!(relative_label(RelMode::Previous, b, None, sel), None);
        assert_eq!(relative_label(RelMode::Previous, None, a, sel), None);
        assert_eq!(relative_label(RelMode::Selected, b, a, None), None);
    }

    #[test]
    fn gaps_need_a_threshold_and_forward_time() {
        assert!(gap_between(ts(0, 0), ts(4, 999), 5.0).is_none());
        assert_eq!(
            gap_between(ts(0, 0), ts(5, 0), 5.0),
            Some(SignedDuration::from_secs(5))
        );
        // Off, backwards jumps and unknown times are not gaps.
        assert!(gap_between(ts(0, 0), ts(100, 0), 0.0).is_none());
        assert!(gap_between(ts(0, 0), ts(100, 0), f64::NAN).is_none());
        assert!(gap_between(ts(100, 0), ts(0, 0), 5.0).is_none());
        assert!(gap_between(None, ts(9, 0), 5.0).is_none());
        // A silly threshold never panics.
        assert!(gap_between(ts(0, 0), ts(1, 0), 1e300).is_none());
    }

    #[test]
    fn gaps_are_formatted_compactly() {
        let ms = SignedDuration::from_millis;
        assert_eq!(format_gap(ms(450)), "450 ms");
        assert_eq!(format_gap(ms(5_300)), "5.3 s");
        assert_eq!(format_gap(ms(125_000)), "2m 05s");
        assert_eq!(format_gap(ms(3_780_000)), "1h 03m");
        assert_eq!(format_gap(ms(3 * 86_400_000 + 4 * 3_600_000)), "3d 04h");
    }

    #[test]
    fn the_cache_is_bounded_and_distinguishes_unknown_from_none() {
        let mut c = TimeCache::default();
        assert_eq!(c.get(1), None);
        c.put(1, None);
        c.put(2, ts(1, 0));
        assert_eq!(c.get(1), Some(None));
        assert_eq!(c.get(2), Some(ts(1, 0)));
        for i in 0..(TimeCache::LIMIT as u64 + 10) {
            c.put(i, None);
        }
        assert!(c.len() <= TimeCache::LIMIT);
        c.clear();
        assert!(c.is_empty());
    }
}
