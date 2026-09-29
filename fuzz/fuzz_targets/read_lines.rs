//! `read_lines` with random content, start, count, limit and display length:
//! no panic, bounded text, lines consistent with the naive split.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use oxtail_core::MemSource;
use oxtail_core::line::read_lines;

#[derive(Debug, Arbitrary)]
struct Input {
    data: Vec<u8>,
    start: u32,
    count: u16,
    limit: u32,
    max_display: u16,
    first_number: u32,
    exact: bool,
}

fuzz_target!(|input: Input| {
    let src = MemSource::new(input.data.clone());
    let len = input.data.len() as u64;
    let start = u64::from(input.start) % (len + 2);
    let limit = u64::from(input.limit) % (len + 8);
    let max = usize::from(input.max_display);
    let count = usize::from(input.count);
    let first = u64::from(input.first_number);
    let lines = read_lines(&src, start, count, limit, first, input.exact, max).unwrap();
    assert!(lines.len() <= count);
    let mut prev_end = start;
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(l.number, first + i as u64);
        assert_eq!(l.number_exact, input.exact);
        assert!(l.offset >= prev_end);
        assert!(l.offset + l.len <= len.max(l.offset));
        assert!(l.offset < limit.min(len).max(1) || l.len == 0);
        // Display text is bounded (a few bytes of slack for U+FFFD expansion).
        assert!(
            l.text.len() <= max + 4 * 3,
            "text len {} max {}",
            l.text.len(),
            max
        );
        prev_end = l.offset + l.len;
    }
    if !lines.is_empty() {
        assert_eq!(lines[0].offset, start);
    }
});
