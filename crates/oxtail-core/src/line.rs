//! Display lines: reading lines out of the UTF-8 view.
//!
//! Only the first `max_display_len` bytes of a line are kept as text; the true
//! length is still measured so the next line can be found. `\r` before `\n` is
//! stripped from the text (it is part of the terminator and counted in `len`).

use std::io;

use crate::source::ReadAt;

/// Default maximum number of bytes of a line kept for display: 16 KiB.
pub const DEFAULT_MAX_DISPLAY_LEN: usize = 16 * 1024;

/// One line of the UTF-8 view, ready for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 0-based line number (show as `number + 1`). Only trustworthy when
    /// `number_exact` is true.
    pub number: u64,
    /// `false` when the line lies beyond the indexed region and `number` is
    /// an estimate (show as "≈").
    pub number_exact: bool,
    /// Byte offset of the line start in the UTF-8 view.
    pub offset: u64,
    /// Length in bytes of the whole line **including** its terminator
    /// (`\n` or `\r\n`, when present). The next line starts at `offset + len`.
    pub len: u64,
    /// Lossy UTF-8 text without terminator, at most `max_display_len` bytes,
    /// cut on a char boundary.
    pub text: String,
    /// `true` if the line was longer than `max_display_len` and `text` is cut.
    pub truncated: bool,
}

/// Cuts `s` to at most `max` bytes on a char boundary.
/// Returns `true` if anything was removed.
pub fn truncate_on_boundary(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    true
}

/// Accumulates one line while streaming over chunks.
struct Partial {
    offset: u64,
    /// Bytes seen so far including the terminator.
    len: u64,
    /// Content bytes seen so far (no terminator).
    content_len: u64,
    /// Stored prefix of the content (bounded).
    content: Vec<u8>,
    last: u8,
}

impl Partial {
    fn new(offset: u64) -> Self {
        Partial {
            offset,
            len: 0,
            content_len: 0,
            content: Vec::new(),
            last: 0,
        }
    }

    fn push(&mut self, seg: &[u8], keep: usize) {
        if seg.is_empty() {
            return;
        }
        self.len += seg.len() as u64;
        self.content_len += seg.len() as u64;
        self.last = seg[seg.len() - 1];
        let room = keep.saturating_sub(self.content.len());
        self.content.extend_from_slice(&seg[..seg.len().min(room)]);
    }

    fn finish(mut self, terminated: bool, number: u64, exact: bool, max: usize) -> Line {
        if terminated {
            self.len += 1;
        }
        // A trailing `\r` is hidden even on an unterminated last line: it is
        // usually the first half of a `\r\n` that is still being written.
        if self.content_len > 0 && self.last == b'\r' {
            if self.content.len() as u64 == self.content_len {
                self.content.pop();
            }
            self.content_len -= 1;
        }
        let shown = self.content.len().min(max.saturating_add(3));
        let mut text = String::from_utf8_lossy(&self.content[..shown]).into_owned();
        if self.offset == 0 && text.starts_with('\u{FEFF}') {
            text.remove(0);
        }
        let cut = truncate_on_boundary(&mut text, max);
        Line {
            number,
            number_exact: exact,
            offset: self.offset,
            len: self.len,
            text,
            truncated: cut || self.content_len > max as u64,
        }
    }
}

