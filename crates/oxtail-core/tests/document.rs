//! Document tests over in-memory sources and real files.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use common::*;
use oxtail_core::{
    DocEvent, DocState, Document, EncodingChoice, LineEnding, LineRequest, MemSource, OpenOptions,
    ReadAt, TextEncoding,
};
use proptest::prelude::*;

fn numbered(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    v
}

fn small_opts() -> OpenOptions {
    OpenOptions {
        index_spacing: 64,
        ..OpenOptions::default()
    }
}

#[test]
fn range_tail_offsets_and_fraction() {
    let data = numbered(1000);
    let src = Arc::new(MemSource::new(data.clone()));
    let doc = Document::from_source_with(src, "mem", small_opts());
    wait_ready(&doc, data.len() as u64);
    let snap = doc.snapshot();
    assert_eq!(snap.lines.known, 1000);
    assert!(snap.lines.exact);
    assert_eq!(snap.state, DocState::Ready);

    let r = request(
        &doc,
        LineRequest::Range {
            first: 500,
            count: 3,
        },
    );
    assert_eq!(texts(&r), ["line 500", "line 501", "line 502"]);
    assert!(r.iter().all(|l| l.number_exact));
    assert_eq!(r[0].number, 500);

    let t = request(&doc, LineRequest::Tail { count: 2 });
    assert_eq!(texts(&t), ["line 998", "line 999"]);
    assert_eq!(t[0].number, 998);
    assert!(t[0].number_exact);

    let offs = vec![r[2].offset, r[0].offset, 0, data.len() as u64 + 10];
    let a = request(&doc, LineRequest::AtOffsets(offs));
    assert_eq!(texts(&a), ["line 502", "line 500", "line 0"]);
    assert_eq!(a[0].number, 502);

    let f = request(
        &doc,
        LineRequest::ByteFraction {
            fraction: 0.5,
            count: 1,
        },
    );
    assert_eq!(f.len(), 1);
    let expect_line = f[0].number as usize;
    assert_eq!(f[0].text, format!("line {expect_line}"));
    assert!((f[0].offset as f64 - data.len() as f64 / 2.0).abs() < 20.0);
    let end = request(
        &doc,
        LineRequest::ByteFraction {
            fraction: 1.0,
            count: 5,
        },
    );
    assert_eq!(texts(&end), ["line 999"]);
    let start = request(
        &doc,
        LineRequest::ByteFraction {
            fraction: -3.0,
            count: 1,
        },
    );
    assert_eq!(texts(&start), ["line 0"]);

    // Beyond the end.
    assert!(
        request(
            &doc,
            LineRequest::Range {
                first: 5000,
                count: 5
            }
        )
        .is_empty()
    );

    // Blocking helpers.
    assert_eq!(doc.read_lines_blocking(10, 1)[0].text, "line 10");
    assert_eq!(doc.offset_of_line(10).unwrap(), Some(70));
    let (_, by_offset) = doc.read_offsets_blocking_with_generation(&[70, u64::MAX]);
    assert_eq!(texts(&by_offset.unwrap()), ["line 10"]);
    assert_eq!(doc.offset_of_line(5000).unwrap(), None);
    let p = doc.line_of_offset(r[1].offset + 2).unwrap();
    assert_eq!((p.line, p.exact, p.start), (501, true, r[1].offset));
    assert_eq!(doc.source().len().unwrap(), data.len() as u64);
}

