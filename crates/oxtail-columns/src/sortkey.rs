//! Typed sort keys for sorting rows by a column (PLAN.md section 8.3).
//!
//! [`sort_key`] turns a cell into a [`SortKey`] according to the column's
//! [`ColumnKind`]: numbers, durations and sizes compare by value, timestamps
//! by instant, levels by severity and text case-insensitively first, then
//! case-sensitively. A missing, empty or unparseable cell is
//! [`SortKey::Missing`], which [`SortKey::cmp_directed`] places after every
//! value in **both** directions.

use std::cmp::Ordering;

use crate::level::Level;
use crate::model::ColumnKind;
use crate::quantity::{parse_bytes, parse_duration_ns, parse_quantity};
use crate::timestamp::{Rfc3339Fallback, TimestampParser};

/// Longest text (in characters) that takes part in a text comparison. Longer
/// cells are compared by their first characters only, which bounds the memory
/// of a sort over many rows.
pub const MAX_KEY_CHARS: usize = 64;

/// A finite `f64` with a total order (`-0.0` equals `0.0`).
#[derive(Debug, Clone, Copy)]
pub struct Num(f64);

impl Num {
    fn new(v: f64) -> Option<Num> {
        // NaN and infinities are not values; `+ 0.0` turns -0.0 into 0.0.
        v.is_finite().then_some(Num(v + 0.0))
    }

    /// The value.
    pub fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for Num {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Num {}

impl PartialOrd for Num {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Num {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// A text sort key: the case-folded text, plus the text as written only when
/// it differs (most cells are already lower case), which halves the memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextKey {
    folded: Box<str>,
    /// `None` when equal to `folded`.
    raw: Option<Box<str>>,
}

impl TextKey {
    fn new(cut: &str) -> Self {
        let folded = cut.to_lowercase();
        let raw = (folded != cut).then(|| cut.into());
        TextKey {
            folded: folded.into_boxed_str(),
            raw,
        }
    }

    /// The text as written (cut to [`MAX_KEY_CHARS`]).
    pub fn raw(&self) -> &str {
        self.raw.as_deref().unwrap_or(&self.folded)
    }
}

impl PartialOrd for TextKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TextKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.folded
            .cmp(&other.folded)
            .then_with(|| self.raw().cmp(other.raw()))
    }
}

/// What a cell sorts by. Keys of different variants never occur in one
/// column; among themselves they follow the derived order, and
/// [`SortKey::Missing`] is the largest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    /// A number, duration (nanoseconds) or size (bytes).
    Num(Num),
    /// A timestamp (unix nanoseconds) or level rank.
    Int(i64),
    /// Text: lower-cased first, the original second.
    Text(TextKey),
    /// No usable value.
    Missing,
}

impl SortKey {
    /// Whether the cell had no usable value.
    pub fn is_missing(&self) -> bool {
        matches!(self, SortKey::Missing)
    }

    /// Compares for a sort in the given direction. Missing keys are equal to
    /// each other and come after every value whatever the direction; equal
    /// keys stay [`Ordering::Equal`] so a stable sort keeps their order.
    pub fn cmp_directed(&self, other: &SortKey, descending: bool) -> Ordering {
        match (self.is_missing(), other.is_missing()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let o = self.cmp(other);
                if descending { o.reverse() } else { o }
            }
        }
    }
}

fn text_key(cell: &str) -> SortKey {
    let cut = match cell.char_indices().nth(MAX_KEY_CHARS) {
        Some((i, _)) => &cell[..i],
        None => cell,
    };
    SortKey::Text(TextKey::new(cut))
}

fn num_key(v: Option<f64>) -> SortKey {
    v.and_then(Num::new).map_or(SortKey::Missing, SortKey::Num)
}

