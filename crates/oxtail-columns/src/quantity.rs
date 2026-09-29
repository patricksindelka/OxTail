//! Numbers with optional duration / size units (`250ms`, `1.5GiB`).

/// The unit family and scale of a [`Quantity`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    /// A duration unit; the payload is nanoseconds per unit.
    Duration(f64),
    /// A size unit; the payload is bytes per unit.
    Size(f64),
}

/// A parsed number with an optional unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantity {
    /// The numeric part (sign included).
    pub value: f64,
    /// The unit, if a suffix was present.
    pub unit: Option<Unit>,
    /// The explicit sign character (`+` or `-`) if one was written.
    pub sign: Option<char>,
}

fn unit_of(s: &str) -> Option<Unit> {
    let l = s.to_lowercase();
    Some(match l.as_str() {
        "ns" => Unit::Duration(1.0),
        "us" | "µs" | "μs" => Unit::Duration(1e3),
        "ms" => Unit::Duration(1e6),
        "s" | "sec" | "secs" => Unit::Duration(1e9),
        "m" | "min" | "mins" => Unit::Duration(6e10),
        "h" | "hr" | "hrs" => Unit::Duration(3.6e12),
        "d" => Unit::Duration(8.64e13),
        "b" => Unit::Size(1.0),
        "kb" => Unit::Size(1e3),
        "mb" => Unit::Size(1e6),
        "gb" => Unit::Size(1e9),
        "tb" => Unit::Size(1e12),
        "kib" => Unit::Size(1024.0),
        "mib" => Unit::Size(1024.0 * 1024.0),
        "gib" => Unit::Size(1024.0 * 1024.0 * 1024.0),
        "tib" => Unit::Size(1024.0 * 1024.0 * 1024.0 * 1024.0),
        _ => return None,
    })
}

/// Parses `[+-]digits[.digits][unit]`. Returns `None` for anything else
/// (including unknown unit suffixes and non-finite numbers).
pub fn parse_quantity(s: &str) -> Option<Quantity> {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut sign = None;
    if let Some(&b) = bytes.first() {
        if b == b'+' || b == b'-' {
            sign = Some(b as char);
            i = 1;
        }
    }
    let num_start = i;
    let mut seen_digit = false;
    let mut seen_dot = false;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => seen_digit = true,
            b'.' if !seen_dot => seen_dot = true,
            _ => break,
        }
        i += 1;
    }
    if !seen_digit {
        return None;
    }
    let value: f64 = s[num_start..i].parse().ok()?;
    let value = if sign == Some('-') { -value } else { value };
    if !value.is_finite() {
        return None;
    }
    let rest = s[i..].trim_start();
    let unit = if rest.is_empty() {
        None
    } else {
        Some(unit_of(rest)?)
    };
    Some(Quantity { value, unit, sign })
}

impl Quantity {
    /// Canonical value: nanoseconds for durations, bytes for sizes, the raw
    /// number when there is no unit.
    pub fn canonical(&self) -> f64 {
        match self.unit {
            Some(Unit::Duration(s)) | Some(Unit::Size(s)) => self.value * s,
            None => self.value,
        }
    }
}

/// Converts two quantities to a common scale for comparison.
///
/// A unit-less side takes the unit family of the other (milliseconds for
/// durations, bytes for sizes). Two unit-less numbers compare raw. Mixing a
/// duration with a size returns `None`.
pub fn canonical_pair(a: &Quantity, b: &Quantity) -> Option<(f64, f64)> {
    let conv = |q: &Quantity, family: Option<Unit>| -> f64 {
        match (q.unit, family) {
            (Some(_), _) => q.canonical(),
            (None, Some(Unit::Duration(_))) => q.value * 1e6,
            (None, _) => q.value,
        }
    };
    match (a.unit, b.unit) {
        (Some(Unit::Duration(_)), Some(Unit::Size(_)))
        | (Some(Unit::Size(_)), Some(Unit::Duration(_))) => None,
        (None, None) => Some((a.value, b.value)),
        (Some(f), _) | (None, Some(f)) => Some((conv(a, Some(f)), conv(b, Some(f)))),
    }
}

/// Parses a duration cell to nanoseconds. A bare number means milliseconds.
pub fn parse_duration_ns(s: &str) -> Option<f64> {
    let q = parse_quantity(s)?;
    match q.unit {
        Some(Unit::Duration(_)) => Some(q.canonical()),
        None => Some(q.value * 1e6),
        Some(Unit::Size(_)) => None,
    }
}

/// Parses a size cell to bytes. A bare number means bytes.
pub fn parse_bytes(s: &str) -> Option<f64> {
    let q = parse_quantity(s)?;
    match q.unit {
        Some(Unit::Size(_)) | None => Some(q.canonical()),
        Some(Unit::Duration(_)) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        let q = parse_quantity("250ms").unwrap();
        assert_eq!(q.canonical(), 250e6);
        assert_eq!(parse_quantity("1.5 KiB").unwrap().canonical(), 1536.0);
        assert_eq!(parse_quantity("-1h").unwrap().sign, Some('-'));
        assert_eq!(parse_quantity("500").unwrap().unit, None);
        assert!(parse_quantity("12xyz").is_none());
        assert!(parse_quantity("abc").is_none());
        assert!(parse_quantity("").is_none());
        assert!(parse_quantity("-").is_none());
    }

    #[test]
    fn pairs() {
        let a = parse_quantity("1s").unwrap();
        let b = parse_quantity("250").unwrap();
        assert_eq!(canonical_pair(&b, &a), Some((250e6, 1e9)));
        let c = parse_quantity("1KB").unwrap();
        assert_eq!(canonical_pair(&a, &c), None);
    }
}