#[test]
fn growth_emits_events_and_updates_snapshot() {
    let src = Arc::new(MemSource::new(numbered(10)));
    let doc = Document::from_source_with(src.clone(), "mem", small_opts());
    let woken = Arc::new(AtomicUsize::new(0));
    let w = woken.clone();
    doc.set_waker(Box::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    wait_ready(&doc, numbered(10).len() as u64);
    src.append(b"appended\nlast partial");
    let total = numbered(10).len() as u64 + 21;
    wait_event(
        &doc,
        "Grew",
        |e| matches!(e, DocEvent::Grew { utf8_len } if *utf8_len == total),
    );
    wait_ready(&doc, total);
    assert_eq!(doc.snapshot().lines.known, 12);
    assert!(doc.snapshot().writing);
    let t = request(&doc, LineRequest::Tail { count: 2 });
    assert_eq!(texts(&t), ["appended", "last partial"]);
    assert!(woken.load(Ordering::SeqCst) > 0);
}

#[test]
fn truncation_bumps_generation_and_resets_view() {
    let src = Arc::new(MemSource::new(numbered(200)));
    let doc = Document::from_source_with(src.clone(), "mem", small_opts());
    wait_ready(&doc, numbered(200).len() as u64);
    assert_eq!(doc.snapshot().generation, 0);
    src.replace(b"fresh 1\nfresh 2\n".to_vec());
    let ev = wait_event(&doc, "Truncated", |e| {
        matches!(e, DocEvent::Truncated { .. })
    });
    let DocEvent::Truncated { generation, .. } = ev else {
        unreachable!()
    };
    assert_eq!(generation, 1);
    wait_ready(&doc, 16);
    let s = doc.snapshot();
    assert_eq!(s.generation, 1);
    assert_eq!(s.lines.known, 2);
    assert!(s.last_event.is_some());
    let r = request(
        &doc,
        LineRequest::Range {
            first: 0,
            count: 10,
        },
    );
    assert_eq!(texts(&r), ["fresh 1", "fresh 2"]);
}

#[test]
fn start_at_tail_sends_initial_lines() {
    let src = Arc::new(MemSource::new(numbered(50)));
    let doc = Document::from_source_with(
        src,
        "mem",
        OpenOptions {
            start_at_tail: Some(3),
            ..small_opts()
        },
    );
    let ev = wait_event(&doc, "initial lines", |e| {
        matches!(e, DocEvent::Lines { .. })
    });
    let DocEvent::Lines { id, lines, .. } = ev else {
        unreachable!()
    };
    assert_eq!(id, oxtail_core::RequestId::INITIAL);
    assert_eq!(texts(&lines), ["line 47", "line 48", "line 49"]);
}

#[test]
fn empty_source_and_long_lines() {
    let src = Arc::new(MemSource::new(Vec::new()));
    let doc = Document::from_source_with(
        src.clone(),
        "mem",
        OpenOptions {
            max_display_len: 100,
            ..small_opts()
        },
    );
    wait_until("ready", || doc.snapshot().state == DocState::Ready);
    assert!(request(&doc, LineRequest::Tail { count: 5 }).is_empty());
    assert_eq!(doc.snapshot().lines.known, 0);
    let mut big = "x".repeat(10_000);
    big.push('\n');
    big.push_str("after\n");
    src.append(big.as_bytes());
    wait_ready(&doc, big.len() as u64);
    let r = request(&doc, LineRequest::Range { first: 0, count: 2 });
    assert!(r[0].truncated);
    assert_eq!(r[0].text.len(), 100);
    assert_eq!(r[0].len, 10_001);
    assert_eq!(r[1].text, "after");
}

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

fn spool_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

#[test]
fn utf16_and_spool_lifecycle() {
    let text = "first ünï\r\nsecond 日本\nthird";
    for (be, bom) in [(false, true), (true, true), (false, false), (true, false)] {
        let spool = tempfile::tempdir().unwrap();
        let raw = utf16(text, be, bom);
        let doc = Document::from_source_with(
            Arc::new(MemSource::new(raw)),
            "u16",
            OpenOptions {
                spool_dir: Some(spool.path().to_path_buf()),
                ..small_opts()
            },
        );
        let expect_len = text.len() as u64;
        wait_ready(&doc, expect_len);
        let s = doc.snapshot();
        assert!(s.spooled);
        assert_eq!(
            s.encoding,
            if be {
                TextEncoding::UTF_16BE
            } else {
                TextEncoding::UTF_16LE
            }
        );
        assert_eq!(spool_files(spool.path()), 1);
        let r = request(
            &doc,
            LineRequest::Range {
                first: 0,
                count: 10,
            },
        );
        assert_eq!(texts(&r), ["first ünï", "second 日本", "third"]);
        // The view is UTF-8: search can scan it directly.
        let src = doc.source();
        let mut buf = vec![0u8; expect_len as usize];
        src.read_exact_at(0, &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), text);
        drop(doc);
        assert_eq!(
            spool_files(spool.path()),
            0,
            "spool must be deleted on drop"
        );
    }
}

