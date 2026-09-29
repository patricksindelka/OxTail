//! Cell formatting helpers: durations, byte sizes, numbers, levels.

use std::borrow::Cow;

use crate::level::Level;
use crate::model::ColumnKind;
use crate::quantity::{parse_bytes, parse_duration_ns};

fn trim_zeros(s: String) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Formats a duration in nanoseconds compactly: `850 ns`, `1.5 ms`, `2.25 s`,
/// `3m 5s`, `2h 3m`. Negative and non-finite values are handled.
pub fn format_duration_ns(ns: f64) -> String {
    if !ns.is_finite() {
        return "-".to_string();
    }
    if ns < 0.0 {
        return format!("-{}", format_duration_ns(-ns));
    }
    if ns < 1e3 {
        return format!("{} ns", trim_zeros(format!("{ns:.0}")));
    }
    if ns < 1e6 {
        return format!("{} \u{b5}s", trim_zeros(format!("{:.2}", ns / 1e3)));
    }
    if ns < 1e9 {
        return format!("{} ms", trim_zeros(format!("{:.2}", ns / 1e6)));
    }
    let secs = ns / 1e9;
    if secs < 60.0 {
        return format!("{} s", trim_zeros(format!("{secs:.2}")));
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {s}s")
    }
}

/// Formats a byte count with binary units: `512 B`, `1.5 KiB`, `3.2 GiB`.
pub fn format_bytes(bytes: f64) -> String {
    if !bytes.is_finite() {
        return "-".to_string();
    }
    if bytes < 0.0 {
        return format!("-{}", format_bytes(-bytes));
    }
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", trim_zeros(format!("{v:.0}")))
    } else {
        format!("{} {}", trim_zeros(format!("{v:.2}")), UNITS[i])
    }
}

/// Formats a number with thousands separators; fractions keep up to three
/// digits (`1234567.891` becomes `1,234,567.891`).
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return "-".to_string();
    }
    let s = trim_zeros(format!("{:.3}", v.abs()));
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s.as_str(), None),
    };
    let mut out = String::new();
    if v < 0.0 && s != "0" {
        out.push('-');
    }
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

/// Renders a cell for display according to its column kind: durations and
/// sizes become human-readable, levels are normalised (`w` shows as `WARN`).
/// Anything that does not parse is returned unchanged.
pub fn format_cell(kind: ColumnKind, value: &str) -> Cow<'_, str> {
    match kind {
        ColumnKind::Duration => match parse_duration_ns(value) {
            Some(ns) => Cow::Owned(format_duration_ns(ns)),
            None => Cow::Borrowed(value),
        },
        ColumnKind::Bytes => match parse_bytes(value) {
            Some(b) => Cow::Owned(format_bytes(b)),
            None => Cow::Borrowed(value),
        },
        ColumnKind::Level => match Level::parse(value) {
            Some(l) => Cow::Borrowed(l.as_str()),
            None => Cow::Borrowed(value),
        },
        _ => Cow::Borrowed(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(format_duration_ns(850.0), "850 ns");
        assert_eq!(format_duration_ns(1.5e6), "1.5 ms");
        assert_eq!(format_duration_ns(2.25e9), "2.25 s");
        assert_eq!(format_duration_ns(185e9), "3m 5s");
        assert_eq!(format_duration_ns(7380e9), "2h 3m");
        assert_eq!(format_duration_ns(-1e6), "-1 ms");
        assert_eq!(format_duration_ns(f64::NAN), "-");
    }

    #[test]
    fn sizes_and_numbers() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1536.0), "1.5 KiB");
        assert_eq!(format_bytes(3.0 * 1024.0 * 1024.0 * 1024.0), "3 GiB");
        assert_eq!(format_number(1234567.891), "1,234,567.891");
        assert_eq!(format_number(-1000.0), "-1,000");
        assert_eq!(format_number(12.0), "12");
        assert_eq!(format_number(0.0), "0");
    }

    #[test]
    fn cells() {
        assert_eq!(format_cell(ColumnKind::Duration, "1500"), "1.5 s");
        assert_eq!(format_cell(ColumnKind::Duration, "250ms"), "250 ms");
        assert_eq!(format_cell(ColumnKind::Bytes, "2048"), "2 KiB");
        assert_eq!(format_cell(ColumnKind::Level, "w"), "WARN");
        assert_eq!(format_cell(ColumnKind::Level, "weird"), "weird");
        assert_eq!(format_cell(ColumnKind::Text, "x"), "x");
        assert_eq!(format_cell(ColumnKind::Duration, "n/a"), "n/a");
    }
}
