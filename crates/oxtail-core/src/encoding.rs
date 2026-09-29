//! Encoding detection and incremental transcoding to the UTF-8 view
//! (PLAN.md §5.6).
//!
//! Detection order: BOM, then UTF-16 NUL heuristics, then valid UTF-8 / ASCII,
//! then `chardetng` on the first 64 KiB. A [`Transcoder`] converts appended
//! raw bytes to UTF-8 incrementally and copes with a multi-byte sequence split
//! across two appends (the partial bytes are held back until the rest arrives).

use encoding_rs::{Decoder, Encoding, UTF_8, UTF_16BE, UTF_16LE};

use crate::error::CoreError;

/// Number of leading bytes examined by [`detect`].
pub const SAMPLE_LEN: usize = 64 * 1024;

/// A text encoding (thin wrapper over `encoding_rs`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TextEncoding(&'static Encoding);

impl std::fmt::Debug for TextEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextEncoding({})", self.0.name())
    }
}

impl TextEncoding {
    /// UTF-8.
    pub const UTF_8: TextEncoding = TextEncoding(UTF_8);
    /// UTF-16 little endian.
    pub const UTF_16LE: TextEncoding = TextEncoding(UTF_16LE);
    /// UTF-16 big endian.
    pub const UTF_16BE: TextEncoding = TextEncoding(UTF_16BE);

    /// Looks an encoding up by its WHATWG label (`"windows-1252"`, `"latin1"`,
    /// `"utf-16le"`, ...).
    pub fn from_label(label: &str) -> Result<Self, CoreError> {
        Encoding::for_label(label.trim().as_bytes())
            .map(TextEncoding)
            .ok_or_else(|| CoreError::UnknownEncoding(label.to_string()))
    }

    /// Canonical name, e.g. `"UTF-8"` or `"windows-1252"`.
    pub fn name(&self) -> &'static str {
        self.0.name()
    }

    /// `true` for UTF-8, whose bytes can be used as-is.
    pub fn is_utf8(&self) -> bool {
        self.0 == UTF_8
    }

    /// The underlying `encoding_rs` encoding.
    pub fn as_encoding_rs(&self) -> &'static Encoding {
        self.0
    }
}

/// How the encoding of a file is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncodingChoice {
    /// Detect from the first 64 KiB.
    #[default]
    Auto,
    /// Use exactly this encoding (manual override).
    Fixed(TextEncoding),
}

/// Line terminator style of the raw file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n` or `\r\n` (the default; `\r\n` is handled at display time).
    #[default]
    Lf,
    /// Lone `\r` (classic Mac); the file is spooled with `\r` -> `\n`.
    Cr,
}

/// Outcome of [`detect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    /// The chosen encoding.
    pub encoding: TextEncoding,
    /// Line terminator style.
    pub line_ending: LineEnding,
}

impl Detected {
    /// `true` when the raw bytes can be used directly as the UTF-8 view.
    pub fn is_passthrough(&self) -> bool {
        self.encoding.is_utf8() && self.line_ending == LineEnding::Lf
    }
}

/// Detects encoding and line-ending style from the start of a file.
/// `choice` may force the encoding; line endings are always detected.
pub fn detect(sample: &[u8], choice: EncodingChoice) -> Detected {
    let sample = &sample[..sample.len().min(SAMPLE_LEN)];
    let encoding = match choice {
        EncodingChoice::Fixed(e) => e,
        EncodingChoice::Auto => detect_encoding(sample),
    };
    let line_ending = detect_line_ending(sample, encoding);
    Detected {
        encoding,
        line_ending,
    }
}