#[test]
fn windows_1252_and_manual_override() {
    let raw = b"caf\xe9 cr\xe8me br\xfbl\xe9e\nna\xefve fa\xe7ade d\xe9j\xe0 vu\n".repeat(30);
    let doc = Document::from_source_with(Arc::new(MemSource::new(raw.clone())), "w", small_opts());
    wait_until("detected", || doc.snapshot().utf8_len > 0);
    let s = doc.snapshot();
    assert!(s.spooled);
    assert!(!s.encoding.is_utf8());
    wait_until("indexed", || {
        doc.snapshot().lines.exact && doc.snapshot().lines.known == 60
    });
    let r = request(&doc, LineRequest::Range { first: 0, count: 2 });
    assert_eq!(texts(&r), ["café crème brûlée", "naïve façade déjà vu"]);

    // Forcing UTF-8 rebuilds the view (lossy) and bumps the generation.
    doc.set_encoding(EncodingChoice::Fixed(TextEncoding::UTF_8));
    wait_event(&doc, "EncodingChanged", |e| {
        matches!(e, DocEvent::EncodingChanged { .. })
    });
    wait_until("utf8 view", || {
        let s = doc.snapshot();
        !s.spooled && s.generation == 1 && s.lines.exact && s.lines.known == 60
    });
    let r = request(&doc, LineRequest::Range { first: 0, count: 1 });
    assert!(r[0].text.contains('\u{FFFD}'));
}

#[test]
fn lone_cr_files_are_spooled_with_newlines() {
    let raw = b"one\rtwo\rthree\r".to_vec();
    let doc = Document::from_source_with(Arc::new(MemSource::new(raw)), "cr", small_opts());
    wait_ready(&doc, 14);
    let s = doc.snapshot();
    assert!(s.spooled);
    assert_eq!(s.line_ending, LineEnding::Cr);
    let r = request(
        &doc,
        LineRequest::Range {
            first: 0,
            count: 10,
        },
    );
    assert_eq!(texts(&r), ["one", "two", "three"]);
}

#[test]
fn invalid_utf8_is_lossy_without_panic() {
    let raw = vec![
        b'a', 0xff, 0xfe, b'\n', 0xc3, b'\n', 0x00, 0x01, b'z', b'\n', 0xe2, 0x82,
    ];
    let doc = Document::from_source_with(
        Arc::new(MemSource::new(raw.clone())),
        "bin",
        OpenOptions {
            encoding: EncodingChoice::Fixed(TextEncoding::UTF_8),
            ..small_opts()
        },
    );
    wait_ready(&doc, raw.len() as u64);
    let r = request(
        &doc,
        LineRequest::Range {
            first: 0,
            count: 10,
        },
    );
    assert_eq!(r.len(), 4);
    assert!(r[0].text.contains('\u{FFFD}'));
    assert_eq!(r[2].text, "\u{0}\u{1}z");
}

#[test]
fn multibyte_char_split_across_appends() {
    // UTF-16LE: "ab\n" with the second code unit cut in half.
    let raw = utf16("ab\n", false, true);
    let src = Arc::new(MemSource::new(raw[..5].to_vec())); // BOM, 'a', half of 'b'
    let doc = Document::from_source_with(src.clone(), "split", small_opts());
    wait_until("first part transcoded", || doc.snapshot().utf8_len == 1);
    src.append(&raw[5..]);
    wait_ready(&doc, 3);
    let r = request(&doc, LineRequest::Range { first: 0, count: 5 });
    assert_eq!(texts(&r), ["ab"]);
}

