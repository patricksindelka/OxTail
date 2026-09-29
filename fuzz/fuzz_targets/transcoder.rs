//! Random bytes, random encoding, random append splits: the transcoder must
//! not panic, must emit valid UTF-8, and must give the same output no matter
//! how the input is split.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use oxtail_core::encoding::{Transcoder, detect};
use oxtail_core::{EncodingChoice, LineEnding, TextEncoding};

#[derive(Debug, Arbitrary)]
struct Input {
    data: Vec<u8>,
    /// 0 = auto-detect, otherwise pick from `LABELS`.
    enc: u8,
    cr: bool,
    splits: Vec<u16>,
}

const LABELS: &[&str] = &[
    "utf-8",
    "utf-16le",
    "utf-16be",
    "windows-1252",
    "iso-8859-2",
    "shift_jis",
    "gbk",
    "euc-kr",
    "big5",
    "koi8-r",
    "macintosh",
];

fuzz_target!(|input: Input| {
    let data = &input.data;
    let (encoding, mut ending) = if input.enc == 0 {
        let d = detect(data, EncodingChoice::Auto);
        (d.encoding, d.line_ending)
    } else {
        let label = LABELS[usize::from(input.enc) % LABELS.len()];
        (
            TextEncoding::from_label(label).expect("known label"),
            LineEnding::Lf,
        )
    };
    if input.cr {
        ending = LineEnding::Cr;
    }

    let mut whole = Vec::new();
    let mut t = Transcoder::new(encoding, ending);
    t.transcode(data, &mut whole);
    assert_eq!(t.raw_consumed(), data.len() as u64);
    assert!(std::str::from_utf8(&whole).is_ok(), "output not UTF-8");

    let mut cuts: Vec<usize> = input
        .splits
        .iter()
        .take(16)
        .map(|s| usize::from(*s) % (data.len() + 1))
        .collect();
    cuts.sort_unstable();
    let mut parts = Vec::new();
    let mut t = Transcoder::new(encoding, ending);
    let mut prev = 0;
    for c in cuts.into_iter().chain([data.len()]) {
        t.transcode(&data[prev..c], &mut parts);
        prev = c;
    }
    assert!(
        std::str::from_utf8(&parts).is_ok(),
        "split output not UTF-8"
    );
    assert_eq!(whole, parts, "split-invariance violated");
});