/// The sort key of `cell`, a value of a column of `kind`.
///
/// `ts` reads timestamp cells; without one the built-in RFC 3339 / epoch
/// fallback is used. Empty cells and values that do not parse as their kind
/// are [`SortKey::Missing`]. Never panics.
pub fn sort_key(kind: ColumnKind, cell: &str, ts: Option<&dyn TimestampParser>) -> SortKey {
    let cell = cell.trim();
    if cell.is_empty() {
        return SortKey::Missing;
    }
    match kind {
        ColumnKind::Text | ColumnKind::Json => text_key(cell),
        ColumnKind::Number => num_key(
            cell.parse::<f64>()
                .ok()
                .or_else(|| parse_quantity(cell).map(|q| q.canonical())),
        ),
        ColumnKind::Duration => num_key(parse_duration_ns(cell)),
        ColumnKind::Bytes => num_key(parse_bytes(cell)),
        ColumnKind::Timestamp => {
            let ns = match ts {
                Some(p) => p.parse_ns(cell),
                None => Rfc3339Fallback.parse_ns(cell),
            };
            ns.map_or(SortKey::Missing, SortKey::Int)
        }
        ColumnKind::Level => {
            Level::parse(cell).map_or(SortKey::Missing, |l| SortKey::Int(l as i64))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(kind: ColumnKind, s: &str) -> SortKey {
        sort_key(kind, s, None)
    }

    fn sorted(kind: ColumnKind, cells: &[&str], desc: bool) -> Vec<String> {
        let mut v: Vec<(SortKey, &str)> = cells.iter().map(|c| (k(kind, c), *c)).collect();
        v.sort_by(|a, b| a.0.cmp_directed(&b.0, desc));
        v.into_iter().map(|(_, c)| c.to_string()).collect()
    }

    #[test]
    fn numbers_compare_by_value_not_text() {
        let cells = ["10", "9", "-3", "2.5", "1e2", "0"];
        assert_eq!(
            sorted(ColumnKind::Number, &cells, false),
            ["-3", "0", "2.5", "9", "10", "1e2"]
        );
        assert_eq!(k(ColumnKind::Number, "-0"), k(ColumnKind::Number, "0"));
    }

    #[test]
    fn durations_and_sizes_use_their_units() {
        assert_eq!(
            sorted(
                ColumnKind::Duration,
                &["1s", "250ms", "3", "2m", "1500us"],
                false
            ),
            ["1500us", "3", "250ms", "1s", "2m"]
        );
        assert_eq!(
            sorted(ColumnKind::Bytes, &["1KiB", "900", "1.5MB", "2KB"], false),
            ["900", "1KiB", "2KB", "1.5MB"]
        );
    }

    #[test]
    fn timestamps_compare_by_instant() {
        let cells = [
            "2026-09-29T10:00:00+02:00", // 08:00Z
            "2026-09-29T09:00:00Z",
            "2026-09-29T08:30:00Z",
        ];
        assert_eq!(
            sorted(ColumnKind::Timestamp, &cells, false),
            [cells[0], cells[2], cells[1]]
        );
    }

    #[test]
    fn a_supplied_timestamp_parser_is_used() {
        struct Len;
        impl TimestampParser for Len {
            fn parse_ns(&self, v: &str) -> Option<i64> {
                i64::try_from(v.len()).ok()
            }
        }
        assert_eq!(
            sort_key(ColumnKind::Timestamp, "abc", Some(&Len)),
            SortKey::Int(3)
        );
    }

    #[test]
    fn levels_sort_by_severity() {
        assert_eq!(
            sorted(ColumnKind::Level, &["ERROR", "debug", "W", "INFO"], false),
            ["debug", "INFO", "W", "ERROR"]
        );
    }

    #[test]
    fn text_is_case_insensitive_then_case_sensitive() {
        assert_eq!(
            sorted(ColumnKind::Text, &["b", "B", "a", "A", "c"], false),
            ["A", "a", "B", "b", "c"]
        );
    }

    #[test]
    fn missing_sorts_last_in_both_directions() {
        let cells = ["3", "", "x", "1", "2"];
        assert_eq!(
            sorted(ColumnKind::Number, &cells, false),
            ["1", "2", "3", "", "x"]
        );
        assert_eq!(
            sorted(ColumnKind::Number, &cells, true),
            ["3", "2", "1", "", "x"]
        );
        assert!(k(ColumnKind::Text, "  ").is_missing());
        assert!(k(ColumnKind::Level, "loud").is_missing());
        assert!(k(ColumnKind::Timestamp, "yesterday").is_missing());
        assert!(k(ColumnKind::Number, "inf").is_missing());
        assert!(k(ColumnKind::Number, "NaN").is_missing());
    }

    #[test]
    fn very_long_text_is_cut() {
        let long = "x".repeat(10_000);
        match k(ColumnKind::Text, &long) {
            SortKey::Text(t) => assert_eq!(t.raw().chars().count(), MAX_KEY_CHARS),
            other => panic!("{other:?}"),
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn any_kind() -> impl Strategy<Value = ColumnKind> {
            prop::sample::select(vec![
                ColumnKind::Text,
                ColumnKind::Number,
                ColumnKind::Timestamp,
                ColumnKind::Duration,
                ColumnKind::Bytes,
                ColumnKind::Json,
                ColumnKind::Level,
            ])
        }

        /// Cells that parse as numbers, units, timestamps and levels as
        /// often as they are noise, so every kind gets real values.
        fn cell() -> impl Strategy<Value = String> {
            prop_oneof![
                "\\PC{0,12}",
                "-?[0-9]{1,4}(\\.[0-9]{1,2})?(ms|s|us|m|KB|KiB|MB)?",
                "20[0-9]{2}-(0[1-9]|1[0-2])-(0[1-9]|1[0-9])T[01][0-9]:[0-5][0-9]:[0-5][0-9]Z",
                "(warn|error|info|debug|W|E|fatal)",
            ]
        }

        proptest! {
            /// Ordering is a total order (antisymmetric, transitive, total),
            /// in both directions, for any cells.
            #[test]
            fn total_order(
                kind in any_kind(),
                a in cell(), b in cell(), c in cell(),
                desc in any::<bool>(),
            ) {
                let (x, y, z) = (k(kind, &a), k(kind, &b), k(kind, &c));
                let ab = x.cmp_directed(&y, desc);
                let ba = y.cmp_directed(&x, desc);
                prop_assert_eq!(ab, ba.reverse());
                if ab != Ordering::Greater && y.cmp_directed(&z, desc) != Ordering::Greater {
                    prop_assert_ne!(x.cmp_directed(&z, desc), Ordering::Greater);
                }
                prop_assert_eq!(x.cmp_directed(&x, desc), Ordering::Equal);
            }

            /// Integers compare like the integers they are.
            #[test]
            fn numbers_match_the_naive_comparison(a in -1_000_000i64..1_000_000, b in -1_000_000i64..1_000_000) {
                let got = k(ColumnKind::Number, &a.to_string()).cmp(&k(ColumnKind::Number, &b.to_string()));
                prop_assert_eq!(got, a.cmp(&b));
            }

            /// Durations compare like milliseconds, sizes like bytes.
            #[test]
            fn durations_and_sizes_match_the_naive_comparison(a in 0u32..100_000, b in 0u32..100_000) {
                let d = k(ColumnKind::Duration, &format!("{a}ms")).cmp(&k(ColumnKind::Duration, &format!("{b}ms")));
                prop_assert_eq!(d, a.cmp(&b));
                let s = k(ColumnKind::Bytes, &format!("{a}KiB")).cmp(&k(ColumnKind::Bytes, &format!("{b}KiB")));
                prop_assert_eq!(s, a.cmp(&b));
            }

            /// Missing is never before a value, whichever way the sort goes.
            #[test]
            fn missing_is_always_last(a in "[0-9]{1,6}", desc in any::<bool>()) {
                let v = k(ColumnKind::Number, &a);
                let m = k(ColumnKind::Number, "");
                prop_assert_eq!(v.cmp_directed(&m, desc), Ordering::Less);
                prop_assert_eq!(m.cmp_directed(&v, desc), Ordering::Greater);
            }

            /// Text order agrees with (lower-case, original) comparison.
            #[test]
            fn text_matches_the_naive_comparison(a in "[a-zA-Z]{1,6}", b in "[a-zA-Z]{1,6}") {
                let got = k(ColumnKind::Text, &a).cmp(&k(ColumnKind::Text, &b));
                let want = (a.to_lowercase(), &a).cmp(&(b.to_lowercase(), &b));
                prop_assert_eq!(got, want);
            }
        }
    }
}
