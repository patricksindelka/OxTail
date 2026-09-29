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
