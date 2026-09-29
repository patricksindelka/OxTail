//! ANSI SGR parsing of arbitrary text: no panic, the stripped text has no
//! escape sequences of the handled kind, and spans are valid.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (plain, spans) = oxtail_highlight::ansi::parse(&text);
    assert!(plain.len() <= text.len());
    let mut prev_end = 0;
    for s in &spans {
        assert!(s.range.start < s.range.end, "empty span");
        assert!(s.range.start >= prev_end, "unsorted or overlapping spans");
        assert!(s.range.end <= plain.len(), "span out of bounds");
        assert!(
            plain.is_char_boundary(s.range.start) && plain.is_char_boundary(s.range.end),
            "span not on char boundary"
        );
        prev_end = s.range.end;
    }
    // Idempotent on already-stripped text.
    let (again, spans2) = oxtail_highlight::ansi::parse(&plain);
    if !plain.contains('\x1b') {
        assert_eq!(again, plain);
        assert!(spans2.is_empty());
    }
});
