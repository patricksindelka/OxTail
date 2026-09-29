//! Minimap binning: search matches and rule hits as ticks positioned by byte
//! fraction, density-binned into one bin per pixel row.
//!
//! Match counts come from `MatchSet::rank`, so a bin costs two binary searches
//! no matter how many matches there are (a 10M-match set bins as fast as a
//! 10-match one).

use oxtail_search::MatchSet;

/// Number of matches whose line starts inside each of `bins` equal slices of
/// `0..utf8_len`.
pub fn bin_matches(set: &MatchSet, utf8_len: u64, bins: usize) -> Vec<u32> {
    if bins == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(bins);
    if utf8_len == 0 {
        out.resize(bins, 0);
        return out;
    }
    let mut prev = set.rank(0);
    for i in 1..=bins {
        let edge = if i == bins {
            u64::MAX
        } else {
            bin_edge(i, bins, utf8_len)
        };
        let r = set.rank(edge);
        out.push(u32::try_from(r.saturating_sub(prev)).unwrap_or(u32::MAX));
        prev = r;
    }
    out
}

/// The first byte offset of bin `i` of `bins`, i.e. the smallest offset `o`
/// with `bin_of(o) >= i` (a ceiling, so it agrees with [`bin_of`]).
fn bin_edge(i: usize, bins: usize, utf8_len: u64) -> u64 {
    (i as u128 * utf8_len as u128).div_ceil(bins as u128) as u64
}

/// The bin that holds `offset`.
pub fn bin_of(offset: u64, utf8_len: u64, bins: usize) -> usize {
    if bins == 0 || utf8_len == 0 {
        return 0;
    }
    let b = (offset as u128 * bins as u128) / utf8_len as u128;
    (b as usize).min(bins - 1)
}

/// Bins `(offset, tag)` points. Each bin holds the number of points in it and
/// the tag of the last one.
pub fn bin_points<T: Copy>(
    points: impl IntoIterator<Item = (u64, T)>,
    utf8_len: u64,
    bins: usize,
) -> Vec<Option<(u32, T)>> {
    let mut out: Vec<Option<(u32, T)>> = vec![None; bins];
    if bins == 0 {
        return out;
    }
    for (off, tag) in points {
        let b = bin_of(off, utf8_len, bins);
        out[b] = Some(match out[b] {
            Some((n, _)) => (n.saturating_add(1), tag),
            None => (1, tag),
        });
    }
    out
}

/// Visual weight in `0.0..=1.0` of a bin with `count` matches when the
/// busiest bin has `max`. Any match is at least faintly visible.
pub fn intensity(count: u32, max: u32) -> f32 {
    if count == 0 || max == 0 {
        return 0.0;
    }
    let f = (count as f32 / max as f32).sqrt();
    f.clamp(0.35, 1.0)
}

/// The byte fraction (`0.0..=1.0`) that a click at `y` on a minimap spanning
/// `top..top + height` selects.
pub fn click_fraction(y: f32, top: f32, height: f32) -> f64 {
    if height <= 0.0 {
        return 0.0;
    }
    (((y - top) / height) as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(offsets: &[u64], len: u64, bins: usize) -> Vec<u32> {
        let mut v = vec![0u32; bins];
        for &o in offsets {
            v[bin_of(o, len, bins)] += 1;
        }
        v
    }

    #[test]
    fn matches_are_binned_by_position() {
        let set = MatchSet::from_offsets(vec![0, 5, 10, 55, 99]);
        let bins = bin_matches(&set, 100, 10);
        assert_eq!(bins, vec![2, 1, 0, 0, 0, 1, 0, 0, 0, 1]);
        assert_eq!(bins.iter().sum::<u32>(), 5);
    }

    #[test]
    fn binning_agrees_with_a_naive_reference() {
        let len = 10_007u64;
        let offsets: Vec<u64> = (0..2000).map(|i| (i * 37 + (i * i) % 11) % len).collect();
        let set = MatchSet::from_offsets(offsets.clone());
        let mut dedup = offsets;
        dedup.sort_unstable();
        dedup.dedup();
        for bins in [1usize, 3, 17, 64, 500] {
            assert_eq!(
                bin_matches(&set, len, bins),
                naive(&dedup, len, bins),
                "bins={bins}"
            );
        }
    }

    #[test]
    fn degenerate_inputs() {
        let set = MatchSet::from_offsets(vec![1, 2, 3]);
        assert!(bin_matches(&set, 100, 0).is_empty());
        assert_eq!(bin_matches(&set, 0, 4), vec![0; 4]);
        assert_eq!(bin_matches(&MatchSet::new(), 100, 4), vec![0; 4]);
        // A match at or beyond the end lands in the last bin.
        let set = MatchSet::from_offsets(vec![100, 250]);
        assert_eq!(bin_matches(&set, 100, 4), vec![0, 0, 0, 2]);
    }

    #[test]
    fn huge_files_do_not_overflow() {
        let len = u64::MAX / 2;
        let set = MatchSet::from_offsets(vec![0, len / 2, len - 1]);
        let bins = bin_matches(&set, len, 10);
        assert_eq!(bins.iter().sum::<u32>(), 3);
        assert_eq!(bin_of(len - 1, len, 10), 9);
    }

    #[test]
    fn points_keep_the_last_tag_and_a_count() {
        let pts = vec![(5u64, 'a'), (6, 'b'), (95, 'c')];
        let bins = bin_points(pts, 100, 10);
        assert_eq!(bins[0], Some((2, 'b')));
        assert_eq!(bins[9], Some((1, 'c')));
        assert!(bins[1..9].iter().all(Option::is_none));
        assert!(bin_points(vec![(1u64, 0u8)], 100, 0).is_empty());
    }

    #[test]
    fn intensity_is_monotonic_and_visible() {
        assert_eq!(intensity(0, 10), 0.0);
        assert_eq!(intensity(10, 10), 1.0);
        assert!(intensity(1, 10_000) >= 0.35);
        assert!(intensity(5, 10) < intensity(9, 10));
        assert_eq!(intensity(3, 0), 0.0);
    }

    #[test]
    fn clicks_map_to_fractions() {
        assert_eq!(click_fraction(50.0, 0.0, 100.0), 0.5);
        assert_eq!(click_fraction(-5.0, 0.0, 100.0), 0.0);
        assert_eq!(click_fraction(500.0, 0.0, 100.0), 1.0);
        assert_eq!(click_fraction(10.0, 0.0, 0.0), 0.0);
        assert_eq!(click_fraction(110.0, 10.0, 200.0), 0.5);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Binning through `rank` agrees with counting every offset.
            #[test]
            fn binning_matches_the_naive_count(
                offsets in proptest::collection::vec(0u64..5000, 0..300),
                len in 1u64..6000,
                bins in 1usize..200,
            ) {
                let set = MatchSet::from_offsets(offsets.clone());
                let mut dedup = offsets;
                dedup.sort_unstable();
                dedup.dedup();
                let mut want = vec![0u32; bins];
                for o in dedup {
                    want[bin_of(o, len, bins)] += 1;
                }
                prop_assert_eq!(bin_matches(&set, len, bins), want);
            }
        }
    }
}
