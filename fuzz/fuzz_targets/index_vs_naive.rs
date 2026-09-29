//! `LineIndex` (built incrementally over a growing source) must agree with a
//! naive `lines()`-style reference for random bytes, spacings and split points.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use oxtail_core::{LineIndex, MemSource, ReadAt};

#[derive(Debug, Arbitrary)]
struct Input {
    data: Vec<u8>,
    spacing: u16,
    /// Split points (taken modulo the data length + 1) at which the source grows.
    splits: Vec<u16>,
}

fn naive_starts(data: &[u8]) -> Vec<u64> {
    let mut v = Vec::new();
    if data.is_empty() {
        return v;
    }
    v.push(0);
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' && i + 1 < data.len() {
            v.push(i as u64 + 1);
        }
    }
    v
}

fuzz_target!(|input: Input| {
    let mut data = input.data;
    data.truncate(8192);
    let spacing = u64::from(input.spacing % 512) + 1;

    let mut cuts: Vec<usize> = input
        .splits
        .iter()
        .take(16)
        .map(|s| usize::from(*s) % (data.len() + 1))
        .collect();
    cuts.push(data.len());
    cuts.sort_unstable();

    let src = MemSource::new(Vec::new());
    let mut idx = LineIndex::with_spacing(spacing);
    let mut fed = 0;
    for cut in cuts {
        src.append(&data[fed..cut]);
        fed = cut;
        let n = idx.extend(&src, cut as u64, &|| false).unwrap();
        assert_eq!(n, cut as u64);
    }

    let starts = naive_starts(&data);
    assert_eq!(idx.indexed_bytes(), data.len() as u64);
    assert_eq!(idx.total_lines(), starts.len() as u64);
    for (n, s) in starts.iter().enumerate() {
        assert_eq!(idx.offset_of_line(&src, n as u64).unwrap(), Some(*s));
    }
    assert_eq!(
        idx.offset_of_line(&src, starts.len() as u64 + 1).unwrap(),
        None
    );
    for off in 0..data.len() as u64 {
        let pos = idx.line_of_offset(&src, off).unwrap();
        let line = starts.partition_point(|s| *s <= off) - 1;
        assert_eq!(pos.line, line as u64, "line_of_offset({off})");
        assert_eq!(pos.start, starts[line]);
    }
    assert_eq!(src.len().unwrap(), data.len() as u64);
});
