//! Sparse line index: one checkpoint per ~64 KiB (PLAN.md §5.4).
//!
//! A [`Checkpoint`] `(offset, line)` says "there are exactly `line` newlines
//! (`\n`) in `[0, offset)`". Checkpoints sit at multiples of the spacing, so a
//! 10 GB file needs about 160k of them (~2.5 MB). Looking up line *N* means
//! binary-searching a checkpoint and counting newlines inside one block.
//!
//! The index only ever sees the UTF-8 view (see the crate docs), so `\n` is the
//! only terminator it has to know; `\r` handling happens when lines are
//! rendered.
//!
//! Extension is incremental: [`LineIndex::scanner`] snapshots the state,
//! [`Scanner::feed`] consumes bytes without any lock held, and
//! [`LineIndex::commit`] publishes the result. [`LineIndex::extend`] does all
//! three for callers that own the index.

use std::io;

use crate::source::ReadAt;

/// Default distance between checkpoints: 64 KiB.
pub const DEFAULT_SPACING: u64 = 64 * 1024;

/// Bytes read per I/O call while indexing (rounded to a multiple of the spacing).
pub const SCAN_CHUNK: usize = 4 * 1024 * 1024;

/// `line` newlines occur in `[0, offset)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    /// Byte offset of the checkpoint.
    pub offset: u64,
    /// Number of `\n` bytes before `offset` (0-based number of the line that
    /// contains `offset`, or that starts there).
    pub line: u64,
}

/// Result of [`LineIndex::line_of_offset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinePos {
    /// 0-based number of the line containing the offset.
    pub line: u64,
    /// Byte offset where that line starts.
    pub start: u64,
}

/// Incremental newline counter; the lock-free half of index extension.
#[derive(Debug, Clone)]
pub struct Scanner {
    spacing: u64,
    pos: u64,
    newlines: u64,
    tail_start: u64,
}

impl Scanner {
    /// Offset of the next byte this scanner expects.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Consumes `data`, which starts at [`Scanner::position`], pushing any
    /// checkpoints crossed into `out`.
    pub fn feed(&mut self, mut data: &[u8], out: &mut Vec<Checkpoint>) {
        while !data.is_empty() {
            let to_boundary = self.spacing - (self.pos % self.spacing);
            let take = (data.len() as u64).min(to_boundary) as usize;
            let seg = &data[..take];
            let count = memchr::memchr_iter(b'\n', seg).count();
            if count > 0 {
                if let Some(last) = memchr::memrchr(b'\n', seg) {
                    self.tail_start = self.pos + last as u64 + 1;
                }
                self.newlines += count as u64;
            }
            self.pos += take as u64;
            if self.pos % self.spacing == 0 {
                out.push(Checkpoint {
                    offset: self.pos,
                    line: self.newlines,
                });
            }
            data = &data[take..];
        }
    }
}

