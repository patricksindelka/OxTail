//! Scanning one chunk of a source.
//!
//! A chunk `[a, b)` *owns* the lines whose first byte lies in `[a, b)`. The
//! chunk aligns itself to line boundaries (it may read past `b` to finish its
//! last line) so chunks are independent and their results simply concatenate.
//! Context lines are found by reading a few lines before and after the owned
//! range, which keeps context exact across chunk boundaries.
//!
//! Memory stays bounded: a chunk buffer is at most `chunk_size` plus the
//! longest line, and every read is capped at [`MAX_REGION`] (a single line
//! longer than that is only searched up to the cap).

use std::io;

use memchr::{memchr, memchr_iter, memrchr_iter};
use oxtail_core::ReadAt;

use crate::matchset::Segment;
use crate::predicate::LinePredicate;

/// Upper bound for one read region; guards against a multi-gigabyte line.
pub(crate) const MAX_REGION: u64 = 256 * 1024 * 1024;

const BLOCK: usize = 32 * 1024;

/// The first line start at or after `x` (`end` if there is none). Lines start
/// at 0 and right after each `\n`.
pub(crate) fn align_up(src: &dyn ReadAt, x: u64, end: u64) -> io::Result<u64> {
    if x == 0 {
        return Ok(0);
    }
    if x >= end {
        return Ok(end);
    }
    let mut buf = [0u8; BLOCK];
    let mut pos = x - 1;
    while pos < end {
        let want = usize::try_from(end - pos).unwrap_or(usize::MAX).min(BLOCK);
        let n = src.read_at(pos, &mut buf[..want])?;
        if n == 0 {
            return Ok(end);
        }
        if let Some(i) = memchr(b'\n', &buf[..n]) {
            return Ok(pos + i as u64 + 1);
        }
        pos += n as u64;
    }
    Ok(end)
}

/// The first line start in `[x, limit)`, or `None` when the range holds none.
/// A line starting at `p > 0` has its preceding `\n` at `p - 1`, so only bytes
/// in `[x - 1, limit - 1)` are read: bounded by the chunk, never by the line.
pub(crate) fn first_start_in(
    src: &dyn ReadAt,
    x: u64,
    limit: u64,
    end: u64,
) -> io::Result<Option<u64>> {
    let limit = limit.min(end);
    if x >= limit {
        return Ok(None);
    }
    if x == 0 {
        return Ok(Some(0));
    }
    let hi = limit - 1;
    let mut buf = [0u8; BLOCK];
    let mut pos = x - 1;
    while pos < hi {
        let want = usize::try_from(hi - pos).unwrap_or(usize::MAX).min(BLOCK);
        let n = src.read_at(pos, &mut buf[..want])?;
        if n == 0 {
            return Ok(None);
        }
        if let Some(i) = memchr(b'\n', &buf[..n]) {
            return Ok(Some(pos + i as u64 + 1));
        }
        pos += n as u64;
    }
    Ok(None)
}

