//! Timestamp detection and parsing on arbitrary lines: no panic, ranges on
//! char boundaries, and a detected token parses (or cleanly fails) with its format.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtail_time::{TimeContext, TimeParser, detect};

fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    let ctx = TimeContext::utc();
    if let Some(d) = detect(&line) {
        assert!(d.range.start <= d.range.end && d.range.end <= line.len());
        assert!(line.is_char_boundary(d.range.start) && line.is_char_boundary(d.range.end));
        let _ = d.format.parse(&line[d.range.clone()], &ctx);
    }
    let mut parser = TimeParser::new(ctx);
    let _ = parser.learn([line.as_ref()]);
    if let Some((_, r)) = parser.parse_with_range(&line) {
        assert!(r.end <= line.len());
        assert!(line.is_char_boundary(r.start) && line.is_char_boundary(r.end));
    }
    let _ = parser.parse(&line);
});