/// Reads up to `count` consecutive lines starting at line-start `start`,
/// never looking at bytes at or past `limit`. Line `i` gets number
/// `first_number + i`. A final unterminated line is included.
///
/// Memory is bounded: at most `max_display_len` bytes are kept per line no
/// matter how long the line is (the rest is only scanned for its newline).
pub fn read_lines(
    src: &dyn ReadAt,
    start: u64,
    count: usize,
    limit: u64,
    first_number: u64,
    exact: bool,
    max_display_len: usize,
) -> io::Result<Vec<Line>> {
    let mut lines = Vec::with_capacity(count.min(4096));
    if count == 0 || start >= limit {
        return Ok(lines);
    }
    let keep = max_display_len.saturating_add(4);
    let mut chunk_size = count.saturating_mul(256).clamp(4096, 256 * 1024);
    let mut buf = vec![0u8; chunk_size];
    let mut pos = start;
    let mut cur = Partial::new(start);
    'outer: while pos < limit {
        let want = ((limit - pos) as usize).min(chunk_size);
        let n = src.read_at(pos, &mut buf[..want])?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        let mut i = 0;
        while i < n {
            match memchr::memchr(b'\n', &chunk[i..]) {
                Some(p) => {
                    cur.push(&chunk[i..i + p], keep);
                    let number = first_number + lines.len() as u64;
                    let next_off = pos + (i + p) as u64 + 1;
                    let done = std::mem::replace(&mut cur, Partial::new(next_off));
                    lines.push(done.finish(true, number, exact, max_display_len));
                    i += p + 1;
                    if lines.len() >= count {
                        break 'outer;
                    }
                }
                None => {
                    cur.push(&chunk[i..], keep);
                    i = n;
                }
            }
        }
        pos += n as u64;
        if chunk_size < 256 * 1024 {
            chunk_size = (chunk_size * 2).min(256 * 1024);
            buf.resize(chunk_size, 0);
        }
    }
    if lines.len() < count && cur.len > 0 {
        let number = first_number + lines.len() as u64;
        lines.push(cur.finish(false, number, exact, max_display_len));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::naive;
    use crate::source::MemSource;
    use proptest::prelude::*;

    fn read_all(data: &[u8], max: usize) -> Vec<Line> {
        let src = MemSource::new(data.to_vec());
        read_lines(&src, 0, usize::MAX >> 1, data.len() as u64, 0, true, max).unwrap()
    }

    #[test]
    fn crlf_lf_and_unterminated() {
        let lines = read_all(b"a\r\nb\n\nlast", 100);
        let texts: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "", "last"]);
        assert_eq!(lines[0].len, 3);
        assert_eq!(lines[1].offset, 3);
        assert_eq!(lines[2].len, 1);
        assert_eq!(lines[3].offset, 6);
        assert_eq!(lines[3].len, 4);
        assert!(lines.iter().all(|l| !l.truncated));
    }

    #[test]
    fn long_lines_truncate_on_char_boundary() {
        let mut data = "é".repeat(100).into_bytes(); // 200 bytes
        data.push(b'\n');
        data.extend_from_slice(b"next\n");
        let lines = read_all(&data, 51);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].truncated);
        assert_eq!(lines[0].text.len(), 50);
        assert_eq!(lines[0].len, 201);
        assert_eq!(lines[1].text, "next");
        assert_eq!(lines[1].offset, 201);
    }

    #[test]
    fn huge_line_is_measured_but_not_stored() {
        let mut data = vec![b'x'; 3 * 1024 * 1024];
        data.extend_from_slice(b"\nz\n");
        let lines = read_all(&data, 16 * 1024);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text.len(), 16 * 1024);
        assert!(lines[0].truncated);
        assert_eq!(lines[0].len, 3 * 1024 * 1024 + 1);
        assert_eq!(lines[1].text, "z");
    }

    #[test]
    fn invalid_utf8_and_binary_do_not_panic() {
        let lines = read_all(&[0xff, 0x00, 0xc3, b'\n', 0x80, 0x80], 10);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].text.contains('\u{FFFD}'));
    }

    #[test]
    fn strips_utf8_bom_on_first_line() {
        let lines = read_all(b"\xef\xbb\xbfhello\nworld\n", 100);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[0].len, 9);
    }

    #[test]
    fn respects_start_count_and_limit() {
        let src = MemSource::new(b"aa\nbb\ncc\ndd\n".to_vec());
        let l = read_lines(&src, 3, 2, 12, 1, false, 100).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].number, l[0].text.as_str()), (1, "bb"));
        assert!(!l[0].number_exact);
        assert_eq!(l[1].text, "cc");
        // limit cuts mid-line
        let l = read_lines(&src, 0, 10, 4, 0, true, 100).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[1].text, "b");
    }

    proptest! {
        #[test]
        fn matches_naive_split(
            data in prop::collection::vec(prop_oneof![
                3 => Just(b'\n'), 2 => Just(b'\r'), 6 => Just(b'x'), 1 => any::<u8>()
            ], 0..400),
            max in 1usize..40,
        ) {
            let lines = read_all(&data, max);
            let starts = naive::line_starts(&data);
            prop_assert_eq!(lines.len(), starts.len());
            for (i, l) in lines.iter().enumerate() {
                prop_assert_eq!(l.offset, starts[i]);
                let end = starts.get(i + 1).copied().unwrap_or(data.len() as u64);
                prop_assert_eq!(l.len, end - l.offset);
                let terminated = data[end as usize - 1] == b'\n';
                let mut content = &data[l.offset as usize..end as usize];
                if terminated {
                    content = &content[..content.len() - 1];
                }
                if content.last() == Some(&b'\r') {
                    content = &content[..content.len() - 1];
                }
                prop_assert!(l.truncated || content.len() <= max);
                prop_assert!(!l.truncated || String::from_utf8_lossy(content).len() > max || content.len() > max);
                prop_assert!(l.text.len() <= max);
                if !l.truncated && l.offset != 0 {
                    let want = String::from_utf8_lossy(content).into_owned();
                    prop_assert_eq!(&l.text, &want);
                }
            }
        }
    }
}
