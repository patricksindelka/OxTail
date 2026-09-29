//! Timestamp parsing hook and a small built-in fallback.

/// Parses timestamp cell text to unix nanoseconds.
///
/// `oxtail-columns` does not depend on `oxtail-time`; the application supplies
/// an implementation. [`Rfc3339Fallback`] is a small built-in one.
pub trait TimestampParser {
    /// Returns unix nanoseconds for `value`, or `None` if it is not a timestamp.
    fn parse_ns(&self, value: &str) -> Option<i64>;
}

/// Built-in parser for RFC 3339 / ISO 8601 (`2026-09-29T10:00:00Z`,
/// `2026-09-29 10:00:00.123`, `2026-09-29T10:00`, `2026-09-29`), Common Log
/// Format (`29/Sep/2026:10:00:00 +0000`) and epoch numbers (10/13/16/19
/// digits). Values without an offset are taken as UTC.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rfc3339Fallback;

impl TimestampParser for Rfc3339Fallback {
    fn parse_ns(&self, value: &str) -> Option<i64> {
        parse_fallback(value)
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn num(&mut self, min: usize, max: usize) -> Option<i64> {
        let start = self.i;
        let mut v: i64 = 0;
        while self.i < self.b.len() && self.i - start < max && self.b[self.i].is_ascii_digit() {
            v = v * 10 + i64::from(self.b[self.i] - b'0');
            self.i += 1;
        }
        (self.i - start >= min).then_some(v)
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
}

fn to_ns(
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    frac_ns: i64,
    off_s: i64,
) -> Option<i64> {
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let secs = days
        .checked_mul(86_400)?
        .checked_add(h * 3600 + mi * 60 + s)?
        .checked_sub(off_s)?;
    secs.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

fn parse_offset(c: &mut Cursor<'_>) -> Option<i64> {
    match c.peek() {
        None => Some(0),
        Some(b'Z') | Some(b'z') => {
            c.i += 1;
            Some(0)
        }
        Some(sg @ (b'+' | b'-')) => {
            c.i += 1;
            let hh = c.num(2, 2)?;
            c.eat(b':');
            let mm = c.num(2, 2).unwrap_or(0);
            let off = hh * 3600 + mm * 60;
            Some(if sg == b'-' { -off } else { off })
        }
        _ => None,
    }
}

fn parse_fallback(value: &str) -> Option<i64> {
    let v = value.trim().trim_matches(|c| c == '[' || c == ']');
    let b = v.as_bytes();
    if b.len() < 4 {
        return None;
    }
    if b.iter().all(u8::is_ascii_digit) {
        let n: i64 = v.parse().ok()?;
        return match b.len() {
            10 => n.checked_mul(1_000_000_000),
            13 => n.checked_mul(1_000_000),
            16 => n.checked_mul(1_000),
            19 => Some(n),
            _ => None,
        };
    }
    let mut c = Cursor { b, i: 0 };
    if b[2] == b'/' {
        // Common Log Format: 29/Sep/2026:10:00:00 +0000
        let d = c.num(2, 2)?;
        c.eat(b'/');
        let mon = v.get(c.i..c.i + 3)?;
        c.i += 3;
        let mo = [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ]
        .iter()
        .position(|m| m.eq_ignore_ascii_case(mon))? as i64
            + 1;
        c.eat(b'/');
        let y = c.num(4, 4)?;
        c.eat(b':');
        let h = c.num(2, 2)?;
        c.eat(b':');
        let mi = c.num(2, 2)?;
        c.eat(b':');
        let s = c.num(2, 2)?;
        while c.eat(b' ') {}
        let off = parse_offset(&mut c)?;
        return if c.done() {
            to_ns(y, mo, d, h, mi, s, 0, off)
        } else {
            None
        };
    }
    let y = c.num(4, 4)?;
    if !c.eat(b'-') {
        return None;
    }
    let mo = c.num(2, 2)?;
    if !c.eat(b'-') {
        return None;
    }
    let d = c.num(2, 2)?;
    if c.done() {
        return to_ns(y, mo, d, 0, 0, 0, 0, 0);
    }
    if !(c.eat(b'T') || c.eat(b't') || c.eat(b' ')) {
        return None;
    }
    let h = c.num(2, 2)?;
    if !c.eat(b':') {
        return None;
    }
    let mi = c.num(2, 2)?;
    let mut s = 0;
    let mut frac = 0i64;
    if c.eat(b':') {
        s = c.num(2, 2)?;
        if c.eat(b'.') || c.eat(b',') {
            let start = c.i;
            let mut digits = 0;
            while let Some(d) = c.peek().filter(u8::is_ascii_digit) {
                if digits < 9 {
                    frac = frac * 10 + i64::from(d - b'0');
                    digits += 1;
                }
                c.i += 1;
            }
            if c.i == start {
                return None;
            }
            for _ in digits..9 {
                frac *= 10;
            }
        }
    }
    while c.eat(b' ') {}
    let off = parse_offset(&mut c)?;
    if !c.done() {
        return None;
    }
    to_ns(y, mo, d, h, mi, s, frac, off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<i64> {
        Rfc3339Fallback.parse_ns(s)
    }

    #[test]
    fn formats() {
        assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(p("1970-01-01 00:00:01.5"), Some(1_500_000_000));
        assert_eq!(p("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(p("2026-09-29T10:00"), p("2026-09-29T10:00:00Z"));
        assert_eq!(p("2026-09-29"), p("2026-09-29T00:00:00"));
        assert_eq!(p("29/Sep/2026:10:00:00 +0000"), p("2026-09-29T10:00:00Z"));
        assert_eq!(p("1700000000"), Some(1_700_000_000_000_000_000));
        assert_eq!(p("2026-09-29,10"), None);
        assert_eq!(p("2026-02-30"), None);
        assert_eq!(p("hello"), None);
        assert_eq!(p("2026-09-29 10:00:00,123"), p("2026-09-29T10:00:00.123Z"));
    }
}