fn detect_encoding(sample: &[u8]) -> TextEncoding {
    if sample.is_empty() {
        return TextEncoding::UTF_8;
    }
    if let Some((enc, _)) = Encoding::for_bom(sample) {
        return TextEncoding(enc);
    }
    if let Some(enc) = utf16_by_nuls(sample) {
        return enc;
    }
    if sample.is_ascii() || is_utf8_allowing_cut_tail(sample) {
        return TextEncoding::UTF_8;
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(sample, false);
    TextEncoding(det.guess(None, chardetng::Utf8Detection::Allow))
}

fn is_utf8_allowing_cut_tail(sample: &[u8]) -> bool {
    match std::str::from_utf8(sample) {
        Ok(_) => true,
        // `error_len() == None` means an incomplete sequence at the very end.
        Err(e) => e.error_len().is_none(),
    }
}

/// Heuristic for BOM-less UTF-16: mostly-ASCII text has a NUL in every other byte.
fn utf16_by_nuls(sample: &[u8]) -> Option<TextEncoding> {
    let pairs = sample.len() / 2;
    if pairs < 2 {
        return None;
    }
    let (mut even, mut odd) = (0usize, 0usize);
    for p in sample.chunks_exact(2) {
        even += usize::from(p[0] == 0);
        odd += usize::from(p[1] == 0);
    }
    // >= 30% NULs on one side and <= 10% (of that) on the other.
    if odd * 10 >= pairs * 3 && even * 10 <= odd {
        Some(TextEncoding::UTF_16LE)
    } else if even * 10 >= pairs * 3 && odd * 10 <= even {
        Some(TextEncoding::UTF_16BE)
    } else {
        None
    }
}

fn detect_line_ending(sample: &[u8], enc: TextEncoding) -> LineEnding {
    let (has_cr, has_lf) = if enc.is_utf8() {
        (
            has_cr_before_end(sample),
            memchr::memchr(b'\n', sample).is_some(),
        )
    } else {
        let mut t = Transcoder::new(enc, LineEnding::Lf);
        let mut out = Vec::new();
        t.transcode(sample, &mut out);
        (
            has_cr_before_end(&out),
            memchr::memchr(b'\n', &out).is_some(),
        )
    };
    if has_cr && !has_lf {
        LineEnding::Cr
    } else {
        LineEnding::Lf
    }
}

/// `true` if `out` (UTF-8) contains a `\r`, not counting one on the very last
/// byte: that may be the first half of a `\r\n` cut off by the end of the
/// sample.
fn has_cr_before_end(out: &[u8]) -> bool {
    let body = match out.split_last() {
        Some((b'\r', rest)) => rest,
        _ => out,
    };
    memchr::memchr(b'\r', body).is_some()
}

/// Incremental raw -> UTF-8 converter with `\r` -> `\n` mapping for
/// [`LineEnding::Cr`] files.
///
/// Invalid sequences become U+FFFD; nothing here can fail or panic on user data.
pub struct Transcoder {
    decoder: Decoder,
    cr_mode: bool,
    last_was_cr: bool,
    fed: u64,
}

impl Transcoder {
    /// Creates a transcoder. A leading BOM matching the encoding is removed.
    pub fn new(encoding: TextEncoding, line_ending: LineEnding) -> Self {
        Self {
            decoder: encoding.0.new_decoder_with_bom_removal(),
            cr_mode: line_ending == LineEnding::Cr,
            last_was_cr: false,
            fed: 0,
        }
    }

    /// Number of raw bytes consumed so far (including any partial sequence
    /// held inside the decoder).
    pub fn raw_consumed(&self) -> u64 {
        self.fed
    }

    /// Converts `input` and appends UTF-8 to `out`. A multi-byte sequence cut
    /// off at the end of `input` is remembered and completed by the next call.
    pub fn transcode(&mut self, input: &[u8], out: &mut Vec<u8>) {
        self.fed += input.len() as u64;
        let start = out.len();
        let mut src = input;
        loop {
            let need = self
                .decoder
                .max_utf8_buffer_length(src.len())
                .unwrap_or(src.len().saturating_mul(3).saturating_add(16))
                .max(16);
            let old = out.len();
            out.resize(old + need, 0);
            let (result, read, written, _errors) =
                self.decoder.decode_to_utf8(src, &mut out[old..], false);
            out.truncate(old + written);
            src = &src[read..];
            if matches!(result, encoding_rs::CoderResult::InputEmpty) {
                break;
            }
        }
        if self.cr_mode {
            self.map_cr(out, start);
        }
    }

    /// Rewrites `\r` and `\r\n` to `\n` in `out[start..]`.
    fn map_cr(&mut self, out: &mut Vec<u8>, start: usize) {
        let mut w = start;
        for r in start..out.len() {
            let b = out[r];
            let skip = b == b'\n' && self.last_was_cr;
            self.last_was_cr = b == b'\r';
            if skip {
                continue;
            }
            out[w] = if b == b'\r' { b'\n' } else { b };
            w += 1;
        }
        out.truncate(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str, be: bool, bom: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if bom {
            v.extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
        }
        for u in s.encode_utf16() {
            v.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        v
    }

    fn convert(enc: TextEncoding, le: LineEnding, raw: &[u8]) -> String {
        let mut t = Transcoder::new(enc, le);
        let mut out = Vec::new();
        t.transcode(raw, &mut out);
        String::from_utf8(out).unwrap()
    }

    const TEXT: &str = "2024-01-01 hello wörld\nsecond line é\n";

    #[test]
    fn utf16_with_and_without_bom() {
        for be in [false, true] {
            for bom in [false, true] {
                let raw = utf16(TEXT, be, bom);
                let d = detect(&raw, EncodingChoice::Auto);
                let expect = if be {
                    TextEncoding::UTF_16BE
                } else {
                    TextEncoding::UTF_16LE
                };
                assert_eq!(d.encoding, expect, "be={be} bom={bom}");
                assert_eq!(d.line_ending, LineEnding::Lf);
                assert_eq!(convert(d.encoding, d.line_ending, &raw), TEXT);
            }
        }
    }

    #[test]
    fn windows_1252_is_detected_and_decoded() {
        // "café résumé naïve" with typical Latin-1 high bytes
        let raw = b"caf\xe9 r\xe9sum\xe9 na\xefve d\xe9j\xe0 vu, se\xf1or\n".repeat(20);
        let d = detect(&raw, EncodingChoice::Auto);
        assert!(!d.encoding.is_utf8(), "{:?}", d.encoding);
        let s = convert(d.encoding, d.line_ending, &raw[..40]);
        assert!(s.starts_with("café résumé"), "{s}");
    }

    #[test]
    fn utf8_and_ascii_pass_through() {
        assert!(detect(b"plain ascii\n", EncodingChoice::Auto).is_passthrough());
        assert!(detect("héllo wörld\n".as_bytes(), EncodingChoice::Auto).is_passthrough());
        assert!(detect(b"", EncodingChoice::Auto).is_passthrough());
        // Utf-8 cut in the middle of a char at the sample end.
        let mut v = "aé".as_bytes().to_vec();
        v.pop();
        assert!(detect(&v, EncodingChoice::Auto).encoding.is_utf8());
    }

    #[test]
    fn lone_cr_detected_and_mapped() {
        let raw = b"one\rtwo\rthree\r";
        let d = detect(raw, EncodingChoice::Auto);
        assert_eq!(d.line_ending, LineEnding::Cr);
        assert!(!d.is_passthrough());
        assert_eq!(convert(d.encoding, d.line_ending, raw), "one\ntwo\nthree\n");
        // CRLF is not lone CR.
        assert_eq!(
            detect(b"a\r\nb\r\n", EncodingChoice::Auto).line_ending,
            LineEnding::Lf
        );
        // A stray CRLF inside a CR file must not double the newline.
        assert_eq!(
            convert(TextEncoding::UTF_8, LineEnding::Cr, b"a\r\nb\rc"),
            "a\nb\nc"
        );
    }

    #[test]
    fn trailing_cr_of_the_sample_is_ambiguous() {
        // Could be the first half of a CRLF: not lone-CR.
        assert_eq!(
            detect(b"a\r", EncodingChoice::Auto).line_ending,
            LineEnding::Lf
        );
        assert_eq!(
            detect(&utf16("a\r", false, true), EncodingChoice::Auto).line_ending,
            LineEnding::Lf
        );
        // An earlier CR settles it.
        assert_eq!(
            detect(b"a\rb\r", EncodingChoice::Auto).line_ending,
            LineEnding::Cr
        );
    }

    #[test]
    fn invalid_utf8_is_lossy_never_panics() {
        let raw = [b'a', 0xff, 0xfe, b'b', 0xc3, b'\n'];
        let s = convert(TextEncoding::UTF_8, LineEnding::Lf, &raw);
        assert!(s.contains('\u{FFFD}'));
        assert!(s.starts_with('a'));
    }

    #[test]
    fn multibyte_char_split_across_appends() {
        let text = "héllo €uro 日本語\n";
        let raw = text.as_bytes();
        for split in 1..raw.len() {
            let mut t = Transcoder::new(TextEncoding::UTF_8, LineEnding::Cr);
            let mut out = Vec::new();
            t.transcode(&raw[..split], &mut out);
            t.transcode(&raw[split..], &mut out);
            assert_eq!(String::from_utf8(out).unwrap(), text, "split {split}");
        }
        // Same for UTF-16LE, splitting inside a code unit and a surrogate pair.
        let raw16 = utf16("a😀b\n", false, false);
        for split in 1..raw16.len() {
            let mut t = Transcoder::new(TextEncoding::UTF_16LE, LineEnding::Lf);
            let mut out = Vec::new();
            t.transcode(&raw16[..split], &mut out);
            t.transcode(&raw16[split..], &mut out);
            assert_eq!(String::from_utf8(out).unwrap(), "a😀b\n", "split {split}");
        }
    }

    #[test]
    fn manual_override_wins() {
        let d = detect(
            b"plain\n",
            EncodingChoice::Fixed(TextEncoding::from_label("windows-1252").unwrap()),
        );
        assert_eq!(d.encoding.name(), "windows-1252");
        assert!(TextEncoding::from_label("nonsense-enc").is_err());
    }
}
