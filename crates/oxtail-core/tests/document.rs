//! Document tests over in-memory sources and real files.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
