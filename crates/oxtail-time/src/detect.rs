//! Timestamp formats and the hand-rolled, allocation-free scanner behind
//! [`detect`].
//!
//! The scanner walks the first [`SCAN_LIMIT`] bytes of a line once. At every
//! position that is not preceded by an ASCII letter or digit it tries the
//! structured formats (dates with a time). The first structured hit wins. If
//! there is none, the first epoch-looking number wins, and after that the first
//! bare `HH:MM:SS` time. Matchers only ever consume ASCII, so ranges start and
//! end on `char` boundaries.

use std::ops::Range;

use jiff::{
    Timestamp,
    civil::{Date, DateTime},
    tz::{Offset, TimeZone},
};

/// Only the first this-many bytes of a line are scanned for a timestamp.
pub const SCAN_LIMIT: usize = 512;

/// A recognised timestamp shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TimestampFormat {
    /// ISO 8601 / RFC 3339: `2026-09-29T10:01:02.123456789Z`, a space instead
    /// of `T`, optional fraction (`.`), optional `Z` or `+hh[:mm]` offset.
    Iso8601,
    /// Like [`Iso8601`](Self::Iso8601) but with a comma fraction:
    /// `2026-09-29 10:01:02,123` (log4j, Python logging).
    Iso8601Comma,
    /// `2026/09/29 10:01:02` (Go's `log`).
    Slashed,
    /// `20260929T100102[.fff][Z]`.
    Compact,
    /// Syslog RFC 3164 `Sep 29 10:01:02` (no year: uses the reference year).
    Syslog,
    /// Apache/Nginx `29/Sep/2026:10:01:02 +0000`.
    Apache,
    /// RFC 2822 `Tue, 29 Sep 2026 10:01:02 +0000`.
    Rfc2822,
    /// US style `9/29/2026 10:01:02 AM` (month first, 12 or 24 hour clock).
    UsDateTime,
    /// Time of day only, `10:01:02.123` (date from the reference date).
    TimeOnly,
    /// 10-digit epoch seconds (optionally with a fraction).
    EpochSeconds,
    /// 13-digit epoch milliseconds.
    EpochMillis,
    /// 16-digit epoch microseconds.
    EpochMicros,
    /// 19-digit epoch nanoseconds.
    EpochNanos,
}

impl TimestampFormat {
    /// Whether two formats share the same scanner (ISO with `.` and with `,`).
    fn same_shape(self, other: Self) -> bool {
        use TimestampFormat::{Iso8601, Iso8601Comma};
        self == other
            || matches!(
                (self, other),
                (Iso8601, Iso8601Comma) | (Iso8601Comma, Iso8601)
            )
    }

    /// Parses `text` (a token as returned in [`Detected::range`]) with this
    /// format. Returns `None` if the text does not match or is not a valid
    /// calendar/clock value (e.g. February 30th).
    pub fn parse(self, text: &str, ctx: &TimeContext) -> Option<Timestamp> {
        let b = text.as_bytes();
        let (end, parsed) = match_at(self, b, 0)?;
        if end != b.len() {
            return None;
        }
        parsed.resolve(ctx)
    }
}

/// Context needed to turn partial timestamps into instants.
#[derive(Clone, Debug)]
pub struct TimeContext {
    /// Zone for timestamps that carry no offset.
    pub tz: TimeZone,
    /// Year for formats without one (syslog).
    pub reference_year: i16,
    /// Date for time-only formats.
    pub reference_date: Date,
}

impl TimeContext {
    /// A context in `tz` whose reference date is today in that zone.
    pub fn new(tz: TimeZone) -> Self {
        let today = Timestamp::now().to_zoned(tz.clone()).date();
        Self::with_reference_date(tz, today)
    }

    /// A context with an explicit reference date (year is taken from it).
    pub fn with_reference_date(tz: TimeZone, date: Date) -> Self {
        Self {
            tz,
            reference_year: date.year(),
            reference_date: date,
        }
    }