#[test]
fn stdin_like_reader_is_spooled_and_followed() {
    let spool = tempfile::tempdir().unwrap();
    let input = numbered(100);
    let doc = Document::from_reader(
        Box::new(std::io::Cursor::new(input.clone())),
        "<stdin>",
        Some(spool.path()),
    )
    .unwrap();
    wait_ready(&doc, input.len() as u64);
    wait_until("writer finished", || !doc.snapshot().writing);
    let t = request(&doc, LineRequest::Tail { count: 1 });
    assert_eq!(texts(&t), ["line 99"]);
    // Every file left in the spool dir carries the shared prefix.
    let names: Vec<String> = std::fs::read_dir(spool.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.is_empty());
    assert!(
        names
            .iter()
            .all(|n| n.starts_with(oxtail_core::SPOOL_PREFIX)),
        "{names:?}"
    );
    drop(doc);
    assert_eq!(spool_files(spool.path()), 0);
}

fn data_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            4 => Just(b'\n'),
            2 => Just(b'\r'),
            10 => any::<u8>().prop_map(|b| b'a' + (b % 26)),
            1 => Just(0xC3u8),
        ],
        0..500,
    )
}

fn naive_lines(data: &[u8]) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' {
            out.push((start as u64, line_text(&data[start..i])));
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push((start as u64, line_text(&data[start..])));
    }
    out
}

fn line_text(mut l: &[u8]) -> String {
    if l.last() == Some(&b'\r') {
        l = &l[..l.len() - 1];
    }
    String::from_utf8_lossy(l).into_owned()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn document_agrees_with_naive_lines(
        data in data_strategy(),
        spacing in 1u64..100,
        extra in data_strategy(),
    ) {
        // Force UTF-8 so the encoding detector stays out of the picture; lone-CR
        // detection still applies, so only plain-LF inputs are compared.
        let first = if data.is_empty() { &extra } else { &data };
        let has_lf = first.contains(&b'\n');
        let has_cr = first.contains(&b'\r');
        prop_assume!(has_lf || !has_cr);
        let src = Arc::new(MemSource::new(data.clone()));
        let doc = Document::from_source_with(src.clone(), "p", OpenOptions {
            index_spacing: spacing,
            encoding: EncodingChoice::Fixed(TextEncoding::UTF_8),
            ..OpenOptions::default()
        });
        let mut model = data;
        if !model.is_empty() {
            wait_ready(&doc, model.len() as u64);
        }
        for round in 0..2 {
            let naive = naive_lines(&model);
            let got = doc.read_lines_blocking(0, 10_000);
            prop_assert_eq!(got.len(), naive.len());
            for (g, (off, text)) in got.iter().zip(&naive) {
                prop_assert_eq!(g.offset, *off);
                prop_assert_eq!(&g.text, text);
            }
            if !naive.is_empty() {
                let k = 3.min(naive.len());
                let t = request(&doc, LineRequest::Tail { count: k });
                let want = &naive[naive.len() - k..];
                prop_assert_eq!(t.len(), k);
                for (g, (off, text)) in t.iter().zip(want) {
                    prop_assert_eq!(g.offset, *off);
                    prop_assert_eq!(&g.text, text);
                    prop_assert!(g.number_exact);
                }
                prop_assert_eq!(t[0].number, (naive.len() - k) as u64);
            }
            if round == 0 {
                if extra.is_empty() {
                    break;
                }
                src.append(&extra);
                model.extend_from_slice(&extra);
                wait_ready(&doc, model.len() as u64);
            }
        }
    }
}

/// A source whose big reads (the indexer's) fail past `gate` until opened,
/// while small reads (line requests) always work. This freezes indexing half
/// way, deterministically, so the approximate-line-number paths can be tested.
struct GateSource {
    inner: MemSource,
    open: std::sync::atomic::AtomicBool,
    gate: u64,
}

impl ReadAt for GateSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.open.load(Ordering::SeqCst) && buf.len() > 300_000 {
            if offset >= self.gate {
                return Err(std::io::Error::other("gate closed"));
            }
            let room = ((self.gate - offset) as usize).min(buf.len());
            return self.inner.read_at(offset, &mut buf[..room]);
        }
        self.inner.read_at(offset, buf)
    }
    fn len(&self) -> std::io::Result<u64> {
        self.inner.len()
    }
}