/// Sparse checkpoint index over a byte source.
#[derive(Debug, Clone)]
pub struct LineIndex {
    spacing: u64,
    cps: Vec<Checkpoint>,
    indexed: u64,
    newlines: u64,
    tail_start: u64,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads as much of `[offset, offset + buf.len())` as exists.
fn read_full(src: &dyn ReadAt, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match src.read_at(offset + got as u64, &mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

impl LineIndex {
    /// Creates an empty index with the default 64 KiB spacing.
    pub fn new() -> Self {
        Self::with_spacing(DEFAULT_SPACING)
    }

    /// Creates an empty index with a custom checkpoint spacing (clamped to >= 1).
    pub fn with_spacing(spacing: u64) -> Self {
        let spacing = spacing.max(1);
        Self {
            spacing,
            cps: vec![Checkpoint { offset: 0, line: 0 }],
            indexed: 0,
            newlines: 0,
            tail_start: 0,
        }
    }

    /// Forgets everything (truncation or rotation).
    pub fn reset(&mut self) {
        *self = Self::with_spacing(self.spacing);
    }

    /// Checkpoint spacing in bytes.
    pub fn spacing(&self) -> u64 {
        self.spacing
    }

    /// Number of stored checkpoints (memory is proportional to this).
    pub fn checkpoint_count(&self) -> usize {
        self.cps.len()
    }

    /// Bytes scanned so far; every newline in `[0, indexed_bytes)` is counted.
    pub fn indexed_bytes(&self) -> u64 {
        self.indexed
    }

    /// Number of lines in the indexed region. A trailing unterminated line
    /// counts as a line.
    pub fn total_lines(&self) -> u64 {
        self.newlines + u64::from(self.indexed > self.tail_start)
    }

    /// Offset just after the last newline in the indexed region (start of the
    /// current, possibly still growing, last line).
    pub fn tail_start(&self) -> u64 {
        self.tail_start
    }

    /// Starts an extension from the current state (cheap; no borrow held).
    pub fn scanner(&self) -> Scanner {
        Scanner {
            spacing: self.spacing,
            pos: self.indexed,
            newlines: self.newlines,
            tail_start: self.tail_start,
        }
    }

    /// Publishes the result of a [`Scanner`] started from this index.
    /// Returns `false` (and changes nothing) if the index moved on meanwhile.
    pub fn commit(&mut self, scanner: &Scanner, checkpoints: Vec<Checkpoint>) -> bool {
        if scanner.pos < self.indexed || scanner.spacing != self.spacing {
            return false;
        }
        if scanner.pos == self.indexed {
            return true;
        }
        self.cps.extend(checkpoints);
        self.indexed = scanner.pos;
        self.newlines = scanner.newlines;
        self.tail_start = scanner.tail_start;
        true
    }

    /// Indexes `[indexed_bytes, up_to)` reading from `src`, stopping early when
    /// `cancel()` returns true or the source ends. Returns the new
    /// `indexed_bytes`.
    pub fn extend(
        &mut self,
        src: &dyn ReadAt,
        up_to: u64,
        cancel: &dyn Fn() -> bool,
    ) -> io::Result<u64> {
        let mut sc = self.scanner();
        let mut cps = Vec::new();
        let chunk = chunk_len(self.spacing);
        let mut buf = vec![0u8; chunk];
        while sc.pos < up_to && !cancel() {
            let want = ((up_to - sc.pos) as usize).min(buf.len());
            let n = read_full(src, sc.pos, &mut buf[..want])?;
            if n == 0 {
                break;
            }
            sc.feed(&buf[..n], &mut cps);
        }
        self.commit(&sc, cps);
        Ok(self.indexed)
    }

    /// Byte offset where line `line` starts, or `None` if the indexed region
    /// has no such line (or the source shrank underneath us).
    ///
    /// Reads at most one checkpoint interval from `src`.
    pub fn offset_of_line(&self, src: &dyn ReadAt, line: u64) -> io::Result<Option<u64>> {
        if line >= self.total_lines() {
            return Ok(None);
        }
        if line == 0 {
            return Ok(Some(0));
        }
        if line == self.newlines {
            return Ok(Some(self.tail_start));
        }
        // Last checkpoint whose newline count is strictly below `line`.
        let i = self.cps.partition_point(|c| c.line < line) - 1;
        let cp = self.cps[i];
        let mut need = line - cp.line; // find the `need`-th newline at/after cp.offset
        let mut pos = cp.offset;
        let mut buf = vec![0u8; (self.spacing as usize).clamp(4096, 64 * 1024)];
        while pos < self.indexed {
            let want = ((self.indexed - pos) as usize).min(buf.len());
            let n = read_full(src, pos, &mut buf[..want])?;
            if n == 0 {
                return Ok(None);
            }
            for p in memchr::memchr_iter(b'\n', &buf[..n]) {
                need -= 1;
                if need == 0 {
                    return Ok(Some(pos + p as u64 + 1));
                }
            }
            pos += n as u64;
        }
        Ok(None)
    }

    /// Line containing byte `offset` (clamped to the indexed region) and where
    /// it starts. Exact. If `offset` is a line start the result is that line.
    ///
    /// Scans at most one checkpoint interval forward, plus a backward scan to
    /// find the line start when the line began before the checkpoint.
    pub fn line_of_offset(&self, src: &dyn ReadAt, offset: u64) -> io::Result<LinePos> {
        let off = offset.min(self.indexed);
        let i = self.cps.partition_point(|c| c.offset <= off) - 1;
        let cp = self.cps[i];
        let mut line = cp.line;
        let mut last_nl: Option<u64> = None;
        let mut pos = cp.offset;
        let mut buf = vec![0u8; (self.spacing as usize).clamp(4096, 64 * 1024)];
        while pos < off {
            let want = ((off - pos) as usize).min(buf.len());
            let n = read_full(src, pos, &mut buf[..want])?;
            if n == 0 {
                break;
            }
            let seg = &buf[..n];
            line += memchr::memchr_iter(b'\n', seg).count() as u64;
            if let Some(p) = memchr::memrchr(b'\n', seg) {
                last_nl = Some(pos + p as u64);
            }
            pos += n as u64;
        }
        let start = match last_nl {
            Some(p) => p + 1,
            None => line_start_before(src, cp.offset, u64::MAX)?,
        };
        Ok(LinePos { line, start })
    }

    /// Average line length seen so far (for estimates); a guess of 100 when
    /// nothing is indexed yet.
    pub fn avg_line_len(&self) -> f64 {
        if self.newlines == 0 {
            100.0
        } else {
            (self.tail_start.max(1) as f64) / self.newlines as f64
        }
    }

    /// Approximate line number for a byte offset beyond the indexed region.
    /// (Offsets inside the region should use [`LineIndex::line_of_offset`].)
    pub fn estimate_line_of_offset(&self, offset: u64) -> u64 {
        if offset <= self.indexed {
            // Cheap approximation from checkpoints; callers wanting exactness
            // use `line_of_offset`.
            let i = self.cps.partition_point(|c| c.offset <= offset) - 1;
            let cp = self.cps[i];
            return cp.line + ((offset - cp.offset) as f64 / self.avg_line_len()) as u64;
        }
        self.newlines + ((offset - self.indexed) as f64 / self.avg_line_len()) as u64
    }

    /// Approximate byte offset of `line` beyond the indexed region.
    pub fn estimate_offset_of_line(&self, line: u64) -> u64 {
        let known = self.total_lines();
        if line < known {
            return 0;
        }
        self.indexed + ((line - known) as f64 * self.avg_line_len()) as u64
    }

    /// Estimated total lines for a source of `len` bytes. Exact when the
    /// index is complete.
    pub fn estimate_total_lines(&self, len: u64) -> u64 {
        if len <= self.indexed {
            return self.total_lines();
        }
        self.total_lines() + ((len - self.indexed) as f64 / self.avg_line_len()).ceil() as u64
    }
}

/// Chunk size for reads: a multiple of `spacing`, about [`SCAN_CHUNK`] for the
/// default spacing and at least 64 KiB for tiny test spacings.
fn chunk_len(spacing: u64) -> usize {
    let target = if spacing >= SCAN_CHUNK as u64 {
        spacing
    } else {
        (SCAN_CHUNK as u64 / spacing) * spacing
    };
    if spacing < 4096 {
        // tiny spacings (tests): keep buffers small but still multi-checkpoint
        return ((64 * 1024 / spacing).max(1) * spacing) as usize;
    }
    target as usize
}

/// Start of the line containing byte `offset` (the position after the last
/// `\n` strictly before `offset`, or 0), scanning backward at most `max_back`
/// bytes. When the limit is hit the scan position is returned (mid-line).
pub fn line_start_before(src: &dyn ReadAt, offset: u64, max_back: u64) -> io::Result<u64> {
    let mut end = offset;
    let floor = offset.saturating_sub(max_back);
    let mut buf = vec![0u8; 64 * 1024];
    while end > floor {
        let start = end.saturating_sub(buf.len() as u64).max(floor);
        let n = read_full(src, start, &mut buf[..(end - start) as usize])?;
        if n < (end - start) as usize {
            // Source shrank; give up gracefully.
            return Ok(floor.max(start));
        }
        if let Some(p) = memchr::memrchr(b'\n', &buf[..n]) {
            return Ok(start + p as u64 + 1);
        }
        end = start;
    }
    Ok(floor)
}

/// Finds the start of the line that begins `count - 1` lines before the last
/// line of `[0, end)`, i.e. the offset from which reading forward yields the
/// last `count` lines. Scans backward from `end` reading at most `max_back`
/// bytes; returns `(offset, complete)` where `complete` is false if the limit
/// was hit before the requested start was found (offset is then mid-line).
pub fn find_tail_start(
    src: &dyn ReadAt,
    end: u64,
    count: usize,
    max_back: u64,
) -> io::Result<(u64, bool)> {
    if end == 0 || count == 0 {
        return Ok((end, true));
    }
    // A terminator on the very last byte belongs to the last line.
    let mut last = [0u8; 1];
    let scan_end = if read_full(src, end - 1, &mut last)? == 1 && last[0] == b'\n' {
        end - 1
    } else {
        end
    };
    let floor = scan_end.saturating_sub(max_back);
    let mut remaining = count;
    let mut cur = scan_end;
    let mut buf = vec![0u8; 256 * 1024];
    while cur > floor {
        let start = cur.saturating_sub(buf.len() as u64).max(floor);
        let want = (cur - start) as usize;
        let n = read_full(src, start, &mut buf[..want])?;
        if n < want {
            return Ok((start, false));
        }
        let seg = &buf[..n];
        let mut hi = seg.len();
        while let Some(p) = memchr::memrchr(b'\n', &seg[..hi]) {
            remaining -= 1;
            if remaining == 0 {
                return Ok((start + p as u64 + 1, true));
            }
            hi = p;
        }
        cur = start;
    }
    Ok((floor, floor == 0))
}

#[cfg(test)]
pub(crate) mod naive {
    /// Reference: start offsets of every line.
    pub fn line_starts(data: &[u8]) -> Vec<u64> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemSource;
    use proptest::prelude::*;

    fn check_against_naive(idx: &LineIndex, data: &[u8]) {
        let src = MemSource::new(data.to_vec());
        let starts = naive::line_starts(data);
        assert_eq!(idx.indexed_bytes(), data.len() as u64);
        assert_eq!(idx.total_lines(), starts.len() as u64);
        for (n, s) in starts.iter().enumerate() {
            assert_eq!(
                idx.offset_of_line(&src, n as u64).unwrap(),
                Some(*s),
                "line {n}"
            );
        }
        assert_eq!(idx.offset_of_line(&src, starts.len() as u64).unwrap(), None);
        for off in 0..=data.len() as u64 {
            let pos = idx.line_of_offset(&src, off).unwrap();
            // Reference: number of newlines before off, and last newline before off.
            let before = &data[..off as usize];
            let line = before.iter().filter(|b| **b == b'\n').count() as u64;
            let start = before
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |p| p as u64 + 1);
            assert_eq!(pos, LinePos { line, start }, "offset {off}");
        }
    }

    fn data_strategy() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(
            prop_oneof![
                4 => Just(b'\n'),
                2 => Just(b'\r'),
                8 => any::<u8>().prop_map(|b| b'a' + (b % 26)),
                1 => Just(b' '),
            ],
            0..600,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn agrees_with_naive_reference(data in data_strategy(), spacing in 1u64..70) {
            let src = MemSource::new(data.clone());
            let mut idx = LineIndex::with_spacing(spacing);
            idx.extend(&src, data.len() as u64, &|| false).unwrap();
            check_against_naive(&idx, &data);
        }

        #[test]
        fn random_appends_and_truncations(
            ops in prop::collection::vec((data_strategy(), 0u8..4, 0usize..600), 1..8),
            spacing in 1u64..70,
        ) {
            let src = MemSource::new(Vec::new());
            let mut model: Vec<u8> = Vec::new();
            let mut idx = LineIndex::with_spacing(spacing);
            for (chunk, kind, cut) in ops {
                if kind == 0 {
                    // truncate: the index must be reset, as the document does.
                    model.truncate(cut.min(model.len()));
                    src.replace(model.clone());
                    idx.reset();
                } else {
                    model.extend_from_slice(&chunk);
                    src.append(&chunk);
                }
                // Sometimes extend only part of the way first.
                if kind == 3 && !model.is_empty() {
                    idx.extend(&src, (model.len() / 2) as u64, &|| false).unwrap();
                }
                idx.extend(&src, model.len() as u64, &|| false).unwrap();
                check_against_naive(&idx, &model);
            }
        }

        #[test]
        fn tail_start_matches_naive(data in data_strategy(), count in 1usize..12) {
            let src = MemSource::new(data.clone());
            let starts = naive::line_starts(&data);
            let (got, complete) = find_tail_start(&src, data.len() as u64, count, u64::MAX).unwrap();
            prop_assert!(complete);
            let expect = if starts.is_empty() { 0 } else { starts[starts.len().saturating_sub(count)] };
            prop_assert_eq!(got, if data.is_empty() { data.len() as u64 } else { expect });
        }
    }

    #[test]
    fn scanner_split_feeds_equal_one_shot() {
        let data: Vec<u8> = (0..5000u32)
            .map(|i| if i % 7 == 0 { b'\n' } else { b'x' })
            .collect();
        let mut a = LineIndex::with_spacing(100);
        let src = MemSource::new(data.clone());
        a.extend(&src, data.len() as u64, &|| false).unwrap();
        let mut b = LineIndex::with_spacing(100);
        for piece in data.chunks(37) {
            let mut sc = b.scanner();
            let mut cps = Vec::new();
            sc.feed(piece, &mut cps);
            assert!(b.commit(&sc, cps));
        }
        assert_eq!(a.total_lines(), b.total_lines());
        assert_eq!(a.checkpoint_count(), b.checkpoint_count());
    }

    #[test]
    fn cancel_stops_early_and_resumes() {
        let data = vec![b'a'; 100_000];
        let src = MemSource::new(data);
        let mut idx = LineIndex::with_spacing(1000);
        assert_eq!(idx.extend(&src, 100_000, &|| true).unwrap(), 0);
        assert_eq!(idx.extend(&src, 100_000, &|| false).unwrap(), 100_000);
        assert_eq!(idx.total_lines(), 1);
    }

    #[test]
    fn estimates_are_sane() {
        let mut data = Vec::new();
        for _ in 0..1000 {
            data.extend_from_slice(b"0123456789\n"); // 11 bytes per line
        }
        let src = MemSource::new(data);
        let mut idx = LineIndex::with_spacing(4096);
        idx.extend(&src, 5500, &|| false).unwrap(); // half
        let est = idx.estimate_total_lines(11_000);
        assert!((990..=1010).contains(&est), "est {est}");
        let l = idx.estimate_line_of_offset(11_000 - 11);
        assert!((985..=1000).contains(&l), "l {l}");
    }

    #[test]
    fn empty_and_single_line() {
        let src = MemSource::new(Vec::new());
        let mut idx = LineIndex::new();
        idx.extend(&src, 0, &|| false).unwrap();
        assert_eq!(idx.total_lines(), 0);
        assert_eq!(idx.offset_of_line(&src, 0).unwrap(), None);
    }
}