    /// A UTC context with today's date as reference.
    pub fn utc() -> Self {
        Self::new(TimeZone::UTC)
    }
}

impl Default for TimeContext {
    /// System-local zone (falls back to UTC when it cannot be determined).
    fn default() -> Self {
        Self::new(TimeZone::system())
    }
}

/// A timestamp-like token found in a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    /// Byte range of the token in the line (on `char` boundaries).
    pub range: Range<usize>,
    /// The recognised format.
    pub format: TimestampFormat,
}

pub(crate) enum DateSpec {
    Full(i16, i8, i8),
    NoYear(i8, i8),
    None,
}

pub(crate) enum Parsed {
    Epoch(Timestamp),
    Civil {
        date: DateSpec,
        hour: i8,
        minute: i8,
        second: i8,
        nanos: i32,
        offset: Option<i32>,
    },
}

impl Parsed {
    pub(crate) fn resolve(&self, ctx: &TimeContext) -> Option<Timestamp> {
        match self {
            Parsed::Epoch(ts) => Some(*ts),
            Parsed::Civil {
                date,
                hour,
                minute,
                second,
                nanos,
                offset,
            } => {
                let (y, m, d) = match date {
                    DateSpec::Full(y, m, d) => (*y, *m, *d),
                    DateSpec::NoYear(m, d) => (ctx.reference_year, *m, *d),
                    DateSpec::None => (
                        ctx.reference_date.year(),
                        ctx.reference_date.month(),
                        ctx.reference_date.day(),
                    ),
                };
                let dt = DateTime::new(y, m, d, *hour, *minute, *second, *nanos).ok()?;
                match offset {
                    Some(secs) => Offset::from_seconds(*secs).ok()?.to_timestamp(dt).ok(),
                    None => ctx.tz.to_timestamp(dt).ok(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------- scanning

/// Finds the first timestamp-like token in `line`.
///
/// Only the shape (and cheap range checks such as month 1..=12) is verified;
/// use [`TimestampFormat::parse`] or [`crate::TimeParser`] to get the instant.
/// Never panics; scans at most [`SCAN_LIMIT`] bytes.
pub fn detect(line: &str) -> Option<Detected> {
    scan(line.as_bytes(), None).map(|(range, format, _)| Detected { range, format })
}

/// Scans `b`; with `filter`, only tokens of that format are considered.
pub(crate) fn scan(
    b: &[u8],
    filter: Option<TimestampFormat>,
) -> Option<(Range<usize>, TimestampFormat, Parsed)> {
    use TimestampFormat as F;
    let limit = b.len().min(SCAN_LIMIT);
    let want = |f: F| filter.is_none_or(|w| w.same_shape(f));
    let mut epoch: Option<(Range<usize>, F, Parsed)> = None;
    let mut time: Option<(Range<usize>, F, Parsed)> = None;
    let mut i = 0;
    while i < limit {
        let c = b[i];
        let boundary = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if boundary {
            if c.is_ascii_digit() {
                const DIGIT_FORMATS: [F; 7] = [
                    F::Iso8601,
                    F::Slashed,
                    F::Compact,
                    F::Apache,
                    F::UsDateTime,
                    F::Rfc2822,
                    F::TimeOnly,
                ];
                for f in DIGIT_FORMATS {
                    if !want(f) {
                        continue;
                    }
                    if f == F::TimeOnly {
                        if time.is_none()
                            && (i == 0 || !matches!(b[i - 1], b':' | b'.'))
                            && let Some((end, p)) = match_at(f, b, i)
                        {
                            time = Some((i..end, f, p));
                        }
                    } else if let Some((end, p)) = match_at(f, b, i) {
                        return Some((i..end, format_of(f, b, i, end), p));
                    }
                }
                if epoch.is_none() {
                    for f in [
                        F::EpochSeconds,
                        F::EpochMillis,
                        F::EpochMicros,
                        F::EpochNanos,
                    ] {
                        if want(f)
                            && let Some((end, p)) = match_at(f, b, i)
                        {
                            epoch = Some((i..end, f, p));
                            break;
                        }
                    }
                }
            } else if c.is_ascii_alphabetic() {
                for f in [F::Syslog, F::Rfc2822] {
                    if want(f)
                        && let Some((end, p)) = match_at(f, b, i)
                    {
                        return Some((i..end, f, p));
                    }
                }
            }
        }
        i += 1;
    }
    epoch.or(time)
}

/// ISO matches report the comma variant when the fraction separator was `,`.
fn format_of(f: TimestampFormat, b: &[u8], start: usize, end: usize) -> TimestampFormat {
    if f == TimestampFormat::Iso8601 && b[start..end].contains(&b',') {
        TimestampFormat::Iso8601Comma
    } else {
        f
    }
}

fn match_at(f: TimestampFormat, b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    use TimestampFormat as F;
    match f {
        F::Iso8601 | F::Iso8601Comma => m_iso(b, i),
        F::Slashed => m_slashed(b, i),
        F::Compact => m_compact(b, i),
        F::Syslog => m_syslog(b, i),
        F::Apache => m_apache(b, i),
        F::Rfc2822 => m_rfc2822(b, i),
        F::UsDateTime => m_us(b, i),
        F::TimeOnly => m_time_only(b, i),
        F::EpochSeconds => m_epoch(b, i, 10),
        F::EpochMillis => m_epoch(b, i, 13),
        F::EpochMicros => m_epoch(b, i, 16),
        F::EpochNanos => m_epoch(b, i, 19),
    }
}

// ---------------------------------------------------------------- helpers

fn at(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

/// Exactly `n` ASCII digits at `i`.
fn fixed(b: &[u8], i: usize, n: usize) -> Option<u32> {
    let s = b.get(i..i.checked_add(n)?)?;
    let mut v = 0u32;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + u32::from(c - b'0');
    }
    Some(v)
}

/// One or two digits at `i`: `(value, length)`.
fn one_or_two(b: &[u8], i: usize) -> Option<(u32, usize)> {
    let a = at(b, i);
    if !a.is_ascii_digit() {
        return None;
    }
    let c = at(b, i + 1);
    if c.is_ascii_digit() {
        Some((u32::from(a - b'0') * 10 + u32::from(c - b'0'), 2))
    } else {
        Some((u32::from(a - b'0'), 1))
    }
}

/// `HH:MM:SS` at `i` (hour 1 or 2 digits when `loose_hour`).
fn clock(b: &[u8], i: usize, loose_hour: bool) -> Option<(usize, i8, i8, i8)> {
    let (h, hl) = if loose_hour {
        one_or_two(b, i)?
    } else {
        (fixed(b, i, 2)?, 2)
    };
    let mut p = i + hl;
    if at(b, p) != b':' {
        return None;
    }
    p += 1;
    let m = fixed(b, p, 2)?;
    p += 2;
    if at(b, p) != b':' {
        return None;
    }
    p += 1;
    let s = fixed(b, p, 2)?;
    p += 2;
    if h > 23 || m > 59 || s > 60 {
        return None;
    }
    // Leap second 23:59:60 is clamped to :59.
    Some((p, h as i8, m as i8, s.min(59) as i8))
}

/// Fraction with one of `seps` at `i`; returns `(end, nanoseconds)`.
fn fraction(b: &[u8], i: usize, seps: &[u8]) -> (usize, i32) {
    if !seps.contains(&at(b, i)) || !at(b, i + 1).is_ascii_digit() {
        return (i, 0);
    }
    let mut p = i + 1;
    let mut ns = 0i32;
    let mut n = 0;
    while at(b, p).is_ascii_digit() {
        if n < 9 {
            ns = ns * 10 + i32::from(at(b, p) - b'0');
            n += 1;
        }
        p += 1;
    }
    while n < 9 {
        ns *= 10;
        n += 1;
    }
    (p, ns)
}

/// `Z`, `+hh`, `+hhmm`, `+hh:mm` (optionally after one space; then only the
/// four-digit forms are accepted to avoid eating unrelated `-12`).
fn offset(b: &[u8], i: usize) -> Option<(usize, i32)> {
    let spaced = at(b, i) == b' ';
    let p = if spaced { i + 1 } else { i };
    let c = at(b, p);
    if c == b'Z' && !spaced && !at(b, p + 1).is_ascii_alphanumeric() {
        return Some((p + 1, 0));
    }
    let sign = match c {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hh = fixed(b, p + 1, 2)?;
    let mut q = p + 3;
    let mm;
    if at(b, q) == b':' {
        mm = fixed(b, q + 1, 2)?;
        q += 3;
    } else if let Some(v) = fixed(b, q, 2) {
        mm = v;
        q += 2;
    } else if spaced {
        return None;
    } else {
        mm = 0;
    }
    if hh > 23 || mm > 59 || at(b, q).is_ascii_digit() {
        return None;
    }
    Some((q, sign * (hh as i32 * 3600 + mm as i32 * 60)))
}

fn month_name(b: &[u8], i: usize) -> Option<i8> {
    let s = b.get(i..i + 3)?;
    let key = [
        s[0].to_ascii_lowercase(),
        s[1].to_ascii_lowercase(),
        s[2].to_ascii_lowercase(),
    ];
    let m = match &key {
        b"jan" => 1,
        b"feb" => 2,
        b"mar" => 3,
        b"apr" => 4,
        b"may" => 5,
        b"jun" => 6,
        b"jul" => 7,
        b"aug" => 8,
        b"sep" => 9,
        b"oct" => 10,
        b"nov" => 11,
        b"dec" => 12,
        _ => return None,
    };
    // The name must not continue as a longer word ("Marker"), except full
    // English month names are not supported.
    if at(b, i + 3).is_ascii_alphabetic() {
        return None;
    }
    Some(m)
}

fn is_weekday(b: &[u8], i: usize) -> bool {
    let Some(s) = b.get(i..i + 3) else {
        return false;
    };
    let key = [
        s[0].to_ascii_lowercase(),
        s[1].to_ascii_lowercase(),
        s[2].to_ascii_lowercase(),
    ];
    matches!(
        &key,
        b"mon" | b"tue" | b"wed" | b"thu" | b"fri" | b"sat" | b"sun"
    )
}

fn civil(
    date: DateSpec,
    (hour, minute, second): (i8, i8, i8),
    nanos: i32,
    offset: Option<i32>,
) -> Parsed {
    Parsed::Civil {
        date,
        hour,
        minute,
        second,
        nanos,
        offset,
    }
}

fn ymd(b: &[u8], i: usize, sep: u8) -> Option<(i16, i8, i8)> {
    let y = fixed(b, i, 4)?;
    if at(b, i + 4) != sep {
        return None;
    }
    let m = fixed(b, i + 5, 2)?;
    if at(b, i + 7) != sep {
        return None;
    }
    let d = fixed(b, i + 8, 2)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y as i16, m as i8, d as i8))
}

// ---------------------------------------------------------------- matchers

fn m_iso(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let (y, m, d) = ymd(b, i, b'-')?;
    let sep = at(b, i + 10);
    if !matches!(sep, b'T' | b't' | b' ') {
        return None;
    }
    let (p, h, mi, s) = clock(b, i + 11, false)?;
    let (p, ns) = fraction(b, p, b".,");
    let (p, off) = match offset(b, p) {
        Some((e, o)) => (e, Some(o)),
        None => (p, None),
    };
    Some((p, civil(DateSpec::Full(y, m, d), (h, mi, s), ns, off)))
}

fn m_slashed(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let (y, m, d) = ymd(b, i, b'/')?;
    if at(b, i + 10) != b' ' {
        return None;
    }
    let (p, h, mi, s) = clock(b, i + 11, false)?;
    let (p, ns) = fraction(b, p, b".,");
    Some((p, civil(DateSpec::Full(y, m, d), (h, mi, s), ns, None)))
}

fn m_compact(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let y = fixed(b, i, 4)?;
    let m = fixed(b, i + 4, 2)?;
    let d = fixed(b, i + 6, 2)?;
    if at(b, i + 8) != b'T' {
        return None;
    }
    let h = fixed(b, i + 9, 2)?;
    let mi = fixed(b, i + 11, 2)?;
    let s = fixed(b, i + 13, 2)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let (p, ns) = fraction(b, i + 15, b".,");
    let (p, off) = if at(b, p) == b'Z' && !at(b, p + 1).is_ascii_alphanumeric() {
        (p + 1, Some(0))
    } else {
        (p, None)
    };
    if at(b, p).is_ascii_digit() {
        return None;
    }
    Some((
        p,
        civil(
            DateSpec::Full(y as i16, m as i8, d as i8),
            (h as i8, mi as i8, s.min(59) as i8),
            ns,
            off,
        ),
    ))
}

fn m_syslog(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let m = month_name(b, i)?;
    let mut p = i + 3;
    if at(b, p) != b' ' {
        return None;
    }
    p += 1;
    if at(b, p) == b' ' {
        p += 1;
    }
    let (d, dl) = one_or_two(b, p)?;
    p += dl;
    if at(b, p) != b' ' || !(1..=31).contains(&d) {
        return None;
    }
    let (p, h, mi, s) = clock(b, p + 1, false)?;
    let (p, ns) = fraction(b, p, b".");
    Some((p, civil(DateSpec::NoYear(m, d as i8), (h, mi, s), ns, None)))
}

fn m_apache(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let (d, dl) = one_or_two(b, i)?;
    let mut p = i + dl;
    if at(b, p) != b'/' {
        return None;
    }
    let m = month_name(b, p + 1)?;
    p += 4;
    if at(b, p) != b'/' {
        return None;
    }
    let y = fixed(b, p + 1, 4)?;
    p += 5;
    if at(b, p) != b':' || !(1..=31).contains(&d) {
        return None;
    }
    let (p, h, mi, s) = clock(b, p + 1, false)?;
    let (p, off) = match offset(b, p) {
        Some((e, o)) if at(b, p) == b' ' => (e, Some(o)),
        _ => (p, None),
    };
    Some((
        p,
        civil(DateSpec::Full(y as i16, m, d as i8), (h, mi, s), 0, off),
    ))
}

fn m_rfc2822(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let mut p = i;
    if at(b, p).is_ascii_alphabetic() {
        if !is_weekday(b, p) || at(b, p + 3) != b',' {
            return None;
        }
        p += 4;
        while at(b, p) == b' ' {
            p += 1;
        }
    }
    let (d, dl) = one_or_two(b, p)?;
    p += dl;
    if at(b, p) != b' ' || !(1..=31).contains(&d) {
        return None;
    }
    let m = month_name(b, p + 1)?;
    p += 4;
    if at(b, p) != b' ' {
        return None;
    }
    let y = fixed(b, p + 1, 4)?;
    p += 5;
    if at(b, p) != b' ' {
        return None;
    }
    let h = fixed(b, p + 1, 2)?;
    if at(b, p + 3) != b':' {
        return None;
    }
    let mi = fixed(b, p + 4, 2)?;
    p += 6;
    let mut s = 0;
    if at(b, p) == b':' {
        s = fixed(b, p + 1, 2)?;
        p += 3;
    }
    if h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut off = None;
    if at(b, p) == b' ' {
        let rest = &b[p + 1..b.len().min(p + 5)];
        if let Some((e, o)) = offset(b, p).filter(|_| matches!(at(b, p + 1), b'+' | b'-')) {
            p = e;
            off = Some(o);
        } else {
            for name in [&b"GMT"[..], b"UTC", b"UT"] {
                if rest.len() >= name.len()
                    && rest[..name.len()].eq_ignore_ascii_case(name)
                    && !at(b, p + 1 + name.len()).is_ascii_alphanumeric()
                {
                    p += 1 + name.len();
                    off = Some(0);
                    break;
                }
            }
        }
    }
    Some((
        p,
        civil(
            DateSpec::Full(y as i16, m, d as i8),
            (h as i8, mi as i8, s.min(59) as i8),
            0,
            off,
        ),
    ))
}

fn m_us(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let (m, ml) = one_or_two(b, i)?;
    let mut p = i + ml;
    if at(b, p) != b'/' {
        return None;
    }
    let (d, dl) = one_or_two(b, p + 1)?;
    p += 1 + dl;
    if at(b, p) != b'/' {
        return None;
    }
    let y = fixed(b, p + 1, 4)?;
    p += 5;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    if at(b, p) == b',' {
        p += 1;
    }
    if at(b, p) != b' ' {
        return None;
    }
    while at(b, p) == b' ' {
        p += 1;
    }
    let (mut p, mut h, mi, s) = clock(b, p, true)?;
    let (p2, ns) = fraction(b, p, b".");
    p = p2;
    let mut q = p;
    if at(b, q) == b' ' {
        q += 1;
    }
    let ap = at(b, q).to_ascii_lowercase();
    if (ap == b'a' || ap == b'p') && at(b, q + 1).eq_ignore_ascii_case(&b'm') {
        if !(1..=12).contains(&h) {
            return None;
        }
        h %= 12;
        if ap == b'p' {
            h += 12;
        }
        p = q + 2;
    }
    Some((
        p,
        civil(
            DateSpec::Full(y as i16, m as i8, d as i8),
            (h, mi, s),
            ns,
            None,
        ),
    ))
}

fn m_time_only(b: &[u8], i: usize) -> Option<(usize, Parsed)> {
    let (p, h, mi, s) = clock(b, i, false)?;
    let (p, ns) = fraction(b, p, b".,");
    if at(b, p).is_ascii_digit() || at(b, p) == b':' {
        return None;
    }
    Some((p, civil(DateSpec::None, (h, mi, s), ns, None)))
}

/// Plausible epoch range: years 2000..2100.
const EPOCH_MIN_S: i64 = 946_684_800;
const EPOCH_MAX_S: i64 = 4_102_444_800;

fn m_epoch(b: &[u8], i: usize, digits: usize) -> Option<(usize, Parsed)> {
    // Full digit run must be exactly `digits` long.
    let mut end = i;
    let mut n: i64 = 0;
    while at(b, end).is_ascii_digit() {
        if end - i >= 19 {
            return None;
        }
        n = n * 10 + i64::from(at(b, end) - b'0');
        end += 1;
    }
    if end - i != digits {
        return None;
    }
    if i > 0 && matches!(b[i - 1], b'.' | b'-' | b'_' | b'/' | b':') {
        return None;
    }
    let mut ns_frac = 0i32;
    if digits == 10 && at(b, end) == b'.' && at(b, end + 1).is_ascii_digit() {
        let (e, ns) = fraction(b, end, b".");
        end = e;
        ns_frac = ns;
    }
    let next = at(b, end);
    if next.is_ascii_alphanumeric()
        || next == b'_'
        || (next == b'.' && at(b, end + 1).is_ascii_digit())
    {
        return None;
    }
    let secs = match digits {
        10 => n,
        13 => n / 1_000,
        16 => n / 1_000_000,
        _ => n / 1_000_000_000,
    };
    if !(EPOCH_MIN_S..EPOCH_MAX_S).contains(&secs) {
        return None;
    }
    let ts = match digits {
        10 => Timestamp::new(n, ns_frac).ok()?,
        13 => Timestamp::from_millisecond(n).ok()?,
        16 => Timestamp::from_microsecond(n).ok()?,
        _ => Timestamp::from_nanosecond(i128::from(n)).ok()?,
    };
    Some((end, Parsed::Epoch(ts)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use TimestampFormat as F;

    fn ctx() -> TimeContext {
        TimeContext::with_reference_date(TimeZone::UTC, Date::new(2026, 9, 29).expect("valid date"))
    }

    fn parse_line(line: &str) -> Option<(TimestampFormat, i64, i32)> {
        let d = detect(line)?;
        let ts = d.format.parse(&line[d.range.clone()], &ctx())?;
        Some((d.format, ts.as_second(), ts.subsec_nanosecond()))
    }

    // 2026-09-29T10:01:02Z
    const T: i64 = 1_790_676_062;

    #[test]
    fn valid_formats() {
        let cases: &[(&str, TimestampFormat, i64, i32)] = &[
            ("2026-09-29T10:01:02Z msg", F::Iso8601, T, 0),
            ("2026-09-29 10:01:02 msg", F::Iso8601, T, 0),
            (
                "[2026-09-29 10:01:02.123456789] x",
                F::Iso8601,
                T,
                123_456_789,
            ),
            ("2026-09-29T12:01:02+02:00", F::Iso8601, T, 0),
            ("2026-09-29T12:01:02.5+0200", F::Iso8601, T, 500_000_000),
            (
                "2026-09-29 10:01:02,123 INFO",
                F::Iso8601Comma,
                T,
                123_000_000,
            ),
            ("Sep 29 10:01:02 host sshd[1]: x", F::Syslog, T, 0),
            ("Sep  9 10:01:02 host", F::Syslog, T - 20 * 86_400, 0),
            (
                "1.2.3.4 - - [29/Sep/2026:10:01:02 +0000] \"GET /\"",
                F::Apache,
                T,
                0,
            ),
            ("[29/Sep/2026:12:01:02 +0200]", F::Apache, T, 0),
            ("Tue, 29 Sep 2026 10:01:02 +0000", F::Rfc2822, T, 0),
            ("29 Sep 2026 10:01 GMT", F::Rfc2822, T - 2, 0),
            ("9/29/2026 10:01:02 AM x", F::UsDateTime, T, 0),
            ("9/29/2026 10:01:02 PM x", F::UsDateTime, T + 12 * 3600, 0),
            ("9/29/2026 12:01:02 AM x", F::UsDateTime, T - 10 * 3600, 0),
            ("20260929T100102 x", F::Compact, T, 0),
            ("20260929T100102.25Z x", F::Compact, T, 250_000_000),
            ("2026/09/29 10:01:02 x", F::Slashed, T, 0),
            ("10:01:02.123 x", F::TimeOnly, T, 123_000_000),
            ("1790676062 x", F::EpochSeconds, T, 0),
            ("1790676062.5 x", F::EpochSeconds, T, 500_000_000),
            ("1790676062123 x", F::EpochMillis, T, 123_000_000),
            ("1790676062123456 x", F::EpochMicros, T, 123_456_000),
            ("1790676062123456789 x", F::EpochNanos, T, 123_456_789),
        ];
        for (line, fmt, secs, ns) in cases {
            let got = parse_line(line).unwrap_or_else(|| panic!("no parse: {line}"));
            assert_eq!(got, (*fmt, *secs, *ns), "{line}");
        }
    }

    #[test]
    fn invalid_or_absent() {
        for line in [
            "",
            "2026-02-30 10:01:02", // shape ok, calendar invalid: detect ok, parse None
            "2026-09-29 25:01:02",
            "9999999999 x",  // epoch before... 2286, out of range
            "123456789 x",   // 9 digits
            "12345678901 x", // 11 digits
            "abc1790676062 x",
        ] {
            let r = parse_line(line);
            assert!(r.is_none(), "{line} -> {r:?}");
        }
    }

    #[test]
    fn calendar_invalid_date_degrades_to_time_only() {
        // Shape matches ISO but the date is invalid; scanning continues and
        // the clock part is still found as a time-only stamp.
        assert_eq!(parse_line("2026-02-30 10:01:02"), None);
        assert_eq!(
            parse_line("2026-13-01 10:01:02").map(|r| r.0),
            Some(F::TimeOnly)
        );
    }

    #[test]
    fn calendar_validity_and_leap_years() {
        assert!(parse_line("2024-02-29 00:00:00").is_some());
        assert!(parse_line("2026-02-29 00:00:00").is_none());
        assert!(parse_line("2100-02-29 00:00:00").is_none());
        assert!(parse_line("2000-02-29 00:00:00").is_some());
        assert!(parse_line("2026-04-31 00:00:00").is_none());
        // Syslog with a non-leap reference year.
        assert!(parse_line("Feb 29 10:00:00 h").is_none());
        // Leap second is clamped.
        assert!(parse_line("2016-12-31 23:59:60").is_some());
    }

    #[test]
    fn dst_transitions_amsterdam() {
        let tz = TimeZone::get("Europe/Amsterdam").expect("bundled tzdb");
        let ctx = TimeContext::with_reference_date(tz, Date::new(2026, 9, 29).expect("date"));
        let p = |s: &str| {
            let d = detect(s).expect("detect");
            d.format.parse(&s[d.range], &ctx).expect("parse")
        };
        // 2026-03-29: 02:00 -> 03:00 (spring forward), gap 02:30 does not exist.
        assert_eq!(p("2026-03-29 01:59:59").as_second(), 1_774_745_999); // 00:59:59Z
        assert_eq!(p("2026-03-29 03:00:00").as_second(), 1_774_746_000); // 01:00:00Z
        // Gap time resolves (compatible) rather than failing.
        assert_eq!(p("2026-03-29 02:30:00").as_second(), 1_774_747_800);
        // 2026-10-25: 03:00 -> 02:00 (fall back); 02:30 is ambiguous; earlier wins.
        assert_eq!(p("2026-10-25 02:30:00").as_second(), 1_792_888_200); // 00:30Z
        assert_eq!(p("2026-10-25 03:30:00").as_second(), 1_792_895_400); // 02:30Z
        // Explicit offset ignores the zone.
        assert_eq!(p("2026-10-25 02:30:00+01:00").as_second(), 1_792_891_800);
    }

    #[test]
    fn range_covers_token() {
        let line = "[2026-09-29 10:01:02,123] INFO x";
        let d = detect(line).expect("detected");
        assert_eq!(&line[d.range], "2026-09-29 10:01:02,123");
        let line = "é 2026-09-29T10:01:02Z";
        let d = detect(line).expect("detected");
        assert_eq!(&line[d.range], "2026-09-29T10:01:02Z");
    }

    #[test]
    fn structured_beats_epoch_and_time_only() {
        let line = "1790676062 at 2026-09-29 10:01:02 12:00:00";
        assert_eq!(detect(line).expect("d").format, F::Iso8601);
        let line = "id 1790676062 at 12:00:00";
        assert_eq!(detect(line).expect("d").format, F::EpochSeconds);
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn detect_never_panics_and_ranges_are_valid(s in "\\PC{0,200}") {
                if let Some(d) = detect(&s) {
                    prop_assert!(d.range.start <= d.range.end && d.range.end <= s.len());
                    prop_assert!(s.is_char_boundary(d.range.start));
                    prop_assert!(s.is_char_boundary(d.range.end));
                    let _ = d.format.parse(&s[d.range], &ctx());
                }
            }

            #[test]
            fn detect_timestampish_bytes(s in "[0-9TZ:/ ,.+\\-a-zA-Z\\[\\]é]{0,80}") {
                if let Some(d) = detect(&s) {
                    prop_assert!(s.is_char_boundary(d.range.start));
                    prop_assert!(s.is_char_boundary(d.range.end));
                }
            }
        }
    }
}
