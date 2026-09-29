//! Parsing of the "Go to line" input: `123`, `+100`, `-50`, `50%`.

/// A parsed go-to request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Goto {
    /// An absolute, 1-based line number.
    Line(u64),
    /// Lines relative to the current line (`+100`, `-50`).
    Relative(i64),
    /// A percentage of the file (`50%`).
    Percent(f64),
}

/// Parses user input. Whitespace, `,`, `_` and `'` digit separators are
/// ignored. Returns `None` for anything else.
pub fn parse(input: &str) -> Option<Goto> {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ',' | '_' | '\''))
        .collect();
    let s = cleaned.as_str();
    if s.is_empty() {
        return None;
    }
    if let Some(p) = s.strip_suffix('%') {
        let v: f64 = p.parse().ok()?;
        return (v.is_finite() && v >= 0.0).then_some(Goto::Percent(v.min(100.0)));
    }
    if let Some(rest) = s.strip_prefix('+') {
        let n: u64 = rest.parse().ok()?;
        return Some(Goto::Relative(i64::try_from(n).unwrap_or(i64::MAX)));
    }
    if let Some(rest) = s.strip_prefix('-') {
        let n: u64 = rest.parse().ok()?;
        return Some(Goto::Relative(-i64::try_from(n).unwrap_or(i64::MAX)));
    }
    s.parse::<u64>().ok().map(Goto::Line)
}

/// The 0-based line a request selects, given the current 0-based line and the
/// number of lines. Always in `0..total` (0 for an empty document).
pub fn resolve(goto: Goto, current: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let last = total - 1;
    let line = match goto {
        Goto::Line(n) => n.saturating_sub(1),
        Goto::Relative(d) => {
            if d >= 0 {
                current.saturating_add(d as u64)
            } else {
                current.saturating_sub(d.unsigned_abs())
            }
        }
        Goto::Percent(p) => ((p / 100.0) * total as f64).floor() as u64,
    };
    line.min(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_forms() {
        assert_eq!(parse("123"), Some(Goto::Line(123)));
        assert_eq!(parse(" 1,234 "), Some(Goto::Line(1234)));
        assert_eq!(parse("+100"), Some(Goto::Relative(100)));
        assert_eq!(parse("-50"), Some(Goto::Relative(-50)));
        assert_eq!(parse("50%"), Some(Goto::Percent(50.0)));
        assert_eq!(parse("12.5 %"), Some(Goto::Percent(12.5)));
        assert_eq!(parse("250%"), Some(Goto::Percent(100.0)));
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "", "  ", "abc", "+", "-", "%", "1.5", "--3", "12x", "-5%", "+-1",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
        assert_eq!(parse("99999999999999999999999"), None);
    }

    #[test]
    fn resolves_to_zero_based_clamped_lines() {
        assert_eq!(resolve(Goto::Line(1), 500, 1000), 0);
        assert_eq!(resolve(Goto::Line(0), 500, 1000), 0);
        assert_eq!(resolve(Goto::Line(1000), 0, 1000), 999);
        assert_eq!(resolve(Goto::Line(5000), 0, 1000), 999);
        assert_eq!(resolve(Goto::Relative(100), 10, 1000), 110);
        assert_eq!(resolve(Goto::Relative(-50), 10, 1000), 0);
        assert_eq!(resolve(Goto::Relative(i64::MAX), 10, 1000), 999);
        assert_eq!(resolve(Goto::Percent(50.0), 0, 1000), 500);
        assert_eq!(resolve(Goto::Percent(100.0), 0, 1000), 999);
        assert_eq!(resolve(Goto::Percent(0.0), 7, 1000), 0);
        assert_eq!(resolve(Goto::Line(5), 0, 0), 0);
    }
}
