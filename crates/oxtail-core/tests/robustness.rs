//! Regression tests for review findings: shutdown latency, spooled tails,
//! silent rewrites, encoding re-detection.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::{DocEvent, Document, LineRequest, MemSource, OpenOptions, RequestId};

use common::*;

fn numbered(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    v
}

#[test]
fn dropping_a_document_does_not_wait_for_queued_requests() {
    let data = numbered(300_000);
    let src = Arc::new(MemSource::new(data.clone()));
    let doc = Document::from_source(src, "busy");
    wait_ready(&doc, data.len() as u64);
    let first = doc.request_lines(LineRequest::Range {
        first: 0,
        count: 20_000,
    });
    for _ in 0..3000 {
        doc.request_lines(LineRequest::Range {
            first: 0,
            count: 20_000,
        });
    }
    // Once the first answer is out, the actor is draining the backlog.
    wait_lines(&doc, first);
    let t = Instant::now();
    drop(doc);
    let took = t.elapsed();
    assert!(took < Duration::from_millis(50), "drop took {took:?}");
}

fn utf16le_rows(n: usize) -> Vec<u8> {
    let mut raw = vec![0xFF, 0xFE];
    for i in 0..n {
        for u in format!("row {i}\n").encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
    }
    raw
}

/// The spool is filled in 4 MiB steps from offset 0; a tail asked for before
/// it has caught up used to return lines from the middle of the file.
#[test]
fn tail_of_a_large_spooled_file_is_the_real_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big16.log");
    let raw = utf16le_rows(400_000);
    assert!(
        raw.len() > 2 * 4 * 1024 * 1024,
        "must exceed several spool steps"
    );
    std::fs::write(&path, &raw).unwrap();
    let doc = Document::open(
        &path,
        OpenOptions {
            start_at_tail: Some(2),
            spool_dir: Some(dir.path().to_path_buf()),
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let ev = wait_event(
        &doc,
        "initial tail",
        |e| matches!(e, DocEvent::Lines { id, .. } if *id == RequestId::INITIAL),
    );
    let DocEvent::Lines { lines, .. } = ev else {
        unreachable!()
    };
    assert_eq!(texts(&lines), ["row 399998", "row 399999"]);
    // Whatever is not indexed yet must not claim an exact number.
    let snap = doc.snapshot();
    if !snap.lines.exact {
        assert!(lines.iter().all(|l| !l.number_exact));
    }
    // A Tail request behaves the same, also while the spool is (re)filling.
    let t = request(&doc, LineRequest::Tail { count: 3 });
    assert_eq!(texts(&t), ["row 399997", "row 399998", "row 399999"]);
    // And offsets in the answer are real view offsets.
    let a = request(&doc, LineRequest::AtOffsets(vec![t[0].offset]));
    assert_eq!(texts(&a), ["row 399997"]);
    wait_until("fully indexed", || doc.snapshot().lines.exact);
    let t = request(&doc, LineRequest::Tail { count: 1 });
    assert_eq!(t[0].number, 399_999);
    assert!(t[0].number_exact);
}

fn lines_of(doc: &Document) -> Vec<String> {
    request(
        doc,
        LineRequest::Range {
            first: 0,
            count: 100,
        },
    )
    .into_iter()
    .map(|l| l.text)
    .collect()
}

/// A rewrite that is not shorter than the old content is not "growth".
#[test]
fn rewrite_to_a_longer_or_equal_size_is_detected_for_memory_sources() {
    let src = Arc::new(MemSource::new(b"old line A\nold line B\n".to_vec()));
    let doc = Document::from_source(src.clone(), "m");
    wait_ready(&doc, 22);
    let g0 = doc.generation();
    src.replace(b"NEW 1\nNEW 2\nNEW 3\nNEW 4\nNEW 5\n".to_vec());
    doc.refresh();
    wait_ready(&doc, 30);
    assert!(doc.generation() > g0, "expected a new generation");
    assert_eq!(
        lines_of(&doc),
        ["NEW 1", "NEW 2", "NEW 3", "NEW 4", "NEW 5"]
    );
    assert_eq!(doc.snapshot().lines.known, 5);

    // Exactly the same length, different content.
    let g1 = doc.generation();
    src.replace(b"aaa 1\nbbb 2\nccc 3\nddd 4\neee 5\n".to_vec());
    doc.refresh();
    wait_until("second rewrite noticed", || doc.generation() > g1);
    wait_ready(&doc, 30);
    assert_eq!(
        lines_of(&doc),
        ["aaa 1", "bbb 2", "ccc 3", "ddd 4", "eee 5"]
    );
}

#[test]
fn rewrite_of_a_real_file_to_a_larger_size_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rw.log");
    std::fs::write(&path, b"first old\nsecond old\n").unwrap();
    let doc = Document::open(&path, OpenOptions::default()).unwrap();
    wait_ready(&doc, 21);
    let g0 = doc.generation();
    // `>` redirect: same inode, truncated and refilled with more data.
    let new = b"fresh 1\nfresh 2\nfresh 3\nfresh 4\n";
    std::fs::write(&path, new).unwrap();
    doc.refresh();
    wait_until("rewrite noticed", || doc.generation() > g0);
    wait_ready(&doc, new.len() as u64);
    assert_eq!(lines_of(&doc), ["fresh 1", "fresh 2", "fresh 3", "fresh 4"]);
}

/// Follow-by-name: the path is gone (renamed away, not recreated) but the
/// writer keeps appending to the old file through its handle.
#[test]
fn growth_of_a_renamed_away_file_is_still_followed() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("moved.log");
    std::fs::write(&path, b"one\ntwo\n").unwrap();
    let doc = Document::open(&path, OpenOptions::default()).unwrap();
    wait_ready(&doc, 8);
    let renamed = dir.path().join("moved.log.1");
    std::fs::rename(&path, &renamed).unwrap();
    wait_until("removal noticed", || doc.snapshot().file_missing);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&renamed)
        .unwrap();
    f.write_all(b"three\nfour\n").unwrap();
    f.flush().unwrap();
    doc.refresh();
    wait_ready(&doc, 19);
    assert!(doc.snapshot().file_missing, "still reported as missing");
    assert_eq!(lines_of(&doc), ["one", "two", "three", "four"]);
}