#[test]
fn approximate_numbers_before_indexing_completes() {
    let mut data = Vec::new();
    for i in 0..174_762 {
        data.extend_from_slice(format!("line {i:06}\n").as_bytes());
    }
    let total = data.len() as u64;
    let gate = 1 << 20;
    let src = Arc::new(GateSource {
        inner: MemSource::new(data),
        open: std::sync::atomic::AtomicBool::new(false),
        gate,
    });
    let doc = Document::from_source_with(src.clone(), "gated", OpenOptions::default());
    wait_until("partial index", || {
        let s = doc.snapshot();
        s.utf8_len == total && s.indexed_bytes == gate
    });
    let s = doc.snapshot();
    assert!(!s.lines.exact);
    assert!(s.lines.known > 80_000 && s.lines.known < 90_000);
    // The estimate of the total is close (fixed-width lines).
    assert!(
        s.lines.estimated_total.abs_diff(174_762) < 50,
        "{:?}",
        s.lines
    );

    // Range beyond the indexed region: approximate numbers, plausible text.
    let r = request(
        &doc,
        LineRequest::Range {
            first: 150_000,
            count: 3,
        },
    );
    assert_eq!(r.len(), 3);
    assert!(r.iter().all(|l| !l.number_exact));
    let n: i64 = r[0].text.trim_start_matches("line ").parse().unwrap();
    assert!((n - 150_000).abs() < 5, "asked 150000, got {n}");
    assert!((r[0].number as i64 - 150_000).abs() < 5);

    // The scrollbar mapping works too, and the tail needs no index at all.
    let f = request(
        &doc,
        LineRequest::ByteFraction {
            fraction: 0.75,
            count: 2,
        },
    );
    assert_eq!(f.len(), 2);
    assert!(!f[0].number_exact);
    let t = request(&doc, LineRequest::Tail { count: 2 });
    assert_eq!(texts(&t), ["line 174760", "line 174761"]);
    assert!(!t[0].number_exact);
    // A line inside the indexed region is exact.
    let head = request(
        &doc,
        LineRequest::Range {
            first: 1000,
            count: 1,
        },
    );
    assert!(head[0].number_exact);
    assert_eq!(head[0].text, "line 001000");

    // Once reads succeed again the actor recovers and finishes the index.
    src.open.store(true, Ordering::SeqCst);
    wait_ready(&doc, total);
    let t = request(&doc, LineRequest::Tail { count: 1 });
    assert!(t[0].number_exact);
    assert_eq!(t[0].number, 174_761);
    assert_eq!(doc.snapshot().state, DocState::Ready);
}

#[test]
fn request_from_an_old_generation_gets_an_empty_answer_tagged_old() {
    let src = Arc::new(MemSource::new(b"aaaa\nbbbb\ncccc\n".to_vec()));
    let doc = Document::from_source_with(src.clone(), "mem", small_opts());
    wait_ready(&doc, 15);
    let g0 = doc.generation();
    src.replace(b"zz\nyyyyyyyy\n".to_vec());
    doc.refresh();
    wait_ready(&doc, 12);
    assert!(doc.generation() > g0);
    // Offsets 5 and 10 belonged to the old content; they must not be resolved
    // against the new content and tagged as current.
    let id = doc.request_lines_in(g0, LineRequest::AtOffsets(vec![5, 10]));
    let (g, lines) = wait_lines(&doc, id);
    assert_eq!(g, g0);
    assert!(lines.is_empty(), "{lines:?}");
    // A request made now is answered in the current generation.
    let id = doc.request_lines(LineRequest::Range { first: 0, count: 5 });
    let (g, lines) = wait_lines(&doc, id);
    assert_eq!(g, doc.generation());
    assert_eq!(texts(&lines), ["zz", "yyyyyyyy"]);
}

