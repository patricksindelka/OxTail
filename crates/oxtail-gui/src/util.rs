//! Small formatting helpers for the status bar and find bar.

/// Formats `n` with thousands separators: `1,204,331`.
pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Formats a byte count: `512 B`, `3.2 GB` (powers of 1024, labelled like
/// file managers do).
pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.2} {}", UNITS[unit])
    }
}

/// The find bar's count text: `"3 of ≥ 58,311"`, `"58,311 matches"`,
/// `"No matches"`. `rank` is the position of the current match among the
/// matches found so far.
pub fn find_count_label(found: u64, done: bool, rank: Option<usize>) -> String {
    let more = if done { "" } else { "\u{2265} " };
    match (found, rank) {
        (0, _) if done => "No matches".to_string(),
        (0, _) => "Searching\u{2026}".to_string(),
        (n, Some(r)) => format!("{} of {more}{}", fmt_count(r as u64), fmt_count(n)),
        (n, None) => format!("{more}{} matches", fmt_count(n)),
    }
}

/// Shortens `s` to at most `max` characters, adding an ellipsis.
pub fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(41_203_118), "41,203,118");
        assert_eq!(fmt_count(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn byte_sizes_are_human_readable() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1023), "1023 B");
        assert_eq!(fmt_bytes(1024), "1.00 KB");
        assert_eq!(fmt_bytes(1536), "1.50 KB");
        assert_eq!(fmt_bytes(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(fmt_bytes(3_435_973_837), "3.20 GB");
        assert!(fmt_bytes(u64::MAX).ends_with("PB"));
    }

    #[test]
    fn find_labels() {
        assert_eq!(find_count_label(0, true, None), "No matches");
        assert_eq!(find_count_label(0, false, None), "Searching\u{2026}");
        assert_eq!(
            find_count_label(58_311, false, Some(1204)),
            "1,204 of \u{2265} 58,311"
        );
        assert_eq!(find_count_label(5, true, Some(2)), "2 of 5");
        assert_eq!(find_count_label(5, true, None), "5 matches");
        assert_eq!(find_count_label(5, false, None), "\u{2265} 5 matches");
    }

    #[test]
    fn shortening() {
        assert_eq!(shorten("abc", 5), "abc");
        assert_eq!(shorten("abcdef", 4), "abc\u{2026}");
        assert_eq!(shorten("äöüß", 2), "ä\u{2026}");
    }
}
