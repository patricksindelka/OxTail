//! Regressions found by `fuzz/fuzz_targets/time_detect.rs`.

use oxtail_time::{TimeContext, TimeParser, detect};

/// BUG: `m_epoch` (detect.rs) accumulates a 19-digit run in an `i64` with
/// `n * 10 + digit`; 19 nines exceed `i64::MAX`, so debug builds panic with
/// "attempt to multiply with overflow" and release builds silently wrap.
/// Input: `9999999999999999999`. Expected: `None` (not a timestamp), never a
/// panic.
#[test]
#[ignore = "BUG: oxtail-time m_epoch overflows i64 on a 19-digit run such as 9999999999999999999"]
fn nineteen_digit_epoch_does_not_overflow() {
    let line = "9999999999999999999";
    let _ = detect(line);
    let parser = TimeParser::new(TimeContext::utc());
    assert_eq!(parser.parse(line), None);
}