#[test]
fn blocking_helpers_report_the_generation() {
    let src = Arc::new(MemSource::new(numbered(50)));
    let doc = Document::from_source_with(src.clone(), "mem", small_opts());
    let len = numbered(50).len() as u64;
    wait_ready(&doc, len);
    let (g, lines) = doc.read_lines_blocking_with_generation(10, 2);
    assert_eq!(g, doc.generation());
    assert_eq!(texts(&lines), ["line 10", "line 11"]);
    assert_eq!(doc.read_lines_blocking(10, 2), lines);
    let (g2, pos) = doc.line_of_offset_with_generation(len - 3).unwrap();
    assert_eq!(g2, g);
    assert_eq!(pos.line, 49);
    src.replace(b"only\n".to_vec());
    doc.refresh();
    wait_ready(&doc, 5);
    let (g3, lines) = doc.read_lines_blocking_with_generation(0, 5);
    assert!(g3 > g);
    assert_eq!(texts(&lines), ["only"]);
}

/// A source that counts the bytes handed out.
struct Counting {
    inner: MemSource,
    bytes: AtomicU64,
}

impl ReadAt for Counting {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read_at(offset, buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
    fn len(&self) -> std::io::Result<u64> {
        self.inner.len()
    }
}

#[test]
fn requests_on_a_huge_single_line_read_bounded_bytes() {
    let n = 64 * 1024 * 1024;
    let src = Arc::new(Counting {
        inner: MemSource::new(vec![b'x'; n]),
        bytes: AtomicU64::new(0),
    });
    let doc = Document::from_source_with(
        src.clone(),
        "long",
        OpenOptions {
            follow: false,
            ..OpenOptions::default()
        },
    );
    wait_ready(&doc, n as u64);
    let read = || src.bytes.swap(0, Ordering::Relaxed);
    read();
    let mib = 1024 * 1024;

    let t = request(&doc, LineRequest::Tail { count: 10 });
    assert_eq!(t.len(), 1);
    assert!(!t[0].number_exact, "start of a cut-off tail is approximate");
    let tail_bytes = read();
    assert!(tail_bytes < 24 * mib, "tail read {} MiB", tail_bytes / mib);

    let f = request(
        &doc,
        LineRequest::ByteFraction {
            fraction: 0.5,
            count: 1,
        },
    );
    assert_eq!(f.len(), 1);
    // Forward measurement of the one long line is inherent; the backward
    // search and the number lookup are what must stay bounded.
    assert!(f[0].offset >= n as u64 / 2 - 4 * mib - 64 * 1024);
    let pos = doc.line_of_offset(n as u64 / 2).unwrap();
    assert_eq!(pos.line, 0);
    assert!(pos.exact);
    assert!(!pos.start_exact);
    let lookup_bytes = read();
    // 2 x 4 MiB back-scans + 2 x <=64 KiB counts + the forward line
    // measurement of the fraction request (32 MiB).
    assert!(
        lookup_bytes < 48 * mib,
        "lookups read {} MiB",
        lookup_bytes / mib
    );

    let a = request(&doc, LineRequest::AtOffsets(vec![n as u64 / 4]));
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].number, 0);
    assert!(a[0].number_exact);
}

#[test]
fn one_response_never_carries_more_than_the_text_budget() {
    // 1500 lines of ~16 KiB = ~24 MiB; asking for all of them must not yield
    // 24 MiB of text in a single event.
    let mut data = Vec::new();
    for i in 0..1500 {
        data.extend_from_slice(format!("{i:05} ").as_bytes());
        data.extend_from_slice(&vec![b'z'; 16 * 1024 - 6]);
        data.push(b'\n');
    }
    let src = Arc::new(MemSource::new(data.clone()));
    let doc = Document::from_source_with(src, "wide", small_opts());
    wait_ready(&doc, data.len() as u64);
    for req in [
        LineRequest::Range {
            first: 0,
            count: 20_000,
        },
        LineRequest::Tail { count: 20_000 },
        LineRequest::AtOffsets((0..1500u64).map(|i| i * 16385).collect()),
    ] {
        let lines = request(&doc, req);
        let total: usize = lines.iter().map(|l| l.text.len()).sum();
        assert!(!lines.is_empty());
        assert!(lines.len() < 1500, "{} lines", lines.len());
        assert!(
            total <= 8 * 1024 * 1024 + 16 * 1024,
            "{total} bytes of text"
        );
    }
}