/// Reads `[a, b)`, clamped to [`MAX_REGION`] and to what the source has.
pub(crate) fn read_range(src: &dyn ReadAt, a: u64, b: u64) -> io::Result<Vec<u8>> {
    let len = usize::try_from((b.saturating_sub(a)).min(MAX_REGION)).unwrap_or(usize::MAX);
    let mut buf = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        let n = src.read_at(a + filled as u64, &mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// The start of the line `k` lines before line start `s`.
pub(crate) fn back_lines(src: &dyn ReadAt, s: u64, k: u32) -> io::Result<u64> {
    if k == 0 || s == 0 {
        return Ok(s);
    }
    let mut buf = [0u8; BLOCK];
    let mut hi = s;
    let mut count = 0u64;
    let floor = s.saturating_sub(MAX_REGION);
    while hi > floor {
        let lo = hi.saturating_sub(BLOCK as u64).max(floor);
        let n = usize::try_from(hi - lo).unwrap_or(BLOCK);
        let got = src.read_at(lo, &mut buf[..n])?;
        if got < n {
            return Ok(s); // source shrank under us: no context
        }
        for i in memrchr_iter(b'\n', &buf[..n]) {
            count += 1;
            if count == u64::from(k) + 1 {
                return Ok(lo + i as u64 + 1);
            }
        }
        if lo == 0 {
            return Ok(0);
        }
        hi = lo;
    }
    Ok(hi)
}

/// The offset just after `k` lines starting at line start `e` (or `end`).
pub(crate) fn fwd_lines(src: &dyn ReadAt, e: u64, k: u32, end: u64) -> io::Result<u64> {
    if k == 0 || e >= end {
        return Ok(e.min(end));
    }
    let mut buf = [0u8; BLOCK];
    let mut pos = e;
    let ceil = e.saturating_add(MAX_REGION).min(end);
    let mut count = 0u64;
    while pos < ceil {
        let want = usize::try_from(ceil - pos).unwrap_or(BLOCK).min(BLOCK);
        let n = src.read_at(pos, &mut buf[..want])?;
        if n == 0 {
            return Ok(pos);
        }
        for i in memchr_iter(b'\n', &buf[..n]) {
            count += 1;
            if count == u64::from(k) {
                return Ok(pos + i as u64 + 1);
            }
        }
        pos += n as u64;
    }
    Ok(pos)
}

/// Scans chunk `[a, b)` (bounded by `end`) and returns the owned lines that
/// pass `pred`, plus context lines when `before`/`after` are non-zero.
pub(crate) fn scan_chunk(
    src: &dyn ReadAt,
    a: u64,
    b: u64,
    end: u64,
    pred: &dyn LinePredicate,
    before: u32,
    after: u32,
) -> io::Result<Segment> {
    // Only bytes inside the chunk are searched for the first owned line; the
    // end of the last owned line is searched for once, by the chunk that owns
    // its start. Chunks inside one huge line therefore cost O(chunk).
    let Some(s) = first_start_in(src, a, b, end)? else {
        return Ok(Segment::default());
    };
    let e = align_up(src, b.min(end), end)?;
    if s >= e {
        return Ok(Segment::default());
    }
    if before == 0 && after == 0 {
        let buf = read_range(src, s, e)?;
        let mut offsets = Vec::new();
        pred.scan_lines(&buf, &mut |ls, _| offsets.push(s + ls as u64));
        return Ok(Segment {
            offsets,
            context: Vec::new(),
        });
    }

    // Lines before the range matter for `context_after`, lines after it for
    // `context_before`.
    let ext_start = back_lines(src, s, after)?;
    let ext_end = fwd_lines(src, e, before, end)?;
    let buf = read_range(src, ext_start, ext_end)?;

    let mut starts = vec![0usize];
    for i in memchr_iter(b'\n', &buf) {
        if i + 1 < buf.len() {
            starts.push(i + 1);
        }
    }
    let n = starts.len();
    let mut pass = vec![false; n];
    pred.scan_lines(&buf, &mut |ls, _| {
        if let Ok(i) = starts.binary_search(&ls) {
            pass[i] = true;
        }
    });
    let mut ctx = vec![false; n];
    let mut remaining = 0u32;
    for i in 0..n {
        if pass[i] {
            remaining = after;
        } else if remaining > 0 {
            ctx[i] = true;
            remaining -= 1;
        }
    }
    remaining = 0;
    for i in (0..n).rev() {
        if pass[i] {
            remaining = before;
        } else if remaining > 0 {
            ctx[i] = true;
            remaining -= 1;
        }
    }

    let own_lo = usize::try_from(s - ext_start).unwrap_or(usize::MAX);
    let own_hi = usize::try_from(e - ext_start).unwrap_or(usize::MAX);
    let mut offsets = Vec::new();
    let mut context = Vec::new();
    for i in 0..n {
        if starts[i] < own_lo || starts[i] >= own_hi {
            continue;
        }
        if pass[i] || ctx[i] {
            offsets.push(ext_start + starts[i] as u64);
            context.push(!pass[i]);
        }
    }
    if !context.contains(&true) {
        context.clear();
    }
    Ok(Segment { offsets, context })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_core::MemSource;

    #[test]
    fn align_up_finds_line_starts() {
        let src = MemSource::new(b"ab\ncd\n\nef".to_vec());
        let end = 9;
        assert_eq!(align_up(&src, 0, end).unwrap(), 0);
        assert_eq!(align_up(&src, 1, end).unwrap(), 3);
        assert_eq!(align_up(&src, 3, end).unwrap(), 3);
        assert_eq!(align_up(&src, 4, end).unwrap(), 6);
        assert_eq!(align_up(&src, 7, end).unwrap(), 7);
        assert_eq!(align_up(&src, 8, end).unwrap(), 9);
        assert_eq!(align_up(&src, 50, end).unwrap(), 9);
    }

    #[test]
    fn huge_line_chunks_read_linear_bytes() {
        use std::sync::atomic::{AtomicU64, Ordering};
        struct Counting(MemSource, AtomicU64);
        impl ReadAt for Counting {
            fn read_at(&self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                let n = self.0.read_at(o, b)?;
                self.1.fetch_add(n as u64, Ordering::Relaxed);
                Ok(n)
            }
            fn len(&self) -> io::Result<u64> {
                self.0.len()
            }
        }
        let mut data = vec![b'a'; 4 << 20];
        data.extend_from_slice(b"\nhit\n");
        let len = data.len() as u64;
        let src = Counting(MemSource::new(data), AtomicU64::new(0));
        let pred = crate::Matcher::compile(&crate::Query::literal("hit")).unwrap();
        let chunk = 16 * 1024u64;
        let mut found = Vec::new();
        let mut a = 0;
        while a < len {
            let seg = scan_chunk(&src, a, (a + chunk).min(len), len, &pred, 0, 0).unwrap();
            found.extend(seg.offsets);
            a += chunk;
        }
        assert_eq!(found, vec![(4 << 20) + 1]);
        let read = src.1.load(Ordering::Relaxed);
        assert!(read < 3 * len, "read {read} bytes for a {len} byte file");
    }

    #[test]
    fn back_and_forward_lines() {
        let src = MemSource::new(b"a\nbb\nccc\ndddd\n".to_vec());
        assert_eq!(back_lines(&src, 9, 1).unwrap(), 5);
        assert_eq!(back_lines(&src, 9, 2).unwrap(), 2);
        assert_eq!(back_lines(&src, 9, 3).unwrap(), 0);
        assert_eq!(back_lines(&src, 9, 99).unwrap(), 0);
        assert_eq!(back_lines(&src, 0, 3).unwrap(), 0);
        assert_eq!(fwd_lines(&src, 2, 1, 14).unwrap(), 5);
        assert_eq!(fwd_lines(&src, 2, 2, 14).unwrap(), 9);
        assert_eq!(fwd_lines(&src, 2, 9, 14).unwrap(), 14);
    }
}
